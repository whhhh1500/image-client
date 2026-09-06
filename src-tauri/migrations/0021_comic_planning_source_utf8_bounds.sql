-- v21: source selections are stored as UTF-8 byte offsets. SQLite's
-- length(TEXT) counts characters, so preserve every existing scope/identity
-- guard while comparing the upper bound to the UTF-8 byte length instead.

DROP TRIGGER comic_planning_chapter_source_scope;

CREATE TRIGGER comic_planning_chapter_source_scope
BEFORE INSERT ON comic_planning_chapter_sources
BEGIN
  SELECT CASE WHEN NOT EXISTS (
    SELECT 1 FROM comic_planning_chapters planning
    JOIN comic_adaptations adaptation ON adaptation.id=planning.comic_adaptation_id
    JOIN novel_chapter_revisions revision ON revision.id=NEW.novel_chapter_revision_id
    JOIN novel_chapters chapter ON chapter.id=revision.novel_chapter_id
    WHERE planning.id=NEW.comic_planning_chapter_id AND chapter.novel_work_id=adaptation.novel_work_id
      AND NEW.source_end<=length(CAST(revision.content AS BLOB))
  ) THEN RAISE(ABORT, 'planning source must belong to adaptation novel work') END;
END;
