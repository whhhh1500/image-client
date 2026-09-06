-- v12: adaptation analysis is a distinct, frozen four-output workflow.
-- It consumes completed source analysis or adopted original artifacts; it never
-- writes the ten original NovelWork analysis artifact types again.

CREATE TABLE adaptation_analysis_runs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  comic_adaptation_id TEXT NOT NULL REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  comic_adaptation_chapter_id TEXT NOT NULL REFERENCES comic_adaptation_chapters(id) ON DELETE RESTRICT,
  input_mode TEXT NOT NULL CHECK(input_mode IN ('source_run','artifact_revisions')),
  source_analysis_run_id TEXT REFERENCES source_analysis_runs(id) ON DELETE RESTRICT,
  base_canon_version_id TEXT NOT NULL REFERENCES novel_canon_versions(id) ON DELETE RESTRICT,
  base_novel_state_version_id TEXT REFERENCES novel_state_versions(id) ON DELETE RESTRICT,
  base_continuity_state_version_id TEXT NOT NULL REFERENCES continuity_state_versions(id) ON DELETE RESTRICT,
  provider_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  prompt_version TEXT NOT NULL,
  schema_version TEXT NOT NULL,
  frozen_input_fingerprint TEXT NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL CHECK(status IN ('draft','queued','running','ready_for_review','error','stale','cancelled','unknown_manual')),
  attempt_no INTEGER NOT NULL DEFAULT 1 CHECK(attempt_no > 0),
  lease_owner TEXT,
  lease_expires_at INTEGER,
  heartbeat_at INTEGER,
  safe_error_code TEXT,
  safe_user_message TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  finished_at INTEGER,
  CHECK((input_mode='source_run' AND source_analysis_run_id IS NOT NULL) OR (input_mode='artifact_revisions' AND source_analysis_run_id IS NULL))
);
CREATE INDEX idx_adaptation_analysis_run_recovery ON adaptation_analysis_runs(status, lease_expires_at);
CREATE INDEX idx_adaptation_analysis_run_adaptation ON adaptation_analysis_runs(comic_adaptation_id, created_at DESC);

CREATE TABLE adaptation_analysis_run_inputs (
  adaptation_analysis_run_id TEXT NOT NULL REFERENCES adaptation_analysis_runs(id) ON DELETE RESTRICT,
  artifact_type TEXT NOT NULL CHECK(artifact_type IN ('chapter_summary','chapter_beats','world_facts','character_facts','faction_facts','location_facts','prop_facts','timeline_delta','continuity_delta','open_threads')),
  analysis_artifact_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  source_order INTEGER NOT NULL CHECK(source_order >= 0),
  PRIMARY KEY(adaptation_analysis_run_id, artifact_type),
  UNIQUE(adaptation_analysis_run_id, analysis_artifact_revision_id),
  UNIQUE(adaptation_analysis_run_id, source_order)
);

CREATE TABLE adaptation_analysis_run_attempts (
  id TEXT PRIMARY KEY,
  adaptation_analysis_run_id TEXT NOT NULL REFERENCES adaptation_analysis_runs(id) ON DELETE RESTRICT,
  attempt_no INTEGER NOT NULL CHECK(attempt_no > 0),
  parent_attempt_id TEXT REFERENCES adaptation_analysis_run_attempts(id) ON DELETE RESTRICT,
  status TEXT NOT NULL CHECK(status IN ('queued','running','success','error','stale','cancelled','abandoned')),
  lease_owner TEXT,
  lease_expires_at INTEGER,
  heartbeat_at INTEGER,
  safe_error_code TEXT,
  safe_user_message TEXT,
  created_at INTEGER NOT NULL,
  finished_at INTEGER,
  UNIQUE(adaptation_analysis_run_id, attempt_no)
);
CREATE INDEX idx_adaptation_analysis_attempt_recovery ON adaptation_analysis_run_attempts(status, lease_expires_at);

CREATE TABLE adaptation_analysis_run_events (
  id TEXT PRIMARY KEY,
  adaptation_analysis_run_id TEXT NOT NULL REFERENCES adaptation_analysis_runs(id) ON DELETE RESTRICT,
  seq INTEGER NOT NULL CHECK(seq > 0),
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(adaptation_analysis_run_id, seq)
);

CREATE TABLE adaptation_analysis_run_artifacts (
  adaptation_analysis_run_id TEXT NOT NULL REFERENCES adaptation_analysis_runs(id) ON DELETE RESTRICT,
  artifact_type TEXT NOT NULL CHECK(artifact_type IN ('adaptation_proposal','comic_chapter_plan','scene_plan','page_panel_plan')),
  analysis_artifact_id TEXT NOT NULL REFERENCES analysis_artifacts(id) ON DELETE RESTRICT,
  PRIMARY KEY(adaptation_analysis_run_id, artifact_type),
  UNIQUE(adaptation_analysis_run_id, analysis_artifact_id)
);

CREATE TRIGGER adaptation_analysis_run_scope_insert
BEFORE INSERT ON adaptation_analysis_runs
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_works work JOIN comic_adaptations adaptation ON adaptation.id=NEW.comic_adaptation_id
    JOIN comic_adaptation_chapters chapter ON chapter.id=NEW.comic_adaptation_chapter_id
    WHERE work.id=NEW.novel_work_id AND work.project_id=NEW.project_id AND work.status='active'
      AND adaptation.project_id=NEW.project_id AND adaptation.novel_work_id=work.id AND adaptation.status='active'
      AND chapter.comic_adaptation_id=adaptation.id
  ) THEN RAISE(ABORT, 'adaptation analysis run owner scope is invalid') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM novel_canon_versions version WHERE version.id=NEW.base_canon_version_id AND version.novel_work_id=NEW.novel_work_id
  ) THEN RAISE(ABORT, 'adaptation analysis canon baseline must belong to novel work') END;
  SELECT CASE WHEN NEW.base_novel_state_version_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM novel_state_versions version WHERE version.id=NEW.base_novel_state_version_id AND version.novel_work_id=NEW.novel_work_id
  ) THEN RAISE(ABORT, 'adaptation analysis state baseline must belong to novel work') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM continuity_state_versions version WHERE version.id=NEW.base_continuity_state_version_id AND version.comic_adaptation_id=NEW.comic_adaptation_id
  ) THEN RAISE(ABORT, 'adaptation analysis continuity baseline must belong to adaptation') END;
  SELECT CASE WHEN NEW.input_mode='source_run' AND NOT EXISTS (
    SELECT 1 FROM source_analysis_runs source JOIN novel_analysis_lineages lineage ON lineage.id=source.novel_analysis_lineage_id
    WHERE source.id=NEW.source_analysis_run_id AND lineage.novel_work_id=NEW.novel_work_id AND source.status='ready_for_review'
  ) THEN RAISE(ABORT, 'adaptation analysis source run must be ready and belong to novel work') END;
END;
CREATE TRIGGER adaptation_analysis_run_scope_update
BEFORE UPDATE OF project_id, novel_work_id, comic_adaptation_id, comic_adaptation_chapter_id, input_mode,
  source_analysis_run_id, base_canon_version_id, base_novel_state_version_id, base_continuity_state_version_id ON adaptation_analysis_runs
BEGIN
  SELECT RAISE(ABORT, 'adaptation analysis frozen owner and input scope is immutable');
END;
CREATE TRIGGER adaptation_analysis_run_frozen_fields_immutable
BEFORE UPDATE OF provider_id, model_id, prompt_version, schema_version, frozen_input_fingerprint, idempotency_key ON adaptation_analysis_runs
BEGIN
  SELECT RAISE(ABORT, 'adaptation analysis frozen request fields are immutable');
END;
CREATE TRIGGER adaptation_analysis_run_attempt_counter
BEFORE UPDATE OF attempt_no ON adaptation_analysis_runs
WHEN NEW.attempt_no<>OLD.attempt_no+1
BEGIN
  SELECT RAISE(ABORT, 'adaptation analysis attempt number must advance by one');
END;

CREATE TRIGGER adaptation_analysis_input_scope_insert
BEFORE INSERT ON adaptation_analysis_run_inputs
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM adaptation_analysis_runs run
    JOIN analysis_artifact_revisions revision ON revision.id=NEW.analysis_artifact_revision_id
    JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE run.id=NEW.adaptation_analysis_run_id AND run.status='draft'
      AND artifact.novel_work_id=run.novel_work_id AND artifact.artifact_type=NEW.artifact_type
      AND ((run.input_mode='source_run' AND artifact.source_analysis_run_id=run.source_analysis_run_id
            AND revision.status IN ('candidate','adopted'))
        OR (run.input_mode='artifact_revisions' AND revision.status='adopted' AND artifact.adopted_head_revision_id=revision.id))
  ) THEN RAISE(ABORT, 'adaptation analysis input must freeze original artifact revision in run scope') END;
END;
CREATE TRIGGER adaptation_analysis_input_immutable
BEFORE UPDATE ON adaptation_analysis_run_inputs
BEGIN
  SELECT RAISE(ABORT, 'adaptation analysis frozen input is immutable');
END;

CREATE TRIGGER adaptation_analysis_attempt_scope_insert
BEFORE INSERT ON adaptation_analysis_run_attempts
BEGIN
  SELECT CASE WHEN NEW.status IN ('queued','running') AND (NEW.lease_owner IS NULL OR NEW.lease_expires_at IS NULL)
    THEN RAISE(ABORT, 'active adaptation analysis attempt requires lease') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM adaptation_analysis_runs run
    WHERE run.id=NEW.adaptation_analysis_run_id AND run.attempt_no=NEW.attempt_no
      AND NEW.attempt_no=(SELECT COUNT(*)+1 FROM adaptation_analysis_run_attempts prior WHERE prior.adaptation_analysis_run_id=NEW.adaptation_analysis_run_id)
  ) THEN RAISE(ABORT, 'adaptation analysis attempt number must be current and contiguous') END;
  SELECT CASE WHEN (NEW.attempt_no=1 AND NEW.parent_attempt_id IS NOT NULL) OR (NEW.attempt_no>1 AND NOT EXISTS (
    SELECT 1 FROM adaptation_analysis_run_attempts parent
    WHERE parent.id=NEW.parent_attempt_id AND parent.adaptation_analysis_run_id=NEW.adaptation_analysis_run_id AND parent.attempt_no=NEW.attempt_no-1
  )) THEN RAISE(ABORT, 'adaptation analysis attempt parent must be preceding attempt in same run') END;
END;
CREATE TRIGGER adaptation_analysis_attempt_immutable
BEFORE UPDATE OF adaptation_analysis_run_id, attempt_no, parent_attempt_id ON adaptation_analysis_run_attempts
BEGIN
  SELECT RAISE(ABORT, 'adaptation analysis attempt lineage is immutable');
END;

CREATE TRIGGER adaptation_analysis_artifact_scope_insert
BEFORE INSERT ON analysis_artifacts
WHEN NEW.adaptation_analysis_run_id IS NOT NULL
BEGIN
  SELECT CASE WHEN NEW.artifact_type NOT IN ('adaptation_proposal','comic_chapter_plan','scene_plan','page_panel_plan')
    THEN RAISE(ABORT, 'adaptation analysis cannot emit original analysis artifact type') END;
  SELECT CASE WHEN EXISTS (
    SELECT 1 FROM analysis_artifacts artifact
    WHERE artifact.adaptation_analysis_run_id=NEW.adaptation_analysis_run_id AND artifact.artifact_type=NEW.artifact_type
  ) THEN RAISE(ABORT, 'adaptation analysis can emit each output type only once') END;
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM adaptation_analysis_runs run
    WHERE run.id=NEW.adaptation_analysis_run_id AND run.novel_work_id=NEW.novel_work_id
      AND run.comic_adaptation_id=NEW.comic_adaptation_id
      AND run.status='running'
      AND ((NEW.artifact_type='adaptation_proposal' AND NEW.comic_chapter_id IS NULL)
        OR (NEW.artifact_type IN ('comic_chapter_plan','scene_plan','page_panel_plan') AND NEW.comic_chapter_id=run.comic_adaptation_chapter_id))
  ) THEN RAISE(ABORT, 'adaptation analysis artifact owner scope is invalid') END;
END;
CREATE TRIGGER adaptation_analysis_artifact_scope_update
BEFORE UPDATE OF adaptation_analysis_run_id, artifact_type, novel_work_id, comic_adaptation_id, comic_chapter_id ON analysis_artifacts
WHEN OLD.adaptation_analysis_run_id IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'adaptation analysis artifact owner scope is immutable');
END;

CREATE TRIGGER adaptation_analysis_artifact_map_insert
BEFORE INSERT ON adaptation_analysis_run_artifacts
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM analysis_artifacts artifact
    WHERE artifact.id=NEW.analysis_artifact_id AND artifact.adaptation_analysis_run_id=NEW.adaptation_analysis_run_id
      AND artifact.artifact_type=NEW.artifact_type
  ) THEN RAISE(ABORT, 'adaptation analysis artifact map must match emitted artifact') END;
END;

CREATE TRIGGER adaptation_analysis_run_status_transition
BEFORE UPDATE OF status ON adaptation_analysis_runs
BEGIN
  SELECT CASE WHEN NOT (
    (OLD.status='draft' AND NEW.status IN ('queued','cancelled','error')) OR
    (OLD.status='queued' AND NEW.status IN ('running','error','stale','cancelled')) OR
    (OLD.status='running' AND NEW.status IN ('ready_for_review','error','stale','cancelled','unknown_manual')) OR
    (OLD.status IN ('error','stale','cancelled','unknown_manual') AND NEW.status='queued')
  ) THEN RAISE(ABORT, 'invalid adaptation analysis status transition') END;
  SELECT CASE WHEN NEW.status IN ('queued','running') AND (NEW.lease_owner IS NULL OR NEW.lease_expires_at IS NULL)
    THEN RAISE(ABORT, 'active adaptation analysis run requires lease') END;
  SELECT CASE WHEN NEW.status IN ('queued','running') AND NOT EXISTS (
    SELECT 1 FROM adaptation_analysis_run_attempts attempt
    WHERE attempt.adaptation_analysis_run_id=NEW.id AND attempt.attempt_no=NEW.attempt_no
      AND attempt.status=NEW.status AND attempt.lease_owner=NEW.lease_owner
      AND attempt.lease_expires_at=NEW.lease_expires_at
  ) THEN RAISE(ABORT, 'adaptation analysis run must mirror current leased attempt') END;
  SELECT CASE WHEN NEW.status='queued' AND (
    (SELECT COUNT(*) FROM adaptation_analysis_run_inputs input WHERE input.adaptation_analysis_run_id=NEW.id)<>10
    OR EXISTS (
      SELECT 1 FROM adaptation_analysis_run_inputs input WHERE input.adaptation_analysis_run_id=NEW.id
      GROUP BY input.artifact_type HAVING COUNT(*)<>1
    )
  ) THEN RAISE(ABORT, 'adaptation analysis requires exactly ten frozen original artifact inputs') END;
  SELECT CASE WHEN NEW.status='ready_for_review' AND (
    (SELECT COUNT(*) FROM adaptation_analysis_run_artifacts artifact WHERE artifact.adaptation_analysis_run_id=NEW.id)<>4
    OR EXISTS (
      SELECT 1 FROM adaptation_analysis_run_artifacts artifact WHERE artifact.adaptation_analysis_run_id=NEW.id
      GROUP BY artifact.artifact_type HAVING COUNT(*)<>1
    )
  ) THEN RAISE(ABORT, 'adaptation analysis must emit exactly four adaptation artifacts') END;
  SELECT CASE WHEN NEW.status='ready_for_review' AND NOT EXISTS (
    SELECT 1 FROM adaptation_analysis_run_attempts attempt
    WHERE attempt.adaptation_analysis_run_id=NEW.id AND attempt.attempt_no=NEW.attempt_no AND attempt.status='success'
  ) THEN RAISE(ABORT, 'ready adaptation analysis must have successful current attempt') END;
END;

-- A planning head is an immutable selection, not a bag of independently
-- adopted artifacts.  New v12 outputs share an adaptation-analysis run;
-- legacy automatic outputs share a source-analysis run.  Never permit a
-- mixed or cross-run head, even when all revisions happen to share an
-- adaptation owner.
CREATE TRIGGER comic_plan_head_lineage_insert
BEFORE INSERT ON comic_adaptation_plan_heads
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM analysis_artifact_revisions proposal_revision
    JOIN analysis_artifacts proposal ON proposal.id=proposal_revision.analysis_artifact_id
    JOIN analysis_artifact_revisions chapter_revision ON chapter_revision.id=NEW.comic_chapter_plan_revision_id
    JOIN analysis_artifacts chapter_plan ON chapter_plan.id=chapter_revision.analysis_artifact_id
    JOIN analysis_artifact_revisions scene_revision ON scene_revision.id=NEW.scene_plan_revision_id
    JOIN analysis_artifacts scene_plan ON scene_plan.id=scene_revision.analysis_artifact_id
    JOIN comic_adaptation_chapters adaptation_chapter ON adaptation_chapter.id=chapter_plan.comic_chapter_id
    LEFT JOIN adaptation_analysis_runs run ON run.id=proposal.adaptation_analysis_run_id
    WHERE proposal_revision.id=NEW.adaptation_proposal_revision_id
      AND proposal.comic_adaptation_id=NEW.comic_adaptation_id
      AND chapter_plan.comic_adaptation_id=NEW.comic_adaptation_id
      AND scene_plan.comic_adaptation_id=NEW.comic_adaptation_id
      AND chapter_plan.comic_chapter_id IS NOT NULL
      AND scene_plan.comic_chapter_id=chapter_plan.comic_chapter_id
      AND adaptation_chapter.comic_adaptation_id=NEW.comic_adaptation_id
      AND (
        (proposal.adaptation_analysis_run_id IS NOT NULL
          AND proposal.source_analysis_run_id IS NULL
          AND chapter_plan.adaptation_analysis_run_id=proposal.adaptation_analysis_run_id
          AND chapter_plan.source_analysis_run_id IS NULL
          AND scene_plan.adaptation_analysis_run_id=proposal.adaptation_analysis_run_id
          AND scene_plan.source_analysis_run_id IS NULL
          AND run.comic_adaptation_chapter_id=chapter_plan.comic_chapter_id)
        OR
        (proposal.source_analysis_run_id IS NOT NULL
          AND proposal.adaptation_analysis_run_id IS NULL
          AND chapter_plan.source_analysis_run_id=proposal.source_analysis_run_id
          AND chapter_plan.adaptation_analysis_run_id IS NULL
          AND scene_plan.source_analysis_run_id=proposal.source_analysis_run_id
          AND scene_plan.adaptation_analysis_run_id IS NULL)
      )
  ) THEN RAISE(ABORT, 'planning head revisions must share one complete analysis lineage') END;
END;

-- The page plan is frozen at production apply, but must still be from the
-- same lineage as the accepted head.  Optimizing a revision preserves its
-- analysis_artifact_id, so this intentionally compares artifact lineage,
-- rather than requiring an original revision id.
CREATE TRIGGER comic_production_chapter_lineage_insert
BEFORE INSERT ON comic_production_chapters
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM comic_planning_chapters planning
    JOIN comic_adaptation_plan_heads head ON head.id=planning.comic_adaptation_plan_head_id
    JOIN analysis_artifact_revisions proposal_revision ON proposal_revision.id=head.adaptation_proposal_revision_id
    JOIN analysis_artifacts proposal ON proposal.id=proposal_revision.analysis_artifact_id
    JOIN analysis_artifact_revisions chapter_revision ON chapter_revision.id=head.comic_chapter_plan_revision_id
    JOIN analysis_artifacts chapter_plan ON chapter_plan.id=chapter_revision.analysis_artifact_id
    JOIN analysis_artifact_revisions scene_revision ON scene_revision.id=head.scene_plan_revision_id
    JOIN analysis_artifacts scene_plan ON scene_plan.id=scene_revision.analysis_artifact_id
    JOIN analysis_artifact_revisions page_revision ON page_revision.id=NEW.page_panel_plan_revision_id
    JOIN analysis_artifacts page_plan ON page_plan.id=page_revision.analysis_artifact_id
    WHERE planning.id=NEW.comic_planning_chapter_id
      AND page_plan.comic_adaptation_id=NEW.comic_adaptation_id
      AND page_plan.comic_chapter_id=chapter_plan.comic_chapter_id
      AND page_plan.comic_chapter_id=scene_plan.comic_chapter_id
      AND (
        (proposal.adaptation_analysis_run_id IS NOT NULL
          AND proposal.source_analysis_run_id IS NULL
          AND chapter_plan.adaptation_analysis_run_id=proposal.adaptation_analysis_run_id
          AND chapter_plan.source_analysis_run_id IS NULL
          AND scene_plan.adaptation_analysis_run_id=proposal.adaptation_analysis_run_id
          AND scene_plan.source_analysis_run_id IS NULL
          AND page_plan.adaptation_analysis_run_id=proposal.adaptation_analysis_run_id
          AND page_plan.source_analysis_run_id IS NULL)
        OR
        (proposal.source_analysis_run_id IS NOT NULL
          AND proposal.adaptation_analysis_run_id IS NULL
          AND chapter_plan.source_analysis_run_id=proposal.source_analysis_run_id
          AND chapter_plan.adaptation_analysis_run_id IS NULL
          AND scene_plan.source_analysis_run_id=proposal.source_analysis_run_id
          AND scene_plan.adaptation_analysis_run_id IS NULL
          AND page_plan.source_analysis_run_id=proposal.source_analysis_run_id
          AND page_plan.adaptation_analysis_run_id IS NULL)
      )
  ) THEN RAISE(ABORT, 'production page plan must share accepted planning lineage') END;
END;
