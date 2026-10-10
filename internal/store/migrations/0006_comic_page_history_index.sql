-- Cross-page comic history is ordered by project and its stable cursor key.
-- The page-scoped v5 index has page_no between project and the ordering keys,
-- so it cannot satisfy this query without a temporary sort.
CREATE INDEX IF NOT EXISTS idx_comic_page_runs_history
ON comic_page_runs(comic_project_id, created_at DESC, id DESC);
