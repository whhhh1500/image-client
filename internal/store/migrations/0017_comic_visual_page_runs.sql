-- v17: durable renderer attempts for immutable NovelWork visual manifests.
CREATE TABLE comic_visual_page_runs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  manifest_id TEXT NOT NULL REFERENCES comic_visual_manifests(id) ON DELETE RESTRICT,
  production_page_id TEXT NOT NULL REFERENCES comic_production_pages(id) ON DELETE RESTRICT,
  manifest_fingerprint TEXT NOT NULL,
  page_stable_key TEXT NOT NULL,
  page_no INTEGER NOT NULL CHECK(page_no > 0),
  compiler_contract_version TEXT NOT NULL,
  request_json TEXT NOT NULL CHECK(json_valid(request_json) AND json_type(request_json)='object'),
  reference_snapshot_json TEXT NOT NULL CHECK(json_valid(reference_snapshot_json) AND json_type(reference_snapshot_json)='object'),
  provider_id TEXT,
  provider_request_id TEXT,
  asset_id TEXT REFERENCES assets(id) ON DELETE RESTRICT,
  status TEXT NOT NULL CHECK(status IN ('blocked_config','queued','running','needs_reconcile','failed','candidate_ready')),
  attempt_no INTEGER NOT NULL CHECK((status='blocked_config' AND attempt_no=0) OR (status<>'blocked_config' AND attempt_no>0)),
  parent_run_id TEXT REFERENCES comic_visual_page_runs(id) ON DELETE RESTRICT,
  generation_attempt_id TEXT NOT NULL UNIQUE,
  idempotency_key TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  owner_app_session_id TEXT,
  submitted_at INTEGER,
  heartbeat_at INTEGER,
  lease_expires_at INTEGER,
  failure_json TEXT,
  created_at INTEGER NOT NULL,
  finished_at INTEGER,
  UNIQUE(manifest_id, production_page_id, attempt_no),
  UNIQUE(idempotency_key)
);
CREATE UNIQUE INDEX idx_comic_visual_page_runs_one_active
  ON comic_visual_page_runs(manifest_id, production_page_id)
  WHERE status IN ('queued','running','needs_reconcile');
CREATE INDEX idx_comic_visual_page_runs_page
  ON comic_visual_page_runs(project_id,novel_work_id,manifest_id,production_page_id,created_at DESC,id DESC);

CREATE TRIGGER comic_visual_page_run_scope_insert
BEFORE INSERT ON comic_visual_page_runs
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_visual_manifests manifest
    JOIN comic_production_pages page ON page.id=NEW.production_page_id
    WHERE manifest.id=NEW.manifest_id AND manifest.project_id=NEW.project_id
      AND manifest.novel_work_id=NEW.novel_work_id
      AND page.comic_production_chapter_id=manifest.production_chapter_id
      AND NEW.manifest_fingerprint=manifest.manifest_fingerprint
      AND NEW.page_stable_key=page.planning_page_stable_key AND NEW.page_no=page.page_no
  ) THEN RAISE(ABORT, 'visual page run scope is invalid') END;
END;

CREATE TRIGGER comic_visual_page_run_identity_immutable
BEFORE UPDATE OF project_id,novel_work_id,manifest_id,production_page_id,manifest_fingerprint,
  page_stable_key,page_no,compiler_contract_version,request_json,reference_snapshot_json,
  attempt_no,parent_run_id,generation_attempt_id,idempotency_key ON comic_visual_page_runs
BEGIN SELECT RAISE(ABORT, 'visual page run identity is immutable'); END;
