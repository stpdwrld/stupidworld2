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

// Cache untuk menyimpan data proxy KV
static mut PROXY_KV_CACHE: Option<HashMap<String, Vec<String>>> = None;
static CACHE_EXPIRY: u64 = 60 * 60 * 24; // 24 jam dalam detik

#[event(fetch)]
async fn main(req: Request, env: Env, ctx: Context) -> Result<Response> {
    // Error handling yang lebih baik untuk inisialisasi
    let uuid = match env.var("UUID") {
        Ok(var) => Uuid::parse_str(&var.to_string()).unwrap_or_default(),
        Err(_) => {
            console_error!("UUID environment variable not set");
            return Response::error("Internal Server Error", 500);
        }
    };

    let host = req.url()?.host().map(|x| x.to_string()).unwrap_or_default();
    
    // Handle error untuk environment variables
    let main_page_url = env.var("MAIN_PAGE_URL").map(|x|x.to_string()).unwrap_or_default();
    let sub_page_url = env.var("SUB_PAGE_URL").map(|x|x.to_string()).unwrap_or_default();
    let link_page_url = env.var("LINK_PAGE_URL").map(|x|x.to_string()).unwrap_or_default();

    let config = Config { 
        uuid, 
        host: host.clone(), 
        proxy_addr: host, 
        proxy_port: 443, 
        main_page_url, 
        sub_page_url, 
        link_page_url
    };

    // Router dengan error handling
    match Router::with_data(config)
        .on_async("/", fe)
        .on_async("/sub", sub)
        .on_async("/link", link)
        .on_async("/:proxyip", tunnel)
        .on_async("/Stupid-World/:proxyip", tunnel)
        .run(req, env)
        .await
    {
        Ok(res) => Ok(res),
        Err(e) => {
            console_error!("Router error: {:?}", e);
            Response::error("Internal Server Error", 500)
        }
    }
}

async fn get_response_from_url(url: String) -> Result<Response> {
    if url.is_empty() {
        return Response::error("Page URL not configured", 500);
    }

    match Fetch::Url(Url::parse(url.as_str())?) {
        req => {
            let mut res = req.send().await?;
            if res.status_code() == 200 {
                Response::from_html(res.text().await?)
            } else {
                Response::error(format!("Upstream error: {}", res.status_code()), res.status_code())
            }
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

async fn load_proxy_kv(kv: &KvStore) -> Result<HashMap<String, Vec<String>>> {
    // Coba dapatkan dari cache memori pertama
    unsafe {
        if let Some(cache) = &PROXY_KV_CACHE {
            return Ok(cache.clone());
        }
    }

    // Coba dapatkan dari KV store
    if let Some(proxy_kv_str) = kv.get("proxy_kv").text().await? {
        if let Ok(proxy_kv) = serde_json::from_str(&proxy_kv_str) {
            // Simpan ke cache memori
            unsafe {
                PROXY_KV_CACHE = Some(proxy_kv.clone());
            }
            return Ok(proxy_kv);
        }
    }

    // Jika tidak ada di KV, ambil dari GitHub
    console_log!("getting proxy kv from github...");
    let req = Fetch::Url(Url::parse("https://raw.githubusercontent.com/FoolVPN-ID/Nautica/refs/heads/main/kvProxyList.json")?);
    let mut res = req.send().await?;
    
    if res.status_code() != 200 {
        return Err(Error::from(format!("error getting proxy kv: {}", res.status_code())));
    }

    let proxy_kv_str = res.text().await?;
    let proxy_kv: HashMap<String, Vec<String>> = serde_json::from_str(&proxy_kv_str)?;

    // Simpan ke KV store dengan expiry
    kv.put("proxy_kv", &proxy_kv_str)?
        .expiration_ttl(CACHE_EXPIRY)
        .execute()
        .await?;

    // Simpan ke cache memori
    unsafe {
        PROXY_KV_CACHE = Some(proxy_kv.clone());
    }

    Ok(proxy_kv)
}

async fn tunnel(req: Request, mut cx: RouteContext<Config>) -> Result<Response> {
    let proxyip_param = cx.param("proxyip").unwrap_or_default();
    let mut proxyip = proxyip_param.to_string();

    if PROXYKV_PATTERN.is_match(&proxyip) {
        let kvid_list: Vec<String> = proxyip.split(',').map(|s| s.to_string()).collect();
        let kv = match cx.kv("SIREN") {
            Ok(kv) => kv,
            Err(e) => {
                console_error!("Failed to access KV store: {:?}", e);
                return Response::error("Internal Server Error", 500);
            }
        };

        let proxy_kv = match load_proxy_kv(&kv).await {
            Ok(kv) => kv,
            Err(e) => {
                console_error!("Failed to load proxy KV: {:?}", e);
                return Response::error("Proxy configuration error", 500);
            }
        };

        // Pilih random KV ID
        let mut rand_buf = [0u8; 1];
        getrandom::getrandom(&mut rand_buf).expect("failed generating random number");
        let kv_index = (rand_buf[0] as usize) % kvid_list.len();
        let selected_kv = &kvid_list[kv_index];

        // Pilih random proxy IP dari daftar yang tersedia
        if let Some(proxy_list) = proxy_kv.get(selected_kv) {
            let proxyip_index = (rand_buf[0] as usize) % proxy_list.len();
            proxyip = proxy_list[proxyip_index].clone().replace(':', "-");
        } else {
            return Response::error("Proxy configuration not found", 404);
        }
    }

    let upgrade = req.headers().get("Upgrade")?.unwrap_or_default();
    if upgrade == "websocket" && PROXYIP_PATTERN.is_match(&proxyip) {
        if let Some((addr, port_str)) = proxyip.split_once('-') {
            if let Ok(port) = port_str.parse() {
                cx.data.proxy_addr = addr.to_string();
                cx.data.proxy_port = port;
            }
        }
        
        let WebSocketPair { server, client } = match WebSocketPair::new() {
            Ok(pair) => pair,
            Err(e) => {
                console_error!("Failed to create WebSocket pair: {:?}", e);
                return Response::error("WebSocket error", 500);
            }
        };

        if let Err(e) = server.accept() {
            console_error!("Failed to accept WebSocket: {:?}", e);
            return Response::error("WebSocket error", 500);
        }
    
        wasm_bindgen_futures::spawn_local(async move {
            match server.events() {
                Ok(events) => {
                    if let Err(e) = ProxyStream::new(cx.data, &server, events).process().await {
                        console_error!("[tunnel]: {}", e);
                    }
                }
                Err(e) => {
                    console_error!("Failed to get WebSocket events: {:?}", e);
                }
            }
        });
    
        Response::from_websocket(client)
    } else {
        // Berikan respons default yang lebih informatif
        Response::ok("WebSocket proxy service is running. Connect with WebSocket protocol to establish a tunnel.")
    }
}
