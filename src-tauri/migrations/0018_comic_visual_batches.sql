-- v18: one explicit, durable image-generation authorization per novel
-- production job.  The text-production job remains authoritative for text
-- completion; this table records the separately authorized visual delivery.
CREATE TABLE comic_visual_batches (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  novel_chapter_id TEXT NOT NULL REFERENCES novel_chapters(id) ON DELETE RESTRICT,
  source_revision_id TEXT NOT NULL REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  production_job_id TEXT NOT NULL UNIQUE REFERENCES novel_production_jobs(id) ON DELETE RESTRICT,
  comic_adaptation_id TEXT REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  apply_operation_id TEXT REFERENCES analysis_apply_operations(id) ON DELETE RESTRICT,
  output_target TEXT NOT NULL CHECK(output_target='comic_pages'),
  authorization_json TEXT NOT NULL CHECK(json_valid(authorization_json) AND json_type(authorization_json)='object'),
  authorization_fingerprint TEXT NOT NULL,
  -- A once-only resolution for an authorization originally made without a
  -- configured model.  Keep authorization_json immutable audit evidence.
  resolved_model TEXT,
  request_hash TEXT NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL CHECK(status IN ('authorized','waiting_text','preparing','running','blocked_config','blocked_stale','needs_reconcile','failed','candidate_ready')),
  total_members INTEGER NOT NULL DEFAULT 0 CHECK(total_members >= 0),
  completed_members INTEGER NOT NULL DEFAULT 0 CHECK(completed_members >= 0 AND completed_members <= total_members),
  current_member_ordinal INTEGER,
  lease_owner TEXT,
  lease_expires_at INTEGER,
  safe_error_code TEXT,
  safe_user_message TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  finished_at INTEGER,
  UNIQUE(production_job_id, authorization_fingerprint)
);

CREATE INDEX idx_comic_visual_batches_recovery
  ON comic_visual_batches(status, lease_expires_at, updated_at);
CREATE INDEX idx_comic_visual_batches_scope
  ON comic_visual_batches(project_id, novel_work_id, production_job_id);

CREATE TABLE comic_visual_batch_command_receipts (
  command_name TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  batch_id TEXT NOT NULL REFERENCES comic_visual_batches(id) ON DELETE RESTRICT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(command_name, idempotency_key)
);

CREATE TRIGGER comic_visual_batch_fixed_lineage
BEFORE UPDATE OF comic_adaptation_id,apply_operation_id ON comic_visual_batches
WHEN OLD.comic_adaptation_id IS NOT NULL OR OLD.apply_operation_id IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'comic visual batch lineage is immutable once fixed'); END;

CREATE TABLE comic_visual_batch_members (
  id TEXT PRIMARY KEY,
  batch_id TEXT NOT NULL REFERENCES comic_visual_batches(id) ON DELETE RESTRICT,
  ordinal INTEGER NOT NULL CHECK(ordinal > 0),
  manifest_id TEXT NOT NULL REFERENCES comic_visual_manifests(id) ON DELETE RESTRICT,
  production_chapter_id TEXT NOT NULL REFERENCES comic_production_chapters(id) ON DELETE RESTRICT,
  production_page_id TEXT NOT NULL REFERENCES comic_production_pages(id) ON DELETE RESTRICT,
  page_no INTEGER NOT NULL CHECK(page_no > 0),
  page_stable_key TEXT NOT NULL,
  predecessor_member_id TEXT REFERENCES comic_visual_batch_members(id) ON DELETE RESTRICT,
  page_run_id TEXT REFERENCES comic_visual_page_runs(id) ON DELETE RESTRICT,
  dispatch_key TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL CHECK(status IN ('pending','queued','running','candidate_ready','blocked_config','blocked_stale','needs_reconcile','failed')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  finished_at INTEGER,
  UNIQUE(batch_id, ordinal),
  UNIQUE(batch_id, manifest_id, production_page_id)
);

CREATE INDEX idx_comic_visual_batch_members_progress
  ON comic_visual_batch_members(batch_id, ordinal, status);
CREATE INDEX idx_comic_visual_batch_members_run
  ON comic_visual_batch_members(page_run_id);

CREATE TRIGGER comic_visual_batch_scope_insert
BEFORE INSERT ON comic_visual_batches
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_production_jobs job
    WHERE job.id=NEW.production_job_id AND job.project_id=NEW.project_id
      AND job.novel_work_id=NEW.novel_work_id AND job.novel_chapter_id=NEW.novel_chapter_id
      AND job.source_revision_id=NEW.source_revision_id
  ) THEN RAISE(ABORT, 'comic visual batch production job scope is invalid') END;
END;

CREATE TRIGGER comic_visual_batch_identity_immutable
BEFORE UPDATE OF project_id,novel_work_id,novel_chapter_id,source_revision_id,production_job_id,
  output_target,authorization_json,authorization_fingerprint,request_hash,idempotency_key
ON comic_visual_batches
BEGIN SELECT RAISE(ABORT, 'comic visual batch identity is immutable'); END;

CREATE TRIGGER comic_visual_batch_member_scope_insert
BEFORE INSERT ON comic_visual_batch_members
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_visual_batches batch
    JOIN comic_visual_manifests manifest ON manifest.id=NEW.manifest_id
    JOIN comic_production_pages page ON page.id=NEW.production_page_id
    WHERE batch.id=NEW.batch_id AND manifest.project_id=batch.project_id
      AND manifest.novel_work_id=batch.novel_work_id
      AND manifest.production_chapter_id=NEW.production_chapter_id
      AND page.comic_production_chapter_id=NEW.production_chapter_id
      AND page.page_no=NEW.page_no AND page.planning_page_stable_key=NEW.page_stable_key
  ) THEN RAISE(ABORT, 'comic visual batch member scope is invalid') END;
END;

CREATE TRIGGER comic_visual_batch_member_identity_immutable
BEFORE UPDATE OF batch_id,ordinal,manifest_id,production_chapter_id,production_page_id,page_no,
  page_stable_key,predecessor_member_id,dispatch_key
ON comic_visual_batch_members
BEGIN SELECT RAISE(ABORT, 'comic visual batch member identity is immutable'); END;
