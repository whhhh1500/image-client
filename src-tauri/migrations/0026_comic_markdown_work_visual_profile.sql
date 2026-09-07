-- Work-level visual contract for the Markdown comic workspace. It is
-- deliberately separate from chapter render options: the constitution and
-- reference board are shared by every chapter in one novel work.
CREATE TABLE comic_md_work_visual_profiles (
  novel_work_id TEXT PRIMARY KEY REFERENCES novel_works(id) ON DELETE RESTRICT,
  project_id TEXT NOT NULL,
  constitution_markdown TEXT NOT NULL DEFAULT '',
  revision INTEGER NOT NULL CHECK(revision >= 0),
  updated_at INTEGER NOT NULL
);

CREATE TABLE comic_md_work_visual_references (
  id TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES comic_md_work_visual_profiles(novel_work_id) ON DELETE RESTRICT,
  asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE RESTRICT,
  role TEXT NOT NULL CHECK(role IN ('character_identity','outfit','style','pose','scene','prop','previous_panel','base_image','mask')),
  weight REAL NOT NULL CHECK(weight > 0 AND weight <= 1),
  sort_order INTEGER NOT NULL CHECK(sort_order >= 0),
  note TEXT NOT NULL DEFAULT '',
  sha256 TEXT NOT NULL CHECK(length(sha256) = 71 AND sha256 LIKE 'sha256:%'),
  created_at INTEGER NOT NULL,
  UNIQUE(novel_work_id, asset_id),
  UNIQUE(novel_work_id, sort_order)
);
CREATE INDEX idx_comic_md_work_visual_references_work
  ON comic_md_work_visual_references(novel_work_id, sort_order, id);

CREATE TRIGGER comic_md_work_visual_profile_scope_insert
BEFORE INSERT ON comic_md_work_visual_profiles
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_works work
    WHERE work.id=NEW.novel_work_id AND work.project_id=NEW.project_id AND work.status='active'
  ) THEN RAISE(ABORT, 'comic markdown visual profile scope is invalid') END;
END;

CREATE TRIGGER comic_md_work_visual_profile_scope_update
BEFORE UPDATE OF novel_work_id, project_id ON comic_md_work_visual_profiles
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_works work
    WHERE work.id=NEW.novel_work_id AND work.project_id=NEW.project_id AND work.status='active'
  ) THEN RAISE(ABORT, 'comic markdown visual profile scope is invalid') END;
END;

-- Persist the frozen visual contract with each generated page. Defaults keep
-- v25 history valid and make the empty profile revision 0 explicit.
ALTER TABLE comic_md_images ADD COLUMN visual_profile_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE comic_md_images ADD COLUMN visual_reference_snapshot TEXT NOT NULL DEFAULT '[]';
