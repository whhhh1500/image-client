-- v10 keeps immutable execution history under a single frozen optimization
-- run.  Earlier schemas kept only the run's latest execution state.

CREATE TABLE novel_artifact_optimization_attempts (
  id TEXT PRIMARY KEY,
  novel_artifact_optimization_run_id TEXT NOT NULL REFERENCES novel_artifact_optimization_runs(id) ON DELETE RESTRICT,
  attempt_no INTEGER NOT NULL CHECK(attempt_no > 0),
  parent_attempt_id TEXT REFERENCES novel_artifact_optimization_attempts(id) ON DELETE RESTRICT,
  status TEXT NOT NULL CHECK(status IN ('queued','running','succeeded','error','cancelled','abandoned')),
  lease_owner TEXT,
  lease_expires_at INTEGER,
  heartbeat_at INTEGER,
  safe_error_code TEXT,
  safe_user_message TEXT,
  created_at INTEGER NOT NULL,
  finished_at INTEGER,
  UNIQUE(novel_artifact_optimization_run_id, attempt_no)
);
CREATE INDEX idx_novel_artifact_optimization_attempt_recovery
ON novel_artifact_optimization_attempts(status, lease_expires_at);

-- A v9 run has only its current snapshot.  It becomes attempt 1 even if an
-- old run-level counter reflected a prior cross-run retry: no nonexistent
-- in-run history is invented during migration.
INSERT INTO novel_artifact_optimization_attempts (
  id,
  novel_artifact_optimization_run_id,
  attempt_no,
  parent_attempt_id,
  status,
  lease_owner,
  lease_expires_at,
  heartbeat_at,
  safe_error_code,
  safe_user_message,
  created_at,
  finished_at
)
SELECT
  'noptattempt_' || id,
  id,
  1,
  NULL,
  CASE
    WHEN status = 'queued' THEN 'queued'
    WHEN status = 'running' AND lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL THEN 'running'
    WHEN status = 'running' THEN 'abandoned'
    WHEN status = 'ready' THEN 'succeeded'
    WHEN status = 'error' THEN 'error'
    WHEN status = 'cancel_requested' THEN 'cancelled'
    ELSE 'abandoned'
  END,
  lease_owner,
  lease_expires_at,
  NULL,
  CASE
    WHEN status = 'running' AND (lease_owner IS NULL OR lease_expires_at IS NULL)
      THEN COALESCE(safe_error_code, 'LEASE_UNAVAILABLE')
    WHEN status = 'unknown_manual'
      THEN COALESCE(safe_error_code, 'UNKNOWN_MANUAL')
    ELSE safe_error_code
  END,
  CASE
    WHEN status = 'running' AND (lease_owner IS NULL OR lease_expires_at IS NULL)
      THEN COALESCE(safe_user_message, '迁移时发现运行记录缺少租约，已标记为放弃')
    WHEN status = 'unknown_manual'
      THEN COALESCE(safe_user_message, '迁移时保留未知人工处理状态为放弃尝试')
    ELSE safe_user_message
  END,
  created_at,
  CASE
    WHEN status IN ('ready','error','cancel_requested','unknown_manual') AND finished_at IS NULL THEN updated_at
    WHEN status = 'running' AND (lease_owner IS NULL OR lease_expires_at IS NULL) AND finished_at IS NULL THEN updated_at
    ELSE finished_at
  END
FROM novel_artifact_optimization_runs;

-- The old counter could describe a retry chain made of separate run rows.
-- The v10 table has only the safely recoverable current snapshot, so keep the
-- run projection aligned with its new first attempt.
UPDATE novel_artifact_optimization_runs SET attempt_no = 1;

CREATE TRIGGER novel_artifact_optimization_attempt_parent_insert
BEFORE INSERT ON novel_artifact_optimization_attempts
BEGIN
  SELECT CASE WHEN NEW.attempt_no = 1 AND NEW.parent_attempt_id IS NOT NULL
    THEN RAISE(ABORT, 'first optimization attempt must not have a parent') END;
  SELECT CASE WHEN NEW.attempt_no > 1 AND NOT EXISTS (
    SELECT 1 FROM novel_artifact_optimization_attempts parent
    WHERE parent.id = NEW.parent_attempt_id
      AND parent.novel_artifact_optimization_run_id = NEW.novel_artifact_optimization_run_id
      AND parent.attempt_no = NEW.attempt_no - 1
  ) THEN RAISE(ABORT, 'optimization attempt parent must be the preceding attempt in the same run') END;
  SELECT CASE WHEN NEW.status = 'running' AND (NEW.lease_owner IS NULL OR NEW.lease_expires_at IS NULL)
    THEN RAISE(ABORT, 'running optimization attempt requires lease') END;
END;

CREATE TRIGGER novel_artifact_optimization_attempt_identity_immutable
BEFORE UPDATE OF novel_artifact_optimization_run_id, attempt_no, parent_attempt_id
ON novel_artifact_optimization_attempts
BEGIN
  SELECT RAISE(ABORT, 'optimization attempt lineage is immutable');
END;

CREATE TRIGGER novel_artifact_optimization_attempt_running_lease
BEFORE UPDATE OF status, lease_owner, lease_expires_at
ON novel_artifact_optimization_attempts
WHEN NEW.status = 'running' AND (NEW.lease_owner IS NULL OR NEW.lease_expires_at IS NULL)
BEGIN
  SELECT RAISE(ABORT, 'running optimization attempt requires lease');
END;
