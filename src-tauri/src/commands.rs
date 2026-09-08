use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::{self, ConfigStatus};
use crate::model::{AssetRef, ProviderInfo, RunNodeRequest, RunResult};
use crate::AppState;

const MAX_REFERENCE_IMAGE_BYTES: usize = 50 * 1024 * 1024;
const MAX_TEXT_ASSET_BYTES: usize = 20 * 1024 * 1024;
pub(crate) const MAX_MEDIA_ASSET_BYTES: usize = 512 * 1024 * 1024;
const MAX_PROMPTLIB_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const PROMPTLIB_IMAGE_HOST: &str = "pub-7ecb2a3a62b94375a9abd336abf0bcc6.r2.dev";
const PROMPTLIB_IMAGE_PREFIX: &str = "/img-case-assets/images/";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    name: String,
    version: String,
    platform: String,
}

#[tauri::command]
pub fn app_info(app: tauri::AppHandle) -> AppInfo {
    AppInfo {
        name: app.package_info().name.to_string(),
        version: app.package_info().version.to_string(),
        platform: std::env::consts::OS.to_string(),
    }
}

#[tauri::command]
pub fn data_dir() -> String {
    let path = crate::paths::data_dir_str();
    crate::logging::debug("command.data_dir", serde_json::json!({ "path": path }));
    path
}

#[tauri::command]
pub fn logs_dir() -> String {
    let path = crate::paths::logs_dir()
        .to_string_lossy()
        .replace('\\', "/");
    crate::logging::debug("command.logs_dir", serde_json::json!({ "path": path }));
    path
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientLogEntry {
    level: String,
    event: String,
    fields: serde_json::Value,
}

#[tauri::command(async)]
pub fn client_logs(entries: Vec<ClientLogEntry>) {
    crate::logging::client_batch(
        entries
            .into_iter()
            .take(100)
            .map(|entry| (entry.level, entry.event, entry.fields))
            .collect(),
    );
}

/// Copy a picked image into the centralized `assets/引用图` folder and return
/// the new path (so reference images live alongside outputs).
#[tauri::command(async)]
pub async fn import_ref_image(src: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || import_ref_image_inner(&src))
        .await
        .map_err(|error| format!("导入参考图任务失败: {error}"))?
}

fn import_ref_image_inner(src: &str) -> Result<String, String> {
    crate::logging::info(
        "asset.reference_import.start",
        serde_json::json!({ "sourcePath": src }),
    );
    let src_path = Path::new(src);
    if !src_path.exists() {
        return Err(format!("文件不存在: {src}"));
    }
    let bytes = std::fs::read(src_path).map_err(|e| format!("读取失败: {e}"))?;
    if bytes.len() > MAX_REFERENCE_IMAGE_BYTES {
        return Err("参考图超过 50 MiB 大小限制".into());
    }
    let format =
        crate::assets::detect_format_checked(&bytes).ok_or("参考图格式不受支持或文件已损坏")?;
    let dir = crate::paths::assets_dir().join("引用图");
    let asset = crate::assets::save_bytes(&dir, "image", &bytes, format)?;
    crate::logging::info(
        "asset.reference_import.end",
        serde_json::json!({ "status": "success", "assetPath": asset.path, "bytes": bytes.len(), "format": format }),
    );
    Ok(asset.path)
}

/// Persist a produced batch of assets in one transaction. The renderer used to
/// issue one `db_execute` per asset (one IPC + one implicit transaction each).
#[tauri::command(async)]
pub fn persist_assets_batch(
    db: tauri::State<'_, crate::db::DbState>,
    assets: Vec<crate::model::AssetRef>,
    source: String,
    model: Option<String>,
    project_id: Option<String>,
    params: serde_json::Value,
) -> Result<(), String> {
    if assets.is_empty() {
        return Ok(());
    }
    crate::db::with_connection(&db, |connection| {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("开始资产入库事务失败: {error}"))?;
        let metadata = serde_json::json!({
            "source": source,
            "model": model,
            "projectId": project_id,
            "params": params,
        })
        .to_string();
        let created_at = chrono::Utc::now().timestamp_millis();
        for asset in &assets {
            transaction
                .execute(
                    "INSERT INTO assets (id, kind, path, width, height, duration_s, format, created_at, metadata) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, path = excluded.path, width = excluded.width, \
                       height = excluded.height, duration_s = excluded.duration_s, format = excluded.format, \
                       metadata = excluded.metadata",
                    rusqlite::params![
                        asset.id,
                        asset.kind,
                        asset.path,
                        asset.width,
                        asset.height,
                        asset.duration_s,
                        asset.format,
                        created_at,
                        metadata,
                    ],
                )
                .map_err(|error| format!("写入资产失败: {error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("提交资产入库失败: {error}"))
    })
}

/// Save generated text (script / storyboard / QC) as a `.md` asset.
#[tauri::command(async)]
pub fn save_text(
    state: tauri::State<'_, AppState>,
    label: String,
    text: String,
    model: Option<String>,
) -> Result<AssetRef, String> {
    let _ = (&label, &model);
    if text.len() > MAX_TEXT_ASSET_BYTES {
        return Err("文本资产超过 20 MiB 大小限制".into());
    }
    let cfg = state.cfg.read().unwrap().clone();
    save_text_with_config(&cfg, &label, &text, model.as_deref())
}

#[tauri::command(async)]
pub fn read_text_asset(state: tauri::State<'_, AppState>, path: String) -> Result<String, String> {
    let cfg = state.cfg.read().unwrap().clone();
    let requested = PathBuf::from(&path);
    let canonical = requested
        .canonicalize()
        .map_err(|error| format!("文档文件不存在或不可访问: {error}"))?;
    let allowed_roots = [crate::paths::data_dir(), cfg.output_path()];
    let allowed = allowed_roots.iter().any(|root| {
        root.canonicalize()
            .ok()
            .is_some_and(|canonical_root| canonical.starts_with(canonical_root))
    });
    if !allowed {
        return Err("文档不在应用数据或输出目录中".into());
    }
    let extension = canonical
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "md" | "txt" | "json") {
        return Err("只允许读取 md、txt、json 文档".into());
    }
    let metadata =
        std::fs::metadata(&canonical).map_err(|error| format!("读取文档信息失败: {error}"))?;
    if metadata.len() > MAX_TEXT_ASSET_BYTES as u64 {
        return Err("文档超过 20 MiB 大小限制".into());
    }
    std::fs::read_to_string(&canonical).map_err(|error| format!("读取文档失败: {error}"))
}

pub(crate) fn save_text_with_config(
    cfg: &crate::config::ConfigState,
    label: &str,
    text: &str,
    model: Option<&str>,
) -> Result<AssetRef, String> {
    crate::logging::info(
        "asset.text_save.start",
        serde_json::json!({ "label": label, "model": model, "textStats": crate::logging::text_stats(text) }),
    );
    let base = cfg.output_path();
    let tag = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = base.join("剧本").join(&tag);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建剧本目录失败: {e}"))?;
    let id = Uuid::new_v4().to_string();
    let path = dir.join(format!("{id}.md"));
    std::fs::write(&path, text).map_err(|e| format!("保存剧本失败: {e}"))?;
    let asset = AssetRef {
        id,
        kind: "text".into(),
        path: path.display().to_string(),
        width: None,
        height: None,
        duration_s: None,
        format: Some("md".into()),
    };
    crate::logging::info(
        "asset.text_save.end",
        serde_json::json!({ "status": "success", "assetId": asset.id, "path": asset.path, "bytes": text.len() }),
    );
    Ok(asset)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentVersionSaveRequest {
    label: String,
    text: String,
    model: Option<String>,
    project_id: Option<String>,
    document_id: String,
    params: Value,
    expected_head_asset_id: Option<String>,
    #[serde(default)]
    allow_branch: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentVersionSaveResult {
    asset: AssetRef,
    params: Value,
    version: i64,
}

#[tauri::command(async)]
pub fn save_document_version(
    app_state: tauri::State<'_, AppState>,
    database: tauri::State<'_, crate::db::DbState>,
    request: DocumentVersionSaveRequest,
) -> Result<DocumentVersionSaveResult, String> {
    let cfg = app_state.cfg.read().unwrap().clone();
    save_document_version_with_config(&cfg, &database, request)
}

fn save_document_version_with_config(
    cfg: &crate::config::ConfigState,
    database: &crate::db::DbState,
    request: DocumentVersionSaveRequest,
) -> Result<DocumentVersionSaveResult, String> {
    let label = request.label.trim();
    let text = request.text.trim();
    if label.is_empty() || text.is_empty() || request.document_id.trim().is_empty() {
        return Err("文档标题、内容和 documentId 不能为空".to_string());
    }
    let asset = save_text_with_config(cfg, label, text, request.model.as_deref())?;
    let asset_path = asset.path.clone();
    let result = crate::db::with_connection_mut(&database, |connection| {
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|error| format!("开始文档版本事务失败: {error}"))?;
        let operation = (|| {
            let current_head: Option<String> = connection
                .query_row(
                    "SELECT id FROM assets WHERE kind='text' AND json_extract(metadata,'$.params.documentId')=? AND COALESCE(json_extract(metadata,'$.params.videoBranch'),0) != 1 ORDER BY CAST(COALESCE(json_extract(metadata,'$.params.version'),0) AS INTEGER) DESC, created_at DESC, id DESC LIMIT 1",
                    [&request.document_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| format!("读取文档当前版本失败: {error}"))?;
            if !request.allow_branch
                && request.expected_head_asset_id.as_deref() != current_head.as_deref()
            {
                return Err(
                    "版本冲突：当前生产版已变化。请创建历史分支，或迁移到最新版本后再保存"
                        .to_string(),
                );
            }
            let version: i64 = connection
                .query_row(
                    "SELECT COALESCE(MAX(CAST(COALESCE(json_extract(metadata,'$.params.version'),0) AS INTEGER)),0)+1 FROM assets WHERE kind='text' AND json_extract(metadata,'$.params.documentId')=?",
                    [&request.document_id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("分配文档版本失败: {error}"))?;
            let mut params = request.params.clone();
            let object = params
                .as_object_mut()
                .ok_or_else(|| "文档 params 必须是对象".to_string())?;
            object.insert("documentId".to_string(), json!(request.document_id));
            object.insert("version".to_string(), json!(version));
            object.insert("text".to_string(), json!(text));
            object.insert("title".to_string(), json!(label));
            // Branch identity is a backend invariant. Do not trust callers to
            // keep allowBranch and params.videoBranch consistent.
            object.insert("videoBranch".to_string(), json!(request.allow_branch));
            let metadata = serde_json::to_string(&json!({
                "source": label,
                "model": request.model,
                "projectId": request.project_id,
                "params": params,
            }))
            .map_err(|error| format!("序列化文档历史失败: {error}"))?;
            connection
                .execute(
                    "INSERT INTO assets (id,kind,path,width,height,duration_s,format,created_at,metadata) VALUES (?,?,?,?,?,?,?,?,?)",
                    rusqlite::params![asset.id, asset.kind, asset.path, asset.width, asset.height, asset.duration_s, asset.format, chrono::Utc::now().timestamp_millis(), metadata],
                )
                .map_err(|error| format!("写入文档版本失败: {error}"))?;
            Ok((params, version))
        })();
        match operation {
            Ok(value) => {
                connection
                    .execute_batch("COMMIT")
                    .map_err(|error| format!("提交文档版本失败: {error}"))?;
                Ok(value)
            }
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    });
    match result {
        Ok((params, version)) => Ok(DocumentVersionSaveResult {
            asset,
            params,
            version,
        }),
        Err(error) => {
            let _ = std::fs::remove_file(&asset_path);
            Err(error)
        }
    }
}

/// 保存前端 mediabunny 处理后的媒体（拼接/混音结果），返回资产引用。
#[tauri::command(async)]
pub async fn save_media_asset(
    state: tauri::State<'_, AppState>,
    kind: String,
    ext: String,
    data_base64: String,
) -> Result<AssetRef, String> {
    let cfg = state.cfg.read().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let data = decode_media_base64(&data_base64)?;
        save_media_asset_with_config(&cfg, &kind, &ext, &data)
    })
    .await
    .map_err(|error| format!("保存媒体资产任务失败: {error}"))?
}

/// Decode a base64 media payload with a size pre-check, so a malformed or
/// oversized request cannot allocate an unbounded buffer first.
fn decode_media_base64(value: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let max_encoded = (MAX_MEDIA_ASSET_BYTES / 3 + 1) * 4;
    if value.len() > max_encoded {
        return Err("媒体资产超过 512 MiB 大小限制".into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| "媒体数据不是有效的 base64".to_string())
}

pub(crate) fn save_media_asset_with_config(
    cfg: &crate::config::ConfigState,
    kind: &str,
    ext: &str,
    data: &[u8],
) -> Result<AssetRef, String> {
    if data.len() > MAX_MEDIA_ASSET_BYTES {
        return Err("媒体资产超过 512 MiB 大小限制".into());
    }
    crate::logging::info(
        "asset.media_save.start",
        serde_json::json!({ "kind": kind, "extension": ext, "bytes": data.len() }),
    );
    let base = cfg.output_path();
    let tag = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = base.join(kind_folder(kind)).join("合成").join(&tag);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败: {e}"))?;
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("无效的媒体扩展名".into());
    }
    let allowed = match kind {
        "image" => matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif"),
        "video" => matches!(ext.as_str(), "mp4" | "webm" | "mov"),
        _ => false,
    };
    if !allowed {
        return Err(format!("不支持的媒体类型或扩展名: {kind}.{ext}"));
    }
    if kind == "image" && !crate::assets::is_valid_image_bytes(data) {
        return Err("图片资产内容格式校验失败".into());
    }
    if kind == "video" && !crate::assets::is_valid_video_bytes(data, &ext) {
        return Err(format!("视频资产内容格式校验失败: {ext}"));
    }
    let id = Uuid::new_v4().to_string();
    let path = dir.join(format!("{id}.{ext}"));
    std::fs::write(&path, data).map_err(|e| format!("写入文件失败: {e}"))?;
    let asset = AssetRef {
        id,
        kind: kind.to_string(),
        path: path.display().to_string(),
        width: None,
        height: None,
        duration_s: None,
        format: Some(ext),
    };
    crate::logging::info(
        "asset.media_save.end",
        serde_json::json!({ "status": "success", "assetId": asset.id, "path": asset.path, "bytes": data.len() }),
    );
    Ok(asset)
}

fn promptlib_image_filename(file_name: &str) -> Result<String, String> {
    let name = file_name.trim().replace('\\', "/");
    let name = name.rsplit('/').next().unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let (stem, ext) = lower
        .rsplit_once('.')
        .ok_or_else(|| "无效的案例图文件名".to_string())?;
    if !matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "gif") {
        return Err("不支持的案例图格式".into());
    }
    if !stem.starts_with("case")
        || stem.len() <= 4
        || !stem[4..].chars().all(|c| c.is_ascii_digit())
    {
        return Err("无效的案例图文件名".into());
    }
    Ok(lower)
}

fn promptlib_image_url(file_name: &str) -> String {
    format!("https://{PROMPTLIB_IMAGE_HOST}{PROMPTLIB_IMAGE_PREFIX}{file_name}")
}

fn cached_promptlib_path(file_name: &str) -> PathBuf {
    crate::paths::promptlib_images_dir().join(file_name)
}

fn promptlib_download_lock(file_name: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("promptlib download lock poisoned");
    guard
        .entry(file_name.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

async fn download_promptlib_image(file_name: &str) -> Result<PathBuf, String> {
    let file_name = promptlib_image_filename(file_name)?;
    let dest = cached_promptlib_path(&file_name);
    let download_lock = promptlib_download_lock(&file_name);
    let _guard = download_lock.lock().await;
    if dest.is_file() {
        let meta = std::fs::metadata(&dest).map_err(|e| format!("读取案例图缓存失败: {e}"))?;
        if meta.len() > 0 {
            return Ok(dest);
        }
    }
    std::fs::create_dir_all(crate::paths::promptlib_images_dir())
        .map_err(|e| format!("创建案例图缓存目录失败: {e}"))?;
    let url = promptlib_image_url(&file_name);
    let tmp = dest.with_extension(format!(
        "{}.part-{}",
        dest.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("tmp"),
        std::process::id()
    ));
    let response = crate::http::shared_client()?
        .get(&url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("下载案例图失败: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("下载案例图失败（{}）", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("读取案例图失败: {e}"))?;
    if bytes.len() > MAX_PROMPTLIB_IMAGE_BYTES {
        return Err("案例图超过 8 MiB 大小限制".into());
    }
    if !crate::assets::is_valid_image_bytes(&bytes) {
        return Err("案例图内容格式校验失败".into());
    }
    std::fs::write(&tmp, &bytes).map_err(|e| format!("写入案例图缓存失败: {e}"))?;
    std::fs::rename(&tmp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("保存案例图缓存失败: {e}")
    })?;
    crate::logging::info(
        "promptlib.image_cached",
        serde_json::json!({ "fileName": file_name, "bytes": bytes.len(), "path": dest.display().to_string() }),
    );
    Ok(dest)
}

/// Return a local cached path for a case-library image. Downloads from R2 once.
#[tauri::command]
pub async fn cache_promptlib_image(file_name: String) -> Result<String, String> {
    let path = download_promptlib_image(&file_name).await?;
    Ok(path.display().to_string())
}

/// Already-downloaded case images, so the UI can skip R2 on later opens.
#[tauri::command(async)]
pub fn list_cached_promptlib_images() -> Result<Vec<String>, String> {
    let dir = crate::paths::promptlib_images_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("读取案例图缓存失败: {e}"))? {
        let path = entry
            .map_err(|e| format!("读取案例图缓存失败: {e}"))?
            .path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if promptlib_image_filename(name).is_ok() {
            out.push(path.display().to_string());
        }
    }
    Ok(out)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResult {
    removed: usize,
    skipped: usize,
    failed: usize,
}

#[tauri::command(async)]
pub fn cleanup_video_segments(
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
) -> CleanupResult {
    let cfg = state.cfg.read().unwrap().clone();
    cleanup_video_segments_with_config(&cfg, paths)
}

fn cleanup_video_segments_with_config(
    cfg: &crate::config::ConfigState,
    paths: Vec<String>,
) -> CleanupResult {
    let base = cfg.output_path();
    let video_root = base.join("视频");
    let canonical_root = video_root.canonicalize().ok();
    let mut result = CleanupResult {
        removed: 0,
        skipped: 0,
        failed: 0,
    };

    for raw in paths {
        let path = PathBuf::from(&raw);
        let Some(canonical_path) = path.canonicalize().ok() else {
            result.skipped += 1;
            crate::logging::warn(
                "video.segment_cleanup.skipped",
                serde_json::json!({ "path": raw, "reason": "not_found" }),
            );
            continue;
        };
        let safe_root = canonical_root
            .as_ref()
            .is_some_and(|root| canonical_path.starts_with(root));
        let safe_name = canonical_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("seg_") && name.ends_with(".mp4"));
        if !safe_root || !safe_name || !canonical_path.is_file() {
            result.skipped += 1;
            crate::logging::warn(
                "video.segment_cleanup.skipped",
                serde_json::json!({ "path": raw, "reason": "outside_generated_segment_scope" }),
            );
            continue;
        }

        match std::fs::remove_file(&canonical_path) {
            Ok(()) => {
                result.removed += 1;
                crate::logging::info(
                    "video.segment_cleanup.removed",
                    serde_json::json!({ "path": canonical_path.display().to_string() }),
                );
                if let (Some(parent), Some(root)) =
                    (canonical_path.parent(), canonical_root.as_ref())
                {
                    if parent != root
                        && parent.starts_with(root)
                        && std::fs::read_dir(parent)
                            .ok()
                            .is_some_and(|mut entries| entries.next().is_none())
                    {
                        let _ = std::fs::remove_dir(parent);
                    }
                }
            }
            Err(error) => {
                result.failed += 1;
                crate::logging::error(
                    "video.segment_cleanup.failed",
                    serde_json::json!({ "path": raw, "error": error.to_string() }),
                );
            }
        }
    }
    crate::logging::info(
        "video.segment_cleanup.end",
        serde_json::json!({ "removed": result.removed, "skipped": result.skipped, "failed": result.failed }),
    );
    result
}

#[tauri::command(async)]
pub fn config_status(state: tauri::State<'_, AppState>) -> ConfigStatus {
    state.cfg.read().unwrap().status()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveConfigRequest {
    image_api_url: String,
    image_api_key: String,
    image_api_model: String,
    video_api_url: String,
    video_api_key: String,
    video_api_model: String,
    llm_api_url: String,
    llm_api_key: String,
    llm_api_model: String,
    output_dir: String,
}

pub fn validate_save_config(config: &SaveConfigRequest) -> Result<(), String> {
    for (name, value) in [
        ("image_api_url", &config.image_api_url),
        ("video_api_url", &config.video_api_url),
        ("llm_api_url", &config.llm_api_url),
    ] {
        if !value.trim().is_empty() {
            let url = reqwest::Url::parse(value.trim())
                .map_err(|error| format!("{name} 地址无效: {error}"))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(format!("{name} 必须是 HTTP(S) 地址"));
            }
        }
    }
    for (name, value) in [
        ("image_api_model", &config.image_api_model),
        ("video_api_model", &config.video_api_model),
        ("llm_api_model", &config.llm_api_model),
    ] {
        if value.chars().count() > 200 {
            return Err(format!("{name} 超过 200 个字符"));
        }
    }
    if config.output_dir.chars().count() > 4_096 {
        return Err("output_dir 超过 4096 个字符".into());
    }
    for (name, value) in [
        ("image_api_key", &config.image_api_key),
        ("video_api_key", &config.video_api_key),
        ("llm_api_key", &config.llm_api_key),
    ] {
        if value.chars().count() > 16_384 {
            return Err(format!("{name} 超过 16384 个字符"));
        }
    }
    Ok(())
}

/// 契约（见前端 settings.ts）：空值保留后端已有值，允许只推部分配置。
///
/// 例外：改变某个服务的地址时不允许继续沿用已保存的旧 Key。否则一个只改
/// 地址的请求就能把已存密钥发往新主机（本地 REST 接口无鉴权，这条路径等于
/// 凭据外发通道）。
pub fn merge_config(
    new: &mut crate::config::ConfigState,
    config: &SaveConfigRequest,
) -> Result<(), String> {
    for (name, incoming_url, stored_url, stored_key, incoming_key) in [
        ("image_api_url", &config.image_api_url, &new.image_api_url, &new.image_api_key, &config.image_api_key),
        ("video_api_url", &config.video_api_url, &new.video_api_url, &new.video_api_key, &config.video_api_key),
        ("llm_api_url", &config.llm_api_url, &new.llm_api_url, &new.llm_api_key, &config.llm_api_key),
    ] {
        let incoming = incoming_url.trim();
        if incoming.is_empty() {
            continue;
        }
        let endpoint_changed = !stored_url.trim().is_empty() && stored_url.trim() != incoming;
        if endpoint_changed && !stored_key.trim().is_empty() && incoming_key.trim().is_empty() {
            return Err(format!(
                "{name} 已变更：请同时提交该服务的新 Key，不能沿用已保存的旧 Key（避免密钥被发往新地址）"
            ));
        }
    }
    if !config.image_api_url.trim().is_empty() {
        new.image_api_url = config.image_api_url.trim().to_string();
    }
    if !config.video_api_url.trim().is_empty() {
        new.video_api_url = config.video_api_url.trim().to_string();
    }
    if !config.llm_api_url.trim().is_empty() {
        new.llm_api_url = config.llm_api_url.trim().to_string();
    }
    if !config.output_dir.trim().is_empty() {
        new.output_dir = config.output_dir.trim().to_string();
    }
    if !config.image_api_model.trim().is_empty() {
        new.image_model = config.image_api_model.trim().to_string();
    }
    if !config.video_api_model.trim().is_empty() {
        new.video_model = config.video_api_model.trim().to_string();
    }
    if !config.llm_api_model.trim().is_empty() {
        new.llm_model = config.llm_api_model.trim().to_string();
    }
    if !config.image_api_key.trim().is_empty() {
        new.image_api_key = config.image_api_key.trim().to_string();
    }
    if !config.video_api_key.trim().is_empty() {
        new.video_api_key = config.video_api_key.trim().to_string();
    }
    if !config.llm_api_key.trim().is_empty() {
        new.llm_api_key = config.llm_api_key.trim().to_string();
    }
    Ok(())
}

#[tauri::command(async)]
pub fn save_config(
    state: tauri::State<'_, AppState>,
    config: SaveConfigRequest,
) -> Result<ConfigStatus, String> {
    validate_save_config(&config)?;
    let mut new = state.cfg.read().unwrap().clone();
    merge_config(&mut new, &config)?;
    new.source = "db".into();
    new.persist_backend()?;

    *state.cfg.write().unwrap() = new.clone();
    crate::logging::info(
        "configuration.updated",
        serde_json::to_value(new.status()).unwrap_or_default(),
    );
    Ok(new.status())
}

/// Send a chat completion to the configured LLM (OpenAI-compatible).
#[tauri::command]
pub async fn llm_chat(
    state: tauri::State<'_, AppState>,
    system: String,
    user: String,
    model: Option<String>,
) -> Result<String, String> {
    let cfg = state.cfg.read().unwrap().clone();
    if cfg.llm_api_url.is_empty() || cfg.llm_api_key.is_empty() {
        return Err("未配置文本 LLM（请在设置中填写 LLM 地址与 Key）".into());
    }
    let base = cfg.llm_api_url.trim_end_matches('/').to_string();
    let url = format!("{base}/chat/completions");
    let model = model.unwrap_or(cfg.llm_model.clone());

    let url_clone = url.clone();
    let key_clone = cfg.llm_api_key.clone();
    llm_once(&url_clone, &key_clone, &model, &system, &user).await
}

fn strip_thinking(text: &str) -> String {
    let mut remaining = text;
    let mut output = String::new();
    loop {
        let lower = remaining.to_ascii_lowercase();
        let Some(start) = lower.find("<think>") else {
            output.push_str(remaining);
            break;
        };
        output.push_str(&remaining[..start]);
        let after_open = start + "<think>".len();
        let tail = &remaining[after_open..];
        let tail_lower = tail.to_ascii_lowercase();
        let Some(close) = tail_lower.find("</think>") else {
            break;
        };
        remaining = &tail[close + "</think>".len()..];
    }
    output.trim().to_string()
}

pub(crate) async fn llm_once(
    url: &str,
    key: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    llm_once_for(url, key, model, system, user, "text_completion").await
}

async fn llm_once_for(
    url: &str,
    key: &str,
    model: &str,
    system: &str,
    user: &str,
    operation: &str,
) -> Result<String, String> {
    let completion =
        crate::llm::complete_text_result(url, key, model, system, user, operation).await?;
    // A truncated answer (finish_reason=length) must never be persisted or
    // handed to a downstream step as if it were complete.
    if !completion.completed {
        return Err("文本服务返回不完整结果（可能被截断），未使用该结果；请重试或缩短输入".into());
    }
    Ok(strip_thinking(&completion.content))
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDef {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) system: String,
}

/// Agent 编排输入上限（Tauri agent_run 与 REST /agents/orchestrations 共用）。
pub(crate) const MAX_AGENT_TEXT_CHARS: usize = 2_000_000;
pub(crate) const MAX_AGENT_MODEL_CHARS: usize = 200;
pub(crate) const MAX_AGENT_TOOLS: usize = 32;
const MAX_TOOL_NAME_CHARS: usize = 100;
const MAX_TOOL_DESCRIPTION_CHARS: usize = 2_000;

/// 每轮 LLM 回合最多执行的工具调用数。
const MAX_TOOL_CALLS_PER_STEP: usize = 8;
/// 一次编排全程最多执行的工具调用数。
const MAX_TOOL_CALLS_TOTAL: usize = 24;
/// 同一轮内并发执行的工具调用数上限（避免同时压满供应商限流）。
const MAX_CONCURRENT_TOOL_CALLS: usize = 3;

/// 校验 agent 编排的工具定义（名称非空唯一、长度上限与 api.rs 保持一致）。
pub(crate) fn validate_agent_tools(tools: &[ToolDef]) -> Result<(), String> {
    if tools.is_empty() || tools.len() > MAX_AGENT_TOOLS {
        return Err(format!("tools 数量必须在 1 到 {MAX_AGENT_TOOLS} 之间"));
    }
    let mut seen = std::collections::HashSet::new();
    for tool in tools {
        if tool.name.trim().is_empty() {
            return Err("tools.name 不能为空".into());
        }
        if tool.name.chars().count() > MAX_TOOL_NAME_CHARS {
            return Err(format!("tools.name 超过 {MAX_TOOL_NAME_CHARS} 个字符"));
        }
        if !seen.insert(tool.name.trim().to_string()) {
            return Err(format!("tools.name 重复: {}", tool.name.trim()));
        }
        if tool.description.trim().is_empty() {
            return Err("tools.description 不能为空".into());
        }
        if tool.description.chars().count() > MAX_TOOL_DESCRIPTION_CHARS {
            return Err(format!(
                "tools.description 超过 {MAX_TOOL_DESCRIPTION_CHARS} 个字符"
            ));
        }
        if tool.system.trim().is_empty() {
            return Err("tools.system 不能为空".into());
        }
        if tool.system.chars().count() > MAX_AGENT_TEXT_CHARS {
            return Err(format!("tools.system 超过 {MAX_AGENT_TEXT_CHARS} 个字符"));
        }
    }
    Ok(())
}

fn validate_agent_request(
    system: &str,
    user: &str,
    model: Option<&str>,
    tools: &[ToolDef],
) -> Result<(), String> {
    if system.chars().count() > MAX_AGENT_TEXT_CHARS {
        return Err(format!("system 超过 {MAX_AGENT_TEXT_CHARS} 个字符"));
    }
    if user.chars().count() > MAX_AGENT_TEXT_CHARS {
        return Err(format!("input 超过 {MAX_AGENT_TEXT_CHARS} 个字符"));
    }
    if let Some(model) = model {
        if model.chars().count() > MAX_AGENT_MODEL_CHARS {
            return Err(format!("model 超过 {MAX_AGENT_MODEL_CHARS} 个字符"));
        }
    }
    validate_agent_tools(tools)
}

/// Agent 编排：LLM 可调用各步骤工具（剧本/分镜/一致性/质检/生成），自动串起来。
#[tauri::command]
pub async fn agent_run(
    state: tauri::State<'_, AppState>,
    system: String,
    user: String,
    model: Option<String>,
    tools: Vec<ToolDef>,
) -> Result<String, String> {
    let cfg = state.cfg.read().unwrap().clone();
    agent_run_with_config(&cfg, system, user, model, tools).await
}

pub(crate) async fn agent_run_with_config(
    cfg: &crate::config::ConfigState,
    system: String,
    user: String,
    model: Option<String>,
    tools: Vec<ToolDef>,
) -> Result<String, String> {
    validate_agent_request(&system, &user, model.as_deref(), &tools)?;
    if cfg.llm_api_url.is_empty() || cfg.llm_api_key.is_empty() {
        return Err("未配置文本 LLM".into());
    }
    let base = cfg.llm_api_url.trim_end_matches('/').to_string();
    let url = format!("{base}/chat/completions");
    let model = model.unwrap_or(cfg.llm_model.clone());
    let key = cfg.llm_api_key.clone();

    let tools_json: Vec<serde_json::Value> = tools.iter().map(|t| serde_json::json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": {
                "type": "object",
                "properties": { "input": { "type": "string", "description": "该步骤的任务输入内容" } },
                "required": ["input"]
            }
        }
    })).collect();

    let mut messages: Vec<serde_json::Value> = vec![
        serde_json::json!({"role":"system","content": system}),
        serde_json::json!({"role":"user","content": user}),
    ];

    let mut total_tool_calls = 0usize;
    for step in 0..6 {
        let completion = crate::llm::request(
            &url,
            &key,
            &model,
            messages.clone(),
            Some(&tools_json),
            &format!("agent_orchestration_step_{}", step + 1),
        )
        .await?;
        if completion.tool_calls.is_empty() {
            if !completion.completed {
                return Err(
                    "文本服务返回不完整结果（可能被截断），未保存文档；请重试或缩短输入".into(),
                );
            }
            return Ok(strip_thinking(&completion.content));
        }
        messages.push(completion.assistant_message);
        let mut executed_this_step = 0usize;
        let mut skipped_by_budget = 0usize;
        // Resolve the calls first so budget/skip semantics and the resulting
        // message order stay identical to the sequential implementation.
        let mut call_ids: Vec<String> = Vec::new();
        let mut skipped: Vec<bool> = Vec::new();
        let mut executable: Vec<(usize, String, String, String)> = Vec::new();
        for call in completion.tool_calls {
            call_ids.push(call.id.clone());
            if executed_this_step >= MAX_TOOL_CALLS_PER_STEP
                || total_tool_calls >= MAX_TOOL_CALLS_TOTAL
            {
                skipped_by_budget += 1;
                skipped.push(true);
                continue;
            }
            skipped.push(false);
            executed_this_step += 1;
            total_tool_calls += 1;
            let name = call.name;
            let input = serde_json::from_str::<serde_json::Value>(&call.arguments)
                .ok()
                .and_then(|v| v["input"].as_str().map(String::from))
                .unwrap_or_default();
            let tool = tools.iter().find(|t| t.name == name).ok_or("未知工具")?;
            executable.push((call_ids.len() - 1, tool.system.clone(), input, name));
        }
        // Independent tool calls are awaited concurrently (bounded) instead of
        // one after another; results are re-ordered to the model's call order.
        let mut results: Vec<Option<String>> = vec![None; call_ids.len()];
        if !executable.is_empty() {
            let semaphore =
                std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_TOOL_CALLS));
            let mut set = tokio::task::JoinSet::new();
            for (index, tool_system, input, name) in executable {
                let semaphore = semaphore.clone();
                let url = url.clone();
                let key = key.clone();
                let model = model.clone();
                set.spawn(async move {
                    let _permit = semaphore.acquire_owned().await;
                    let result =
                        llm_once_for(&url, &key, &model, &tool_system, &input, &format!("agent_tool_{name}"))
                            .await;
                    (index, result)
                });
            }
            while let Some(joined) = set.join_next().await {
                let (index, result) =
                    joined.map_err(|error| format!("工具调用任务失败: {error}"))?;
                results[index] = Some(result.unwrap_or_else(|error| error));
            }
        }
        for (index, id) in call_ids.iter().enumerate() {
            let content = if skipped[index] {
                "工具调用预算已用尽，本次调用未执行。".to_string()
            } else {
                results[index]
                    .clone()
                    .unwrap_or_else(|| "工具调用未返回结果。".to_string())
            };
            messages.push(serde_json::json!({"role":"tool","tool_call_id": id, "content": content}));
        }
        if skipped_by_budget > 0 {
            return Ok(format!(
                "编排已停止：工具调用达到预算上限（每轮最多 {MAX_TOOL_CALLS_PER_STEP} 次、全程最多 {MAX_TOOL_CALLS_TOTAL} 次），已执行 {total_tool_calls} 次，另有 {skipped_by_budget} 次调用未执行。"
            ));
        }
    }
    Err("编排超过步数上限".into())
}

/// Fetch the list of image models from the gateway's `/v1/models` (single
/// selection in the UI). Falls back to the known catalogue on error.
#[tauri::command]
pub async fn list_image_models(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let cfg = state.cfg.read().unwrap().clone();
    list_image_models_with_config(&cfg).await
}

pub(crate) async fn list_image_models_with_config(
    cfg: &crate::config::ConfigState,
) -> Result<Vec<String>, String> {
    let Some(url) = config::models_endpoint(&cfg.image_api_url) else {
        return Ok(config::known_image_models());
    };
    let client = crate::http::client_for_url(&url)?;
    match client
        .get(&url)
        .bearer_auth(&cfg.image_api_key)
        .timeout(Duration::from_secs(20))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            if let Ok(json) = r.json::<serde_json::Value>().await {
                if let Some(ids) = json.get("data").and_then(|d| d.as_array()) {
                    let list: Vec<String> = ids
                        .iter()
                        .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                        .collect();
                    if !list.is_empty() {
                        return Ok(list);
                    }
                }
            }
            Ok(config::known_image_models())
        }
        _ => Ok(config::known_image_models()),
    }
}

#[tauri::command]
pub async fn list_video_models(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let cfg = state.cfg.read().unwrap().clone();
    list_video_models_with_config(&cfg).await
}

pub(crate) async fn list_video_models_with_config(
    cfg: &crate::config::ConfigState,
) -> Result<Vec<String>, String> {
    let Some(url) = config::models_endpoint(&cfg.video_api_url) else {
        return Ok(config::known_video_models());
    };
    if cfg.video_api_key.trim().is_empty() {
        return Ok(config::known_video_models());
    }
    let client = crate::http::client_for_url(&url)?;
    let response = client
        .get(&url)
        .bearer_auth(&cfg.video_api_key)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|error| format!("读取视频模型目录失败: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "视频模型目录返回 {status}，请检查视频 API Key、权限和 Base URL"
        ));
    }
    let json = response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| format!("解析视频模型目录失败: {error}"))?;
    let models = json
        .get("data")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err(
            "视频模型目录为空，请确认使用的是视频 API Key，并检查权限和 Base URL".to_string(),
        );
    }
    Ok(models)
}

#[tauri::command(async)]
pub fn list_video_model_capabilities() -> Vec<crate::video::VideoModelCapability> {
    crate::video::model_capabilities()
}

#[tauri::command]
pub async fn run_video(
    state: tauri::State<'_, AppState>,
    db: tauri::State<'_, crate::db::DbState>,
    req: RunNodeRequest,
) -> Result<RunResult, String> {
    let cfg = state.cfg.read().unwrap().clone();
    let base = cfg.output_path();
    let tag = run_tag(&req.config);
    let dir = base.join("视频").join(&tag);

    let mut images = string_array(&req.config, "images");
    let local_images = crate::local_video_images::sources_from_config(&req.config)?;
    if !local_images.is_empty() {
        let project_id = crate::util::get_str(&req.config, "project_id").unwrap_or_default();
        images.extend(crate::local_video_images::resolve_local_video_images(
            &db,
            &project_id,
            &local_images,
        )?);
    }
    let vr = crate::video::VideoGenRequest {
        model: crate::util::get_str(&req.config, "model")
            .unwrap_or_else(|| cfg.video_model.clone()),
        prompt: crate::util::get_str(&req.config, "prompt").unwrap_or_default(),
        duration_s: crate::util::get_u32(&req.config, "duration_s").unwrap_or(5),
        aspect_ratio: crate::util::get_str(&req.config, "aspect_ratio"),
        resolution: crate::util::get_str(&req.config, "resolution"),
        mode: crate::util::get_str(&req.config, "mode"),
        images,
        videos: string_array(&req.config, "videos"),
        audios: string_array(&req.config, "audios"),
    };
    if vr.prompt.trim().is_empty() {
        return Err("缺少提示词".into());
    }

    let assets = crate::video::generate_segments(&cfg, &vr, &dir).await?;
    Ok(RunResult { assets })
}

fn string_array(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

#[tauri::command(async)]
pub fn list_providers(state: tauri::State<'_, AppState>) -> Vec<ProviderInfo> {
    state.registry.list()
}

#[tauri::command(async)]
pub fn set_active_provider(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Vec<ProviderInfo>, String> {
    state.registry.set_active(id)?;
    Ok(state.registry.list())
}

/// Shared provider dispatch for the legacy node command and durable comic
/// workers. Callers own their explicit output directory and provenance; this
/// helper deliberately has no project/store ambient state.
pub(crate) async fn generate_active_image(
    state: &AppState,
    req: &RunNodeRequest,
    output_dir: &std::path::Path,
) -> Result<Vec<AssetRef>, String> {
    state
        .registry
        .active()
        .generate_image(req, output_dir)
        .await
}

/// Dispatch a durable attempt through the provider selected at creation time.
/// This prevents a later active-provider switch from changing a paid retry.
pub(crate) async fn generate_provider_image(
    state: &AppState,
    provider_id: &str,
    req: &RunNodeRequest,
    output_dir: &std::path::Path,
) -> Result<Vec<AssetRef>, String> {
    state
        .registry
        .get(provider_id)
        .ok_or("VISUAL_PROVIDER_UNAVAILABLE")?
        .generate_image(req, output_dir)
        .await
}

#[tauri::command]
pub async fn run_node(
    state: tauri::State<'_, AppState>,
    req: RunNodeRequest,
) -> Result<RunResult, String> {
    let cfg = state.cfg.read().unwrap().clone();
    let base = cfg.output_path();
    let tag = run_tag(&req.config);

    match req.category.as_str() {
        "generate" | "ai_process" => {
            let dir = base.join(kind_folder("image")).join(&tag);
            let assets = generate_active_image(&state, &req, &dir).await?;
            Ok(RunResult { assets })
        }
        other => Err(format!("未知节点类别: {other}")),
    }
}

fn path_in_allowed_roots(path: &Path, cfg: &crate::config::ConfigState) -> Result<PathBuf, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("文件不存在或不可访问: {error}"))?;
    let allowed_roots = [crate::paths::data_dir(), cfg.output_path()];
    let allowed = allowed_roots.iter().any(|root| {
        root.canonicalize()
            .ok()
            .is_some_and(|canonical_root| canonical.starts_with(canonical_root))
    });
    if !allowed {
        return Err("文件不在应用数据或输出目录中".into());
    }
    Ok(canonical)
}

fn original_stem(file_stem: &str) -> String {
    let parts: Vec<&str> = file_stem.split('-').collect();
    if parts.len() >= 3 {
        let last = *parts.last().unwrap_or(&"");
        let grade = parts[parts.len() - 2];
        if last.len() == 14
            && last.chars().all(|c| c.is_ascii_digit())
            && matches!(
                grade,
                "lossless" | "q80" | "q60" | "jpg" | "png" | "webp" | "bmp" | "gif"
            )
        {
            return parts[..parts.len() - 2].join("-");
        }
    }
    file_stem.to_string()
}

fn compression_level_label(level: &str) -> Result<&'static str, String> {
    match level {
        "lossless" => Ok("lossless"),
        "q80" => Ok("q80"),
        "q60" => Ok("q60"),
        _ => Err("未知压缩等级".into()),
    }
}

fn convert_format_label(format: &str) -> Result<&'static str, String> {
    match format {
        "jpg" | "jpeg" => Ok("jpg"),
        "png" => Ok("png"),
        "webp" => Ok("webp"),
        "bmp" => Ok("bmp"),
        "gif" => Ok("gif"),
        _ => Err("不支持的目标格式".into()),
    }
}

fn encode_converted(img: &image::DynamicImage, format: &str) -> Result<Vec<u8>, String> {
    let mut out = std::io::Cursor::new(Vec::new());
    match format {
        "jpg" => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
            img.to_rgb8()
                .write_with_encoder(encoder)
                .map_err(|e| format!("JPEG 编码失败: {e}"))?;
        }
        "png" => img
            .write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| format!("PNG 编码失败: {e}"))?,
        "webp" => img
            .write_to(&mut out, image::ImageFormat::WebP)
            .map_err(|e| format!("WebP 编码失败: {e}"))?,
        "bmp" => img
            .write_to(&mut out, image::ImageFormat::Bmp)
            .map_err(|e| format!("BMP 编码失败: {e}"))?,
        "gif" => img
            .write_to(&mut out, image::ImageFormat::Gif)
            .map_err(|e| format!("GIF 编码失败: {e}"))?,
        _ => return Err(format!("不支持的目标格式: {format}")),
    }
    Ok(out.into_inner())
}

fn encode_compressed(
    img: &image::DynamicImage,
    format: &str,
    level: &str,
) -> Result<Vec<u8>, String> {
    let mut out = std::io::Cursor::new(Vec::new());
    match (format, level) {
        ("png", "lossless") => img
            .write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| format!("PNG 无损编码失败: {e}"))?,
        ("jpg" | "jpeg", "lossless") => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95);
            img.to_rgb8()
                .write_with_encoder(encoder)
                .map_err(|e| format!("JPEG 近无损编码失败: {e}"))?;
        }
        ("webp", "lossless") => img
            .write_to(&mut out, image::ImageFormat::WebP)
            .map_err(|e| format!("WebP 编码失败: {e}"))?,
        ("jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp", "q80" | "q60") => {
            let quality = if level == "q80" { 80 } else { 60 };
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
            img.to_rgb8()
                .write_with_encoder(encoder)
                .map_err(|e| format!("JPEG 有损编码失败: {e}"))?;
        }
        ("gif" | "bmp", "lossless") => img
            .write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| format!("无损转 PNG 失败: {e}"))?,
        _ => return Err(format!("不支持压缩该格式: {format}")),
    }
    Ok(out.into_inner())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageFileInfo {
    path: String,
    directory: String,
    file_name: String,
    bytes: u64,
    kb: f64,
    width: Option<u32>,
    height: Option<u32>,
    format: Option<String>,
    display_path: String,
    variants: Vec<ImageVariantInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageVariantInfo {
    path: String,
    file_name: String,
    level: String,
    bytes: u64,
    kb: f64,
    modified_at: u64,
}

fn list_image_variants(dir: &Path, stem: &str) -> Vec<ImageVariantInfo> {
    let mut variants = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return variants;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(file_stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if original_stem(file_stem) != stem {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let modified_at = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        let level = if file_stem == stem {
            "original".to_string()
        } else if file_stem.contains("-lossless-") {
            "lossless".to_string()
        } else if file_stem.contains("-q80-") {
            "q80".to_string()
        } else if file_stem.contains("-q60-") {
            "q60".to_string()
        } else if file_stem.contains("-jpg-") {
            "jpg".to_string()
        } else if file_stem.contains("-png-") {
            "png".to_string()
        } else if file_stem.contains("-webp-") {
            "webp".to_string()
        } else if file_stem.contains("-bmp-") {
            "bmp".to_string()
        } else if file_stem.contains("-gif-") {
            "gif".to_string()
        } else {
            "other".to_string()
        };
        variants.push(ImageVariantInfo {
            path: path.display().to_string(),
            file_name: name.to_string(),
            level,
            bytes: meta.len(),
            kb: (meta.len() as f64 / 1024.0 * 10.0).round() / 10.0,
            modified_at,
        });
    }
    variants.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    variants
}

#[tauri::command(async)]
pub async fn inspect_image_file(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<ImageFileInfo, String> {
    let cfg = state.cfg.read().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || inspect_image_file_inner(&cfg, &path))
        .await
        .map_err(|error| format!("读取图片信息任务失败: {error}"))?
}

fn inspect_image_file_inner(
    cfg: &crate::config::ConfigState,
    path: &str,
) -> Result<ImageFileInfo, String> {
    let canonical = path_in_allowed_roots(Path::new(path), cfg)?;
    let meta = std::fs::metadata(&canonical).map_err(|e| format!("读取文件信息失败: {e}"))?;
    let dir = canonical.parent().ok_or("无法解析文件目录")?;
    let file_name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("无效文件名")?
        .to_string();
    let stem = canonical
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_string();
    let origin = original_stem(&stem);
    let variants = list_image_variants(dir, &origin);
    let display_path = variants
        .first()
        .map(|item| item.path.clone())
        .unwrap_or_else(|| canonical.display().to_string());
    let (width, height, format) = image::image_dimensions(&canonical)
        .ok()
        .map(|(w, h)| {
            (
                Some(w),
                Some(h),
                canonical
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| value.to_ascii_lowercase()),
            )
        })
        .unwrap_or((None, None, None));
    Ok(ImageFileInfo {
        path: canonical.display().to_string(),
        directory: dir.display().to_string(),
        file_name,
        bytes: meta.len(),
        kb: (meta.len() as f64 / 1024.0 * 10.0).round() / 10.0,
        width,
        height,
        format,
        display_path,
        variants,
    })
}

#[tauri::command(async)]
pub async fn compress_image_file(
    state: tauri::State<'_, AppState>,
    path: String,
    level: String,
) -> Result<AssetRef, String> {
    let cfg = state.cfg.read().unwrap().clone();
    // Decoding and re-encoding a large image takes seconds; keep it off both the
    // UI thread and the async runtime's worker threads.
    tauri::async_runtime::spawn_blocking(move || compress_image_file_inner(&cfg, &path, &level))
        .await
        .map_err(|error| format!("压缩图片任务失败: {error}"))?
}

fn compress_image_file_inner(
    cfg: &crate::config::ConfigState,
    path: &str,
    level: &str,
) -> Result<AssetRef, String> {
    let canonical = path_in_allowed_roots(Path::new(path), cfg)?;
    let grade = compression_level_label(level)?;
    let bytes = std::fs::read(&canonical).map_err(|e| format!("读取原图失败: {e}"))?;
    if bytes.len() > MAX_MEDIA_ASSET_BYTES {
        return Err("图片超过 512 MiB 大小限制".into());
    }
    let format = crate::assets::detect_format_checked(&bytes).ok_or("不支持的图片格式")?;
    let img = crate::assets::decode_image_checked(&bytes)?;
    let encoded = encode_compressed(&img, format, grade)?;
    let dir = canonical.parent().ok_or("无法解析文件目录")?;
    let stem = canonical
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    let origin = original_stem(stem);
    let stamp = chrono::Local::now().format("%Y%m%d%H%M%S%3f").to_string();
    let ext = match (grade, format) {
        ("lossless", "png" | "gif" | "bmp") => "png",
        ("lossless", "webp") => "webp",
        _ => "jpg",
    };
    let file_name = format!("{origin}-{grade}-{stamp}.{ext}");
    let out_path = dir.join(&file_name);
    std::fs::write(&out_path, &encoded).map_err(|e| format!("写入压缩图失败: {e}"))?;
    crate::logging::info(
        "asset.image_compress.end",
        serde_json::json!({
            "source": canonical.display().to_string(),
            "output": out_path.display().to_string(),
            "level": grade,
            "bytes": encoded.len(),
        }),
    );
    Ok(AssetRef {
        id: Uuid::new_v4().to_string(),
        kind: "image".into(),
        path: out_path.display().to_string(),
        width: Some(img.width()),
        height: Some(img.height()),
        duration_s: None,
        format: Some(ext.into()),
    })
}

#[tauri::command(async)]
pub async fn convert_image_file(
    state: tauri::State<'_, AppState>,
    path: String,
    format: String,
) -> Result<AssetRef, String> {
    let cfg = state.cfg.read().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || convert_image_file_inner(&cfg, &path, &format))
        .await
        .map_err(|error| format!("转换图片任务失败: {error}"))?
}

fn convert_image_file_inner(
    cfg: &crate::config::ConfigState,
    path: &str,
    format: &str,
) -> Result<AssetRef, String> {
    let canonical = path_in_allowed_roots(Path::new(path), cfg)?;
    let target = convert_format_label(format)?;
    let bytes = std::fs::read(&canonical).map_err(|e| format!("读取原图失败: {e}"))?;
    if bytes.len() > MAX_MEDIA_ASSET_BYTES {
        return Err("图片超过 512 MiB 大小限制".into());
    }
    crate::assets::detect_format_checked(&bytes).ok_or("不支持的图片格式")?;
    let img = crate::assets::decode_image_checked(&bytes)?;
    let encoded = encode_converted(&img, target)?;
    let dir = canonical.parent().ok_or("无法解析文件目录")?;
    let stem = canonical
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    let origin = original_stem(stem);
    let stamp = chrono::Local::now().format("%Y%m%d%H%M%S%3f").to_string();
    let file_name = format!("{origin}-{target}-{stamp}.{target}");
    let out_path = dir.join(&file_name);
    std::fs::write(&out_path, &encoded).map_err(|e| format!("写入转换图失败: {e}"))?;
    crate::logging::info(
        "asset.image_convert.end",
        serde_json::json!({
            "source": canonical.display().to_string(),
            "output": out_path.display().to_string(),
            "format": target,
            "bytes": encoded.len(),
        }),
    );
    Ok(AssetRef {
        id: Uuid::new_v4().to_string(),
        kind: "image".into(),
        path: out_path.display().to_string(),
        width: Some(img.width()),
        height: Some(img.height()),
        duration_s: None,
        format: Some(target.into()),
    })
}

/// "图片" | "视频" folder for a kind.
fn kind_folder(kind: &str) -> &'static str {
    if kind == "video" {
        "视频"
    } else {
        "图片"
    }
}

/// A human-readable run folder: `时间-提示词-md5前6位`.
fn run_tag(config: &serde_json::Value) -> String {
    let prompt = crate::util::get_str(config, "prompt").unwrap_or_default();
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let clean: String = prompt
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join("-");
    let clean = if clean.is_empty() {
        "gen".to_string()
    } else {
        clean
    };
    let short: String = clean.chars().take(24).collect();
    let md5pre: String = format!("{:x}", md5::compute(prompt.as_bytes()))
        .chars()
        .take(6)
        .collect();
    format!("{ts}_{short}_{md5pre}")
}

#[cfg(test)]
mod tests {
    use super::{
        cleanup_video_segments_with_config, original_stem, promptlib_image_filename,
        save_document_version_with_config, strip_thinking, validate_agent_request,
        validate_agent_tools, DocumentVersionSaveRequest, ToolDef,
    };
    use serde_json::json;
    use std::path::Path;
    use std::sync::{Arc, Barrier};

    fn test_config(root: &Path) -> crate::config::ConfigState {
        crate::config::ConfigState {
            image_api_url: String::new(),
            image_api_key: String::new(),
            image_model: "image".into(),
            video_api_url: String::new(),
            video_api_key: String::new(),
            video_model: "video".into(),
            llm_api_url: String::new(),
            llm_api_key: String::new(),
            llm_model: "llm".into(),
            output_dir: root.join("output").display().to_string(),
            source: "test".into(),
        }
    }

    fn document_request(
        document_id: &str,
        text: &str,
        expected_head_asset_id: Option<String>,
        allow_branch: bool,
    ) -> DocumentVersionSaveRequest {
        DocumentVersionSaveRequest {
            label: "视频剧本".into(),
            text: text.into(),
            model: Some("test-llm".into()),
            project_id: Some("project-test".into()),
            document_id: document_id.into(),
            // Deliberately lie here for branch requests: the backend must
            // derive branch identity from allow_branch instead of trusting it.
            params: json!({ "documentType": "video_script", "videoBranch": false }),
            expected_head_asset_id,
            allow_branch,
        }
    }

    fn count_files(root: &Path) -> usize {
        if !root.exists() {
            return 0;
        }
        std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .map(|path| if path.is_dir() { count_files(&path) } else { 1 })
            .sum()
    }

    fn tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: "步骤说明".into(),
            system: "步骤 system".into(),
        }
    }

    #[test]
    fn validates_agent_request_and_tools() {
        let tools = vec![tool("writer"), tool("storyboard")];
        assert!(validate_agent_request("系统", "输入", None, &tools).is_ok());

        // 空工具列表与超量拒绝
        assert!(validate_agent_tools(&[]).is_err());
        let too_many: Vec<ToolDef> = (0..33).map(|index| tool(&format!("t{index}"))).collect();
        assert!(validate_agent_tools(&too_many).is_err());

        // 工具名非空、唯一且 <=100 字符
        assert!(validate_agent_tools(&[tool("  ")]).is_err());
        assert!(validate_agent_tools(&[tool("writer"), tool("writer")]).is_err());
        let long_name = "a".repeat(101);
        assert!(validate_agent_tools(&[tool(&long_name)]).is_err());
        assert!(validate_agent_tools(&[tool(&"a".repeat(100))]).is_ok());

        // description / system 非空与长度上限
        let bad_description = ToolDef {
            name: "writer".into(),
            description: "   ".into(),
            system: "s".into(),
        };
        assert!(validate_agent_tools(&[bad_description]).is_err());
        let long_description = ToolDef {
            name: "writer".into(),
            description: "d".repeat(2_001),
            system: "s".into(),
        };
        assert!(validate_agent_tools(&[long_description]).is_err());
        let bad_system = ToolDef {
            name: "writer".into(),
            description: "d".into(),
            system: String::new(),
        };
        assert!(validate_agent_tools(&[bad_system]).is_err());

        // system/input/model 长度上限
        let long_text = "x".repeat(2_000_001);
        assert!(validate_agent_request(&long_text, "输入", None, &tools).is_err());
        assert!(validate_agent_request("系统", &long_text, None, &tools).is_err());
        let long_model = "m".repeat(201);
        assert!(validate_agent_request("系统", "输入", Some(&long_model), &tools).is_err());
        assert!(
            validate_agent_request("系统", "输入", Some("m".repeat(200).as_str()), &tools).is_ok()
        );
    }

    #[test]
    fn strips_compression_suffix_from_file_stem() {
        assert_eq!(original_stem("abc"), "abc");
        assert_eq!(original_stem("abc-lossless-20260901010101"), "abc");
        assert_eq!(original_stem("hero-q80-20260901010101"), "hero");
        assert_eq!(original_stem("hero-q60-20260901010101"), "hero");
        assert_eq!(original_stem("hero-png-20260901010101"), "hero");
    }

    #[test]
    fn accepts_plain_case_image_names() {
        assert_eq!(promptlib_image_filename("case1.jpg").unwrap(), "case1.jpg");
        assert_eq!(
            promptlib_image_filename("/images/case334.png").unwrap(),
            "case334.png"
        );
        assert!(promptlib_image_filename("../secret.jpg").is_err());
        assert!(promptlib_image_filename("case1.exe").is_err());
        assert!(promptlib_image_filename("banner.svg").is_err());
    }

    #[test]
    fn removes_reasoning_blocks_from_model_output() {
        assert_eq!(strip_thinking("<think>hidden</think>结果"), "结果");
        assert_eq!(strip_thinking("前文<think>unfinished"), "前文");
        assert_eq!(strip_thinking("A<THINK>x</THINK>B"), "AB");
    }

    #[test]
    fn only_removes_generated_video_segments_inside_output_root() {
        let root =
            std::env::temp_dir().join(format!("image-client-cleanup-{}", uuid::Uuid::new_v4()));
        let run_dir = root.join("视频").join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let segment = run_dir.join("seg_00_test.mp4");
        let keep = run_dir.join("keep.mp4");
        let outside = root.join("seg_outside.mp4");
        std::fs::write(&segment, b"segment").unwrap();
        std::fs::write(&keep, b"keep").unwrap();
        std::fs::write(&outside, b"outside").unwrap();
        let cfg = crate::config::ConfigState {
            image_api_url: String::new(),
            image_api_key: String::new(),
            image_model: "image".into(),
            video_api_url: String::new(),
            video_api_key: String::new(),
            video_model: "video".into(),
            llm_api_url: String::new(),
            llm_api_key: String::new(),
            llm_model: "llm".into(),
            output_dir: root.display().to_string(),
            source: "none".into(),
        };
        let result = cleanup_video_segments_with_config(
            &cfg,
            vec![
                segment.display().to_string(),
                keep.display().to_string(),
                outside.display().to_string(),
            ],
        );
        assert_eq!(result.removed, 1);
        assert_eq!(result.skipped, 2);
        assert!(!segment.exists());
        assert!(keep.exists());
        assert!(outside.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn document_version_cas_allows_exactly_one_concurrent_head_and_cleans_loser_file() {
        let root = std::env::temp_dir().join(format!(
            "image-client-document-cas-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let database = Arc::new(crate::db::DbState::open(root.join("history.db")).unwrap());
        let config = Arc::new(test_config(&root));
        let barrier = Arc::new(Barrier::new(2));
        let mut workers = Vec::new();
        for text in ["并发版本 A", "并发版本 B"] {
            let database = Arc::clone(&database);
            let config = Arc::clone(&config);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                save_document_version_with_config(
                    &config,
                    &database,
                    document_request("video:script:concurrent", text, None, false),
                )
            }));
        }
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert!(results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .any(|error| error.contains("版本冲突")));

        crate::db::with_connection(&database, |connection| {
            let (rows, version): (i64, i64) = connection
                .query_row(
                    "SELECT COUNT(*), MAX(CAST(json_extract(metadata,'$.params.version') AS INTEGER)) FROM assets WHERE json_extract(metadata,'$.params.documentId')=?",
                    ["video:script:concurrent"],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(rows, 1);
            assert_eq!(version, 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(count_files(&root.join("output")), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn historical_branch_from_old_parent_does_not_replace_production_head() {
        let root = std::env::temp_dir().join(format!(
            "image-client-document-branch-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let database = crate::db::DbState::open(root.join("history.db")).unwrap();
        let config = test_config(&root);
        let first = save_document_version_with_config(
            &config,
            &database,
            document_request("video:script:branch", "生产版一", None, false),
        )
        .unwrap();
        let second = save_document_version_with_config(
            &config,
            &database,
            document_request(
                "video:script:branch",
                "生产版二",
                Some(first.asset.id.clone()),
                false,
            ),
        )
        .unwrap();
        let branch = save_document_version_with_config(
            &config,
            &database,
            document_request(
                "video:script:branch",
                "从旧父版本创建的历史分支",
                Some(first.asset.id.clone()),
                true,
            ),
        )
        .unwrap();

        assert_eq!((first.version, second.version, branch.version), (1, 2, 3));
        crate::db::with_connection(&database, |connection| {
            let production_head: String = connection
                .query_row(
                    "SELECT id FROM assets WHERE json_extract(metadata,'$.params.documentId')=? AND COALESCE(json_extract(metadata,'$.params.videoBranch'),0) != 1 ORDER BY CAST(json_extract(metadata,'$.params.version') AS INTEGER) DESC LIMIT 1",
                    ["video:script:branch"],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            let branch_flag: i64 = connection
                .query_row(
                    "SELECT CAST(json_extract(metadata,'$.params.videoBranch') AS INTEGER) FROM assets WHERE id=?",
                    [&branch.asset.id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(production_head, second.asset.id);
            assert_eq!(branch_flag, 1);
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
