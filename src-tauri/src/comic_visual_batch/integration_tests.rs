//! Batch assertions exercised from the migrated, formally-applied fixture in
//! `novel_adaptation`.  This module owns no shadow schema and never calls a
//! provider: the renderer test boundary writes an isolated 1x1 PNG instead.

use std::path::Path;

use rusqlite::{params, Connection};

use crate::comic_visual_render;
use crate::novel::NovelProductionJob;

use super::*;

pub(crate) fn assert_real_authorized_two_page_batch(
    conn: &Connection,
    job: &NovelProductionJob,
    authorization: &serde_json::Value,
    output_root: &Path,
) -> Result<(), String> {
    if get_by_job(conn, &job.project_id, &job.novel_work_id, &job.id)?.is_some() {
        return Err("VISUAL_BATCH_LEGACY_JOB_WAS_IMPLICITLY_AUTHORIZED".into());
    }
    crate::novel::test_authorize_succeeded_visual_job(conn, job, authorization)?;

    let mut batch = prepare_real_batch(conn, job)?;
    if batch.members.len() != 2 || batch.total_members != 2 || batch.status != "running" {
        return Err("VISUAL_BATCH_TWO_PAGE_PREPARE_FAILED".into());
    }
    if get_by_id(conn, "other-project", "other-work", &batch.id)
        .map(|_| ())
        .unwrap_err()
        != "VISUAL_BATCH_SCOPE_MISMATCH"
    {
        return Err("VISUAL_BATCH_FOREIGN_SCOPE_ACCEPTED".into());
    }

    // A missing image config pauses this exact batch; a later valid config is
    // allowed to continue without a new authorization or page request.
    if next_page_action(conn, &batch.id, false, "visual-test-provider")?.is_some() {
        return Err("VISUAL_BATCH_CONFIG_PAUSE_STARTED_A_PAGE".into());
    }
    batch = get_by_id(conn, &job.project_id, &job.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_MISSING_AFTER_CONFIG_PAUSE")?;
    if batch.status != "blocked_config" {
        return Err("VISUAL_BATCH_CONFIG_PAUSE_MISSING".into());
    }
    // A stale upstream plan must be detected before a run is linked.  Once a
    // page is already durably queued/running, next_page_action deliberately
    // observes that run first and must not overwrite its reconciliation state.
    assert_stale_manifest_pauses_current_batch(conn, &batch)
        .map_err(|code| format!("VISUAL_BATCH_TEST_STALE:{code}"))?;

    // Simulate the only intentional crash gap: renderer start commits, then
    // the process dies before the member points to the durable run.
    let (first, first_input) = start_without_link(conn, &batch.id)
        .map_err(|code| format!("VISUAL_BATCH_TEST_START_UNLINKED:{code}"))?;
    let first_key = first_input.idempotency_key.clone();
    let same = comic_visual_render::start_inner(
        conn,
        first_input,
        false,
        true,
        "visual-batch-test-session",
        "visual-test-provider",
        "visual-test-model",
        1,
        None,
    )?;
    if same.id != first.id || count_runs_for_key(conn, &first_key)? != 1 {
        return Err("VISUAL_BATCH_PAGE_IDEMPOTENCY_FAILED".into());
    }
    let row = read_batch(conn, &batch.id)?.ok_or("VISUAL_BATCH_MISSING")?;
    reconcile_unlinked_members(conn, &row)?;
    batch = get_by_id(conn, &job.project_id, &job.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_MISSING_AFTER_LINK_RECOVERY")?;
    if batch.members[0].page_run_id.as_deref() != Some(first.id.as_str()) {
        return Err("VISUAL_BATCH_UNLINKED_RUN_NOT_RECOVERED".into());
    }
    assert_linked_run_lease_recovery(conn, &batch, &first)?;
    assert_unknown_result_pauses_current_batch(conn, &batch, &first)
        .map_err(|code| format!("VISUAL_BATCH_TEST_UNKNOWN:{code}"))?;

    let first_ready =
        comic_visual_render::test_claim_finish_existing_page_run(conn, &first, output_root)
            .map_err(|code| format!("VISUAL_BATCH_TEST_FIRST_FINISH:{code}"))?;
    if first_ready.status != "candidate_ready" || first_ready.asset_id.is_none() {
        return Err("VISUAL_BATCH_FIRST_PAGE_NOT_CANDIDATE_READY".into());
    }
    // The second page first records a real never-submitted config-blocked
    // parent (attempt zero), then explicit batch resume must create exactly
    // one legal child attempt and repoint this member atomically.
    let first_asset_id = first_ready
        .asset_id
        .as_deref()
        .ok_or("VISUAL_BATCH_FIRST_ASSET_MISSING")?;
    let config_child = assert_config_blocked_child_resume(conn, &batch, job, first_asset_id)?;
    assert_terminal_resume_replay(conn, &batch, job, &config_child)?;
    let second = assert_failed_member_retry(conn, &batch, job, &config_child)?;
    batch = get_by_id(conn, &job.project_id, &job.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_MISSING_AFTER_FIRST_RECOVERY")?;
    if batch.completed_members != 1 || batch.current_member_ordinal != Some(2) {
        return Err("VISUAL_BATCH_CANDIDATE_DID_NOT_ADVANCE".into());
    }
    let second_ready =
        comic_visual_render::test_claim_finish_existing_page_run(conn, &second, output_root)
            .map_err(|code| format!("VISUAL_BATCH_TEST_SECOND_FINISH:{code}"))?;
    if second_ready.status != "candidate_ready" || second_ready.asset_id.is_none() {
        return Err("VISUAL_BATCH_SECOND_PAGE_NOT_CANDIDATE_READY".into());
    }
    if comic_visual_render::test_claim_finish_existing_page_run(conn, &second, output_root).is_ok()
    {
        return Err("VISUAL_BATCH_CONFIG_CHILD_CLAIMED_TWICE".into());
    }
    if next_page_action(conn, &batch.id, true, "visual-test-provider")?.is_some() {
        return Err("VISUAL_BATCH_STARTED_AFTER_ALL_PAGES_READY".into());
    }
    batch = get_by_id(conn, &job.project_id, &job.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_MISSING_AFTER_COMPLETION")?;
    if batch.status != "candidate_ready" || batch.completed_members != 2 {
        return Err("VISUAL_BATCH_TWO_PAGE_COMPLETION_FAILED".into());
    }

    Ok(())
}

fn assert_terminal_resume_replay(
    conn: &Connection,
    batch: &ComicVisualBatch,
    job: &NovelProductionJob,
    child: &comic_visual_render::ComicVisualPageRun,
) -> Result<(), String> {
    let resume = ComicVisualBatchResumeInput {
        project_id: job.project_id.clone(),
        novel_work_id: job.novel_work_id.clone(),
        production_job_id: job.id.clone(),
        idempotency_key: "visual-config-resume".into(),
    };
    let (baseline, baseline_queued, baseline_schedule) = resume_batch_inner(
        conn,
        &resume,
        true,
        "visual-test-model",
        "visual-test-provider",
        "visual-batch-test-session",
    )?;
    if baseline.status != "running"
        || baseline_queued.as_deref() != Some(child.id.as_str())
        || baseline_schedule
    {
        return Err("VISUAL_BATCH_RESUME_BASELINE_DID_NOT_FIND_QUEUED_CHILD".into());
    }
    for status in ["needs_reconcile", "blocked_stale"] {
        conn.execute(
            "UPDATE comic_visual_batches SET status=? WHERE id=?",
            params![status, batch.id],
        )
        .map_err(|_| "VISUAL_BATCH_TERMINAL_REPLAY_WRITE_FAILED".to_string())?;
        let (replayed, queued_child, should_schedule) = resume_batch_inner(
            conn,
            &resume,
            true,
            "visual-test-model",
            "visual-test-provider",
            "visual-batch-test-session",
        )?;
        if replayed.status != status || queued_child.is_some() || should_schedule {
            return Err("VISUAL_BATCH_TERMINAL_REPLAY_SCHEDULED".into());
        }
    }
    conn.execute(
        "UPDATE comic_visual_batches SET status='running' WHERE id=?",
        params![batch.id],
    )
    .map_err(|_| "VISUAL_BATCH_TERMINAL_REPLAY_RESET_FAILED".to_string())?;
    Ok(())
}

fn assert_failed_member_retry(
    conn: &Connection,
    batch: &ComicVisualBatch,
    job: &NovelProductionJob,
    parent: &comic_visual_render::ComicVisualPageRun,
) -> Result<comic_visual_render::ComicVisualPageRun, String> {
    let before = get_by_id(conn, &job.project_id, &job.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_RETRY_BATCH_MISSING")?;
    let member = before
        .members
        .iter()
        .find(|member| member.ordinal == 2)
        .ok_or("VISUAL_BATCH_RETRY_MEMBER_MISSING")?
        .clone();
    conn.execute(
        "UPDATE comic_visual_page_runs SET status='failed',failure_json=?,finished_at=?,lease_expires_at=NULL WHERE id=? AND status='queued'",
        params![json!({"code":"TEST_KNOWN_FAILURE","retryable":true}).to_string(), now(), parent.id],
    )
    .map_err(|_| "VISUAL_BATCH_RETRY_PARENT_FAILURE_WRITE_FAILED".to_string())?;
    let stored_status: String = conn
        .query_row(
            "SELECT status FROM comic_visual_batch_members WHERE id=?",
            params![member.member_id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_RETRY_MEMBER_STATUS_READ_FAILED".to_string())?;
    let legacy_projection = get_by_id(conn, &job.project_id, &job.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_RETRY_BATCH_MISSING")?;
    let projected = legacy_projection
        .members
        .iter()
        .find(|candidate| candidate.member_id == member.member_id)
        .ok_or("VISUAL_BATCH_RETRY_MEMBER_MISSING")?;
    if stored_status != "queued" || projected.status != "failed" {
        return Err("VISUAL_BATCH_RETRY_LEGACY_MEMBER_PROJECTION_FAILED".into());
    }
    let generic = comic_visual_render::test_retry_inner(
        conn,
        comic_visual_render::ComicVisualPageRunRetryInput {
            project_id: job.project_id.clone(),
            novel_work_id: job.novel_work_id.clone(),
            run_id: parent.id.clone(),
            idempotency_key: "visual-batch-generic-history-parent".into(),
        },
        true,
        "visual-batch-test-session",
        "visual-test-provider",
        "visual-test-model",
    );
    if generic.err().as_deref() != Some("VISUAL_BATCH_MEMBER_RETRY_REQUIRED") {
        return Err("VISUAL_BATCH_RETRY_GENERIC_PARENT_BYPASS".into());
    }

    let base = ComicVisualBatchMemberRetryInput {
        project_id: job.project_id.clone(),
        novel_work_id: job.novel_work_id.clone(),
        production_job_id: job.id.clone(),
        batch_id: batch.id.clone(),
        member_id: member.member_id.clone(),
        production_page_id: member.production_page_id.clone(),
        expected_parent_run_id: parent.id.clone(),
        confirm_possible_charge: true,
        idempotency_key: "visual-batch-member-retry".into(),
    };
    let child_count = || -> Result<i64, String> {
        conn.query_row(
            "SELECT COUNT(*) FROM comic_visual_page_runs WHERE parent_run_id=?",
            params![parent.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_RETRY_CHILD_COUNT_FAILED".to_string())
    };
    let no_confirm = retry_member_inner(
        conn,
        &ComicVisualBatchMemberRetryInput {
            confirm_possible_charge: false,
            idempotency_key: "visual-batch-member-retry-no-confirm".into(),
            ..base.clone()
        },
        true,
        "visual-test-provider",
        "visual-batch-test-session",
    );
    if no_confirm.err().as_deref() != Some("VISUAL_BATCH_RETRY_CONFIRMATION_REQUIRED")
        || child_count()? != 0
    {
        return Err("VISUAL_BATCH_RETRY_CONFIRMATION_BYPASS".into());
    }
    let config = retry_member_inner(
        conn,
        &ComicVisualBatchMemberRetryInput {
            idempotency_key: "visual-batch-member-retry-config".into(),
            ..base.clone()
        },
        false,
        "visual-test-provider",
        "visual-batch-test-session",
    );
    if config.err().as_deref() != Some("VISUAL_BATCH_CONFIG_REQUIRED") || child_count()? != 0 {
        return Err("VISUAL_BATCH_RETRY_CONFIG_ACCEPTED".into());
    }
    let provider_mismatch = retry_member_inner(
        conn,
        &ComicVisualBatchMemberRetryInput {
            idempotency_key: "visual-batch-member-retry-provider".into(),
            ..base.clone()
        },
        true,
        "other-provider",
        "visual-batch-test-session",
    );
    if provider_mismatch.err().as_deref() != Some("VISUAL_BATCH_CONFIG_REQUIRED")
        || child_count()? != 0
    {
        return Err("VISUAL_BATCH_RETRY_PROVIDER_ACCEPTED".into());
    }
    let wrong_scope = retry_member_inner(
        conn,
        &ComicVisualBatchMemberRetryInput {
            project_id: "other-project".into(),
            idempotency_key: "visual-batch-member-retry-scope".into(),
            ..base.clone()
        },
        true,
        "visual-test-provider",
        "visual-batch-test-session",
    );
    if wrong_scope.err().as_deref() != Some("VISUAL_BATCH_SCOPE_MISMATCH") || child_count()? != 0 {
        return Err("VISUAL_BATCH_RETRY_SCOPE_ACCEPTED".into());
    }
    conn.execute(
        "UPDATE comic_visual_batches SET lease_owner='other-session',lease_expires_at=? WHERE id=?",
        params![now() + 60_000, batch.id],
    )
    .map_err(|_| "VISUAL_BATCH_RETRY_LEASE_WRITE_FAILED".to_string())?;
    let busy = retry_member_inner(
        conn,
        &ComicVisualBatchMemberRetryInput {
            idempotency_key: "visual-batch-member-retry-busy".into(),
            ..base.clone()
        },
        true,
        "visual-test-provider",
        "visual-batch-test-session",
    );
    conn.execute(
        "UPDATE comic_visual_batches SET lease_owner=NULL,lease_expires_at=NULL WHERE id=?",
        params![batch.id],
    )
    .map_err(|_| "VISUAL_BATCH_RETRY_LEASE_RESET_FAILED".to_string())?;
    if busy.err().as_deref() != Some("VISUAL_BATCH_RETRY_BUSY") || child_count()? != 0 {
        return Err("VISUAL_BATCH_RETRY_LEASE_ACCEPTED".into());
    }
    for (status, expected) in [
        ("needs_reconcile", "VISUAL_BATCH_RECONCILE_REQUIRED"),
        ("blocked_stale", "VISUAL_BATCH_STALE_REQUIRED"),
    ] {
        conn.execute(
            "UPDATE comic_visual_batches SET status=? WHERE id=?",
            params![status, batch.id],
        )
        .map_err(|_| "VISUAL_BATCH_RETRY_TERMINAL_WRITE_FAILED".to_string())?;
        let rejected = retry_member_inner(
            conn,
            &ComicVisualBatchMemberRetryInput {
                idempotency_key: format!("visual-batch-member-retry-{status}"),
                ..base.clone()
            },
            true,
            "visual-test-provider",
            "visual-batch-test-session",
        );
        if rejected.err().as_deref() != Some(expected) || child_count()? != 0 {
            return Err("VISUAL_BATCH_RETRY_TERMINAL_ACCEPTED".into());
        }
    }
    conn.execute(
        "UPDATE comic_visual_batches SET status='running' WHERE id=?",
        params![batch.id],
    )
    .map_err(|_| "VISUAL_BATCH_RETRY_TERMINAL_RESET_FAILED".to_string())?;
    let wrong_parent = retry_member_inner(
        conn,
        &ComicVisualBatchMemberRetryInput {
            expected_parent_run_id: "wrong-parent".into(),
            idempotency_key: "visual-batch-member-retry-wrong-parent".into(),
            ..base.clone()
        },
        true,
        "visual-test-provider",
        "visual-batch-test-session",
    );
    if wrong_parent.err().as_deref() != Some("VISUAL_BATCH_RETRY_PARENT_CHANGED")
        || child_count()? != 0
    {
        return Err("VISUAL_BATCH_RETRY_PARENT_MISMATCH_ACCEPTED".into());
    }

    // `request_json` is immutable in production.  The fixture temporarily
    // drops that guard solely to prove a corrupted historical request cannot
    // be retried under the batch's frozen model/size authorization.
    let original_request: String = conn
        .query_row(
            "SELECT request_json FROM comic_visual_page_runs WHERE id=?",
            params![parent.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_RETRY_REQUEST_READ_FAILED".to_string())?;
    let identity_trigger_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='comic_visual_page_run_identity_immutable'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_RETRY_TRIGGER_READ_FAILED".to_string())?;
    conn.execute_batch("DROP TRIGGER comic_visual_page_run_identity_immutable")
        .map_err(|_| "VISUAL_BATCH_RETRY_TRIGGER_DROP_FAILED".to_string())?;
    let mut drifted: Value = serde_json::from_str(&original_request)
        .map_err(|_| "VISUAL_BATCH_RETRY_REQUEST_PARSE_FAILED".to_string())?;
    drifted["model"] = json!("different-frozen-model");
    conn.execute(
        "UPDATE comic_visual_page_runs SET request_json=? WHERE id=?",
        params![drifted.to_string(), parent.id],
    )
    .map_err(|_| "VISUAL_BATCH_RETRY_DRIFT_WRITE_FAILED".to_string())?;
    let drift = retry_member_inner(
        conn,
        &ComicVisualBatchMemberRetryInput {
            idempotency_key: "visual-batch-member-retry-drift".into(),
            ..base.clone()
        },
        true,
        "visual-test-provider",
        "visual-batch-test-session",
    );
    conn.execute(
        "UPDATE comic_visual_page_runs SET request_json=? WHERE id=?",
        params![original_request, parent.id],
    )
    .map_err(|_| "VISUAL_BATCH_RETRY_REQUEST_RESET_FAILED".to_string())?;
    conn.execute_batch(&identity_trigger_sql)
        .map_err(|_| "VISUAL_BATCH_RETRY_TRIGGER_RESTORE_FAILED".to_string())?;
    if conn
        .execute(
            "UPDATE comic_visual_page_runs SET request_json='{}' WHERE id=?",
            params![parent.id],
        )
        .is_ok()
    {
        return Err("VISUAL_BATCH_RETRY_TRIGGER_NOT_RESTORED".into());
    }
    if drift.err().as_deref() != Some("VISUAL_BATCH_RETRY_FROZEN_REQUEST_MISMATCH")
        || child_count()? != 0
    {
        return Err("VISUAL_BATCH_RETRY_FROZEN_REQUEST_DRIFT_ACCEPTED".into());
    }

    conn.execute_batch("CREATE TRIGGER comic_visual_batch_retry_member_abort BEFORE UPDATE OF page_run_id ON comic_visual_batch_members BEGIN SELECT RAISE(ABORT, 'retry member abort'); END")
        .map_err(|_| "VISUAL_BATCH_RETRY_TRIGGER_CREATE_FAILED".to_string())?;
    let rollback = retry_member_inner(
        conn,
        &ComicVisualBatchMemberRetryInput {
            idempotency_key: "visual-batch-member-retry-rollback".into(),
            ..base.clone()
        },
        true,
        "visual-test-provider",
        "visual-batch-test-session",
    );
    conn.execute_batch("DROP TRIGGER comic_visual_batch_retry_member_abort")
        .map_err(|_| "VISUAL_BATCH_RETRY_TRIGGER_DROP_FAILED".to_string())?;
    let receipt_after_rollback: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_batch_command_receipts WHERE command_name=? AND idempotency_key='visual-batch-member-retry-rollback'",
            params![COMMAND_MEMBER_RETRY],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_RETRY_RECEIPT_COUNT_FAILED".to_string())?;
    if rollback.err().as_deref() != Some("VISUAL_BATCH_WRITE_FAILED")
        || child_count()? != 0
        || receipt_after_rollback != 0
    {
        return Err("VISUAL_BATCH_RETRY_NOT_ATOMIC".into());
    }

    let (retried, queued) = retry_member_inner(
        conn,
        &base,
        true,
        "visual-test-provider",
        "visual-batch-test-session",
    )?;
    let child_id = queued.ok_or("VISUAL_BATCH_RETRY_CHILD_NOT_QUEUED")?;
    let child = comic_visual_render::get_run(conn, &job.project_id, &job.novel_work_id, &child_id)?;
    if retried.status != "running"
        || child.status != "queued"
        || child.parent_run_id.as_deref() != Some(parent.id.as_str())
        || child.attempt_no != parent.attempt_no + 1
        || retried.members[1].page_run_id.as_deref() != Some(child.id.as_str())
        || retried.members[1].status != "queued"
        || child_count()? != 1
    {
        return Err("VISUAL_BATCH_RETRY_CHILD_LINEAGE_INVALID".into());
    }
    let (_, replay_queued) = retry_member_inner(
        conn,
        &base,
        true,
        "visual-test-provider",
        "visual-batch-test-session",
    )?;
    let different = retry_member_inner(
        conn,
        &ComicVisualBatchMemberRetryInput {
            idempotency_key: "visual-batch-member-retry-different".into(),
            ..base
        },
        true,
        "visual-test-provider",
        "visual-batch-test-session",
    );
    if replay_queued.as_deref() != Some(child.id.as_str())
        || different.err().as_deref() != Some("VISUAL_BATCH_RETRY_PARENT_CHANGED")
        || child_count()? != 1
    {
        return Err("VISUAL_BATCH_RETRY_REPLAY_FORKED_CHILD".into());
    }
    comic_visual_render::test_late_failed_callback(conn, parent)?;
    sync_member_outcomes(
        conn,
        &read_batch(conn, &batch.id)?.ok_or("VISUAL_BATCH_RETRY_BATCH_MISSING")?,
    )?;
    let linked_after_late_parent = get_by_id(conn, &job.project_id, &job.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_RETRY_BATCH_MISSING")?;
    let old_parent_status: String = conn
        .query_row(
            "SELECT status FROM comic_visual_page_runs WHERE id=?",
            params![parent.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_RETRY_PARENT_STATUS_READ_FAILED".to_string())?;
    if linked_after_late_parent.members[1].page_run_id.as_deref() != Some(child.id.as_str())
        || old_parent_status != "failed"
    {
        return Err("VISUAL_BATCH_RETRY_OLD_PARENT_OVERWROTE_CHILD".into());
    }
    Ok(child)
}

fn assert_config_blocked_child_resume(
    conn: &Connection,
    batch: &ComicVisualBatch,
    job: &NovelProductionJob,
    evidence_asset_id: &str,
) -> Result<comic_visual_render::ComicVisualPageRun, String> {
    let token = new_id("visual-batch-config-parent-lease");
    if !claim_batch(conn, &batch.id, &token)? {
        return Err("VISUAL_BATCH_CONFIG_PARENT_CLAIM_FAILED".into());
    }
    let action = next_page_action(conn, &batch.id, true, "visual-test-provider")?
        .ok_or("VISUAL_BATCH_CONFIG_PARENT_ACTION_MISSING")?;
    let parent = comic_visual_render::start_inner(
        conn,
        action.input,
        false,
        false,
        "visual-batch-test-session",
        &action.provider_id,
        &action.model,
        1,
        None,
    )?;
    link_member_run(
        conn,
        &batch.id,
        &token,
        &action.member_id,
        &action.dispatch_key,
        &parent.id,
    )?;
    release_batch_claim(conn, &batch.id, &token)?;
    if parent.status != "blocked_config"
        || parent.attempt_no != 0
        || parent.submitted_at.is_some()
        || parent.provider_request_id.is_some()
        || parent.asset_id.is_some()
    {
        return Err("VISUAL_BATCH_CONFIG_PARENT_NOT_UNSUBMITTED".into());
    }

    let resume = ComicVisualBatchResumeInput {
        project_id: job.project_id.clone(),
        novel_work_id: job.novel_work_id.clone(),
        production_job_id: job.id.clone(),
        idempotency_key: "visual-config-resume".into(),
    };
    // A parent that has any submission evidence is never converted into a
    // child. The failed transaction must leave member, receipt and children
    // untouched.
    conn.execute(
        "UPDATE comic_visual_page_runs SET submitted_at=? WHERE id=?",
        params![now(), parent.id],
    )
    .map_err(|_| "VISUAL_BATCH_CONFIG_PARENT_WRITE_FAILED".to_string())?;
    let rejected = resume_batch_inner(
        conn,
        &resume,
        true,
        "visual-test-model",
        "visual-test-provider",
        "visual-batch-test-session",
    );
    let children_after_reject: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_page_runs WHERE parent_run_id=?",
            params![parent.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_CONFIG_CHILD_COUNT_FAILED".to_string())?;
    let receipt_after_reject: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_batch_command_receipts WHERE command_name=? AND idempotency_key=?",
            params![COMMAND_RESUME, resume.idempotency_key],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_CONFIG_RECEIPT_COUNT_FAILED".to_string())?;
    if rejected.err().as_deref() != Some("VISUAL_CONFIG_RESUME_RECONCILE_REQUIRED")
        || children_after_reject != 0
        || receipt_after_reject != 0
    {
        return Err("VISUAL_BATCH_CONFIG_SUBMITTED_PARENT_ACCEPTED".into());
    }
    conn.execute(
        "UPDATE comic_visual_page_runs SET submitted_at=NULL WHERE id=?",
        params![parent.id],
    )
    .map_err(|_| "VISUAL_BATCH_CONFIG_PARENT_RESET_FAILED".to_string())?;

    conn.execute(
        "UPDATE comic_visual_page_runs SET provider_request_id='already-submitted' WHERE id=?",
        params![parent.id],
    )
    .map_err(|_| "VISUAL_BATCH_CONFIG_PARENT_WRITE_FAILED".to_string())?;
    let provider_rejected = resume_batch_inner(
        conn,
        &ComicVisualBatchResumeInput {
            idempotency_key: "visual-config-resume-provider".into(),
            ..resume.clone()
        },
        true,
        "visual-test-model",
        "visual-test-provider",
        "visual-batch-test-session",
    );
    conn.execute(
        "UPDATE comic_visual_page_runs SET provider_request_id=NULL WHERE id=?",
        params![parent.id],
    )
    .map_err(|_| "VISUAL_BATCH_CONFIG_PARENT_RESET_FAILED".to_string())?;
    if provider_rejected.err().as_deref() != Some("VISUAL_CONFIG_RESUME_RECONCILE_REQUIRED") {
        return Err("VISUAL_BATCH_CONFIG_PROVIDER_EVIDENCE_ACCEPTED".into());
    }

    conn.execute(
        "UPDATE comic_visual_page_runs SET asset_id=? WHERE id=?",
        params![evidence_asset_id, parent.id],
    )
    .map_err(|_| "VISUAL_BATCH_CONFIG_PARENT_WRITE_FAILED".to_string())?;
    let asset_rejected = resume_batch_inner(
        conn,
        &ComicVisualBatchResumeInput {
            idempotency_key: "visual-config-resume-asset".into(),
            ..resume.clone()
        },
        true,
        "visual-test-model",
        "visual-test-provider",
        "visual-batch-test-session",
    );
    conn.execute(
        "UPDATE comic_visual_page_runs SET asset_id=NULL WHERE id=?",
        params![parent.id],
    )
    .map_err(|_| "VISUAL_BATCH_CONFIG_PARENT_RESET_FAILED".to_string())?;
    if asset_rejected.err().as_deref() != Some("VISUAL_CONFIG_RESUME_RECONCILE_REQUIRED") {
        return Err("VISUAL_BATCH_CONFIG_ASSET_EVIDENCE_ACCEPTED".into());
    }

    conn.execute(
        "UPDATE comic_visual_batches SET lease_owner='other-session',lease_expires_at=? WHERE id=?",
        params![now() + 60_000, batch.id],
    )
    .map_err(|_| "VISUAL_BATCH_CONFIG_LEASE_WRITE_FAILED".to_string())?;
    let busy = resume_batch_inner(
        conn,
        &ComicVisualBatchResumeInput {
            idempotency_key: "visual-config-resume-busy".into(),
            ..resume.clone()
        },
        true,
        "visual-test-model",
        "visual-test-provider",
        "visual-batch-test-session",
    );
    conn.execute(
        "UPDATE comic_visual_batches SET lease_owner=NULL,lease_expires_at=NULL WHERE id=?",
        params![batch.id],
    )
    .map_err(|_| "VISUAL_BATCH_CONFIG_LEASE_RESET_FAILED".to_string())?;
    if busy.err().as_deref() != Some("VISUAL_BATCH_RESUME_BUSY") {
        return Err("VISUAL_BATCH_CONFIG_ACTIVE_LEASE_ACCEPTED".into());
    }

    conn.execute_batch("CREATE TRIGGER visual_batch_test_resume_member_abort BEFORE UPDATE OF page_run_id ON comic_visual_batch_members BEGIN SELECT RAISE(ABORT, 'resume member abort'); END")
        .map_err(|_| "VISUAL_BATCH_CONFIG_TRIGGER_CREATE_FAILED".to_string())?;
    let rollback = resume_batch_inner(
        conn,
        &ComicVisualBatchResumeInput {
            idempotency_key: "visual-config-resume-rollback".into(),
            ..resume.clone()
        },
        true,
        "visual-test-model",
        "visual-test-provider",
        "visual-batch-test-session",
    );
    conn.execute_batch("DROP TRIGGER visual_batch_test_resume_member_abort")
        .map_err(|_| "VISUAL_BATCH_CONFIG_TRIGGER_DROP_FAILED".to_string())?;
    let children_after_rollback: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_page_runs WHERE parent_run_id=?",
            params![parent.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_CONFIG_CHILD_COUNT_FAILED".to_string())?;
    let receipt_after_rollback: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_batch_command_receipts WHERE command_name=? AND idempotency_key='visual-config-resume-rollback'",
            params![COMMAND_RESUME],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_CONFIG_RECEIPT_COUNT_FAILED".to_string())?;
    let linked_after_rollback: String = conn
        .query_row(
            "SELECT page_run_id FROM comic_visual_batch_members WHERE id=?",
            params![action.member_id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_CONFIG_MEMBER_READ_FAILED".to_string())?;
    if rollback.err().as_deref() != Some("VISUAL_BATCH_WRITE_FAILED")
        || children_after_rollback != 0
        || receipt_after_rollback != 0
        || linked_after_rollback != parent.id
    {
        return Err("VISUAL_BATCH_CONFIG_RESUME_NOT_ATOMIC".into());
    }

    conn.execute(
        "UPDATE comic_visual_batches SET status='blocked_stale' WHERE id=?",
        params![batch.id],
    )
    .map_err(|_| "VISUAL_BATCH_CONFIG_STALE_WRITE_FAILED".to_string())?;
    let stale_resume = ComicVisualBatchResumeInput {
        idempotency_key: "visual-config-resume-stale".into(),
        ..resume.clone()
    };
    let stale = resume_batch_inner(
        conn,
        &stale_resume,
        true,
        "visual-test-model",
        "visual-test-provider",
        "visual-batch-test-session",
    );
    let stale_children: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_page_runs WHERE parent_run_id=?",
            params![parent.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_CONFIG_CHILD_COUNT_FAILED".to_string())?;
    if stale.err().as_deref() != Some("VISUAL_BATCH_STALE_REQUIRED") || stale_children != 0 {
        return Err("VISUAL_BATCH_CONFIG_STALE_RESUMED".into());
    }
    conn.execute(
        "UPDATE comic_visual_batches SET status='blocked_config' WHERE id=?",
        params![batch.id],
    )
    .map_err(|_| "VISUAL_BATCH_CONFIG_STALE_RESET_FAILED".to_string())?;

    let (resumed, queued, scheduled_batch) = resume_batch_inner(
        conn,
        &resume,
        true,
        "visual-test-model",
        "visual-test-provider",
        "visual-batch-test-session",
    )?;
    let child_id = queued.ok_or("VISUAL_BATCH_CONFIG_CHILD_NOT_QUEUED")?;
    if scheduled_batch || resumed.status != "running" {
        return Err("VISUAL_BATCH_CONFIG_RESUME_DID_NOT_QUEUE_CHILD".into());
    }
    let child = comic_visual_render::get_run(conn, &job.project_id, &job.novel_work_id, &child_id)?;
    let child_key: String = conn
        .query_row(
            "SELECT idempotency_key FROM comic_visual_page_runs WHERE id=?",
            params![child.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_CONFIG_CHILD_KEY_READ_FAILED".to_string())?;
    if child.status != "queued"
        || child.attempt_no != 1
        || child.parent_run_id.as_deref() != Some(parent.id.as_str())
        || child_key != format!("{}:config-resume:{}", action.dispatch_key, parent.id)
    {
        return Err("VISUAL_BATCH_CONFIG_CHILD_LINEAGE_INVALID".into());
    }
    if resumed.members[1].page_run_id.as_deref() != Some(child.id.as_str())
        || resumed.members[1].status != "queued"
    {
        return Err("VISUAL_BATCH_CONFIG_MEMBER_NOT_REPOINTED".into());
    }

    let (_, replay_queued, replay_batch) = resume_batch_inner(
        conn,
        &resume,
        true,
        "visual-test-model",
        "visual-test-provider",
        "visual-batch-test-session",
    )?;
    let different = ComicVisualBatchResumeInput {
        idempotency_key: "visual-config-resume-different".into(),
        ..resume
    };
    let (_, different_queued, different_batch) = resume_batch_inner(
        conn,
        &different,
        true,
        "visual-test-model",
        "visual-test-provider",
        "visual-batch-test-session",
    )?;
    let child_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_page_runs WHERE parent_run_id=?",
            params![parent.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_CONFIG_CHILD_COUNT_FAILED".to_string())?;
    if replay_queued.as_deref() != Some(child.id.as_str())
        || replay_batch
        || different_queued.as_deref() != Some(child.id.as_str())
        || different_batch
        || child_count != 1
    {
        return Err("VISUAL_BATCH_CONFIG_RESUME_REPLAY_FORKED_CHILD".into());
    }
    Ok(child)
}

fn assert_linked_run_lease_recovery(
    conn: &Connection,
    batch: &ComicVisualBatch,
    run: &comic_visual_render::ComicVisualPageRun,
) -> Result<(), String> {
    conn.execute_batch("SAVEPOINT visual_batch_run_lease")
        .map_err(|_| "VISUAL_BATCH_LEASE_SAVEPOINT_FAILED".to_string())?;
    conn.execute(
        "UPDATE comic_visual_page_runs SET status='running',lease_expires_at=? WHERE id=?",
        params![now() + 60_000, run.id],
    )
    .map_err(|_| "VISUAL_BATCH_LEASE_FIXTURE_WRITE_FAILED".to_string())?;
    match reconcile_linked_run_lease(conn, &batch.id, &run.id)? {
        LinkedRunRecoveryDecision::WaitUntil(_) => {}
        _ => return Err("VISUAL_BATCH_HEARTBEAT_EXTENSION_NOT_RESPECTED".into()),
    }
    conn.execute(
        "UPDATE comic_visual_page_runs SET lease_expires_at=? WHERE id=?",
        params![now() - 1, run.id],
    )
    .map_err(|_| "VISUAL_BATCH_LEASE_FIXTURE_WRITE_FAILED".to_string())?;
    if !matches!(
        reconcile_linked_run_lease(conn, &batch.id, &run.id)?,
        LinkedRunRecoveryDecision::Reconciled
    ) {
        return Err("VISUAL_BATCH_EXPIRED_RUN_NOT_RECONCILED".into());
    }
    let stopped = get_by_id(conn, &batch.project_id, &batch.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_LEASE_READ_FAILED")?;
    conn.execute_batch("ROLLBACK TO visual_batch_run_lease; RELEASE visual_batch_run_lease")
        .map_err(|_| "VISUAL_BATCH_LEASE_ROLLBACK_FAILED".to_string())?;
    if stopped.status != "needs_reconcile" {
        return Err("VISUAL_BATCH_EXPIRED_RUN_DID_NOT_STOP_BATCH".into());
    }
    Ok(())
}

fn assert_unknown_result_pauses_current_batch(
    conn: &Connection,
    batch: &ComicVisualBatch,
    run: &comic_visual_render::ComicVisualPageRun,
) -> Result<(), String> {
    conn.execute_batch("SAVEPOINT visual_batch_unknown_result")
        .map_err(|_| "VISUAL_BATCH_UNKNOWN_SAVEPOINT_FAILED".to_string())?;
    conn.execute(
        "UPDATE comic_visual_page_runs SET status='needs_reconcile', lease_expires_at=NULL WHERE id=? AND status='queued'",
        params![run.id],
    )
    .map_err(|_| "VISUAL_BATCH_UNKNOWN_FIXTURE_WRITE_FAILED".to_string())?;
    let action = next_page_action(conn, &batch.id, true, "visual-test-provider")?;
    let stopped = get_by_id(conn, &batch.project_id, &batch.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_UNKNOWN_READ_FAILED")?;
    conn.execute_batch(
        "ROLLBACK TO visual_batch_unknown_result; RELEASE visual_batch_unknown_result",
    )
    .map_err(|_| "VISUAL_BATCH_UNKNOWN_ROLLBACK_FAILED".to_string())?;
    if action.is_some() || stopped.status != "needs_reconcile" {
        return Err("VISUAL_BATCH_UNKNOWN_RESULT_NOT_PAUSED".into());
    }
    Ok(())
}

fn assert_stale_manifest_pauses_current_batch(
    conn: &Connection,
    batch: &ComicVisualBatch,
) -> Result<(), String> {
    conn.execute_batch("SAVEPOINT visual_batch_stale_manifest")
        .map_err(|_| "VISUAL_BATCH_STALE_SAVEPOINT_FAILED".to_string())?;
    let adaptation_id: String = conn
        .query_row(
            "SELECT comic_adaptation_id FROM comic_visual_batches WHERE id=?",
            params![batch.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_STALE_SCOPE_READ_FAILED".to_string())?;
    let version: i64 = conn
        .query_row(
            "SELECT optimistic_version FROM comic_adaptations WHERE id=?",
            params![adaptation_id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_BATCH_STALE_VERSION_READ_FAILED".to_string())?;
    conn.execute(
        "UPDATE comic_adaptations SET optimistic_version=? WHERE id=?",
        params![version + 1, adaptation_id],
    )
    .map_err(|_| "VISUAL_BATCH_STALE_VERSION_WRITE_FAILED".to_string())?;
    let action = next_page_action(conn, &batch.id, true, "visual-test-provider")?;
    let stopped = get_by_id(conn, &batch.project_id, &batch.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_STALE_READ_FAILED")?;
    conn.execute_batch(
        "ROLLBACK TO visual_batch_stale_manifest; RELEASE visual_batch_stale_manifest",
    )
    .map_err(|_| "VISUAL_BATCH_STALE_ROLLBACK_FAILED".to_string())?;
    if action.is_some() || stopped.status != "blocked_stale" {
        return Err("VISUAL_BATCH_STALE_MANIFEST_NOT_PAUSED".into());
    }
    Ok(())
}

fn prepare_real_batch(
    conn: &Connection,
    job: &NovelProductionJob,
) -> Result<ComicVisualBatch, String> {
    let batch = get_by_job(conn, &job.project_id, &job.novel_work_id, &job.id)?
        .ok_or("VISUAL_BATCH_AUTHORIZATION_MISSING")?;
    prepare_if_text_complete(conn, &batch.id)?;
    get_by_id(conn, &job.project_id, &job.novel_work_id, &batch.id)?
        .ok_or("VISUAL_BATCH_PREPARE_READ_FAILED".into())
}

fn start_without_link(
    conn: &Connection,
    batch_id: &str,
) -> Result<
    (
        comic_visual_render::ComicVisualPageRun,
        ComicVisualPageRunStartInput,
    ),
    String,
> {
    let token = new_id("visual-batch-test-lease");
    if !claim_batch(conn, batch_id, &token)? {
        return Err("VISUAL_BATCH_TEST_CLAIM_FAILED".into());
    }
    let action = next_page_action(conn, batch_id, true, "visual-test-provider")?
        .ok_or("VISUAL_BATCH_FIRST_ACTION_MISSING")?;
    let run = comic_visual_render::start_inner(
        conn,
        action.input.clone(),
        false,
        true,
        "visual-batch-test-session",
        &action.provider_id,
        &action.model,
        1,
        None,
    )?;
    release_batch_claim(conn, batch_id, &token)?;
    Ok((run, action.input))
}

fn count_runs_for_key(conn: &Connection, key: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COUNT(*) FROM comic_visual_page_runs WHERE idempotency_key=?",
        params![key],
        |row| row.get(0),
    )
    .map_err(|_| "VISUAL_BATCH_RUN_COUNT_FAILED".to_string())
}
