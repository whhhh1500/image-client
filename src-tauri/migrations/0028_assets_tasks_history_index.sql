-- The asset library and task history are always read newest-first with no
-- filter, so both tables need a created_at index. Without one every refresh is
-- a full table scan plus a temporary B-tree sort.
CREATE INDEX IF NOT EXISTS idx_assets_created_at ON assets(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at DESC);
