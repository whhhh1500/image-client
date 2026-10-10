-- Novel workbench v7.  This is append-only: v1-v6 comic tables remain
-- untouched while new novel/adaptation records use explicit ownership keys.

CREATE TABLE novel_works (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  title TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL CHECK(status IN ('active','archived')),
  published_canon_version_id TEXT,
  current_novel_state_version_id TEXT,
  current_analysis_lineage_id TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_novel_works_project ON novel_works(project_id, status, created_at, id);

CREATE TABLE novel_volumes (
  id TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  volume_no INTEGER NOT NULL CHECK(volume_no > 0),
  title TEXT,
  created_at INTEGER NOT NULL,
  UNIQUE(novel_work_id, volume_no)
);

CREATE TABLE novel_chapters (
  id TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  volume_id TEXT REFERENCES novel_volumes(id) ON DELETE RESTRICT,
  sequence_no INTEGER NOT NULL CHECK(sequence_no > 0),
  chapter_no INTEGER NOT NULL CHECK(chapter_no > 0),
  title TEXT,
  current_revision_id TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(novel_work_id, sequence_no),
  UNIQUE(volume_id, chapter_no)
);
CREATE INDEX idx_novel_chapters_work_sequence ON novel_chapters(novel_work_id, sequence_no);

CREATE TABLE novel_chapter_revisions (
  id TEXT PRIMARY KEY,
  novel_chapter_id TEXT NOT NULL REFERENCES novel_chapters(id) ON DELETE RESTRICT,
  version INTEGER NOT NULL CHECK(version > 0),
  content TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  asset_id TEXT REFERENCES assets(id) ON DELETE RESTRICT,
  requested_parent_context_revision_id TEXT REFERENCES novel_chapter_context_revisions(id) ON DELETE RESTRICT,
  source_kind TEXT NOT NULL CHECK(source_kind IN ('paste','asset','legacy_import')),
  created_at INTEGER NOT NULL,
  UNIQUE(novel_chapter_id, version)
);
CREATE INDEX idx_novel_chapter_revisions_chapter ON novel_chapter_revisions(novel_chapter_id, version DESC);

CREATE TABLE novel_canon_versions (
  id TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  version INTEGER NOT NULL CHECK(version >= 0),
  parent_version_id TEXT REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  body_json TEXT NOT NULL,
  rendered_markdown TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('published','superseded')),
  created_at INTEGER NOT NULL,
  UNIQUE(novel_work_id, version)
);

CREATE TABLE novel_state_versions (
  id TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  version INTEGER NOT NULL CHECK(version >= 0),
  parent_version_id TEXT REFERENCES novel_state_versions(id) ON DELETE RESTRICT,
  through_novel_chapter_revision_id TEXT REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  body_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(novel_work_id, version)
);

CREATE TABLE novel_analysis_lineages (
  id TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  base_published_canon_version_id TEXT REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  base_published_novel_state_version_id TEXT REFERENCES novel_state_versions(id) ON DELETE RESTRICT,
  current_context_revision_id TEXT,
  continuous_through_sequence_no INTEGER NOT NULL DEFAULT 0 CHECK(continuous_through_sequence_no >= 0),
  status TEXT NOT NULL CHECK(status IN ('active','archived')),
  optimistic_version INTEGER NOT NULL DEFAULT 0 CHECK(optimistic_version >= 0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX idx_novel_lineages_work ON novel_analysis_lineages(novel_work_id, status, created_at);

CREATE TABLE novel_entities (
  id TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  entity_kind TEXT NOT NULL CHECK(entity_kind IN ('world_rule','character','faction','location','prop')),
  stable_key TEXT NOT NULL,
  lifecycle TEXT NOT NULL CHECK(lifecycle IN ('candidate','active','disputed','retired','merged','rejected')),
  candidate_lineage_id TEXT REFERENCES novel_analysis_lineages(id) ON DELETE RESTRICT,
  created_from_context_revision_id TEXT REFERENCES novel_chapter_context_revisions(id) ON DELETE RESTRICT,
  created_from_canon_version_id TEXT REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  CHECK((lifecycle <> 'candidate') OR candidate_lineage_id IS NOT NULL),
  UNIQUE(novel_work_id, stable_key)
);
CREATE INDEX idx_novel_entities_work_lifecycle ON novel_entities(novel_work_id, lifecycle, entity_kind);

CREATE TABLE source_analysis_runs (
  id TEXT PRIMARY KEY,
  novel_chapter_revision_id TEXT NOT NULL REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  novel_analysis_lineage_id TEXT NOT NULL REFERENCES novel_analysis_lineages(id) ON DELETE RESTRICT,
  base_working_context_revision_id TEXT REFERENCES novel_chapter_context_revisions(id) ON DELETE RESTRICT,
  base_canon_version_id TEXT REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  base_novel_state_version_id TEXT REFERENCES novel_state_versions(id) ON DELETE RESTRICT,
  frozen_input_fingerprint TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('queued','running','ready_for_review','error','stale','cancelled')),
  review_status TEXT NOT NULL DEFAULT 'pending',
  current_stage TEXT NOT NULL DEFAULT 'queued',
  prompt_version TEXT NOT NULL,
  schema_version TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  progress_json TEXT NOT NULL DEFAULT '{}',
  safe_error_code TEXT,
  safe_user_message TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  completed_at INTEGER,
  UNIQUE(idempotency_key)
);

CREATE TABLE analysis_artifacts (
  id TEXT PRIMARY KEY,
  source_analysis_run_id TEXT REFERENCES source_analysis_runs(id) ON DELETE RESTRICT,
  adaptation_analysis_run_id TEXT,
  artifact_type TEXT NOT NULL,
  novel_work_id TEXT REFERENCES novel_works(id) ON DELETE RESTRICT,
  novel_chapter_revision_id TEXT REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  comic_adaptation_id TEXT,
  comic_chapter_id TEXT,
  candidate_head_revision_id TEXT,
  adopted_head_revision_id TEXT,
  status TEXT NOT NULL CHECK(status IN ('active','archived')),
  optimistic_version INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  CHECK((source_analysis_run_id IS NOT NULL) <> (adaptation_analysis_run_id IS NOT NULL))
);

CREATE TABLE analysis_artifact_revisions (
  id TEXT PRIMARY KEY,
  analysis_artifact_id TEXT NOT NULL REFERENCES analysis_artifacts(id) ON DELETE RESTRICT,
  version INTEGER NOT NULL CHECK(version > 0),
  parent_revision_id TEXT REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  body_json TEXT NOT NULL,
  rendered_markdown TEXT NOT NULL,
  change_type TEXT NOT NULL,
  change_instruction TEXT,
  model TEXT,
  provenance_json TEXT NOT NULL DEFAULT '{}',
  validation_json TEXT NOT NULL DEFAULT '{}',
  status TEXT NOT NULL CHECK(status IN ('candidate','adopted','rejected','stale','needs_review')),
  created_at INTEGER NOT NULL,
  UNIQUE(analysis_artifact_id, version)
);

CREATE TABLE novel_chapter_context_revisions (
  id TEXT PRIMARY KEY,
  novel_analysis_lineage_id TEXT NOT NULL REFERENCES novel_analysis_lineages(id) ON DELETE RESTRICT,
  novel_chapter_revision_id TEXT NOT NULL REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  parent_context_revision_id TEXT REFERENCES novel_chapter_context_revisions(id) ON DELETE RESTRICT,
  source_analysis_run_id TEXT NOT NULL REFERENCES source_analysis_runs(id) ON DELETE RESTRICT,
  base_published_canon_version_id TEXT REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  base_published_novel_state_version_id TEXT REFERENCES novel_state_versions(id) ON DELETE RESTRICT,
  resolved_working_canon_json TEXT NOT NULL,
  resolved_working_canon_hash TEXT NOT NULL,
  resolved_working_state_json TEXT NOT NULL,
  resolved_working_state_hash TEXT NOT NULL,
  sequence_gap_json TEXT NOT NULL DEFAULT '[]',
  branch_kind TEXT NOT NULL CHECK(branch_kind IN ('main','detached_gap','rebase','conflicted')),
  status TEXT NOT NULL CHECK(status IN ('ready','provisional','needs_rebase','conflicted','rejected')),
  created_at INTEGER NOT NULL,
  UNIQUE(source_analysis_run_id)
);
CREATE INDEX idx_novel_context_lineage ON novel_chapter_context_revisions(novel_analysis_lineage_id, created_at, id);

CREATE TABLE novel_chapter_context_artifact_revisions (
  context_revision_id TEXT NOT NULL REFERENCES novel_chapter_context_revisions(id) ON DELETE RESTRICT,
  analysis_artifact_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  role TEXT NOT NULL,
  source_order INTEGER NOT NULL,
  PRIMARY KEY(context_revision_id, analysis_artifact_revision_id, role)
);

CREATE TABLE novel_chapter_context_entity_refs (
  context_revision_id TEXT NOT NULL REFERENCES novel_chapter_context_revisions(id) ON DELETE RESTRICT,
  novel_entity_id TEXT NOT NULL REFERENCES novel_entities(id) ON DELETE RESTRICT,
  origin_context_revision_id TEXT REFERENCES novel_chapter_context_revisions(id) ON DELETE RESTRICT,
  source_artifact_revision_id TEXT REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  usage_role TEXT NOT NULL,
  resolved_entity_json TEXT NOT NULL,
  resolved_entity_hash TEXT NOT NULL,
  source_order INTEGER NOT NULL,
  PRIMARY KEY(context_revision_id, novel_entity_id, usage_role)
);

CREATE TABLE novel_entity_aliases (
  id TEXT PRIMARY KEY,
  novel_entity_id TEXT NOT NULL REFERENCES novel_entities(id) ON DELETE RESTRICT,
  normalized_alias TEXT NOT NULL,
  display_alias TEXT NOT NULL,
  alias_type TEXT NOT NULL,
  introduced_context_revision_id TEXT REFERENCES novel_chapter_context_revisions(id) ON DELETE RESTRICT,
  introduced_canon_version_id TEXT REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  source_chapter_revision_id TEXT NOT NULL REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  source_start_utf8_byte INTEGER NOT NULL CHECK(source_start_utf8_byte >= 0),
  source_end_utf8_byte INTEGER NOT NULL CHECK(source_end_utf8_byte > source_start_utf8_byte),
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_novel_entity_aliases_lookup ON novel_entity_aliases(normalized_alias, novel_entity_id);

CREATE TABLE novel_entity_redirects (
  source_entity_id TEXT PRIMARY KEY REFERENCES novel_entities(id) ON DELETE RESTRICT,
  survivor_entity_id TEXT NOT NULL REFERENCES novel_entities(id) ON DELETE RESTRICT,
  effective_canon_version_id TEXT NOT NULL REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  approved_by TEXT NOT NULL,
  reason TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  CHECK(source_entity_id <> survivor_entity_id)
);

CREATE TABLE novel_entity_field_lock_events (
  id TEXT PRIMARY KEY,
  novel_entity_id TEXT NOT NULL REFERENCES novel_entities(id) ON DELETE RESTRICT,
  field_path TEXT NOT NULL,
  action TEXT NOT NULL CHECK(action IN ('lock','unlock')),
  effective_canon_version_id TEXT NOT NULL REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  reason TEXT,
  actor TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE comic_adaptations (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  title TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('active','archived')),
  config_json TEXT NOT NULL DEFAULT '{}',
  current_continuity_version_id TEXT,
  optimistic_version INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE continuity_state_versions (
  id TEXT PRIMARY KEY,
  comic_adaptation_id TEXT NOT NULL REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  version INTEGER NOT NULL CHECK(version >= 0),
  parent_version_id TEXT REFERENCES continuity_state_versions(id) ON DELETE RESTRICT,
  through_comic_chapter_id TEXT,
  body_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(comic_adaptation_id, version)
);

CREATE TABLE comic_card_revisions (
  id TEXT PRIMARY KEY,
  comic_card_id TEXT NOT NULL,
  comic_adaptation_id TEXT NOT NULL REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  novel_entity_id TEXT REFERENCES novel_entities(id) ON DELETE RESTRICT,
  version INTEGER NOT NULL CHECK(version > 0),
  parent_revision_id TEXT REFERENCES comic_card_revisions(id) ON DELETE RESTRICT,
  body_json TEXT NOT NULL,
  asset_id TEXT REFERENCES assets(id) ON DELETE RESTRICT,
  content_hash TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('candidate','approved','retired')),
  created_at INTEGER NOT NULL,
  UNIQUE(comic_card_id, version)
);

CREATE TABLE comic_scene_context_snapshots (
  id TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  comic_adaptation_id TEXT NOT NULL REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  scene_plan_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  planning_scene_stable_key TEXT NOT NULL,
  working_context_revision_id TEXT REFERENCES novel_chapter_context_revisions(id) ON DELETE RESTRICT,
  novel_canon_version_id TEXT NOT NULL REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  novel_state_version_id TEXT REFERENCES novel_state_versions(id) ON DELETE RESTRICT,
  continuity_state_version_id TEXT REFERENCES continuity_state_versions(id) ON DELETE RESTRICT,
  adaptation_plan_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  resolver_contract_version TEXT NOT NULL,
  prompt_compiler_contract_version TEXT NOT NULL,
  context_fingerprint TEXT NOT NULL,
  resolved_context_json TEXT NOT NULL,
  resolved_context_hash TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('provisional','approved','frozen','stale','rejected')),
  created_at INTEGER NOT NULL,
  UNIQUE(scene_plan_revision_id, planning_scene_stable_key, context_fingerprint)
);

CREATE TABLE comic_scene_context_entities (
  scene_context_snapshot_id TEXT NOT NULL REFERENCES comic_scene_context_snapshots(id) ON DELETE RESTRICT,
  novel_entity_id TEXT NOT NULL REFERENCES novel_entities(id) ON DELETE RESTRICT,
  origin_context_revision_id TEXT REFERENCES novel_chapter_context_revisions(id) ON DELETE RESTRICT,
  resolved_entity_hash TEXT NOT NULL,
  usage_role TEXT NOT NULL,
  source_order INTEGER NOT NULL,
  PRIMARY KEY(scene_context_snapshot_id, novel_entity_id, usage_role)
);

CREATE TABLE comic_scene_context_visual_cards (
  scene_context_snapshot_id TEXT NOT NULL REFERENCES comic_scene_context_snapshots(id) ON DELETE RESTRICT,
  comic_card_revision_id TEXT NOT NULL REFERENCES comic_card_revisions(id) ON DELETE RESTRICT,
  usage_role TEXT NOT NULL,
  source_order INTEGER NOT NULL,
  PRIMARY KEY(scene_context_snapshot_id, comic_card_revision_id, usage_role)
);

CREATE TABLE analysis_apply_operations (
  id TEXT PRIMARY KEY,
  operation_type TEXT NOT NULL CHECK(operation_type IN ('publish_canon','publish_novel_state','accept_adaptation','apply_comic_plan')),
  novel_work_id TEXT REFERENCES novel_works(id) ON DELETE RESTRICT,
  comic_adaptation_id TEXT REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  base_target_version_id TEXT,
  idempotency_key TEXT NOT NULL UNIQUE,
  preview_fingerprint TEXT NOT NULL,
  approval_token_hash TEXT NOT NULL,
  status TEXT NOT NULL,
  safe_error_code TEXT,
  safe_user_message TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  completed_at INTEGER,
  CHECK((novel_work_id IS NOT NULL) <> (comic_adaptation_id IS NOT NULL))
);

CREATE TABLE analysis_apply_scene_context_selections (
  apply_operation_id TEXT NOT NULL REFERENCES analysis_apply_operations(id) ON DELETE RESTRICT,
  planning_scene_stable_key TEXT NOT NULL,
  scene_context_snapshot_id TEXT NOT NULL REFERENCES comic_scene_context_snapshots(id) ON DELETE RESTRICT,
  source_order INTEGER NOT NULL,
  PRIMARY KEY(apply_operation_id, planning_scene_stable_key),
  UNIQUE(apply_operation_id, scene_context_snapshot_id)
);

CREATE TABLE comic_scene_context_bindings (
  comic_scene_id TEXT NOT NULL REFERENCES comic_scenes(id) ON DELETE RESTRICT,
  scene_context_snapshot_id TEXT NOT NULL REFERENCES comic_scene_context_snapshots(id) ON DELETE RESTRICT,
  bound_at INTEGER NOT NULL,
  UNIQUE(comic_scene_id),
  UNIQUE(scene_context_snapshot_id)
);

CREATE TABLE novel_operation_receipts (
  id TEXT PRIMARY KEY,
  command_name TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  response_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(command_name, idempotency_key)
);

CREATE TRIGGER novel_chapter_volume_same_work_insert
BEFORE INSERT ON novel_chapters WHEN NEW.volume_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_volumes WHERE id = NEW.volume_id AND novel_work_id = NEW.novel_work_id
  ) THEN RAISE(ABORT, 'volume must belong to novel work') END;
END;

CREATE TRIGGER novel_chapter_current_revision_same_chapter
BEFORE UPDATE OF current_revision_id ON novel_chapters
WHEN NEW.current_revision_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_chapter_revisions
    WHERE id = NEW.current_revision_id AND novel_chapter_id = NEW.id
  ) THEN RAISE(ABORT, 'current revision must belong to chapter') END;
END;

CREATE TRIGGER novel_entity_candidate_lineage_same_work
BEFORE INSERT ON novel_entities WHEN NEW.candidate_lineage_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_analysis_lineages WHERE id = NEW.candidate_lineage_id AND novel_work_id = NEW.novel_work_id
  ) THEN RAISE(ABORT, 'candidate lineage must belong to novel work') END;
END;

CREATE TRIGGER novel_context_parent_same_lineage
BEFORE INSERT ON novel_chapter_context_revisions WHEN NEW.parent_context_revision_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_chapter_context_revisions
    WHERE id = NEW.parent_context_revision_id AND novel_analysis_lineage_id = NEW.novel_analysis_lineage_id
  ) THEN RAISE(ABORT, 'context parent must belong to lineage') END;
END;

CREATE TRIGGER novel_context_run_same_lineage_and_revision
BEFORE INSERT ON novel_chapter_context_revisions
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM source_analysis_runs
    WHERE id = NEW.source_analysis_run_id
      AND novel_analysis_lineage_id = NEW.novel_analysis_lineage_id
      AND novel_chapter_revision_id = NEW.novel_chapter_revision_id
  ) THEN RAISE(ABORT, 'context run must match lineage and chapter revision') END;
END;

CREATE TRIGGER source_run_base_context_same_lineage
BEFORE INSERT ON source_analysis_runs WHEN NEW.base_working_context_revision_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_chapter_context_revisions
    WHERE id = NEW.base_working_context_revision_id
      AND novel_analysis_lineage_id = NEW.novel_analysis_lineage_id
  ) THEN RAISE(ABORT, 'run base context must belong to lineage') END;
END;

CREATE TRIGGER novel_context_gap_is_detached
BEFORE INSERT ON novel_chapter_context_revisions
WHEN NEW.sequence_gap_json <> '[]' AND NEW.branch_kind <> 'detached_gap'
BEGIN
  SELECT RAISE(ABORT, 'gap context must be detached');
END;

CREATE TRIGGER novel_lineage_head_scope
BEFORE UPDATE OF current_context_revision_id ON novel_analysis_lineages
WHEN NEW.current_context_revision_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_chapter_context_revisions
    WHERE id = NEW.current_context_revision_id
      AND novel_analysis_lineage_id = NEW.id
      AND branch_kind <> 'detached_gap'
  ) THEN RAISE(ABORT, 'lineage head must be a non-gap context in the lineage') END;
END;

CREATE TRIGGER novel_context_candidate_owner
BEFORE INSERT ON novel_chapter_context_entity_refs
BEGIN
  SELECT CASE WHEN EXISTS (
    SELECT 1 FROM novel_entities e
    JOIN novel_chapter_context_revisions c ON c.id = NEW.context_revision_id
    WHERE e.id = NEW.novel_entity_id AND e.lifecycle = 'candidate'
      AND e.candidate_lineage_id <> c.novel_analysis_lineage_id
  ) THEN RAISE(ABORT, 'candidate entity belongs to another lineage') END;
END;

CREATE TRIGGER novel_redirect_same_work_and_acyclic
BEFORE INSERT ON novel_entity_redirects
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_entities source JOIN novel_entities survivor
      ON source.novel_work_id = survivor.novel_work_id
    WHERE source.id = NEW.source_entity_id AND survivor.id = NEW.survivor_entity_id
  ) THEN RAISE(ABORT, 'redirect entities must belong to same novel work') END;
  SELECT CASE WHEN EXISTS (
    WITH RECURSIVE chain(id) AS (
      SELECT NEW.survivor_entity_id
      UNION ALL
      SELECT redirect.survivor_entity_id FROM novel_entity_redirects redirect
      JOIN chain ON redirect.source_entity_id = chain.id
    ) SELECT 1 FROM chain WHERE id = NEW.source_entity_id
  ) THEN RAISE(ABORT, 'entity redirect cycle') END;
END;

CREATE TRIGGER scene_snapshot_scope
BEFORE INSERT ON comic_scene_context_snapshots
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_adaptations a WHERE a.id = NEW.comic_adaptation_id AND a.novel_work_id = NEW.novel_work_id
  ) THEN RAISE(ABORT, 'adaptation must belong to snapshot novel work') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_canon_versions WHERE id = NEW.novel_canon_version_id AND novel_work_id = NEW.novel_work_id
  ) THEN RAISE(ABORT, 'canon must belong to snapshot novel work') END;
  SELECT CASE WHEN NEW.novel_state_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM novel_state_versions WHERE id = NEW.novel_state_version_id AND novel_work_id = NEW.novel_work_id
  ) THEN RAISE(ABORT, 'novel state must belong to snapshot novel work') END;
  SELECT CASE WHEN NEW.working_context_revision_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM novel_chapter_context_revisions context
    JOIN novel_analysis_lineages lineage ON lineage.id = context.novel_analysis_lineage_id
    WHERE context.id = NEW.working_context_revision_id AND lineage.novel_work_id = NEW.novel_work_id
  ) THEN RAISE(ABORT, 'working context must belong to snapshot novel work') END;
  SELECT CASE WHEN NEW.continuity_state_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM continuity_state_versions WHERE id = NEW.continuity_state_version_id AND comic_adaptation_id = NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'continuity must belong to snapshot adaptation') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision
    JOIN analysis_artifacts artifact ON artifact.id = revision.analysis_artifact_id
    WHERE revision.id = NEW.scene_plan_revision_id AND artifact.comic_adaptation_id = NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'scene plan must belong to snapshot adaptation') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifact_revisions revision
    JOIN analysis_artifacts artifact ON artifact.id = revision.analysis_artifact_id
    WHERE revision.id = NEW.adaptation_plan_revision_id AND artifact.comic_adaptation_id = NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'adaptation plan must belong to snapshot adaptation') END;
END;

CREATE TRIGGER scene_snapshot_entity_scope
BEFORE INSERT ON comic_scene_context_entities
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_scene_context_snapshots snapshot
    JOIN novel_entities entity ON entity.id = NEW.novel_entity_id
    WHERE snapshot.id = NEW.scene_context_snapshot_id AND entity.novel_work_id = snapshot.novel_work_id
  ) THEN RAISE(ABORT, 'scene entity must belong to snapshot novel work') END;
END;

CREATE TRIGGER scene_snapshot_card_scope
BEFORE INSERT ON comic_scene_context_visual_cards
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_scene_context_snapshots snapshot
    JOIN comic_card_revisions card ON card.id = NEW.comic_card_revision_id
    WHERE snapshot.id = NEW.scene_context_snapshot_id AND card.comic_adaptation_id = snapshot.comic_adaptation_id
  ) THEN RAISE(ABORT, 'scene card must belong to snapshot adaptation') END;
END;
