CREATE TABLE analysis_apply_operation_sources (
  analysis_apply_operation_id TEXT NOT NULL REFERENCES analysis_apply_operations(id) ON DELETE RESTRICT,
  analysis_artifact_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  source_role TEXT NOT NULL CHECK(source_role IN ('canonical_input','state_input','adaptation_input','scene_input')),
  source_order INTEGER NOT NULL CHECK(source_order >= 0),
  PRIMARY KEY(analysis_apply_operation_id, analysis_artifact_revision_id),
  UNIQUE(analysis_apply_operation_id, source_role, source_order)
);

CREATE TABLE analysis_apply_receipts (
  analysis_apply_operation_id TEXT PRIMARY KEY REFERENCES analysis_apply_operations(id) ON DELETE RESTRICT,
  idempotency_key TEXT NOT NULL UNIQUE,
  operation_type TEXT NOT NULL CHECK(operation_type IN ('publish_canon','publish_novel_state','accept_adaptation','apply_comic_plan')),
  result_novel_canon_version_id TEXT REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  result_novel_state_version_id TEXT REFERENCES novel_state_versions(id) ON DELETE RESTRICT,
  created_at INTEGER NOT NULL,
  CHECK(
    (operation_type='publish_canon' AND result_novel_canon_version_id IS NOT NULL AND result_novel_state_version_id IS NULL) OR
    (operation_type='publish_novel_state' AND result_novel_canon_version_id IS NULL AND result_novel_state_version_id IS NOT NULL) OR
    (operation_type IN ('accept_adaptation','apply_comic_plan') AND result_novel_canon_version_id IS NULL AND result_novel_state_version_id IS NULL)
  )
);

CREATE TABLE novel_canon_version_sources (
  novel_canon_version_id TEXT NOT NULL REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  analysis_artifact_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  source_role TEXT NOT NULL CHECK(source_role IN ('canonical_input','derived_canon')),
  source_order INTEGER NOT NULL CHECK(source_order >= 0),
  PRIMARY KEY(novel_canon_version_id, analysis_artifact_revision_id),
  UNIQUE(novel_canon_version_id, source_role, source_order)
);

CREATE TABLE novel_state_version_sources (
  novel_state_version_id TEXT NOT NULL REFERENCES novel_state_versions(id) ON DELETE RESTRICT,
  analysis_artifact_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  source_role TEXT NOT NULL CHECK(source_role IN ('state_input','derived_state')),
  source_order INTEGER NOT NULL CHECK(source_order >= 0),
  PRIMARY KEY(novel_state_version_id, analysis_artifact_revision_id),
  UNIQUE(novel_state_version_id, source_role, source_order)
);

ALTER TABLE analysis_apply_operations ADD COLUMN approval_expires_at INTEGER NOT NULL DEFAULT 0;

CREATE TRIGGER analysis_apply_source_same_work
BEFORE INSERT ON analysis_apply_operation_sources
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op
    JOIN analysis_artifact_revisions revision ON revision.id=NEW.analysis_artifact_revision_id
    JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE op.id=NEW.analysis_apply_operation_id AND op.novel_work_id=artifact.novel_work_id
  ) THEN RAISE(ABORT, 'apply source must belong to operation novel work') END;
END;

CREATE TRIGGER analysis_apply_receipt_shape
BEFORE INSERT ON analysis_apply_receipts
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op WHERE op.id=NEW.analysis_apply_operation_id AND op.idempotency_key=NEW.idempotency_key AND op.operation_type=NEW.operation_type AND op.approval_expires_at>=NEW.created_at
  ) THEN RAISE(ABORT, 'apply receipt operation or idempotency mismatch') END;
  SELECT CASE WHEN NEW.result_novel_canon_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op JOIN novel_canon_versions version ON version.id=NEW.result_novel_canon_version_id WHERE op.id=NEW.analysis_apply_operation_id AND op.novel_work_id=version.novel_work_id
  ) THEN RAISE(ABORT, 'canon result must belong to operation novel work') END;
  SELECT CASE WHEN NEW.result_novel_state_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op JOIN novel_state_versions version ON version.id=NEW.result_novel_state_version_id WHERE op.id=NEW.analysis_apply_operation_id AND op.novel_work_id=version.novel_work_id
  ) THEN RAISE(ABORT, 'state result must belong to operation novel work') END;
END;

CREATE TRIGGER novel_canon_version_source_same_work
BEFORE INSERT ON novel_canon_version_sources
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_canon_versions version JOIN analysis_artifact_revisions revision ON revision.id=NEW.analysis_artifact_revision_id JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE version.id=NEW.novel_canon_version_id AND version.novel_work_id=artifact.novel_work_id AND revision.status='adopted'
  ) THEN RAISE(ABORT, 'canon source must be adopted and belong to version novel work') END;
END;

CREATE TRIGGER novel_state_version_source_same_work
BEFORE INSERT ON novel_state_version_sources
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_state_versions version JOIN analysis_artifact_revisions revision ON revision.id=NEW.analysis_artifact_revision_id JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE version.id=NEW.novel_state_version_id AND version.novel_work_id=artifact.novel_work_id AND revision.status='adopted'
  ) THEN RAISE(ABORT, 'state source must be adopted and belong to version novel work') END;
END;
