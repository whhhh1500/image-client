use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let db_path = args
        .next()
        .map(PathBuf::from)
        .ok_or("usage: import_original_material <database-path> <asset-id> <source-file>")?;
    let asset_id = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or("asset id is required")?;
    let source_path = args
        .next()
        .map(PathBuf::from)
        .ok_or("source file is required")?;

    let source_text = fs::read_to_string(&source_path)?;
    if source_text.trim().is_empty() {
        return Err("source file is empty".into());
    }
    let source_name = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("original.txt");
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;

    let mut conn = Connection::open(&db_path)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    let transaction = conn.transaction()?;
    let row: Option<(String, String)> = transaction
        .query_row(
            "SELECT kind, metadata FROM assets WHERE id = ?",
            [&asset_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                ))
            },
        )
        .optional()?;
    let (kind, metadata_raw) = row.ok_or("target asset does not exist")?;
    if kind != "text" {
        return Err(format!("target asset is {kind}, expected text").into());
    }

    let mut metadata: Value = serde_json::from_str(&metadata_raw).unwrap_or_else(|_| json!({}));
    let params_value = metadata
        .get_mut("params")
        .and_then(Value::as_object_mut)
        .ok_or("target asset has no params metadata")?;
    let version = params_value
        .get("version")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    if version != 1 {
        return Err(format!("target asset is version {version}, expected version 1").into());
    }
    let existing_original = params_value
        .get("provenance")
        .and_then(|value| value.get("originalInput"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !existing_original.trim().is_empty() {
        return Err("target already contains an original input snapshot".into());
    }

    let data_root = db_path.parent().ok_or("database path has no parent")?;
    let material_dir = data_root
        .join("assets")
        .join("source-materials")
        .join(&asset_id);
    fs::create_dir_all(&material_dir)?;
    let imported_path = material_dir.join("original.txt");
    fs::write(&imported_path, source_text.as_bytes())?;

    let backup_dir = data_root.join("migration-backups");
    fs::create_dir_all(&backup_dir)?;
    let backup_path = backup_dir.join(format!(
        "{asset_id}-metadata-before-original-import-{now}.json"
    ));
    fs::write(&backup_path, metadata_raw.as_bytes())?;

    params_value.insert(
        "provenance".into(),
        json!({
            "schemaVersion": 1,
            "originalInput": source_text,
            "generationInput": source_text,
            "sourceMaterials": [{
                "kind": "text",
                "label": "第一版原始资料",
                "source": source_name,
                "path": imported_path.to_string_lossy(),
                "text": source_text
            }],
            "parentAssetIds": [],
            "revision": { "type": "generated" },
            "recordedAt": now
        }),
    );
    params_value.insert("updatedAt".into(), json!(now));
    let updated_metadata = serde_json::to_string(&metadata)?;
    let rows = transaction.execute(
        "UPDATE assets SET metadata = ? WHERE id = ?",
        params![updated_metadata, asset_id],
    )?;
    if rows != 1 {
        return Err(format!("expected one updated row, got {rows}").into());
    }
    transaction.commit()?;

    println!("asset_id={asset_id}");
    println!("source_chars={}", source_text.chars().count());
    println!("imported_path={}", imported_path.display());
    println!("metadata_backup={}", backup_path.display());
    Ok(())
}
