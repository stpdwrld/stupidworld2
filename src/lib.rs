mod common;
mod config;
mod proxy;

use crate::config::Config;
use crate::proxy::*;

use std::collections::HashMap;
use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;
use worker::*;
use once_cell::sync::Lazy;
use regex::Regex;

static PROXYIP_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^.+-\d+$").unwrap());
static PROXYKV_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^([A-Z]{2})").unwrap());

// Generate dynamic path based on date
fn get_dynamic_path() -> String {
    let today = Utc::now().format("%Y%m%d").to_string();
    let hash = format!("{:x}", md5::compute(today.as_bytes()));
    format!("/dynamic-{}", &hash[..8])
}

#[event(fetch)]
async fn main(req: Request, env: Env, _: Context) -> Result<Response> {
    let uuid = env
        .var("UUID")
        .map(|x| Uuid::parse_str(&x.to_string()).unwrap_or_default())?;
    let host = req.url()?.host().map(|x| x.to_string()).unwrap_or_default();
    let main_page_url = env.var("MAIN_PAGE_URL").map(|x|x.to_string()).unwrap();
    let link_page_url = env.var("LINK_PAGE_URL").map(|x|x.to_string()).unwrap();
    let sub_page_url = env.var("SUB_PAGE_URL").map(|x|x.to_string()).unwrap();
    
    // Get dynamic path
    let dynamic_path = get_dynamic_path();
    
    let config = Config { 
        uuid, 
        host: host.clone(), 
        proxy_addr: host.clone(), 
        proxy_port: 443, 
        main_page_url, 
        link_page_url, 
        sub_page_url,
        dynamic_path: dynamic_path.clone()
    };

    Router::with_data(config)
        .on_async("/", fe)
        .on_async("/link", link)
        .on_async("/sub", sub)
        .on("/v2r", v2r)
        .on_async(&dynamic_path, tunnel) // Dynamic path endpoint
        .on_async("/:proxyip", tunnel)
        .on_async("/Stupid-World/:proxyip", tunnel)
        .run(req, env)
        .await
}

// ... (fungsi lainnya tetap sama)

async fn tunnel(req: Request, mut cx: RouteContext<Config>) -> Result<Response> {
    let mut proxyip = match cx.param("proxyip") {
        Some(ip) => ip.to_string(),
        None => {
            // If no proxyip param, use dynamic path
            cx.data.dynamic_path.trim_start_matches('/').to_string()
        }
    };

    // ... (kode proxy selection tetap sama)
    
    // Add random delay to avoid pattern detection
    let delay_ms = (rand::random::<u8>() as u64) * 10;
    worker::Delay::from_millis(delay_ms).await;

    let upgrade = req.headers().get("Upgrade")?.unwrap_or_default();
    if upgrade == "websocket".to_string() && PROXYIP_PATTERN.is_match(&proxyip) {
        // ... (kode websocket tetap sama)
    } else {
        // Return fake 404 page for non-websocket requests
        Response::error("Not Found", 404)
    }
}

fn v2r(_: Request, cx: RouteContext<Config>) -> Result<Response> {
    let host = cx.data.host.to_string();
    let uuid = cx.data.uuid.to_string();
    let dynamic_path = cx.data.dynamic_path.clone();

    // Use dynamic path in configuration
    let vmess_v2r = {
        let config = json!({
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
            "path": dynamic_path,
            "tls": "",
            "sni": "",
            "alpn": ""}
        );
        format!("vmess://{}", URL_SAFE.encode(config.to_string()))
    };
    
    let vless_v2r = format!("vless://{uuid}@{host}:443?encryption=none&type=ws&host={host}&path={}&security=tls&sni={host}#siren vless", 
        urlencoding::encode(&dynamic_path));
    
    // ... (konfigurasi lainnya dengan dynamic path)
    
    Response::from_body(ResponseBody::Body(format!("{vmess_v2r}\n{vless_v2r}\n{trojan_v2r}\n{ss_v2r}").into()))
}
