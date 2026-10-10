-- Page-level candidate review and a single optimistic approval head.
-- Runs remain immutable history; the head selects the currently deliverable page.
CREATE TABLE comic_page_reviews (
  id TEXT PRIMARY KEY,
  comic_project_id TEXT NOT NULL REFERENCES comic_projects(id) ON DELETE RESTRICT,
  page_no INTEGER NOT NULL CHECK(page_no > 0),
  page_run_id TEXT NOT NULL REFERENCES comic_page_runs(id) ON DELETE RESTRICT,
  decision TEXT NOT NULL CHECK(decision IN ('approved','rejected')),
  report_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(page_run_id)
);

CREATE INDEX idx_comic_page_reviews_page
ON comic_page_reviews(comic_project_id, page_no, created_at DESC, id DESC);

CREATE TABLE comic_page_approval_heads (
  comic_project_id TEXT NOT NULL REFERENCES comic_projects(id) ON DELETE RESTRICT,
  page_no INTEGER NOT NULL CHECK(page_no > 0),
  approved_page_run_id TEXT NOT NULL REFERENCES comic_page_runs(id) ON DELETE RESTRICT,
  approved_asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE RESTRICT,
  optimistic_version INTEGER NOT NULL CHECK(optimistic_version > 0),
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(comic_project_id, page_no),
  UNIQUE(approved_page_run_id)
);

CREATE TRIGGER comic_page_review_scope_insert
BEFORE INSERT ON comic_page_reviews
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM comic_page_runs run
    JOIN comic_projects project ON project.id=run.comic_project_id
    JOIN assets asset ON asset.id=run.asset_id
    WHERE run.id=NEW.page_run_id
      AND run.comic_project_id=NEW.comic_project_id
      AND run.page_no=NEW.page_no
      AND run.status='success'
      AND run.asset_id IS NOT NULL
      AND json_valid(asset.metadata)=1
      AND json_extract(asset.metadata,'$.projectId')=project.project_id
  ) THEN RAISE(ABORT,'page review run scope mismatch') END;
END;

CREATE TRIGGER comic_page_review_immutable
BEFORE UPDATE ON comic_page_reviews
BEGIN
  SELECT RAISE(ABORT,'page review is immutable');
END;

CREATE TRIGGER comic_page_approval_head_scope_insert
BEFORE INSERT ON comic_page_approval_heads
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM comic_page_reviews review
    JOIN comic_page_runs run ON run.id=review.page_run_id
    WHERE review.comic_project_id=NEW.comic_project_id
      AND review.page_no=NEW.page_no
      AND review.page_run_id=NEW.approved_page_run_id
      AND review.decision='approved'
      AND run.asset_id=NEW.approved_asset_id
  ) THEN RAISE(ABORT,'page approval head scope mismatch') END;
END;

CREATE TRIGGER comic_page_approval_head_scope_update
BEFORE UPDATE OF comic_project_id,page_no,approved_page_run_id,approved_asset_id ON comic_page_approval_heads
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM comic_page_reviews review
    JOIN comic_page_runs run ON run.id=review.page_run_id
    WHERE review.comic_project_id=NEW.comic_project_id
      AND review.page_no=NEW.page_no
      AND review.page_run_id=NEW.approved_page_run_id
      AND review.decision='approved'
      AND run.asset_id=NEW.approved_asset_id
  ) THEN RAISE(ABORT,'page approval head scope mismatch') END;
END;

CREATE TRIGGER comic_page_approval_head_version_update
BEFORE UPDATE ON comic_page_approval_heads
WHEN NEW.optimistic_version<>OLD.optimistic_version+1
BEGIN
  SELECT RAISE(ABORT,'page approval optimistic version mismatch');
END;
