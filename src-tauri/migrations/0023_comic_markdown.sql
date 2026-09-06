CREATE TABLE comic_md_documents (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL, novel_work_id TEXT NOT NULL REFERENCES novel_works(id),
 chapter_id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('settings','script','storyboard','page_prompt')),
 page_no INTEGER NOT NULL DEFAULT 0, markdown TEXT NOT NULL, revision INTEGER NOT NULL,
 dependencies TEXT NOT NULL, updated_at INTEGER NOT NULL,
 UNIQUE(novel_work_id,chapter_id,kind,page_no)
);
CREATE TABLE comic_md_revisions (
 document_id TEXT NOT NULL REFERENCES comic_md_documents(id), revision INTEGER NOT NULL,
 markdown TEXT NOT NULL, dependencies TEXT NOT NULL, created_at INTEGER NOT NULL,
 PRIMARY KEY(document_id,revision)
);
CREATE TABLE comic_md_jobs (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL, novel_work_id TEXT NOT NULL REFERENCES novel_works(id), chapter_id TEXT NOT NULL,
 kind TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('running','succeeded','failed','interrupted')),
 message TEXT, output_markdown TEXT, input_snapshot TEXT NOT NULL,
 completed_pages INTEGER NOT NULL DEFAULT 0,total_pages INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX comic_md_one_running_job ON comic_md_jobs(novel_work_id) WHERE status='running';
CREATE TABLE comic_md_images (
 id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES comic_md_jobs(id),
 document_id TEXT NOT NULL REFERENCES comic_md_documents(id),document_revision INTEGER NOT NULL,
 page_no INTEGER NOT NULL,path TEXT NOT NULL,created_at INTEGER NOT NULL,
 FOREIGN KEY(document_id,document_revision) REFERENCES comic_md_revisions(document_id,revision)
);
