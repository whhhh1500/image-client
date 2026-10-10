-- Each saved source revision needs its own immutable adaptation chapter scope.
-- Multiple revisions of the same novel chapter share its sequence number.
CREATE TABLE comic_adaptation_chapters_v22 (
  id TEXT PRIMARY KEY,
  comic_adaptation_id TEXT NOT NULL REFERENCES comic_adaptations(id) ON DELETE RESTRICT,
  novel_chapter_revision_id TEXT NOT NULL REFERENCES novel_chapter_revisions(id) ON DELETE RESTRICT,
  sequence_no INTEGER NOT NULL CHECK(sequence_no > 0),
  created_at INTEGER NOT NULL,
  UNIQUE(comic_adaptation_id, novel_chapter_revision_id)
);
INSERT INTO comic_adaptation_chapters_v22
  (id, comic_adaptation_id, novel_chapter_revision_id, sequence_no, created_at)
SELECT id, comic_adaptation_id, novel_chapter_revision_id, sequence_no, created_at
FROM comic_adaptation_chapters;
DROP TABLE comic_adaptation_chapters;
ALTER TABLE comic_adaptation_chapters_v22 RENAME TO comic_adaptation_chapters;
CREATE INDEX idx_comic_adaptation_chapters_revision ON comic_adaptation_chapters(novel_chapter_revision_id);
CREATE INDEX idx_comic_adaptation_chapters_sequence ON comic_adaptation_chapters(comic_adaptation_id, sequence_no);
