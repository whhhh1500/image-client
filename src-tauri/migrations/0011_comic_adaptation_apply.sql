-- v11: immutable adaptation planning heads and atomic production apply targets.
-- Legacy comic_* rows remain untouched; this is the NovelWork-backed production tree.

ALTER TABLE analysis_apply_operations ADD COLUMN expected_adaptation_version INTEGER;

CREATE TRIGGER analysis_apply_expected_adaptation_version_insert
BEFORE INSERT ON analysis_apply_operations
BEGIN
  SELECT CASE WHEN NEW.operation_type IN ('accept_adaptation','apply_comic_plan')
    AND (NEW.expected_adaptation_version IS NULL OR NEW.expected_adaptation_version < 0)
    THEN RAISE(ABORT, 'adaptation operation requires nonnegative expected version') END;
  SELECT CASE WHEN NEW.operation_type IN ('publish_canon','publish_novel_state')
    AND NEW.expected_adaptation_version IS NOT NULL
    THEN RAISE(ABORT, 'novel work operation cannot carry adaptation version') END;
END;
CREATE TRIGGER analysis_apply_expected_adaptation_version_update
BEFORE UPDATE OF operation_type, expected_adaptation_version ON analysis_apply_operations
BEGIN
  SELECT CASE WHEN NEW.operation_type IN ('accept_adaptation','apply_comic_plan')
    AND (NEW.expected_adaptation_version IS NULL OR NEW.expected_adaptation_version < 0)
    THEN RAISE(ABORT, 'adaptation operation requires nonnegative expected version') END;
  SELECT CASE WHEN NEW.operation_type IN ('publish_canon','publish_novel_state')
    AND NEW.expected_adaptation_version IS NOT NULL
    THEN RAISE(ABORT, 'novel work operation cannot carry adaptation version') END;
END;
CREATE TRIGGER analysis_apply_receipt_requires_adaptation_version
BEFORE INSERT ON analysis_apply_receipts
WHEN NEW.operation_type IN ('accept_adaptation','apply_comic_plan')
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op
    WHERE op.id=NEW.analysis_apply_operation_id AND op.expected_adaptation_version IS NOT NULL
      AND op.expected_adaptation_version>=0
  ) THEN RAISE(ABORT, 'adaptation receipt requires expected version') END;
END;

CREATE TABLE comic_adaptation_plan_heads (
  id TEXT PRIMARY KEY,
  comic_adaptation_id TEXT NOT NULL REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  adaptation_proposal_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  comic_chapter_plan_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  scene_plan_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  accept_apply_operation_id TEXT NOT NULL UNIQUE REFERENCES analysis_apply_operations(id) ON DELETE RESTRICT,
  status TEXT NOT NULL CHECK(status IN ('active','superseded')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_comic_adaptation_plan_one_active
  ON comic_adaptation_plan_heads(comic_adaptation_id) WHERE status='active';

CREATE TABLE comic_planning_chapters (
  id TEXT PRIMARY KEY,
  comic_adaptation_plan_head_id TEXT NOT NULL REFERENCES comic_adaptation_plan_heads(id) ON DELETE RESTRICT,
  comic_adaptation_id TEXT NOT NULL REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  planning_chapter_stable_key TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('planning','applied','superseded')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(comic_adaptation_plan_head_id, planning_chapter_stable_key)
);

CREATE TABLE comic_planning_chapter_sources (
  comic_planning_chapter_id TEXT NOT NULL REFERENCES comic_planning_chapters(id) ON DELETE RESTRICT,
  novel_chapter_revision_id TEXT NOT NULL REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  source_order INTEGER NOT NULL CHECK(source_order >= 0),
  source_start INTEGER NOT NULL CHECK(source_start >= 0),
  source_end INTEGER NOT NULL CHECK(source_end > source_start),
  PRIMARY KEY(comic_planning_chapter_id, source_order),
  UNIQUE(comic_planning_chapter_id, novel_chapter_revision_id, source_start, source_end)
);

CREATE TABLE comic_scene_context_snapshot_approvals (
  scene_context_snapshot_id TEXT PRIMARY KEY REFERENCES comic_scene_context_snapshots(id) ON DELETE RESTRICT,
  approved_context_fingerprint TEXT NOT NULL,
  approved_by TEXT NOT NULL,
  approved_at INTEGER NOT NULL
);

CREATE TABLE comic_production_chapters (
  id TEXT PRIMARY KEY,
  comic_adaptation_id TEXT NOT NULL REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  comic_planning_chapter_id TEXT NOT NULL REFERENCES comic_planning_chapters(id) ON DELETE RESTRICT,
  page_panel_plan_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  apply_operation_id TEXT NOT NULL REFERENCES analysis_apply_operations(id) ON DELETE RESTRICT,
  status TEXT NOT NULL CHECK(status IN ('active','superseded')),
  created_at INTEGER NOT NULL,
  UNIQUE(comic_planning_chapter_id),
  UNIQUE(apply_operation_id, comic_planning_chapter_id)
);

CREATE TABLE comic_production_scenes (
  id TEXT PRIMARY KEY,
  comic_production_chapter_id TEXT NOT NULL REFERENCES comic_production_chapters(id) ON DELETE RESTRICT,
  planning_scene_stable_key TEXT NOT NULL,
  scene_context_snapshot_id TEXT NOT NULL REFERENCES comic_scene_context_snapshots(id) ON DELETE RESTRICT,
  scene_no INTEGER NOT NULL CHECK(scene_no > 0),
  created_at INTEGER NOT NULL,
  UNIQUE(comic_production_chapter_id, planning_scene_stable_key),
  UNIQUE(scene_context_snapshot_id),
  UNIQUE(comic_production_chapter_id, scene_no)
);

CREATE TABLE comic_production_pages (
  id TEXT PRIMARY KEY,
  comic_production_chapter_id TEXT NOT NULL REFERENCES comic_production_chapters(id) ON DELETE RESTRICT,
  planning_page_stable_key TEXT NOT NULL,
  page_no INTEGER NOT NULL CHECK(page_no > 0),
  created_at INTEGER NOT NULL,
  UNIQUE(comic_production_chapter_id, planning_page_stable_key),
  UNIQUE(comic_production_chapter_id, page_no)
);

CREATE TABLE comic_production_panels (
  id TEXT PRIMARY KEY,
  comic_production_page_id TEXT NOT NULL REFERENCES comic_production_pages(id) ON DELETE RESTRICT,
  comic_production_scene_id TEXT REFERENCES comic_production_scenes(id) ON DELETE RESTRICT,
  planning_panel_stable_key TEXT NOT NULL,
  panel_no INTEGER NOT NULL CHECK(panel_no > 0),
  spec_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(comic_production_page_id, planning_panel_stable_key),
  UNIQUE(comic_production_page_id, panel_no)
);

CREATE TABLE analysis_apply_receipt_entity_maps (
  analysis_apply_operation_id TEXT NOT NULL REFERENCES analysis_apply_operations(id) ON DELETE RESTRICT,
  entity_kind TEXT NOT NULL CHECK(entity_kind IN ('planning_chapter','production_chapter','production_scene','production_page','production_panel')),
  stable_key TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(analysis_apply_operation_id, entity_kind, stable_key),
  UNIQUE(analysis_apply_operation_id, entity_kind, entity_id)
);

-- v9 treated every source as a NovelWork operation. Adaptation operations own
-- adaptation-scoped sources instead, and both branches deliberately fail closed.
DROP TRIGGER analysis_apply_source_same_work;
CREATE TRIGGER analysis_apply_source_scope_insert
BEFORE INSERT ON analysis_apply_operation_sources
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op
    JOIN analysis_artifact_revisions revision ON revision.id=NEW.analysis_artifact_revision_id
    JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE op.id=NEW.analysis_apply_operation_id
      AND ((op.novel_work_id IS NOT NULL AND op.novel_work_id=artifact.novel_work_id)
        OR (op.comic_adaptation_id IS NOT NULL AND op.comic_adaptation_id=artifact.comic_adaptation_id))
  ) THEN RAISE(ABORT, 'apply source must belong to operation scope') END;
END;
CREATE TRIGGER analysis_apply_source_scope_update
BEFORE UPDATE OF analysis_apply_operation_id, analysis_artifact_revision_id ON analysis_apply_operation_sources
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op
    JOIN analysis_artifact_revisions revision ON revision.id=NEW.analysis_artifact_revision_id
    JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE op.id=NEW.analysis_apply_operation_id
      AND ((op.novel_work_id IS NOT NULL AND op.novel_work_id=artifact.novel_work_id)
        OR (op.comic_adaptation_id IS NOT NULL AND op.comic_adaptation_id=artifact.comic_adaptation_id))
  ) THEN RAISE(ABORT, 'apply source must belong to operation scope') END;
END;

CREATE TRIGGER comic_plan_head_scope_insert
BEFORE INSERT ON comic_adaptation_plan_heads
BEGIN
  SELECT CASE WHEN NEW.status<>'active' THEN RAISE(ABORT, 'new planning head must be active') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op
    WHERE op.id=NEW.accept_apply_operation_id AND op.operation_type='accept_adaptation'
      AND op.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'plan head must belong to accept adaptation operation') END;
  SELECT CASE WHEN EXISTS (
    SELECT 1 FROM analysis_apply_receipts receipt WHERE receipt.analysis_apply_operation_id=NEW.accept_apply_operation_id
  ) THEN RAISE(ABORT, 'accepted operation receipt cannot create a new planning head') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.adaptation_proposal_revision_id AND revision.status='adopted'
      AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='adaptation_proposal'
      AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'adoption proposal must be adopted and scoped') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.comic_chapter_plan_revision_id AND revision.status='adopted'
      AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='comic_chapter_plan'
      AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'chapter plan must be adopted and scoped') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.scene_plan_revision_id AND revision.status='adopted'
      AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='scene_plan'
      AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'scene plan must be adopted and scoped') END;
END;
CREATE TRIGGER comic_plan_head_scope_update
BEFORE UPDATE OF comic_adaptation_id, adaptation_proposal_revision_id, comic_chapter_plan_revision_id, scene_plan_revision_id, accept_apply_operation_id ON comic_adaptation_plan_heads
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op WHERE op.id=NEW.accept_apply_operation_id AND op.operation_type='accept_adaptation' AND op.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'plan head must belong to accept adaptation operation') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.adaptation_proposal_revision_id AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='adaptation_proposal' AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'adoption proposal must be adopted and scoped') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.comic_chapter_plan_revision_id AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='comic_chapter_plan' AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'chapter plan must be adopted and scoped') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.scene_plan_revision_id AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='scene_plan' AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'scene plan must be adopted and scoped') END;
END;
CREATE TRIGGER comic_plan_head_identity_immutable
BEFORE UPDATE OF comic_adaptation_id, adaptation_proposal_revision_id, comic_chapter_plan_revision_id, scene_plan_revision_id, accept_apply_operation_id ON comic_adaptation_plan_heads
BEGIN
  SELECT RAISE(ABORT, 'planning head identity is immutable');
END;
CREATE TRIGGER comic_plan_head_status_transition
BEFORE UPDATE OF status ON comic_adaptation_plan_heads
BEGIN
  SELECT CASE WHEN NOT (OLD.status='active' AND NEW.status='superseded')
    THEN RAISE(ABORT, 'planning head status can only supersede active head') END;
END;

CREATE TRIGGER comic_planning_chapter_scope
BEFORE INSERT ON comic_planning_chapters
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_adaptation_plan_heads head
    WHERE head.id=NEW.comic_adaptation_plan_head_id AND head.comic_adaptation_id=NEW.comic_adaptation_id AND head.status='active'
  ) THEN RAISE(ABORT, 'planning chapter must belong to active adaptation plan head') END;
END;
CREATE TRIGGER comic_planning_chapter_identity_immutable
BEFORE UPDATE OF comic_adaptation_plan_head_id, comic_adaptation_id, planning_chapter_stable_key ON comic_planning_chapters
BEGIN
  SELECT RAISE(ABORT, 'planning chapter identity is immutable');
END;
CREATE TRIGGER comic_planning_chapter_source_scope
BEFORE INSERT ON comic_planning_chapter_sources
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_planning_chapters planning
    JOIN comic_adaptations adaptation ON adaptation.id=planning.comic_adaptation_id
    JOIN novel_chapter_revisions revision ON revision.id=NEW.novel_chapter_revision_id
    JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
    WHERE planning.id=NEW.comic_planning_chapter_id AND chapter.novel_work_id=adaptation.novel_work_id
      AND NEW.source_end<=length(revision.content)
  ) THEN RAISE(ABORT, 'planning source must belong to adaptation novel work') END;
END;
CREATE TRIGGER comic_planning_chapter_source_immutable
BEFORE UPDATE ON comic_planning_chapter_sources
BEGIN
  SELECT RAISE(ABORT, 'planning chapter source is immutable');
END;

CREATE TRIGGER comic_snapshot_v11_insert
BEFORE INSERT ON comic_scene_context_snapshots
BEGIN
  SELECT CASE WHEN NEW.status<>'provisional' THEN RAISE(ABORT, 'new scene snapshot must be provisional') END;
  SELECT CASE WHEN NEW.continuity_state_version_id IS NULL OR NOT EXISTS (
    SELECT 1 FROM comic_adaptations adaptation
    JOIN continuity_state_versions continuity ON continuity.id=NEW.continuity_state_version_id
    WHERE adaptation.id=NEW.comic_adaptation_id
      AND continuity.comic_adaptation_id=adaptation.id
      AND adaptation.current_continuity_version_id=continuity.id
  ) THEN RAISE(ABORT, 'snapshot requires current adaptation continuity baseline') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.scene_plan_revision_id AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='scene_plan' AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'snapshot scene plan must be adopted and scoped') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.adaptation_plan_revision_id AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='adaptation_proposal' AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'snapshot adaptation plan must be adopted and scoped') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_adaptation_plan_heads head
    WHERE head.comic_adaptation_id=NEW.comic_adaptation_id AND head.status='active'
      AND head.scene_plan_revision_id=NEW.scene_plan_revision_id
      AND head.adaptation_proposal_revision_id=NEW.adaptation_plan_revision_id
  ) THEN RAISE(ABORT, 'snapshot must use active adaptation planning head') END;
END;
CREATE TRIGGER comic_snapshot_approval_insert
BEFORE INSERT ON comic_scene_context_snapshot_approvals
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_scene_context_snapshots snapshot
    WHERE snapshot.id=NEW.scene_context_snapshot_id AND snapshot.status='provisional' AND snapshot.context_fingerprint=NEW.approved_context_fingerprint
  ) THEN RAISE(ABORT, 'snapshot approval must match provisional snapshot fingerprint') END;
END;
CREATE TRIGGER comic_snapshot_status_transition
BEFORE UPDATE OF status ON comic_scene_context_snapshots
BEGIN
  SELECT CASE WHEN NOT (
    (OLD.status='provisional' AND NEW.status IN ('approved','stale','rejected')) OR
    (OLD.status='approved' AND NEW.status IN ('frozen','stale','rejected'))
  ) THEN RAISE(ABORT, 'invalid scene snapshot status transition') END;
  SELECT CASE WHEN NEW.status='approved' AND NOT EXISTS (
    SELECT 1 FROM comic_scene_context_snapshot_approvals approval
    WHERE approval.scene_context_snapshot_id=NEW.id AND approval.approved_context_fingerprint=NEW.context_fingerprint
  ) THEN RAISE(ABORT, 'approved snapshot requires matching approval') END;
END;
CREATE TRIGGER comic_snapshot_frozen_immutable
BEFORE UPDATE ON comic_scene_context_snapshots WHEN OLD.status='frozen'
BEGIN
  SELECT RAISE(ABORT, 'frozen scene snapshot is immutable');
END;
CREATE TRIGGER comic_snapshot_approved_content_immutable
BEFORE UPDATE OF novel_work_id, comic_adaptation_id, scene_plan_revision_id, planning_scene_stable_key,
  working_context_revision_id, novel_canon_version_id, novel_state_version_id, continuity_state_version_id,
  adaptation_plan_revision_id, resolver_contract_version, prompt_compiler_contract_version,
  context_fingerprint, resolved_context_json, resolved_context_hash ON comic_scene_context_snapshots
WHEN OLD.status<>'provisional'
BEGIN
  SELECT RAISE(ABORT, 'approved scene snapshot content is immutable');
END;
CREATE TRIGGER comic_snapshot_scope_update
BEFORE UPDATE OF novel_work_id, comic_adaptation_id, scene_plan_revision_id, adaptation_plan_revision_id,
  working_context_revision_id, novel_canon_version_id, novel_state_version_id, continuity_state_version_id ON comic_scene_context_snapshots
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_adaptations adaptation WHERE adaptation.id=NEW.comic_adaptation_id AND adaptation.novel_work_id=NEW.novel_work_id
  ) THEN RAISE(ABORT, 'adaptation must belong to snapshot novel work') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_canon_versions version WHERE version.id=NEW.novel_canon_version_id AND version.novel_work_id=NEW.novel_work_id
  ) THEN RAISE(ABORT, 'canon must belong to snapshot novel work') END;
  SELECT CASE WHEN NEW.novel_state_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM novel_state_versions version WHERE version.id=NEW.novel_state_version_id AND version.novel_work_id=NEW.novel_work_id
  ) THEN RAISE(ABORT, 'state must belong to snapshot novel work') END;
  SELECT CASE WHEN NEW.working_context_revision_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM novel_chapter_context_revisions context JOIN novel_analysis_lineages lineage ON lineage.id=context.novel_analysis_lineage_id
    WHERE context.id=NEW.working_context_revision_id AND lineage.novel_work_id=NEW.novel_work_id
  ) THEN RAISE(ABORT, 'working context must belong to snapshot novel work') END;
  SELECT CASE WHEN NEW.continuity_state_version_id IS NULL OR NOT EXISTS (
    SELECT 1 FROM comic_adaptations adaptation JOIN continuity_state_versions continuity ON continuity.id=NEW.continuity_state_version_id
    WHERE adaptation.id=NEW.comic_adaptation_id AND continuity.comic_adaptation_id=adaptation.id AND adaptation.current_continuity_version_id=continuity.id
  ) THEN RAISE(ABORT, 'snapshot requires current adaptation continuity baseline') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.scene_plan_revision_id AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='scene_plan' AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'snapshot scene plan must be adopted and scoped') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.adaptation_plan_revision_id AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='adaptation_proposal' AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'snapshot adaptation plan must be adopted and scoped') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_adaptation_plan_heads head
    WHERE head.comic_adaptation_id=NEW.comic_adaptation_id AND head.status='active'
      AND head.scene_plan_revision_id=NEW.scene_plan_revision_id
      AND head.adaptation_proposal_revision_id=NEW.adaptation_plan_revision_id
  ) THEN RAISE(ABORT, 'snapshot must use active adaptation planning head') END;
END;

CREATE TRIGGER comic_apply_selection_scope
BEFORE INSERT ON analysis_apply_scene_context_selections
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op JOIN comic_scene_context_snapshots snapshot ON snapshot.id=NEW.scene_context_snapshot_id
    WHERE op.id=NEW.apply_operation_id AND op.operation_type='apply_comic_plan' AND op.comic_adaptation_id=snapshot.comic_adaptation_id AND snapshot.status='approved'
  ) THEN RAISE(ABORT, 'apply selection must use approved snapshot in adaptation scope') END;
END;
CREATE TRIGGER comic_apply_selection_scope_update
BEFORE UPDATE OF apply_operation_id, planning_scene_stable_key, scene_context_snapshot_id ON analysis_apply_scene_context_selections
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op JOIN comic_scene_context_snapshots snapshot ON snapshot.id=NEW.scene_context_snapshot_id
    WHERE op.id=NEW.apply_operation_id AND op.operation_type='apply_comic_plan' AND op.comic_adaptation_id=snapshot.comic_adaptation_id AND snapshot.status='approved'
  ) THEN RAISE(ABORT, 'apply selection must use approved snapshot in adaptation scope') END;
END;

CREATE TRIGGER comic_production_chapter_scope
BEFORE INSERT ON comic_production_chapters
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op WHERE op.id=NEW.apply_operation_id AND op.operation_type='apply_comic_plan' AND op.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'production chapter must belong to comic apply operation') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_planning_chapters planning JOIN comic_adaptation_plan_heads head ON head.id=planning.comic_adaptation_plan_head_id
    WHERE planning.id=NEW.comic_planning_chapter_id AND planning.comic_adaptation_id=NEW.comic_adaptation_id AND planning.status='planning' AND head.status='active'
  ) THEN RAISE(ABORT, 'production chapter must use planning chapter in adaptation scope') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE revision.id=NEW.page_panel_plan_revision_id AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id AND artifact.artifact_type='page_panel_plan' AND artifact.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'page panel plan must be adopted and scoped') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_operation_sources source
    WHERE source.analysis_apply_operation_id=NEW.apply_operation_id AND source.analysis_artifact_revision_id=NEW.page_panel_plan_revision_id
  ) THEN RAISE(ABORT, 'production chapter page plan must be frozen apply source') END;
END;
CREATE TRIGGER comic_production_chapter_identity_immutable
BEFORE UPDATE OF comic_adaptation_id, comic_planning_chapter_id, page_panel_plan_revision_id, apply_operation_id ON comic_production_chapters
BEGIN
  SELECT RAISE(ABORT, 'production chapter identity is immutable');
END;
CREATE TRIGGER comic_production_scene_scope
BEFORE INSERT ON comic_production_scenes
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_production_chapters chapter
    JOIN comic_planning_chapters planning ON planning.id=chapter.comic_planning_chapter_id
    JOIN comic_adaptation_plan_heads head ON head.id=planning.comic_adaptation_plan_head_id
    JOIN analysis_apply_scene_context_selections selection ON selection.apply_operation_id=chapter.apply_operation_id AND selection.planning_scene_stable_key=NEW.planning_scene_stable_key AND selection.scene_context_snapshot_id=NEW.scene_context_snapshot_id
    JOIN comic_scene_context_snapshots snapshot ON snapshot.id=NEW.scene_context_snapshot_id
    WHERE chapter.id=NEW.comic_production_chapter_id AND snapshot.status='frozen'
      AND snapshot.comic_adaptation_id=chapter.comic_adaptation_id AND snapshot.scene_plan_revision_id=head.scene_plan_revision_id
  ) THEN RAISE(ABORT, 'production scene must use selected frozen snapshot from planning head') END;
END;
CREATE TRIGGER comic_production_scene_identity_immutable
BEFORE UPDATE OF comic_production_chapter_id, planning_scene_stable_key, scene_context_snapshot_id, scene_no ON comic_production_scenes
BEGIN
  SELECT RAISE(ABORT, 'production scene identity is immutable');
END;
CREATE TRIGGER comic_production_page_scope
BEFORE INSERT ON comic_production_pages
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_production_chapters chapter WHERE chapter.id=NEW.comic_production_chapter_id
  ) THEN RAISE(ABORT, 'production page must belong to production chapter') END;
END;
CREATE TRIGGER comic_production_page_identity_immutable
BEFORE UPDATE OF comic_production_chapter_id, planning_page_stable_key, page_no ON comic_production_pages
BEGIN
  SELECT RAISE(ABORT, 'production page identity is immutable');
END;
CREATE TRIGGER comic_production_panel_scope
BEFORE INSERT ON comic_production_panels
BEGIN
  SELECT CASE WHEN NEW.comic_production_scene_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM comic_production_pages page
    JOIN comic_production_scenes scene ON scene.id=NEW.comic_production_scene_id
    WHERE page.id=NEW.comic_production_page_id AND page.comic_production_chapter_id=scene.comic_production_chapter_id
  ) THEN RAISE(ABORT, 'optional panel scene must belong to page chapter') END;
END;
CREATE TRIGGER comic_production_panel_identity_immutable
BEFORE UPDATE OF comic_production_page_id, comic_production_scene_id, planning_panel_stable_key, panel_no ON comic_production_panels
BEGIN
  SELECT RAISE(ABORT, 'production panel identity is immutable');
END;

CREATE TRIGGER comic_apply_receipt_entity_map_scope
BEFORE INSERT ON analysis_apply_receipt_entity_maps
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_apply_receipts receipt WHERE receipt.analysis_apply_operation_id=NEW.analysis_apply_operation_id
  ) THEN RAISE(ABORT, 'entity map requires apply receipt') END;
  SELECT CASE WHEN NEW.entity_kind='planning_chapter' AND NOT EXISTS (
    SELECT 1 FROM analysis_apply_operations op JOIN comic_planning_chapters chapter ON chapter.id=NEW.entity_id
    JOIN comic_adaptation_plan_heads head ON head.id=chapter.comic_adaptation_plan_head_id
    WHERE op.id=NEW.analysis_apply_operation_id AND op.operation_type='accept_adaptation' AND head.accept_apply_operation_id=op.id AND chapter.planning_chapter_stable_key=NEW.stable_key
  ) THEN RAISE(ABORT, 'planning chapter map must belong to accept operation') END;
  SELECT CASE WHEN NEW.entity_kind='production_chapter' AND NOT EXISTS (
    SELECT 1 FROM comic_production_chapters chapter JOIN comic_planning_chapters planning ON planning.id=chapter.comic_planning_chapter_id
    WHERE chapter.id=NEW.entity_id AND chapter.apply_operation_id=NEW.analysis_apply_operation_id AND planning.planning_chapter_stable_key=NEW.stable_key
  ) THEN RAISE(ABORT, 'production chapter map must belong to apply operation') END;
  SELECT CASE WHEN NEW.entity_kind='production_scene' AND NOT EXISTS (
    SELECT 1 FROM comic_production_scenes scene JOIN comic_production_chapters chapter ON chapter.id=scene.comic_production_chapter_id WHERE scene.id=NEW.entity_id AND chapter.apply_operation_id=NEW.analysis_apply_operation_id AND scene.planning_scene_stable_key=NEW.stable_key
  ) THEN RAISE(ABORT, 'production scene map must belong to apply operation') END;
  SELECT CASE WHEN NEW.entity_kind='production_page' AND NOT EXISTS (
    SELECT 1 FROM comic_production_pages page JOIN comic_production_chapters chapter ON chapter.id=page.comic_production_chapter_id WHERE page.id=NEW.entity_id AND chapter.apply_operation_id=NEW.analysis_apply_operation_id AND page.planning_page_stable_key=NEW.stable_key
  ) THEN RAISE(ABORT, 'production page map must belong to apply operation') END;
  SELECT CASE WHEN NEW.entity_kind='production_panel' AND NOT EXISTS (
    SELECT 1 FROM comic_production_panels panel JOIN comic_production_pages page ON page.id=panel.comic_production_page_id JOIN comic_production_chapters chapter ON chapter.id=page.comic_production_chapter_id WHERE panel.id=NEW.entity_id AND chapter.apply_operation_id=NEW.analysis_apply_operation_id AND panel.planning_panel_stable_key=NEW.stable_key
  ) THEN RAISE(ABORT, 'production panel map must belong to apply operation') END;
END;
CREATE TRIGGER comic_apply_receipt_entity_map_immutable
BEFORE UPDATE ON analysis_apply_receipt_entity_maps
BEGIN
  SELECT RAISE(ABORT, 'apply receipt entity map is immutable');
END;
