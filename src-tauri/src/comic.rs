use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::{self, DbState};

fn now() -> i64 {
    chrono::Local::now().timestamp_millis()
}

const RUN_LEASE_MS: i64 = 15 * 60 * 1000;
// The production contract is deliberately shared with TypeScript.  Rust does
// not duplicate its capacity constants; input-plan compilation is performed
// by the frontend before a run is created, while this inclusion makes schema
// drift a build/test-visible failure at the persistence boundary.
const COMIC_PRODUCTION_CONTRACT: &str =
    include_str!("../../src/shared/comic-production-contract.json");

fn comic_production_contract() -> Result<Value, String> {
    let value: Value = serde_json::from_str(COMIC_PRODUCTION_CONTRACT)
        .map_err(|e| format!("漫画生产契约 JSON 无效: {e}"))?;
    validate_comic_production_contract(&value)?;
    Ok(value)
}

fn validate_comic_production_contract(value: &Value) -> Result<(), String> {
    if value.get("schemaVersion").and_then(Value::as_i64) != Some(1) {
        return Err("漫画生产契约 schemaVersion 必须为 1".into());
    }
    let limits = value
        .get("comicPageText")
        .ok_or("漫画生产契约缺少 comicPageText")?;
    for key in [
        "maxPageTextCodePoints",
        "maxTextItemCodePoints",
        "maxTextItems",
    ] {
        if limits
            .get(key)
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .is_none()
        {
            return Err(format!("漫画生产契约缺少正整数 {key}"));
        }
    }
    Ok(())
}

fn ensure_positive(value: i64, label: &str) -> Result<(), String> {
    if value <= 0 {
        Err(format!("{label} 必须是正整数"))
    } else {
        Ok(())
    }
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4())
}

fn json_text(value: &Value) -> String {
    if value.is_null() {
        "{}".into()
    } else {
        value.to_string()
    }
}

fn parse_json(raw: String) -> Value {
    serde_json::from_str(&raw).unwrap_or(Value::Null)
}

fn request_hash(value: &Value) -> String {
    format!("sha256:{:x}", Sha256::digest(json_text(value).as_bytes()))
}

fn legacy_request_hash(value: &Value) -> String {
    format!("{:x}", md5::compute(json_text(value).as_bytes()))
}

fn receipt_hash_matches(stored_hash: &str, request: &Value) -> Result<bool, String> {
    if let Some(digest) = stored_hash.strip_prefix("sha256:") {
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("操作回执指纹格式无效".into());
        }
        return Ok(stored_hash == request_hash(request));
    }
    if stored_hash.len() == 32 && stored_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(stored_hash == legacy_request_hash(request));
    }
    Err("操作回执指纹格式无效".into())
}

/// Execute an externally retried mutation exactly once.  Receipts deliberately
/// contain the full successful response rather than recomputing it from rows
/// that may have changed since the original request.
fn with_receipt<T>(
    conn: &Connection,
    command_name: &str,
    idempotency_key: &str,
    request: &Value,
    action: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String>
where
    T: Serialize + DeserializeOwned,
{
    let key = idempotency_key.trim();
    if key.is_empty() {
        return Err("idempotencyKey 不能为空".into());
    }
    let hash = request_hash(request);
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| format!("开始幂等操作事务失败: {e}"))?;
    let receipt: Option<(String, String)> = tx
        .query_row(
            "SELECT request_hash, response_json FROM comic_operation_receipts WHERE command_name = ? AND idempotency_key = ?",
            params![command_name, key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("读取操作回执失败: {e}"))?;
    if let Some((stored_hash, response)) = receipt {
        if !receipt_hash_matches(&stored_hash, request)? {
            return Err("idempotencyKey 已用于不同业务载荷".into());
        }
        return serde_json::from_str(&response).map_err(|e| format!("读取操作回执响应失败: {e}"));
    }
    let value = action(&tx)?;
    let response = serde_json::to_string(&value).map_err(|e| format!("序列化操作回执失败: {e}"))?;
    tx.execute(
        "INSERT INTO comic_operation_receipts (id, command_name, idempotency_key, request_hash, response_json, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        params![new_id("coreceipt"), command_name, key, hash, response, now()],
    )
    .map_err(|e| format!("保存操作回执失败: {e}"))?;
    tx.commit()
        .map_err(|e| format!("提交幂等操作事务失败: {e}"))?;
    Ok(value)
}

struct RunEventInput<'a> {
    kind: &'a str,
    run_id: &'a str,
    event_type: &'a str,
    from_status: Option<&'a str>,
    to_status: Option<&'a str>,
    payload: Value,
    idempotency_key: Option<&'a str>,
    generation_attempt_id: Option<&'a str>,
    owner_app_session_id: Option<&'a str>,
}

fn append_run_event(conn: &Connection, event: RunEventInput<'_>) -> Result<(), String> {
    let seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM comic_run_events WHERE run_kind = ? AND run_id = ?",
            params![event.kind, event.run_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("读取生成事件序号失败: {e}"))?;
    conn.execute(
        "INSERT INTO comic_run_events (id, run_kind, run_id, seq, event_type, from_status, to_status, payload_json, idempotency_key, generation_attempt_id, owner_app_session_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![new_id("crevent"), event.kind, event.run_id, seq, event.event_type, event.from_status, event.to_status, json_text(&event.payload), event.idempotency_key, event.generation_attempt_id, event.owner_app_session_id, now()],
    )
    .map_err(|e| format!("保存生成状态事件失败: {e}"))?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicProject {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub format: String,
    pub status: String,
    pub config: Value,
    pub current_state: Value,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicSource {
    pub id: String,
    pub comic_project_id: String,
    pub asset_id: Option<String>,
    pub source_order: i64,
    pub title: Option<String>,
    pub source_range: Value,
    pub content_hash: String,
    pub summary: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicCard {
    pub id: String,
    pub comic_project_id: String,
    pub card_type: String,
    pub entity_key: String,
    pub version: i64,
    pub name: String,
    pub data: Value,
    pub locked: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicReference {
    pub id: String,
    pub comic_project_id: String,
    pub owner_type: String,
    pub owner_id: String,
    pub asset_id: String,
    pub role: String,
    pub weight: Option<f64>,
    pub sort_order: i64,
    pub approved: bool,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicChapter {
    pub id: String,
    pub comic_project_id: String,
    pub chapter_no: i64,
    pub title: Option<String>,
    pub source: Value,
    pub outline: Value,
    pub state_before: Value,
    pub state_after: Value,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicScene {
    pub id: String,
    pub chapter_id: String,
    pub scene_no: i64,
    pub spec: Value,
    pub status: String,
    pub keyframe_asset_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPanel {
    pub id: String,
    pub scene_id: String,
    pub page_no: i64,
    pub panel_no: i64,
    pub spec: Value,
    pub prompt: Value,
    pub status: String,
    pub approved_run_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicSnapshot {
    pub project: ComicProject,
    pub sources: Vec<ComicSource>,
    pub cards: Vec<ComicCard>,
    pub references: Vec<ComicReference>,
    pub chapters: Vec<ComicChapter>,
    pub scenes: Vec<ComicScene>,
    pub panels: Vec<ComicPanel>,
}

fn next_i64(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    label: &str,
) -> Result<i64, String> {
    conn.query_row(sql, params, |row| row.get(0))
        .map_err(|e| format!("读取{label}失败: {e}"))
}

fn empty_state() -> Value {
    json!({
        "version": 1,
        "time": "",
        "characters": {},
        "props": {},
        "irreversibleFacts": [],
        "unresolvedHooks": []
    })
}

fn default_config() -> Value {
    json!({
        "format": "page",
        "pageSize": "1024x1536",
        "planningLlm": "",
        "draftImageModel": "",
        "finalImageModel": "",
        "repairImageModel": ""
    })
}

fn get_project_by_app_id(
    conn: &Connection,
    project_id: &str,
) -> Result<Option<ComicProject>, String> {
    conn.query_row(
        "SELECT id, project_id, title, format, status, config_json, current_state_json, created_at, updated_at
         FROM comic_projects WHERE project_id = ? ORDER BY updated_at DESC LIMIT 1",
        params![project_id],
        |row| {
            Ok(ComicProject {
                id: row.get(0)?,
                project_id: row.get(1)?,
                title: row.get(2)?,
                format: row.get(3)?,
                status: row.get(4)?,
                config: parse_json(row.get(5)?),
                current_state: parse_json(row.get(6)?),
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("读取漫画项目失败: {e}"))
}

fn get_project(conn: &Connection, id: &str) -> Result<ComicProject, String> {
    conn.query_row(
        "SELECT id, project_id, title, format, status, config_json, current_state_json, created_at, updated_at
         FROM comic_projects WHERE id = ?",
        params![id],
        |row| {
            Ok(ComicProject {
                id: row.get(0)?,
                project_id: row.get(1)?,
                title: row.get(2)?,
                format: row.get(3)?,
                status: row.get(4)?,
                config: parse_json(row.get(5)?),
                current_state: parse_json(row.get(6)?),
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        },
    )
    .map_err(|e| format!("漫画项目不存在: {e}"))
}

fn list_sources(conn: &Connection, comic_project_id: &str) -> Result<Vec<ComicSource>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, comic_project_id, asset_id, source_order, title, source_range_json, content_hash, summary, created_at
             FROM comic_sources WHERE comic_project_id = ? ORDER BY source_order ASC",
        )
        .map_err(|e| format!("准备原著查询失败: {e}"))?;
    let rows = stmt
        .query_map(params![comic_project_id], |row| {
            Ok(ComicSource {
                id: row.get(0)?,
                comic_project_id: row.get(1)?,
                asset_id: row.get(2)?,
                source_order: row.get(3)?,
                title: row.get(4)?,
                source_range: parse_json(row.get::<_, String>(5).unwrap_or_else(|_| "{}".into())),
                content_hash: row.get(6)?,
                summary: row.get(7)?,
                created_at: row.get(8)?,
            })
        })
        .map_err(|e| format!("查询原著失败: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取原著失败: {e}"))
}

fn list_cards(conn: &Connection, comic_project_id: &str) -> Result<Vec<ComicCard>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, comic_project_id, card_type, entity_key, version, name, data_json, locked, created_at, updated_at
             FROM comic_cards WHERE comic_project_id = ? ORDER BY card_type, entity_key, version DESC",
        )
        .map_err(|e| format!("准备设定卡查询失败: {e}"))?;
    let rows = stmt
        .query_map(params![comic_project_id], |row| {
            Ok(ComicCard {
                id: row.get(0)?,
                comic_project_id: row.get(1)?,
                card_type: row.get(2)?,
                entity_key: row.get(3)?,
                version: row.get(4)?,
                name: row.get(5)?,
                data: parse_json(row.get(6)?),
                locked: row.get::<_, i64>(7)? != 0,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })
        .map_err(|e| format!("查询设定卡失败: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取设定卡失败: {e}"))
}

fn list_references(
    conn: &Connection,
    comic_project_id: &str,
) -> Result<Vec<ComicReference>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, comic_project_id, owner_type, owner_id, asset_id, role, weight, sort_order, approved, created_at
             FROM comic_references WHERE comic_project_id = ? ORDER BY owner_type, owner_id, sort_order",
        )
        .map_err(|e| format!("准备参考图查询失败: {e}"))?;
    let rows = stmt
        .query_map(params![comic_project_id], |row| {
            Ok(ComicReference {
                id: row.get(0)?,
                comic_project_id: row.get(1)?,
                owner_type: row.get(2)?,
                owner_id: row.get(3)?,
                asset_id: row.get(4)?,
                role: row.get(5)?,
                weight: row.get(6)?,
                sort_order: row.get(7)?,
                approved: row.get::<_, i64>(8)? != 0,
                created_at: row.get(9)?,
            })
        })
        .map_err(|e| format!("查询参考图失败: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取参考图失败: {e}"))
}

fn list_chapters(conn: &Connection, comic_project_id: &str) -> Result<Vec<ComicChapter>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, comic_project_id, chapter_no, title, source_json, outline_json, state_before_json, state_after_json, status, created_at, updated_at
             FROM comic_chapters WHERE comic_project_id = ? ORDER BY chapter_no",
        )
        .map_err(|e| format!("准备章节查询失败: {e}"))?;
    let rows = stmt
        .query_map(params![comic_project_id], |row| {
            Ok(ComicChapter {
                id: row.get(0)?,
                comic_project_id: row.get(1)?,
                chapter_no: row.get(2)?,
                title: row.get(3)?,
                source: parse_json(row.get(4)?),
                outline: parse_json(
                    row.get::<_, Option<String>>(5)?
                        .unwrap_or_else(|| "{}".into()),
                ),
                state_before: parse_json(row.get(6)?),
                state_after: parse_json(
                    row.get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "null".into()),
                ),
                status: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })
        .map_err(|e| format!("查询章节失败: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取章节失败: {e}"))
}

fn list_scenes(conn: &Connection, chapter_ids: &[String]) -> Result<Vec<ComicScene>, String> {
    if chapter_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut scenes = Vec::new();
    for chapter_id in chapter_ids {
        let mut stmt = conn
            .prepare(
                "SELECT id, chapter_id, scene_no, spec_json, status, keyframe_asset_id, created_at, updated_at
                 FROM comic_scenes WHERE chapter_id = ? ORDER BY scene_no",
            )
            .map_err(|e| format!("准备场景查询失败: {e}"))?;
        let rows = stmt
            .query_map(params![chapter_id], |row| {
                Ok(ComicScene {
                    id: row.get(0)?,
                    chapter_id: row.get(1)?,
                    scene_no: row.get(2)?,
                    spec: parse_json(row.get(3)?),
                    status: row.get(4)?,
                    keyframe_asset_id: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })
            .map_err(|e| format!("查询场景失败: {e}"))?;
        for row in rows {
            scenes.push(row.map_err(|e| format!("读取场景失败: {e}"))?);
        }
    }
    Ok(scenes)
}

fn list_panels(conn: &Connection, scene_ids: &[String]) -> Result<Vec<ComicPanel>, String> {
    let mut panels = Vec::new();
    for scene_id in scene_ids {
        let mut stmt = conn
            .prepare(
                "SELECT id, scene_id, page_no, panel_no, spec_json, prompt_json, status, approved_run_id, created_at, updated_at
                 FROM comic_panels WHERE scene_id = ? ORDER BY page_no, panel_no",
            )
            .map_err(|e| format!("准备漫画格查询失败: {e}"))?;
        let rows = stmt
            .query_map(params![scene_id], |row| {
                Ok(ComicPanel {
                    id: row.get(0)?,
                    scene_id: row.get(1)?,
                    page_no: row.get(2)?,
                    panel_no: row.get(3)?,
                    spec: parse_json(row.get(4)?),
                    prompt: parse_json(
                        row.get::<_, Option<String>>(5)?
                            .unwrap_or_else(|| "null".into()),
                    ),
                    status: row.get(6)?,
                    approved_run_id: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            })
            .map_err(|e| format!("查询漫画格失败: {e}"))?;
        for row in rows {
            panels.push(row.map_err(|e| format!("读取漫画格失败: {e}"))?);
        }
    }
    Ok(panels)
}

fn snapshot_of(conn: &Connection, project: ComicProject) -> Result<ComicSnapshot, String> {
    let sources = list_sources(conn, &project.id)?;
    let cards = list_cards(conn, &project.id)?;
    let references = list_references(conn, &project.id)?;
    let chapters = list_chapters(conn, &project.id)?;
    let chapter_ids: Vec<String> = chapters.iter().map(|item| item.id.clone()).collect();
    let scenes = list_scenes(conn, &chapter_ids)?;
    let scene_ids: Vec<String> = scenes.iter().map(|item| item.id.clone()).collect();
    let panels = list_panels(conn, &scene_ids)?;
    Ok(ComicSnapshot {
        project,
        sources,
        cards,
        references,
        chapters,
        scenes,
        panels,
    })
}

#[tauri::command]
pub fn comic_snapshot(
    state: tauri::State<'_, DbState>,
    project_id: String,
    title: Option<String>,
) -> Result<ComicSnapshot, String> {
    db::with_connection(&state, |conn| comic_snapshot_inner(conn, project_id, title))
}

fn comic_snapshot_inner(
    conn: &Connection,
    project_id: String,
    title: Option<String>,
) -> Result<ComicSnapshot, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|error| format!("开始漫画项目初始化事务失败: {error}"))?;
    let existing = get_project_by_app_id(&tx, &project_id)?;
    let project = if let Some(project) = existing {
        project
    } else {
        let ts = now();
        let project = ComicProject {
            id: new_id("comic"),
            project_id,
            title: title.unwrap_or_else(|| "未命名漫画".into()),
            format: "page".into(),
            status: "draft".into(),
            config: default_config(),
            current_state: empty_state(),
            created_at: ts,
            updated_at: ts,
        };
        tx.execute(
            "INSERT INTO comic_projects (id, project_id, title, format, status, config_json, current_state_json, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                project.id,
                project.project_id,
                project.title,
                project.format,
                project.status,
                json_text(&project.config),
                json_text(&project.current_state),
                project.created_at,
                project.updated_at
            ],
        )
        .map_err(|e| format!("创建漫画项目失败: {e}"))?;
        project
    };
    tx.commit()
        .map_err(|error| format!("提交漫画项目初始化事务失败: {error}"))?;
    snapshot_of(conn, project)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicSourceInput {
    pub comic_project_id: String,
    pub title: Option<String>,
    pub asset_id: Option<String>,
    pub content: String,
    pub summary: Option<String>,
}

#[tauri::command]
pub fn comic_source_add(
    state: tauri::State<'_, DbState>,
    input: ComicSourceInput,
) -> Result<ComicSource, String> {
    db::with_connection(&state, |conn| {
        let _ = get_project(conn, &input.comic_project_id)?;
        if let Some(asset_id) = &input.asset_id {
            ensure_asset_in_project(conn, asset_id, &input.comic_project_id)?;
        }
        let next_order = next_i64(
            conn,
            "SELECT COALESCE(MAX(source_order), 0) + 1 FROM comic_sources WHERE comic_project_id = ?",
            params![input.comic_project_id],
            "原著序号",
        )?;
        let hash = format!("{:x}", md5::compute(input.content.as_bytes()));
        let source = ComicSource {
            id: new_id("csrc"),
            comic_project_id: input.comic_project_id,
            asset_id: input.asset_id,
            source_order: next_order,
            title: input.title,
            source_range: json!({ "text": input.content }),
            content_hash: hash,
            summary: input.summary,
            created_at: now(),
        };
        conn.execute(
            "INSERT INTO comic_sources (id, comic_project_id, asset_id, source_order, title, source_range_json, content_hash, summary, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                source.id,
                source.comic_project_id,
                source.asset_id,
                source.source_order,
                source.title,
                json_text(&source.source_range),
                source.content_hash,
                source.summary,
                source.created_at
            ],
        )
        .map_err(|e| format!("保存原著失败: {e}"))?;
        Ok(source)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicCardInput {
    pub id: Option<String>,
    pub comic_project_id: String,
    pub card_type: String,
    pub entity_key: String,
    pub name: String,
    pub data: Value,
    pub locked: Option<bool>,
    pub new_version: Option<bool>,
}

#[tauri::command]
pub fn comic_card_save(
    state: tauri::State<'_, DbState>,
    input: ComicCardInput,
) -> Result<ComicCard, String> {
    db::with_connection(&state, |conn| comic_card_save_inner(conn, input))
}

fn comic_card_save_inner(conn: &Connection, input: ComicCardInput) -> Result<ComicCard, String> {
    let _ = get_project(conn, &input.comic_project_id)?;
    let ts = now();
    if let Some(id) = input.id.clone() {
        let locked: Option<i64> = conn
            .query_row(
                "SELECT locked FROM comic_cards WHERE id = ? AND comic_project_id = ?",
                params![id, input.comic_project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("读取设定卡失败: {e}"))?;
        let Some(locked) = locked else {
            return Err("卡片不存在或不属于当前漫画项目".into());
        };
        if locked != 0 && !input.new_version.unwrap_or(false) {
            return Err("已锁定卡片不能原地更新；请使用 newVersion 创建新版本".into());
        }
        if input.new_version.unwrap_or(false) {
            let current_version = next_i64(
                    conn,
                    "SELECT COALESCE(MAX(version), 0) FROM comic_cards WHERE comic_project_id = ? AND card_type = ? AND entity_key = ?",
                    params![input.comic_project_id, input.card_type, input.entity_key],
                    "设定卡版本",
                )?;
            let card = ComicCard {
                id: new_id("card"),
                comic_project_id: input.comic_project_id,
                card_type: input.card_type,
                entity_key: input.entity_key,
                version: current_version + 1,
                name: input.name,
                data: input.data,
                locked: input.locked.unwrap_or(false),
                created_at: ts,
                updated_at: ts,
            };
            insert_card(conn, &card)?;
            return Ok(card);
        }
        let updated = conn
                .execute(
                    "UPDATE comic_cards SET name = ?, data_json = ?, locked = ?, updated_at = ? WHERE id = ? AND comic_project_id = ?",
                    params![
                        input.name,
                        json_text(&input.data),
                        i64::from(input.locked.unwrap_or(false)),
                        ts,
                        id,
                        input.comic_project_id
                    ],
                )
                .map_err(|e| format!("更新设定卡失败: {e}"))?;
        if updated == 0 {
            return Err("卡片不存在或不属于当前漫画项目".into());
        }
        let card = conn
                .query_row(
                    "SELECT id, comic_project_id, card_type, entity_key, version, name, data_json, locked, created_at, updated_at FROM comic_cards WHERE id = ?",
                    params![id],
                    |row| {
                        Ok(ComicCard {
                            id: row.get(0)?,
                            comic_project_id: row.get(1)?,
                            card_type: row.get(2)?,
                            entity_key: row.get(3)?,
                            version: row.get(4)?,
                            name: row.get(5)?,
                            data: parse_json(row.get(6)?),
                            locked: row.get::<_, i64>(7)? != 0,
                            created_at: row.get(8)?,
                            updated_at: row.get(9)?,
                        })
                    },
                )
                .map_err(|e| format!("读取设定卡失败: {e}"))?;
        return Ok(card);
    }
    let current_version = next_i64(
            conn,
            "SELECT COALESCE(MAX(version), 0) FROM comic_cards WHERE comic_project_id = ? AND card_type = ? AND entity_key = ?",
            params![input.comic_project_id, input.card_type, input.entity_key],
            "设定卡版本",
        )?;
    let card = ComicCard {
        id: new_id("card"),
        comic_project_id: input.comic_project_id,
        card_type: input.card_type,
        entity_key: input.entity_key,
        version: current_version + 1,
        name: input.name,
        data: input.data,
        locked: input.locked.unwrap_or(false),
        created_at: ts,
        updated_at: ts,
    };
    insert_card(conn, &card)?;
    Ok(card)
}

fn insert_card(conn: &Connection, card: &ComicCard) -> Result<(), String> {
    conn.execute(
        "INSERT INTO comic_cards (id, comic_project_id, card_type, entity_key, version, name, data_json, locked, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            card.id,
            card.comic_project_id,
            card.card_type,
            card.entity_key,
            card.version,
            card.name,
            json_text(&card.data),
            i64::from(card.locked),
            card.created_at,
            card.updated_at
        ],
    )
    .map_err(|e| format!("保存设定卡失败: {e}"))?;
    Ok(())
}

/// 校验素材存在，且其记录明确属于指定应用项目。
fn ensure_asset_in_project(
    conn: &Connection,
    asset_id: &str,
    comic_project_id: &str,
) -> Result<(), String> {
    let project = get_project(conn, comic_project_id)?;
    let metadata: Option<String> = conn
        .query_row(
            "SELECT metadata FROM assets WHERE id = ?",
            params![asset_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("查询素材归属失败: {e}"))?;
    let Some(raw_metadata) = metadata else {
        return Err(format!("素材不存在或不属于当前漫画项目: {asset_id}"));
    };
    let owner_project_id = serde_json::from_str::<Value>(&raw_metadata)
        .ok()
        .and_then(|value| {
            value
                .get("projectId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    if owner_project_id.as_deref() != Some(project.project_id.as_str()) {
        return Err(format!("素材不存在或不属于当前漫画项目: {asset_id}"));
    }
    Ok(())
}

/// 校验章节存在且归属指定漫画项目。
fn ensure_chapter_in_project(
    conn: &Connection,
    chapter_id: &str,
    comic_project_id: &str,
) -> Result<(), String> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM comic_chapters WHERE id = ? AND comic_project_id = ?",
            params![chapter_id, comic_project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("查询章节归属失败: {e}"))?;
    if exists.is_none() {
        return Err("章节不存在或不属于当前漫画项目".into());
    }
    Ok(())
}

/// 校验场景存在，且经 chapter 链归属指定漫画项目。
fn ensure_scene_in_project(
    conn: &Connection,
    scene_id: &str,
    comic_project_id: &str,
) -> Result<(), String> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM comic_scenes s JOIN comic_chapters c ON c.id = s.chapter_id
             WHERE s.id = ? AND c.comic_project_id = ?",
            params![scene_id, comic_project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("查询场景归属失败: {e}"))?;
    if exists.is_none() {
        return Err("场景不存在或不属于当前漫画项目".into());
    }
    Ok(())
}

/// 校验参考图 owner 存在且归属当前漫画项目。
fn ensure_reference_owner(
    conn: &Connection,
    comic_project_id: &str,
    owner_type: &str,
    owner_id: &str,
) -> Result<(), String> {
    let sql = match owner_type {
        "card" => "SELECT 1 FROM comic_cards WHERE id = ? AND comic_project_id = ?",
        "scene" => {
            "SELECT 1 FROM comic_scenes s JOIN comic_chapters c ON c.id = s.chapter_id
             WHERE s.id = ? AND c.comic_project_id = ?"
        }
        "panel" => {
            "SELECT 1 FROM comic_panels p JOIN comic_scenes s ON s.id = p.scene_id
             JOIN comic_chapters c ON c.id = s.chapter_id
             WHERE p.id = ? AND c.comic_project_id = ?"
        }
        other => return Err(format!("不支持的参考图归属类型: {other}")),
    };
    let exists: Option<i64> = conn
        .query_row(sql, params![owner_id, comic_project_id], |row| row.get(0))
        .optional()
        .map_err(|e| format!("查询参考图归属失败: {e}"))?;
    if exists.is_none() {
        return Err("参考图归属对象不存在或不属于当前漫画项目".into());
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicReferenceInput {
    pub comic_project_id: String,
    pub owner_type: String,
    pub owner_id: String,
    pub asset_id: String,
    pub role: String,
    pub weight: Option<f64>,
    /// Optional for one release so legacy callers remain source-compatible.
    /// When supplied it is also protected by an operation receipt.
    pub idempotency_key: Option<String>,
}

#[tauri::command]
pub fn comic_reference_add(
    state: tauri::State<'_, DbState>,
    input: ComicReferenceInput,
) -> Result<ComicReference, String> {
    db::with_connection(&state, |conn| {
        if let Some(key) = input.idempotency_key.clone() {
            let request =
                serde_json::to_value(&input).map_err(|e| format!("序列化参考图请求失败: {e}"))?;
            with_receipt(conn, "comic_reference_add", &key, &request, |conn| {
                comic_reference_add_inner(conn, input)
            })
        } else {
            comic_reference_add_inner(conn, input)
        }
    })
}

fn comic_reference_add_inner(
    conn: &Connection,
    input: ComicReferenceInput,
) -> Result<ComicReference, String> {
    let _ = get_project(conn, &input.comic_project_id)?;
    ensure_reference_owner(
        conn,
        &input.comic_project_id,
        &input.owner_type,
        &input.owner_id,
    )?;
    ensure_asset_in_project(conn, &input.asset_id, &input.comic_project_id)?;
    // The v5 unique index is the final concurrency guard.  Look up the
    // business key first so replaying a legacy request neither changes its
    // approval state nor its ordering.
    if let Some(existing) = conn
        .query_row(
            "SELECT id, comic_project_id, owner_type, owner_id, asset_id, role, weight, sort_order, approved, created_at
             FROM comic_references WHERE comic_project_id = ? AND owner_type = ? AND owner_id = ? AND asset_id = ? AND role = ?",
            params![input.comic_project_id, input.owner_type, input.owner_id, input.asset_id, input.role],
            |row| {
                Ok(ComicReference {
                    id: row.get(0)?, comic_project_id: row.get(1)?, owner_type: row.get(2)?, owner_id: row.get(3)?,
                    asset_id: row.get(4)?, role: row.get(5)?, weight: row.get(6)?, sort_order: row.get(7)?,
                    approved: row.get::<_, i64>(8)? != 0, created_at: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(|e| format!("读取既有参考图失败: {e}"))?
    {
        return Ok(existing);
    }
    let next_order = next_i64(
        conn,
        "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM comic_references WHERE comic_project_id = ? AND owner_type = ? AND owner_id = ?",
        params![input.comic_project_id, input.owner_type, input.owner_id],
        "参考图序号",
    )?;
    let reference = ComicReference {
        id: new_id("cref"),
        comic_project_id: input.comic_project_id,
        owner_type: input.owner_type,
        owner_id: input.owner_id,
        asset_id: input.asset_id,
        role: input.role,
        weight: input.weight,
        sort_order: next_order,
        approved: false,
        created_at: now(),
    };
    match conn.execute(
        "INSERT INTO comic_references (id, comic_project_id, owner_type, owner_id, asset_id, role, weight, sort_order, approved, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            reference.id,
            reference.comic_project_id,
            reference.owner_type,
            reference.owner_id,
            reference.asset_id,
            reference.role,
            reference.weight,
            reference.sort_order,
            0,
            reference.created_at
        ],
    ) {
        Ok(_) => {}
        // A second process can insert the same business key between the
        // lookup and INSERT.  Return its canonical row rather than leaking a
        // SQLite uniqueness error to a retrying caller.
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.code == rusqlite::ErrorCode::ConstraintViolation => {
                return conn
                    .query_row(
                        "SELECT id, comic_project_id, owner_type, owner_id, asset_id, role, weight, sort_order, approved, created_at
                         FROM comic_references WHERE comic_project_id = ? AND owner_type = ? AND owner_id = ? AND asset_id = ? AND role = ?",
                        params![reference.comic_project_id, reference.owner_type, reference.owner_id, reference.asset_id, reference.role],
                        |row| Ok(ComicReference {
                            id: row.get(0)?, comic_project_id: row.get(1)?, owner_type: row.get(2)?, owner_id: row.get(3)?,
                            asset_id: row.get(4)?, role: row.get(5)?, weight: row.get(6)?, sort_order: row.get(7)?,
                            approved: row.get::<_, i64>(8)? != 0, created_at: row.get(9)?,
                        }),
                    )
                    .map_err(|e| format!("读取并发写入的参考图失败: {e}"));
        }
        Err(e) => return Err(format!("保存参考图失败: {e}")),
    }
    Ok(reference)
}

#[tauri::command]
pub fn comic_reference_set_approved(
    state: tauri::State<'_, DbState>,
    comic_project_id: String,
    id: String,
    approved: bool,
) -> Result<(), String> {
    db::with_connection(&state, |conn| {
        comic_reference_set_approved_inner(conn, &comic_project_id, &id, approved)
    })
}

fn comic_reference_set_approved_inner(
    conn: &Connection,
    comic_project_id: &str,
    id: &str,
    approved: bool,
) -> Result<(), String> {
    let updated = conn
        .execute(
            "UPDATE comic_references SET approved = ? WHERE id = ? AND comic_project_id = ?",
            params![i64::from(approved), id, comic_project_id],
        )
        .map_err(|e| format!("更新参考图批准状态失败: {e}"))?;
    if updated == 0 {
        return Err("参考图不存在或不属于当前漫画项目".into());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicChapterInput {
    pub id: Option<String>,
    #[serde(default)]
    pub comic_project_id: String,
    pub chapter_no: i64,
    pub title: Option<String>,
    pub source: Value,
    pub outline: Option<Value>,
    pub state_before: Option<Value>,
    pub status: Option<String>,
}

#[tauri::command]
pub fn comic_chapter_save(
    state: tauri::State<'_, DbState>,
    input: ComicChapterInput,
) -> Result<ComicChapter, String> {
    db::with_connection(&state, |conn| comic_chapter_save_inner(conn, input))
}

fn comic_chapter_save_inner(
    conn: &Connection,
    input: ComicChapterInput,
) -> Result<ComicChapter, String> {
    ensure_positive(input.chapter_no, "chapterNo")?;
    let project = get_project(conn, &input.comic_project_id)?;
    let ts = now();
    if let Some(id) = input.id {
        let updated = conn
            .execute(
                "UPDATE comic_chapters SET chapter_no = ?, title = ?, source_json = ?, outline_json = ?, status = ?, updated_at = ? WHERE id = ? AND comic_project_id = ?",
                params![
                    input.chapter_no,
                    input.title,
                    json_text(&input.source),
                    input.outline.as_ref().map(json_text),
                    input.status.unwrap_or_else(|| "draft".into()),
                    ts,
                    id,
                    input.comic_project_id
                ],
            )
            .map_err(|e| format!("更新章节失败: {e}"))?;
        if updated == 0 {
            return Err("章节不存在或不属于当前漫画项目".into());
        }
        let chapters = list_chapters(conn, &input.comic_project_id)?;
        return chapters
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| "章节不存在".into());
    }
    let chapter = ComicChapter {
        id: new_id("cch"),
        comic_project_id: input.comic_project_id,
        chapter_no: input.chapter_no,
        title: input.title,
        source: input.source,
        outline: input.outline.unwrap_or(Value::Null),
        state_before: input.state_before.unwrap_or(project.current_state),
        state_after: Value::Null,
        status: input.status.unwrap_or_else(|| "draft".into()),
        created_at: ts,
        updated_at: ts,
    };
    conn.execute(
        "INSERT INTO comic_chapters (id, comic_project_id, chapter_no, title, source_json, outline_json, state_before_json, state_after_json, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            chapter.id,
            chapter.comic_project_id,
            chapter.chapter_no,
            chapter.title,
            json_text(&chapter.source),
            json_text(&chapter.outline),
            json_text(&chapter.state_before),
            Option::<String>::None,
            chapter.status,
            chapter.created_at,
            chapter.updated_at
        ],
    )
    .map_err(|e| format!("保存章节失败: {e}"))?;
    Ok(chapter)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicSceneInput {
    pub id: Option<String>,
    #[serde(default)]
    pub comic_project_id: String,
    #[serde(default)]
    pub chapter_id: String,
    pub scene_no: i64,
    pub spec: Value,
    pub status: Option<String>,
    pub keyframe_asset_id: Option<String>,
}

#[tauri::command]
pub fn comic_scene_save(
    state: tauri::State<'_, DbState>,
    input: ComicSceneInput,
) -> Result<ComicScene, String> {
    db::with_connection(&state, |conn| {
        ensure_positive(input.scene_no, "sceneNo")?;
        ensure_chapter_in_project(conn, &input.chapter_id, &input.comic_project_id)?;
        if let Some(asset_id) = input.keyframe_asset_id.as_deref() {
            ensure_asset_in_project(conn, asset_id, &input.comic_project_id)?;
        }
        let ts = now();
        if let Some(id) = input.id {
            let updated = conn
                .execute(
                    "UPDATE comic_scenes SET scene_no = ?, spec_json = ?, status = ?, keyframe_asset_id = ?, updated_at = ? WHERE id = ? AND chapter_id = ?",
                    params![
                        input.scene_no,
                        json_text(&input.spec),
                        input.status.unwrap_or_else(|| "draft".into()),
                        input.keyframe_asset_id,
                        ts,
                        id,
                        input.chapter_id
                    ],
                )
                .map_err(|e| format!("更新场景失败: {e}"))?;
            if updated == 0 {
                return Err("场景不存在或不属于当前漫画项目".into());
            }
            let scenes = list_scenes(conn, &[input.chapter_id])?;
            return scenes
                .into_iter()
                .find(|item| item.id == id)
                .ok_or_else(|| "场景不存在".into());
        }
        let scene = ComicScene {
            id: new_id("csc"),
            chapter_id: input.chapter_id,
            scene_no: input.scene_no,
            spec: input.spec,
            status: input.status.unwrap_or_else(|| "draft".into()),
            keyframe_asset_id: input.keyframe_asset_id,
            created_at: ts,
            updated_at: ts,
        };
        conn.execute(
            "INSERT INTO comic_scenes (id, chapter_id, scene_no, spec_json, status, keyframe_asset_id, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                scene.id,
                scene.chapter_id,
                scene.scene_no,
                json_text(&scene.spec),
                scene.status,
                scene.keyframe_asset_id,
                scene.created_at,
                scene.updated_at
            ],
        )
        .map_err(|e| format!("保存场景失败: {e}"))?;
        Ok(scene)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPanelInput {
    pub id: Option<String>,
    #[serde(default)]
    pub comic_project_id: String,
    #[serde(default)]
    pub scene_id: String,
    pub page_no: i64,
    pub panel_no: i64,
    pub spec: Value,
    pub prompt: Option<Value>,
    pub status: Option<String>,
}

#[tauri::command]
pub fn comic_panel_save(
    state: tauri::State<'_, DbState>,
    input: ComicPanelInput,
) -> Result<ComicPanel, String> {
    db::with_connection(&state, |conn| comic_panel_save_inner(conn, input))
}

fn comic_panel_save_inner(conn: &Connection, input: ComicPanelInput) -> Result<ComicPanel, String> {
    ensure_positive(input.page_no, "pageNo")?;
    ensure_positive(input.panel_no, "panelNo")?;
    ensure_scene_in_project(conn, &input.scene_id, &input.comic_project_id)?;
    let ts = now();
    if let Some(id) = input.id {
        let updated = conn
            .execute(
                "UPDATE comic_panels SET page_no = ?, panel_no = ?, spec_json = ?, prompt_json = ?, status = ?, updated_at = ? WHERE id = ? AND scene_id = ?",
                params![
                    input.page_no,
                    input.panel_no,
                    json_text(&input.spec),
                    input.prompt.as_ref().map(json_text),
                    input.status.unwrap_or_else(|| "draft".into()),
                    ts,
                    id,
                    input.scene_id
                ],
            )
            .map_err(|e| format!("更新漫画格失败: {e}"))?;
        if updated == 0 {
            return Err("漫画格不存在或不属于当前场景".into());
        }
        let panels = list_panels(conn, &[input.scene_id])?;
        return panels
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| "漫画格不存在".into());
    }
    let panel = ComicPanel {
        id: new_id("cpn"),
        scene_id: input.scene_id,
        page_no: input.page_no,
        panel_no: input.panel_no,
        spec: input.spec,
        prompt: input.prompt.unwrap_or(Value::Null),
        status: input.status.unwrap_or_else(|| "draft".into()),
        approved_run_id: None,
        created_at: ts,
        updated_at: ts,
    };
    conn.execute(
        "INSERT INTO comic_panels (id, scene_id, page_no, panel_no, spec_json, prompt_json, status, approved_run_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            panel.id,
            panel.scene_id,
            panel.page_no,
            panel.panel_no,
            json_text(&panel.spec),
            json_text(&panel.prompt),
            panel.status,
            panel.approved_run_id,
            panel.created_at,
            panel.updated_at
        ],
    )
    .map_err(|e| format!("保存漫画格失败: {e}"))?;
    Ok(panel)
}

/// Append a panel with a server-assigned number.  The assignment lives in the
/// same receipt transaction as the insert, so a replay returns the original
/// panel and concurrent writers cannot silently reuse a number.
#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPanelAppendInput {
    pub comic_project_id: String,
    pub scene_id: String,
    pub page_no: i64,
    pub spec: Value,
    pub idempotency_key: String,
}

#[tauri::command]
pub fn comic_panel_append(
    state: tauri::State<'_, DbState>,
    input: ComicPanelAppendInput,
) -> Result<ComicPanel, String> {
    db::with_connection(&state, |conn| comic_panel_append_inner(conn, input))
}

fn comic_panel_append_inner(
    conn: &Connection,
    input: ComicPanelAppendInput,
) -> Result<ComicPanel, String> {
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    with_receipt(
        conn,
        "comic_panel_append",
        &input.idempotency_key,
        &request,
        |conn| {
            ensure_positive(input.page_no, "pageNo")?;
            ensure_scene_in_project(conn, &input.scene_id, &input.comic_project_id)?;
            let panel_no: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(panel_no), 0) + 1 FROM comic_panels WHERE scene_id = ? AND page_no = ?",
                    params![input.scene_id, input.page_no],
                    |row| row.get(0),
                )
                .map_err(|e| format!("读取下一格编号失败: {e}"))?;
            let ts = now();
            let panel = ComicPanel {
                id: new_id("cpn"),
                scene_id: input.scene_id.clone(),
                page_no: input.page_no,
                panel_no,
                spec: input.spec.clone(),
                prompt: Value::Null,
                status: "draft".into(),
                approved_run_id: None,
                created_at: ts,
                updated_at: ts,
            };
            conn.execute(
                "INSERT INTO comic_panels (id, scene_id, page_no, panel_no, spec_json, prompt_json, status, approved_run_id, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    panel.id,
                    panel.scene_id,
                    panel.page_no,
                    panel.panel_no,
                    json_text(&panel.spec),
                    json_text(&panel.prompt),
                    panel.status,
                    panel.approved_run_id,
                    panel.created_at,
                    panel.updated_at,
                ],
            )
            .map_err(|e| format!("追加漫画格失败: {e}"))?;
            Ok(panel)
        },
    )
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPageRun {
    pub id: String,
    pub comic_project_id: String,
    pub page_no: i64,
    pub asset_id: Option<String>,
    pub request_json: Value,
    pub reference_snapshot_json: Value,
    pub status: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub parent_run_id: Option<String>,
    pub attempt_no: i64,
    pub generation_attempt_id: Option<String>,
    pub owner_app_session_id: Option<String>,
    pub failure_json: Option<Value>,
    pub submitted_at: Option<i64>,
    pub heartbeat_at: Option<i64>,
    pub lease_expires_at: Option<i64>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPageRunCreateInput {
    pub comic_project_id: String,
    pub page_no: i64,
    pub request_json: Value,
    pub reference_snapshot_json: Value,
    pub idempotency_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPageRunFinishInput {
    pub id: String,
    pub asset_id: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

fn read_page_run(conn: &Connection, id: &str) -> Result<ComicPageRun, String> {
    conn.query_row(
        "SELECT id, comic_project_id, page_no, asset_id, request_json, reference_snapshot_json, status, error, created_at, finished_at,
                parent_run_id, attempt_no, generation_attempt_id, owner_app_session_id, failure_json, submitted_at, heartbeat_at, lease_expires_at
         FROM comic_page_runs WHERE id = ?",
        params![id],
        |row| {
            Ok(ComicPageRun {
                id: row.get(0)?,
                comic_project_id: row.get(1)?,
                page_no: row.get(2)?,
                asset_id: row.get(3)?,
                request_json: parse_json(row.get(4)?),
                reference_snapshot_json: parse_json(row.get(5)?),
                status: row.get(6)?,
                error: row.get(7)?,
                created_at: row.get(8)?,
                finished_at: row.get(9)?,
                parent_run_id: row.get(10)?,
                attempt_no: row.get(11)?,
                generation_attempt_id: row.get(12)?,
                owner_app_session_id: row.get(13)?,
                failure_json: row.get::<_, Option<String>>(14)?.map(parse_json),
                submitted_at: row.get(15)?,
                heartbeat_at: row.get(16)?,
                lease_expires_at: row.get(17)?,
            })
        },
    )
    .map_err(|e| format!("读取整页生成记录失败: {e}"))
}

#[tauri::command]
pub fn comic_page_run_create(
    state: tauri::State<'_, DbState>,
    input: ComicPageRunCreateInput,
) -> Result<ComicPageRun, String> {
    db::with_connection(&state, |conn| {
        if input.idempotency_key.is_some() {
            comic_page_run_create_v5_inner(conn, input, state.app_session_id())
        } else {
            comic_page_run_create_inner(conn, input)
        }
    })
}

fn comic_page_run_create_inner(
    conn: &Connection,
    input: ComicPageRunCreateInput,
) -> Result<ComicPageRun, String> {
    let _ = get_project(conn, &input.comic_project_id)?;
    ensure_positive(input.page_no, "pageNo")?;
    let created_at = now();
    let run = ComicPageRun {
        id: new_id("cprun"),
        comic_project_id: input.comic_project_id,
        page_no: input.page_no,
        asset_id: None,
        request_json: input.request_json,
        reference_snapshot_json: input.reference_snapshot_json,
        // The old command had no idempotency key and historically returned a
        // provider-ready running row.  Keep that narrow compatibility path;
        // all new callers send a key and start in queued.
        status: if input.idempotency_key.is_some() {
            "queued"
        } else {
            "running"
        }
        .into(),
        error: None,
        created_at: now(),
        finished_at: None,
        parent_run_id: None,
        attempt_no: 1,
        generation_attempt_id: Some(new_id("gatt")),
        owner_app_session_id: None,
        failure_json: None,
        submitted_at: None,
        heartbeat_at: None,
        lease_expires_at: Some(created_at + RUN_LEASE_MS),
    };
    conn.execute(
        "INSERT INTO comic_page_runs (id, comic_project_id, page_no, asset_id, request_json, reference_snapshot_json, status, error, created_at, finished_at,
          parent_run_id, attempt_no, generation_attempt_id, owner_app_session_id, failure_json, submitted_at, heartbeat_at, lease_expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            run.id,
            run.comic_project_id,
            run.page_no,
            run.asset_id,
            json_text(&run.request_json),
            json_text(&run.reference_snapshot_json),
            run.status,
            run.error,
            run.created_at,
            run.finished_at
            , run.parent_run_id, run.attempt_no, run.generation_attempt_id, run.owner_app_session_id,
            run.failure_json.as_ref().map(json_text), run.submitted_at, run.heartbeat_at, run.lease_expires_at
        ],
    )
    .map_err(|e| format!("保存整页生成记录失败: {e}"))?;
    Ok(run)
}

fn comic_page_run_create_v5_inner(
    conn: &Connection,
    input: ComicPageRunCreateInput,
    app_session_id: &str,
) -> Result<ComicPageRun, String> {
    let key = input
        .idempotency_key
        .clone()
        .ok_or("v5 create 需要 idempotencyKey")?;
    let request =
        serde_json::to_value(&input).map_err(|e| format!("序列化整页生成请求失败: {e}"))?;
    with_receipt(conn, "comic_page_run_create", &key, &request, |conn| {
        let _ = comic_production_contract()?;
        let _ = get_project(conn, &input.comic_project_id)?;
        ensure_positive(input.page_no, "pageNo")?;
        let ts = now();
        let run = ComicPageRun {
            id: new_id("cprun"),
            comic_project_id: input.comic_project_id,
            page_no: input.page_no,
            asset_id: None,
            request_json: input.request_json,
            reference_snapshot_json: input.reference_snapshot_json,
            status: "queued".into(),
            error: None,
            created_at: ts,
            finished_at: None,
            parent_run_id: None,
            attempt_no: 1,
            generation_attempt_id: Some(Uuid::new_v4().to_string()),
            owner_app_session_id: Some(app_session_id.to_owned()),
            failure_json: None,
            submitted_at: None,
            heartbeat_at: Some(ts),
            lease_expires_at: Some(ts + RUN_LEASE_MS),
        };
        conn.execute(
            "INSERT INTO comic_page_runs (id, comic_project_id, page_no, asset_id, request_json, reference_snapshot_json, status, error, created_at, finished_at, parent_run_id, attempt_no, generation_attempt_id, owner_app_session_id, failure_json, submitted_at, heartbeat_at, lease_expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![run.id, run.comic_project_id, run.page_no, run.asset_id, json_text(&run.request_json), json_text(&run.reference_snapshot_json), run.status, run.error, run.created_at, run.finished_at, run.parent_run_id, run.attempt_no, run.generation_attempt_id, run.owner_app_session_id, Option::<String>::None, run.submitted_at, run.heartbeat_at, run.lease_expires_at],
        ).map_err(|e| format!("保存 queued 整页生成记录失败: {e}"))?;
        append_run_event(
            conn,
            RunEventInput {
                kind: "page",
                run_id: &run.id,
                event_type: "created",
                from_status: None,
                to_status: Some("queued"),
                payload: json!({}),
                idempotency_key: Some(&key),
                generation_attempt_id: run.generation_attempt_id.as_deref(),
                owner_app_session_id: Some(app_session_id),
            },
        )?;
        Ok(run)
    })
}

#[tauri::command]
pub fn comic_page_run_finish(
    state: tauri::State<'_, DbState>,
    input: ComicPageRunFinishInput,
) -> Result<ComicPageRun, String> {
    db::with_connection(&state, |conn| comic_page_run_finish_inner(conn, input))
}

fn comic_page_run_finish_inner(
    conn: &Connection,
    input: ComicPageRunFinishInput,
) -> Result<ComicPageRun, String> {
    if input.status != "success" && input.status != "error" {
        return Err(format!("非法的整页生成状态: {}", input.status));
    }
    let current = read_page_run(conn, &input.id).map_err(|error| {
        if error.contains("读取整页生成记录失败") {
            "整页生成记录不存在".to_string()
        } else {
            error
        }
    })?;
    if current.status != "running" {
        return Err("整页生成记录已结束，不能重复完成".into());
    }
    if input.status == "success" {
        let asset_id = input
            .asset_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "整页生成成功时必须提供素材 ID".to_string())?;
        ensure_asset_in_project(conn, asset_id, &current.comic_project_id)?;
    }
    let error = if input.status == "error" {
        input.error.as_deref().map(safe_failure_text)
    } else {
        None
    };
    let finished_at = now();
    let updated = conn
        .execute(
            "UPDATE comic_page_runs SET asset_id = ?, status = ?, error = ?, finished_at = ? WHERE id = ? AND status = 'running'",
            params![input.asset_id, input.status, error, finished_at, input.id],
        )
        .map_err(|e| format!("更新整页生成记录失败: {e}"))?;
    if updated == 0 {
        return Err("整页生成记录已结束，不能重复完成".into());
    }
    read_page_run(conn, &input.id)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPanelRun {
    pub id: String,
    pub panel_id: String,
    pub task_id: Option<String>,
    pub asset_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub strategy: String,
    pub request_json: Value,
    pub reference_snapshot_json: Value,
    pub score_json: Option<Value>,
    pub status: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub attempt_no: i64,
    pub generation_attempt_id: Option<String>,
    pub owner_app_session_id: Option<String>,
    pub failure_json: Option<Value>,
    pub submitted_at: Option<i64>,
    pub heartbeat_at: Option<i64>,
    pub lease_expires_at: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicQcReport {
    pub id: String,
    pub panel_run_id: String,
    pub report_json: Value,
    pub decision: String,
    pub created_at: i64,
}

/// 校验漫画格存在，且经 scene→chapter 链归属指定漫画项目。
fn ensure_panel_in_project(
    conn: &Connection,
    panel_id: &str,
    comic_project_id: &str,
) -> Result<(), String> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM comic_panels p JOIN comic_scenes s ON s.id = p.scene_id
             JOIN comic_chapters c ON c.id = s.chapter_id
             WHERE p.id = ? AND c.comic_project_id = ?",
            params![panel_id, comic_project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("查询漫画格归属失败: {e}"))?;
    if exists.is_none() {
        return Err("漫画格不存在或不属于当前漫画项目".into());
    }
    Ok(())
}

fn panel_comic_project_id(conn: &Connection, panel_id: &str) -> Result<String, String> {
    conn.query_row(
        "SELECT c.comic_project_id
         FROM comic_panels p
         JOIN comic_scenes s ON s.id = p.scene_id
         JOIN comic_chapters c ON c.id = s.chapter_id
         WHERE p.id = ?",
        params![panel_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| format!("查询漫画格项目归属失败: {e}"))?
    .ok_or_else(|| "漫画格不存在或不属于任何漫画项目".into())
}

/// 校验格子生成记录存在，且经 panel→scene→chapter 链归属指定漫画项目。
fn ensure_panel_run_in_project(
    conn: &Connection,
    run_id: &str,
    comic_project_id: &str,
) -> Result<(), String> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM comic_panel_runs r
             JOIN comic_panels p ON p.id = r.panel_id
             JOIN comic_scenes s ON s.id = p.scene_id
             JOIN comic_chapters c ON c.id = s.chapter_id
             WHERE r.id = ? AND c.comic_project_id = ?",
            params![run_id, comic_project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("查询格子生成记录归属失败: {e}"))?;
    if exists.is_none() {
        return Err("格子生成记录不存在或不属于当前漫画项目".into());
    }
    Ok(())
}

fn get_panel(conn: &Connection, id: &str) -> Result<ComicPanel, String> {
    let scene_id = conn
        .query_row(
            "SELECT scene_id FROM comic_panels WHERE id = ?",
            params![id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| format!("漫画格不存在: {e}"))?;
    list_panels(conn, &[scene_id])?
        .into_iter()
        .find(|panel| panel.id == id)
        .ok_or_else(|| "漫画格不存在".into())
}

fn read_panel_run(conn: &Connection, id: &str) -> Result<ComicPanelRun, String> {
    conn.query_row(
        "SELECT id, panel_id, task_id, asset_id, parent_run_id, strategy, request_json, reference_snapshot_json, score_json, status, error, created_at, finished_at,
                attempt_no, generation_attempt_id, owner_app_session_id, failure_json, submitted_at, heartbeat_at, lease_expires_at
         FROM comic_panel_runs WHERE id = ?",
        params![id],
        |row| {
            Ok(ComicPanelRun {
                id: row.get(0)?,
                panel_id: row.get(1)?,
                task_id: row.get(2)?,
                asset_id: row.get(3)?,
                parent_run_id: row.get(4)?,
                strategy: row.get(5)?,
                request_json: parse_json(row.get(6)?),
                reference_snapshot_json: parse_json(row.get(7)?),
                score_json: row.get::<_, Option<String>>(8)?.map(parse_json),
                status: row.get(9)?,
                error: row.get(10)?,
                created_at: row.get(11)?,
                finished_at: row.get(12)?,
                attempt_no: row.get(13)?,
                generation_attempt_id: row.get(14)?,
                owner_app_session_id: row.get(15)?,
                failure_json: row.get::<_, Option<String>>(16)?.map(parse_json),
                submitted_at: row.get(17)?,
                heartbeat_at: row.get(18)?,
                lease_expires_at: row.get(19)?,
            })
        },
    )
    .map_err(|e| format!("读取格子生成记录失败: {e}"))
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPanelRunCreateInput {
    pub comic_project_id: String,
    pub panel_id: String,
    pub strategy: String,
    pub request_json: Value,
    pub reference_snapshot_json: Value,
    pub parent_run_id: Option<String>,
    pub idempotency_key: Option<String>,
}

#[tauri::command]
pub fn comic_panel_run_create(
    state: tauri::State<'_, DbState>,
    input: ComicPanelRunCreateInput,
) -> Result<ComicPanelRun, String> {
    db::with_connection(&state, |conn| {
        if input.idempotency_key.is_some() {
            comic_panel_run_create_v5_inner(conn, input, state.app_session_id())
        } else {
            comic_panel_run_create_inner(conn, input)
        }
    })
}

fn comic_panel_run_create_inner(
    conn: &Connection,
    input: ComicPanelRunCreateInput,
) -> Result<ComicPanelRun, String> {
    let _ = get_project(conn, &input.comic_project_id)?;
    ensure_panel_in_project(conn, &input.panel_id, &input.comic_project_id)?;
    if let Some(parent_run_id) = &input.parent_run_id {
        let parent = read_panel_run(conn, parent_run_id)?;
        if parent.panel_id != input.panel_id {
            return Err("父级生成记录不属于该漫画格".into());
        }
    }
    let run = ComicPanelRun {
        id: new_id("cpnrun"),
        panel_id: input.panel_id,
        task_id: None,
        asset_id: None,
        parent_run_id: input.parent_run_id,
        strategy: input.strategy,
        request_json: input.request_json,
        reference_snapshot_json: input.reference_snapshot_json,
        score_json: None,
        status: if input.idempotency_key.is_some() {
            "queued"
        } else {
            "running"
        }
        .into(),
        error: None,
        created_at: now(),
        finished_at: None,
        attempt_no: 1,
        generation_attempt_id: Some(new_id("gatt")),
        owner_app_session_id: None,
        failure_json: None,
        submitted_at: None,
        heartbeat_at: None,
        lease_expires_at: Some(now() + RUN_LEASE_MS),
    };
    conn.execute(
        "INSERT INTO comic_panel_runs (id, panel_id, task_id, asset_id, parent_run_id, strategy, request_json, reference_snapshot_json, score_json, status, error, created_at, finished_at,
          attempt_no, generation_attempt_id, owner_app_session_id, failure_json, submitted_at, heartbeat_at, lease_expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            run.id,
            run.panel_id,
            run.task_id,
            run.asset_id,
            run.parent_run_id,
            run.strategy,
            json_text(&run.request_json),
            json_text(&run.reference_snapshot_json),
            run.score_json.as_ref().map(json_text),
            run.status,
            run.error,
            run.created_at,
            run.finished_at
            , run.attempt_no, run.generation_attempt_id, run.owner_app_session_id,
            run.failure_json.as_ref().map(json_text), run.submitted_at, run.heartbeat_at, run.lease_expires_at
        ],
    )
    .map_err(|e| format!("保存格子生成记录失败: {e}"))?;
    Ok(run)
}

fn comic_panel_run_create_v5_inner(
    conn: &Connection,
    input: ComicPanelRunCreateInput,
    app_session_id: &str,
) -> Result<ComicPanelRun, String> {
    let key = input
        .idempotency_key
        .clone()
        .ok_or("v5 create 需要 idempotencyKey")?;
    let request =
        serde_json::to_value(&input).map_err(|e| format!("序列化格子生成请求失败: {e}"))?;
    with_receipt(conn, "comic_panel_run_create", &key, &request, |conn| {
        let _ = comic_production_contract()?;
        let _ = get_project(conn, &input.comic_project_id)?;
        ensure_panel_in_project(conn, &input.panel_id, &input.comic_project_id)?;
        let attempt_no = if let Some(parent_run_id) = &input.parent_run_id {
            let parent = read_panel_run(conn, parent_run_id)?;
            if parent.panel_id != input.panel_id {
                return Err("父级生成记录不属于该漫画格".into());
            }
            if parent.status != "error" {
                return Err("仅 error 或已放弃 stale 的父记录可以重试".into());
            }
            ensure_retry_parent_has_no_child(conn, "panel", parent_run_id)?;
            next_attempt_no(parent.attempt_no)?
        } else {
            1
        };
        let ts = now();
        let run = ComicPanelRun {
            id: new_id("cpnrun"),
            panel_id: input.panel_id,
            task_id: None,
            asset_id: None,
            parent_run_id: input.parent_run_id,
            strategy: input.strategy,
            request_json: input.request_json,
            reference_snapshot_json: input.reference_snapshot_json,
            score_json: None,
            status: "queued".into(),
            error: None,
            created_at: ts,
            finished_at: None,
            attempt_no,
            generation_attempt_id: Some(Uuid::new_v4().to_string()),
            owner_app_session_id: Some(app_session_id.to_owned()),
            failure_json: None,
            submitted_at: None,
            heartbeat_at: Some(ts),
            lease_expires_at: Some(ts + RUN_LEASE_MS),
        };
        conn.execute(
            "INSERT INTO comic_panel_runs (id, panel_id, task_id, asset_id, parent_run_id, strategy, request_json, reference_snapshot_json, score_json, status, error, created_at, finished_at, attempt_no, generation_attempt_id, owner_app_session_id, failure_json, submitted_at, heartbeat_at, lease_expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![run.id, run.panel_id, run.task_id, run.asset_id, run.parent_run_id, run.strategy, json_text(&run.request_json), json_text(&run.reference_snapshot_json), Option::<String>::None, run.status, run.error, run.created_at, run.finished_at, run.attempt_no, run.generation_attempt_id, run.owner_app_session_id, Option::<String>::None, run.submitted_at, run.heartbeat_at, run.lease_expires_at],
        ).map_err(|e| format!("保存 queued 格子生成记录失败: {e}"))?;
        append_run_event(
            conn,
            RunEventInput {
                kind: "panel",
                run_id: &run.id,
                event_type: "created",
                from_status: None,
                to_status: Some("queued"),
                payload: json!({ "parentRunId": run.parent_run_id }),
                idempotency_key: Some(&key),
                generation_attempt_id: run.generation_attempt_id.as_deref(),
                owner_app_session_id: Some(app_session_id),
            },
        )?;
        Ok(run)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPanelRunFinishInput {
    pub id: String,
    pub asset_id: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub score_json: Option<Value>,
}

#[tauri::command]
pub fn comic_panel_run_finish(
    state: tauri::State<'_, DbState>,
    input: ComicPanelRunFinishInput,
) -> Result<ComicPanelRun, String> {
    db::with_connection(&state, |conn| comic_panel_run_finish_inner(conn, input))
}

fn comic_panel_run_finish_inner(
    conn: &Connection,
    input: ComicPanelRunFinishInput,
) -> Result<ComicPanelRun, String> {
    if input.status != "success" && input.status != "error" {
        return Err(format!("非法的格子生成状态: {}", input.status));
    }
    let current = read_panel_run(conn, &input.id).map_err(|error| {
        if error.contains("读取格子生成记录失败") {
            "格子生成记录不存在".to_string()
        } else {
            error
        }
    })?;
    if current.status != "running" {
        return Err("格子生成记录已结束，不能重复完成".into());
    }
    if input.status == "success" {
        let asset_id = input
            .asset_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "格子生成成功时必须提供素材 ID".to_string())?;
        let comic_project_id = panel_comic_project_id(conn, &current.panel_id)?;
        ensure_asset_in_project(conn, asset_id, &comic_project_id)?;
    }
    let error = if input.status == "error" {
        input.error.as_deref().map(safe_failure_text)
    } else {
        None
    };
    let finished_at = now();
    let updated = conn
        .execute(
            "UPDATE comic_panel_runs SET asset_id = ?, status = ?, error = ?, score_json = ?, finished_at = ? WHERE id = ? AND status = 'running'",
            params![
                input.asset_id,
                input.status,
                error,
                input.score_json.as_ref().map(json_text),
                finished_at,
                input.id
            ],
    )
    .map_err(|e| format!("更新格子生成记录失败: {e}"))?;
    if updated == 0 {
        return Err("格子生成记录已结束，不能重复完成".into());
    }
    read_panel_run(conn, &input.id)
}

#[tauri::command]
pub fn comic_panel_runs_list(
    state: tauri::State<'_, DbState>,
    panel_id: Option<String>,
    comic_project_id: Option<String>,
    input: Option<ComicRunListInput>,
) -> Result<Value, String> {
    db::with_connection(&state, |conn| {
        if let Some(input) = input {
            let _ = get_project(conn, &input.comic_project_id)?;
            serde_json::to_value(history_page(conn, "panel", &input)?)
                .map_err(|e| format!("序列化格子生成历史失败: {e}"))
        } else {
            let panel_id = panel_id.ok_or("panelId 不能为空")?;
            serde_json::to_value(comic_panel_runs_list_inner(
                conn,
                &panel_id,
                comic_project_id.as_deref(),
            )?)
            .map_err(|e| format!("序列化格子生成记录失败: {e}"))
        }
    })
}

fn comic_panel_runs_list_inner(
    conn: &Connection,
    panel_id: &str,
    comic_project_id: Option<&str>,
) -> Result<Vec<ComicPanelRun>, String> {
    if let Some(comic_project_id) = comic_project_id {
        ensure_panel_in_project(conn, panel_id, comic_project_id)?;
    } else {
        let _ = panel_comic_project_id(conn, panel_id)?;
    }
    let mut stmt = conn
        .prepare("SELECT id FROM comic_panel_runs WHERE panel_id = ? ORDER BY created_at DESC")
        .map_err(|e| format!("准备格子生成记录查询失败: {e}"))?;
    let ids = stmt
        .query_map(params![panel_id], |row| row.get::<_, String>(0))
        .map_err(|e| format!("查询格子生成记录失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取格子生成记录失败: {e}"))?;
    ids.iter().map(|id| read_panel_run(conn, id)).collect()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPageReview {
    pub id: String,
    pub comic_project_id: String,
    pub page_no: i64,
    pub page_run_id: String,
    pub decision: String,
    pub report_json: Value,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPageApprovalHead {
    pub comic_project_id: String,
    pub page_no: i64,
    pub approved_page_run_id: String,
    pub approved_asset_id: String,
    pub optimistic_version: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPageReviewState {
    pub head: Option<ComicPageApprovalHead>,
    pub reviews: Vec<ComicPageReview>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPageReviewStateInput {
    pub comic_project_id: String,
    pub page_no: i64,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPageReviewSubmitInput {
    pub comic_project_id: String,
    pub page_run_id: String,
    pub decision: String,
    pub report_json: Value,
    pub expected_optimistic_version: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPageReviewResult {
    pub review: ComicPageReview,
    pub head: Option<ComicPageApprovalHead>,
}

fn read_page_approval_head(
    conn: &Connection,
    comic_project_id: &str,
    page_no: i64,
) -> Result<Option<ComicPageApprovalHead>, String> {
    conn.query_row(
        "SELECT comic_project_id,page_no,approved_page_run_id,approved_asset_id,optimistic_version,updated_at
         FROM comic_page_approval_heads WHERE comic_project_id=? AND page_no=?",
        params![comic_project_id, page_no],
        |row| {
            Ok(ComicPageApprovalHead {
                comic_project_id: row.get(0)?,
                page_no: row.get(1)?,
                approved_page_run_id: row.get(2)?,
                approved_asset_id: row.get(3)?,
                optimistic_version: row.get(4)?,
                updated_at: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(|error| format!("读取页面批准状态失败: {error}"))
}

fn read_page_review_state(
    conn: &Connection,
    comic_project_id: &str,
    page_no: i64,
) -> Result<ComicPageReviewState, String> {
    let head = read_page_approval_head(conn, comic_project_id, page_no)?;
    let mut stmt = conn
        .prepare(
            "SELECT id,comic_project_id,page_no,page_run_id,decision,report_json,created_at
             FROM comic_page_reviews WHERE comic_project_id=? AND page_no=?
             ORDER BY created_at DESC,id DESC",
        )
        .map_err(|error| format!("准备页面质检历史失败: {error}"))?;
    let reviews = stmt
        .query_map(params![comic_project_id, page_no], |row| {
            Ok(ComicPageReview {
                id: row.get(0)?,
                comic_project_id: row.get(1)?,
                page_no: row.get(2)?,
                page_run_id: row.get(3)?,
                decision: row.get(4)?,
                report_json: parse_json(row.get(5)?),
                created_at: row.get(6)?,
            })
        })
        .map_err(|error| format!("查询页面质检历史失败: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("读取页面质检历史失败: {error}"))?;
    Ok(ComicPageReviewState { head, reviews })
}

#[tauri::command]
pub fn comic_page_review_state(
    state: tauri::State<'_, DbState>,
    input: ComicPageReviewStateInput,
) -> Result<ComicPageReviewState, String> {
    db::with_connection(&state, |conn| {
        let _ = get_project(conn, &input.comic_project_id)?;
        ensure_positive(input.page_no, "pageNo")?;
        read_page_review_state(conn, &input.comic_project_id, input.page_no)
    })
}

fn comic_page_review_submit_inner(
    conn: &Connection,
    input: ComicPageReviewSubmitInput,
) -> Result<ComicPageReviewResult, String> {
    let _ = get_project(conn, &input.comic_project_id)?;
    if input.decision != "approved" && input.decision != "rejected" {
        return Err("页面质检结论必须是 approved 或 rejected".into());
    }
    if input.expected_optimistic_version < 0 {
        return Err("expectedOptimisticVersion 不能为负数".into());
    }
    if !input.report_json.is_object() {
        return Err("页面质检报告必须是 JSON 对象".into());
    }
    if json_text(&input.report_json).len() > 64 * 1024 {
        return Err("页面质检报告不能超过 64 KiB".into());
    }
    let run = read_page_run(conn, &input.page_run_id)?;
    if run.comic_project_id != input.comic_project_id {
        return Err("页面候选不属于当前漫画项目".into());
    }
    if run.status != "success" {
        return Err("只有成功且绑定资产的页面候选可以质检".into());
    }
    let asset_id = run
        .asset_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or("页面候选没有可用资产")?;
    ensure_asset_in_project(conn, &asset_id, &input.comic_project_id)?;
    let request = serde_json::to_value(&input).map_err(|error| error.to_string())?;
    let key = input.idempotency_key.clone();
    with_receipt(conn, "comic_page_review_submit", &key, &request, |conn| {
        if conn
            .query_row(
                "SELECT 1 FROM comic_page_reviews WHERE page_run_id=?",
                params![input.page_run_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("查询页面候选质检状态失败: {error}"))?
            .is_some()
        {
            return Err("该页面候选已经完成质检，不能修改不可变结论".into());
        }
        let current = read_page_approval_head(conn, &input.comic_project_id, run.page_no)?;
        if input.decision == "approved" {
            let current_version = current
                .as_ref()
                .map(|head| head.optimistic_version)
                .unwrap_or(0);
            if current_version != input.expected_optimistic_version {
                return Err(format!(
                    "页面批准版本已变化：期望 {}，当前 {}",
                    input.expected_optimistic_version, current_version
                ));
            }
        }
        let review = ComicPageReview {
            id: new_id("cpage_review"),
            comic_project_id: input.comic_project_id.clone(),
            page_no: run.page_no,
            page_run_id: run.id.clone(),
            decision: input.decision.clone(),
            report_json: input.report_json.clone(),
            created_at: now(),
        };
        conn.execute(
                "INSERT INTO comic_page_reviews(id,comic_project_id,page_no,page_run_id,decision,report_json,created_at)
                 VALUES (?,?,?,?,?,?,?)",
                params![
                    review.id,
                    review.comic_project_id,
                    review.page_no,
                    review.page_run_id,
                    review.decision,
                    json_text(&review.report_json),
                    review.created_at
                ],
            )
            .map_err(|error| format!("保存页面质检结论失败: {error}"))?;
        if input.decision == "approved" {
            if current.is_some() {
                let changed = conn
                        .execute(
                            "UPDATE comic_page_approval_heads
                             SET approved_page_run_id=?,approved_asset_id=?,optimistic_version=optimistic_version+1,updated_at=?
                             WHERE comic_project_id=? AND page_no=? AND optimistic_version=?",
                            params![run.id, asset_id, now(), input.comic_project_id, run.page_no, input.expected_optimistic_version],
                        )
                        .map_err(|error| format!("更新页面批准版本失败: {error}"))?;
                if changed != 1 {
                    return Err("页面批准版本已变化，请刷新后重试".into());
                }
            } else {
                conn.execute(
                        "INSERT INTO comic_page_approval_heads(comic_project_id,page_no,approved_page_run_id,approved_asset_id,optimistic_version,updated_at)
                         VALUES (?,?,?,?,1,?)",
                        params![input.comic_project_id, run.page_no, run.id, asset_id, now()],
                    )
                    .map_err(|error| format!("创建页面批准版本失败: {error}"))?;
            }
        }
        Ok(ComicPageReviewResult {
            review,
            head: read_page_approval_head(conn, &input.comic_project_id, run.page_no)?,
        })
    })
}

#[tauri::command]
pub fn comic_page_review_submit(
    state: tauri::State<'_, DbState>,
    input: ComicPageReviewSubmitInput,
) -> Result<ComicPageReviewResult, String> {
    db::with_connection(&state, |conn| comic_page_review_submit_inner(conn, input))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicQcReportSaveInput {
    pub comic_project_id: String,
    pub panel_run_id: String,
    pub report_json: Value,
    pub decision: String,
}

#[tauri::command]
pub fn comic_qc_report_save(
    state: tauri::State<'_, DbState>,
    input: ComicQcReportSaveInput,
) -> Result<ComicQcReport, String> {
    db::with_connection(&state, |conn| comic_qc_report_save_inner(conn, input))
}

fn comic_qc_report_save_inner(
    conn: &Connection,
    input: ComicQcReportSaveInput,
) -> Result<ComicQcReport, String> {
    let _ = get_project(conn, &input.comic_project_id)?;
    if input.decision != "approved" && input.decision != "rejected" {
        return Err(format!("非法的质检结论: {}", input.decision));
    }
    ensure_panel_run_in_project(conn, &input.panel_run_id, &input.comic_project_id)?;
    let report = ComicQcReport {
        id: new_id("cqc"),
        panel_run_id: input.panel_run_id,
        report_json: input.report_json,
        decision: input.decision,
        created_at: now(),
    };
    conn.execute(
        "INSERT INTO comic_qc_reports (id, panel_run_id, report_json, decision, created_at)
         VALUES (?, ?, ?, ?, ?)",
        params![
            report.id,
            report.panel_run_id,
            json_text(&report.report_json),
            report.decision,
            report.created_at
        ],
    )
    .map_err(|e| format!("保存质检报告失败: {e}"))?;
    Ok(report)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPanelSetApprovedRunInput {
    pub comic_project_id: String,
    pub panel_id: String,
    pub run_id: String,
}

#[tauri::command]
pub fn comic_panel_set_approved_run(
    state: tauri::State<'_, DbState>,
    input: ComicPanelSetApprovedRunInput,
) -> Result<ComicPanel, String> {
    db::with_connection(&state, |conn| {
        comic_panel_set_approved_run_inner(conn, input)
    })
}

fn comic_panel_set_approved_run_inner(
    conn: &Connection,
    input: ComicPanelSetApprovedRunInput,
) -> Result<ComicPanel, String> {
    let _ = get_project(conn, &input.comic_project_id)?;
    ensure_panel_in_project(conn, &input.panel_id, &input.comic_project_id)?;
    let run = read_panel_run(conn, &input.run_id)?;
    if run.panel_id != input.panel_id {
        return Err("生成记录不属于该漫画格".into());
    }
    if run.status != "success" {
        return Err("只允许采纳成功状态的生成记录".into());
    }
    let asset_id = run
        .asset_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "只允许采纳绑定了有效项目素材的生成记录".to_string())?;
    ensure_asset_in_project(conn, asset_id, &input.comic_project_id)?;
    let updated = conn
        .execute(
            "UPDATE comic_panels SET approved_run_id = ?, updated_at = ? WHERE id = ?",
            params![input.run_id, now(), input.panel_id],
        )
        .map_err(|e| format!("更新漫画格采纳记录失败: {e}"))?;
    if updated == 0 {
        return Err("漫画格不存在或不属于当前漫画项目".into());
    }
    get_panel(conn, &input.panel_id)
}

// ---- v5 immutable attempt API -------------------------------------------------

fn page_run_value(conn: &Connection, id: &str) -> Result<Value, String> {
    serde_json::to_value(read_page_run(conn, id)?)
        .map_err(|e| format!("序列化整页生成记录失败: {e}"))
}

fn panel_run_value(conn: &Connection, id: &str) -> Result<Value, String> {
    serde_json::to_value(read_panel_run(conn, id)?)
        .map_err(|e| format!("序列化格子生成记录失败: {e}"))
}

fn ensure_attempt_asset(
    conn: &Connection,
    asset_id: &str,
    comic_project_id: &str,
    run_id: &str,
    generation_attempt_id: &str,
) -> Result<(), String> {
    ensure_asset_in_project(conn, asset_id, comic_project_id)?;
    let metadata: String = conn
        .query_row(
            "SELECT metadata FROM assets WHERE id = ?",
            params![asset_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("读取素材 provenance 失败: {e}"))?;
    let metadata = serde_json::from_str::<Value>(&metadata)
        .map_err(|_| "素材缺少有效的漫画生成 provenance".to_string())?;
    if metadata.get("comicRunId").and_then(Value::as_str) != Some(run_id)
        || metadata.get("generationAttemptId").and_then(Value::as_str)
            != Some(generation_attempt_id)
    {
        return Err("素材不属于该生成 attempt，不能完成记录".into());
    }
    Ok(())
}

fn run_table(kind: &str) -> Result<&'static str, String> {
    match kind {
        "page" => Ok("comic_page_runs"),
        "panel" => Ok("comic_panel_runs"),
        _ => Err("kind 必须是 page 或 panel".into()),
    }
}

fn ensure_retry_parent_has_no_child(
    conn: &Connection,
    kind: &str,
    parent_run_id: &str,
) -> Result<(), String> {
    let table = run_table(kind)?;
    let has_child: Option<i64> = conn
        .query_row(
            &format!("SELECT 1 FROM {table} WHERE parent_run_id = ? LIMIT 1"),
            params![parent_run_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("查询 child attempt 失败: {error}"))?;
    if has_child.is_some() {
        return Err("父生成记录已有 child attempt，必须从该 child 继续重试".into());
    }
    Ok(())
}

fn next_attempt_no(parent_attempt_no: i64) -> Result<i64, String> {
    parent_attempt_no
        .checked_add(1)
        .ok_or_else(|| "父级生成记录的 attemptNo 超出可重试范围".into())
}

#[derive(Clone)]
struct RunOwnership {
    comic_project_id: String,
    status: String,
    generation_attempt_id: String,
    owner_app_session_id: Option<String>,
}

fn read_run_ownership(conn: &Connection, kind: &str, run_id: &str) -> Result<RunOwnership, String> {
    match kind {
        "page" => conn.query_row(
            "SELECT comic_project_id, status, generation_attempt_id, owner_app_session_id, lease_expires_at FROM comic_page_runs WHERE id = ?",
            params![run_id],
            |row| Ok(RunOwnership { comic_project_id: row.get(0)?, status: row.get(1)?, generation_attempt_id: row.get::<_, Option<String>>(2)?.unwrap_or_default(), owner_app_session_id: row.get(3)? }),
        ),
        "panel" => conn.query_row(
            "SELECT c.comic_project_id, r.status, r.generation_attempt_id, r.owner_app_session_id, r.lease_expires_at
             FROM comic_panel_runs r JOIN comic_panels p ON p.id = r.panel_id JOIN comic_scenes s ON s.id = p.scene_id JOIN comic_chapters c ON c.id = s.chapter_id WHERE r.id = ?",
            params![run_id],
            |row| Ok(RunOwnership { comic_project_id: row.get(0)?, status: row.get(1)?, generation_attempt_id: row.get::<_, Option<String>>(2)?.unwrap_or_default(), owner_app_session_id: row.get(3)? }),
        ),
        _ => return Err("kind 必须是 page 或 panel".into()),
    }
    .map_err(|_| "生成记录不存在或不属于当前漫画项目".to_string())
}

fn verify_attempt(input: &ComicRunMutationInput, current: &RunOwnership) -> Result<(), String> {
    if current.comic_project_id != input.comic_project_id {
        return Err("生成记录不存在或不属于当前漫画项目".into());
    }
    if current.generation_attempt_id != input.generation_attempt_id {
        return Err("generationAttemptId 与生成记录不匹配".into());
    }
    if current.status != input.expected_status {
        return Err(format!(
            "生成记录当前状态为 {}，不是期望的 {}",
            current.status, input.expected_status
        ));
    }
    Ok(())
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicRunMutationInput {
    pub kind: String,
    pub run_id: String,
    pub comic_project_id: String,
    pub generation_attempt_id: String,
    pub expected_status: String,
    pub idempotency_key: String,
}

#[tauri::command]
pub fn comic_run_mark_submitted(
    state: tauri::State<'_, DbState>,
    input: ComicRunMutationInput,
) -> Result<Value, String> {
    let session = state.app_session_id().to_owned();
    db::with_connection(&state, |conn| {
        comic_run_mark_submitted_inner(conn, input, &session)
    })
}

fn comic_run_mark_submitted_inner(
    conn: &Connection,
    input: ComicRunMutationInput,
    session: &str,
) -> Result<Value, String> {
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    with_receipt(
        conn,
        "comic_run_mark_submitted",
        &input.idempotency_key,
        &request,
        |conn| {
            if input.expected_status != "queued" {
                return Err("markSubmitted 的 expectedStatus 必须为 queued".into());
            }
            let current = read_run_ownership(conn, &input.kind, &input.run_id)?;
            verify_attempt(&input, &current)?;
            if current.owner_app_session_id.as_deref() != Some(session) {
                return Err("当前 Rust 会话不拥有该 attempt lease".into());
            }
            let table = run_table(&input.kind)?;
            let ts = now();
            let changed = conn.execute(&format!("UPDATE {table} SET status = 'running', submitted_at = ?, heartbeat_at = ?, lease_expires_at = ? WHERE id = ? AND status = 'queued' AND owner_app_session_id = ?"), params![ts, ts, ts + RUN_LEASE_MS, input.run_id, session])
                .map_err(|e| format!("标记生成已提交失败: {e}"))?;
            if changed != 1 {
                return Err("生成记录状态或 lease owner 已变化，不能标记已提交".into());
            }
            append_run_event(
                conn,
                RunEventInput {
                    kind: &input.kind,
                    run_id: &input.run_id,
                    event_type: "submitted",
                    from_status: Some("queued"),
                    to_status: Some("running"),
                    payload: json!({}),
                    idempotency_key: Some(&input.idempotency_key),
                    generation_attempt_id: Some(&input.generation_attempt_id),
                    owner_app_session_id: Some(session),
                },
            )?;
            if input.kind == "page" {
                page_run_value(conn, &input.run_id)
            } else {
                panel_run_value(conn, &input.run_id)
            }
        },
    )
}

#[tauri::command]
pub fn comic_run_heartbeat(
    state: tauri::State<'_, DbState>,
    input: ComicRunMutationInput,
) -> Result<Value, String> {
    let session = state.app_session_id().to_owned();
    db::with_connection(&state, |conn| {
        comic_run_heartbeat_inner(conn, input, &session)
    })
}

fn comic_run_heartbeat_inner(
    conn: &Connection,
    input: ComicRunMutationInput,
    session: &str,
) -> Result<Value, String> {
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    with_receipt(
        conn,
        "comic_run_heartbeat",
        &input.idempotency_key,
        &request,
        |conn| {
            if input.expected_status != "queued" && input.expected_status != "running" {
                return Err("heartbeat 的 expectedStatus 必须为 queued 或 running".into());
            }
            let current = read_run_ownership(conn, &input.kind, &input.run_id)?;
            verify_attempt(&input, &current)?;
            if current.owner_app_session_id.as_deref() != Some(session) {
                return Err("当前 Rust 会话不拥有该 attempt lease".into());
            }
            let table = run_table(&input.kind)?;
            let ts = now();
            let changed = conn.execute(&format!("UPDATE {table} SET heartbeat_at = ?, lease_expires_at = ? WHERE id = ? AND status = ? AND owner_app_session_id = ?"), params![ts, ts + RUN_LEASE_MS, input.run_id, input.expected_status, session])
                .map_err(|e| format!("续约生成 lease 失败: {e}"))?;
            if changed != 1 {
                return Err("生成记录状态或 lease owner 已变化，不能续约".into());
            }
            append_run_event(
                conn,
                RunEventInput {
                    kind: &input.kind,
                    run_id: &input.run_id,
                    event_type: "heartbeat",
                    from_status: Some(&input.expected_status),
                    to_status: Some(&input.expected_status),
                    payload: json!({ "leaseExpiresAt": ts + RUN_LEASE_MS }),
                    idempotency_key: Some(&input.idempotency_key),
                    generation_attempt_id: Some(&input.generation_attempt_id),
                    owner_app_session_id: Some(session),
                },
            )?;
            if input.kind == "page" {
                page_run_value(conn, &input.run_id)
            } else {
                panel_run_value(conn, &input.run_id)
            }
        },
    )
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicRunFinishV5Input {
    #[serde(flatten)]
    pub run: ComicRunMutationInput,
    pub outcome: Value,
}

fn failure_from_outcome(outcome: &Value, phase: &str) -> Value {
    let supplied = outcome.get("failureJson").unwrap_or(outcome);
    json!({"phase":sanitize_failure_phase(supplied.get("phase").and_then(Value::as_str).unwrap_or(phase)),"code":sanitize_failure_code(supplied.get("code").and_then(Value::as_str).unwrap_or("GENERATION_FAILED")),"message":safe_failure_text(supplied.get("message").or_else(||outcome.get("error")).and_then(Value::as_str).unwrap_or("生成失败")),"retryable":supplied.get("retryable").and_then(Value::as_bool).unwrap_or(true),"observedAt":now(),"providerRequestId":supplied.get("providerRequestId").and_then(Value::as_str).map(safe_failure_text),"providerStatus":supplied.get("providerStatus").and_then(Value::as_str).map(safe_failure_text)})
}

fn sanitize_failure_phase(value: &str) -> &'static str {
    match value.trim() {
        "validate" => "validate",
        "create" => "create",
        "dispatch" => "dispatch",
        "provider" => "provider",
        "asset_persist" => "asset_persist",
        "finish" => "finish",
        "reconcile" => "reconcile",
        "abandon" => "abandon",
        _ => "finish",
    }
}

fn sanitize_failure_code(value: &str) -> String {
    let value = value.trim();
    let safe = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        && !value.to_ascii_lowercase().contains("secret");
    if safe {
        value.into()
    } else {
        "GENERATION_FAILED".into()
    }
}

/// Failure JSON is a deliberately small allowlist.  A provider error can be
/// useful to diagnose, but it must never turn an authentication credential
/// into durable run history, events, or an idempotency receipt.
fn safe_failure_text(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "生成失败，未提供详情".into();
    }
    let lower = value.to_ascii_lowercase();
    if [
        "authorization",
        "api_key",
        "apikey",
        "access_token",
        "accesstoken",
        "secret_key",
        "secretkey",
        "password",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        "生成服务认证失败，敏感信息已隐藏".into()
    } else {
        crate::logging::error_text(value)
    }
}

fn comic_run_finish_inner(
    conn: &Connection,
    input: ComicRunFinishV5Input,
    session: &str,
) -> Result<Value, String> {
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    with_receipt(
        conn,
        "comic_run_finish",
        &input.run.idempotency_key,
        &request,
        |conn| {
            if input.run.expected_status != "running" {
                return Err("finish 的 expectedStatus 必须为 running".into());
            }
            let status = input
                .outcome
                .get("status")
                .and_then(Value::as_str)
                .ok_or("outcome.status 必须为 success 或 error")?;
            if status != "success" && status != "error" {
                return Err("outcome.status 必须为 success 或 error".into());
            }
            let current = read_run_ownership(conn, &input.run.kind, &input.run.run_id)?;
            verify_attempt(&input.run, &current)?;
            if current.owner_app_session_id.as_deref() != Some(session) {
                return Err("当前 Rust 会话不拥有该 attempt lease".into());
            }
            let asset_id = input.outcome.get("assetId").and_then(Value::as_str);
            if status == "success" {
                ensure_attempt_asset(
                    conn,
                    asset_id
                        .filter(|id| !id.trim().is_empty())
                        .ok_or("success outcome 必须提供 assetId")?,
                    &input.run.comic_project_id,
                    &input.run.run_id,
                    &input.run.generation_attempt_id,
                )?;
            }
            let table = run_table(&input.run.kind)?;
            let failure_value =
                (status == "error").then(|| failure_from_outcome(&input.outcome, "finish"));
            let failure = failure_value.as_ref().map(json_text);
            let error = failure_value
                .as_ref()
                .and_then(|value| value.get("message"))
                .and_then(Value::as_str);
            let changed=conn.execute(&format!("UPDATE {table} SET asset_id=?,status=?,error=?,failure_json=?,finished_at=?,lease_expires_at=NULL WHERE id=? AND status='running'"),params![asset_id,status,error,failure,now(),input.run.run_id]).map_err(|e|format!("完成生成记录失败: {e}"))?;
            if changed != 1 {
                return Err("生成记录已结束，不能覆盖终态".into());
            }
            append_run_event(
                conn,
                RunEventInput {
                    kind: &input.run.kind,
                    run_id: &input.run.run_id,
                    event_type: "finished",
                    from_status: Some("running"),
                    to_status: Some(status),
                    payload: json!({"status":status,"assetId":asset_id,"failure":failure_value}),
                    idempotency_key: Some(&input.run.idempotency_key),
                    generation_attempt_id: Some(&input.run.generation_attempt_id),
                    owner_app_session_id: current.owner_app_session_id.as_deref(),
                },
            )?;
            if input.run.kind == "page" {
                page_run_value(conn, &input.run.run_id)
            } else {
                panel_run_value(conn, &input.run.run_id)
            }
        },
    )
}

#[tauri::command]
pub fn comic_run_finish(
    state: tauri::State<'_, DbState>,
    input: ComicRunFinishV5Input,
) -> Result<Value, String> {
    let session = state.app_session_id().to_owned();
    db::with_connection(&state, |conn| comic_run_finish_inner(conn, input, &session))
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicPanelQcAndApproveInput {
    pub comic_project_id: String,
    pub panel_id: String,
    pub run_id: String,
    pub report_json: Value,
    pub decision: String,
    pub idempotency_key: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicPanelQcAndApproveResult {
    pub report: ComicQcReport,
    pub panel: ComicPanel,
}

#[tauri::command]
pub fn comic_panel_qc_and_approve(
    state: tauri::State<'_, DbState>,
    input: ComicPanelQcAndApproveInput,
) -> Result<ComicPanelQcAndApproveResult, String> {
    db::with_connection(&state, |conn| comic_panel_qc_and_approve_inner(conn, input))
}

fn comic_panel_qc_and_approve_inner(
    conn: &Connection,
    input: ComicPanelQcAndApproveInput,
) -> Result<ComicPanelQcAndApproveResult, String> {
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    with_receipt(
        conn,
        "comic_panel_qc_and_approve",
        &input.idempotency_key,
        &request,
        |conn| {
            let _ = get_project(conn, &input.comic_project_id)?;
            if input.decision != "approved" && input.decision != "rejected" {
                return Err("decision 必须为 approved 或 rejected".into());
            }
            ensure_panel_in_project(conn, &input.panel_id, &input.comic_project_id)?;
            ensure_panel_run_in_project(conn, &input.run_id, &input.comic_project_id)?;
            let run = read_panel_run(conn, &input.run_id)?;
            if run.panel_id != input.panel_id {
                return Err("生成记录不属于该漫画格".into());
            }
            if input.decision == "approved" {
                if run.status != "success" {
                    return Err("只允许批准 success 状态的生成记录".into());
                }
                let asset_id = run
                    .asset_id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
                    .ok_or("批准的生成记录必须绑定素材")?;
                ensure_asset_in_project(conn, asset_id, &input.comic_project_id)?;
            }
            let report = ComicQcReport {
                id: new_id("cqc"),
                panel_run_id: input.run_id.clone(),
                report_json: input.report_json.clone(),
                decision: input.decision.clone(),
                created_at: now(),
            };
            conn.execute("INSERT INTO comic_qc_reports (id, panel_run_id, report_json, decision, created_at) VALUES (?, ?, ?, ?, ?)", params![report.id, report.panel_run_id, json_text(&report.report_json), report.decision, report.created_at])
                .map_err(|e| format!("保存质检报告失败: {e}"))?;
            let (approved_run_id, status) = if input.decision == "approved" {
                (Some(input.run_id.as_str()), "approved")
            } else {
                (None, "needs_repair")
            };
            conn.execute("UPDATE comic_panels SET approved_run_id = COALESCE(?, approved_run_id), status = ?, updated_at = ? WHERE id = ?", params![approved_run_id, status, now(), input.panel_id])
                .map_err(|e| format!("更新漫画格质检状态失败: {e}"))?;
            Ok(ComicPanelQcAndApproveResult {
                report,
                panel: get_panel(conn, &input.panel_id)?,
            })
        },
    )
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicChapterScenePanelCreateInput {
    pub comic_project_id: String,
    pub chapter: Value,
    pub scene: Value,
    pub first_panel: Value,
    pub idempotency_key: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicChapterScenePanelCreateResult {
    pub chapter: ComicChapter,
    pub scene: ComicScene,
    pub first_panel: ComicPanel,
}

#[tauri::command]
pub fn comic_chapter_scene_panel_create(
    state: tauri::State<'_, DbState>,
    input: ComicChapterScenePanelCreateInput,
) -> Result<ComicChapterScenePanelCreateResult, String> {
    db::with_connection(&state, |conn| {
        comic_chapter_scene_panel_create_inner(conn, input)
    })
}

fn comic_chapter_scene_panel_create_inner(
    conn: &Connection,
    input: ComicChapterScenePanelCreateInput,
) -> Result<ComicChapterScenePanelCreateResult, String> {
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    with_receipt(
        conn,
        "comic_chapter_scene_panel_create",
        &input.idempotency_key,
        &request,
        |conn| {
            let project = get_project(conn, &input.comic_project_id)?;
            let mut chapter: ComicChapterInput = serde_json::from_value(input.chapter.clone())
                .map_err(|e| format!("chapter 输入无效: {e}"))?;
            let mut scene: ComicSceneInput = serde_json::from_value(input.scene.clone())
                .map_err(|e| format!("scene 输入无效: {e}"))?;
            let mut panel: ComicPanelInput = serde_json::from_value(input.first_panel.clone())
                .map_err(|e| format!("firstPanel 输入无效: {e}"))?;
            if chapter.id.is_some() || scene.id.is_some() || panel.id.is_some() {
                return Err("原子创建不能携带既有实体 ID".into());
            }
            ensure_positive(chapter.chapter_no, "chapterNo")?;
            ensure_positive(scene.scene_no, "sceneNo")?;
            ensure_positive(panel.page_no, "pageNo")?;
            ensure_positive(panel.panel_no, "panelNo")?;
            chapter.comic_project_id = input.comic_project_id.clone();
            let ts = now();
            let chapter_out = ComicChapter {
                id: new_id("cch"),
                comic_project_id: input.comic_project_id.clone(),
                chapter_no: chapter.chapter_no,
                title: chapter.title,
                source: chapter.source,
                outline: chapter.outline.unwrap_or(Value::Null),
                state_before: chapter.state_before.unwrap_or(project.current_state),
                state_after: Value::Null,
                status: chapter.status.unwrap_or_else(|| "draft".into()),
                created_at: ts,
                updated_at: ts,
            };
            conn.execute("INSERT INTO comic_chapters (id, comic_project_id, chapter_no, title, source_json, outline_json, state_before_json, state_after_json, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", params![chapter_out.id, chapter_out.comic_project_id, chapter_out.chapter_no, chapter_out.title, json_text(&chapter_out.source), json_text(&chapter_out.outline), json_text(&chapter_out.state_before), Option::<String>::None, chapter_out.status, ts, ts]).map_err(|e| format!("保存章节失败: {e}"))?;
            scene.comic_project_id = input.comic_project_id.clone();
            scene.chapter_id = chapter_out.id.clone();
            if let Some(asset) = scene.keyframe_asset_id.as_deref() {
                ensure_asset_in_project(conn, asset, &input.comic_project_id)?;
            }
            let scene_out = ComicScene {
                id: new_id("csc"),
                chapter_id: chapter_out.id.clone(),
                scene_no: scene.scene_no,
                spec: scene.spec,
                status: scene.status.unwrap_or_else(|| "draft".into()),
                keyframe_asset_id: scene.keyframe_asset_id,
                created_at: ts,
                updated_at: ts,
            };
            conn.execute("INSERT INTO comic_scenes (id, chapter_id, scene_no, spec_json, status, keyframe_asset_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)", params![scene_out.id, scene_out.chapter_id, scene_out.scene_no, json_text(&scene_out.spec), scene_out.status, scene_out.keyframe_asset_id, ts, ts]).map_err(|e| format!("保存场景失败: {e}"))?;
            panel.comic_project_id = input.comic_project_id.clone();
            panel.scene_id = scene_out.id.clone();
            let panel_out = ComicPanel {
                id: new_id("cpn"),
                scene_id: scene_out.id.clone(),
                page_no: panel.page_no,
                panel_no: panel.panel_no,
                spec: panel.spec,
                prompt: panel.prompt.unwrap_or(Value::Null),
                status: panel.status.unwrap_or_else(|| "draft".into()),
                approved_run_id: None,
                created_at: ts,
                updated_at: ts,
            };
            conn.execute("INSERT INTO comic_panels (id, scene_id, page_no, panel_no, spec_json, prompt_json, status, approved_run_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", params![panel_out.id, panel_out.scene_id, panel_out.page_no, panel_out.panel_no, json_text(&panel_out.spec), json_text(&panel_out.prompt), panel_out.status, Option::<String>::None, ts, ts]).map_err(|e| format!("保存漫画格失败: {e}"))?;
            Ok(ComicChapterScenePanelCreateResult {
                chapter: chapter_out,
                scene: scene_out,
                first_panel: panel_out,
            })
        },
    )
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicRunReconcileInput {
    #[serde(flatten)]
    pub run: ComicRunMutationInput,
    pub observation: Value,
}

#[tauri::command]
pub fn comic_run_reconcile(
    state: tauri::State<'_, DbState>,
    input: ComicRunReconcileInput,
) -> Result<Value, String> {
    db::with_connection(&state, |conn| comic_run_reconcile_inner(conn, input))
}

fn comic_run_reconcile_inner(
    conn: &Connection,
    input: ComicRunReconcileInput,
) -> Result<Value, String> {
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    with_receipt(
        conn,
        "comic_run_reconcile",
        &input.run.idempotency_key,
        &request,
        |conn| {
            if input.run.expected_status != "stale" {
                return Err("reconcile 的 expectedStatus 必须为 stale".into());
            }
            let current = read_run_ownership(conn, &input.run.kind, &input.run.run_id)?;
            verify_attempt(&input.run, &current)?;
            let asset_id = input
                .observation
                .get("assetId")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or("reconcile 需要同 attempt 的本地 assetId 证据")?;
            ensure_attempt_asset(
                conn,
                asset_id,
                &input.run.comic_project_id,
                &input.run.run_id,
                &input.run.generation_attempt_id,
            )?;
            let table = run_table(&input.run.kind)?;
            let changed = conn.execute(&format!("UPDATE {table} SET asset_id = ?, status = 'success', error = NULL, failure_json = NULL, finished_at = ?, lease_expires_at = NULL WHERE id = ? AND status = 'stale'"), params![asset_id, now(), input.run.run_id]).map_err(|e| format!("对账生成记录失败: {e}"))?;
            if changed != 1 {
                return Err("生成记录状态已变化，不能完成对账".into());
            }
            append_run_event(
                conn,
                RunEventInput {
                    kind: &input.run.kind,
                    run_id: &input.run.run_id,
                    event_type: "reconciled",
                    from_status: Some("stale"),
                    to_status: Some("success"),
                    payload: json!({ "assetId": asset_id }),
                    idempotency_key: Some(&input.run.idempotency_key),
                    generation_attempt_id: Some(&input.run.generation_attempt_id),
                    owner_app_session_id: current.owner_app_session_id.as_deref(),
                },
            )?;
            if input.run.kind == "page" {
                page_run_value(conn, &input.run.run_id)
            } else {
                panel_run_value(conn, &input.run.run_id)
            }
        },
    )
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicRunAbandonInput {
    #[serde(flatten)]
    pub run: ComicRunMutationInput,
    pub reason: String,
}

#[tauri::command]
pub fn comic_run_abandon_stale(
    state: tauri::State<'_, DbState>,
    input: ComicRunAbandonInput,
) -> Result<Value, String> {
    db::with_connection(&state, |conn| comic_run_abandon_stale_inner(conn, input))
}

fn comic_run_abandon_stale_inner(
    conn: &Connection,
    input: ComicRunAbandonInput,
) -> Result<Value, String> {
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    with_receipt(
        conn,
        "comic_run_abandon_stale",
        &input.run.idempotency_key,
        &request,
        |conn| {
            if input.run.expected_status != "stale" {
                return Err("abandon 的 expectedStatus 必须为 stale".into());
            }
            let current = read_run_ownership(conn, &input.run.kind, &input.run.run_id)?;
            verify_attempt(&input.run, &current)?;
            let table = run_table(&input.run.kind)?;
            let reason = safe_failure_text(&input.reason);
            let failure = json!({"phase":"abandon","code":"RECOVERY_ABANDONED","message":reason,"retryable":true,"observedAt":now()});
            let changed = conn.execute(&format!("UPDATE {table} SET status = 'error', error = ?, failure_json = ?, finished_at = ?, lease_expires_at = NULL WHERE id = ? AND status = 'stale'"), params![failure["message"].as_str(), json_text(&failure), now(), input.run.run_id]).map_err(|e| format!("放弃未知生成结果失败: {e}"))?;
            if changed != 1 {
                return Err("生成记录状态已变化，不能放弃对账".into());
            }
            append_run_event(
                conn,
                RunEventInput {
                    kind: &input.run.kind,
                    run_id: &input.run.run_id,
                    event_type: "abandoned",
                    from_status: Some("stale"),
                    to_status: Some("error"),
                    payload: failure,
                    idempotency_key: Some(&input.run.idempotency_key),
                    generation_attempt_id: Some(&input.run.generation_attempt_id),
                    owner_app_session_id: current.owner_app_session_id.as_deref(),
                },
            )?;
            if input.run.kind == "page" {
                page_run_value(conn, &input.run.run_id)
            } else {
                panel_run_value(conn, &input.run.run_id)
            }
        },
    )
}

/// Recovery intentionally only turns an expired local lease into stale.  It
/// never invents a provider task or dispatches a second request.
#[tauri::command]
pub fn comic_runs_recover_stale(state: tauri::State<'_, DbState>) -> Result<usize, String> {
    let session = state.app_session_id().to_owned();
    db::with_connection(&state, |conn| {
        comic_runs_recover_stale_inner(conn, &session)
    })
}

fn comic_runs_recover_stale_inner(conn: &Connection, _session: &str) -> Result<usize, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| format!("开始恢复事务失败: {e}"))?;
    let ts = now();
    let mut stale = Vec::new();
    for kind in ["page", "panel"] {
        let table = run_table(kind)?;
        let mut statement = tx.prepare(&format!("SELECT id, generation_attempt_id, owner_app_session_id, status FROM {table} WHERE status IN ('queued','running') AND lease_expires_at IS NOT NULL AND lease_expires_at <= ?")) .map_err(|e| format!("查询过期 lease 失败: {e}"))?;
        let rows = statement
            .query_map(params![ts], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("读取过期 lease 失败: {e}"))?;
        for row in rows {
            stale.push((kind.to_owned(), row.map_err(|e| e.to_string())?));
        }
    }
    let mut transitioned = 0usize;
    for (kind, (id, attempt, owner, status)) in &stale {
        let table = run_table(kind)?;
        let legacy = attempt.is_none();
        let code = if legacy {
            "LEGACY_RUNNING_NO_ATTEMPT"
        } else {
            "LEASE_EXPIRED"
        };
        let changed = tx.execute(&format!("UPDATE {table} SET status = 'stale', failure_json = ?, lease_expires_at = NULL WHERE id = ? AND status = ? AND lease_expires_at <= ?"), params![json_text(&json!({"phase":"reconcile","code":code,"message":"本地生成 lease 已过期，等待对账或放弃","retryable":true,"observedAt":ts})), id, status, ts]).map_err(|e| format!("标记 stale 失败: {e}"))?;
        if changed == 1 {
            transitioned += 1;
            append_run_event(
                &tx,
                RunEventInput {
                    kind,
                    run_id: id,
                    event_type: if legacy { "legacy_stale" } else { "stale" },
                    from_status: Some(status),
                    to_status: Some("stale"),
                    payload: json!({"reason":"lease_expired", "code": code}),
                    idempotency_key: None,
                    generation_attempt_id: attempt.as_deref(),
                    owner_app_session_id: owner.as_deref(),
                },
            )?;
        }
    }
    tx.commit().map_err(|e| format!("提交恢复事务失败: {e}"))?;
    Ok(transitioned)
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicRunRetryInput {
    #[serde(flatten)]
    pub run: ComicRunMutationInput,
    pub retry_reason: String,
}

#[tauri::command]
pub fn comic_run_retry(
    state: tauri::State<'_, DbState>,
    input: ComicRunRetryInput,
) -> Result<Value, String> {
    let session = state.app_session_id().to_owned();
    db::with_connection(&state, |conn| comic_run_retry_inner(conn, input, &session))
}

fn comic_run_retry_inner(
    conn: &Connection,
    input: ComicRunRetryInput,
    session: &str,
) -> Result<Value, String> {
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    with_receipt(
        conn,
        "comic_run_retry",
        &input.run.idempotency_key,
        &request,
        |conn| {
            if input.run.expected_status != "error" {
                return Err("retry 只接受 error 状态的父记录".into());
            }
            let current = read_run_ownership(conn, &input.run.kind, &input.run.run_id)?;
            verify_attempt(&input.run, &current)?;
            ensure_retry_parent_has_no_child(conn, &input.run.kind, &input.run.run_id)?;
            let ts = now();
            let retry_reason = safe_failure_text(&input.retry_reason);
            match input.run.kind.as_str() {
                "page" => {
                    let parent = read_page_run(conn, &input.run.run_id)?;
                    let child = ComicPageRun {
                        id: new_id("cprun"),
                        comic_project_id: parent.comic_project_id,
                        page_no: parent.page_no,
                        asset_id: None,
                        request_json: parent.request_json,
                        reference_snapshot_json: parent.reference_snapshot_json,
                        status: "queued".into(),
                        error: None,
                        created_at: ts,
                        finished_at: None,
                        parent_run_id: Some(parent.id),
                        attempt_no: next_attempt_no(parent.attempt_no)?,
                        generation_attempt_id: Some(Uuid::new_v4().to_string()),
                        owner_app_session_id: Some(session.to_owned()),
                        failure_json: None,
                        submitted_at: None,
                        heartbeat_at: Some(ts),
                        lease_expires_at: Some(ts + RUN_LEASE_MS),
                    };
                    conn.execute("INSERT INTO comic_page_runs (id,comic_project_id,page_no,asset_id,request_json,reference_snapshot_json,status,error,created_at,finished_at,parent_run_id,attempt_no,generation_attempt_id,owner_app_session_id,failure_json,submitted_at,heartbeat_at,lease_expires_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", params![child.id,child.comic_project_id,child.page_no,child.asset_id,json_text(&child.request_json),json_text(&child.reference_snapshot_json),child.status,child.error,child.created_at,child.finished_at,child.parent_run_id,child.attempt_no,child.generation_attempt_id,child.owner_app_session_id,Option::<String>::None,child.submitted_at,child.heartbeat_at,child.lease_expires_at]).map_err(|e|format!("创建重试记录失败: {e}"))?;
                    append_run_event(
                        conn,
                        RunEventInput {
                            kind: "page",
                            run_id: &child.id,
                            event_type: "retry_created",
                            from_status: None,
                            to_status: Some("queued"),
                            payload: json!({"parentRunId":input.run.run_id,"reason":retry_reason}),
                            idempotency_key: Some(&input.run.idempotency_key),
                            generation_attempt_id: child.generation_attempt_id.as_deref(),
                            owner_app_session_id: Some(session),
                        },
                    )?;
                    serde_json::to_value(child).map_err(|e| e.to_string())
                }
                "panel" => {
                    let parent = read_panel_run(conn, &input.run.run_id)?;
                    let child = ComicPanelRun {
                        id: new_id("cpnrun"),
                        panel_id: parent.panel_id,
                        task_id: None,
                        asset_id: None,
                        parent_run_id: Some(parent.id),
                        strategy: parent.strategy,
                        request_json: parent.request_json,
                        reference_snapshot_json: parent.reference_snapshot_json,
                        score_json: None,
                        status: "queued".into(),
                        error: None,
                        created_at: ts,
                        finished_at: None,
                        attempt_no: next_attempt_no(parent.attempt_no)?,
                        generation_attempt_id: Some(Uuid::new_v4().to_string()),
                        owner_app_session_id: Some(session.to_owned()),
                        failure_json: None,
                        submitted_at: None,
                        heartbeat_at: Some(ts),
                        lease_expires_at: Some(ts + RUN_LEASE_MS),
                    };
                    conn.execute("INSERT INTO comic_panel_runs (id,panel_id,task_id,asset_id,parent_run_id,strategy,request_json,reference_snapshot_json,score_json,status,error,created_at,finished_at,attempt_no,generation_attempt_id,owner_app_session_id,failure_json,submitted_at,heartbeat_at,lease_expires_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", params![child.id,child.panel_id,child.task_id,child.asset_id,child.parent_run_id,child.strategy,json_text(&child.request_json),json_text(&child.reference_snapshot_json),Option::<String>::None,child.status,child.error,child.created_at,child.finished_at,child.attempt_no,child.generation_attempt_id,child.owner_app_session_id,Option::<String>::None,child.submitted_at,child.heartbeat_at,child.lease_expires_at]).map_err(|e|format!("创建重试记录失败: {e}"))?;
                    append_run_event(
                        conn,
                        RunEventInput {
                            kind: "panel",
                            run_id: &child.id,
                            event_type: "retry_created",
                            from_status: None,
                            to_status: Some("queued"),
                            payload: json!({"parentRunId":input.run.run_id,"reason":retry_reason}),
                            idempotency_key: Some(&input.run.idempotency_key),
                            generation_attempt_id: child.generation_attempt_id.as_deref(),
                            owner_app_session_id: Some(session),
                        },
                    )?;
                    serde_json::to_value(child).map_err(|e| e.to_string())
                }
                _ => Err("kind 必须是 page 或 panel".into()),
            }
        },
    )
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComicRunListInput {
    pub comic_project_id: String,
    pub page_no: Option<i64>,
    pub panel_id: Option<String>,
    pub statuses: Option<Vec<String>>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicRunHistoryPage {
    pub items: Vec<Value>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
struct HistoryCursor {
    created_at: i64,
    id: String,
    scope_fingerprint: Option<String>,
}

fn normalized_history_statuses(input: &ComicRunListInput) -> Result<Vec<String>, String> {
    let mut statuses = input.statuses.clone().unwrap_or_default();
    for status in &statuses {
        if !["queued", "running", "success", "error", "stale"].contains(&status.as_str()) {
            return Err(format!("不支持的生成状态筛选: {status}"));
        }
    }
    statuses.sort();
    statuses.dedup();
    Ok(statuses)
}

fn history_scope_fingerprint(
    kind: &str,
    input: &ComicRunListInput,
    statuses: &[String],
) -> Result<String, String> {
    let (page_no, panel_id) = match kind {
        "page" => (input.page_no, None),
        "panel" => (None, input.panel_id.as_deref()),
        _ => return Err("kind 必须是 page 或 panel".into()),
    };
    let scope = json!({
        "version": 1,
        "kind": kind,
        "comicProjectId": input.comic_project_id,
        "pageNo": page_no,
        "panelId": panel_id,
        "statuses": statuses,
    });
    Ok(format!(
        "{:x}",
        Sha256::digest(json_text(&scope).as_bytes())
    ))
}

fn parse_history_cursor(
    raw: Option<&str>,
    expected_scope_fingerprint: &str,
) -> Result<Option<HistoryCursor>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let parts: Vec<&str> = raw.split('|').collect();
    let (time, id, scope_fingerprint) = match parts.as_slice() {
        [time, id] => (*time, *id, None),
        [time, id, fingerprint] if *fingerprint == expected_scope_fingerprint => {
            (*time, *id, Some((*fingerprint).to_owned()))
        }
        [_, _, _] => return Err("cursor 无效".into()),
        _ => return Err("cursor 无效".into()),
    };
    if time.is_empty() || id.is_empty() {
        return Err("cursor 无效".into());
    }
    Ok(Some(HistoryCursor {
        created_at: time.parse::<i64>().map_err(|_| "cursor 无效")?,
        id: id.to_owned(),
        scope_fingerprint,
    }))
}

const PAGE_HISTORY_BY_PAGE_SQL: &str =
    "SELECT id, created_at, status FROM comic_page_runs WHERE comic_project_id = ? AND page_no = ? ORDER BY created_at DESC, id DESC";
const PAGE_HISTORY_ALL_SQL: &str =
    "SELECT id, created_at, status FROM comic_page_runs WHERE comic_project_id = ? ORDER BY created_at DESC, id DESC";

fn history_page(
    conn: &Connection,
    kind: &str,
    input: &ComicRunListInput,
) -> Result<ComicRunHistoryPage, String> {
    let statuses = normalized_history_statuses(input)?;
    let scope_fingerprint = history_scope_fingerprint(kind, input, &statuses)?;
    let cursor = parse_history_cursor(input.cursor.as_deref(), &scope_fingerprint)?;
    let limit = input.limit.unwrap_or(30).clamp(1, 100);
    let mut candidates = Vec::with_capacity(limit.saturating_add(1));
    let mut cursor_matches_current_scope = cursor.is_none();
    let mut inspect_row = |id: String, created_at: i64, status: String| -> Result<bool, String> {
        let status_matches = statuses.is_empty() || statuses.iter().any(|value| value == &status);
        if let Some(cursor) = cursor.as_ref() {
            let matches_cursor_identity =
                id == cursor.id && (cursor.scope_fingerprint.is_some() || status_matches);
            if matches_cursor_identity {
                if created_at != cursor.created_at {
                    return Err("cursor 无效".into());
                }
                cursor_matches_current_scope = true;
            }
        }
        if !status_matches {
            return Ok(false);
        }
        if let Some(cursor) = cursor.as_ref() {
            if created_at > cursor.created_at
                || (created_at == cursor.created_at && id.as_str() >= cursor.id.as_str())
            {
                return Ok(false);
            }
        }
        candidates.push((id, created_at));
        Ok(candidates.len() > limit)
    };
    match kind {
        "page" => {
            if let Some(page_no) = input.page_no {
                ensure_positive(page_no, "pageNo")?;
                let mut statement = conn
                    .prepare(PAGE_HISTORY_BY_PAGE_SQL)
                    .map_err(|e| format!("准备整页历史查询失败: {e}"))?;
                let mut rows = statement
                    .query(params![input.comic_project_id, page_no])
                    .map_err(|e| format!("读取整页历史失败: {e}"))?;
                while let Some(row) = rows.next().map_err(|e| format!("读取整页历史失败: {e}"))?
                {
                    if inspect_row(
                        row.get(0).map_err(|e| format!("读取整页历史失败: {e}"))?,
                        row.get(1).map_err(|e| format!("读取整页历史失败: {e}"))?,
                        row.get(2).map_err(|e| format!("读取整页历史失败: {e}"))?,
                    )? {
                        break;
                    }
                }
            } else {
                let mut statement = conn
                    .prepare(PAGE_HISTORY_ALL_SQL)
                    .map_err(|e| format!("准备整页历史查询失败: {e}"))?;
                let mut rows = statement
                    .query(params![input.comic_project_id])
                    .map_err(|e| format!("读取整页历史失败: {e}"))?;
                while let Some(row) = rows.next().map_err(|e| format!("读取整页历史失败: {e}"))?
                {
                    if inspect_row(
                        row.get(0).map_err(|e| format!("读取整页历史失败: {e}"))?,
                        row.get(1).map_err(|e| format!("读取整页历史失败: {e}"))?,
                        row.get(2).map_err(|e| format!("读取整页历史失败: {e}"))?,
                    )? {
                        break;
                    }
                }
            }
        }
        "panel" => {
            let panel_id = input
                .panel_id
                .as_deref()
                .ok_or("panel 历史查询需要 panelId")?;
            ensure_panel_in_project(conn, panel_id, &input.comic_project_id)?;
            let mut statement = conn.prepare("SELECT id, created_at, status FROM comic_panel_runs WHERE panel_id = ? ORDER BY created_at DESC, id DESC").map_err(|e| format!("准备格子历史查询失败: {e}"))?;
            let mut rows = statement
                .query(params![panel_id])
                .map_err(|e| format!("读取格子历史失败: {e}"))?;
            while let Some(row) = rows.next().map_err(|e| format!("读取格子历史失败: {e}"))?
            {
                if inspect_row(
                    row.get(0).map_err(|e| format!("读取格子历史失败: {e}"))?,
                    row.get(1).map_err(|e| format!("读取格子历史失败: {e}"))?,
                    row.get(2).map_err(|e| format!("读取格子历史失败: {e}"))?,
                )? {
                    break;
                }
            }
        }
        _ => return Err("kind 必须是 page 或 panel".into()),
    }
    if !cursor_matches_current_scope {
        return Err("cursor 无效".into());
    }
    let more = candidates.len() > limit;
    candidates.truncate(limit);
    let next_cursor = if more {
        candidates
            .last()
            .map(|(id, created_at)| format!("{created_at}|{id}|{scope_fingerprint}"))
    } else {
        None
    };
    let items = candidates
        .into_iter()
        .map(|(id, _)| {
            if kind == "page" {
                page_run_value(conn, &id)
            } else {
                panel_run_value(conn, &id)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ComicRunHistoryPage { items, next_cursor })
}

#[tauri::command]
pub fn comic_page_runs_list(
    state: tauri::State<'_, DbState>,
    input: ComicRunListInput,
) -> Result<ComicRunHistoryPage, String> {
    db::with_connection(&state, |conn| {
        let _ = get_project(conn, &input.comic_project_id)?;
        history_page(conn, "page", &input)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbState;

    fn open_test_db(tag: &str) -> (std::path::PathBuf, DbState) {
        let dir = std::env::temp_dir().join(format!("image-client-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        (dir, state)
    }

    fn seed_two_projects(conn: &Connection) -> Result<(), String> {
        for (id, project_id, title) in [
            ("comic_1", "p_1", "旧照相馆"),
            ("comic_2", "p_2", "另一部漫画"),
        ] {
            conn.execute(
                "INSERT INTO comic_projects (id, project_id, title, format, status, config_json, current_state_json, created_at, updated_at)
                 VALUES (?, ?, ?, 'page', 'draft', '{}', '{}', 1, 1)",
                params![id, project_id, title],
            )
            .map_err(|e| format!("seed {id} 失败: {e}"))?;
        }
        Ok(())
    }

    fn seed_chapter(
        conn: &Connection,
        id: &str,
        comic_project_id: &str,
        chapter_no: i64,
    ) -> Result<(), String> {
        conn.execute(
            "INSERT INTO comic_chapters (id, comic_project_id, chapter_no, title, source_json, outline_json, state_before_json, state_after_json, status, created_at, updated_at)
             VALUES (?, ?, ?, NULL, '{}', NULL, '{}', NULL, 'draft', 1, 1)",
            params![id, comic_project_id, chapter_no],
        )
        .map_err(|e| format!("seed {id} 失败: {e}"))?;
        Ok(())
    }

    fn seed_scene(
        conn: &Connection,
        id: &str,
        chapter_id: &str,
        scene_no: i64,
    ) -> Result<(), String> {
        conn.execute(
            "INSERT INTO comic_scenes (id, chapter_id, scene_no, spec_json, status, keyframe_asset_id, created_at, updated_at)
             VALUES (?, ?, ?, '{}', 'draft', NULL, 1, 1)",
            params![id, chapter_id, scene_no],
        )
        .map_err(|e| format!("seed {id} 失败: {e}"))?;
        Ok(())
    }

    fn seed_card(conn: &Connection, id: &str, comic_project_id: &str) -> Result<(), String> {
        conn.execute(
            "INSERT INTO comic_cards (id, comic_project_id, card_type, entity_key, version, name, data_json, locked, created_at, updated_at)
             VALUES (?, ?, 'character', 'hero', 1, '主角', '{}', 0, 1, 1)",
            params![id, comic_project_id],
        )
        .map_err(|e| format!("seed {id} 失败: {e}"))?;
        Ok(())
    }

    fn seed_reference(conn: &Connection, id: &str, comic_project_id: &str) -> Result<(), String> {
        conn.execute(
            "INSERT INTO comic_references (id, comic_project_id, owner_type, owner_id, asset_id, role, weight, sort_order, approved, created_at)
             VALUES (?, ?, 'card', 'card_1', 'asset_1', 'ref', NULL, 1, 0, 1)",
            params![id, comic_project_id],
        )
        .map_err(|e| format!("seed {id} 失败: {e}"))?;
        Ok(())
    }

    fn seed_asset(conn: &Connection, id: &str, project_id: &str) -> Result<(), String> {
        conn.execute(
            "INSERT INTO assets (id, kind, path, created_at, metadata) VALUES (?, 'image', 'ref.png', 1, ?)",
            params![id, json!({ "projectId": project_id }).to_string()],
        )
        .map_err(|e| format!("seed {id} 失败: {e}"))?;
        Ok(())
    }

    #[test]
    fn creates_comic_snapshot_and_cards() {
        let (dir, state) = open_test_db("comic");
        let snapshot = db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            get_project(conn, "comic_1")
        })
        .unwrap();
        assert_eq!(snapshot.title, "旧照相馆");
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_snapshot_initialization_keeps_one_project_identity() {
        let (dir, state) = open_test_db("comic-snapshot-race");
        let db_path = dir.join("test.db");
        drop(state);

        for round in 0..32 {
            let project_id = format!("snapshot-race-{round}");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
            let mut workers = Vec::new();
            for _ in 0..2 {
                let db_path = db_path.clone();
                let project_id = project_id.clone();
                let barrier = barrier.clone();
                workers.push(std::thread::spawn(
                    move || -> Result<ComicSnapshot, String> {
                        let conn = Connection::open(db_path).map_err(|error| error.to_string())?;
                        conn.busy_timeout(std::time::Duration::from_secs(5))
                            .map_err(|error| error.to_string())?;
                        barrier.wait();
                        comic_snapshot_inner(&conn, project_id, Some("并发项目".into()))
                    },
                ));
            }
            barrier.wait();
            let first = workers.remove(0).join().unwrap().unwrap();
            let second = workers.remove(0).join().unwrap().unwrap();
            assert_eq!(
                first.project.id, second.project.id,
                "concurrent initialization must return one canonical comic project"
            );
            let project_count: i64 = Connection::open(&db_path)
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM comic_projects WHERE project_id = ?",
                    params![project_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(project_count, 1);
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_cross_project_chapter_update() {
        let (dir, state) = open_test_db("comic-chapter");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "cch_1", "comic_1", 1)?;
            let input = ComicChapterInput {
                id: Some("cch_1".into()),
                comic_project_id: "comic_2".into(),
                chapter_no: 9,
                title: Some("越权更新".into()),
                source: json!({}),
                outline: None,
                state_before: None,
                status: None,
            };
            let error = comic_chapter_save_inner(conn, input).unwrap_err();
            assert!(error.contains("不属于当前漫画项目"), "实际错误: {error}");
            let chapter_no: i64 = conn
                .query_row(
                    "SELECT chapter_no FROM comic_chapters WHERE id = 'cch_1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(chapter_no, 1, "跨项目更新不应改动原章节");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reference_add_is_idempotent_by_business_key_and_rejects_cross_project_operations() {
        let (dir, state) = open_test_db("comic-reference");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "cch_1", "comic_1", 1)?;
            seed_card(conn, "card_1", "comic_1")?;
            seed_asset(conn, "asset_1", "p_1")?;
            seed_asset(conn, "asset_foreign", "p_2")?;
            seed_reference(conn, "cref_1", "comic_1")?;

            // 越权批准：参考图属于 comic_1，却以 comic_2 名义更新
            let error =
                comic_reference_set_approved_inner(conn, "comic_2", "cref_1", true).unwrap_err();
            assert!(error.contains("不属于当前漫画项目"), "实际错误: {error}");
            let approved: i64 = conn
                .query_row(
                    "SELECT approved FROM comic_references WHERE id = 'cref_1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(approved, 0, "越权批准不应改动原参考图");

            // 越权添加：owner 卡片属于 comic_1，却挂在 comic_2 下
            let input = ComicReferenceInput {
                comic_project_id: "comic_2".into(),
                owner_type: "card".into(),
                owner_id: "card_1".into(),
                asset_id: "asset_1".into(),
                role: "ref".into(),
                weight: None,
                idempotency_key: None,
            };
            let error = comic_reference_add_inner(conn, input).unwrap_err();
            assert!(
                error.contains("不存在或不属于当前漫画项目"),
                "实际错误: {error}"
            );
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM comic_references", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 1, "越权添加不应产生新参考图");

            // 归属项目内的正常添加：owner 与 asset 均有效
            let input = ComicReferenceInput {
                comic_project_id: "comic_1".into(),
                owner_type: "card".into(),
                owner_id: "card_1".into(),
                asset_id: "asset_1".into(),
                role: "ref".into(),
                weight: Some(0.5),
                idempotency_key: None,
            };
            let added = comic_reference_add_inner(conn, input).unwrap();
            assert_eq!(added.comic_project_id, "comic_1");
            assert_eq!(
                added.sort_order, 1,
                "重复业务键必须返回既有 canonical reference"
            );

            // 素材存在但属于另一个应用项目时拒绝绑定
            let input = ComicReferenceInput {
                comic_project_id: "comic_1".into(),
                owner_type: "card".into(),
                owner_id: "card_1".into(),
                asset_id: "asset_foreign".into(),
                role: "ref".into(),
                weight: None,
                idempotency_key: None,
            };
            let error = comic_reference_add_inner(conn, input).unwrap_err();
            assert!(error.contains("不属于当前漫画项目"), "实际错误: {error}");

            // 素材不存在时拒绝
            let input = ComicReferenceInput {
                comic_project_id: "comic_1".into(),
                owner_type: "card".into(),
                owner_id: "card_1".into(),
                asset_id: "asset_missing".into(),
                role: "ref".into(),
                weight: None,
                idempotency_key: None,
            };
            let error = comic_reference_add_inner(conn, input).unwrap_err();
            assert!(error.contains("素材不存在"), "实际错误: {error}");

            // 项目内批准成功
            comic_reference_set_approved_inner(conn, "comic_1", "cref_1", true).unwrap();
            let approved: i64 = conn
                .query_row(
                    "SELECT approved FROM comic_references WHERE id = 'cref_1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(approved, 1);
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_panel_attached_to_missing_or_foreign_scene() {
        let (dir, state) = open_test_db("comic-panel");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "cch_1", "comic_1", 1)?;
            seed_scene(conn, "csc_1", "cch_1", 1)?;
            seed_scene(conn, "csc_2", "cch_1", 2)?;

            // 场景不存在
            let input = ComicPanelInput {
                id: None,
                comic_project_id: "comic_1".into(),
                scene_id: "csc_missing".into(),
                page_no: 1,
                panel_no: 1,
                spec: json!({}),
                prompt: None,
                status: None,
            };
            let error = comic_panel_save_inner(conn, input).unwrap_err();
            assert!(
                error.contains("场景不存在或不属于当前漫画项目"),
                "实际错误: {error}"
            );

            // 正常创建
            let input = ComicPanelInput {
                id: None,
                comic_project_id: "comic_1".into(),
                scene_id: "csc_1".into(),
                page_no: 1,
                panel_no: 1,
                spec: json!({ "dialogue": "你好" }),
                prompt: None,
                status: None,
            };
            let panel = comic_panel_save_inner(conn, input).unwrap();
            assert_eq!(panel.scene_id, "csc_1");

            // 更新分支：id 存在但 scene_id 不匹配（同项目另一场景）→ 0 行受影响
            let input = ComicPanelInput {
                id: Some(panel.id.clone()),
                comic_project_id: "comic_1".into(),
                scene_id: "csc_2".into(),
                page_no: 2,
                panel_no: 1,
                spec: json!({}),
                prompt: None,
                status: None,
            };
            let error = comic_panel_save_inner(conn, input).unwrap_err();
            assert!(
                error.contains("漫画格不存在或不属于当前场景"),
                "实际错误: {error}"
            );

            // 跨项目更新被场景归属校验拒绝
            let input = ComicPanelInput {
                id: Some(panel.id.clone()),
                comic_project_id: "comic_2".into(),
                scene_id: "csc_1".into(),
                page_no: 9,
                panel_no: 1,
                spec: json!({}),
                prompt: None,
                status: None,
            };
            let error = comic_panel_save_inner(conn, input).unwrap_err();
            assert!(
                error.contains("场景不存在或不属于当前漫画项目"),
                "实际错误: {error}"
            );
            let page_no: i64 = conn
                .query_row(
                    "SELECT page_no FROM comic_panels WHERE id = ?",
                    params![panel.id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(page_no, 1, "越权更新不应改动原漫画格");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn seed_panel(
        conn: &Connection,
        id: &str,
        scene_id: &str,
        panel_no: i64,
    ) -> Result<(), String> {
        conn.execute(
            "INSERT INTO comic_panels (id, scene_id, page_no, panel_no, spec_json, prompt_json, status, approved_run_id, created_at, updated_at)
             VALUES (?, ?, 1, ?, '{}', NULL, 'draft', NULL, 1, 1)",
            params![id, scene_id, panel_no],
        )
        .map_err(|e| format!("seed {id} 失败: {e}"))?;
        Ok(())
    }

    #[test]
    fn runs_panel_generation_round_trip_with_qc_and_approval() {
        let (dir, state) = open_test_db("comic-panel-run");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "cch_1", "comic_1", 1)?;
            seed_scene(conn, "csc_1", "cch_1", 1)?;
            seed_panel(conn, "cpn_1", "csc_1", 1)?;
            seed_asset(conn, "asset_7", "p_1")?;
            seed_asset(conn, "asset_foreign", "p_2")?;

            // 漫画格不存在时拒绝
            let input = ComicPanelRunCreateInput {
                comic_project_id: "comic_1".into(),
                panel_id: "cpn_missing".into(),
                strategy: "draft".into(),
                request_json: json!({}),
                reference_snapshot_json: json!([]),
                parent_run_id: None,
                idempotency_key: None,
            };
            let error = comic_panel_run_create_inner(conn, input).unwrap_err();
            assert!(
                error.contains("漫画格不存在或不属于当前漫画项目"),
                "实际错误: {error}"
            );

            // 正常创建
            let input = ComicPanelRunCreateInput {
                comic_project_id: "comic_1".into(),
                panel_id: "cpn_1".into(),
                strategy: "draft".into(),
                request_json: json!({ "prompt": "第 1 格草稿" }),
                reference_snapshot_json: json!([{ "assetId": "asset_1", "role": "ref" }]),
                parent_run_id: None,
                idempotency_key: None,
            };
            let created = comic_panel_run_create_inner(conn, input).unwrap();
            assert!(created.id.starts_with("cpnrun_"));
            assert_eq!(created.panel_id, "cpn_1");
            assert_eq!(created.status, "running");
            assert!(created.asset_id.is_none());
            assert!(created.score_json.is_none());
            assert!(created.finished_at.is_none());

            // 序列化字段名与前端契约一致
            let serialized = serde_json::to_value(&created).unwrap();
            for key in [
                "panelId",
                "taskId",
                "assetId",
                "parentRunId",
                "strategy",
                "requestJson",
                "referenceSnapshotJson",
                "scoreJson",
                "status",
                "error",
                "createdAt",
                "finishedAt",
            ] {
                assert!(serialized.get(key).is_some(), "缺少序列化字段 {key}");
            }

            // 父级 run 不属于该漫画格时拒绝
            let other_panel_run = comic_panel_run_create_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_other".into(),
                    strategy: "draft".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: None,
                    idempotency_key: None,
                },
            );
            assert!(other_panel_run.is_err(), "不存在的漫画格不应创建成功");
            let input = ComicPanelRunCreateInput {
                comic_project_id: "comic_1".into(),
                panel_id: "cpn_1".into(),
                strategy: "draft".into(),
                request_json: json!({}),
                reference_snapshot_json: json!([]),
                parent_run_id: Some("cpnrun_missing".into()),
                idempotency_key: None,
            };
            let error = comic_panel_run_create_inner(conn, input).unwrap_err();
            assert!(error.contains("读取格子生成记录失败"), "实际错误: {error}");

            // 只允许 success / error
            let input = ComicPanelRunFinishInput {
                id: created.id.clone(),
                asset_id: None,
                status: "cancelled".into(),
                error: None,
                score_json: None,
            };
            let error = comic_panel_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("非法的格子生成状态"), "实际错误: {error}");

            // success 必须绑定存在的素材；失败状态则可不带素材。
            let input = ComicPanelRunFinishInput {
                id: created.id.clone(),
                asset_id: None,
                status: "success".into(),
                error: None,
                score_json: None,
            };
            let error = comic_panel_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("必须提供素材 ID"), "实际错误: {error}");
            let input = ComicPanelRunFinishInput {
                id: created.id.clone(),
                asset_id: Some("asset_missing".into()),
                status: "success".into(),
                error: None,
                score_json: None,
            };
            let error = comic_panel_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("素材不存在"), "实际错误: {error}");

            // 素材存在但属于另一个应用项目时也必须拒绝
            let input = ComicPanelRunFinishInput {
                id: created.id.clone(),
                asset_id: Some("asset_foreign".into()),
                status: "success".into(),
                error: None,
                score_json: None,
            };
            let error = comic_panel_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("不属于当前漫画项目"), "实际错误: {error}");

            // 完成成功：写入 asset_id、score_json 与 finished_at
            let input = ComicPanelRunFinishInput {
                id: created.id.clone(),
                asset_id: Some("asset_7".into()),
                status: "success".into(),
                error: None,
                score_json: Some(json!({ "overall": 92 })),
            };
            let finished = comic_panel_run_finish_inner(conn, input).unwrap();
            assert_eq!(finished.status, "success");
            assert_eq!(finished.asset_id.as_deref(), Some("asset_7"));
            assert_eq!(finished.score_json, Some(json!({ "overall": 92 })));
            assert!(finished.finished_at.is_some());

            // terminal run 不允许被第二次 finish 覆盖。
            let input = ComicPanelRunFinishInput {
                id: created.id.clone(),
                asset_id: None,
                status: "error".into(),
                error: Some("不应覆盖".into()),
                score_json: None,
            };
            let error = comic_panel_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("已结束"), "实际错误: {error}");
            assert_eq!(read_panel_run(conn, &created.id)?.status, "success");

            // 质检结论只允许 approved / rejected
            let input = ComicQcReportSaveInput {
                comic_project_id: "comic_1".into(),
                panel_run_id: created.id.clone(),
                report_json: json!({ "score": 92 }),
                decision: "maybe".into(),
            };
            let error = comic_qc_report_save_inner(conn, input).unwrap_err();
            assert!(error.contains("非法的质检结论"), "实际错误: {error}");

            // 质检通过
            let input = ComicQcReportSaveInput {
                comic_project_id: "comic_1".into(),
                panel_run_id: created.id.clone(),
                report_json: json!({ "score": 92 }),
                decision: "approved".into(),
            };
            let report = comic_qc_report_save_inner(conn, input).unwrap();
            assert_eq!(report.panel_run_id, created.id);
            assert_eq!(report.decision, "approved");
            let serialized = serde_json::to_value(&report).unwrap();
            for key in ["panelRunId", "reportJson", "decision", "createdAt"] {
                assert!(serialized.get(key).is_some(), "缺少序列化字段 {key}");
            }

            // 采纳为格子最终产物
            let input = ComicPanelSetApprovedRunInput {
                comic_project_id: "comic_1".into(),
                panel_id: "cpn_1".into(),
                run_id: created.id.clone(),
            };
            let approved_panel = comic_panel_set_approved_run_inner(conn, input).unwrap();
            assert_eq!(
                approved_panel.approved_run_id.as_deref(),
                Some(created.id.as_str())
            );

            // 列表按 created_at 倒序
            let second = comic_panel_run_create_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    strategy: "final".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: Some(created.id.clone()),
                    idempotency_key: None,
                },
            )
            .unwrap();
            assert_eq!(second.parent_run_id.as_deref(), Some(created.id.as_str()));
            conn.execute(
                "UPDATE comic_panel_runs SET created_at = 1 WHERE id = ?",
                params![created.id],
            )
            .unwrap();
            let error = comic_panel_runs_list_inner(conn, "cpn_1", Some("comic_2")).unwrap_err();
            assert!(error.contains("不属于当前漫画项目"), "实际错误: {error}");
            assert!(comic_panel_runs_list_inner(conn, "cpn_1", Some("comic_1")).is_ok());
            let runs = comic_panel_runs_list_inner(conn, "cpn_1", None).unwrap();
            assert_eq!(runs.len(), 2);
            assert_eq!(runs[0].id, second.id, "最新记录应排在最前");
            assert_eq!(runs[1].id, created.id);
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_cross_project_panel_run_operations() {
        let (dir, state) = open_test_db("comic-panel-run-cross");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "cch_1", "comic_1", 1)?;
            seed_scene(conn, "csc_1", "cch_1", 1)?;
            seed_panel(conn, "cpn_1", "csc_1", 1)?;

            // 越权创建：panel 属于 comic_1，却以 comic_2 名义发起
            let input = ComicPanelRunCreateInput {
                comic_project_id: "comic_2".into(),
                panel_id: "cpn_1".into(),
                strategy: "draft".into(),
                request_json: json!({}),
                reference_snapshot_json: json!([]),
                parent_run_id: None,
                idempotency_key: None,
            };
            let error = comic_panel_run_create_inner(conn, input).unwrap_err();
            assert!(error.contains("不属于当前漫画项目"), "实际错误: {error}");

            // 项目内创建成功后再越权质检
            let created = comic_panel_run_create_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    strategy: "draft".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: None,
                    idempotency_key: None,
                },
            )
            .unwrap();
            let input = ComicQcReportSaveInput {
                comic_project_id: "comic_2".into(),
                panel_run_id: created.id.clone(),
                report_json: json!({}),
                decision: "approved".into(),
            };
            let error = comic_qc_report_save_inner(conn, input).unwrap_err();
            assert!(error.contains("不属于当前漫画项目"), "实际错误: {error}");
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM comic_qc_reports", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "越权质检不应产生报告");

            // 越权采纳不应改动原漫画格
            let input = ComicPanelSetApprovedRunInput {
                comic_project_id: "comic_2".into(),
                panel_id: "cpn_1".into(),
                run_id: created.id.clone(),
            };
            let error = comic_panel_set_approved_run_inner(conn, input).unwrap_err();
            assert!(error.contains("不属于当前漫画项目"), "实际错误: {error}");
            let approved_run_id: Option<String> = conn
                .query_row(
                    "SELECT approved_run_id FROM comic_panels WHERE id = 'cpn_1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                approved_run_id.is_none(),
                "越权采纳不应写入 approved_run_id"
            );
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_approving_failed_or_foreign_panel_run() {
        let (dir, state) = open_test_db("comic-panel-run-approve");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "cch_1", "comic_1", 1)?;
            seed_scene(conn, "csc_1", "cch_1", 1)?;
            seed_panel(conn, "cpn_1", "csc_1", 1)?;
            seed_panel(conn, "cpn_2", "csc_1", 2)?;
            seed_asset(conn, "asset_8", "p_1")?;
            seed_asset(conn, "asset_foreign", "p_2")?;

            // error 状态的 run 不允许采纳
            let failed = comic_panel_run_create_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    strategy: "draft".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: None,
                    idempotency_key: None,
                },
            )
            .unwrap();
            comic_panel_run_finish_inner(
                conn,
                ComicPanelRunFinishInput {
                    id: failed.id.clone(),
                    asset_id: None,
                    status: "error".into(),
                    error: Some("生成失败".into()),
                    score_json: None,
                },
            )
            .unwrap();
            let input = ComicPanelSetApprovedRunInput {
                comic_project_id: "comic_1".into(),
                panel_id: "cpn_1".into(),
                run_id: failed.id.clone(),
            };
            let error = comic_panel_set_approved_run_inner(conn, input).unwrap_err();
            assert!(error.contains("只允许采纳成功状态的生成记录"), "实际错误: {error}");
            let approved_run_id: Option<String> = conn
                .query_row("SELECT approved_run_id FROM comic_panels WHERE id = 'cpn_1'", [], |row| row.get(0))
                .unwrap();
            assert!(approved_run_id.is_none(), "失败 run 不应被采纳");

            // 历史脏数据：success run 没有素材时不允许采纳
            let empty_asset_run = comic_panel_run_create_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    strategy: "legacy".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: None,
                    idempotency_key: None,
                },
            )
            .unwrap();
            conn.execute(
                "UPDATE comic_panel_runs SET status = 'success', finished_at = 1 WHERE id = ?",
                params![empty_asset_run.id],
            )
            .map_err(|e| format!("更新历史空素材 run 失败: {e}"))?;
            let error = comic_panel_set_approved_run_inner(
                conn,
                ComicPanelSetApprovedRunInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    run_id: empty_asset_run.id,
                },
            )
            .unwrap_err();
            assert!(error.contains("有效项目素材"), "实际错误: {error}");

            // 历史脏数据：success run 绑定其他项目素材时不允许采纳
            let foreign_asset_run = comic_panel_run_create_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    strategy: "legacy".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: None,
                    idempotency_key: None,
                },
            )
            .unwrap();
            conn.execute(
                "UPDATE comic_panel_runs SET asset_id = 'asset_foreign', status = 'success', finished_at = 1 WHERE id = ?",
                params![foreign_asset_run.id],
            )
            .map_err(|e| format!("更新历史跨项目素材 run 失败: {e}"))?;
            let error = comic_panel_set_approved_run_inner(
                conn,
                ComicPanelSetApprovedRunInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    run_id: foreign_asset_run.id,
                },
            )
            .unwrap_err();
            assert!(error.contains("不属于当前漫画项目"), "实际错误: {error}");

            // 同项目其他漫画格的 run 不允许挂到当前漫画格
            let foreign = comic_panel_run_create_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_2".into(),
                    strategy: "draft".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: None,
                    idempotency_key: None,
                },
            )
            .unwrap();
            comic_panel_run_finish_inner(
                conn,
                ComicPanelRunFinishInput {
                    id: foreign.id.clone(),
                    asset_id: Some("asset_8".into()),
                    status: "success".into(),
                    error: None,
                    score_json: None,
                },
            )
            .unwrap();
            let input = ComicPanelSetApprovedRunInput {
                comic_project_id: "comic_1".into(),
                panel_id: "cpn_1".into(),
                run_id: foreign.id.clone(),
            };
            let error = comic_panel_set_approved_run_inner(conn, input).unwrap_err();
            assert!(error.contains("不属于该漫画格"), "实际错误: {error}");

            // 记录不存在时 finish 拒绝
            let input = ComicPanelRunFinishInput {
                id: "cpnrun_missing".into(),
                asset_id: None,
                status: "success".into(),
                error: None,
                score_json: None,
            };
            let error = comic_panel_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("格子生成记录不存在"), "实际错误: {error}");

            // 项目内正常采纳
            let input = ComicPanelSetApprovedRunInput {
                comic_project_id: "comic_1".into(),
                panel_id: "cpn_2".into(),
                run_id: foreign.id.clone(),
            };
            let approved_panel = comic_panel_set_approved_run_inner(conn, input).unwrap();
            assert_eq!(approved_panel.approved_run_id.as_deref(), Some(foreign.id.as_str()));
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn creates_and_finishes_page_run_round_trip() {
        let (dir, state) = open_test_db("comic-page-run");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_asset(conn, "asset_9", "p_1")?;
            seed_asset(conn, "asset_foreign", "p_2")?;

            // 项目不存在时拒绝
            let input = ComicPageRunCreateInput {
                comic_project_id: "comic_missing".into(),
                page_no: 1,
                request_json: json!({}),
                reference_snapshot_json: json!([]),
                idempotency_key: None,
            };
            let error = comic_page_run_create_inner(conn, input).unwrap_err();
            assert!(error.contains("漫画项目不存在"), "实际错误: {error}");

            let input = ComicPageRunCreateInput {
                comic_project_id: "comic_1".into(),
                page_no: 3,
                request_json: json!({ "prompt": "第 3 页整页草稿" }),
                reference_snapshot_json: json!([{ "assetId": "asset_1", "role": "ref" }]),
                idempotency_key: None,
            };
            let created = comic_page_run_create_inner(conn, input).unwrap();
            assert!(created.id.starts_with("cprun_"));
            assert_eq!(created.comic_project_id, "comic_1");
            assert_eq!(created.page_no, 3);
            assert_eq!(created.status, "running");
            assert!(created.asset_id.is_none());
            assert!(created.finished_at.is_none());

            // 序列化字段名与前端契约一致
            let serialized = serde_json::to_value(&created).unwrap();
            for key in [
                "comicProjectId",
                "pageNo",
                "assetId",
                "requestJson",
                "referenceSnapshotJson",
                "status",
                "error",
                "createdAt",
                "finishedAt",
            ] {
                assert!(serialized.get(key).is_some(), "缺少序列化字段 {key}");
            }

            // 只允许 success / error
            let input = ComicPageRunFinishInput {
                id: created.id.clone(),
                asset_id: None,
                status: "cancelled".into(),
                error: None,
            };
            let error = comic_page_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("非法的整页生成状态"), "实际错误: {error}");

            // success 必须绑定存在的素材；失败状态仍允许没有素材。
            let input = ComicPageRunFinishInput {
                id: created.id.clone(),
                asset_id: None,
                status: "success".into(),
                error: None,
            };
            let error = comic_page_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("必须提供素材 ID"), "实际错误: {error}");
            let input = ComicPageRunFinishInput {
                id: created.id.clone(),
                asset_id: Some("asset_missing".into()),
                status: "success".into(),
                error: None,
            };
            let error = comic_page_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("素材不存在"), "实际错误: {error}");

            let input = ComicPageRunFinishInput {
                id: created.id.clone(),
                asset_id: Some("asset_foreign".into()),
                status: "success".into(),
                error: None,
            };
            let error = comic_page_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("不属于当前漫画项目"), "实际错误: {error}");

            // 完成成功：写入 asset_id 与 finished_at
            let input = ComicPageRunFinishInput {
                id: created.id.clone(),
                asset_id: Some("asset_9".into()),
                status: "success".into(),
                error: None,
            };
            let finished = comic_page_run_finish_inner(conn, input).unwrap();
            assert_eq!(finished.status, "success");
            assert_eq!(finished.asset_id.as_deref(), Some("asset_9"));
            assert!(finished.finished_at.is_some());
            assert_eq!(
                finished.request_json,
                json!({ "prompt": "第 3 页整页草稿" })
            );

            let input = ComicPageRunFinishInput {
                id: created.id.clone(),
                asset_id: None,
                status: "error".into(),
                error: Some("不应覆盖".into()),
            };
            let error = comic_page_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("已结束"), "实际错误: {error}");
            assert_eq!(read_page_run(conn, &created.id)?.status, "success");

            // 失败路径：写入 error 与 finished_at
            let created2 = comic_page_run_create_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 4,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: None,
                },
            )
            .unwrap();
            let input = ComicPageRunFinishInput {
                id: created2.id.clone(),
                asset_id: None,
                status: "error".into(),
                error: Some("生成失败".into()),
            };
            let finished = comic_page_run_finish_inner(conn, input).unwrap();
            assert_eq!(finished.status, "error");
            assert_eq!(finished.error.as_deref(), Some("生成失败"));
            assert!(finished.finished_at.is_some());

            // 记录不存在时拒绝
            let input = ComicPageRunFinishInput {
                id: "cprun_missing".into(),
                asset_id: None,
                status: "success".into(),
                error: None,
            };
            let error = comic_page_run_finish_inner(conn, input).unwrap_err();
            assert!(error.contains("整页生成记录不存在"), "实际错误: {error}");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn operation_receipt_replays_same_payload_and_rejects_changed_payload() {
        let (dir, state) = open_test_db("comic-operation-receipt");
        db::with_connection(&state, |conn| {
            let first: Value = with_receipt(
                conn,
                "receipt_test",
                "idem-1",
                &json!({ "run": "one" }),
                |_| Ok(json!({ "value": 1 })),
            )?;
            assert_eq!(first, json!({ "value": 1 }));
            let stored_hash: String = conn
                .query_row(
                    "SELECT request_hash FROM comic_operation_receipts WHERE command_name='receipt_test' AND idempotency_key='idem-1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert!(
                stored_hash.starts_with("sha256:")
                    && stored_hash.len() == "sha256:".len() + 64
                    && stored_hash["sha256:".len()..]
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit()),
                "new receipts must store a versioned SHA-256 fingerprint"
            );
            let replay: Value = with_receipt(
                conn,
                "receipt_test",
                "idem-1",
                &json!({ "run": "one" }),
                |_| Ok(json!({ "value": 2 })),
            )?;
            assert_eq!(replay, json!({ "value": 1 }), "必须回放首个响应");
            let error = with_receipt::<Value>(
                conn,
                "receipt_test",
                "idem-1",
                &json!({ "run": "different" }),
                |_| Ok(json!({})),
            )
            .unwrap_err();
            assert!(error.contains("不同业务载荷"), "实际错误: {error}");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_md5_receipt_replays_same_request_and_rejects_different_request() {
        let (dir, state) = open_test_db("legacy-md5-receipt");
        db::with_connection(&state, |conn| {
            let request = json!({"legacy": true});
            conn.execute(
                "INSERT INTO comic_operation_receipts (id, command_name, idempotency_key, request_hash, response_json, created_at)
                 VALUES (?, 'legacy_test', 'legacy-key', ?, ?, 1)",
                params![
                    new_id("coreceipt"),
                    legacy_request_hash(&request),
                    json!({"replayed": true}).to_string()
                ],
            )
            .map_err(|e| e.to_string())?;
            let replay: Value = with_receipt(
                conn,
                "legacy_test",
                "legacy-key",
                &request,
                |_| Ok(json!({"mustNotRun": true})),
            )?;
            assert_eq!(replay, json!({"replayed": true}));
            let changed = with_receipt::<Value>(
                conn,
                "legacy_test",
                "legacy-key",
                &json!({"legacy": false}),
                |_| Ok(json!({})),
            )
            .unwrap_err();
            assert!(changed.contains("不同业务载荷"));
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_receipt_hash_is_rejected_without_running_action() {
        let (dir, state) = open_test_db("malformed-receipt-hash");
        db::with_connection(&state, |conn| {
            conn.execute(
                "INSERT INTO comic_operation_receipts (id, command_name, idempotency_key, request_hash, response_json, created_at)
                 VALUES (?, 'malformed_test', 'malformed-key', 'sha256:not-a-digest', '{}', 1)",
                params![new_id("coreceipt")],
            )
            .map_err(|e| e.to_string())?;
            let error = with_receipt::<Value>(
                conn,
                "malformed_test",
                "malformed-key",
                &json!({"request": "ignored"}),
                |_| Ok(json!({"mustNotRun": true})),
            )
            .unwrap_err();
            assert_eq!(error, "操作回执指纹格式无效");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_page_create_then_submitted_uses_owner_session_and_writes_event() {
        let (dir, state) = open_test_db("v5-submit");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let run = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("create".into()),
                },
                state.app_session_id(),
            )?;
            let attempt = run.generation_attempt_id.clone().unwrap();
            let result = comic_run_mark_submitted_inner(
                conn,
                ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt.clone(),
                    expected_status: "queued".into(),
                    idempotency_key: "submit".into(),
                },
                state.app_session_id(),
            )?;
            assert_eq!(result["status"], "running");
            let error = comic_run_mark_submitted_inner(
                conn,
                ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt,
                    expected_status: "queued".into(),
                    idempotency_key: "wrong-owner".into(),
                },
                "other",
            )
            .unwrap_err();
            assert!(error.contains("期望的") || error.contains("不拥有"));
            let events: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM comic_run_events WHERE run_id=?",
                    params![run.id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(events, 2);
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_finish_success_is_idempotent_and_rejects_bad_provenance() {
        let (dir, state) = open_test_db("v5-finish");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let run = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("c".into()),
                },
                state.app_session_id(),
            )?;
            let attempt = run.generation_attempt_id.clone().unwrap();
            comic_run_mark_submitted_inner(
                conn,
                ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt.clone(),
                    expected_status: "queued".into(),
                    idempotency_key: "s".into(),
                },
                state.app_session_id(),
            )?;
            seed_asset(conn, "asset", "p_1")?;
            conn.execute(
                "UPDATE assets SET metadata=? WHERE id='asset'",
                params![
                    json!({"projectId":"p_1","comicRunId":run.id,"generationAttemptId":attempt})
                        .to_string()
                ],
            )
            .map_err(|e| e.to_string())?;
            let input = ComicRunFinishV5Input {
                run: ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt.clone(),
                    expected_status: "running".into(),
                    idempotency_key: "f".into(),
                },
                outcome: json!({"status":"success","assetId":"asset"}),
            };
            let ok = comic_run_finish_inner(conn, input.clone(), state.app_session_id())?;
            assert_eq!(ok["status"], "success");
            assert_eq!(
                comic_run_finish_inner(conn, input, state.app_session_id())?["status"],
                "success"
            );
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_recovery_only_marks_expired_leases_stale() {
        let (dir, state) = open_test_db("v5-recovery");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let fresh = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("fresh".into()),
                },
                state.app_session_id(),
            )?;
            let expired = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 2,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("expired".into()),
                },
                state.app_session_id(),
            )?;
            conn.execute(
                "UPDATE comic_page_runs SET lease_expires_at=? WHERE id=?",
                params![now() + 60_000, fresh.id],
            )
            .map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE comic_page_runs SET lease_expires_at=? WHERE id=?",
                params![now() - 1, expired.id],
            )
            .map_err(|e| e.to_string())?;
            assert_eq!(
                comic_runs_recover_stale_inner(conn, state.app_session_id())?,
                1
            );
            assert_eq!(read_page_run(conn, &fresh.id)?.status, "queued");
            assert_eq!(read_page_run(conn, &expired.id)?.status, "stale");
            let events: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM comic_run_events WHERE run_id=? AND event_type='stale'",
                    params![expired.id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(events, 1);
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_page_lifecycle_rejects_wrong_provenance_recovers_stale_and_retries_immutably() {
        let (dir, state) = open_test_db("comic-v5-page-lifecycle");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let run = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({"frozen":true}),
                    reference_snapshot_json: json!(["ref"]),
                    idempotency_key: Some("create".into()),
                },
                state.app_session_id(),
            )?;
            let attempt = run.generation_attempt_id.clone().unwrap();
            comic_run_mark_submitted_inner(
                conn,
                ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt.clone(),
                    expected_status: "queued".into(),
                    idempotency_key: "submit".into(),
                },
                state.app_session_id(),
            )?;
            assert!(
                comic_run_finish_inner(
                    conn,
                    ComicRunFinishV5Input {
                        run: ComicRunMutationInput {
                            kind: "page".into(),
                            run_id: run.id.clone(),
                            comic_project_id: "comic_1".into(),
                            generation_attempt_id: attempt.clone(),
                            expected_status: "stale".into(),
                            idempotency_key: "finish-stale".into()
                        },
                        outcome: json!({"status":"error","error":"no"})
                    },
                    state.app_session_id(),
                )
                .is_err(),
                "stale 不能普通 finish"
            );
            conn.execute(
                "UPDATE comic_page_runs SET status='stale', lease_expires_at=NULL WHERE id=?",
                params![run.id],
            )
            .map_err(|e| e.to_string())?;
            seed_asset(conn, "bad", "p_1")?;
            assert!(comic_run_reconcile_inner(
                conn,
                ComicRunReconcileInput {
                    run: ComicRunMutationInput {
                        kind: "page".into(),
                        run_id: run.id.clone(),
                        comic_project_id: "comic_1".into(),
                        generation_attempt_id: attempt.clone(),
                        expected_status: "stale".into(),
                        idempotency_key: "bad-reconcile".into()
                    },
                    observation: json!({"assetId":"bad"})
                }
            )
            .is_err());
            seed_asset(conn, "good", "p_1")?;
            conn.execute(
                "UPDATE assets SET metadata=? WHERE id='good'",
                params![
                    json!({"projectId":"p_1","comicRunId":run.id,"generationAttemptId":attempt})
                        .to_string()
                ],
            )
            .map_err(|e| e.to_string())?;
            assert_eq!(
                comic_run_reconcile_inner(
                    conn,
                    ComicRunReconcileInput {
                        run: ComicRunMutationInput {
                            kind: "page".into(),
                            run_id: run.id.clone(),
                            comic_project_id: "comic_1".into(),
                            generation_attempt_id: attempt.clone(),
                            expected_status: "stale".into(),
                            idempotency_key: "good-reconcile".into()
                        },
                        observation: json!({"assetId":"good"})
                    }
                )?["status"],
                "success"
            );
            conn.execute(
                "UPDATE comic_page_runs SET status='stale', asset_id=NULL WHERE id=?",
                params![run.id],
            )
            .map_err(|e| e.to_string())?;
            assert_eq!(
                comic_run_abandon_stale_inner(
                    conn,
                    ComicRunAbandonInput {
                        run: ComicRunMutationInput {
                            kind: "page".into(),
                            run_id: run.id.clone(),
                            comic_project_id: "comic_1".into(),
                            generation_attempt_id: attempt.clone(),
                            expected_status: "stale".into(),
                            idempotency_key: "abandon".into()
                        },
                        reason: "确认放弃".into()
                    }
                )?["status"],
                "error"
            );
            let child = comic_run_retry_inner(
                conn,
                ComicRunRetryInput {
                    run: ComicRunMutationInput {
                        kind: "page".into(),
                        run_id: run.id.clone(),
                        comic_project_id: "comic_1".into(),
                        generation_attempt_id: attempt,
                        expected_status: "error".into(),
                        idempotency_key: "retry".into(),
                    },
                    retry_reason: "retry".into(),
                },
                state.app_session_id(),
            )?;
            assert_eq!(child["status"], "queued");
            assert_eq!(child["parentRunId"], run.id);
            assert_eq!(child["attemptNo"], 2);
            assert_eq!(child["requestJson"], json!({"frozen":true}));
            assert_eq!(child["referenceSnapshotJson"], json!(["ref"]));
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn panel_append_is_idempotent_scoped_and_assigns_next_number() {
        let (dir, state) = open_test_db("comic-panel-append");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "chapter", "comic_1", 1)?;
            seed_scene(conn, "scene", "chapter", 1)?;
            let first = comic_panel_append_inner(
                conn,
                ComicPanelAppendInput {
                    comic_project_id: "comic_1".into(),
                    scene_id: "scene".into(),
                    page_no: 1,
                    spec: json!({"action":"one"}),
                    idempotency_key: "append-one".into(),
                },
            )?;
            let replay = comic_panel_append_inner(
                conn,
                ComicPanelAppendInput {
                    comic_project_id: "comic_1".into(),
                    scene_id: "scene".into(),
                    page_no: 1,
                    spec: json!({"action":"one"}),
                    idempotency_key: "append-one".into(),
                },
            )?;
            let second = comic_panel_append_inner(
                conn,
                ComicPanelAppendInput {
                    comic_project_id: "comic_1".into(),
                    scene_id: "scene".into(),
                    page_no: 1,
                    spec: json!({"action":"two"}),
                    idempotency_key: "append-two".into(),
                },
            )?;
            assert_eq!(first.id, replay.id);
            assert_eq!(first.panel_no, 1);
            assert_eq!(second.panel_no, 2);
            assert!(comic_panel_append_inner(
                conn,
                ComicPanelAppendInput {
                    comic_project_id: "comic_2".into(),
                    scene_id: "scene".into(),
                    page_no: 1,
                    spec: json!({}),
                    idempotency_key: "foreign".into()
                }
            )
            .is_err());
            assert!(comic_panel_append_inner(
                conn,
                ComicPanelAppendInput {
                    comic_project_id: "comic_1".into(),
                    scene_id: "scene".into(),
                    page_no: 0,
                    spec: json!({}),
                    idempotency_key: "zero".into()
                }
            )
            .is_err());
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_qc_approval_and_rejection_preserve_project_boundaries() {
        let (dir, state) = open_test_db("comic-atomic-qc");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "chapter", "comic_1", 1)?;
            seed_scene(conn, "scene", "chapter", 1)?;
            let panel = comic_panel_save_inner(
                conn,
                ComicPanelInput {
                    id: None,
                    comic_project_id: "comic_1".into(),
                    scene_id: "scene".into(),
                    page_no: 1,
                    panel_no: 1,
                    spec: json!({}),
                    prompt: None,
                    status: None,
                },
            )?;
            let run = comic_panel_run_create_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: panel.id.clone(),
                    strategy: "test".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!({}),
                    parent_run_id: None,
                    idempotency_key: None,
                },
            )?;
            seed_asset(conn, "good", "p_1")?;
            conn.execute(
                "UPDATE comic_panel_runs SET status='success', asset_id='good' WHERE id=?",
                params![run.id],
            )
            .map_err(|e| e.to_string())?;
            let approved = comic_panel_qc_and_approve_inner(
                conn,
                ComicPanelQcAndApproveInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: panel.id.clone(),
                    run_id: run.id.clone(),
                    report_json: json!({"ok":true}),
                    decision: "approved".into(),
                    idempotency_key: "approve".into(),
                },
            )?;
            assert_eq!(
                approved.panel.approved_run_id.as_deref(),
                Some(run.id.as_str())
            );
            let reports: i64 = conn
                .query_row("SELECT COUNT(*) FROM comic_qc_reports", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            seed_asset(conn, "foreign", "p_2")?;
            conn.execute(
                "UPDATE comic_panel_runs SET asset_id='foreign' WHERE id=?",
                params![run.id],
            )
            .map_err(|e| e.to_string())?;
            assert!(comic_panel_qc_and_approve_inner(
                conn,
                ComicPanelQcAndApproveInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: panel.id.clone(),
                    run_id: run.id.clone(),
                    report_json: json!({}),
                    decision: "approved".into(),
                    idempotency_key: "foreign-approve".into()
                }
            )
            .is_err());
            assert_eq!(
                conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM comic_qc_reports", [], |r| r
                    .get(0))
                    .map_err(|e| e.to_string())?,
                reports
            );
            conn.execute(
                "UPDATE comic_panel_runs SET asset_id='good' WHERE id=?",
                params![run.id],
            )
            .map_err(|e| e.to_string())?;
            let rejected = comic_panel_qc_and_approve_inner(
                conn,
                ComicPanelQcAndApproveInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: panel.id.clone(),
                    run_id: run.id.clone(),
                    report_json: json!({}),
                    decision: "rejected".into(),
                    idempotency_key: "reject".into(),
                },
            )?;
            assert_eq!(
                rejected.panel.approved_run_id.as_deref(),
                Some(run.id.as_str())
            );
            assert_eq!(rejected.panel.status, "needs_repair");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_chapter_tree_rolls_back_and_locked_cards_version_immutably() {
        let (dir, state) = open_test_db("comic-atomic-tree-card");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let invalid = ComicChapterScenePanelCreateInput {
                comic_project_id: "comic_1".into(),
                chapter: json!({"chapterNo":1,"source":{}}),
                scene: json!({"sceneNo":1,"spec":{}}),
                first_panel: json!({"pageNo":0,"panelNo":1,"spec":{}}),
                idempotency_key: "invalid-tree".into(),
            };
            assert!(comic_chapter_scene_panel_create_inner(conn, invalid).is_err());
            assert_eq!(
                conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM comic_chapters", [], |r| r
                    .get(0))
                    .map_err(|e| e.to_string())?,
                0
            );
            let tree = ComicChapterScenePanelCreateInput {
                comic_project_id: "comic_1".into(),
                chapter: json!({"chapterNo":1,"source":{}}),
                scene: json!({"sceneNo":1,"spec":{}}),
                first_panel: json!({"pageNo":1,"panelNo":1,"spec":{}}),
                idempotency_key: "tree".into(),
            };
            comic_chapter_scene_panel_create_inner(conn, tree.clone())?;
            let duplicate = ComicChapterScenePanelCreateInput {
                idempotency_key: "duplicate-tree".into(),
                ..tree
            };
            assert!(comic_chapter_scene_panel_create_inner(conn, duplicate).is_err());
            assert_eq!(
                conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM comic_scenes", [], |r| r.get(0))
                    .map_err(|e| e.to_string())?,
                1
            );
            let original = comic_card_save_inner(
                conn,
                ComicCardInput {
                    id: None,
                    comic_project_id: "comic_1".into(),
                    card_type: "character_identity".into(),
                    entity_key: "hero".into(),
                    name: "old".into(),
                    data: json!({"v":1}),
                    locked: Some(true),
                    new_version: None,
                },
            )?;
            assert!(comic_card_save_inner(
                conn,
                ComicCardInput {
                    id: Some(original.id.clone()),
                    comic_project_id: "comic_1".into(),
                    card_type: "character_identity".into(),
                    entity_key: "hero".into(),
                    name: "bad".into(),
                    data: json!({}),
                    locked: Some(false),
                    new_version: None
                }
            )
            .is_err());
            let versioned = comic_card_save_inner(
                conn,
                ComicCardInput {
                    id: Some(original.id.clone()),
                    comic_project_id: "comic_1".into(),
                    card_type: "character_identity".into(),
                    entity_key: "hero".into(),
                    name: "new".into(),
                    data: json!({"v":2}),
                    locked: Some(false),
                    new_version: Some(true),
                },
            )?;
            assert_ne!(versioned.id, original.id);
            assert_eq!(versioned.version, 2);
            let original_name: String = conn
                .query_row(
                    "SELECT name FROM comic_cards WHERE id=?",
                    params![original.id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(original_name, "old");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn comic_production_contract_requires_schema_version_one() {
        let limits = json!({
            "maxPageTextCodePoints": 1,
            "maxTextItemCodePoints": 1,
            "maxTextItems": 1
        });
        assert!(validate_comic_production_contract(&json!({"comicPageText": limits})).is_err());
        assert!(validate_comic_production_contract(&json!({
            "schemaVersion": 2,
            "comicPageText": {
                "maxPageTextCodePoints": 1,
                "maxTextItemCodePoints": 1,
                "maxTextItems": 1
            }
        }))
        .is_err());
        assert!(validate_comic_production_contract(&json!({
            "schemaVersion": 1,
            "comicPageText": {
                "maxPageTextCodePoints": 1,
                "maxTextItemCodePoints": 1,
                "maxTextItems": 1
            }
        }))
        .is_ok());
    }

    #[test]
    fn v5_finish_error_sanitizes_failure_event_and_receipt() {
        let (dir, state) = open_test_db("v5-finish-sanitize");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let run = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("create-sanitize".into()),
                },
                state.app_session_id(),
            )?;
            let attempt = run.generation_attempt_id.clone().unwrap();
            comic_run_mark_submitted_inner(
                conn,
                ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt.clone(),
                    expected_status: "queued".into(),
                    idempotency_key: "submit-sanitize".into(),
                },
                state.app_session_id(),
            )?;
            let finished = comic_run_finish_inner(
                conn,
                ComicRunFinishV5Input {
                    run: ComicRunMutationInput {
                        kind: "page".into(),
                        run_id: run.id.clone(),
                        comic_project_id: "comic_1".into(),
                        generation_attempt_id: attempt,
                        expected_status: "running".into(),
                        idempotency_key: "finish-sanitize".into(),
                    },
                    outcome: json!({
                        "status": "error",
                        "error": "Bearer TOPSECRET sk-secret api_key=TOPSECRET",
                        "authorization": "TOPSECRET",
                        "api_key": "TOPSECRET",
                        "failureJson": {
                            "phase": "TOPSECRET",
                            "code": "TOPSECRET",
                            "message": "Authorization failed: Bearer TOPSECRET sk-secret api_key=TOPSECRET",
                            "retryable": false,
                            "providerRequestId": "api_key=TOPSECRET",
                            "providerStatus": "Bearer TOPSECRET",
                            "extra": "must not persist"
                        }
                    }),
                },
                state.app_session_id(),
            )?;
            assert_eq!(finished["status"], "error");
            let failure: String = conn
                .query_row(
                    "SELECT failure_json FROM comic_page_runs WHERE id=?",
                    params![run.id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let error: String = conn
                .query_row(
                    "SELECT error FROM comic_page_runs WHERE id=?",
                    params![run.id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let event: String = conn
                .query_row(
                    "SELECT payload_json FROM comic_run_events WHERE run_id=? AND event_type='finished'",
                    params![run.id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let receipt: String = conn
                .query_row(
                    "SELECT response_json FROM comic_operation_receipts WHERE command_name='comic_run_finish' AND idempotency_key='finish-sanitize'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let persisted = format!("{failure}\n{error}\n{event}\n{receipt}").to_ascii_lowercase();
            for forbidden in ["topsecret", "secret", "authorization"] {
                assert!(
                    !persisted.contains(forbidden),
                    "持久化结果包含敏感内容 {forbidden}: {persisted}"
                );
            }
            let failure_json: Value = serde_json::from_str(&failure).map_err(|e| e.to_string())?;
            assert!(failure_json.get("extra").is_none());
            assert_eq!(failure_json["phase"], "finish");
            assert_eq!(failure_json["code"], "GENERATION_FAILED");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_history_validates_statuses_cursor_and_empty_filter_means_all() {
        let (dir, state) = open_test_db("v5-history-validation");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            for (page_no, key) in [(1, "history-one"), (2, "history-two")] {
                comic_page_run_create_v5_inner(
                    conn,
                    ComicPageRunCreateInput {
                        comic_project_id: "comic_1".into(),
                        page_no,
                        request_json: json!({}),
                        reference_snapshot_json: json!([]),
                        idempotency_key: Some(key.into()),
                    },
                    state.app_session_id(),
                )?;
            }
            let all = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    comic_project_id: "comic_1".into(),
                    page_no: None,
                    panel_id: None,
                    statuses: Some(vec![]),
                    cursor: None,
                    limit: None,
                },
            )?;
            assert_eq!(all.items.len(), 2, "空 statuses 应等价于不筛选");
            let error_id = all.items[0]["id"].as_str().unwrap().to_owned();
            let success_id = all.items[1]["id"].as_str().unwrap().to_owned();
            let error_created_at = all.items[0]["createdAt"].as_i64().unwrap();
            conn.execute(
                "UPDATE comic_page_runs SET status = 'error' WHERE id = ?",
                params![error_id],
            )
            .map_err(|error| error.to_string())?;
            conn.execute(
                "UPDATE comic_page_runs SET status = 'success' WHERE id = ?",
                params![success_id],
            )
            .map_err(|error| error.to_string())?;
            let cursor_outside_status_filter = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    comic_project_id: "comic_1".into(),
                    page_no: None,
                    panel_id: None,
                    statuses: Some(vec!["success".into()]),
                    cursor: Some(format!("{error_created_at}|{error_id}")),
                    limit: None,
                },
            );
            assert!(
                matches!(cursor_outside_status_filter, Err(error) if error.contains("cursor 无效"))
            );
            let invalid_status = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    comic_project_id: "comic_1".into(),
                    page_no: None,
                    panel_id: None,
                    statuses: Some(vec!["invented".into()]),
                    cursor: None,
                    limit: None,
                },
            );
            assert!(matches!(invalid_status, Err(error) if error.contains("不支持")));
            let invalid_cursor = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    comic_project_id: "comic_1".into(),
                    page_no: None,
                    panel_id: None,
                    statuses: None,
                    cursor: Some("broken-cursor".into()),
                    limit: None,
                },
            );
            assert!(matches!(invalid_cursor, Err(error) if error.contains("cursor 无效")));
            let forged_cursor = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    comic_project_id: "comic_1".into(),
                    page_no: None,
                    panel_id: None,
                    statuses: None,
                    cursor: Some("0|forged".into()),
                    limit: None,
                },
            );
            assert!(matches!(forged_cursor, Err(error) if error.contains("cursor 无效")));
            let invalid_page = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    comic_project_id: "comic_1".into(),
                    page_no: Some(0),
                    panel_id: None,
                    statuses: None,
                    cursor: None,
                    limit: None,
                },
            );
            assert!(matches!(invalid_page, Err(error) if error.contains("pageNo")));
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn history_page_reads_only_limit_plus_one_full_run_records() {
        let (dir, state) = open_test_db("v5-history-limit-plus-one");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            for (id, created_at, attempt_no) in [
                ("history-newest", 3_i64, "1"),
                ("history-next", 2_i64, "1"),
                ("history-tail-invalid", 1_i64, "not-an-integer"),
            ] {
                conn.execute(
                    "INSERT INTO comic_page_runs (id, comic_project_id, page_no, asset_id, request_json, reference_snapshot_json, status, error, created_at, finished_at, parent_run_id, attempt_no, generation_attempt_id, owner_app_session_id, failure_json, submitted_at, heartbeat_at, lease_expires_at)
                     VALUES (?, 'comic_1', 1, NULL, '{}', '[]', 'success', NULL, ?, NULL, NULL, ?, NULL, NULL, NULL, NULL, NULL, NULL)",
                    params![id, created_at, attempt_no],
                )
                .map_err(|error| error.to_string())?;
            }
            let page = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    comic_project_id: "comic_1".into(),
                    page_no: Some(1),
                    panel_id: None,
                    statuses: None,
                    cursor: None,
                    limit: Some(1),
                },
            )?;
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0]["id"], "history-newest");
            assert!(page.next_cursor.is_some());
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scoped_history_cursor_survives_status_changes_and_rejects_other_scopes() {
        let (dir, state) = open_test_db("v5-history-scoped-cursor");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            for (id, created_at, status) in [
                ("cursor-newest", 3_i64, "success"),
                ("cursor-next", 2_i64, "success"),
                ("cursor-oldest", 1_i64, "error"),
            ] {
                conn.execute(
                    "INSERT INTO comic_page_runs (id, comic_project_id, page_no, asset_id, request_json, reference_snapshot_json, status, error, created_at, finished_at, parent_run_id, attempt_no, generation_attempt_id, owner_app_session_id, failure_json, submitted_at, heartbeat_at, lease_expires_at)
                     VALUES (?, 'comic_1', 1, NULL, '{}', '[]', ?, NULL, ?, NULL, NULL, 1, NULL, NULL, NULL, NULL, NULL, NULL)",
                    params![id, status, created_at],
                )
                .map_err(|error| error.to_string())?;
            }
            let success_input = ComicRunListInput {
                comic_project_id: "comic_1".into(),
                page_no: Some(1),
                panel_id: None,
                statuses: Some(vec!["success".into()]),
                cursor: None,
                limit: Some(1),
            };
            let first = history_page(conn, "page", &success_input)?;
            assert_eq!(first.items[0]["id"], "cursor-newest");
            let cursor = first.next_cursor.clone().unwrap();
            assert_eq!(cursor.split('|').count(), 3);

            conn.execute(
                "UPDATE comic_page_runs SET status = 'error' WHERE id = 'cursor-newest'",
                [],
            )
            .map_err(|error| error.to_string())?;
            let after_transition = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    cursor: Some(cursor.clone()),
                    ..success_input.clone()
                },
            )?;
            assert_eq!(after_transition.items[0]["id"], "cursor-next");

            let cross_scope = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    page_no: Some(2),
                    cursor: Some(cursor.clone()),
                    ..success_input.clone()
                },
            );
            assert!(matches!(cross_scope, Err(error) if error.contains("cursor 无效")));

            let mixed = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    statuses: Some(vec!["success".into(), "error".into(), "success".into()]),
                    cursor: None,
                    ..success_input.clone()
                },
            )?;
            let equivalent_status_order = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    statuses: Some(vec!["error".into(), "success".into()]),
                    cursor: mixed.next_cursor,
                    ..success_input.clone()
                },
            );
            assert!(equivalent_status_order.is_ok());

            let legacy_unfiltered = history_page(
                conn,
                "page",
                &ComicRunListInput {
                    statuses: None,
                    cursor: Some("3|cursor-newest".into()),
                    ..success_input
                },
            )?;
            assert_eq!(legacy_unfiltered.items[0]["id"], "cursor-next");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_cross_session_lease_rejects_takeover_and_recovers_only_expired_run() {
        let (dir, owner) = open_test_db("v5-cross-session-lease");
        let contender = DbState::open(dir.join("test.db")).unwrap();
        let run = db::with_connection(&owner, |conn| {
            seed_two_projects(conn)?;
            let run = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("create-cross-session".into()),
                },
                owner.app_session_id(),
            )?;
            let attempt = run.generation_attempt_id.clone().unwrap();
            comic_run_mark_submitted_inner(
                conn,
                ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt,
                    expected_status: "queued".into(),
                    idempotency_key: "submitted-cross-session".into(),
                },
                owner.app_session_id(),
            )?;
            Ok(run)
        })
        .unwrap();
        let attempt = run.generation_attempt_id.clone().unwrap();
        let mutation = ComicRunMutationInput {
            kind: "page".into(),
            run_id: run.id.clone(),
            comic_project_id: "comic_1".into(),
            generation_attempt_id: attempt.clone(),
            expected_status: "running".into(),
            idempotency_key: "heartbeat-owner".into(),
        };

        let owner_heartbeat = db::with_connection(&owner, |conn| {
            comic_run_heartbeat_inner(conn, mutation.clone(), owner.app_session_id())
        })
        .unwrap();
        let replay = db::with_connection(&owner, |conn| {
            comic_run_heartbeat_inner(conn, mutation.clone(), owner.app_session_id())
        })
        .unwrap();
        assert_eq!(owner_heartbeat, replay, "same receipt must replay exactly");

        let foreign_heartbeat = db::with_connection(&contender, |conn| {
            comic_run_heartbeat_inner(
                conn,
                ComicRunMutationInput {
                    idempotency_key: "heartbeat-contender".into(),
                    ..mutation.clone()
                },
                contender.app_session_id(),
            )
        });
        assert!(matches!(foreign_heartbeat, Err(error) if error.contains("不拥有")));
        let foreign_finish = db::with_connection(&contender, |conn| {
            comic_run_finish_inner(
                conn,
                ComicRunFinishV5Input {
                    run: ComicRunMutationInput {
                        idempotency_key: "finish-contender".into(),
                        ..mutation.clone()
                    },
                    outcome: json!({"status":"error", "error":"contender must not finish"}),
                },
                contender.app_session_id(),
            )
        });
        assert!(matches!(foreign_finish, Err(error) if error.contains("不拥有")));

        let before_expiry = db::with_connection(&contender, |conn| {
            comic_runs_recover_stale_inner(conn, contender.app_session_id())
        })
        .unwrap();
        assert_eq!(
            before_expiry, 0,
            "unexpired run cannot be taken over by sweep"
        );
        assert_eq!(
            db::with_connection(&owner, |conn| read_page_run(conn, &run.id))
                .unwrap()
                .status,
            "running"
        );
        db::with_connection(&owner, |conn| {
            conn.execute(
                "UPDATE comic_page_runs SET lease_expires_at=? WHERE id=?",
                params![now() - 1, run.id],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            db::with_connection(&contender, |conn| {
                comic_runs_recover_stale_inner(conn, contender.app_session_id())
            })
            .unwrap(),
            1,
            "expired row must be counted only when the UPDATE changed it"
        );
        assert_eq!(
            db::with_connection(&contender, |conn| {
                comic_runs_recover_stale_inner(conn, contender.app_session_id())
            })
            .unwrap(),
            0,
            "second sweep must not recount an already stale row"
        );
        assert_eq!(
            db::with_connection(&owner, |conn| read_page_run(conn, &run.id))
                .unwrap()
                .status,
            "stale"
        );
        let heartbeat_events: i64 = db::with_connection(&owner, |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM comic_run_events WHERE run_id=? AND event_type='heartbeat'",
                params![run.id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(
            heartbeat_events, 1,
            "receipt replay must not append an event"
        );
        drop(contender);
        drop(owner);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_legacy_null_identity_recovery_becomes_stale_without_inventing_identity() {
        let (dir, state) = open_test_db("v5-legacy-recovery");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            conn.execute(
                "INSERT INTO comic_page_runs (id, comic_project_id, page_no, asset_id, request_json, reference_snapshot_json, status, error, created_at, finished_at, parent_run_id, attempt_no, generation_attempt_id, owner_app_session_id, failure_json, submitted_at, heartbeat_at, lease_expires_at)
                 VALUES ('legacy-running', 'comic_1', 1, NULL, '{}', '[]', 'running', NULL, 1, NULL, NULL, 1, NULL, NULL, NULL, NULL, NULL, ?)",
                params![now() - 1],
            )
            .map_err(|e| e.to_string())?;
            assert_eq!(comic_runs_recover_stale_inner(conn, state.app_session_id())?, 1);
            let run = read_page_run(conn, "legacy-running")?;
            assert_eq!(run.status, "stale");
            assert_eq!(run.generation_attempt_id, None);
            assert_eq!(run.owner_app_session_id, None);
            let failure = run.failure_json.unwrap();
            assert_eq!(failure["code"], "LEGACY_RUNNING_NO_ATTEMPT");
            let event_owner: Option<String> = conn
                .query_row(
                    "SELECT owner_app_session_id FROM comic_run_events WHERE run_kind='page' AND run_id='legacy-running' AND event_type='legacy_stale'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(
                event_owner, None,
                "legacy recovery must not invent an attempt lease owner in its event"
            );
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_retry_reason_is_sanitized_in_event_and_receipt() {
        let (dir, state) = open_test_db("v5-retry-reason-redaction");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let session = state.app_session_id().to_owned();
            let run = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({"prompt":"retry"}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("retry-reason-create".into()),
                },
                &session,
            )?;
            let attempt = run.generation_attempt_id.clone().unwrap();
            comic_run_mark_submitted_inner(
                conn,
                ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt.clone(),
                    expected_status: "queued".into(),
                    idempotency_key: "retry-reason-submitted".into(),
                },
                &session,
            )?;
            comic_run_finish_inner(
                conn,
                ComicRunFinishV5Input {
                    run: ComicRunMutationInput {
                        kind: "page".into(),
                        run_id: run.id.clone(),
                        comic_project_id: "comic_1".into(),
                        generation_attempt_id: attempt.clone(),
                        expected_status: "running".into(),
                        idempotency_key: "retry-reason-finish".into(),
                    },
                    outcome: json!({"status":"error","error":"provider unavailable"}),
                },
                &session,
            )?;
            let child = comic_run_retry_inner(
                conn,
                ComicRunRetryInput {
                    run: ComicRunMutationInput {
                        kind: "page".into(),
                        run_id: run.id.clone(),
                        comic_project_id: "comic_1".into(),
                        generation_attempt_id: attempt,
                        expected_status: "error".into(),
                        idempotency_key: "retry-reason-secret".into(),
                    },
                    retry_reason: "Authorization: Bearer TOPSECRET api_key=TOPSECRET".into(),
                },
                &session,
            )?;
            let event: String = conn
                .query_row(
                    "SELECT payload_json FROM comic_run_events WHERE run_kind='page' AND run_id=? AND event_type='retry_created'",
                    params![child["id"].as_str()],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            let receipt: String = conn
                .query_row(
                    "SELECT response_json FROM comic_operation_receipts WHERE command_name='comic_run_retry' AND idempotency_key='retry-reason-secret'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            for forbidden in ["TOPSECRET", "api_key", "Authorization"] {
                assert!(
                    !event.contains(forbidden),
                    "retry event must not persist {forbidden}"
                );
                assert!(
                    !receipt.contains(forbidden),
                    "retry receipt response must not persist {forbidden}"
                );
            }
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_retry_does_not_fork_an_active_attempt_lineage() {
        let (dir, state) = open_test_db("v5-retry-lineage");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let session = state.app_session_id().to_owned();
            let run = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({"prompt":"lineage"}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("lineage-create".into()),
                },
                &session,
            )?;
            let attempt = run.generation_attempt_id.clone().unwrap();
            comic_run_mark_submitted_inner(
                conn,
                ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt.clone(),
                    expected_status: "queued".into(),
                    idempotency_key: "lineage-submitted".into(),
                },
                &session,
            )?;
            comic_run_finish_inner(
                conn,
                ComicRunFinishV5Input {
                    run: ComicRunMutationInput {
                        kind: "page".into(),
                        run_id: run.id.clone(),
                        comic_project_id: "comic_1".into(),
                        generation_attempt_id: attempt.clone(),
                        expected_status: "running".into(),
                        idempotency_key: "lineage-finish".into(),
                    },
                    outcome: json!({"status":"error","error":"retry me"}),
                },
                &session,
            )?;
            let retry = ComicRunRetryInput {
                run: ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt,
                    expected_status: "error".into(),
                    idempotency_key: "lineage-retry-one".into(),
                },
                retry_reason: "retry".into(),
            };
            let first = comic_run_retry_inner(conn, retry.clone(), &session)?;
            let second = comic_run_retry_inner(
                conn,
                ComicRunRetryInput {
                    run: ComicRunMutationInput {
                        idempotency_key: "lineage-retry-two".into(),
                        ..retry.run
                    },
                    ..retry
                },
                &session,
            );
            assert!(
                matches!(second, Err(ref error) if error.contains("已有 child")),
                "a second key must not fork the active attempt lineage: {second:?}"
            );
            let children: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM comic_page_runs WHERE parent_run_id = ?",
                    params![run.id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(children, 1);
            assert_eq!(first["attemptNo"], 2);
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_panel_create_cannot_fork_a_supplied_parent_attempt() {
        let (dir, state) = open_test_db("v5-panel-create-lineage");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "cch_1", "comic_1", 1)?;
            seed_scene(conn, "csc_1", "cch_1", 1)?;
            seed_panel(conn, "cpn_1", "csc_1", 1)?;
            let session = state.app_session_id().to_owned();
            let parent = comic_panel_run_create_v5_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    strategy: "text_to_image".into(),
                    request_json: json!({"prompt":"parent"}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: None,
                    idempotency_key: Some("panel-parent".into()),
                },
                &session,
            )?;
            conn.execute(
                "UPDATE comic_panel_runs SET status='error' WHERE id=?",
                params![parent.id],
            )
            .map_err(|error| error.to_string())?;
            let make_child = |key: &str| ComicPanelRunCreateInput {
                comic_project_id: "comic_1".into(),
                panel_id: "cpn_1".into(),
                strategy: "text_to_image".into(),
                request_json: json!({"prompt":"child"}),
                reference_snapshot_json: json!([]),
                parent_run_id: Some(parent.id.clone()),
                idempotency_key: Some(key.into()),
            };
            let first =
                comic_panel_run_create_v5_inner(conn, make_child("panel-child-one"), &session)?;
            let second =
                comic_panel_run_create_v5_inner(conn, make_child("panel-child-two"), &session);
            assert!(
                matches!(second, Err(ref error) if error.contains("已有 child")),
                "different create keys must not fork the supplied parent: {second:?}"
            );
            let children: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM comic_panel_runs WHERE parent_run_id=?",
                    params![parent.id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(children, 1);
            assert_eq!(first.attempt_no, 2);
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_retry_and_parent_create_reject_attempt_number_overflow() {
        let (dir, state) = open_test_db("v5-attempt-overflow");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "cch_1", "comic_1", 1)?;
            seed_scene(conn, "csc_1", "cch_1", 1)?;
            seed_panel(conn, "cpn_1", "csc_1", 1)?;
            let session = state.app_session_id().to_owned();
            let panel_parent = comic_panel_run_create_v5_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    strategy: "text_to_image".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: None,
                    idempotency_key: Some("overflow-panel-parent".into()),
                },
                &session,
            )?;
            conn.execute(
                "UPDATE comic_panel_runs SET status='error', attempt_no=? WHERE id=?",
                params![i64::MAX, panel_parent.id],
            )
            .map_err(|error| error.to_string())?;
            let panel_create = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                comic_panel_run_create_v5_inner(
                    conn,
                    ComicPanelRunCreateInput {
                        comic_project_id: "comic_1".into(),
                        panel_id: "cpn_1".into(),
                        strategy: "text_to_image".into(),
                        request_json: json!({}),
                        reference_snapshot_json: json!([]),
                        parent_run_id: Some(panel_parent.id.clone()),
                        idempotency_key: Some("overflow-panel-child".into()),
                    },
                    &session,
                )
            }));
            assert!(
                matches!(panel_create, Ok(Err(ref error)) if error.contains("attemptNo")),
                "overflowing panel parent attempt must be rejected, not panic or wrap: {panel_create:?}"
            );

            conn.execute(
                "INSERT INTO comic_page_runs (id, comic_project_id, page_no, asset_id, request_json, reference_snapshot_json, status, error, created_at, finished_at, parent_run_id, attempt_no, generation_attempt_id, owner_app_session_id, failure_json, submitted_at, heartbeat_at, lease_expires_at)
                 VALUES ('overflow-page-parent', 'comic_1', 1, NULL, '{}', '[]', 'error', 'failed', 1, 1, NULL, ?, 'overflow-attempt', NULL, NULL, NULL, NULL, NULL)",
                params![i64::MAX],
            )
            .map_err(|error| error.to_string())?;
            let retry = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                comic_run_retry_inner(
                    conn,
                    ComicRunRetryInput {
                        run: ComicRunMutationInput {
                            kind: "page".into(),
                            run_id: "overflow-page-parent".into(),
                            comic_project_id: "comic_1".into(),
                            generation_attempt_id: "overflow-attempt".into(),
                            expected_status: "error".into(),
                            idempotency_key: "overflow-page-retry".into(),
                        },
                        retry_reason: "retry".into(),
                    },
                    &session,
                )
            }));
            assert!(
                matches!(retry, Ok(Err(ref error)) if error.contains("attemptNo")),
                "overflowing retry attempt must be rejected, not panic or wrap: {retry:?}"
            );
            let children: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM comic_page_runs WHERE parent_run_id='overflow-page-parent'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(children, 0);
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_finish_errors_are_sanitized_and_success_ignores_error_text() {
        let (dir, state) = open_test_db("legacy-finish-redaction");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            seed_chapter(conn, "cch_1", "comic_1", 1)?;
            seed_scene(conn, "csc_1", "cch_1", 1)?;
            seed_panel(conn, "cpn_1", "csc_1", 1)?;
            let page = comic_page_run_create_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: None,
                },
            )?;
            let panel = comic_panel_run_create_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    strategy: "text_to_image".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: None,
                    idempotency_key: None,
                },
            )?;
            let secret = "Authorization: Bearer TOPSECRET api_key=TOPSECRET";
            let page = comic_page_run_finish_inner(
                conn,
                ComicPageRunFinishInput {
                    id: page.id,
                    asset_id: None,
                    status: "error".into(),
                    error: Some(secret.into()),
                },
            )?;
            let panel = comic_panel_run_finish_inner(
                conn,
                ComicPanelRunFinishInput {
                    id: panel.id,
                    asset_id: None,
                    status: "error".into(),
                    error: Some(secret.into()),
                    score_json: None,
                },
            )?;
            for error in [page.error, panel.error] {
                let error = error.unwrap_or_default();
                assert!(!error.contains("TOPSECRET"));
                assert!(!error.contains("api_key"));
                assert!(!error.contains("Authorization"));
            }
            seed_asset(conn, "asset_success", "p_1")?;
            let success_page = comic_page_run_create_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 2,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: None,
                },
            )?;
            let success_panel = comic_panel_run_create_inner(
                conn,
                ComicPanelRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    panel_id: "cpn_1".into(),
                    strategy: "text_to_image".into(),
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    parent_run_id: None,
                    idempotency_key: None,
                },
            )?;
            let success_page = comic_page_run_finish_inner(
                conn,
                ComicPageRunFinishInput {
                    id: success_page.id,
                    asset_id: Some("asset_success".into()),
                    status: "success".into(),
                    error: Some(secret.into()),
                },
            )?;
            let success_panel = comic_panel_run_finish_inner(
                conn,
                ComicPanelRunFinishInput {
                    id: success_panel.id,
                    asset_id: Some("asset_success".into()),
                    status: "success".into(),
                    error: Some(secret.into()),
                    score_json: None,
                },
            )?;
            assert_eq!(success_page.status, "success");
            assert_eq!(success_panel.status, "success");
            assert_eq!(success_page.error, None);
            assert_eq!(success_panel.error, None);
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_concurrent_receipt_replays_once_and_rejects_changed_payload() {
        let (dir, state) = open_test_db("v5-concurrent-receipt");
        let db_path = dir.join("test.db");
        let (run, session) = db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let run = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("create-concurrent".into()),
                },
                state.app_session_id(),
            )?;
            let session = state.app_session_id().to_owned();
            let attempt = run.generation_attempt_id.clone().unwrap();
            comic_run_mark_submitted_inner(
                conn,
                ComicRunMutationInput {
                    kind: "page".into(),
                    run_id: run.id.clone(),
                    comic_project_id: "comic_1".into(),
                    generation_attempt_id: attempt,
                    expected_status: "queued".into(),
                    idempotency_key: "submitted-concurrent".into(),
                },
                &session,
            )?;
            Ok((run, session))
        })
        .unwrap();
        drop(state);
        let input = ComicRunMutationInput {
            kind: "page".into(),
            run_id: run.id.clone(),
            comic_project_id: "comic_1".into(),
            generation_attempt_id: run.generation_attempt_id.clone().unwrap(),
            expected_status: "running".into(),
            idempotency_key: "heartbeat-concurrent".into(),
        };
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let barrier = barrier.clone();
            let db_path = db_path.clone();
            let input = input.clone();
            let session = session.clone();
            workers.push(std::thread::spawn(move || -> Result<Value, String> {
                let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
                conn.busy_timeout(std::time::Duration::from_secs(5))
                    .map_err(|e| e.to_string())?;
                barrier.wait();
                comic_run_heartbeat_inner(&conn, input, &session)
            }));
        }
        barrier.wait();
        let first = workers.remove(0).join().unwrap().unwrap();
        let second = workers.remove(0).join().unwrap().unwrap();
        assert_eq!(
            first, second,
            "concurrent equal requests must replay one receipt"
        );

        let changed_payload =
            db::with_connection(&DbState::open(db_path.clone()).unwrap(), |conn| {
                comic_run_heartbeat_inner(
                    conn,
                    ComicRunMutationInput {
                        expected_status: "queued".into(),
                        ..input
                    },
                    &session,
                )
            });
        assert!(matches!(changed_payload, Err(error) if error.contains("不同业务载荷")));
        let verify = DbState::open(db_path).unwrap();
        let events: i64 = db::with_connection(&verify, |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM comic_run_events WHERE run_id=? AND event_type='heartbeat'",
                params![run.id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(
            events, 1,
            "receipt replay and conflicting request must not duplicate events"
        );
        drop(verify);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn receipt_action_error_rolls_back_immediate_transaction_for_same_connection() {
        let (dir, state) = open_test_db("receipt-immediate-rollback");
        db::with_connection(&state, |conn| {
            let failed = with_receipt::<Value>(
                conn,
                "rollback-test",
                "rollback-key",
                &json!({"version": 1}),
                |conn| {
                    conn.execute(
                        "INSERT INTO settings (key, value) VALUES ('receipt-rollback', 'must disappear')",
                        [],
                    )
                    .map_err(|e| e.to_string())?;
                    Err("intentional action failure".into())
                },
            );
            assert!(matches!(failed, Err(error) if error.contains("intentional action failure")));
            let writes: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM settings WHERE key='receipt-rollback'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(writes, 0, "failed receipt action must roll back its writes");
            let recovered: Value = with_receipt(
                conn,
                "rollback-test",
                "rollback-key",
                &json!({"version": 1}),
                |_| Ok(json!({"ok": true})),
            )?;
            assert_eq!(recovered, json!({"ok": true}));
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_reconcile_event_and_receipt_allowlist_observation() {
        let (dir, state) = open_test_db("v5-reconcile-observation-redaction");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let run = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("reconcile-create".into()),
                },
                state.app_session_id(),
            )?;
            let attempt = run.generation_attempt_id.clone().unwrap();
            conn.execute(
                "UPDATE comic_page_runs SET status='stale', lease_expires_at=NULL WHERE id=?",
                params![run.id],
            )
            .map_err(|e| e.to_string())?;
            seed_asset(conn, "reconcile-asset", "p_1")?;
            conn.execute(
                "UPDATE assets SET metadata=? WHERE id='reconcile-asset'",
                params![json!({
                    "projectId":"p_1",
                    "comicRunId":run.id,
                    "generationAttemptId":attempt
                })
                .to_string()],
            )
            .map_err(|e| e.to_string())?;
            let result = comic_run_reconcile_inner(
                conn,
                ComicRunReconcileInput {
                    run: ComicRunMutationInput {
                        kind: "page".into(),
                        run_id: run.id.clone(),
                        comic_project_id: "comic_1".into(),
                        generation_attempt_id: attempt,
                        expected_status: "stale".into(),
                        idempotency_key: "reconcile-secret".into(),
                    },
                    observation: json!({
                        "assetId":"reconcile-asset",
                        "authorization":"TOPSECRET",
                        "debug":"Bearer TOPSECRET sk-secret api_key=TOPSECRET"
                    }),
                },
            )?;
            assert_eq!(result["status"], "success");
            let event: String = conn
                .query_row(
                    "SELECT payload_json FROM comic_run_events WHERE run_id=? AND event_type='reconciled'",
                    params![run.id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let receipt: String = conn
                .query_row(
                    "SELECT response_json FROM comic_operation_receipts WHERE command_name='comic_run_reconcile' AND idempotency_key='reconcile-secret'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let run_row: (Option<String>, Option<String>, Option<String>) = conn
                .query_row(
                    "SELECT asset_id, error, failure_json FROM comic_page_runs WHERE id=?",
                    params![run.id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(run_row.0.as_deref(), Some("reconcile-asset"));
            let persisted = format!("{run_row:?}\n{event}\n{receipt}").to_ascii_lowercase();
            for secret in ["topsecret", "secret", "authorization"] {
                assert!(!persisted.contains(secret), "persisted {secret}: {persisted}");
            }
            assert_eq!(serde_json::from_str::<Value>(&event).unwrap(), json!({"assetId":"reconcile-asset"}));
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v5_abandon_reason_is_sanitized_in_run_event_and_receipt() {
        let (dir, state) = open_test_db("v5-abandon-reason-redaction");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            let run = comic_page_run_create_v5_inner(
                conn,
                ComicPageRunCreateInput {
                    comic_project_id: "comic_1".into(),
                    page_no: 1,
                    request_json: json!({}),
                    reference_snapshot_json: json!([]),
                    idempotency_key: Some("abandon-create".into()),
                },
                state.app_session_id(),
            )?;
            let attempt = run.generation_attempt_id.clone().unwrap();
            conn.execute(
                "UPDATE comic_page_runs SET status='stale', lease_expires_at=NULL WHERE id=?",
                params![run.id],
            )
            .map_err(|e| e.to_string())?;
            let result = comic_run_abandon_stale_inner(
                conn,
                ComicRunAbandonInput {
                    run: ComicRunMutationInput {
                        kind: "page".into(),
                        run_id: run.id.clone(),
                        comic_project_id: "comic_1".into(),
                        generation_attempt_id: attempt,
                        expected_status: "stale".into(),
                        idempotency_key: "abandon-secret".into(),
                    },
                    reason: "Bearer TOPSECRET sk-secret api_key=TOPSECRET password=TOPSECRET".into(),
                },
            )?;
            assert_eq!(result["status"], "error");
            let error: String = conn
                .query_row(
                    "SELECT error FROM comic_page_runs WHERE id=?",
                    params![run.id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let failure: String = conn
                .query_row(
                    "SELECT failure_json FROM comic_page_runs WHERE id=?",
                    params![run.id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let event: String = conn
                .query_row(
                    "SELECT payload_json FROM comic_run_events WHERE run_id=? AND event_type='abandoned'",
                    params![run.id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let receipt: String = conn
                .query_row(
                    "SELECT response_json FROM comic_operation_receipts WHERE command_name='comic_run_abandon_stale' AND idempotency_key='abandon-secret'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let persisted = format!("{error}\n{failure}\n{event}\n{receipt}").to_ascii_lowercase();
            for secret in ["topsecret", "secret", "authorization", "password"] {
                assert!(!persisted.contains(secret), "persisted {secret}: {persisted}");
            }
            assert_eq!(safe_failure_text("  "), "生成失败，未提供详情");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn page_history_scoped_query_uses_stable_index_order() {
        let (dir, state) = open_test_db("page-history-query-plan");
        db::with_connection(&state, |conn| {
            let explain = format!("EXPLAIN QUERY PLAN {PAGE_HISTORY_BY_PAGE_SQL}");
            let mut statement = conn.prepare(&explain).map_err(|e| e.to_string())?;
            let details = statement
                .query_map(params!["comic_1", 1_i64], |row| row.get::<_, String>(3))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            let plan = details.join(" | ");
            assert!(
                plan.contains("idx_comic_page_runs_stable"),
                "page-scoped history must use its v5 stable index: {plan}"
            );
            assert!(
                !plan.contains("USE TEMP B-TREE"),
                "page-scoped history must not sort outside its stable index: {plan}"
            );
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn page_review_is_idempotent_scoped_and_optimistically_selects_one_head() {
        let (dir, state) = open_test_db("page-review");
        db::with_connection(&state, |conn| {
            seed_two_projects(conn)?;
            for (asset_id, project_id) in [
                ("page-asset-1", "p_1"),
                ("page-asset-2", "p_1"),
                ("page-asset-3", "p_1"),
                ("foreign-page-asset", "p_2"),
            ] {
                seed_asset(conn, asset_id, project_id)?;
            }
            for (run_id, project_id, asset_id, created_at) in [
                ("page-run-1", "comic_1", "page-asset-1", 1_i64),
                ("page-run-2", "comic_1", "page-asset-2", 2_i64),
                ("page-run-3", "comic_1", "page-asset-3", 3_i64),
                ("foreign-page-run", "comic_2", "foreign-page-asset", 4_i64),
            ] {
                conn.execute(
                    "INSERT INTO comic_page_runs(id,comic_project_id,page_no,asset_id,request_json,reference_snapshot_json,status,created_at,finished_at)
                     VALUES (?,?,?,?, '{}','{}','success',?,?)",
                    params![run_id, project_id, 1_i64, asset_id, created_at, created_at],
                )
                .map_err(|error| error.to_string())?;
            }

            let first_input = ComicPageReviewSubmitInput {
                comic_project_id: "comic_1".into(),
                page_run_id: "page-run-1".into(),
                decision: "approved".into(),
                report_json: json!({"checks":{"text":"manual-pass"}}),
                expected_optimistic_version: 0,
                idempotency_key: "page-review-1".into(),
            };
            let first = comic_page_review_submit_inner(conn, first_input.clone())?;
            assert_eq!(first.head.as_ref().unwrap().optimistic_version, 1);
            assert_eq!(first.head.as_ref().unwrap().approved_page_run_id, "page-run-1");
            let replay = comic_page_review_submit_inner(conn, first_input)?;
            assert_eq!(replay.review.id, first.review.id);

            let stale = comic_page_review_submit_inner(
                conn,
                ComicPageReviewSubmitInput {
                    comic_project_id: "comic_1".into(),
                    page_run_id: "page-run-2".into(),
                    decision: "approved".into(),
                    report_json: json!({"checks":{}}),
                    expected_optimistic_version: 0,
                    idempotency_key: "page-review-stale".into(),
                },
            )
            .unwrap_err();
            assert!(stale.contains("版本已变化"), "实际错误: {stale}");
            assert_eq!(
                conn.query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM comic_page_reviews WHERE page_run_id='page-run-2'",
                    [],
                    |row| row.get(0)
                )
                .map_err(|error| error.to_string())?,
                0,
                "CAS 失败必须回滚 review"
            );

            let second = comic_page_review_submit_inner(
                conn,
                ComicPageReviewSubmitInput {
                    comic_project_id: "comic_1".into(),
                    page_run_id: "page-run-2".into(),
                    decision: "approved".into(),
                    report_json: json!({"checks":{"layout":"pass"}}),
                    expected_optimistic_version: 1,
                    idempotency_key: "page-review-2".into(),
                },
            )?;
            assert_eq!(second.head.as_ref().unwrap().optimistic_version, 2);
            assert_eq!(second.head.as_ref().unwrap().approved_page_run_id, "page-run-2");

            let rejected = comic_page_review_submit_inner(
                conn,
                ComicPageReviewSubmitInput {
                    comic_project_id: "comic_1".into(),
                    page_run_id: "page-run-3".into(),
                    decision: "rejected".into(),
                    report_json: json!({"reason":"中文乱码"}),
                    expected_optimistic_version: 0,
                    idempotency_key: "page-review-3".into(),
                },
            )?;
            assert_eq!(rejected.head.as_ref().unwrap().approved_page_run_id, "page-run-2");

            let foreign = comic_page_review_submit_inner(
                conn,
                ComicPageReviewSubmitInput {
                    comic_project_id: "comic_1".into(),
                    page_run_id: "foreign-page-run".into(),
                    decision: "approved".into(),
                    report_json: json!({}),
                    expected_optimistic_version: 2,
                    idempotency_key: "page-review-foreign".into(),
                },
            )
            .unwrap_err();
            assert!(foreign.contains("不属于当前漫画项目"), "实际错误: {foreign}");

            let state = read_page_review_state(conn, "comic_1", 1)?;
            assert_eq!(state.reviews.len(), 3);
            assert_eq!(state.head.unwrap().optimistic_version, 2);
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }
}
