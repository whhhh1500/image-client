-- v15: durable, resumable orchestration records for one saved novel chapter
-- revision.  The job never replaces the existing analysis/apply receipts: it
-- records orchestration progress and points at the immutable records they
-- create.

CREATE TABLE novel_production_jobs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  novel_chapter_id TEXT NOT NULL REFERENCES novel_chapters(id) ON DELETE RESTRICT,
  source_revision_id TEXT NOT NULL REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  default_adaptation_id TEXT REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  source_analysis_run_id TEXT REFERENCES source_analysis_runs(id) ON DELETE RESTRICT,
  adaptation_analysis_run_id TEXT REFERENCES adaptation_analysis_runs(id) ON DELETE RESTRICT,
  apply_operation_id TEXT REFERENCES analysis_apply_operations(id) ON DELETE RESTRICT,
  request_hash TEXT NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL CHECK(status IN ('queued','running','waiting_for_predecessor','blocked_config','blocked_conflict','needs_rebase','stale','error','succeeded')),
  stage TEXT NOT NULL CHECK(stage IN ('saving_source','resolving_inheritance','source_analysis','integrating_context','ensuring_adaptation','adaptation_analysis','applying_production','succeeded')),
  stage_index INTEGER NOT NULL CHECK(stage_index >= 0 AND stage_index <= 8),
  stage_total INTEGER NOT NULL CHECK(stage_total = 8),
  completed_artifact_count INTEGER NOT NULL DEFAULT 0 CHECK(completed_artifact_count >= 0 AND completed_artifact_count <= total_artifact_count),
  total_artifact_count INTEGER NOT NULL DEFAULT 14 CHECK(total_artifact_count = 14),
  attempt_no INTEGER NOT NULL DEFAULT 1 CHECK(attempt_no > 0),
  lease_owner TEXT,
  lease_expires_at INTEGER,
  safe_error_code TEXT,
  safe_user_message TEXT,
  next_action TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  finished_at INTEGER,
  UNIQUE(novel_work_id, source_revision_id)
);

CREATE INDEX idx_novel_production_jobs_chapter
ON novel_production_jobs(project_id, novel_work_id, novel_chapter_id, created_at DESC, id DESC);
CREATE INDEX idx_novel_production_jobs_active_lease
ON novel_production_jobs(status, lease_expires_at, updated_at);

CREATE TABLE novel_production_job_events (
  id TEXT PRIMARY KEY,
  novel_production_job_id TEXT NOT NULL REFERENCES novel_production_jobs(id) ON DELETE RESTRICT,
  seq INTEGER NOT NULL CHECK(seq > 0),
  event_type TEXT NOT NULL,
  stage TEXT NOT NULL,
  payload_json TEXT NOT NULL CHECK(json_valid(payload_json) AND json_type(payload_json)='object'),
  created_at INTEGER NOT NULL,
  UNIQUE(novel_production_job_id, seq)
);

CREATE INDEX idx_novel_production_job_events_job
ON novel_production_job_events(novel_production_job_id, seq);

CREATE TRIGGER novel_production_job_scope_insert
BEFORE INSERT ON novel_production_jobs
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM novel_chapter_revisions revision
    JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
    JOIN novel_works work ON work.id=chapter.novel_work_id
    WHERE revision.id=NEW.source_revision_id
      AND chapter.id=NEW.novel_chapter_id
      AND work.id=NEW.novel_work_id
      AND work.project_id=NEW.project_id
  ) THEN RAISE(ABORT, 'production job source scope is invalid') END;
  SELECT CASE WHEN NEW.default_adaptation_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM comic_adaptations adaptation
    WHERE adaptation.id=NEW.default_adaptation_id
      AND adaptation.novel_work_id=NEW.novel_work_id
      AND adaptation.project_id=NEW.project_id
  ) THEN RAISE(ABORT, 'production job adaptation scope is invalid') END;
END;

CREATE TRIGGER novel_production_job_identity_immutable
BEFORE UPDATE OF project_id, novel_work_id, novel_chapter_id, source_revision_id, default_adaptation_id, idempotency_key, request_hash
ON novel_production_jobs
BEGIN
  SELECT RAISE(ABORT, 'production job identity is immutable');
END;

-- Runtime pointers are normally NULL at creation, but this also prevents a
-- direct/import insert from attaching a job to a run or apply receipt owned by
-- another chapter, work, or adaptation.
CREATE TRIGGER novel_production_job_runtime_scope_insert
BEFORE INSERT ON novel_production_jobs
BEGIN
  SELECT CASE WHEN NEW.source_analysis_run_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM source_analysis_runs run WHERE run.id=NEW.source_analysis_run_id AND run.novel_chapter_revision_id=NEW.source_revision_id
  ) THEN RAISE(ABORT, 'production job source run scope is invalid') END;
  SELECT CASE WHEN NEW.adaptation_analysis_run_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM adaptation_analysis_runs run WHERE run.id=NEW.adaptation_analysis_run_id AND NEW.source_analysis_run_id IS NOT NULL AND run.novel_work_id=NEW.novel_work_id AND run.comic_adaptation_id=NEW.default_adaptation_id AND (run.source_analysis_run_id=NEW.source_analysis_run_id OR (run.input_mode='artifact_revisions' AND (SELECT COUNT(*) FROM adaptation_analysis_run_inputs input JOIN analysis_artifact_revisions revision ON revision.id=input.analysis_artifact_revision_id JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id WHERE input.adaptation_analysis_run_id=run.id AND artifact.source_analysis_run_id=NEW.source_analysis_run_id)=10))
  ) THEN RAISE(ABORT, 'production job adaptation run scope is invalid') END;
  SELECT CASE WHEN NEW.apply_operation_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations operation WHERE operation.id=NEW.apply_operation_id AND operation.operation_type='apply_comic_plan' AND operation.comic_adaptation_id=NEW.default_adaptation_id
  ) THEN RAISE(ABORT, 'production job apply operation scope is invalid') END;
END;

CREATE TRIGGER novel_production_job_runtime_scope_update
BEFORE UPDATE OF source_analysis_run_id, adaptation_analysis_run_id, apply_operation_id ON novel_production_jobs
BEGIN
  SELECT CASE WHEN NEW.source_analysis_run_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM source_analysis_runs run WHERE run.id=NEW.source_analysis_run_id AND run.novel_chapter_revision_id=NEW.source_revision_id
  ) THEN RAISE(ABORT, 'production job source run scope is invalid') END;
  SELECT CASE WHEN NEW.adaptation_analysis_run_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM adaptation_analysis_runs run WHERE run.id=NEW.adaptation_analysis_run_id AND NEW.source_analysis_run_id IS NOT NULL AND run.novel_work_id=NEW.novel_work_id AND run.comic_adaptation_id=NEW.default_adaptation_id AND (run.source_analysis_run_id=NEW.source_analysis_run_id OR (run.input_mode='artifact_revisions' AND (SELECT COUNT(*) FROM adaptation_analysis_run_inputs input JOIN analysis_artifact_revisions revision ON revision.id=input.analysis_artifact_revision_id JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id WHERE input.adaptation_analysis_run_id=run.id AND artifact.source_analysis_run_id=NEW.source_analysis_run_id)=10))
  ) THEN RAISE(ABORT, 'production job adaptation run scope is invalid') END;
  SELECT CASE WHEN NEW.apply_operation_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations operation WHERE operation.id=NEW.apply_operation_id AND operation.operation_type='apply_comic_plan' AND operation.comic_adaptation_id=NEW.default_adaptation_id
  ) THEN RAISE(ABORT, 'production job apply operation scope is invalid') END;
END;
