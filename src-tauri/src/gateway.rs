use std::collections::HashSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant;

use base64::Engine;
use image::{imageops, DynamicImage, ImageFormat, Rgba, RgbaImage};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;

use crate::assets;
use crate::config::ConfigState;
use crate::model::{AssetRef, RunNodeRequest};
use crate::util::{get_str, get_u32};
use uuid::Uuid;

const MAX_IMAGE_RESPONSE_BYTES: usize = 100 * 1024 * 1024;
const MAX_DOWNLOADED_IMAGE_BYTES: usize = 50 * 1024 * 1024;
const MAX_REFERENCE_IMAGES: usize = 8;
const MAX_REFERENCE_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_REFERENCE_TOTAL_BYTES: usize = 50 * 1024 * 1024;
const REFERENCE_TILE_EDGE: u32 = 512;
const REFERENCE_TILE_GUTTER: u32 = 8;

const REFERENCE_ROLES: &[&str] = &[
    "character_identity",
    "outfit",
    "style",
    "pose",
    "scene",
    "prop",
    "previous_panel",
    "base_image",
    "mask",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReferenceInput {
    path: String,
    role: String,
    #[serde(default)]
    weight: Option<f64>,
    #[serde(default)]
    sort_order: Option<i64>,
}

#[derive(Clone, Copy)]
struct ReferenceLimits {
    max_count: usize,
    max_each_bytes: usize,
    max_total_bytes: usize,
}

const REFERENCE_LIMITS: ReferenceLimits = ReferenceLimits {
    max_count: MAX_REFERENCE_IMAGES,
    max_each_bytes: MAX_REFERENCE_IMAGE_BYTES,
    max_total_bytes: MAX_REFERENCE_TOTAL_BYTES,
};

#[derive(Clone, Debug)]
struct ValidatedReference {
    bytes: Vec<u8>,
    kind: String,
    role: String,
    weight: f64,
    sort_order: i64,
    input_order: usize,
}

struct ReferenceUpload {
    bytes: Vec<u8>,
    kind: String,
    file_name: String,
}

/// The multipart body owns its bytes, so this guard can remove the board when
/// the request completes or unwinds with an error.
struct TemporaryReferenceBoard {
    path: PathBuf,
}

impl Drop for TemporaryReferenceBoard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn size_str(config: &serde_json::Value) -> String {
    if let Some(s) = get_str(config, "size") {
        // Values look like "1024x1024 (1:1)" — keep only the WxH part.
        let base = s.split('(').next().unwrap_or(&s).trim();
        if base.contains('x') {
            return base.to_string();
        }
    }
    let w = get_u32(config, "width").unwrap_or(1024);
    let h = get_u32(config, "height").unwrap_or(1024);
    format!("{w}x{h}")
}

/// Generate an image through the OpenAI-compatible gateway.
/// - No reference image -> POST /v1/images/generations (JSON).
/// - Reference image set -> POST /v1/images/edits (multipart).
///
/// Config: prompt, size ("WxH"), quality (low/medium/high), background
/// (transparent/opaque/auto), legacy referencePath (local file), or
/// references[] ({ path, role, weight, sortOrder }).
pub async fn generate_image(
    cfg: &ConfigState,
    req: &RunNodeRequest,
    out_dir: &Path,
) -> Result<Vec<AssetRef>, String> {
    #[cfg(feature = "real-e2e-harness")]
    let generation_client = crate::harness_transport::generation_client()?;
    #[cfg(not(feature = "real-e2e-harness"))]
    let generation_client = reqwest::Client::new();
    // Asset URLs are fetched with a separate client.  Bearer authentication is
    // attached to generation POSTs below, never to these GET requests.
    let asset_download_client = reqwest::Client::new();
    generate_image_with_clients(
        cfg,
        req,
        out_dir,
        &generation_client,
        &asset_download_client,
    )
    .await
}

async fn generate_image_with_clients(
    cfg: &ConfigState,
    req: &RunNodeRequest,
    out_dir: &Path,
    generation_client: &reqwest::Client,
    asset_download_client: &reqwest::Client,
) -> Result<Vec<AssetRef>, String> {
    if cfg.image_api_url.is_empty() || cfg.image_api_key.is_empty() {
        return Err("未配置图像 API（请在设置中填写图像地址与 Key）".into());
    }

    let prompt = get_str(&req.config, "prompt").ok_or("缺少提示词 (prompt)")?;
    if prompt.trim().is_empty() {
        return Err("提示词为空".into());
    }
    let model = get_str(&req.config, "model").unwrap_or_else(|| cfg.image_model.clone());
    let size = size_str(&req.config);
    let quality = get_str(&req.config, "quality").unwrap_or_default();
    let background = get_str(&req.config, "background").unwrap_or_default();
    let references = normalize_references(&req.config, cfg)?;
    let has_reference = !references.is_empty();
    let request_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    crate::logging::info(
        "image.request.start",
        serde_json::json!({
            "requestId": request_id,
            "model": model,
            "endpoint": crate::logging::safe_url(&cfg.image_api_url),
            "mode": if has_reference { "image_to_image" } else { "text_to_image" },
            "size": size,
            "quality": quality,
            "background": background,
            "promptStats": crate::logging::text_stats(&prompt),
            "hasReference": has_reference,
            "referenceCount": references.len(),
            "referenceRoles": references.iter().map(|reference| reference.role.as_str()).collect::<Vec<_>>(),
        }),
    );

    if has_reference {
        let (upload, _temporary_board) = prepare_reference_upload(&references, out_dir)?;
        let mime = if upload.kind == "jpg" {
            "image/jpeg".to_string()
        } else {
            format!("image/{}", upload.kind)
        };
        let part = Part::bytes(upload.bytes)
            .file_name(upload.file_name)
            .mime_str(&mime)
            .map_err(|e| format!("构造上传失败: {e}"))?;
        let provider_prompt = prompt_with_reference_roles(&prompt, &references);
        let mut form = Form::new()
            .text("model", model.clone())
            .text("prompt", provider_prompt)
            .text("n", "1")
            .part("image", part);
        if !size.is_empty() {
            form = form.text("size", size.clone());
        }
        if !quality.is_empty() {
            form = form.text("quality", quality.clone());
        }
        if !background.is_empty() && background != "auto" {
            form = form.text("background", background.clone());
        }

        // "/v1/images/generations" -> "/v1/images/edits"
        let edits_url = edits_endpoint(&cfg.image_api_url)?;
        let send_started = Instant::now();
        #[cfg(feature = "real-e2e-harness")]
        crate::harness_transport::reserve_image_post()?;
        let resp = generation_client
            .post(&edits_url)
            .bearer_auth(&cfg.image_api_key)
            .multipart(form)
            .timeout(Duration::from_secs(240))
            .send()
            .await
            .map_err(|e| {
                crate::logging::error("image.request.end", serde_json::json!({ "requestId": request_id, "status": "error", "durationMs": started.elapsed().as_millis(), "error": e.to_string() }));
                format!("请求图像编辑接口失败: {e}")
            })?;
        #[cfg(feature = "real-e2e-harness")]
        crate::harness_transport::mark_image_http_received()?;
        crate::logging::debug(
            "image.response.headers",
            serde_json::json!({ "requestId": request_id, "durationMs": send_started.elapsed().as_millis(), "status": resp.status().as_u16() }),
        );
        finish_request(
            parse_and_save(resp, &asset_download_client, out_dir).await,
            &request_id,
            started,
        )
    } else {
        let mut body = serde_json::json!({ "model": model, "prompt": prompt, "n": 1 });
        if !size.is_empty() {
            body["size"] = serde_json::json!(size);
        }
        if !quality.is_empty() {
            body["quality"] = serde_json::json!(quality);
        }
        if !background.is_empty() && background != "auto" {
            body["background"] = serde_json::json!(background);
        }

        let send_started = Instant::now();
        #[cfg(feature = "real-e2e-harness")]
        crate::harness_transport::reserve_image_post()?;
        let resp = generation_client
            .post(&cfg.image_api_url)
            .bearer_auth(&cfg.image_api_key)
            .json(&body)
            .timeout(Duration::from_secs(240))
            .send()
            .await
            .map_err(|e| {
                crate::logging::error("image.request.end", serde_json::json!({ "requestId": request_id, "status": "error", "durationMs": started.elapsed().as_millis(), "error": e.to_string() }));
                format!("请求图像接口失败: {e}")
            })?;
        #[cfg(feature = "real-e2e-harness")]
        crate::harness_transport::mark_image_http_received()?;
        crate::logging::debug(
            "image.response.headers",
            serde_json::json!({ "requestId": request_id, "durationMs": send_started.elapsed().as_millis(), "status": resp.status().as_u16() }),
        );
        finish_request(
            parse_and_save(resp, &asset_download_client, out_dir).await,
            &request_id,
            started,
        )
    }
}

fn finish_request(
    result: Result<Vec<AssetRef>, String>,
    request_id: &str,
    started: Instant,
) -> Result<Vec<AssetRef>, String> {
    match &result {
        Ok(assets) => {
            let total_bytes: u64 = assets
                .iter()
                .filter_map(|asset| {
                    std::fs::metadata(&asset.path)
                        .ok()
                        .map(|metadata| metadata.len())
                })
                .sum();
            crate::logging::info(
                "image.request.end",
                serde_json::json!({ "requestId": request_id, "status": "success", "durationMs": started.elapsed().as_millis(), "assetCount": assets.len(), "totalBytes": total_bytes }),
            );
        }
        Err(error) => crate::logging::error(
            "image.request.end",
            serde_json::json!({ "requestId": request_id, "status": "error", "durationMs": started.elapsed().as_millis(), "error": crate::logging::error_text(error) }),
        ),
    }
    result
}

fn normalize_references(
    config: &serde_json::Value,
    cfg: &ConfigState,
) -> Result<Vec<ValidatedReference>, String> {
    let mut inputs = match config.get("references") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(_)) => serde_json::from_value::<Vec<ReferenceInput>>(
            config.get("references").cloned().unwrap_or_default(),
        )
        .map_err(|_| "references 必须是参考图对象数组")?,
        Some(_) => return Err("references 必须是参考图对象数组".into()),
    };

    // references[] is authoritative when present. This lets older callers keep
    // sending referencePath while newer callers migrate without uploading the
    // same file twice. An empty references[] still falls back to legacy input.
    if inputs.is_empty() {
        if let Some(path) = get_str(config, "referencePath") {
            if let Some(path) = non_empty(&path) {
                inputs.push(ReferenceInput {
                    path: path.to_string(),
                    role: "base_image".into(),
                    weight: Some(1.0),
                    sort_order: Some(0),
                });
            }
        }
    }

    validate_reference_inputs(&inputs, cfg, REFERENCE_LIMITS)
}

fn validate_reference_inputs(
    inputs: &[ReferenceInput],
    cfg: &ConfigState,
    limits: ReferenceLimits,
) -> Result<Vec<ValidatedReference>, String> {
    if inputs.len() > limits.max_count {
        return Err(format!("参考图最多支持 {} 张", limits.max_count));
    }

    let mut total_bytes = 0usize;
    let mut paths = HashSet::new();
    let mut references = Vec::with_capacity(inputs.len());
    for (input_order, input) in inputs.iter().enumerate() {
        let role = input.role.trim();
        if !REFERENCE_ROLES.contains(&role) {
            return Err("参考图 role 无效".into());
        }
        let weight = input.weight.unwrap_or(1.0);
        if !weight.is_finite() || !(0.0..=1.0).contains(&weight) || weight == 0.0 {
            return Err("参考图 weight 必须在 0 到 1 之间".into());
        }
        let sort_order = input.sort_order.unwrap_or(input_order as i64);
        if sort_order < 0 {
            return Err("参考图 sortOrder 必须为非负整数".into());
        }

        let canonical = validate_reference_path(input.path.trim(), cfg)?;
        if !paths.insert(canonical.clone()) {
            return Err("references 不允许重复引用同一文件".into());
        }
        let metadata = std::fs::metadata(&canonical).map_err(|_| "参考图文件不可访问")?;
        if !metadata.is_file() {
            return Err("参考图必须是普通文件".into());
        }
        let declared_bytes = usize::try_from(metadata.len()).map_err(|_| "参考图文件过大")?;
        if declared_bytes > limits.max_each_bytes {
            return Err(format!(
                "单张参考图不能超过 {} MiB",
                limits.max_each_bytes / 1024 / 1024
            ));
        }
        let bytes = std::fs::read(&canonical).map_err(|_| "读取参考图失败")?;
        if bytes.len() > limits.max_each_bytes {
            return Err(format!(
                "单张参考图不能超过 {} MiB",
                limits.max_each_bytes / 1024 / 1024
            ));
        }
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .ok_or("参考图总大小超过限制")?;
        if total_bytes > limits.max_total_bytes {
            return Err(format!(
                "参考图总大小不能超过 {} MiB",
                limits.max_total_bytes / 1024 / 1024
            ));
        }
        let kind = assets::detect_format_checked(&bytes)
            .ok_or("参考图格式不受支持或文件已损坏")?
            .to_string();
        references.push(ValidatedReference {
            bytes,
            kind,
            role: role.to_string(),
            weight,
            sort_order,
            input_order,
        });
    }
    references.sort_by(|left, right| {
        left.sort_order
            .cmp(&right.sort_order)
            .then_with(|| left.role.cmp(&right.role))
            .then_with(|| left.input_order.cmp(&right.input_order))
    });
    Ok(references)
}

fn prepare_reference_upload(
    references: &[ValidatedReference],
    out_dir: &Path,
) -> Result<(ReferenceUpload, Option<TemporaryReferenceBoard>), String> {
    let reference = references.first().ok_or("缺少参考图")?;
    if references.len() == 1 {
        return Ok((
            ReferenceUpload {
                bytes: reference.bytes.clone(),
                kind: reference.kind.clone(),
                file_name: format!("ref.{}", reference.kind),
            },
            None,
        ));
    }

    let columns = match references.len() {
        2..=4 => 2,
        _ => 3,
    };
    let rows = (references.len() as u32).div_ceil(columns);
    let board_width = columns * REFERENCE_TILE_EDGE + (columns + 1) * REFERENCE_TILE_GUTTER;
    let board_height = rows * REFERENCE_TILE_EDGE + (rows + 1) * REFERENCE_TILE_GUTTER;
    let mut board = RgbaImage::from_pixel(board_width, board_height, Rgba([242, 242, 242, 255]));

    for (index, reference) in references.iter().enumerate() {
        let source = image::load_from_memory(&reference.bytes)
            .map_err(|_| "参考图格式不受支持或文件已损坏")?;
        place_reference_tile(&mut board, source, index as u32, columns);
    }

    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(board)
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|_| "合成参考图板失败")?;
    let bytes = bytes.into_inner();
    std::fs::create_dir_all(out_dir).map_err(|_| "创建参考图临时目录失败")?;
    let path = out_dir.join(format!(".reference-board-{}.png", Uuid::new_v4()));
    std::fs::write(&path, &bytes).map_err(|_| "写入参考图临时板失败")?;

    Ok((
        ReferenceUpload {
            bytes,
            kind: "png".into(),
            file_name: "reference-board.png".into(),
        },
        Some(TemporaryReferenceBoard { path }),
    ))
}

fn place_reference_tile(board: &mut RgbaImage, source: DynamicImage, index: u32, columns: u32) {
    let tile = source
        .thumbnail(REFERENCE_TILE_EDGE, REFERENCE_TILE_EDGE)
        .to_rgba8();
    let column = index % columns;
    let row = index / columns;
    let origin_x = REFERENCE_TILE_GUTTER + column * (REFERENCE_TILE_EDGE + REFERENCE_TILE_GUTTER);
    let origin_y = REFERENCE_TILE_GUTTER + row * (REFERENCE_TILE_EDGE + REFERENCE_TILE_GUTTER);
    let x = origin_x + (REFERENCE_TILE_EDGE - tile.width()) / 2;
    let y = origin_y + (REFERENCE_TILE_EDGE - tile.height()) / 2;
    imageops::overlay(board, &tile, i64::from(x), i64::from(y));
}

fn prompt_with_reference_roles(prompt: &str, references: &[ValidatedReference]) -> String {
    if references.len() <= 1 {
        return prompt.to_string();
    }
    let mapping = references
        .iter()
        .enumerate()
        .map(|(index, reference)| {
            format!(
                "tile {} = {} (weight {:.3})",
                index + 1,
                reference.role,
                reference.weight
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{prompt}\n\nReference board responsibility map: {mapping}. Use each tile only for its named responsibility; preserve identity and style consistency."
    )
}

fn non_empty(s: &str) -> Option<&str> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s.trim())
    }
}

/// 参考图必须位于应用资产目录或配置的输出目录内，防止读取任意本地文件。
/// 同时覆盖 Tauri run_node 与 REST /media/images 两条入口（都走 generate_image）。
fn validate_reference_path(
    ref_path: &str,
    cfg: &ConfigState,
) -> Result<std::path::PathBuf, String> {
    let canonical =
        std::fs::canonicalize(ref_path).map_err(|e| format!("参考图文件不存在或不可访问: {e}"))?;
    let allowed_roots = [crate::paths::assets_dir(), cfg.output_path()];
    let allowed = allowed_roots.iter().any(|root| {
        root.canonicalize()
            .ok()
            .is_some_and(|canonical_root| canonical.starts_with(&canonical_root))
    });
    if !allowed {
        return Err("参考图必须位于应用资产目录或输出目录".into());
    }
    Ok(canonical)
}

async fn parse_and_save(
    resp: reqwest::Response,
    client: &reqwest::Client,
    out_dir: &Path,
) -> Result<Vec<AssetRef>, String> {
    if !resp.status().is_success() {
        let status = resp.status();
        let text = String::from_utf8_lossy(
            &read_limited(resp, 1024 * 1024, "图像接口错误响应")
                .await
                .unwrap_or_default(),
        )
        .into_owned();
        return Err(format!(
            "图像接口返回 {status}: {}",
            text.chars().take(400).collect::<String>()
        ));
    }

    let response_bytes = read_limited(resp, MAX_IMAGE_RESPONSE_BYTES, "图像接口响应").await?;
    let json: serde_json::Value =
        serde_json::from_slice(&response_bytes).map_err(|e| format!("解析图像响应失败: {e}"))?;

    let first = json
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .ok_or("图像响应缺少 data")?;

    let bytes: Vec<u8> = if let Some(b64) = first.get("b64_json").and_then(|b| b.as_str()) {
        if b64.is_empty() {
            return Err("模型未返回图像数据（可能不支持该参数，如透明背景）".into());
        }
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("base64 解码失败: {e}"))?
    } else if let Some(url) = first.get("url").and_then(|u| u.as_str()) {
        let download_started = Instant::now();
        let r = client
            .get(url)
            .timeout(Duration::from_secs(240))
            .send()
            .await
            .map_err(|e| format!("下载图片失败: {e}"))?;
        if !r.status().is_success() {
            return Err(format!("下载图片失败: {}", r.status()));
        }
        let bytes = read_limited(r, MAX_DOWNLOADED_IMAGE_BYTES, "图片下载").await?;
        crate::logging::info(
            "image.download",
            serde_json::json!({ "durationMs": download_started.elapsed().as_millis(), "bytes": bytes.len(), "source": crate::logging::safe_url(url) }),
        );
        bytes
    } else {
        return Err("图像响应中没有 b64_json 或 url".into());
    };

    if bytes.is_empty() {
        return Err("模型未返回图像数据".into());
    }

    let format =
        assets::detect_format_checked(&bytes).ok_or("图像接口返回的内容不是支持的图片格式")?;
    let asset = assets::save_bytes(out_dir, "image", &bytes, format)?;
    Ok(vec![asset])
}

async fn read_limited(
    response: reqwest::Response,
    limit: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!(
            "{label}超过大小限制（{} MiB）",
            limit / 1024 / 1024
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("读取{label}失败: {error}"))?;
    if bytes.len() > limit {
        return Err(format!(
            "{label}超过大小限制（{} MiB）",
            limit / 1024 / 1024
        ));
    }
    Ok(bytes.to_vec())
}

fn edits_endpoint(api_url: &str) -> Result<String, String> {
    let mut url =
        reqwest::Url::parse(api_url).map_err(|error| format!("图像 API 地址无效: {error}"))?;
    let path = url.path().to_string();
    let prefix = path
        .strip_suffix("/generations")
        .ok_or("图生图要求图像 API 地址以 /generations 结尾")?;
    url.set_path(&format!("{prefix}/edits"));
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use super::{
        edits_endpoint, generate_image_with_clients, normalize_references,
        prepare_reference_upload, prompt_with_reference_roles, validate_reference_inputs,
        validate_reference_path, ReferenceInput, ReferenceLimits,
    };
    use crate::config::ConfigState;
    use crate::model::RunNodeRequest;
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
        time::timeout,
    };

    struct GatewayLoopback {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
        task: Option<JoinHandle<()>>,
    }

    impl GatewayLoopback {
        async fn start(asset_bytes: Vec<u8>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&requests);
            let task = tokio::spawn(async move {
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    loop {
                        let mut chunk = [0u8; 2048];
                        let read = stream.read(&mut chunk).await.unwrap();
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..read]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let header_end = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .unwrap()
                        + 4;
                    let head = String::from_utf8_lossy(&request[..header_end]).into_owned();
                    let content_length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("Content-Length: "))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while request.len() < header_end + content_length {
                        let mut chunk = [0u8; 2048];
                        let read = stream.read(&mut chunk).await.unwrap();
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..read]);
                    }
                    let is_get = head.starts_with("GET ");
                    captured.lock().unwrap().push(head);
                    if is_get {
                        let headers = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            asset_bytes.len()
                        );
                        stream.write_all(headers.as_bytes()).await.unwrap();
                        stream.write_all(&asset_bytes).await.unwrap();
                    } else {
                        let body =
                            format!("{{\"data\":[{{\"url\":\"http://{address}/asset.png\"}}]}}");
                        let headers = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        stream.write_all(headers.as_bytes()).await.unwrap();
                        stream.write_all(body.as_bytes()).await.unwrap();
                    }
                    stream.flush().await.unwrap();
                }
            });
            Self {
                url: format!("http://{address}/v1/images/generations"),
                requests,
                task: Some(task),
            }
        }

        async fn finish(mut self) -> Vec<String> {
            let mut task = self.task.take().unwrap();
            match timeout(Duration::from_secs(1), &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => panic!("gateway loopback task failed: {error}"),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    panic!("gateway loopback must finish");
                }
            }
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for GatewayLoopback {
        fn drop(&mut self) {
            if let Some(task) = self.task.take() {
                task.abort();
            }
        }
    }

    fn test_config(output_dir: &std::path::Path) -> ConfigState {
        ConfigState {
            image_api_url: String::new(),
            image_api_key: String::new(),
            image_model: "image".into(),
            video_api_url: String::new(),
            video_api_key: String::new(),
            video_model: "video".into(),
            llm_api_url: String::new(),
            llm_api_key: String::new(),
            llm_model: "llm".into(),
            output_dir: output_dir.display().to_string(),
            source: "none".into(),
        }
    }

    fn temp_root() -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("image-client-gateway-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_png(path: &std::path::Path, color: [u8; 4]) {
        RgbaImage::from_pixel(24, 16, Rgba(color))
            .save(path)
            .unwrap();
    }

    fn png_bytes(color: [u8; 4]) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 3, Rgba(color)))
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn loopback_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn generation_post_and_asset_get_are_separate_for_plain_and_reference_requests() {
        for has_reference in [false, true] {
            let root = temp_root();
            let server = GatewayLoopback::start(png_bytes([2, 3, 4, 255])).await;
            let mut cfg = test_config(&root);
            cfg.image_api_url = server.url.clone();
            cfg.image_api_key = "local-test-key".into();
            let mut config = json!({"prompt":"local test","model":"local-model"});
            if has_reference {
                let reference = root.join("reference.png");
                write_png(&reference, [8, 9, 10, 255]);
                config["references"] = json!([{
                    "path": reference,
                    "role": "style",
                }]);
            }
            let output = root.join("output");
            let result = timeout(
                Duration::from_secs(3),
                generate_image_with_clients(
                    &cfg,
                    &RunNodeRequest {
                        node_type: "image".into(),
                        category: "image".into(),
                        config,
                        input_assets: Vec::new(),
                    },
                    &output,
                    &loopback_client(),
                    &loopback_client(),
                ),
            )
            .await
            .expect("loopback image generation must return promptly")
            .unwrap();
            assert_eq!(result.len(), 1);
            assert!(std::path::Path::new(&result[0].path).is_file());
            let requests = server.finish().await;
            assert_eq!(requests.len(), 2);
            assert!(requests[0].starts_with("POST "));
            assert!(requests[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer local-test-key"));
            assert!(requests[1].starts_with("GET /asset.png "));
            assert!(
                !requests[1].to_ascii_lowercase().contains("authorization:"),
                "asset GET must be made by the separate unprivileged client"
            );
            if has_reference {
                assert!(requests[0].starts_with("POST /v1/images/edits "));
            } else {
                assert!(requests[0].starts_with("POST /v1/images/generations "));
            }
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn derives_edits_endpoint_without_replacing_unrelated_text() {
        assert_eq!(
            edits_endpoint("https://example.com/v1/images/generations?tenant=generations").unwrap(),
            "https://example.com/v1/images/edits?tenant=generations"
        );
        assert!(edits_endpoint("https://example.com/v1/images").is_err());
    }

    #[test]
    fn rejects_reference_paths_outside_allowed_roots() {
        let root = temp_root();
        let inside = root.join("ref.png");
        std::fs::write(&inside, b"png").unwrap();
        let outside =
            std::env::temp_dir().join(format!("image-client-gateway-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&outside, b"png").unwrap();
        let cfg = test_config(&root);
        assert!(validate_reference_path(inside.to_str().unwrap(), &cfg).is_ok());
        assert!(validate_reference_path(outside.to_str().unwrap(), &cfg).is_err());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn references_sort_stably_by_sort_order_then_role() {
        let root = temp_root();
        let first = root.join("first.png");
        let second = root.join("second.png");
        let third = root.join("third.png");
        write_png(&first, [255, 0, 0, 255]);
        write_png(&second, [0, 255, 0, 255]);
        write_png(&third, [0, 0, 255, 255]);
        let cfg = test_config(&root);
        let config = json!({"references": [
            {"path": first, "role": "style", "weight": 0.6, "sortOrder": 2},
            {"path": second, "role": "scene", "weight": 1.0, "sortOrder": 1},
            {"path": third, "role": "character_identity", "weight": 0.8, "sortOrder": 2}
        ]});

        let references = normalize_references(&config, &cfg).unwrap();
        assert_eq!(
            references
                .iter()
                .map(|reference| reference.role.as_str())
                .collect::<Vec<_>>(),
            ["scene", "character_identity", "style"]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn references_reject_invalid_role_duplicate_path_and_limits() {
        let root = temp_root();
        let image = root.join("ref.png");
        write_png(&image, [1, 2, 3, 255]);
        let cfg = test_config(&root);
        let invalid_role = json!({"references": [{"path": image, "role": "anything"}]});
        assert!(normalize_references(&invalid_role, &cfg).is_err());
        let duplicate = json!({"references": [
            {"path": image, "role": "style"},
            {"path": image, "role": "scene"}
        ]});
        assert!(normalize_references(&duplicate, &cfg).is_err());
        let inputs = vec![
            ReferenceInput {
                path: image.display().to_string(),
                role: "style".into(),
                weight: None,
                sort_order: None,
            },
            ReferenceInput {
                path: root.join("other.png").display().to_string(),
                role: "scene".into(),
                weight: None,
                sort_order: None,
            },
        ];
        assert!(validate_reference_inputs(
            &inputs,
            &cfg,
            ReferenceLimits {
                max_count: 1,
                max_each_bytes: 1024,
                max_total_bytes: 1024
            }
        )
        .is_err());
        let one_input = vec![ReferenceInput {
            path: image.display().to_string(),
            role: "style".into(),
            weight: None,
            sort_order: None,
        }];
        assert!(validate_reference_inputs(
            &one_input,
            &cfg,
            ReferenceLimits {
                max_count: 1,
                max_each_bytes: 1024,
                max_total_bytes: 1
            }
        )
        .is_err());
        assert!(validate_reference_inputs(
            &one_input,
            &cfg,
            ReferenceLimits {
                max_count: 1,
                max_each_bytes: 1,
                max_total_bytes: 1024
            }
        )
        .is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_reference_path_becomes_a_single_base_image_reference() {
        let root = temp_root();
        let image = root.join("legacy.png");
        write_png(&image, [1, 2, 3, 255]);
        let config = json!({"referencePath": image});
        let references = normalize_references(&config, &test_config(&root)).unwrap();
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].role, "base_image");
        assert_eq!(references[0].sort_order, 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn temporary_reference_board_is_deleted_when_request_scope_unwinds() {
        let root = temp_root();
        let first = root.join("first.png");
        let second = root.join("second.png");
        write_png(&first, [255, 0, 0, 255]);
        write_png(&second, [0, 255, 0, 255]);
        let config = json!({"references": [
            {"path": first, "role": "style", "sortOrder": 1},
            {"path": second, "role": "scene", "sortOrder": 0}
        ]});
        let references = normalize_references(&config, &test_config(&root)).unwrap();
        let (upload, guard) = prepare_reference_upload(&references, &root).unwrap();
        assert_eq!(upload.file_name, "reference-board.png");
        let prompt = prompt_with_reference_roles("draw a scene", &references);
        assert!(prompt.contains("tile 1 = scene"));
        assert!(prompt.contains("tile 2 = style"));
        assert!(!prompt.contains(root.to_string_lossy().as_ref()));
        let board_path = guard.as_ref().unwrap().path.clone();
        assert!(board_path.exists());
        drop(guard);
        assert!(!board_path.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
