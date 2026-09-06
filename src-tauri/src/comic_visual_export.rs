//! Explicit, non-overwriting exports for immutable visual-page candidates.

use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tauri::{Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::{
    comic_visual_asset::{self, ComicVisualPageAssetGetInput},
    db::{self, DbState},
    novel::{new_id, now, request_hash},
};

const COMMAND_EXPORT: &str = "comic_visual_batch_export";
const EXPORT_UNAVAILABLE: &str = "VISUAL_EXPORT_UNAVAILABLE";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualExportSelection {
    pub member_id: String,
    pub run_id: String,
    pub asset_id: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualBatchExportInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub batch_id: String,
    pub selections: Vec<ComicVisualExportSelection>,
    pub destination_dir: String,
    pub idempotency_key: String,
}
/// A one-page export whose destination is derived only by the Rust process.
/// The frontend deliberately cannot name, or discover, the application's data
/// root for this path.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualPageExportDefaultInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub batch_id: String,
    pub selection: ComicVisualExportSelection,
    pub idempotency_key: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualExportOpenInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub export_id: String,
    pub member_id: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualExportFile {
    pub member_id: String,
    pub ordinal: i64,
    pub manifest_id: String,
    pub production_chapter_id: String,
    pub production_page_id: String,
    pub run_id: String,
    pub asset_id: String,
    pub path: String,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualExport {
    pub export_id: String,
    pub batch_id: String,
    pub directory_path: String,
    pub manifest_path: String,
    pub created_at: i64,
    pub files: Vec<ComicVisualExportFile>,
}

#[tauri::command]
pub async fn comic_visual_batch_export(
    app: tauri::AppHandle,
    input: ComicVisualBatchExportInput,
) -> Result<ComicVisualExport, String> {
    let destination = canonical_destination(&input.destination_dir)?;
    tauri::async_runtime::spawn_blocking(move || {
        let state: State<'_, DbState> = app.state();
        db::with_connection(&state, |conn| export_inner(conn, &input, &destination))
    })
    .await
    .map_err(|_| EXPORT_UNAVAILABLE.to_string())?
}

/// Exports exactly one immutable PNG candidate below the application's data
/// directory.  The destination is never accepted from the renderer process.
#[tauri::command]
pub async fn comic_visual_page_export_default(
    app: tauri::AppHandle,
    input: ComicVisualPageExportDefaultInput,
) -> Result<ComicVisualExport, String> {
    let data_root = crate::paths::data_dir();
    let assets_root = crate::paths::assets_dir();
    tauri::async_runtime::spawn_blocking(move || {
        let state: State<'_, DbState> = app.state();
        db::with_connection(&state, |conn| {
            export_default_page_inner_at(conn, &input, &data_root, &assets_root)
        })
    })
    .await
    .map_err(|_| EXPORT_UNAVAILABLE.to_string())?
}

#[tauri::command]
pub async fn comic_visual_export_open(
    app: tauri::AppHandle,
    input: ComicVisualExportOpenInput,
) -> Result<String, String> {
    let opener = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state: State<'_, DbState> = app.state();
        let target = db::with_connection(&state, |conn| {
            let export = read_export_verified(
                conn,
                &input.project_id,
                &input.novel_work_id,
                &input.export_id,
            )?;
            if let Some(member_id) = input.member_id {
                export
                    .files
                    .into_iter()
                    .find(|file| file.member_id == member_id)
                    .map(|file| file.path)
                    .ok_or(EXPORT_UNAVAILABLE.into())
            } else {
                Ok(export.directory_path)
            }
        })?;
        opener
            .opener()
            .open_path(&target, None::<String>)
            .map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
        Ok(target)
    })
    .await
    .map_err(|_| EXPORT_UNAVAILABLE.to_string())?
}

pub(crate) fn export_inner(
    conn: &Connection,
    input: &ComicVisualBatchExportInput,
    destination: &Path,
) -> Result<ComicVisualExport, String> {
    export_inner_at(conn, input, destination, &crate::paths::assets_dir())
}

/// Testable variant: `assets_root` is the root containing 漫画/小说生产/runId.
pub(crate) fn export_inner_at(
    conn: &Connection,
    input: &ComicVisualBatchExportInput,
    destination: &Path,
    assets_root: &Path,
) -> Result<ComicVisualExport, String> {
    export_inner_at_with_policy(conn, input, destination, assets_root, false)
}

fn export_inner_at_with_policy(
    conn: &Connection,
    input: &ComicVisualBatchExportInput,
    destination: &Path,
    assets_root: &Path,
    require_png: bool,
) -> Result<ComicVisualExport, String> {
    if input.idempotency_key.trim().is_empty() || input.selections.is_empty() {
        return Err("VISUAL_EXPORT_INPUT_REQUIRED".into());
    }
    let mut selections = input.selections.clone();
    selections.sort_by(|a, b| a.member_id.cmp(&b.member_id));
    if selections
        .windows(2)
        .any(|pair| pair[0].member_id == pair[1].member_id)
    {
        return Err("VISUAL_EXPORT_SELECTION_DUPLICATE".into());
    }
    let request = json!({"projectId":input.project_id,"novelWorkId":input.novel_work_id,"batchId":input.batch_id,"selections":selections,"destinationDir":destination});
    let hash = request_hash(&request)?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    if let Some((stored, id)) = tx.query_row("SELECT request_hash,export_id FROM comic_visual_export_command_receipts WHERE command_name=? AND idempotency_key=?",params![COMMAND_EXPORT,input.idempotency_key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(|_| EXPORT_UNAVAILABLE.to_string())? {
        if stored != hash { return Err("VISUAL_EXPORT_IDEMPOTENCY_MISMATCH".into()); }
        tx.commit().map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
        return read_export_verified(conn, &input.project_id, &input.novel_work_id, &id);
    }
    let export_id = new_id("comic_visual_export");
    let directory = destination.join(format!("comic-pages-{export_id}"));
    let created_at = now();
    let manifest_path = directory.join("manifest.json");
    tx.execute("INSERT INTO comic_visual_exports(id,project_id,novel_work_id,batch_id,destination_dir,directory_path,manifest_path,selection_json,request_hash,idempotency_key,status,safe_error_code,created_at) VALUES (?,?,?,?,?,?,?,?,?,?, 'writing',NULL,?)",params![export_id,input.project_id,input.novel_work_id,input.batch_id,destination.to_string_lossy(),directory.to_string_lossy(),manifest_path.to_string_lossy(),json!({"candidate":true,"requestedSelections":selections}).to_string(),hash,input.idempotency_key,created_at]).map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    tx.execute("INSERT INTO comic_visual_export_command_receipts(command_name,idempotency_key,request_hash,export_id,created_at) VALUES (?,?,?,?,?)",params![COMMAND_EXPORT,input.idempotency_key,hash,export_id,created_at]).map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    tx.commit().map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    let outcome = (|| -> Result<(), String> {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
        fs::create_dir(&directory)
            .map_err(|_| "VISUAL_EXPORT_DIRECTORY_CREATE_FAILED".to_string())?;
        let directory = verify_created_export_directory(destination, &directory)?;
        let output_manifest_path = directory.join("manifest.json");
        let mut files = Vec::with_capacity(selections.len());
        for selection in &selections {
            let row = load_selection(&tx, input, selection)?;
            let asset = comic_visual_asset::get_inner(
                &tx,
                &ComicVisualPageAssetGetInput {
                    project_id: input.project_id.clone(),
                    novel_work_id: input.novel_work_id.clone(),
                    run_id: selection.run_id.clone(),
                },
                &assets_root
                    .join("漫画")
                    .join("小说生产")
                    .join(&selection.run_id),
            )?;
            if asset.id != selection.asset_id {
                return Err(EXPORT_UNAVAILABLE.into());
            }
            if require_png && asset.format.as_deref() != Some("png") {
                return Err("VISUAL_EXPORT_DEFAULT_PNG_REQUIRED".into());
            }
            let bytes = fs::read(&asset.path).map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
            image::load_from_memory(&bytes)
                .map_err(|_| "VISUAL_EXPORT_SOURCE_INVALID".to_string())?;
            let digest = sha256(&bytes);
            let extension = asset
                .format
                .as_deref()
                .filter(|v| matches!(*v, "png" | "jpg" | "jpeg" | "webp"))
                .ok_or(EXPORT_UNAVAILABLE)?;
            let path = directory.join(format!(
                "{:03}-page-{}.{}",
                row.ordinal, row.page_no, extension
            ));
            create_new(&path, &bytes)?;
            files.push(ComicVisualExportFile {
                member_id: selection.member_id.clone(),
                ordinal: row.ordinal,
                manifest_id: row.manifest_id,
                production_chapter_id: row.production_chapter_id,
                production_page_id: row.production_page_id,
                run_id: selection.run_id.clone(),
                asset_id: selection.asset_id.clone(),
                path: path.to_string_lossy().into_owned(),
                sha256: digest,
            });
        }
        files.sort_by_key(|file| file.ordinal);
        let selection_json = json!({"kind": if files.len() == batch_member_count(&tx, input)? {"full_batch"} else {"subset"},"candidate":true,"files":files});
        create_new(
            &output_manifest_path,
            serde_json::to_string_pretty(&selection_json)
                .map_err(|_| EXPORT_UNAVAILABLE.to_string())?
                .as_bytes(),
        )?;
        for file in &files {
            tx.execute("INSERT INTO comic_visual_export_files(export_id,member_id,ordinal,manifest_id,production_chapter_id,production_page_id,run_id,asset_id,path,sha256) VALUES (?,?,?,?,?,?,?,?,?,?)",params![export_id,file.member_id,file.ordinal,file.manifest_id,file.production_chapter_id,file.production_page_id,file.run_id,file.asset_id,file.path,file.sha256]).map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
        }
        tx.execute("UPDATE comic_visual_exports SET status='complete',selection_json=?,safe_error_code=NULL WHERE id=? AND status='writing'",params![selection_json.to_string(),export_id]).map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
        tx.commit().map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
        Ok(())
    })();
    if let Err(error) = outcome {
        mark_failed(conn, &export_id, &error)?;
        return Err(error);
    }
    read_export_verified(conn, &input.project_id, &input.novel_work_id, &export_id)
}

/// Testable default-export variant.  It deliberately performs its candidate
/// lookup twice: once before creating the stable destination and again inside
/// the existing immediate export transaction.
pub(crate) fn export_default_page_inner_at(
    conn: &Connection,
    input: &ComicVisualPageExportDefaultInput,
    data_root: &Path,
    assets_root: &Path,
) -> Result<ComicVisualExport, String> {
    if input.idempotency_key.trim().is_empty() {
        return Err("VISUAL_EXPORT_INPUT_REQUIRED".into());
    }
    let preflight = ComicVisualBatchExportInput {
        project_id: input.project_id.clone(),
        novel_work_id: input.novel_work_id.clone(),
        batch_id: input.batch_id.clone(),
        selections: vec![input.selection.clone()],
        // `load_selection` never observes a destination.  Keeping this empty
        // makes it impossible for a preflight value to become an output path.
        destination_dir: String::new(),
        idempotency_key: input.idempotency_key.clone(),
    };
    let row = load_selection(conn, &preflight, &input.selection)?;
    let candidate = comic_visual_asset::get_inner(
        conn,
        &ComicVisualPageAssetGetInput {
            project_id: input.project_id.clone(),
            novel_work_id: input.novel_work_id.clone(),
            run_id: input.selection.run_id.clone(),
        },
        &assets_root
            .join("漫画")
            .join("小说生产")
            .join(&input.selection.run_id),
    )?;
    if candidate.id != input.selection.asset_id {
        return Err(EXPORT_UNAVAILABLE.into());
    }
    if candidate.format.as_deref() != Some("png") {
        return Err("VISUAL_EXPORT_DEFAULT_PNG_REQUIRED".into());
    }
    let destination =
        default_destination_root_at(data_root, &input.novel_work_id, &row.production_chapter_id)?;
    let export = ComicVisualBatchExportInput {
        project_id: input.project_id.clone(),
        novel_work_id: input.novel_work_id.clone(),
        batch_id: input.batch_id.clone(),
        selections: vec![input.selection.clone()],
        destination_dir: destination.to_string_lossy().into_owned(),
        idempotency_key: input.idempotency_key.clone(),
    };
    export_inner_at_with_policy(conn, &export, &destination, assets_root, true)
}

fn default_destination_root_at(
    data_root: &Path,
    novel_work_id: &str,
    production_chapter_id: &str,
) -> Result<PathBuf, String> {
    let root_metadata = fs::symlink_metadata(data_root)
        .map_err(|_| "VISUAL_EXPORT_DEFAULT_ROOT_INVALID".to_string())?;
    if !root_metadata.is_dir() || is_reparse(&root_metadata) {
        return Err("VISUAL_EXPORT_DEFAULT_PATH_UNSAFE".into());
    }
    let root = data_root
        .canonicalize()
        .map_err(|_| "VISUAL_EXPORT_DEFAULT_ROOT_INVALID".to_string())?;
    if !root.is_dir() {
        return Err("VISUAL_EXPORT_DEFAULT_ROOT_INVALID".into());
    }
    let work = safe_path_component(novel_work_id)?;
    let chapter = safe_path_component(production_chapter_id)?;
    let exports = create_verified_child(&root, "漫画导出")?;
    let work_root = create_verified_child(&exports, work)?;
    create_verified_child(&work_root, chapter)
}

fn safe_path_component(value: &str) -> Result<&str, String> {
    if value.is_empty()
        || value.len() > 160
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("VISUAL_EXPORT_DEFAULT_SEGMENT_INVALID".into());
    }
    Ok(value)
}

fn create_verified_child(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let child = parent.join(name);
    match fs::symlink_metadata(&child) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_reparse(&metadata) {
                return Err("VISUAL_EXPORT_DEFAULT_PATH_UNSAFE".into());
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir(&child).map_err(|_| "VISUAL_EXPORT_DIRECTORY_CREATE_FAILED")?;
            let metadata =
                fs::symlink_metadata(&child).map_err(|_| "VISUAL_EXPORT_DEFAULT_PATH_UNSAFE")?;
            if !metadata.is_dir() || is_reparse(&metadata) {
                return Err("VISUAL_EXPORT_DEFAULT_PATH_UNSAFE".into());
            }
        }
        Err(_) => return Err("VISUAL_EXPORT_DEFAULT_PATH_UNSAFE".into()),
    }
    let canonical = child
        .canonicalize()
        .map_err(|_| "VISUAL_EXPORT_DEFAULT_PATH_UNSAFE".to_string())?;
    if !canonical.starts_with(parent) {
        return Err("VISUAL_EXPORT_DEFAULT_PATH_UNSAFE".into());
    }
    Ok(canonical)
}

fn verify_created_export_directory(
    destination: &Path,
    directory: &Path,
) -> Result<PathBuf, String> {
    let metadata =
        fs::symlink_metadata(directory).map_err(|_| "VISUAL_EXPORT_DIRECTORY_CREATE_FAILED")?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err("VISUAL_EXPORT_PATH_UNSAFE".into());
    }
    let canonical = directory
        .canonicalize()
        .map_err(|_| "VISUAL_EXPORT_DIRECTORY_CREATE_FAILED".to_string())?;
    let canonical_destination = destination
        .canonicalize()
        .map_err(|_| "VISUAL_EXPORT_PATH_UNSAFE".to_string())?;
    if !canonical.starts_with(&canonical_destination) {
        return Err("VISUAL_EXPORT_PATH_UNSAFE".into());
    }
    Ok(canonical)
}

#[cfg(windows)]
fn is_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

struct SelectionRow {
    ordinal: i64,
    manifest_id: String,
    production_chapter_id: String,
    production_page_id: String,
    page_no: i64,
}
fn load_selection(
    conn: &Connection,
    input: &ComicVisualBatchExportInput,
    selection: &ComicVisualExportSelection,
) -> Result<SelectionRow, String> {
    conn.query_row("SELECT member.ordinal,member.manifest_id,member.production_chapter_id,member.production_page_id,member.page_no FROM comic_visual_batches batch JOIN comic_visual_batch_members member ON member.batch_id=batch.id JOIN comic_visual_page_runs run ON run.id=member.page_run_id WHERE batch.id=? AND batch.project_id=? AND batch.novel_work_id=? AND member.id=? AND member.status='candidate_ready' AND run.id=? AND run.asset_id=? AND run.status='candidate_ready' AND run.project_id=batch.project_id AND run.novel_work_id=batch.novel_work_id AND run.manifest_id=member.manifest_id AND run.production_page_id=member.production_page_id",params![input.batch_id,input.project_id,input.novel_work_id,selection.member_id,selection.run_id,selection.asset_id],|r|Ok(SelectionRow{ordinal:r.get(0)?,manifest_id:r.get(1)?,production_chapter_id:r.get(2)?,production_page_id:r.get(3)?,page_no:r.get(4)?})).optional().map_err(|_|EXPORT_UNAVAILABLE.to_string())?.ok_or(EXPORT_UNAVAILABLE.into())
}
fn batch_member_count(
    conn: &Connection,
    input: &ComicVisualBatchExportInput,
) -> Result<usize, String> {
    conn.query_row("SELECT COUNT(*) FROM comic_visual_batch_members member JOIN comic_visual_batches batch ON batch.id=member.batch_id WHERE batch.id=? AND batch.project_id=? AND batch.novel_work_id=?",params![input.batch_id,input.project_id,input.novel_work_id],|r|r.get::<_,i64>(0)).map(|v|v as usize).map_err(|_|EXPORT_UNAVAILABLE.into())
}
fn canonical_destination(value: &str) -> Result<PathBuf, String> {
    let raw = PathBuf::from(value);
    if !raw.is_absolute() {
        return Err("VISUAL_EXPORT_DESTINATION_INVALID".into());
    };
    let path = raw
        .canonicalize()
        .map_err(|_| "VISUAL_EXPORT_DESTINATION_INVALID".to_string())?;
    if path.is_dir() {
        Ok(path)
    } else {
        Err("VISUAL_EXPORT_DESTINATION_INVALID".into())
    }
}
fn create_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "VISUAL_EXPORT_WRITE_FAILED".to_string())?;
    file.write_all(bytes)
        .map_err(|_| "VISUAL_EXPORT_WRITE_FAILED".to_string())?;
    file.sync_all()
        .map_err(|_| "VISUAL_EXPORT_WRITE_FAILED".to_string())
}
fn mark_failed(conn: &Connection, export_id: &str, error: &str) -> Result<(), String> {
    let code = if error.starts_with("VISUAL_EXPORT_") {
        error
    } else {
        "VISUAL_EXPORT_WRITE_FAILED"
    };
    conn.execute("UPDATE comic_visual_exports SET status='failed',safe_error_code=? WHERE id=? AND status='writing'",params![code,export_id]).map_err(|_|EXPORT_UNAVAILABLE.to_string())?;
    Ok(())
}
fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn read_export_verified(
    conn: &Connection,
    project: &str,
    work: &str,
    id: &str,
) -> Result<ComicVisualExport, String> {
    let header:Option<(String,String,String,i64,String,String)>=conn.query_row("SELECT batch_id,directory_path,manifest_path,created_at,status,selection_json FROM comic_visual_exports WHERE id=? AND project_id=? AND novel_work_id=?",params![id,project,work],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(|_|EXPORT_UNAVAILABLE.to_string())?;
    let Some((batch_id, directory_path, manifest_path, created_at, status, selection_json)) =
        header
    else {
        return Err(EXPORT_UNAVAILABLE.into());
    };
    let directory = Path::new(&directory_path)
        .canonicalize()
        .map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    let manifest = Path::new(&manifest_path)
        .canonicalize()
        .map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    if status != "complete"
        || !directory.is_dir()
        || !manifest.is_file()
        || !manifest.starts_with(&directory)
    {
        return Err(EXPORT_UNAVAILABLE.into());
    };
    let mut stmt=conn.prepare("SELECT member_id,ordinal,manifest_id,production_chapter_id,production_page_id,run_id,asset_id,path,sha256 FROM comic_visual_export_files WHERE export_id=? ORDER BY ordinal").map_err(|_|EXPORT_UNAVAILABLE.to_string())?;
    let files = stmt
        .query_map(params![id], |r| {
            Ok(ComicVisualExportFile {
                member_id: r.get(0)?,
                ordinal: r.get(1)?,
                manifest_id: r.get(2)?,
                production_chapter_id: r.get(3)?,
                production_page_id: r.get(4)?,
                run_id: r.get(5)?,
                asset_id: r.get(6)?,
                path: r.get(7)?,
                sha256: r.get(8)?,
            })
        })
        .map_err(|_| EXPORT_UNAVAILABLE.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    let manifest_value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).map_err(|_| EXPORT_UNAVAILABLE.to_string())?)
            .map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    if files.is_empty()
        || files.iter().any(|f| {
            let path = Path::new(&f.path).canonicalize().ok();
            path.as_ref().is_none_or(|p| !p.starts_with(&directory))
                || fs::read(&f.path)
                    .ok()
                    .is_none_or(|b| sha256(&b) != f.sha256)
        })
        || manifest_value
            != serde_json::from_str::<serde_json::Value>(&selection_json)
                .map_err(|_| EXPORT_UNAVAILABLE.to_string())?
    {
        return Err(EXPORT_UNAVAILABLE.into());
    };
    Ok(ComicVisualExport {
        export_id: id.into(),
        batch_id,
        directory_path,
        manifest_path,
        created_at,
        files,
    })
}

#[cfg(test)]
pub(crate) fn assert_real_batch_export(
    conn: &Connection,
    batch_id: &str,
    project_id: &str,
    work_id: &str,
    assets_root: &Path,
    destination_root: &Path,
) -> Result<(), String> {
    let mut statement=conn.prepare("SELECT member.id,run.id,run.asset_id FROM comic_visual_batch_members member JOIN comic_visual_page_runs run ON run.id=member.page_run_id JOIN comic_visual_batches batch ON batch.id=member.batch_id WHERE batch.id=? AND batch.project_id=? AND batch.novel_work_id=? AND member.status='candidate_ready' AND run.status='candidate_ready' ORDER BY member.ordinal").map_err(|_|EXPORT_UNAVAILABLE.to_string())?;
    let selections = statement
        .query_map(params![batch_id, project_id, work_id], |r| {
            Ok(ComicVisualExportSelection {
                member_id: r.get(0)?,
                run_id: r.get(1)?,
                asset_id: r.get(2)?,
            })
        })
        .map_err(|_| EXPORT_UNAVAILABLE.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    if selections.len() != 2 {
        return Err("VISUAL_EXPORT_TEST_REQUIRES_TWO_READY_PAGES".into());
    }
    let input = ComicVisualBatchExportInput {
        project_id: project_id.into(),
        novel_work_id: work_id.into(),
        batch_id: batch_id.into(),
        selections: selections.clone(),
        destination_dir: destination_root.to_string_lossy().into_owned(),
        idempotency_key: "real-batch-export".into(),
    };
    let first = export_inner_at(conn, &input, destination_root, assets_root)?;
    if first.files.len() != 2 || first.files[0].ordinal >= first.files[1].ordinal {
        return Err("VISUAL_EXPORT_TEST_ORDER_FAILED".into());
    }
    for file in &first.files {
        let bytes =
            fs::read(&file.path).map_err(|_| "VISUAL_EXPORT_TEST_FILE_MISSING".to_string())?;
        image::load_from_memory(&bytes)
            .map_err(|_| "VISUAL_EXPORT_TEST_DECODE_FAILED".to_string())?;
        if sha256(&bytes) != file.sha256 {
            return Err("VISUAL_EXPORT_TEST_HASH_FAILED".into());
        }
    }
    let replay = export_inner_at(conn, &input, destination_root, assets_root)?;
    if replay.export_id != first.export_id || replay.directory_path != first.directory_path {
        return Err("VISUAL_EXPORT_TEST_REPLAY_FAILED".into());
    }
    let mut second = input.clone();
    second.idempotency_key = "real-batch-export-2".into();
    let second_export = export_inner_at(conn, &second, destination_root, assets_root)?;
    if second_export.directory_path == first.directory_path {
        return Err("VISUAL_EXPORT_TEST_OVERWRITE_FAILED".into());
    }
    let mut duplicate = input.clone();
    duplicate.idempotency_key = "real-batch-export-duplicate".into();
    duplicate.selections.push(selections[0].clone());
    if export_inner_at(conn, &duplicate, destination_root, assets_root).is_ok() {
        return Err("VISUAL_EXPORT_TEST_DUPLICATE_ACCEPTED".into());
    }
    let mut wrong_scope = input.clone();
    wrong_scope.idempotency_key = "real-batch-export-scope".into();
    wrong_scope.project_id = "wrong-project".into();
    if export_inner_at(conn, &wrong_scope, destination_root, assets_root).is_ok() {
        return Err("VISUAL_EXPORT_TEST_SCOPE_ACCEPTED".into());
    }

    let default_data_root = destination_root.join("default-data-root");
    fs::create_dir(&default_data_root)
        .map_err(|_| "VISUAL_EXPORT_TEST_DEFAULT_ROOT_CREATE_FAILED")?;
    let default_input = ComicVisualPageExportDefaultInput {
        project_id: project_id.into(),
        novel_work_id: work_id.into(),
        batch_id: batch_id.into(),
        selection: selections[0].clone(),
        idempotency_key: "real-default-page-export".into(),
    };
    let default_export =
        export_default_page_inner_at(conn, &default_input, &default_data_root, assets_root)?;
    let expected_default_root = default_data_root
        .canonicalize()
        .map_err(|_| "VISUAL_EXPORT_TEST_DEFAULT_ROOT_MISSING")?
        .join("漫画导出")
        .join(work_id);
    if default_export.files.len() != 1
        || !Path::new(&default_export.directory_path).starts_with(&expected_default_root)
        || default_export.files[0].sha256.is_empty()
    {
        return Err("VISUAL_EXPORT_TEST_DEFAULT_RECEIPT_FAILED".into());
    }
    let default_replay =
        export_default_page_inner_at(conn, &default_input, &default_data_root, assets_root)?;
    if default_replay.export_id != default_export.export_id
        || default_replay.files[0].sha256 != default_export.files[0].sha256
    {
        return Err("VISUAL_EXPORT_TEST_DEFAULT_REPLAY_FAILED".into());
    }
    fs::write(&first.manifest_path, b"{}").map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    if read_export_verified(conn, project_id, work_id, &first.export_id).is_ok() {
        return Err("VISUAL_EXPORT_TEST_MANIFEST_TAMPER_ACCEPTED".into());
    }
    conn.execute(
        "UPDATE comic_visual_exports SET status='writing' WHERE id=?",
        params![second_export.export_id],
    )
    .map_err(|_| EXPORT_UNAVAILABLE.to_string())?;
    if read_export_verified(conn, project_id, work_id, &second_export.export_id).is_ok() {
        return Err("VISUAL_EXPORT_TEST_INCOMPLETE_RECEIPT_ACCEPTED".into());
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{default_destination_root_at, safe_path_component};
    use std::{fs, path::PathBuf};

    fn fresh_temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "image-client-comic-export-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&root).expect("create isolated test root");
        root
    }

    #[test]
    fn default_destination_uses_only_safe_stable_identifiers() {
        let root = fresh_temp_root("default-path");
        let destination = default_destination_root_at(&root, "work_1", "chapter-2")
            .expect("safe default destination");
        let expected = root
            .canonicalize()
            .unwrap()
            .join("漫画导出")
            .join("work_1")
            .join("chapter-2");
        assert_eq!(destination, expected);
        assert!(destination.is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn default_destination_rejects_path_escape_segments_and_unsafe_children() {
        for invalid in ["", ".", "..", "work/name", "work\\name", "work name"] {
            assert!(safe_path_component(invalid).is_err(), "{invalid:?}");
        }
        let root = fresh_temp_root("unsafe-child");
        fs::write(root.join("漫画导出"), b"not a directory").unwrap();
        assert!(default_destination_root_at(&root, "work", "chapter").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn default_destination_rejects_a_windows_reparse_child() {
        let root = fresh_temp_root("reparse-root");
        let target = fresh_temp_root("reparse-target");
        let junction = root.join("漫画导出");
        let status = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                junction.to_str().unwrap(),
                target.to_str().unwrap(),
            ])
            .status()
            .expect("invoke mklink");
        if !status.success() {
            eprintln!("junction creation unavailable; reparse subcase skipped");
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(target);
            return;
        }
        assert!(default_destination_root_at(&root, "work", "chapter").is_err());
        fs::remove_dir(&junction).expect("remove test junction");
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(target);
    }
}
