use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: cargo run --example inspect_history -- <database-path>")?;
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = conn.prepare(
        "SELECT id, kind, path, created_at, metadata FROM assets WHERE kind = 'text' ORDER BY created_at ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    for row in rows {
        let (id, kind, path, created_at, metadata) = row?;
        let metadata: Value = metadata
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or(Value::Null);
        let params = metadata.get("params").unwrap_or(&Value::Null);
        let source = metadata
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("历史");
        let project_id = metadata
            .get("projectId")
            .and_then(Value::as_str)
            .unwrap_or("");
        let document_type = params
            .get("documentType")
            .and_then(Value::as_str)
            .unwrap_or("");
        let version = params.get("version").and_then(Value::as_i64).unwrap_or(1);
        let text_chars = params
            .get("text")
            .and_then(Value::as_str)
            .map(|text| text.chars().count())
            .unwrap_or(0);
        let original_chars = params
            .get("provenance")
            .and_then(|value| value.get("originalInput"))
            .and_then(Value::as_str)
            .map(|text| text.chars().count())
            .unwrap_or(0);
        println!(
            "created_at={created_at}\tid={id}\tkind={kind}\tproject={project_id}\ttype={document_type}\tversion={version}\ttext_chars={text_chars}\toriginal_chars={original_chars}\tsource={source}\tpath={path}"
        );
    }
    Ok(())
}
