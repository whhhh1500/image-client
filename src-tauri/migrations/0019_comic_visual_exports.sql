-- v19: durable, lineage-bound exports of candidate-ready novel comic pages.
CREATE TABLE comic_visual_exports (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  batch_id TEXT NOT NULL REFERENCES comic_visual_batches(id) ON DELETE RESTRICT,
  destination_dir TEXT NOT NULL,
  directory_path TEXT NOT NULL,
  manifest_path TEXT NOT NULL,
  selection_json TEXT NOT NULL CHECK(json_valid(selection_json) AND json_type(selection_json)='object'),
  request_hash TEXT NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL CHECK(status IN ('writing','complete','failed')),
  safe_error_code TEXT,
  created_at INTEGER NOT NULL
);

CREATE INDEX idx_comic_visual_exports_scope
  ON comic_visual_exports(project_id, novel_work_id, batch_id, created_at DESC, id DESC);

CREATE TABLE comic_visual_export_files (
  export_id TEXT NOT NULL REFERENCES comic_visual_exports(id) ON DELETE RESTRICT,
  member_id TEXT NOT NULL REFERENCES comic_visual_batch_members(id) ON DELETE RESTRICT,
  ordinal INTEGER NOT NULL CHECK(ordinal > 0),
  manifest_id TEXT NOT NULL REFERENCES comic_visual_manifests(id) ON DELETE RESTRICT,
  production_chapter_id TEXT NOT NULL REFERENCES comic_production_chapters(id) ON DELETE RESTRICT,
  production_page_id TEXT NOT NULL REFERENCES comic_production_pages(id) ON DELETE RESTRICT,
  run_id TEXT NOT NULL REFERENCES comic_visual_page_runs(id) ON DELETE RESTRICT,
  asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE RESTRICT,
  path TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  PRIMARY KEY(export_id, member_id),
  UNIQUE(export_id, ordinal)
);

CREATE TABLE comic_visual_export_command_receipts (
  command_name TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  export_id TEXT NOT NULL REFERENCES comic_visual_exports(id) ON DELETE RESTRICT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(command_name, idempotency_key)
);

CREATE TRIGGER comic_visual_export_scope_insert
BEFORE INSERT ON comic_visual_exports
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_visual_batches batch
    WHERE batch.id=NEW.batch_id AND batch.project_id=NEW.project_id
      AND batch.novel_work_id=NEW.novel_work_id
  ) THEN RAISE(ABORT, 'comic visual export scope is invalid') END;
END;
