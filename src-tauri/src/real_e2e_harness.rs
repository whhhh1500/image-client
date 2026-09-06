//! Opt-in backend acceptance runner for one real, isolated comic page.
//!
//! It is compiled only with `real-e2e-harness`, and is inert until the exact
//! runtime switch is present.  It intentionally uses the production Tauri
//! commands and workers rather than a fixture, HTTP shortcut, or frontend
//! active-store state.  No credential is copied, printed, or persisted here:
//! the caller must place an already-authorized temporary backend snapshot in
//! the isolated data directory before process startup.

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

use crate::{
    comic_visual::{self, ComicVisualManifest, ComicVisualManifestPrepareInput},
    comic_visual_batch::{
        self, ComicVisualBatch, ComicVisualBatchGetInput, NovelProductionVisualAuthorizeInput,
        VisualImageOptionsInput, VisualOutputInput,
    },
    comic_visual_export::{self, ComicVisualBatchExportInput, ComicVisualExportSelection},
    comic_visual_render::{self, ComicVisualPageRunGetInput},
    config,
    db::DbState,
    novel::{
        self, ComicPlanDialogueIntent, ComicPlanIntent, ComicPlanPageIntent,
        NovelChapterRevisionCreateInput, NovelProductionRetryStageInput, NovelProductionStartInput,
        NovelWorkCreateInput,
    },
    novel_adaptation::{self, ComicApplyOperationInput},
    paths, AppState,
};

const ENABLE_ENV: &str = "IMAGE_CLIENT_REAL_E2E_HARNESS";
const ENABLE_VALUE: &str = "1";
const RESUME_MODE_ENV: &str = "IMAGE_CLIENT_REAL_E2E_MODE";
const RESUME_MODE_VALUE: &str = "resume-stage-retry";
const IMAGE_ONLY_MODE_VALUE: &str = "image-only-continuation";
const RESUME_PROVENANCE_FILE: &str = "real-e2e-resume-stage-retry.provenance.json";
const IMAGE_ONLY_PROVENANCE_FILE: &str = "real-e2e-image-only-continuation.provenance.json";
const D_AUDIT_ROOT: &str = "D:\\cc\\image-client\\.test-tmp";
const RUN_ID: &str = "rain-alley-letter-v1";
const PROJECT_ID: &str = "real-e2e-rain-alley-letter";
const WORK_KEY: &str = "real-e2e:rain-alley-letter:work";
const CHAPTER_KEY: &str = "real-e2e:rain-alley-letter:chapter";
const PRODUCTION_KEY: &str = "real-e2e:rain-alley-letter:production";
const MANIFEST_KEY: &str = "real-e2e:rain-alley-letter:manifest";
const AUTHORIZE_KEY: &str = "real-e2e:rain-alley-letter:authorize";
const EXPORT_KEY: &str = "real-e2e:rain-alley-letter:export";
const MARKER_FILE: &str = "real-e2e-rain-alley-letter.marker.json";
const AUDIT_FILE: &str = "real-e2e-rain-alley-letter.audit.jsonl";
const TEXT_TIMEOUT_SECONDS: u64 = 15 * 60;
const IMAGE_TIMEOUT_SECONDS: u64 = 10 * 60;
const POLL_MILLIS: u64 = 750;

const SAMPLE_DOCUMENT: &str = include_str!("../../docs/小说漫画/验收短篇-雨巷来信.md");
const EXPECTED_DIALOGUES: [(&str, &str); 6] = [
    ("小川", "信不能湿。"),
    ("林青", "先进来。"),
    ("小川", "就是这里！"),
    ("林青", "我等你很久了。"),
    ("小川", "门后是什么？"),
    ("林青", "天亮就知道。"),
];

struct RunProgress {
    stage: Mutex<&'static str>,
}

impl Default for RunProgress {
    fn default() -> Self {
        Self {
            stage: Mutex::new("bootstrap"),
        }
    }
}

impl RunProgress {
    fn advance(&self, audit: &Audit, stage: &'static str) -> Result<(), String> {
        *self
            .stage
            .lock()
            .map_err(|_| "E2E_PROGRESS_UNAVAILABLE".to_string())? = stage;
        audit.event("stage_started", json!({"stage": stage}))
    }

    fn current(&self) -> &'static str {
        self.stage.lock().map(|stage| *stage).unwrap_or("unknown")
    }
}

fn safe_stop_code(error: &str) -> String {
    // A backend/provider error is not safe to persist or display.  Harness
    // errors are deliberately compact ASCII identifiers; everything else is
    // reduced to this one explicit safe terminal code.
    if error.starts_with("E2E_")
        && error
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        error.into()
    } else {
        "E2E_INTERNAL_STOPPED".into()
    }
}

pub(crate) fn requested() -> bool {
    std::env::var(ENABLE_ENV).ok().as_deref() == Some(ENABLE_VALUE)
}

fn resume_stage_retry_requested() -> bool {
    std::env::var(RESUME_MODE_ENV).ok().as_deref() == Some(RESUME_MODE_VALUE)
}

fn image_only_continuation_requested() -> bool {
    std::env::var(RESUME_MODE_ENV).ok().as_deref() == Some(IMAGE_ONLY_MODE_VALUE)
}

/// This gate runs before `paths::ensure_data_dirs` and before SQLite opens.
/// A harness run never creates its own root: the caller must deliberately
/// create a named empty directory and place the temporary config snapshot.
pub(crate) fn validate_bootstrap() -> Result<(), String> {
    if !requested() {
        return Err("E2E_RUNTIME_SWITCH_REQUIRED".into());
    }
    let raw = std::env::var_os("IMAGE_CLIENT_DATA_DIR").ok_or("E2E_ISOLATED_ROOT_REQUIRED")?;
    let root = PathBuf::from(raw);
    if image_only_continuation_requested() {
        validate_image_only_bootstrap_root(&root)
    } else if resume_stage_retry_requested() {
        validate_resume_bootstrap_root(&root)
    } else {
        validate_bootstrap_root(&root)
    }
}

/// Claims the run marker while the root is still pristine.  `run()` happens
/// only after Tauri has opened the isolated database, so it must *not* repeat
/// the pristine-root checks there.
fn validate_bootstrap_root(root: &Path) -> Result<(), String> {
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("E2E_ISOLATED_ROOT_INVALID")?;
    if !root.is_absolute() || !name.starts_with("image-client-real-e2e-") || !root.is_dir() {
        return Err("E2E_ISOLATED_ROOT_REQUIRED".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "E2E_ISOLATED_ROOT_INVALID")?;
    if !root.join("backend-config.json").is_file()
        || root.join("image-client.db").exists()
        || root.join(MARKER_FILE).exists()
        || root.join(AUDIT_FILE).exists()
    {
        return Err("E2E_ISOLATED_ROOT_NOT_PRISTINE".into());
    }
    // Claim before `ensure_data_dirs`/SQLite run.  A crash now deliberately
    // leaves a durable marker, so a later invocation cannot silently reuse a
    // potentially charged or partially migrated root.
    RunLock::claim(&root)?;
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResumeProvenance {
    mode: String,
    project_id: String,
    novel_work_id: String,
    production_job_id: String,
    source_revision_id: String,
    source_analysis_run_id: String,
    invalid_adaptation_run_id: String,
    source_runtime_root: String,
    source_db_sha256: String,
    clone_db_sha256: String,
}

/// Binds an image-only continuation to the exact stopped adaptation retry it
/// clones, and to the earlier stopped source/adaptation run that supplied the
/// original source lineage.  It is intentionally separate from the ordinary
/// stage-retry provenance: this mode has no authority to create text requests.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageOnlyProvenance {
    mode: String,
    project_id: String,
    novel_work_id: String,
    production_job_id: String,
    source_revision_id: String,
    source_analysis_run_id: String,
    invalid_adaptation_run_id: String,
    ready_adaptation_run_id: String,
    source_runtime_root: String,
    source_db_sha256: String,
    clone_db_sha256: String,
    ancestor_root: String,
    ancestor_db_sha256: String,
    ancestor_marker_sha256: String,
    #[serde(default)]
    ancestor_snapshot_root: Option<String>,
    #[serde(default)]
    ancestor_snapshot_db_sha256: Option<String>,
}

fn validate_resume_bootstrap_root(root: &Path) -> Result<(), String> {
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("E2E_ISOLATED_ROOT_INVALID")?;
    if !root.is_absolute() || !name.starts_with("image-client-real-e2e-") || !root.is_dir() {
        return Err("E2E_ISOLATED_ROOT_REQUIRED".into());
    }
    let root = canonical_d_audit_root(root)?;
    if !root.join("backend-config.json").is_file()
        || !root.join("image-client.db").is_file()
        || root.join(MARKER_FILE).exists()
        || root.join(AUDIT_FILE).exists()
    {
        return Err("E2E_RESUME_ROOT_INVALID".into());
    }
    let provenance_path = root.join(RESUME_PROVENANCE_FILE);
    let provenance: ResumeProvenance = serde_json::from_slice(
        &std::fs::read(&provenance_path).map_err(|_| "E2E_RESUME_PROVENANCE_MISSING")?,
    )
    .map_err(|_| "E2E_RESUME_PROVENANCE_INVALID")?;
    if provenance.mode != RESUME_MODE_VALUE
        || [
            &provenance.project_id,
            &provenance.novel_work_id,
            &provenance.production_job_id,
            &provenance.source_revision_id,
            &provenance.source_analysis_run_id,
            &provenance.invalid_adaptation_run_id,
            &provenance.source_runtime_root,
            &provenance.source_db_sha256,
            &provenance.clone_db_sha256,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        return Err("E2E_RESUME_PROVENANCE_INVALID".into());
    }
    let bytes = std::fs::read(root.join("image-client.db"))
        .map_err(|_| "E2E_RESUME_CLONE_HASH_READ_FAILED")?;
    let actual = format!("sha256:{:x}", Sha256::digest(bytes));
    if actual != provenance.clone_db_sha256 {
        return Err("E2E_RESUME_CLONE_HASH_MISMATCH".into());
    }
    RunLock::claim(&root)?;
    Ok(())
}

fn validate_image_only_bootstrap_root(root: &Path) -> Result<(), String> {
    validate_resume_root_layout(root, IMAGE_ONLY_PROVENANCE_FILE)?;
    let root = canonical_d_audit_root(root)?;
    let provenance = read_image_only_provenance(&root)?;
    if provenance.mode != IMAGE_ONLY_MODE_VALUE
        || image_only_fields(&provenance)
            .iter()
            .any(|v| v.trim().is_empty())
        || !optional_pair_is_valid(
            provenance.ancestor_snapshot_root.as_deref(),
            provenance.ancestor_snapshot_db_sha256.as_deref(),
        )
    {
        return Err("E2E_IMAGE_ONLY_PROVENANCE_INVALID".into());
    }
    validate_clone_hash(
        &root,
        &provenance.clone_db_sha256,
        "E2E_IMAGE_ONLY_CLONE_HASH",
    )?;
    // This runs before SQLite is opened in the clone.  It verifies both
    // immutable stopped ancestors without writing either origin database.
    validate_image_only_source_chain(&root, &provenance)?;
    RunLock::claim(&root).map(|_| ())
}

fn validate_resume_root_layout(root: &Path, provenance_file: &str) -> Result<(), String> {
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("E2E_ISOLATED_ROOT_INVALID")?;
    if !root.is_absolute() || !name.starts_with("image-client-real-e2e-") || !root.is_dir() {
        return Err("E2E_ISOLATED_ROOT_REQUIRED".into());
    }
    let canonical = canonical_d_audit_root(root)?;
    if !canonical.join("backend-config.json").is_file()
        || !canonical.join("image-client.db").is_file()
        || canonical.join(MARKER_FILE).exists()
        || canonical.join(AUDIT_FILE).exists()
        || !canonical.join(provenance_file).is_file()
    {
        return Err("E2E_RESUME_ROOT_INVALID".into());
    }
    Ok(())
}

fn image_only_fields(provenance: &ImageOnlyProvenance) -> [&str; 14] {
    [
        &provenance.project_id,
        &provenance.novel_work_id,
        &provenance.production_job_id,
        &provenance.source_revision_id,
        &provenance.source_analysis_run_id,
        &provenance.invalid_adaptation_run_id,
        &provenance.ready_adaptation_run_id,
        &provenance.source_runtime_root,
        &provenance.source_db_sha256,
        &provenance.clone_db_sha256,
        &provenance.ancestor_root,
        &provenance.ancestor_db_sha256,
        &provenance.ancestor_marker_sha256,
        &provenance.mode,
    ]
}

fn optional_pair_is_valid(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => !left.trim().is_empty() && !right.trim().is_empty(),
        _ => false,
    }
}

fn validate_clone_hash(root: &Path, expected: &str, code: &str) -> Result<(), String> {
    let bytes =
        std::fs::read(root.join("image-client.db")).map_err(|_| format!("{code}_READ_FAILED"))?;
    let actual = format!("sha256:{:x}", Sha256::digest(bytes));
    if actual != expected {
        return Err(format!("{code}_MISMATCH"));
    }
    Ok(())
}

fn canonical_d_audit_root(path: &Path) -> Result<PathBuf, String> {
    reject_reparse_ancestors(path)?;
    let audit_root = Path::new(D_AUDIT_ROOT)
        .canonicalize()
        .map_err(|_| "E2E_ISOLATED_ROOT_INVALID")?;
    let root = path
        .canonicalize()
        .map_err(|_| "E2E_ISOLATED_ROOT_INVALID")?;
    if root == audit_root || !root.starts_with(&audit_root) {
        return Err("E2E_ISOLATED_ROOT_REQUIRED".into());
    }
    Ok(root)
}

fn reject_reparse_ancestors(path: &Path) -> Result<(), String> {
    let mut current = Some(path);
    while let Some(item) = current {
        #[cfg(windows)]
        if std::fs::symlink_metadata(item)
            .map_err(|_| "E2E_ISOLATED_ROOT_INVALID")?
            .file_attributes()
            & 0x400
            != 0
        {
            return Err("E2E_ISOLATED_ROOT_REPARSE".into());
        }
        current = item.parent();
    }
    Ok(())
}

pub(crate) fn launch(app: AppHandle) -> Result<(), std::io::Error> {
    if !requested() {
        return Ok(());
    }
    tauri::async_runtime::spawn(async move {
        let root = paths::data_dir();
        let result = async {
            let lock = RunLock::from_bootstrap(&root)?;
            let audit = Audit::create(root.join(AUDIT_FILE))?;
            let progress = RunProgress::default();
            match run(app.clone(), &audit, &progress).await {
                Ok(()) => {
                    audit.event("finished", json!({"runId": RUN_ID}))?;
                    lock.finish("finished")
                }
                Err(error) => {
                    // Never serialize provider/error text: the audit has
                    // only a whitelisted harness code and current stage.
                    audit.event(
                        "stopped",
                        json!({
                            "runId": RUN_ID,
                            "code": safe_stop_code(&error),
                            "stage": progress.current(),
                        }),
                    )?;
                    lock.finish("stopped")?;
                    Err("E2E_STOPPED".to_string())
                }
            }
        }
        .await;
        // A feature-run is a one-shot process.  It never leaves an invisible
        // app or local API server waiting after a completed/stopped audit.
        app.exit(if result.is_ok() { 0 } else { 2 });
    });
    Ok(())
}

async fn run(app: AppHandle, audit: &Audit, progress: &RunProgress) -> Result<(), String> {
    if image_only_continuation_requested() {
        return run_image_only_continuation(app, audit, progress).await;
    }
    if resume_stage_retry_requested() {
        return run_resume_stage_retry(app, audit, progress).await;
    }
    run_fresh(app, audit, progress).await
}

async fn run_fresh(app: AppHandle, audit: &Audit, progress: &RunProgress) -> Result<(), String> {
    progress.advance(audit, "preflight")?;
    let root = validate_isolated_root()?;
    guard_pristine_run(&app)?;
    validate_provider_readiness(&app)?;
    // Claim a separate, durable request-budget audit before any worker can
    // reach a provider.  It is feature+runtime gated and contains no config,
    // prompt, URL, response, or credential material.
    crate::harness_transport::initialize(&root, crate::harness_transport::BudgetProfile::fresh())?;
    audit.event("started", json!({"runId": RUN_ID, "imageRequestBudget": 1}))?;

    progress.advance(audit, "text_input")?;
    let (facts, chapter) = sample_sections()?;
    // Facts and chapter prose are source material.  Acceptance requirements
    // (such as exact dialogue and five panels) deliberately stay out of this
    // prompt material and are checked later against formal production output.
    let source = format!("【固定事实】\n{facts}\n\n【章节正文】\n{chapter}");
    let work = novel::novel_work_create(
        app.state::<DbState>(),
        NovelWorkCreateInput {
            project_id: PROJECT_ID.into(),
            title: "雨巷来信（隔离验收）".into(),
            description: Some("仅用于一次隔离真实服务验收".into()),
            idempotency_key: WORK_KEY.into(),
        },
    )?;
    let revision = novel::novel_chapter_revision_create(
        app.state::<DbState>(),
        NovelChapterRevisionCreateInput {
            project_id: PROJECT_ID.into(),
            novel_work_id: work.id.clone(),
            chapter_id: None,
            volume_id: None,
            sequence_no: Some(1),
            chapter_no: Some(1),
            title: Some("雨巷来信".into()),
            content: source,
            parent_context_revision_id: None,
            asset_id: None,
            // `paste` is the existing user-facing text-input contract.  Do
            // not invent a harness-only source kind that production rejects.
            source_kind: Some("paste".into()),
            idempotency_key: CHAPTER_KEY.into(),
        },
    )?;
    // This is a formal, frozen production intent, not a branch-config or SQL
    // shortcut.  `visualOutput` remains omitted: no image batch/page run can
    // exist before the post-apply five-panel gate below.
    progress.advance(audit, "text_production")?;
    let job = novel::novel_production_start(
        app.state::<DbState>(),
        app.clone(),
        app.state::<AppState>(),
        NovelProductionStartInput {
            project_id: PROJECT_ID.into(),
            novel_work_id: work.id.clone(),
            novel_chapter_id: revision.chapter_id.clone(),
            source_revision_id: revision.id.clone(),
            idempotency_key: PRODUCTION_KEY.into(),
            provider_id: None,
            model_id: None,
            visual_output: None,
            comic_plan_intent: Some(acceptance_comic_plan_intent()),
        },
    )?;
    audit.event(
        "text_production_submitted",
        json!({"productionJobId": job.id, "sourceRevisionId": revision.id}),
    )?;

    let succeeded = wait_for_text(&app, &work.id, &job.id, audit).await?;
    return run_output_stages(app, audit, progress, root, &work.id, succeeded).await;
}

async fn run_resume_stage_retry(
    app: AppHandle,
    audit: &Audit,
    progress: &RunProgress,
) -> Result<(), String> {
    progress.advance(audit, "resume_preflight")?;
    let root = validate_isolated_root()?;
    let provenance = read_resume_provenance(&root)?;
    validate_resume_source_audit(&root, &provenance)?;
    validate_provider_readiness(&app)?;
    crate::harness_transport::initialize(
        &root,
        crate::harness_transport::BudgetProfile::resume_stage_retry(),
    )?;
    let job = novel::novel_production_get(
        app.state::<DbState>(),
        novel::NovelProductionGetInput {
            project_id: provenance.project_id.clone(),
            novel_work_id: provenance.novel_work_id.clone(),
            production_job_id: provenance.production_job_id.clone(),
        },
    )?;
    if provenance.project_id != PROJECT_ID
        || job.source_revision_id != provenance.source_revision_id
        || job.source_analysis_run_id.as_deref() != Some(&provenance.source_analysis_run_id)
        || job.adaptation_analysis_run_id.as_deref() != Some(&provenance.invalid_adaptation_run_id)
        || job.stage != "ensuring_adaptation"
        || !matches!(job.status.as_str(), "error" | "stale")
        || serde_json::to_value(&job.comic_plan_intent).ok()
            != serde_json::to_value(Some(acceptance_comic_plan_intent())).ok()
    {
        return Err("E2E_RESUME_PROVENANCE_MISMATCH".into());
    }
    audit.event("resume_retry_requested", json!({"productionJobId": job.id}))?;
    let retried = novel::novel_production_retry_stage(
        app.state::<DbState>(),
        app.clone(),
        NovelProductionRetryStageInput {
            project_id: provenance.project_id.clone(),
            novel_work_id: provenance.novel_work_id.clone(),
            production_job_id: provenance.production_job_id.clone(),
            stage: "ensuring_adaptation".into(),
            idempotency_key: format!(
                "real-e2e:resume-stage-retry:{}",
                provenance.production_job_id
            ),
        },
    )?;
    progress.advance(audit, "adaptation_retry")?;
    let succeeded = wait_for_text(&app, &provenance.novel_work_id, &retried.id, audit).await?;
    run_output_stages(
        app,
        audit,
        progress,
        root,
        &provenance.novel_work_id,
        succeeded,
    )
    .await
}

async fn run_image_only_continuation(
    app: AppHandle,
    audit: &Audit,
    progress: &RunProgress,
) -> Result<(), String> {
    progress.advance(audit, "image_only_preflight")?;
    let root = canonical_d_audit_root(&validate_isolated_root()?)?;
    let provenance = read_image_only_provenance(&root)?;
    validate_image_only_source_chain(&root, &provenance)?;
    // The clone must still contain the same ready pointer and frozen intent
    // that were checked in the stopped 90b8 source root.  Hashes alone do
    // not establish this after the application has opened the clone.
    validate_persisted_image_only_job(&root, &provenance)?;
    validate_provider_readiness(&app)?;
    // This durable gate is initialized before the public retry command can
    // schedule a worker.  A source or adaptation send is therefore rejected
    // locally before HTTP, even if a future regression misroutes the retry.
    crate::harness_transport::initialize(
        &root,
        crate::harness_transport::BudgetProfile::image_only_continuation(),
    )?;
    let job = novel::novel_production_get(
        app.state::<DbState>(),
        novel::NovelProductionGetInput {
            project_id: provenance.project_id.clone(),
            novel_work_id: provenance.novel_work_id.clone(),
            production_job_id: provenance.production_job_id.clone(),
        },
    )?;
    if provenance.project_id != PROJECT_ID
        || job.source_revision_id != provenance.source_revision_id
        || job.source_analysis_run_id.as_deref() != Some(&provenance.source_analysis_run_id)
        || job.adaptation_analysis_run_id.as_deref() != Some(&provenance.ready_adaptation_run_id)
        || job.stage != "ensuring_adaptation"
        || !matches!(job.status.as_str(), "error" | "stale")
        || job.comic_plan_intent != Some(acceptance_comic_plan_intent())
    {
        return Err("E2E_IMAGE_ONLY_PROVENANCE_MISMATCH".into());
    }
    audit.event(
        "image_only_retry_requested",
        json!({"productionJobId": job.id, "sourceRequestBudget": 0, "adaptationRequestBudget": 0, "imageRequestBudget": 1}),
    )?;
    // This is the public retry command.  Its exact-ready-pointer branch must
    // continue the stored ready adaptation; this harness never invokes a
    // private continuation helper or writes production state directly.
    let retried = novel::novel_production_retry_stage(
        app.state::<DbState>(),
        app.clone(),
        NovelProductionRetryStageInput {
            project_id: provenance.project_id.clone(),
            novel_work_id: provenance.novel_work_id.clone(),
            production_job_id: provenance.production_job_id.clone(),
            stage: "ensuring_adaptation".into(),
            idempotency_key: format!(
                "real-e2e:image-only-continuation:{}",
                provenance.production_job_id
            ),
        },
    )?;
    progress.advance(audit, "image_only_continue")?;
    let succeeded = wait_for_text(&app, &provenance.novel_work_id, &retried.id, audit).await?;
    run_output_stages(
        app,
        audit,
        progress,
        root,
        &provenance.novel_work_id,
        succeeded,
    )
    .await
}

fn read_resume_provenance(root: &Path) -> Result<ResumeProvenance, String> {
    serde_json::from_slice(
        &std::fs::read(root.join(RESUME_PROVENANCE_FILE))
            .map_err(|_| "E2E_RESUME_PROVENANCE_MISSING")?,
    )
    .map_err(|_| "E2E_RESUME_PROVENANCE_INVALID".into())
}

fn read_image_only_provenance(root: &Path) -> Result<ImageOnlyProvenance, String> {
    serde_json::from_slice(
        &std::fs::read(root.join(IMAGE_ONLY_PROVENANCE_FILE))
            .map_err(|_| "E2E_IMAGE_ONLY_PROVENANCE_MISSING")?,
    )
    .map_err(|_| "E2E_IMAGE_ONLY_PROVENANCE_INVALID".into())
}

fn file_sha256(path: &Path, read_code: &str) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|_| read_code.to_string())?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn stopped_marker(root: &Path, missing_code: &str, invalid_code: &str) -> Result<Value, String> {
    let marker: Value = serde_json::from_slice(
        &std::fs::read(root.join(MARKER_FILE)).map_err(|_| missing_code.to_string())?,
    )
    .map_err(|_| invalid_code.to_string())?;
    if marker.get("status").and_then(Value::as_str) != Some("stopped") {
        return Err(invalid_code.into());
    }
    Ok(marker)
}

fn audit_budget_counts(
    root: &Path,
    missing_code: &str,
    invalid_code: &str,
) -> Result<([u8; 3], [u8; 3]), String> {
    let mut reserved = [0_u8; 3];
    let mut received = [0_u8; 3];
    for line in std::fs::read_to_string(root.join("real-e2e-request-budget.audit.jsonl"))
        .map_err(|_| missing_code.to_string())?
        .lines()
    {
        let event: Value = serde_json::from_str(line).map_err(|_| invalid_code.to_string())?;
        let target = match event.get("event").and_then(Value::as_str) {
            Some("generation_post_reserved") => &mut reserved,
            Some("generation_http_received") => &mut received,
            _ => continue,
        };
        let index = match event.get("kind").and_then(Value::as_str) {
            Some("source") => 0,
            Some("adaptation") => 1,
            Some("image") => 2,
            _ => return Err(invalid_code.into()),
        };
        let next = target[index]
            .checked_add(1)
            .ok_or_else(|| invalid_code.to_string())?;
        target[index] = next;
    }
    Ok((reserved, received))
}

fn validate_image_only_source_chain(
    clone_root: &Path,
    provenance: &ImageOnlyProvenance,
) -> Result<(), String> {
    let source = canonical_d_audit_root(Path::new(&provenance.source_runtime_root))?;
    if source == clone_root {
        return Err("E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID".into());
    }
    if file_sha256(
        &source.join("image-client.db"),
        "E2E_IMAGE_ONLY_SOURCE_AUDIT_MISSING",
    )? != provenance.source_db_sha256
    {
        return Err("E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID".into());
    }
    stopped_marker(
        &source,
        "E2E_IMAGE_ONLY_SOURCE_AUDIT_MISSING",
        "E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID",
    )?;
    if audit_budget_counts(
        &source,
        "E2E_IMAGE_ONLY_SOURCE_AUDIT_MISSING",
        "E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID",
    )? != ([0, 1, 0], [0, 1, 0])
    {
        return Err("E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID".into());
    }
    let source_provenance =
        read_resume_provenance(&source).map_err(|_| "E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID")?;
    if source_provenance.mode != RESUME_MODE_VALUE
        || source_provenance.project_id != provenance.project_id
        || source_provenance.novel_work_id != provenance.novel_work_id
        || source_provenance.production_job_id != provenance.production_job_id
        || source_provenance.source_revision_id != provenance.source_revision_id
        || source_provenance.source_analysis_run_id != provenance.source_analysis_run_id
        || source_provenance.invalid_adaptation_run_id != provenance.invalid_adaptation_run_id
    {
        return Err("E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID".into());
    }
    let ancestor = canonical_d_audit_root(Path::new(&provenance.ancestor_root))?;
    let source_ancestor =
        canonical_d_audit_root(Path::new(&source_provenance.source_runtime_root))?;
    if ancestor == clone_root || ancestor == source || source_ancestor != ancestor {
        return Err("E2E_IMAGE_ONLY_ANCESTOR_AUDIT_INVALID".into());
    }
    if file_sha256(
        &ancestor.join(MARKER_FILE),
        "E2E_IMAGE_ONLY_ANCESTOR_AUDIT_MISSING",
    )? != provenance.ancestor_marker_sha256
    {
        return Err("E2E_IMAGE_ONLY_ANCESTOR_AUDIT_INVALID".into());
    }
    stopped_marker(
        &ancestor,
        "E2E_IMAGE_ONLY_ANCESTOR_AUDIT_MISSING",
        "E2E_IMAGE_ONLY_ANCESTOR_AUDIT_INVALID",
    )?;
    if audit_budget_counts(
        &ancestor,
        "E2E_IMAGE_ONLY_ANCESTOR_AUDIT_MISSING",
        "E2E_IMAGE_ONLY_ANCESTOR_AUDIT_INVALID",
    )? != ([1, 1, 0], [1, 1, 0])
    {
        return Err("E2E_IMAGE_ONLY_ANCESTOR_AUDIT_INVALID".into());
    }
    match validate_sealed_ancestor_snapshot(
        clone_root,
        &source,
        &ancestor,
        &source_provenance,
        provenance,
    )? {
        Some(_) => {}
        None => {
            if file_sha256(
                &ancestor.join("image-client.db"),
                "E2E_IMAGE_ONLY_ANCESTOR_AUDIT_MISSING",
            )? != provenance.ancestor_db_sha256
                || source_provenance.source_db_sha256 != provenance.ancestor_db_sha256
            {
                return Err("E2E_IMAGE_ONLY_ANCESTOR_AUDIT_INVALID".into());
            }
            validate_persisted_origin_job(&ancestor, provenance)?;
        }
    }
    validate_persisted_image_only_job(&source, provenance)
}

fn validate_sealed_ancestor_snapshot(
    clone_root: &Path,
    source: &Path,
    ancestor: &Path,
    source_provenance: &ResumeProvenance,
    provenance: &ImageOnlyProvenance,
) -> Result<Option<PathBuf>, String> {
    if !optional_pair_is_valid(
        provenance.ancestor_snapshot_root.as_deref(),
        provenance.ancestor_snapshot_db_sha256.as_deref(),
    ) {
        return Err("E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_INVALID".into());
    }
    let (Some(raw_root), Some(expected_db_hash)) = (
        provenance.ancestor_snapshot_root.as_deref(),
        provenance.ancestor_snapshot_db_sha256.as_deref(),
    ) else {
        return Ok(None);
    };
    let snapshot = canonical_d_audit_root(Path::new(raw_root))?;
    if snapshot == clone_root
        || snapshot == source
        || snapshot == ancestor
        || !snapshot.join("image-client.db").is_file()
        || !snapshot.join(RESUME_PROVENANCE_FILE).is_file()
        || snapshot.join(MARKER_FILE).exists()
        || snapshot.join(AUDIT_FILE).exists()
        || snapshot
            .join("real-e2e-request-budget.audit.jsonl")
            .exists()
    {
        return Err("E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_INVALID".into());
    }
    // A sealed snapshot is a completed SQLite backup.  An uncheckpointed WAL
    // would be outside the hash-pinned main-db bytes, so reject it rather than
    // pretending `immutable=1` makes those logical rows safe to ignore.
    let wal = snapshot.join("image-client.db-wal");
    if wal.exists()
        && wal
            .metadata()
            .map_err(|_| "E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_INVALID")?
            .len()
            != 0
    {
        return Err("E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_INVALID".into());
    }
    let before_db_hash = file_sha256(
        &snapshot.join("image-client.db"),
        "E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_MISSING",
    )?;
    if before_db_hash != expected_db_hash {
        return Err("E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_INVALID".into());
    }
    let sealed: ResumeProvenance = read_resume_provenance(&snapshot)
        .map_err(|_| "E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_INVALID")?;
    let sealed_root = canonical_d_audit_root(Path::new(&sealed.source_runtime_root))?;
    if sealed.mode != RESUME_MODE_VALUE
        || sealed_root != ancestor
        || sealed.project_id != provenance.project_id
        || sealed.novel_work_id != provenance.novel_work_id
        || sealed.production_job_id != provenance.production_job_id
        || sealed.source_revision_id != provenance.source_revision_id
        || sealed.source_analysis_run_id != provenance.source_analysis_run_id
        || sealed.invalid_adaptation_run_id != provenance.invalid_adaptation_run_id
        || sealed.source_db_sha256 != source_provenance.source_db_sha256
        || sealed.clone_db_sha256 != expected_db_hash
    {
        return Err("E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_INVALID".into());
    }
    // `validate_persisted_origin_job` opens the sealed backup read-only.  Hash
    // it once more afterwards so any unexpected physical change fails closed.
    validate_persisted_origin_job(&snapshot, provenance)?;
    if file_sha256(
        &snapshot.join("image-client.db"),
        "E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_MISSING",
    )? != before_db_hash
        || (wal.exists()
            && wal
                .metadata()
                .map_err(|_| "E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_INVALID")?
                .len()
                != 0)
    {
        return Err("E2E_IMAGE_ONLY_ANCESTOR_SNAPSHOT_INVALID".into());
    }
    Ok(Some(snapshot))
}

fn validate_persisted_origin_job(
    ancestor: &Path,
    provenance: &ImageOnlyProvenance,
) -> Result<(), String> {
    let conn = rusqlite::Connection::open_with_flags(
        ancestor.join("image-client.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|_| "E2E_IMAGE_ONLY_ANCESTOR_AUDIT_INVALID")?;
    let record: (String, String, String, String, Option<String>) = conn
        .query_row(
            "SELECT project_id,novel_work_id,source_revision_id,source_analysis_run_id,adaptation_analysis_run_id FROM novel_production_jobs WHERE id=?",
            [&provenance.production_job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .map_err(|_| "E2E_IMAGE_ONLY_ANCESTOR_AUDIT_INVALID")?;
    if record.0 != provenance.project_id
        || record.1 != provenance.novel_work_id
        || record.2 != provenance.source_revision_id
        || record.3 != provenance.source_analysis_run_id
        || record.4.as_deref() != Some(&provenance.invalid_adaptation_run_id)
    {
        return Err("E2E_IMAGE_ONLY_ANCESTOR_AUDIT_INVALID".into());
    }
    Ok(())
}

fn validate_persisted_image_only_job(
    source: &Path,
    provenance: &ImageOnlyProvenance,
) -> Result<(), String> {
    let conn = rusqlite::Connection::open_with_flags(
        source.join("image-client.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|_| "E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID")?;
    let record: (String, String, String, String, Option<String>, String, String, Option<String>) = conn
        .query_row(
            "SELECT project_id,novel_work_id,source_revision_id,source_analysis_run_id,adaptation_analysis_run_id,status,stage,comic_plan_intent_json FROM novel_production_jobs WHERE id=?",
            [&provenance.production_job_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
        )
        .map_err(|_| "E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID")?;
    let intent: Option<ComicPlanIntent> = record
        .7
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| "E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID")?;
    if record.0 != provenance.project_id
        || record.1 != provenance.novel_work_id
        || record.2 != provenance.source_revision_id
        || record.3 != provenance.source_analysis_run_id
        || record.4.as_deref() != Some(&provenance.ready_adaptation_run_id)
        || record.6 != "ensuring_adaptation"
        || !matches!(record.5.as_str(), "error" | "stale")
        || intent != Some(acceptance_comic_plan_intent())
    {
        return Err("E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID".into());
    }
    let (run_work, run_status, prompt_version, inputs, outputs, has_frozen_event):
        (String, String, String, i64, i64, i64) = conn
        .query_row(
            "SELECT novel_work_id,status,prompt_version,(SELECT COUNT(*) FROM adaptation_analysis_run_inputs WHERE adaptation_analysis_run_id=run.id),(SELECT COUNT(*) FROM adaptation_analysis_run_artifacts WHERE adaptation_analysis_run_id=run.id),(SELECT COUNT(*) FROM adaptation_analysis_run_events WHERE adaptation_analysis_run_id=run.id AND seq=1) FROM adaptation_analysis_runs run WHERE id=?",
            [&provenance.ready_adaptation_run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .map_err(|_| "E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID")?;
    if run_work != provenance.novel_work_id
        || run_status != "ready_for_review"
        || prompt_version != "adaptation-analysis.v5"
        || inputs != 10
        || outputs != 4
        || has_frozen_event != 1
    {
        return Err("E2E_IMAGE_ONLY_SOURCE_AUDIT_INVALID".into());
    }
    Ok(())
}

fn validate_resume_source_audit(
    clone_root: &Path,
    provenance: &ResumeProvenance,
) -> Result<(), String> {
    let source = canonical_d_audit_root(Path::new(&provenance.source_runtime_root))?;
    if source == clone_root {
        return Err("E2E_RESUME_SOURCE_AUDIT_INVALID".into());
    }
    let source_bytes = std::fs::read(source.join("image-client.db"))
        .map_err(|_| "E2E_RESUME_SOURCE_AUDIT_MISSING")?;
    if format!("sha256:{:x}", Sha256::digest(source_bytes)) != provenance.source_db_sha256 {
        return Err("E2E_RESUME_SOURCE_AUDIT_INVALID".into());
    }
    let marker: Value = serde_json::from_slice(
        &std::fs::read(source.join(MARKER_FILE)).map_err(|_| "E2E_RESUME_SOURCE_AUDIT_MISSING")?,
    )
    .map_err(|_| "E2E_RESUME_SOURCE_AUDIT_INVALID")?;
    if marker.get("status").and_then(Value::as_str) != Some("stopped") {
        return Err("E2E_RESUME_SOURCE_AUDIT_INVALID".into());
    }
    let source_conn = rusqlite::Connection::open_with_flags(
        source.join("image-client.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|_| "E2E_RESUME_SOURCE_AUDIT_INVALID")?;
    let origin: (String, String, String, String, String, String) = source_conn
        .query_row(
            "SELECT id,project_id,novel_work_id,source_revision_id,source_analysis_run_id,adaptation_analysis_run_id FROM novel_production_jobs WHERE id=?",
            [&provenance.production_job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .map_err(|_| "E2E_RESUME_SOURCE_AUDIT_INVALID")?;
    if origin.0 != provenance.production_job_id
        || origin.1 != provenance.project_id
        || origin.2 != provenance.novel_work_id
        || origin.3 != provenance.source_revision_id
        || origin.4 != provenance.source_analysis_run_id
        || origin.5 != provenance.invalid_adaptation_run_id
    {
        return Err("E2E_RESUME_SOURCE_AUDIT_INVALID".into());
    }
    let mut reserved = [0_u8; 3];
    let mut received = [0_u8; 3];
    for line in std::fs::read_to_string(source.join("real-e2e-request-budget.audit.jsonl"))
        .map_err(|_| "E2E_RESUME_SOURCE_AUDIT_MISSING")?
        .lines()
    {
        let event: Value =
            serde_json::from_str(line).map_err(|_| "E2E_RESUME_SOURCE_AUDIT_INVALID")?;
        let target = match event.get("event").and_then(Value::as_str) {
            Some("generation_post_reserved") => &mut reserved,
            Some("generation_http_received") => &mut received,
            _ => continue,
        };
        match event.get("kind").and_then(Value::as_str) {
            Some("source") => target[0] += 1,
            Some("adaptation") => target[1] += 1,
            Some("image") => target[2] += 1,
            _ => return Err("E2E_RESUME_SOURCE_AUDIT_INVALID".into()),
        }
    }
    if reserved != [1, 1, 0] || received != [1, 1, 0] {
        return Err("E2E_RESUME_SOURCE_AUDIT_INVALID".into());
    }
    Ok(())
}

async fn run_output_stages(
    app: AppHandle,
    audit: &Audit,
    progress: &RunProgress,
    root: PathBuf,
    work_id: &str,
    succeeded: novel::NovelProductionJob,
) -> Result<(), String> {
    let adaptation_id = succeeded
        .default_adaptation_id
        .as_deref()
        .ok_or("E2E_APPLY_ADAPTATION_MISSING")?;
    let operation_id = succeeded
        .apply_operation_id
        .as_deref()
        .ok_or("E2E_APPLY_RECEIPT_MISSING")?;
    progress.advance(audit, "apply_receipt")?;
    let receipt = novel_adaptation::novel_comic_apply_get_receipt(
        app.state::<DbState>(),
        ComicApplyOperationInput {
            project_id: PROJECT_ID.into(),
            novel_work_id: work_id.into(),
            comic_adaptation_id: adaptation_id.into(),
            operation_id: operation_id.into(),
        },
    )?;
    if receipt.status != "succeeded" {
        return Err("E2E_APPLY_NOT_SUCCEEDED".into());
    }
    let chapters = receipt
        .entity_map
        .iter()
        .filter(|entry| entry.entity_kind == "production_chapter")
        .collect::<Vec<_>>();
    if chapters.len() != 1 {
        return Err("E2E_EXPECTED_EXACTLY_ONE_PRODUCTION_CHAPTER".into());
    }
    progress.advance(audit, "manifest_gate")?;
    let manifest = comic_visual::comic_visual_manifest_prepare(
        app.state::<DbState>(),
        ComicVisualManifestPrepareInput {
            project_id: PROJECT_ID.into(),
            novel_work_id: work_id.into(),
            comic_adaptation_id: adaptation_id.into(),
            apply_operation_id: operation_id.into(),
            production_chapter_id: chapters[0].entity_id.clone(),
            idempotency_key: MANIFEST_KEY.into(),
        },
    )?;
    assert_one_irregular_five_panel_page(&manifest)?;
    audit.event("image_gate_passed", json!({"manifestId":manifest.id,"productionChapterId":manifest.production_chapter_id,"pageCount":1,"panelCount":5}))?;
    progress.advance(audit, "image_authorization")?;
    let batch = comic_visual_batch::novel_production_visual_authorize(
        app.clone(),
        app.state::<DbState>(),
        app.state::<AppState>(),
        NovelProductionVisualAuthorizeInput {
            project_id: PROJECT_ID.into(),
            novel_work_id: work_id.into(),
            production_job_id: succeeded.id.clone(),
            visual_output: VisualOutputInput {
                target: "comic_pages".into(),
                image_options: Some(VisualImageOptionsInput {
                    model: None,
                    size: Some("1024x1536".into()),
                }),
            },
            idempotency_key: AUTHORIZE_KEY.into(),
        },
    )?;
    audit.event(
        "image_authorized",
        json!({"batchId":batch.id,"imageRequestBudget":1}),
    )?;
    progress.advance(audit, "image_candidate_wait")?;
    let ready_batch = wait_for_single_candidate(&app, work_id, &succeeded.id, audit).await?;
    let member = ready_batch
        .members
        .first()
        .ok_or("E2E_CANDIDATE_MEMBER_MISSING")?;
    let run_id = member
        .page_run_id
        .as_deref()
        .ok_or("E2E_CANDIDATE_RUN_MISSING")?;
    let run = comic_visual_render::comic_visual_page_run_get(
        app.state::<DbState>(),
        ComicVisualPageRunGetInput {
            project_id: PROJECT_ID.into(),
            novel_work_id: work_id.into(),
            run_id: run_id.into(),
        },
    )?;
    let asset_id = run
        .asset_id
        .as_deref()
        .ok_or("E2E_CANDIDATE_ASSET_MISSING")?;
    audit.event("image_candidate_ready", json!({"batchId":ready_batch.id,"runId":run.id,"assetId":asset_id,"providerRequestId":run.provider_request_id}))?;
    progress.advance(audit, "export")?;
    let export_destination = create_export_destination(&root)?;
    let export = comic_visual_export::comic_visual_batch_export(
        app.clone(),
        ComicVisualBatchExportInput {
            project_id: PROJECT_ID.into(),
            novel_work_id: work_id.into(),
            batch_id: ready_batch.id.clone(),
            selections: vec![ComicVisualExportSelection {
                member_id: member.member_id.clone(),
                run_id: run.id.clone(),
                asset_id: asset_id.into(),
            }],
            destination_dir: export_destination.to_string_lossy().into_owned(),
            idempotency_key: EXPORT_KEY.into(),
        },
    )
    .await?;
    if export.files.len() != 1 {
        return Err("E2E_EXPORT_FILE_COUNT_INVALID".into());
    }
    audit.event(
        "export_ready",
        json!({"exportId":export.export_id,"fileCount":1}),
    )?;
    Ok(())
}

fn create_export_destination(root: &Path) -> Result<PathBuf, String> {
    let destination = root.join("real-e2e-export");
    std::fs::create_dir(&destination).map_err(|_| "E2E_EXPORT_DESTINATION_CREATE_FAILED")?;
    Ok(destination)
}

async fn wait_for_text(
    app: &AppHandle,
    work_id: &str,
    job_id: &str,
    audit: &Audit,
) -> Result<novel::NovelProductionJob, String> {
    for _ in 0..(TEXT_TIMEOUT_SECONDS * 1_000 / POLL_MILLIS) {
        let job = novel::novel_production_get(
            app.state::<DbState>(),
            novel::NovelProductionGetInput {
                project_id: PROJECT_ID.into(),
                novel_work_id: work_id.into(),
                production_job_id: job_id.into(),
            },
        )?;
        if job.status == "succeeded" {
            return Ok(job);
        }
        if matches!(
            job.status.as_str(),
            "error" | "blocked_config" | "blocked_conflict" | "needs_rebase"
        ) {
            audit.event(
                "text_production_stopped",
                json!({"status": job.status, "stage": job.stage}),
            )?;
            return Err("E2E_TEXT_PRODUCTION_STOPPED".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(POLL_MILLIS)).await;
    }
    Err("E2E_TEXT_TIMEOUT".into())
}

async fn wait_for_single_candidate(
    app: &AppHandle,
    work_id: &str,
    job_id: &str,
    audit: &Audit,
) -> Result<ComicVisualBatch, String> {
    for _ in 0..(IMAGE_TIMEOUT_SECONDS * 1_000 / POLL_MILLIS) {
        let batch = comic_visual_batch::comic_visual_batch_get(
            app.state::<DbState>(),
            ComicVisualBatchGetInput {
                project_id: PROJECT_ID.into(),
                novel_work_id: work_id.into(),
                production_job_id: job_id.into(),
            },
        )?
        .ok_or("E2E_AUTHORIZED_BATCH_MISSING")?;
        if batch.total_members > 1 || batch.members.len() > 1 {
            return Err("E2E_IMAGE_PAGE_BUDGET_EXCEEDED".into());
        }
        if batch.status == "candidate_ready" {
            if batch.total_members == 1 && batch.members.len() == 1 {
                return Ok(batch);
            }
            return Err("E2E_CANDIDATE_MEMBER_COUNT_INVALID".into());
        }
        if matches!(
            batch.status.as_str(),
            "needs_reconcile" | "failed" | "blocked_config" | "blocked_stale"
        ) {
            audit.event("image_production_stopped", json!({"status": batch.status}))?;
            return Err("E2E_IMAGE_PRODUCTION_STOPPED".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(POLL_MILLIS)).await;
    }
    Err("E2E_IMAGE_TIMEOUT".into())
}

fn validate_isolated_root() -> Result<PathBuf, String> {
    validate_runtime_root(&paths::data_dir())
}

/// Runtime validation deliberately accepts the marker, audit, and SQLite file
/// created after bootstrap.  It proves only that the already-claimed process
/// remains inside its named isolated root.
fn validate_runtime_root(root: &Path) -> Result<PathBuf, String> {
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("E2E_ISOLATED_ROOT_INVALID")?;
    if !root.is_absolute() || !name.starts_with("image-client-real-e2e-") {
        return Err("E2E_ISOLATED_ROOT_REQUIRED".into());
    }
    if !root.is_dir() || !root.join("backend-config.json").is_file() {
        return Err("E2E_ISOLATED_CONFIG_REQUIRED".into());
    }
    Ok(root.to_path_buf())
}

fn guard_pristine_run(app: &AppHandle) -> Result<(), String> {
    let existing: i64 = crate::db::with_connection(&app.state::<DbState>(), |conn| {
        conn.query_row(
            "SELECT (SELECT COUNT(*) FROM novel_works) + (SELECT COUNT(*) FROM novel_production_jobs) + (SELECT COUNT(*) FROM comic_visual_batches) + (SELECT COUNT(*) FROM comic_visual_page_runs) + (SELECT COUNT(*) FROM assets)",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "E2E_EXISTING_RUN_QUERY_FAILED".to_string())
    })?;
    if existing != 0 {
        return Err("E2E_EXISTING_PRODUCTION_DATA".into());
    }
    Ok(())
}

fn validate_provider_readiness(app: &AppHandle) -> Result<(), String> {
    let cfg = app
        .state::<AppState>()
        .cfg
        .read()
        .map_err(|_| "E2E_CONFIG_UNAVAILABLE")?
        .clone();
    let status = config::ConfigState::status(&cfg);
    if !status.llm_ready {
        return Err("E2E_LLM_CONFIG_REQUIRED".into());
    }
    if !status.image_ready {
        return Err("E2E_IMAGE_CONFIG_REQUIRED".into());
    }
    if status.llm_model.trim().is_empty() || status.image_model.trim().is_empty() {
        return Err("E2E_MODEL_REQUIRED".into());
    }
    Ok(())
}

fn sample_sections() -> Result<(String, String), String> {
    let facts = markdown_section("## 固定事实", "## 章节正文")?;
    let chapter = markdown_section("## 章节正文", "## 逐字核对台词")?;
    Ok((facts, chapter))
}

fn acceptance_comic_plan_intent() -> ComicPlanIntent {
    let panel_numbers = [1_i64, 2, 3, 4, 5, 5];
    ComicPlanIntent {
        pages: vec![ComicPlanPageIntent {
            panel_count: 5,
            layout_profile: Some("hero_middle_5".into()),
            dialogues: Some(
                EXPECTED_DIALOGUES
                    .iter()
                    .zip(panel_numbers)
                    .map(|((speaker, text), panel_no)| ComicPlanDialogueIntent {
                        panel_no,
                        speaker: (*speaker).into(),
                        text: (*text).into(),
                    })
                    .collect(),
            ),
        }],
    }
}

fn markdown_section(start: &str, end: &str) -> Result<String, String> {
    let (_, after_start) = SAMPLE_DOCUMENT
        .split_once(start)
        .ok_or("E2E_SAMPLE_DOCUMENT_INVALID")?;
    let (section, _) = after_start
        .split_once(end)
        .ok_or("E2E_SAMPLE_DOCUMENT_INVALID")?;
    let value = section.trim();
    if value.is_empty() {
        return Err("E2E_SAMPLE_DOCUMENT_INVALID".into());
    }
    Ok(value.into())
}

fn assert_one_irregular_five_panel_page(manifest: &ComicVisualManifest) -> Result<(), String> {
    if manifest.freshness != "ready" {
        return Err("E2E_MANIFEST_NOT_FRESH".into());
    }
    let pages = manifest
        .manifest
        .get("pages")
        .and_then(Value::as_array)
        .ok_or("E2E_MANIFEST_PAGES_INVALID")?;
    if pages.len() != 1 {
        return Err("E2E_EXPECTED_EXACTLY_ONE_PAGE".into());
    }
    let page = &pages[0];
    if page.get("pageNo").and_then(Value::as_i64) != Some(1) {
        return Err("E2E_EXPECTED_FIRST_PAGE".into());
    }
    let layout = page.get("layout").ok_or("E2E_LAYOUT_MISSING")?;
    let panels = page
        .get("panels")
        .and_then(Value::as_array)
        .ok_or("E2E_PANELS_MISSING")?;
    if panels.len() != 5 || layout.get("panelCount").and_then(Value::as_u64) != Some(5) {
        return Err("E2E_EXPECTED_FIVE_PANELS".into());
    }
    let expected_reading_order = [1_i64, 2, 3, 4, 5];
    let reading_order = layout
        .get("readingOrder")
        .and_then(Value::as_array)
        .ok_or("E2E_READING_ORDER_INVALID")?;
    if reading_order.len() != expected_reading_order.len()
        || reading_order
            .iter()
            .zip(expected_reading_order)
            .any(|(value, expected)| value.as_i64() != Some(expected))
    {
        return Err("E2E_READING_ORDER_INVALID".into());
    }
    crate::novel_adaptation::validate_page_layout(layout, panels)
        .map_err(|_| "E2E_LAYOUT_GEOMETRY_INVALID".to_string())?;
    let irregular = matches!(
        layout.get("templateId").and_then(Value::as_str),
        Some(
            "reference_story_5"
                | "hero_middle_5"
                | "diagonal_action_5"
                | "detail_to_wide_5"
                | "custom_irregular"
        )
    ) || layout.get("layoutKind").and_then(Value::as_str)
        == Some("custom_irregular");
    if !irregular {
        return Err("E2E_IRREGULAR_LAYOUT_REQUIRED".into());
    }
    if !has_diagonal_edge(layout)? || !has_top_middle_bottom_five_shape(layout)? {
        return Err("E2E_IRREGULAR_GEOMETRY_REQUIRED".into());
    }
    let expected_per_panel = [1_usize, 1, 1, 1, 2];
    let mut expected_offset = 0_usize;
    for (panel_no, expected_count) in expected_reading_order.into_iter().zip(expected_per_panel) {
        let panel = panels
            .iter()
            .find(|panel| panel.get("panelNo").and_then(Value::as_i64) == Some(panel_no))
            .ok_or("E2E_PANEL_SEQUENCE_INVALID")?;
        let dialogues = panel
            .get("spec")
            .and_then(|spec| spec.get("dialogues"))
            .and_then(Value::as_array)
            .ok_or("E2E_DIALOGUE_PANEL_ALLOCATION_REQUIRED")?;
        if dialogues.len() != expected_count {
            return Err("E2E_DIALOGUE_PANEL_ALLOCATION_REQUIRED".into());
        }
        for (dialogue, expected) in dialogues
            .iter()
            .zip(&EXPECTED_DIALOGUES[expected_offset..expected_offset + expected_count])
        {
            if dialogue.get("speaker").and_then(Value::as_str) != Some(expected.0)
                || dialogue.get("text").and_then(Value::as_str) != Some(expected.1)
            {
                return Err("E2E_DIALOGUE_EXACT_MATCH_REQUIRED".into());
            }
        }
        expected_offset += expected_count;
    }
    Ok(())
}

fn geometry_panel(layout: &Value, panel_no: i64) -> Result<&Value, String> {
    layout
        .get("geometry")
        .and_then(|geometry| geometry.get("panels"))
        .and_then(Value::as_array)
        .and_then(|panels| {
            panels
                .iter()
                .find(|panel| panel.get("panelNo").and_then(Value::as_i64) == Some(panel_no))
        })
        .ok_or("E2E_LAYOUT_GEOMETRY_INVALID".into())
}

fn has_diagonal_edge(layout: &Value) -> Result<bool, String> {
    for panel_no in 1..=5 {
        let points = geometry_panel(layout, panel_no)?
            .get("polygon")
            .and_then(Value::as_array)
            .ok_or("E2E_LAYOUT_GEOMETRY_INVALID")?;
        for pair in points
            .iter()
            .zip(points.iter().cycle().skip(1))
            .take(points.len())
        {
            let (left, right) = pair;
            let dx = left
                .get("x")
                .and_then(Value::as_f64)
                .ok_or("E2E_LAYOUT_GEOMETRY_INVALID")?
                - right
                    .get("x")
                    .and_then(Value::as_f64)
                    .ok_or("E2E_LAYOUT_GEOMETRY_INVALID")?;
            let dy = left
                .get("y")
                .and_then(Value::as_f64)
                .ok_or("E2E_LAYOUT_GEOMETRY_INVALID")?
                - right
                    .get("y")
                    .and_then(Value::as_f64)
                    .ok_or("E2E_LAYOUT_GEOMETRY_INVALID")?;
            if dx.abs() > 0.000_001 && dy.abs() > 0.000_001 {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn has_top_middle_bottom_five_shape(layout: &Value) -> Result<bool, String> {
    let bounds = |number| -> Result<(f64, f64), String> {
        let bounds = geometry_panel(layout, number)?
            .get("bounds")
            .ok_or("E2E_LAYOUT_GEOMETRY_INVALID")?;
        Ok((
            bounds
                .get("y")
                .and_then(Value::as_f64)
                .ok_or("E2E_LAYOUT_GEOMETRY_INVALID")?,
            bounds
                .get("width")
                .and_then(Value::as_f64)
                .ok_or("E2E_LAYOUT_GEOMETRY_INVALID")?,
        ))
    };
    let (top_one_y, _) = bounds(1)?;
    let (top_two_y, _) = bounds(2)?;
    let (middle_y, middle_width) = bounds(3)?;
    let (bottom_four_y, _) = bounds(4)?;
    let (bottom_five_y, _) = bounds(5)?;
    Ok(top_one_y < middle_y
        && top_two_y < middle_y
        && middle_width >= 0.8
        && bottom_four_y > middle_y
        && bottom_five_y > middle_y)
}

struct RunLock {
    path: PathBuf,
}

impl RunLock {
    fn claim(root: &Path) -> Result<Self, String> {
        let path = root.join(MARKER_FILE);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "E2E_EXISTING_RUN_MARKER".to_string())?;
        serde_json::to_writer(&mut file, &json!({"runId": RUN_ID, "status":"claimed"}))
            .map_err(|_| "E2E_MARKER_WRITE_FAILED".to_string())?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_all())
            .map_err(|_| "E2E_MARKER_WRITE_FAILED".to_string())?;
        Ok(Self { path })
    }

    fn from_bootstrap(root: &Path) -> Result<Self, String> {
        let path = root.join(MARKER_FILE);
        if !path.is_file() {
            return Err("E2E_RUN_MARKER_MISSING".into());
        }
        Ok(Self { path })
    }

    fn finish(self, status: &str) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(self.path)
            .map_err(|_| "E2E_MARKER_WRITE_FAILED".to_string())?;
        serde_json::to_writer(&mut file, &json!({"runId": RUN_ID, "status": status}))
            .map_err(|_| "E2E_MARKER_WRITE_FAILED".to_string())?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_all())
            .map_err(|_| "E2E_MARKER_WRITE_FAILED".to_string())
    }
}

struct Audit {
    path: PathBuf,
}

impl Audit {
    fn create(path: PathBuf) -> Result<Self, String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "E2E_AUDIT_WRITE_FAILED".to_string())?;
        file.sync_all()
            .map_err(|_| "E2E_AUDIT_WRITE_FAILED".to_string())?;
        Ok(Self { path })
    }

    fn event(&self, kind: &str, payload: Value) -> Result<(), String> {
        let line = json!({"runId": RUN_ID, "event": kind, "payload": payload});
        let serialized = serde_json::to_string(&line).map_err(|_| "E2E_AUDIT_WRITE_FAILED")?;
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|_| "E2E_AUDIT_WRITE_FAILED")?;
        writeln!(file, "{serialized}")
            .and_then(|_| file.sync_data())
            .map_err(|_| "E2E_AUDIT_WRITE_FAILED".to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};

    use super::{
        acceptance_comic_plan_intent, assert_one_irregular_five_panel_page, sample_sections,
        create_export_destination, validate_bootstrap_root, validate_runtime_root,
        validate_sealed_ancestor_snapshot, Audit,
        ComicVisualManifest, ImageOnlyProvenance, ResumeProvenance, RunLock, AUDIT_FILE,
        EXPECTED_DIALOGUES, MARKER_FILE, RESUME_PROVENANCE_FILE,
    };
    #[cfg(windows)]
    use super::{canonical_d_audit_root, D_AUDIT_ROOT};

    fn pristine_root() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "image-client-real-e2e-bootstrap-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("backend-config.json"), b"{}").unwrap();
        root
    }

    #[cfg(windows)]
    fn d_audit_child(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::path::Path::new(D_AUDIT_ROOT).join(format!(
            "image-client-real-e2e-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    fn manifest(dialogues: Vec<(&str, &str)>) -> ComicVisualManifest {
        ComicVisualManifest {
            id: "manifest".into(),
            contract_version: "comic-visual.v1".into(),
            project_id: "project".into(),
            novel_work_id: "work".into(),
            comic_adaptation_id: "adaptation".into(),
            apply_operation_id: "apply".into(),
            production_chapter_id: "chapter".into(),
            page_panel_plan_revision_id: "revision".into(),
            freshness: "ready".into(),
            manifest_fingerprint: "fingerprint".into(),
            created_at: 1,
            manifest: json!({"pages":[{"pageNo":1,"layout":{
                "templateId":"hero_middle_5","layoutKind":"template","panelCount":5,"readingOrder":[1,2,3,4,5],"dominantPanel":3,
                "geometry":{"coordinateSystem":"normalized-0-1","panelCount":5,"readingOrder":[1,2,3,4,5],"gutter":0.012,"safeArea":{"x":0.04,"y":0.04,"width":0.92,"height":0.92},"panels":[
                    {"panelNo":1,"polygon":[{"x":0.04,"y":0.04},{"x":0.59,"y":0.04},{"x":0.54,"y":0.25},{"x":0.04,"y":0.25}],"bounds":{"x":0.04,"y":0.04,"width":0.55,"height":0.21},"textZone":{"x":0.08,"y":0.08,"width":0.34,"height":0.1}},
                    {"panelNo":2,"polygon":[{"x":0.61,"y":0.04},{"x":0.96,"y":0.04},{"x":0.96,"y":0.25},{"x":0.56,"y":0.25}],"bounds":{"x":0.56,"y":0.04,"width":0.40,"height":0.21},"textZone":{"x":0.67,"y":0.08,"width":0.2,"height":0.1}},
                    {"panelNo":3,"polygon":[{"x":0.04,"y":0.27},{"x":0.96,"y":0.27},{"x":0.96,"y":0.66},{"x":0.04,"y":0.66}],"bounds":{"x":0.04,"y":0.27,"width":0.92,"height":0.39},"textZone":{"x":0.16,"y":0.37,"width":0.62,"height":0.12}},
                    {"panelNo":4,"polygon":[{"x":0.04,"y":0.68},{"x":0.43,"y":0.68},{"x":0.43,"y":0.96},{"x":0.04,"y":0.96}],"bounds":{"x":0.04,"y":0.68,"width":0.39,"height":0.28},"textZone":{"x":0.08,"y":0.75,"width":0.23,"height":0.1}},
                    {"panelNo":5,"polygon":[{"x":0.45,"y":0.68},{"x":0.96,"y":0.68},{"x":0.96,"y":0.96},{"x":0.45,"y":0.96}],"bounds":{"x":0.45,"y":0.68,"width":0.51,"height":0.28},"textZone":{"x":0.57,"y":0.75,"width":0.28,"height":0.1}}
                ]}},"panels":[
                {"panelNo":1,"spec":{"dialogues":[{"speaker":dialogues[0].0,"text":dialogues[0].1}]}},
                {"panelNo":2,"spec":{"dialogues":[{"speaker":dialogues[1].0,"text":dialogues[1].1}]}},
                {"panelNo":3,"spec":{"dialogues":[{"speaker":dialogues[2].0,"text":dialogues[2].1}]}},
                {"panelNo":4,"spec":{"dialogues":[{"speaker":dialogues[3].0,"text":dialogues[3].1}]}},
                {"panelNo":5,"spec":{"dialogues":[{"speaker":dialogues[4].0,"text":dialogues[4].1},{"speaker":dialogues[5].0,"text":dialogues[5].1}]}}
            ]}]}),
        }
    }

    #[cfg(windows)]
    fn write_snapshot_job(path: &std::path::Path, adaptation_run_id: &str) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE novel_production_jobs (id TEXT PRIMARY KEY,project_id TEXT,novel_work_id TEXT,source_revision_id TEXT,source_analysis_run_id TEXT,adaptation_analysis_run_id TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO novel_production_jobs VALUES ('job','project','work','revision','source-run',?)",
            [adaptation_run_id],
        )
        .unwrap();
    }

    #[cfg(windows)]
    fn test_image_only_provenance(snapshot: &std::path::Path) -> ImageOnlyProvenance {
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(fs::read(snapshot.join("image-client.db")).unwrap())
        );
        ImageOnlyProvenance {
            mode: "image-only-continuation".into(),
            project_id: "project".into(),
            novel_work_id: "work".into(),
            production_job_id: "job".into(),
            source_revision_id: "revision".into(),
            source_analysis_run_id: "source-run".into(),
            invalid_adaptation_run_id: "invalid-run".into(),
            ready_adaptation_run_id: "ready-run".into(),
            source_runtime_root: "source-runtime-unused-here".into(),
            source_db_sha256: "sha256:source".into(),
            clone_db_sha256: "sha256:clone".into(),
            ancestor_root: "ancestor-unused-here".into(),
            ancestor_db_sha256: "sha256:ancestor".into(),
            ancestor_marker_sha256: "sha256:marker".into(),
            ancestor_snapshot_root: Some(snapshot.to_string_lossy().into_owned()),
            ancestor_snapshot_db_sha256: Some(digest),
        }
    }

    #[cfg(windows)]
    fn write_snapshot_resume_provenance(
        snapshot: &std::path::Path,
        snapshot_hash: &str,
        production_job_id: &str,
        source_root: &std::path::Path,
    ) {
        let value = json!({
            "mode":"resume-stage-retry",
            "projectId":"project",
            "novelWorkId":"work",
            "productionJobId":production_job_id,
            "sourceRevisionId":"revision",
            "sourceAnalysisRunId":"source-run",
            "invalidAdaptationRunId":"invalid-run",
            "sourceRuntimeRoot":source_root.to_string_lossy(),
            "sourceDbSha256":"sha256:ancestor-original",
            "cloneDbSha256":snapshot_hash,
        });
        fs::write(
            snapshot.join(RESUME_PROVENANCE_FILE),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn sample_keeps_acceptance_metadata_out_of_source_prose() {
        let (facts, chapter) = sample_sections().unwrap();
        assert!(facts.contains("林青"));
        assert!(chapter.contains("信不能湿"));
        assert!(!chapter.contains("逐字核对台词"));
    }

    #[test]
    fn formal_production_intent_is_one_irregular_five_panel_page_with_exact_dialogues() {
        let intent = acceptance_comic_plan_intent();
        assert_eq!(intent.pages.len(), 1);
        let page = &intent.pages[0];
        assert_eq!(page.panel_count, 5);
        assert_eq!(page.layout_profile.as_deref(), Some("hero_middle_5"));
        let dialogues = page.dialogues.as_ref().unwrap();
        assert_eq!(
            dialogues
                .iter()
                .map(|item| item.panel_no)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 5]
        );
        assert_eq!(
            dialogues
                .iter()
                .map(|item| (item.speaker.as_str(), item.text.as_str()))
                .collect::<Vec<_>>(),
            EXPECTED_DIALOGUES
        );
    }

    #[test]
    fn exact_gate_accepts_only_one_five_panel_irregular_page_with_dialogues() {
        let expected = EXPECTED_DIALOGUES.to_vec();
        assert!(assert_one_irregular_five_panel_page(&manifest(expected)).is_ok());
        let mut wrong = EXPECTED_DIALOGUES.to_vec();
        wrong[0] = ("小川", "信不能淋湿。");
        assert_eq!(
            assert_one_irregular_five_panel_page(&manifest(wrong)).unwrap_err(),
            "E2E_DIALOGUE_EXACT_MATCH_REQUIRED"
        );

        let mut piled = manifest(EXPECTED_DIALOGUES.to_vec());
        let all_dialogues = piled.manifest["pages"][0]["panels"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|panel| {
                panel["spec"]["dialogues"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
            })
            .collect::<Vec<_>>();
        piled.manifest["pages"][0]["panels"][0]["spec"]["dialogues"] = Value::Array(all_dialogues);
        for index in 1..5 {
            piled.manifest["pages"][0]["panels"][index]["spec"]["dialogues"] =
                Value::Array(Vec::new());
        }
        assert_eq!(
            assert_one_irregular_five_panel_page(&piled).unwrap_err(),
            "E2E_DIALOGUE_PANEL_ALLOCATION_REQUIRED"
        );

        let mut missing_speaker = manifest(EXPECTED_DIALOGUES.to_vec());
        missing_speaker.manifest["pages"][0]["panels"][0]["spec"]["dialogues"][0]["speaker"] =
            Value::Null;
        assert_eq!(
            assert_one_irregular_five_panel_page(&missing_speaker).unwrap_err(),
            "E2E_DIALOGUE_EXACT_MATCH_REQUIRED"
        );
    }

    #[test]
    fn bootstrap_claims_before_database_and_runtime_validation_accepts_its_artifacts() {
        let root = pristine_root();
        // The pure helper omits the process environment switch so this test
        // can cover the pre-DB claim without mutating global environment.
        validate_bootstrap_root(&root).unwrap();
        let lock = RunLock::from_bootstrap(&root).unwrap();
        fs::write(root.join("image-client.db"), b"sqlite-placeholder").unwrap();
        let audit = Audit::create(root.join(AUDIT_FILE)).unwrap();
        audit.event("started", json!({"runId":"test"})).unwrap();
        assert!(validate_runtime_root(&root).is_ok());
        assert!(root.join(MARKER_FILE).is_file());
        lock.finish("stopped").unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn sealed_ancestor_snapshot_rejects_tampering_wrong_scope_and_runtime_artifacts() {
        let clone = d_audit_child("sealed-clone");
        let source = d_audit_child("sealed-source");
        let ancestor = d_audit_child("sealed-ancestor");
        let snapshot = d_audit_child("sealed-snapshot");
        let clone = canonical_d_audit_root(&clone).unwrap();
        let source = canonical_d_audit_root(&source).unwrap();
        let ancestor = canonical_d_audit_root(&ancestor).unwrap();
        write_snapshot_job(&snapshot.join("image-client.db"), "invalid-run");
        let provenance = test_image_only_provenance(&snapshot);
        let source_provenance = ResumeProvenance {
            mode: "resume-stage-retry".into(),
            project_id: "project".into(),
            novel_work_id: "work".into(),
            production_job_id: "job".into(),
            source_revision_id: "revision".into(),
            source_analysis_run_id: "source-run".into(),
            invalid_adaptation_run_id: "invalid-run".into(),
            source_runtime_root: ancestor.to_string_lossy().into_owned(),
            source_db_sha256: "sha256:ancestor-original".into(),
            clone_db_sha256: "sha256:source-clone".into(),
        };
        write_snapshot_resume_provenance(
            &snapshot,
            provenance.ancestor_snapshot_db_sha256.as_deref().unwrap(),
            "job",
            &ancestor,
        );
        assert_eq!(
            validate_sealed_ancestor_snapshot(
                &clone,
                &source,
                &ancestor,
                &source_provenance,
                &provenance,
            )
            .unwrap(),
            Some(snapshot.canonicalize().unwrap())
        );
        let mut half_pair = test_image_only_provenance(&snapshot);
        half_pair.ancestor_snapshot_db_sha256 = None;
        assert!(validate_sealed_ancestor_snapshot(
            &clone,
            &source,
            &ancestor,
            &source_provenance,
            &half_pair,
        )
        .is_err());

        write_snapshot_resume_provenance(
            &snapshot,
            provenance.ancestor_snapshot_db_sha256.as_deref().unwrap(),
            "wrong-job",
            &ancestor,
        );
        assert!(validate_sealed_ancestor_snapshot(
            &clone,
            &source,
            &ancestor,
            &source_provenance,
            &provenance,
        )
        .is_err());

        write_snapshot_resume_provenance(
            &snapshot,
            provenance.ancestor_snapshot_db_sha256.as_deref().unwrap(),
            "job",
            &source,
        );
        assert!(validate_sealed_ancestor_snapshot(
            &clone,
            &source,
            &ancestor,
            &source_provenance,
            &provenance,
        )
        .is_err());

        write_snapshot_resume_provenance(
            &snapshot,
            provenance.ancestor_snapshot_db_sha256.as_deref().unwrap(),
            "job",
            &ancestor,
        );
        fs::write(snapshot.join(MARKER_FILE), b"{}").unwrap();
        assert!(validate_sealed_ancestor_snapshot(
            &clone,
            &source,
            &ancestor,
            &source_provenance,
            &provenance,
        )
        .is_err());
        fs::remove_file(snapshot.join(MARKER_FILE)).unwrap();
        fs::write(snapshot.join("real-e2e-request-budget.audit.jsonl"), b"").unwrap();
        assert!(validate_sealed_ancestor_snapshot(
            &clone,
            &source,
            &ancestor,
            &source_provenance,
            &provenance,
        )
        .is_err());
        fs::remove_file(snapshot.join("real-e2e-request-budget.audit.jsonl")).unwrap();
        fs::write(snapshot.join("image-client.db-wal"), b"uncheckpointed").unwrap();
        assert!(validate_sealed_ancestor_snapshot(
            &clone,
            &source,
            &ancestor,
            &source_provenance,
            &provenance,
        )
        .is_err());
        fs::remove_file(snapshot.join("image-client.db-wal")).unwrap();
        let conn = rusqlite::Connection::open(snapshot.join("image-client.db")).unwrap();
        conn.execute(
            "UPDATE novel_production_jobs SET adaptation_analysis_run_id='tampered' WHERE id='job'",
            [],
        )
        .unwrap();
        drop(conn);
        assert!(validate_sealed_ancestor_snapshot(
            &clone,
            &source,
            &ancestor,
            &source_provenance,
            &provenance,
        )
        .is_err());
        fs::remove_dir_all(clone).unwrap();
        fs::remove_dir_all(source).unwrap();
        fs::remove_dir_all(ancestor).unwrap();
        fs::remove_dir_all(snapshot).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn canonical_d_audit_root_uses_component_paths_and_rejects_outside_or_reparse() {
        let root = d_audit_child("component-boundary");
        assert!(canonical_d_audit_root(&root).is_ok());
        assert_eq!(
            canonical_d_audit_root(std::path::Path::new(r"D:\\cc\\image-client")).unwrap_err(),
            "E2E_ISOLATED_ROOT_REQUIRED"
        );
        let sibling = std::path::Path::new(r"D:\\cc\\image-client")
            .join(format!(".test-tmp-sibling-{}", std::process::id()));
        fs::create_dir_all(&sibling).unwrap();
        assert_eq!(
            canonical_d_audit_root(&sibling).unwrap_err(),
            "E2E_ISOLATED_ROOT_REQUIRED"
        );
        fs::remove_dir_all(&sibling).unwrap();

        let target = d_audit_child("reparse-target");
        let junction = root.join("junction");
        let result = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                junction.to_str().unwrap(),
                target.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        if !result.success() {
            eprintln!("junction creation unavailable; reparse subcase skipped");
            fs::remove_dir_all(target).unwrap();
            fs::remove_dir_all(root).unwrap();
            return;
        }
        assert_eq!(
            canonical_d_audit_root(&junction).unwrap_err(),
            "E2E_ISOLATED_ROOT_REPARSE"
        );
        fs::remove_dir(&junction).unwrap();
        fs::remove_dir_all(target).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn output_stages_create_a_fresh_export_destination_inside_the_isolated_root() {
        let root = d_audit_child("export-destination");
        let destination = create_export_destination(&root).unwrap();
        assert!(destination.is_dir());
        assert_eq!(destination.parent(), Some(root.as_path()));
        assert!(create_export_destination(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
