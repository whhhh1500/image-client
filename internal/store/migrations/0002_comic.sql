CREATE TABLE IF NOT EXISTS comic_projects (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  title TEXT NOT NULL,
  format TEXT NOT NULL,
  status TEXT NOT NULL,
  config_json TEXT NOT NULL,
  current_state_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_comic_projects_project_id ON comic_projects(project_id);

CREATE TABLE IF NOT EXISTS comic_sources (
  id TEXT PRIMARY KEY,
  comic_project_id TEXT NOT NULL,
  asset_id TEXT,
  source_order INTEGER NOT NULL,
  title TEXT,
  source_range_json TEXT,
  content_hash TEXT NOT NULL,
  summary TEXT,
  created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_comic_sources_project ON comic_sources(comic_project_id, source_order);

CREATE TABLE IF NOT EXISTS comic_cards (
  id TEXT PRIMARY KEY,
  comic_project_id TEXT NOT NULL,
  card_type TEXT NOT NULL,
  entity_key TEXT NOT NULL,
  version INTEGER NOT NULL,
  name TEXT NOT NULL,
  data_json TEXT NOT NULL,
  locked INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(comic_project_id, card_type, entity_key, version)
);

CREATE INDEX IF NOT EXISTS idx_comic_cards_project ON comic_cards(comic_project_id, card_type, entity_key, version);

CREATE TABLE IF NOT EXISTS comic_references (
  id TEXT PRIMARY KEY,
  comic_project_id TEXT NOT NULL,
  owner_type TEXT NOT NULL,
  owner_id TEXT NOT NULL,
  asset_id TEXT NOT NULL,
  role TEXT NOT NULL,
  weight REAL,
  sort_order INTEGER NOT NULL,
  approved INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_comic_references_owner ON comic_references(comic_project_id, owner_type, owner_id, sort_order);

CREATE TABLE IF NOT EXISTS comic_chapters (
  id TEXT PRIMARY KEY,
  comic_project_id TEXT NOT NULL,
  chapter_no INTEGER NOT NULL,
  title TEXT,
  source_json TEXT NOT NULL,
  outline_json TEXT,
  state_before_json TEXT NOT NULL,
  state_after_json TEXT,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(comic_project_id, chapter_no)
);

CREATE INDEX IF NOT EXISTS idx_comic_chapters_project ON comic_chapters(comic_project_id, chapter_no);

CREATE TABLE IF NOT EXISTS comic_scenes (
  id TEXT PRIMARY KEY,
  chapter_id TEXT NOT NULL,
  scene_no INTEGER NOT NULL,
  spec_json TEXT NOT NULL,
  status TEXT NOT NULL,
  keyframe_asset_id TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(chapter_id, scene_no)
);

CREATE INDEX IF NOT EXISTS idx_comic_scenes_chapter ON comic_scenes(chapter_id, scene_no);

CREATE TABLE IF NOT EXISTS comic_panels (
  id TEXT PRIMARY KEY,
  scene_id TEXT NOT NULL,
  page_no INTEGER NOT NULL,
  panel_no INTEGER NOT NULL,
  spec_json TEXT NOT NULL,
  prompt_json TEXT,
  status TEXT NOT NULL,
  approved_run_id TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(scene_id, page_no, panel_no)
);

CREATE INDEX IF NOT EXISTS idx_comic_panels_scene ON comic_panels(scene_id, page_no, panel_no);

CREATE TABLE IF NOT EXISTS comic_panel_runs (
  id TEXT PRIMARY KEY,
  panel_id TEXT NOT NULL,
  task_id TEXT,
  asset_id TEXT,
  parent_run_id TEXT,
  strategy TEXT NOT NULL,
  request_json TEXT NOT NULL,
  reference_snapshot_json TEXT NOT NULL,
  score_json TEXT,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  finished_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_comic_panel_runs_panel ON comic_panel_runs(panel_id, created_at);

CREATE TABLE IF NOT EXISTS comic_qc_reports (
  id TEXT PRIMARY KEY,
  panel_run_id TEXT NOT NULL,
  report_json TEXT NOT NULL,
  decision TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_comic_qc_reports_run ON comic_qc_reports(panel_run_id, created_at);
