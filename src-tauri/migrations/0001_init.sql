CREATE TABLE IF NOT EXISTS assets (
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL,              -- 'image' | 'video' | 'text'
  path        TEXT NOT NULL,
  width       INTEGER,
  height      INTEGER,
  duration_s  REAL,
  format      TEXT,
  thumbnail   TEXT,
  created_at  INTEGER NOT NULL,
  metadata    TEXT
);

CREATE TABLE IF NOT EXISTS workflows (
  id          TEXT PRIMARY KEY,
  name        TEXT,
  definition  TEXT NOT NULL,
  updated_at  INTEGER NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
  id              TEXT PRIMARY KEY,
  workflow_id     TEXT,
  node_id         TEXT,
  provider_id     TEXT,
  status          TEXT NOT NULL,          -- queued/running/success/error/cancelled
  progress        REAL NOT NULL DEFAULT 0,
  external_job_id TEXT,
  input_json      TEXT,
  output_assets   TEXT,
  error           TEXT,
  cost_usd        REAL,
  created_at      INTEGER NOT NULL,
  finished_at     INTEGER
);

CREATE TABLE IF NOT EXISTS settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
