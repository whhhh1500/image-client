//! Renderer assertions run against novel_adaptation's migrated, applied-plan
//! fixture. This deliberately avoids hand-written shadow tables.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};

use crate::comic_visual::ComicVisualManifest;
use crate::comic_visual_asset::{get_inner, ComicVisualPageAssetGetInput};
use crate::model::AssetRef;

use super::{
    claim_dispatch_at, finish_dispatch_at, get_run, new_id, start_inner, ComicVisualPageRun,
    ComicVisualPageRunRetryInput, ComicVisualPageRunStartInput, COMPILER_CONTRACT,
    COMPILER_CONTRACT_V2,
};

pub(crate) fn assert_real_manifest_page_run(
    conn: &Connection,
    manifest: &ComicVisualManifest,
    output_root: &Path,
) -> Result<(), String> {
    let page_id = manifest
        .manifest
        .get("pages")
        .and_then(Value::as_array)
        .and_then(|pages| pages.first())
        .and_then(|page| page.get("productionPageId"))
        .and_then(Value::as_str)
        .ok_or("VISUAL_INTEGRATION_PAGE_MISSING")?;
    // The full batch fixture may already have produced the authoritative
    // first candidate. Reuse it rather than violating the immutable
    // (manifest, page, attempt) identity by starting another attempt #1.
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM comic_visual_page_runs WHERE manifest_id=? AND production_page_id=? AND status='candidate_ready' ORDER BY attempt_no DESC,id DESC LIMIT 1",
            params![manifest.id, page_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "VISUAL_INTEGRATION_RUN_READ_FAILED".to_string())?;
    let ready = match existing {
        Some(id) => get_run(conn, &manifest.project_id, &manifest.novel_work_id, &id)?,
        None => super::test_start_claim_finish_manifest_page(conn, manifest, output_root)?,
    };
    if ready.status != "candidate_ready" || ready.asset_id.is_none() {
        return Err("VISUAL_INTEGRATION_CANDIDATE_MISSING".into());
    }
    assert_v2_receipt_retry_and_dispatch_route(conn, manifest, &ready)?;
    if !ready
        .request_json
        .get("compiledPrompt")
        .is_some_and(Value::is_string)
        || !ready
            .request_json
            .get("compilerInput")
            .is_some_and(Value::is_object)
    {
        return Err("VISUAL_INTEGRATION_COMPILED_REQUEST_MISSING".into());
    }

    let (kind, path, metadata_raw): (String, String, String) = conn
        .query_row(
            "SELECT kind,path,metadata FROM assets WHERE id=?",
            params![ready.asset_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|_| "VISUAL_INTEGRATION_ASSET_MISSING".to_string())?;
    let metadata: Value = serde_json::from_str(&metadata_raw)
        .map_err(|_| "VISUAL_INTEGRATION_ASSET_METADATA_INVALID".to_string())?;
    let expected_dir = output_root
        .join("漫画")
        .join("小说生产")
        .join(&ready.id)
        .canonicalize()
        .map_err(|_| "VISUAL_INTEGRATION_OUTPUT_MISSING".to_string())?;
    let candidate_path = Path::new(&path)
        .canonicalize()
        .map_err(|_| "VISUAL_INTEGRATION_CANDIDATE_MISSING".to_string())?;
    if kind != "image"
        || !candidate_path.starts_with(&expected_dir)
        || metadata
            .get("comicVisualManifestId")
            .and_then(Value::as_str)
            != Some(&manifest.id)
        || metadata.get("productionPageId").and_then(Value::as_str)
            != Some(ready.production_page_id.as_str())
        || metadata.get("visualRunId").and_then(Value::as_str) != Some(&ready.id)
        || metadata.get("novelWorkId").and_then(Value::as_str) != Some(&manifest.novel_work_id)
    {
        return Err("VISUAL_INTEGRATION_PROVENANCE_MISMATCH".into());
    }

    let queued = start_next_attempt(conn, manifest, &ready)?;
    let version: i64 = conn
        .query_row(
            "SELECT optimistic_version FROM comic_adaptations WHERE id=?",
            params![manifest.comic_adaptation_id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_INTEGRATION_SCOPE_MISSING".to_string())?;
    conn.execute(
        "UPDATE comic_adaptations SET optimistic_version=? WHERE id=?",
        params![version + 1, manifest.comic_adaptation_id],
    )
    .map_err(|_| "VISUAL_INTEGRATION_SCOPE_WRITE_FAILED".to_string())?;
    let historical_asset = get_inner(
        conn,
        &ComicVisualPageAssetGetInput {
            project_id: manifest.project_id.clone(),
            novel_work_id: manifest.novel_work_id.clone(),
            run_id: ready.id.clone(),
        },
        &expected_dir,
    )?;
    if historical_asset.id != ready.asset_id.as_deref().unwrap_or_default() {
        return Err("VISUAL_INTEGRATION_HISTORICAL_ASSET_UNAVAILABLE".into());
    }
    let stale_claim = claim_dispatch_at(conn, &queued.id, "visual-test-session", output_root)
        .map(|_| ())
        .unwrap_err();
    conn.execute(
        "UPDATE comic_adaptations SET optimistic_version=? WHERE id=?",
        params![version, manifest.comic_adaptation_id],
    )
    .map_err(|_| "VISUAL_INTEGRATION_SCOPE_WRITE_FAILED".to_string())?;
    if stale_claim != "VISUAL_MANIFEST_STALE_OR_SCOPE_MISMATCH" {
        return Err("VISUAL_INTEGRATION_STALE_CLAIM_ACCEPTED".into());
    }

    let (_, output_dir, running) =
        claim_dispatch_at(conn, &queued.id, "visual-test-session", output_root)?;
    let invalid_path = output_dir.join("not-an-image.html");
    std::fs::write(&invalid_path, b"<html>provider error</html>")
        .map_err(|_| "VISUAL_INTEGRATION_WRITE_FAILED".to_string())?;
    let invalid_asset_id = new_id("visual-test-invalid-asset");
    finish_dispatch_at(
        conn,
        &running,
        &output_dir,
        Ok(vec![AssetRef {
            id: invalid_asset_id.clone(),
            kind: "image".into(),
            path: invalid_path.to_string_lossy().to_string(),
            width: None,
            height: None,
            duration_s: None,
            format: Some("png".into()),
        }]),
    )?;
    let invalid = get_run(
        conn,
        &manifest.project_id,
        &manifest.novel_work_id,
        &running.id,
    )?;
    let stored_invalid_asset: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM assets WHERE id=?",
            params![invalid_asset_id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_INTEGRATION_ASSET_READ_FAILED".to_string())?;
    if invalid.status != "needs_reconcile"
        || invalid.asset_id.is_some()
        || stored_invalid_asset != 0
    {
        return Err("VISUAL_INTEGRATION_INVALID_OUTPUT_ACCEPTED".into());
    }
    Ok(())
}

fn assert_v2_receipt_retry_and_dispatch_route(
    conn: &Connection,
    manifest: &ComicVisualManifest,
    ready: &ComicVisualPageRun,
) -> Result<(), String> {
    let page_id = ready.production_page_id.clone();
    // Keep this compatibility probe out of the fixture's normal attempt
    // sequence, which deliberately starts its next v3 attempt at +1.
    let legacy_attempt = 100;
    let legacy_input = ComicVisualPageRunStartInput {
        project_id: manifest.project_id.clone(),
        novel_work_id: manifest.novel_work_id.clone(),
        manifest_id: manifest.id.clone(),
        production_page_id: page_id.clone(),
        manifest_fingerprint: manifest.manifest_fingerprint.clone(),
        compiler_contract_version: COMPILER_CONTRACT_V2.into(),
        request_json: json!({
            "schemaVersion": COMPILER_CONTRACT_V2,
            "manifestFingerprint": manifest.manifest_fingerprint,
            "productionPageId": page_id,
        }),
        reference_snapshot_json: json!({}),
        idempotency_key: new_id("visual-v2-receipt"),
    };
    let fresh_v2_key = new_id("visual-v2-new-rejected");
    let fresh_v2 = ComicVisualPageRunStartInput {
        idempotency_key: fresh_v2_key.clone(),
        ..legacy_input.clone()
    };
    if start_inner(
        conn,
        fresh_v2,
        false,
        true,
        "visual-test-session",
        "visual-test-provider",
        "visual-test-model",
        legacy_attempt,
        Some(&ready.id),
    )
    .err()
    .unwrap()
        != "VISUAL_COMPILER_VERSION_UNSUPPORTED"
    {
        return Err("VISUAL_INTEGRATION_V2_NEW_START_ACCEPTED".into());
    }
    let rejected_v2_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_page_runs WHERE idempotency_key=? AND manifest_id=? AND production_page_id=?",
            params![fresh_v2_key, manifest.id, page_id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_INTEGRATION_V2_REJECT_READ_FAILED".to_string())?;
    if rejected_v2_count != 0 {
        return Err("VISUAL_INTEGRATION_V2_REJECT_PERSISTED".into());
    }
    // Test-only legacy creation models a durable v2 record written before v3.
    // The public-new path below may replay it, but cannot create another v2.
    let legacy = start_inner(
        conn,
        legacy_input.clone(),
        true,
        true,
        "visual-test-session",
        "visual-test-provider",
        "visual-test-model",
        legacy_attempt,
        Some(&ready.id),
    )?;
    conn.execute(
        "UPDATE comic_visual_page_runs SET status='failed',lease_expires_at=NULL,owner_app_session_id=NULL,finished_at=? WHERE id=? AND status='queued'",
        params![crate::novel::now(), legacy.id],
    )
    .map_err(|_| "VISUAL_INTEGRATION_V2_STATUS_WRITE_FAILED".to_string())?;
    let legacy = get_run(
        conn,
        &manifest.project_id,
        &manifest.novel_work_id,
        &legacy.id,
    )?;
    let replay = start_inner(
        conn,
        legacy_input,
        false,
        true,
        "visual-test-session",
        "visual-test-provider",
        "visual-test-model",
        legacy_attempt,
        Some(&ready.id),
    )?;
    if replay.id != legacy.id || replay.compiler_contract_version != COMPILER_CONTRACT_V2 {
        return Err("VISUAL_INTEGRATION_V2_RECEIPT_REPLAY_FAILED".into());
    }
    let page = manifest
        .manifest
        .get("pages")
        .and_then(Value::as_array)
        .and_then(|pages| {
            pages.iter().find(|page| {
                page.get("productionPageId").and_then(Value::as_str) == Some(page_id.as_str())
            })
        })
        .ok_or("VISUAL_INTEGRATION_PAGE_MISSING")?;
    let verified = super::verify_frozen_compiler_request(&legacy, manifest, page)?;
    if verified.get("schemaVersion").and_then(Value::as_str) != Some(COMPILER_CONTRACT_V2)
        || verified
            .get("compilerInput")
            .and_then(|value| value.get("textRenderPlan"))
            .is_some()
    {
        return Err("VISUAL_INTEGRATION_V2_DISPATCH_ROUTE_FAILED".into());
    }
    let generic_child_count_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_page_runs WHERE parent_run_id=?",
            params![legacy.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_INTEGRATION_V2_CHILD_READ_FAILED".to_string())?;
    let generic_retry = super::test_retry_inner(
        conn,
        ComicVisualPageRunRetryInput {
            project_id: manifest.project_id.clone(),
            novel_work_id: manifest.novel_work_id.clone(),
            run_id: legacy.id.clone(),
            idempotency_key: new_id("visual-v2-generic-retry"),
        },
        true,
        "visual-test-session",
        "visual-test-provider",
        "visual-test-model",
    );
    if generic_retry.err().as_deref() != Some("VISUAL_BATCH_MEMBER_RETRY_REQUIRED") {
        return Err("VISUAL_INTEGRATION_V2_GENERIC_GUARD_FAILED".into());
    }
    let generic_child_count_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_page_runs WHERE parent_run_id=?",
            params![legacy.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_INTEGRATION_V2_CHILD_READ_FAILED".to_string())?;
    if generic_child_count_after != generic_child_count_before {
        return Err("VISUAL_INTEGRATION_V2_GENERIC_CHILD_CREATED".into());
    }
    let child_key = new_id("visual-v2-child");
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "VISUAL_INTEGRATION_V2_RETRY_WRITE_FAILED".to_string())?;
    let child = super::retry_failed_child_tx(
        &tx,
        &legacy,
        child_key.clone(),
        "visual-test-session",
        "visual-test-provider",
    )?;
    tx.commit()
        .map_err(|_| "VISUAL_INTEGRATION_V2_RETRY_WRITE_FAILED".to_string())?;
    let replay_tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| "VISUAL_INTEGRATION_V2_RETRY_WRITE_FAILED".to_string())?;
    let replayed_child = super::retry_failed_child_tx(
        &replay_tx,
        &legacy,
        child_key,
        "visual-test-session",
        "visual-test-provider",
    )?;
    replay_tx
        .commit()
        .map_err(|_| "VISUAL_INTEGRATION_V2_RETRY_WRITE_FAILED".to_string())?;
    if replayed_child.id != child.id {
        return Err("VISUAL_INTEGRATION_V2_CHILD_RECEIPT_REPLAY_FAILED".into());
    }
    let child = get_run(
        conn,
        &manifest.project_id,
        &manifest.novel_work_id,
        &child.id,
    )?;
    let child_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comic_visual_page_runs WHERE parent_run_id=?",
            params![legacy.id],
            |row| row.get(0),
        )
        .map_err(|_| "VISUAL_INTEGRATION_V2_CHILD_READ_FAILED".to_string())?;
    let verified_child = super::verify_frozen_compiler_request(&child, manifest, page)?;
    if child.compiler_contract_version != COMPILER_CONTRACT_V2
        || child.parent_run_id.as_deref() != Some(legacy.id.as_str())
        || child.attempt_no != legacy_attempt + 1
        || child.status != "queued"
        || child_count != generic_child_count_before + 1
        || child
            .request_json
            .get("schemaVersion")
            .and_then(Value::as_str)
            != Some(COMPILER_CONTRACT_V2)
        || verified_child.get("schemaVersion").and_then(Value::as_str) != Some(COMPILER_CONTRACT_V2)
    {
        return Err("VISUAL_INTEGRATION_V2_CHILD_ROUTE_FAILED".into());
    }
    conn.execute(
        "UPDATE comic_visual_page_runs SET status='failed',lease_expires_at=NULL,owner_app_session_id=NULL,finished_at=? WHERE id=? AND status='queued'",
        params![crate::novel::now(), child.id],
    )
    .map_err(|_| "VISUAL_INTEGRATION_V2_STATUS_WRITE_FAILED".to_string())?;
    Ok(())
}

fn start_next_attempt(
    conn: &Connection,
    manifest: &ComicVisualManifest,
    parent: &ComicVisualPageRun,
) -> Result<ComicVisualPageRun, String> {
    let page_id = manifest
        .manifest
        .get("pages")
        .and_then(Value::as_array)
        .and_then(|pages| pages.first())
        .and_then(|page| page.get("productionPageId"))
        .and_then(Value::as_str)
        .ok_or("VISUAL_INTEGRATION_PAGE_MISSING")?;
    start_inner(
        conn,
        ComicVisualPageRunStartInput {
            project_id: manifest.project_id.clone(),
            novel_work_id: manifest.novel_work_id.clone(),
            manifest_id: manifest.id.clone(),
            production_page_id: page_id.into(),
            manifest_fingerprint: manifest.manifest_fingerprint.clone(),
            compiler_contract_version: COMPILER_CONTRACT.into(),
            request_json: json!({
                "schemaVersion": COMPILER_CONTRACT,
                "manifestFingerprint": manifest.manifest_fingerprint,
                "productionPageId": page_id,
            }),
            reference_snapshot_json: json!({}),
            idempotency_key: new_id("visual-test-invalid-start"),
        },
        false,
        true,
        "visual-test-session",
        "visual-test-provider",
        "visual-test-model",
        parent.attempt_no + 1,
        Some(&parent.id),
    )
}
