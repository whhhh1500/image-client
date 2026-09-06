CREATE TABLE IF NOT EXISTS comic_page_runs (
  id TEXT PRIMARY KEY,
  comic_project_id TEXT NOT NULL,
  page_no INTEGER NOT NULL,
  asset_id TEXT,
  request_json TEXT NOT NULL,
  reference_snapshot_json TEXT NOT NULL,
  status TEXT NOT NULL,
  error TEXT,
  created_at INTEGER NOT NULL,
  finished_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_comic_page_runs_project ON comic_page_runs(comic_project_id, page_no, created_at);
