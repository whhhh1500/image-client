-- Comic production reliability: immutable attempts, recovery metadata and
-- operation-level idempotency.  This migration is intentionally transactional
-- (the Rust migration runner wraps the whole file in BEGIN IMMEDIATE).
ALTER TABLE comic_page_runs ADD COLUMN parent_run_id TEXT;
ALTER TABLE comic_page_runs ADD COLUMN attempt_no INTEGER NOT NULL DEFAULT 1;
ALTER TABLE comic_page_runs ADD COLUMN generation_attempt_id TEXT;
ALTER TABLE comic_page_runs ADD COLUMN owner_app_session_id TEXT;
ALTER TABLE comic_page_runs ADD COLUMN failure_json TEXT;
ALTER TABLE comic_page_runs ADD COLUMN submitted_at INTEGER;
ALTER TABLE comic_page_runs ADD COLUMN heartbeat_at INTEGER;
ALTER TABLE comic_page_runs ADD COLUMN lease_expires_at INTEGER;

ALTER TABLE comic_panel_runs ADD COLUMN attempt_no INTEGER NOT NULL DEFAULT 1;
ALTER TABLE comic_panel_runs ADD COLUMN generation_attempt_id TEXT;
ALTER TABLE comic_panel_runs ADD COLUMN owner_app_session_id TEXT;
ALTER TABLE comic_panel_runs ADD COLUMN failure_json TEXT;
ALTER TABLE comic_panel_runs ADD COLUMN submitted_at INTEGER;
ALTER TABLE comic_panel_runs ADD COLUMN heartbeat_at INTEGER;
ALTER TABLE comic_panel_runs ADD COLUMN lease_expires_at INTEGER;

-- v4 did not retain an owner/attempt for old running rows.  Give them an
-- immediately expired lease so the first startup recovery marks them stale.
UPDATE comic_page_runs
SET lease_expires_at = strftime('%s','now') * 1000
WHERE status = 'running' AND lease_expires_at IS NULL;
UPDATE comic_panel_runs
SET lease_expires_at = strftime('%s','now') * 1000
WHERE status = 'running' AND lease_expires_at IS NULL;

-- Merge legacy duplicate references before enforcing the business-key unique
-- index.  The earliest (sort_order, created_at, id) row is canonical and an
-- approval on any duplicate is retained.
UPDATE comic_references AS canonical
SET approved = CASE WHEN EXISTS (
  SELECT 1 FROM comic_references AS duplicate
  WHERE duplicate.comic_project_id = canonical.comic_project_id
    AND duplicate.owner_type = canonical.owner_type
    AND duplicate.owner_id = canonical.owner_id
    AND duplicate.asset_id = canonical.asset_id
    AND duplicate.role = canonical.role
    AND (duplicate.sort_order < canonical.sort_order
      OR (duplicate.sort_order = canonical.sort_order AND duplicate.created_at < canonical.created_at)
      OR (duplicate.sort_order = canonical.sort_order AND duplicate.created_at = canonical.created_at AND duplicate.id < canonical.id))
) THEN canonical.approved ELSE canonical.approved END
WHERE canonical.id = (
  SELECT c.id FROM comic_references AS c
  WHERE c.comic_project_id = canonical.comic_project_id
    AND c.owner_type = canonical.owner_type
    AND c.owner_id = canonical.owner_id
    AND c.asset_id = canonical.asset_id
    AND c.role = canonical.role
  ORDER BY c.sort_order ASC, c.created_at ASC, c.id ASC LIMIT 1
);
UPDATE comic_references AS canonical
SET approved = 1
WHERE EXISTS (
  SELECT 1 FROM comic_references AS duplicate
  WHERE duplicate.comic_project_id = canonical.comic_project_id
    AND duplicate.owner_type = canonical.owner_type
    AND duplicate.owner_id = canonical.owner_id
    AND duplicate.asset_id = canonical.asset_id
    AND duplicate.role = canonical.role
    AND duplicate.approved = 1
)
AND canonical.id = (
  SELECT c.id FROM comic_references AS c
  WHERE c.comic_project_id = canonical.comic_project_id
    AND c.owner_type = canonical.owner_type
    AND c.owner_id = canonical.owner_id
    AND c.asset_id = canonical.asset_id
    AND c.role = canonical.role
  ORDER BY c.sort_order ASC, c.created_at ASC, c.id ASC LIMIT 1
);
DELETE FROM comic_references AS duplicate
WHERE EXISTS (
  SELECT 1 FROM comic_references AS canonical
  WHERE canonical.comic_project_id = duplicate.comic_project_id
    AND canonical.owner_type = duplicate.owner_type
    AND canonical.owner_id = duplicate.owner_id
    AND canonical.asset_id = duplicate.asset_id
    AND canonical.role = duplicate.role
    AND (canonical.sort_order < duplicate.sort_order
      OR (canonical.sort_order = duplicate.sort_order AND canonical.created_at < duplicate.created_at)
      OR (canonical.sort_order = duplicate.sort_order AND canonical.created_at = duplicate.created_at AND canonical.id < duplicate.id))
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_comic_references_business_key
ON comic_references(comic_project_id, owner_type, owner_id, asset_id, role);

CREATE TABLE IF NOT EXISTS comic_run_events (
  id TEXT PRIMARY KEY,
  run_kind TEXT NOT NULL,
  run_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  event_type TEXT NOT NULL,
  from_status TEXT,
  to_status TEXT,
  payload_json TEXT NOT NULL,
  idempotency_key TEXT,
  generation_attempt_id TEXT,
  owner_app_session_id TEXT,
  created_at INTEGER NOT NULL,
  UNIQUE(run_kind, run_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_comic_run_events_run ON comic_run_events(run_kind, run_id, seq);

CREATE TABLE IF NOT EXISTS comic_operation_receipts (
  id TEXT PRIMARY KEY,
  command_name TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  response_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(command_name, idempotency_key)
);
CREATE INDEX IF NOT EXISTS idx_comic_operation_receipts_created
ON comic_operation_receipts(created_at);

-- Keep historical terminal rows readable through the structured v5 failure
-- field.  The legacy text remains untouched for compatibility.
UPDATE comic_page_runs
SET failure_json = json_object(
  'phase', 'finish', 'code', 'LEGACY_ERROR', 'message', error,
  'retryable', 1, 'observedAt', strftime('%s','now') * 1000
)
WHERE status = 'error' AND error IS NOT NULL AND failure_json IS NULL;
UPDATE comic_panel_runs
SET failure_json = json_object(
  'phase', 'finish', 'code', 'LEGACY_ERROR', 'message', error,
  'retryable', 1, 'observedAt', strftime('%s','now') * 1000
)
WHERE status = 'error' AND error IS NOT NULL AND failure_json IS NULL;

CREATE INDEX IF NOT EXISTS idx_comic_page_runs_stable
ON comic_page_runs(comic_project_id, page_no, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_comic_page_runs_parent
ON comic_page_runs(parent_run_id, attempt_no);
CREATE INDEX IF NOT EXISTS idx_comic_panel_runs_stable
ON comic_panel_runs(panel_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_comic_panel_runs_parent
ON comic_panel_runs(parent_run_id, attempt_no);
