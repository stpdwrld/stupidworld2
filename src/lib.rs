mod common;
mod config;
mod proxy;

use crate::config::Config;
use crate::proxy::*;

use std::collections::HashMap;
use uuid::Uuid;
use worker::*;
use once_cell::sync::Lazy;
use regex::Regex;

static PROXYIP_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^.+-\d+$").unwrap());
static PROXYKV_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^([A-Z]{2})").unwrap());

const KV_CACHE_TTL: u64 = 60 * 60 * 24; // 24 hours

#[event(fetch)]
async fn main(req: Request, env: Env, _: Context) -> Result<Response> {
    let uuid = match env.var("UUID") {
        Ok(var) => Uuid::parse_str(&var.to_string()).unwrap_or_default(),
        Err(_) => {
            console_error!("UUID environment variable not set");
            return Response::error("Internal Server Error", 500);
        }
    };

    let host = req.url()?.host().map(|x| x.to_string()).unwrap_or_default();
    
    let main_page_url = env.var("MAIN_PAGE_URL").map(|x| x.to_string()).unwrap_or_default();
    let sub_page_url = env.var("SUB_PAGE_URL").map(|x| x.to_string()).unwrap_or_default();
    let link_page_url = env.var("LINK_PAGE_URL").map(|x| x.to_string()).unwrap_or_default();
    
    let config = Config { 
        uuid, 
        host: host.clone(), 
        proxy_addr: host, 
        proxy_port: 443, 
        main_page_url, 
        sub_page_url, 
        link_page_url
    };

    Router::with_data(config)
        .on_async("/", fe)
        .on_async("/sub", sub)
        .on_async("/link", link)
        .on_async("/:proxyip", tunnel)
        .on_async("/Stupid-World/:proxyip", tunnel)
        .run(req, env)
        .await
}

async fn get_response_from_url(url: String) -> Result<Response> {
    if url.is_empty() {
        return Response::error("Page URL not configured", 500);
    }

    let req = Request::new(url.as_str(), Method::Get)?;
    let mut res = Fetch::Request(req).send().await?;
    
    match res.text().await {
        Ok(text) => Response::from_html(text),
        Err(e) => {
            console_error!("Failed to parse response text: {}", e);
            Response::error("Failed to parse content", 500)
        }
    }
}

async fn fe(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.main_page_url.clone()).await
}

async fn sub(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.sub_page_url.clone()).await
}

async fn link(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.link_page_url.clone()).await
}

async fn tunnel(req: Request, mut cx: RouteContext<Config>) -> Result<Response> {
    let proxyip_param = match cx.param("proxyip") {
        Some(param) => param.to_string(),
        None => return Response::error("Proxy IP parameter missing", 400),
    };
    
    let mut proxyip = proxyip_param;
    
    if PROXYKV_PATTERN.is_match(&proxyip) {
        let kvid_list: Vec<String> = proxyip.split(",").map(|s| s.to_string()).collect();
        let kv = match cx.kv("SIREN") {
            Ok(kv) => kv,
            Err(e) => {
                console_error!("Failed to access KV store: {}", e);
                return Response::error("Internal Server Error", 500);
            }
        };
        
        let proxy_kv_str = match kv.get("proxy_kv").text().await {
            Ok(Some(str)) => str,
            Ok(None) => {
                console_log!("Proxy KV not found in cache, fetching from GitHub...");
                match fetch_proxy_kv_from_github().await {
                    Ok(str) => {
                        if let Err(e) = kv.put("proxy_kv", &str)?.expiration_ttl(KV_CACHE_TTL).execute().await {
                            console_error!("Failed to cache proxy KV: {}", e);
                        }
                        str
                    }
                    Err(e) => {
                        console_error!("Failed to fetch proxy KV: {}", e);
                        return Response::error("Failed to fetch proxy list", 502);
                    }
                }
            }
            Err(e) => {
                console_error!("Failed to read proxy KV: {}", e);
                return Response::error("Internal Server Error", 500);
            }
        };
        
        let proxy_kv: HashMap<String, Vec<String>> = match serde_json::from_str(&proxy_kv_str) {
            Ok(map) => map,
            Err(e) => {
                console_error!("Failed to parse proxy KV: {}", e);
                return Response::error("Invalid proxy list format", 500);
            }
        };
        
        let rand_buf = match get_random_bytes(1) {
            Ok(buf) => buf,
            Err(e) => {
                console_error!("Failed to generate random bytes: {}", e);
                return Response::error("Internal Server Error", 500);
            }
        };
        
        let kv_index = (rand_buf[0] as usize) % kvid_list.len();
        proxyip = kvid_list[kv_index].clone();
        
        if let Some(proxy_list) = proxy_kv.get(&proxyip) {
            if proxy_list.is_empty() {
                return Response::error("No proxies available for this region", 404);
            }
            let proxyip_index = (rand_buf[0] as usize) % proxy_list.len();
            proxyip = proxy_list[proxyip_index].clone().replace(":", "-");
        } else {
            return Response::error("Proxy region not found", 404);
        }
    }

    let upgrade = req.headers().get("Upgrade").unwrap_or_default();
    if upgrade == Some("websocket".to_string()) && PROXYIP_PATTERN.is_match(&proxyip) {
        if let Some((addr, port_str)) = proxyip.split_once('-') {
            if let Ok(port) = port_str.parse() {
                cx.data.proxy_addr = addr.to_string();
                cx.data.proxy_port = port;
            }
        }
        
        let WebSocketPair { server, client } = match WebSocketPair::new() {
            Ok(pair) => pair,
            Err(e) => {
                console_error!("Failed to create WebSocket pair: {}", e);
                return Response::error("WebSocket error", 500);
            }
        };
        
        match server.accept() {
            Ok(_) => (),
            Err(e) => {
                console_error!("Failed to accept WebSocket: {}", e);
                return Response::error("WebSocket error", 500);
            }
        };
    
        wasm_bindgen_futures::spawn_local(async move {
            match server.events() {
                Ok(events) => {
                    if let Err(e) = ProxyStream::new(cx.data, &server, events).process().await {
                        console_error!("[tunnel]: {}", e);
                    }
                }
                Err(e) => {
                    console_error!("Failed to get WebSocket events: {}", e);
                }
            }
        });
    
        Response::from_websocket(client)
    } else {
        Response::from_html("hi from wasm!")
    }
}

async fn fetch_proxy_kv_from_github() -> Result<String> {
    let req = Request::new("https://raw.githubusercontent.com/FoolVPN-ID/Nautica/refs/heads/main/kvProxyList.json", Method::Get)?;
    let mut res = Fetch::Request(req).send().await?;
    
    if res.status_code() != 200 {
        return Err(Error::from(format!("GitHub returned status code: {}", res.status_code())));
    }
    
    res.text().await.map_err(|e| e.into())
}

fn get_random_bytes(count: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; count];
    getrandom::getrandom(&mut buf).map_err(|e| Error::from(e.to_string()))?;
    Ok(buf)
}
