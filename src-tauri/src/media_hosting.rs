//! Explicit, user-initiated publishing of approved local image and video assets.
//!
//! This module intentionally owns neither application configuration nor a background
//! worker.  The only network operation is `asset_publish_media`, which is invoked by
//! the UI after a user has selected the media to publish.

use std::{net::IpAddr, path::PathBuf, time::Duration};

use reqwest::{header, multipart, redirect::Policy, Client, Url};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    comic_markdown,
    db::{self, DbState},
};

const SETTINGS_KEY: &str = "media_hosting.v1";
const MAX_SOURCE_COUNT: usize = 8;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    Bearer,
    Raw,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaHostingSaveInput {
    pub endpoint: String,
    pub file_field: String,
    pub url_field: String,
    pub auth_mode: AuthMode,
    pub token: Option<String>,
    #[serde(default)]
    pub clear_token: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaHostingConfig {
    pub endpoint: String,
    pub file_field: String,
    pub url_field: String,
    pub auth_mode: AuthMode,
    pub has_token: bool,
    pub configured: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPublishMediaInput {
    pub project_id: String,
    pub expected_endpoint: String,
    pub sources: Vec<AssetPublishMediaSource>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPublishMediaSource {
    pub asset_id: Option<String>,
    pub source_uri: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPublishMediaResult {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPublishMediaOutput {
    pub results: Vec<AssetPublishMediaResult>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredMediaHostingConfig {
    endpoint: String,
    file_field: String,
    url_field: String,
    auth_mode: AuthMode,
    token: String,
}

#[derive(Clone)]
struct ResolvedSource {
    key: String,
    file_name: String,
    mime: &'static str,
    bytes: Vec<u8>,
}

fn default_stored_config() -> StoredMediaHostingConfig {
    StoredMediaHostingConfig {
        endpoint: String::new(),
        file_field: "file".into(),
        url_field: "url".into(),
        auth_mode: AuthMode::Bearer,
        token: String::new(),
    }
}

fn public_config(config: &StoredMediaHostingConfig) -> MediaHostingConfig {
    MediaHostingConfig {
        endpoint: config.endpoint.clone(),
        file_field: config.file_field.clone(),
        url_field: config.url_field.clone(),
        auth_mode: config.auth_mode,
        has_token: !config.token.is_empty(),
        configured: validate_stored_config(config).is_ok(),
    }
}

fn stored_config(db: &DbState) -> Result<StoredMediaHostingConfig, String> {
    db::with_connection(db, |connection| {
        let raw: Option<String> = connection
            .query_row(
                "SELECT value FROM app_secrets WHERE key=?",
                [SETTINGS_KEY],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("读取媒体托管设置失败: {error}"))?;
        match raw {
            None => Ok(default_stored_config()),
            Some(raw) => {
                serde_json::from_str(&raw).map_err(|_| "媒体托管设置无效，请重新保存".to_string())
            }
        }
    })
}

fn save_config_inner(
    db: &DbState,
    input: MediaHostingSaveInput,
) -> Result<MediaHostingConfig, String> {
    let previous = stored_config(db)?;
    let endpoint = input.endpoint.trim().to_string();
    let endpoint_changed_host = match (endpoint_host(&previous.endpoint), endpoint_host(&endpoint))
    {
        (Some(before), Some(after)) => !before.eq_ignore_ascii_case(&after),
        _ => previous.endpoint.trim() != endpoint,
    };
    let mut token = previous.token;
    if input.clear_token
        || (endpoint_changed_host && input.token.as_deref().is_none_or(str::is_empty))
    {
        token.clear();
    }
    if let Some(value) = input.token.filter(|value| !value.is_empty()) {
        token = value;
    }
    let config = StoredMediaHostingConfig {
        endpoint,
        file_field: input.file_field.trim().to_string(),
        url_field: input.url_field.trim().to_string(),
        auth_mode: input.auth_mode,
        token,
    };
    validate_stored_config(&config)?;
    let serialized =
        serde_json::to_string(&config).map_err(|_| "保存媒体托管设置失败".to_string())?;
    db::with_connection(db, |connection| {
        connection
            .execute(
                "INSERT INTO app_secrets(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                rusqlite::params![SETTINGS_KEY, serialized],
            )
            .map_err(|error| format!("保存媒体托管设置失败: {error}"))?;
        Ok(())
    })?;
    Ok(public_config(&config))
}

#[tauri::command(async)]
pub fn media_hosting_get(db: tauri::State<'_, DbState>) -> Result<MediaHostingConfig, String> {
    Ok(public_config(&stored_config(&db)?))
}

#[tauri::command(async)]
pub fn media_hosting_save(
    db: tauri::State<'_, DbState>,
    input: MediaHostingSaveInput,
) -> Result<MediaHostingConfig, String> {
    save_config_inner(&db, input)
}

#[tauri::command]
pub async fn asset_publish_media(
    db: tauri::State<'_, DbState>,
    input: AssetPublishMediaInput,
) -> Result<AssetPublishMediaOutput, String> {
    asset_publish_media_inner(&db, input).await
}

async fn asset_publish_media_inner(
    db: &DbState,
    input: AssetPublishMediaInput,
) -> Result<AssetPublishMediaOutput, String> {
    let config = stored_config(db)?;
    validate_stored_config(&config)?;
    if input.expected_endpoint != config.endpoint {
        return Err("媒体托管配置已变更，请重新确认后再发布".into());
    }
    let sources = resolve_sources(db, &input)?;
    publish_resolved(&config, sources, false).await
}

fn resolve_sources(
    db: &DbState,
    input: &AssetPublishMediaInput,
) -> Result<Vec<ResolvedSource>, String> {
    if input.project_id.trim().is_empty() {
        return Err("项目不能为空".into());
    }
    if input.sources.is_empty() || input.sources.len() > MAX_SOURCE_COUNT {
        return Err("每次只能发布 1 至 8 个媒体文件".into());
    }
    let catalog = input
        .sources
        .iter()
        .any(|source| {
            source
                .source_uri
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        })
        .then(|| {
            db::with_connection(db, |connection| {
                comic_markdown::catalog_list(connection, &input.project_id)
            })
        })
        .transpose()?;
    let mut total = 0_u64;
    let mut resolved = Vec::with_capacity(input.sources.len());
    for source in &input.sources {
        let has_asset = source
            .asset_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let has_uri = source
            .source_uri
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        if has_asset == has_uri {
            return Err("每个媒体来源必须且只能提供 assetId 或 sourceUri".into());
        }
        let (key, raw_path, kind, format) = if let Some(asset_id) = source
            .asset_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
        {
            db::with_connection(db, |connection| {
                let found: Option<(String, String, Option<String>, Option<String>)> = connection
                    .query_row(
                        "SELECT kind,path,metadata,format FROM assets WHERE id=?",
                        [asset_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .optional()
                    .map_err(|_| "读取媒体资产失败".to_string())?;
                let Some((kind, path, metadata, format)) = found else {
                    return Err("媒体资产不存在".into());
                };
                let project_matches = metadata
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                    .and_then(|value| {
                        value
                            .get("projectId")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .as_deref()
                    == Some(input.project_id.as_str());
                if !project_matches {
                    return Err("媒体资产不属于当前项目".into());
                }
                Ok((format!("asset:{asset_id}"), path, kind, format))
            })?
        } else {
            let source_uri = source.source_uri.as_deref().unwrap().trim();
            let entry = catalog
                .as_ref()
                .expect("catalog is loaded when sourceUri is present")
                .iter()
                .find(|entry| entry.source_uri == source_uri)
                .ok_or("漫画媒体来源不存在或不属于当前项目")?;
            let path = entry.path.clone().ok_or("漫画来源不是可发布的媒体")?;
            (
                format!("comic:{source_uri}"),
                path,
                entry.kind.clone(),
                None,
            )
        };
        if !matches!(kind.as_str(), "image" | "video") {
            return Err("只能发布图片或视频资产".into());
        }
        let path = PathBuf::from(raw_path)
            .canonicalize()
            .map_err(|_| "媒体文件不存在或不可访问")?;
        let metadata = std::fs::metadata(&path).map_err(|_| "读取媒体文件失败")?;
        if !metadata.is_file() {
            return Err("媒体来源必须是普通文件".into());
        }
        let remaining = MAX_TOTAL_BYTES
            .checked_sub(total)
            .ok_or("媒体文件总大小超出限制")?;
        if metadata.len() > remaining {
            return Err("媒体文件总大小不能超过 512 MiB".into());
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .map_err(|_| "读取媒体文件失败")?
            .take(remaining.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| "读取媒体文件失败")?;
        if u64::try_from(bytes.len()).map_err(|_| "媒体文件总大小超出限制")? > remaining
        {
            return Err("媒体文件总大小不能超过 512 MiB".into());
        }
        total = total
            .checked_add(u64::try_from(bytes.len()).map_err(|_| "媒体文件总大小超出限制")?)
            .ok_or("媒体文件总大小超出限制")?;
        if total > MAX_TOTAL_BYTES {
            return Err("媒体文件总大小不能超过 512 MiB".into());
        }
        let (file_name, extension) = media_file_name_and_extension(&path, format.as_deref())?;
        let mime = match kind.as_str() {
            "image" if crate::assets::is_valid_image_bytes(&bytes) => image_mime(&extension),
            "video" if crate::assets::is_valid_video_bytes(&bytes, &extension) => {
                video_mime(&extension)
            }
            _ => None,
        }
        .ok_or("媒体文件格式与资产类型不匹配")?;
        resolved.push(ResolvedSource {
            key,
            file_name,
            mime,
            bytes,
        });
    }
    Ok(resolved)
}

async fn publish_resolved(
    config: &StoredMediaHostingConfig,
    sources: Vec<ResolvedSource>,
    allow_test_endpoint: bool,
) -> Result<AssetPublishMediaOutput, String> {
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(REQUEST_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| "创建媒体托管请求失败".to_string())?;
    let mut results = Vec::with_capacity(sources.len());
    for source in sources {
        let key = source.key.clone();
        match upload_one(&client, config, source, allow_test_endpoint).await {
            Ok((url, sha256)) => results.push(AssetPublishMediaResult {
                key,
                url: Some(url),
                sha256: Some(sha256),
                error: None,
            }),
            Err(error) => results.push(AssetPublishMediaResult {
                key,
                url: None,
                sha256: None,
                error: Some(error),
            }),
        }
    }
    Ok(AssetPublishMediaOutput { results })
}

async fn upload_one(
    client: &Client,
    config: &StoredMediaHostingConfig,
    source: ResolvedSource,
    allow_test_endpoint: bool,
) -> Result<(String, String), String> {
    if !allow_test_endpoint {
        validate_public_https_url(&config.endpoint).map_err(|_| "媒体托管端点无效")?;
    }
    let sha256 = format!("{:x}", Sha256::digest(&source.bytes));
    let part = multipart::Part::bytes(source.bytes)
        .file_name(source.file_name)
        .mime_str(source.mime)
        .map_err(|_| "媒体文件格式无效")?;
    let mut request = client
        .post(&config.endpoint)
        .multipart(multipart::Form::new().part(config.file_field.clone(), part));
    if !config.token.is_empty() {
        let value = match config.auth_mode {
            AuthMode::Bearer => format!("Bearer {}", config.token),
            AuthMode::Raw => config.token.clone(),
        };
        let value = header::HeaderValue::from_str(&value).map_err(|_| "媒体托管凭据无效")?;
        request = request.header(header::AUTHORIZATION, value);
    }
    let response = request.send().await.map_err(|_| "媒体托管请求失败")?;
    if !response.status().is_success() {
        return Err("媒体托管服务返回失败状态".into());
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("媒体托管响应过大".into());
    }
    let mut response = response;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "读取媒体托管响应失败")? {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("媒体托管响应过大".into());
        }
        body.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&body).map_err(|_| "媒体托管响应格式无效")?;
    let url = json_dot_string(&value, &config.url_field).ok_or("媒体托管响应未包含媒体地址")?;
    validate_public_https_url(url).map_err(|_| "媒体托管返回了无效地址")?;
    Ok((url.to_string(), sha256))
}

fn validate_stored_config(config: &StoredMediaHostingConfig) -> Result<(), String> {
    validate_public_https_url(&config.endpoint)
        .map_err(|_| "媒体托管端点必须是公网 HTTPS 地址，且不能包含用户信息")?;
    validate_form_field(&config.file_field, "文件字段")?;
    validate_dot_path(&config.url_field, "地址字段")?;
    Ok(())
}

fn validate_form_field(field: &str, label: &str) -> Result<(), String> {
    if field.is_empty()
        || field.len() > 128
        || !field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(format!("{label}无效"));
    }
    Ok(())
}

fn validate_dot_path(path: &str, label: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 256
        || path.split('.').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err(format!("{label}无效"));
    }
    Ok(())
}

fn endpoint_host(endpoint: &str) -> Option<String> {
    Url::parse(endpoint.trim())
        .ok()?
        .host()
        .map(|host| host.to_string())
}

fn validate_public_https_url(raw: &str) -> Result<(), ()> {
    let url = Url::parse(raw.trim()).map_err(|_| ())?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err(());
    }
    let host = url.host().ok_or(())?.to_string();
    let lowered = host.to_ascii_lowercase();
    if lowered == "localhost"
        || lowered.ends_with(".localhost")
        || lowered.ends_with(".local")
        || lowered.ends_with(".internal")
        || lowered.ends_with(".lan")
        || !lowered.contains('.')
    {
        return Err(());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(ip)
                if ip.is_private()
                    || ip.is_loopback()
                    || ip.is_link_local()
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || ip.is_broadcast()
                    || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])) =>
            {
                return Err(())
            }
            IpAddr::V6(ip)
                if ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || ip.is_unique_local()
                    || ip.is_unicast_link_local() =>
            {
                return Err(())
            }
            _ => {}
        }
    }
    Ok(())
}

fn media_file_name_and_extension(
    path: &std::path::Path,
    declared: Option<&str>,
) -> Result<(String, String), String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("媒体文件名无效")?
        .to_string();
    let extension = declared
        .or_else(|| path.extension().and_then(|extension| extension.to_str()))
        .unwrap_or_default()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    Ok((file_name, extension))
}

fn image_mime(extension: &str) -> Option<&'static str> {
    match extension {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

fn video_mime(extension: &str) -> Option<&'static str> {
    match extension {
        "mp4" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "mov" => Some("video/quicktime"),
        _ => None,
    }
}

fn json_dot_string<'a>(value: &'a Value, path: &str) -> Option<&'a str> {
    path.split('.')
        .try_fold(value, |current, segment| current.get(segment))?
        .as_str()
}

#[cfg(test)]
mod tests {
    use std::{
        future::IntoFuture,
        sync::{Arc, Mutex},
    };

    use axum::{
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };

    use super::*;

    fn test_db() -> (DbState, PathBuf) {
        let root = std::env::temp_dir().join(format!("media-hosting-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = DbState::open(root.join("test.db")).unwrap();
        (db, root)
    }

    fn input(endpoint: &str) -> MediaHostingSaveInput {
        MediaHostingSaveInput {
            endpoint: endpoint.into(),
            file_field: "file".into(),
            url_field: "payload.url".into(),
            auth_mode: AuthMode::Bearer,
            token: Some("secret-token".into()),
            clear_token: false,
        }
    }

    #[test]
    fn public_urls_reject_http_local_and_userinfo() {
        for value in [
            "http://example.com/upload",
            "https://localhost/upload",
            "https://127.0.0.1/upload",
            "https://[::1]/upload",
            "https://100.64.0.1/upload",
            "https://user@example.com/upload",
        ] {
            assert!(validate_public_https_url(value).is_err(), "{value}");
        }
        assert!(validate_public_https_url("https://cdn.example.com/upload").is_ok());
    }

    #[tokio::test]
    async fn production_entry_rejects_missing_config_cross_project_http_and_local() {
        let (db, root) = test_db();
        let file = root.join("image.png");
        std::fs::write(&file, b"image").unwrap();
        let publish = || AssetPublishMediaInput {
            project_id: "project-a".into(),
            expected_endpoint: "https://upload.example.com/media".into(),
            sources: vec![AssetPublishMediaSource {
                asset_id: Some("asset-a".into()),
                source_uri: None,
            }],
        };
        assert!(asset_publish_media_inner(&db, publish()).await.is_err());
        db::with_connection(&db, |connection| {
            connection.execute("INSERT INTO assets(id,kind,path,created_at,metadata) VALUES('asset-a','image',?,?,?)", rusqlite::params![file.display().to_string(), 1_i64, r#"{"projectId":"project-b"}"#]).map_err(|error| error.to_string())?;
            Ok(())
        }).unwrap();
        save_config_inner(&db, input("https://upload.example.com/media")).unwrap();
        assert!(asset_publish_media_inner(&db, publish()).await.is_err());
        db::with_connection(&db, |connection| {
            connection.execute("UPDATE assets SET metadata=? WHERE id='asset-a'", [r#"{"projectId":"project-a"}"#]).map_err(|error| error.to_string())?;
            connection.execute("UPDATE app_secrets SET value=? WHERE key=?", rusqlite::params![r#"{"endpoint":"http://example.com/upload","fileField":"file","urlField":"url","authMode":"bearer","token":"x"}"#, SETTINGS_KEY]).map_err(|error| error.to_string())?;
            Ok(())
        }).unwrap();
        assert!(asset_publish_media_inner(&db, publish()).await.is_err());
        db::with_connection(&db, |connection| {
            connection.execute("UPDATE app_secrets SET value=? WHERE key=?", rusqlite::params![r#"{"endpoint":"https://localhost/upload","fileField":"file","urlField":"url","authMode":"bearer","token":"x"}"#, SETTINGS_KEY]).map_err(|error| error.to_string())?;
            Ok(())
        }).unwrap();
        assert!(asset_publish_media_inner(&db, publish()).await.is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn loopback_transport_sends_multipart_accepts_public_url_and_keeps_token_private() {
        #[derive(Clone)]
        struct Capture(Arc<Mutex<Option<(String, String)>>>);
        async fn handler(
            State(capture): State<Capture>,
            headers: HeaderMap,
            body: Bytes,
        ) -> (StatusCode, &'static str) {
            *capture.0.lock().unwrap() = Some((
                headers
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string(),
                String::from_utf8_lossy(&body).to_string(),
            ));
            (
                StatusCode::CREATED,
                r#"{"payload":{"url":"https://cdn.example.com/a.png"}}"#,
            )
        }
        let capture = Capture(Arc::new(Mutex::new(None)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/", post(handler))
                    .with_state(capture.clone()),
            )
            .into_future(),
        );
        let path =
            std::env::temp_dir().join(format!("media-hosting-upload-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"test-image").unwrap();
        let config = StoredMediaHostingConfig {
            endpoint,
            file_field: "media".into(),
            url_field: "payload.url".into(),
            auth_mode: AuthMode::Bearer,
            token: "secret-token".into(),
        };
        let output = publish_resolved(
            &config,
            vec![ResolvedSource {
                key: "asset:a".into(),
                file_name: "upload.png".into(),
                mime: "image/png",
                bytes: b"test-image".to_vec(),
            }],
            true,
        )
        .await
        .unwrap();
        let sent = capture.0.lock().unwrap().clone().unwrap();
        assert_eq!(sent.0, "Bearer secret-token");
        assert!(sent.1.contains("name=\"media\""));
        assert_eq!(
            output.results[0].url.as_deref(),
            Some("https://cdn.example.com/a.png")
        );
        assert!(!serde_json::to_string(&output)
            .unwrap()
            .contains("secret-token"));
        server.abort();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn loopback_transport_returns_partial_results_and_rejects_non_public_response_url() {
        #[derive(Clone)]
        struct Calls(Arc<Mutex<usize>>);
        async fn handler(State(calls): State<Calls>) -> (StatusCode, &'static str) {
            let mut call = calls.0.lock().unwrap();
            *call += 1;
            if *call == 1 {
                (
                    StatusCode::OK,
                    r#"{"url":"https://cdn.example.com/ok.png"}"#,
                )
            } else {
                (StatusCode::OK, r#"{"url":"https://[::1]/nope.png"}"#)
            }
        }
        let calls = Calls(Arc::new(Mutex::new(0)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new().route("/", post(handler)).with_state(calls),
            )
            .into_future(),
        );
        let path = std::env::temp_dir().join(format!(
            "media-hosting-partial-{}.png",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"test-image").unwrap();
        let config = StoredMediaHostingConfig {
            endpoint,
            file_field: "file".into(),
            url_field: "url".into(),
            auth_mode: AuthMode::Raw,
            token: String::new(),
        };
        let output = publish_resolved(
            &config,
            vec![
                ResolvedSource {
                    key: "asset:1".into(),
                    file_name: "partial.png".into(),
                    mime: "image/png",
                    bytes: b"test-image".to_vec(),
                },
                ResolvedSource {
                    key: "asset:2".into(),
                    file_name: "partial.png".into(),
                    mime: "image/png",
                    bytes: b"test-image".to_vec(),
                },
            ],
            true,
        )
        .await
        .unwrap();
        assert!(output.results[0].url.is_some());
        assert_eq!(
            output.results[1].error.as_deref(),
            Some("媒体托管返回了无效地址")
        );
        server.abort();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn production_entry_rejects_an_endpoint_that_changed_after_ui_preview() {
        let (db, root) = test_db();
        save_config_inner(&db, input("https://upload.example.com/current")).unwrap();
        let output = asset_publish_media_inner(
            &db,
            AssetPublishMediaInput {
                project_id: "project-a".into(),
                expected_endpoint: "https://upload.example.com/previewed".into(),
                sources: vec![AssetPublishMediaSource {
                    asset_id: Some("not-read".into()),
                    source_uri: None,
                }],
            },
        )
        .await;
        assert!(matches!(
            output,
            Err(error) if error == "媒体托管配置已变更，请重新确认后再发布"
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn changed_endpoint_host_clears_empty_token_and_safe_status_never_returns_it() {
        let (db, root) = test_db();
        let saved = save_config_inner(&db, input("https://first.example.com/upload")).unwrap();
        assert!(saved.has_token);
        let mut changed = input("https://second.example.com/upload");
        changed.token = Some(String::new());
        let saved = save_config_inner(&db, changed).unwrap();
        assert!(!saved.has_token);
        assert!(!serde_json::to_string(&saved)
            .unwrap()
            .contains("secret-token"));
        let _ = std::fs::remove_dir_all(root);
    }
}
