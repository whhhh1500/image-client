ALTER TABLE comic_md_documents ADD COLUMN optimization_instruction TEXT NOT NULL DEFAULT '';
ALTER TABLE comic_md_revisions ADD COLUMN optimization_instruction TEXT NOT NULL DEFAULT '';
ALTER TABLE comic_md_images ADD COLUMN prompt_injection TEXT NOT NULL DEFAULT '';
CREATE TABLE comic_md_render_options (
  chapter_id TEXT PRIMARY KEY REFERENCES novel_chapters(id),
  novel_work_id TEXT NOT NULL REFERENCES novel_works(id),
  project_id TEXT NOT NULL,
  prompt_injection TEXT NOT NULL DEFAULT '',
  revision INTEGER NOT NULL CHECK(revision > 0),
  updated_at INTEGER NOT NULL
);
