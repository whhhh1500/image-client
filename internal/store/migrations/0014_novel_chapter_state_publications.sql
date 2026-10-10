-- v14: immutable chapter-completion snapshots bridge an approved source
-- analysis to the existing NovelState head. They are audit records, not a
-- second state system: state_after_version_id always points at the new
-- novel_state_versions row created by the same transaction.

CREATE TABLE novel_chapter_state_publications (
  id TEXT PRIMARY KEY,
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id) ON DELETE RESTRICT,
  novel_chapter_revision_id TEXT NOT NULL REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  source_analysis_run_id TEXT NOT NULL REFERENCES source_analysis_runs(id) ON DELETE RESTRICT,
  adaptation_analysis_run_id TEXT REFERENCES adaptation_analysis_runs(id) ON DELETE RESTRICT,
  state_before_version_id TEXT NOT NULL REFERENCES novel_state_versions(id) ON DELETE RESTRICT,
  state_after_version_id TEXT NOT NULL UNIQUE REFERENCES novel_state_versions(id) ON DELETE RESTRICT,
  canon_delta_json TEXT NOT NULL CHECK(json_valid(canon_delta_json) AND json_type(canon_delta_json)='object'),
  continuity_delta_json TEXT NOT NULL CHECK(json_valid(continuity_delta_json) AND json_type(continuity_delta_json)='object'),
  source_fingerprint TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_novel_chapter_state_publications_inheritance
ON novel_chapter_state_publications(novel_work_id, created_at DESC, id DESC);
CREATE INDEX idx_novel_chapter_state_publications_chapter
ON novel_chapter_state_publications(novel_chapter_revision_id, created_at DESC, id DESC);

CREATE TABLE novel_chapter_state_publication_sources (
  novel_chapter_state_publication_id TEXT NOT NULL REFERENCES novel_chapter_state_publications(id) ON DELETE RESTRICT,
  analysis_artifact_revision_id TEXT NOT NULL REFERENCES analysis_artifact_revisions(id) ON DELETE RESTRICT,
  source_role TEXT NOT NULL CHECK(source_role IN ('canon_delta','continuity_delta')),
  source_order INTEGER NOT NULL CHECK(source_order >= 0),
  PRIMARY KEY(novel_chapter_state_publication_id, analysis_artifact_revision_id),
  UNIQUE(novel_chapter_state_publication_id, source_role, source_order)
);

CREATE TRIGGER novel_chapter_state_publication_scope_insert
BEFORE INSERT ON novel_chapter_state_publications
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM novel_chapter_revisions revision
    JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
    JOIN source_analysis_runs source ON source.id=NEW.source_analysis_run_id
    JOIN novel_analysis_lineages lineage ON lineage.id=source.novel_analysis_lineage_id
    JOIN novel_state_versions state_before ON state_before.id=NEW.state_before_version_id
    JOIN novel_state_versions state_after ON state_after.id=NEW.state_after_version_id
    WHERE revision.id=NEW.novel_chapter_revision_id
      AND chapter.novel_work_id=NEW.novel_work_id
      AND source.novel_chapter_revision_id=revision.id
      AND lineage.novel_work_id=NEW.novel_work_id
      AND source.status='ready_for_review'
      AND state_before.novel_work_id=NEW.novel_work_id
      AND state_after.novel_work_id=NEW.novel_work_id
      AND state_after.parent_version_id=state_before.id
      AND state_after.through_novel_chapter_revision_id=revision.id
  ) THEN RAISE(ABORT, 'chapter state publication source or state scope is invalid') END;
  SELECT CASE WHEN NEW.adaptation_analysis_run_id IS NOT NULL AND NOT EXISTS (
    SELECT 1
    FROM adaptation_analysis_runs run
    JOIN comic_adaptation_chapters adaptation_chapter ON adaptation_chapter.id=run.comic_adaptation_chapter_id
    JOIN comic_adaptation_plan_heads head ON head.comic_adaptation_id=run.comic_adaptation_id AND head.status='active'
    JOIN analysis_artifact_revisions proposal_revision ON proposal_revision.id=head.adaptation_proposal_revision_id
    JOIN analysis_artifacts proposal ON proposal.id=proposal_revision.analysis_artifact_id
    JOIN analysis_artifact_revisions chapter_plan_revision ON chapter_plan_revision.id=head.comic_chapter_plan_revision_id
    JOIN analysis_artifacts chapter_plan ON chapter_plan.id=chapter_plan_revision.analysis_artifact_id
    JOIN analysis_artifact_revisions scene_plan_revision ON scene_plan_revision.id=head.scene_plan_revision_id
    JOIN analysis_artifacts scene_plan ON scene_plan.id=scene_plan_revision.analysis_artifact_id
    WHERE run.id=NEW.adaptation_analysis_run_id
      AND run.novel_work_id=NEW.novel_work_id
      AND run.status='ready_for_review'
      AND adaptation_chapter.novel_chapter_revision_id=NEW.novel_chapter_revision_id
      AND (
        run.source_analysis_run_id=NEW.source_analysis_run_id
        OR (
          run.input_mode='artifact_revisions'
          AND (SELECT COUNT(*) FROM adaptation_analysis_run_inputs input
               JOIN analysis_artifact_revisions input_revision ON input_revision.id=input.analysis_artifact_revision_id
               JOIN analysis_artifacts input_artifact ON input_artifact.id=input_revision.analysis_artifact_id
               WHERE input.adaptation_analysis_run_id=run.id
                 AND input_artifact.source_analysis_run_id=NEW.source_analysis_run_id)=10
        )
      )
      AND proposal.adaptation_analysis_run_id=run.id
      AND chapter_plan.adaptation_analysis_run_id=run.id
      AND scene_plan.adaptation_analysis_run_id=run.id
      AND proposal_revision.status='adopted' AND proposal.adopted_head_revision_id=proposal_revision.id
      AND chapter_plan_revision.status='adopted' AND chapter_plan.adopted_head_revision_id=chapter_plan_revision.id
      AND scene_plan_revision.status='adopted' AND scene_plan.adopted_head_revision_id=scene_plan_revision.id
  ) THEN RAISE(ABORT, 'chapter state adaptation run must be approved for this chapter') END;
END;

CREATE TRIGGER novel_chapter_state_publication_immutable
BEFORE UPDATE ON novel_chapter_state_publications
BEGIN
  SELECT RAISE(ABORT, 'chapter state publication is immutable');
END;

CREATE TRIGGER novel_chapter_state_publication_source_scope_insert
BEFORE INSERT ON novel_chapter_state_publication_sources
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1
    FROM novel_chapter_state_publications publication
    JOIN analysis_artifact_revisions revision ON revision.id=NEW.analysis_artifact_revision_id
    JOIN analysis_artifacts artifact ON artifact.id=revision.analysis_artifact_id
    WHERE publication.id=NEW.novel_chapter_state_publication_id
      AND artifact.novel_work_id=publication.novel_work_id
      AND artifact.source_analysis_run_id=publication.source_analysis_run_id
      AND artifact.adopted_head_revision_id=revision.id
      AND revision.status='adopted'
      AND (
        (NEW.source_role='canon_delta' AND artifact.artifact_type IN ('world_facts','character_facts','faction_facts','location_facts','prop_facts'))
        OR
        (NEW.source_role='continuity_delta' AND artifact.artifact_type IN ('timeline_delta','continuity_delta','open_threads'))
      )
  ) THEN RAISE(ABORT, 'chapter state publication source must be an adopted source-run revision') END;
END;

CREATE TRIGGER novel_chapter_state_publication_source_immutable
BEFORE UPDATE ON novel_chapter_state_publication_sources
BEGIN
  SELECT RAISE(ABORT, 'chapter state publication source is immutable');
END;
