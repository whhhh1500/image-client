use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::Manager;
use uuid::Uuid;

use crate::db::{self, DbState};
use crate::AppState;

const MAX_TITLE_BYTES: usize = 512;
const MAX_CHAPTER_CONTENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_ANALYSIS_PROMPT_BYTES: usize = 256 * 1024;
const MAX_CHAPTER_STATE_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
// LLM HTTP calls may consume the configured 300s timeout; leave a commit margin.
const ANALYSIS_LEASE_MS: i64 = 360_000;
const REQUIRED_ARTIFACT_TYPES: [&str; 14] = [
    "chapter_summary",
    "chapter_beats",
    "world_facts",
    "character_facts",
    "faction_facts",
    "location_facts",
    "prop_facts",
    "timeline_delta",
    "continuity_delta",
    "open_threads",
    "page_panel_plan",
    "adaptation_proposal",
    "comic_chapter_plan",
    "scene_plan",
];
// The adaptation provider must receive a frozen, explicit version of these
// source outputs.  Keep this order aligned with novel_adaptation's contract.
const PRODUCTION_SOURCE_ARTIFACT_TYPES: [&str; 10] = [
    "chapter_summary",
    "chapter_beats",
    "world_facts",
    "character_facts",
    "faction_facts",
    "location_facts",
    "prop_facts",
    "timeline_delta",
    "continuity_delta",
    "open_threads",
];
const PRODUCTION_ADAPTATION_ARTIFACT_TYPES: [&str; 4] = [
    "adaptation_proposal",
    "comic_chapter_plan",
    "scene_plan",
    "page_panel_plan",
];
pub(crate) const COMIC_PLAN_INTENT_VERSION: &str = "comic-plan-intent.v1";
const COMIC_PLAN_INTENT_MAX_PAGES: usize = 32;
const COMIC_PLAN_INTENT_MAX_PANEL_COUNT: i64 = 64;
const COMIC_PLAN_INTENT_MAX_BYTES: usize = MAX_ANALYSIS_PROMPT_BYTES / 4;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ComicPlanDialogueIntent {
    pub panel_no: i64,
    pub speaker: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ComicPlanPageIntent {
    pub panel_count: i64,
    #[serde(default)]
    pub layout_profile: Option<String>,
    #[serde(default)]
    pub dialogues: Option<Vec<ComicPlanDialogueIntent>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ComicPlanIntent {
    pub pages: Vec<ComicPlanPageIntent>,
}

pub(crate) fn comic_plan_profile_instruction(profile: &str) -> Option<&'static str> {
    match profile {
        "reference_story_5" => Some(
            "严格使用五格 2-1-2：第 1、2 格位于顶部且不等宽、以轻微斜切分隔；第 3 格位于中部并横跨整页、约占页面 40%，是唯一主视觉；第 4、5 格位于底部且大小不等。阅读顺序固定为 1,2,3,4,5。",
        ),
        "hero_middle_5" => Some(
            "严格使用五格 2-1-2：第 1、2 格位于顶部且不等宽、只有一条轻微斜切分隔；第 3 格位于中部通栏、约占页面 40%，承载唯一高光；第 4、5 格位于底部且大小不等。阅读顺序固定为 1,2,3,4,5。",
        ),
        _ => None,
    }
}

pub(crate) fn canonicalize_comic_plan_intent(
    input: Option<ComicPlanIntent>,
) -> Result<Option<ComicPlanIntent>, String> {
    let Some(mut intent) = input else {
        return Ok(None);
    };
    if intent.pages.is_empty() || intent.pages.len() > COMIC_PLAN_INTENT_MAX_PAGES {
        return Err("COMIC_PLAN_INTENT_PAGES_REQUIRED".into());
    }
    let mut bytes = 0usize;
    for page in &mut intent.pages {
        if page.panel_count <= 0 || page.panel_count > COMIC_PLAN_INTENT_MAX_PANEL_COUNT {
            return Err("COMIC_PLAN_INTENT_PANEL_COUNT_INVALID".into());
        }
        if let Some(profile) = &mut page.layout_profile {
            *profile = profile.trim().to_owned();
            let Some(_) = comic_plan_profile_instruction(profile) else {
                return Err("COMIC_PLAN_INTENT_LAYOUT_PROFILE_UNSUPPORTED".into());
            };
            if page.panel_count != 5 {
                return Err("COMIC_PLAN_INTENT_LAYOUT_PANEL_COUNT_MISMATCH".into());
            }
        }
        if let Some(dialogues) = &page.dialogues {
            let mut previous_panel_no = 0;
            for dialogue in dialogues {
                if dialogue.panel_no <= 0 || dialogue.panel_no > page.panel_count {
                    return Err("COMIC_PLAN_INTENT_DIALOGUE_PANEL_INVALID".into());
                }
                if dialogue.panel_no < previous_panel_no {
                    return Err("COMIC_PLAN_INTENT_DIALOGUE_ORDER_INVALID".into());
                }
                if dialogue.speaker.trim().is_empty() || dialogue.text.trim().is_empty() {
                    return Err("COMIC_PLAN_INTENT_DIALOGUE_INVALID".into());
                }
                bytes = bytes
                    .saturating_add(dialogue.speaker.len())
                    .saturating_add(dialogue.text.len());
                if bytes > COMIC_PLAN_INTENT_MAX_BYTES {
                    return Err("COMIC_PLAN_INTENT_TOO_LARGE".into());
                }
                previous_panel_no = dialogue.panel_no;
            }
        }
    }
    Ok(Some(intent))
}

pub(crate) fn comic_plan_intent_constraints(intent: &ComicPlanIntent) -> Value {
    json!({
        "version": COMIC_PLAN_INTENT_VERSION,
        "pages": intent.pages.iter().enumerate().map(|(index, page)| json!({
            "pageNo": index + 1,
            "panelCount": page.panel_count,
            "layoutProfile": &page.layout_profile,
            "layoutProfileInstruction": page.layout_profile.as_deref().and_then(comic_plan_profile_instruction),
            "layoutProfileGeometry": page.layout_profile.as_deref().map(|_| json!({
                "readingOrder": [1, 2, 3, 4, 5],
                "dominantPanel": 3,
                "topMaxY": 0.06,
                "minUnequalWidthDelta": 0.02,
                "requiresSlantedTopDivider": true,
                "topReadingOrder": "panel 1 must be left of panel 2 and both finish before panel 3",
                "middle": {"maxX": 0.06, "minWidth": 0.9, "minY": 0.24, "maxY": 0.30, "minHeight": 0.35},
                "bottomMinY": 0.66,
                "bottomReadingOrder": "panel 4 must be left of panel 5 and both start after panel 3"
            })),
            "dialogues": &page.dialogues,
        })).collect::<Vec<_>>(),
    })
}
// A process-local wake registry only de-duplicates timers. Durable leases and
// the per-job recovery decision remain the source of truth across restarts.
static PRODUCTION_RECOVERY_WAKE_JOBS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
const NOVEL_ANALYSIS_SCHEMA: &str =
    include_str!("../../src/shared/contracts/novel-analysis.v1.schema.json");
type CompletionBase = (
    i64,
    i64,
    i64,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

pub(crate) fn now() -> i64 {
    chrono::Local::now().timestamp_millis()
}

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4())
}

pub(crate) fn request_hash(value: &Value) -> Result<String, String> {
    let serialized =
        serde_json::to_vec(value).map_err(|error| format!("序列化幂等请求失败: {error}"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(serialized)))
}

fn completion_endpoint(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.into()
    } else {
        format!("{base}/chat/completions")
    }
}

pub(crate) fn json_value(raw: String) -> Value {
    serde_json::from_str(&raw).unwrap_or(Value::Null)
}

fn ensure_nonempty<'a>(value: &'a str, field: &str) -> Result<&'a str, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} 不能为空"));
    }
    Ok(value)
}

fn ensure_title(value: &str, field: &str) -> Result<String, String> {
    let value = ensure_nonempty(value, field)?;
    if value.len() > MAX_TITLE_BYTES {
        return Err(format!("{field} 不能超过 {MAX_TITLE_BYTES} 字节"));
    }
    Ok(value.to_owned())
}

fn normalize_content(content: &str) -> Result<String, String> {
    if content.len() > MAX_CHAPTER_CONTENT_BYTES {
        return Err(format!("章节正文不能超过 {MAX_CHAPTER_CONTENT_BYTES} 字节"));
    }
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.trim().is_empty() {
        return Err("章节正文不能为空".into());
    }
    Ok(normalized)
}

pub(crate) fn with_receipt<T>(
    conn: &Connection,
    command_name: &str,
    idempotency_key: &str,
    request: &Value,
    action: impl FnOnce(&Transaction<'_>) -> Result<T, String>,
) -> Result<T, String>
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    let key = ensure_nonempty(idempotency_key, "idempotencyKey")?;
    let hash = request_hash(request)?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|error| format!("开始小说幂等事务失败: {error}"))?;
    let receipt: Option<(String, String)> = tx
        .query_row(
            "SELECT request_hash, response_json FROM novel_operation_receipts
             WHERE command_name = ? AND idempotency_key = ?",
            params![command_name, key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| format!("读取小说操作回执失败: {error}"))?;
    if let Some((stored_hash, response_json)) = receipt {
        if stored_hash != hash {
            return Err("idempotencyKey 已用于不同业务载荷".into());
        }
        return serde_json::from_str(&response_json)
            .map_err(|error| format!("读取小说操作回执响应失败: {error}"));
    }
    let response = action(&tx)?;
    let response_json = serde_json::to_string(&response)
        .map_err(|error| format!("序列化小说操作回执失败: {error}"))?;
    tx.execute(
        "INSERT INTO novel_operation_receipts
         (id, command_name, idempotency_key, request_hash, response_json, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        params![
            new_id("nreceipt"),
            command_name,
            key,
            hash,
            response_json,
            now()
        ],
    )
    .map_err(|error| format!("保存小说操作回执失败: {error}"))?;
    tx.commit()
        .map_err(|error| format!("提交小说幂等事务失败: {error}"))?;
    Ok(response)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelWork {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub published_canon_version_id: String,
    pub current_novel_state_version_id: String,
    pub current_analysis_lineage_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelVolume {
    pub id: String,
    pub novel_work_id: String,
    pub volume_no: i64,
    pub title: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelChapter {
    pub id: String,
    pub novel_work_id: String,
    pub volume_id: Option<String>,
    pub sequence_no: i64,
    pub chapter_no: i64,
    pub title: Option<String>,
    pub latest_revision_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelChapterRevision {
    pub id: String,
    pub novel_work_id: String,
    pub chapter_id: String,
    pub revision_no: i64,
    pub content: String,
    pub content_hash: String,
    pub asset_id: Option<String>,
    pub requested_parent_context_revision_id: Option<String>,
    pub source_kind: String,
    pub analysis_status: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelAnalysisLineage {
    pub id: String,
    pub novel_work_id: String,
    pub current_context_revision_id: Option<String>,
    pub continuous_through_sequence_no: i64,
    pub optimistic_version: i64,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelChapterContext {
    pub id: String,
    pub novel_analysis_lineage_id: String,
    pub novel_chapter_revision_id: String,
    pub parent_context_revision_id: Option<String>,
    pub source_analysis_run_id: String,
    pub resolved_working_canon_hash: String,
    pub resolved_working_state_hash: String,
    pub sequence_gap: Value,
    pub branch_kind: String,
    pub status: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelSnapshot {
    pub work: NovelWork,
    pub volumes: Vec<NovelVolume>,
    pub chapters: Vec<NovelChapter>,
    pub revisions: Vec<NovelChapterRevision>,
    pub entities: Vec<NovelEntity>,
    pub context_status: String,
    pub current_context_revision_id: Option<String>,
    pub current_novel_state_through_sequence_no: i64,
    pub canonical_version_id: String,
    pub lineages: Vec<NovelAnalysisLineage>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelEntity {
    pub id: String,
    pub novel_work_id: String,
    pub entity_type: String,
    pub name: String,
    pub status: String,
    pub facts: Value,
    pub aliases: Vec<String>,
    pub provisional: bool,
}

fn work_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NovelWork> {
    Ok(NovelWork {
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        status: row.get(4)?,
        published_canon_version_id: row.get(5)?,
        current_novel_state_version_id: row.get(6)?,
        current_analysis_lineage_id: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

pub(crate) fn get_work(
    conn: &Connection,
    project_id: &str,
    novel_work_id: &str,
) -> Result<NovelWork, String> {
    let project_id = ensure_nonempty(project_id, "projectId")?;
    let novel_work_id = ensure_nonempty(novel_work_id, "novelWorkId")?;
    conn.query_row(
        "SELECT id, project_id, title, description, status, published_canon_version_id,
                current_novel_state_version_id, current_analysis_lineage_id, created_at, updated_at
         FROM novel_works WHERE id = ? AND project_id = ?",
        params![novel_work_id, project_id],
        work_from_row,
    )
    .optional()
    .map_err(|error| format!("读取小说失败: {error}"))?
    .ok_or_else(|| "小说不存在或不属于当前项目".into())
}

pub(crate) fn ensure_active_work(work: &NovelWork) -> Result<(), String> {
    if work.status == "active" {
        Ok(())
    } else {
        Err("小说已归档，恢复后才能修改".into())
    }
}

fn ensure_active_work_in_tx(tx: &Transaction<'_>, work: &NovelWork) -> Result<(), String> {
    let active: bool = tx
        .query_row(
            "SELECT status='active' FROM novel_works WHERE id=? AND project_id=?",
            params![work.id, work.project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("验证小说状态失败: {error}"))?
        .unwrap_or(false);
    if active {
        Ok(())
    } else {
        Err("小说已归档，恢复后才能修改".into())
    }
}

fn list_volumes(conn: &Connection, work_id: &str) -> Result<Vec<NovelVolume>, String> {
    let mut statement = conn
        .prepare(
            "SELECT id, novel_work_id, volume_no, title, created_at FROM novel_volumes
             WHERE novel_work_id = ? ORDER BY volume_no, id",
        )
        .map_err(|error| format!("准备卷列表失败: {error}"))?;
    let rows = statement
        .query_map(params![work_id], |row| {
            Ok(NovelVolume {
                id: row.get(0)?,
                novel_work_id: row.get(1)?,
                volume_no: row.get(2)?,
                title: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(|error| format!("查询卷列表失败: {error}"))?;
    rows.map(|row| row.map_err(|error| format!("读取卷失败: {error}")))
        .collect()
}

fn list_chapters(conn: &Connection, work_id: &str) -> Result<Vec<NovelChapter>, String> {
    let mut statement = conn
        .prepare(
            "SELECT id, novel_work_id, volume_id, sequence_no, chapter_no, title,
                    current_revision_id, created_at, updated_at
             FROM novel_chapters WHERE novel_work_id = ? ORDER BY sequence_no, id",
        )
        .map_err(|error| format!("准备章节列表失败: {error}"))?;
    let rows = statement
        .query_map(params![work_id], |row| {
            Ok(NovelChapter {
                id: row.get(0)?,
                novel_work_id: row.get(1)?,
                volume_id: row.get(2)?,
                sequence_no: row.get(3)?,
                chapter_no: row.get(4)?,
                title: row.get(5)?,
                latest_revision_id: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })
        .map_err(|error| format!("查询章节列表失败: {error}"))?;
    rows.map(|row| row.map_err(|error| format!("读取章节失败: {error}")))
        .collect()
}

fn list_lineages(conn: &Connection, work_id: &str) -> Result<Vec<NovelAnalysisLineage>, String> {
    let mut statement = conn
        .prepare(
            "SELECT id, novel_work_id, current_context_revision_id, continuous_through_sequence_no,
                    optimistic_version, status
             FROM novel_analysis_lineages WHERE novel_work_id = ? ORDER BY created_at, id",
        )
        .map_err(|error| format!("准备工作线列表失败: {error}"))?;
    let rows = statement
        .query_map(params![work_id], |row| {
            Ok(NovelAnalysisLineage {
                id: row.get(0)?,
                novel_work_id: row.get(1)?,
                current_context_revision_id: row.get(2)?,
                continuous_through_sequence_no: row.get(3)?,
                optimistic_version: row.get(4)?,
                status: row.get(5)?,
            })
        })
        .map_err(|error| format!("查询工作线列表失败: {error}"))?;
    rows.map(|row| row.map_err(|error| format!("读取工作线失败: {error}")))
        .collect()
}

fn list_revisions(conn: &Connection, work_id: &str) -> Result<Vec<NovelChapterRevision>, String> {
    let mut statement = conn
        .prepare(
            "SELECT revision.id, chapter.novel_work_id, revision.novel_chapter_id, revision.version,
                    revision.content, revision.content_hash, revision.asset_id,
                    revision.requested_parent_context_revision_id, revision.source_kind, revision.created_at,
                    (SELECT run.status FROM source_analysis_runs run WHERE run.novel_chapter_revision_id=revision.id ORDER BY run.created_at DESC,run.id DESC LIMIT 1)
             FROM novel_chapter_revisions revision
             JOIN novel_chapters chapter ON chapter.id = revision.novel_chapter_id
             WHERE chapter.novel_work_id = ? ORDER BY chapter.sequence_no, revision.version, revision.id",
        )
        .map_err(|error| format!("准备正文版本列表失败: {error}"))?;
    let rows = statement
        .query_map(params![work_id], |row| {
            Ok(NovelChapterRevision {
                id: row.get(0)?,
                novel_work_id: row.get(1)?,
                chapter_id: row.get(2)?,
                revision_no: row.get(3)?,
                content: row.get(4)?,
                content_hash: row.get(5)?,
                asset_id: row.get(6)?,
                requested_parent_context_revision_id: row.get(7)?,
                source_kind: row.get(8)?,
                created_at: row.get(9)?,
                analysis_status: row.get(10)?,
            })
        })
        .map_err(|error| format!("查询正文版本列表失败: {error}"))?;
    rows.map(|row| row.map_err(|error| format!("读取正文版本失败: {error}")))
        .collect()
}

fn list_entities(conn: &Connection, work_id: &str) -> Result<Vec<NovelEntity>, String> {
    let mut statement = conn
        .prepare(
            "SELECT id, novel_work_id, entity_kind, stable_key, lifecycle,
                    candidate_lineage_id FROM novel_entities
             WHERE novel_work_id = ? ORDER BY stable_key, id",
        )
        .map_err(|error| format!("准备小说实体列表失败: {error}"))?;
    let rows = statement
        .query_map(params![work_id], |row| {
            let lifecycle: String = row.get(4)?;
            Ok(NovelEntity {
                id: row.get(0)?,
                novel_work_id: row.get(1)?,
                entity_type: row.get(2)?,
                name: row.get(3)?,
                status: lifecycle.clone(),
                facts: Value::Object(Default::default()),
                aliases: Vec::new(),
                provisional: lifecycle == "candidate",
            })
        })
        .map_err(|error| format!("查询小说实体列表失败: {error}"))?;
    rows.map(|row| row.map_err(|error| format!("读取小说实体失败: {error}")))
        .collect()
}

fn snapshot_of(conn: &Connection, work: NovelWork) -> Result<NovelSnapshot, String> {
    let lineages = list_lineages(conn, &work.id)?;
    let main = lineages
        .iter()
        .find(|lineage| lineage.id == work.current_analysis_lineage_id)
        .ok_or("小说缺少主工作线")?;
    let state_through = conn.query_row(
        "SELECT chapter.sequence_no FROM novel_state_versions state
         LEFT JOIN novel_chapter_revisions revision ON revision.id=state.through_novel_chapter_revision_id
         LEFT JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
         WHERE state.id=?",
        params![work.current_novel_state_version_id],
        |row| row.get::<_, Option<i64>>(0),
    ).optional().map_err(|error| format!("读取小说状态推进位置失败: {error}"))?
        .flatten().unwrap_or(0);
    Ok(NovelSnapshot {
        volumes: list_volumes(conn, &work.id)?,
        chapters: list_chapters(conn, &work.id)?,
        revisions: list_revisions(conn, &work.id)?,
        entities: list_entities(conn, &work.id)?,
        context_status: if main.current_context_revision_id.is_some() {
            "ready".into()
        } else {
            "not_started".into()
        },
        current_context_revision_id: main.current_context_revision_id.clone(),
        current_novel_state_through_sequence_no: state_through,
        canonical_version_id: work.published_canon_version_id.clone(),
        lineages,
        work,
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelWorkCreateInput {
    pub project_id: String,
    pub title: String,
    pub description: Option<String>,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelWorkListInput {
    pub project_id: String,
    pub include_archived: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelWorkLookupInput {
    pub project_id: String,
    pub novel_work_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelWorkStatusInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub expected_status: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelVolumeCreateInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub volume_no: i64,
    pub title: Option<String>,
    pub idempotency_key: String,
}

#[tauri::command]
pub fn novel_work_create(
    state: tauri::State<'_, DbState>,
    input: NovelWorkCreateInput,
) -> Result<NovelWork, String> {
    db::with_connection(&state, |conn| novel_work_create_inner(conn, input))
}

pub(crate) fn novel_work_create_inner(
    conn: &Connection,
    input: NovelWorkCreateInput,
) -> Result<NovelWork, String> {
    let project_id = ensure_nonempty(&input.project_id, "projectId")?.to_owned();
    let title = ensure_title(&input.title, "title")?;
    let request =
        serde_json::to_value(&input).map_err(|error| format!("序列化创建小说请求失败: {error}"))?;
    with_receipt(
        conn,
        "novel_work_create",
        &input.idempotency_key,
        &request,
        move |tx| {
            let timestamp = now();
            let work = NovelWork {
                id: new_id("nwork"),
                project_id,
                title,
                description: input.description.unwrap_or_default(),
                status: "active".into(),
                published_canon_version_id: new_id("ncanon"),
                current_novel_state_version_id: new_id("nstate"),
                current_analysis_lineage_id: new_id("nlineage"),
                created_at: timestamp,
                updated_at: timestamp,
            };
            tx.execute(
            "INSERT INTO novel_works (id, project_id, title, description, status, published_canon_version_id,
             current_novel_state_version_id, current_analysis_lineage_id, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?)",
            params![work.id, work.project_id, work.title, work.description, work.status, timestamp, timestamp],
        )
        .map_err(|error| format!("创建小说失败: {error}"))?;
            tx.execute(
                "INSERT INTO novel_canon_versions (id, novel_work_id, version, parent_version_id,
             body_json, rendered_markdown, status, created_at)
             VALUES (?, ?, 0, NULL, '{}', '', 'published', ?)",
                params![work.published_canon_version_id, work.id, timestamp],
            )
            .map_err(|error| format!("创建 Canon C0 失败: {error}"))?;
            tx.execute(
                "INSERT INTO novel_state_versions (id, novel_work_id, version, parent_version_id,
             through_novel_chapter_revision_id, body_json, created_at)
             VALUES (?, ?, 0, NULL, NULL, '{}', ?)",
                params![work.current_novel_state_version_id, work.id, timestamp],
            )
            .map_err(|error| format!("创建 NovelState S0 失败: {error}"))?;
            tx.execute(
                "INSERT INTO novel_analysis_lineages (id, novel_work_id, name,
             base_published_canon_version_id, base_published_novel_state_version_id,
             current_context_revision_id, continuous_through_sequence_no, status,
             optimistic_version, created_at, updated_at)
             VALUES (?, ?, 'main', ?, ?, NULL, 0, 'active', 0, ?, ?)",
                params![
                    work.current_analysis_lineage_id,
                    work.id,
                    work.published_canon_version_id,
                    work.current_novel_state_version_id,
                    timestamp,
                    timestamp
                ],
            )
            .map_err(|error| format!("创建主工作线失败: {error}"))?;
            let changed = tx
                .execute(
                    "UPDATE novel_works SET published_canon_version_id = ?,
                 current_novel_state_version_id = ?, current_analysis_lineage_id = ?
                 WHERE id = ? AND project_id = ?",
                    params![
                        work.published_canon_version_id,
                        work.current_novel_state_version_id,
                        work.current_analysis_lineage_id,
                        work.id,
                        work.project_id
                    ],
                )
                .map_err(|error| format!("设置小说初始 head 失败: {error}"))?;
            if changed != 1 {
                return Err("创建小说时未能设置初始 head".into());
            }
            Ok(work)
        },
    )
}

#[tauri::command]
pub fn novel_work_list(
    state: tauri::State<'_, DbState>,
    input: NovelWorkListInput,
) -> Result<Vec<NovelWork>, String> {
    db::with_connection(&state, |conn| {
        let project_id = ensure_nonempty(&input.project_id, "projectId")?;
        let sql = if input.include_archived.unwrap_or(false) {
            "SELECT id, project_id, title, description, status, published_canon_version_id,
                    current_novel_state_version_id, current_analysis_lineage_id, created_at, updated_at
             FROM novel_works WHERE project_id = ? ORDER BY created_at, id"
        } else {
            "SELECT id, project_id, title, description, status, published_canon_version_id,
                    current_novel_state_version_id, current_analysis_lineage_id, created_at, updated_at
             FROM novel_works WHERE project_id = ? AND status = 'active' ORDER BY created_at, id"
        };
        let mut statement = conn
            .prepare(sql)
            .map_err(|error| format!("准备小说列表失败: {error}"))?;
        let rows = statement
            .query_map(params![project_id], work_from_row)
            .map_err(|error| format!("查询小说列表失败: {error}"))?;
        rows.map(|row| row.map_err(|error| format!("读取小说失败: {error}")))
            .collect()
    })
}

#[tauri::command]
pub fn novel_work_get(
    state: tauri::State<'_, DbState>,
    input: NovelWorkLookupInput,
) -> Result<NovelSnapshot, String> {
    db::with_connection(&state, |conn| {
        snapshot_of(
            conn,
            get_work(conn, &input.project_id, &input.novel_work_id)?,
        )
    })
}

fn novel_work_set_status_inner(
    conn: &Connection,
    input: NovelWorkStatusInput,
    target_status: &'static str,
) -> Result<NovelWork, String> {
    let required_status = if target_status == "archived" {
        "active"
    } else {
        "archived"
    };
    if input.expected_status != required_status {
        return Err("expectedStatus 无效".into());
    }
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    let request = serde_json::to_value(&input).map_err(|error| error.to_string())?;
    let command = if target_status == "archived" {
        "novel_work_archive"
    } else {
        "novel_work_restore"
    };
    with_receipt(conn, command, &input.idempotency_key, &request, move |tx| {
        if work.status != input.expected_status {
            return Err("小说状态已变化，请刷新后重试".into());
        }
        if target_status == "archived" {
            let running: bool = tx.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM source_analysis_runs run
                   JOIN novel_analysis_lineages lineage ON lineage.id=run.novel_analysis_lineage_id
                   WHERE lineage.novel_work_id=? AND run.status='running'
                   UNION ALL
                   SELECT 1 FROM novel_artifact_optimization_runs WHERE novel_work_id=? AND status='running'
                 )",
                params![work.id, work.id], |row| row.get(0),
            ).map_err(|error| format!("检查运行中的小说任务失败: {error}"))?;
            if running {
                return Err("存在运行中的分析或优化任务，请先完成或恢复后再归档".into());
            }
        }
        let changed = tx.execute(
            "UPDATE novel_works SET status=?,updated_at=? WHERE id=? AND project_id=? AND status=?",
            params![target_status, now(), work.id, work.project_id, input.expected_status],
        ).map_err(|error| format!("更新小说状态失败: {error}"))?;
        if changed != 1 {
            return Err("小说状态已变化，请刷新后重试".into());
        }
        let mut result = work.clone();
        result.status = target_status.into();
        result.updated_at = now();
        Ok(result)
    })
}

#[tauri::command]
pub fn novel_work_archive(
    state: tauri::State<'_, DbState>,
    input: NovelWorkStatusInput,
) -> Result<NovelWork, String> {
    db::with_connection(&state, |conn| {
        novel_work_set_status_inner(conn, input, "archived")
    })
}

#[tauri::command]
pub fn novel_work_restore(
    state: tauri::State<'_, DbState>,
    input: NovelWorkStatusInput,
) -> Result<NovelWork, String> {
    db::with_connection(&state, |conn| {
        novel_work_set_status_inner(conn, input, "active")
    })
}

fn novel_volume_create_inner(
    conn: &Connection,
    input: NovelVolumeCreateInput,
) -> Result<NovelVolume, String> {
    if input.volume_no <= 0 {
        return Err("volumeNo 必须是正整数".into());
    }
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let title = input
        .title
        .as_deref()
        .map(|value| ensure_title(value, "title"))
        .transpose()?;
    let request = serde_json::to_value(&input).map_err(|error| error.to_string())?;
    with_receipt(
        conn,
        "novel_volume_create",
        &input.idempotency_key,
        &request,
        move |tx| {
            let active: bool = tx
                .query_row(
                    "SELECT status='active' FROM novel_works WHERE id=? AND project_id=?",
                    params![work.id, work.project_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
                .unwrap_or(false);
            if !active {
                return Err("小说已归档，恢复后才能修改".into());
            }
            let volume = NovelVolume {
                id: new_id("nvolume"),
                novel_work_id: work.id.clone(),
                volume_no: input.volume_no,
                title,
                created_at: now(),
            };
            tx.execute("INSERT INTO novel_volumes (id,novel_work_id,volume_no,title,created_at) VALUES (?,?,?,?,?)",params![volume.id,volume.novel_work_id,volume.volume_no,volume.title,volume.created_at]).map_err(|error| {
            if error.to_string().contains("UNIQUE") { "卷号已存在于当前小说".into() } else { format!("创建卷失败: {error}") }
        })?;
            Ok(volume)
        },
    )
}

#[tauri::command]
pub fn novel_volume_create(
    state: tauri::State<'_, DbState>,
    input: NovelVolumeCreateInput,
) -> Result<NovelVolume, String> {
    db::with_connection(&state, |conn| novel_volume_create_inner(conn, input))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelChapterRevisionCreateInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub chapter_id: Option<String>,
    pub volume_id: Option<String>,
    pub sequence_no: Option<i64>,
    pub chapter_no: Option<i64>,
    pub title: Option<String>,
    pub content: String,
    pub parent_context_revision_id: Option<String>,
    pub asset_id: Option<String>,
    pub source_kind: Option<String>,
    pub idempotency_key: String,
}

fn ensure_asset_in_project(
    conn: &Connection,
    asset_id: &str,
    project_id: &str,
) -> Result<(), String> {
    let metadata: Option<String> = conn
        .query_row(
            "SELECT metadata FROM assets WHERE id = ?",
            params![asset_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("读取正文资产失败: {error}"))?;
    let metadata = metadata.ok_or_else(|| "正文资产不存在".to_string())?;
    let belongs = serde_json::from_str::<Value>(&metadata)
        .ok()
        .and_then(|value| {
            value
                .get("projectId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|owner| owner == project_id);
    if !belongs {
        return Err("正文资产不属于当前项目".into());
    }
    Ok(())
}

fn chapter_in_work(
    conn: &Connection,
    chapter_id: &str,
    work_id: &str,
) -> Result<NovelChapter, String> {
    conn.query_row(
        "SELECT id, novel_work_id, volume_id, sequence_no, chapter_no, title,
                current_revision_id, created_at, updated_at
         FROM novel_chapters WHERE id = ? AND novel_work_id = ?",
        params![chapter_id, work_id],
        |row| {
            Ok(NovelChapter {
                id: row.get(0)?,
                novel_work_id: row.get(1)?,
                volume_id: row.get(2)?,
                sequence_no: row.get(3)?,
                chapter_no: row.get(4)?,
                title: row.get(5)?,
                latest_revision_id: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        },
    )
    .optional()
    .map_err(|error| format!("读取章节失败: {error}"))?
    .ok_or_else(|| "章节不存在或不属于当前小说".into())
}

#[tauri::command]
pub fn novel_chapter_revision_create(
    state: tauri::State<'_, DbState>,
    input: NovelChapterRevisionCreateInput,
) -> Result<NovelChapterRevision, String> {
    db::with_connection(&state, |conn| {
        novel_chapter_revision_create_inner(conn, input)
    })
}

fn novel_chapter_revision_create_inner(
    conn: &Connection,
    input: NovelChapterRevisionCreateInput,
) -> Result<NovelChapterRevision, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let content = normalize_content(&input.content)?;
    let source_kind = input.source_kind.clone().unwrap_or_else(|| "paste".into());
    if !matches!(source_kind.as_str(), "paste" | "asset" | "legacy_import") {
        return Err("sourceKind 无效".into());
    }
    if source_kind == "asset" && input.asset_id.is_none() {
        return Err("asset sourceKind 必须提供 assetId".into());
    }
    if let Some(asset_id) = &input.asset_id {
        ensure_asset_in_project(conn, asset_id, &work.project_id)?;
    }
    if let Some(parent_context_id) = &input.parent_context_revision_id {
        let belongs: bool = conn
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM novel_chapter_context_revisions context
                   JOIN novel_analysis_lineages lineage ON lineage.id = context.novel_analysis_lineage_id
                   WHERE context.id = ? AND lineage.novel_work_id = ?
                 )",
                params![parent_context_id, work.id],
                |row| row.get(0),
            )
            .map_err(|error| format!("验证冻结父上下文失败: {error}"))?;
        if !belongs {
            return Err("冻结父上下文不存在或不属于当前小说".into());
        }
    }
    let request =
        serde_json::to_value(&input).map_err(|error| format!("序列化正文版本请求失败: {error}"))?;
    with_receipt(
        conn,
        "novel_chapter_revision_create",
        &input.idempotency_key,
        &request,
        move |tx| {
            ensure_active_work_in_tx(tx, &work)?;
            let timestamp = now();
            let chapter = if let Some(chapter_id) = input.chapter_id.as_deref() {
                let mut chapter = chapter_in_work(tx, chapter_id, &work.id)?;
                if let Some(title) = input.title.as_deref() {
                    chapter.title = Some(ensure_title(title, "title")?);
                }
                chapter
            } else {
                let chapter_no = input.chapter_no.ok_or("新章节必须提供 chapterNo")?;
                let sequence_no = input.sequence_no.unwrap_or(chapter_no);
                if sequence_no <= 0 || chapter_no <= 0 {
                    return Err("sequenceNo 和 chapterNo 必须是正整数".into());
                }
                if let Some(volume_id) = input.volume_id.as_deref() {
                    let exists: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM novel_volumes WHERE id = ? AND novel_work_id = ?)",
                            params![volume_id, work.id],
                            |row| row.get(0),
                        )
                        .map_err(|error| format!("验证卷归属失败: {error}"))?;
                    if !exists {
                        return Err("卷不存在或不属于当前小说".into());
                    }
                }
                let chapter = NovelChapter {
                    id: new_id("nchapter"),
                    novel_work_id: work.id.clone(),
                    volume_id: input.volume_id.clone(),
                    sequence_no,
                    chapter_no,
                    title: input
                        .title
                        .as_deref()
                        .map(|title| ensure_title(title, "title"))
                        .transpose()?,
                    latest_revision_id: None,
                    created_at: timestamp,
                    updated_at: timestamp,
                };
                tx.execute(
                    "INSERT INTO novel_chapters (id, novel_work_id, volume_id, sequence_no, chapter_no,
                     title, current_revision_id, created_at, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)",
                    params![
                        chapter.id,
                        chapter.novel_work_id,
                        chapter.volume_id,
                        chapter.sequence_no,
                        chapter.chapter_no,
                        chapter.title,
                        timestamp,
                        timestamp
                    ],
                )
                .map_err(|error| format!("创建章节失败: {error}"))?;
                chapter
            };
            let version: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(version), 0) + 1 FROM novel_chapter_revisions WHERE novel_chapter_id = ?",
                    params![chapter.id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("读取正文版本号失败: {error}"))?;
            let revision = NovelChapterRevision {
                id: new_id("nrevision"),
                novel_work_id: work.id.clone(),
                chapter_id: chapter.id.clone(),
                revision_no: version,
                content: content.clone(),
                content_hash: format!("sha256:{:x}", Sha256::digest(content.as_bytes())),
                asset_id: input.asset_id.clone(),
                requested_parent_context_revision_id: input.parent_context_revision_id.clone(),
                source_kind: source_kind.clone(),
                analysis_status: Some("saved".into()),
                created_at: timestamp,
            };
            tx.execute(
                "INSERT INTO novel_chapter_revisions (id, novel_chapter_id, version, content, content_hash,
                 asset_id, requested_parent_context_revision_id, source_kind, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    revision.id,
                    revision.chapter_id,
                    revision.revision_no,
                    revision.content,
                    revision.content_hash,
                    revision.asset_id,
                    revision.requested_parent_context_revision_id,
                    revision.source_kind,
                    revision.created_at
                ],
            )
            .map_err(|error| format!("创建正文版本失败: {error}"))?;
            let changed = tx
                .execute(
                    "UPDATE novel_chapters SET current_revision_id = ?, title = ?, updated_at = ?
                     WHERE id = ? AND novel_work_id = ?",
                    params![revision.id, chapter.title, timestamp, chapter.id, work.id],
                )
                .map_err(|error| format!("更新章节当前版本失败: {error}"))?;
            if changed != 1 {
                return Err("创建正文版本时章节归属发生变化".into());
            }
            Ok(revision)
        },
    )
}

#[tauri::command]
pub fn novel_snapshot(
    state: tauri::State<'_, DbState>,
    input: NovelWorkLookupInput,
) -> Result<NovelSnapshot, String> {
    db::with_connection(&state, |conn| {
        snapshot_of(
            conn,
            get_work(conn, &input.project_id, &input.novel_work_id)?,
        )
    })
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelContextListInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub lineage_id: String,
    pub limit: Option<i64>,
}

#[tauri::command]
pub fn novel_context_list(
    state: tauri::State<'_, DbState>,
    input: NovelContextListInput,
) -> Result<Vec<NovelChapterContext>, String> {
    db::with_connection(&state, |conn| novel_context_list_inner(conn, &input))
}

fn novel_context_list_inner(
    conn: &Connection,
    input: &NovelContextListInput,
) -> Result<Vec<NovelChapterContext>, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    let lineage_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM novel_analysis_lineages WHERE id = ? AND novel_work_id = ?)",
            params![input.lineage_id, work.id],
            |row| row.get(0),
        )
        .map_err(|error| format!("验证工作线归属失败: {error}"))?;
    if !lineage_exists {
        return Err("工作线不存在或不属于当前小说".into());
    }
    let limit = input.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return Err("limit 必须在 1 到 500 之间".into());
    }
    let mut statement = conn
        .prepare(
            "SELECT id, novel_analysis_lineage_id, novel_chapter_revision_id, parent_context_revision_id,
                    source_analysis_run_id, resolved_working_canon_hash, resolved_working_state_hash,
                    sequence_gap_json, branch_kind, status, created_at
             FROM novel_chapter_context_revisions
             WHERE novel_analysis_lineage_id = ? ORDER BY created_at, id LIMIT ?",
        )
        .map_err(|error| format!("准备工作上下文列表失败: {error}"))?;
    let rows = statement
        .query_map(params![input.lineage_id, limit], |row| {
            Ok(NovelChapterContext {
                id: row.get(0)?,
                novel_analysis_lineage_id: row.get(1)?,
                novel_chapter_revision_id: row.get(2)?,
                parent_context_revision_id: row.get(3)?,
                source_analysis_run_id: row.get(4)?,
                resolved_working_canon_hash: row.get(5)?,
                resolved_working_state_hash: row.get(6)?,
                sequence_gap: json_value(row.get(7)?),
                branch_kind: row.get(8)?,
                status: row.get(9)?,
                created_at: row.get(10)?,
            })
        })
        .map_err(|error| format!("查询工作上下文列表失败: {error}"))?;
    rows.map(|row| row.map_err(|error| format!("读取工作上下文失败: {error}")))
        .collect()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelAnalysisStartInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub chapter_revision_id: String,
    pub parent_context_revision_id: Option<String>,
    pub idempotency_key: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelAnalysisRun {
    pub id: String,
    pub novel_work_id: String,
    pub chapter_revision_id: String,
    pub lineage_id: String,
    pub parent_context_revision_id: Option<String>,
    pub status: String,
    pub attempt_no: i64,
    pub safe_error: Option<String>,
}

fn safe_error(error: &str) -> String {
    crate::logging::error_text(error)
        .chars()
        .take(800)
        .collect()
}

fn parse_model_json(raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    let raw = raw
        .strip_prefix("```json")
        .or_else(|| raw.strip_prefix("```JSON"))
        .unwrap_or(raw);
    let raw = raw.strip_suffix("```").unwrap_or(raw).trim();
    let raw = if let Some(end) = raw.rfind("</think>") {
        &raw[end + 8..]
    } else {
        raw
    };
    serde_json::from_str(raw.trim()).map_err(|_| "模型未返回合法 JSON".into())
}

#[derive(Clone)]
struct AnalysisPromptInput {
    chapter_revision_id: String,
    chapter_content: String,
    chapter_hash: String,
    parent_context_id: Option<String>,
    parent_canon_json: Value,
    parent_state_json: Value,
    base_canon_version_id: String,
    base_state_version_id: String,
    model_id: String,
    novel_work_id: String,
    frozen_adaptation_id: String,
    frozen_comic_chapter_id: String,
}

fn analysis_prompt_input(conn: &Connection, run_id: &str) -> Result<AnalysisPromptInput, String> {
    let row = conn.query_row(
        "SELECT revision.id, revision.content, revision.content_hash, run.base_working_context_revision_id,
                COALESCE(context.resolved_working_canon_json, '{}'),
                COALESCE(context.resolved_working_state_json, '{}'),
                run.base_canon_version_id, run.base_novel_state_version_id, run.model_id, lineage.novel_work_id, run.frozen_comic_adaptation_id, run.frozen_comic_chapter_id
         FROM source_analysis_runs run
         JOIN novel_chapter_revisions revision ON revision.id=run.novel_chapter_revision_id
         JOIN novel_analysis_lineages lineage ON lineage.id=run.novel_analysis_lineage_id
         LEFT JOIN novel_chapter_context_revisions context ON context.id=run.base_working_context_revision_id
         WHERE run.id=? AND run.status='running'",
        params![run_id],
        |row| Ok(AnalysisPromptInput {
            chapter_revision_id: row.get(0)?, chapter_content: row.get(1)?, chapter_hash: row.get(2)?, parent_context_id: row.get(3)?, parent_canon_json: json_value(row.get(4)?), parent_state_json: json_value(row.get(5)?), base_canon_version_id: row.get(6)?, base_state_version_id: row.get(7)?, model_id: row.get(8)?, novel_work_id:row.get(9)?, frozen_adaptation_id:row.get(10)?, frozen_comic_chapter_id:row.get(11)?,
        }),
    ).optional().map_err(|error| format!("读取冻结分析输入失败: {error}"))?
     .ok_or_else(|| String::from("分析 run 不在运行状态"))?;
    let prompt_size = row.chapter_content.len()
        + row.parent_canon_json.to_string().len()
        + row.parent_state_json.to_string().len();
    if prompt_size > MAX_ANALYSIS_PROMPT_BYTES {
        return Err("章节与继承上下文超过分析输入上限".into());
    }
    Ok(row)
}

fn validate_analysis_output_for_scope(
    value: &Value,
    scope: Option<(&str, &str, &str, &str)>,
) -> Result<Vec<(String, Value)>, String> {
    let items = value
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or("模型结果缺少 artifacts 数组")?;
    if items.len() != REQUIRED_ARTIFACT_TYPES.len() {
        return Err("模型结果必须包含 14 种且仅一种产物".into());
    }
    let mut output = Vec::new();
    for item in items {
        if item.get("schemaVersion").and_then(Value::as_str) != Some("novel-analysis.v1") {
            return Err("产物 schemaVersion 无效".into());
        }
        let owner = item
            .get("owner")
            .and_then(Value::as_object)
            .ok_or("产物缺少 owner")?;
        let kind = kind_hint(item)?;
        let expected_type = expected_owner_type(kind);
        let expected_id = scope.map(
            |(work, chapter, adaptation, comic_chapter)| match expected_type {
                "novel_work" => work,
                "novel_chapter_revision" => chapter,
                "comic_adaptation" => adaptation,
                _ => comic_chapter,
            },
        );
        if owner.get("ownerType").and_then(Value::as_str) != Some(expected_type)
            || owner
                .get("ownerId")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            || expected_id
                .is_some_and(|id| owner.get("ownerId").and_then(Value::as_str) != Some(id))
        {
            return Err("产物 owner 无效".into());
        }
        let kind = item
            .get("artifactType")
            .and_then(Value::as_str)
            .ok_or("产物缺少 artifactType")?;
        if !REQUIRED_ARTIFACT_TYPES.contains(&kind) || output.iter().any(|(known, _)| known == kind)
        {
            return Err("模型产物类型不完整或重复".into());
        }
        let content = item
            .get("content")
            .filter(|content| content.is_object())
            .ok_or("产物 content 必须是对象")?
            .clone();
        if !item.get("warnings").is_some_and(Value::is_array) {
            return Err("产物 warnings 必须是数组".into());
        }
        validate_artifact_content(kind, &content)?;
        if matches!(
            kind,
            "world_facts" | "character_facts" | "faction_facts" | "location_facts" | "prop_facts"
        ) && contains_dynamic_fact_key(&content)
        {
            return Err("事实类产物不得写入动态状态字段".into());
        }
        output.push((kind.to_owned(), content));
    }
    if output.len() != REQUIRED_ARTIFACT_TYPES.len() {
        return Err("模型结果缺少必需产物".into());
    }
    Ok(output)
}
#[cfg(test)]
fn validate_analysis_output(value: &Value) -> Result<Vec<(String, Value)>, String> {
    validate_analysis_output_for_scope(value, None)
}

fn kind_hint(item: &Value) -> Result<&str, String> {
    item.get("artifactType")
        .and_then(Value::as_str)
        .ok_or_else(|| "产物缺少 artifactType".into())
}
fn expected_owner_type(kind: &str) -> &'static str {
    match kind {
        "world_facts" | "character_facts" | "faction_facts" | "location_facts" | "prop_facts" => {
            "novel_work"
        }
        "adaptation_proposal" => "comic_adaptation",
        "comic_chapter_plan" | "scene_plan" | "page_panel_plan" => "comic_chapter",
        _ => "novel_chapter_revision",
    }
}
fn validate_artifact_content(kind: &str, content: &Value) -> Result<(), String> {
    let required: &[&str] = match kind {
        "chapter_summary" => &["summary", "keyEvents"],
        "chapter_beats" => &["beats"],
        "world_facts" | "character_facts" | "faction_facts" | "location_facts" | "prop_facts" => {
            &["items"]
        }
        "timeline_delta" => &["events"],
        "continuity_delta" => &["changes"],
        "open_threads" => &["threads"],
        "adaptation_proposal" => &["decisions"],
        "comic_chapter_plan" => &["chapters"],
        "scene_plan" => &["comicChapterDraftId", "scenes"],
        "page_panel_plan" => &["comicChapterDraftId", "pages"],
        _ => &[],
    };
    for key in required {
        if content.get(*key).is_none() {
            return Err(format!("{kind} 缺少必填字段 {key}"));
        }
    }
    for key in [
        "keyEvents",
        "beats",
        "items",
        "events",
        "changes",
        "threads",
        "decisions",
        "chapters",
        "scenes",
        "pages",
    ] {
        if let Some(value) = content.get(key) {
            if !value.is_array() {
                return Err(format!("{kind}.{key} 必须是数组"));
            }
            if value.as_array().is_some_and(Vec::is_empty)
                && content
                    .get("emptyReason")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err(format!("{kind} 空集合必须给出 emptyReason"));
            }
        }
    }
    Ok(())
}

fn contains_dynamic_fact_key(value: &Value) -> bool {
    const DYNAMIC_KEYS: [&str; 6] = [
        "position",
        "injury",
        "clothing",
        "weather",
        "time_of_day",
        "holder",
    ];
    match value {
        Value::Object(values) => values.iter().any(|(key, value)| {
            DYNAMIC_KEYS.contains(&key.to_ascii_lowercase().as_str())
                || contains_dynamic_fact_key(value)
        }),
        Value::Array(values) => values.iter().any(contains_dynamic_fact_key),
        _ => false,
    }
}

fn merge_working(parent: Value, additions: impl Iterator<Item = (String, Value)>) -> Value {
    let mut root = parent.as_object().cloned().unwrap_or_default();
    for (key, value) in additions {
        let merged = match (root.get(&key).and_then(Value::as_object), value.as_object()) {
            (Some(previous), Some(next))
                if ["items", "events", "changes", "threads"]
                    .iter()
                    .any(|field| {
                        previous.get(*field).and_then(Value::as_array).is_some()
                            && next.get(*field).and_then(Value::as_array).is_some()
                    }) =>
            {
                let mut object = previous.clone();
                let field = ["items", "events", "changes", "threads"]
                    .into_iter()
                    .find(|field| {
                        previous.get(*field).and_then(Value::as_array).is_some()
                            && next.get(*field).and_then(Value::as_array).is_some()
                    })
                    .expect("matched collection");
                let mut items = previous
                    .get(field)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for candidate in next
                    .get(field)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let stable_key = if field == "changes" {
                        candidate
                            .get("novelEntityId")
                            .and_then(Value::as_str)
                            .zip(candidate.get("fieldPath").and_then(Value::as_str))
                            .zip(candidate.get("timeNode").and_then(Value::as_str))
                            .map(|((a, b), c)| format!("{a}\u{1f}{b}\u{1f}{c}"))
                    } else {
                        candidate
                            .get("stableKey")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    };
                    if let Some(stable_key) = stable_key {
                        if let Some(existing) = items.iter_mut().find(|item| {
                            if field == "changes" {
                                item.get("novelEntityId")
                                    .and_then(Value::as_str)
                                    .zip(item.get("fieldPath").and_then(Value::as_str))
                                    .zip(item.get("timeNode").and_then(Value::as_str))
                                    .map(|((a, b), c)| format!("{a}\u{1f}{b}\u{1f}{c}"))
                                    == Some(stable_key.clone())
                            } else {
                                item.get("stableKey").and_then(Value::as_str)
                                    == Some(stable_key.as_str())
                            }
                        }) {
                            *existing = candidate.clone();
                        } else {
                            items.push(candidate.clone());
                        }
                    } else {
                        items.push(candidate.clone());
                    }
                }
                object.insert(field.into(), Value::Array(items));
                for (next_field, value) in next {
                    if next_field != field {
                        object.insert(next_field.clone(), value.clone());
                    }
                }
                Value::Object(object)
            }
            _ => value,
        };
        root.insert(key, merged);
    }
    Value::Object(root)
}

fn default_adaptation_scope(
    tx: &Transaction<'_>,
    work_id: &str,
    chapter_revision_id: &str,
    sequence: i64,
) -> Result<(String, String), String> {
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM novel_works WHERE id=?",
            params![work_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let adaptation_id: String=tx.query_row("SELECT id FROM comic_adaptations WHERE novel_work_id=? AND title='默认改编' AND status='active' ORDER BY created_at,id LIMIT 1",params![work_id],|r|r.get(0)).optional().map_err(|e|e.to_string())?.unwrap_or_else(||new_id("nadaptation"));
    tx.execute("INSERT OR IGNORE INTO comic_adaptations (id,project_id,novel_work_id,title,status,config_json,optimistic_version,created_at,updated_at) VALUES (?,?,?,'默认改编','active','{}',0,?,?)",params![adaptation_id,project_id,work_id,now(),now()]).map_err(|e|e.to_string())?;
    let continuity_id = new_id("continuity");
    tx.execute("INSERT OR IGNORE INTO continuity_state_versions (id,comic_adaptation_id,version,parent_version_id,through_comic_chapter_id,body_json,created_at) VALUES (?, ?, 0, NULL, NULL, '{}', ?)", params![continuity_id, adaptation_id, now()]).map_err(|e|e.to_string())?;
    tx.execute("UPDATE comic_adaptations SET current_continuity_version_id=COALESCE(current_continuity_version_id,(SELECT id FROM continuity_state_versions WHERE comic_adaptation_id=? AND version=0)) WHERE id=?",params![adaptation_id,adaptation_id]).map_err(|e|e.to_string())?;
    let chapter_id: String=tx.query_row("SELECT id FROM comic_adaptation_chapters WHERE comic_adaptation_id=? AND novel_chapter_revision_id=?",params![adaptation_id,chapter_revision_id],|r|r.get(0)).optional().map_err(|e|e.to_string())?.unwrap_or_else(||new_id("nadaptationchapter"));
    tx.execute("INSERT INTO comic_adaptation_chapters (id,comic_adaptation_id,novel_chapter_revision_id,sequence_no,created_at) VALUES (?,?,?,?,?) ON CONFLICT(comic_adaptation_id,novel_chapter_revision_id) DO NOTHING",params![chapter_id,adaptation_id,chapter_revision_id,sequence,now()]).map_err(|e|format!("创建改编章节映射失败: {e}"))?;
    Ok((adaptation_id, chapter_id))
}

fn analysis_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NovelAnalysisRun> {
    Ok(NovelAnalysisRun {
        id: row.get(0)?,
        novel_work_id: row.get(1)?,
        chapter_revision_id: row.get(2)?,
        lineage_id: row.get(3)?,
        parent_context_revision_id: row.get(4)?,
        status: row.get(5)?,
        attempt_no: row.get(6)?,
        safe_error: row.get(7)?,
    })
}

#[derive(Clone, Debug)]
struct AnalysisStartOutcome {
    run: NovelAnalysisRun,
    /// This is deliberately process-local. Persisting it in an idempotency
    /// receipt would turn a replay into a second provider submission.
    created: bool,
}

fn analysis_start_outcome_inner(
    conn: &Connection,
    input: NovelAnalysisStartInput,
    configured: bool,
    provider_id: String,
    model_id: String,
    owner: &str,
) -> Result<AnalysisStartOutcome, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let request =
        serde_json::to_value(&input).map_err(|error| format!("序列化分析请求失败: {error}"))?;
    let receipt_key = input.idempotency_key.clone();
    let mut created = false;
    let receipt_run = with_receipt(conn, "novel_analysis_start", &receipt_key, &request, |tx| {
        ensure_active_work_in_tx(tx, &work)?;
        let chapter: (String, i64, String) = tx.query_row(
            "SELECT revision.id, chapter.sequence_no, revision.content_hash FROM novel_chapter_revisions revision JOIN novel_chapters chapter ON chapter.id = revision.novel_chapter_id WHERE revision.id = ? AND chapter.novel_work_id = ?",
            params![input.chapter_revision_id, work.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        ).optional().map_err(|error| format!("读取分析章节失败: {error}"))?.ok_or("章节版本不存在或不属于当前小说")?;
        let lineage: NovelAnalysisLineage = tx.query_row(
            "SELECT id, novel_work_id, current_context_revision_id, continuous_through_sequence_no, optimistic_version, status FROM novel_analysis_lineages WHERE id = ? AND novel_work_id = ?",
            params![work.current_analysis_lineage_id, work.id], |row| Ok(NovelAnalysisLineage { id: row.get(0)?, novel_work_id: row.get(1)?, current_context_revision_id: row.get(2)?, continuous_through_sequence_no: row.get(3)?, optimistic_version: row.get(4)?, status: row.get(5)? })
        ).map_err(|error| format!("读取主工作线失败: {error}"))?;
        let parent = input
            .parent_context_revision_id
            .clone()
            .or_else(|| lineage.current_context_revision_id.clone());
        if parent != lineage.current_context_revision_id {
            return Err("冻结父上下文不是当前主工作线 head".into());
        }
        let (frozen_adaptation_id, frozen_comic_chapter_id) =
            default_adaptation_scope(tx, &work.id, &chapter.0, chapter.1)?;
        let timestamp = now();
        let status = if configured { "running" } else { "error" };
        let run = NovelAnalysisRun {
            id: new_id("narun"),
            novel_work_id: work.id.clone(),
            chapter_revision_id: chapter.0.clone(),
            lineage_id: lineage.id.clone(),
            parent_context_revision_id: parent.clone(),
            status: status.into(),
            attempt_no: 1,
            safe_error: (!configured).then(|| "NOT_CONFIGURED".into()),
        };
        let fingerprint = request_hash(
            &json!({"chapterHash":chapter.2,"parent":parent,"canon":work.published_canon_version_id,"state":work.current_novel_state_version_id,"provider":provider_id,"model":model_id,"schemaHash":format!("sha256:{:x}",Sha256::digest(NOVEL_ANALYSIS_SCHEMA.as_bytes()))}),
        )?;
        tx.execute("INSERT INTO source_analysis_runs (id, novel_chapter_revision_id, novel_analysis_lineage_id, base_working_context_revision_id, base_canon_version_id, base_novel_state_version_id, frozen_comic_adaptation_id, frozen_comic_chapter_id, frozen_input_fingerprint, provider_id, model_id, status, prompt_version, schema_version, idempotency_key, progress_json, safe_error_code, safe_user_message, created_at, updated_at, completed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'novel-analysis.v1', 'novel-analysis.v1', ?, '{}', ?, ?, ?, ?, ?)", params![run.id, run.chapter_revision_id, run.lineage_id, run.parent_context_revision_id, work.published_canon_version_id, work.current_novel_state_version_id, frozen_adaptation_id, frozen_comic_chapter_id, fingerprint, provider_id, model_id, run.status, input.idempotency_key, run.safe_error, if configured {Option::<String>::None} else {Some("未配置 LLM，正文已保存，可配置后重试".into())}, timestamp, timestamp, if configured {Option::<i64>::None} else {Some(timestamp)}]).map_err(|error| format!("创建分析 run 失败: {error}"))?;
        tx.execute("INSERT INTO source_analysis_run_attempts (id, source_analysis_run_id, attempt_no, status, lease_owner, lease_expires_at, heartbeat_at, safe_error_code, safe_user_message, created_at, finished_at) VALUES (?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?)", params![new_id("naattempt"), run.id, run.status, if configured {Some(owner)} else {None}, if configured {Some(timestamp + ANALYSIS_LEASE_MS)} else {None}, if configured {Some(timestamp)} else {None}, run.safe_error, if configured {Option::<String>::None} else {Some("未配置 LLM".into())}, timestamp, if configured {Option::<i64>::None} else {Some(timestamp)}]).map_err(|error| format!("创建分析 attempt 失败: {error}"))?;
        created = true;
        Ok(run)
    })?;
    // The receipt records the run as it was first created (normally
    // `running`). A replay must reflect the durable row now, but must never
    // dispatch that row again.
    let run = if created {
        receipt_run
    } else {
        conn.query_row("SELECT run.id,lineage.novel_work_id,run.novel_chapter_revision_id,run.novel_analysis_lineage_id,run.base_working_context_revision_id,run.status,COALESCE((SELECT MAX(attempt_no) FROM source_analysis_run_attempts WHERE source_analysis_run_id=run.id),0),run.safe_error_code FROM source_analysis_runs run JOIN novel_analysis_lineages lineage ON lineage.id=run.novel_analysis_lineage_id WHERE run.id=? AND lineage.novel_work_id=?",params![receipt_run.id,work.id],analysis_run_from_row).optional().map_err(|error|format!("读取幂等分析 run 失败: {error}"))?.ok_or("幂等分析 run 不存在或不属于当前小说")?
    };
    Ok(AnalysisStartOutcome { run, created })
}

/// Compatibility wrapper for tests that only need the durable run. Provider
/// dispatch must use `analysis_start_outcome_inner`.
#[cfg(test)]
fn analysis_start_inner(
    conn: &Connection,
    input: NovelAnalysisStartInput,
    configured: bool,
    provider_id: String,
    model_id: String,
    owner: &str,
) -> Result<NovelAnalysisRun, String> {
    analysis_start_outcome_inner(conn, input, configured, provider_id, model_id, owner)
        .map(|outcome| outcome.run)
}

async fn request_analysis_llm(
    app: &AppState,
    frozen: &AnalysisPromptInput,
) -> Result<Vec<(String, Value)>, String> {
    let cfg = app.cfg.read().map_err(|_| "读取 LLM 配置失败")?.clone();
    if cfg.llm_api_url.trim().is_empty() || cfg.llm_api_key.trim().is_empty() {
        return Err("NOT_CONFIGURED".into());
    }
    let system = format!("你是小说分析器。仅输出一个聚合 JSON：{{\"artifacts\":[...14 items...]}}，不得 Markdown 或解释。artifacts 必须恰有 14 项、每种 artifactType 一项；每一项都须分别符合下列冻结 Schema root oneOf，包含 schemaVersion/owner/content/warnings。不得生成 canon_diff；它只能由已采用 canon delta 确定性派生。\n{}", NOVEL_ANALYSIS_SCHEMA);
    let user = json!({
        "chapterContent": frozen.chapter_content,
        "chapterContentHash": frozen.chapter_hash,
        "parentContextRevisionId": frozen.parent_context_id,
        "basePublishedCanonVersionId": frozen.base_canon_version_id,
        "baseNovelStateVersionId": frozen.base_state_version_id,
        "resolvedWorkingCanon": frozen.parent_canon_json,
        "resolvedWorkingState": frozen.parent_state_json,
        "ownerMap":{"novel_work":frozen.novel_work_id,"novel_chapter_revision":frozen.chapter_revision_id,"comic_adaptation":frozen.frozen_adaptation_id,"comic_chapter":frozen.frozen_comic_chapter_id},
        "instruction": "仅输出 JSON，不要 Markdown 或解释。"
    })
    .to_string();
    let raw = crate::llm::complete_text(
        &completion_endpoint(&cfg.llm_api_url),
        &cfg.llm_api_key,
        &frozen.model_id,
        &system,
        &user,
        "novel.analysis",
    )
    .await
    .map_err(|error| safe_error(&error))?;
    validate_analysis_output_for_scope(
        &parse_model_json(&raw)?,
        Some((
            &frozen.novel_work_id,
            &frozen.chapter_revision_id,
            &frozen.frozen_adaptation_id,
            &frozen.frozen_comic_chapter_id,
        )),
    )
}

#[tauri::command]
pub async fn novel_analysis_start(
    state: tauri::State<'_, DbState>,
    app: tauri::State<'_, AppState>,
    input: NovelAnalysisStartInput,
) -> Result<NovelAnalysisRun, String> {
    let cfg = app.cfg.read().map_err(|_| "读取 LLM 配置失败")?.clone();
    let configured = !cfg.llm_api_url.trim().is_empty() && !cfg.llm_api_key.trim().is_empty();
    let owner = state.app_session_id().to_owned();
    let mut provider = input
        .provider_id
        .clone()
        .unwrap_or_else(|| "configured_llm".into());
    let mut model = input
        .model_id
        .clone()
        .unwrap_or_else(|| cfg.llm_model.clone());
    if provider.trim().eq_ignore_ascii_case("default") {
        provider = "configured_llm".into();
    }
    if model.trim().eq_ignore_ascii_case("default") {
        model = cfg.llm_model.clone();
    }
    let outcome = db::with_connection(&state, |conn| {
        analysis_start_outcome_inner(conn, input, configured, provider, model, &owner)
    })?;
    let mut run = outcome.run;
    if !configured || !outcome.created {
        return Ok(run);
    }
    let frozen = db::with_connection(&state, |conn| analysis_prompt_input(conn, &run.id))?;
    match request_analysis_llm(&app, &frozen).await {
        Ok(artifacts) => {
            run = db::with_connection(&state, |conn| {
                complete_analysis_inner(conn, &run, artifacts, &owner)
            })?;
        }
        Err(error) => {
            run = db::with_connection(&state, |conn| {
                fail_analysis_inner(conn, &run.id, &owner, &error)
            })?;
        }
    }
    Ok(run)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelAnalysisStatusInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub analysis_run_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelAnalysisListInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub chapter_revision_id: Option<String>,
    pub status: Option<String>,
}
#[tauri::command]
pub fn novel_analysis_list(
    state: tauri::State<'_, DbState>,
    input: NovelAnalysisListInput,
) -> Result<Vec<NovelAnalysisRun>, String> {
    db::with_connection(&state, |conn| novel_analysis_list_inner(conn, input))
}
fn novel_analysis_list_inner(
    conn: &Connection,
    input: NovelAnalysisListInput,
) -> Result<Vec<NovelAnalysisRun>, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    let mut statement = conn.prepare("SELECT run.id,lineage.novel_work_id,run.novel_chapter_revision_id,run.novel_analysis_lineage_id,run.base_working_context_revision_id,run.status,COALESCE((SELECT MAX(attempt_no) FROM source_analysis_run_attempts WHERE source_analysis_run_id=run.id),0),run.safe_error_code FROM source_analysis_runs run JOIN novel_analysis_lineages lineage ON lineage.id=run.novel_analysis_lineage_id WHERE lineage.novel_work_id=? AND (? IS NULL OR run.novel_chapter_revision_id=?) AND (? IS NULL OR run.status=?) ORDER BY run.created_at,run.id").map_err(|e|format!("准备分析列表失败: {e}"))?;
    let rows = statement
        .query_map(
            params![
                work.id,
                input.chapter_revision_id,
                input.chapter_revision_id,
                input.status,
                input.status
            ],
            analysis_run_from_row,
        )
        .map_err(|e| format!("查询分析列表失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取分析列表失败: {e}"))?;
    Ok(rows)
}
#[tauri::command]
pub fn novel_analysis_status(
    state: tauri::State<'_, DbState>,
    input: NovelAnalysisStatusInput,
) -> Result<NovelAnalysisRun, String> {
    db::with_connection(&state, |conn| {
        let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
        conn.query_row("SELECT run.id,lineage.novel_work_id,run.novel_chapter_revision_id,run.novel_analysis_lineage_id,run.base_working_context_revision_id,run.status,COALESCE((SELECT MAX(attempt_no) FROM source_analysis_run_attempts WHERE source_analysis_run_id=run.id),0),run.safe_error_code FROM source_analysis_runs run JOIN novel_analysis_lineages lineage ON lineage.id=run.novel_analysis_lineage_id WHERE run.id=? AND lineage.novel_work_id=?",params![input.analysis_run_id,work.id],analysis_run_from_row).optional().map_err(|e|format!("读取分析状态失败: {e}"))?.ok_or_else(||"分析 run 不存在或不属于当前小说".into())
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelAnalysisRetryInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub analysis_run_id: String,
    pub idempotency_key: String,
}
#[tauri::command]
pub async fn novel_analysis_retry(
    state: tauri::State<'_, DbState>,
    app: tauri::State<'_, AppState>,
    input: NovelAnalysisRetryInput,
) -> Result<NovelAnalysisRun, String> {
    let frozen = db::with_connection(&state, |conn| {
        let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
        ensure_active_work(&work)?;
        conn.query_row("SELECT run.novel_chapter_revision_id,run.base_working_context_revision_id,run.provider_id,run.model_id FROM source_analysis_runs run JOIN novel_analysis_lineages lineage ON lineage.id=run.novel_analysis_lineage_id WHERE run.id=? AND lineage.novel_work_id=? AND run.status IN ('error','stale')",params![input.analysis_run_id,work.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|e|e.to_string())?.ok_or_else(||"只有 error 或 stale 分析 run 可以重试".into())
    })?;
    novel_analysis_start(
        state,
        app,
        NovelAnalysisStartInput {
            project_id: input.project_id,
            novel_work_id: input.novel_work_id,
            chapter_revision_id: frozen.0,
            parent_context_revision_id: frozen.1,
            idempotency_key: input.idempotency_key,
            provider_id: Some(frozen.2),
            model_id: Some(frozen.3),
        },
    )
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelAnalysisRecoverInput {
    pub project_id: String,
    pub novel_work_id: String,
}

#[tauri::command]
pub fn novel_analysis_recover_stale(
    state: tauri::State<'_, DbState>,
    input: NovelAnalysisRecoverInput,
) -> Result<i64, String> {
    db::with_connection(&state, |conn| {
        let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(|error| format!("开始分析恢复事务失败: {error}"))?;
        let timestamp = now();
        let changed = tx.execute(
            "UPDATE source_analysis_run_attempts SET status='stale', finished_at=?, safe_error_code='LEASE_EXPIRED', safe_user_message='分析执行租约已过期，可重试'
             WHERE status='running' AND lease_expires_at<? AND source_analysis_run_id IN (
               SELECT run.id FROM source_analysis_runs run JOIN novel_analysis_lineages lineage ON lineage.id=run.novel_analysis_lineage_id WHERE lineage.novel_work_id=?
             )",
            params![timestamp, timestamp, work.id],
        ).map_err(|error| format!("恢复过期分析 attempt 失败: {error}"))?;
        tx.execute(
            "UPDATE source_analysis_runs SET status='stale',safe_error_code='LEASE_EXPIRED',safe_user_message='分析执行租约已过期，可重试',updated_at=?,completed_at=?
             WHERE status='running' AND id IN (SELECT source_analysis_run_id FROM source_analysis_run_attempts WHERE status='stale')",
            params![timestamp, timestamp],
        ).map_err(|error| format!("恢复过期分析 run 失败: {error}"))?;
        tx.commit()
            .map_err(|error| format!("提交分析恢复事务失败: {error}"))?;
        Ok(changed as i64)
    })
}

const PRODUCTION_STAGE_TOTAL: i64 = 8;
const PRODUCTION_LEASE_MS: i64 = 120_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelProductionJob {
    pub id: String,
    pub project_id: String,
    pub novel_work_id: String,
    pub novel_chapter_id: String,
    pub source_revision_id: String,
    pub default_adaptation_id: Option<String>,
    pub source_analysis_run_id: Option<String>,
    pub adaptation_analysis_run_id: Option<String>,
    pub apply_operation_id: Option<String>,
    pub comic_plan_intent: Option<ComicPlanIntent>,
    pub status: String,
    pub stage: String,
    pub stage_index: i64,
    pub stage_total: i64,
    pub completed_artifact_count: i64,
    pub total_artifact_count: i64,
    pub attempt_no: i64,
    pub safe_error_code: Option<String>,
    pub safe_user_message: Option<String>,
    pub next_action: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelProductionStartInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub novel_chapter_id: String,
    pub source_revision_id: String,
    pub idempotency_key: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    #[serde(default)]
    pub visual_output: Option<crate::comic_visual_batch::VisualOutputInput>,
    #[serde(default)]
    pub comic_plan_intent: Option<ComicPlanIntent>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelProductionJobInput {
    pub project_id: String,
    pub novel_work_id: String,
    #[serde(alias = "novelProductionJobId")]
    pub production_job_id: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelProductionGetInput {
    pub project_id: String,
    pub novel_work_id: String,
    #[serde(alias = "novelProductionJobId")]
    pub production_job_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelProductionRetryStageInput {
    pub project_id: String,
    pub novel_work_id: String,
    #[serde(alias = "novelProductionJobId")]
    pub production_job_id: String,
    pub stage: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelProductionListForChapterInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub novel_chapter_id: String,
}

fn production_job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NovelProductionJob> {
    Ok(NovelProductionJob {
        id: row.get(0)?,
        project_id: row.get(1)?,
        novel_work_id: row.get(2)?,
        novel_chapter_id: row.get(3)?,
        source_revision_id: row.get(4)?,
        default_adaptation_id: row.get(5)?,
        source_analysis_run_id: row.get(6)?,
        adaptation_analysis_run_id: row.get(7)?,
        apply_operation_id: row.get(8)?,
        comic_plan_intent: row
            .get::<_, Option<String>>(22)?
            .map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        status: row.get(9)?,
        stage: row.get(10)?,
        stage_index: row.get(11)?,
        stage_total: row.get(12)?,
        completed_artifact_count: row.get(13)?,
        total_artifact_count: row.get(14)?,
        attempt_no: row.get(15)?,
        safe_error_code: row.get(16)?,
        safe_user_message: row.get(17)?,
        next_action: row.get(18)?,
        created_at: row.get(19)?,
        updated_at: row.get(20)?,
        finished_at: row.get(21)?,
    })
}

const PRODUCTION_JOB_SELECT: &str = "SELECT id,project_id,novel_work_id,novel_chapter_id,source_revision_id,default_adaptation_id,source_analysis_run_id,adaptation_analysis_run_id,apply_operation_id,status,stage,stage_index,stage_total,completed_artifact_count,total_artifact_count,attempt_no,safe_error_code,safe_user_message,next_action,created_at,updated_at,finished_at,comic_plan_intent_json FROM novel_production_jobs";

fn production_job_value(
    conn: &Connection,
    project_id: &str,
    work_id: &str,
    job_id: &str,
) -> Result<NovelProductionJob, String> {
    get_work(conn, project_id, work_id)?;
    conn.query_row(
        &format!("{PRODUCTION_JOB_SELECT} WHERE id=? AND project_id=? AND novel_work_id=?"),
        params![job_id, project_id, work_id],
        production_job_from_row,
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "PRODUCTION_JOB_NOT_FOUND".into())
}

/// Narrow backend-only accessor for the visual batch.  It preserves the same
/// exact project/work/job ownership checks as the public production commands.
pub(crate) fn production_job_for_visual(
    conn: &Connection,
    project_id: &str,
    work_id: &str,
    job_id: &str,
) -> Result<NovelProductionJob, String> {
    production_job_value(conn, project_id, work_id, job_id)
}

/// Fixture hook for the visual-batch integration test.  It creates a durable
/// succeeded job over an already-applied real fixture.  Authorization is
/// optional so tests can prove that a legacy succeeded job remains incapable
/// of producing images until a separate explicit authorization occurs.
#[cfg(test)]
pub(crate) fn test_create_succeeded_visual_job(
    conn: &Connection,
    project_id: &str,
    work_id: &str,
    chapter_id: &str,
    source_revision_id: &str,
    adaptation_id: &str,
    apply_operation_id: &str,
    authorization: Option<&Value>,
) -> Result<NovelProductionJob, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "PRODUCTION_TEST_FIXTURE_WRITE_FAILED".to_string())?;
    let timestamp = now();
    let job = NovelProductionJob {
        id: new_id("nprod_test_visual"),
        project_id: project_id.into(),
        novel_work_id: work_id.into(),
        novel_chapter_id: chapter_id.into(),
        source_revision_id: source_revision_id.into(),
        default_adaptation_id: Some(adaptation_id.into()),
        source_analysis_run_id: None,
        adaptation_analysis_run_id: None,
        apply_operation_id: Some(apply_operation_id.into()),
        comic_plan_intent: None,
        status: "succeeded".into(),
        stage: "succeeded".into(),
        stage_index: 8,
        stage_total: PRODUCTION_STAGE_TOTAL,
        completed_artifact_count: 14,
        total_artifact_count: 14,
        attempt_no: 1,
        safe_error_code: None,
        safe_user_message: None,
        next_action: Some("漫画生产树已创建".into()),
        created_at: timestamp,
        updated_at: timestamp,
        finished_at: Some(timestamp),
    };
    tx.execute("INSERT INTO novel_production_jobs(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,default_adaptation_id,source_analysis_run_id,adaptation_analysis_run_id,apply_operation_id,request_hash,idempotency_key,status,stage,stage_index,stage_total,completed_artifact_count,total_artifact_count,attempt_no,lease_owner,lease_expires_at,safe_error_code,safe_user_message,next_action,created_at,updated_at,finished_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",params![job.id,job.project_id,job.novel_work_id,job.novel_chapter_id,job.source_revision_id,job.default_adaptation_id,Option::<String>::None,Option::<String>::None,job.apply_operation_id,"fixture-visual-job",format!("{}:fixture-visual",job.id),job.status,job.stage,job.stage_index,job.stage_total,job.completed_artifact_count,job.total_artifact_count,job.attempt_no,Option::<String>::None,Option::<i64>::None,Option::<String>::None,Option::<String>::None,job.next_action,job.created_at,job.updated_at,job.finished_at]).map_err(|e|format!("PRODUCTION_TEST_FIXTURE_WRITE_FAILED:{e}"))?;
    if let Some(authorization) = authorization {
        crate::comic_visual_batch::authorize_new_job_tx(&tx, &job, authorization)?;
    }
    tx.commit()
        .map_err(|e| format!("PRODUCTION_TEST_FIXTURE_WRITE_FAILED:{e}"))?;
    Ok(job)
}

/// Test-only counterpart of the explicit legacy-job authorize command.  It
/// deliberately runs in a second transaction so the fixture can prove that a
/// pre-v18 succeeded job first reads as un-authorized, then gains permission
/// only after this explicit operation.
#[cfg(test)]
pub(crate) fn test_authorize_succeeded_visual_job(
    conn: &Connection,
    job: &NovelProductionJob,
    authorization: &Value,
) -> Result<(), String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "PRODUCTION_TEST_FIXTURE_WRITE_FAILED".to_string())?;
    crate::comic_visual_batch::authorize_new_job_tx(&tx, job, authorization)?;
    tx.commit()
        .map_err(|_| "PRODUCTION_TEST_FIXTURE_WRITE_FAILED".to_string())
}

fn production_event(
    tx: &Transaction<'_>,
    job: &NovelProductionJob,
    event_type: &str,
    payload: Value,
) -> Result<(), String> {
    let next: i64 = tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM novel_production_job_events WHERE novel_production_job_id=?", params![job.id], |row| row.get(0)).map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO novel_production_job_events(id,novel_production_job_id,seq,event_type,stage,payload_json,created_at) VALUES (?,?,?,?,?,?,?)", params![new_id("nprodevent"),job.id,next,event_type,job.stage,payload.to_string(),now()]).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
fn production_start_inner(
    conn: &Connection,
    input: &NovelProductionStartInput,
) -> Result<NovelProductionJob, String> {
    production_start_with_visual_inner(conn, input, None)
}

fn production_start_with_visual_inner(
    conn: &Connection,
    input: &NovelProductionStartInput,
    authorization: Option<&Value>,
) -> Result<NovelProductionJob, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let comic_plan_intent = canonicalize_comic_plan_intent(input.comic_plan_intent.clone())?;
    // Preserve the pre-v18 request-hash shape for old no-visual calls.  A
    // saved legacy idempotency key must replay its existing text job rather
    // than failing hash comparison or implicitly gaining image permission.
    let mut request = json!({"projectId":input.project_id,"novelWorkId":input.novel_work_id,"novelChapterId":input.novel_chapter_id,"sourceRevisionId":input.source_revision_id,"providerId":input.provider_id,"modelId":input.model_id});
    if let Some(visual_output) = &input.visual_output {
        request["visualOutput"] =
            serde_json::to_value(visual_output).map_err(|_| "VISUAL_OUTPUT_SERIALIZE_FAILED")?;
    }
    if let Some(intent) = &comic_plan_intent {
        request["comicPlanIntent"] =
            serde_json::to_value(intent).map_err(|_| "COMIC_PLAN_INTENT_SERIALIZE_FAILED")?;
    }
    let hash = request_hash(&request)?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    if let Some((stored_hash, job_id)) = tx
        .query_row(
            "SELECT request_hash,id FROM novel_production_jobs WHERE idempotency_key=?",
            params![input.idempotency_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
    {
        if stored_hash != hash {
            return Err("idempotencyKey 已用于不同业务载荷".into());
        }
        let job = tx
            .query_row(
                &format!("{PRODUCTION_JOB_SELECT} WHERE id=?"),
                params![job_id],
                production_job_from_row,
            )
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(job);
    }
    let chapter: (String, i64) = tx.query_row("SELECT chapter.id,chapter.sequence_no FROM novel_chapter_revisions revision JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id WHERE revision.id=? AND chapter.id=? AND chapter.novel_work_id=?",params![input.source_revision_id,input.novel_chapter_id,work.id],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|e|e.to_string())?.ok_or("NOVEL_CHAPTER_REVISION_MISMATCH")?;
    if let Some(job_id) = tx
        .query_row(
            "SELECT id FROM novel_production_jobs WHERE novel_work_id=? AND source_revision_id=?",
            params![work.id, input.source_revision_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
    {
        let job = tx
            .query_row(
                &format!("{PRODUCTION_JOB_SELECT} WHERE id=?"),
                params![job_id],
                production_job_from_row,
            )
            .map_err(|e| e.to_string())?;
        if comic_plan_intent.is_some() && job.comic_plan_intent != comic_plan_intent {
            return Err("COMIC_PLAN_INTENT_CONFLICT".into());
        }
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(job);
    }
    let (adaptation_id, _) =
        default_adaptation_scope(&tx, &work.id, &input.source_revision_id, chapter.1)?;
    let timestamp = now();
    let job = NovelProductionJob {
        id: new_id("nprod"),
        project_id: input.project_id.clone(),
        novel_work_id: work.id.clone(),
        novel_chapter_id: chapter.0,
        source_revision_id: input.source_revision_id.clone(),
        default_adaptation_id: Some(adaptation_id),
        source_analysis_run_id: None,
        adaptation_analysis_run_id: None,
        apply_operation_id: None,
        comic_plan_intent: comic_plan_intent.clone(),
        status: "queued".into(),
        stage: "resolving_inheritance".into(),
        stage_index: 1,
        stage_total: PRODUCTION_STAGE_TOTAL,
        completed_artifact_count: 0,
        total_artifact_count: 14,
        attempt_no: 1,
        safe_error_code: None,
        safe_user_message: None,
        next_action: Some("开始原著分析".into()),
        created_at: timestamp,
        updated_at: timestamp,
        finished_at: None,
    };
    let comic_plan_intent_json = job
        .comic_plan_intent
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| "COMIC_PLAN_INTENT_SERIALIZE_FAILED")?;
    tx.execute("INSERT INTO novel_production_jobs(id,project_id,novel_work_id,novel_chapter_id,source_revision_id,default_adaptation_id,request_hash,idempotency_key,status,stage,stage_index,stage_total,completed_artifact_count,total_artifact_count,attempt_no,lease_owner,lease_expires_at,safe_error_code,safe_user_message,next_action,created_at,updated_at,finished_at,comic_plan_intent_json) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",params![job.id,job.project_id,job.novel_work_id,job.novel_chapter_id,job.source_revision_id,job.default_adaptation_id,hash,input.idempotency_key,job.status,job.stage,job.stage_index,job.stage_total,job.completed_artifact_count,job.total_artifact_count,job.attempt_no,Option::<String>::None,Option::<i64>::None,Option::<String>::None,Option::<String>::None,job.next_action,job.created_at,job.updated_at,Option::<i64>::None,comic_plan_intent_json]).map_err(|e|format!("创建生产任务失败: {e}"))?;
    if let Some(authorization) = authorization {
        crate::comic_visual_batch::authorize_new_job_tx(&tx, &job, authorization)?;
    }
    production_event(
        &tx,
        &job,
        "created",
        json!({"sourceRevisionId":job.source_revision_id,"defaultAdaptationId":job.default_adaptation_id,"comicPlanIntent":&job.comic_plan_intent}),
    )?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(job)
}

fn production_claim_source(
    conn: &Connection,
    job: &NovelProductionJob,
    owner: &str,
) -> Result<bool, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let ts = now();
    let changed=tx.execute("UPDATE novel_production_jobs SET status='running',stage='source_analysis',stage_index=2,lease_owner=?,lease_expires_at=?,safe_error_code=NULL,safe_user_message=NULL,next_action='正在分析正文',updated_at=?,finished_at=NULL WHERE id=? AND project_id=? AND novel_work_id=? AND source_revision_id=? AND attempt_no=? AND status IN ('queued','stale','error','blocked_config') AND (lease_expires_at IS NULL OR lease_expires_at<?)",params![owner,ts+PRODUCTION_LEASE_MS,ts,job.id,job.project_id,job.novel_work_id,job.source_revision_id,job.attempt_no,ts]).map_err(|e|e.to_string())?;
    if changed == 1 {
        let claimed = tx
            .query_row(
                &format!("{PRODUCTION_JOB_SELECT} WHERE id=?"),
                params![job.id],
                production_job_from_row,
            )
            .map_err(|e| e.to_string())?;
        production_event(
            &tx,
            &claimed,
            "source_claimed",
            json!({"attemptNo":claimed.attempt_no}),
        )?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(changed == 1)
}

fn production_finish_source(
    conn: &Connection,
    expected_job: &NovelProductionJob,
    run: &NovelAnalysisRun,
) -> Result<NovelProductionJob, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let ts = now();
    let (status, stage, index, count, code, message, next, finished) = match run.status.as_str() {
        "ready_for_review" => (
            "waiting_for_predecessor",
            "integrating_context",
            3,
            14_i64,
            None,
            None,
            Some("原著产物已生成，正在进入默认改编"),
            None,
        ),
        "error" if run.safe_error.as_deref() == Some("NOT_CONFIGURED") => (
            "blocked_config",
            "source_analysis",
            2,
            0,
            Some("NOT_CONFIGURED"),
            Some("未配置 LLM，正文已保存；配置后可继续。"),
            Some("配置 LLM 后继续"),
            Some(ts),
        ),
        "error" | "stale" => (
            "error",
            "source_analysis",
            2,
            0,
            run.safe_error.as_deref(),
            Some("原著分析失败，可重试本阶段。"),
            Some("重试原著分析"),
            Some(ts),
        ),
        _ => (
            "waiting_for_predecessor",
            "source_analysis",
            2,
            0,
            None,
            None,
            Some("等待原著分析完成"),
            None,
        ),
    };
    let changed = tx.execute("UPDATE novel_production_jobs SET source_analysis_run_id=?,status=?,stage=?,stage_index=?,completed_artifact_count=?,lease_owner=NULL,lease_expires_at=NULL,safe_error_code=?,safe_user_message=?,next_action=?,updated_at=?,finished_at=? WHERE id=? AND project_id=? AND novel_work_id=? AND source_revision_id=? AND attempt_no=? AND status='running'",params![run.id,status,stage,index,count,code,message,next,ts,finished,expected_job.id,expected_job.project_id,expected_job.novel_work_id,expected_job.source_revision_id,expected_job.attempt_no]).map_err(|e|e.to_string())?;
    if changed != 1 {
        tx.commit().map_err(|e| e.to_string())?;
        return Err("PRODUCTION_ATTEMPT_SUPERSEDED".into());
    }
    let result = tx
        .query_row(
            &format!("{PRODUCTION_JOB_SELECT} WHERE id=?"),
            params![expected_job.id],
            production_job_from_row,
        )
        .map_err(|e| e.to_string())?;
    production_event(
        &tx,
        &result,
        "source_finished",
        json!({"analysisRunId":run.id,"analysisStatus":run.status,"safeError":run.safe_error}),
    )?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

/// Production owns no second adoption path.  Every generated candidate is
/// still bound to the existing preview token and CAS-protected adopt command.
fn production_adopt_candidates(
    conn: &Connection,
    project_id: &str,
    work_id: &str,
    where_sql: &str,
    run_id: &str,
    key_prefix: &str,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let expected_change = if where_sql == "artifact.source_analysis_run_id" {
        "ai_analysis"
    } else {
        "ai_adaptation_analysis"
    };
    let mut stmt=conn.prepare(&format!("SELECT artifact.id,artifact.artifact_type,artifact.candidate_head_revision_id,artifact.adopted_head_revision_id,artifact.optimistic_version,candidate.status,candidate.change_type,candidate.parent_revision_id,adopted.status FROM analysis_artifacts artifact LEFT JOIN analysis_artifact_revisions candidate ON candidate.id=artifact.candidate_head_revision_id LEFT JOIN analysis_artifact_revisions adopted ON adopted.id=artifact.adopted_head_revision_id WHERE {where_sql}=? ORDER BY artifact.artifact_type")) .map_err(|e|e.to_string())?;
    let candidates = stmt
        .query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut adopted = std::collections::BTreeMap::new();
    for (
        artifact_id,
        kind,
        candidate,
        adopted_head,
        version,
        candidate_status,
        candidate_change,
        candidate_parent,
        adopted_status,
    ) in candidates
    {
        if let Some(revision) = adopted_head {
            if adopted_status.as_deref() == Some("adopted") {
                adopted.insert(kind, revision);
                continue;
            }
            return Err(format!("PRODUCTION_ADOPTED_HEAD_INVALID:{kind}"));
        }
        let revision = candidate.ok_or_else(|| format!("PRODUCTION_CANDIDATE_MISSING:{kind}"))?;
        if candidate_status.as_deref() != Some("candidate")
            || candidate_change.as_deref() != Some(expected_change)
            || candidate_parent.is_some()
        {
            return Err(format!("PRODUCTION_CANDIDATE_DRIFT:{kind}"));
        }
        let preview = novel_artifact_adopt_preview_inner(
            conn,
            NovelArtifactAdoptPreviewInput {
                project_id: project_id.into(),
                novel_work_id: work_id.into(),
                artifact_id: artifact_id.clone(),
                revision_id: revision.clone(),
                expected_optimistic_version: version,
            },
        )?;
        let adopt_key = format!(
            "{key_prefix}:adopt:{project_id}:{work_id}:{where_sql}:{run_id}:{artifact_id}:{revision}"
        );
        novel_artifact_adopt_inner(
            conn,
            NovelArtifactAdoptInput {
                project_id: project_id.into(),
                novel_work_id: work_id.into(),
                artifact_id,
                revision_id: revision.clone(),
                expected_optimistic_version: version,
                approval_token: preview.approval_token,
                idempotency_key: adopt_key,
            },
        )?;
        adopted.insert(kind, revision);
    }
    Ok(adopted)
}

fn production_update_stage(
    conn: &Connection,
    job_id: &str,
    status: &str,
    stage: &str,
    index: i64,
    completed: i64,
    next: Option<&str>,
    error: Option<(&str, &str)>,
) -> Result<NovelProductionJob, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let (previous_status, previous_stage): (String, String) = tx
        .query_row(
            "SELECT status,stage FROM novel_production_jobs WHERE id=?",
            params![job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or("PRODUCTION_JOB_NOT_FOUND")?;
    if !production_transition_allowed(&previous_status, &previous_stage, status, stage) {
        return Err("PRODUCTION_INVALID_TRANSITION".into());
    }
    let ts = now();
    let finished = matches!(
        status,
        "succeeded" | "error" | "blocked_config" | "blocked_conflict" | "needs_rebase" | "stale"
    )
    .then_some(ts);
    tx.execute("UPDATE novel_production_jobs SET status=?,stage=?,stage_index=?,completed_artifact_count=?,lease_owner=NULL,lease_expires_at=NULL,safe_error_code=?,safe_user_message=?,next_action=?,updated_at=?,finished_at=? WHERE id=?",params![status,stage,index,completed,error.map(|v|v.0),error.map(|v|v.1),next,ts,finished,job_id]).map_err(|e|e.to_string())?;
    let job = tx
        .query_row(
            &format!("{PRODUCTION_JOB_SELECT} WHERE id=?"),
            params![job_id],
            production_job_from_row,
        )
        .map_err(|e| e.to_string())?;
    production_event(
        &tx,
        &job,
        "stage_changed",
        json!({"status":status,"stage":stage,"completedArtifactCount":completed,"safeErrorCode":error.map(|v|v.0)}),
    )?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(job)
}

fn production_transition_allowed(
    from_status: &str,
    from_stage: &str,
    to_status: &str,
    to_stage: &str,
) -> bool {
    if from_status == to_status && from_stage == to_stage {
        return true;
    }
    if from_status == "succeeded" {
        return false;
    }
    let status_ok = matches!(
        (from_status, to_status),
        ("queued", "running")
            | (
                "running",
                "running"
                    | "waiting_for_predecessor"
                    | "blocked_config"
                    | "blocked_conflict"
                    | "needs_rebase"
                    | "stale"
                    | "error"
                    | "succeeded"
            )
            | (
                "waiting_for_predecessor",
                "running" | "blocked_config" | "blocked_conflict" | "needs_rebase" | "error"
            )
            | (
                "blocked_config" | "blocked_conflict" | "needs_rebase" | "stale" | "error",
                "queued" | "running"
            )
    );
    let stage_ok = to_stage == "succeeded"
        || matches!(
            to_stage,
            "resolving_inheritance"
                | "source_analysis"
                | "integrating_context"
                | "ensuring_adaptation"
                | "adaptation_analysis"
                | "applying_production"
        ) && (to_stage == from_stage
            || production_stage_rank(to_stage) >= production_stage_rank(from_stage));
    status_ok && stage_ok
}

fn production_stage_rank(stage: &str) -> i64 {
    match stage {
        "saving_source" => 0,
        "resolving_inheritance" => 1,
        "source_analysis" => 2,
        "integrating_context" => 3,
        "ensuring_adaptation" => 4,
        "adaptation_analysis" => 5,
        "applying_production" => 7,
        "succeeded" => 8,
        _ => -1,
    }
}

fn production_adaptation_baselines(
    conn: &Connection,
    job: &NovelProductionJob,
) -> Result<(String, String, String), String> {
    conn.query_row("SELECT work.published_canon_version_id,work.current_novel_state_version_id,adaptation.current_continuity_version_id FROM novel_works work JOIN comic_adaptations adaptation ON adaptation.id=? WHERE work.id=? AND work.project_id=?",params![job.default_adaptation_id,job.novel_work_id,job.project_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(|e|e.to_string())?.ok_or_else(||"PRODUCTION_BASELINE_MISSING".into())
}

fn production_current_context(
    conn: &Connection,
    source_run_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row("SELECT id FROM novel_chapter_context_revisions WHERE source_analysis_run_id=? ORDER BY created_at DESC,id DESC LIMIT 1",params![source_run_id],|row|row.get(0)).optional().map_err(|e|e.to_string())
}

fn production_scene_context_resolve_input(
    job: &NovelProductionJob,
    adaptation_id: &str,
    scene_plan_revision_id: &str,
    planning_scene_stable_key: &str,
    working_context_revision_id: Option<String>,
    canon_version_id: &str,
    novel_state_version_id: &str,
    continuity_version_id: &str,
    adaptation_proposal_revision_id: &str,
    selected_entity_ids: Vec<String>,
    visual_card_revision_ids: Vec<String>,
) -> crate::novel_adaptation::SceneContextResolveInput {
    crate::novel_adaptation::SceneContextResolveInput {
        project_id: job.project_id.clone(),
        novel_work_id: job.novel_work_id.clone(),
        comic_adaptation_id: adaptation_id.into(),
        scene_plan_revision_id: scene_plan_revision_id.into(),
        planning_scene_stable_key: planning_scene_stable_key.into(),
        working_context_revision_id,
        canon_version_id: canon_version_id.into(),
        novel_state_version_id: novel_state_version_id.into(),
        continuity_version_id: Some(continuity_version_id.into()),
        adaptation_plan_revision_id: adaptation_proposal_revision_id.into(),
        selected_entity_ids,
        visual_card_revision_ids,
        idempotency_key: format!("{}:scene:{}", job.id, planning_scene_stable_key),
    }
}

/// Reconcile a ready run before asking a provider for another adaptation. The
/// job-owned pointer plus frozen source/adaptation scope is the replay key.
fn production_source_input_revisions(
    source_revisions: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    PRODUCTION_SOURCE_ARTIFACT_TYPES
        .iter()
        .map(|kind| {
            source_revisions
                .get(*kind)
                .cloned()
                .ok_or_else(|| format!("SOURCE_ARTIFACT_MISSING:{kind}"))
        })
        .collect()
}

/// The default production path uses `artifact_revisions`, so an adaptation
/// run intentionally has a NULL `source_analysis_run_id`.  Its ownership is
/// instead proved by its frozen ten input revisions: each must be the current
/// adopted revision from this exact source run.  Keep this predicate shared by
/// normal replay and crash recovery; a nullable run column is never evidence
/// that the run is unrelated.
fn production_adaptation_run_inputs_match(
    conn: &Connection,
    run_id: &str,
    source_run_id: &str,
    current_source_revisions: &[String],
) -> Result<bool, String> {
    if current_source_revisions.len() != PRODUCTION_SOURCE_ARTIFACT_TYPES.len() {
        return Ok(false);
    }
    let mut stmt=conn.prepare("SELECT input.artifact_type,input.analysis_artifact_revision_id,artifact.source_analysis_run_id,artifact.adopted_head_revision_id,revision.status FROM adaptation_analysis_run_inputs input JOIN analysis_artifact_revisions revision ON revision.id=input.analysis_artifact_revision_id JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id WHERE input.adaptation_analysis_run_id=? ORDER BY input.source_order").map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows.len() == PRODUCTION_SOURCE_ARTIFACT_TYPES.len()
        && rows
            .iter()
            .enumerate()
            .all(|(index, (kind, revision, source, adopted, status))| {
                kind == PRODUCTION_SOURCE_ARTIFACT_TYPES[index]
                    && revision == &current_source_revisions[index]
                    && source.as_deref() == Some(source_run_id)
                    && adopted.as_deref() == Some(revision.as_str())
                    && status == "adopted"
            }))
}

fn production_current_source_revisions(
    conn: &Connection,
    source_run_id: &str,
) -> Result<Vec<String>, String> {
    PRODUCTION_SOURCE_ARTIFACT_TYPES
        .iter()
        .map(|kind| {
            conn.query_row(
                "SELECT adopted_head_revision_id FROM analysis_artifacts WHERE source_analysis_run_id=? AND artifact_type=?",
                params![source_run_id, kind],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?
            .flatten()
            .ok_or_else(|| "PRODUCTION_SOURCE_INPUTS_INCOMPLETE".to_string())
        })
        .collect()
}

enum ProductionAdaptationRunEvidence {
    None,
    Exact {
        id: String,
        status: String,
    },
    /// The pointer, scope, frozen inputs, and frozen baselines are exact, but
    /// an older completed plan contains an invalid UTF-8 evidence range. This
    /// is deliberately distinct from a baseline mismatch: only an explicit
    /// retry-stage path may replace this known-invalid result.
    EvidenceInvalid {
        id: String,
    },
    BaselineMismatch,
}

fn production_adaptation_run_outputs_are_original(
    conn: &Connection,
    run_id: &str,
) -> Result<bool, String> {
    let mut statement = conn
        .prepare(
            "SELECT artifact.artifact_type,revision.change_type,revision.parent_revision_id
             FROM analysis_artifacts artifact
             JOIN analysis_artifact_revisions revision ON revision.id=artifact.candidate_head_revision_id
             WHERE artifact.adaptation_analysis_run_id=?
             ORDER BY artifact.artifact_type",
        )
        .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
    let rows = statement
        .query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
    let kinds = rows
        .iter()
        .map(|(kind, _, _)| kind.as_str())
        .collect::<HashSet<_>>();
    Ok(rows.len() == PRODUCTION_ADAPTATION_ARTIFACT_TYPES.len()
        && kinds.len() == PRODUCTION_ADAPTATION_ARTIFACT_TYPES.len()
        && PRODUCTION_ADAPTATION_ARTIFACT_TYPES
            .iter()
            .all(|kind| kinds.contains(*kind))
        && rows.iter().all(|(_, change_type, parent)| {
            change_type == "ai_adaptation_analysis" && parent.is_none()
        }))
}

fn production_adaptation_run_evidence_is_valid(
    conn: &Connection,
    run_id: &str,
    novel_work_id: &str,
) -> Result<bool, String> {
    let row: Option<(String, String, bool)> = conn
        .query_row(
            "SELECT revision.body_json,revision.status,artifact.adopted_head_revision_id IS NOT NULL
             FROM analysis_artifacts artifact
             JOIN analysis_artifact_revisions revision ON revision.id=COALESCE(artifact.adopted_head_revision_id,artifact.candidate_head_revision_id) AND revision.analysis_artifact_id=artifact.id
             WHERE artifact.adaptation_analysis_run_id=?
               AND artifact.novel_work_id=?
               AND artifact.artifact_type='comic_chapter_plan'",
            params![run_id, novel_work_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
    let Some((body, status, adopted)) = row else {
        return Err("PRODUCTION_RECOVERY_READ_FAILED".into());
    };
    if (adopted && status != "adopted") || (!adopted && status != "candidate") {
        return Err("PRODUCTION_RECOVERY_READ_FAILED".into());
    }
    match crate::novel_adaptation::validate_comic_chapter_plan_evidence_ranges(
        conn,
        novel_work_id,
        &json_value(body),
    ) {
        Ok(()) => Ok(true),
        Err(error) if error == "EVIDENCE_RANGE_INVALID" => Ok(false),
        Err(_) => Err("PRODUCTION_RECOVERY_READ_FAILED".into()),
    }
}

fn production_adaptation_run_intent_matches(
    conn: &Connection,
    run_id: &str,
    expected: &Option<ComicPlanIntent>,
) -> Result<bool, String> {
    let event: Option<Value> = conn
        .query_row(
            "SELECT payload_json FROM adaptation_analysis_run_events WHERE adaptation_analysis_run_id=? AND seq=1",
            params![run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?
        .map(json_value);
    let Some(event) = event else {
        return Ok(false);
    };
    let actual = match event.get("comicPlanIntent") {
        None | Some(Value::Null) => None,
        Some(value) => canonicalize_comic_plan_intent(Some(
            serde_json::from_value(value.clone())
                .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?,
        ))?,
    };
    Ok(&actual == expected)
}

fn production_job_owned_adaptation_run(
    conn: &Connection,
    job: &NovelProductionJob,
    source_run_id: &str,
    adaptation_id: &str,
    current_source_revisions: &[String],
) -> Result<ProductionAdaptationRunEvidence, String> {
    // A retry creates a new production attempt, but an already-persisted
    // adaptation run may have crossed the provider boundary in the previous
    // attempt.  The job pointer is therefore stronger evidence than the
    // attempt-derived idempotency key.  It may only be reused when every
    // immutable input still describes this exact job; otherwise fail closed
    // instead of submitting a replacement request.
    if let Some(pointer) = job.adaptation_analysis_run_id.as_deref() {
        let candidate: Option<(String, String, String, String, String, String, String, Option<String>, String)> = conn
            .query_row(
                "SELECT run.id,run.status,run.project_id,run.novel_work_id,run.comic_adaptation_id,chapter.novel_chapter_revision_id,run.base_canon_version_id,run.base_novel_state_version_id,run.base_continuity_state_version_id FROM adaptation_analysis_runs run JOIN comic_adaptation_chapters chapter ON chapter.id=run.comic_adaptation_chapter_id WHERE run.id=?",
                params![pointer],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
            )
            .optional()
            .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
        let Some((
            id,
            status,
            project,
            work,
            adaptation,
            chapter_revision,
            canon,
            state,
            continuity,
        )) = candidate
        else {
            return Ok(ProductionAdaptationRunEvidence::BaselineMismatch);
        };
        let (expected_canon, expected_state, expected_continuity) =
            production_adaptation_baselines(conn, job)?;
        let exact = project == job.project_id
            && work == job.novel_work_id
            && adaptation == adaptation_id
            && chapter_revision == job.source_revision_id
            && canon == expected_canon
            && state.as_deref() == Some(expected_state.as_str())
            && continuity == expected_continuity
            && production_adaptation_run_inputs_match(
                conn,
                &id,
                source_run_id,
                current_source_revisions,
            )?
            && production_adaptation_run_intent_matches(conn, &id, &job.comic_plan_intent)?;
        if !exact {
            return Ok(ProductionAdaptationRunEvidence::BaselineMismatch);
        }
        if status != "ready_for_review" {
            return Ok(ProductionAdaptationRunEvidence::Exact { id, status });
        }
        return Ok(
            if production_adaptation_run_evidence_is_valid(conn, &id, &job.novel_work_id)? {
                ProductionAdaptationRunEvidence::Exact { id, status }
            } else {
                ProductionAdaptationRunEvidence::EvidenceInvalid { id }
            },
        );
    }
    let key = format!("{}:adaptation:{}", job.id, job.attempt_no);
    let candidate: Option<(String, String, String, String, String, String, String, Option<String>, String)> = conn
        .query_row(
            "SELECT run.id,run.status,run.project_id,run.novel_work_id,run.comic_adaptation_id,chapter.novel_chapter_revision_id,run.base_canon_version_id,run.base_novel_state_version_id,run.base_continuity_state_version_id FROM adaptation_analysis_runs run JOIN comic_adaptation_chapters chapter ON chapter.id=run.comic_adaptation_chapter_id WHERE run.project_id=? AND run.novel_work_id=? AND run.comic_adaptation_id=? AND chapter.novel_chapter_revision_id=? AND run.idempotency_key=?",
            params![job.project_id,job.novel_work_id,adaptation_id,job.source_revision_id,key],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
        )
        .optional()
        .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
    match candidate {
        Some((
            id,
            status,
            project,
            work,
            adaptation,
            chapter_revision,
            canon,
            state,
            continuity,
        )) => {
            let (expected_canon, expected_state, expected_continuity) =
                production_adaptation_baselines(conn, job)?;
            if project == job.project_id
                && work == job.novel_work_id
                && adaptation == adaptation_id
                && chapter_revision == job.source_revision_id
                && canon == expected_canon
                && state.as_deref() == Some(expected_state.as_str())
                && continuity == expected_continuity
                && production_adaptation_run_inputs_match(
                    conn,
                    &id,
                    source_run_id,
                    current_source_revisions,
                )?
                && production_adaptation_run_intent_matches(conn, &id, &job.comic_plan_intent)?
            {
                if status != "ready_for_review" {
                    return Ok(ProductionAdaptationRunEvidence::Exact { id, status });
                }
                if production_adaptation_run_evidence_is_valid(conn, &id, &job.novel_work_id)? {
                    Ok(ProductionAdaptationRunEvidence::Exact { id, status })
                } else {
                    Ok(ProductionAdaptationRunEvidence::EvidenceInvalid { id })
                }
            } else {
                // The deterministic key proves a request existed, but its
                // adopted inputs changed.  It is not safe to treat it as if
                // no request had ever been submitted.
                Ok(ProductionAdaptationRunEvidence::BaselineMismatch)
            }
        }
        None => Ok(ProductionAdaptationRunEvidence::None),
    }
}

fn production_adaptation_start_input(
    job: &NovelProductionJob,
    adaptation_id: String,
    current_source_revisions: Vec<String>,
    canon: String,
    state_version: String,
    continuity: String,
) -> crate::novel_adaptation::AdaptationAnalysisStartInput {
    crate::novel_adaptation::AdaptationAnalysisStartInput {
        project_id: job.project_id.clone(),
        novel_work_id: job.novel_work_id.clone(),
        comic_adaptation_id: adaptation_id,
        comic_adaptation_chapter_id: None,
        source_analysis_run_id: None,
        source_artifact_revision_ids: current_source_revisions,
        novel_chapter_revision_id: Some(job.source_revision_id.clone()),
        base_canon_version_id: canon,
        base_novel_state_version_id: Some(state_version),
        base_continuity_version_id: continuity,
        provider_id: None,
        model_id: None,
        comic_plan_intent: job.comic_plan_intent.clone(),
        idempotency_key: format!("{}:adaptation:{}", job.id, job.attempt_no),
    }
}

fn production_validate_frozen_comic_plan_intent(
    conn: &Connection,
    job: &NovelProductionJob,
    adaptation_run_id: &str,
    page_plan_revision_id: &str,
) -> Result<(), String> {
    let Some(intent) = job.comic_plan_intent.as_ref() else {
        return Ok(());
    };
    let body: String = conn
        .query_row(
            "SELECT revision.body_json FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id JOIN novel_works work ON work.id=artifact.novel_work_id WHERE revision.id=? AND artifact.adaptation_analysis_run_id=? AND artifact.artifact_type='page_panel_plan' AND artifact.novel_work_id=? AND work.project_id=?",
            params![page_plan_revision_id, adaptation_run_id, job.novel_work_id, job.project_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "COMIC_PLAN_INTENT_PAGE_PLAN_MISSING".to_string())?
        .ok_or_else(|| "COMIC_PLAN_INTENT_PAGE_PLAN_MISSING".to_string())?;
    crate::novel_adaptation::validate_comic_plan_intent_page_plan(intent, &json_value(body))
}

fn production_accept_key(
    job: &NovelProductionJob,
    adaptation_id: &str,
    proposal: &str,
    chapter_plan: &str,
    scene_plan: &str,
) -> Result<String, String> {
    Ok(format!(
        "{}:accept:{}",
        job.id,
        request_hash(
            &json!({"adaptationId":adaptation_id,"proposal":proposal,"chapterPlan":chapter_plan,"scenePlan":scene_plan})
        )?
    ))
}

fn production_apply_key(
    job: &NovelProductionJob,
    adaptation_id: &str,
    head: &str,
    page_plan: &str,
    selections: &Value,
) -> Result<String, String> {
    Ok(format!(
        "{}:apply:{}",
        job.id,
        request_hash(
            &json!({"adaptationId":adaptation_id,"head":head,"pagePlan":page_plan,"selections":selections})
        )?
    ))
}

/// A successful receipt and its production mapping are authoritative even if
/// the process died before the job row was marked succeeded.
fn production_replayed_apply_operation(
    conn: &Connection,
    idempotency_key: &str,
    adaptation_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row("SELECT operation.id FROM analysis_apply_operations operation WHERE operation.idempotency_key=? AND operation.operation_type='apply_comic_plan' AND operation.comic_adaptation_id=? AND operation.status='succeeded' AND EXISTS(SELECT 1 FROM analysis_apply_receipts receipt WHERE receipt.analysis_apply_operation_id=operation.id) AND EXISTS(SELECT 1 FROM comic_production_chapters chapter WHERE chapter.apply_operation_id=operation.id)",params![idempotency_key,adaptation_id],|row|row.get(0)).optional().map_err(|e|e.to_string())
}

fn production_scene_keys(
    conn: &Connection,
    revision_id: &str,
) -> Result<Vec<(String, Value)>, String> {
    let body: String = conn
        .query_row(
            "SELECT body_json FROM analysis_artifact_revisions WHERE id=?",
            params![revision_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let scenes = json_value(body)
        .get("scenes")
        .and_then(Value::as_array)
        .cloned()
        .ok_or("SCENE_PLAN_SCHEMA_INVALID")?;
    scenes
        .into_iter()
        .map(|scene| {
            scene
                .get("stableKey")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(|key| (key.to_string(), scene.clone()))
                .ok_or_else(|| "SCENE_PLAN_SCHEMA_INVALID".into())
        })
        .collect()
}

fn production_scene_references(scene: &Value) -> Result<(Vec<String>, Vec<String>), String> {
    let string_list = |key: &str| -> Result<Vec<String>, String> {
        match scene.get(key) {
            None => Ok(vec![]),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .filter(|v| !v.is_empty())
                        .map(str::to_string)
                        .ok_or_else(|| format!("SCENE_REFERENCE_INVALID:{key}"))
                })
                .collect(),
            Some(_) => Err(format!("SCENE_REFERENCE_INVALID:{key}")),
        }
    };
    let entity_ids = string_list("selectedEntityIds")?;
    let card_ids = string_list("visualCardRevisionIds")?;
    for unresolved in ["entityRefs", "visualCardRefs", "references"] {
        if scene.get(unresolved).is_some_and(|value| {
            !value.is_null() && value.as_array().is_none_or(|items| !items.is_empty())
        }) {
            return Err(format!("SCENE_REFERENCE_UNRESOLVED:{unresolved}"));
        }
    }
    Ok((entity_ids, card_ids))
}

async fn production_continue_after_source(
    state: &tauri::State<'_, DbState>,
    app: &tauri::State<'_, AppState>,
    job: NovelProductionJob,
) -> Result<NovelProductionJob, String> {
    let job = db::with_connection(state, |conn| {
        production_reconcile_ready_run_pointers(conn, &job)
    })?;
    let source_run_id: String = db::with_connection(state, |conn| {
        conn.query_row(
            "SELECT source_analysis_run_id FROM novel_production_jobs WHERE id=?",
            params![job.id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten()
        .ok_or_else(|| "PRODUCTION_SOURCE_RUN_MISSING".into())
    })?;
    let source_revisions = match db::with_connection(state, |conn| {
        production_adopt_candidates(
            conn,
            &job.project_id,
            &job.novel_work_id,
            "artifact.source_analysis_run_id",
            &source_run_id,
            &format!("{}:source", job.id),
        )
    }) {
        Ok(value) => value,
        Err(error) if error.starts_with("PRODUCTION_CANDIDATE_DRIFT:") => {
            return production_update_stage_from_error(
                state,
                &job,
                "blocked_conflict",
                "integrating_context",
                3,
                "PRODUCTION_CANDIDATE_DRIFT",
                "产物候选已被手改或优化；请明确采用后再继续。",
            )
        }
        Err(error) => return Err(error),
    };
    if source_revisions.len() != 14 {
        return production_update_stage_from_error(
            state,
            &job,
            "blocked_conflict",
            "integrating_context",
            3,
            "SOURCE_ARTIFACTS_INCOMPLETE",
            "原著分析产物不完整，无法进入改编。",
        );
    }
    let current_source_revisions = production_source_input_revisions(&source_revisions)?;
    let job = db::with_connection(state, |conn| {
        production_update_stage(
            conn,
            &job.id,
            "running",
            "ensuring_adaptation",
            4,
            14,
            Some("启动默认改编分析"),
            None,
        )
    })?;
    let (canon, state_version, continuity) =
        db::with_connection(state, |conn| production_adaptation_baselines(conn, &job))?;
    let adaptation_id = job
        .default_adaptation_id
        .clone()
        .ok_or("PRODUCTION_DEFAULT_ADAPTATION_MISSING")?;
    // A crash can leave the durable, deterministic adaptation run without
    // the job pointer.  Do not call `start` again just to obtain its receipt:
    // a queued/running/unknown run may already have crossed a billable
    // provider boundary.  Ready rows are handled by the usual replay path;
    // all other states remain visibly pending or require explicit recovery.
    let job_owned_adaptation = db::with_connection(state, |conn| {
        production_job_owned_adaptation_run(
            conn,
            &job,
            &source_run_id,
            &adaptation_id,
            &current_source_revisions,
        )
    })?;
    match &job_owned_adaptation {
        ProductionAdaptationRunEvidence::EvidenceInvalid { .. } => {
            return production_update_stage_from_error(
                state,
                &job,
                "blocked_conflict",
                "ensuring_adaptation",
                4,
                "ADAPTATION_EVIDENCE_INVALID",
                "改编引用无效，需重新生成改编（可能计费）。",
            )
        }
        ProductionAdaptationRunEvidence::BaselineMismatch => {
            return db::with_connection(state, |conn| {
                production_update_stage(
                    conn,
                    &job.id,
                    "blocked_conflict",
                    "adaptation_analysis",
                    5,
                    14,
                    Some("核对已变更的原著产物后继续"),
                    Some((
                        "PRODUCTION_BASELINE_STALE",
                        "已发现同一生产请求的改编输入已过期；未重复提交。",
                    )),
                )
            });
        }
        ProductionAdaptationRunEvidence::Exact {
            status: orphan_status,
            ..
        } if orphan_status != "ready_for_review" => {
            let (status, message, next) = match orphan_status.as_str() {
                "draft" | "queued" | "running" => (
                    "waiting_for_predecessor",
                    "已发现尚未完成的默认改编分析，未重复提交。",
                    "等待默认改编分析完成",
                ),
                _ => (
                    "error",
                    "已发现无法自动确认的默认改编分析；未重复提交。",
                    "核对默认改编分析后继续",
                ),
            };
            return db::with_connection(state, |conn| {
                production_update_stage(
                    conn,
                    &job.id,
                    status,
                    "adaptation_analysis",
                    5,
                    14,
                    Some(next),
                    Some(("ADAPTATION_ANALYSIS_UNRESOLVED", message)),
                )
            });
        }
        _ => {}
    }
    let existing_adaptation_run = match job_owned_adaptation {
        ProductionAdaptationRunEvidence::Exact { id, status } if status == "ready_for_review" => {
            Some(id)
        }
        _ => None,
    };
    let adaptation_run = if let Some(run_id) = existing_adaptation_run {
        crate::novel_adaptation::novel_adaptation_analysis_status(
            state.clone(),
            crate::novel_adaptation::AdaptationAnalysisStatusInput {
                project_id: job.project_id.clone(),
                novel_work_id: job.novel_work_id.clone(),
                comic_adaptation_id: adaptation_id.clone(),
                adaptation_analysis_run_id: run_id,
            },
        )?
    } else {
        crate::novel_adaptation::novel_adaptation_analysis_start(
            state.clone(),
            app.clone(),
            production_adaptation_start_input(
                &job,
                adaptation_id.clone(),
                current_source_revisions.clone(),
                canon.clone(),
                state_version.clone(),
                continuity.clone(),
            ),
        )
        .await?
    };
    db::with_connection(state, |conn| {
        conn.execute(
            "UPDATE novel_production_jobs SET adaptation_analysis_run_id=?,updated_at=? WHERE id=?",
            params![adaptation_run.id, now(), job.id],
        )
        .map_err(|e| e.to_string())
    })?;
    if adaptation_run.status != "ready_for_review" {
        let (status, code, message, next) =
            if adaptation_run.safe_error_code.as_deref() == Some("NOT_CONFIGURED") {
                (
                    "blocked_config",
                    "NOT_CONFIGURED",
                    "未配置 LLM，默认改编尚未生成。",
                    "配置 LLM 后继续",
                )
            } else {
                (
                    "error",
                    adaptation_run
                        .safe_error_code
                        .as_deref()
                        .unwrap_or("ADAPTATION_ANALYSIS_FAILED"),
                    adaptation_run
                        .safe_user_message
                        .as_deref()
                        .unwrap_or("默认改编分析失败。"),
                    "重试默认改编",
                )
            };
        return db::with_connection(state, |conn| {
            production_update_stage(
                conn,
                &job.id,
                status,
                "adaptation_analysis",
                5,
                14,
                Some(next),
                Some((code, message)),
            )
        });
    }
    let adaptation_revisions = match db::with_connection(state, |conn| {
        production_adopt_candidates(
            conn,
            &job.project_id,
            &job.novel_work_id,
            "artifact.adaptation_analysis_run_id",
            &adaptation_run.id,
            &format!("{}:adaptation", job.id),
        )
    }) {
        Ok(value) => value,
        Err(error) if error.starts_with("PRODUCTION_CANDIDATE_DRIFT:") => {
            return production_update_stage_from_error(
                state,
                &job,
                "blocked_conflict",
                "adaptation_analysis",
                5,
                "PRODUCTION_CANDIDATE_DRIFT",
                "改编候选已被手改或优化；请明确采用后再继续。",
            )
        }
        Err(error) => return Err(error),
    };
    let proposal = adaptation_revisions
        .get("adaptation_proposal")
        .cloned()
        .ok_or("ADAPTATION_PROPOSAL_MISSING")?;
    let chapter_plan = adaptation_revisions
        .get("comic_chapter_plan")
        .cloned()
        .ok_or("COMIC_CHAPTER_PLAN_MISSING")?;
    let scene_plan = adaptation_revisions
        .get("scene_plan")
        .cloned()
        .ok_or("SCENE_PLAN_MISSING")?;
    let page_plan = adaptation_revisions
        .get("page_panel_plan")
        .cloned()
        .ok_or("PAGE_PANEL_PLAN_MISSING")?;
    if db::with_connection(state, |conn| {
        production_validate_frozen_comic_plan_intent(conn, &job, &adaptation_run.id, &page_plan)
    })
    .is_err()
    {
        return production_update_stage_from_error(
            state,
            &job,
            "blocked_conflict",
            "applying_production",
            7,
            "COMIC_PLAN_INTENT_MISMATCH",
            "改编规划未满足本次已保存的漫画页面要求，未开始生成漫画页。",
        );
    }
    let accepted: bool = db::with_connection(state, |conn| {
        conn.query_row("SELECT EXISTS(SELECT 1 FROM comic_adaptation_plan_heads WHERE comic_adaptation_id=? AND adaptation_proposal_revision_id=? AND comic_chapter_plan_revision_id=? AND scene_plan_revision_id=? AND status='active')",params![adaptation_id,proposal,chapter_plan,scene_plan],|row|row.get(0)).map_err(|e|e.to_string())
    })?;
    if !accepted {
        let accept_key =
            production_accept_key(&job, &adaptation_id, &proposal, &chapter_plan, &scene_plan)?;
        let accept_preview = crate::novel_adaptation::novel_adaptation_accept_preview(
            state.clone(),
            crate::novel_adaptation::AdaptationAcceptPreviewInput {
                project_id: job.project_id.clone(),
                novel_work_id: job.novel_work_id.clone(),
                comic_adaptation_id: adaptation_id.clone(),
                adaptation_proposal_revision_id: proposal.clone(),
                comic_chapter_plan_revision_id: chapter_plan,
                scene_plan_revision_id: scene_plan.clone(),
                base_canon_version_id: Some(canon.clone()),
                base_continuity_version_id: Some(continuity.clone()),
                expected_adaptation_version: None,
                idempotency_key: accept_key.clone(),
            },
        )?;
        crate::novel_adaptation::novel_adaptation_accept(
            state.clone(),
            crate::novel_adaptation::AdaptationAcceptInput {
                project_id: job.project_id.clone(),
                novel_work_id: job.novel_work_id.clone(),
                comic_adaptation_id: adaptation_id.clone(),
                operation_id: accept_preview.operation_id,
                approval_token: accept_preview.approval_token,
                idempotency_key: accept_key,
            },
        )?;
    }
    let job = db::with_connection(state, |conn| {
        production_update_stage(
            conn,
            &job.id,
            "running",
            "applying_production",
            7,
            14,
            Some("冻结场景上下文并创建漫画生产树"),
            None,
        )
    })?;
    let (current_canon, current_state, current_continuity, version, head): (
        String,
        String,
        String,
        i64,
        String,
    ) = db::with_connection(state, |conn| {
        conn.query_row("SELECT work.published_canon_version_id,work.current_novel_state_version_id,adaptation.current_continuity_version_id,adaptation.optimistic_version,head.id FROM novel_works work JOIN comic_adaptations adaptation ON adaptation.id=? JOIN comic_adaptation_plan_heads head ON head.comic_adaptation_id=adaptation.id AND head.status='active' WHERE work.id=?",params![adaptation_id,job.novel_work_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(|e|e.to_string())
    })?;
    let context = db::with_connection(state, |conn| {
        production_current_context(conn, &source_run_id)
    })?;
    let scenes = db::with_connection(state, |conn| production_scene_keys(conn, &scene_plan))?;
    let mut selections = Vec::with_capacity(scenes.len());
    for (key, scene) in scenes {
        let (entities, cards) = match production_scene_references(&scene) {
            Ok(value) => value,
            Err(code) => {
                return db::with_connection(state, |conn| {
                    production_update_stage(
                        conn,
                        &job.id,
                        "blocked_conflict",
                        "applying_production",
                        7,
                        14,
                        Some("补全场景引用后继续"),
                        Some((&code, "场景计划包含无法解析的实体或视觉卡引用。")),
                    )
                })
            }
        };
        let snapshot = crate::novel_adaptation::novel_scene_context_resolve(
            state.clone(),
            production_scene_context_resolve_input(
                &job,
                &adaptation_id,
                &scene_plan,
                &key,
                context.clone(),
                &current_canon,
                &current_state,
                &current_continuity,
                &proposal,
                entities,
                cards,
            ),
        )?;
        let approved = crate::novel_adaptation::novel_scene_context_approve(
            state.clone(),
            crate::novel_adaptation::SceneContextApproveInput {
                project_id: job.project_id.clone(),
                novel_work_id: job.novel_work_id.clone(),
                comic_adaptation_id: adaptation_id.clone(),
                scene_context_snapshot_id: snapshot.snapshot_id.clone(),
                context_fingerprint: snapshot.context_fingerprint,
                idempotency_key: format!("{}:scene-approve:{}", job.id, key),
            },
        )?;
        selections.push(crate::novel_adaptation::ComicPlanSelection {
            planning_scene_stable_key: key,
            scene_context_snapshot_id: approved.snapshot_id,
        });
    }
    let apply_key = production_apply_key(
        &job,
        &adaptation_id,
        &head,
        &page_plan,
        &serde_json::to_value(&selections).map_err(|e| e.to_string())?,
    )?;
    let replayed_apply = db::with_connection(state, |conn| {
        production_replayed_apply_operation(conn, &apply_key, &adaptation_id)
    })?;
    let apply_operation_id = if let Some(operation_id) = replayed_apply {
        operation_id
    } else {
        let apply_preview = crate::novel_adaptation::novel_comic_plan_apply_preview(
            state.clone(),
            crate::novel_adaptation::ComicPlanApplyPreviewInput {
                project_id: job.project_id.clone(),
                novel_work_id: job.novel_work_id.clone(),
                comic_adaptation_id: adaptation_id.clone(),
                accepted_plan_version_id: head,
                page_panel_plan_revision_id: page_plan,
                scene_context_selections: selections,
                base_canon_version_id: current_canon,
                base_continuity_version_id: current_continuity,
                expected_adaptation_version: version,
                idempotency_key: apply_key.clone(),
            },
        )?;
        crate::novel_adaptation::novel_comic_plan_apply(
            state.clone(),
            crate::novel_adaptation::ComicPlanApplyInput {
                project_id: job.project_id.clone(),
                novel_work_id: job.novel_work_id.clone(),
                comic_adaptation_id: adaptation_id,
                operation_id: apply_preview.operation_id.clone(),
                approval_token: apply_preview.approval_token,
                idempotency_key: apply_key,
            },
        )?
        .operation_id
    };
    db::with_connection(state, |conn| {
        let finished = production_update_stage(
            conn,
            &job.id,
            "succeeded",
            "succeeded",
            8,
            14,
            Some("漫画生产树已创建"),
            None,
        )?;
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        tx.execute("UPDATE novel_production_jobs SET adaptation_analysis_run_id=?,apply_operation_id=? WHERE id=?",params![adaptation_run.id,apply_operation_id,finished.id]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        production_job_value(
            conn,
            &finished.project_id,
            &finished.novel_work_id,
            &finished.id,
        )
    })
}

fn production_update_stage_from_error(
    state: &tauri::State<'_, DbState>,
    job: &NovelProductionJob,
    status: &str,
    stage: &str,
    index: i64,
    code: &str,
    message: &str,
) -> Result<NovelProductionJob, String> {
    db::with_connection(state, |conn| {
        production_update_stage(
            conn,
            &job.id,
            status,
            stage,
            index,
            job.completed_artifact_count,
            Some("修复后重试"),
            Some((code, message)),
        )
    })
}

async fn production_run_source(
    state: tauri::State<'_, DbState>,
    app: tauri::State<'_, AppState>,
    job: NovelProductionJob,
    provider_id: Option<String>,
    model_id: Option<String>,
) -> Result<NovelProductionJob, String> {
    let completed_source: Option<String> = db::with_connection(&state, |conn| {
        conn.query_row("SELECT source_analysis_run_id FROM novel_production_jobs job JOIN source_analysis_runs run ON run.id=job.source_analysis_run_id WHERE job.id=? AND run.status='ready_for_review'",params![job.id],|row|row.get(0)).optional().map_err(|e|e.to_string())
    })?;
    if completed_source.is_some() {
        return production_continue_after_source(&state, &app, job).await;
    }
    let owner = state.app_session_id().to_owned();
    if !db::with_connection(&state, |conn| production_claim_source(conn, &job, &owner))? {
        return db::with_connection(&state, |conn| {
            production_job_value(conn, &job.project_id, &job.novel_work_id, &job.id)
        });
    }
    let run = novel_analysis_start(
        state.clone(),
        app.clone(),
        NovelAnalysisStartInput {
            project_id: job.project_id.clone(),
            novel_work_id: job.novel_work_id.clone(),
            chapter_revision_id: job.source_revision_id.clone(),
            parent_context_revision_id: None,
            idempotency_key: format!("{}:source:{}", job.id, job.attempt_no),
            provider_id,
            model_id,
        },
    )
    .await?;
    let finished = db::with_connection(&state, |conn| production_finish_source(conn, &job, &run))?;
    if finished.status == "waiting_for_predecessor" && finished.stage == "integrating_context" {
        production_continue_after_source(&state, &app, finished).await
    } else {
        Ok(finished)
    }
}

fn production_record_background_failure(
    conn: &Connection,
    expected_job: &NovelProductionJob,
    error: &str,
) -> Result<(), String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let job: Option<NovelProductionJob> = tx
        .query_row(
            &format!("{PRODUCTION_JOB_SELECT} WHERE id=?"),
            params![expected_job.id],
            production_job_from_row,
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some(job) = job else {
        return Ok(());
    };
    if job.status == "succeeded"
        || job.project_id != expected_job.project_id
        || job.novel_work_id != expected_job.novel_work_id
        || job.source_revision_id != expected_job.source_revision_id
        || job.attempt_no != expected_job.attempt_no
    {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let ts = now();
    let code = if error.contains("NOT_CONFIGURED") {
        "NOT_CONFIGURED"
    } else if error.contains("BASELINE_STALE") || error.contains("REVISION_CONFLICT") {
        "BASELINE_STALE"
    } else {
        "PRODUCTION_FAILED"
    };
    let status = if code == "NOT_CONFIGURED" {
        "blocked_config"
    } else if code == "BASELINE_STALE" {
        "needs_rebase"
    } else {
        "error"
    };
    let message = if status == "blocked_config" {
        "未配置 LLM，任务已保存；配置后可继续。"
    } else if status == "needs_rebase" {
        "依赖基线已变化，需要刷新后继续。"
    } else {
        "自动生产中断，可从当前阶段重试。"
    };
    let changed = tx.execute("UPDATE novel_production_jobs SET status=?,lease_owner=NULL,lease_expires_at=NULL,safe_error_code=?,safe_user_message=?,next_action=?,updated_at=?,finished_at=? WHERE id=? AND attempt_no=? AND status<>'succeeded'",params![status,code,message,"重试当前阶段",ts,ts,job.id,expected_job.attempt_no]).map_err(|e|e.to_string())?;
    if changed != 1 {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let result = tx
        .query_row(
            &format!("{PRODUCTION_JOB_SELECT} WHERE id=?"),
            params![job.id],
            production_job_from_row,
        )
        .map_err(|e| e.to_string())?;
    production_event(
        &tx,
        &result,
        "background_failed",
        json!({"safeErrorCode":code,"stage":result.stage}),
    )?;
    tx.commit().map_err(|e| e.to_string())
}

fn production_schedule(
    app_handle: tauri::AppHandle,
    job: NovelProductionJob,
    provider_id: Option<String>,
    model_id: Option<String>,
) {
    tauri::async_runtime::spawn(async move {
        let state = app_handle.state::<DbState>();
        let app = app_handle.state::<AppState>();
        match production_run_source(state.clone(), app, job.clone(), provider_id, model_id).await {
            Ok(finished) if finished.status == "succeeded" => {
                crate::comic_visual_batch::schedule_for_production_job(&app_handle, &finished.id);
            }
            Ok(_) => {}
            Err(error) if error == "PRODUCTION_ATTEMPT_SUPERSEDED" => {}
            Err(error) => {
                crate::logging::error(
                    "novel.production.background_failed",
                    json!({
                        "jobId": job.id,
                        "attemptNo": job.attempt_no,
                        "error": safe_error(&error),
                    }),
                );
                let _ = db::with_connection(&state, |conn| {
                    production_record_background_failure(conn, &job, &error)
                });
            }
        }
    });
}

/// Recovers the narrow crash windows after a durable source/adaptation run
/// becomes ready but before its owning production-job pointer is saved.  The
/// idempotency keys are deterministic job-owned identities; only a live,
/// exact-scope `ready_for_review` row may be attached.  Running/error rows
/// remain unresolved and are never treated as a safe resubmission.
fn production_job_owned_source_run(
    conn: &Connection,
    job: &NovelProductionJob,
) -> Result<Option<(String, String)>, String> {
    conn.query_row(
        "SELECT id,status FROM source_analysis_runs WHERE novel_chapter_revision_id=? AND idempotency_key=?",
        params![
            job.source_revision_id,
            format!("{}:source:{}", job.id, job.attempt_no),
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())
}

fn production_reconcile_ready_run_pointers(
    conn: &Connection,
    job: &NovelProductionJob,
) -> Result<NovelProductionJob, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
    let source = production_job_owned_source_run(&tx, job)?;
    if let Some((source, _status)) = source.filter(|(_, status)| status == "ready_for_review") {
        tx.execute("UPDATE novel_production_jobs SET source_analysis_run_id=?,updated_at=? WHERE id=? AND source_analysis_run_id IS NULL",params![source,now(),job.id])
            .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
    }
    let current_source: Option<String> = tx.query_row("SELECT source_analysis_run_id FROM novel_production_jobs WHERE id=? AND project_id=? AND novel_work_id=?",params![job.id,job.project_id,job.novel_work_id],|row|row.get(0))
        .optional().map_err(|_|"PRODUCTION_RECOVERY_READ_FAILED".to_string())?.flatten();
    if let (Some(source), Some(adaptation)) = (current_source, job.default_adaptation_id.as_deref())
    {
        // Initial production reaches this repair point before its source
        // candidates are adopted below.  An adaptation run can only be
        // matched against all ten exact adopted inputs, so defer this optional
        // pointer repair until those inputs exist rather than treating a
        // normal first pass as a recovery failure.
        let revisions = match production_current_source_revisions(&tx, &source) {
            Ok(revisions) => Some(revisions),
            Err(error) if error == "PRODUCTION_SOURCE_INPUTS_INCOMPLETE" => None,
            Err(error) => return Err(error),
        };
        if let Some(revisions) = revisions {
            let ready = match production_job_owned_adaptation_run(
                &tx, job, &source, adaptation, &revisions,
            )? {
                ProductionAdaptationRunEvidence::Exact { id, status }
                    if status == "ready_for_review" =>
                {
                    Some(id)
                }
                ProductionAdaptationRunEvidence::BaselineMismatch => {
                    return Err("PRODUCTION_BASELINE_STALE".into())
                }
                _ => None,
            };
            if let Some(ready) = ready {
                tx.execute("UPDATE novel_production_jobs SET adaptation_analysis_run_id=?,updated_at=? WHERE id=? AND adaptation_analysis_run_id IS NULL",params![ready,now(),job.id])
                    .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
            }
        }
    }
    tx.commit()
        .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
    production_job_value(conn, &job.project_id, &job.novel_work_id, &job.id)
}

/// Startup recovery is deliberately limited to jobs that carry an explicit
/// visual authorization.  It only resumes a provably unclaimed source stage
/// or continues already-ready durable analysis; it never guesses that an
/// in-flight/unknown LLM request was safe to repeat.
fn production_pause_recovery_error(state: &DbState, job_id: &str, code: &str) {
    let (status, message, next) = if code == "PRODUCTION_BASELINE_STALE" {
        (
            "blocked_conflict",
            "已发现生产来源版本变化；为避免重复生成，已暂停。",
            "核对来源后继续",
        )
    } else {
        (
            "error",
            "恢复生产时无法确认已有任务状态，已安全暂停。",
            "核对后继续",
        )
    };
    let _ = db::with_connection(state, |conn| {
        conn.execute(
            "UPDATE novel_production_jobs SET status=?,lease_owner=NULL,lease_expires_at=NULL,safe_error_code=?,safe_user_message=?,next_action=?,updated_at=?,finished_at=? WHERE id=? AND status<>'succeeded'",
            params![status,code,message,next,now(),now(),job_id],
        )
        .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())
    });
    eprintln!("production recovery safely paused job {job_id}: {code}");
}

fn schedule_production_recovery_wake(
    app_handle: &tauri::AppHandle,
    job_id: &str,
    lease_expires_at: i64,
) {
    let wait_ms = (lease_expires_at - now() + 1).max(1) as u64;
    let job_id = job_id.to_owned();
    let inserted = PRODUCTION_RECOVERY_WAKE_JOBS
        .lock()
        .map(|mut jobs| jobs.insert(job_id.clone()))
        .unwrap_or(false);
    if !inserted {
        return;
    }
    let app = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
        if let Ok(mut jobs) = PRODUCTION_RECOVERY_WAKE_JOBS.lock() {
            jobs.remove(&job_id);
        }
        let state = app.state::<DbState>();
        // This is a scoped, conservative re-check. It never replays an
        // unknown provider request merely because the old lease elapsed.
        let _ = recover_authorized_production_job(&app, state.inner(), &job_id);
    });
}

pub(crate) fn recover_authorized_production_jobs(
    app_handle: &tauri::AppHandle,
    state: &DbState,
) -> Result<usize, String> {
    recover_authorized_production_jobs_scoped(app_handle, state, None)
}

fn recover_authorized_production_job(
    app_handle: &tauri::AppHandle,
    state: &DbState,
    job_id: &str,
) -> Result<usize, String> {
    recover_authorized_production_jobs_scoped(app_handle, state, Some(job_id))
}

fn recover_authorized_production_jobs_scoped(
    app_handle: &tauri::AppHandle,
    state: &DbState,
    only_job_id: Option<&str>,
) -> Result<usize, String> {
    let candidates = db::with_connection(state, |conn| {
        let mut stmt = conn.prepare("SELECT job.id,job.project_id,job.novel_work_id,job.status,job.source_analysis_run_id,job.adaptation_analysis_run_id,job.lease_expires_at FROM novel_production_jobs job JOIN comic_visual_batches batch ON batch.production_job_id=job.id WHERE batch.status IN ('authorized','waiting_text','preparing','running','blocked_config') AND (?1 IS NULL OR job.id=?1) ORDER BY job.created_at,job.id")
            .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
        let candidates = stmt
            .query_map(params![only_job_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            })
            .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
        Ok(candidates)
    })?;
    let mut scheduled = 0;
    for (job_id, project_id, work_id, _stored_status, source_run, adaptation_run, lease) in
        candidates
    {
        macro_rules! recover_or_pause {
            ($value:expr) => {
                match $value {
                    Ok(value) => value,
                    Err(code) => {
                        production_pause_recovery_error(state, &job_id, &code);
                        continue;
                    }
                }
            };
        }
        let original = recover_or_pause!(db::with_connection(state, |conn| {
            production_job_value(conn, &project_id, &work_id, &job_id)
        }));
        let reconciled = recover_or_pause!(db::with_connection(state, |conn| {
            production_reconcile_ready_run_pointers(conn, &original)
        }));
        let status = reconciled.status.clone();
        let source_run = reconciled.source_analysis_run_id.clone().or(source_run);
        let adaptation_run = reconciled
            .adaptation_analysis_run_id
            .clone()
            .or(adaptation_run);
        let (source_ready, adaptation_ready, source_orphan, adaptation_orphan) =
            recover_or_pause!(db::with_connection(state, |conn| {
                let source_ready = source_run
                    .as_deref()
                    .map(|id| {
                        conn.query_row(
                            "SELECT status FROM source_analysis_runs WHERE id=?",
                            params![id],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()
                        .map(|status| status.as_deref() == Some("ready_for_review"))
                    })
                    .transpose()
                    .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?
                    .unwrap_or(false);
                let adaptation_ready = adaptation_run
                    .as_deref()
                    .map(|id| {
                        conn.query_row(
                            "SELECT status FROM adaptation_analysis_runs WHERE id=?",
                            params![id],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()
                        .map(|status| status.as_deref() == Some("ready_for_review"))
                    })
                    .transpose()
                    .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?
                    .unwrap_or(false);
                let source_orphan = production_job_owned_source_run(conn, &reconciled)?;
                let adaptation_orphan = match (
                    source_run.as_deref(),
                    reconciled.default_adaptation_id.as_deref(),
                ) {
                    (Some(source), Some(adaptation)) if source_ready => {
                        let revisions = production_current_source_revisions(conn, source)?;
                        production_job_owned_adaptation_run(
                            conn,
                            &reconciled,
                            source,
                            adaptation,
                            &revisions,
                        )?
                    }
                    _ => ProductionAdaptationRunEvidence::None,
                };
                if matches!(
                    adaptation_orphan,
                    ProductionAdaptationRunEvidence::BaselineMismatch
                ) {
                    return Err("PRODUCTION_BASELINE_STALE".into());
                }
                Ok((
                    source_ready,
                    adaptation_ready,
                    source_orphan,
                    adaptation_orphan,
                ))
            }));
        let can_start = status == "queued"
            && source_run.is_none()
            && source_orphan.is_none()
            && lease.is_none();
        // A known-ready adaptation can safely continue to apply.  A
        // known-ready source may begin adaptation only when no prior
        // adaptation request exists; an existing non-ready run is ambiguous
        // and must not be replaced automatically.
        let can_continue = matches!(status.as_str(), "running" | "waiting_for_predecessor")
            && (adaptation_ready
                || (source_ready
                    && adaptation_run.is_none()
                    && matches!(adaptation_orphan, ProductionAdaptationRunEvidence::None)));
        if let Some(expires) = lease.filter(|expires| *expires >= now()) {
            // Even a ready downstream stage must not race a worker whose
            // durable lease has not expired. Revisit this one job only when
            // that lease becomes eligible for conservative recovery.
            schedule_production_recovery_wake(app_handle, &job_id, expires);
            continue;
        }
        if can_start || can_continue {
            let job = recover_or_pause!(db::with_connection(state, |conn| {
                production_job_value(conn, &project_id, &work_id, &job_id)
            }));
            production_schedule(app_handle.clone(), job, None, None);
            scheduled += 1;
        } else if status == "running" && lease.is_some_and(|expires| expires < now()) {
            // The old worker could have submitted an LLM request. Stop with a
            // readable recovery state rather than automatically spending again.
            let _ = db::with_connection(state, |conn| {
                conn.execute("UPDATE novel_production_jobs SET status='error',lease_owner=NULL,lease_expires_at=NULL,safe_error_code='PRODUCTION_RECOVERY_UNKNOWN',safe_user_message='上次文字生产中断，无法确认远端请求结果；请核对后手动继续。',next_action='核对后继续',updated_at=?,finished_at=? WHERE id=? AND status='running'",params![now(),now(),job_id]).map_err(|_|"PRODUCTION_RECOVERY_WRITE_FAILED".to_string())
            });
        }
    }
    Ok(scheduled)
}

#[tauri::command]
pub fn novel_production_start(
    state: tauri::State<'_, DbState>,
    app_handle: tauri::AppHandle,
    app_state: tauri::State<'_, AppState>,
    input: NovelProductionStartInput,
) -> Result<NovelProductionJob, String> {
    let authorization = if let Some(visual) = &input.visual_output {
        let cfg = app_state
            .cfg
            .read()
            .map_err(|_| "CONFIG_UNAVAILABLE")?
            .clone();
        Some(crate::comic_visual_batch::freeze_authorization(
            visual,
            app_state.registry.active().id(),
            &cfg.image_model,
        )?)
    } else {
        None
    };
    let job = db::with_connection(&state, |conn| {
        production_start_with_visual_inner(conn, &input, authorization.as_ref())
    })?;
    if job.status == "succeeded" {
        // Replayed/legacy jobs remain unmodified unless an explicit authorize
        // command created a batch; this schedule is a harmless scoped lookup.
        crate::comic_visual_batch::schedule_for_production_job(&app_handle, &job.id);
    } else {
        production_schedule(app_handle, job.clone(), input.provider_id, input.model_id);
    }
    Ok(job)
}

#[tauri::command]
pub fn novel_production_get(
    state: tauri::State<'_, DbState>,
    input: NovelProductionGetInput,
) -> Result<NovelProductionJob, String> {
    db::with_connection(&state, |conn| {
        production_job_value(
            conn,
            &input.project_id,
            &input.novel_work_id,
            &input.production_job_id,
        )
    })
}

#[tauri::command]
pub fn novel_production_list_for_chapter(
    state: tauri::State<'_, DbState>,
    input: NovelProductionListForChapterInput,
) -> Result<Vec<NovelProductionJob>, String> {
    db::with_connection(&state, |conn| {
        get_work(conn, &input.project_id, &input.novel_work_id)?;
        let mut stmt=conn.prepare(&format!("{PRODUCTION_JOB_SELECT} WHERE project_id=? AND novel_work_id=? AND novel_chapter_id=? ORDER BY created_at DESC,id DESC")).map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map(
                params![
                    input.project_id,
                    input.novel_work_id,
                    input.novel_chapter_id
                ],
                production_job_from_row,
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    })
}

fn production_resume_should_schedule(job: &NovelProductionJob) -> bool {
    job.status == "queued"
        || (job.status == "waiting_for_predecessor" && job.source_analysis_run_id.is_some())
}

/// Return the old run only for an explicit, exact recovery of a known-invalid
/// result or a known terminal provider failure. Everything else is preserved
/// and paused: a production retry must never turn an ambiguous pointer into a
/// fresh billable request.
fn production_adaptation_pointer_for_explicit_retry(
    conn: &Connection,
    job: &NovelProductionJob,
    command_name: &str,
    expected_stage: &str,
) -> Result<Option<(String, &'static str)>, String> {
    let Some(source_run_id) = job.source_analysis_run_id.as_deref() else {
        return Ok(None);
    };
    let Some(adaptation_id) = job.default_adaptation_id.as_deref() else {
        return Ok(None);
    };
    if job.adaptation_analysis_run_id.is_none()
        || !matches!(
            job.stage.as_str(),
            "ensuring_adaptation" | "adaptation_analysis"
        )
    {
        return Ok(None);
    }
    let current_source_revisions = production_current_source_revisions(conn, source_run_id)?;
    match production_job_owned_adaptation_run(
        conn,
        job,
        source_run_id,
        adaptation_id,
        &current_source_revisions,
    )? {
        ProductionAdaptationRunEvidence::Exact { id: _, status }
            if status == "ready_for_review" =>
        {
            Ok(None)
        }
        ProductionAdaptationRunEvidence::Exact { id, status }
            if status == "error"
                && command_name == "novel_production_retry_stage"
                && matches!(
                    expected_stage,
                    "ensuring_adaptation" | "adaptation_analysis"
                ) =>
        {
            let known_terminal_error: bool = conn
                .query_row(
                    "SELECT status='error' AND safe_error_code IS NOT NULL AND trim(safe_error_code)<>'' FROM adaptation_analysis_runs WHERE id=?",
                    params![id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?
                .ok_or("PRODUCTION_BASELINE_STALE")?;
            if !known_terminal_error {
                return Err("PRODUCTION_ADAPTATION_UNRESOLVED".into());
            }
            let active_plan: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM comic_adaptation_plan_heads WHERE comic_adaptation_id=? AND status='active')",
                    params![adaptation_id],
                    |row| row.get(0),
                )
                .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
            if job.apply_operation_id.is_some() || active_plan {
                return Err("PRODUCTION_STAGE_CONFLICT".into());
            }
            Ok(Some((id, "ADAPTATION_ANALYSIS_FAILED")))
        }
        ProductionAdaptationRunEvidence::EvidenceInvalid { id }
            if command_name == "novel_production_retry_stage"
                && expected_stage == "ensuring_adaptation" =>
        {
            if !production_adaptation_run_outputs_are_original(conn, &id)? {
                return Err("PRODUCTION_BASELINE_STALE".into());
            }
            let active_plan: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM comic_adaptation_plan_heads WHERE comic_adaptation_id=? AND status='active')",
                    params![adaptation_id],
                    |row| row.get(0),
                )
                .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
            if job.apply_operation_id.is_some() || active_plan {
                return Err("PRODUCTION_STAGE_CONFLICT".into());
            }
            Ok(Some((id, "EVIDENCE_RANGE_INVALID")))
        }
        ProductionAdaptationRunEvidence::EvidenceInvalid { .. } => {
            Err("PRODUCTION_EVIDENCE_INVALID_RETRY_REQUIRED".into())
        }
        ProductionAdaptationRunEvidence::BaselineMismatch => {
            Err("PRODUCTION_BASELINE_STALE".into())
        }
        ProductionAdaptationRunEvidence::None => Err("PRODUCTION_BASELINE_STALE".into()),
        ProductionAdaptationRunEvidence::Exact { .. } => {
            Err("PRODUCTION_ADAPTATION_UNRESOLVED".into())
        }
    }
}

fn production_retry_or_resume(
    conn: &Connection,
    command_name: &str,
    project_id: &str,
    novel_work_id: &str,
    production_job_id: &str,
    idempotency_key: &str,
    requested_stage: Option<&str>,
    allow_active_resume: bool,
) -> Result<NovelProductionJob, String> {
    let idempotency_key = ensure_nonempty(idempotency_key, "idempotencyKey")?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
    let receipt: Option<(String, String)> = tx
        .query_row(
            "SELECT request_hash,response_json FROM novel_operation_receipts WHERE command_name=? AND idempotency_key=?",
            params![command_name, idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
    // Check an existing receipt before the current job's status: a first
    // recovery may already have completed while this exact request was in
    // flight. Its replay returns current durable truth, never a stale queued
    // snapshot and never another attempt.
    if let Some((stored_hash, response_json)) = receipt {
        let recorded: NovelProductionJob = serde_json::from_str(&response_json)
            .map_err(|_| "PRODUCTION_RECOVERY_RECEIPT_INVALID".to_string())?;
        let expected_stage = requested_stage.unwrap_or(&recorded.stage);
        let request = json!({
            "projectId": project_id,
            "novelWorkId": novel_work_id,
            "productionJobId": production_job_id,
            "expectedStage": expected_stage,
        });
        if stored_hash != request_hash(&request)? {
            return Err("IDEMPOTENCY_MISMATCH".into());
        }
        tx.commit()
            .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
        return production_job_value(conn, project_id, novel_work_id, production_job_id);
    }
    let job = tx
        .query_row(
            &format!("{PRODUCTION_JOB_SELECT} WHERE id=? AND project_id=? AND novel_work_id=?"),
            params![production_job_id, project_id, novel_work_id],
            production_job_from_row,
        )
        .optional()
        .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?
        .ok_or("生产任务不存在或不属于当前小说")?;
    let expected_stage = requested_stage.unwrap_or(&job.stage);
    let request = json!({
        "projectId": project_id,
        "novelWorkId": novel_work_id,
        "productionJobId": production_job_id,
        "expectedStage": expected_stage,
    });
    if job.stage != expected_stage {
        return Err("PRODUCTION_STAGE_CONFLICT".into());
    }
    let retryable = matches!(
        job.status.as_str(),
        "error" | "stale" | "blocked_config" | "blocked_conflict" | "needs_rebase"
    );
    if !retryable {
        if allow_active_resume
            && matches!(
                job.status.as_str(),
                "queued" | "running" | "waiting_for_predecessor" | "succeeded"
            )
        {
            tx.execute(
                "INSERT INTO novel_operation_receipts (id,command_name,idempotency_key,request_hash,response_json,created_at) VALUES (?,?,?,?,?,?)",
                params![new_id("nreceipt"), command_name, idempotency_key, request_hash(&request)?, serde_json::to_string(&job).map_err(|_| "PRODUCTION_RECOVERY_RECEIPT_INVALID")?, now()],
            ).map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
            tx.commit()
                .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
            return Ok(job);
        }
        return Err("PRODUCTION_STAGE_NOT_RETRYABLE".into());
    }
    if allow_active_resume && matches!(job.status.as_str(), "blocked_conflict" | "needs_rebase") {
        return Err("PRODUCTION_RESUME_REQUIRES_STAGE_RETRY".into());
    }
    let replacement_adaptation_run = match production_adaptation_pointer_for_explicit_retry(
        &tx,
        &job,
        command_name,
        expected_stage,
    ) {
        Ok(run) => run,
        Err(error) if error == "PRODUCTION_EVIDENCE_INVALID_RETRY_REQUIRED" => {
            let changed = tx.execute(
                "UPDATE novel_production_jobs SET status='blocked_conflict',safe_error_code='ADAPTATION_EVIDENCE_INVALID',safe_user_message='改编引用无效，需重新生成改编（可能计费）。',next_action='重新生成改编（可能计费）',updated_at=?,finished_at=? WHERE id=? AND project_id=? AND novel_work_id=? AND attempt_no=? AND stage=? AND status IN ('error','stale','blocked_config')",
                params![now(),now(),job.id,project_id,novel_work_id,job.attempt_no,expected_stage],
            ).map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
            if changed != 1 {
                return Err("PRODUCTION_STAGE_CONFLICT".into());
            }
            let result = tx
                .query_row(
                    &format!("{PRODUCTION_JOB_SELECT} WHERE id=?"),
                    params![job.id],
                    production_job_from_row,
                )
                .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
            production_event(
                &tx,
                &result,
                "adaptation_evidence_invalid",
                json!({"reason":"EVIDENCE_RANGE_INVALID"}),
            )?;
            tx.execute(
                "INSERT INTO novel_operation_receipts (id,command_name,idempotency_key,request_hash,response_json,created_at) VALUES (?,?,?,?,?,?)",
                params![new_id("nreceipt"),command_name,idempotency_key,request_hash(&request)?,serde_json::to_string(&result).map_err(|_| "PRODUCTION_RECOVERY_RECEIPT_INVALID")?,now()],
            ).map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
            tx.commit()
                .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
            return Ok(result);
        }
        Err(error) => return Err(error),
    };
    let changed = if replacement_adaptation_run.is_some() {
        tx.execute(
            "UPDATE novel_production_jobs SET status='queued',attempt_no=attempt_no+1,adaptation_analysis_run_id=NULL,safe_error_code=NULL,safe_user_message=NULL,next_action='重试当前阶段',updated_at=?,finished_at=NULL WHERE id=? AND project_id=? AND novel_work_id=? AND attempt_no=? AND stage=? AND status IN ('error','stale','blocked_config','blocked_conflict','needs_rebase')",
            params![now(), job.id, project_id, novel_work_id, job.attempt_no, expected_stage],
        )
    } else {
        tx.execute(
            "UPDATE novel_production_jobs SET status='queued',attempt_no=attempt_no+1,safe_error_code=NULL,safe_user_message=NULL,next_action='重试当前阶段',updated_at=?,finished_at=NULL WHERE id=? AND project_id=? AND novel_work_id=? AND attempt_no=? AND stage=? AND status IN ('error','stale','blocked_config','blocked_conflict','needs_rebase')",
            params![now(), job.id, project_id, novel_work_id, job.attempt_no, expected_stage],
        )
    }
    .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
    if changed != 1 {
        return Err("PRODUCTION_STAGE_CONFLICT".into());
    }
    let result = tx
        .query_row(
            &format!("{PRODUCTION_JOB_SELECT} WHERE id=?"),
            params![job.id],
            production_job_from_row,
        )
        .map_err(|_| "PRODUCTION_RECOVERY_READ_FAILED".to_string())?;
    production_event(
        &tx,
        &result,
        "retry_requested",
        json!({"idempotencyKey":idempotency_key,"stage":expected_stage,"command":command_name}),
    )?;
    if let Some((previous_run_id, reason)) = replacement_adaptation_run {
        production_event(
            &tx,
            &result,
            "adaptation_retry_authorized",
            json!({"previousAdaptationAnalysisRunId":previous_run_id,"reason":reason}),
        )?;
    }
    tx.execute(
        "INSERT INTO novel_operation_receipts (id,command_name,idempotency_key,request_hash,response_json,created_at) VALUES (?,?,?,?,?,?)",
        params![new_id("nreceipt"), command_name, idempotency_key, request_hash(&request)?, serde_json::to_string(&result).map_err(|_| "PRODUCTION_RECOVERY_RECEIPT_INVALID")?, now()],
    ).map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
    tx.commit()
        .map_err(|_| "PRODUCTION_RECOVERY_WRITE_FAILED".to_string())?;
    Ok(result)
}

#[tauri::command]
pub fn novel_production_resume(
    state: tauri::State<'_, DbState>,
    app_handle: tauri::AppHandle,
    input: NovelProductionJobInput,
) -> Result<NovelProductionJob, String> {
    let job = db::with_connection(&state, |conn| {
        production_retry_or_resume(
            conn,
            "novel_production_resume",
            &input.project_id,
            &input.novel_work_id,
            &input.production_job_id,
            &input.idempotency_key,
            None,
            true,
        )
    })?;
    if job.status == "succeeded" {
        crate::comic_visual_batch::schedule_for_production_job(&app_handle, &job.id);
    } else if production_resume_should_schedule(&job) {
        production_schedule(app_handle, job.clone(), None, None);
    }
    Ok(job)
}

#[tauri::command]
pub fn novel_production_retry_stage(
    state: tauri::State<'_, DbState>,
    app_handle: tauri::AppHandle,
    input: NovelProductionRetryStageInput,
) -> Result<NovelProductionJob, String> {
    let job = db::with_connection(&state, |conn| {
        production_retry_or_resume(
            conn,
            "novel_production_retry_stage",
            &input.project_id,
            &input.novel_work_id,
            &input.production_job_id,
            &input.idempotency_key,
            Some(&input.stage),
            false,
        )
    })?;
    // A replay can observe a later running/succeeded row. Only a still-queued
    // recovery needs a safe wake; `production_claim_source` owns the final
    // duplicate-dispatch guard.
    if job.status == "queued" {
        production_schedule(app_handle, job.clone(), None, None);
    }
    Ok(job)
}

fn fail_analysis_inner(
    conn: &Connection,
    run_id: &str,
    owner: &str,
    error: &str,
) -> Result<NovelAnalysisRun, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|error| format!("开始分析失败事务失败: {error}"))?;
    let changed = tx.execute("UPDATE source_analysis_runs SET status='error', safe_error_code='ANALYSIS_FAILED', safe_user_message=?, updated_at=?, completed_at=? WHERE id=? AND status='running'", params![safe_error(error), now(), now(), run_id]).map_err(|error| format!("保存分析失败状态失败: {error}"))?;
    if changed != 1 {
        return Err("分析 run 状态已变化".into());
    }
    let attempt_changed = tx.execute("UPDATE source_analysis_run_attempts SET status='error', safe_error_code='ANALYSIS_FAILED', safe_user_message=?, finished_at=? WHERE source_analysis_run_id=? AND status='running' AND lease_owner=? AND lease_expires_at >= ?", params![safe_error(error), now(), run_id, owner, now()]).map_err(|error| format!("保存分析 attempt 失败: {error}"))?;
    if attempt_changed != 1 {
        return Err("分析执行权已失效".into());
    }
    let result = tx.query_row("SELECT r.id, l.novel_work_id, r.novel_chapter_revision_id, r.novel_analysis_lineage_id, r.base_working_context_revision_id, r.status, 1, r.safe_error_code FROM source_analysis_runs r JOIN novel_analysis_lineages l ON l.id=r.novel_analysis_lineage_id WHERE r.id=?", params![run_id], analysis_run_from_row).map_err(|error| format!("读取失败分析 run 失败: {error}"))?;
    tx.commit()
        .map_err(|error| format!("提交分析失败事务失败: {error}"))?;
    Ok(result)
}

fn complete_analysis_inner(
    conn: &Connection,
    run: &NovelAnalysisRun,
    artifacts: Vec<(String, Value)>,
    owner: &str,
) -> Result<NovelAnalysisRun, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|error| format!("开始分析完成事务失败: {error}"))?;
    let (sequence, lineage_version, continuous, current_parent, canon, state, run_canon, run_state, parent_canon, parent_state, adaptation_id, adaptation_chapter_id): CompletionBase = tx.query_row("SELECT chapter.sequence_no, lineage.optimistic_version, lineage.continuous_through_sequence_no, lineage.current_context_revision_id, work.published_canon_version_id, work.current_novel_state_version_id, run.base_canon_version_id, run.base_novel_state_version_id, COALESCE(parent.resolved_working_canon_json, '{}'), COALESCE(parent.resolved_working_state_json, '{}'), run.frozen_comic_adaptation_id, run.frozen_comic_chapter_id FROM source_analysis_runs run JOIN novel_chapter_revisions revision ON revision.id=run.novel_chapter_revision_id JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id JOIN novel_analysis_lineages lineage ON lineage.id=run.novel_analysis_lineage_id JOIN novel_works work ON work.id=lineage.novel_work_id LEFT JOIN novel_chapter_context_revisions parent ON parent.id=run.base_working_context_revision_id WHERE run.id=? AND run.status='running'", params![run.id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?,row.get(11)?))).optional().map_err(|error| format!("读取完成前分析 run 失败: {error}"))?.ok_or("分析 run 不在运行状态")?;
    let owns_active_attempt: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_analysis_run_attempts WHERE source_analysis_run_id=? AND status='running' AND lease_owner=? AND lease_expires_at>=?)",
        params![run.id, owner, now()], |row| row.get(0),
    ).map_err(|error| format!("验证分析执行权失败: {error}"))?;
    if !owns_active_attempt {
        return Err("分析执行权已失效".into());
    }
    let detached = sequence != continuous + 1 || current_parent != run.parent_context_revision_id;
    let context_id = new_id("ncontext");
    let timestamp = now();
    let mut bodies = serde_json::Map::new();
    let mut artifact_revisions = Vec::new();
    for (kind, content) in artifacts {
        bodies.insert(kind.clone(), content.clone());
        let artifact_id = new_id("nartifact");
        let revision_id = new_id("nartifactrev");
        let comic_scope = matches!(
            kind.as_str(),
            "adaptation_proposal" | "comic_chapter_plan" | "scene_plan" | "page_panel_plan"
        );
        let chapter_scope = matches!(
            kind.as_str(),
            "comic_chapter_plan" | "scene_plan" | "page_panel_plan"
        );
        tx.execute("INSERT INTO analysis_artifacts (id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,candidate_head_revision_id,adopted_head_revision_id,status,optimistic_version,created_at,updated_at) VALUES (?, ?, NULL, ?, (SELECT novel_work_id FROM novel_analysis_lineages WHERE id=?), ?, ?, ?, ?, NULL, 'active',0,?,?)", params![artifact_id,run.id,kind,run.lineage_id,run.chapter_revision_id,if comic_scope {Some(adaptation_id.clone())} else {None},if chapter_scope {Some(adaptation_chapter_id.clone())} else {None},revision_id,timestamp,timestamp]).map_err(|error| format!("保存分析产物失败: {error}"))?;
        tx.execute("INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?, ?,1,NULL,?,'','ai_analysis','{}','{}','candidate',?)", params![revision_id,artifact_id,content.to_string(),timestamp]).map_err(|error| format!("保存分析产物版本失败: {error}"))?;
        artifact_revisions.push((revision_id, kind));
    }
    let canon_kinds = [
        "world_facts",
        "character_facts",
        "faction_facts",
        "location_facts",
        "prop_facts",
        "timeline_delta",
    ];
    let state_kinds = ["continuity_delta", "open_threads"];
    let resolved_canon = merge_working(
        json_value(parent_canon),
        bodies
            .iter()
            .filter(|(kind, _)| canon_kinds.contains(&kind.as_str()))
            .map(|(kind, value)| (kind.clone(), value.clone())),
    );
    let resolved_state = merge_working(
        json_value(parent_state),
        bodies
            .iter()
            .filter(|(kind, _)| state_kinds.contains(&kind.as_str()))
            .map(|(kind, value)| (kind.clone(), value.clone())),
    );
    let canon_hash = request_hash(&resolved_canon)?;
    let state_hash = request_hash(&resolved_state)?;
    let gap = if detached {
        json!([sequence])
    } else {
        json!([])
    };
    let baseline_stale = canon != run_canon || state != run_state;
    let context_status = if detached || baseline_stale {
        "provisional"
    } else {
        "ready"
    };
    let branch_kind = if detached {
        "detached_gap"
    } else if baseline_stale {
        "conflicted"
    } else {
        "main"
    };
    tx.execute("INSERT INTO novel_chapter_context_revisions (id,novel_analysis_lineage_id,novel_chapter_revision_id,parent_context_revision_id,source_analysis_run_id,base_published_canon_version_id,base_published_novel_state_version_id,resolved_working_canon_json,resolved_working_canon_hash,resolved_working_state_json,resolved_working_state_hash,sequence_gap_json,branch_kind,status,created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", params![context_id,run.lineage_id,run.chapter_revision_id,run.parent_context_revision_id,run.id,run_canon,run_state,resolved_canon.to_string(),canon_hash,resolved_state.to_string(),state_hash,gap.to_string(),branch_kind,context_status,timestamp]).map_err(|error| format!("保存工作上下文失败: {error}"))?;
    for (source_order, (revision_id, kind)) in artifact_revisions.iter().enumerate() {
        tx.execute("INSERT INTO novel_chapter_context_artifact_revisions (context_revision_id,analysis_artifact_revision_id,role,source_order) VALUES (?, ?, ?, ?)", params![context_id,revision_id,kind,source_order as i64]).map_err(|error| format!("关联工作上下文产物失败: {error}"))?;
    }
    // Candidates are explicit fact items only; free text never becomes an entity.
    for (revision_id, kind) in &artifact_revisions {
        let Some(entity_kind) = (match kind.as_str() {
            "world_facts" => Some("world_rule"),
            "character_facts" => Some("character"),
            "faction_facts" => Some("faction"),
            "location_facts" => Some("location"),
            "prop_facts" => Some("prop"),
            _ => None,
        }) else {
            continue;
        };
        let Some(items) = bodies
            .get(kind)
            .and_then(|body| body.get("items"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for (source_order, item) in items.iter().enumerate() {
            let Some(stable_key) = item
                .get("stableKey")
                .or_else(|| item.get("stable_key"))
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|key| !key.is_empty() && key.len() <= 128)
            else {
                return Err("事实候选缺少 stableKey".into());
            };
            let resolution = item
                .get("resolutionKind")
                .and_then(Value::as_str)
                .ok_or("事实候选缺少 resolutionKind")?;
            let (entity_id, usage_role) = match resolution {
                "new_entity_candidate" => {
                    let candidate = item
                        .get("candidateNovelEntityId")
                        .and_then(Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                        .ok_or("新候选缺少 candidateNovelEntityId")?;
                    let existing: Option<String> = tx
                        .query_row(
                            "SELECT id FROM novel_entities WHERE id=? AND novel_work_id=?",
                            params![candidate, run.novel_work_id],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(|e| e.to_string())?;
                    if existing.is_some() {
                        return Err("candidateNovelEntityId 已属于现有实体".into());
                    }
                    let entity_id = new_id("nentity");
                    tx.execute("INSERT INTO novel_entities (id,novel_work_id,entity_kind,stable_key,lifecycle,candidate_lineage_id,created_from_context_revision_id,created_from_canon_version_id,created_at,updated_at) VALUES (?,?,?,?, 'candidate', ?, ?, NULL, ?, ?)",params![entity_id,run.novel_work_id,entity_kind,stable_key,run.lineage_id,context_id,timestamp,timestamp]).map_err(|e|format!("保存候选实体失败: {e}"))?;
                    (entity_id, "candidate")
                }
                "field_delta" | "no_change" => {
                    let existing = item
                        .get("existingNovelEntityId")
                        .and_then(Value::as_str)
                        .ok_or("既有实体变更缺少 existingNovelEntityId")?;
                    let entity: String = tx
                        .query_row(
                            "SELECT id FROM novel_entities WHERE id=? AND novel_work_id=?",
                            params![existing, run.novel_work_id],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(|e| e.to_string())?
                        .ok_or("既有实体不属于当前小说")?;
                    (entity, "existing")
                }
                "conflict" | "unresolved" => continue,
                _ => return Err("未知 resolutionKind".into()),
            };
            tx.execute("INSERT OR REPLACE INTO novel_chapter_context_entity_refs (context_revision_id,novel_entity_id,origin_context_revision_id,source_artifact_revision_id,usage_role,resolved_entity_json,resolved_entity_hash,source_order) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",params![context_id,entity_id,context_id,revision_id,usage_role,item.to_string(),request_hash(item)?,source_order as i64]).map_err(|error|format!("关联候选实体失败: {error}"))?;
        }
    }
    let mut conflicted = detached || baseline_stale;
    if !conflicted {
        let changed=tx.execute("UPDATE novel_analysis_lineages SET current_context_revision_id=?,continuous_through_sequence_no=?,optimistic_version=optimistic_version+1,updated_at=? WHERE id=? AND current_context_revision_id IS ? AND continuous_through_sequence_no=? AND optimistic_version=?", params![context_id,sequence,timestamp,run.lineage_id,run.parent_context_revision_id,sequence-1,lineage_version]).map_err(|error| format!("推进工作线失败: {error}"))?;
        if changed != 1 {
            conflicted = true;
        }
    }
    if conflicted && !detached {
        tx.execute("UPDATE novel_chapter_context_revisions SET branch_kind='conflicted',status='conflicted' WHERE id=?", params![context_id]).map_err(|error| format!("标记冲突上下文失败: {error}"))?;
    }
    tx.execute("UPDATE source_analysis_runs SET status='ready_for_review', safe_error_code=?, safe_user_message=?, updated_at=?,completed_at=? WHERE id=? AND status='running'",params![if conflicted {Some("BASELINE_STALE")} else {Option::<&str>::None},if conflicted {Some("基准已变化，分析结果已保留待重新基准化")} else {Option::<&str>::None},timestamp,timestamp,run.id]).map_err(|error|format!("更新分析状态失败: {error}"))?;
    let attempt_changed=tx.execute("UPDATE source_analysis_run_attempts SET status='success',finished_at=? WHERE source_analysis_run_id=? AND status='running' AND lease_owner=? AND lease_expires_at>=?",params![timestamp,run.id,owner,now()]).map_err(|error|format!("更新分析 attempt 失败: {error}"))?;
    if attempt_changed != 1 {
        return Err("分析执行权已失效".into());
    }
    tx.commit()
        .map_err(|error| format!("提交分析完成事务失败: {error}"))?;
    Ok(NovelAnalysisRun {
        status: "ready_for_review".into(),
        safe_error: conflicted.then(|| "BASELINE_STALE".into()),
        ..run.clone()
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactRevision {
    pub id: String,
    pub artifact_id: String,
    pub novel_work_id: String,
    pub revision_no: i64,
    pub parent_revision_id: Option<String>,
    pub status: String,
    pub content: Value,
    pub created_at: i64,
    pub change_note: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifact {
    pub id: String,
    pub project_id: String,
    pub novel_work_id: String,
    pub source_analysis_run_id: Option<String>,
    pub artifact_type: String,
    pub current_revision_id: Option<String>,
    pub adopted_head_revision_id: Option<String>,
    pub adopted_revision: Option<NovelArtifactRevision>,
    pub comic_adaptation_id: Option<String>,
    pub comic_chapter_id: Option<String>,
    pub adaptation_analysis_run_id: Option<String>,
    pub current_revision: Option<NovelArtifactRevision>,
    pub source_chapter_id: Option<String>,
    pub source_chapter_no: Option<i64>,
    pub source_sequence_no: Option<i64>,
    pub classification: String,
    pub inherited_status: Option<String>,
    pub optimistic_version: i64,
    pub updated_at: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactListInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub artifact_types: Option<Vec<String>>,
    pub comic_adaptation_id: Option<String>,
    pub comic_chapter_id: Option<String>,
    pub adaptation_analysis_run_id: Option<String>,
    pub revision_status: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactGetInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub artifact_id: String,
}

fn artifact_from_row(
    conn: &Connection,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<NovelArtifact> {
    let revision_id: Option<String> = row.get(4)?;
    let adopted_revision_id: Option<String> = row.get(5)?;
    let revision_value = |id: &str| {
        conn.query_row("SELECT id,analysis_artifact_id,(SELECT novel_work_id FROM analysis_artifacts WHERE id=analysis_artifact_id),version,parent_revision_id,status,body_json,created_at,change_instruction FROM analysis_artifact_revisions WHERE id=?",params![id],|r|Ok(NovelArtifactRevision{id:r.get(0)?,artifact_id:r.get(1)?,novel_work_id:r.get(2)?,revision_no:r.get(3)?,parent_revision_id:r.get(4)?,status:r.get(5)?,content:json_value(r.get(6)?),created_at:r.get(7)?,change_note:r.get(8)?})).ok()
    };
    let revision = revision_id.as_deref().and_then(revision_value);
    let adopted_revision = adopted_revision_id.as_deref().and_then(revision_value);
    Ok(NovelArtifact {
        id: row.get(0)?,
        project_id: row.get(1)?,
        novel_work_id: row.get(2)?,
        source_analysis_run_id: row.get(16)?,
        artifact_type: row.get(3)?,
        current_revision_id: revision_id,
        adopted_head_revision_id: adopted_revision_id,
        adopted_revision,
        comic_adaptation_id: row.get(6)?,
        comic_chapter_id: row.get(7)?,
        adaptation_analysis_run_id: row.get(8)?,
        current_revision: revision,
        source_chapter_id: row.get(9)?,
        source_chapter_no: row.get(10)?,
        source_sequence_no: row.get(11)?,
        classification: row.get(12)?,
        inherited_status: row.get(13)?,
        optimistic_version: row.get(14)?,
        updated_at: row.get(15)?,
    })
}
pub(crate) fn list_artifacts_inner(
    conn: &Connection,
    input: &NovelArtifactListInput,
) -> Result<Vec<NovelArtifact>, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    let mut stmt=conn.prepare("SELECT a.id,w.project_id,a.novel_work_id,a.artifact_type,a.candidate_head_revision_id,a.adopted_head_revision_id,a.comic_adaptation_id,a.comic_chapter_id,a.adaptation_analysis_run_id,
        chapter.id,chapter.chapter_no,chapter.sequence_no,
        CASE
          WHEN a.artifact_type IN ('world_facts','character_facts','faction_facts','location_facts','prop_facts') THEN 'canon_delta'
          WHEN a.artifact_type IN ('timeline_delta','continuity_delta','open_threads') THEN 'state_delta'
          WHEN a.artifact_type IN ('chapter_summary','chapter_beats') THEN 'chapter_local'
          ELSE 'adaptation'
        END,
        context.status,a.optimistic_version,a.updated_at,a.source_analysis_run_id
      FROM analysis_artifacts a
      JOIN novel_works w ON w.id=a.novel_work_id
      LEFT JOIN source_analysis_runs run ON run.id=a.source_analysis_run_id
      LEFT JOIN novel_chapter_revisions chapter_revision ON chapter_revision.id=COALESCE(a.novel_chapter_revision_id,run.novel_chapter_revision_id)
      LEFT JOIN novel_chapters chapter ON chapter.id=chapter_revision.novel_chapter_id
      LEFT JOIN novel_chapter_context_revisions context ON context.source_analysis_run_id=run.id
      WHERE a.novel_work_id=? AND a.status='active' AND (? IS NULL OR a.adaptation_analysis_run_id=?) ORDER BY a.updated_at DESC,a.id DESC").map_err(|e|format!("准备产物列表失败: {e}"))?;
    let rows = stmt
        .query_map(
            params![
                work.id,
                input.adaptation_analysis_run_id,
                input.adaptation_analysis_run_id
            ],
            |row| artifact_from_row(conn, row),
        )
        .map_err(|e| format!("查询产物列表失败: {e}"))?;
    let mut items = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取产物失败: {e}"))?;
    if let Some(types) = &input.artifact_types {
        items.retain(|item| types.contains(&item.artifact_type));
    }
    if let Some(adaptation_id) = &input.comic_adaptation_id {
        items.retain(|item| item.comic_adaptation_id.as_deref() == Some(adaptation_id));
    }
    if let Some(chapter_id) = &input.comic_chapter_id {
        items.retain(|item| item.comic_chapter_id.as_deref() == Some(chapter_id));
    }
    if input.revision_status.as_deref() == Some("adopted") {
        items.retain(|item| item.adopted_head_revision_id.is_some());
    }
    Ok(items)
}
#[tauri::command]
pub fn novel_artifact_list(
    state: tauri::State<'_, DbState>,
    input: NovelArtifactListInput,
) -> Result<Vec<NovelArtifact>, String> {
    db::with_connection(&state, |conn| list_artifacts_inner(conn, &input))
}
#[tauri::command]
pub fn novel_artifact_get(
    state: tauri::State<'_, DbState>,
    input: NovelArtifactGetInput,
) -> Result<NovelArtifact, String> {
    db::with_connection(&state, |conn| {
        let items = list_artifacts_inner(
            conn,
            &NovelArtifactListInput {
                project_id: input.project_id,
                novel_work_id: input.novel_work_id,
                artifact_types: None,
                comic_adaptation_id: None,
                comic_chapter_id: None,
                adaptation_analysis_run_id: None,
                revision_status: None,
            },
        )?;
        items
            .into_iter()
            .find(|item| item.id == input.artifact_id)
            .ok_or_else(|| "产物不存在或不属于当前小说".into())
    })
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactUpdateInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub artifact_id: String,
    pub parent_revision_id: String,
    pub expected_optimistic_version: i64,
    pub content: Value,
    pub change_note: Option<String>,
    pub idempotency_key: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactMutationResult {
    pub artifact_id: String,
    pub candidate_revision_id: String,
    pub revision_status: String,
    pub optimistic_version: i64,
}
#[tauri::command]
pub fn novel_artifact_update(
    state: tauri::State<'_, DbState>,
    input: NovelArtifactUpdateInput,
) -> Result<NovelArtifactMutationResult, String> {
    db::with_connection(&state, |conn| artifact_update_inner(conn, input))
}
fn artifact_update_inner(
    conn: &Connection,
    input: NovelArtifactUpdateInput,
) -> Result<NovelArtifactMutationResult, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    if !input.content.is_object() {
        return Err("content 必须是对象".into());
    }
    let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
    let key = input.idempotency_key.clone();
    with_receipt(conn, "novel_artifact_update", &key, &request, move |tx| {
        ensure_active_work_in_tx(tx, &work)?;
        let (head,version):(Option<String>,i64)=tx.query_row("SELECT candidate_head_revision_id,optimistic_version FROM analysis_artifacts WHERE id=? AND novel_work_id=?",params![input.artifact_id,work.id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|format!("读取产物失败: {e}"))?.ok_or("产物不存在或不属于当前小说")?;
        if head.as_deref() != Some(&input.parent_revision_id)
            || version != input.expected_optimistic_version
        {
            return Err("产物版本已变化，请重新基准化".into());
        }
        let no:i64=tx.query_row("SELECT COALESCE(MAX(version),0)+1 FROM analysis_artifact_revisions WHERE analysis_artifact_id=?",params![input.artifact_id],|r|r.get(0)).map_err(|e|e.to_string())?;
        let id = new_id("nartifactrev");
        tx.execute("INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,change_instruction,provenance_json,validation_json,status,created_at) VALUES (?, ?, ?, ?, ?, '', 'manual_edit', ?, '{}','{}','candidate',?)",params![id,input.artifact_id,no,input.parent_revision_id,input.content.to_string(),input.change_note,now()]).map_err(|e|format!("保存产物版本失败: {e}"))?;
        let changed=tx.execute("UPDATE analysis_artifacts SET candidate_head_revision_id=?,optimistic_version=optimistic_version+1,updated_at=? WHERE id=? AND optimistic_version=?",params![id,now(),input.artifact_id,version]).map_err(|e|e.to_string())?;
        if changed != 1 {
            return Err("产物并发更新冲突".into());
        }
        Ok(NovelArtifactMutationResult {
            artifact_id: input.artifact_id,
            candidate_revision_id: id,
            revision_status: "candidate".into(),
            optimistic_version: version + 1,
        })
    })
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactAdoptInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub artifact_id: String,
    pub revision_id: String,
    pub expected_optimistic_version: i64,
    pub approval_token: String,
    pub idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactAdoptPreviewInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub artifact_id: String,
    pub revision_id: String,
    pub expected_optimistic_version: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactAdoptPreviewResult {
    pub approval_token: String,
    pub preview_fingerprint: String,
    pub summary: String,
    pub expires_at: i64,
}

#[tauri::command]
pub fn novel_artifact_adopt_preview(
    state: tauri::State<'_, DbState>,
    input: NovelArtifactAdoptPreviewInput,
) -> Result<NovelArtifactAdoptPreviewResult, String> {
    db::with_connection(&state, |conn| {
        novel_artifact_adopt_preview_inner(conn, input)
    })
}
fn novel_artifact_adopt_preview_inner(
    conn: &Connection,
    input: NovelArtifactAdoptPreviewInput,
) -> Result<NovelArtifactAdoptPreviewResult, String> {
    {
        let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
        ensure_active_work(&work)?;
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(|error| format!("开始采用预览事务失败: {error}"))?;
        ensure_active_work_in_tx(&tx, &work)?;
        let body: String = tx.query_row(
            "SELECT revision.body_json FROM analysis_artifacts artifact
             JOIN analysis_artifact_revisions revision ON revision.id=artifact.candidate_head_revision_id
             WHERE artifact.id=? AND artifact.novel_work_id=? AND artifact.optimistic_version=?
               AND revision.id=? AND revision.status='candidate'",
            params![input.artifact_id, work.id, input.expected_optimistic_version, input.revision_id],
            |row| row.get(0),
        ).optional().map_err(|error| format!("读取采用预览失败: {error}"))?
         .ok_or("产物不存在、候选版本无效或版本已变化")?;
        let fingerprint = request_hash(
            &json!({"artifactId":input.artifact_id,"revisionId":input.revision_id,"version":input.expected_optimistic_version,"body":json_value(body)}),
        )?;
        let timestamp = now();
        let token = new_id("nadopt");
        let token_hash = request_hash(&Value::String(token.clone()))?;
        let expires_at = timestamp + 15 * 60 * 1000;
        tx.execute("INSERT INTO novel_artifact_adoption_previews (token_hash,novel_work_id,analysis_artifact_id,analysis_artifact_revision_id,expected_optimistic_version,preview_fingerprint,expires_at,used_at,created_at) VALUES (?,?,?,?,?,?,?,?,?)",params![token_hash,work.id,input.artifact_id,input.revision_id,input.expected_optimistic_version,fingerprint,expires_at,Option::<i64>::None,timestamp]).map_err(|error|format!("保存采用预览失败: {error}"))?;
        tx.commit()
            .map_err(|error| format!("提交采用预览事务失败: {error}"))?;
        Ok(NovelArtifactAdoptPreviewResult {
            approval_token: token,
            preview_fingerprint: fingerprint,
            summary: "候选版本已绑定为一次性采用令牌".into(),
            expires_at,
        })
    }
}

#[tauri::command]
pub fn novel_artifact_adopt(
    state: tauri::State<'_, DbState>,
    input: NovelArtifactAdoptInput,
) -> Result<NovelArtifactMutationResult, String> {
    db::with_connection(&state, |conn| novel_artifact_adopt_inner(conn, input))
}
fn novel_artifact_adopt_inner(
    conn: &Connection,
    input: NovelArtifactAdoptInput,
) -> Result<NovelArtifactMutationResult, String> {
    {
        let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
        ensure_active_work(&work)?;
        if input.approval_token.trim().is_empty() {
            return Err("approvalToken 不能为空".into());
        }
        let request = serde_json::to_value(&input).map_err(|e| e.to_string())?;
        let key = input.idempotency_key.clone();
        with_receipt(conn, "novel_artifact_adopt", &key, &request, move |tx| {
            ensure_active_work_in_tx(tx, &work)?;
            let token_hash = request_hash(&Value::String(input.approval_token.clone()))?;
            let consumed = tx
                .execute(
                    "UPDATE novel_artifact_adoption_previews SET used_at=?
                 WHERE token_hash=? AND novel_work_id=? AND analysis_artifact_id=?
                   AND analysis_artifact_revision_id=? AND expected_optimistic_version=?
                   AND used_at IS NULL AND expires_at>=?",
                    params![
                        now(),
                        token_hash,
                        work.id,
                        input.artifact_id,
                        input.revision_id,
                        input.expected_optimistic_version,
                        now()
                    ],
                )
                .map_err(|error| format!("验证采用令牌失败: {error}"))?;
            if consumed != 1 {
                return Err("approvalToken 无效、已使用或已过期".into());
            }
            let changed=tx.execute("UPDATE analysis_artifacts SET adopted_head_revision_id=?,optimistic_version=optimistic_version+1,updated_at=? WHERE id=? AND novel_work_id=? AND optimistic_version=? AND EXISTS(SELECT 1 FROM analysis_artifact_revisions WHERE id=? AND analysis_artifact_id=analysis_artifacts.id AND status='candidate')",params![input.revision_id,now(),input.artifact_id,work.id,input.expected_optimistic_version,input.revision_id]).map_err(|e|format!("采用产物失败: {e}"))?;
            if changed != 1 {
                return Err("产物不存在、版本冲突或候选版本无效".into());
            }
            tx.execute(
                "UPDATE analysis_artifact_revisions SET status='adopted' WHERE id=?",
                params![input.revision_id],
            )
            .map_err(|e| e.to_string())?;
            // A user adoption is an explicit dependency change.  It must not
            // silently leave a completed production tree claiming to reflect
            // the old source/adaptation revision.  Automatic production
            // adoption occurs while the job is running and is deliberately
            // excluded by the succeeded-state predicate below.
            let mut affected_stmt=tx.prepare(&format!("SELECT job.id,CASE WHEN artifact.source_analysis_run_id=job.source_analysis_run_id THEN 'source' ELSE 'adaptation' END FROM novel_production_jobs job JOIN analysis_artifacts artifact ON artifact.id=? WHERE job.novel_work_id=? AND job.status='succeeded' AND (artifact.source_analysis_run_id=job.source_analysis_run_id OR artifact.adaptation_analysis_run_id=job.adaptation_analysis_run_id)")).map_err(|e|e.to_string())?;
            let affected = affected_stmt
                .query_map(params![input.artifact_id, work.id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            drop(affected_stmt);
            for (job_id, origin) in affected {
                let ts = now();
                tx.execute("UPDATE novel_production_jobs SET status='needs_rebase',stage='integrating_context',stage_index=3,lease_owner=NULL,lease_expires_at=NULL,safe_error_code='PRODUCTION_INPUT_ADOPTED',safe_user_message='已采用新的上游产物版本，需要更新下游产物。',next_action='更新受影响的下游产物',updated_at=?,finished_at=? WHERE id=? AND status='succeeded'",params![ts,ts,job_id]).map_err(|e|e.to_string())?;
                let job = tx
                    .query_row(
                        &format!("{PRODUCTION_JOB_SELECT} WHERE id=?"),
                        params![job_id],
                        production_job_from_row,
                    )
                    .map_err(|e| e.to_string())?;
                production_event(
                    tx,
                    &job,
                    "manual_adoption_invalidated_production",
                    json!({"artifactId":input.artifact_id,"revisionId":input.revision_id,"origin":origin}),
                )?;
            }
            Ok(NovelArtifactMutationResult {
                artifact_id: input.artifact_id,
                candidate_revision_id: input.revision_id,
                revision_status: "adopted".into(),
                optimistic_version: input.expected_optimistic_version + 1,
            })
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactOptimizeStatusResult {
    pub artifact_optimization_run_id: String,
    pub operation_id: Option<String>,
    pub status: String,
    pub attempt_no: i64,
    pub parent_revision_id: Option<String>,
    pub candidate_revision_id: Option<String>,
    pub validation_report: Option<Value>,
    pub receipt_id: Option<String>,
    pub safe_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactOptimizeStartInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub artifact_id: String,
    pub parent_revision_id: String,
    pub expected_optimistic_version: i64,
    pub instruction: String,
    pub provider_id: String,
    pub model_id: String,
    pub selected_source_revision_ids: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactOptimizeStatusInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub artifact_optimization_run_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactOptimizeRecoverInput {
    pub project_id: String,
    pub novel_work_id: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactOptimizeRetryInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub artifact_optimization_run_id: String,
    pub idempotency_key: String,
}

fn optimize_status_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<NovelArtifactOptimizeStatusResult> {
    Ok(NovelArtifactOptimizeStatusResult {
        artifact_optimization_run_id: row.get(0)?,
        operation_id: row.get(1)?,
        status: row.get(2)?,
        attempt_no: row.get(3)?,
        parent_revision_id: row.get(4)?,
        candidate_revision_id: row.get(5)?,
        validation_report: row.get::<_, Option<String>>(6)?.map(json_value),
        receipt_id: None,
        safe_error: row.get(7)?,
    })
}

fn optimize_start_inner(
    conn: &Connection,
    input: NovelArtifactOptimizeStartInput,
    configured: bool,
    owner: &str,
) -> Result<NovelArtifactOptimizeStatusResult, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    ensure_nonempty(&input.instruction, "instruction")?;
    ensure_nonempty(&input.provider_id, "providerId")?;
    ensure_nonempty(&input.model_id, "modelId")?;
    if input.selected_source_revision_ids.len() > 64 {
        return Err("selectedSourceRevisionIds 不能超过 64 项".into());
    }
    let request = serde_json::to_value(&input).map_err(|e| format!("序列化优化请求失败: {e}"))?;
    let key = input.idempotency_key.clone();
    with_receipt(
        conn,
        "novel_artifact_optimize_start",
        &key,
        &request,
        move |tx| {
            ensure_active_work_in_tx(tx, &work)?;
            let parent_body: String = tx.query_row(
            "SELECT r.body_json FROM analysis_artifacts a JOIN analysis_artifact_revisions r ON r.id=a.candidate_head_revision_id
             WHERE a.id=? AND a.novel_work_id=? AND a.optimistic_version=? AND r.id=?",
            params![input.artifact_id, work.id, input.expected_optimistic_version, input.parent_revision_id], |r| r.get(0),
        ).optional().map_err(|e| format!("读取待优化产物失败: {e}"))?.ok_or("产物不存在或版本已变化，请重新基准化")?;
            let input_bytes = parent_body.len() + input.instruction.len();
            if input_bytes > MAX_ANALYSIS_PROMPT_BYTES {
                return Err("优化输入超过上限".into());
            }
            for revision_id in &input.selected_source_revision_ids {
                let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM analysis_artifact_revisions r JOIN analysis_artifacts a ON a.id=r.analysis_artifact_id WHERE r.id=? AND a.novel_work_id=?)", params![revision_id,work.id], |r|r.get(0)).map_err(|e|e.to_string())?;
                if !valid {
                    return Err("选择的来源版本不存在或不属于当前小说".into());
                }
            }
            let timestamp = now();
            let id = new_id("nopt");
            let attempt_id = new_id("noptattempt");
            let status = if configured { "running" } else { "error" };
            let fingerprint = request_hash(
                &json!({"parent":input.parent_revision_id,"instruction":input.instruction,"sources":input.selected_source_revision_ids,"provider":input.provider_id,"model":input.model_id}),
            )?;
            tx.execute("INSERT INTO novel_artifact_optimization_runs (id,novel_work_id,analysis_artifact_id,parent_artifact_revision_id,provider_id,model_id,instruction,frozen_input_fingerprint,idempotency_key,status,attempt_no,lease_owner,lease_expires_at,safe_error_code,safe_user_message,created_at,updated_at,finished_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",params![id,work.id,input.artifact_id,input.parent_revision_id,input.provider_id,input.model_id,input.instruction,fingerprint,input.idempotency_key,status,1,if configured {Some(owner)} else {None},if configured {Some(timestamp+ANALYSIS_LEASE_MS)} else {None},if configured {Option::<String>::None} else {Some("NOT_CONFIGURED".into())},if configured {Option::<String>::None} else {Some("未配置 LLM，可配置后重试".into())},timestamp,timestamp,if configured {Option::<i64>::None} else {Some(timestamp)}]).map_err(|e|format!("创建优化 run 失败: {e}"))?;
            for (source_order, revision_id) in input.selected_source_revision_ids.iter().enumerate()
            {
                tx.execute("INSERT INTO novel_artifact_optimization_inputs (novel_artifact_optimization_run_id,analysis_artifact_revision_id,source_order) VALUES (?,?,?)",params![id,revision_id,source_order as i64]).map_err(|e|e.to_string())?;
            }
            tx.execute(
                "INSERT INTO novel_artifact_optimization_attempts
                 (id,novel_artifact_optimization_run_id,attempt_no,parent_attempt_id,status,lease_owner,lease_expires_at,heartbeat_at,safe_error_code,safe_user_message,created_at,finished_at)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    attempt_id,
                    id,
                    1,
                    Option::<String>::None,
                    if configured { "running" } else { "error" },
                    if configured { Some(owner) } else { None },
                    if configured { Some(timestamp + ANALYSIS_LEASE_MS) } else { None },
                    if configured { Some(timestamp) } else { None },
                    if configured { Option::<String>::None } else { Some("NOT_CONFIGURED".into()) },
                    if configured { Option::<String>::None } else { Some("未配置 LLM，可配置后重试".into()) },
                    timestamp,
                    if configured { Option::<i64>::None } else { Some(timestamp) },
                ],
            ).map_err(|error| format!("创建优化 attempt 失败: {error}"))?;
            Ok(NovelArtifactOptimizeStatusResult {
                artifact_optimization_run_id: id.clone(),
                operation_id: Some(id),
                status: status.into(),
                attempt_no: 1,
                parent_revision_id: Some(input.parent_revision_id),
                candidate_revision_id: None,
                validation_report: None,
                receipt_id: None,
                safe_error: (!configured).then(|| "NOT_CONFIGURED".into()),
            })
        },
    )
}

fn optimization_prompt_input(conn: &Connection, run_id: &str) -> Result<(String, String), String> {
    let row: (String,String,String,String) = conn.query_row("SELECT run.model_id,run.instruction,parent.body_json,COALESCE((SELECT json_group_array(body_json) FROM (SELECT revision.body_json FROM novel_artifact_optimization_inputs input JOIN analysis_artifact_revisions revision ON revision.id=input.analysis_artifact_revision_id WHERE input.novel_artifact_optimization_run_id=run.id ORDER BY input.source_order)),'[]') FROM novel_artifact_optimization_runs run JOIN novel_artifact_optimization_attempts attempt ON attempt.novel_artifact_optimization_run_id=run.id AND attempt.attempt_no=run.attempt_no JOIN analysis_artifact_revisions parent ON parent.id=run.parent_artifact_revision_id WHERE run.id=? AND run.status='running' AND attempt.status='running'",params![run_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|e|e.to_string())?.ok_or("优化 run 不在运行状态")?;
    if row.1.len() + row.2.len() + row.3.len() > MAX_ANALYSIS_PROMPT_BYTES {
        return Err("优化输入超过上限".into());
    }
    Ok((row.0,json!({"instruction":row.1,"parentContent":json_value(row.2),"selectedSources":json_value(row.3),"instructionForModel":"仅返回替换后的 JSON 对象，不要 Markdown 或解释。"}).to_string()))
}

async fn request_optimization_llm(
    app: &AppState,
    model: &str,
    user: &str,
) -> Result<Value, String> {
    let cfg = app.cfg.read().map_err(|_| "读取 LLM 配置失败")?.clone();
    if cfg.llm_api_url.trim().is_empty() || cfg.llm_api_key.trim().is_empty() {
        return Err("NOT_CONFIGURED".into());
    }
    let raw = crate::llm::complete_text(
        &completion_endpoint(&cfg.llm_api_url),
        &cfg.llm_api_key,
        model,
        "你是小说产物编辑器。仅输出 JSON 对象。",
        user,
        "novel.artifact_optimize",
    )
    .await
    .map_err(|e| safe_error(&e))?;
    let content = parse_model_json(&raw)?;
    if !content.is_object() {
        return Err("模型优化结果必须是 JSON 对象".into());
    }
    Ok(content)
}

fn complete_optimization_inner(
    conn: &Connection,
    run_id: &str,
    content: Value,
    owner: &str,
) -> Result<NovelArtifactOptimizeStatusResult, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| format!("开始优化完成事务失败: {e}"))?;
    let lease_now = now();
    let (artifact_id,parent_id,work_id,version,artifact_type,attempt_no,attempt_id):(String,String,String,i64,String,i64,String)=tx.query_row("SELECT run.analysis_artifact_id,run.parent_artifact_revision_id,run.novel_work_id,artifact.optimistic_version,artifact.artifact_type,attempt.attempt_no,attempt.id FROM novel_artifact_optimization_runs run JOIN novel_artifact_optimization_attempts attempt ON attempt.novel_artifact_optimization_run_id=run.id AND attempt.attempt_no=run.attempt_no JOIN analysis_artifacts artifact ON artifact.id=run.analysis_artifact_id WHERE run.id=? AND run.status='running' AND run.lease_owner=? AND run.lease_expires_at>=? AND attempt.status='running' AND attempt.lease_owner=? AND attempt.lease_expires_at>=?",params![run_id,owner,lease_now,owner,lease_now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional().map_err(|e|e.to_string())?.ok_or("优化执行权已失效")?;
    validate_artifact_content(&artifact_type, &content)?;
    let parent_is_head: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM analysis_artifacts WHERE id=? AND novel_work_id=? AND candidate_head_revision_id=?)",params![artifact_id,work_id,parent_id],|r|r.get(0)).map_err(|e|e.to_string())?;
    let timestamp = now();
    let revision_id = new_id("nartifactrev");
    let mut status = "ready";
    let mut safe: Option<&str> = None;
    if parent_is_head {
        let next:i64=tx.query_row("SELECT COALESCE(MAX(version),0)+1 FROM analysis_artifact_revisions WHERE analysis_artifact_id=?",params![artifact_id],|r|r.get(0)).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,?,? ,?,'','ai_optimize','{}','{}','candidate',?)",params![revision_id,artifact_id,next,parent_id,content.to_string(),timestamp]).map_err(|e|e.to_string())?;
        let changed=tx.execute("UPDATE analysis_artifacts SET candidate_head_revision_id=?,optimistic_version=optimistic_version+1,updated_at=? WHERE id=? AND optimistic_version=? AND candidate_head_revision_id=?",params![revision_id,timestamp,artifact_id,version,parent_id]).map_err(|e|e.to_string())?;
        if changed != 1 {
            return Err("产物并发更新冲突".into());
        }
    } else {
        status = "error";
        safe = Some("BASELINE_STALE");
    }
    let safe_message = if parent_is_head {
        None
    } else {
        Some("产物基准已变化，请重新优化")
    };
    let attempt_changed = tx.execute("UPDATE novel_artifact_optimization_attempts SET status=?,safe_error_code=?,safe_user_message=?,lease_owner=NULL,lease_expires_at=NULL,heartbeat_at=?,finished_at=? WHERE id=? AND novel_artifact_optimization_run_id=? AND attempt_no=? AND status='running' AND lease_owner=? AND lease_expires_at>=?",params![if parent_is_head {"succeeded"} else {"error"},safe,safe_message,timestamp,timestamp,attempt_id,run_id,attempt_no,owner,lease_now]).map_err(|e|e.to_string())?;
    if attempt_changed != 1 {
        return Err("优化执行权已失效".into());
    }
    let run_changed = tx.execute("UPDATE novel_artifact_optimization_runs SET status=?,result_artifact_revision_id=?,safe_error_code=?,safe_user_message=?,lease_owner=NULL,lease_expires_at=NULL,updated_at=?,finished_at=? WHERE id=? AND status='running' AND attempt_no=? AND lease_owner=? AND lease_expires_at>=?",params![status,if parent_is_head {Some(revision_id.clone())} else {Option::<String>::None},safe,safe_message,timestamp,timestamp,run_id,attempt_no,owner,lease_now]).map_err(|e|e.to_string())?;
    if run_changed != 1 {
        return Err("优化执行权已失效".into());
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(NovelArtifactOptimizeStatusResult {
        artifact_optimization_run_id: run_id.into(),
        operation_id: Some(run_id.into()),
        status: status.into(),
        attempt_no,
        parent_revision_id: Some(parent_id),
        candidate_revision_id: parent_is_head.then_some(revision_id),
        validation_report: None,
        receipt_id: None,
        safe_error: safe.map(str::to_owned),
    })
}

fn fail_optimization_inner(
    conn: &Connection,
    run_id: &str,
    owner: &str,
    error: &str,
) -> Result<NovelArtifactOptimizeStatusResult, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let safe = safe_error(error);
    let timestamp = now();
    let attempt: Option<(String, i64)> = tx.query_row("SELECT attempt.id,attempt.attempt_no FROM novel_artifact_optimization_runs run JOIN novel_artifact_optimization_attempts attempt ON attempt.novel_artifact_optimization_run_id=run.id AND attempt.attempt_no=run.attempt_no WHERE run.id=? AND run.status='running' AND run.lease_owner=? AND run.lease_expires_at>=? AND attempt.status='running' AND attempt.lease_owner=? AND attempt.lease_expires_at>=?",params![run_id,owner,timestamp,owner,timestamp],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|error| error.to_string())?;
    let Some((attempt_id, attempt_no)) = attempt else {
        return Err("优化执行权已失效".into());
    };
    let attempt_changed=tx.execute("UPDATE novel_artifact_optimization_attempts SET status='error',safe_error_code='OPTIMIZATION_FAILED',safe_user_message=?,lease_owner=NULL,lease_expires_at=NULL,heartbeat_at=?,finished_at=? WHERE id=? AND status='running' AND lease_owner=? AND lease_expires_at>=?",params![safe,timestamp,timestamp,attempt_id,owner,timestamp]).map_err(|e|e.to_string())?;
    let run_changed=tx.execute("UPDATE novel_artifact_optimization_runs SET status='error',safe_error_code='OPTIMIZATION_FAILED',safe_user_message=?,lease_owner=NULL,lease_expires_at=NULL,updated_at=?,finished_at=? WHERE id=? AND status='running' AND attempt_no=? AND lease_owner=? AND lease_expires_at>=?",params![safe,timestamp,timestamp,run_id,attempt_no,owner,timestamp]).map_err(|e|e.to_string())?;
    if attempt_changed != 1 || run_changed != 1 {
        return Err("优化执行权已失效".into());
    }
    let result=tx.query_row("SELECT run.id,run.id,run.status,attempt.attempt_no,run.parent_artifact_revision_id,run.result_artifact_revision_id,NULL,run.safe_error_code FROM novel_artifact_optimization_runs run JOIN novel_artifact_optimization_attempts attempt ON attempt.novel_artifact_optimization_run_id=run.id AND attempt.attempt_no=run.attempt_no WHERE run.id=?",params![run_id],optimize_status_from_row).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

#[tauri::command]
pub async fn novel_artifact_optimize_start(
    state: tauri::State<'_, DbState>,
    app: tauri::State<'_, AppState>,
    mut input: NovelArtifactOptimizeStartInput,
) -> Result<NovelArtifactOptimizeStatusResult, String> {
    let cfg = app.cfg.read().map_err(|_| "读取 LLM 配置失败")?.clone();
    if input.provider_id.trim().eq_ignore_ascii_case("default") {
        input.provider_id = "configured_llm".into();
    }
    if input.model_id.trim().eq_ignore_ascii_case("default") {
        input.model_id = cfg.llm_model.clone();
    }
    let configured = {
        let cfg = &cfg;
        !cfg.llm_api_url.trim().is_empty() && !cfg.llm_api_key.trim().is_empty()
    };
    let owner = state.app_session_id().to_owned();
    let mut run = db::with_connection(&state, |conn| {
        optimize_start_inner(conn, input, configured, &owner)
    })?;
    if !configured {
        return Ok(run);
    }
    let (model, user) = db::with_connection(&state, |conn| {
        optimization_prompt_input(conn, &run.artifact_optimization_run_id)
    })?;
    run = match request_optimization_llm(&app, &model, &user).await {
        Ok(content) => db::with_connection(&state, |conn| {
            complete_optimization_inner(conn, &run.artifact_optimization_run_id, content, &owner)
        })?,
        Err(error) => db::with_connection(&state, |conn| {
            fail_optimization_inner(conn, &run.artifact_optimization_run_id, &owner, &error)
        })?,
    };
    Ok(run)
}

#[tauri::command]
pub fn novel_artifact_optimize_status(
    state: tauri::State<'_, DbState>,
    input: NovelArtifactOptimizeStatusInput,
) -> Result<NovelArtifactOptimizeStatusResult, String> {
    db::with_connection(&state, |conn| {
        let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
        conn.query_row("SELECT run.id,run.id,run.status,attempt.attempt_no,run.parent_artifact_revision_id,run.result_artifact_revision_id,NULL,run.safe_error_code FROM novel_artifact_optimization_runs run JOIN novel_artifact_optimization_attempts attempt ON attempt.novel_artifact_optimization_run_id=run.id AND attempt.attempt_no=run.attempt_no WHERE run.id=? AND run.novel_work_id=?",params![input.artifact_optimization_run_id,work.id],optimize_status_from_row).optional().map_err(|e|e.to_string())?.ok_or_else(||"优化 run 不存在或不属于当前小说".into())
    })
}

#[tauri::command]
pub fn novel_artifact_optimize_recover_stale(
    state: tauri::State<'_, DbState>,
    input: NovelArtifactOptimizeRecoverInput,
) -> Result<i64, String> {
    db::with_connection(&state, |conn| {
        optimize_recover_stale_inner(conn, &input.project_id, &input.novel_work_id)
    })
}
fn optimize_recover_stale_inner(
    conn: &Connection,
    project_id: &str,
    novel_work_id: &str,
) -> Result<i64, String> {
    let work = get_work(conn, project_id, novel_work_id)?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let timestamp = now();
    let mut statement = tx.prepare("SELECT run.id,attempt.id,attempt.attempt_no FROM novel_artifact_optimization_runs run JOIN novel_artifact_optimization_attempts attempt ON attempt.novel_artifact_optimization_run_id=run.id AND attempt.attempt_no=run.attempt_no WHERE run.novel_work_id=? AND run.status='running' AND run.lease_expires_at<? AND attempt.status='running' AND attempt.lease_expires_at<?").map_err(|error| error.to_string())?;
    let expired = statement
        .query_map(params![work.id, timestamp, timestamp], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(statement);
    for (run_id, attempt_id, attempt_no) in &expired {
        let attempt_changed = tx.execute("UPDATE novel_artifact_optimization_attempts SET status='error',safe_error_code='LEASE_EXPIRED',safe_user_message='优化执行租约已过期，可重试',lease_owner=NULL,lease_expires_at=NULL,heartbeat_at=?,finished_at=? WHERE id=? AND status='running' AND attempt_no=? AND lease_expires_at<?",params![timestamp,timestamp,attempt_id,attempt_no,timestamp]).map_err(|error| error.to_string())?;
        let run_changed = tx.execute("UPDATE novel_artifact_optimization_runs SET status='error',safe_error_code='LEASE_EXPIRED',safe_user_message='优化执行租约已过期，可重试',lease_owner=NULL,lease_expires_at=NULL,updated_at=?,finished_at=? WHERE id=? AND novel_work_id=? AND status='running' AND attempt_no=? AND lease_expires_at<?",params![timestamp,timestamp,run_id,work.id,attempt_no,timestamp]).map_err(|error| error.to_string())?;
        if attempt_changed != 1 || run_changed != 1 {
            return Err("优化过期恢复时执行权已变化".into());
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(expired.len() as i64)
}

fn optimize_retry_inner(
    conn: &Connection,
    input: NovelArtifactOptimizeRetryInput,
    configured: bool,
    owner: &str,
) -> Result<NovelArtifactOptimizeStatusResult, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let request =
        serde_json::to_value(&input).map_err(|error| format!("序列化优化重试请求失败: {error}"))?;
    let key = input.idempotency_key.clone();
    with_receipt(
        conn,
        "novel_artifact_optimize_retry",
        &key,
        &request,
        move |tx| {
            ensure_active_work_in_tx(tx, &work)?;
            let (parent_revision_id, instruction, previous_attempt, parent_attempt_id, parent_body): (String, String, i64, String, String) = tx.query_row(
                "SELECT run.parent_artifact_revision_id,run.instruction,attempt.attempt_no,attempt.id,parent.body_json
                 FROM novel_artifact_optimization_runs run
                 JOIN novel_artifact_optimization_attempts attempt ON attempt.novel_artifact_optimization_run_id=run.id AND attempt.attempt_no=run.attempt_no
                 JOIN analysis_artifacts artifact ON artifact.id=run.analysis_artifact_id AND artifact.novel_work_id=run.novel_work_id
                 JOIN analysis_artifact_revisions parent ON parent.id=run.parent_artifact_revision_id AND parent.analysis_artifact_id=artifact.id
                 WHERE run.id=? AND run.novel_work_id=? AND run.status='error' AND attempt.status='error'",
                params![input.artifact_optimization_run_id, work.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            ).optional().map_err(|error| format!("读取可重试优化 run 失败: {error}"))?.ok_or("只有当前小说内的错误优化 run 可以重试")?;
            let attempt_no = previous_attempt
                .checked_add(1)
                .ok_or("优化尝试次数已超出范围")?;
            let (source_count, scoped_source_count): (i64, i64) = tx.query_row(
                "SELECT COUNT(*),SUM(CASE WHEN source_artifact.novel_work_id=? THEN 1 ELSE 0 END)
                 FROM novel_artifact_optimization_inputs input
                 JOIN analysis_artifact_revisions revision ON revision.id=input.analysis_artifact_revision_id
                 JOIN analysis_artifacts source_artifact ON source_artifact.id=revision.analysis_artifact_id
                 WHERE input.novel_artifact_optimization_run_id=?",
                params![work.id,input.artifact_optimization_run_id],
                |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
            ).map_err(|error| format!("读取冻结优化来源失败: {error}"))?;
            if source_count > 64 {
                return Err("冻结的优化来源超过 64 项".into());
            }
            if source_count != scoped_source_count {
                return Err("冻结的优化来源不属于当前小说".into());
            }
            if instruction.len() + parent_body.len() > MAX_ANALYSIS_PROMPT_BYTES {
                return Err("冻结的优化输入超过上限".into());
            }
            let timestamp = now();
            let attempt_id = new_id("noptattempt");
            let status = if configured { "running" } else { "error" };
            tx.execute(
                "INSERT INTO novel_artifact_optimization_attempts
                 (id,novel_artifact_optimization_run_id,attempt_no,parent_attempt_id,status,lease_owner,lease_expires_at,heartbeat_at,safe_error_code,safe_user_message,created_at,finished_at)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    attempt_id, input.artifact_optimization_run_id, attempt_no, parent_attempt_id,
                    if configured { "running" } else { "error" },
                    if configured { Some(owner) } else { None },
                    if configured { Some(timestamp + ANALYSIS_LEASE_MS) } else { None },
                    if configured { Some(timestamp) } else { None },
                    if configured { Option::<String>::None } else { Some("NOT_CONFIGURED".into()) },
                    if configured { Option::<String>::None } else { Some("未配置 LLM，可配置后重试".into()) },
                    timestamp, if configured { Option::<i64>::None } else { Some(timestamp) },
                ],
            ).map_err(|error| format!("创建优化重试 attempt 失败: {error}"))?;
            let run_changed = tx.execute(
                "UPDATE novel_artifact_optimization_runs
                 SET status=?,attempt_no=?,lease_owner=?,lease_expires_at=?,result_artifact_revision_id=NULL,safe_error_code=?,safe_user_message=?,updated_at=?,finished_at=?
                 WHERE id=? AND novel_work_id=? AND status='error' AND attempt_no=?",
                params![
                    status, attempt_no,
                    if configured { Some(owner) } else { None },
                    if configured { Some(timestamp + ANALYSIS_LEASE_MS) } else { None },
                    if configured { Option::<String>::None } else { Some("NOT_CONFIGURED".into()) },
                    if configured { Option::<String>::None } else { Some("未配置 LLM，可配置后重试".into()) },
                    timestamp, if configured { Option::<i64>::None } else { Some(timestamp) },
                    input.artifact_optimization_run_id, work.id, previous_attempt,
                ],
            ).map_err(|error| format!("更新优化 run 当前 attempt 失败: {error}"))?;
            if run_changed != 1 {
                return Err("优化 run 已变化，请重新读取后重试".into());
            }
            Ok(NovelArtifactOptimizeStatusResult {
                artifact_optimization_run_id: input.artifact_optimization_run_id.clone(),
                operation_id: Some(input.artifact_optimization_run_id),
                status: status.into(),
                attempt_no,
                parent_revision_id: Some(parent_revision_id),
                candidate_revision_id: None,
                validation_report: None,
                receipt_id: None,
                safe_error: (!configured).then(|| "NOT_CONFIGURED".into()),
            })
        },
    )
}

#[tauri::command]
pub async fn novel_artifact_optimize_retry(
    state: tauri::State<'_, DbState>,
    app: tauri::State<'_, AppState>,
    input: NovelArtifactOptimizeRetryInput,
) -> Result<NovelArtifactOptimizeStatusResult, String> {
    let cfg = app.cfg.read().map_err(|_| "读取 LLM 配置失败")?.clone();
    let configured = !cfg.llm_api_url.trim().is_empty() && !cfg.llm_api_key.trim().is_empty();
    let owner = state.app_session_id().to_owned();
    let mut run = db::with_connection(&state, |conn| {
        optimize_retry_inner(conn, input, configured, &owner)
    })?;
    if !configured {
        return Ok(run);
    }
    let (model, user) = db::with_connection(&state, |conn| {
        optimization_prompt_input(conn, &run.artifact_optimization_run_id)
    })?;
    run = match request_optimization_llm(&app, &model, &user).await {
        Ok(content) => db::with_connection(&state, |conn| {
            complete_optimization_inner(conn, &run.artifact_optimization_run_id, content, &owner)
        })?,
        Err(error) => db::with_connection(&state, |conn| {
            fail_optimization_inner(conn, &run.artifact_optimization_run_id, &owner, &error)
        })?,
    };
    Ok(run)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactHistoryInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub artifact_id: String,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelArtifactHistoryResult {
    pub items: Vec<NovelArtifactRevision>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelCanonDiffInput {
    pub project_id: String,
    pub novel_work_id: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelCanonDiffResult {
    pub source_revision_ids: Vec<String>,
    pub content: Value,
    pub fingerprint: String,
}
#[tauri::command]
pub fn novel_canon_diff(
    state: tauri::State<'_, DbState>,
    input: NovelCanonDiffInput,
) -> Result<NovelCanonDiffResult, String> {
    db::with_connection(&state, |conn| novel_canon_diff_inner(conn, input))
}

fn novel_canon_diff_inner(
    conn: &Connection,
    input: NovelCanonDiffInput,
) -> Result<NovelCanonDiffResult, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    let mut statement=conn.prepare("SELECT revision.id FROM analysis_artifacts artifact JOIN analysis_artifact_revisions revision ON revision.id=artifact.adopted_head_revision_id WHERE artifact.novel_work_id=? AND artifact.artifact_type IN ('world_facts','character_facts','faction_facts','location_facts','prop_facts')").map_err(|e|e.to_string())?;
    let ids = statement
        .query_map(params![work.id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let sources = publish_sources(conn, &work.id, &ids, PublishTrack::Canon, true)?;
    let ids = sources
        .iter()
        .map(|source| source.revision_id.clone())
        .collect::<Vec<_>>();
    let value = merged_publish_body(Value::Object(Default::default()), &sources);
    let fingerprint = request_hash(
        &json!({"publishedCanonVersionId":work.published_canon_version_id,"sources":ids,"content":value}),
    )?;
    Ok(NovelCanonDiffResult {
        source_revision_ids: ids,
        content: value,
        fingerprint,
    })
}

const CANON_PUBLISH_TYPES: [&str; 5] = [
    "world_facts",
    "character_facts",
    "faction_facts",
    "location_facts",
    "prop_facts",
];
const STATE_PUBLISH_TYPES: [&str; 3] = ["timeline_delta", "continuity_delta", "open_threads"];
const APPLY_TOKEN_TTL_MS: i64 = 15 * 60 * 1000;

#[derive(Clone, Copy)]
enum PublishTrack {
    Canon,
    State,
}

impl PublishTrack {
    fn operation_type(self) -> &'static str {
        match self {
            Self::Canon => "publish_canon",
            Self::State => "publish_novel_state",
        }
    }
    fn source_role(self) -> &'static str {
        match self {
            Self::Canon => "canonical_input",
            Self::State => "state_input",
        }
    }
    fn allowed(self, kind: &str) -> bool {
        match self {
            Self::Canon => CANON_PUBLISH_TYPES.contains(&kind),
            Self::State => STATE_PUBLISH_TYPES.contains(&kind),
        }
    }
    fn head(self, work: &NovelWork) -> &str {
        match self {
            Self::Canon => &work.published_canon_version_id,
            Self::State => &work.current_novel_state_version_id,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelPublishPreviewInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub expected_head_version_id: String,
    pub source_revision_ids: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelPublishInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub operation_id: String,
    pub approval_token: String,
    pub idempotency_key: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelPublishPreviewResult {
    pub operation_id: String,
    pub approval_token: String,
    pub preview_fingerprint: String,
    pub base_head_version_id: String,
    pub source_revision_ids: Vec<String>,
    pub content: Value,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelPublishResult {
    pub operation_id: String,
    pub version_id: String,
    pub version: i64,
}

#[derive(Clone)]
struct PublishSource {
    revision_id: String,
    artifact_type: String,
    body: Value,
    chapter_revision_id: Option<String>,
    sequence_no: Option<i64>,
    context_status: Option<String>,
    branch_kind: Option<String>,
}

fn publish_sources(
    conn: &Connection,
    work_id: &str,
    ids: &[String],
    track: PublishTrack,
    require_adopted: bool,
) -> Result<Vec<PublishSource>, String> {
    if ids.is_empty() || ids.len() > 64 {
        return Err("sourceRevisionIds 必须在 1 到 64 项之间".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut sources = Vec::with_capacity(ids.len());
    for id in ids {
        if !seen.insert(id.as_str()) {
            return Err("sourceRevisionIds 不能重复".into());
        }
        let row = conn.query_row(
            "SELECT revision.id, artifact.artifact_type, revision.body_json,
                    COALESCE(artifact.novel_chapter_revision_id, run.novel_chapter_revision_id),
                    chapter.sequence_no,
                    context.status, context.branch_kind
             FROM analysis_artifact_revisions revision
             JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
             LEFT JOIN source_analysis_runs run ON run.id=artifact.source_analysis_run_id
             LEFT JOIN novel_chapter_revisions chapter_revision ON chapter_revision.id=COALESCE(artifact.novel_chapter_revision_id, run.novel_chapter_revision_id)
             LEFT JOIN novel_chapters chapter ON chapter.id=chapter_revision.novel_chapter_id
             LEFT JOIN novel_chapter_context_revisions context ON context.source_analysis_run_id=run.id
             WHERE revision.id=? AND artifact.novel_work_id=?",
            params![id, work_id],
            |r| Ok(PublishSource {
                revision_id: r.get(0)?, artifact_type: r.get(1)?, body: json_value(r.get(2)?),
                chapter_revision_id: r.get(3)?, sequence_no: r.get(4)?,
                context_status: r.get(5)?, branch_kind: r.get(6)?,
            }),
        ).optional().map_err(|e| format!("读取发布来源失败: {e}"))?
            .ok_or("发布来源不存在或不属于当前小说")?;
        if !track.allowed(&row.artifact_type) {
            return Err("发布来源类型不属于当前发布轨道".into());
        }
        if require_adopted {
            let adopted: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM analysis_artifact_revisions revision
                 JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
                 WHERE revision.id=? AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id)",
                params![id], |r| r.get(0),
            ).map_err(|e| e.to_string())?;
            if !adopted {
                return Err("发布来源必须是当前已采用版本".into());
            }
        }
        sources.push(row);
    }
    sources.sort_by(|a, b| {
        a.sequence_no
            .unwrap_or(i64::MAX)
            .cmp(&b.sequence_no.unwrap_or(i64::MAX))
            .then(a.artifact_type.cmp(&b.artifact_type))
            .then(a.revision_id.cmp(&b.revision_id))
    });
    Ok(sources)
}

fn version_body(conn: &Connection, track: PublishTrack, version_id: &str) -> Result<Value, String> {
    let table = match track {
        PublishTrack::Canon => "novel_canon_versions",
        PublishTrack::State => "novel_state_versions",
    };
    let sql = format!("SELECT body_json FROM {table} WHERE id=?");
    conn.query_row(&sql, params![version_id], |r| r.get::<_, String>(0))
        .optional()
        .map_err(|e| e.to_string())?
        .map(json_value)
        .ok_or("发布基线版本不存在".into())
}

fn merged_publish_body(parent: Value, sources: &[PublishSource]) -> Value {
    merge_working(
        parent,
        sources
            .iter()
            .map(|source| (source.artifact_type.clone(), source.body.clone())),
    )
}

fn publish_fingerprint(
    track: PublishTrack,
    base: &str,
    sources: &[PublishSource],
    body: &Value,
) -> Result<String, String> {
    request_hash(
        &json!({"operationType":track.operation_type(),"baseTargetVersionId":base,
        "sourceRevisionIds":sources.iter().map(|s|s.revision_id.clone()).collect::<Vec<_>>(),"body":body}),
    )
}

fn render_canon_markdown(body: &Value, version: i64) -> String {
    format!(
        "# Novel Canon v{version}\n\n```json\n{}\n```",
        serde_json::to_string_pretty(body).unwrap_or_else(|_| "{}".into())
    )
}

fn state_through_revision(
    conn: &Connection,
    work: &NovelWork,
    sources: &[PublishSource],
) -> Result<Option<String>, String> {
    for source in sources {
        if matches!(
            source.context_status.as_deref(),
            Some("conflicted") | Some("needs_rebase")
        ) || source.branch_kind.as_deref() == Some("detached_gap")
        {
            return Err("存在不可连续合并的章节上下文，不能发布小说状态".into());
        }
    }
    let previous: Option<i64> = conn.query_row(
        "SELECT chapter.sequence_no FROM novel_state_versions state
         LEFT JOIN novel_chapter_revisions revision ON revision.id=state.through_novel_chapter_revision_id
         LEFT JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id WHERE state.id=?",
        params![work.current_novel_state_version_id], |r| r.get(0),
    ).optional().map_err(|e|e.to_string())?.flatten();
    let mut chapters =
        std::collections::BTreeMap::<i64, (String, std::collections::BTreeSet<String>)>::new();
    for source in sources {
        let sequence = source.sequence_no.ok_or("状态发布来源必须绑定章节版本")?;
        let revision_id = source
            .chapter_revision_id
            .clone()
            .ok_or("状态发布来源必须绑定章节版本")?;
        let entry = chapters
            .entry(sequence)
            .or_insert_with(|| (revision_id.clone(), std::collections::BTreeSet::new()));
        if entry.0 != revision_id {
            return Err("同一章节序号不能混用不同章节版本发布状态".into());
        }
        if !entry.1.insert(source.artifact_type.clone()) {
            return Err("同一章节状态来源不能重复同类产物".into());
        }
    }
    if chapters.is_empty() {
        return Err("状态发布来源必须绑定章节版本".into());
    }
    let mut expected = previous.unwrap_or(0) + 1;
    for (sequence, (_, kinds)) in &chapters {
        if *sequence != expected {
            return Err("状态发布章节必须连续，不能跨越缺口".into());
        }
        if kinds.len() != 3
            || !["timeline_delta", "continuity_delta", "open_threads"]
                .iter()
                .all(|kind| kinds.contains(*kind))
        {
            return Err(
                "每个状态章节必须且只能包含 timeline_delta、continuity_delta、open_threads".into(),
            );
        }
        expected += 1;
    }
    Ok(chapters.last_key_value().map(|(_, (id, _))| id.clone()))
}

fn publish_preview_inner(
    conn: &Connection,
    input: NovelPublishPreviewInput,
    track: PublishTrack,
) -> Result<NovelPublishPreviewResult, String> {
    let expected = ensure_nonempty(&input.expected_head_version_id, "expectedHeadVersionId")?;
    let key = ensure_nonempty(&input.idempotency_key, "idempotencyKey")?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let work = get_work(&tx, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    if track.head(&work) != expected {
        return Err("BASELINE_STALE".into());
    }
    let sources = publish_sources(&tx, &work.id, &input.source_revision_ids, track, true)?;
    if matches!(track, PublishTrack::State) {
        state_through_revision(&tx, &work, &sources)?;
    }
    let body = merged_publish_body(version_body(&tx, track, expected)?, &sources);
    let fingerprint = publish_fingerprint(track, expected, &sources, &body)?;
    let existing: Option<(String,String,String)> = tx.query_row(
        "SELECT id,preview_fingerprint,status FROM analysis_apply_operations WHERE idempotency_key=?", params![key], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))
    ).optional().map_err(|e|e.to_string())?;
    let operation_id = if let Some((id, stored, status)) = existing {
        if stored != fingerprint || status != "previewed" {
            return Err("idempotencyKey 已用于不同业务载荷".into());
        }
        id
    } else {
        let id = new_id("apply");
        tx.execute("INSERT INTO analysis_apply_operations (id,operation_type,novel_work_id,comic_adaptation_id,base_target_version_id,idempotency_key,preview_fingerprint,approval_token_hash,approval_expires_at,status,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            params![id,track.operation_type(),work.id,Option::<String>::None,expected,key,fingerprint,"pending",0_i64,"previewed",now(),now()]).map_err(|e|format!("创建发布预览失败: {e}"))?;
        for (order, source) in sources.iter().enumerate() {
            tx.execute("INSERT INTO analysis_apply_operation_sources (analysis_apply_operation_id,analysis_artifact_revision_id,source_role,source_order) VALUES (?,?,?,?)",params![id,source.revision_id,track.source_role(),order as i64]).map_err(|e|e.to_string())?;
        }
        id
    };
    let token = new_id("approval");
    let expires = now() + APPLY_TOKEN_TTL_MS;
    tx.execute("UPDATE analysis_apply_operations SET approval_token_hash=?,approval_expires_at=?,updated_at=? WHERE id=?", params![request_hash(&json!(token))?,expires,now(),operation_id]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(NovelPublishPreviewResult {
        operation_id,
        approval_token: token,
        preview_fingerprint: fingerprint,
        base_head_version_id: expected.into(),
        source_revision_ids: sources.into_iter().map(|s| s.revision_id).collect(),
        content: body,
        expires_at: expires,
    })
}

fn publish_inner(
    conn: &Connection,
    input: NovelPublishInput,
    track: PublishTrack,
) -> Result<NovelPublishResult, String> {
    let key = ensure_nonempty(&input.idempotency_key, "idempotencyKey")?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let work = get_work(&tx, &input.project_id, &input.novel_work_id)?;
    ensure_active_work(&work)?;
    let op: (String,String,String,i64,String)=tx.query_row("SELECT operation_type,base_target_version_id,status,approval_expires_at,approval_token_hash FROM analysis_apply_operations WHERE id=? AND novel_work_id=?",params![input.operation_id,work.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|e|e.to_string())?.ok_or("发布操作不存在")?;
    if op.0 != track.operation_type() {
        return Err("发布操作轨道不匹配".into());
    }
    if op.2 == "succeeded" {
        let result: Option<(Option<String>,Option<String>)>=tx.query_row("SELECT result_novel_canon_version_id,result_novel_state_version_id FROM analysis_apply_receipts WHERE analysis_apply_operation_id=? AND idempotency_key=?",params![input.operation_id,key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|e.to_string())?;
        if let Some((canon, state)) = result {
            let id = canon.or(state).ok_or("发布回执无结果")?;
            let version: i64 = match track {
                PublishTrack::Canon => tx.query_row(
                    "SELECT version FROM novel_canon_versions WHERE id=?",
                    params![id],
                    |r| r.get(0),
                ),
                PublishTrack::State => tx.query_row(
                    "SELECT version FROM novel_state_versions WHERE id=?",
                    params![id],
                    |r| r.get(0),
                ),
            }
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(NovelPublishResult {
                operation_id: input.operation_id,
                version_id: id,
                version,
            });
        }
        return Err("发布操作回执无效".into());
    }
    if op.2 != "previewed" || op.3 < now() || op.4 != request_hash(&json!(input.approval_token))? {
        return Err("发布确认令牌无效或已过期".into());
    }
    if track.head(&work) != op.1 {
        return Err("BASELINE_STALE".into());
    }
    let ids: Vec<String> = {
        let mut statement = tx.prepare("SELECT analysis_artifact_revision_id FROM analysis_apply_operation_sources WHERE analysis_apply_operation_id=? ORDER BY source_order").map_err(|e|e.to_string())?;
        let rows = statement
            .query_map(params![input.operation_id], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    let sources = publish_sources(&tx, &work.id, &ids, track, true)?;
    let body = merged_publish_body(version_body(&tx, track, &op.1)?, &sources);
    let fingerprint = publish_fingerprint(track, &op.1, &sources, &body)?;
    let stored: String = tx
        .query_row(
            "SELECT preview_fingerprint FROM analysis_apply_operations WHERE id=?",
            params![input.operation_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if stored != fingerprint {
        return Err("发布预览已失效".into());
    }
    let version_id = new_id(match track {
        PublishTrack::Canon => "ncanon",
        PublishTrack::State => "nstate",
    });
    let version: i64 = match track {
        PublishTrack::Canon => tx.query_row(
            "SELECT version + 1 FROM novel_canon_versions WHERE id=? AND novel_work_id=?",
            params![op.1, work.id],
            |r| r.get(0),
        ),
        PublishTrack::State => tx.query_row(
            "SELECT version + 1 FROM novel_state_versions WHERE id=? AND novel_work_id=?",
            params![op.1, work.id],
            |r| r.get(0),
        ),
    }
    .map_err(|_| "发布基线版本不存在".to_string())?;
    match track {
        PublishTrack::Canon => {
            tx.execute("UPDATE novel_canon_versions SET status='superseded' WHERE id=? AND novel_work_id=?",params![op.1,work.id]).map_err(|e|e.to_string())?;
            tx.execute("INSERT INTO novel_canon_versions (id,novel_work_id,version,parent_version_id,body_json,rendered_markdown,status,created_at) VALUES (?,?,?,?,?,?, 'published',?)",params![version_id,work.id,version,op.1,serde_json::to_string(&body).unwrap(),render_canon_markdown(&body,version),now()]).map_err(|e|e.to_string())?;
            let changed = tx.execute("UPDATE novel_works SET published_canon_version_id=?,updated_at=? WHERE id=? AND published_canon_version_id=?",params![version_id,now(),work.id,op.1]).map_err(|e|e.to_string())?;
            if changed != 1 {
                return Err("BASELINE_STALE".into());
            }
        }
        PublishTrack::State => {
            let through = state_through_revision(&tx, &work, &sources)?;
            tx.execute("INSERT INTO novel_state_versions (id,novel_work_id,version,parent_version_id,through_novel_chapter_revision_id,body_json,created_at) VALUES (?,?,?,?,?,?,?)",params![version_id,work.id,version,op.1,through,serde_json::to_string(&body).unwrap(),now()]).map_err(|e|e.to_string())?;
            let changed = tx.execute("UPDATE novel_works SET current_novel_state_version_id=?,updated_at=? WHERE id=? AND current_novel_state_version_id=?",params![version_id,now(),work.id,op.1]).map_err(|e|e.to_string())?;
            if changed != 1 {
                return Err("BASELINE_STALE".into());
            }
        }
    }
    for (order, source) in sources.iter().enumerate() {
        let sql=match track{PublishTrack::Canon=>"INSERT INTO novel_canon_version_sources (novel_canon_version_id,analysis_artifact_revision_id,source_role,source_order) VALUES (?,?,?,?)",PublishTrack::State=>"INSERT INTO novel_state_version_sources (novel_state_version_id,analysis_artifact_revision_id,source_role,source_order) VALUES (?,?,?,?)"};
        tx.execute(
            sql,
            params![
                version_id,
                source.revision_id,
                track.source_role(),
                order as i64
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.execute("INSERT INTO analysis_apply_receipts (analysis_apply_operation_id,idempotency_key,operation_type,result_novel_canon_version_id,result_novel_state_version_id,created_at) VALUES (?,?,?,?,?,?)",params![input.operation_id,key,track.operation_type(),if matches!(track,PublishTrack::Canon){Some(version_id.clone())}else{None},if matches!(track,PublishTrack::State){Some(version_id.clone())}else{None},now()]).map_err(|e|format!("保存发布回执失败: {e}"))?;
    tx.execute("UPDATE analysis_apply_operations SET status='succeeded',completed_at=?,updated_at=? WHERE id=? AND status='previewed'",params![now(),now(),input.operation_id]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(NovelPublishResult {
        operation_id: input.operation_id,
        version_id,
        version,
    })
}

#[tauri::command]
pub fn novel_canon_publish_preview(
    state: tauri::State<'_, DbState>,
    input: NovelPublishPreviewInput,
) -> Result<NovelPublishPreviewResult, String> {
    db::with_connection(&state, |conn| {
        publish_preview_inner(conn, input, PublishTrack::Canon)
    })
}
#[tauri::command]
pub fn novel_state_publish_preview(
    state: tauri::State<'_, DbState>,
    input: NovelPublishPreviewInput,
) -> Result<NovelPublishPreviewResult, String> {
    db::with_connection(&state, |conn| {
        publish_preview_inner(conn, input, PublishTrack::State)
    })
}
#[tauri::command]
pub fn novel_canon_publish(
    state: tauri::State<'_, DbState>,
    input: NovelPublishInput,
) -> Result<NovelPublishResult, String> {
    db::with_connection(&state, |conn| {
        publish_inner(conn, input, PublishTrack::Canon)
    })
}
#[tauri::command]
pub fn novel_state_publish(
    state: tauri::State<'_, DbState>,
    input: NovelPublishInput,
) -> Result<NovelPublishResult, String> {
    db::with_connection(&state, |conn| {
        publish_inner(conn, input, PublishTrack::State)
    })
}

/// A chapter-completion publication is an immutable audit snapshot over the
/// existing NovelState head. It deliberately does not introduce another head:
/// state_after_version_id is the newly-created novel_state_versions row.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelChapterStatePublishInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub novel_chapter_revision_id: String,
    pub state_before_version_id: String,
    pub source_analysis_run_id: String,
    pub adaptation_analysis_run_id: Option<String>,
    pub allow_source_analysis_without_adaptation: bool,
    pub source_revision_ids: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelChapterStatePublication {
    pub id: String,
    pub novel_work_id: String,
    pub novel_chapter_revision_id: String,
    pub source_analysis_run_id: String,
    pub adaptation_analysis_run_id: Option<String>,
    pub state_before_version_id: String,
    pub state_after_version_id: String,
    pub state_after_version: i64,
    pub state_after: Value,
    pub canon_delta: Value,
    pub continuity_delta: Value,
    pub source_revision_ids: Vec<String>,
    pub source_fingerprint: String,
    pub created_at: i64,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelChapterStateBaselineInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub novel_chapter_revision_id: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelChapterStateBaseline {
    pub state_version_id: String,
    pub state_version: i64,
    pub through_novel_chapter_revision_id: Option<String>,
    pub state_after: Value,
    pub publication: Option<NovelChapterStatePublication>,
}

fn strict_json_object(raw: &str, field: &str) -> Result<Value, String> {
    if raw.len() > MAX_CHAPTER_STATE_SNAPSHOT_BYTES {
        return Err(format!("{field} 超过大小限制"));
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| format!("{field} 必须是有效 JSON"))?;
    if !value.is_object() {
        return Err(format!("{field} 必须是 JSON 对象"));
    }
    Ok(value)
}

fn ensure_chapter_state_size(value: &Value, field: &str) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|_| format!("序列化 {field} 失败"))?;
    if bytes.len() > MAX_CHAPTER_STATE_SNAPSHOT_BYTES {
        return Err(format!("{field} 超过大小限制"));
    }
    if !value.is_object() {
        return Err(format!("{field} 必须是 JSON 对象"));
    }
    Ok(())
}

fn chapter_state_sources(
    conn: &Connection,
    work_id: &str,
    chapter_revision_id: &str,
    source_run_id: &str,
    ids: &[String],
) -> Result<(Vec<PublishSource>, Vec<PublishSource>), String> {
    if !(3..=8).contains(&ids.len()) {
        return Err("sourceRevisionIds 必须在 3 到 8 项之间".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut canon_ids = Vec::new();
    let mut state_ids = Vec::new();
    for id in ids {
        if !seen.insert(id.as_str()) {
            return Err("sourceRevisionIds 不能重复".into());
        }
        let (artifact_type, artifact_run, source_chapter): (
            String,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT artifact.artifact_type,artifact.source_analysis_run_id,
                        COALESCE(artifact.novel_chapter_revision_id, run.novel_chapter_revision_id)
                 FROM analysis_artifact_revisions revision
                 JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
                 LEFT JOIN source_analysis_runs run ON run.id=artifact.source_analysis_run_id
                 WHERE revision.id=? AND artifact.novel_work_id=?",
                params![id, work_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("读取章节状态来源失败: {error}"))?
            .ok_or("章节状态来源不存在或不属于当前小说")?;
        if artifact_run.as_deref() != Some(source_run_id)
            || source_chapter.as_deref() != Some(chapter_revision_id)
        {
            return Err("章节状态来源必须来自指定章节分析 run".into());
        }
        if CANON_PUBLISH_TYPES.contains(&artifact_type.as_str()) {
            canon_ids.push(id.clone());
        } else if STATE_PUBLISH_TYPES.contains(&artifact_type.as_str()) {
            state_ids.push(id.clone());
        } else {
            return Err("章节状态来源类型不受支持".into());
        }
    }
    let canon = if canon_ids.is_empty() {
        Vec::new()
    } else {
        publish_sources(conn, work_id, &canon_ids, PublishTrack::Canon, true)?
    };
    let state = publish_sources(conn, work_id, &state_ids, PublishTrack::State, true)?;
    let kinds = state
        .iter()
        .map(|source| source.artifact_type.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if state.len() != 3
        || kinds.len() != 3
        || !STATE_PUBLISH_TYPES.iter().all(|kind| kinds.contains(kind))
    {
        return Err("章节状态必须且只能包含 timeline_delta、continuity_delta、open_threads".into());
    }
    for source in canon.iter().chain(state.iter()) {
        if source.chapter_revision_id.as_deref() != Some(chapter_revision_id)
            || matches!(
                source.context_status.as_deref(),
                Some("conflicted") | Some("needs_rebase")
            )
            || source.branch_kind.as_deref() == Some("detached_gap")
        {
            return Err("章节分析上下文不能作为连续状态来源".into());
        }
        ensure_chapter_state_size(&source.body, "章节状态来源")?;
    }
    Ok((canon, state))
}

fn ensure_chapter_state_transition(
    conn: &Connection,
    work: &NovelWork,
    chapter_revision_id: &str,
) -> Result<(), String> {
    let target: i64 = conn
        .query_row(
            "SELECT chapter.sequence_no FROM novel_chapter_revisions revision
             JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
             WHERE revision.id=? AND chapter.novel_work_id=?",
            params![chapter_revision_id, work.id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("读取章节顺序失败: {error}"))?
        .ok_or("章节版本不存在或不属于当前小说")?;
    let previous: (Option<String>, Option<i64>) = conn
        .query_row(
            "SELECT state.through_novel_chapter_revision_id, chapter.sequence_no
             FROM novel_state_versions state
             LEFT JOIN novel_chapter_revisions revision ON revision.id=state.through_novel_chapter_revision_id
             LEFT JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
             WHERE state.id=? AND state.novel_work_id=?",
            params![work.current_novel_state_version_id, work.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| format!("读取当前小说状态失败: {error}"))?;
    if previous.0.as_deref() == Some(chapter_revision_id) {
        return Ok(());
    }
    let expected = previous.1.unwrap_or(0) + 1;
    if target != expected {
        return Err("章节状态发布必须从当前状态的下一连续章节开始".into());
    }
    Ok(())
}

fn ensure_approved_chapter_adaptation_run(
    conn: &Connection,
    work_id: &str,
    chapter_revision_id: &str,
    source_analysis_run_id: &str,
    adaptation_run_id: &str,
) -> Result<(), String> {
    let approved: bool = conn
        .query_row(
            "SELECT EXISTS(
               SELECT 1
               FROM adaptation_analysis_runs run
               JOIN comic_adaptation_chapters adaptation_chapter ON adaptation_chapter.id=run.comic_adaptation_chapter_id
               JOIN comic_adaptation_plan_heads head ON head.comic_adaptation_id=run.comic_adaptation_id AND head.status='active'
               JOIN analysis_artifact_revisions proposal_revision ON proposal_revision.id=head.adaptation_proposal_revision_id
               JOIN analysis_artifacts proposal ON proposal.id=proposal_revision.analysis_artifact_id
               JOIN analysis_artifact_revisions chapter_revision ON chapter_revision.id=head.comic_chapter_plan_revision_id
               JOIN analysis_artifacts chapter_plan ON chapter_plan.id=chapter_revision.analysis_artifact_id
               JOIN analysis_artifact_revisions scene_revision ON scene_revision.id=head.scene_plan_revision_id
               JOIN analysis_artifacts scene_plan ON scene_plan.id=scene_revision.analysis_artifact_id
               WHERE run.id=? AND run.novel_work_id=? AND run.status='ready_for_review'
                 AND adaptation_chapter.novel_chapter_revision_id=?
                 AND (
                   run.source_analysis_run_id=?
                   OR (
                     run.input_mode='artifact_revisions'
                     AND (SELECT COUNT(*) FROM adaptation_analysis_run_inputs input
                          JOIN analysis_artifact_revisions input_revision ON input_revision.id=input.analysis_artifact_revision_id
                          JOIN analysis_artifacts input_artifact ON input_artifact.id=input_revision.analysis_artifact_id
                          WHERE input.adaptation_analysis_run_id=run.id
                            AND input_artifact.source_analysis_run_id=? )=10
                   )
                 )
                 AND proposal.adaptation_analysis_run_id=run.id
                 AND chapter_plan.adaptation_analysis_run_id=run.id
                 AND scene_plan.adaptation_analysis_run_id=run.id
                 AND proposal_revision.status='adopted' AND proposal.adopted_head_revision_id=proposal_revision.id
                 AND chapter_revision.status='adopted' AND chapter_plan.adopted_head_revision_id=chapter_revision.id
                 AND scene_revision.status='adopted' AND scene_plan.adopted_head_revision_id=scene_revision.id
             )",
            params![
                adaptation_run_id,
                work_id,
                chapter_revision_id,
                source_analysis_run_id,
                source_analysis_run_id
            ],
            |row| row.get(0),
        )
        .map_err(|error| format!("验证改编分析来源失败: {error}"))?;
    if approved {
        Ok(())
    } else {
        Err("改编分析 run 尚未作为当前已批准计划应用于该章节".into())
    }
}

fn chapter_state_publication_from_row(
    conn: &Connection,
    id: &str,
) -> Result<NovelChapterStatePublication, String> {
    let mut publication: NovelChapterStatePublication = conn
        .query_row(
            "SELECT id,novel_work_id,novel_chapter_revision_id,source_analysis_run_id,adaptation_analysis_run_id,
                    state_before_version_id,state_after_version_id,
                    (SELECT version FROM novel_state_versions WHERE id=publication.state_after_version_id),
                    (SELECT body_json FROM novel_state_versions WHERE id=publication.state_after_version_id),
                    canon_delta_json,continuity_delta_json,source_fingerprint,created_at
             FROM novel_chapter_state_publications publication WHERE id=?",
            params![id],
            |row| {
                Ok(NovelChapterStatePublication {
                    id: row.get(0)?,
                    novel_work_id: row.get(1)?,
                    novel_chapter_revision_id: row.get(2)?,
                    source_analysis_run_id: row.get(3)?,
                    adaptation_analysis_run_id: row.get(4)?,
                    state_before_version_id: row.get(5)?,
                    state_after_version_id: row.get(6)?,
                    state_after_version: row.get(7)?,
                    state_after: strict_json_object(&row.get::<_, String>(8)?, "stateAfter")
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    canon_delta: strict_json_object(&row.get::<_, String>(9)?, "canonDelta")
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    continuity_delta: strict_json_object(
                        &row.get::<_, String>(10)?,
                        "continuityDelta",
                    )
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    source_revision_ids: Vec::new(),
                    source_fingerprint: row.get(11)?,
                    created_at: row.get(12)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("读取章节状态发布失败: {error}"))?
        .ok_or("章节状态发布不存在")?;
    let mut statement = conn
        .prepare(
            "SELECT analysis_artifact_revision_id FROM novel_chapter_state_publication_sources
             WHERE novel_chapter_state_publication_id=? ORDER BY source_role,source_order",
        )
        .map_err(|error| format!("准备章节状态来源读取失败: {error}"))?;
    publication.source_revision_ids = statement
        .query_map(params![id], |row| row.get(0))
        .map_err(|error| format!("读取章节状态来源失败: {error}"))?
        .collect::<Result<Vec<String>, _>>()
        .map_err(|error| format!("读取章节状态来源失败: {error}"))?;
    Ok(publication)
}

pub(crate) fn novel_chapter_state_publish_inner(
    conn: &Connection,
    input: NovelChapterStatePublishInput,
) -> Result<NovelChapterStatePublication, String> {
    let request = serde_json::to_value(&input).map_err(|error| error.to_string())?;
    with_receipt(
        conn,
        "novel_chapter_state_publish",
        &input.idempotency_key,
        &request,
        |tx| {
            let work = get_work(tx, &input.project_id, &input.novel_work_id)?;
            ensure_active_work_in_tx(tx, &work)?;
            if work.current_novel_state_version_id != input.state_before_version_id {
                return Err("BASELINE_STALE".into());
            }
            let source_ready: bool = tx
                .query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM source_analysis_runs run
                       JOIN novel_analysis_lineages lineage ON lineage.id=run.novel_analysis_lineage_id
                       WHERE run.id=? AND run.novel_chapter_revision_id=?
                         AND lineage.novel_work_id=? AND run.status='ready_for_review'
                     )",
                    params![input.source_analysis_run_id, input.novel_chapter_revision_id, work.id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("验证章节分析来源失败: {error}"))?;
            if !source_ready {
                return Err("章节分析 run 不存在、未完成或不属于该章节".into());
            }
            match input.adaptation_analysis_run_id.as_deref() {
                Some(_) if input.allow_source_analysis_without_adaptation => {
                    return Err("指定改编分析 run 时不能声明无改编来源".into())
                }
                Some(run_id) => ensure_approved_chapter_adaptation_run(
                    tx,
                    &work.id,
                    &input.novel_chapter_revision_id,
                    &input.source_analysis_run_id,
                    run_id,
                )?,
                None if !input.allow_source_analysis_without_adaptation => {
                    return Err(
                        "无改编来源必须显式确认 allowSourceAnalysisWithoutAdaptation".into(),
                    )
                }
                None => {}
            }
            ensure_chapter_state_transition(tx, &work, &input.novel_chapter_revision_id)?;
            let (canon_sources, state_sources) = chapter_state_sources(
                tx,
                &work.id,
                &input.novel_chapter_revision_id,
                &input.source_analysis_run_id,
                &input.source_revision_ids,
            )?;
            let state_before =
                version_body(tx, PublishTrack::State, &input.state_before_version_id)?;
            ensure_chapter_state_size(&state_before, "stateBefore")?;
            let state_after = merged_publish_body(state_before, &state_sources);
            let canon_delta =
                merged_publish_body(Value::Object(Default::default()), &canon_sources);
            let continuity_delta =
                merged_publish_body(Value::Object(Default::default()), &state_sources);
            ensure_chapter_state_size(&state_after, "stateAfter")?;
            ensure_chapter_state_size(&canon_delta, "canonDelta")?;
            ensure_chapter_state_size(&continuity_delta, "continuityDelta")?;
            let source_fingerprint = request_hash(&json!({
                "stateBeforeVersionId": input.state_before_version_id,
                "sourceAnalysisRunId": input.source_analysis_run_id,
                "adaptationAnalysisRunId": input.adaptation_analysis_run_id,
                "sourceRevisionIds": input.source_revision_ids,
                "stateAfter": state_after,
                "canonDelta": canon_delta,
                "continuityDelta": continuity_delta,
            }))?;
            let next_version: i64 = tx
                .query_row(
                    "SELECT version+1 FROM novel_state_versions WHERE id=? AND novel_work_id=?",
                    params![input.state_before_version_id, work.id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| format!("读取 State 基线版本失败: {error}"))?
                .ok_or("stateBeforeVersionId 不属于当前小说")?;
            let state_after_id = new_id("nstate");
            let publication_id = new_id("chapter_state");
            let timestamp = now();
            tx.execute(
                "INSERT INTO novel_state_versions
                 (id,novel_work_id,version,parent_version_id,through_novel_chapter_revision_id,body_json,created_at)
                 VALUES (?,?,?,?,?,?,?)",
                params![
                    state_after_id,
                    work.id,
                    next_version,
                    input.state_before_version_id,
                    input.novel_chapter_revision_id,
                    serde_json::to_string(&state_after).map_err(|_| "序列化 stateAfter 失败")?,
                    timestamp
                ],
            )
            .map_err(|error| format!("创建章节 State 版本失败: {error}"))?;
            tx.execute(
                "INSERT INTO novel_chapter_state_publications
                 (id,novel_work_id,novel_chapter_revision_id,source_analysis_run_id,adaptation_analysis_run_id,
                  state_before_version_id,state_after_version_id,canon_delta_json,continuity_delta_json,source_fingerprint,created_at)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    publication_id,
                    work.id,
                    input.novel_chapter_revision_id,
                    input.source_analysis_run_id,
                    input.adaptation_analysis_run_id,
                    input.state_before_version_id,
                    state_after_id,
                    serde_json::to_string(&canon_delta).map_err(|_| "序列化 canonDelta 失败")?,
                    serde_json::to_string(&continuity_delta).map_err(|_| "序列化 continuityDelta 失败")?,
                    source_fingerprint,
                    timestamp,
                ],
            )
            .map_err(|error| format!("创建章节状态快照失败: {error}"))?;
            for (order, source) in canon_sources.iter().enumerate() {
                tx.execute(
                    "INSERT INTO novel_chapter_state_publication_sources
                     (novel_chapter_state_publication_id,analysis_artifact_revision_id,source_role,source_order)
                     VALUES (?,?, 'canon_delta',?)",
                    params![publication_id, source.revision_id, order as i64],
                )
                .map_err(|error| format!("保存章节 Canon 来源失败: {error}"))?;
            }
            for (order, source) in state_sources.iter().enumerate() {
                tx.execute(
                    "INSERT INTO novel_chapter_state_publication_sources
                     (novel_chapter_state_publication_id,analysis_artifact_revision_id,source_role,source_order)
                     VALUES (?,?, 'continuity_delta',?)",
                    params![publication_id, source.revision_id, order as i64],
                )
                .map_err(|error| format!("保存章节连续状态来源失败: {error}"))?;
                tx.execute(
                    "INSERT INTO novel_state_version_sources
                     (novel_state_version_id,analysis_artifact_revision_id,source_role,source_order)
                     VALUES (?,?,'derived_state',?)",
                    params![state_after_id, source.revision_id, order as i64],
                )
                .map_err(|error| format!("保存 State 版本来源失败: {error}"))?;
            }
            let changed = tx
                .execute(
                    "UPDATE novel_works SET current_novel_state_version_id=?,updated_at=?
                     WHERE id=? AND project_id=? AND current_novel_state_version_id=?",
                    params![
                        state_after_id,
                        timestamp,
                        work.id,
                        work.project_id,
                        input.state_before_version_id
                    ],
                )
                .map_err(|error| format!("推进小说状态 head 失败: {error}"))?;
            if changed != 1 {
                return Err("BASELINE_STALE".into());
            }
            chapter_state_publication_from_row(tx, &publication_id)
        },
    )
}

#[tauri::command]
pub fn novel_chapter_state_publish(
    state: tauri::State<'_, DbState>,
    input: NovelChapterStatePublishInput,
) -> Result<NovelChapterStatePublication, String> {
    db::with_connection(&state, |conn| {
        novel_chapter_state_publish_inner(conn, input)
    })
}

fn novel_chapter_state_baseline_inner(
    conn: &Connection,
    input: NovelChapterStateBaselineInput,
) -> Result<NovelChapterStateBaseline, String> {
    let work = get_work(conn, &input.project_id, &input.novel_work_id)?;
    if let Some(chapter_revision_id) = input.novel_chapter_revision_id.as_deref() {
        let target: i64 = conn
            .query_row(
                "SELECT chapter.sequence_no FROM novel_chapter_revisions revision
                 JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
                 WHERE revision.id=? AND chapter.novel_work_id=?",
                params![chapter_revision_id, work.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("读取目标章节失败: {error}"))?
            .ok_or("目标章节版本不存在或不属于当前小说")?;
        let through: Option<i64> = conn
            .query_row(
                "SELECT chapter.sequence_no FROM novel_state_versions state
                 LEFT JOIN novel_chapter_revisions revision ON revision.id=state.through_novel_chapter_revision_id
                 LEFT JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
                 WHERE state.id=?",
                params![work.current_novel_state_version_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("读取当前 State 位置失败: {error}"))?
            .flatten();
        if through.is_some_and(|through| through > target) {
            return Err("目标章节早于当前已发布状态，不能作为继承起点".into());
        }
    }
    let (state_version, through_revision, raw): (i64, Option<String>, String) = conn
        .query_row(
            "SELECT version,through_novel_chapter_revision_id,body_json
             FROM novel_state_versions WHERE id=? AND novel_work_id=?",
            params![work.current_novel_state_version_id, work.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| format!("读取 State 继承基线失败: {error}"))?;
    let publication_id: Option<String> = conn
        .query_row(
            "SELECT id FROM novel_chapter_state_publications
             WHERE state_after_version_id=? ORDER BY created_at DESC,id DESC LIMIT 1",
            params![work.current_novel_state_version_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("读取章节状态快照失败: {error}"))?;
    Ok(NovelChapterStateBaseline {
        state_version_id: work.current_novel_state_version_id,
        state_version,
        through_novel_chapter_revision_id: through_revision,
        state_after: strict_json_object(&raw, "stateAfter")?,
        publication: publication_id
            .as_deref()
            .map(|id| chapter_state_publication_from_row(conn, id))
            .transpose()?,
    })
}

#[tauri::command]
pub fn novel_chapter_state_baseline(
    state: tauri::State<'_, DbState>,
    input: NovelChapterStateBaselineInput,
) -> Result<NovelChapterStateBaseline, String> {
    db::with_connection(&state, |conn| {
        novel_chapter_state_baseline_inner(conn, input)
    })
}

#[tauri::command]
pub fn novel_artifact_history(
    state: tauri::State<'_, DbState>,
    input: NovelArtifactHistoryInput,
) -> Result<NovelArtifactHistoryResult, String> {
    db::with_connection(&state, |conn| {
        let _ = get_work(conn, &input.project_id, &input.novel_work_id)?;
        let limit = input.limit.unwrap_or(50);
        if !(1..=100).contains(&limit) {
            return Err("limit 必须在 1 到 100 之间".into());
        }
        let (cursor_created_at, cursor_id) = match input.cursor.as_deref() {
            Some(cursor) => {
                let (created_at, id) = cursor.split_once(':').ok_or("cursor 格式无效")?;
                let created_at = created_at.parse::<i64>().map_err(|_| "cursor 格式无效")?;
                if id.is_empty() {
                    return Err("cursor 格式无效".into());
                }
                (Some(created_at), Some(id))
            }
            None => (None, None),
        };
        let mut stmt=conn.prepare("SELECT r.id,r.analysis_artifact_id,a.novel_work_id,r.version,r.parent_revision_id,r.status,r.body_json,r.created_at,r.change_instruction FROM analysis_artifact_revisions r JOIN analysis_artifacts a ON a.id=r.analysis_artifact_id WHERE r.analysis_artifact_id=? AND a.novel_work_id=? AND (? IS NULL OR r.created_at < ? OR (r.created_at = ? AND r.id < ?)) ORDER BY r.created_at DESC,r.id DESC LIMIT ?").map_err(|e|e.to_string())?;
        let mut all = stmt
            .query_map(
                params![
                    input.artifact_id,
                    input.novel_work_id,
                    cursor_created_at,
                    cursor_created_at,
                    cursor_created_at,
                    cursor_id,
                    limit + 1
                ],
                |r| {
                    Ok(NovelArtifactRevision {
                        id: r.get(0)?,
                        artifact_id: r.get(1)?,
                        novel_work_id: r.get(2)?,
                        revision_no: r.get(3)?,
                        parent_revision_id: r.get(4)?,
                        status: r.get(5)?,
                        content: json_value(r.get(6)?),
                        created_at: r.get(7)?,
                        change_note: r.get(8)?,
                    })
                },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let next = if all.len() > limit as usize {
            let last = all.pop().unwrap();
            Some(format!("{}:{}", last.created_at, last.id))
        } else {
            None
        };
        Ok(NovelArtifactHistoryResult {
            items: all,
            next_cursor: next,
        })
    })
}

#[cfg(test)]
fn advance_context_head_inner(
    conn: &Connection,
    lineage_id: &str,
    context_id: &str,
    expected_parent_id: Option<&str>,
    expected_sequence_no: i64,
    expected_optimistic_version: i64,
) -> Result<(), String> {
    let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|error| format!("开始工作线推进事务失败: {error}"))?;
    let valid_context: bool = transaction
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM novel_chapter_context_revisions context
               JOIN novel_chapter_revisions revision ON revision.id = context.novel_chapter_revision_id
               JOIN novel_chapters chapter ON chapter.id = revision.novel_chapter_id
               WHERE context.id = ? AND context.novel_analysis_lineage_id = ?
                 AND context.parent_context_revision_id IS ? AND chapter.sequence_no = ?
                 AND context.branch_kind <> 'detached_gap' AND context.status = 'ready'
             )",
            params![context_id, lineage_id, expected_parent_id, expected_sequence_no],
            |row| row.get(0),
        )
        .map_err(|error| format!("验证工作上下文推进条件失败: {error}"))?;
    if !valid_context {
        return Err("工作上下文不是当前主线可连续推进的候选".into());
    }
    let changed = transaction
        .execute(
            "UPDATE novel_analysis_lineages
             SET current_context_revision_id = ?, continuous_through_sequence_no = ?,
                 optimistic_version = optimistic_version + 1, updated_at = ?
             WHERE id = ? AND current_context_revision_id IS ?
               AND continuous_through_sequence_no = ? AND optimistic_version = ?",
            params![
                context_id,
                expected_sequence_no,
                now(),
                lineage_id,
                expected_parent_id,
                expected_sequence_no - 1,
                expected_optimistic_version
            ],
        )
        .map_err(|error| format!("推进工作线失败: {error}"))?;
    if changed != 1 {
        return Err("工作线已变化，请重新基准化".into());
    }
    transaction
        .commit()
        .map_err(|error| format!("提交工作线推进事务失败: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbState;

    fn open_test_db(tag: &str) -> (std::path::PathBuf, DbState) {
        let dir = std::env::temp_dir().join(format!("image-client-novel-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        (dir, state)
    }

    fn create_work(conn: &Connection, project_id: &str, key: &str) -> NovelWork {
        novel_work_create_inner(
            conn,
            NovelWorkCreateInput {
                project_id: project_id.into(),
                title: "测试小说".into(),
                description: None,
                idempotency_key: key.into(),
            },
        )
        .unwrap()
    }

    #[test]
    fn comic_plan_intent_preserves_dialogue_tristate_and_rejects_unbounded_input() {
        let intent = canonicalize_comic_plan_intent(Some(ComicPlanIntent {
            pages: vec![
                ComicPlanPageIntent {
                    panel_count: 5,
                    layout_profile: Some(" hero_middle_5 ".into()),
                    dialogues: None,
                },
                ComicPlanPageIntent {
                    panel_count: 1,
                    layout_profile: None,
                    dialogues: Some(vec![]),
                },
                ComicPlanPageIntent {
                    panel_count: 1,
                    layout_profile: None,
                    dialogues: Some(vec![ComicPlanDialogueIntent {
                        panel_no: 1,
                        speaker: "  小川  ".into(),
                        text: " 信不能湿。 ".into(),
                    }]),
                },
            ],
        }))
        .unwrap()
        .unwrap();
        assert_eq!(
            intent.pages[0].layout_profile.as_deref(),
            Some("hero_middle_5")
        );
        assert!(intent.pages[0].dialogues.is_none());
        assert_eq!(intent.pages[1].dialogues, Some(vec![]));
        assert_eq!(
            intent.pages[2].dialogues.as_ref().unwrap()[0].speaker,
            "  小川  "
        );
        assert_eq!(
            intent.pages[2].dialogues.as_ref().unwrap()[0].text,
            " 信不能湿。 "
        );
        assert!(canonicalize_comic_plan_intent(Some(ComicPlanIntent {
            pages: vec![ComicPlanPageIntent {
                panel_count: 65,
                layout_profile: None,
                dialogues: None
            }],
        }))
        .is_err());
        assert!(canonicalize_comic_plan_intent(Some(ComicPlanIntent {
            pages: vec![ComicPlanPageIntent {
                panel_count: 5,
                layout_profile: Some("diagonal_action_5".into()),
                dialogues: None
            }],
        }))
        .is_err());
    }

    fn strict_five_page_body() -> Value {
        json!({"comicChapterDraftId":"chapter","pages":[{"stableKey":"page-1","pageNo":1,"layout":{"templateId":"hero_middle_5","layoutKind":"template","panelCount":5,"readingOrder":[1,2,3,4,5],"dominantPanel":3,"geometry":{"coordinateSystem":"normalized-0-1","panelCount":5,"readingOrder":[1,2,3,4,5],"gutter":0.012,"safeArea":{"x":0.04,"y":0.04,"width":0.92,"height":0.92},"panels":[
            {"panelNo":1,"polygon":[{"x":0.04,"y":0.04},{"x":0.59,"y":0.04},{"x":0.54,"y":0.25},{"x":0.04,"y":0.25}],"bounds":{"x":0.04,"y":0.04,"width":0.55,"height":0.21},"textZone":{"x":0.08,"y":0.08,"width":0.34,"height":0.1}},
            {"panelNo":2,"polygon":[{"x":0.61,"y":0.04},{"x":0.96,"y":0.04},{"x":0.96,"y":0.25},{"x":0.56,"y":0.25}],"bounds":{"x":0.56,"y":0.04,"width":0.40,"height":0.21},"textZone":{"x":0.67,"y":0.08,"width":0.2,"height":0.1}},
            {"panelNo":3,"polygon":[{"x":0.04,"y":0.27},{"x":0.96,"y":0.27},{"x":0.96,"y":0.66},{"x":0.04,"y":0.66}],"bounds":{"x":0.04,"y":0.27,"width":0.92,"height":0.39},"textZone":{"x":0.16,"y":0.37,"width":0.62,"height":0.12}},
            {"panelNo":4,"polygon":[{"x":0.04,"y":0.68},{"x":0.43,"y":0.68},{"x":0.43,"y":0.96},{"x":0.04,"y":0.96}],"bounds":{"x":0.04,"y":0.68,"width":0.39,"height":0.28},"textZone":{"x":0.08,"y":0.75,"width":0.23,"height":0.1}},
            {"panelNo":5,"polygon":[{"x":0.45,"y":0.68},{"x":0.96,"y":0.68},{"x":0.96,"y":0.96},{"x":0.45,"y":0.96}],"bounds":{"x":0.45,"y":0.68,"width":0.51,"height":0.28},"textZone":{"x":0.57,"y":0.75,"width":0.28,"height":0.1}}
        ]}},"panels":[
            {"stableKey":"panel-1","panelNo":1,"planningSceneStableKey":"scene"},
            {"stableKey":"panel-2","panelNo":2,"planningSceneStableKey":"scene"},
            {"stableKey":"panel-3","panelNo":3,"planningSceneStableKey":"scene"},
            {"stableKey":"panel-4","panelNo":4,"planningSceneStableKey":"scene"},
            {"stableKey":"panel-5","panelNo":5,"planningSceneStableKey":"scene"}
        ]}]})
    }

    #[test]
    fn pasted_chapter_revision_uses_the_real_user_input_contract() {
        let (dir, state) = open_test_db("harness-paste-input");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "harness-project", "harness-work");
            let revision = novel_chapter_revision_create_inner(
                conn,
                NovelChapterRevisionCreateInput {
                    project_id: "harness-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_id: None,
                    volume_id: None,
                    sequence_no: Some(1),
                    chapter_no: Some(1),
                    title: Some("验收章节".into()),
                    content: "实际入库正文".into(),
                    parent_context_revision_id: None,
                    asset_id: None,
                    source_kind: Some("paste".into()),
                    idempotency_key: "harness-paste-chapter".into(),
                },
            )?;
            assert_eq!(revision.source_kind, "paste");
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM novel_chapter_revisions WHERE id=?",
                    params![revision.id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(count, 1);
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn completed_production_source(
        conn: &Connection,
        key: &str,
    ) -> Result<(NovelWork, NovelAnalysisRun), String> {
        let work = create_work(conn, "production-candidate-project", key);
        let revision = novel_chapter_revision_create_inner(
            conn,
            NovelChapterRevisionCreateInput {
                project_id: "production-candidate-project".into(),
                novel_work_id: work.id.clone(),
                chapter_id: None,
                volume_id: None,
                sequence_no: Some(1),
                chapter_no: Some(1),
                title: None,
                content: "正文".into(),
                parent_context_revision_id: None,
                asset_id: None,
                source_kind: None,
                idempotency_key: format!("{key}-revision"),
            },
        )?;
        let run = analysis_start_inner(
            conn,
            NovelAnalysisStartInput {
                project_id: "production-candidate-project".into(),
                novel_work_id: work.id.clone(),
                chapter_revision_id: revision.id,
                parent_context_revision_id: None,
                idempotency_key: format!("{key}-run"),
                provider_id: None,
                model_id: None,
            },
            true,
            "test".into(),
            "test".into(),
            "owner",
        )?;
        let finished = complete_analysis_inner(
            conn,
            &run,
            validate_analysis_output(&valid_analysis_output(
                json!({"items":[{"stableKey":"hero","resolutionKind":"conflict"}]}),
            ))?,
            "owner",
        )?;
        Ok((work, finished))
    }

    fn production_job_with_ready_source(
        conn: &Connection,
        key: &str,
    ) -> Result<
        (
            NovelWork,
            NovelChapterRevision,
            NovelProductionJob,
            NovelAnalysisRun,
        ),
        String,
    > {
        let work = create_work(conn, "production-replay-project", key);
        let revision = novel_chapter_revision_create_inner(
            conn,
            NovelChapterRevisionCreateInput {
                project_id: "production-replay-project".into(),
                novel_work_id: work.id.clone(),
                chapter_id: None,
                volume_id: None,
                sequence_no: Some(1),
                chapter_no: Some(1),
                title: None,
                content: "正文".into(),
                parent_context_revision_id: None,
                asset_id: None,
                source_kind: None,
                idempotency_key: format!("{key}-revision"),
            },
        )?;
        let job = production_start_inner(
            conn,
            &NovelProductionStartInput {
                project_id: "production-replay-project".into(),
                novel_work_id: work.id.clone(),
                novel_chapter_id: revision.chapter_id.clone(),
                source_revision_id: revision.id.clone(),
                idempotency_key: format!("{key}-job"),
                provider_id: None,
                model_id: None,
                visual_output: None,
                comic_plan_intent: None,
            },
        )?;
        let run = analysis_start_inner(
            conn,
            NovelAnalysisStartInput {
                project_id: "production-replay-project".into(),
                novel_work_id: work.id.clone(),
                chapter_revision_id: revision.id.clone(),
                parent_context_revision_id: None,
                idempotency_key: format!("{key}-source"),
                provider_id: None,
                model_id: None,
            },
            true,
            "test".into(),
            "test".into(),
            "owner",
        )?;
        let run = complete_analysis_inner(
            conn,
            &run,
            validate_analysis_output(&valid_analysis_output(
                json!({"items":[{"stableKey":"hero","resolutionKind":"conflict"}]}),
            ))?,
            "owner",
        )?;
        production_adopt_candidates(
            conn,
            "production-replay-project",
            &work.id,
            "artifact.source_analysis_run_id",
            &run.id,
            &format!("{key}-adopt"),
        )?;
        conn.execute(
            "UPDATE novel_production_jobs SET source_analysis_run_id=? WHERE id=?",
            params![run.id, job.id],
        )
        .map_err(|e| e.to_string())?;
        Ok((work, revision, job, run))
    }

    fn insert_adaptation_run_for_production(
        conn: &Connection,
        work: &NovelWork,
        job: &NovelProductionJob,
        source_run_id: &str,
        key: &str,
        status: &str,
    ) -> Result<String, String> {
        let adaptation = job
            .default_adaptation_id
            .clone()
            .ok_or("missing adaptation")?;
        let continuity: String = conn
            .query_row(
                "SELECT current_continuity_version_id FROM comic_adaptations WHERE id=?",
                params![job.default_adaptation_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let source_revisions = production_current_source_revisions(conn, source_run_id)?;
        let started = crate::novel_adaptation::start_adaptation_analysis_inner(
            conn,
            crate::novel_adaptation::AdaptationAnalysisStartInput {
                project_id: "production-replay-project".into(),
                novel_work_id: work.id.clone(),
                comic_adaptation_id: adaptation,
                comic_adaptation_chapter_id: None,
                // This is the real default production contract: ownership is
                // frozen through the adopted original artifact revisions,
                // while the nullable source_run_id remains NULL.
                source_analysis_run_id: None,
                source_artifact_revision_ids: source_revisions,
                novel_chapter_revision_id: Some(job.source_revision_id.clone()),
                base_canon_version_id: work.published_canon_version_id.clone(),
                base_novel_state_version_id: Some(work.current_novel_state_version_id.clone()),
                base_continuity_version_id: continuity,
                provider_id: None,
                model_id: None,
                comic_plan_intent: None,
                idempotency_key: key.into(),
            },
            true,
            "provider".into(),
            "model".into(),
            "test-owner",
        )?;
        if status == "ready_for_review" {
            let content: String = conn
                .query_row(
                    "SELECT content FROM novel_chapter_revisions WHERE id=?",
                    params![job.source_revision_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            crate::novel_adaptation::complete_adaptation_analysis_inner(
                conn,
                &started.run.id,
                vec![
                    ("adaptation_proposal".into(), json!({"decisions":[]})),
                    (
                        "comic_chapter_plan".into(),
                        json!({"chapters":[{"stableKey":"chapter-1","sourceSelections":[{"novelChapterRevisionId":job.source_revision_id.clone(),"startUtf8Byte":0,"endUtf8Byte":content.len()}]}]}),
                    ),
                    (
                        "scene_plan".into(),
                        json!({"comicChapterDraftId":"chapter-1","scenes":[]}),
                    ),
                    (
                        "page_panel_plan".into(),
                        json!({"comicChapterDraftId":"chapter-1","pages":[]}),
                    ),
                ],
                "test-owner",
            )?;
        }
        Ok(started.run.id)
    }

    #[test]
    fn production_frozen_comic_plan_guard_is_exact_scope_and_has_no_side_effects() {
        let (dir, state) = open_test_db("production-frozen-comic-plan-guard");
        db::with_connection(&state, |conn| {
            let (work, _revision, job, source) =
                production_job_with_ready_source(conn, "frozen-comic-plan-guard")?;
            let intent = ComicPlanIntent {
                pages: vec![ComicPlanPageIntent {
                    panel_count: 5,
                    layout_profile: Some("hero_middle_5".into()),
                    dialogues: None,
                }],
            };
            let mut scoped_job = job.clone();
            scoped_job.comic_plan_intent = Some(intent);
            let adaptation_run = insert_adaptation_run_for_production(
                conn,
                &work,
                &job,
                &source.id,
                "frozen-comic-plan-run",
                "running",
            )?;
            let adaptation = job.default_adaptation_id.clone().ok_or("missing adaptation")?;
            let chapter: String = conn.query_row(
                "SELECT id FROM comic_adaptation_chapters WHERE comic_adaptation_id=? AND novel_chapter_revision_id=?",
                params![adaptation, job.source_revision_id],
                |row| row.get(0),
            ).map_err(|e| e.to_string())?;
            let artifact = new_id("intent-page-artifact");
            let revision = new_id("intent-page-revision");
            let ts = now();
            conn.execute(
                "INSERT INTO analysis_artifacts(id,adaptation_analysis_run_id,artifact_type,novel_work_id,comic_adaptation_id,comic_chapter_id,status,optimistic_version,created_at,updated_at) VALUES (?,?, 'page_panel_plan',?,?,?,'active',0,?,?)",
                params![artifact, adaptation_run, work.id, adaptation, chapter, ts, ts],
            ).map_err(|e| e.to_string())?;
            conn.execute(
                "INSERT INTO analysis_artifact_revisions(id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,1,NULL,?,'','ai_adaptation_analysis','{}','{}','adopted',?)",
                params![revision, artifact, strict_five_page_body().to_string(), ts],
            ).map_err(|e| e.to_string())?;
            let before: (i64, i64) = conn.query_row(
                "SELECT (SELECT COUNT(*) FROM comic_production_chapters),(SELECT COUNT(*) FROM comic_visual_batches)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).map_err(|e| e.to_string())?;
            production_validate_frozen_comic_plan_intent(conn, &scoped_job, &adaptation_run, &revision)?;
            conn.execute(
                "UPDATE analysis_artifact_revisions SET body_json='{}' WHERE id=?",
                params![revision],
            ).map_err(|e| e.to_string())?;
            assert!(production_validate_frozen_comic_plan_intent(conn, &scoped_job, &adaptation_run, &revision).is_err());
            conn.execute(
                "UPDATE analysis_artifact_revisions SET body_json=? WHERE id=?",
                params![strict_five_page_body().to_string(), revision],
            ).map_err(|e| e.to_string())?;
            assert!(production_validate_frozen_comic_plan_intent(conn, &scoped_job, "wrong-run", &revision).is_err());
            let mut wrong_work = scoped_job.clone();
            wrong_work.novel_work_id = "wrong-work".into();
            assert!(production_validate_frozen_comic_plan_intent(conn, &wrong_work, &adaptation_run, &revision).is_err());
            let mut wrong_project = scoped_job.clone();
            wrong_project.project_id = "wrong-project".into();
            assert!(production_validate_frozen_comic_plan_intent(conn, &wrong_project, &adaptation_run, &revision).is_err());
            let after: (i64, i64) = conn.query_row(
                "SELECT (SELECT COUNT(*) FROM comic_production_chapters),(SELECT COUNT(*) FROM comic_visual_batches)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).map_err(|e| e.to_string())?;
            assert_eq!(after, before, "guard must reject before any tree or visual batch write");
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_recovery_repairs_only_ready_job_owned_run_pointers() {
        let (dir, state) = open_test_db("production-pointer-recovery");
        db::with_connection(&state, |conn| {
            let (work, _revision, job, source) =
                production_job_with_ready_source(conn, "pointer-recovery")?;
            let source_key = format!("{}:source:{}", job.id, job.attempt_no);
            conn.execute(
                "UPDATE source_analysis_runs SET idempotency_key=? WHERE id=?",
                params![source_key, source.id],
            )
            .map_err(|e| e.to_string())?;
            let adaptation = insert_adaptation_run_for_production(
                conn,
                &work,
                &job,
                &source.id,
                &format!("{}:adaptation:{}", job.id, job.attempt_no),
                "ready_for_review",
            )?;
            conn.execute(
                "UPDATE novel_production_jobs SET source_analysis_run_id=NULL,adaptation_analysis_run_id=NULL WHERE id=?",
                params![job.id],
            )
            .map_err(|e| e.to_string())?;
            let recovered = production_reconcile_ready_run_pointers(conn, &job)?;
            assert_eq!(recovered.source_analysis_run_id.as_deref(), Some(source.id.as_str()));
            assert_eq!(recovered.adaptation_analysis_run_id.as_deref(), Some(adaptation.as_str()));
            let counts: (i64, i64) = conn.query_row(
                "SELECT (SELECT COUNT(*) FROM source_analysis_runs),(SELECT COUNT(*) FROM adaptation_analysis_runs)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).map_err(|e|e.to_string())?;
            let replay = production_reconcile_ready_run_pointers(conn, &recovered)?;
            let replay_counts: (i64, i64) = conn.query_row(
                "SELECT (SELECT COUNT(*) FROM source_analysis_runs),(SELECT COUNT(*) FROM adaptation_analysis_runs)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).map_err(|e|e.to_string())?;
            assert_eq!(replay.source_analysis_run_id, recovered.source_analysis_run_id);
            assert_eq!(counts, replay_counts, "recovery must attach, not create runs");

            let input_mode: String = conn
                .query_row(
                    "SELECT input_mode FROM adaptation_analysis_runs WHERE id=?",
                    params![adaptation],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(input_mode, "artifact_revisions");

            // A separate job gets a real, initially-running artifact-revision
            // run with its deterministic key. It must not be attached as a
            // ready pointer or treated like no prior request existed.
            let (pending_work, _pending_revision, pending_job, pending_source) =
                production_job_with_ready_source(conn, "pointer-pending")?;
            conn.execute(
                "UPDATE source_analysis_runs SET idempotency_key=? WHERE id=?",
                params![
                    format!("{}:source:{}", pending_job.id, pending_job.attempt_no),
                    pending_source.id
                ],
            )
            .map_err(|e| e.to_string())?;
            let _pending_adaptation = insert_adaptation_run_for_production(
                conn,
                &pending_work,
                &pending_job,
                &pending_source.id,
                &format!("{}:adaptation:{}", pending_job.id, pending_job.attempt_no),
                "running",
            )?;
            conn.execute("UPDATE novel_production_jobs SET source_analysis_run_id=NULL,adaptation_analysis_run_id=NULL WHERE id=?",params![pending_job.id]).map_err(|e|e.to_string())?;
            let pending_adaptation = production_reconcile_ready_run_pointers(conn, &pending_job)?;
            assert_eq!(pending_adaptation.source_analysis_run_id.as_deref(), Some(pending_source.id.as_str()));
            assert!(pending_adaptation.adaptation_analysis_run_id.is_none(), "a non-ready deterministic artifact-revision run must not be attached as ready");
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_reconcile_defers_adaptation_pointer_until_source_candidates_are_adopted() {
        let (dir, state) = open_test_db("production-reconcile-unadopted-source");
        db::with_connection(&state, |conn| {
            let (work, source) = completed_production_source(conn, "reconcile-unadopted")?;
            let chapter_id: String = conn
                .query_row(
                    "SELECT novel_chapter_id FROM novel_chapter_revisions WHERE id=?",
                    params![source.chapter_revision_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let job = production_start_inner(
                conn,
                &NovelProductionStartInput {
                    project_id: "production-candidate-project".into(),
                    novel_work_id: work.id.clone(),
                    novel_chapter_id: chapter_id,
                    source_revision_id: source.chapter_revision_id.clone(),
                    idempotency_key: "reconcile-unadopted-job".into(),
                    provider_id: None,
                    model_id: None,
                    visual_output: None,
                    comic_plan_intent: None,
                },
            )?;
            conn.execute(
                "UPDATE novel_production_jobs SET status='running',stage='source_analysis',stage_index=2 WHERE id=?",
                params![job.id],
            )
            .map_err(|e| e.to_string())?;
            let running = production_job_value(
                conn,
                "production-candidate-project",
                &work.id,
                &job.id,
            )?;
            let staged = production_finish_source(conn, &running, &source)?;
            assert_eq!(staged.stage, "integrating_context");

            let candidate_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM analysis_artifacts WHERE source_analysis_run_id=? AND candidate_head_revision_id IS NOT NULL AND adopted_head_revision_id IS NULL",
                    params![source.id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(candidate_count, REQUIRED_ARTIFACT_TYPES.len() as i64);
            assert_eq!(
                production_current_source_revisions(conn, &source.id).unwrap_err(),
                "PRODUCTION_SOURCE_INPUTS_INCOMPLETE"
            );

            let reconciled = production_reconcile_ready_run_pointers(conn, &staged)?;
            assert_eq!(reconciled.source_analysis_run_id.as_deref(), Some(source.id.as_str()));
            assert!(reconciled.adaptation_analysis_run_id.is_none());
            let adaptation_runs: i64 = conn
                .query_row("SELECT COUNT(*) FROM adaptation_analysis_runs", [], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(adaptation_runs, 0, "pointer repair must not create a replacement run");

            let adopted = production_adopt_candidates(
                conn,
                "production-candidate-project",
                &work.id,
                "artifact.source_analysis_run_id",
                &source.id,
                "reconcile-unadopted-adopt",
            )?;
            assert_eq!(adopted.len(), REQUIRED_ARTIFACT_TYPES.len());
            let inputs = production_source_input_revisions(&adopted)?;
            assert_eq!(inputs.len(), PRODUCTION_SOURCE_ARTIFACT_TYPES.len());
            let mut missing = adopted.clone();
            missing.remove(PRODUCTION_SOURCE_ARTIFACT_TYPES[0]);
            assert_eq!(
                production_source_input_revisions(&missing).unwrap_err(),
                format!("SOURCE_ARTIFACT_MISSING:{}", PRODUCTION_SOURCE_ARTIFACT_TYPES[0])
            );

            let replay = production_reconcile_ready_run_pointers(conn, &reconciled)?;
            assert!(replay.adaptation_analysis_run_id.is_none());
            let replay_adaptation_runs: i64 = conn
                .query_row("SELECT COUNT(*) FROM adaptation_analysis_runs", [], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(replay_adaptation_runs, 0);

            let broken = Connection::open_in_memory().map_err(|e| e.to_string())?;
            assert_eq!(
                production_reconcile_ready_run_pointers(&broken, &job).unwrap_err(),
                "PRODUCTION_RECOVERY_READ_FAILED"
            );
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_recovery_error_pauses_only_the_faulted_job() {
        let (dir, state) = open_test_db("production-recovery-isolation");
        let job = db::with_connection(&state, |conn| {
            let (_work, _revision, job, _source) =
                production_job_with_ready_source(conn, "recovery-isolation")?;
            Ok(job)
        })
        .unwrap();
        production_pause_recovery_error(&state, &job.id, "PRODUCTION_BASELINE_STALE");
        db::with_connection(&state, |conn| {
            let paused = production_job_value(conn, &job.project_id, &job.novel_work_id, &job.id)?;
            assert_eq!(paused.status, "blocked_conflict");
            assert_eq!(
                paused.safe_error_code.as_deref(),
                Some("PRODUCTION_BASELINE_STALE")
            );
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_job_is_idempotent_scoped_and_persists_its_default_adaptation() {
        let (dir, state) = open_test_db("production-job");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "production-project", "production-work");
            let revision = novel_chapter_revision_create_inner(conn, NovelChapterRevisionCreateInput {
                project_id: "production-project".into(), novel_work_id: work.id.clone(), chapter_id: None,
                volume_id: None, sequence_no: Some(1), chapter_no: Some(1), title: Some("第一章".into()),
                content: "正文".into(), parent_context_revision_id: None, asset_id: None, source_kind: None,
                idempotency_key: "production-revision".into(),
            })?;
            let input=NovelProductionStartInput { project_id:"production-project".into(),novel_work_id:work.id.clone(),novel_chapter_id:revision.chapter_id.clone(),source_revision_id:revision.id.clone(),idempotency_key:"production-start".into(),provider_id:None,model_id:None,visual_output:None,comic_plan_intent:None };
            let first=production_start_inner(conn,&input)?;
            let replay=production_start_inner(conn,&input)?;
            assert_eq!(first.id,replay.id);
            let legacy_alias = NovelProductionStartInput {
                idempotency_key: "production-start-legacy-alias".into(),
                ..input.clone()
            };
            assert_eq!(production_start_inner(conn, &legacy_alias)?.id, first.id);
            let conflicting_intent = NovelProductionStartInput {
                idempotency_key: "production-start-intent".into(),
                comic_plan_intent: Some(ComicPlanIntent {
                    pages: vec![ComicPlanPageIntent {
                        panel_count: 5,
                        layout_profile: Some("hero_middle_5".into()),
                        dialogues: None,
                    }],
                }),
                ..input.clone()
            };
            assert_eq!(
                production_start_inner(conn, &conflicting_intent).unwrap_err(),
                "COMIC_PLAN_INTENT_CONFLICT"
            );
            assert_eq!(first.status,"queued");
            assert_eq!(first.default_adaptation_id.as_deref().is_some(),true);
            let events:i64=conn.query_row("SELECT COUNT(*) FROM novel_production_job_events WHERE novel_production_job_id=?",params![first.id],|row|row.get(0)).map_err(|e|e.to_string())?;
            assert_eq!(events,1);
            assert!(production_job_value(conn,"foreign-project",&work.id,&first.id).is_err());
            assert!(production_start_inner(conn,&NovelProductionStartInput { project_id:"production-project".into(),novel_work_id:work.id.clone(),novel_chapter_id:revision.chapter_id.clone(),source_revision_id:revision.id.clone(),idempotency_key:"production-start".into(),provider_id:Some("other".into()),model_id:None,visual_output:None,comic_plan_intent:None }).is_err());
            assert_eq!(production_start_inner(conn,&NovelProductionStartInput { project_id:"production-project".into(),novel_work_id:work.id,novel_chapter_id:"wrong-chapter".into(),source_revision_id:revision.id,idempotency_key:"production-wrong-scope".into(),provider_id:None,model_id:None,visual_output:None,comic_plan_intent:None }).unwrap_err(),"NOVEL_CHAPTER_REVISION_MISMATCH");
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn analysis_start_replay_returns_current_run_without_a_second_dispatch_lease() {
        let (dir, state) = open_test_db("analysis-start-replay-current");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "production-project", "replay-current-work");
            let revision = novel_chapter_revision_create_inner(conn, NovelChapterRevisionCreateInput {
                project_id: "production-project".into(), novel_work_id: work.id.clone(), chapter_id: None,
                volume_id: None, sequence_no: Some(1), chapter_no: Some(1), title: None,
                content: "正文".into(), parent_context_revision_id: None, asset_id: None, source_kind: None,
                idempotency_key: "analysis-replay-revision".into(),
            })?;
            let input = NovelAnalysisStartInput {
                project_id: "production-project".into(), novel_work_id: work.id.clone(),
                chapter_revision_id: revision.id, parent_context_revision_id: None,
                idempotency_key: "analysis-replay-key".into(), provider_id: None, model_id: None,
            };
            let first = analysis_start_outcome_inner(conn, input.clone(), true, "provider".into(), "model".into(), "owner")?;
            assert!(first.created);
            let running_replay = analysis_start_outcome_inner(conn, input.clone(), true, "provider".into(), "model".into(), "owner")?;
            assert!(!running_replay.created);
            assert_eq!(running_replay.run.status, "running");
            conn.execute("UPDATE source_analysis_runs SET status='error',safe_error_code='ANALYSIS_FAILED' WHERE id=?", params![first.run.id]).map_err(|e| e.to_string())?;
            let error_replay = analysis_start_outcome_inner(conn, input.clone(), true, "provider".into(), "model".into(), "owner")?;
            assert!(!error_replay.created);
            assert_eq!(error_replay.run.id, first.run.id);
            assert_eq!(error_replay.run.status, "error", "replay must read durable status instead of the receipt's initial running snapshot");
            conn.execute("UPDATE source_analysis_runs SET status='ready_for_review',safe_error_code=NULL WHERE id=?", params![first.run.id]).map_err(|e| e.to_string())?;
            let ready_replay = analysis_start_outcome_inner(conn, input, true, "provider".into(), "model".into(), "owner")?;
            assert!(!ready_replay.created);
            assert_eq!(ready_replay.run.status, "ready_for_review");
            let attempts: i64 = conn.query_row("SELECT COUNT(*) FROM source_analysis_run_attempts WHERE source_analysis_run_id=?", params![first.run.id], |row| row.get(0)).map_err(|e| e.to_string())?;
            assert_eq!(attempts, 1, "receipt replay must not create a second dispatch lease");
            let receipt: Value = conn.query_row("SELECT response_json FROM novel_operation_receipts WHERE command_name='novel_analysis_start' AND idempotency_key='analysis-replay-key'", [], |row| row.get::<_, String>(0)).map_err(|e| e.to_string()).and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))?;
            assert!(receipt.get("created").is_none(), "dispatch disposition must never be persisted in the replay receipt");
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_recovery_receipt_is_attempt_scoped_and_rejects_an_old_claim() {
        let (dir, state) = open_test_db("production-recovery-attempt");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "production-project", "recovery-attempt-work");
            let revision = novel_chapter_revision_create_inner(conn, NovelChapterRevisionCreateInput {
                project_id: "production-project".into(), novel_work_id: work.id.clone(), chapter_id: None,
                volume_id: None, sequence_no: Some(1), chapter_no: Some(1), title: None,
                content: "正文".into(), parent_context_revision_id: None, asset_id: None, source_kind: None,
                idempotency_key: "recovery-attempt-revision".into(),
            })?;
            let old = production_start_inner(conn, &NovelProductionStartInput {
                project_id: "production-project".into(), novel_work_id: work.id.clone(), novel_chapter_id: revision.chapter_id,
                source_revision_id: revision.id, idempotency_key: "recovery-attempt-start".into(), provider_id: None, model_id: None, visual_output: None, comic_plan_intent: None,
            })?;
            let queued = production_retry_or_resume(conn, "novel_production_resume", "production-project", &work.id, &old.id, "queued-once", None, true)?;
            assert_eq!(queued.attempt_no, old.attempt_no);
            assert!(production_resume_should_schedule(&queued));
            let waiting_ready = NovelProductionJob { status: "waiting_for_predecessor".into(), source_analysis_run_id: Some("ready-source".into()), ..queued.clone() };
            assert!(production_resume_should_schedule(&waiting_ready), "a durable ready source must still be eligible for downstream continuation");
            conn.execute("UPDATE novel_production_jobs SET status='error',stage='source_analysis',stage_index=2 WHERE id=?", params![old.id]).map_err(|e| e.to_string())?;
            let queued_replay = production_retry_or_resume(conn, "novel_production_resume", "production-project", &work.id, &old.id, "queued-once", None, true)?;
            assert_eq!(queued_replay.status, "error", "a replay must report current truth instead of creating a new retry");
            assert_eq!(queued_replay.attempt_no, old.attempt_no);
            let first = production_retry_or_resume(conn, "novel_production_resume", "production-project", &work.id, &old.id, "resume-once", None, true)?;
            assert_eq!(first.status, "queued");
            assert_eq!(first.attempt_no, old.attempt_no + 1);
            let replay = production_retry_or_resume(conn, "novel_production_resume", "production-project", &work.id, &old.id, "resume-once", None, true)?;
            assert_eq!(replay.attempt_no, first.attempt_no, "same recovery receipt must not create another attempt");
            assert!(!production_claim_source(conn, &old, "late-owner")?, "a stale scheduled snapshot must not claim the new attempt");
            let current = production_job_value(conn, "production-project", &work.id, &old.id)?;
            assert_eq!(current.attempt_no, first.attempt_no);
            assert_eq!(current.status, "queued");
            assert!(production_claim_source(conn, &current, "attempt-two-owner")?);
            let late_source = NovelAnalysisRun {
                id: "old-source-run".into(), novel_work_id: work.id.clone(), chapter_revision_id: old.source_revision_id.clone(),
                lineage_id: "old-lineage".into(), parent_context_revision_id: None, status: "ready_for_review".into(),
                attempt_no: 1, safe_error: None,
            };
            assert_eq!(production_finish_source(conn, &old, &late_source).unwrap_err(), "PRODUCTION_ATTEMPT_SUPERSEDED");
            production_record_background_failure(conn, &old, "late provider failure")?;
            let after_running_callbacks = production_job_value(conn, "production-project", &work.id, &old.id)?;
            assert_eq!(after_running_callbacks.attempt_no, current.attempt_no);
            assert_eq!(after_running_callbacks.status, "running", "an old callback must not overwrite the new attempt while its source lease is live");
            let running_late_events: i64 = conn.query_row("SELECT COUNT(*) FROM novel_production_job_events WHERE novel_production_job_id=? AND event_type IN ('source_finished','background_failed')", params![old.id], |row| row.get(0)).map_err(|e| e.to_string())?;
            assert_eq!(running_late_events, 0, "discarded callbacks must not create events while the new attempt is running");
            conn.execute("UPDATE novel_production_jobs SET status='waiting_for_predecessor',stage='integrating_context',stage_index=3 WHERE id=? AND attempt_no=?", params![old.id, current.attempt_no]).map_err(|e| e.to_string())?;
            assert_eq!(production_finish_source(conn, &old, &late_source).unwrap_err(), "PRODUCTION_ATTEMPT_SUPERSEDED");
            production_record_background_failure(conn, &old, "late provider failure after progress")?;
            let after_late_error = production_job_value(conn, "production-project", &work.id, &old.id)?;
            assert_eq!(after_late_error.attempt_no, current.attempt_no);
            assert_eq!(after_late_error.status, "waiting_for_predecessor", "old failure must not mark the new attempt failed");
            assert_eq!(after_late_error.stage, "integrating_context", "old completion must not continue the new attempt downstream");
            let late_events: i64 = conn.query_row("SELECT COUNT(*) FROM novel_production_job_events WHERE novel_production_job_id=? AND event_type IN ('source_finished','background_failed')", params![old.id], |row| row.get(0)).map_err(|e| e.to_string())?;
            assert_eq!(late_events, 0, "discarded callbacks must not create events");
            let retry_events: i64 = conn.query_row("SELECT COUNT(*) FROM novel_production_job_events WHERE novel_production_job_id=? AND event_type='retry_requested'", params![old.id], |row| row.get(0)).map_err(|e| e.to_string())?;
            assert_eq!(retry_events, 1);
            for (sequence, status, key) in [(2_i64, "stale", "resume-stale"), (3_i64, "blocked_config", "resume-config")] {
                let revision = novel_chapter_revision_create_inner(conn, NovelChapterRevisionCreateInput {
                    project_id: "production-project".into(), novel_work_id: work.id.clone(), chapter_id: None,
                    volume_id: None, sequence_no: Some(sequence), chapter_no: Some(sequence), title: None,
                    content: format!("正文 {sequence}"), parent_context_revision_id: None, asset_id: None, source_kind: None,
                    idempotency_key: format!("recovery-{status}-revision"),
                })?;
                let job = production_start_inner(conn, &NovelProductionStartInput {
                    project_id: "production-project".into(), novel_work_id: work.id.clone(), novel_chapter_id: revision.chapter_id,
                    source_revision_id: revision.id, idempotency_key: format!("recovery-{status}-start"), provider_id: None, model_id: None, visual_output: None, comic_plan_intent: None,
                })?;
                conn.execute("UPDATE novel_production_jobs SET status=?,stage='source_analysis',stage_index=2 WHERE id=?", params![status, job.id]).map_err(|e| e.to_string())?;
                let resumed = production_retry_or_resume(conn, "novel_production_resume", "production-project", &work.id, &job.id, key, None, true)?;
                assert_eq!(resumed.status, "queued");
                assert_eq!(resumed.attempt_no, job.attempt_no + 1, "{status} must create exactly one explicit new attempt");
            }
            assert!(production_retry_or_resume(conn, "novel_production_resume", "production-project", &work.id, &old.id, "   ", None, true).is_err(), "empty recovery keys must fail before any receipt or attempt mutation");
            assert!(production_retry_or_resume(conn, "novel_production_retry_stage", "production-project", "foreign-work", &old.id, "foreign-scope", Some("source_analysis"), false).is_err());
            assert!(production_retry_or_resume(conn, "novel_production_retry_stage", "production-project", &work.id, &old.id, "stage-conflict", Some("succeeded"), false).is_err());
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_transition_matrix_fails_closed_and_allows_resume_only_forward() {
        assert!(production_transition_allowed(
            "queued",
            "resolving_inheritance",
            "running",
            "source_analysis"
        ));
        assert!(production_transition_allowed(
            "running",
            "source_analysis",
            "waiting_for_predecessor",
            "integrating_context"
        ));
        assert!(production_transition_allowed(
            "waiting_for_predecessor",
            "integrating_context",
            "running",
            "ensuring_adaptation"
        ));
        assert!(production_transition_allowed(
            "error",
            "adaptation_analysis",
            "queued",
            "adaptation_analysis"
        ));
        assert!(production_transition_allowed(
            "running",
            "ensuring_adaptation",
            "running",
            "applying_production"
        ));
        assert!(production_transition_allowed(
            "running",
            "applying_production",
            "succeeded",
            "succeeded"
        ));
        assert!(!production_transition_allowed(
            "succeeded",
            "succeeded",
            "running",
            "applying_production"
        ));
        assert!(!production_transition_allowed(
            "running",
            "applying_production",
            "running",
            "ensuring_adaptation"
        ));
        assert!(!production_transition_allowed(
            "succeeded",
            "succeeded",
            "running",
            "succeeded"
        ));
        assert!(!production_transition_allowed(
            "running",
            "applying_production",
            "running",
            "source_analysis"
        ));
        assert!(!production_transition_allowed(
            "waiting_for_predecessor",
            "integrating_context",
            "succeeded",
            "succeeded"
        ));
    }

    #[test]
    fn production_update_stage_advances_running_adoption_without_dropping_pointers() {
        let (dir, state) = open_test_db("production-running-stage-advance");
        db::with_connection(&state, |conn| {
            let (work, _revision, job, source) =
                production_job_with_ready_source(conn, "running-stage-advance")?;
            let adaptation = insert_adaptation_run_for_production(
                conn,
                &work,
                &job,
                &source.id,
                "running-stage-advance-adaptation",
                "running",
            )?;
            conn.execute(
                "UPDATE novel_production_jobs SET status='running',stage='ensuring_adaptation',stage_index=4,source_analysis_run_id=?,adaptation_analysis_run_id=? WHERE id=?",
                params![source.id, adaptation, job.id],
            )
            .map_err(|e| e.to_string())?;

            let advanced = production_update_stage(
                conn,
                &job.id,
                "running",
                "applying_production",
                7,
                14,
                Some("冻结场景上下文并创建漫画生产树"),
                None,
            )?;
            assert_eq!(advanced.status, "running");
            assert_eq!(advanced.stage, "applying_production");
            assert_eq!(advanced.stage_index, 7);
            assert_eq!(advanced.completed_artifact_count, 14);
            assert_eq!(advanced.attempt_no, job.attempt_no);
            assert_eq!(advanced.source_analysis_run_id.as_deref(), Some(source.id.as_str()));
            assert_eq!(advanced.adaptation_analysis_run_id.as_deref(), Some(adaptation.as_str()));
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_scene_context_input_uses_proposal_revision_not_plan_head() {
        let (dir, state) = open_test_db("production-scene-context-proposal");
        db::with_connection(&state, |conn| {
            let (_work, _revision, job, _source) =
                production_job_with_ready_source(conn, "scene-context-proposal")?;
            let proposal_revision = "adaptartifactrev-proposal";
            let active_plan_head = "aplan-active-head";
            let input = production_scene_context_resolve_input(
                &job,
                "nadaptation-test",
                "adaptartifactrev-scene",
                "scene-1",
                None,
                "ncanon-test",
                "nstate-test",
                "continuity-test",
                proposal_revision,
                vec![],
                vec![],
            );
            assert_ne!(active_plan_head, proposal_revision);
            assert_eq!(input.adaptation_plan_revision_id, proposal_revision);
            assert_eq!(input.scene_plan_revision_id, "adaptartifactrev-scene");
            assert_eq!(input.idempotency_key, format!("{}:scene:scene-1", job.id));
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_revised_chapter_keeps_distinct_frozen_analysis_scopes() {
        let (dir, state) = open_test_db("production-revised-chapter");
        db::with_connection(&state, |conn| {
            let (work, original, _old_job, old_run) =
                production_job_with_ready_source(conn, "revised-chapter")?;
            let revised = novel_chapter_revision_create_inner(conn, NovelChapterRevisionCreateInput {
                project_id: work.project_id.clone(), novel_work_id: work.id.clone(),
                chapter_id: Some(original.chapter_id.clone()), volume_id: None,
                sequence_no: None, chapter_no: None, title: None, content: "修改后的正文".into(),
                parent_context_revision_id: None, asset_id: None, source_kind: None,
                idempotency_key: "revised-chapter-save".into(),
            })?;
            let job = production_start_inner(conn, &NovelProductionStartInput {
                project_id: work.project_id.clone(), novel_work_id: work.id.clone(),
                novel_chapter_id: revised.chapter_id.clone(), source_revision_id: revised.id.clone(),
                idempotency_key: "revised-chapter-new-job".into(), provider_id: None, model_id: None,
                visual_output: None, comic_plan_intent: None,
            })?;
            assert!(production_claim_source(conn, &job, "owner")?);
            let input = NovelAnalysisStartInput {
                project_id: work.project_id.clone(), novel_work_id: work.id.clone(),
                chapter_revision_id: revised.id.clone(), parent_context_revision_id: None,
                idempotency_key: format!("{}:source:1", job.id), provider_id: None, model_id: None,
            };
            let run = analysis_start_inner(conn, input.clone(), true, "test".into(), "test".into(), "owner")?;
            let replay = analysis_start_outcome_inner(conn, input, true, "test".into(), "test".into(), "owner")?;
            assert_eq!(run.status, "running");
            assert_eq!(run.id, replay.run.id);
            assert!(!replay.created);
            let (old_revision, old_adaptation, old_chapter): (String, String, String) = conn.query_row(
                "SELECT novel_chapter_revision_id,frozen_comic_adaptation_id,frozen_comic_chapter_id FROM source_analysis_runs WHERE id=?",
                params![old_run.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).map_err(|error| error.to_string())?;
            let new = analysis_prompt_input(conn, &run.id)?;
            assert_eq!(old_revision, original.id);
            assert_eq!(new.chapter_revision_id, revised.id);
            assert_eq!(old_adaptation, new.frozen_adaptation_id);
            assert_ne!(old_chapter, new.frozen_comic_chapter_id);
            for (scope, revision) in [(&old_chapter, &original.id), (&new.frozen_comic_chapter_id, &revised.id)] {
                let actual: String = conn.query_row("SELECT novel_chapter_revision_id FROM comic_adaptation_chapters WHERE id=?", params![scope], |row| row.get(0)).map_err(|error| error.to_string())?;
                assert_eq!(&actual, revision);
            }
            let broken: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| row.get(0)).map_err(|error| error.to_string())?;
            assert_eq!(broken, 0);
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_background_failure_is_durable_and_releases_the_job_lease() {
        let (dir, state) = open_test_db("production-background-failure");
        db::with_connection(&state, |conn| {
            let work=create_work(conn,"production-project","production-background-work");
            let revision=novel_chapter_revision_create_inner(conn,NovelChapterRevisionCreateInput { project_id:"production-project".into(),novel_work_id:work.id.clone(),chapter_id:None,volume_id:None,sequence_no:Some(1),chapter_no:Some(1),title:None,content:"正文".into(),parent_context_revision_id:None,asset_id:None,source_kind:None,idempotency_key:"production-background-revision".into() })?;
            let job=production_start_inner(conn,&NovelProductionStartInput { project_id:"production-project".into(),novel_work_id:work.id.clone(),novel_chapter_id:revision.chapter_id,source_revision_id:revision.id,idempotency_key:"production-background-start".into(),provider_id:None,model_id:None,visual_output:None,comic_plan_intent:None })?;
            conn.execute("UPDATE novel_production_jobs SET status='running',stage='source_analysis',stage_index=2,lease_owner='test-owner',lease_expires_at=? WHERE id=?",params![now()+1000,job.id]).map_err(|e|e.to_string())?;
            production_record_background_failure(conn,&job,"provider transport failure")?;
            let (status,lease,error):(String,Option<String>,Option<String>)=conn.query_row("SELECT status,lease_owner,safe_error_code FROM novel_production_jobs WHERE id=?",params![job.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).map_err(|e|e.to_string())?;
            assert_eq!(status,"error"); assert_eq!(lease,None); assert_eq!(error.as_deref(),Some("PRODUCTION_FAILED"));
            let events:i64=conn.query_row("SELECT COUNT(*) FROM novel_production_job_events WHERE novel_production_job_id=? AND event_type='background_failed'",params![job.id],|row|row.get(0)).map_err(|e|e.to_string())?;
            assert_eq!(events,1);
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_command_inputs_match_the_camel_case_frontend_contract() {
        let start:NovelProductionStartInput=serde_json::from_value(json!({"projectId":"p","novelWorkId":"w","novelChapterId":"c","sourceRevisionId":"r","idempotencyKey":"k"})).unwrap();
        assert_eq!(
            (
                &start.project_id,
                &start.novel_work_id,
                &start.novel_chapter_id,
                &start.source_revision_id
            ),
            (&"p".into(), &"w".into(), &"c".into(), &"r".into())
        );
        let get: NovelProductionGetInput = serde_json::from_value(
            json!({"projectId":"p","novelWorkId":"w","productionJobId":"job"}),
        )
        .unwrap();
        assert_eq!(get.production_job_id, "job");
        let legacy_get: NovelProductionGetInput = serde_json::from_value(
            json!({"projectId":"p","novelWorkId":"w","novelProductionJobId":"legacy"}),
        )
        .unwrap();
        assert_eq!(legacy_get.production_job_id, "legacy");
        let resume:NovelProductionJobInput=serde_json::from_value(json!({"projectId":"p","novelWorkId":"w","productionJobId":"job","idempotencyKey":"resume"})).unwrap();
        assert_eq!(resume.production_job_id, "job");
        let retry:NovelProductionRetryStageInput=serde_json::from_value(json!({"projectId":"p","novelWorkId":"w","productionJobId":"job","stage":"adaptation_analysis","idempotencyKey":"retry"})).unwrap();
        assert_eq!(
            (retry.production_job_id, retry.stage),
            ("job".to_string(), "adaptation_analysis".to_string())
        );
    }

    #[test]
    fn production_wrapper_rejects_manual_candidate_head_without_adopted_head() {
        let (dir, state) = open_test_db("production-manual-candidate");
        db::with_connection(&state,|conn| { let (work,run)=completed_production_source(conn,"manual-candidate")?; let (artifact,_version):(String,i64)=conn.query_row("SELECT id,optimistic_version FROM analysis_artifacts WHERE source_analysis_run_id=? ORDER BY id LIMIT 1",params![run.id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?; let manual=new_id("manual"); conn.execute("INSERT INTO analysis_artifact_revisions(id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,2,(SELECT candidate_head_revision_id FROM analysis_artifacts WHERE id=?),(SELECT body_json FROM analysis_artifact_revisions WHERE id=(SELECT candidate_head_revision_id FROM analysis_artifacts WHERE id=?)),'','manual_edit','{}','{}','candidate',?)",params![manual,artifact,artifact,artifact,now()]).map_err(|e|e.to_string())?; conn.execute("UPDATE analysis_artifacts SET candidate_head_revision_id=?,optimistic_version=1 WHERE id=?",params![manual,artifact]).map_err(|e|e.to_string())?; let result=production_adopt_candidates(conn,"production-candidate-project",&work.id,"artifact.source_analysis_run_id",&run.id,"manual-candidate"); assert!(matches!(result,Err(error) if error.starts_with("PRODUCTION_CANDIDATE_DRIFT:"))); Ok(()) }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_wrapper_reuses_explicit_manual_adopted_head() {
        let (dir, state) = open_test_db("production-manual-adopted");
        db::with_connection(&state,|conn| { let (work,run)=completed_production_source(conn,"manual-adopted")?; let (artifact,_version):(String,i64)=conn.query_row("SELECT id,optimistic_version FROM analysis_artifacts WHERE source_analysis_run_id=? ORDER BY id LIMIT 1",params![run.id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?; let manual=new_id("manual"); conn.execute("INSERT INTO analysis_artifact_revisions(id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,2,(SELECT candidate_head_revision_id FROM analysis_artifacts WHERE id=?),(SELECT body_json FROM analysis_artifact_revisions WHERE id=(SELECT candidate_head_revision_id FROM analysis_artifacts WHERE id=?)),'','manual_edit','{}','{}','adopted',?)",params![manual,artifact,artifact,artifact,now()]).map_err(|e|e.to_string())?; conn.execute("UPDATE analysis_artifacts SET candidate_head_revision_id=?,adopted_head_revision_id=?,optimistic_version=1 WHERE id=?",params![manual,manual,artifact]).map_err(|e|e.to_string())?; let result=production_adopt_candidates(conn,"production-candidate-project",&work.id,"artifact.source_analysis_run_id",&run.id,"manual-adopted")?; assert!(result.values().any(|id|id==&manual)); Ok(()) }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_adoption_receipt_is_scoped_to_the_exact_adaptation_run_and_revision() {
        let (dir, state) = open_test_db("production-adoption-retry-scope");
        db::with_connection(&state, |conn| {
            let (work, _revision, job, source) =
                production_job_with_ready_source(conn, "adoption-retry-scope")?;
            let prefix = format!("{}:adaptation", job.id);
            let receipt_count = |conn: &Connection| {
                conn.query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM novel_operation_receipts
                     WHERE command_name='novel_artifact_adopt' AND idempotency_key LIKE ?",
                    params![format!("{prefix}:adopt:%")],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
            };
            let first_run = insert_adaptation_run_for_production(
                conn,
                &work,
                &job,
                &source.id,
                "adoption-retry-run-a",
                "ready_for_review",
            )?;
            let first = production_adopt_candidates(
                conn,
                "production-replay-project",
                &work.id,
                "artifact.adaptation_analysis_run_id",
                &first_run,
                &prefix,
            )?;
            assert_eq!(first.len(), PRODUCTION_ADAPTATION_ARTIFACT_TYPES.len());
            assert_eq!(
                receipt_count(conn)?,
                PRODUCTION_ADAPTATION_ARTIFACT_TYPES.len() as i64
            );
            let first_heads = conn
                .prepare(
                    "SELECT artifact_type,adopted_head_revision_id FROM analysis_artifacts
                     WHERE adaptation_analysis_run_id=? ORDER BY artifact_type",
                )
                .map_err(|error| error.to_string())?
                .query_map(params![first_run], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
                .map_err(|error| error.to_string())?;

            let second_run = insert_adaptation_run_for_production(
                conn,
                &work,
                &job,
                &source.id,
                "adoption-retry-run-b",
                "ready_for_review",
            )?;
            let second = production_adopt_candidates(
                conn,
                "production-replay-project",
                &work.id,
                "artifact.adaptation_analysis_run_id",
                &second_run,
                &prefix,
            )?;
            assert_eq!(second.len(), PRODUCTION_ADAPTATION_ARTIFACT_TYPES.len());
            assert!(PRODUCTION_ADAPTATION_ARTIFACT_TYPES
                .iter()
                .all(|kind| first.get(*kind) != second.get(*kind)));
            assert_eq!(
                receipt_count(conn)?,
                (PRODUCTION_ADAPTATION_ARTIFACT_TYPES.len() * 2) as i64
            );
            let first_heads_after = conn
                .prepare(
                    "SELECT artifact_type,adopted_head_revision_id FROM analysis_artifacts
                     WHERE adaptation_analysis_run_id=? ORDER BY artifact_type",
                )
                .map_err(|error| error.to_string())?
                .query_map(params![first_run], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
                .map_err(|error| error.to_string())?;
            assert_eq!(first_heads_after, first_heads);

            let replay = production_adopt_candidates(
                conn,
                "production-replay-project",
                &work.id,
                "artifact.adaptation_analysis_run_id",
                &second_run,
                &prefix,
            )?;
            assert_eq!(replay, second, "the same run/revision reuses adopted heads");
            assert_eq!(
                receipt_count(conn)?,
                (PRODUCTION_ADAPTATION_ARTIFACT_TYPES.len() * 2) as i64
            );

            let third_run = insert_adaptation_run_for_production(
                conn,
                &work,
                &job,
                &source.id,
                "adoption-retry-run-c",
                "ready_for_review",
            )?;
            let (artifact_id, candidate_id, candidate_version): (String, String, i64) = conn
                .query_row(
                    "SELECT artifact.id,candidate.id,candidate.version
                     FROM analysis_artifacts artifact
                     JOIN analysis_artifact_revisions candidate
                       ON candidate.id=artifact.candidate_head_revision_id
                     WHERE artifact.adaptation_analysis_run_id=?
                     ORDER BY artifact.artifact_type LIMIT 1",
                    params![third_run],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(|error| error.to_string())?;
            let manual_id = new_id("manual-adaptation");
            conn.execute(
                "INSERT INTO analysis_artifact_revisions
                 (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at)
                 SELECT ?,analysis_artifact_id,?,id,body_json,'','manual_edit','{}','{}','candidate',?
                 FROM analysis_artifact_revisions WHERE id=?",
                params![manual_id, candidate_version + 1, now(), candidate_id],
            )
            .map_err(|error| error.to_string())?;
            conn.execute(
                "UPDATE analysis_artifacts
                 SET candidate_head_revision_id=?,optimistic_version=optimistic_version+1
                 WHERE id=?",
                params![manual_id, artifact_id],
            )
            .map_err(|error| error.to_string())?;
            assert!(matches!(
                production_adopt_candidates(
                    conn,
                    "production-replay-project",
                    &work.id,
                    "artifact.adaptation_analysis_run_id",
                    &third_run,
                    &prefix,
                ),
                Err(error) if error.starts_with("PRODUCTION_CANDIDATE_DRIFT:")
            ));
            assert_eq!(
                receipt_count(conn)?,
                (PRODUCTION_ADAPTATION_ARTIFACT_TYPES.len() * 2) as i64
            );
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_wrapper_reuses_ready_adaptation_run_after_later_failure() {
        let (dir, state) = open_test_db("production-replay-adaptation");
        db::with_connection(&state,|conn| {
            let (work,_revision,job,source)=production_job_with_ready_source(conn,"replay-adaptation")?;
            let adaptation_run=insert_adaptation_run_for_production(conn,&work,&job,&source.id,"replay","ready_for_review")?;
            conn.execute("UPDATE novel_production_jobs SET adaptation_analysis_run_id=?,status='error',stage='ensuring_adaptation',stage_index=4 WHERE id=?",params![adaptation_run,job.id]).map_err(|e|e.to_string())?;
            let retried=production_retry_or_resume(conn,"novel_production_retry_stage","production-replay-project",&work.id,&job.id,"replay-adaptation-retry",Some("ensuring_adaptation"),false)?;
            assert_eq!(retried.attempt_no,job.attempt_no+1,"the explicit retry has a new attempt-derived key");
            let current=production_current_source_revisions(conn,&source.id)?;
            assert!(matches!(production_job_owned_adaptation_run(conn,&retried,&source.id,retried.default_adaptation_id.as_deref().unwrap(),&current)?,ProductionAdaptationRunEvidence::Exact { ref id, status: ref run_status } if id==&adaptation_run && run_status=="ready_for_review"),"the persisted exact ready pointer must win over the new attempt key");
            let runs:i64=conn.query_row("SELECT COUNT(*) FROM adaptation_analysis_runs WHERE comic_adaptation_id=?",params![job.default_adaptation_id],|r|r.get(0)).map_err(|e|e.to_string())?;
            assert_eq!(runs,1,"retry must reconcile the ready job-owned adaptation run rather than start a paid replacement");
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_wrapper_keeps_an_exact_pending_adaptation_pointer_pending() {
        let (dir, state) = open_test_db("production-pending-adaptation-pointer");
        db::with_connection(&state, |conn| {
            let (work, _revision, job, source) =
                production_job_with_ready_source(conn, "pending-adaptation-pointer")?;
            let run = insert_adaptation_run_for_production(
                conn,
                &work,
                &job,
                &source.id,
                "pending-adaptation-pointer",
                "running",
            )?;
            conn.execute(
                "UPDATE novel_production_jobs SET adaptation_analysis_run_id=? WHERE id=?",
                params![run, job.id],
            )
            .map_err(|e| e.to_string())?;
            let pointed = production_job_value(conn, "production-replay-project", &work.id, &job.id)?;
            let current = production_current_source_revisions(conn, &source.id)?;
            assert!(matches!(production_job_owned_adaptation_run(conn,&pointed,&source.id,pointed.default_adaptation_id.as_deref().unwrap(),&current)?,ProductionAdaptationRunEvidence::Exact { status, .. } if status=="running"));
            let artifacts: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM analysis_artifacts WHERE adaptation_analysis_run_id=?",
                    params![run],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(artifacts, 0, "pending runs have no outputs to validate or replace");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_retry_stage_replaces_an_exact_terminal_adaptation_error_but_resume_does_not() {
        let (dir, state) = open_test_db("production-terminal-adaptation-retry");
        db::with_connection(&state, |conn| {
            let (work, _revision, job, source) =
                production_job_with_ready_source(conn, "terminal-adaptation-retry")?;
            let run = insert_adaptation_run_for_production(
                conn,
                &work,
                &job,
                &source.id,
                "terminal-adaptation-error",
                "running",
            )?;
            conn.execute(
                "UPDATE adaptation_analysis_runs SET status='error',safe_error_code='ADAPTATION_ANALYSIS_FAILED' WHERE id=?",
                params![run],
            )
            .map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE novel_production_jobs SET adaptation_analysis_run_id=?,status='error',stage='adaptation_analysis',stage_index=5 WHERE id=?",
                params![run, job.id],
            )
            .map_err(|e| e.to_string())?;
            assert_eq!(
                production_retry_or_resume(
                    conn,
                    "novel_production_resume",
                    "production-replay-project",
                    &work.id,
                    &job.id,
                    "terminal-adaptation-resume",
                    None,
                    true,
                )
                .unwrap_err(),
                "PRODUCTION_ADAPTATION_UNRESOLVED"
            );
            let retried = production_retry_or_resume(
                conn,
                "novel_production_retry_stage",
                "production-replay-project",
                &work.id,
                &job.id,
                "terminal-adaptation-retry",
                Some("adaptation_analysis"),
                false,
            )?;
            assert_eq!(retried.attempt_no, job.attempt_no + 1);
            assert!(retried.adaptation_analysis_run_id.is_none());
            assert_eq!(retried.source_analysis_run_id.as_deref(), Some(source.id.as_str()));
            let event: (String, String) = conn
                .query_row(
                    "SELECT event_type,payload_json FROM novel_production_job_events WHERE novel_production_job_id=? AND event_type='adaptation_retry_authorized'",
                    params![job.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(event.0, "adaptation_retry_authorized");
            assert_eq!(json_value(event.1)["reason"].as_str(), Some("ADAPTATION_ANALYSIS_FAILED"));
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_wrapper_blocks_a_tampered_ready_adaptation_pointer_without_starting_a_replacement(
    ) {
        let (dir, state) = open_test_db("production-rebase-source-inputs");
        db::with_connection(&state,|conn| {
            let (work,_revision,job,source)=production_job_with_ready_source(conn,"rebase-source-inputs")?;
            let ready=insert_adaptation_run_for_production(conn,&work,&job,&source.id,"ready","ready_for_review")?;
            conn.execute("UPDATE novel_production_jobs SET adaptation_analysis_run_id=? WHERE id=?",params![ready,job.id]).map_err(|e|e.to_string())?;
            let job = production_job_value(conn, "production-replay-project", &work.id, &job.id)?;
            let kind=PRODUCTION_SOURCE_ARTIFACT_TYPES[0];
            let artifact:String=conn.query_row("SELECT id FROM analysis_artifacts WHERE source_analysis_run_id=? AND artifact_type=?",params![source.id,kind],|r|r.get(0)).map_err(|e|e.to_string())?;
            let manual=new_id("manual-source"); let ts=now();
            conn.execute("INSERT INTO analysis_artifact_revisions(id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,2,(SELECT adopted_head_revision_id FROM analysis_artifacts WHERE id=?),(SELECT body_json FROM analysis_artifact_revisions WHERE id=(SELECT adopted_head_revision_id FROM analysis_artifacts WHERE id=?)),'','manual_edit','{}','{}','adopted',?)",params![manual,artifact,artifact,artifact,ts]).map_err(|e|e.to_string())?;
            conn.execute("UPDATE analysis_artifacts SET adopted_head_revision_id=?,candidate_head_revision_id=?,optimistic_version=optimistic_version+1 WHERE id=?",params![manual,manual,artifact]).map_err(|e|e.to_string())?;
            let current=PRODUCTION_SOURCE_ARTIFACT_TYPES.iter().map(|source_kind|conn.query_row("SELECT adopted_head_revision_id FROM analysis_artifacts WHERE source_analysis_run_id=? AND artifact_type=?",params![source.id,source_kind],|r|r.get::<_,String>(0)).map_err(|e|e.to_string())).collect::<Result<Vec<_>,_>>()?;
            assert_eq!(current[0],manual);
            assert!(matches!(production_job_owned_adaptation_run(conn,&job,&source.id,job.default_adaptation_id.as_deref().unwrap(),&current)?,ProductionAdaptationRunEvidence::BaselineMismatch),"a pointer frozen against old adopted source must fail closed");
            let runs:i64=conn.query_row("SELECT COUNT(*) FROM adaptation_analysis_runs WHERE comic_adaptation_id=?",params![job.default_adaptation_id],|r|r.get(0)).map_err(|e|e.to_string())?;
            assert_eq!(runs,1,"a stale pointer must not silently start a paid replacement");
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_retry_stage_replaces_only_an_exact_ready_pointer_with_invalid_utf8_evidence() {
        let (dir, state) = open_test_db("production-invalid-evidence-retry");
        db::with_connection(&state, |conn| {
            let (work, revision, job, source) =
                production_job_with_ready_source(conn, "invalid-evidence-retry")?;
            let adaptation_run = insert_adaptation_run_for_production(
                conn,
                &work,
                &job,
                &source.id,
                "invalid-evidence-ready",
                "ready_for_review",
            )?;
            conn.execute(
                "UPDATE novel_production_jobs SET adaptation_analysis_run_id=?,status='error',stage='ensuring_adaptation',stage_index=4 WHERE id=?",
                params![adaptation_run, job.id],
            )
            .map_err(|e| e.to_string())?;
            let job = production_job_value(conn, "production-replay-project", &work.id, &job.id)?;
            let invalid_plan = json!({"chapters":[{"stableKey":"chapter-1","sourceSelections":[{"novelChapterRevisionId":revision.id,"startUtf8Byte":1,"endUtf8Byte":6}]}]});
            conn.execute(
                "UPDATE analysis_artifact_revisions SET body_json=? WHERE id=(SELECT candidate_head_revision_id FROM analysis_artifacts WHERE adaptation_analysis_run_id=? AND artifact_type='comic_chapter_plan')",
                params![invalid_plan.to_string(), adaptation_run],
            )
            .map_err(|e| e.to_string())?;
            let current = production_current_source_revisions(conn, &source.id)?;
            assert!(matches!(production_job_owned_adaptation_run(conn,&job,&source.id,job.default_adaptation_id.as_deref().unwrap(),&current)?,ProductionAdaptationRunEvidence::EvidenceInvalid { .. }));
            let paused = production_retry_or_resume(
                conn,
                "novel_production_resume",
                "production-replay-project",
                &work.id,
                &job.id,
                "invalid-evidence-resume",
                None,
                true,
            )?;
            assert_eq!(paused.status, "blocked_conflict");
            assert_eq!(paused.safe_error_code.as_deref(), Some("ADAPTATION_EVIDENCE_INVALID"));
            assert_eq!(paused.attempt_no, job.attempt_no, "the resume must not create a billable retry");
            let retried = production_retry_or_resume(
                conn,
                "novel_production_retry_stage",
                "production-replay-project",
                &work.id,
                &job.id,
                "invalid-evidence-retry",
                Some("ensuring_adaptation"),
                false,
            )?;
            assert_eq!(retried.attempt_no, job.attempt_no + 1);
            assert!(retried.adaptation_analysis_run_id.is_none());
            assert_eq!(retried.source_analysis_run_id.as_deref(), Some(source.id.as_str()));
            let replay = production_retry_or_resume(
                conn,
                "novel_production_retry_stage",
                "production-replay-project",
                &work.id,
                &job.id,
                "invalid-evidence-retry",
                Some("ensuring_adaptation"),
                false,
            )?;
            assert_eq!(replay.attempt_no, retried.attempt_no);
            assert!(replay.adaptation_analysis_run_id.is_none());
            let counts: (i64, i64, i64) = conn
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM source_analysis_runs),(SELECT COUNT(*) FROM adaptation_analysis_runs),(SELECT COUNT(*) FROM novel_production_job_events WHERE novel_production_job_id=? AND event_type='adaptation_retry_authorized')",
                    params![job.id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(counts, (1, 1, 1), "classification and replay must not create or dispatch a run");
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_missing_adaptation_evidence_is_not_reclassified_as_a_retryable_range_error() {
        let (dir, state) = open_test_db("production-missing-adaptation-evidence");
        db::with_connection(&state, |conn| {
            let (work, _revision, _job, _source) =
                production_job_with_ready_source(conn, "missing-adaptation-evidence")?;
            assert_eq!(
                production_adaptation_run_evidence_is_valid(conn, "missing-run", &work.id)
                    .unwrap_err(),
                "PRODUCTION_RECOVERY_READ_FAILED"
            );
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_manual_adoption_marks_succeeded_job_needs_rebase_but_running_auto_adoption_does_not(
    ) {
        let (dir, state) = open_test_db("production-manual-adopt-invalidates");
        db::with_connection(&state,|conn| {
            let (work,_revision,job,source)=production_job_with_ready_source(conn,"manual-invalidate")?;
            conn.execute("UPDATE novel_production_jobs SET status='succeeded',stage='succeeded',stage_index=8 WHERE id=?",params![job.id]).map_err(|e|e.to_string())?;
            let (artifact,version):(String,i64)=conn.query_row("SELECT id,optimistic_version FROM analysis_artifacts WHERE source_analysis_run_id=? ORDER BY id LIMIT 1",params![source.id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
            let candidate=new_id("manual-candidate"); let ts=now();
            conn.execute("INSERT INTO analysis_artifact_revisions(id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,2,(SELECT adopted_head_revision_id FROM analysis_artifacts WHERE id=?),(SELECT body_json FROM analysis_artifact_revisions WHERE id=(SELECT adopted_head_revision_id FROM analysis_artifacts WHERE id=?)),'','manual_edit','{}','{}','candidate',?)",params![candidate,artifact,artifact,artifact,ts]).map_err(|e|e.to_string())?;
            conn.execute("UPDATE analysis_artifacts SET candidate_head_revision_id=? WHERE id=?",params![candidate,artifact]).map_err(|e|e.to_string())?;
            let preview=novel_artifact_adopt_preview_inner(conn,NovelArtifactAdoptPreviewInput { project_id:"production-replay-project".into(),novel_work_id:work.id.clone(),artifact_id:artifact.clone(),revision_id:candidate.clone(),expected_optimistic_version:version })?;
            novel_artifact_adopt_inner(conn,NovelArtifactAdoptInput { project_id:"production-replay-project".into(),novel_work_id:work.id.clone(),artifact_id:artifact.clone(),revision_id:candidate,expected_optimistic_version:version,approval_token:preview.approval_token,idempotency_key:"manual-adopt".into() })?;
            let (status,next):(String,Option<String>)=conn.query_row("SELECT status,next_action FROM novel_production_jobs WHERE id=?",params![job.id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
            assert_eq!(status,"needs_rebase"); assert_eq!(next.as_deref(),Some("更新受影响的下游产物"));
            let events:i64=conn.query_row("SELECT COUNT(*) FROM novel_production_job_events WHERE novel_production_job_id=? AND event_type='manual_adoption_invalidated_production'",params![job.id],|r|r.get(0)).map_err(|e|e.to_string())?; assert_eq!(events,1);

            let (running_work,_running_revision,running_job,running_source)=production_job_with_ready_source(conn,"running-auto")?;
            conn.execute("UPDATE novel_production_jobs SET status='running',stage='integrating_context',stage_index=3 WHERE id=?",params![running_job.id]).map_err(|e|e.to_string())?;
            let running_artifact:String=conn.query_row("SELECT id FROM analysis_artifacts WHERE source_analysis_run_id=? ORDER BY id LIMIT 1",params![running_source.id],|r|r.get(0)).map_err(|e|e.to_string())?;
            let automatic=new_id("auto-candidate");
            conn.execute("INSERT INTO analysis_artifact_revisions(id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,2,NULL,(SELECT body_json FROM analysis_artifact_revisions WHERE id=(SELECT adopted_head_revision_id FROM analysis_artifacts WHERE id=?)),'','ai_analysis','{}','{}','candidate',?)",params![automatic,running_artifact,running_artifact,ts]).map_err(|e|e.to_string())?;
            conn.execute("UPDATE analysis_artifacts SET candidate_head_revision_id=? WHERE id=?",params![automatic,running_artifact]).map_err(|e|e.to_string())?;
            production_adopt_candidates(conn,"production-replay-project",&running_work.id,"artifact.source_analysis_run_id",&running_source.id,"running-auto")?;
            let running_status:String=conn.query_row("SELECT status FROM novel_production_jobs WHERE id=?",params![running_job.id],|r|r.get(0)).map_err(|e|e.to_string())?; assert_eq!(running_status,"running");
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_downstream_keys_replay_identical_inputs_and_change_for_new_revisions() {
        let (dir, state) = open_test_db("production-downstream-keys");
        db::with_connection(&state, |conn| {
            let (_work, _revision, job, _source) =
                production_job_with_ready_source(conn, "downstream-keys")?;
            let adaptation = job.default_adaptation_id.clone().unwrap();
            let selections = json!([{"scene":"s1","snapshot":"snap1"}]);
            let accept_one =
                production_accept_key(&job, &adaptation, "proposal-a", "chapter-a", "scene-a")?;
            assert_eq!(
                accept_one,
                production_accept_key(&job, &adaptation, "proposal-a", "chapter-a", "scene-a")?
            );
            assert_ne!(
                accept_one,
                production_accept_key(&job, &adaptation, "proposal-b", "chapter-a", "scene-a")?
            );
            let apply_one =
                production_apply_key(&job, &adaptation, "head-a", "page-a", &selections)?;
            assert_eq!(
                apply_one,
                production_apply_key(&job, &adaptation, "head-a", "page-a", &selections)?
            );
            assert_ne!(
                apply_one,
                production_apply_key(&job, &adaptation, "head-a", "page-b", &selections)?
            );
            assert_ne!(
                apply_one,
                production_apply_key(
                    &job,
                    &adaptation,
                    "head-a",
                    "page-a",
                    &json!([{"scene":"s1","snapshot":"snap2"}])
                )?
            );
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_wrapper_replays_succeeded_apply_receipt_without_second_apply() {
        let (dir, state) = open_test_db("production-replay-apply");
        db::with_connection(&state,|conn| {
            let (work,_revision,job,source)=production_job_with_ready_source(conn,"replay-apply")?;
            let adaptation_id=job.default_adaptation_id.clone().unwrap();
            let run=insert_adaptation_run_for_production(conn,&work,&job,&source.id,"apply","running")?;
            let chapter:String=conn.query_row("SELECT id FROM comic_adaptation_chapters WHERE comic_adaptation_id=? ORDER BY sequence_no LIMIT 1",params![adaptation_id],|r|r.get(0)).map_err(|e|e.to_string())?;
            let ts=now();
            let mut revisions=std::collections::BTreeMap::new();
            for kind in ["adaptation_proposal","comic_chapter_plan","scene_plan","page_panel_plan"] {
                let artifact=new_id("artifact"); let revision_id=new_id("revision"); let comic_chapter=if kind=="adaptation_proposal" { None } else { Some(chapter.as_str()) };
                conn.execute("INSERT INTO analysis_artifacts(id,adaptation_analysis_run_id,artifact_type,novel_work_id,comic_adaptation_id,comic_chapter_id,status,optimistic_version,created_at,updated_at) VALUES (?,?,?,?,?,?, 'active',0,?,?)",params![artifact,run,kind,work.id,adaptation_id,comic_chapter,ts,ts]).map_err(|e|e.to_string())?;
                conn.execute("INSERT INTO analysis_artifact_revisions(id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,1,NULL,'{}','','ai_adaptation_analysis','{}','{}','adopted',?)",params![revision_id,artifact,ts]).map_err(|e|e.to_string())?;
                conn.execute("UPDATE analysis_artifacts SET adopted_head_revision_id=?,candidate_head_revision_id=?,optimistic_version=1 WHERE id=?",params![revision_id,revision_id,artifact]).map_err(|e|e.to_string())?;
                revisions.insert(kind,revision_id);
            }
            let accept=new_id("accept");
            conn.execute("INSERT INTO analysis_apply_operations(id,operation_type,novel_work_id,comic_adaptation_id,base_target_version_id,idempotency_key,preview_fingerprint,approval_token_hash,status,safe_error_code,safe_user_message,created_at,updated_at,completed_at,approval_expires_at,expected_adaptation_version) VALUES (?,'accept_adaptation',NULL,?,NULL,?,'fp','hash','succeeded',NULL,NULL,?,?,?, ?,0)",params![accept,adaptation_id,"replay-accept",ts,ts,ts,ts+10_000]).map_err(|e|e.to_string())?;
            let head=new_id("head");
            conn.execute("INSERT INTO comic_adaptation_plan_heads(id,comic_adaptation_id,adaptation_proposal_revision_id,comic_chapter_plan_revision_id,scene_plan_revision_id,accept_apply_operation_id,status,created_at,updated_at) VALUES (?,?,?,?,?,?,'active',?,?)",params![head,adaptation_id,revisions["adaptation_proposal"],revisions["comic_chapter_plan"],revisions["scene_plan"],accept,ts,ts]).map_err(|e|e.to_string())?;
            let planning=new_id("planning");
            conn.execute("INSERT INTO comic_planning_chapters(id,comic_adaptation_plan_head_id,comic_adaptation_id,planning_chapter_stable_key,status,created_at,updated_at) VALUES (?,?,?,'chapter-1','planning',?,?)",params![planning,head,adaptation_id,ts,ts]).map_err(|e|e.to_string())?;
            let operation=new_id("apply"); let apply_key=production_apply_key(&job,&adaptation_id,&head,&revisions["page_panel_plan"],&json!([]))?;
            conn.execute("INSERT INTO analysis_apply_operations(id,operation_type,novel_work_id,comic_adaptation_id,base_target_version_id,idempotency_key,preview_fingerprint,approval_token_hash,status,safe_error_code,safe_user_message,created_at,updated_at,completed_at,approval_expires_at,expected_adaptation_version) VALUES (?,'apply_comic_plan',NULL,?,NULL,?,'fp','hash','succeeded',NULL,NULL,?,?,?, ?,0)",params![operation,adaptation_id,apply_key,ts,ts,ts,ts+10_000]).map_err(|e|e.to_string())?;
            conn.execute("INSERT INTO analysis_apply_operation_sources(analysis_apply_operation_id,analysis_artifact_revision_id,source_role,source_order) VALUES (?,?,'adaptation_input',0)",params![operation,revisions["page_panel_plan"]]).map_err(|e|e.to_string())?;
            conn.execute("INSERT INTO comic_production_chapters(id,comic_adaptation_id,comic_planning_chapter_id,page_panel_plan_revision_id,apply_operation_id,status,created_at) VALUES (?,?,?,?,?,'active',?)",params![new_id("production"),adaptation_id,planning,revisions["page_panel_plan"],operation,ts]).map_err(|e|e.to_string())?;
            conn.execute("INSERT INTO analysis_apply_receipts(analysis_apply_operation_id,idempotency_key,operation_type,result_novel_canon_version_id,result_novel_state_version_id,created_at) VALUES (?,?,'apply_comic_plan',NULL,NULL,?)",params![operation,apply_key,ts]).map_err(|e|e.to_string())?;
            conn.execute("UPDATE novel_production_jobs SET adaptation_analysis_run_id=?,status='error',stage='applying_production',stage_index=7 WHERE id=?",params![run,job.id]).map_err(|e|e.to_string())?;
            assert_eq!(production_replayed_apply_operation(conn,&apply_key,&adaptation_id)?.as_deref(),Some(operation.as_str()));
            let operations:i64=conn.query_row("SELECT COUNT(*) FROM analysis_apply_operations WHERE idempotency_key=?",params![apply_key],|r|r.get(0)).map_err(|e|e.to_string())?;
            assert_eq!(operations,1,"resume must use the existing successful apply receipt and production mapping");
            let changed_input_key=production_apply_key(&job,&adaptation_id,&head,"new-page-plan",&json!([]))?;
            assert_ne!(changed_input_key,apply_key,"a changed page plan must create a new downstream operation key");
            let receipts:i64=conn.query_row("SELECT COUNT(*) FROM analysis_apply_receipts WHERE idempotency_key=?",params![apply_key],|r|r.get(0)).map_err(|e|e.to_string())?;
            assert_eq!(receipts,1,"the old successful receipt remains immutable for audit/replay");
            Ok(())
        }).unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_runtime_scope_trigger_rejects_cross_work_source_run_update() {
        let (dir, state) = open_test_db("production-runtime-scope");
        db::with_connection(&state, |conn| {
            let (_work, _revision, job, _run) =
                production_job_with_ready_source(conn, "scope-own")?;
            let (_other, foreign) = completed_production_source(conn, "scope-foreign")?;
            let error = conn
                .execute(
                    "UPDATE novel_production_jobs SET source_analysis_run_id=? WHERE id=?",
                    params![foreign.id, job.id],
                )
                .unwrap_err()
                .to_string();
            assert!(error.contains("production job source run scope is invalid"));
            Ok(())
        })
        .unwrap();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v10_archive_restore_and_volume_enforce_owner_status_and_running_gate() {
        let (dir, state) = open_test_db("archive-volume");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "archive-project", "archive-work");
            let volume_input = NovelVolumeCreateInput {
                project_id: "archive-project".into(),
                novel_work_id: work.id.clone(),
                volume_no: 1,
                title: Some("第一卷".into()),
                idempotency_key: "volume-one".into(),
            };
            let volume = novel_volume_create_inner(conn, volume_input.clone())?;
            assert_eq!(volume.id, novel_volume_create_inner(conn, volume_input)?.id);
            assert!(novel_volume_create_inner(
                conn,
                NovelVolumeCreateInput {
                    project_id: "archive-project".into(),
                    novel_work_id: work.id.clone(),
                    volume_no: 1,
                    title: None,
                    idempotency_key: "volume-duplicate".into()
                }
            )
            .is_err());
            let chapter = novel_chapter_revision_create_inner(
                conn,
                NovelChapterRevisionCreateInput {
                    project_id: "archive-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_id: None,
                    volume_id: Some(volume.id),
                    sequence_no: Some(1),
                    chapter_no: Some(1),
                    title: None,
                    content: "归档前正文".into(),
                    parent_context_revision_id: None,
                    asset_id: None,
                    source_kind: None,
                    idempotency_key: "archive-chapter".into(),
                },
            )?;
            let archive = NovelWorkStatusInput {
                project_id: "archive-project".into(),
                novel_work_id: work.id.clone(),
                expected_status: "active".into(),
                idempotency_key: "archive".into(),
            };
            assert_eq!(
                novel_work_set_status_inner(conn, archive.clone(), "archived")?.status,
                "archived"
            );
            assert_eq!(
                novel_work_set_status_inner(conn, archive, "archived")?.status,
                "archived"
            );
            assert_eq!(
                novel_work_list_inner(conn, "archive-project", false)?.len(),
                0
            );
            assert_eq!(
                novel_work_list_inner(conn, "archive-project", true)?.len(),
                1
            );
            assert_eq!(
                novel_snapshot_inner(conn, "archive-project", &work.id)?
                    .revisions
                    .len(),
                1
            );
            assert!(novel_chapter_revision_create_inner(
                conn,
                NovelChapterRevisionCreateInput {
                    project_id: "archive-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_id: None,
                    volume_id: None,
                    sequence_no: Some(2),
                    chapter_no: Some(2),
                    title: None,
                    content: "应拒绝".into(),
                    parent_context_revision_id: None,
                    asset_id: None,
                    source_kind: None,
                    idempotency_key: "archived-chapter".into()
                }
            )
            .is_err());
            assert!(analysis_start_inner(
                conn,
                NovelAnalysisStartInput {
                    project_id: "archive-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_revision_id: chapter.id,
                    parent_context_revision_id: None,
                    idempotency_key: "archived-analysis".into(),
                    provider_id: None,
                    model_id: None
                },
                false,
                "test".into(),
                "test".into(),
                "owner"
            )
            .is_err());
            assert!(novel_volume_create_inner(
                conn,
                NovelVolumeCreateInput {
                    project_id: "archive-project".into(),
                    novel_work_id: work.id.clone(),
                    volume_no: 2,
                    title: None,
                    idempotency_key: "archived-volume".into()
                }
            )
            .is_err());
            assert!(novel_work_set_status_inner(
                conn,
                NovelWorkStatusInput {
                    project_id: "other-project".into(),
                    novel_work_id: work.id.clone(),
                    expected_status: "archived".into(),
                    idempotency_key: "cross-owner".into()
                },
                "active"
            )
            .is_err());
            assert!(novel_work_set_status_inner(
                conn,
                NovelWorkStatusInput {
                    project_id: "archive-project".into(),
                    novel_work_id: work.id.clone(),
                    expected_status: "active".into(),
                    idempotency_key: "wrong-cas".into()
                },
                "archived"
            )
            .is_err());
            assert_eq!(
                novel_work_set_status_inner(
                    conn,
                    NovelWorkStatusInput {
                        project_id: "archive-project".into(),
                        novel_work_id: work.id.clone(),
                        expected_status: "archived".into(),
                        idempotency_key: "restore".into()
                    },
                    "active"
                )?
                .status,
                "active"
            );
            assert_eq!(
                novel_volume_create_inner(
                    conn,
                    NovelVolumeCreateInput {
                        project_id: "archive-project".into(),
                        novel_work_id: work.id.clone(),
                        volume_no: 2,
                        title: None,
                        idempotency_key: "restored-volume".into()
                    }
                )?
                .volume_no,
                2
            );
            let running = create_work(conn, "archive-project", "running-work");
            let running_chapter = novel_chapter_revision_create_inner(
                conn,
                NovelChapterRevisionCreateInput {
                    project_id: "archive-project".into(),
                    novel_work_id: running.id.clone(),
                    chapter_id: None,
                    volume_id: None,
                    sequence_no: Some(1),
                    chapter_no: Some(1),
                    title: None,
                    content: "运行中".into(),
                    parent_context_revision_id: None,
                    asset_id: None,
                    source_kind: None,
                    idempotency_key: "running-chapter".into(),
                },
            )?;
            let _run = analysis_start_inner(
                conn,
                NovelAnalysisStartInput {
                    project_id: "archive-project".into(),
                    novel_work_id: running.id.clone(),
                    chapter_revision_id: running_chapter.id,
                    parent_context_revision_id: None,
                    idempotency_key: "running-analysis".into(),
                    provider_id: None,
                    model_id: None,
                },
                true,
                "test".into(),
                "test".into(),
                "owner",
            )?;
            assert!(novel_work_set_status_inner(
                conn,
                NovelWorkStatusInput {
                    project_id: "archive-project".into(),
                    novel_work_id: running.id,
                    expected_status: "active".into(),
                    idempotency_key: "running-archive".into()
                },
                "archived"
            )
            .is_err());
            Ok(())
        })
        .unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn analysis_list_is_project_scoped_filtered_and_stably_ordered() {
        let (dir, state) = open_test_db("analysis-list");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "analysis-list-project", "analysis-list-work");
            let revision = novel_chapter_revision_create_inner(
                conn,
                NovelChapterRevisionCreateInput {
                    project_id: "analysis-list-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_id: None,
                    volume_id: None,
                    sequence_no: Some(1),
                    chapter_no: Some(1),
                    title: None,
                    content: "正文".into(),
                    parent_context_revision_id: None,
                    asset_id: None,
                    source_kind: None,
                    idempotency_key: "analysis-list-revision".into(),
                },
            )?;
            let run = analysis_start_inner(
                conn,
                NovelAnalysisStartInput {
                    project_id: "analysis-list-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_revision_id: revision.id.clone(),
                    parent_context_revision_id: None,
                    idempotency_key: "analysis-list-run".into(),
                    provider_id: None,
                    model_id: None,
                },
                false,
                "default".into(),
                "default".into(),
                "test",
            )?;
            let listed = novel_analysis_list_inner(
                conn,
                NovelAnalysisListInput {
                    project_id: "analysis-list-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_revision_id: Some(revision.id),
                    status: Some("error".into()),
                },
            )?;
            assert_eq!(
                listed
                    .iter()
                    .map(|item| item.id.as_str())
                    .collect::<Vec<_>>(),
                vec![run.id.as_str()]
            );
            assert!(novel_analysis_list_inner(
                conn,
                NovelAnalysisListInput {
                    project_id: "other-project".into(),
                    novel_work_id: work.id,
                    chapter_revision_id: None,
                    status: None
                }
            )
            .is_err());
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v7_work_create_is_idempotent_and_initializes_genesis_isolated_by_project() {
        let (dir, state) = open_test_db("work");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "project-a", "work-create-a");
            let replay = create_work(conn, "project-a", "work-create-a");
            assert_eq!(work.id, replay.id);
            assert_eq!(novel_work_list_inner(conn, "project-a", false)?.len(), 1);
            assert!(novel_work_list_inner(conn, "project-b", false)?.is_empty());
            let heads: (i64, i64, i64) = conn.query_row(
                "SELECT (SELECT COUNT(*) FROM novel_canon_versions WHERE novel_work_id = ? AND version = 0),
                        (SELECT COUNT(*) FROM novel_state_versions WHERE novel_work_id = ? AND version = 0),
                        (SELECT COUNT(*) FROM novel_analysis_lineages WHERE novel_work_id = ? AND continuous_through_sequence_no = 0)",
                params![work.id, work.id, work.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).map_err(|error| format!("验证 genesis 失败: {error}"))?;
            assert_eq!(heads, (1, 1, 1));
            assert!(get_work(conn, "project-b", &work.id).is_err());
            Ok(())
        }).unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn novel_work_list_inner(
        conn: &Connection,
        project_id: &str,
        include_archived: bool,
    ) -> Result<Vec<NovelWork>, String> {
        let sql = if include_archived {
            "SELECT id, project_id, title, description, status, published_canon_version_id, current_novel_state_version_id, current_analysis_lineage_id, created_at, updated_at FROM novel_works WHERE project_id = ? ORDER BY created_at, id"
        } else {
            "SELECT id, project_id, title, description, status, published_canon_version_id, current_novel_state_version_id, current_analysis_lineage_id, created_at, updated_at FROM novel_works WHERE project_id = ? AND status = 'active' ORDER BY created_at, id"
        };
        let mut statement = conn.prepare(sql).map_err(|error| error.to_string())?;
        let works = statement
            .query_map(params![project_id], work_from_row)
            .map_err(|error| error.to_string())?
            .map(|row| row.map_err(|error| error.to_string()))
            .collect();
        works
    }

    #[test]
    fn chapter_revision_save_updates_title_and_none_preserves_it() {
        let (dir, state) = open_test_db("chapter-rename");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "rename-project", "rename-work");
            let mut input = NovelChapterRevisionCreateInput {
                project_id: "rename-project".into(),
                novel_work_id: work.id.clone(),
                chapter_id: None,
                volume_id: None,
                sequence_no: Some(1),
                chapter_no: Some(1),
                title: Some("原章节名".into()),
                content: "原正文".into(),
                parent_context_revision_id: None,
                asset_id: None,
                source_kind: None,
                idempotency_key: "rename-source-1".into(),
            };
            let first = novel_chapter_revision_create_inner(conn, input.clone())?;
            input.chapter_id = Some(first.chapter_id.clone());
            input.title = Some("  新章节名  ".into());
            input.content = "新正文".into();
            input.idempotency_key = "rename-source-2".into();
            let second = novel_chapter_revision_create_inner(conn, input.clone())?;
            let chapter = chapter_in_work(conn, &first.chapter_id, &work.id)?;
            assert_eq!(chapter.title.as_deref(), Some("新章节名"));
            assert_eq!(
                chapter.latest_revision_id.as_deref(),
                Some(second.id.as_str())
            );
            let replay = novel_chapter_revision_create_inner(conn, input.clone())?;
            assert_eq!(replay.id, second.id);
            input.title = None;
            input.idempotency_key = "rename-source-3".into();
            novel_chapter_revision_create_inner(conn, input)?;
            assert_eq!(
                chapter_in_work(conn, &first.chapter_id, &work.id)?
                    .title
                    .as_deref(),
                Some("新章节名")
            );
            Ok(())
        })
        .unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chapter_revision_invalid_rename_does_not_save_content_or_change_head() {
        let (dir, state) = open_test_db("chapter-rename-invalid");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "rename-project", "rename-invalid-work");
            let mut input = NovelChapterRevisionCreateInput {
                project_id: "rename-project".into(),
                novel_work_id: work.id.clone(),
                chapter_id: None,
                volume_id: None,
                sequence_no: Some(1),
                chapter_no: Some(1),
                title: Some("原章节名".into()),
                content: "原正文".into(),
                parent_context_revision_id: None,
                asset_id: None,
                source_kind: None,
                idempotency_key: "rename-invalid-source".into(),
            };
            let first = novel_chapter_revision_create_inner(conn, input.clone())?;
            input.chapter_id = Some(first.chapter_id.clone());
            input.content = "不应写入的正文".into();
            for (index, title) in ["   ".to_owned(), "x".repeat(MAX_TITLE_BYTES + 1)]
                .into_iter()
                .enumerate()
            {
                input.title = Some(title);
                input.idempotency_key = format!("rename-invalid-{index}");
                assert!(novel_chapter_revision_create_inner(conn, input.clone()).is_err());
                let chapter = chapter_in_work(conn, &first.chapter_id, &work.id)?;
                assert_eq!(chapter.title.as_deref(), Some("原章节名"));
                assert_eq!(
                    chapter.latest_revision_id.as_deref(),
                    Some(first.id.as_str())
                );
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM novel_chapter_revisions WHERE novel_chapter_id=?",
                        params![first.chapter_id],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                assert_eq!(count, 1);
            }
            Ok(())
        })
        .unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v7_chapter_revision_is_idempotent_normalized_and_owner_scoped() {
        let (dir, state) = open_test_db("revision");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "project-a", "work-create-revision");
            let input = NovelChapterRevisionCreateInput {
                project_id: "project-a".into(),
                novel_work_id: work.id.clone(),
                chapter_id: None,
                volume_id: None,
                sequence_no: Some(1),
                chapter_no: Some(1),
                title: Some("第一章".into()),
                content: "第一行\r\n第二行".into(),
                parent_context_revision_id: None,
                asset_id: None,
                source_kind: None,
                idempotency_key: "revision-create-1".into(),
            };
            let revision = novel_chapter_revision_create_inner(conn, input.clone())?;
            let replay = novel_chapter_revision_create_inner(conn, input)?;
            assert_eq!(revision.id, replay.id);
            assert_eq!(revision.content, "第一行\n第二行");
            let snapshot = novel_snapshot_inner(conn, "project-a", &work.id)?;
            assert_eq!(snapshot.chapters.len(), 1);
            assert_eq!(
                snapshot.chapters[0].latest_revision_id.as_deref(),
                Some(revision.id.as_str())
            );
            assert_eq!(snapshot.current_novel_state_through_sequence_no, 0);
            let second = novel_chapter_revision_create_inner(
                conn,
                NovelChapterRevisionCreateInput {
                    project_id: "project-a".into(),
                    novel_work_id: work.id.clone(),
                    chapter_id: Some(snapshot.chapters[0].id.clone()),
                    volume_id: None,
                    sequence_no: None,
                    chapter_no: None,
                    title: None,
                    content: "修订".into(),
                    parent_context_revision_id: None,
                    asset_id: None,
                    source_kind: None,
                    idempotency_key: "revision-create-2".into(),
                },
            )?;
            assert_eq!(second.revision_no, 2);
            assert!(novel_chapter_revision_create_inner(
                conn,
                NovelChapterRevisionCreateInput {
                    project_id: "project-b".into(),
                    novel_work_id: work.id,
                    chapter_id: None,
                    volume_id: None,
                    sequence_no: Some(1),
                    chapter_no: Some(1),
                    title: None,
                    content: "越权".into(),
                    parent_context_revision_id: None,
                    asset_id: None,
                    source_kind: None,
                    idempotency_key: "revision-wrong-owner".into(),
                }
            )
            .is_err());
            Ok(())
        })
        .unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn novel_snapshot_inner(
        conn: &Connection,
        project_id: &str,
        work_id: &str,
    ) -> Result<NovelSnapshot, String> {
        let work = get_work(conn, project_id, work_id)?;
        snapshot_of(conn, work)
    }

    fn valid_analysis_output(character_content: Value) -> Value {
        let artifacts = REQUIRED_ARTIFACT_TYPES.iter().map(|kind| json!({
            "schemaVersion":"novel-analysis.v1", "artifactType":kind,
            "owner":{"ownerType":expected_owner_type(kind),"ownerId":"owner"},
            "content": if *kind == "character_facts" { character_content.clone() } else { valid_content(kind) },
            "warnings":[]
        })).collect::<Vec<_>>();
        json!({"artifacts":artifacts})
    }
    fn valid_content(kind: &str) -> Value {
        match kind {
            "chapter_summary" => json!({"summary":"s","keyEvents":[{"x":1}]}),
            "chapter_beats" => json!({"beats":[{"x":1}]}),
            "world_facts" | "faction_facts" | "location_facts" | "prop_facts" => {
                json!({"items":[{"stableKey":"x","resolutionKind":"conflict"}]})
            }
            "timeline_delta" => json!({"events":[{"stableKey":"x"}]}),
            "continuity_delta" => json!({"changes":[{"stableKey":"x"}]}),
            "open_threads" => json!({"threads":[{"stableKey":"x"}]}),
            "adaptation_proposal" => json!({"decisions":[{"stableKey":"x"}]}),
            "comic_chapter_plan" => json!({"chapters":[{"stableKey":"x"}]}),
            "scene_plan" => json!({"comicChapterDraftId":"draft","scenes":[{"stableKey":"x"}]}),
            "page_panel_plan" => json!({"comicChapterDraftId":"draft","pages":[{"stableKey":"x"}]}),
            _ => json!({}),
        }
    }

    fn adopted_revision_for(
        conn: &Connection,
        run_id: &str,
        artifact_type: &str,
    ) -> Result<String, String> {
        let (artifact_id, revision_id): (String, String) = conn
            .query_row(
                "SELECT id,candidate_head_revision_id FROM analysis_artifacts WHERE source_analysis_run_id=? AND artifact_type=?",
                params![run_id, artifact_type],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        conn.execute(
            "UPDATE analysis_artifact_revisions SET status='adopted' WHERE id=?",
            params![revision_id],
        )
        .map_err(|error| error.to_string())?;
        conn.execute(
            "UPDATE analysis_artifacts SET adopted_head_revision_id=? WHERE id=?",
            params![revision_id, artifact_id],
        )
        .map_err(|error| error.to_string())?;
        Ok(revision_id)
    }

    #[test]
    fn v9_dual_publish_is_scoped_atomic_and_idempotent() {
        let (dir, state) = open_test_db("dual-publish");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "publish-project", "publish-work");
            let chapter = novel_chapter_revision_create_inner(
                conn,
                NovelChapterRevisionCreateInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_id: None,
                    volume_id: None,
                    sequence_no: Some(1),
                    chapter_no: Some(1),
                    title: None,
                    content: "正文".into(),
                    parent_context_revision_id: None,
                    asset_id: None,
                    source_kind: None,
                    idempotency_key: "publish-chapter".into(),
                },
            )?;
            let run = analysis_start_inner(
                conn,
                NovelAnalysisStartInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_revision_id: chapter.id,
                    parent_context_revision_id: None,
                    idempotency_key: "publish-analysis".into(),
                    provider_id: None,
                    model_id: None,
                },
                true,
                "test".into(),
                "test".into(),
                "owner",
            )?;
            complete_analysis_inner(
                conn,
                &run,
                validate_analysis_output(&valid_analysis_output(
                    json!({"items":[{"stableKey":"hero","resolutionKind":"conflict"}]}),
                ))?,
                "owner",
            )?;
            let world = adopted_revision_for(conn, &run.id, "world_facts")?;
            let timeline = adopted_revision_for(conn, &run.id, "timeline_delta")?;
            let continuity = adopted_revision_for(conn, &run.id, "continuity_delta")?;
            let open_threads = adopted_revision_for(conn, &run.id, "open_threads")?;
            let character = adopted_revision_for(conn, &run.id, "character_facts")?;
            conn.execute(
                "UPDATE analysis_artifacts SET novel_chapter_revision_id=NULL WHERE candidate_head_revision_id=?",
                params![world],
            )
            .map_err(|e| e.to_string())?;
            assert_eq!(
                publish_sources(conn, &work.id, std::slice::from_ref(&world), PublishTrack::Canon, true)?[0]
                    .sequence_no,
                Some(1)
            );
            let world_artifact = list_artifacts_inner(conn, &NovelArtifactListInput {
                project_id: "publish-project".into(), novel_work_id: work.id.clone(),
                artifact_types: Some(vec!["world_facts".into()]),
                comic_adaptation_id: None,
                comic_chapter_id: None,
                adaptation_analysis_run_id: None,
                revision_status: None,
            })?.pop().unwrap();
            assert_eq!(world_artifact.source_chapter_no, Some(1));
            assert_eq!(world_artifact.source_sequence_no, Some(1));
            assert_eq!(world_artifact.classification, "canon_delta");
            assert_eq!(world_artifact.inherited_status.as_deref(), Some("ready"));
            assert!(world_artifact.source_chapter_id.is_some());
            let original_canon = work.published_canon_version_id.clone();
            conn.execute(
                "INSERT INTO novel_canon_versions (id,novel_work_id,version,parent_version_id,body_json,rendered_markdown,status,created_at) VALUES ('unrelated-canon-version',?,99,?,'{}','','superseded',0)",
                params![work.id, original_canon],
            ).map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE novel_canon_versions SET body_json=? WHERE id=?",
                params![r#"{"legacy":{"kept":true},"world_facts":{"items":[{"stableKey":"old-fact"}]}}"#, original_canon],
            )
            .map_err(|e| e.to_string())?;
            let preview = publish_preview_inner(
                conn,
                NovelPublishPreviewInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    expected_head_version_id: original_canon.clone(),
                    source_revision_ids: vec![world.clone()],
                    idempotency_key: "canon-preview".into(),
                },
                PublishTrack::Canon,
            )?;
            let stored: String = conn
                .query_row(
                    "SELECT approval_token_hash FROM analysis_apply_operations WHERE id=?",
                    params![preview.operation_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert!(!stored.contains(&preview.approval_token));
            assert_eq!(
                preview.content["world_facts"]["items"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert!(publish_inner(
                conn,
                NovelPublishInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    operation_id: preview.operation_id.clone(),
                    approval_token: "wrong".into(),
                    idempotency_key: "canon-preview".into()
                },
                PublishTrack::Canon
            )
            .is_err());
            let published = publish_inner(
                conn,
                NovelPublishInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    operation_id: preview.operation_id.clone(),
                    approval_token: preview.approval_token.clone(),
                    idempotency_key: "canon-preview".into(),
                },
                PublishTrack::Canon,
            )?;
            let replay = publish_inner(
                conn,
                NovelPublishInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    operation_id: preview.operation_id.clone(),
                    approval_token: "used-token".into(),
                    idempotency_key: "canon-preview".into(),
                },
                PublishTrack::Canon,
            )?;
            assert_eq!(published.version_id, replay.version_id);
            let canon: String = conn
                .query_row(
                    "SELECT body_json FROM novel_canon_versions WHERE id=?",
                    params![published.version_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            let canon = json_value(canon);
            assert_eq!(published.version, 1, "version follows its head, not MAX(version)");
            assert!(canon.get("legacy").is_some());
            assert_eq!(canon["world_facts"]["items"].as_array().unwrap().len(), 2);
            let after_canon =
                get_work(conn, "publish-project", &work.id)?.published_canon_version_id;
            assert!(publish_preview_inner(
                conn,
                NovelPublishPreviewInput {
                    project_id: "publish-project".into(), novel_work_id: work.id.clone(),
                    expected_head_version_id: work.current_novel_state_version_id.clone(),
                    source_revision_ids: vec![timeline.clone()], idempotency_key: "state-missing".into(),
                },
                PublishTrack::State,
            ).is_err());
            let state_preview = publish_preview_inner(
                conn,
                NovelPublishPreviewInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    expected_head_version_id: work.current_novel_state_version_id.clone(),
                    source_revision_ids: vec![timeline.clone(), continuity, open_threads],
                    idempotency_key: "state-preview".into(),
                },
                PublishTrack::State,
            )?;
            let state = publish_inner(
                conn,
                NovelPublishInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    operation_id: state_preview.operation_id,
                    approval_token: state_preview.approval_token,
                    idempotency_key: "state-preview".into(),
                },
                PublishTrack::State,
            )?;
            assert_eq!(
                get_work(conn, "publish-project", &work.id)?.published_canon_version_id,
                after_canon
            );
            assert_eq!(state.version, 1);
            assert_eq!(
                novel_snapshot_inner(conn, "publish-project", &work.id)?
                    .current_novel_state_through_sequence_no,
                1
            );
            let stale_preview = publish_preview_inner(
                conn,
                NovelPublishPreviewInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    expected_head_version_id: after_canon.clone(),
                    source_revision_ids: vec![character.clone()],
                    idempotency_key: "stale-preview".into(),
                },
                PublishTrack::Canon,
            )?;
            conn.execute(
                "UPDATE novel_works SET published_canon_version_id=? WHERE id=?",
                params![original_canon, work.id],
            )
            .map_err(|e| e.to_string())?;
            assert_eq!(
                publish_inner(
                    conn,
                    NovelPublishInput {
                        project_id: "publish-project".into(),
                        novel_work_id: work.id.clone(),
                        operation_id: stale_preview.operation_id.clone(),
                        approval_token: stale_preview.approval_token,
                        idempotency_key: "stale-preview".into(),
                    },
                    PublishTrack::Canon,
                )
                .unwrap_err(),
                "BASELINE_STALE"
            );
            let status: String = conn
                .query_row(
                    "SELECT status FROM analysis_apply_operations WHERE id=?",
                    params![stale_preview.operation_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(status, "previewed");
            conn.execute(
                "UPDATE novel_works SET published_canon_version_id=? WHERE id=?",
                params![after_canon, work.id],
            )
            .map_err(|e| e.to_string())?;
            let expired_preview = publish_preview_inner(
                conn,
                NovelPublishPreviewInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    expected_head_version_id: get_work(conn, "publish-project", &work.id)?
                        .published_canon_version_id,
                    source_revision_ids: vec![character],
                    idempotency_key: "expired-preview".into(),
                },
                PublishTrack::Canon,
            )?;
            conn.execute(
                "UPDATE analysis_apply_operations SET approval_expires_at=0 WHERE id=?",
                params![expired_preview.operation_id],
            )
            .map_err(|e| e.to_string())?;
            assert!(publish_inner(
                conn,
                NovelPublishInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    operation_id: expired_preview.operation_id.clone(),
                    approval_token: expired_preview.approval_token,
                    idempotency_key: "expired-preview".into()
                },
                PublishTrack::Canon
            )
            .is_err());
            assert_eq!(
                conn.query_row::<String, _, _>(
                    "SELECT status FROM analysis_apply_operations WHERE id=?",
                    params![expired_preview.operation_id],
                    |r| r.get(0)
                )
                .map_err(|e| e.to_string())?,
                "previewed"
            );
            assert!(publish_preview_inner(
                conn,
                NovelPublishPreviewInput {
                    project_id: "publish-project".into(),
                    novel_work_id: work.id.clone(),
                    expected_head_version_id: after_canon,
                    source_revision_ids: vec![timeline],
                    idempotency_key: "wrong-track".into()
                },
                PublishTrack::Canon
            )
            .is_err());
            let other = create_work(conn, "other-project", "other-work");
            assert!(publish_preview_inner(
                conn,
                NovelPublishPreviewInput {
                    project_id: "other-project".into(),
                    novel_work_id: other.id,
                    expected_head_version_id: other.published_canon_version_id,
                    source_revision_ids: vec![world],
                    idempotency_key: "cross-work".into()
                },
                PublishTrack::Canon
            )
            .is_err());
            Ok(())
        })
        .unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v8_analysis_contract_requires_page_panel_and_validates_fact_keys() {
        assert!(REQUIRED_ARTIFACT_TYPES.contains(&"page_panel_plan"));
        assert!(!REQUIRED_ARTIFACT_TYPES.contains(&"canon_diff"));
        assert!(validate_analysis_output(&valid_analysis_output(
            json!({"items":[{"stableKey":"hero","identity":"position is a normal word in prose"}]})
        ))
        .is_ok());
        assert!(validate_analysis_output(&valid_analysis_output(
            json!({"items":[{"stableKey":"hero","position":"north"}]})
        ))
        .is_err());
        let mut invalid = valid_analysis_output(json!({"items":[]}));
        invalid["artifacts"][0]["artifactType"] = json!("canon_diff");
        assert!(validate_analysis_output(&invalid).is_err());
        assert!(validate_analysis_output(&json!({"schemaVersion":"novel-analysis.v1"})).is_err());
        assert_eq!(
            completion_endpoint("https://llm.example/v1"),
            "https://llm.example/v1/chat/completions"
        );
        assert_eq!(
            completion_endpoint("https://llm.example/v1/chat/completions/"),
            "https://llm.example/v1/chat/completions"
        );
    }

    #[test]
    fn v8_completion_atomically_links_artifacts_entities_and_context() {
        let (dir, state) = open_test_db("analysis-complete");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "project-a", "complete-work");
            let revision = novel_chapter_revision_create_inner(conn, NovelChapterRevisionCreateInput { project_id:"project-a".into(),novel_work_id:work.id.clone(),chapter_id:None,volume_id:None,sequence_no:Some(1),chapter_no:Some(1),title:None,content:"正文".into(),parent_context_revision_id:None,asset_id:None,source_kind:None,idempotency_key:"complete-revision".into() })?;
            let run=analysis_start_inner(conn,NovelAnalysisStartInput {project_id:"project-a".into(),novel_work_id:work.id.clone(),chapter_revision_id:revision.id,parent_context_revision_id:None,idempotency_key:"complete-start".into(),provider_id:None,model_id:None},true,"provider".into(),"model".into(),"owner")?;
            let artifacts=validate_analysis_output(&valid_analysis_output(json!({"items":[{"stableKey":"hero","identity":"Li","goals":[],"secrets":[],"relations":[],"resolutionKind":"new_entity_candidate","candidateNovelEntityId":"new-hero","evidenceRefs":[],"conflictState":"none"}]})))?;
            let finished=complete_analysis_inner(conn,&run,artifacts,"owner")?;
            assert_eq!(finished.status,"ready_for_review");
            let counts:(i64,i64,i64)=conn.query_row("SELECT (SELECT COUNT(*) FROM analysis_artifacts WHERE source_analysis_run_id=?),(SELECT COUNT(*) FROM novel_chapter_context_artifact_revisions),(SELECT COUNT(*) FROM novel_entities WHERE novel_work_id=?)",params![run.id,work.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|e|e.to_string())?;
            assert_eq!(counts,(14,14,1));
            Ok(())
        }).unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v8_consecutive_context_merges_collections_and_gap_does_not_advance_main() {
        let (dir, state) = open_test_db("context-merge");
        db::with_connection(&state,|conn|{
            let work=create_work(conn,"p","merge-work");
            let make_revision=|seq:i64,key:&str| novel_chapter_revision_create_inner(conn,NovelChapterRevisionCreateInput{project_id:"p".into(),novel_work_id:work.id.clone(),chapter_id:None,volume_id:None,sequence_no:Some(seq),chapter_no:Some(seq),title:None,content:format!("chapter {seq}"),parent_context_revision_id:None,asset_id:None,source_kind:None,idempotency_key:key.into()});
            let rev1=make_revision(1,"rev-1")?;
            let run1=analysis_start_inner(conn,NovelAnalysisStartInput{project_id:"p".into(),novel_work_id:work.id.clone(),chapter_revision_id:rev1.id.clone(),parent_context_revision_id:None,idempotency_key:"run-1".into(),provider_id:None,model_id:None},true,"test".into(),"test".into(),"owner")?;
            let mut first=valid_analysis_output(json!({"items":[{"stableKey":"hero","resolutionKind":"conflict"}]}));
            first["artifacts"].as_array_mut().unwrap().iter_mut().find(|v|v["artifactType"]=="world_facts").unwrap()["content"]=json!({"items":[{"stableKey":"world-a","resolutionKind":"conflict"}]});
            first["artifacts"].as_array_mut().unwrap().iter_mut().find(|v|v["artifactType"]=="timeline_delta").unwrap()["content"]=json!({"events":[{"stableKey":"event-a"}]});
            first["artifacts"].as_array_mut().unwrap().iter_mut().find(|v|v["artifactType"]=="continuity_delta").unwrap()["content"]=json!({"changes":[{"novelEntityId":"e","fieldPath":"x","timeNode":"t","after":1}]});
            first["artifacts"].as_array_mut().unwrap().iter_mut().find(|v|v["artifactType"]=="open_threads").unwrap()["content"]=json!({"threads":[{"stableKey":"thread-a"}]});
            complete_analysis_inner(conn,&run1,validate_analysis_output(&first)?,"owner")?;
            let world_one = adopted_revision_for(conn, &run1.id, "world_facts")?;
            let parent:String=conn.query_row("SELECT current_context_revision_id FROM novel_analysis_lineages WHERE id=?",params![work.current_analysis_lineage_id],|r|r.get(0)).map_err(|e|e.to_string())?;
            let rev2=make_revision(2,"rev-2")?;
            let run2=analysis_start_inner(conn,NovelAnalysisStartInput{project_id:"p".into(),novel_work_id:work.id.clone(),chapter_revision_id:rev2.id,parent_context_revision_id:Some(parent),idempotency_key:"run-2".into(),provider_id:None,model_id:None},true,"test".into(),"test".into(),"owner")?;
            let mut second=valid_analysis_output(json!({"items":[{"stableKey":"hero","resolutionKind":"conflict"}]}));
            second["artifacts"].as_array_mut().unwrap().iter_mut().find(|v|v["artifactType"]=="world_facts").unwrap()["content"]=json!({"items":[{"stableKey":"world-b","resolutionKind":"conflict"}]});
            second["artifacts"].as_array_mut().unwrap().iter_mut().find(|v|v["artifactType"]=="timeline_delta").unwrap()["content"]=json!({"events":[{"stableKey":"event-b"}]});
            second["artifacts"].as_array_mut().unwrap().iter_mut().find(|v|v["artifactType"]=="continuity_delta").unwrap()["content"]=json!({"changes":[{"novelEntityId":"e","fieldPath":"x","timeNode":"t","after":2},{"novelEntityId":"e","fieldPath":"y","timeNode":"t","after":3}]});
            second["artifacts"].as_array_mut().unwrap().iter_mut().find(|v|v["artifactType"]=="open_threads").unwrap()["content"]=json!({"threads":[{"stableKey":"thread-b"}]});
            complete_analysis_inner(conn,&run2,validate_analysis_output(&second)?,"owner")?;
            let world_two = adopted_revision_for(conn, &run2.id, "world_facts")?;
            let diff = novel_canon_diff_inner(conn, NovelCanonDiffInput { project_id:"p".into(),novel_work_id:work.id.clone() })?;
            assert_eq!(diff.source_revision_ids, vec![world_one, world_two]);
            assert_eq!(diff.content["world_facts"]["items"].as_array().unwrap().len(), 2);
            let (canon,state_json):(String,String)=conn.query_row("SELECT resolved_working_canon_json,resolved_working_state_json FROM novel_chapter_context_revisions WHERE source_analysis_run_id=?",params![run2.id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
            assert_eq!(json_value(canon)["timeline_delta"]["events"].as_array().unwrap().len(),2);
            assert_eq!(json_value(state_json.clone())["open_threads"]["threads"].as_array().unwrap().len(),2);
            assert_eq!(json_value(state_json)["continuity_delta"]["changes"].as_array().unwrap().len(),2);
            let head_before:String=conn.query_row("SELECT current_context_revision_id FROM novel_analysis_lineages WHERE id=?",params![work.current_analysis_lineage_id],|r|r.get(0)).map_err(|e|e.to_string())?;
            let rev4=make_revision(4,"rev-4")?; let run4=analysis_start_inner(conn,NovelAnalysisStartInput{project_id:"p".into(),novel_work_id:work.id.clone(),chapter_revision_id:rev4.id,parent_context_revision_id:Some(head_before.clone()),idempotency_key:"run-4".into(),provider_id:None,model_id:None},true,"test".into(),"test".into(),"owner")?; complete_analysis_inner(conn,&run4,validate_analysis_output(&valid_analysis_output(json!({"items":[{"stableKey":"hero","resolutionKind":"conflict"}]})))?,"owner")?;let head_after:String=conn.query_row("SELECT current_context_revision_id FROM novel_analysis_lineages WHERE id=?",params![work.current_analysis_lineage_id],|r|r.get(0)).map_err(|e|e.to_string())?;assert_eq!(head_before,head_after);Ok(())}).unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v8_adoption_token_is_hashed_scoped_once_and_rolls_back_on_cas_failure() {
        let (dir, state) = open_test_db("adopt-token");
        db::with_connection(&state,|conn|{let work=create_work(conn,"p","adopt-work");let rev=novel_chapter_revision_create_inner(conn,NovelChapterRevisionCreateInput{project_id:"p".into(),novel_work_id:work.id.clone(),chapter_id:None,volume_id:None,sequence_no:Some(1),chapter_no:Some(1),title:None,content:"x".into(),parent_context_revision_id:None,asset_id:None,source_kind:None,idempotency_key:"ar".into()})?;let run=analysis_start_inner(conn,NovelAnalysisStartInput{project_id:"p".into(),novel_work_id:work.id.clone(),chapter_revision_id:rev.id,parent_context_revision_id:None,idempotency_key:"as".into(),provider_id:None,model_id:None},true,"t".into(),"t".into(),"o")?;complete_analysis_inner(conn,&run,validate_analysis_output(&valid_analysis_output(json!({"items":[{"stableKey":"h","resolutionKind":"conflict"}]})))?,"o")?;let (artifact,revision,version):(String,String,i64)=conn.query_row("SELECT id,candidate_head_revision_id,optimistic_version FROM analysis_artifacts WHERE source_analysis_run_id=? LIMIT 1",params![run.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|e|e.to_string())?;let preview=novel_artifact_adopt_preview_inner(conn,NovelArtifactAdoptPreviewInput{project_id:"p".into(),novel_work_id:work.id.clone(),artifact_id:artifact.clone(),revision_id:revision.clone(),expected_optimistic_version:version})?;let stored:String=conn.query_row("SELECT token_hash FROM novel_artifact_adoption_previews",[],|r|r.get(0)).map_err(|e|e.to_string())?;assert_ne!(stored,preview.approval_token);assert!(!stored.contains(&preview.approval_token));let bad=NovelArtifactAdoptInput{project_id:"p".into(),novel_work_id:work.id.clone(),artifact_id:"foreign".into(),revision_id:revision.clone(),expected_optimistic_version:version,approval_token:preview.approval_token.clone(),idempotency_key:"cross".into()};assert!(novel_artifact_adopt_inner(conn,bad).is_err());conn.execute("UPDATE novel_artifact_adoption_previews SET expires_at=0",[]).map_err(|e|e.to_string())?;let expired=NovelArtifactAdoptInput{project_id:"p".into(),novel_work_id:work.id.clone(),artifact_id:artifact.clone(),revision_id:revision.clone(),expected_optimistic_version:version,approval_token:preview.approval_token.clone(),idempotency_key:"expired".into()};assert!(novel_artifact_adopt_inner(conn,expired).is_err());let unused:i64=conn.query_row("SELECT COUNT(*) FROM novel_artifact_adoption_previews WHERE used_at IS NULL",[],|r|r.get(0)).map_err(|e|e.to_string())?;assert_eq!(unused,1);Ok(())}).unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v8_artifact_scope_trigger_rejects_invalid_runtime_scope() {
        let (dir, state) = open_test_db("scope-trigger");
        db::with_connection(&state,|conn|{let work=create_work(conn,"p","scope-work");let rev=novel_chapter_revision_create_inner(conn,NovelChapterRevisionCreateInput{project_id:"p".into(),novel_work_id:work.id.clone(),chapter_id:None,volume_id:None,sequence_no:Some(1),chapter_no:Some(1),title:None,content:"x".into(),parent_context_revision_id:None,asset_id:None,source_kind:None,idempotency_key:"r".into()})?;let run=analysis_start_inner(conn,NovelAnalysisStartInput{project_id:"p".into(),novel_work_id:work.id.clone(),chapter_revision_id:rev.id.clone(),parent_context_revision_id:None,idempotency_key:"s".into(),provider_id:None,model_id:None},true,"t".into(),"t".into(),"o")?;let (adaptation,chapter):(String,String)=conn.query_row("SELECT frozen_comic_adaptation_id,frozen_comic_chapter_id FROM source_analysis_runs WHERE id=?",params![run.id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;let bad=conn.execute("INSERT INTO analysis_artifacts(id,source_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,status,optimistic_version,created_at,updated_at) VALUES('bad',?,'world_facts',?,?,?,?,'active',0,1,1)",params![run.id,work.id,rev.id,adaptation,chapter]);assert!(bad.is_err());let ok=conn.execute("INSERT INTO analysis_artifacts(id,source_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,status,optimistic_version,created_at,updated_at) VALUES('ok',?,'scene_plan',?,?,?,?, 'active',0,1,1)",params![run.id,work.id,rev.id,adaptation,chapter]);assert!(ok.is_ok());Ok(())}).unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v7_context_head_requires_continuity_cas_and_rejects_detached_gap() {
        let (dir, state) = open_test_db("context");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "project-a", "work-create-context");
            let revision = novel_chapter_revision_create_inner(conn, NovelChapterRevisionCreateInput {
                project_id: "project-a".into(), novel_work_id: work.id.clone(), chapter_id: None,
                volume_id: None, sequence_no: Some(1), chapter_no: Some(1), title: None, content: "正文".into(),
                parent_context_revision_id: None,
                asset_id: None, source_kind: None, idempotency_key: "context-revision".into(),
            })?;
            let lineage = list_lineages(conn, &work.id)?.remove(0);
            conn.execute(
                "INSERT INTO source_analysis_runs (id, novel_chapter_revision_id, novel_analysis_lineage_id, base_working_context_revision_id, base_canon_version_id, base_novel_state_version_id, frozen_input_fingerprint, provider_id, model_id, status, prompt_version, schema_version, idempotency_key, created_at, updated_at)
                 VALUES ('run-main', ?, ?, NULL, ?, ?, 'hash', 'test', 'test', 'ready_for_review', 'v1', 'v1', 'run-main-key', 1, 1)",
                params![revision.id, lineage.id, work.published_canon_version_id, work.current_novel_state_version_id],
            ).map_err(|error| format!("seed run 失败: {error}"))?;
            conn.execute(
                "INSERT INTO novel_chapter_context_revisions (id, novel_analysis_lineage_id, novel_chapter_revision_id, parent_context_revision_id, source_analysis_run_id, base_published_canon_version_id, base_published_novel_state_version_id, resolved_working_canon_json, resolved_working_canon_hash, resolved_working_state_json, resolved_working_state_hash, sequence_gap_json, branch_kind, status, created_at)
                 VALUES ('ctx-main', ?, ?, NULL, 'run-main', ?, ?, '{}', 'canon-hash', '{}', 'state-hash', '[]', 'main', 'ready', 1)",
                params![lineage.id, revision.id, work.published_canon_version_id, work.current_novel_state_version_id],
            ).map_err(|error| format!("seed context 失败: {error}"))?;
            advance_context_head_inner(conn, &lineage.id, "ctx-main", None, 1, 0)?;
            assert!(advance_context_head_inner(conn, &lineage.id, "ctx-main", None, 1, 0).is_err());
            let error = conn.execute(
                "INSERT INTO novel_chapter_context_revisions (id, novel_analysis_lineage_id, novel_chapter_revision_id, parent_context_revision_id, source_analysis_run_id, base_published_canon_version_id, base_published_novel_state_version_id, resolved_working_canon_json, resolved_working_canon_hash, resolved_working_state_json, resolved_working_state_hash, sequence_gap_json, branch_kind, status, created_at)
                 VALUES ('ctx-gap', ?, ?, 'ctx-main', 'run-main', ?, ?, '{}', 'canon-hash', '{}', 'state-hash', '[2]', 'main', 'ready', 2)",
                params![lineage.id, revision.id, work.published_canon_version_id, work.current_novel_state_version_id],
            );
            assert!(error.is_err());
            assert!(novel_context_list_inner(conn, &NovelContextListInput { project_id: "project-b".into(), novel_work_id: work.id, lineage_id: lineage.id, limit: None }).is_err());
            Ok(())
        }).unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v8_analysis_without_llm_is_persisted_error_and_replays_receipt() {
        let (dir, state) = open_test_db("analysis-not-configured");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "project-a", "analysis-work");
            let revision = novel_chapter_revision_create_inner(conn, NovelChapterRevisionCreateInput {
                project_id: "project-a".into(), novel_work_id: work.id.clone(), chapter_id: None,
                volume_id: None, sequence_no: Some(1), chapter_no: Some(1), title: None, content: "正文".into(),
                parent_context_revision_id: None, asset_id: None, source_kind: None, idempotency_key: "analysis-revision".into(),
            })?;
            let input = NovelAnalysisStartInput { project_id: "project-a".into(), novel_work_id: work.id, chapter_revision_id: revision.id, parent_context_revision_id: None, idempotency_key: "analysis-start".into(), provider_id: None, model_id: None };
            let first = analysis_start_inner(conn, input.clone(), false, "configured_llm".into(), "test".into(), "session")?;
            let replay = analysis_start_inner(conn, input, false, "configured_llm".into(), "test".into(), "session")?;
            assert_eq!(first.id, replay.id);
            assert_eq!(first.status, "error");
            assert_eq!(first.safe_error.as_deref(), Some("NOT_CONFIGURED"));
            let attempts: i64 = conn.query_row("SELECT COUNT(*) FROM source_analysis_run_attempts WHERE source_analysis_run_id=? AND status='error'", params![first.id], |row| row.get(0)).map_err(|e| e.to_string())?;
            assert_eq!(attempts, 1);
            Ok(())
        }).unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v8_optimize_retry_and_recovery_are_scoped_and_cas_safe() {
        let (dir, state) = open_test_db("optimize-retry-recovery");
        db::with_connection(&state, |conn| {
            let seed = |project_id: &str, key: &str| -> Result<(NovelWork, String, String, i64, String), String> {
                let work = create_work(conn, project_id, &format!("{key}-work"));
                let chapter = novel_chapter_revision_create_inner(conn, NovelChapterRevisionCreateInput {
                    project_id: project_id.into(), novel_work_id: work.id.clone(), chapter_id: None,
                    volume_id: None, sequence_no: Some(1), chapter_no: Some(1), title: None,
                    content: "优化测试正文".into(), parent_context_revision_id: None, asset_id: None,
                    source_kind: None, idempotency_key: format!("{key}-chapter"),
                })?;
                let analysis = analysis_start_inner(conn, NovelAnalysisStartInput {
                    project_id: project_id.into(), novel_work_id: work.id.clone(), chapter_revision_id: chapter.id,
                    parent_context_revision_id: None, idempotency_key: format!("{key}-analysis"),
                    provider_id: None, model_id: None,
                }, true, "test".into(), "test".into(), "owner")?;
                complete_analysis_inner(conn, &analysis, validate_analysis_output(&valid_analysis_output(json!({"items": [{"stableKey": "hero", "resolutionKind": "conflict"}]})))?, "owner")?;
                let target: (String, String, i64) = conn.query_row(
                    "SELECT id,candidate_head_revision_id,optimistic_version FROM analysis_artifacts WHERE source_analysis_run_id=? AND artifact_type='chapter_summary'",
                    params![analysis.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                ).map_err(|error| error.to_string())?;
                let source: String = conn.query_row(
                    "SELECT candidate_head_revision_id FROM analysis_artifacts WHERE source_analysis_run_id=? AND artifact_type='world_facts'",
                    params![analysis.id], |row| row.get(0),
                ).map_err(|error| error.to_string())?;
                Ok((work, target.0, target.1, target.2, source))
            };

            let (work_a, artifact_a, parent_a, version_a, source_a) = seed("project-a", "opt-a")?;
            let (work_b, artifact_b, parent_b, version_b, _) = seed("project-b", "opt-b")?;
            let start = NovelArtifactOptimizeStartInput {
                project_id: "project-a".into(), novel_work_id: work_a.id.clone(), artifact_id: artifact_a.clone(),
                parent_revision_id: parent_a.clone(), expected_optimistic_version: version_a,
                instruction: "只优化摘要表达".into(), provider_id: "provider-a".into(), model_id: "model-a".into(),
                selected_source_revision_ids: vec![source_a.clone()], idempotency_key: "opt-start-a".into(),
            };
            let first = optimize_start_inner(conn, start.clone(), true, "owner")?;
            let replay = optimize_start_inner(conn, start, true, "owner")?;
            assert_eq!(first.artifact_optimization_run_id, replay.artifact_optimization_run_id);
            assert_eq!(first.status, "running");
            let persisted_start: (String, String, String, i64, i64, i64) = conn.query_row(
                "SELECT novel_work_id,analysis_artifact_id,parent_artifact_revision_id,attempt_no,(SELECT COUNT(*) FROM novel_artifact_optimization_inputs WHERE novel_artifact_optimization_run_id=run.id),(SELECT COUNT(*) FROM novel_artifact_optimization_attempts WHERE novel_artifact_optimization_run_id=run.id) FROM novel_artifact_optimization_runs run WHERE id=?",
                params![first.artifact_optimization_run_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            ).map_err(|error| error.to_string())?;
            assert_eq!(persisted_start, (work_a.id.clone(), artifact_a.clone(), parent_a.clone(), 1, 1, 1));
            assert!(optimize_start_inner(conn, NovelArtifactOptimizeStartInput {
                project_id: "project-b".into(), novel_work_id: work_b.id.clone(), artifact_id: artifact_a.clone(),
                parent_revision_id: parent_a.clone(), expected_optimistic_version: version_a,
                instruction: "越权 work".into(), provider_id: "provider-a".into(), model_id: "model-a".into(),
                selected_source_revision_ids: vec![source_a.clone()], idempotency_key: "opt-cross-work".into(),
            }, true, "owner").is_err());
            assert!(optimize_start_inner(conn, NovelArtifactOptimizeStartInput {
                project_id: "project-a".into(), novel_work_id: work_a.id.clone(), artifact_id: artifact_a.clone(),
                parent_revision_id: source_a.clone(), expected_optimistic_version: version_a,
                instruction: "错误 parent".into(), provider_id: "provider-a".into(), model_id: "model-a".into(),
                selected_source_revision_ids: vec![], idempotency_key: "opt-cross-artifact-parent".into(),
            }, true, "owner").is_err());

            let fresh = optimize_start_inner(conn, NovelArtifactOptimizeStartInput {
                project_id: "project-a".into(), novel_work_id: work_a.id.clone(), artifact_id: artifact_a.clone(),
                parent_revision_id: parent_a.clone(), expected_optimistic_version: version_a,
                instruction: "未过期 run".into(), provider_id: "provider-a".into(), model_id: "model-a".into(),
                selected_source_revision_ids: vec![], idempotency_key: "opt-fresh".into(),
            }, true, "owner")?;
            let other_work = optimize_start_inner(conn, NovelArtifactOptimizeStartInput {
                project_id: "project-b".into(), novel_work_id: work_b.id.clone(), artifact_id: artifact_b,
                parent_revision_id: parent_b, expected_optimistic_version: version_b,
                instruction: "其他 work 过期 run".into(), provider_id: "provider-b".into(), model_id: "model-b".into(),
                selected_source_revision_ids: vec![], idempotency_key: "opt-other-work".into(),
            }, true, "owner")?;
            let expired_at = now() - 1;
            conn.execute("UPDATE novel_artifact_optimization_runs SET lease_expires_at=? WHERE id IN (?,?)", params![expired_at, first.artifact_optimization_run_id, other_work.artifact_optimization_run_id]).map_err(|error| error.to_string())?;
            conn.execute("UPDATE novel_artifact_optimization_attempts SET lease_expires_at=? WHERE novel_artifact_optimization_run_id IN (?,?)", params![expired_at, first.artifact_optimization_run_id, other_work.artifact_optimization_run_id]).map_err(|error| error.to_string())?;
            assert_eq!(optimize_recover_stale_inner(conn, "project-a", &work_a.id)?, 1);
            let recovered: (String, Option<String>) = conn.query_row("SELECT status,safe_error_code FROM novel_artifact_optimization_runs WHERE id=?", params![first.artifact_optimization_run_id], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|error| error.to_string())?;
            assert_eq!(recovered, ("error".into(), Some("LEASE_EXPIRED".into())));
            let recovered_attempt: (String, Option<String>) = conn.query_row("SELECT status,safe_error_code FROM novel_artifact_optimization_attempts WHERE novel_artifact_optimization_run_id=? AND attempt_no=1", params![first.artifact_optimization_run_id], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|error| error.to_string())?;
            assert_eq!(recovered_attempt, ("error".into(), Some("LEASE_EXPIRED".into())));
            let unaffected: Vec<(String, String)> = [fresh.artifact_optimization_run_id.as_str(), other_work.artifact_optimization_run_id.as_str()].into_iter().map(|id| conn.query_row("SELECT status,novel_work_id FROM novel_artifact_optimization_runs WHERE id=?", params![id], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|error| error.to_string())).collect::<Result<_, _>>()?;
            assert_eq!(unaffected, vec![("running".into(), work_a.id.clone()), ("running".into(), work_b.id.clone())]);
            assert!(fail_optimization_inner(conn, &fresh.artifact_optimization_run_id, "different-owner", "provider failed").is_err());
            let failed = fail_optimization_inner(conn, &fresh.artifact_optimization_run_id, "owner", "provider failed")?;
            assert_eq!(failed.status, "error");
            let failed_attempt: (String, Option<String>) = conn.query_row("SELECT status,safe_error_code FROM novel_artifact_optimization_attempts WHERE novel_artifact_optimization_run_id=? AND attempt_no=1", params![fresh.artifact_optimization_run_id], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|error| error.to_string())?;
            assert_eq!(failed_attempt, ("error".into(), Some("OPTIMIZATION_FAILED".into())));

            let drift_revision = new_id("nartifactrev");
            let next_version: i64 = conn.query_row("SELECT COALESCE(MAX(version),0)+1 FROM analysis_artifact_revisions WHERE analysis_artifact_id=?", params![artifact_a], |row| row.get(0)).map_err(|error| error.to_string())?;
            conn.execute("INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?,?,?,? ,?,'','manual','{}','{}','candidate',?)", params![drift_revision, artifact_a, next_version, parent_a, valid_content("chapter_summary").to_string(), now()]).map_err(|error| error.to_string())?;
            conn.execute("UPDATE analysis_artifacts SET candidate_head_revision_id=?,optimistic_version=optimistic_version+1,updated_at=? WHERE id=?", params![drift_revision, now(), artifact_a]).map_err(|error| error.to_string())?;

            let retry_input = NovelArtifactOptimizeRetryInput { project_id: "project-a".into(), novel_work_id: work_a.id.clone(), artifact_optimization_run_id: first.artifact_optimization_run_id.clone(), idempotency_key: "opt-retry-a".into() };
            let child = optimize_retry_inner(conn, retry_input.clone(), true, "owner")?;
            let retry_replay = optimize_retry_inner(conn, retry_input, true, "owner")?;
            assert_eq!(child.artifact_optimization_run_id, retry_replay.artifact_optimization_run_id);
            let frozen_child: (String, String, String, String, String, i64, String) = conn.query_row(
                "SELECT novel_work_id,analysis_artifact_id,parent_artifact_revision_id,instruction,provider_id,attempt_no,model_id FROM novel_artifact_optimization_runs WHERE id=?",
                params![child.artifact_optimization_run_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            ).map_err(|error| error.to_string())?;
            assert_eq!(frozen_child, (work_a.id.clone(), artifact_a.clone(), parent_a.clone(), "只优化摘要表达".into(), "provider-a".into(), 2, "model-a".into()));
            let retry_lineage: (i64, i64, String, i64) = conn.query_row(
                "SELECT (SELECT COUNT(*) FROM novel_artifact_optimization_runs WHERE id=?),(SELECT COUNT(*) FROM novel_artifact_optimization_attempts WHERE novel_artifact_optimization_run_id=?),child.parent_attempt_id,parent.attempt_no FROM novel_artifact_optimization_attempts child JOIN novel_artifact_optimization_attempts parent ON parent.id=child.parent_attempt_id WHERE child.novel_artifact_optimization_run_id=? AND child.attempt_no=2",
                params![first.artifact_optimization_run_id, first.artifact_optimization_run_id, first.artifact_optimization_run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).map_err(|error| error.to_string())?;
            assert_eq!((retry_lineage.0, retry_lineage.1, retry_lineage.3), (1, 2, 1));
            assert!(!retry_lineage.2.is_empty());
            let prompt = optimization_prompt_input(conn, &child.artifact_optimization_run_id)?;
            let frozen_prompt: Value = serde_json::from_str(&prompt.1).map_err(|error| error.to_string())?;
            assert_eq!(frozen_prompt["parentContent"], json_value(conn.query_row("SELECT body_json FROM analysis_artifact_revisions WHERE id=?", params![parent_a], |row| row.get(0)).map_err(|error| error.to_string())?));
            assert_eq!(frozen_prompt["selectedSources"].as_array().unwrap().len(), 1);
            let finished = complete_optimization_inner(conn, &child.artifact_optimization_run_id, valid_content("chapter_summary"), "owner")?;
            assert_eq!(finished.status, "error");
            assert_eq!(finished.safe_error.as_deref(), Some("BASELINE_STALE"));
            let finished_attempt: (String, Option<String>) = conn.query_row("SELECT status,safe_error_code FROM novel_artifact_optimization_attempts WHERE novel_artifact_optimization_run_id=? AND attempt_no=2", params![first.artifact_optimization_run_id], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|error| error.to_string())?;
            assert_eq!(finished_attempt, ("error".into(), Some("BASELINE_STALE".into())));
            let current_head: String = conn.query_row("SELECT candidate_head_revision_id FROM analysis_artifacts WHERE id=?", params![artifact_a], |row| row.get(0)).map_err(|error| error.to_string())?;
            assert_eq!(current_head, drift_revision);
            assert!(optimize_retry_inner(conn, NovelArtifactOptimizeRetryInput { project_id: "project-b".into(), novel_work_id: work_b.id, artifact_optimization_run_id: first.artifact_optimization_run_id, idempotency_key: "opt-retry-cross-work".into() }, true, "owner").is_err());
            Ok(())
        }).unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v14_chapter_state_publication_is_immutable_cas_scoped_and_inheritable() {
        let (dir, state) = open_test_db("chapter-state-publication");
        db::with_connection(&state, |conn| {
            let work = create_work(conn, "chapter-state-project", "chapter-state-work");
            let revision = novel_chapter_revision_create_inner(
                conn,
                NovelChapterRevisionCreateInput {
                    project_id: "chapter-state-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_id: None,
                    volume_id: None,
                    sequence_no: Some(1),
                    chapter_no: Some(1),
                    title: None,
                    content: "第一章正文".into(),
                    parent_context_revision_id: None,
                    asset_id: None,
                    source_kind: None,
                    idempotency_key: "chapter-state-revision".into(),
                },
            )?;
            let run = analysis_start_inner(
                conn,
                NovelAnalysisStartInput {
                    project_id: "chapter-state-project".into(),
                    novel_work_id: work.id.clone(),
                    chapter_revision_id: revision.id.clone(),
                    parent_context_revision_id: None,
                    idempotency_key: "chapter-state-analysis".into(),
                    provider_id: None,
                    model_id: None,
                },
                true,
                "test".into(),
                "test".into(),
                "owner",
            )?;
            complete_analysis_inner(
                conn,
                &run,
                validate_analysis_output(&valid_analysis_output(json!({
                    "items":[{"stableKey":"world-1","resolutionKind":"conflict"}]
                })))?,
                "owner",
            )?;
            let world = adopted_revision_for(conn, &run.id, "world_facts")?;
            let timeline = adopted_revision_for(conn, &run.id, "timeline_delta")?;
            let continuity = adopted_revision_for(conn, &run.id, "continuity_delta")?;
            let threads = adopted_revision_for(conn, &run.id, "open_threads")?;
            let initial = NovelChapterStatePublishInput {
                project_id: "chapter-state-project".into(),
                novel_work_id: work.id.clone(),
                novel_chapter_revision_id: revision.id.clone(),
                state_before_version_id: work.current_novel_state_version_id.clone(),
                source_analysis_run_id: run.id.clone(),
                adaptation_analysis_run_id: None,
                allow_source_analysis_without_adaptation: true,
                source_revision_ids: vec![world.clone(), timeline.clone(), continuity.clone(), threads.clone()],
                idempotency_key: "chapter-state-publish-one".into(),
            };
            let published = novel_chapter_state_publish_inner(conn, initial.clone())?;
            assert_eq!(published.state_after_version, 1);
            assert_eq!(published.state_before_version_id, work.current_novel_state_version_id);
            assert!(published.state_after.get("timeline_delta").is_some());
            assert!(published.canon_delta.get("world_facts").is_some());
            assert!(published.continuity_delta.get("open_threads").is_some());
            assert_eq!(
                novel_chapter_state_publish_inner(conn, initial.clone())?.id,
                published.id,
                "same receipt must replay the committed publication"
            );
            assert!(novel_chapter_state_publish_inner(
                conn,
                NovelChapterStatePublishInput {
                    idempotency_key: "chapter-state-stale".into(),
                    ..initial.clone()
                }
            )
            .is_err());
            assert!(novel_chapter_state_publish_inner(
                conn,
                NovelChapterStatePublishInput {
                    allow_source_analysis_without_adaptation: false,
                    idempotency_key: "chapter-state-missing-explicit-source".into(),
                    ..initial.clone()
                }
            )
            .is_err());
            let republished = novel_chapter_state_publish_inner(
                conn,
                NovelChapterStatePublishInput {
                    state_before_version_id: published.state_after_version_id.clone(),
                    idempotency_key: "chapter-state-publish-two".into(),
                    ..initial.clone()
                },
            )?;
            assert_ne!(republished.id, published.id);
            assert_ne!(republished.state_after_version_id, published.state_after_version_id);
            assert_eq!(republished.state_after_version, 2);
            assert!(conn
                .execute(
                    "UPDATE novel_chapter_state_publications SET source_fingerprint='changed' WHERE id=?",
                    params![published.id],
                )
                .is_err());
            assert!(conn
                .execute(
                    "UPDATE novel_chapter_state_publication_sources SET source_order=9
                     WHERE novel_chapter_state_publication_id=?",
                    params![published.id],
                )
                .is_err());
            let baseline = novel_chapter_state_baseline_inner(
                conn,
                NovelChapterStateBaselineInput {
                    project_id: "chapter-state-project".into(),
                    novel_work_id: work.id.clone(),
                    novel_chapter_revision_id: Some(revision.id.clone()),
                },
            )?;
            assert_eq!(baseline.state_version_id, republished.state_after_version_id);
            assert_eq!(baseline.publication.as_ref().map(|item| &item.id), Some(&republished.id));
            assert_eq!(baseline.through_novel_chapter_revision_id, Some(revision.id.clone()));
            assert!(novel_chapter_state_publish_inner(
                conn,
                NovelChapterStatePublishInput {
                    state_before_version_id: republished.state_after_version_id.clone(),
                    adaptation_analysis_run_id: Some("not-an-approved-run".into()),
                    allow_source_analysis_without_adaptation: false,
                    idempotency_key: "chapter-state-bad-adaptation".into(),
                    ..initial.clone()
                }
            )
            .is_err());
            conn.execute(
                "UPDATE analysis_artifact_revisions SET body_json='[]' WHERE id=?",
                params![world],
            )
            .map_err(|error| error.to_string())?;
            assert!(novel_chapter_state_publish_inner(
                conn,
                NovelChapterStatePublishInput {
                    state_before_version_id: republished.state_after_version_id.clone(),
                    idempotency_key: "chapter-state-malformed-source".into(),
                    ..initial.clone()
                }
            )
            .is_err());
            let foreign = create_work(conn, "chapter-state-project", "chapter-state-foreign");
            assert!(novel_chapter_state_publish_inner(
                conn,
                NovelChapterStatePublishInput {
                    novel_work_id: foreign.id.clone(),
                    state_before_version_id: foreign.current_novel_state_version_id.clone(),
                    idempotency_key: "chapter-state-cross-work".into(),
                    ..initial
                }
            )
            .is_err());
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM novel_chapter_state_publications WHERE novel_work_id=?",
                    params![work.id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(count, 2, "failed validation must not leave a partial State version");
            Ok(())
        })
        .unwrap();
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
