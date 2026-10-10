use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

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

/// Why a body read stopped early. A transfer failure may succeed on a retry;
/// a body over the size limit never will.
#[derive(Debug)]
pub enum BodyReadError {
    TooLarge(String),
    Transfer(String),
}

impl BodyReadError {
    pub fn message(self) -> String {
        match self {
            Self::TooLarge(message) | Self::Transfer(message) => message,
        }
    }
}

/// Append a response body to `body`, aborting as soon as the accumulated size
/// exceeds `limit`.
///
/// With `idle_timeout`, only silence between chunks ends the read, so a slow
/// transfer that keeps making progress is never cut off. Bytes received
/// before a failure stay in `body`, which lets a caller resume with a Range
/// request. `on_progress` sees the accumulated length after every chunk.
pub async fn append_limited_bytes(
    response: &mut reqwest::Response,
    body: &mut Vec<u8>,
    limit: usize,
    label: &str,
    idle_timeout: Option<Duration>,
    on_progress: &mut (dyn FnMut(usize) + Send),
) -> Result<(), BodyReadError> {
    let too_large = || BodyReadError::TooLarge(format!("{label}超过大小限制（{}）", limit_label(limit)));
    if response
        .content_length()
        .is_some_and(|length| body.len() as u64 + length > limit as u64)
    {
        return Err(too_large());
    }
    loop {
        let next = match idle_timeout {
            Some(idle) => match tokio::time::timeout(idle, response.chunk()).await {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(_) => Err(format!("超过 {} 秒没有收到新数据", idle.as_secs())),
            },
            None => response.chunk().await.map_err(|error| error.to_string()),
        };
        let Some(chunk) =
            next.map_err(|error| BodyReadError::Transfer(format!("读取{label}失败: {error}")))?
        else {
            return Ok(());
        };
        if body.len() + chunk.len() > limit {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
        on_progress(body.len());
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
    let mut body = Vec::new();
    append_limited_bytes(&mut response, &mut body, limit, label, None, &mut |_| {})
        .await
        .map_err(BodyReadError::message)?;
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
