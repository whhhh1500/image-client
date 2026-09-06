//! Immutable renderer inputs for NovelWork-backed comic production.
//!
//! This module intentionally stops at a verified, frozen manifest.  It never
//! writes legacy comic projects or invokes a provider; a later page-run module
//! consumes these rows and reuses the existing durable image-run lifecycle.

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{self, DbState};
use crate::novel::{new_id, now, request_hash};

const COMMAND_PREPARE: &str = "comic_visual_manifest_prepare";
const CONTRACT_VERSION: &str = "comic-visual.v1";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualManifestPrepareInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub apply_operation_id: String,
    pub production_chapter_id: String,
    pub idempotency_key: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualManifestGetInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub manifest_id: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualManifestListInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub production_chapter_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualManifest {
    pub id: String,
    pub contract_version: String,
    pub project_id: String,
    pub novel_work_id: String,
    pub comic_adaptation_id: String,
    pub apply_operation_id: String,
    pub production_chapter_id: String,
    pub page_panel_plan_revision_id: String,
    pub freshness: String,
    pub manifest_fingerprint: String,
    pub created_at: i64,
    pub manifest: Value,
}

type ManifestRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
);

#[tauri::command]
pub fn comic_visual_manifest_prepare(
    state: tauri::State<'_, DbState>,
    input: ComicVisualManifestPrepareInput,
) -> Result<ComicVisualManifest, String> {
    db::with_connection(&state, |conn| prepare_inner(conn, input))
}

#[tauri::command]
pub fn comic_visual_manifest_get(
    state: tauri::State<'_, DbState>,
    input: ComicVisualManifestGetInput,
) -> Result<ComicVisualManifest, String> {
    db::with_connection(&state, |conn| get_inner(conn, input))
}

#[tauri::command]
pub fn comic_visual_manifest_list_for_chapter(
    state: tauri::State<'_, DbState>,
    input: ComicVisualManifestListInput,
) -> Result<Vec<ComicVisualManifest>, String> {
    db::with_connection(&state, |conn| list_inner(conn, input))
}

pub(crate) fn prepare_inner(
    conn: &Connection,
    input: ComicVisualManifestPrepareInput,
) -> Result<ComicVisualManifest, String> {
    let request_hash = request_hash(&json!({
        "projectId": input.project_id,
        "novelWorkId": input.novel_work_id,
        "comicAdaptationId": input.comic_adaptation_id,
        "applyOperationId": input.apply_operation_id,
        "productionChapterId": input.production_chapter_id,
    }))?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "VISUAL_MANIFEST_WRITE_FAILED".to_string())?;
    let receipt: Option<(String, String)> = tx
        .query_row(
            "SELECT request_hash, manifest_id FROM comic_visual_manifest_receipts WHERE command_name=? AND idempotency_key=?",
            params![COMMAND_PREPARE, input.idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    if let Some((existing_hash, manifest_id)) = receipt {
        if existing_hash != request_hash {
            return Err("IDEMPOTENCY_MISMATCH".into());
        }
        tx.commit()
            .map_err(|_| "VISUAL_MANIFEST_WRITE_FAILED".to_string())?;
        return get_by_id(conn, &input.project_id, &input.novel_work_id, &manifest_id);
    }

    let tuple = (&input.apply_operation_id, &input.production_chapter_id);
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM comic_visual_manifests WHERE apply_operation_id=? AND production_chapter_id=?",
            params![tuple.0, tuple.1],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    let manifest_id = if let Some(id) = existing {
        // A historical record may be stale, but must never be overwritten to
        // look current. Only replaying its original idempotency key above can
        // read stale history; a new key must not bless stale input.
        validate_manifest_owner(&tx, &id, &input)?;
        let revision: String = tx
            .query_row(
                "SELECT page_panel_plan_revision_id FROM comic_visual_manifests WHERE id=?",
                params![id],
                |row| row.get(0),
            )
            .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
        if !current_scope_is_ready(
            &tx,
            &input.project_id,
            &input.novel_work_id,
            &input.comic_adaptation_id,
            &input.apply_operation_id,
            &input.production_chapter_id,
            &revision,
        )? {
            return Err("VISUAL_MANIFEST_STALE_OR_SCOPE_MISMATCH".into());
        }
        id
    } else {
        let built = build_manifest(&tx, &input)?;
        let id = new_id("comic_visual_manifest");
        tx.execute(
            "INSERT INTO comic_visual_manifests (id,project_id,novel_work_id,comic_adaptation_id,apply_operation_id,production_chapter_id,page_panel_plan_revision_id,contract_version,manifest_json,manifest_fingerprint,created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            params![
                id, input.project_id, input.novel_work_id, input.comic_adaptation_id,
                input.apply_operation_id, input.production_chapter_id,
                built.page_panel_plan_revision_id, CONTRACT_VERSION,
                serde_json::to_string(&built.manifest).map_err(|_| "VISUAL_MANIFEST_SERIALIZE_FAILED")?,
                built.fingerprint, now(),
            ],
        ).map_err(|_| "VISUAL_MANIFEST_WRITE_FAILED".to_string())?;
        id
    };
    tx.execute(
        "INSERT INTO comic_visual_manifest_receipts (command_name,idempotency_key,request_hash,manifest_id,created_at) VALUES (?,?,?,?,?)",
        params![COMMAND_PREPARE, input.idempotency_key, request_hash, manifest_id, now()],
    ).map_err(|_| "VISUAL_MANIFEST_WRITE_FAILED".to_string())?;
    tx.commit()
        .map_err(|_| "VISUAL_MANIFEST_WRITE_FAILED".to_string())?;
    get_by_id(conn, &input.project_id, &input.novel_work_id, &manifest_id)
}

struct BuiltManifest {
    page_panel_plan_revision_id: String,
    fingerprint: String,
    manifest: Value,
}

/// Builds only from the already-applied, exact adopted plan. Any missing map,
/// frozen context, source, or current head causes a safe fail-closed code.
fn build_manifest(
    conn: &Connection,
    input: &ComicVisualManifestPrepareInput,
) -> Result<BuiltManifest, String> {
    let scope: Option<(String, String, String)> = conn.query_row(
        "SELECT chapter.comic_planning_chapter_id, planning.planning_chapter_stable_key, chapter.page_panel_plan_revision_id
         FROM comic_production_chapters chapter
         JOIN comic_planning_chapters planning ON planning.id=chapter.comic_planning_chapter_id
         JOIN comic_adaptations adaptation ON adaptation.id=chapter.comic_adaptation_id
         JOIN analysis_apply_operations operation ON operation.id=chapter.apply_operation_id
         JOIN analysis_apply_receipts receipt ON receipt.analysis_apply_operation_id=operation.id
         JOIN comic_adaptation_plan_heads head ON head.id=planning.comic_adaptation_plan_head_id AND head.status='active'
         JOIN analysis_artifact_revisions revision ON revision.id=chapter.page_panel_plan_revision_id
         JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
         WHERE chapter.id=? AND chapter.status='active'
           AND chapter.apply_operation_id=? AND chapter.comic_adaptation_id=?
           AND adaptation.project_id=? AND adaptation.novel_work_id=?
           AND operation.operation_type='apply_comic_plan' AND operation.status='succeeded'
           AND operation.comic_adaptation_id=chapter.comic_adaptation_id
           AND adaptation.optimistic_version=operation.expected_adaptation_version+1
           AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id
           AND artifact.artifact_type='page_panel_plan' AND artifact.comic_adaptation_id=chapter.comic_adaptation_id",
        params![input.production_chapter_id, input.apply_operation_id, input.comic_adaptation_id, input.project_id, input.novel_work_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional().map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    let Some((planning_chapter_id, planning_chapter_stable_key, page_plan_revision_id)) = scope
    else {
        return Err("VISUAL_MANIFEST_STALE_OR_SCOPE_MISMATCH".into());
    };
    ensure_entity_map(
        conn,
        &input.apply_operation_id,
        "production_chapter",
        &planning_chapter_stable_key,
        &input.production_chapter_id,
    )?;

    let source_revision_ids = string_rows(conn, "SELECT novel_chapter_revision_id FROM comic_planning_chapter_sources WHERE comic_planning_chapter_id=? ORDER BY source_order", &planning_chapter_id)?;
    if source_revision_ids.is_empty() {
        return Err("VISUAL_MANIFEST_SOURCE_MISSING".into());
    }
    let plan_raw: String = conn.query_row(
        "SELECT revision.body_json FROM analysis_artifact_revisions revision WHERE revision.id=?",
        params![page_plan_revision_id], |row| row.get(0),
    ).map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    let plan: Value =
        serde_json::from_str(&plan_raw).map_err(|_| "VISUAL_MANIFEST_PLAN_INVALID".to_string())?;
    let pages = plan
        .get("pages")
        .and_then(Value::as_array)
        .ok_or("VISUAL_MANIFEST_PLAN_INVALID")?;
    if pages.is_empty() {
        return Err("VISUAL_MANIFEST_PLAN_INVALID".into());
    }

    let contexts = load_contexts(
        conn,
        &input.production_chapter_id,
        &input.comic_adaptation_id,
        &input.novel_work_id,
    )?;
    let scene_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_production_scenes WHERE comic_production_chapter_id=?",
            params![input.production_chapter_id],
            |r| r.get(0),
        )
        .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    if scene_count != contexts.len() as i64 {
        return Err("VISUAL_MANIFEST_CONTEXT_MISSING".into());
    }
    for context in contexts.values() {
        ensure_entity_map(
            conn,
            &input.apply_operation_id,
            "production_scene",
            &context.planning_scene_stable_key,
            &context.production_scene_id,
        )?;
        let selected: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM analysis_apply_scene_context_selections WHERE apply_operation_id=? AND planning_scene_stable_key=? AND scene_context_snapshot_id=?)", params![input.apply_operation_id, context.planning_scene_stable_key, context.snapshot_id], |row| row.get(0)).map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
        if !selected {
            return Err("VISUAL_MANIFEST_RECEIPT_MISMATCH".into());
        }
    }
    let mut frozen_contexts = Vec::new();
    let mut output_pages = Vec::new();
    let db_page_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_production_pages WHERE comic_production_chapter_id=?",
            params![input.production_chapter_id],
            |r| r.get(0),
        )
        .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    if db_page_count != pages.len() as i64 {
        return Err("VISUAL_MANIFEST_TREE_MISMATCH".into());
    }

    for page in pages {
        let stable_key = required_str(page, "stableKey")?;
        let page_no = required_i64(page, "pageNo")?;
        let layout = page
            .get("layout")
            .filter(|value| value.is_object())
            .ok_or("VISUAL_MANIFEST_PLAN_INVALID")?
            .clone();
        let plan_panels = page
            .get("panels")
            .and_then(Value::as_array)
            .ok_or("VISUAL_MANIFEST_PLAN_INVALID")?;
        if plan_panels.is_empty() {
            return Err("VISUAL_MANIFEST_PLAN_INVALID".into());
        }
        let production_page: Option<String> = conn.query_row(
            "SELECT id FROM comic_production_pages WHERE comic_production_chapter_id=? AND planning_page_stable_key=? AND page_no=?",
            params![input.production_chapter_id, stable_key, page_no], |r| r.get(0),
        ).optional().map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
        let production_page = production_page.ok_or("VISUAL_MANIFEST_TREE_MISMATCH")?;
        ensure_entity_map(
            conn,
            &input.apply_operation_id,
            "production_page",
            stable_key,
            &production_page,
        )?;
        let panel_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM comic_production_panels WHERE comic_production_page_id=?",
                params![production_page],
                |r| r.get(0),
            )
            .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
        if panel_count != plan_panels.len() as i64 {
            return Err("VISUAL_MANIFEST_TREE_MISMATCH".into());
        }
        let mut output_panels = Vec::new();
        for panel in plan_panels {
            let panel_key = required_str(panel, "stableKey")?;
            let panel_no = required_i64(panel, "panelNo")?;
            let (panel_id, scene_id, spec_raw): (String, Option<String>, String) = conn.query_row(
                "SELECT id,comic_production_scene_id,spec_json FROM comic_production_panels WHERE comic_production_page_id=? AND planning_panel_stable_key=? AND panel_no=?",
                params![production_page, panel_key, panel_no], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            ).optional().map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?.ok_or("VISUAL_MANIFEST_TREE_MISMATCH")?;
            let spec: Value = serde_json::from_str(&spec_raw)
                .map_err(|_| "VISUAL_MANIFEST_TREE_MISMATCH".to_string())?;
            if spec != *panel {
                return Err("VISUAL_MANIFEST_TREE_MISMATCH".into());
            }
            ensure_entity_map(
                conn,
                &input.apply_operation_id,
                "production_panel",
                panel_key,
                &panel_id,
            )?;
            let scene_id = scene_id.ok_or("VISUAL_MANIFEST_CONTEXT_MISSING")?;
            let context = contexts
                .get(&scene_id)
                .ok_or("VISUAL_MANIFEST_CONTEXT_MISSING")?;
            let requested_scene = panel
                .get("planningSceneStableKey")
                .or_else(|| panel.get("sceneStableKey"))
                .and_then(Value::as_str)
                .ok_or("VISUAL_MANIFEST_PLAN_INVALID")?;
            if requested_scene != context.planning_scene_stable_key {
                return Err("VISUAL_MANIFEST_TREE_MISMATCH".into());
            }
            output_panels.push(json!({
                "productionPanelId": panel_id, "productionSceneId": scene_id,
                "stableKey": panel_key, "panelNo": panel_no,
                "sceneContextSnapshotId": context.snapshot_id, "spec": spec,
            }));
        }
        output_pages.push(json!({"productionPageId": production_page, "stableKey": stable_key, "pageNo": page_no, "layout": layout, "panels": output_panels}));
    }
    for context in contexts.values() {
        frozen_contexts.push(json!({
            "productionSceneId": context.production_scene_id,
            "planningSceneStableKey": context.planning_scene_stable_key,
            "sceneContextSnapshotId": context.snapshot_id,
            "contextFingerprint": context.fingerprint,
            "novelCanonVersionId": context.canon_version_id,
            "novelStateVersionId": context.state_version_id,
            "continuityStateVersionId": context.continuity_version_id,
            "resolvedContext": context.resolved_context,
            "resolvedContextHash": context.resolved_context_hash,
        }));
    }
    let manifest = json!({
        "schemaVersion": CONTRACT_VERSION,
        "provenance": {"planningChapterStableKey": planning_chapter_stable_key, "sourceRevisionIds": source_revision_ids, "sceneContexts": frozen_contexts},
        "pages": output_pages,
    });
    let fingerprint = format!(
        "{:x}",
        md5::compute(
            serde_json::to_vec(&manifest).map_err(|_| "VISUAL_MANIFEST_SERIALIZE_FAILED")?
        )
    );
    Ok(BuiltManifest {
        page_panel_plan_revision_id: page_plan_revision_id,
        fingerprint,
        manifest,
    })
}

#[derive(Clone)]
struct ContextRow {
    production_scene_id: String,
    planning_scene_stable_key: String,
    snapshot_id: String,
    fingerprint: String,
    canon_version_id: String,
    state_version_id: Option<String>,
    continuity_version_id: String,
    resolved_context: Value,
    resolved_context_hash: String,
}

fn load_contexts(
    conn: &Connection,
    chapter_id: &str,
    adaptation_id: &str,
    work_id: &str,
) -> Result<BTreeMap<String, ContextRow>, String> {
    let mut statement = conn.prepare(
        "SELECT scene.id,scene.planning_scene_stable_key,snapshot.id,snapshot.context_fingerprint,snapshot.novel_canon_version_id,snapshot.novel_state_version_id,snapshot.continuity_state_version_id,snapshot.resolved_context_json,snapshot.resolved_context_hash
         FROM comic_production_scenes scene JOIN comic_scene_context_snapshots snapshot ON snapshot.id=scene.scene_context_snapshot_id
         WHERE scene.comic_production_chapter_id=? AND snapshot.status='frozen' AND snapshot.comic_adaptation_id=? AND snapshot.novel_work_id=? ORDER BY scene.scene_no",
    ).map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    let rows = statement
        .query_map(params![chapter_id, adaptation_id, work_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
            ))
        })
        .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (
            production_scene_id,
            planning_scene_stable_key,
            snapshot_id,
            fingerprint,
            canon_version_id,
            state_version_id,
            continuity_version_id,
            context_raw,
            context_hash,
        ) = row.map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
        let resolved_context: Value = serde_json::from_str(&context_raw)
            .map_err(|_| "VISUAL_MANIFEST_CONTEXT_MISSING".to_string())?;
        if request_hash(&resolved_context)? != context_hash {
            return Err("VISUAL_MANIFEST_CONTEXT_MISMATCH".into());
        }
        let row = ContextRow {
            production_scene_id,
            planning_scene_stable_key,
            snapshot_id,
            fingerprint,
            canon_version_id,
            state_version_id,
            continuity_version_id,
            resolved_context,
            resolved_context_hash: context_hash,
        };
        out.insert(row.production_scene_id.clone(), row);
    }
    if out.is_empty() {
        return Err("VISUAL_MANIFEST_CONTEXT_MISSING".into());
    }
    Ok(out)
}

fn ensure_entity_map(
    conn: &Connection,
    operation_id: &str,
    kind: &str,
    stable_key: &str,
    id: &str,
) -> Result<(), String> {
    let valid: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM analysis_apply_receipt_entity_maps WHERE analysis_apply_operation_id=? AND entity_kind=? AND stable_key=? AND entity_id=?)", params![operation_id,kind,stable_key,id], |r|r.get(0)).map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    valid
        .then_some(())
        .ok_or("VISUAL_MANIFEST_RECEIPT_MISMATCH".into())
}

fn string_rows(conn: &Connection, sql: &str, value: &str) -> Result<Vec<String>, String> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    let rows = statement
        .query_map(params![value], |row| row.get(0))
        .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    rows.collect::<Result<Vec<String>, _>>()
        .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())
}
fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or("VISUAL_MANIFEST_PLAN_INVALID".into())
}
fn required_i64(value: &Value, key: &str) -> Result<i64, String> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or("VISUAL_MANIFEST_PLAN_INVALID".into())
}

fn validate_manifest_owner(
    conn: &Connection,
    id: &str,
    input: &ComicVisualManifestPrepareInput,
) -> Result<(), String> {
    let valid: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM comic_visual_manifests WHERE id=? AND project_id=? AND novel_work_id=? AND comic_adaptation_id=? AND apply_operation_id=? AND production_chapter_id=?)",params![id,input.project_id,input.novel_work_id,input.comic_adaptation_id,input.apply_operation_id,input.production_chapter_id],|r|r.get(0)).map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    valid
        .then_some(())
        .ok_or("VISUAL_MANIFEST_SCOPE_MISMATCH".into())
}

pub(crate) fn get_inner(
    conn: &Connection,
    input: ComicVisualManifestGetInput,
) -> Result<ComicVisualManifest, String> {
    get_by_id(
        conn,
        &input.project_id,
        &input.novel_work_id,
        &input.manifest_id,
    )
}
pub(crate) fn get_by_id(
    conn: &Connection,
    project_id: &str,
    work_id: &str,
    id: &str,
) -> Result<ComicVisualManifest, String> {
    let row: ManifestRow = conn.query_row("SELECT id,contract_version,project_id,novel_work_id,comic_adaptation_id,apply_operation_id,production_chapter_id,page_panel_plan_revision_id,manifest_fingerprint,created_at,manifest_json FROM comic_visual_manifests WHERE id=? AND project_id=? AND novel_work_id=?",params![id,project_id,work_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?))).optional().map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?.ok_or("VISUAL_MANIFEST_UNKNOWN")?;
    manifest_from_row(conn, row)
}
pub(crate) fn list_inner(
    conn: &Connection,
    input: ComicVisualManifestListInput,
) -> Result<Vec<ComicVisualManifest>, String> {
    let chapter_owned: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM comic_production_chapters chapter JOIN comic_adaptations adaptation ON adaptation.id=chapter.comic_adaptation_id WHERE chapter.id=? AND adaptation.project_id=? AND adaptation.novel_work_id=?)",params![input.production_chapter_id,input.project_id,input.novel_work_id],|r|r.get(0)).map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    if !chapter_owned {
        return Err("VISUAL_MANIFEST_SCOPE_MISMATCH".into());
    }
    let mut statement=conn.prepare("SELECT id,contract_version,project_id,novel_work_id,comic_adaptation_id,apply_operation_id,production_chapter_id,page_panel_plan_revision_id,manifest_fingerprint,created_at,manifest_json FROM comic_visual_manifests WHERE project_id=? AND novel_work_id=? AND production_chapter_id=? ORDER BY created_at DESC,id DESC").map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    let rows = statement
        .query_map(
            params![
                input.project_id,
                input.novel_work_id,
                input.production_chapter_id
            ],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            },
        )
        .map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?;
    rows.map(|row| {
        manifest_from_row(
            conn,
            row.map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())?,
        )
    })
    .collect()
}
fn manifest_from_row(conn: &Connection, row: ManifestRow) -> Result<ComicVisualManifest, String> {
    let manifest: Value =
        serde_json::from_str(&row.10).map_err(|_| "VISUAL_MANIFEST_CORRUPT".to_string())?;
    let actual_fingerprint = format!(
        "{:x}",
        md5::compute(serde_json::to_vec(&manifest).map_err(|_| "VISUAL_MANIFEST_CORRUPT")?)
    );
    if actual_fingerprint != row.8 {
        return Err("VISUAL_MANIFEST_CORRUPT".into());
    }
    let freshness = if current_scope_is_ready(conn, &row.2, &row.3, &row.4, &row.5, &row.6, &row.7)?
    {
        "ready"
    } else {
        "stale"
    };
    Ok(ComicVisualManifest {
        id: row.0,
        contract_version: row.1,
        project_id: row.2,
        novel_work_id: row.3,
        comic_adaptation_id: row.4,
        apply_operation_id: row.5,
        production_chapter_id: row.6,
        page_panel_plan_revision_id: row.7,
        manifest_fingerprint: row.8,
        created_at: row.9,
        freshness: freshness.into(),
        manifest,
    })
}
fn current_scope_is_ready(
    conn: &Connection,
    project: &str,
    work: &str,
    adaptation: &str,
    operation: &str,
    chapter: &str,
    revision: &str,
) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM comic_production_chapters chapter JOIN comic_planning_chapters planning ON planning.id=chapter.comic_planning_chapter_id JOIN comic_adaptations adaptation ON adaptation.id=chapter.comic_adaptation_id JOIN analysis_apply_operations operation ON operation.id=chapter.apply_operation_id JOIN analysis_apply_receipts receipt ON receipt.analysis_apply_operation_id=operation.id JOIN comic_adaptation_plan_heads head ON head.id=planning.comic_adaptation_plan_head_id AND head.status='active' JOIN analysis_artifact_revisions revision ON revision.id=chapter.page_panel_plan_revision_id JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id WHERE chapter.id=? AND chapter.status='active' AND chapter.apply_operation_id=? AND chapter.comic_adaptation_id=? AND chapter.page_panel_plan_revision_id=? AND adaptation.project_id=? AND adaptation.novel_work_id=? AND operation.operation_type='apply_comic_plan' AND operation.status='succeeded' AND adaptation.optimistic_version=operation.expected_adaptation_version+1 AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='page_panel_plan' AND artifact.comic_adaptation_id=chapter.comic_adaptation_id)",params![chapter,operation,adaptation,revision,project,work],|r|r.get(0)).map_err(|_| "VISUAL_MANIFEST_READ_FAILED".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_page_semantics_without_leaking_content() {
        assert_eq!(
            required_str(&json!({"stableKey":""}), "stableKey").unwrap_err(),
            "VISUAL_MANIFEST_PLAN_INVALID"
        );
        assert_eq!(
            required_i64(&json!({"pageNo":0}), "pageNo").unwrap_err(),
            "VISUAL_MANIFEST_PLAN_INVALID"
        );
    }

    #[test]
    fn rejects_manifest_with_tampered_fingerprint_before_scope_lookup() {
        let conn = Connection::open_in_memory().unwrap();
        let row: ManifestRow = (
            "manifest".into(),
            CONTRACT_VERSION.into(),
            "project".into(),
            "work".into(),
            "adaptation".into(),
            "operation".into(),
            "chapter".into(),
            "revision".into(),
            "not-the-content-hash".into(),
            1,
            "{\"schemaVersion\":\"comic-visual.v1\"}".into(),
        );
        assert_eq!(
            manifest_from_row(&conn, row).map(|_| ()).unwrap_err(),
            "VISUAL_MANIFEST_CORRUPT"
        );
    }
}
