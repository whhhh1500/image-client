use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::model::AssetRef;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryChange {
    pub revision: u64,
    pub operation: String,
    pub asset_ids: Vec<String>,
    pub changed_at: i64,
}

#[derive(Default)]
pub struct HistorySyncState {
    emitted_revision: AtomicU64,
    acknowledged_revision: AtomicU64,
    frontend_listener_ready: AtomicBool,
    last_change: Mutex<Option<HistoryChange>>,
}

impl HistorySyncState {
    pub fn publish(&self, operation: &str, asset_ids: &[String]) -> HistoryChange {
        let change = HistoryChange {
            revision: self.emitted_revision.fetch_add(1, Ordering::SeqCst) + 1,
            operation: operation.to_string(),
            asset_ids: asset_ids.to_vec(),
            changed_at: chrono::Utc::now().timestamp_millis(),
        };
        if let Ok(mut last_change) = self.last_change.lock() {
            *last_change = Some(change.clone());
        }
        change
    }

    pub fn acknowledge(&self, revision: u64) {
        self.acknowledged_revision
            .fetch_max(revision, Ordering::SeqCst);
    }

    pub fn mark_frontend_ready(&self) {
        self.frontend_listener_ready.store(true, Ordering::SeqCst);
    }

    pub fn status(&self) -> Value {
        let emitted_revision = self.emitted_revision.load(Ordering::SeqCst);
        let acknowledged_revision = self.acknowledged_revision.load(Ordering::SeqCst);
        let last_change = self
            .last_change
            .lock()
            .ok()
            .and_then(|change| change.clone());
        json!({
            "mode": "event_driven",
            "polling": false,
            "frontendListenerReady": self.frontend_listener_ready.load(Ordering::SeqCst),
            "emittedRevision": emitted_revision,
            "acknowledgedRevision": acknowledged_revision,
            "pendingRevision": emitted_revision.saturating_sub(acknowledged_revision),
            "lastChange": last_change,
        })
    }
}

#[tauri::command]
pub fn mark_history_listener_ready(state: tauri::State<'_, Arc<HistorySyncState>>) {
    state.mark_frontend_ready();
}

#[tauri::command]
pub fn acknowledge_history_revision(state: tauri::State<'_, Arc<HistorySyncState>>, revision: u64) {
    state.acknowledge(revision);
}

fn connection(path: &Path) -> Result<Connection, String> {
    let connection =
        Connection::open(path).map_err(|error| format!("打开历史数据库失败: {error}"))?;
    connection
        .busy_timeout(Duration::from_secs(10))
        .map_err(|error| format!("设置历史数据库等待时间失败: {error}"))?;
    Ok(connection)
}

pub fn persist_assets(
    assets: &[AssetRef],
    source: &str,
    model: Option<&str>,
    project_id: Option<&str>,
    params_value: &Value,
) -> Result<(), String> {
    persist_assets_to(
        &crate::paths::data_dir().join("image-client.db"),
        assets,
        source,
        model,
        project_id,
        params_value,
    )
}

fn persist_assets_to(
    database_path: &Path,
    assets: &[AssetRef],
    source: &str,
    model: Option<&str>,
    project_id: Option<&str>,
    params_value: &Value,
) -> Result<(), String> {
    if assets.is_empty() {
        return Ok(());
    }
    let mut connection = connection(database_path)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("开始历史写入事务失败: {error}"))?;
    let created_at = chrono::Utc::now().timestamp_millis();
    let metadata = serde_json::to_string(&json!({
        "source": source,
        "model": model,
        "projectId": project_id,
        "params": params_value,
    }))
    .map_err(|error| format!("序列化资产历史失败: {error}"))?;
    for asset in assets {
        transaction
            .execute(
                "INSERT OR REPLACE INTO assets (id, kind, path, width, height, duration_s, format, created_at, metadata) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
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
            .map_err(|error| format!("写入资产历史失败: {error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("提交资产历史失败: {error}"))?;
    crate::logging::info(
        "history.backend_assets_persisted",
        json!({
            "source": source,
            "assetCount": assets.len(),
            "assetIds": assets.iter().map(|asset| asset.id.as_str()).collect::<Vec<_>>(),
            "projectId": project_id,
        }),
    );
    Ok(())
}

pub fn generated_provenance(
    original_input: &str,
    generation_input: &str,
    system_instruction: Option<&str>,
    source_materials: Value,
    parent_asset_ids: Value,
) -> Value {
    json!({
        "schemaVersion": 1,
        "originalInput": original_input,
        "generationInput": generation_input,
        "systemInstruction": system_instruction,
        "sourceMaterials": source_materials,
        "parentAssetIds": parent_asset_ids,
        "revision": { "type": "generated" },
        "recordedAt": chrono::Utc::now().timestamp_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_api_writers_do_not_lose_assets() {
        let database_path = std::env::temp_dir().join(format!(
            "image-client-history-concurrency-{}.db",
            uuid::Uuid::new_v4()
        ));
        {
            let connection = Connection::open(&database_path).unwrap();
            connection
                .execute_batch(include_str!("../migrations/0001_init.sql"))
                .unwrap();
        }
        let writer_count = 12;
        let barrier = Arc::new(Barrier::new(writer_count));
        let handles = (0..writer_count)
            .map(|index| {
                let database_path = database_path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let asset = AssetRef {
                        id: format!("api-asset-{index}"),
                        kind: "text".into(),
                        path: format!("/tmp/api-asset-{index}.md"),
                        width: None,
                        height: None,
                        duration_s: None,
                        format: Some("md".into()),
                    };
                    barrier.wait();
                    persist_assets_to(
                        &database_path,
                        std::slice::from_ref(&asset),
                        "API并发测试",
                        Some("test-model"),
                        Some("test-project"),
                        &json!({ "origin": "rest_api", "index": index }),
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        let connection = Connection::open(&database_path).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM assets", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, writer_count as i64);
        drop(connection);
        std::fs::remove_file(&database_path).unwrap();
    }
}
