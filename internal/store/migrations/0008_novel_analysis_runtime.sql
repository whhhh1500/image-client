-- Runtime state is deliberately separate from v7's immutable source/artifact
-- records.  Provider calls happen outside write transactions; attempts and
-- terminal integration are persisted before and after those calls.

ALTER TABLE source_analysis_runs ADD COLUMN frozen_comic_adaptation_id TEXT REFERENCES comic_adaptations(id) ON DELETE RESTRICT;
ALTER TABLE source_analysis_runs ADD COLUMN frozen_comic_chapter_id TEXT REFERENCES comic_adaptation_chapters(id) ON DELETE RESTRICT;

CREATE TABLE source_analysis_run_attempts (
  id TEXT PRIMARY KEY,
  source_analysis_run_id TEXT NOT NULL REFERENCES source_analysis_runs(id) ON DELETE RESTRICT,
  attempt_no INTEGER NOT NULL CHECK(attempt_no > 0),
  status TEXT NOT NULL CHECK(status IN ('queued','running','success','error','stale','cancelled')),
  lease_owner TEXT,
  lease_expires_at INTEGER,
  heartbeat_at INTEGER,
  safe_error_code TEXT,
  safe_user_message TEXT,
  created_at INTEGER NOT NULL,
  finished_at INTEGER,
  UNIQUE(source_analysis_run_id, attempt_no)
);
CREATE INDEX idx_source_analysis_attempt_recovery
ON source_analysis_run_attempts(status, lease_expires_at);

CREATE TABLE source_analysis_run_events (
  id TEXT PRIMARY KEY,
  source_analysis_run_id TEXT NOT NULL REFERENCES source_analysis_runs(id) ON DELETE RESTRICT,
  seq INTEGER NOT NULL,
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(source_analysis_run_id, seq)
);

CREATE TABLE novel_artifact_optimization_runs (
  id TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  analysis_artifact_id TEXT NOT NULL REFERENCES analysis_artifacts(id) ON DELETE RESTRICT,
  parent_artifact_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  provider_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  instruction TEXT NOT NULL,
  frozen_input_fingerprint TEXT NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL CHECK(status IN ('queued','running','ready','error','cancel_requested','unknown_manual')),
  attempt_no INTEGER NOT NULL DEFAULT 1 CHECK(attempt_no > 0),
  lease_owner TEXT,
  lease_expires_at INTEGER,
  result_artifact_revision_id TEXT REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  safe_error_code TEXT,
  safe_user_message TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  finished_at INTEGER
);
CREATE INDEX idx_novel_artifact_optimization_status
ON novel_artifact_optimization_runs(status, lease_expires_at);

CREATE TABLE novel_artifact_optimization_inputs (
  novel_artifact_optimization_run_id TEXT NOT NULL REFERENCES novel_artifact_optimization_runs(id) ON DELETE RESTRICT,
  analysis_artifact_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  source_order INTEGER NOT NULL,
  PRIMARY KEY(novel_artifact_optimization_run_id, analysis_artifact_revision_id)
);

CREATE TABLE novel_artifact_adoption_previews (
  token_hash TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  analysis_artifact_id TEXT NOT NULL REFERENCES analysis_artifacts(id) ON DELETE RESTRICT,
  analysis_artifact_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  expected_optimistic_version INTEGER NOT NULL,
  preview_fingerprint TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_novel_artifact_adoption_preview_expiry
ON novel_artifact_adoption_previews(expires_at, used_at);

CREATE TABLE comic_adaptation_chapters (
  id TEXT PRIMARY KEY,
  comic_adaptation_id TEXT NOT NULL REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  novel_chapter_revision_id TEXT NOT NULL REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  sequence_no INTEGER NOT NULL CHECK(sequence_no > 0),
  created_at INTEGER NOT NULL,
  UNIQUE(comic_adaptation_id, novel_chapter_revision_id),
  UNIQUE(comic_adaptation_id, sequence_no)
);
CREATE INDEX idx_comic_adaptation_chapters_revision ON comic_adaptation_chapters(novel_chapter_revision_id);

CREATE TRIGGER analysis_artifact_runtime_scope
BEFORE INSERT ON analysis_artifacts
WHEN NEW.source_analysis_run_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NEW.artifact_type IN ('world_facts','character_facts','faction_facts','location_facts','prop_facts')
    AND (NEW.comic_adaptation_id IS NOT NULL OR NEW.comic_chapter_id IS NOT NULL)
    THEN RAISE(ABORT, 'novel fact artifact must not have comic scope') END;
  SELECT CASE WHEN NEW.artifact_type = 'adaptation_proposal'
    AND (NEW.comic_adaptation_id IS NULL OR NEW.comic_chapter_id IS NOT NULL)
    THEN RAISE(ABORT, 'adaptation proposal requires adaptation scope') END;
  SELECT CASE WHEN NEW.artifact_type IN ('comic_chapter_plan','scene_plan','page_panel_plan')
    AND (NEW.comic_adaptation_id IS NULL OR NEW.comic_chapter_id IS NULL)
    THEN RAISE(ABORT, 'comic plan requires adaptation chapter scope') END;
END;

CREATE TRIGGER source_analysis_attempt_lease_shape
BEFORE INSERT ON source_analysis_run_attempts
WHEN NEW.status IN ('queued','running') AND (NEW.lease_owner IS NULL OR NEW.lease_expires_at IS NULL)
BEGIN
  SELECT RAISE(ABORT, 'active analysis attempt requires lease');
END;

CREATE TRIGGER artifact_optimization_owner_scope
BEFORE INSERT ON novel_artifact_optimization_runs
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifacts
    WHERE id = NEW.analysis_artifact_id AND novel_work_id = NEW.novel_work_id
  ) THEN RAISE(ABORT, 'optimization artifact must belong to novel work') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions
    WHERE id = NEW.parent_artifact_revision_id AND analysis_artifact_id = NEW.analysis_artifact_id
  ) THEN RAISE(ABORT, 'optimization parent revision must belong to artifact') END;
END;
