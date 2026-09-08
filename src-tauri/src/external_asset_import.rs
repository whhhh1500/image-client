use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::Manager;

use crate::{db, model::AssetRef};

const MAX_IMPORT_FILES: usize = 8;
const MAX_IMAGE_IMPORT_BYTES: usize = 50 * 1024 * 1024;
const MAX_VIDEO_IMPORT_BYTES: usize = 512 * 1024 * 1024;
// Keep a multi-select request bounded as one unit.  Otherwise eight valid
// 512-MiB videos would be collected in memory before any transactional work.
const MAX_BATCH_IMPORT_BYTES: usize = MAX_VIDEO_IMPORT_BYTES;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetImportFilesInput {
    pub project_id: String,
    pub import_entry: String,
    pub upload_batch_id: String,
    pub files: Vec<AssetImportFile>,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "source",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AssetImportFile {
    Path { path: String },
    Bytes { file_name: String, data_base64: String },
}

/// Decode a base64 import payload with a size pre-check so an oversized request
/// is rejected before the decoded buffer is allocated.
fn decode_import_base64(value: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    if value.len() > (MAX_VIDEO_IMPORT_BYTES / 3 + 1) * 4 {
        return Err("导入文件超过 512 MiB 大小限制".into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| "导入文件内容不是有效的 base64".to_string())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedLibraryAsset {
    pub asset: AssetRef,
    pub source: String,
    pub project_id: String,
    pub params: Value,
    pub created_at: i64,
}

struct PendingImport {
    source: String,
    bytes: Vec<u8>,
    kind: String,
    format: String,
}

fn import_entry_is_allowed(entry: &str) -> bool {
    matches!(
        entry,
        "image_reference"
            | "prompt_library_reference"
            | "video_reference"
            | "short_drama_video_reference"
            | "asset_library_upload"
    )
}

fn import_entry_allows_kind(entry: &str, kind: &str) -> bool {
    match entry {
        "image_reference" | "prompt_library_reference" => kind == "image",
        "short_drama_video_reference" => kind == "video",
        "video_reference" | "asset_library_upload" => matches!(kind, "image" | "video"),
        _ => false,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("外部文件")
        .to_string()
}

fn read_input(input: AssetImportFile) -> Result<(String, Vec<u8>), String> {
    match input {
        AssetImportFile::Path { path } => {
            let canonical = std::fs::canonicalize(&path)
                .map_err(|error| format!("读取导入文件失败：{error}"))?;
            let metadata = std::fs::metadata(&canonical)
                .map_err(|error| format!("读取导入文件信息失败：{error}"))?;
            if !metadata.is_file() {
                return Err("导入目标必须是文件".into());
            }
            if metadata.len() > MAX_VIDEO_IMPORT_BYTES as u64 {
                return Err("导入文件超过 512 MiB 大小限制".into());
            }
            let bytes =
                std::fs::read(&canonical).map_err(|error| format!("读取导入文件失败：{error}"))?;
            Ok((file_name(&canonical), bytes))
        }
        AssetImportFile::Bytes { file_name, data_base64 } => {
            if file_name.trim().is_empty() {
                return Err("导入文件名不能为空".into());
            }
            let data = decode_import_base64(&data_base64)?;
            if data.len() > MAX_VIDEO_IMPORT_BYTES {
                return Err("导入文件超过 512 MiB 大小限制".into());
            }
            Ok((file_name, data))
        }
    }
}

/// Check every path-backed selection before loading it. This keeps an eight-file
/// dialog selection from allocating several GiB just to discover that the batch
/// exceeds its declared limit.
fn input_len(input: &AssetImportFile) -> Result<usize, String> {
    match input {
        AssetImportFile::Path { path } => {
            let canonical = std::fs::canonicalize(path)
                .map_err(|error| format!("读取导入文件失败：{error}"))?;
            let metadata = std::fs::metadata(canonical)
                .map_err(|error| format!("读取导入文件信息失败：{error}"))?;
            if !metadata.is_file() {
                return Err("导入目标必须是文件".into());
            }
            usize::try_from(metadata.len()).map_err(|_| "导入文件超过允许大小".into())
        }
        AssetImportFile::Bytes { file_name, data_base64 } => {
            if file_name.trim().is_empty() {
                return Err("导入文件名不能为空".into());
            }
            // Approximate the decoded length so the batch preflight stays
            // allocation-free; the exact check happens after decoding.
            Ok(data_base64.len() / 4 * 3)
        }
    }
}

fn classify(source: String, bytes: Vec<u8>) -> Result<PendingImport, String> {
    if let Some(format) = crate::assets::detect_format_checked(&bytes) {
        if bytes.len() > MAX_IMAGE_IMPORT_BYTES {
            return Err(format!("图片“{source}”超过 50 MiB 大小限制"));
        }
        crate::assets::validate_image_checked(&bytes)
            .map_err(|_| format!("图片“{source}”内容格式校验失败"))?;
        return Ok(PendingImport {
            source,
            bytes,
            kind: "image".to_string(),
            format: format.to_string(),
        });
    }
    let extension = source
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(extension.as_str(), "mp4" | "webm" | "mov")
        && crate::assets::is_valid_video_bytes(&bytes, &extension)
    {
        return Ok(PendingImport {
            source,
            bytes,
            kind: "video".to_string(),
            format: extension,
        });
    }
    Err("仅支持内容校验通过的 PNG、JPG、WebP、GIF、BMP、MP4、WebM 或 MOV 文件".into())
}

fn verify_project(connection: &Connection, project_id: &str) -> Result<(), String> {
    if project_id.trim().is_empty() {
        return Err("导入资产必须指定项目".into());
    }
    let raw: Option<String> = connection
        .query_row(
            "SELECT value FROM settings WHERE key='projects'",
            [],
            |row| row.get(0),
        )
        .ok();
    let Some(raw) = raw else {
        return Err("项目不存在，不能导入资产".into());
    };
    let known = serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .is_some_and(|projects| {
            projects
                .iter()
                .any(|project| project.get("id").and_then(Value::as_str) == Some(project_id))
        });
    if known {
        Ok(())
    } else {
        Err("项目不存在，不能导入资产".into())
    }
}

fn catalog_metadata(input: &AssetImportFilesInput) -> Result<Value, String> {
    let mut params = input.params.clone();
    let object = params
        .as_object_mut()
        .ok_or_else(|| "导入资产 params 必须是对象".to_string())?;
    object.insert(
        "catalog".to_string(),
        json!({
            "version": 1,
            "category": "upload",
            "origin": "local_upload",
            "group": { "type": "upload_batch", "id": input.upload_batch_id },
        }),
    );
    object.insert(
        "origin".to_string(),
        Value::String("external_upload".to_string()),
    );
    object.insert(
        "importEntry".to_string(),
        Value::String(input.import_entry.clone()),
    );
    object.insert(
        "uploadBatchId".to_string(),
        Value::String(input.upload_batch_id.clone()),
    );
    Ok(params)
}

/// Everything needed to persist an import, prepared without holding the global
/// SQLite connection lock.
struct PreparedImport {
    input: AssetImportFilesInput,
    params: Value,
    pending: Vec<PendingImport>,
}

/// Validate the request and read/decode every file. This runs outside the DB
/// lock: reading up to 512 MiB and decoding images used to block every other
/// database caller for the duration of the import.
fn prepare_import(
    mut input: AssetImportFilesInput,
    _destination: &Path,
) -> Result<PreparedImport, String> {
    input.project_id = input.project_id.trim().to_string();
    input.import_entry = input.import_entry.trim().to_string();
    input.upload_batch_id = input.upload_batch_id.trim().to_string();
    if input.files.is_empty() || input.files.len() > MAX_IMPORT_FILES {
        return Err(format!("一次请选择 1 到 {MAX_IMPORT_FILES} 个文件"));
    }
    if !import_entry_is_allowed(&input.import_entry) {
        return Err("未知的外部导入入口".into());
    }
    if input.upload_batch_id.trim().is_empty() {
        return Err("导入批次 ID 不能为空".into());
    }
    let params = catalog_metadata(&input)?;
    let mut declared_batch_bytes = 0usize;
    for file in &input.files {
        let length = input_len(file)?;
        if length > MAX_VIDEO_IMPORT_BYTES {
            return Err("导入文件超过 512 MiB 大小限制".into());
        }
        declared_batch_bytes = declared_batch_bytes
            .checked_add(length)
            .ok_or("导入批次超过 512 MiB 大小限制")?;
        if declared_batch_bytes > MAX_BATCH_IMPORT_BYTES {
            return Err("导入批次超过 512 MiB 大小限制".into());
        }
    }
    let files = std::mem::take(&mut input.files);
    let pending = files
        .into_iter()
        .map(read_input)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(source, bytes)| classify(source, bytes))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(item) = pending
        .iter()
        .find(|item| !import_entry_allows_kind(&input.import_entry, &item.kind))
    {
        let required = match input.import_entry.as_str() {
            "image_reference" | "prompt_library_reference" => "图片",
            "short_drama_video_reference" => "视频",
            _ => "图片或视频",
        };
        return Err(format!(
            "{} 只接受实际{}文件，未导入“{}”",
            input.import_entry, required, item.source
        ));
    }
    Ok(PreparedImport { input, params, pending })
}

fn persist_import(
    connection: &mut Connection,
    prepared: PreparedImport,
    destination: &Path,
) -> Result<Vec<ImportedLibraryAsset>, String> {
    let PreparedImport { input, params, pending } = prepared;
    verify_project(connection, &input.project_id)?;

    let mut written: Vec<(AssetRef, String)> = Vec::with_capacity(pending.len());
    for item in pending {
        let folder = destination.join(if item.kind == "video" {
            "视频"
        } else {
            "图片"
        });
        match crate::assets::save_bytes(&folder, &item.kind, &item.bytes, &item.format) {
            Ok(asset) => written.push((asset, item.source)),
            Err(error) => {
                for (asset, _) in &written {
                    let _ = std::fs::remove_file(&asset.path);
                }
                return Err(error);
            }
        }
    }

    let created_at = chrono::Utc::now().timestamp_millis();
    let persisted = (|| {
        let transaction = connection
            .transaction()
            .map_err(|error| format!("开始外部资产导入事务失败：{error}"))?;
        for (asset, source) in &written {
            let mut asset_params = params.clone();
            asset_params
                .as_object_mut()
                .expect("validated params object")
                .insert(
                    "originalFileName".to_string(),
                    Value::String(source.clone()),
                );
            let metadata = json!({
                "source": source,
                "projectId": input.project_id,
                "params": asset_params,
            });
            transaction.execute(
                "INSERT INTO assets(id,kind,path,width,height,duration_s,format,created_at,metadata) VALUES(?,?,?,?,?,?,?,?,?)",
                params![asset.id, asset.kind, asset.path, asset.width, asset.height, asset.duration_s, asset.format, created_at, metadata.to_string()],
            ).map_err(|error| format!("登记外部资产失败：{error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("提交外部资产导入事务失败：{error}"))
    })();
    if let Err(error) = persisted {
        for (asset, _) in &written {
            let _ = std::fs::remove_file(&asset.path);
        }
        return Err(error);
    }

    Ok(written
        .into_iter()
        .map(|(asset, source)| {
            let mut asset_params = params.clone();
            asset_params
                .as_object_mut()
                .expect("validated params object")
                .insert(
                    "originalFileName".to_string(),
                    Value::String(source.clone()),
                );
            ImportedLibraryAsset {
                asset,
                source,
                project_id: input.project_id.clone(),
                params: asset_params,
                created_at,
            }
        })
        .collect())
}

#[tauri::command(async)]
pub async fn asset_import_files(
    app: tauri::AppHandle,
    input: AssetImportFilesInput,
) -> Result<Vec<ImportedLibraryAsset>, String> {
    let project_id = input.project_id.clone();
    let import_entry = input.import_entry.clone();
    // The whole import (up to 512 MiB of reads plus image decoding) runs on the
    // blocking pool; the connection lock is only taken for the persist phase.
    let result = tauri::async_runtime::spawn_blocking(move || {
        let destination = crate::paths::assets_dir().join("外部导入");
        let prepared = prepare_import(input, &destination)?;
        let db_state: tauri::State<'_, db::DbState> = app.state();
        db::with_connection_mut(&db_state, |connection| {
            persist_import(connection, prepared, &destination)
        })
    })
    .await
    .map_err(|error| format!("导入资产任务失败: {error}"))?;
    match &result {
        Ok(imported) => crate::logging::info(
            "asset.external_import.end",
            json!({ "projectId": project_id, "importEntry": import_entry, "assetIds": imported.iter().map(|item| item.asset.id.as_str()).collect::<Vec<_>>() }),
        ),
        Err(error) => crate::logging::warn(
            "asset.external_import.failed",
            json!({ "projectId": project_id, "importEntry": import_entry, "error": error }),
        ),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::io::Cursor;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("image-client-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE assets(id TEXT PRIMARY KEY,kind TEXT NOT NULL,path TEXT NOT NULL,width INTEGER,height INTEGER,duration_s REAL,format TEXT,created_at INTEGER NOT NULL,metadata TEXT);").unwrap();
        connection
            .execute(
                "INSERT INTO settings(key,value) VALUES('projects',?)",
                [r#"[{"id":"project-a"}]"#],
            )
            .unwrap();
        connection
    }

    fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn base64_of(bytes: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn input() -> AssetImportFilesInput {
        AssetImportFilesInput {
            project_id: "project-a".into(),
            import_entry: "image_reference".into(),
            upload_batch_id: "batch-a".into(),
            files: vec![AssetImportFile::Bytes {
                file_name: "source.png".into(),
                data_base64: base64_of(&png()),
            }],
            params: json!({ "scope": "reference" }),
        }
    }

    fn import_files_inner(
        connection: &mut Connection,
        input: AssetImportFilesInput,
        destination: &Path,
    ) -> Result<Vec<ImportedLibraryAsset>, String> {
        let prepared = prepare_import(input, destination)?;
        persist_import(connection, prepared, destination)
    }

    #[test]
    fn persists_external_asset_with_catalog_metadata() {
        let root = temp_dir("external-import-success");
        let mut db = connection();
        let imported = import_files_inner(&mut db, input(), &root).unwrap();
        assert_eq!(imported.len(), 1);
        assert!(Path::new(&imported[0].asset.path).is_file());
        assert_eq!(imported[0].params["catalog"]["category"], "upload");
        assert_eq!(imported[0].params["catalog"]["group"]["id"], "batch-a");
        assert_eq!(imported[0].params["originalFileName"], "source.png");
        let metadata: String = db
            .query_row(
                "SELECT metadata FROM assets WHERE id=?",
                [&imported[0].asset.id],
                |row| row.get(0),
            )
            .unwrap();
        let metadata: Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(metadata["projectId"], "project-a");
        assert_eq!(metadata["params"]["origin"], "external_upload");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unknown_project_before_writing_files() {
        let root = temp_dir("external-import-project");
        let mut import = input();
        import.project_id = "missing".into();
        assert!(import_files_inner(&mut connection(), import, &root).is_err());
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_an_image_disguised_as_short_drama_video_before_writing_files() {
        let root = temp_dir("external-import-kind");
        let mut request = input();
        request.import_entry = "short_drama_video_reference".into();
        request.files = vec![AssetImportFile::Bytes {
            file_name: "not-video.mp4".into(),
            data_base64: base64_of(&png()),
        }];
        assert!(import_files_inner(&mut connection(), request, &root).is_err());
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_a_magic_number_only_image_before_writing_files() {
        let root = temp_dir("external-import-incomplete-image");
        let mut request = input();
        request.files = vec![AssetImportFile::Bytes {
            file_name: "truncated.png".into(),
            data_base64: base64_of(&vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        }];
        assert!(import_files_inner(&mut connection(), request, &root).is_err());
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn accepts_the_browser_file_json_shape() {
        let file: AssetImportFile = serde_json::from_value(json!({
            "source": "bytes", "fileName": "browser.png", "dataBase64": base64_of(&png()),
        }))
        .unwrap();
        let AssetImportFile::Bytes { file_name, data_base64 } = file else {
            panic!("browser payload must deserialize as bytes")
        };
        assert_eq!(file_name, "browser.png");
        assert_eq!(decode_import_base64(&data_base64).unwrap(), png());
    }

    #[test]
    fn removes_only_this_batch_files_when_database_insert_fails() {
        let root = temp_dir("external-import-rollback");
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE assets(id TEXT PRIMARY KEY);").unwrap();
        connection
            .execute(
                "INSERT INTO settings(key,value) VALUES('projects',?)",
                [r#"[{"id":"project-a"}]"#],
            )
            .unwrap();
        assert!(import_files_inner(&mut connection, input(), &root).is_err());
        let files = std::fs::read_dir(&root)
            .unwrap()
            .flat_map(|entry| std::fs::read_dir(entry.unwrap().path()).unwrap())
            .count();
        assert_eq!(files, 0);
        let _ = std::fs::remove_dir_all(root);
    }
}
