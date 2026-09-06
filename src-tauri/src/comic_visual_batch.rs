//! Durable, explicitly authorized sequential rendering for a production job.
//!
//! This module never discovers work from a mounted frontend.  A batch exists
//! only after a user-authorized production request, freezes its image options,
//! and advances a single manifest page from durable renderer outcomes.

use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::Manager;

use crate::comic_visual::{self, ComicVisualManifestPrepareInput};
use crate::comic_visual_render::{self, ComicVisualPageRunStartInput, COMPILER_CONTRACT};
use crate::db::{self, DbState};
use crate::novel::{new_id, now, request_hash, NovelProductionJob};
use crate::AppState;

const BATCH_LEASE_MS: i64 = 60_000;
const TARGET_COMIC_PAGES: &str = "comic_pages";
const COMMAND_RESUME: &str = "comic_visual_batch_resume";
const COMMAND_MEMBER_RETRY: &str = "comic_visual_batch_member_retry";
static LINKED_RUN_RECOVERY_WAKES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static BATCH_LEASE_WAKES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VisualImageOptionsInput {
    pub model: Option<String>,
    pub size: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VisualOutputInput {
    pub target: String,
    #[serde(default)]
    pub image_options: Option<VisualImageOptionsInput>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrozenAuthorization {
    target: String,
    provider_id: String,
    model: Option<String>,
    size: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualBatchGetInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub production_job_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualBatchResumeInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub production_job_id: String,
    pub idempotency_key: String,
}

/// Retry one known, failed visual page through its owning batch.  The caller
/// must bind the exact currently-linked parent run and acknowledge that the
/// new child can submit a separately billable generation request.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualBatchMemberRetryInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub production_job_id: String,
    pub batch_id: String,
    pub member_id: String,
    pub production_page_id: String,
    pub expected_parent_run_id: String,
    pub confirm_possible_charge: bool,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NovelProductionVisualAuthorizeInput {
    pub project_id: String,
    pub novel_work_id: String,
    pub production_job_id: String,
    pub visual_output: VisualOutputInput,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualBatchMember {
    pub member_id: String,
    pub ordinal: i64,
    pub manifest_id: String,
    pub production_chapter_id: String,
    pub production_page_id: String,
    pub page_no: i64,
    pub page_stable_key: String,
    pub status: String,
    pub page_run_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComicVisualBatch {
    pub id: String,
    pub project_id: String,
    pub novel_work_id: String,
    pub novel_chapter_id: String,
    pub source_revision_id: String,
    pub production_job_id: String,
    pub output_target: String,
    pub status: String,
    pub total_members: i64,
    pub completed_members: i64,
    pub current_member_ordinal: Option<i64>,
    pub safe_error_code: Option<String>,
    pub safe_user_message: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
    pub members: Vec<ComicVisualBatchMember>,
}

#[derive(Clone)]
struct BatchRow {
    id: String,
    project_id: String,
    novel_work_id: String,
    novel_chapter_id: String,
    source_revision_id: String,
    production_job_id: String,
    comic_adaptation_id: Option<String>,
    apply_operation_id: Option<String>,
    authorization: FrozenAuthorization,
    resolved_model: Option<String>,
    status: String,
}

/// Build an authorization snapshot before opening the job-creation
/// transaction.  It contains no credential material.
pub(crate) fn freeze_authorization(
    input: &VisualOutputInput,
    provider_id: &str,
    configured_model: &str,
) -> Result<Value, String> {
    if input.target != TARGET_COMIC_PAGES {
        return Err("VISUAL_OUTPUT_TARGET_UNSUPPORTED".into());
    }
    let options = input.image_options.as_ref();
    let requested_model = options
        .and_then(|value| value.model.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let requested_size = options
        .and_then(|value| value.size.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if requested_size.is_some_and(|value| value != "1024x1536") {
        return Err("VISUAL_IMAGE_SIZE_UNSUPPORTED".into());
    }
    let configured_model = configured_model.trim();
    let authorization = FrozenAuthorization {
        target: TARGET_COMIC_PAGES.into(),
        provider_id: provider_id.to_owned(),
        model: requested_model
            .or_else(|| (!configured_model.is_empty()).then(|| configured_model.to_owned())),
        size: "1024x1536".into(),
    };
    serde_json::to_value(authorization).map_err(|_| "VISUAL_AUTHORIZATION_SERIALIZE_FAILED".into())
}

/// Called by the new-job creation transaction only.  Existing source-revision
/// jobs intentionally bypass this function: a new start payload must not add
/// paid authorization to an older job.
pub(crate) fn authorize_new_job_tx(
    tx: &Transaction<'_>,
    job: &NovelProductionJob,
    authorization: &Value,
) -> Result<(), String> {
    let intent = json!({"productionJobId":job.id,"authorization":authorization});
    insert_batch_tx(
        tx,
        job,
        authorization,
        &format!("{}:visual", job.id),
        &intent,
    )
    .map(|_| ())
}

fn insert_batch_tx(
    tx: &Transaction<'_>,
    job: &NovelProductionJob,
    authorization: &Value,
    idempotency_key: &str,
    request: &Value,
) -> Result<String, String> {
    let frozen: FrozenAuthorization = serde_json::from_value(authorization.clone())
        .map_err(|_| "VISUAL_AUTHORIZATION_INVALID".to_string())?;
    if frozen.target != TARGET_COMIC_PAGES || frozen.size != "1024x1536" {
        return Err("VISUAL_AUTHORIZATION_INVALID".into());
    }
    let authorization_fingerprint = request_hash(authorization)?;
    let request_hash_value = request_hash(&request)?;
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT id,request_hash FROM comic_visual_batches WHERE production_job_id=?",
            params![job.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    if let Some((id, stored_hash)) = existing {
        if stored_hash != request_hash_value {
            return Err("VISUAL_BATCH_AUTHORIZATION_MISMATCH".into());
        }
        return Ok(id);
    }
    let id = new_id("comic_visual_batch");
    let timestamp = now();
    tx.execute(
        "INSERT INTO comic_visual_batches (id,project_id,novel_work_id,novel_chapter_id,source_revision_id,production_job_id,comic_adaptation_id,apply_operation_id,output_target,authorization_json,authorization_fingerprint,request_hash,idempotency_key,status,total_members,completed_members,current_member_ordinal,lease_owner,lease_expires_at,safe_error_code,safe_user_message,created_at,updated_at,finished_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,'waiting_text',0,0,NULL,NULL,NULL,NULL,NULL,?,?,NULL)",
        params![id,job.project_id,job.novel_work_id,job.novel_chapter_id,job.source_revision_id,job.id,Option::<String>::None,Option::<String>::None,TARGET_COMIC_PAGES,authorization.to_string(),authorization_fingerprint,request_hash_value,idempotency_key,timestamp,timestamp],
    ).map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok(id)
}

#[tauri::command]
pub fn comic_visual_batch_get(
    state: tauri::State<'_, DbState>,
    input: ComicVisualBatchGetInput,
) -> Result<Option<ComicVisualBatch>, String> {
    db::with_connection(&state, |conn| {
        get_by_job(
            conn,
            &input.project_id,
            &input.novel_work_id,
            &input.production_job_id,
        )
    })
}

#[tauri::command]
pub fn novel_production_visual_authorize(
    app: tauri::AppHandle,
    state: tauri::State<'_, DbState>,
    app_state: tauri::State<'_, AppState>,
    input: NovelProductionVisualAuthorizeInput,
) -> Result<ComicVisualBatch, String> {
    let intent =
        json!({"productionJobId":input.production_job_id,"visualOutput":input.visual_output});
    let intent_hash = request_hash(&intent)?;
    let (batch, should_schedule) = db::with_connection(&state, |conn| {
        let job = load_job_scope(
            conn,
            &input.project_id,
            &input.novel_work_id,
            &input.production_job_id,
        )?;
        if job.status != "succeeded" {
            return Err("VISUAL_AUTHORIZATION_REQUIRES_SUCCEEDED_PRODUCTION".into());
        }
        let replay: Option<(String, String)> = conn
            .query_row(
                "SELECT id,request_hash FROM comic_visual_batches WHERE idempotency_key=?",
                params![input.idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
        if let Some((id, stored_hash)) = replay {
            if stored_hash != intent_hash {
                return Err("IDEMPOTENCY_MISMATCH".into());
            }
            let batch = get_by_id(conn, &input.project_id, &input.novel_work_id, &id)?
                .ok_or("VISUAL_BATCH_READ_FAILED")?;
            return Ok((batch, false));
        }
        let cfg = app_state
            .cfg
            .read()
            .map_err(|_| "CONFIG_UNAVAILABLE")?
            .clone();
        let authorization = freeze_authorization(
            &input.visual_output,
            app_state.registry.active().id(),
            &cfg.image_model,
        )?;
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
        let id = insert_batch_tx(&tx, &job, &authorization, &input.idempotency_key, &intent)?;
        tx.commit()
            .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
        Ok((
            get_by_id(conn, &input.project_id, &input.novel_work_id, &id)?
                .ok_or("VISUAL_BATCH_READ_FAILED")?,
            true,
        ))
    })?;
    if should_schedule {
        schedule_batch(&app, &batch.id);
    }
    Ok(batch)
}

#[tauri::command]
pub fn comic_visual_batch_resume(
    app: tauri::AppHandle,
    state: tauri::State<'_, DbState>,
    app_state: tauri::State<'_, AppState>,
    input: ComicVisualBatchResumeInput,
) -> Result<ComicVisualBatch, String> {
    let cfg = app_state
        .cfg
        .read()
        .map_err(|_| "CONFIG_UNAVAILABLE")?
        .clone();
    let active_provider = app_state.registry.active().id().to_owned();
    let (batch, queued_child, should_schedule_batch) = db::with_connection(&state, |conn| {
        resume_batch_inner(
            conn,
            &input,
            cfg.status().image_ready,
            cfg.image_model.trim(),
            &active_provider,
            state.app_session_id(),
        )
    })?;
    if let Some(run_id) = queued_child {
        comic_visual_render::schedule_dispatch(app, run_id);
    } else if should_schedule_batch && batch.status != "needs_reconcile" {
        schedule_batch(&app, &batch.id);
    }
    Ok(batch)
}

fn resume_batch_inner(
    conn: &Connection,
    input: &ComicVisualBatchResumeInput,
    image_ready: bool,
    configured_model: &str,
    active_provider: &str,
    session: &str,
) -> Result<(ComicVisualBatch, Option<String>, bool), String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    let current = get_by_job(
        &tx,
        &input.project_id,
        &input.novel_work_id,
        &input.production_job_id,
    )?
    .ok_or("VISUAL_BATCH_NOT_AUTHORIZED")?;
    let resume_hash = request_hash(&json!({"batchId":current.id,"action":"resume"}))?;
    let replay: Option<(String, String)> = tx.query_row("SELECT request_hash,batch_id FROM comic_visual_batch_command_receipts WHERE command_name=? AND idempotency_key=?",params![COMMAND_RESUME,input.idempotency_key],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|_|"VISUAL_BATCH_READ_FAILED".to_string())?;
    if let Some((stored, receipt_batch_id)) = replay {
        if stored != resume_hash || receipt_batch_id != current.id {
            return Err("IDEMPOTENCY_MISMATCH".into());
        }
        tx.commit()
            .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
        return Ok((
            get_by_id(conn, &input.project_id, &input.novel_work_id, &current.id)?
                .ok_or("VISUAL_BATCH_READ_FAILED")?,
            queued_resumed_child(conn, &current.id)?,
            false,
        ));
    }
    ensure_resume_lease_available(&tx, &current.id)?;
    let raw: String = tx
        .query_row(
            "SELECT authorization_json FROM comic_visual_batches WHERE id=?",
            params![current.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    let authorization: FrozenAuthorization =
        serde_json::from_str(&raw).map_err(|_| "VISUAL_AUTHORIZATION_INVALID".to_string())?;
    if !image_ready || authorization.provider_id != active_provider {
        return Err("VISUAL_BATCH_CONFIG_REQUIRED".into());
    }
    let resolved_model: Option<String> = tx
        .query_row(
            "SELECT resolved_model FROM comic_visual_batches WHERE id=?",
            params![current.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    let model = if let Some(model) = authorization.model.clone().or(resolved_model) {
        model
    } else {
        if configured_model.is_empty() {
            return Err("VISUAL_BATCH_CONFIG_REQUIRED".into());
        }
        // Only a model that was absent at authorization may be resolved here.
        // Keep the original authorization JSON/fingerprint intact.
        tx.execute("UPDATE comic_visual_batches SET resolved_model=COALESCE(resolved_model,?),updated_at=? WHERE id=?",params![configured_model,now(),current.id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
        configured_model.to_owned()
    };
    let before_sync = read_batch(&tx, &current.id)?.ok_or("VISUAL_BATCH_READ_FAILED")?;
    reconcile_unlinked_members(&tx, &before_sync)?;
    sync_member_outcomes(&tx, &before_sync)?;
    let batch = read_batch(&tx, &current.id)?.ok_or("VISUAL_BATCH_READ_FAILED")?;
    if batch.status == "needs_reconcile" {
        return Err("VISUAL_BATCH_RECONCILE_REQUIRED".into());
    }
    if batch.status == "blocked_stale" {
        return Err("VISUAL_BATCH_STALE_REQUIRED".into());
    }
    let resumed = resume_first_blocked_config_member_tx(
        &tx,
        &batch,
        session,
        &authorization.provider_id,
        &model,
    )?;
    let (queued_child, should_schedule_batch, has_running_member) = match resumed {
        ResumedMember::NewQueued(run_id) | ResumedMember::ExistingQueued(run_id) => {
            (Some(run_id), false, false)
        }
        ResumedMember::ExistingRunning => (None, true, true),
        ResumedMember::NoLinkedRun => (None, true, false),
    };
    if queued_child.is_some() || has_running_member {
        tx.execute("UPDATE comic_visual_batches SET status='running',safe_error_code=NULL,safe_user_message=NULL,updated_at=? WHERE id=?",params![now(),current.id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
    } else {
        tx.execute("UPDATE comic_visual_batches SET status='authorized',safe_error_code=NULL,safe_user_message=NULL,updated_at=? WHERE id=? AND status='blocked_config'",params![now(),current.id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
    }
    tx.execute("INSERT INTO comic_visual_batch_command_receipts(command_name,idempotency_key,request_hash,batch_id,created_at) VALUES (?,?,?,?,?)",params![COMMAND_RESUME,input.idempotency_key,resume_hash,current.id,now()]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
    tx.commit()
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok((
        get_by_id(conn, &input.project_id, &input.novel_work_id, &current.id)?
            .ok_or("VISUAL_BATCH_READ_FAILED")?,
        queued_child,
        should_schedule_batch,
    ))
}

#[tauri::command]
pub fn comic_visual_batch_member_retry(
    app: tauri::AppHandle,
    state: tauri::State<'_, DbState>,
    app_state: tauri::State<'_, AppState>,
    input: ComicVisualBatchMemberRetryInput,
) -> Result<ComicVisualBatch, String> {
    if !input.confirm_possible_charge {
        return Err("VISUAL_BATCH_RETRY_CONFIRMATION_REQUIRED".into());
    }
    let cfg = app_state
        .cfg
        .read()
        .map_err(|_| "CONFIG_UNAVAILABLE")?
        .clone();
    let provider_id = app_state.registry.active().id().to_owned();
    let (batch, queued_child) = db::with_connection(&state, |conn| {
        retry_member_inner(
            conn,
            &input,
            cfg.status().image_ready,
            &provider_id,
            state.app_session_id(),
        )
    })?;
    if let Some(run_id) = queued_child {
        comic_visual_render::schedule_dispatch(app, run_id);
    }
    Ok(batch)
}

fn retry_member_inner(
    conn: &Connection,
    input: &ComicVisualBatchMemberRetryInput,
    image_ready: bool,
    active_provider: &str,
    session: &str,
) -> Result<(ComicVisualBatch, Option<String>), String> {
    if input.idempotency_key.trim().is_empty() {
        return Err("IDEMPOTENCY_KEY_REQUIRED".into());
    }
    if !input.confirm_possible_charge {
        return Err("VISUAL_BATCH_RETRY_CONFIRMATION_REQUIRED".into());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    let public = get_by_id(
        &tx,
        &input.project_id,
        &input.novel_work_id,
        &input.batch_id,
    )?
    .ok_or("VISUAL_BATCH_NOT_FOUND")?;
    if public.production_job_id != input.production_job_id {
        return Err("VISUAL_BATCH_RETRY_SCOPE_MISMATCH".into());
    }
    let job = load_job_scope(
        &tx,
        &input.project_id,
        &input.novel_work_id,
        &input.production_job_id,
    )?;
    if job.id != public.production_job_id {
        return Err("VISUAL_BATCH_RETRY_SCOPE_MISMATCH".into());
    }
    let request = json!({
        "projectId": &input.project_id,
        "novelWorkId": &input.novel_work_id,
        "productionJobId": &input.production_job_id,
        "batchId": &input.batch_id,
        "memberId": &input.member_id,
        "productionPageId": &input.production_page_id,
        "expectedParentRunId": &input.expected_parent_run_id,
        "confirmPossibleCharge": input.confirm_possible_charge,
    });
    let hash = request_hash(&request)?;
    let replay: Option<(String, String)> = tx
        .query_row(
            "SELECT request_hash,batch_id FROM comic_visual_batch_command_receipts WHERE command_name=? AND idempotency_key=?",
            params![COMMAND_MEMBER_RETRY, input.idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    if let Some((stored_hash, receipt_batch_id)) = replay {
        if stored_hash != hash || receipt_batch_id != input.batch_id {
            return Err("IDEMPOTENCY_MISMATCH".into());
        }
        let batch = get_by_id(
            &tx,
            &input.project_id,
            &input.novel_work_id,
            &input.batch_id,
        )?
        .ok_or("VISUAL_BATCH_READ_FAILED")?;
        let queued = queued_retried_child(
            &tx,
            &input.batch_id,
            &input.member_id,
            &input.expected_parent_run_id,
        )?;
        tx.commit()
            .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
        return Ok((batch, queued));
    }
    let initial = read_batch(&tx, &input.batch_id)?.ok_or("VISUAL_BATCH_READ_FAILED")?;
    if initial.production_job_id != input.production_job_id
        || initial.project_id != input.project_id
        || initial.novel_work_id != input.novel_work_id
    {
        return Err("VISUAL_BATCH_RETRY_SCOPE_MISMATCH".into());
    }
    if matches!(initial.status.as_str(), "needs_reconcile" | "blocked_stale") {
        return Err(if initial.status == "needs_reconcile" {
            "VISUAL_BATCH_RECONCILE_REQUIRED".into()
        } else {
            "VISUAL_BATCH_STALE_REQUIRED".into()
        });
    }
    ensure_member_retry_lease_available(&tx, &initial.id)?;
    if !image_ready || initial.authorization.provider_id != active_provider {
        return Err("VISUAL_BATCH_CONFIG_REQUIRED".into());
    }
    reconcile_unlinked_members(&tx, &initial)?;
    sync_member_outcomes(&tx, &initial)?;
    let batch = read_batch(&tx, &initial.id)?.ok_or("VISUAL_BATCH_READ_FAILED")?;
    if matches!(batch.status.as_str(), "needs_reconcile" | "blocked_stale") {
        return Err(if batch.status == "needs_reconcile" {
            "VISUAL_BATCH_RECONCILE_REQUIRED".into()
        } else {
            "VISUAL_BATCH_STALE_REQUIRED".into()
        });
    }
    let member: Option<(String, String, String, String)> = tx
        .query_row(
            "SELECT id,production_page_id,page_run_id,status FROM comic_visual_batch_members WHERE id=? AND batch_id=?",
            params![input.member_id, batch.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    let Some((member_id, page_id, linked_run_id, member_status)) = member else {
        return Err("VISUAL_BATCH_RETRY_SCOPE_MISMATCH".into());
    };
    if page_id != input.production_page_id {
        return Err("VISUAL_BATCH_RETRY_SCOPE_MISMATCH".into());
    }
    if linked_run_id != input.expected_parent_run_id {
        return Err("VISUAL_BATCH_RETRY_PARENT_CHANGED".into());
    }
    if batch.status != "failed" {
        return Err("VISUAL_BATCH_RETRY_STATE_INVALID".into());
    }
    let first_member: Option<String> = tx
        .query_row(
            "SELECT id FROM comic_visual_batch_members WHERE batch_id=? AND status<>'candidate_ready' ORDER BY ordinal LIMIT 1",
            params![batch.id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    if first_member.as_deref() != Some(member_id.as_str()) {
        return Err("VISUAL_BATCH_RETRY_NOT_CURRENT_MEMBER".into());
    }
    if member_status != "failed" {
        return Err("VISUAL_BATCH_RETRY_MEMBER_NOT_FAILED".into());
    }
    let parent = comic_visual_render::get_run(
        &tx,
        &batch.project_id,
        &batch.novel_work_id,
        &input.expected_parent_run_id,
    )?;
    if parent.status != "failed" {
        return Err("VISUAL_BATCH_RETRY_PARENT_NOT_FAILED".into());
    }
    if parent.production_page_id != page_id
        || parent.provider_id.as_deref() != Some(active_provider)
        || parent.provider_id.as_deref() != Some(batch.authorization.provider_id.as_str())
    {
        return Err("VISUAL_BATCH_RETRY_PARENT_SCOPE_MISMATCH".into());
    }
    let frozen_model = batch
        .authorization
        .model
        .as_deref()
        .or(batch.resolved_model.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("VISUAL_BATCH_RETRY_FROZEN_MODEL_REQUIRED")?;
    let parent_model = parent
        .request_json
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("VISUAL_BATCH_RETRY_FROZEN_REQUEST_MISMATCH")?;
    let parent_size = parent
        .request_json
        .get("size")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("VISUAL_BATCH_RETRY_FROZEN_REQUEST_MISMATCH")?;
    if parent_model != frozen_model || parent_size != batch.authorization.size {
        return Err("VISUAL_BATCH_RETRY_FROZEN_REQUEST_MISMATCH".into());
    }
    let manifest_id: String = tx
        .query_row(
            "SELECT manifest_id FROM comic_visual_batch_members WHERE id=? AND batch_id=?",
            params![member_id, batch.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    if parent.manifest_id != manifest_id {
        return Err("VISUAL_BATCH_RETRY_PARENT_SCOPE_MISMATCH".into());
    }
    let child_key = format!(
        "{}:retry:{}",
        batch_member_dispatch_key(&tx, &member_id)?,
        parent.id
    );
    let child = comic_visual_render::retry_failed_child_tx(
        &tx,
        &parent,
        child_key,
        session,
        active_provider,
    )?;
    if child.status != "queued" {
        return Err("VISUAL_BATCH_RETRY_CHILD_STATE_INVALID".into());
    }
    let linked = tx
        .execute(
            "UPDATE comic_visual_batch_members SET page_run_id=?,status='queued',updated_at=?,finished_at=NULL WHERE id=? AND batch_id=? AND production_page_id=? AND page_run_id=? AND status='failed'",
            params![child.id, now(), member_id, batch.id, page_id, parent.id],
        )
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    if linked != 1 {
        return Err("VISUAL_BATCH_RETRY_PARENT_CHANGED".into());
    }
    let advanced = tx
        .execute(
            "UPDATE comic_visual_batches SET status='running',safe_error_code=NULL,safe_user_message=NULL,finished_at=NULL,updated_at=? WHERE id=? AND status='failed'",
            params![now(), batch.id],
        )
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    if advanced != 1 {
        return Err("VISUAL_BATCH_RETRY_STATE_INVALID".into());
    }
    tx.execute(
        "INSERT INTO comic_visual_batch_command_receipts(command_name,idempotency_key,request_hash,batch_id,created_at) VALUES (?,?,?,?,?)",
        params![COMMAND_MEMBER_RETRY, input.idempotency_key, hash, batch.id, now()],
    )
    .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    let result = get_by_id(
        &tx,
        &input.project_id,
        &input.novel_work_id,
        &input.batch_id,
    )?
    .ok_or("VISUAL_BATCH_READ_FAILED")?;
    tx.commit()
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok((result, Some(child.id)))
}

fn ensure_member_retry_lease_available(tx: &Transaction<'_>, batch_id: &str) -> Result<(), String> {
    let lease: Option<i64> = tx
        .query_row(
            "SELECT lease_expires_at FROM comic_visual_batches WHERE id=?",
            params![batch_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?
        .flatten();
    if lease.is_some_and(|expires| expires > now()) {
        return Err("VISUAL_BATCH_RETRY_BUSY".into());
    }
    tx.execute(
        "UPDATE comic_visual_batches SET lease_owner=NULL,lease_expires_at=NULL,updated_at=? WHERE id=? AND (lease_expires_at IS NULL OR lease_expires_at<=?)",
        params![now(), batch_id, now()],
    )
    .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok(())
}

fn batch_member_dispatch_key(tx: &Transaction<'_>, member_id: &str) -> Result<String, String> {
    tx.query_row(
        "SELECT dispatch_key FROM comic_visual_batch_members WHERE id=?",
        params![member_id],
        |row| row.get(0),
    )
    .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())
}

fn queued_retried_child(
    conn: &Connection,
    batch_id: &str,
    member_id: &str,
    parent_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT run.id FROM comic_visual_batches batch JOIN comic_visual_batch_members member ON member.batch_id=batch.id JOIN comic_visual_page_runs run ON run.id=member.page_run_id WHERE batch.id=? AND batch.status='running' AND member.id=? AND member.ordinal=(SELECT MIN(ordinal) FROM comic_visual_batch_members WHERE batch_id=? AND status<>'candidate_ready') AND member.status='queued' AND run.status='queued' AND run.parent_run_id=? AND run.idempotency_key=member.dispatch_key || ':retry:' || ?",
        params![batch_id, member_id, batch_id, parent_id, parent_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())
}

enum ResumedMember {
    NewQueued(String),
    ExistingQueued(String),
    ExistingRunning,
    NoLinkedRun,
}

fn ensure_resume_lease_available(tx: &Transaction<'_>, batch_id: &str) -> Result<(), String> {
    let lease: Option<i64> = tx
        .query_row(
            "SELECT lease_expires_at FROM comic_visual_batches WHERE id=?",
            params![batch_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?
        .flatten();
    if lease.is_some_and(|expires| expires > now()) {
        return Err("VISUAL_BATCH_RESUME_BUSY".into());
    }
    tx.execute(
        "UPDATE comic_visual_batches SET lease_owner=NULL,lease_expires_at=NULL,updated_at=? WHERE id=? AND (lease_expires_at IS NULL OR lease_expires_at<=?)",
        params![now(), batch_id, now()],
    )
    .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok(())
}

fn queued_resumed_child(conn: &Connection, batch_id: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT run.id FROM comic_visual_batches batch JOIN comic_visual_batch_members member ON member.batch_id=batch.id JOIN comic_visual_page_runs run ON run.id=member.page_run_id JOIN comic_visual_page_runs parent ON parent.id=run.parent_run_id WHERE batch.id=? AND batch.status='running' AND member.ordinal=(SELECT MIN(ordinal) FROM comic_visual_batch_members WHERE batch_id=? AND status<>'candidate_ready') AND member.status='queued' AND run.status='queued' AND parent.status='blocked_config' AND parent.attempt_no=0 AND run.idempotency_key=member.dispatch_key || ':config-resume:' || parent.id",
        params![batch_id, batch_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())
}

fn resume_first_blocked_config_member_tx(
    tx: &Transaction<'_>,
    batch: &BatchRow,
    session: &str,
    provider_id: &str,
    model: &str,
) -> Result<ResumedMember, String> {
    let member: Option<(String, String, String, String, Option<String>)> = tx
        .query_row(
            "SELECT id,dispatch_key,manifest_id,production_page_id,page_run_id FROM comic_visual_batch_members WHERE batch_id=? AND status<>'candidate_ready' ORDER BY ordinal LIMIT 1",
            params![batch.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    let Some((member_id, dispatch_key, manifest_id, production_page_id, Some(parent_id))) = member
    else {
        return Ok(ResumedMember::NoLinkedRun);
    };
    let parent =
        comic_visual_render::get_run(tx, &batch.project_id, &batch.novel_work_id, &parent_id)?;
    if parent.manifest_id != manifest_id || parent.production_page_id != production_page_id {
        return Err("VISUAL_BATCH_CONFIG_PARENT_SCOPE_MISMATCH".into());
    }
    match parent.status.as_str() {
        "queued" => return Ok(ResumedMember::ExistingQueued(parent.id)),
        "running" => return Ok(ResumedMember::ExistingRunning),
        "needs_reconcile" => return Err("VISUAL_BATCH_RECONCILE_REQUIRED".into()),
        "blocked_config" => {}
        _ => return Err("VISUAL_BATCH_CONFIG_RESUME_STATE_INVALID".into()),
    }
    let child_key = format!("{dispatch_key}:config-resume:{}", parent.id);
    let child = comic_visual_render::resume_blocked_config_child_tx(
        tx,
        &parent,
        child_key,
        session,
        provider_id,
        model,
    )?;
    if child.status != "queued" {
        return Err("VISUAL_BATCH_CONFIG_RESUME_CHILD_STATE_INVALID".into());
    }
    let changed = tx
        .execute(
            "UPDATE comic_visual_batch_members SET page_run_id=?,status='queued',updated_at=?,finished_at=NULL WHERE id=? AND batch_id=? AND page_run_id=?",
            params![child.id, now(), member_id, batch.id, parent.id],
        )
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    if changed != 1 {
        let linked: Option<String> = tx
            .query_row(
                "SELECT page_run_id FROM comic_visual_batch_members WHERE id=? AND batch_id=?",
                params![member_id, batch.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?
            .flatten();
        if linked.as_deref() != Some(child.id.as_str()) {
            return Err("VISUAL_BATCH_CONFIG_MEMBER_LINK_CONFLICT".into());
        }
    }
    Ok(ResumedMember::NewQueued(child.id))
}

/// A completed text job invokes this after its immutable apply receipt is
/// committed.  Unauthorised/legacy jobs intentionally produce no batch work.
pub(crate) fn schedule_for_production_job(app: &tauri::AppHandle, job_id: &str) {
    let state = app.state::<DbState>();
    let batch = db::with_connection(&state, |conn| {
        conn.query_row(
            "SELECT id FROM comic_visual_batches WHERE production_job_id=?",
            params![job_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())
    });
    if let Ok(Some(id)) = batch {
        schedule_batch(app, &id);
    }
}

/// Renderer completion notification.  Startup recovery provides the same
/// transition if a process dies between result persistence and this callback.
pub(crate) fn schedule_for_page_run(app: &tauri::AppHandle, run_id: &str) {
    let state = app.state::<DbState>();
    let batch = db::with_connection(&state, |conn| {
        conn.query_row(
            "SELECT batch_id FROM comic_visual_batch_members WHERE page_run_id=?",
            params![run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())
    });
    if let Ok(Some(id)) = batch {
        schedule_batch(app, &id);
    }
}

pub(crate) fn recover_authorized_batches(
    app: &tauri::AppHandle,
    state: &DbState,
) -> Result<usize, String> {
    let ids = db::with_connection(state, |conn| {
        let mut stmt = conn.prepare("SELECT id FROM comic_visual_batches WHERE status IN ('authorized','waiting_text','preparing','running','blocked_config') ORDER BY created_at,id").map_err(|_|"VISUAL_BATCH_READ_FAILED".to_string())?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
        Ok(ids)
    })?;
    for id in &ids {
        schedule_batch(app, id);
    }
    Ok(ids.len())
}

fn schedule_batch(app: &tauri::AppHandle, batch_id: &str) {
    let app = app.clone();
    let batch_id = batch_id.to_owned();
    tauri::async_runtime::spawn(async move {
        drive_batch(app, batch_id).await;
    });
}

fn schedule_batch_after_lease(app: &tauri::AppHandle, batch_id: &str, expires_at: i64) {
    let inserted = BATCH_LEASE_WAKES
        .lock()
        .map(|mut batches| batches.insert(batch_id.to_owned()))
        .unwrap_or(false);
    if !inserted {
        return;
    }
    let app = app.clone();
    let batch_id = batch_id.to_owned();
    let delay_ms = (expires_at - now() + 1).max(1) as u64;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        if let Ok(mut batches) = BATCH_LEASE_WAKES.lock() {
            batches.remove(&batch_id);
        }
        schedule_batch(&app, &batch_id);
    });
}

enum LinkedRunRecoveryDecision {
    WaitUntil(i64),
    Reconciled,
    NoAction,
}

/// Resolves only an already-linked run after its observed lease deadline. A
/// page that might have reached the provider is never retried here: an
/// expired running row becomes `needs_reconcile`, while a heartbeat extension
/// simply returns a later deadline for another wake.
fn reconcile_linked_run_lease(
    conn: &Connection,
    batch_id: &str,
    run_id: &str,
) -> Result<LinkedRunRecoveryDecision, String> {
    let row: Option<(String, Option<i64>)> = conn
        .query_row(
            "SELECT run.status,run.lease_expires_at FROM comic_visual_batch_members member JOIN comic_visual_page_runs run ON run.id=member.page_run_id WHERE member.batch_id=? AND run.id=?",
            params![batch_id,run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    let Some((status, lease)) = row else {
        return Ok(LinkedRunRecoveryDecision::NoAction);
    };
    if status != "running" {
        return Ok(LinkedRunRecoveryDecision::NoAction);
    }
    if let Some(expires) = lease.filter(|expires| *expires > now()) {
        return Ok(LinkedRunRecoveryDecision::WaitUntil(expires));
    }
    let changed = conn
        .execute(
            "UPDATE comic_visual_page_runs SET status='needs_reconcile',lease_expires_at=NULL,owner_app_session_id=NULL,finished_at=?,failure_json=? WHERE id=? AND status='running' AND (lease_expires_at IS NULL OR lease_expires_at<=?)",
            params![now(),json!({"code":"VISUAL_RUN_LEASE_EXPIRED","message":"页面生成租约已过期，无法确认远端结果。","retryable":false,"observedAt":now()}).to_string(),run_id,now()],
        )
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    if changed == 1 {
        if let Some(batch) = read_batch(conn, batch_id)? {
            sync_member_outcomes(conn, &batch)?;
        }
        Ok(LinkedRunRecoveryDecision::Reconciled)
    } else {
        // A late candidate/terminal result won the CAS. Do not overwrite it.
        Ok(LinkedRunRecoveryDecision::NoAction)
    }
}

fn schedule_linked_run_recovery_wake(app: &tauri::AppHandle, batch_id: &str) {
    let state = app.state::<DbState>();
    let linked = db::with_connection(&state, |conn| {
        conn.query_row(
            "SELECT run.id,run.lease_expires_at FROM comic_visual_batch_members member JOIN comic_visual_page_runs run ON run.id=member.page_run_id WHERE member.batch_id=? AND run.status='running' ORDER BY member.ordinal LIMIT 1",
            params![batch_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())
    })
    .ok()
    .flatten();
    let Some((run_id, Some(expires_at))) = linked else {
        return;
    };
    let inserted = LINKED_RUN_RECOVERY_WAKES
        .lock()
        .map(|mut runs| runs.insert(run_id.clone()))
        .unwrap_or(false);
    if !inserted {
        return;
    }
    let app = app.clone();
    let batch_id = batch_id.to_owned();
    tauri::async_runtime::spawn(async move {
        let delay_ms = (expires_at - now() + 1).max(1) as u64;
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        if let Ok(mut runs) = LINKED_RUN_RECOVERY_WAKES.lock() {
            runs.remove(&run_id);
        }
        let state = app.state::<DbState>();
        let decision = db::with_connection(&state, |conn| {
            reconcile_linked_run_lease(conn, &batch_id, &run_id)
        });
        match decision {
            Ok(LinkedRunRecoveryDecision::WaitUntil(expires)) => {
                // Re-read before arming so a concurrent heartbeat is never
                // shortened to the deadline we just observed.
                if expires > now() {
                    schedule_linked_run_recovery_wake(&app, &batch_id)
                } else {
                    schedule_batch(&app, &batch_id)
                }
            }
            Ok(LinkedRunRecoveryDecision::Reconciled) => schedule_batch(&app, &batch_id),
            Ok(LinkedRunRecoveryDecision::NoAction) | Err(_) => {}
        }
    });
}

async fn drive_batch(app: tauri::AppHandle, batch_id: String) {
    let state = app.state::<DbState>();
    let claim_token = new_id("comic_visual_batch_lease");
    let claimed = db::with_connection(&state, |conn| claim_batch(conn, &batch_id, &claim_token));
    if matches!(claimed, Ok(false)) {
        let expires = db::with_connection(&state, |conn| {
            conn.query_row(
                "SELECT lease_expires_at FROM comic_visual_batches WHERE id=?",
                params![batch_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map(|value| value.flatten())
            .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())
        })
        .ok()
        .flatten();
        if let Some(expires) = expires.filter(|value| *value > now()) {
            schedule_batch_after_lease(&app, &batch_id, expires);
        }
    }
    if !matches!(claimed, Ok(true)) {
        return;
    }
    let prepared = db::with_connection(&state, |conn| prepare_if_text_complete(conn, &batch_id));
    if let Err(code) = prepared {
        let _ = db::with_connection(&state, |conn| {
            record_driver_error(conn, &batch_id, &claim_token, &code)
        });
        let _ = db::with_connection(&state, |conn| {
            release_batch_claim(conn, &batch_id, &claim_token)
        });
        return;
    }
    let cfg = match app.state::<AppState>().cfg.read() {
        Ok(value) => value.clone(),
        Err(_) => {
            let _ = db::with_connection(&state, |conn| {
                set_batch_status(
                    conn,
                    &batch_id,
                    "blocked_config",
                    Some(("CONFIG_UNAVAILABLE", "图像配置暂时不可读取，已暂停生成。")),
                )
            });
            let _ = db::with_connection(&state, |conn| {
                release_batch_claim(conn, &batch_id, &claim_token)
            });
            return;
        }
    };
    let registry = app.state::<AppState>().registry.active().id().to_owned();
    let next = db::with_connection(&state, |conn| {
        next_page_action(conn, &batch_id, cfg.status().image_ready, &registry)
    });
    let action = match next {
        Ok(Some(action)) => action,
        Ok(None) => {
            // A linked `running` page has no next action by design. Keep a
            // single deadline wake so a crash/restart cannot leave the batch
            // silently running forever once its renderer lease expires.
            schedule_linked_run_recovery_wake(&app, &batch_id);
            let _ = db::with_connection(&state, |conn| {
                release_batch_claim(conn, &batch_id, &claim_token)
            });
            return;
        }
        Err(code) => {
            let _ = db::with_connection(&state, |conn| {
                record_driver_error(conn, &batch_id, &claim_token, &code)
            });
            let _ = db::with_connection(&state, |conn| {
                release_batch_claim(conn, &batch_id, &claim_token)
            });
            return;
        }
    };
    let run = db::with_connection(&state, |conn| {
        comic_visual_render::start_inner(
            conn,
            action.input,
            false,
            true,
            state.app_session_id(),
            &action.provider_id,
            &action.model,
            1,
            None,
        )
    });
    let run = match run {
        Ok(run) => run,
        Err(code) => {
            let _ = db::with_connection(&state, |conn| {
                record_driver_error(conn, &batch_id, &claim_token, &code)
            });
            let _ = db::with_connection(&state, |conn| {
                release_batch_claim(conn, &batch_id, &claim_token)
            });
            return;
        }
    };
    let linked = db::with_connection(&state, |conn| {
        link_member_run(
            conn,
            &batch_id,
            &claim_token,
            &action.member_id,
            &action.dispatch_key,
            &run.id,
        )
    });
    if let Err(code) = linked {
        let _ = db::with_connection(&state, |conn| {
            record_driver_error(conn, &batch_id, &claim_token, &code)
        });
        let _ = db::with_connection(&state, |conn| {
            release_batch_claim(conn, &batch_id, &claim_token)
        });
        return;
    }
    let _ = db::with_connection(&state, |conn| {
        release_batch_claim(conn, &batch_id, &claim_token)
    });
    if run.status == "queued" {
        comic_visual_render::schedule_dispatch(app, run.id);
    }
}

struct StartAction {
    member_id: String,
    dispatch_key: String,
    provider_id: String,
    model: String,
    input: ComicVisualPageRunStartInput,
}

fn prepare_if_text_complete(conn: &Connection, batch_id: &str) -> Result<(), String> {
    let batch = read_batch(conn, batch_id)?.ok_or("VISUAL_BATCH_NOT_FOUND")?;
    let job = load_job_scope(
        conn,
        &batch.project_id,
        &batch.novel_work_id,
        &batch.production_job_id,
    )?;
    if job.novel_chapter_id != batch.novel_chapter_id
        || job.source_revision_id != batch.source_revision_id
    {
        return set_batch_status(
            conn,
            batch_id,
            "blocked_stale",
            Some((
                "VISUAL_BATCH_SOURCE_MISMATCH",
                "漫画生成授权的章节来源不再匹配，已暂停生成。",
            )),
        );
    }
    if job.status != "succeeded" {
        set_batch_status(
            conn,
            batch_id,
            "waiting_text",
            Some((
                "VISUAL_BATCH_WAITING_TEXT",
                "正在等待文字生产与漫画结构完成。",
            )),
        )?;
        return Ok(());
    }
    let (Some(adaptation), Some(operation)) = (
        job.default_adaptation_id.clone(),
        job.apply_operation_id.clone(),
    ) else {
        return set_batch_status(
            conn,
            batch_id,
            "blocked_stale",
            Some((
                "VISUAL_BATCH_APPLY_MISSING",
                "文字生产缺少已确认的漫画生产回执。",
            )),
        );
    };
    if let Some(fixed) = &batch.comic_adaptation_id {
        if fixed != &adaptation {
            return set_batch_status(
                conn,
                batch_id,
                "blocked_stale",
                Some((
                    "VISUAL_BATCH_ADAPTATION_DRIFT",
                    "漫画改编来源已变化，已暂停生成。",
                )),
            );
        }
    }
    if let Some(fixed) = &batch.apply_operation_id {
        if fixed != &operation {
            return set_batch_status(
                conn,
                batch_id,
                "blocked_stale",
                Some((
                    "VISUAL_BATCH_APPLY_DRIFT",
                    "漫画生产回执已变化，已暂停生成。",
                )),
            );
        }
    }
    if batch.comic_adaptation_id.is_none() || batch.apply_operation_id.is_none() {
        conn.execute("UPDATE comic_visual_batches SET comic_adaptation_id=?,apply_operation_id=?,status='preparing',updated_at=? WHERE id=? AND comic_adaptation_id IS NULL AND apply_operation_id IS NULL",params![adaptation,operation,now(),batch_id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
    }
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_batch_members WHERE batch_id=?",
            params![batch_id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    if count > 0 {
        return Ok(());
    }
    let chapters = ordered_chapters_from_receipt(conn, &batch, &adaptation, &operation)?;
    let mut pending = Vec::new();
    for (chapter_id, stable_key) in chapters {
        let manifest = comic_visual::prepare_inner(
            conn,
            ComicVisualManifestPrepareInput {
                project_id: batch.project_id.clone(),
                novel_work_id: batch.novel_work_id.clone(),
                comic_adaptation_id: adaptation.clone(),
                apply_operation_id: operation.clone(),
                production_chapter_id: chapter_id.clone(),
                idempotency_key: format!("{}:manifest:{}", batch.id, chapter_id),
            },
        )?;
        if manifest.freshness != "ready" {
            return set_batch_status(
                conn,
                batch_id,
                "blocked_stale",
                Some((
                    "VISUAL_BATCH_MANIFEST_STALE",
                    "漫画生产结构已变更，已暂停生成。",
                )),
            );
        }
        let mut pages = manifest
            .manifest
            .get("pages")
            .and_then(Value::as_array)
            .ok_or("VISUAL_BATCH_MANIFEST_INVALID")?
            .iter()
            .collect::<Vec<_>>();
        pages.sort_by_key(|page| {
            page.get("pageNo")
                .and_then(Value::as_i64)
                .unwrap_or_default()
        });
        let mut previous = None;
        for page in pages {
            let page_id = page
                .get("productionPageId")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or("VISUAL_BATCH_MANIFEST_INVALID")?;
            let page_no = page
                .get("pageNo")
                .and_then(Value::as_i64)
                .filter(|v| *v > 0)
                .ok_or("VISUAL_BATCH_MANIFEST_INVALID")?;
            let key = page
                .get("stableKey")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or("VISUAL_BATCH_MANIFEST_INVALID")?;
            pending.push((
                manifest.id.clone(),
                chapter_id.clone(),
                stable_key.clone(),
                page_id.to_owned(),
                page_no,
                key.to_owned(),
                previous,
            ));
            previous = Some(pending.len() - 1);
        }
    }
    if pending.is_empty() {
        return set_batch_status(
            conn,
            batch_id,
            "blocked_stale",
            Some(("VISUAL_BATCH_EMPTY", "漫画生产回执没有可生成的页面。")),
        );
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    let existing: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_batch_members WHERE batch_id=?",
            params![batch_id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    if existing == 0 {
        let mut ids = Vec::with_capacity(pending.len());
        for _ in &pending {
            ids.push(new_id("comic_visual_batch_member"));
        }
        for (index, (manifest_id, chapter_id, _chapter_key, page_id, page_no, key, previous)) in
            pending.iter().enumerate()
        {
            tx.execute("INSERT INTO comic_visual_batch_members (id,batch_id,ordinal,manifest_id,production_chapter_id,production_page_id,page_no,page_stable_key,predecessor_member_id,page_run_id,dispatch_key,status,created_at,updated_at,finished_at) VALUES (?,?,?,?,?,?,?,?,?,NULL,?,'pending',?,?,NULL)",params![ids[index],batch_id,(index+1) as i64,manifest_id,chapter_id,page_id,page_no,key,previous.and_then(|i|ids.get(i)).cloned(),format!("{}:{}:{}",batch_id,manifest_id,page_id),now(),now()]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
        }
        tx.execute("UPDATE comic_visual_batches SET status='running',total_members=?,completed_members=0,current_member_ordinal=1,safe_error_code=NULL,safe_user_message=NULL,updated_at=? WHERE id=?",params![ids.len() as i64,now(),batch_id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
    }
    tx.commit()
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok(())
}

fn ordered_chapters_from_receipt(
    conn: &Connection,
    batch: &BatchRow,
    adaptation: &str,
    operation: &str,
) -> Result<Vec<(String, String)>, String> {
    // The receipt may apply only a subset of a multi-chapter plan. Its
    // production chapters point at the exact revision that supplied their
    // stable keys; do not substitute the current active head or sort by IDs.
    let mut stmt=conn.prepare("SELECT map.stable_key,map.entity_id,revision.id,revision.body_json FROM analysis_apply_receipt_entity_maps map JOIN comic_production_chapters chapter ON chapter.id=map.entity_id JOIN comic_planning_chapters planning ON planning.id=chapter.comic_planning_chapter_id JOIN comic_adaptation_plan_heads head ON head.id=planning.comic_adaptation_plan_head_id JOIN analysis_artifact_revisions revision ON revision.id=head.comic_chapter_plan_revision_id JOIN comic_adaptations adaptation ON adaptation.id=chapter.comic_adaptation_id WHERE map.analysis_apply_operation_id=? AND map.entity_kind='production_chapter' AND chapter.apply_operation_id=? AND chapter.comic_adaptation_id=? AND adaptation.project_id=? AND adaptation.novel_work_id=?").map_err(|_|"VISUAL_BATCH_READ_FAILED".to_string())?;
    let rows = stmt
        .query_map(
            params![
                operation,
                operation,
                adaptation,
                batch.project_id,
                batch.novel_work_id
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    let Some((_, _, revision_id, raw)) = rows.first() else {
        return Err("VISUAL_BATCH_CHAPTER_ORDER_MISSING".into());
    };
    if rows
        .iter()
        .any(|(_, _, id, body)| id != revision_id || body != raw)
    {
        return Err("VISUAL_BATCH_CHAPTER_ORDER_MISMATCH".into());
    }
    let plan: Value =
        serde_json::from_str(raw).map_err(|_| "VISUAL_BATCH_CHAPTER_ORDER_INVALID".to_string())?;
    let keys = plan
        .get("chapters")
        .and_then(Value::as_array)
        .ok_or("VISUAL_BATCH_CHAPTER_ORDER_INVALID")?
        .iter()
        .map(|v| {
            v.get("stableKey")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .ok_or("VISUAL_BATCH_CHAPTER_ORDER_INVALID".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let maps = rows
        .into_iter()
        .map(|(key, id, _, _)| (key, id))
        .collect::<std::collections::HashMap<_, _>>();
    if maps.is_empty() || maps.keys().any(|key| !keys.contains(key)) {
        return Err("VISUAL_BATCH_CHAPTER_ORDER_MISMATCH".into());
    }
    let ordered = keys
        .into_iter()
        .filter_map(|key| maps.get(&key).cloned().map(|id| (id, key)))
        .collect::<Vec<_>>();
    if ordered.len() != maps.len() {
        return Err("VISUAL_BATCH_CHAPTER_ORDER_MISMATCH".into());
    }
    Ok(ordered)
}

fn next_page_action(
    conn: &Connection,
    batch_id: &str,
    image_ready: bool,
    active_provider: &str,
) -> Result<Option<StartAction>, String> {
    let batch = read_batch(conn, batch_id)?.ok_or("VISUAL_BATCH_NOT_FOUND")?;
    reconcile_unlinked_members(conn, &batch)?;
    sync_member_outcomes(conn, &batch)?;
    let batch = read_batch(conn, batch_id)?.ok_or("VISUAL_BATCH_NOT_FOUND")?;
    if !matches!(
        batch.status.as_str(),
        "running" | "authorized" | "preparing" | "blocked_config"
    ) {
        return Ok(None);
    }
    let Some(model) = batch
        .authorization
        .model
        .clone()
        .or(batch.resolved_model.clone())
    else {
        set_batch_status(
            conn,
            batch_id,
            "blocked_config",
            Some((
                "VISUAL_BATCH_MODEL_REQUIRED",
                "需要补齐图像模型后继续生成。",
            )),
        )?;
        return Ok(None);
    };
    if !image_ready || batch.authorization.provider_id != active_provider {
        set_batch_status(
            conn,
            batch_id,
            "blocked_config",
            Some((
                "VISUAL_BATCH_CONFIG_REQUIRED",
                "需要补齐已授权图像服务配置后继续生成。",
            )),
        )?;
        return Ok(None);
    }
    let member:Option<(String,String,String,String,String,String)>=conn.query_row("SELECT id,manifest_id,production_page_id,dispatch_key,page_run_id,status FROM comic_visual_batch_members WHERE batch_id=? AND status NOT IN ('candidate_ready') ORDER BY ordinal LIMIT 1",params![batch_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get::<_,Option<String>>(4)?.unwrap_or_default(),row.get(5)?))).optional().map_err(|_|"VISUAL_BATCH_READ_FAILED".to_string())?;
    let Some((member_id, manifest_id, page_id, dispatch_key, run_id, status)) = member else {
        set_batch_status(conn, batch_id, "candidate_ready", None)?;
        return Ok(None);
    };
    if !run_id.is_empty() || matches!(status.as_str(), "queued" | "running") {
        return Ok(None);
    }
    let manifest =
        comic_visual::get_by_id(conn, &batch.project_id, &batch.novel_work_id, &manifest_id)?;
    if manifest.freshness != "ready" {
        set_batch_status(
            conn,
            batch_id,
            "blocked_stale",
            Some((
                "VISUAL_BATCH_MANIFEST_STALE",
                "漫画生产结构已变化，已暂停生成。",
            )),
        )?;
        return Ok(None);
    }
    let manifest_fingerprint = manifest.manifest_fingerprint;
    let request_json = json!({"schemaVersion":COMPILER_CONTRACT,"manifestFingerprint":manifest_fingerprint,"productionPageId":page_id});
    Ok(Some(StartAction {
        member_id,
        dispatch_key: dispatch_key.clone(),
        provider_id: batch.authorization.provider_id.clone(),
        model,
        input: ComicVisualPageRunStartInput {
            project_id: batch.project_id,
            novel_work_id: batch.novel_work_id,
            manifest_id: manifest.id,
            production_page_id: page_id,
            manifest_fingerprint,
            compiler_contract_version: COMPILER_CONTRACT.into(),
            request_json,
            reference_snapshot_json: json!({}),
            idempotency_key: dispatch_key,
        },
    }))
}

fn link_member_run(
    conn: &Connection,
    batch_id: &str,
    claim_token: &str,
    member_id: &str,
    dispatch_key: &str,
    run_id: &str,
) -> Result<(), String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    let found: Option<String> = tx
        .query_row(
            "SELECT id FROM comic_visual_page_runs WHERE id=? AND idempotency_key=?",
            params![run_id, dispatch_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    if found.is_none() {
        return Err("VISUAL_BATCH_RUN_LINK_MISMATCH".into());
    }
    let changed = tx.execute("UPDATE comic_visual_batch_members SET page_run_id=?,status='queued',updated_at=? WHERE id=? AND (page_run_id IS NULL OR page_run_id=?) AND EXISTS(SELECT 1 FROM comic_visual_batches batch WHERE batch.id=? AND batch.lease_owner=? AND batch.lease_expires_at>=?)",params![run_id,now(),member_id,run_id,batch_id,claim_token,now()]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
    if changed != 1 {
        return Err("VISUAL_BATCH_LEASE_LOST".into());
    }
    tx.commit()
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok(())
}

/// Repairs the only intentional cross-transaction gap: renderer start is
/// durable before the member can point to it.  The deterministic dispatch key
/// makes recovery a lookup, never a new paid attempt.
fn reconcile_unlinked_members(conn: &Connection, batch: &BatchRow) -> Result<(), String> {
    let mut stmt = conn.prepare("SELECT id,dispatch_key FROM comic_visual_batch_members WHERE batch_id=? AND page_run_id IS NULL ORDER BY ordinal")
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    let members = stmt
        .query_map(params![batch.id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    for (member_id, dispatch_key) in members {
        let run: Option<(String, String)> = conn.query_row(
            "SELECT id,status FROM comic_visual_page_runs WHERE idempotency_key=? AND project_id=? AND novel_work_id=?",
            params![dispatch_key, batch.project_id, batch.novel_work_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
        if let Some((run_id, status)) = run {
            conn.execute("UPDATE comic_visual_batch_members SET page_run_id=?,status=?,updated_at=? WHERE id=? AND page_run_id IS NULL",params![run_id,status,now(),member_id])
                .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
        }
    }
    Ok(())
}

fn sync_member_outcomes(conn: &Connection, batch: &BatchRow) -> Result<(), String> {
    let mut stmt=conn.prepare("SELECT member.id,member.status,run.status FROM comic_visual_batch_members member LEFT JOIN comic_visual_page_runs run ON run.id=member.page_run_id WHERE member.batch_id=? ORDER BY member.ordinal").map_err(|_|"VISUAL_BATCH_READ_FAILED".to_string())?;
    let rows = stmt
        .query_map(params![batch.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    // Once a batch has an unknown or stale outcome, a later historical
    // failure must not downgrade it back to ordinary failed/retryable state.
    let mut terminal_batch = matches!(batch.status.as_str(), "needs_reconcile" | "blocked_stale");
    for (id, _old, status) in rows {
        let Some(status) = status else { continue };
        match status.as_str() {
            "candidate_ready" => {
                conn.execute("UPDATE comic_visual_batch_members SET status='candidate_ready',finished_at=?,updated_at=? WHERE id=? AND status<>'candidate_ready'",params![now(),now(),id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
            }
            "queued" | "running" => {
                conn.execute(
                    "UPDATE comic_visual_batch_members SET status=?,updated_at=? WHERE id=?",
                    params![status, now(), id],
                )
                .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
            }
            "needs_reconcile" => {
                conn.execute("UPDATE comic_visual_batch_members SET status='needs_reconcile',finished_at=?,updated_at=? WHERE id=?",params![now(),now(),id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
                if !terminal_batch {
                    set_batch_status(
                        conn,
                        &batch.id,
                        "needs_reconcile",
                        Some((
                            "VISUAL_BATCH_RECONCILE_REQUIRED",
                            "某页图像结果未知，已暂停后续生成。",
                        )),
                    )?;
                    terminal_batch = true;
                }
            }
            "blocked_config" => {
                conn.execute("UPDATE comic_visual_batch_members SET status='blocked_config',finished_at=NULL,updated_at=? WHERE id=?",params![now(),id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
                // A stale batch must never be made resumable merely because
                // its old linked run still records a local config pause.
                if !terminal_batch {
                    set_batch_status(
                        conn,
                        &batch.id,
                        "blocked_config",
                        Some((
                            "VISUAL_BATCH_CONFIG_REQUIRED",
                            "某页图像配置不足，已暂停后续生成。",
                        )),
                    )?;
                }
            }
            "failed" => {
                conn.execute("UPDATE comic_visual_batch_members SET status='failed',finished_at=?,updated_at=? WHERE id=?",params![now(),now(),id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
                if !terminal_batch {
                    set_batch_status(
                        conn,
                        &batch.id,
                        "failed",
                        Some(("VISUAL_BATCH_PAGE_FAILED", "某页生成失败，已保留已有页面。")),
                    )?;
                }
            }
            _ => return Err("VISUAL_BATCH_RUN_STATUS_INVALID".into()),
        }
    }
    let completed:i64=conn.query_row("SELECT COUNT(*) FROM comic_visual_batch_members WHERE batch_id=? AND status='candidate_ready'",params![batch.id],|row|row.get(0)).map_err(|_|"VISUAL_BATCH_READ_FAILED".to_string())?;
    conn.execute("UPDATE comic_visual_batches SET completed_members=?,current_member_ordinal=(SELECT ordinal FROM comic_visual_batch_members WHERE batch_id=? AND status<>'candidate_ready' ORDER BY ordinal LIMIT 1),updated_at=? WHERE id=?",params![completed,batch.id,now(),batch.id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok(())
}

fn set_batch_status(
    conn: &Connection,
    id: &str,
    status: &str,
    error: Option<(&str, &str)>,
) -> Result<(), String> {
    conn.execute("UPDATE comic_visual_batches SET status=?,safe_error_code=?,safe_user_message=?,updated_at=?,finished_at=CASE WHEN ? IN ('candidate_ready','failed','needs_reconcile','blocked_stale') THEN ? ELSE finished_at END WHERE id=?",params![status,error.map(|v|v.0),error.map(|v|v.1),now(),status,now(),id]).map_err(|_|"VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok(())
}

fn claim_batch(conn: &Connection, id: &str, token: &str) -> Result<bool, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    let changed = tx.execute("UPDATE comic_visual_batches SET lease_owner=?,lease_expires_at=?,updated_at=? WHERE id=? AND status IN ('authorized','waiting_text','preparing','running','blocked_config') AND (lease_expires_at IS NULL OR lease_expires_at<?)",params![token,now()+BATCH_LEASE_MS,now(),id,now()])
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    tx.commit()
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok(changed == 1)
}

fn release_batch_claim(conn: &Connection, id: &str, token: &str) -> Result<(), String> {
    conn.execute("UPDATE comic_visual_batches SET lease_owner=NULL,lease_expires_at=NULL,updated_at=? WHERE id=? AND lease_owner=?",params![now(),id,token])
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok(())
}

fn record_driver_error(conn: &Connection, id: &str, token: &str, code: &str) -> Result<(), String> {
    let current: Option<String> = conn
        .query_row(
            "SELECT status FROM comic_visual_batches WHERE id=? AND lease_owner=? AND lease_expires_at>=?",
            params![id, token, now()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    let Some(current) = current else {
        return Ok(());
    };
    if current == "needs_reconcile" {
        return Ok(());
    }
    let status = if code.contains("STALE") || code.contains("DRIFT") || code.contains("ORDER") {
        "blocked_stale"
    } else {
        "failed"
    };
    conn.execute("UPDATE comic_visual_batches SET status=?,safe_error_code=?,safe_user_message=?,updated_at=?,finished_at=CASE WHEN ? IN ('failed','blocked_stale') THEN ? ELSE finished_at END WHERE id=? AND lease_owner=? AND lease_expires_at>=?",params![status,code,"漫画页后台接续未能安全继续，已有结果已保留。",now(),status,now(),id,token,now()])
        .map_err(|_| "VISUAL_BATCH_WRITE_FAILED".to_string())?;
    Ok(())
}

fn read_batch(conn: &Connection, id: &str) -> Result<Option<BatchRow>, String> {
    let raw:Option<(String,String,String,String,String,String,Option<String>,Option<String>,String,Option<String>,String)>=conn.query_row("SELECT id,project_id,novel_work_id,novel_chapter_id,source_revision_id,production_job_id,comic_adaptation_id,apply_operation_id,authorization_json,resolved_model,status FROM comic_visual_batches WHERE id=?",params![id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?))).optional().map_err(|_|"VISUAL_BATCH_READ_FAILED".to_string())?;
    raw.map(
        |(
            id,
            project_id,
            novel_work_id,
            novel_chapter_id,
            source_revision_id,
            production_job_id,
            comic_adaptation_id,
            apply_operation_id,
            authorization,
            resolved_model,
            status,
        )| {
            Ok(BatchRow {
                id,
                project_id,
                novel_work_id,
                novel_chapter_id,
                source_revision_id,
                production_job_id,
                comic_adaptation_id,
                apply_operation_id,
                authorization: serde_json::from_str(&authorization)
                    .map_err(|_| "VISUAL_AUTHORIZATION_INVALID")?,
                resolved_model,
                status,
            })
        },
    )
    .transpose()
}

fn get_by_id(
    conn: &Connection,
    project_id: &str,
    work_id: &str,
    id: &str,
) -> Result<Option<ComicVisualBatch>, String> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM novel_works WHERE id=? AND project_id=?)",
            params![work_id, project_id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    if !exists {
        return Err("VISUAL_BATCH_SCOPE_MISMATCH".into());
    }
    let row:Option<(String,String,String,String,String,String,String,i64,i64,Option<i64>,Option<String>,Option<String>,i64,i64,Option<i64>)>=conn.query_row("SELECT id,project_id,novel_work_id,novel_chapter_id,source_revision_id,production_job_id,output_target,total_members,completed_members,current_member_ordinal,safe_error_code,safe_user_message,created_at,updated_at,finished_at FROM comic_visual_batches WHERE id=? AND project_id=? AND novel_work_id=?",params![id,project_id,work_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?,r.get(12)?,r.get(13)?,r.get(14)?))).optional().map_err(|_|"VISUAL_BATCH_READ_FAILED".to_string())?;
    let Some((
        id,
        project_id,
        novel_work_id,
        novel_chapter_id,
        source_revision_id,
        production_job_id,
        output_target,
        total_members,
        completed_members,
        current_member_ordinal,
        safe_error_code,
        safe_user_message,
        created_at,
        updated_at,
        finished_at,
    )) = row
    else {
        return Ok(None);
    };
    let status: String = conn
        .query_row(
            "SELECT status FROM comic_visual_batches WHERE id=?",
            params![id],
            |r| r.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    let mut stmt=conn.prepare("SELECT member.id,member.ordinal,member.manifest_id,member.production_chapter_id,member.production_page_id,member.page_no,member.page_stable_key,member.status,member.page_run_id,run.status FROM comic_visual_batch_members member LEFT JOIN comic_visual_page_runs run ON run.id=member.page_run_id WHERE member.batch_id=? ORDER BY member.ordinal").map_err(|_|"VISUAL_BATCH_READ_FAILED".to_string())?;
    let raw_members = stmt
        .query_map(params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "VISUAL_BATCH_READ_FAILED".to_string())?;
    // Old v18 rows can have a durable terminal run while member.status still
    // says queued/running.  Read projection is authoritative for UI retry
    // eligibility and makes no dispatch or paid mutation.  A stale batch is
    // stronger than a historical failed child and remains visibly blocked.
    let members = raw_members
        .into_iter()
        .map(
            |(
                member_id,
                ordinal,
                manifest_id,
                production_chapter_id,
                production_page_id,
                page_no,
                page_stable_key,
                stored_status,
                page_run_id,
                run_status,
            )| {
                let is_current = current_member_ordinal == Some(ordinal);
                let status = if stored_status == "candidate_ready" {
                    stored_status
                } else if stored_status == "blocked_stale"
                    || (status == "blocked_stale" && is_current)
                {
                    "blocked_stale".to_owned()
                } else if let Some(run_status @ ("failed" | "needs_reconcile" | "blocked_config")) =
                    run_status.as_deref()
                {
                    run_status.to_owned()
                } else if status == "needs_reconcile" && is_current {
                    "needs_reconcile".to_owned()
                } else {
                    stored_status
                };
                ComicVisualBatchMember {
                    member_id,
                    ordinal,
                    manifest_id,
                    production_chapter_id,
                    production_page_id,
                    page_no,
                    page_stable_key,
                    status,
                    page_run_id,
                }
            },
        )
        .collect::<Vec<_>>();
    Ok(Some(ComicVisualBatch {
        id,
        project_id,
        novel_work_id,
        novel_chapter_id,
        source_revision_id,
        production_job_id,
        output_target,
        status,
        total_members,
        completed_members,
        current_member_ordinal,
        safe_error_code,
        safe_user_message,
        created_at,
        updated_at,
        finished_at,
        members,
    }))
}

fn get_by_job(
    conn: &Connection,
    project_id: &str,
    work_id: &str,
    job_id: &str,
) -> Result<Option<ComicVisualBatch>, String> {
    let id:Option<String>=conn.query_row("SELECT id FROM comic_visual_batches WHERE production_job_id=? AND project_id=? AND novel_work_id=?",params![job_id,project_id,work_id],|row|row.get(0)).optional().map_err(|_|"VISUAL_BATCH_READ_FAILED".to_string())?;
    id.map(|id| get_by_id(conn, project_id, work_id, &id))
        .transpose()
        .map(Option::flatten)
}

fn load_job_scope(
    conn: &Connection,
    project_id: &str,
    work_id: &str,
    job_id: &str,
) -> Result<NovelProductionJob, String> {
    // The public job accessor owns this mapping and is intentionally reused
    // rather than duplicating partial project/work checks in the batch layer.
    crate::novel::production_job_for_visual(conn, project_id, work_id, job_id)
}

#[cfg(test)]
mod integration_tests;

#[cfg(test)]
pub(crate) use integration_tests::assert_real_authorized_two_page_batch;
