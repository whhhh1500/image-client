-- v16: immutable, NovelWork-scoped renderer inputs. These records deliberately
-- do not project into the legacy comic_projects/comic_page_runs tables.
CREATE TABLE comic_visual_manifests (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  comic_adaptation_id TEXT NOT NULL REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  apply_operation_id TEXT NOT NULL REFERENCES analysis_apply_operations(id) ON DELETE RESTRICT,
  production_chapter_id TEXT NOT NULL REFERENCES comic_production_chapters(id) ON DELETE RESTRICT,
  page_panel_plan_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  contract_version TEXT NOT NULL,
  manifest_json TEXT NOT NULL CHECK(json_valid(manifest_json) AND json_type(manifest_json)='object'),
  manifest_fingerprint TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(apply_operation_id, production_chapter_id, page_panel_plan_revision_id)
);

CREATE INDEX idx_comic_visual_manifests_chapter
  ON comic_visual_manifests(project_id, novel_work_id, production_chapter_id, created_at DESC, id DESC);

CREATE TABLE comic_visual_manifest_receipts (
  command_name TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  manifest_id TEXT NOT NULL REFERENCES comic_visual_manifests(id) ON DELETE RESTRICT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(command_name, idempotency_key)
);

CREATE TRIGGER comic_visual_manifest_scope_insert
BEFORE INSERT ON comic_visual_manifests
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM comic_production_chapters chapter
    JOIN comic_adaptations adaptation ON adaptation.id=chapter.comic_adaptation_id
    JOIN analysis_apply_operations operation ON operation.id=chapter.apply_operation_id
    WHERE chapter.id=NEW.production_chapter_id
      AND chapter.comic_adaptation_id=NEW.comic_adaptation_id
      AND chapter.apply_operation_id=NEW.apply_operation_id
      AND chapter.page_panel_plan_revision_id=NEW.page_panel_plan_revision_id
      AND adaptation.project_id=NEW.project_id
      AND adaptation.novel_work_id=NEW.novel_work_id
      AND operation.operation_type='apply_comic_plan'
  ) THEN RAISE(ABORT, 'visual manifest scope is invalid') END;
END;

CREATE TRIGGER comic_visual_manifest_identity_immutable
BEFORE UPDATE OF project_id, novel_work_id, comic_adaptation_id, apply_operation_id,
  production_chapter_id, page_panel_plan_revision_id, contract_version, manifest_json,
  manifest_fingerprint ON comic_visual_manifests
BEGIN
  SELECT RAISE(ABORT, 'visual manifest is immutable');
END;
