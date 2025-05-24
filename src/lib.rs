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
    let main_page_url = env.var("MAIN_PAGE_URL").map(|x|x.to_string()).unwrap();
    let sub_page_url = env.var("SUB_PAGE_URL").map(|x|x.to_string()).unwrap();
    let link_page_url = env.var("LINK_PAGE_URL").map(|x|x.to_string()).unwrap();
    let convert_page_url = env.var("CONVERT_PAGE_URL").map(|x|x.to_string()).unwrap();
    let config = Config { 
        uuid, 
        host: host.clone(), 
        proxy_addr: host, 
        proxy_port: 443, 
        main_page_url, 
        sub_page_url, 
        link_page_url, 
        convert_page_url 
    };

    Router::with_data(config)
        .on_async("/", fe)
        .on_async("/sub", sub)
        .on_async("/link", link)
        .on_async("/convert", convert)
        .on_async("/:proxyip", tunnel)
        .on_async("/Stupid-World/:proxyip", tunnel)
        .run(req, env)
        .await
}

async fn get_response_from_url(url: String) -> Result<Response> {
    let req = Fetch::Url(Url::parse(url.as_str())?);
    let mut res = req.send().await?;
    Response::from_html(res.text().await?)
}

async fn fe(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.main_page_url).await
}

async fn sub(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.sub_page_url).await
}

async fn link(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.link_page_url).await
}

async fn convert(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    get_response_from_url(cx.data.convert_page_url).await
}

async fn tunnel(req: Request, mut cx: RouteContext<Config>) -> Result<Response> {
    let mut proxyip = cx.param("proxyip").unwrap().to_string();
    
    // Enhanced proxy selection with retry mechanism
    if PROXYKV_PATTERN.is_match(&proxyip) {
        let kvid_list: Vec<String> = proxyip.split(",").map(|s|s.to_string()).collect();
        let kv = cx.kv("SIREN")?;
        let mut proxy_kv_str = match kv.get("proxy_kv").text().await {
            Ok(Some(str)) => str,
            Ok(None) => {
                console_log!("Proxy KV cache empty, fetching from GitHub...");
                fetch_and_cache_proxy_kv(&kv).await?
            },
            Err(e) => {
                console_error!("Error accessing KV: {}", e);
                fetch_and_cache_proxy_kv(&kv).await?
            }
        };
        
        let proxy_kv: HashMap<String, Vec<String>> = match serde_json::from_str(&proxy_kv_str) {
            Ok(map) => map,
            Err(e) => {
                console_error!("Error parsing proxy KV: {}", e);
                // Try fetching fresh data if parsing fails
                proxy_kv_str = fetch_and_cache_proxy_kv(&kv).await?;
                serde_json::from_str(&proxy_kv_str)?
            }
        };
        
        // Select random KV ID with better randomness
        let mut rand_buf = [0u8; 4];
        getrandom::getrandom(&mut rand_buf).expect("failed generating random number");
        let kv_index = (u32::from_ne_bytes(rand_buf) as usize) % kvid_list.len();
        proxyip = kvid_list[kv_index].clone();
        
        // Select random proxy ip with health check
        let mut attempts = 0;
        let max_attempts = 3;
        let mut selected_proxy = None;
        
        while attempts < max_attempts && selected_proxy.is_none() {
            let proxy_list = match proxy_kv.get(&proxyip) {
                Some(list) if !list.is_empty() => list,
                _ => {
                    console_error!("No proxies found for key: {}", proxyip);
                    break;
                }
            };
            
            let proxy_index = (u32::from_ne_bytes(rand_buf) as usize) % proxy_list.len();
            let candidate = proxy_list[proxy_index].clone().replace(":", "-");
            
            // Simple health check (could be enhanced with actual connection test)
            if PROXYIP_PATTERN.is_match(&candidate) {
                selected_proxy = Some(candidate);
            }
            
            attempts += 1;
            getrandom::getrandom(&mut rand_buf).expect("failed generating random number");
        }
        
        proxyip = selected_proxy.unwrap_or_else(|| {
            console_error!("Failed to find healthy proxy after {} attempts", max_attempts);
            "fallback-proxy-443".to_string()
        });
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
            let mut retries = 0;
            let max_retries = 2;
            
            while retries < max_retries {
                match ProxyStream::new(cx.data.clone(), &server, events.clone()).process().await {
                    Ok(_) => break,
                    Err(e) => {
                        console_error!("[tunnel] attempt {} failed: {}", retries + 1, e);
                        retries += 1;
                        if retries >= max_retries {
                            console_error!("[tunnel] max retries reached");
                            let _ = server.close_with_reason(1011, "Connection failed after retries");
                        }
                    }
                }
            }
        });
    
        Response::from_websocket(client)
    } else {
        Response::error("Invalid request", 400)
    }
}

async fn fetch_and_cache_proxy_kv(kv: &worker::KvStore) -> Result<String> {
    let req = Fetch::Url(Url::parse("https://raw.githubusercontent.com/FoolVPN-ID/Nautica/refs/heads/main/kvProxyList.json")?);
    let mut res = req.send().await?;
    
    if res.status_code() == 200 {
        let proxy_kv_str = res.text().await?;
        kv.put("proxy_kv", &proxy_kv_str)?
            .expiration_ttl(60 * 60 * 6) // 6 hours cache instead of 24
            .execute()
            .await?;
        Ok(proxy_kv_str)
    } else {
        Err(Error::from(format!("Failed to fetch proxy KV: HTTP {}", res.status_code())))
    }
}
