use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::ConfigState;
use crate::model::AssetRef;

const MAX_VIDEO_BYTES: usize = 512 * 1024 * 1024;

pub struct VideoGenRequest {
    pub model: String,
    pub prompt: String,
    pub duration_s: u32,
    pub aspect_ratio: Option<String>,
    pub resolution: Option<String>,
    /// 公网 HTTP(S) 参考素材 URL；网关不接受本地文件路径。
    pub reference: Option<String>,
}

fn validate_reference(reference: Option<&str>) -> Result<Option<&str>, String> {
    let Some(value) = reference.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if value.starts_with("http://") || value.starts_with("https://") {
        Ok(Some(value))
    } else {
        Err("视频参考素材需公网 HTTP(S) URL，网关不接受本地文件".into())
    }
}

fn segment_durations(total: u32) -> Vec<u32> {
    let total = total.max(1);
    let per = 15u32.min(total);
    let segs = total.div_ceil(per);
    (0..segs)
        .map(|i| {
            if i == segs - 1 {
                total - per * (segs - 1)
            } else {
                per
            }
        })
        .collect()
}

/// Generate video segments via the async gateway API. Each segment is one
/// gateway task (create → poll → download); durations beyond 15s are split
/// into multiple segments. Concatenation and music muxing happen in the
/// frontend via mediabunny — the Rust side stays a single, dependency-free exe.
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
    let durations = segment_durations(req.duration_s);
    crate::logging::info(
        "video.request.start",
        json!({
            "requestId": request_id,
            "model": req.model,
            "endpoint": crate::logging::safe_url(&cfg.video_api_url),
            "durationS": req.duration_s,
            "segmentDurations": durations,
            "aspectRatio": req.aspect_ratio,
            "resolution": req.resolution,
            "hasReference": req.reference.as_deref().is_some_and(|value| !value.trim().is_empty()),
            "promptStats": crate::logging::text_stats(&req.prompt),
        }),
    );
    let client = reqwest::Client::new();
    let mut assets = Vec::new();
    for (i, d) in durations.into_iter().enumerate() {
        let segment_started = Instant::now();
        crate::logging::info(
            "video.segment.start",
            json!({ "requestId": request_id, "segmentIndex": i, "durationS": d }),
        );
        let bytes = match generate_segment(&client, cfg, req, d, &request_id, i).await {
            Ok(bytes) => bytes,
            Err(error) => {
                crate::logging::error(
                    "video.request.end",
                    json!({ "requestId": request_id, "status": "error", "segmentIndex": i, "durationMs": started.elapsed().as_millis(), "error": crate::logging::error_text(&error) }),
                );
                return Err(error);
            }
        };
        let id = Uuid::new_v4().to_string();
        let path: PathBuf = out_dir.join(format!("seg_{i:02}_{id}.mp4"));
        std::fs::write(&path, &bytes).map_err(|e| format!("保存视频段失败: {e}"))?;
        assets.push(AssetRef {
            id,
            kind: "video".into(),
            path: path.display().to_string(),
            width: None,
            height: None,
            duration_s: Some(d as f64),
            format: Some("mp4".into()),
        });
        crate::logging::info(
            "video.segment.end",
            json!({ "requestId": request_id, "segmentIndex": i, "status": "success", "durationMs": segment_started.elapsed().as_millis(), "bytes": bytes.len(), "path": path.display().to_string() }),
        );
    }
    crate::logging::info(
        "video.request.end",
        json!({ "requestId": request_id, "status": "success", "durationMs": started.elapsed().as_millis(), "assetCount": assets.len() }),
    );
    Ok(assets)
}

async fn generate_segment(
    client: &reqwest::Client,
    cfg: &ConfigState,
    req: &VideoGenRequest,
    duration_s: u32,
    request_id: &str,
    segment_index: usize,
) -> Result<Vec<u8>, String> {
    let task_id = create_task(client, cfg, req, duration_s, request_id, segment_index).await?;
    wait_task(client, cfg, &task_id, request_id, segment_index).await?;
    download_content(client, cfg, &task_id, request_id, segment_index).await
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
    let mut body = json!({
        "model": req.model,
        "prompt": req.prompt,
        "seconds": duration_s.to_string(),
        "aspect_ratio": req.aspect_ratio.clone().unwrap_or_else(|| "16:9".into()),
        "resolution": req.resolution.clone().unwrap_or_else(|| "480p".into()),
    });
    if let Some(url) = validate_reference(req.reference.as_deref())? {
        body["images"] = json!([url]);
        if req.model.starts_with("grok-imagine-video") {
            body["video_mode"] = json!("first_frame");
        }
    } else if req.model.starts_with("grok-imagine-video") {
        body["video_mode"] = json!("text");
    }

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
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    let mut poll_count = 0u32;
    loop {
        poll_count += 1;
        let poll_started = Instant::now();
        let resp = client
            .get(&url)
            .bearer_auth(&cfg.video_api_key)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| format!("查询任务状态失败: {e}"))?;
        let status = resp.status();
        let text = read_limited_text(resp, 1024 * 1024, "视频轮询响应")
            .await
            .unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "查询任务返回 {status}: {}",
                text.chars().take(300).collect::<String>()
            ));
        }
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
            "failed" | "error" | "cancelled" => {
                let err = j.get("error").map(|v| v.to_string()).unwrap_or_default();
                return Err(format!("视频任务失败: {err}"));
            }
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("视频任务超时（task_id={task_id}，status={st}）"));
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
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
    let resp = client
        .get(&url)
        .bearer_auth(&cfg.video_api_key)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .map_err(|e| format!("下载视频失败: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = read_limited_text(resp, 1024 * 1024, "视频下载错误响应")
            .await
            .unwrap_or_default();
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
        json!({ "requestId": request_id, "segmentIndex": segment_index, "taskId": task_id, "durationMs": started.elapsed().as_millis(), "bytes": bytes.len() }),
    );
    Ok(bytes.to_vec())
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
    use super::{segment_durations, validate_reference};

    #[test]
    fn splits_long_video_into_max_15_second_segments() {
        assert_eq!(segment_durations(0), vec![1]);
        assert_eq!(segment_durations(15), vec![15]);
        assert_eq!(segment_durations(31), vec![15, 15, 1]);
    }

    #[test]
    fn only_accepts_public_http_reference_urls() {
        assert_eq!(validate_reference(None).unwrap(), None);
        assert_eq!(
            validate_reference(Some(" https://example.com/ref.png ")).unwrap(),
            Some("https://example.com/ref.png")
        );
        assert!(validate_reference(Some(r"C:\images\ref.png")).is_err());
    }
}
