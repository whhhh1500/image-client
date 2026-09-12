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

/// Requested image count ("n"). The gateway supports at most four images per
/// request, so the app clamps instead of forwarding an out-of-range value.
pub(crate) const MAX_IMAGE_BATCH: u32 = 4;

/// Grok image models reject the OpenAI `size` contract: the gateway documents
/// `aspect_ratio` plus `resolution` ("1k" | "2k") and no `background`.
/// Prompts are forwarded verbatim — the gateway is the only authority on what
/// a model accepts, and it does not enforce a length limit.
const GROK_IMAGE_RESOLUTIONS: &[&str] = &["1k", "2k"];
/// Ratios the Grok image endpoint documents, as (label, numeric value).
const GROK_ASPECT_RATIOS: &[(&str, f64)] = &[
    ("1:1", 1.0),
    ("3:4", 3.0 / 4.0),
    ("4:3", 4.0 / 3.0),
    ("9:16", 9.0 / 16.0),
    ("16:9", 16.0 / 9.0),
    ("2:3", 2.0 / 3.0),
    ("3:2", 3.0 / 2.0),
    ("9:19.5", 9.0 / 19.5),
    ("19.5:9", 19.5 / 9.0),
    ("9:20", 9.0 / 20.0),
    ("20:9", 20.0 / 9.0),
    ("1:2", 1.0 / 2.0),
    ("2:1", 2.0),
];

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
    let base = normalize_size(&size_selection(config));
    if !base.is_empty() {
        return base;
    }
    let w = get_u32(config, "width").unwrap_or(1024);
    let h = get_u32(config, "height").unwrap_or(1024);
    format!("{w}x{h}")
}

/// Values look like "1024x1024 (1:1)" — keep only the WxH part.
fn normalize_size(selection: &str) -> String {
    let base = selection.split('(').next().unwrap_or(selection).trim();
    if base.contains('x') {
        base.to_string()
    } else {
        String::new()
    }
}

/// Raw size selection, keeping the "(1:1)" / "(2K)" annotation the UI adds.
fn size_selection(config: &serde_json::Value) -> String {
    if let Some(s) = get_str(config, "size") {
        if !s.trim().is_empty() {
            return s;
        }
    }
    let w = get_u32(config, "width").unwrap_or(1024);
    let h = get_u32(config, "height").unwrap_or(1024);
    format!("{w}x{h}")
}

/// Whether the model is served by the Grok image adapter, whose request
/// contract differs from OpenAI's (`aspect_ratio` + `resolution`).
///
/// Matching is segment-aware so a namespaced id (`xai/grok-2-image`) still
/// matches while an unrelated id that merely contains the letters (`grokking`)
/// keeps the OpenAI contract.
fn is_grok_image_model(model: &str) -> bool {
    model
        .trim()
        .to_ascii_lowercase()
        .split(['/', ':', '@'])
        .any(is_grok_name_segment)
}

/// A vendor segment is Grok when it is `grok`, uses a separator
/// (`grok-imagine-image`), or continues straight into a version
/// (`grok3-image`). `grokking-image` is deliberately not a match.
fn is_grok_name_segment(segment: &str) -> bool {
    if segment == "grok" {
        return true;
    }
    match segment.strip_prefix("grok") {
        Some(rest) => rest.starts_with(['-', '_']) || rest.starts_with(|c: char| c.is_ascii_digit()),
        None => false,
    }
}

/// Grok rejects an unexpected parameter list with 400/422, which is also how a
/// gateway that proxies Grok behind an OpenAI-only adapter answers the Grok
/// contract. Those statuses trigger one retry with the OpenAI body.
fn is_contract_rejection(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 400 | 422)
}

/// Images requested for this run: `n` when present, otherwise one.
pub(crate) fn requested_image_count(config: &serde_json::Value) -> u32 {
    get_u32(config, "n")
        .unwrap_or(1)
        .clamp(1, MAX_IMAGE_BATCH)
}

fn grok_aspect_ratio(selection: &str) -> String {
    // An explicit "(2:3)" annotation wins: it is what the user picked.
    let annotation = selection
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(inner, _)| inner.trim().to_ascii_lowercase());
    if let Some(annotation) = annotation {
        if let Some((label, _)) = GROK_ASPECT_RATIOS
            .iter()
            .find(|(label, _)| *label == annotation)
        {
            return (*label).to_string();
        }
    }
    let Some((width, height)) = pixel_size(selection) else {
        return "1:1".to_string();
    };
    let ratio = width as f64 / height as f64;
    GROK_ASPECT_RATIOS
        .iter()
        .min_by(|(_, left), (_, right)| {
            (left - ratio)
                .abs()
                .partial_cmp(&(right - ratio).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(label, _)| (*label).to_string())
        .unwrap_or_else(|| "1:1".to_string())
}

/// Grok only generates "1k" or "2k". Larger selections (the UI offers 2K and
/// 4K) collapse onto the documented maximum instead of being rejected.
fn grok_resolution(selection: &str) -> String {
    let hinted = selection.to_ascii_lowercase();
    if hinted.contains("2k") || hinted.contains("4k") || hinted.contains("8k") {
        return GROK_IMAGE_RESOLUTIONS[1].to_string();
    }
    match pixel_size(selection) {
        Some((width, height)) if width.max(height) >= 2000 => GROK_IMAGE_RESOLUTIONS[1].to_string(),
        _ => GROK_IMAGE_RESOLUTIONS[0].to_string(),
    }
}

/// Parse the leading "1024x1536" (or "1024×1536") of a size selection.
fn pixel_size(selection: &str) -> Option<(u32, u32)> {
    let base = selection.split('(').next().unwrap_or(selection).trim();
    let normalized = base.replace(['×', 'X'], "x");
    let (width, height) = normalized.split_once('x')?;
    let width = width.trim().parse::<u32>().ok()?;
    let height = height.trim().parse::<u32>().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

/// Request body for POST /v1/images/generations.
///
/// OpenAI-compatible providers take `size`/`quality`/`background`; Grok takes
/// `aspect_ratio` + `resolution` and rejects the others, so the app's size
/// selection is translated instead of forwarded blindly.
fn image_generation_body(
    model: &str,
    prompt: &str,
    size: &str,
    size_selection: &str,
    quality: &str,
    background: &str,
    count: u32,
) -> serde_json::Value {
    if is_grok_image_model(model) {
        return grok_image_generation_body(model, prompt, size_selection, quality, count);
    }
    openai_image_generation_body(model, prompt, size, quality, background, count)
}

/// The historical OpenAI-compatible body, kept intact for every non-Grok model
/// and reused as the fallback when a gateway rejects the Grok contract.
fn openai_image_generation_body(
    model: &str,
    prompt: &str,
    size: &str,
    quality: &str,
    background: &str,
    count: u32,
) -> serde_json::Value {
    let mut body = serde_json::json!({ "model": model, "prompt": prompt, "n": count });
    if !size.is_empty() {
        body["size"] = serde_json::json!(size);
    }
    if !quality.is_empty() {
        body["quality"] = serde_json::json!(quality);
    }
    if !background.is_empty() && background != "auto" {
        body["background"] = serde_json::json!(background);
    }
    body
}

fn grok_image_generation_body(
    model: &str,
    prompt: &str,
    size_selection: &str,
    quality: &str,
    count: u32,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "n": count,
        "aspect_ratio": grok_aspect_ratio(size_selection),
        "resolution": grok_resolution(size_selection),
        "response_format": "b64_json",
    });
    // Grok accepts low/medium/high only; anything else is dropped rather than
    // sent as an invalid enum value.
    if matches!(quality, "low" | "medium" | "high") {
        body["quality"] = serde_json::json!(quality);
    }
    body
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
    let generation_client = crate::http::client_for_url(&cfg.image_api_url)?;
    // Asset URLs are fetched with a separate client.  Bearer authentication is
    // attached to generation POSTs below, never to these GET requests.
    let asset_download_client = crate::http::shared_client()?;
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
    let selection = size_selection(&req.config);
    let size = size_str(&req.config);
    let quality = get_str(&req.config, "quality").unwrap_or_default();
    let background = get_str(&req.config, "background").unwrap_or_default();
    let count = requested_image_count(&req.config);
    let grok = is_grok_image_model(&model);
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
            "count": count,
            "aspectRatio": grok.then(|| grok_aspect_ratio(&selection)),
            "resolution": grok.then(|| grok_resolution(&selection)),
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
        let provider_prompt = prompt_with_reference_roles(&prompt, &references);
        // Built on demand: the fallback request needs a second, OpenAI-shaped
        // form, and `Part` owns its bytes.
        let build_form = |grok_contract: bool| -> Result<Form, String> {
            let part = Part::bytes(upload.bytes.clone())
                .file_name(upload.file_name.clone())
                .mime_str(&mime)
                .map_err(|e| format!("构造上传失败: {e}"))?;
            let mut form = Form::new()
                .text("model", model.clone())
                .text("prompt", provider_prompt.clone())
                .text("n", count.to_string())
                .part("image", part);
            if grok_contract {
                // Grok takes aspect_ratio/resolution and rejects size/background.
                form = form
                    .text("aspect_ratio", grok_aspect_ratio(&selection))
                    .text("resolution", grok_resolution(&selection));
            } else if !size.is_empty() {
                form = form.text("size", size.clone());
            }
            if !quality.is_empty() {
                form = form.text("quality", quality.clone());
            }
            if !grok_contract && !background.is_empty() && background != "auto" {
                form = form.text("background", background.clone());
            }
            Ok(form)
        };

        // "/v1/images/generations" -> "/v1/images/edits"
        let edits_url = edits_endpoint(&cfg.image_api_url)?;
        let mut resp = post_image_multipart(
            generation_client,
            &edits_url,
            &cfg.image_api_key,
            build_form(grok)?,
            &request_id,
            started,
        )
        .await?;
        if grok && is_contract_rejection(resp.status()) {
            crate::logging::warn(
                "image.request.grok_contract_fallback",
                serde_json::json!({ "requestId": request_id, "status": resp.status().as_u16(), "mode": "image_to_image" }),
            );
            resp = post_image_multipart(
                generation_client,
                &edits_url,
                &cfg.image_api_key,
                build_form(false)?,
                &request_id,
                started,
            )
            .await?;
        }
        finish_request(
            parse_and_save(resp, &asset_download_client, out_dir).await,
            &request_id,
            started,
        )
    } else {
        let body = image_generation_body(&model, &prompt, &size, &selection, &quality, &background, count);
        let mut resp = post_image_json(
            generation_client,
            &cfg.image_api_url,
            &cfg.image_api_key,
            &body,
            &request_id,
            started,
        )
        .await?;
        if grok && is_contract_rejection(resp.status()) {
            // Compatibility net: a gateway may serve Grok models through an
            // OpenAI-only adapter. Retrying once with the OpenAI body can only
            // add capacity — the rejected request produced no image.
            crate::logging::warn(
                "image.request.grok_contract_fallback",
                serde_json::json!({ "requestId": request_id, "status": resp.status().as_u16(), "mode": "text_to_image" }),
            );
            let fallback = openai_image_generation_body(&model, &prompt, &size, &quality, &background, count);
            resp = post_image_json(
                generation_client,
                &cfg.image_api_url,
                &cfg.image_api_key,
                &fallback,
                &request_id,
                started,
            )
            .await?;
        }
        finish_request(
            parse_and_save(resp, &asset_download_client, out_dir).await,
            &request_id,
            started,
        )
    }
}

async fn post_image_json(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    body: &serde_json::Value,
    request_id: &str,
    started: Instant,
) -> Result<reqwest::Response, String> {
    let send_started = Instant::now();
    #[cfg(feature = "real-e2e-harness")]
    crate::harness_transport::reserve_image_post()?;
    let resp = client
        .post(url)
        .bearer_auth(key)
        .json(body)
        .timeout(Duration::from_secs(240))
        .send()
        .await
        .map_err(|e| {
            crate::logging::error("image.request.end", serde_json::json!({ "requestId": request_id, "status": "error", "durationMs": started.elapsed().as_millis(), "error": crate::logging::error_text(&e) }));
            format!("请求图像接口失败: {e}")
        })?;
    #[cfg(feature = "real-e2e-harness")]
    crate::harness_transport::mark_image_http_received()?;
    crate::logging::debug(
        "image.response.headers",
        serde_json::json!({ "requestId": request_id, "durationMs": send_started.elapsed().as_millis(), "status": resp.status().as_u16() }),
    );
    Ok(resp)
}

async fn post_image_multipart(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    form: Form,
    request_id: &str,
    started: Instant,
) -> Result<reqwest::Response, String> {
    let send_started = Instant::now();
    #[cfg(feature = "real-e2e-harness")]
    crate::harness_transport::reserve_image_post()?;
    let resp = client
        .post(url)
        .bearer_auth(key)
        .multipart(form)
        .timeout(Duration::from_secs(240))
        .send()
        .await
        .map_err(|e| {
            crate::logging::error("image.request.end", serde_json::json!({ "requestId": request_id, "status": "error", "durationMs": started.elapsed().as_millis(), "error": crate::logging::error_text(&e) }));
            format!("请求图像编辑接口失败: {e}")
        })?;
    #[cfg(feature = "real-e2e-harness")]
    crate::harness_transport::mark_image_http_received()?;
    crate::logging::debug(
        "image.response.headers",
        serde_json::json!({ "requestId": request_id, "durationMs": send_started.elapsed().as_millis(), "status": resp.status().as_u16() }),
    );
    Ok(resp)
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
        let source = crate::assets::decode_image_checked(&reference.bytes)
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

    let entries = json
        .get("data")
        .and_then(|d| d.as_array())
        .filter(|items| !items.is_empty())
        .ok_or("图像响应缺少 data")?;

    // Decode every returned image before writing anything, so a malformed
    // entry cannot leave a half-saved batch behind.
    let mut payloads: Vec<Vec<u8>> = Vec::with_capacity(entries.len().min(MAX_IMAGE_BATCH as usize));
    for entry in entries.iter().take(MAX_IMAGE_BATCH as usize) {
        payloads.push(decode_image_entry(entry, client).await?);
    }
    if payloads.is_empty() {
        return Err("模型未返回图像数据".into());
    }

    let mut formats = Vec::with_capacity(payloads.len());
    for bytes in &payloads {
        formats.push(
            assets::detect_format_checked(bytes)
                .ok_or("图像接口返回的内容不是支持的图片格式")?
                .to_string(),
        );
    }

    let mut saved = Vec::with_capacity(payloads.len());
    for (bytes, format) in payloads.into_iter().zip(formats) {
        saved.push(assets::save_bytes(out_dir, "image", &bytes, &format)?);
    }
    Ok(saved)
}

/// One `data[]` entry: inline base64 or a URL to download.
async fn decode_image_entry(
    entry: &serde_json::Value,
    client: &reqwest::Client,
) -> Result<Vec<u8>, String> {
    if let Some(b64) = entry.get("b64_json").and_then(|b| b.as_str()) {
        if b64.is_empty() {
            return Err("模型未返回图像数据（可能不支持该参数，如透明背景）".into());
        }
        return base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("base64 解码失败: {e}"));
    }
    if let Some(url) = entry.get("url").and_then(|u| u.as_str()) {
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
        return Ok(bytes);
    }
    Err("图像响应中没有 b64_json 或 url".into())
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
        edits_endpoint, generate_image_with_clients, grok_aspect_ratio, grok_resolution,
        image_generation_body, is_grok_image_model, normalize_references,
        prepare_reference_upload, prompt_with_reference_roles, requested_image_count,
        validate_reference_inputs, validate_reference_path, ReferenceInput, ReferenceLimits,
        MAX_IMAGE_BATCH,
    };
    use base64::Engine as _;
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
        bodies: Arc<Mutex<Vec<String>>>,
        task: Option<JoinHandle<()>>,
    }

    impl GatewayLoopback {
        async fn start(asset_bytes: Vec<u8>) -> Self {
            Self::start_with(asset_bytes, 2, Vec::new()).await
        }

        /// `connections` is how many requests the loop serves before finishing;
        /// `post_responses` supplies (status, body) pairs for POSTs in order,
        /// with the default `data[].url` response once the queue is empty.
        async fn start_with(
            asset_bytes: Vec<u8>,
            connections: usize,
            post_responses: Vec<(u16, String)>,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&requests);
            let bodies = Arc::new(Mutex::new(Vec::new()));
            let captured_bodies = Arc::clone(&bodies);
            let task = tokio::spawn(async move {
                let mut served = 0usize;
                for _ in 0..connections {
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
                    // hyper writes lowercase header names, so the lookup must be
                    // case-insensitive: a missed content-length left the body
                    // unread and reset the connection once it exceeded the
                    // socket buffer.
                    let content_length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
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
                    captured_bodies.lock().unwrap().push(
                        String::from_utf8_lossy(&request[header_end..]).into_owned(),
                    );
                    if is_get {
                        let headers = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            asset_bytes.len()
                        );
                        stream.write_all(headers.as_bytes()).await.unwrap();
                        stream.write_all(&asset_bytes).await.unwrap();
                    } else {
                        let (status, body) = post_responses
                            .get(served)
                            .cloned()
                            .unwrap_or_else(|| {
                                (
                                    200,
                                    format!("{{\"data\":[{{\"url\":\"http://{address}/asset.png\"}}]}}"),
                                )
                            });
                        served += 1;
                        let reason = match status {
                            200 => "OK",
                            400 => "Bad Request",
                            422 => "Unprocessable Entity",
                            _ => "Error",
                        };
                        let headers = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
                bodies,
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

        /// Request heads plus the raw bodies that were posted.
        async fn finish_with_bodies(mut self) -> (Vec<String>, Vec<String>) {
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
            (
                self.requests.lock().unwrap().clone(),
                self.bodies.lock().unwrap().clone(),
            )
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
    fn grok_bodies_use_aspect_ratio_and_resolution_instead_of_size() {
        let body = image_generation_body(
            "grok-imagine-image",
            "a cat",
            "1536x1024",
            "1536x1024 (3:2)",
            "high",
            "transparent",
            4,
        );
        assert_eq!(body["model"], "grok-imagine-image");
        assert_eq!(body["n"], 4);
        assert_eq!(body["aspect_ratio"], "3:2");
        assert_eq!(body["resolution"], "1k");
        assert_eq!(body["response_format"], "b64_json");
        assert_eq!(body["quality"], "high");
        assert!(body.get("size").is_none(), "grok rejects the OpenAI size field");
        assert!(
            body.get("background").is_none(),
            "grok has no background parameter"
        );

        // An unknown quality value is dropped instead of sent as a bad enum.
        let body = image_generation_body("grok-3-image", "a cat", "1024x1024", "1024x1024 (1:1)", "hd", "", 1);
        assert_eq!(body["aspect_ratio"], "1:1");
        assert_eq!(body["n"], 1);
        assert!(body.get("quality").is_none());
    }

    #[test]
    fn grok_ratio_comes_from_the_annotation_or_the_pixels() {
        assert_eq!(grok_aspect_ratio("1280x720 (16:9)"), "16:9");
        assert_eq!(grok_aspect_ratio("720x1280 (9:16)"), "9:16");
        // The comic pipeline sends a bare WxH without the UI annotation.
        assert_eq!(grok_aspect_ratio("1024x1536"), "2:3");
        assert_eq!(grok_aspect_ratio("1024x1024"), "1:1");
        // A non-documented ratio snaps to the closest supported one.
        assert_eq!(grok_aspect_ratio("1500x1000"), "3:2");
        assert_eq!(grok_aspect_ratio(""), "1:1");
        assert_eq!(grok_aspect_ratio("not-a-size"), "1:1");
    }

    #[test]
    fn grok_resolution_collapses_oversized_selections_to_the_documented_maximum() {
        assert_eq!(grok_resolution("1024x1024 (1:1)"), "1k");
        assert_eq!(grok_resolution("1280x720 (16:9)"), "1k");
        assert_eq!(grok_resolution("2048x2048 (2K)"), "2k");
        assert_eq!(grok_resolution("4096x4096 (4K)"), "2k");
        assert_eq!(grok_resolution("2000x1000"), "2k");
    }

    #[test]
    fn openai_compatible_bodies_keep_size_quality_and_background() {
        let body = image_generation_body(
            "gpt-image-2",
            "a cat",
            "1024x1024",
            "1024x1024 (1:1)",
            "high",
            "transparent",
            2,
        );
        assert_eq!(body["size"], "1024x1024");
        assert_eq!(body["n"], 2);
        assert_eq!(body["quality"], "high");
        assert_eq!(body["background"], "transparent");
        assert!(body.get("aspect_ratio").is_none());
        assert!(body.get("resolution").is_none());
        assert!(body.get("response_format").is_none());

        // "auto" background is still omitted for OpenAI-compatible providers.
        let body = image_generation_body("gemini-3-pro-image", "a cat", "1024x1024", "1024x1024 (1:1)", "", "auto", 1);
        assert!(body.get("background").is_none());
        assert!(body.get("quality").is_none());
    }

    #[test]
    fn requested_count_is_clamped_to_the_supported_batch() {
        assert_eq!(requested_image_count(&json!({})), 1);
        assert_eq!(requested_image_count(&json!({ "n": 4 })), 4);
        assert_eq!(requested_image_count(&json!({ "n": 0 })), 1);
        assert_eq!(requested_image_count(&json!({ "n": 5 })), MAX_IMAGE_BATCH);
        assert_eq!(requested_image_count(&json!({ "n": 99 })), MAX_IMAGE_BATCH);
        assert_eq!(MAX_IMAGE_BATCH, 4);
    }

    #[test]
    fn is_grok_image_model_matches_every_gateway_id_shape() {
        assert!(is_grok_image_model("grok-imagine-image"));
        assert!(is_grok_image_model("grok-imagine-image-2.0"));
        assert!(is_grok_image_model("grok-imagine-image-quality"));
        assert!(is_grok_image_model("  Grok-3-Image "));
        // Namespaced ids still resolve to the Grok adapter ...
        assert!(is_grok_image_model("xai/grok-2-image"));
        // ... while an id that merely contains the letters keeps OpenAI params.
        assert!(!is_grok_image_model("grokking-image"));
        assert!(!is_grok_image_model("gpt-image-2.5"));
        assert!(!is_grok_image_model("gemini-3-pro-image"));
        assert!(!is_grok_image_model("nana-banana-pro"));
    }

    #[tokio::test]
    async fn openai_models_post_the_openai_body_over_the_wire() {
        let root = temp_root();
        let server = GatewayLoopback::start(png_bytes([3, 3, 3, 255])).await;
        let mut cfg = test_config(&root);
        cfg.image_api_url = server.url.clone();
        cfg.image_api_key = "local-test-key".into();

        let result = timeout(
            Duration::from_secs(3),
            generate_image_with_clients(
                &cfg,
                &RunNodeRequest {
                    node_type: "image".into(),
                    category: "generate".into(),
                    config: json!({
                        "prompt": "一只猫",
                        "model": "gpt-image-2.5",
                        "size": "1536x1024 (3:2)",
                        "quality": "high",
                        "background": "transparent",
                        "n": 2,
                    }),
                    input_assets: Vec::new(),
                },
                &root.join("output"),
                &loopback_client(),
                &loopback_client(),
            ),
        )
        .await
        .expect("loopback image generation must return promptly")
        .unwrap_or_else(|error| panic!("generation failed: {error}"));
        assert_eq!(result.len(), 1);

        let (_requests, bodies) = server.finish_with_bodies().await;
        let posted: serde_json::Value = serde_json::from_str(bodies[0].trim()).unwrap();
        assert_eq!(
            posted,
            json!({
                "model": "gpt-image-2.5",
                "prompt": "一只猫",
                "n": 2,
                "size": "1536x1024",
                "quality": "high",
                "background": "transparent",
            }),
            "non-Grok models must keep the untouched OpenAI contract"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_gateway_that_rejects_the_grok_contract_gets_an_openai_retry() {
        let root = temp_root();
        let png = png_bytes([6, 6, 6, 255]);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        let asset_body = json!({ "data": [{ "b64_json": encoded }] }).to_string();
        let server = GatewayLoopback::start_with(
            png,
            2,
            vec![
                (400, json!({ "error": { "message": "unknown parameter aspect_ratio" } }).to_string()),
                (200, asset_body),
            ],
        )
        .await;
        let mut cfg = test_config(&root);
        cfg.image_api_url = server.url.clone();
        cfg.image_api_key = "local-test-key".into();

        let result = timeout(
            Duration::from_secs(3),
            generate_image_with_clients(
                &cfg,
                &RunNodeRequest {
                    node_type: "image".into(),
                    category: "generate".into(),
                    config: json!({ "prompt": "一只猫", "model": "grok-imagine-image", "size": "1024x1536 (2:3)" }),
                    input_assets: Vec::new(),
                },
                &root.join("output"),
                &loopback_client(),
                &loopback_client(),
            ),
        )
        .await
        .expect("loopback image generation must return promptly")
        .unwrap_or_else(|error| panic!("generation failed: {error}"));
        assert_eq!(result.len(), 1, "the fallback response must be saved");

        let (_requests, bodies) = server.finish_with_bodies().await;
        assert_eq!(bodies.len(), 2, "expected the Grok body plus one retry");
        let first: serde_json::Value = serde_json::from_str(bodies[0].trim()).unwrap();
        assert_eq!(first["aspect_ratio"], "2:3");
        assert!(first.get("size").is_none());
        let retry: serde_json::Value = serde_json::from_str(bodies[1].trim()).unwrap();
        assert_eq!(retry["size"], "1024x1536");
        assert!(retry.get("aspect_ratio").is_none());
        assert!(retry.get("resolution").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn grok_text_to_image_posts_grok_parameters_over_the_wire() {
        let root = temp_root();
        let server = GatewayLoopback::start(png_bytes([7, 8, 9, 255])).await;
        let mut cfg = test_config(&root);
        cfg.image_api_url = server.url.clone();
        cfg.image_api_key = "local-test-key".into();
        let output = root.join("output");

        let result = timeout(
            Duration::from_secs(3),
            generate_image_with_clients(
                &cfg,
                &RunNodeRequest {
                    node_type: "image".into(),
                    category: "image".into(),
                    config: json!({
                        "prompt": "一只猫",
                        "model": "grok-3-image",
                        "size": "1024x1536 (2:3)",
                        "quality": "high",
                        "background": "transparent",
                    }),
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

        let (requests, bodies) = server.finish_with_bodies().await;
        assert!(requests[0].starts_with("POST /v1/images/generations "));
        let posted: serde_json::Value = serde_json::from_str(bodies[0].trim()).unwrap();
        assert_eq!(
            posted,
            json!({
                "model": "grok-3-image",
                "prompt": "一只猫",
                "n": 1,
                "aspect_ratio": "2:3",
                "resolution": "1k",
                "response_format": "b64_json",
                "quality": "high",
            })
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn every_returned_image_is_saved_and_n_is_forwarded() {
        let root = temp_root();
        let png = png_bytes([4, 5, 6, 255]);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        let body = json!({
            "data": [
                { "b64_json": encoded },
                { "b64_json": encoded },
                { "b64_json": encoded },
            ]
        })
        .to_string();
        let server = GatewayLoopback::start_with(png, 1, vec![(200, body)]).await;
        let mut cfg = test_config(&root);
        cfg.image_api_url = server.url.clone();
        cfg.image_api_key = "local-test-key".into();

        let result = timeout(
            Duration::from_secs(3),
            generate_image_with_clients(
                &cfg,
                &RunNodeRequest {
                    node_type: "image".into(),
                    category: "generate".into(),
                    config: json!({ "prompt": "三张", "model": "gpt-image-2", "n": 3 }),
                    input_assets: Vec::new(),
                },
                &root.join("output"),
                &loopback_client(),
                &loopback_client(),
            ),
        )
        .await
        .expect("loopback image generation must return promptly")
        .unwrap();
        assert_eq!(result.len(), 3, "every returned image must be saved");
        let mut ids: Vec<String> = result.iter().map(|asset| asset.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3, "each saved image keeps its own asset id");
        for asset in &result {
            assert!(std::path::Path::new(&asset.path).is_file(), "{}", asset.path);
        }

        let (_requests, bodies) = server.finish_with_bodies().await;
        let posted: serde_json::Value = serde_json::from_str(bodies[0].trim()).unwrap();
        assert_eq!(posted["n"], 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn long_prompts_are_forwarded_verbatim() {
        // No prompt length rule of our own: whatever the user typed (or the
        // comic pipeline composed) goes to the provider unchanged.
        let root = temp_root();
        let server = GatewayLoopback::start(png_bytes([1, 2, 3, 255])).await;
        let mut cfg = test_config(&root);
        cfg.image_api_url = server.url.clone();
        cfg.image_api_key = "local-test-key".into();
        let prompt = "猫".repeat(1500);

        let result = timeout(
            Duration::from_secs(3),
            generate_image_with_clients(
                &cfg,
                &RunNodeRequest {
                    node_type: "image".into(),
                    category: "image".into(),
                    config: json!({ "prompt": prompt, "model": "grok-imagine-image" }),
                    input_assets: Vec::new(),
                },
                &root.join("output"),
                &loopback_client(),
                &loopback_client(),
            ),
        )
        .await
        .expect("loopback image generation must return promptly")
        .unwrap_or_else(|error| panic!("generation failed: {error}"));
        assert_eq!(result.len(), 1);

        let (_requests, bodies) = server.finish_with_bodies().await;
        let posted: serde_json::Value = serde_json::from_str(bodies[0].trim()).unwrap();
        assert_eq!(posted["prompt"].as_str().unwrap().chars().count(), 1500);
        assert_eq!(posted["aspect_ratio"], "1:1");
        let _ = std::fs::remove_dir_all(root);
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
