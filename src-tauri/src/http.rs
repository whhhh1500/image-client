use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// A loopback endpoint must bypass system proxies, otherwise a local model
/// server can be routed through an HTTP proxy.
pub fn is_loopback_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .is_some_and(|host| {
            // `host_str` keeps the brackets around IPv6 literals.
            let host = host.trim_start_matches('[').trim_end_matches(']');
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        })
}

fn cached(no_proxy: bool) -> Result<reqwest::Client, String> {
    // Clients are cheap to clone and share their connection pool + TLS session
    // cache. Building one per call (the previous behaviour) forced a fresh
    // TCP+TLS handshake for every LLM/agent/API request.
    static CLIENTS: OnceLock<Mutex<HashMap<bool, reqwest::Client>>> = OnceLock::new();
    let cache = CLIENTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(client) = guard.get(&no_proxy) {
            return Ok(client.clone());
        }
    }
    let builder = reqwest::Client::builder();
    let builder = if no_proxy { builder.no_proxy() } else { builder };
    let client = builder
        .build()
        .map_err(|error| format!("初始化 HTTP 客户端失败: {error}"))?;
    if let Ok(mut guard) = cache.lock() {
        guard.insert(no_proxy, client.clone());
    }
    Ok(client)
}

/// Client matching the endpoint's proxy policy. Per-request timeouts stay on
/// the request builder, so one client serves every caller.
pub fn client_for_url(url: &str) -> Result<reqwest::Client, String> {
    cached(is_loopback_url(url))
}

/// Client for arbitrary public URLs (asset downloads, provider result fetches).
pub fn shared_client() -> Result<reqwest::Client, String> {
    cached(false)
}

fn limit_label(limit: usize) -> String {
    if limit >= 1024 * 1024 {
        format!("{} MiB", limit / 1024 / 1024)
    } else {
        format!("{} KiB", limit / 1024)
    }
}

/// Read a response body into memory, aborting as soon as the accumulated size
/// exceeds `limit`.
///
/// `Response::bytes()` buffers the whole body first, so a chunked response
/// without a Content-Length header could exhaust memory before any size check
/// ran.
pub async fn read_limited_bytes(
    mut response: reqwest::Response,
    limit: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!("{label}超过大小限制（{}）", limit_label(limit)));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("读取{label}失败: {error}"))?
    {
        if body.len() + chunk.len() > limit {
            return Err(format!("{label}超过大小限制（{}）", limit_label(limit)));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::{client_for_url, is_loopback_url, shared_client};

    #[test]
    fn detects_loopback_hosts() {
        assert!(is_loopback_url("http://127.0.0.1:11434/v1/chat/completions"));
        assert!(is_loopback_url("http://localhost:8080/v1"));
        assert!(is_loopback_url("http://[::1]:8080/v1"));
        assert!(!is_loopback_url("https://api.example.com/v1"));
        assert!(!is_loopback_url("not a url"));
    }

    #[test]
    fn builds_clients_for_each_proxy_policy() {
        assert!(client_for_url("https://api.example.com/v1").is_ok());
        assert!(client_for_url("https://other.example.com/v1").is_ok());
        assert!(client_for_url("http://127.0.0.1:9/v1").is_ok());
        assert!(shared_client().is_ok());
    }
}
