mod common;
mod config;
mod proxy;

use crate::config::Config;
use crate::proxy::*;

use std::collections::HashMap;
use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use serde_json::json;
use uuid::Uuid;
use worker::*;
use once_cell::sync::Lazy;
use regex::Regex;

static PROXYIP_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^.+-\d+$").unwrap());
static PROXYKV_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^([A-Z]{2})").unwrap());

#[event(fetch)]
async fn main(req: Request, env: Env, _: Context) -> Result<Response> {
    let uuid = env
        .var("UUID")
        .map(|x| Uuid::parse_str(&x.to_string()).unwrap_or_default())?;
    let host = req.url()?.host().map(|x| x.to_string()).unwrap_or_default();
    let main_page_url = env.var("MAIN_PAGE_URL").map(|x| x.to_string()).unwrap();
    let link_page_url = env.var("LINK_PAGE_URL").map(|x| x.to_string()).unwrap();
    let sub_page_url = env.var("SUB_PAGE_URL").map(|x| x.to_string()).unwrap();

    let config = Config { 
        uuid, 
        host: host.clone(), 
        proxy_addr: host, 
        proxy_port: 443, 
        main_page_url, 
        link_page_url, 
        sub_page_url 
    };

    Router::with_data(config)
        .on_async("/", fe)
        .on_async("/link", link)
        .on_async("/sub", sub)
        .on("/v2r", v2r)
        .on_async("/:proxyip", tunnel)
        .on_async("/Stupid-World/:proxyip", tunnel)
        .run(req, env)
        .await
}

async fn get_response_from_url(url: &str) -> Result<Response> {
    let req = Fetch::Url(Url::parse(url)?);
    let res = req.send().await?;
    if res.status_code() == 200 {
        Response::from_html(res.text().await?)
    } else {
        Err(Error::from(format!("Failed to fetch: {}", res.status_code())))
    }
}

async fn fe(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(&cx.data.main_page_url).await
}

async fn link(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(&cx.data.link_page_url).await
}

async fn sub(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(&cx.data.sub_page_url).await
}

async fn tunnel(req: Request, mut cx: RouteContext<Config>) -> Result<Response> {
    let mut proxyip = cx.param("proxyip").unwrap().to_string();

    // Proxy Key-Value handling
    if PROXYKV_PATTERN.is_match(&proxyip) {
        let kvid_list: Vec<String> = proxyip.split(',').map(String::from).collect();
        let kv = cx.kv("SIREN")?;
        let mut proxy_kv_str = kv.get("proxy_kv").text().await?.unwrap_or_else(|| "".to_string());
        
        // Fetch proxy KV if needed
        if proxy_kv_str.is_empty() {
            console_log!("Fetching proxy KV from external source...");
            let req = Fetch::Url(Url::parse("https://raw.githubusercontent.com/FoolVPN-ID/Nautica/refs/heads/main/kvProxyList.json")?);
            let res = req.send().await?;
            if res.status_code() == 200 {
                proxy_kv_str = res.text().await?.to_string();
                kv.put("proxy_kv", &proxy_kv_str)?
                    .expiration_ttl(60 * 60 * 24) // 24 hours
                    .execute().await?;
            } else {
                return Err(Error::from(format!("Error getting proxy KV: {}", res.status_code())));
            }
        }

        let proxy_kv: HashMap<String, Vec<String>> = serde_json::from_str(&proxy_kv_str)?;

        // Select random KV and proxy IP
        let rand_buf = rand::random::<[u8; 2]>();
        let kv_index = (rand_buf[0] as usize) % kvid_list.len();
        proxyip = kvid_list[kv_index].clone();

        let proxyip_index = (rand_buf[0] as usize) % proxy_kv[&proxyip].len();
        proxyip = proxy_kv[&proxyip][proxyip_index].clone().replace(":", "-");
    }

    let upgrade = req.headers().get("Upgrade")?.unwrap_or_default();
    if upgrade == "websocket" && PROXYIP_PATTERN.is_match(&proxyip) {
        if let Some((addr, port_str)) = proxyip.split_once('-') {
            if let Ok(port) = port_str.parse() {
                cx.data.proxy_addr = addr.to_string();
                cx.data.proxy_port = port;
            }
        }

        let WebSocketPair { server, client } = WebSocketPair::new()?;
        server.accept()?;

        wasm_bindgen_futures::spawn_local(async move {
            let events = server.events().unwrap();
            if let Err(e) = ProxyStream::new(cx.data, &server, events).process().await {
                console_error!("[tunnel]: {}", e);
            }
        });

        Response::from_websocket(client)
    } else {
        Response::from_html("hi from wasm!")
    }
}

fn v2r(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    let host = cx.data.host.to_string();
    let uuid = cx.data.uuid.to_string();

    let vmess_v2r = json!({
        "ps": "siren vmess",
        "v": "2",
        "add": host,
        "port": "80",
        "id": uuid,
        "aid": "0",
        "scy": "zero",
        "net": "ws",
        "type": "none",
        "host": host,
        "path": "/KR",
        "tls": "",
        "sni": "",
        "alpn": ""
    });
    let vmess_v2r_str = format!("vmess://{}", URL_SAFE.encode(vmess_v2r.to_string()));

    let vless_v2r = format!("vless://{uuid}@{host}:443?encryption=none&type=ws&host={host}&path=%2FKR&security=tls&sni={host}#siren vless");
    let trojan_v2r = format!("trojan://{uuid}@{host}:443?encryption=none&type=ws&host={host}&path=%2FKR&security=tls&sni={host}#siren trojan");
    let ss_v2r = format!("ss://{}@{host}:443?plugin=v2ray-plugin%3Btls%3Bmux%3D0%3Bmode%3Dwebsocket%3Bpath%3D%2FKR%3Bhost%3D{host}#siren ss", URL_SAFE.encode(format!("none:{uuid}")));

    Response::from_body(ResponseBody::Body(format!("{vmess_v2r_str}\n{vless_v2r}\n{trojan_v2r}\n{ss_v2r}").into()))
}
