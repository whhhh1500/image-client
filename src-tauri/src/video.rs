use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::ConfigState;
use crate::model::AssetRef;

const MAX_VIDEO_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoModelCapability {
    pub id: &'static str,
    pub label: &'static str,
    pub modes: Vec<&'static str>,
    pub min_duration_s: u32,
    pub max_duration_s: u32,
    pub duration_options: Vec<u32>,
    pub resolutions: Vec<&'static str>,
    pub aspect_ratios: Vec<&'static str>,
    pub max_images: usize,
    pub max_videos: usize,
    pub max_audios: usize,
    pub max_total_references: Option<usize>,
    pub reference_image_count: Option<usize>,
    pub max_reference_duration_s: Option<u32>,
    pub note: &'static str,
}

pub struct VideoGenRequest {
    pub model: String,
    pub prompt: String,
    pub duration_s: u32,
    pub aspect_ratio: Option<String>,
    pub resolution: Option<String>,
    pub mode: Option<String>,
    pub images: Vec<String>,
    pub videos: Vec<String>,
    pub audios: Vec<String>,
}

fn range(min: u32, max: u32) -> Vec<u32> {
    (min..=max).collect()
}

pub fn model_capabilities() -> Vec<VideoModelCapability> {
    vec![
        VideoModelCapability {
            id: "video-ds-2.5",
            label: "即梦 2.5 · 720p",
            modes: vec!["text", "reference"],
            min_duration_s: 4,
            max_duration_s: 30,
            duration_options: range(4, 30),
            resolutions: vec!["720p"],
            aspect_ratios: vec!["16:9", "9:16", "1:1"],
            max_images: 30,
            max_videos: 10,
            max_audios: 10,
            max_total_references: None,
            reference_image_count: None,
            max_reference_duration_s: None,
            note: "多模态长镜头；单次可直接生成 4–30 秒。",
        },
        VideoModelCapability {
            id: "video-ds-2.5-480",
            label: "即梦 2.5 · 480p",
            modes: vec!["text", "reference"],
            min_duration_s: 4,
            max_duration_s: 30,
            duration_options: range(4, 30),
            resolutions: vec!["480p"],
            aspect_ratios: vec!["16:9", "9:16", "1:1"],
            max_images: 30,
            max_videos: 10,
            max_audios: 10,
            max_total_references: None,
            reference_image_count: None,
            max_reference_duration_s: None,
            note: "多模态快速版本；适合多图、多视频和音频参考。",
        },
        minimax_capability("minimax-h3", "MiniMax H3 · 2K", "2K"),
        minimax_capability("minimax-h3-2k", "MiniMax H3 · 2K", "2K"),
        minimax_capability("minimax-h3-4k", "MiniMax H3 · 4K", "4k"),
        kling_capability("kling-video-v3", "可灵 V3", vec!["720p", "1080p", "4k"]),
        kling_capability(
            "kling-video-v3-omni",
            "可灵 V3 Omni",
            vec!["720p", "1080p", "4k"],
        ),
        kling_capability(
            "kling-video-v3-turbo",
            "可灵 V3 Turbo",
            vec!["720p", "1080p"],
        ),
        grok_capability("grok-imagine-video", "Grok Imagine Video", Some(10)),
        grok_capability(
            "grok-imagine-video-1.5-preview",
            "Grok Imagine Video 1.5 Preview",
            None,
        ),
        drama_capability(
            "drama-video-v2",
            "Drama Video V2",
            vec!["480p", "720p", "1080p"],
        ),
        drama_capability(
            "drama-video-v2-fast",
            "Drama Video V2 Fast",
            vec!["480p", "720p"],
        ),
        standard_capability("video-ds-2.0", "即梦 2.0"),
        standard_capability("video-ds-2.0-fast", "即梦 2.0 Fast"),
        standard_capability("as-sd2.0-fast", "即梦 2.0 Fast"),
        VideoModelCapability {
            id: "seedance2.5",
            label: "即梦 2.5",
            modes: vec!["text", "reference"],
            min_duration_s: 4,
            max_duration_s: 30,
            duration_options: range(4, 30),
            resolutions: vec!["480p", "720p"],
            aspect_ratios: vec!["16:9", "9:16", "1:1"],
            max_images: 30,
            max_videos: 10,
            max_audios: 10,
            max_total_references: None,
            reference_image_count: None,
            max_reference_duration_s: None,
            note: "模型标识以当前 API Key 返回的目录为准。",
        },
    ]
}

fn minimax_capability(
    id: &'static str,
    label: &'static str,
    resolution: &'static str,
) -> VideoModelCapability {
    VideoModelCapability {
        id,
        label,
        modes: vec!["text", "reference"],
        min_duration_s: 5,
        max_duration_s: 15,
        duration_options: range(5, 15),
        resolutions: vec![resolution],
        aspect_ratios: vec!["16:9", "9:16", "1:1"],
        max_images: 5,
        max_videos: 3,
        max_audios: 1,
        max_total_references: None,
        reference_image_count: None,
        max_reference_duration_s: None,
        note: "MiniMax H3 多模态视频；最多 5 图、3 视频、1 音频。",
    }
}

fn kling_capability(
    id: &'static str,
    label: &'static str,
    resolutions: Vec<&'static str>,
) -> VideoModelCapability {
    VideoModelCapability {
        id,
        label,
        modes: vec!["text", "first_frame", "reference"],
        min_duration_s: 3,
        max_duration_s: 15,
        duration_options: range(3, 15),
        resolutions,
        aspect_ratios: vec!["16:9", "9:16", "1:1"],
        max_images: 9,
        max_videos: 3,
        max_audios: 3,
        max_total_references: None,
        reference_image_count: None,
        max_reference_duration_s: None,
        note: "按秒生成；清晰度与时长必须使用模型支持值。",
    }
}

fn grok_capability(
    id: &'static str,
    label: &'static str,
    max_reference_duration_s: Option<u32>,
) -> VideoModelCapability {
    VideoModelCapability {
        id,
        label,
        modes: vec!["text", "first_frame", "reference"],
        min_duration_s: 1,
        max_duration_s: 15,
        duration_options: range(1, 15),
        resolutions: vec!["480p", "720p", "1080p"],
        aspect_ratios: vec!["16:9", "9:16", "1:1"],
        max_images: 7,
        max_videos: 0,
        max_audios: 0,
        max_total_references: None,
        reference_image_count: Some(7),
        max_reference_duration_s,
        note: "首帧模式 1 图；参考图模式必须恰好 7 图。",
    }
}

fn drama_capability(
    id: &'static str,
    label: &'static str,
    resolutions: Vec<&'static str>,
) -> VideoModelCapability {
    VideoModelCapability {
        id,
        label,
        modes: vec!["text", "reference"],
        min_duration_s: 5,
        max_duration_s: 15,
        duration_options: vec![5, 10, 15],
        resolutions,
        aspect_ratios: vec!["16:9", "9:16", "1:1", "4:3", "3:4", "21:9"],
        max_images: 9,
        max_videos: 3,
        max_audios: 3,
        max_total_references: Some(12),
        reference_image_count: None,
        max_reference_duration_s: None,
        note: "支持多模态参考；使用视频或音频参考时必须同时提供图片。",
    }
}

fn standard_capability(id: &'static str, label: &'static str) -> VideoModelCapability {
    VideoModelCapability {
        id,
        label,
        modes: vec!["text", "first_frame"],
        min_duration_s: 1,
        max_duration_s: 15,
        duration_options: range(1, 15),
        resolutions: vec!["480p", "720p"],
        aspect_ratios: vec!["16:9", "9:16", "1:1"],
        max_images: 1,
        max_videos: 0,
        max_audios: 0,
        max_total_references: None,
        reference_image_count: None,
        max_reference_duration_s: None,
        note: "具体能力以当前 API Key 返回结果为准。",
    }
}

fn capability_for(model: &str) -> Result<VideoModelCapability, String> {
    model_capabilities()
        .into_iter()
        .find(|item| item.id == model)
        .ok_or_else(|| {
            format!("当前客户端没有视频模型 {model} 的能力定义，请更新客户端或改用已验证模型")
        })
}

fn normalized_mode(req: &VideoGenRequest) -> &str {
    req.mode
        .as_deref()
        .filter(|mode| !mode.trim().is_empty())
        .unwrap_or(if req.images.is_empty() {
            "text"
        } else {
            "first_frame"
        })
}

fn validate_urls(values: &[String], label: &str) -> Result<(), String> {
    for value in values {
        validate_public_https_url(value)
            .map_err(|reason| format!("{label}必须是公网 HTTPS URL：{value}（{reason}）"))?;
    }
    Ok(())
}

fn validate_public_https_url(value: &str) -> Result<(), &'static str> {
    let url = reqwest::Url::parse(value.trim()).map_err(|_| "URL 格式无效")?;
    if url.scheme() != "https" {
        return Err("只允许 HTTPS");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL 不能包含用户名或密码");
    }
    let host = url.host_str().ok_or("缺少主机名")?.to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".lan")
    {
        return Err("不能使用本机或内网主机名");
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        if !is_public_ip(address) {
            return Err("不能使用内网、回环、链路本地或保留 IP");
        }
    } else if !host.contains('.') {
        return Err("主机名必须是公网域名");
    }
    Ok(())
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    !(address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_unspecified()
        || address.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1])))
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    let first = address.segments()[0];
    !(address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || first & 0xfe00 == 0xfc00
        || first & 0xffc0 == 0xfe80)
}

fn validate_single_duration(
    duration_s: u32,
    min: u32,
    max: u32,
    options: &[u32],
) -> Result<u32, String> {
    if duration_s < min || duration_s > max {
        return Err(format!(
            "当前模型单镜时长必须为 {min}–{max} 秒；不会自动拆分 {duration_s} 秒镜头"
        ));
    }
    if !options.is_empty() && !options.contains(&duration_s) {
        return Err(format!(
            "当前模型不支持 {duration_s} 秒，可选值：{options:?}；不会自动拆分镜头"
        ));
    }
    Ok(duration_s)
}

fn validate_and_plan(req: &VideoGenRequest) -> Result<u32, String> {
    let cap = capability_for(&req.model)?;
    let mode = normalized_mode(req);
    if !cap.modes.contains(&mode) {
        return Err(format!("模型 {} 不支持生成模式 {mode}", req.model));
    }
    validate_urls(&req.images, "参考图片")?;
    validate_urls(&req.videos, "参考视频")?;
    validate_urls(&req.audios, "参考音频")?;
    if req.images.len() > cap.max_images
        || req.videos.len() > cap.max_videos
        || req.audios.len() > cap.max_audios
    {
        return Err(format!(
            "参考素材超过模型上限（图片 {}/{}, 视频 {}/{}, 音频 {}/{})",
            req.images.len(),
            cap.max_images,
            req.videos.len(),
            cap.max_videos,
            req.audios.len(),
            cap.max_audios
        ));
    }
    if cap
        .max_total_references
        .is_some_and(|maximum| req.images.len() + req.videos.len() + req.audios.len() > maximum)
    {
        return Err(format!(
            "{} 的参考素材总数最多为 {} 项",
            cap.label,
            cap.max_total_references.unwrap_or_default()
        ));
    }
    match mode {
        "text" if !req.images.is_empty() || !req.videos.is_empty() || !req.audios.is_empty() => {
            return Err("纯文本模式不能携带参考素材，请切换生成模式".into());
        }
        "first_frame" if req.images.len() != 1 => {
            return Err("首帧模式需要且只能提供 1 张参考图片".into())
        }
        "reference" if req.images.is_empty() && req.videos.is_empty() && req.audios.is_empty() => {
            return Err("多素材参考模式至少需要一个参考素材 URL".into());
        }
        _ => {}
    }
    if mode == "reference" {
        if let Some(required) = cap.reference_image_count {
            if req.images.len() != required {
                return Err(format!(
                    "{} 的参考图模式必须恰好提供 {required} 张图片",
                    cap.label
                ));
            }
        }
    }
    if req.model == "drama-video-v2"
        && (!req.videos.is_empty() || !req.audios.is_empty())
        && req.images.is_empty()
    {
        return Err("Drama Video 使用视频或音频参考时，必须同时提供至少一张参考图".into());
    }
    if let Some(resolution) = req.resolution.as_deref() {
        if !cap.resolutions.is_empty() && !cap.resolutions.contains(&resolution) {
            return Err(format!("{} 不支持分辨率 {resolution}", cap.label));
        }
    }
    if let Some(ratio) = req.aspect_ratio.as_deref() {
        if !cap.aspect_ratios.is_empty() && !cap.aspect_ratios.contains(&ratio) {
            return Err(format!("{} 不支持画幅 {ratio}", cap.label));
        }
    }
    let max = if mode == "reference" {
        cap.max_reference_duration_s.unwrap_or(cap.max_duration_s)
    } else {
        cap.max_duration_s
    };
    validate_single_duration(
        req.duration_s,
        cap.min_duration_s,
        max,
        &cap.duration_options,
    )
}

/// Generate exactly one independent video asset for one storyboard shot.
/// Unsupported durations fail closed; this layer never splits or concatenates shots.
pub async fn generate_segments(
    cfg: &ConfigState,
    req: &VideoGenRequest,
    out_dir: &Path,
) -> Result<Vec<AssetRef>, String> {
    if cfg.video_api_url.is_empty() || cfg.video_api_key.is_empty() {
        return Err("未配置视频 API（请在设置中填写视频地址与 Key）".into());
    }
    std::fs::create_dir_all(out_dir).map_err(|e| format!("创建目录失败: {e}"))?;

    let request_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    let duration_s = validate_and_plan(req)?;
    crate::logging::info(
        "video.request.start",
        json!({
            "requestId": request_id,
            "model": req.model,
            "endpoint": crate::logging::safe_url(&cfg.video_api_url),
            "durationS": duration_s,
            "aspectRatio": req.aspect_ratio,
            "resolution": req.resolution,
            "mode": normalized_mode(req),
            "referenceCounts": { "images": req.images.len(), "videos": req.videos.len(), "audios": req.audios.len() },
            "promptStats": crate::logging::text_stats(&req.prompt),
        }),
    );
    let client = reqwest::Client::new();
    let (provider_task_id, bytes) = match generate_segment(
        &client,
        cfg,
        req,
        duration_s,
        &request_id,
        0,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            crate::logging::error(
                "video.request.end",
                json!({ "requestId": request_id, "status": "error", "durationMs": started.elapsed().as_millis(), "error": crate::logging::error_text(&error) }),
            );
            return Err(error);
        }
    };
    let local_file_id = Uuid::new_v4().to_string();
    let path: PathBuf = out_dir.join(format!("video_{local_file_id}.mp4"));
    std::fs::write(&path, &bytes).map_err(|e| format!("保存视频失败: {e}"))?;
    let asset = AssetRef {
        id: provider_asset_id(&provider_task_id),
        kind: "video".into(),
        path: path.display().to_string(),
        width: None,
        height: None,
        duration_s: Some(duration_s as f64),
        format: Some("mp4".into()),
    };
    crate::logging::info(
        "video.request.end",
        json!({ "requestId": request_id, "status": "success", "durationMs": started.elapsed().as_millis(), "assetCount": 1, "bytes": bytes.len(), "path": path.display().to_string() }),
    );
    Ok(vec![asset])
}

fn provider_asset_id(task_id: &str) -> String {
    format!("zzone:{task_id}")
}

async fn generate_segment(
    client: &reqwest::Client,
    cfg: &ConfigState,
    req: &VideoGenRequest,
    duration_s: u32,
    request_id: &str,
    segment_index: usize,
) -> Result<(String, Vec<u8>), String> {
    let task_id = create_task(client, cfg, req, duration_s, request_id, segment_index).await?;
    wait_task(client, cfg, &task_id, request_id, segment_index).await?;
    let bytes = download_content(client, cfg, &task_id, request_id, segment_index).await?;
    Ok((task_id, bytes))
}

fn request_body(req: &VideoGenRequest, duration_s: u32) -> Value {
    let mut body = json!({
        "model": req.model,
        "prompt": req.prompt,
        "seconds": duration_s.to_string(),
        "resolution": req.resolution.clone().unwrap_or_else(|| "720p".into()),
    });
    // In first-frame mode the source image determines the frame geometry;
    // several gateway models reject an additional aspect_ratio field.
    if normalized_mode(req) != "first_frame" {
        body["aspect_ratio"] = json!(req.aspect_ratio.clone().unwrap_or_else(|| "16:9".into()));
    }
    if !req.images.is_empty() {
        body["images"] = json!(req.images);
    }
    if !req.videos.is_empty() {
        body["videos"] = json!(req.videos);
    }
    if !req.audios.is_empty() {
        body["audios"] = json!(req.audios);
    }
    if req.model.starts_with("grok-imagine-video") {
        body["video_mode"] = json!(normalized_mode(req));
    }
    body
}

/// 提交异步视频任务（文档：POST /v1/videos），返回任务 id。
async fn create_task(
    client: &reqwest::Client,
    cfg: &ConfigState,
    req: &VideoGenRequest,
    duration_s: u32,
    request_id: &str,
    segment_index: usize,
) -> Result<String, String> {
    let body = request_body(req, duration_s);

    let started = Instant::now();
    let resp = client
        .post(&cfg.video_api_url)
        .bearer_auth(&cfg.video_api_key)
        .json(&body)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| format!("创建视频任务失败: {e}"))?;
    let status = resp.status();
    let text = read_limited_text(resp, 1024 * 1024, "视频创建响应")
        .await
        .unwrap_or_default();
    crate::logging::info(
        "video.task.create_response",
        json!({ "requestId": request_id, "segmentIndex": segment_index, "status": status.as_u16(), "durationMs": started.elapsed().as_millis() }),
    );
    if !status.is_success() {
        return Err(format!(
            "创建视频任务返回 {status}: {}",
            text.chars().take(400).collect::<String>()
        ));
    }
    let j: Value = serde_json::from_str(&text).map_err(|e| {
        format!(
            "解析创建响应失败: {e}: {}",
            text.chars().take(200).collect::<String>()
        )
    })?;
    let id = j
        .get("id")
        .or_else(|| j.get("task_id"))
        .or_else(|| j.get("taskId"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "创建响应缺少任务 id: {}",
                text.chars().take(200).collect::<String>()
            )
        })?;
    if id.is_empty()
        || id.len() > 256
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.:".contains(character))
    {
        return Err("创建响应中的任务 id 格式无效".to_string());
    }
    Ok(id.to_string())
}

/// 轮询任务直到 completed（文档建议先等 3-5 秒，之后 5-10 秒间隔）。
async fn wait_task(
    client: &reqwest::Client,
    cfg: &ConfigState,
    task_id: &str,
    request_id: &str,
    segment_index: usize,
) -> Result<(), String> {
    let url = format!("{}/{}", cfg.video_api_url.trim_end_matches('/'), task_id);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let deadline = Instant::now() + Duration::from_secs(1800);
    let mut poll_count = 0u32;
    let mut transient_failures = 0u32;
    loop {
        poll_count += 1;
        let poll_started = Instant::now();
        let response = client
            .get(&url)
            .bearer_auth(&cfg.video_api_key)
            .timeout(Duration::from_secs(60))
            .send()
            .await;
        let resp = match response {
            Ok(response) => response,
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "视频任务轮询超时（task_id={task_id}）：最后一次网络错误：{error}"
                    ));
                }
                transient_failures += 1;
                let delay = transient_poll_backoff_seconds(transient_failures);
                crate::logging::warn(
                    "video.task.poll_retry",
                    json!({ "requestId": request_id, "segmentIndex": segment_index, "taskId": task_id, "pollCount": poll_count, "reason": "network", "retryInS": delay, "error": error.to_string() }),
                );
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }
        };
        let status = resp.status();
        let text = read_limited_text(resp, 1024 * 1024, "视频轮询响应")
            .await
            .unwrap_or_default();
        if !status.is_success() {
            if is_transient_poll_status(status) && Instant::now() < deadline {
                transient_failures += 1;
                let delay = transient_poll_backoff_seconds(transient_failures);
                crate::logging::warn(
                    "video.task.poll_retry",
                    json!({ "requestId": request_id, "segmentIndex": segment_index, "taskId": task_id, "pollCount": poll_count, "reason": "http", "httpStatus": status.as_u16(), "retryInS": delay }),
                );
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }
            return Err(format!(
                "查询任务返回 {status}: {}",
                text.chars().take(300).collect::<String>()
            ));
        }
        transient_failures = 0;
        let j: Value = serde_json::from_str(&text).map_err(|e| {
            format!(
                "解析任务状态失败: {e}: {}",
                text.chars().take(200).collect::<String>()
            )
        })?;
        let st = j
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        crate::logging::debug(
            "video.task.poll",
            json!({ "requestId": request_id, "segmentIndex": segment_index, "taskId": task_id, "pollCount": poll_count, "taskStatus": st, "httpStatus": status.as_u16(), "durationMs": poll_started.elapsed().as_millis() }),
        );
        match st.as_str() {
            "completed" | "succeeded" | "success" => return Ok(()),
            "failed" | "error" | "cancelled" | "expired" => {
                let err = provider_error_message(&j)
                    .unwrap_or_else(|| "服务端未返回失败原因".to_string());
                return Err(format!("视频任务失败（{st}）: {err}"));
            }
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("视频任务超时（task_id={task_id}，status={st}）"));
        }
        tokio::time::sleep(Duration::from_secs(7)).await;
    }
}

fn provider_error_message(value: &Value) -> Option<String> {
    [
        value.pointer("/error/message"),
        value.get("error"),
        value.get("failure_reason"),
        value.get("failReason"),
        value.get("message"),
    ]
    .into_iter()
    .flatten()
    .find_map(|candidate| match candidate {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        Value::Object(_) | Value::Array(_) => Some(candidate.to_string()),
        _ => None,
    })
}

fn is_transient_poll_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status.as_u16(),
        408 | 425 | 429 | 500 | 502 | 503 | 504 | 520..=529
    )
}

fn transient_poll_backoff_seconds(failure_count: u32) -> u64 {
    u64::from(failure_count.clamp(1, 6)) * 5
}

/// 任务完成后经 /content 端点下载成片二进制。
async fn download_content(
    client: &reqwest::Client,
    cfg: &ConfigState,
    task_id: &str,
    request_id: &str,
    segment_index: usize,
) -> Result<Vec<u8>, String> {
    let url = format!(
        "{}/{}/content",
        cfg.video_api_url.trim_end_matches('/'),
        task_id
    );
    let started = Instant::now();
    for attempt in 1..=3u32 {
        let response = client
            .get(&url)
            .bearer_auth(&cfg.video_api_key)
            .timeout(Duration::from_secs(600))
            .send()
            .await;
        let resp = match response {
            Ok(response) => response,
            Err(error) if attempt < 3 => {
                let delay = transient_poll_backoff_seconds(attempt);
                crate::logging::warn(
                    "video.download.retry",
                    json!({ "requestId": request_id, "segmentIndex": segment_index, "taskId": task_id, "attempt": attempt, "reason": "network", "retryInS": delay, "error": error.to_string() }),
                );
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }
            Err(error) => return Err(format!("下载视频失败: {error}")),
        };
        let status = resp.status();
        if !status.is_success() {
            let text = read_limited_text(resp, 1024 * 1024, "视频下载错误响应")
                .await
                .unwrap_or_default();
            if attempt < 3 && is_transient_poll_status(status) {
                let delay = transient_poll_backoff_seconds(attempt);
                crate::logging::warn(
                    "video.download.retry",
                    json!({ "requestId": request_id, "segmentIndex": segment_index, "taskId": task_id, "attempt": attempt, "reason": "http", "httpStatus": status.as_u16(), "retryInS": delay }),
                );
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }
            return Err(format!(
                "下载视频返回 {status}: {}",
                text.chars().take(300).collect::<String>()
            ));
        }
        if resp
            .content_length()
            .is_some_and(|length| length > MAX_VIDEO_BYTES as u64)
        {
            return Err("视频内容超过 512 MiB 大小限制".into());
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("读取视频内容失败: {e}"))?;
        if bytes.len() > MAX_VIDEO_BYTES {
            return Err("视频内容超过 512 MiB 大小限制".into());
        }
        if !crate::assets::is_valid_mp4_bytes(&bytes) {
            return Err("视频接口返回的内容不是有效 MP4".into());
        }
        crate::logging::info(
            "video.download",
            json!({ "requestId": request_id, "segmentIndex": segment_index, "taskId": task_id, "attempt": attempt, "durationMs": started.elapsed().as_millis(), "bytes": bytes.len() }),
        );
        return Ok(bytes.to_vec());
    }
    Err("下载视频失败：重试次数耗尽".into())
}

async fn read_limited_text(
    response: reqwest::Response,
    limit: usize,
    label: &str,
) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!("{label}超过 1 MiB 大小限制"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("读取{label}失败: {error}"))?;
    if bytes.len() > limit {
        return Err(format!("{label}超过 1 MiB 大小限制"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        is_transient_poll_status, model_capabilities, provider_asset_id, provider_error_message,
        request_body, transient_poll_backoff_seconds, validate_and_plan, validate_single_duration,
        VideoGenRequest,
    };

    fn request(model: &str, duration_s: u32) -> VideoGenRequest {
        VideoGenRequest {
            model: model.into(),
            prompt: "镜头缓慢推进".into(),
            duration_s,
            aspect_ratio: Some("16:9".into()),
            resolution: Some("720p".into()),
            mode: Some("text".into()),
            images: vec![],
            videos: vec![],
            audios: vec![],
        }
    }

    #[test]
    fn keeps_native_thirty_second_seedance_request_single() {
        assert_eq!(validate_and_plan(&request("video-ds-2.5", 30)).unwrap(), 30);
    }

    #[test]
    fn rejects_unsupported_duration_instead_of_splitting_a_shot() {
        let error = validate_single_duration(31, 4, 30, &(4..=30).collect::<Vec<_>>()).unwrap_err();
        assert!(error.contains("不会自动拆分"));
        assert!(validate_and_plan(&request("video-ds-2.0", 16))
            .unwrap_err()
            .contains("不会自动拆分"));
    }

    #[test]
    fn grok_reference_mode_requires_exactly_seven_https_images() {
        let mut req = request("grok-imagine-video", 10);
        req.mode = Some("reference".into());
        req.images = (0..6)
            .map(|i| format!("https://example.com/{i}.png"))
            .collect();
        assert!(validate_and_plan(&req)
            .unwrap_err()
            .contains("恰好提供 7 张"));
        req.images.push("https://example.com/6.png".into());
        assert_eq!(validate_and_plan(&req).unwrap(), 10);
    }

    #[test]
    fn rejects_local_reference_paths() {
        let mut req = request("grok-imagine-video", 5);
        req.mode = Some("first_frame".into());
        req.images = vec![r"C:\images\ref.png".into()];
        assert!(validate_and_plan(&req).unwrap_err().contains("HTTPS URL"));
    }

    #[test]
    fn builds_zzone_multimodal_arrays_and_grok_mode() {
        let mut req = request("grok-imagine-video", 8);
        req.mode = Some("first_frame".into());
        req.images = vec!["https://example.com/first.png".into()];
        let body = request_body(&req, 8);
        assert_eq!(body["seconds"], "8");
        assert_eq!(body["resolution"], "720p");
        assert_eq!(body["video_mode"], "first_frame");
        assert_eq!(body["images"][0], "https://example.com/first.png");
        assert!(body.get("aspect_ratio").is_none());
        assert!(body.get("videos").is_none());
        assert!(body.get("audios").is_none());
    }

    #[test]
    fn rejects_non_public_reference_urls() {
        for url in [
            "http://cdn.example.com/ref.png",
            "https://localhost/ref.png",
            "https://127.0.0.1/ref.png",
            "https://192.168.1.2/ref.png",
            "https://user:secret@cdn.example.com/ref.png",
        ] {
            let mut req = request("grok-imagine-video", 5);
            req.mode = Some("first_frame".into());
            req.images = vec![url.into()];
            assert!(validate_and_plan(&req).unwrap_err().contains("公网 HTTPS"));
        }
    }

    #[test]
    fn drama_models_enforce_the_documented_total_reference_limit() {
        for model in ["drama-video-v2", "drama-video-v2-fast"] {
            let mut req = request(model, 5);
            req.mode = Some("reference".into());
            req.images = (0..9)
                .map(|i| format!("https://cdn.example.com/{i}.png"))
                .collect();
            req.videos = (0..3)
                .map(|i| format!("https://cdn.example.com/{i}.mp4"))
                .collect();
            req.audios = vec!["https://cdn.example.com/a.mp3".into()];
            assert!(validate_and_plan(&req)
                .unwrap_err()
                .contains("总数最多为 12"));
        }
    }

    #[test]
    fn only_standard_drama_requires_an_image_with_video_reference() {
        let mut standard = request("drama-video-v2", 5);
        standard.mode = Some("reference".into());
        standard.videos = vec!["https://cdn.example.com/ref.mp4".into()];
        assert!(validate_and_plan(&standard)
            .unwrap_err()
            .contains("至少一张参考图"));

        let mut fast = request("drama-video-v2-fast", 5);
        fast.mode = Some("reference".into());
        fast.videos = vec!["https://cdn.example.com/ref.mp4".into()];
        assert_eq!(validate_and_plan(&fast).unwrap(), 5);
    }

    #[test]
    fn recognizes_live_minimax_aliases_and_fails_closed_for_unknown_models() {
        let ids = model_capabilities()
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&"minimax-h3-2k"));
        assert!(ids.contains(&"minimax-h3-4k"));
        let mut req = request("minimax-h3-4k", 5);
        req.resolution = Some("4k".into());
        assert_eq!(validate_and_plan(&req).unwrap(), 5);
        assert!(validate_and_plan(&request("wan3-720p", 5))
            .unwrap_err()
            .contains("没有视频模型"));
        assert_eq!(provider_asset_id("video_abc123"), "zzone:video_abc123");
    }

    #[test]
    fn retries_only_transient_poll_failures_with_bounded_backoff() {
        for code in [408, 425, 429, 500, 502, 503, 504, 520, 524, 529] {
            assert!(is_transient_poll_status(
                reqwest::StatusCode::from_u16(code).unwrap()
            ));
        }
        for code in [400, 401, 403, 404, 409, 422] {
            assert!(!is_transient_poll_status(
                reqwest::StatusCode::from_u16(code).unwrap()
            ));
        }
        assert_eq!(transient_poll_backoff_seconds(1), 5);
        assert_eq!(transient_poll_backoff_seconds(3), 15);
        assert_eq!(transient_poll_backoff_seconds(99), 30);
        assert_eq!(
            provider_error_message(&serde_json::json!({ "error": { "message": "额度不足" } })),
            Some("额度不足".to_string())
        );
        assert_eq!(
            provider_error_message(&serde_json::json!({ "failure_reason": "参考图无效" })),
            Some("参考图无效".to_string())
        );
    }
}
