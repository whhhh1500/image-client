use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{params_from_iter, Connection};
use serde::Serialize;
use serde_json::{Map, Number, Value};

const MIGRATION_V1: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_V2: &str = include_str!("../migrations/0002_comic.sql");
const MIGRATION_V3: &str = include_str!("../migrations/0003_comic_page_runs.sql");
const MIGRATION_V4: &str = include_str!("../migrations/0004_comic_panel_runs_error.sql");
const MIGRATION_V5: &str = include_str!("../migrations/0005_comic_reliability.sql");
const MIGRATION_V6: &str = include_str!("../migrations/0006_comic_page_history_index.sql");
const MIGRATION_V7: &str = include_str!("../migrations/0007_novel_workbench.sql");
const MIGRATION_V8: &str = include_str!("../migrations/0008_novel_analysis_runtime.sql");
const MIGRATION_V9: &str = include_str!("../migrations/0009_analysis_apply_lineage.sql");
const MIGRATION_V10: &str =
    include_str!("../migrations/0010_novel_artifact_optimization_attempts.sql");
const MIGRATION_V11: &str = include_str!("../migrations/0011_comic_adaptation_apply.sql");
const MIGRATION_V12: &str = include_str!("../migrations/0012_adaptation_analysis_runs.sql");
const MIGRATION_V13: &str = include_str!("../migrations/0013_comic_page_reviews.sql");
const MIGRATION_V14: &str = include_str!("../migrations/0014_novel_chapter_state_publications.sql");
const MIGRATION_V15: &str = include_str!("../migrations/0015_novel_production_jobs.sql");
const MIGRATION_V16: &str = include_str!("../migrations/0016_comic_visual_manifests.sql");
const MIGRATION_V17: &str = include_str!("../migrations/0017_comic_visual_page_runs.sql");
const MIGRATION_V18: &str = include_str!("../migrations/0018_comic_visual_batches.sql");
const MIGRATION_V19: &str = include_str!("../migrations/0019_comic_visual_exports.sql");
const MIGRATION_V20: &str = include_str!("../migrations/0020_production_comic_plan_intent.sql");
const MIGRATION_V21: &str =
    include_str!("../migrations/0021_comic_planning_source_utf8_bounds.sql");
const MIGRATION_V22: &str =
    include_str!("../migrations/0022_adaptation_chapter_revisions.sql");
const MIGRATION_V23: &str = include_str!("../migrations/0023_comic_markdown.sql");
const MIGRATION_V24: &str = include_str!("../migrations/0024_comic_markdown_optimization.sql");
const MIGRATION_V25: &str =
    include_str!("../migrations/0025_comic_markdown_rerun_prompt_injection.sql");
const SCHEMA_VERSION: i64 = 25;
const ALLOWED_TABLES: &[&str] = &["assets", "tasks", "settings", "workflows"];
const MAX_QUERY_CHARS: usize = 8_192;
const MAX_BIND_VALUES: usize = 128;
const MAX_BIND_STRING_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESULT_ROWS: usize = 50_000;
const BACKUP_PAGE_COUNT: i32 = 100;
const BACKUP_RETRY_DELAY: Duration = Duration::from_millis(25);
const BACKUP_TOTAL_TIMEOUT: Duration = Duration::from_secs(3);
const BACKUP_RETENTION_COUNT: usize = 3;

pub struct DbState {
    connection: Mutex<Connection>,
    app_session_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbQueryResult {
    rows_affected: usize,
    last_insert_id: Option<i64>,
}

impl DbState {
    pub fn open(path: PathBuf) -> Result<Self, std::io::Error> {
        let existed_before_open = path.is_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            crate::paths::secure_dir(parent)?;
        }
        let connection = Connection::open(&path)
            .map_err(|error| std::io::Error::other(format!("打开 SQLite 失败: {error}")))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| std::io::Error::other(format!("设置 SQLite 超时失败: {error}")))?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
            .map_err(|error| {
                std::io::Error::other(format!("初始化 SQLite PRAGMA 失败: {error}"))
            })?;
        migrate_with_backup(
            &connection,
            &path,
            existed_before_open,
            create_migration_backup,
            migrate,
        )
        .map_err(|error| std::io::Error::other(format!("执行 SQLite 迁移失败: {error}")))?;
        crate::paths::secure_file(&path)?;
        crate::logging::info(
            "database.backend_open",
            serde_json::json!({ "path": path.display().to_string(), "driver": "rusqlite", "journalMode": "WAL" }),
        );
        Ok(Self {
            connection: Mutex::new(connection),
            app_session_id: uuid::Uuid::new_v4().to_string(),
        })
    }

    pub fn app_session_id(&self) -> &str {
        &self.app_session_id
    }
}

fn migrate(connection: &Connection) -> Result<(), String> {
    let current: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| format!("读取 SQLite schema 版本失败: {error}"))?;
    if current > SCHEMA_VERSION {
        return Err(format!(
            "数据库 schema v{current} 高于当前程序支持的 v{SCHEMA_VERSION}，请升级应用"
        ));
    }
    if current < 1 {
        run_migration(connection, MIGRATION_V1, 1, "v1")?;
    }
    if current < 2 {
        run_migration(connection, MIGRATION_V2, 2, "v2")?;
    }
    if current < 3 {
        run_migration(connection, MIGRATION_V3, 3, "v3")?;
    }
    if current < 4 {
        run_migration(connection, MIGRATION_V4, 4, "v4")?;
    }
    if current < 5 {
        run_migration(connection, MIGRATION_V5, 5, "v5")?;
    }
    if current < 6 {
        run_migration(connection, MIGRATION_V6, 6, "v6")?;
    }
    if current < 7 {
        run_migration(connection, MIGRATION_V7, 7, "v7")?;
    }
    if current < 8 {
        run_migration(connection, MIGRATION_V8, 8, "v8")?;
    }
    if current < 9 {
        run_migration(connection, MIGRATION_V9, 9, "v9")?;
    }
    if current < 10 {
        run_migration(connection, MIGRATION_V10, 10, "v10")?;
    }
    if current < 11 {
        run_migration(connection, MIGRATION_V11, 11, "v11")?;
    }
    if current < 12 {
        run_migration(connection, MIGRATION_V12, 12, "v12")?;
    }
    if current < 13 {
        run_migration(connection, MIGRATION_V13, 13, "v13")?;
    }
    if current < 14 {
        run_migration(connection, MIGRATION_V14, 14, "v14")?;
    }
    if current < 15 {
        run_migration(connection, MIGRATION_V15, 15, "v15")?;
    }
    if current < 16 {
        run_migration(connection, MIGRATION_V16, 16, "v16")?;
    }
    if current < 17 {
        run_migration(connection, MIGRATION_V17, 17, "v17")?;
    }
    if current < 18 {
        run_migration(connection, MIGRATION_V18, 18, "v18")?;
    }
    if current < 19 {
        run_migration(connection, MIGRATION_V19, 19, "v19")?;
    }
    if current < 20 {
        run_migration(connection, MIGRATION_V20, 20, "v20")?;
    }
    if current < 21 {
        run_migration(connection, MIGRATION_V21, 21, "v21")?;
    }
    if current < 22 {
        migrate_adaptation_chapter_revisions(connection)?;
    }
    if current < 23 {
        run_migration(connection, MIGRATION_V23, 23, "v23")?;
    }
    if current < 24 {
        run_migration(connection, MIGRATION_V24, 24, "v24")?;
    }
    if current < 25 {
        run_migration(connection, MIGRATION_V25, 25, "v25")?;
    }
    Ok(())
}

fn migrate_adaptation_chapter_revisions(connection: &Connection) -> Result<(), String> {
    // Rebuild the referenced table without deleting or retargeting child rows.
    // Foreign keys must be disabled outside the transaction; validate them
    // before committing and restore connection settings even after rollback.
    let foreign_keys: bool = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .map_err(|error| error.to_string())?;
    let legacy_alter: bool = connection
        .pragma_query_value(None, "legacy_alter_table", |row| row.get(0))
        .map_err(|error| error.to_string())?;
    connection.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA legacy_alter_table=ON;")
        .map_err(|error| error.to_string())?;
    let result = (|| -> Result<(), String> {
        let tx = connection.unchecked_transaction().map_err(|error| error.to_string())?;
        tx.execute_batch(MIGRATION_V22).map_err(|error| error.to_string())?;
        let invalid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)", [], |row| row.get(0),
        ).map_err(|error| error.to_string())?;
        if invalid {
            return Err("v22 foreign key validation failed".into());
        }
        tx.pragma_update(None, "user_version", 22).map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    })();
    let restore_legacy = connection.pragma_update(None, "legacy_alter_table", legacy_alter);
    let restore_foreign_keys = connection.pragma_update(None, "foreign_keys", foreign_keys);
    restore_legacy.and(restore_foreign_keys).map_err(|error| error.to_string())?;
    result.map_err(|error| format!("执行 v22 SQLite 迁移失败: {error}"))
}

fn schema_version(connection: &Connection) -> Result<i64, String> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| format!("读取 SQLite schema 版本失败: {error}"))
}

fn migrate_with_backup<B, M>(
    connection: &Connection,
    path: &Path,
    existed_before_open: bool,
    backup: B,
    migration: M,
) -> Result<(), String>
where
    B: FnOnce(&Connection, &Path, i64) -> Result<(), String>,
    M: FnOnce(&Connection) -> Result<(), String>,
{
    let current = schema_version(connection)?;
    if existed_before_open && current < SCHEMA_VERSION {
        backup(connection, path, current)?;
    }
    migration(connection)
}

fn backup_directory(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or("SQLite 数据库路径缺少父目录，无法创建迁移前备份")?;
    Ok(parent.join("backups"))
}

fn backup_prefix(path: &Path) -> Result<String, String> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or("SQLite 数据库文件名无效，无法创建迁移前备份")?;
    Ok(format!("{stem}-pre-migration-v"))
}

fn backup_files(directory: &Path, prefix: String) -> Result<Vec<PathBuf>, String> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)
        .map_err(|error| format!("读取 SQLite 备份目录失败: {error}"))?
    {
        let entry = entry.map_err(|error| format!("读取 SQLite 备份目录条目失败: {error}"))?;
        if !entry
            .file_type()
            .map_err(|error| format!("读取 SQLite 备份目录条目类型失败: {error}"))?
            .is_file()
        {
            continue;
        }
        let path = entry.path();
        let matches_prefix = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".sqlite"));
        if matches_prefix {
            files.push((backup_timestamp(&path, &prefix)?, path));
        }
    }
    files.sort_by(|(left_time, left_path), (right_time, right_path)| {
        right_time
            .cmp(left_time)
            .then_with(|| right_path.file_name().cmp(&left_path.file_name()))
    });
    Ok(files.into_iter().map(|(_, path)| path).collect())
}

fn backup_timestamp(path: &Path, prefix: &str) -> Result<u128, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("SQLite 迁移前备份文件名无效")?;
    let remainder = name
        .strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(".sqlite"))
        .ok_or("SQLite 迁移前备份文件名无效")?;
    let uuid_start = remainder
        .len()
        .checked_sub(uuid::fmt::Hyphenated::LENGTH)
        .ok_or("SQLite 迁移前备份文件名无效")?;
    let identifier = remainder
        .get(uuid_start..)
        .ok_or("SQLite 迁移前备份文件名无效")?;
    let before_identifier = remainder
        .get(..uuid_start)
        .and_then(|value| value.strip_suffix('-'))
        .ok_or("SQLite 迁移前备份文件名无效")?;
    let (version, timestamp) = before_identifier
        .rsplit_once('-')
        .ok_or("SQLite 迁移前备份文件名无效")?;
    if version.parse::<i64>().is_err() || uuid::Uuid::parse_str(identifier).is_err() {
        return Err("SQLite 迁移前备份文件名无效".to_string());
    }
    timestamp
        .parse::<u128>()
        .map_err(|_| "SQLite 迁移前备份文件名无效".to_string())
}

fn create_migration_backup(
    source: &Connection,
    source_path: &Path,
    source_version: i64,
) -> Result<(), String> {
    let directory = backup_directory(source_path)?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("创建 SQLite 迁移前备份目录失败: {error}"))?;
    crate::paths::secure_dir(&directory)
        .map_err(|error| format!("保护 SQLite 迁移前备份目录失败: {error}"))?;
    let prefix = backup_prefix(source_path)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("读取备份时间失败: {error}"))?
        .as_nanos();
    let filename = format!(
        "{prefix}{source_version}-{timestamp}-{}.sqlite",
        uuid::Uuid::new_v4()
    );
    let final_path = directory.join(&filename);
    let temporary_path = directory.join(format!(".migration-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut destination = Connection::open(&temporary_path)
            .map_err(|error| format!("创建 SQLite 迁移前备份临时文件失败: {error}"))?;
        let backup = Backup::new(source, &mut destination)
            .map_err(|error| format!("初始化 SQLite Online Backup 失败: {error}"))?;
        run_backup_to_completion(&backup)?;
        drop(backup);
        destination
            .close()
            .map_err(|(_, error)| format!("关闭 SQLite 迁移前备份临时文件失败: {error}"))?;
        crate::paths::secure_file(&temporary_path)
            .map_err(|error| format!("保护 SQLite 迁移前备份临时文件失败: {error}"))?;
        validate_backup(&temporary_path, source_version)?;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary_path)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("同步 SQLite 迁移前备份临时文件失败: {error}"))?;
        std::fs::rename(&temporary_path, &final_path)
            .map_err(|error| format!("提交 SQLite 迁移前备份失败: {error}"))?;
        crate::paths::secure_file(&final_path)
            .map_err(|error| format!("保护 SQLite 迁移前备份失败: {error}"))?;
        retain_recent_backups(&directory, &prefix)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result
}

fn run_backup_to_completion(backup: &Backup<'_, '_>) -> Result<(), String> {
    let deadline = Instant::now() + BACKUP_TOTAL_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err("SQLite 迁移前备份在 3 秒内未获得可用锁".to_string());
        }
        match backup
            .step(BACKUP_PAGE_COUNT)
            .map_err(|error| format!("执行 SQLite Online Backup 失败: {error}"))?
        {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            StepResult::Busy | StepResult::Locked => std::thread::sleep(BACKUP_RETRY_DELAY),
            _ => return Err("SQLite Online Backup 返回了不支持的步骤状态".to_string()),
        }
    }
}

fn validate_backup(path: &Path, source_version: i64) -> Result<(), String> {
    let backup = Connection::open(path)
        .map_err(|error| format!("打开 SQLite 迁移前备份进行验证失败: {error}"))?;
    let integrity: String = backup
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|error| format!("校验 SQLite 迁移前备份完整性失败: {error}"))?;
    if integrity != "ok" {
        return Err("SQLite 迁移前备份完整性校验未通过".to_string());
    }
    let version = schema_version(&backup)?;
    if version != source_version {
        return Err("SQLite 迁移前备份 schema 版本与源数据库不一致".to_string());
    }
    backup
        .close()
        .map_err(|(_, error)| format!("关闭已验证 SQLite 迁移前备份失败: {error}"))
}

fn retain_recent_backups(directory: &Path, prefix: &str) -> Result<(), String> {
    let backups = backup_files(directory, prefix.to_string())?;
    for path in backups.into_iter().skip(BACKUP_RETENTION_COUNT) {
        std::fs::remove_file(&path)
            .map_err(|error| format!("清理过期 SQLite 迁移前备份失败: {error}"))?;
    }
    Ok(())
}

fn run_migration(
    connection: &Connection,
    sql: &str,
    version: i64,
    label: &str,
) -> Result<(), String> {
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| format!("开始 SQLite {label} 迁移事务失败: {error}"))?;
    let result = connection
        .execute_batch(sql)
        .and_then(|_| connection.pragma_update(None, "user_version", version));
    match result {
        Ok(()) => connection
            .execute_batch("COMMIT")
            .map_err(|error| format!("提交 SQLite {label} 迁移失败: {error}")),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(format!("执行 {label} SQLite 迁移失败: {error}"))
        }
    }
}

pub fn with_connection<T>(
    state: &DbState,
    f: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String> {
    let connection = state
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁已损坏".to_string())?;
    f(&connection)
}

pub fn with_connection_mut<T>(
    state: &DbState,
    f: impl FnOnce(&mut Connection) -> Result<T, String>,
) -> Result<T, String> {
    let mut connection = state
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁已损坏".to_string())?;
    f(&mut connection)
}

#[tauri::command]
pub fn db_execute(
    state: tauri::State<'_, DbState>,
    query: String,
    bind_values: Vec<Value>,
) -> Result<DbQueryResult, String> {
    execute_inner(&state, &query, &bind_values)
}

fn execute_inner(
    state: &DbState,
    query: &str,
    bind_values: &[Value],
) -> Result<DbQueryResult, String> {
    validate_bind_values(bind_values)?;
    validate_query(query, false)?;
    let values = bind_values
        .iter()
        .map(json_to_sql)
        .collect::<Result<Vec<_>, _>>()?;
    let connection = state
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁已损坏".to_string())?;
    let rows_affected = connection
        .execute(query, params_from_iter(values.iter()))
        .map_err(|error| format!("SQLite 执行失败: {error}"))?;
    Ok(DbQueryResult {
        rows_affected,
        last_insert_id: Some(connection.last_insert_rowid()),
    })
}

#[tauri::command]
pub fn db_select(
    state: tauri::State<'_, DbState>,
    query: String,
    bind_values: Vec<Value>,
) -> Result<Value, String> {
    select_inner(&state, &query, &bind_values)
}

fn select_inner(state: &DbState, query: &str, bind_values: &[Value]) -> Result<Value, String> {
    validate_bind_values(bind_values)?;
    validate_query(query, true)?;
    let values = bind_values
        .iter()
        .map(json_to_sql)
        .collect::<Result<Vec<_>, _>>()?;
    let connection = state
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁已损坏".to_string())?;
    let mut statement = connection
        .prepare(query)
        .map_err(|error| format!("SQLite 查询准备失败: {error}"))?;
    let column_names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let rows = statement
        .query_map(params_from_iter(values.iter()), |row| {
            let mut object = Map::new();
            for (index, name) in column_names.iter().enumerate() {
                object.insert(name.clone(), sql_to_json(row.get_ref(index)?));
            }
            Ok(Value::Object(object))
        })
        .map_err(|error| format!("SQLite 查询失败: {error}"))?;
    let mut values = Vec::new();
    for row in rows {
        if values.len() >= MAX_RESULT_ROWS {
            return Err(format!("SQLite 查询结果超过 {MAX_RESULT_ROWS} 行限制"));
        }
        values.push(row.map_err(|error| format!("读取 SQLite 查询结果失败: {error}"))?);
    }
    Ok(Value::Array(values))
}

fn validate_query(query: &str, select: bool) -> Result<(), String> {
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(format!("SQLite 语句超过 {MAX_QUERY_CHARS} 个字符"));
    }
    let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty()
        || normalized.contains(';')
        || normalized.contains("--")
        || normalized.contains("/*")
    {
        return Err("SQLite 语句为空或包含不允许的多语句/注释".into());
    }
    let lower = normalized.to_ascii_lowercase();
    for forbidden in [
        " join ",
        " union ",
        " pragma ",
        " attach ",
        " detach ",
        " vacuum ",
        " reindex ",
    ] {
        if format!(" {lower} ").contains(forbidden) {
            return Err(format!(
                "SQLite 语句包含不允许的关键字: {}",
                forbidden.trim()
            ));
        }
    }
    let operation_ok = if select {
        lower.starts_with("select ")
    } else {
        lower.starts_with("insert ") || lower.starts_with("update ") || lower.starts_with("delete ")
    };
    if !operation_ok {
        return Err("SQLite 操作类型不允许".into());
    }
    if (select && lower["select ".len()..].contains("select "))
        || (!select && lower.contains("select "))
    {
        return Err("SQLite 不允许子查询".into());
    }
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    let table = if select || lower.starts_with("delete ") {
        tokens
            .iter()
            .position(|token| *token == "from")
            .and_then(|index| tokens.get(index + 1))
    } else if lower.starts_with("update ") {
        tokens.get(1)
    } else {
        tokens
            .iter()
            .position(|token| *token == "into")
            .and_then(|index| tokens.get(index + 1))
    }
    .map(|table| {
        table.trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '_')
    })
    .ok_or("无法识别 SQLite 表名")?;
    if !ALLOWED_TABLES.contains(&table) {
        return Err(format!("SQLite 表不允许访问: {table}"));
    }
    Ok(())
}

fn validate_bind_values(values: &[Value]) -> Result<(), String> {
    if values.len() > MAX_BIND_VALUES {
        return Err(format!("SQLite 参数数量超过 {MAX_BIND_VALUES}"));
    }
    for value in values {
        let size = match value {
            Value::String(value) => value.len(),
            Value::Array(_) | Value::Object(_) => serde_json::to_vec(value)
                .map_err(|error| format!("序列化 SQLite 参数失败: {error}"))?
                .len(),
            _ => 0,
        };
        if size > MAX_BIND_STRING_BYTES {
            return Err("SQLite 单个参数超过 32 MiB 大小限制".into());
        }
    }
    Ok(())
}

fn json_to_sql(value: &Value) -> Result<SqlValue, String> {
    match value {
        Value::Null => Ok(SqlValue::Null),
        Value::Bool(value) => Ok(SqlValue::Integer(i64::from(*value))),
        Value::Number(value) => value
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| {
                value
                    .as_u64()
                    .and_then(|value| i64::try_from(value).ok())
                    .map(SqlValue::Integer)
            })
            .or_else(|| value.as_f64().map(SqlValue::Real))
            .ok_or("SQLite 数字参数超出范围".into()),
        Value::String(value) => Ok(SqlValue::Text(value.clone())),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value)
            .map(SqlValue::Text)
            .map_err(|error| format!("序列化 SQLite JSON 参数失败: {error}")),
    }
}

fn sql_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::Number(Number::from(value)),
        ValueRef::Real(value) => Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(value) => Value::String(String::from_utf8_lossy(value).into_owned()),
        ValueRef::Blob(value) => {
            Value::String(base64::engine::general_purpose::STANDARD.encode(value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, OptionalExtension};

    #[test]
    fn restricts_sql_to_expected_operations_and_tables() {
        assert!(validate_query("SELECT value FROM settings WHERE key = ?", true).is_ok());
        assert!(validate_query("INSERT OR REPLACE INTO assets (id) VALUES (?)", false).is_ok());
        assert!(validate_query("DROP TABLE settings", false).is_err());
        assert!(validate_query("SELECT * FROM sqlite_master", true).is_err());
        assert!(validate_query("SELECT * FROM settings; DELETE FROM settings", true).is_err());
        assert!(validate_query("SELECT * FROM settings UNION SELECT * FROM tasks", true).is_err());
        assert!(validate_query(
            "SELECT * FROM settings WHERE value IN (SELECT value FROM settings)",
            true
        )
        .is_err());
        assert!(validate_query("x", true).is_err());
    }

    #[test]
    fn opens_migrates_and_round_trips_settings() {
        let dir = std::env::temp_dir().join(format!("image-client-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = DbState::open(dir.join("test.db")).unwrap();
        let version: i64 = state
            .connection
            .lock()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        execute_inner(
            &state,
            "INSERT INTO settings (key, value) VALUES (?, ?)",
            &[
                Value::String("strict-test".into()),
                Value::String("ok".into()),
            ],
        )
        .unwrap();
        let rows = select_inner(
            &state,
            "SELECT value FROM settings WHERE key = ?",
            &[Value::String("strict-test".into())],
        )
        .unwrap();
        assert_eq!(rows[0]["value"], "ok");
        let comic_count: i64 = state
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'comic_projects'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(comic_count, 1);
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fresh_database_installs_utf8_planning_source_bound_trigger() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();

        migrate(&connection).unwrap();

        assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
        let trigger_sql: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='comic_planning_chapter_source_scope'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            trigger_sql.contains("length(CAST(revision.content AS BLOB))"),
            "fresh databases must enforce source selections in UTF-8 bytes"
        );
    }

    #[test]
    fn v14_migrates_an_existing_v13_database_without_rebuilding_prior_tables() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (sql, version, name) in [
            (MIGRATION_V1, 1, "v1"),
            (MIGRATION_V2, 2, "v2"),
            (MIGRATION_V3, 3, "v3"),
            (MIGRATION_V4, 4, "v4"),
            (MIGRATION_V5, 5, "v5"),
            (MIGRATION_V6, 6, "v6"),
            (MIGRATION_V7, 7, "v7"),
            (MIGRATION_V8, 8, "v8"),
            (MIGRATION_V9, 9, "v9"),
            (MIGRATION_V10, 10, "v10"),
            (MIGRATION_V11, 11, "v11"),
            (MIGRATION_V12, 12, "v12"),
            (MIGRATION_V13, 13, "v13"),
        ] {
            run_migration(&connection, sql, version, name).unwrap();
        }
        run_migration(&connection, MIGRATION_V14, 14, "v14").unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 14);
        for table in [
            "novel_chapter_state_publications",
            "novel_chapter_state_publication_sources",
        ] {
            let exists: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "v14 must add {table}");
        }
        let settings: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='settings'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(settings, 1, "v14 must not rebuild or lose existing tables");
    }

    #[test]
    fn v15_adds_durable_production_jobs_and_events_on_an_existing_v14_database() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (sql, version, name) in [
            (MIGRATION_V1, 1, "v1"),
            (MIGRATION_V2, 2, "v2"),
            (MIGRATION_V3, 3, "v3"),
            (MIGRATION_V4, 4, "v4"),
            (MIGRATION_V5, 5, "v5"),
            (MIGRATION_V6, 6, "v6"),
            (MIGRATION_V7, 7, "v7"),
            (MIGRATION_V8, 8, "v8"),
            (MIGRATION_V9, 9, "v9"),
            (MIGRATION_V10, 10, "v10"),
            (MIGRATION_V11, 11, "v11"),
            (MIGRATION_V12, 12, "v12"),
            (MIGRATION_V13, 13, "v13"),
            (MIGRATION_V14, 14, "v14"),
        ] {
            run_migration(&connection, sql, version, name).unwrap();
        }
        run_migration(&connection, MIGRATION_V15, 15, "v15").unwrap();
        assert_eq!(schema_version(&connection).unwrap(), 15);
        for table in ["novel_production_jobs", "novel_production_job_events"] {
            let exists: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "v15 must add {table}");
        }
        migrate(&connection).unwrap();
    }

    #[test]
    fn v9_migrates_empty_v8_and_exposes_apply_lineage_constraints() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (sql, version, name) in [
            (MIGRATION_V1, 1, "v1"),
            (MIGRATION_V2, 2, "v2"),
            (MIGRATION_V3, 3, "v3"),
            (MIGRATION_V4, 4, "v4"),
            (MIGRATION_V5, 5, "v5"),
            (MIGRATION_V6, 6, "v6"),
            (MIGRATION_V7, 7, "v7"),
            (MIGRATION_V8, 8, "v8"),
        ] {
            run_migration(&connection, sql, version, name).unwrap();
        }
        run_migration(&connection, MIGRATION_V9, 9, "v9").unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 9);
        for table in [
            "analysis_apply_operation_sources",
            "analysis_apply_receipts",
            "novel_canon_version_sources",
            "novel_state_version_sources",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    params![table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1);
        }
        let expires: Option<i64> = connection
            .query_row(
                "SELECT approval_expires_at FROM analysis_apply_operations LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(expires, None);
        assert!(connection.execute("INSERT INTO analysis_apply_receipts(analysis_apply_operation_id,idempotency_key,operation_type,result_novel_canon_version_id,result_novel_state_version_id,created_at) VALUES('missing','k','publish_canon','missing',NULL,1)",[]).is_err());
    }

    #[test]
    fn v10_backfills_optimization_attempts_and_enforces_same_run_lineage() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (sql, version, name) in [
            (MIGRATION_V1, 1, "v1"),
            (MIGRATION_V2, 2, "v2"),
            (MIGRATION_V3, 3, "v3"),
            (MIGRATION_V4, 4, "v4"),
            (MIGRATION_V5, 5, "v5"),
            (MIGRATION_V6, 6, "v6"),
            (MIGRATION_V7, 7, "v7"),
            (MIGRATION_V8, 8, "v8"),
            (MIGRATION_V9, 9, "v9"),
        ] {
            run_migration(&connection, sql, version, name).unwrap();
        }
        connection.execute_batch(
            "INSERT INTO novel_works (id,project_id,title,description,status,created_at,updated_at)
                 VALUES ('work-a','project-a','A','', 'active',1,1), ('work-b','project-b','B','', 'active',1,1);
             INSERT INTO analysis_artifacts (id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,candidate_head_revision_id,adopted_head_revision_id,status,optimistic_version,created_at,updated_at)
                 VALUES ('artifact-a',NULL,'legacy-adaptation','chapter_summary','work-a',NULL,NULL,NULL,NULL,NULL,'active',0,1,1),
                        ('artifact-b',NULL,'legacy-adaptation','chapter_summary','work-b',NULL,NULL,NULL,NULL,NULL,'active',0,1,1);
             INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at)
                 VALUES ('revision-a','artifact-a',1,NULL,'{}','','seed','{}','{}','candidate',1),
                        ('revision-b','artifact-b',1,NULL,'{}','','seed','{}','{}','candidate',1);
             INSERT INTO novel_artifact_optimization_runs (id,novel_work_id,analysis_artifact_id,parent_artifact_revision_id,provider_id,model_id,instruction,frozen_input_fingerprint,idempotency_key,status,attempt_no,lease_owner,lease_expires_at,result_artifact_revision_id,safe_error_code,safe_user_message,created_at,updated_at,finished_at)
                 VALUES ('run-queued','work-a','artifact-a','revision-a','p','m','i','f','key-queued','queued',9,NULL,NULL,NULL,NULL,NULL,10,11,NULL),
                        ('run-running','work-a','artifact-a','revision-a','p','m','i','f','key-running','running',3,'owner',1000,NULL,NULL,NULL,20,21,NULL),
                        ('run-ready','work-a','artifact-a','revision-a','p','m','i','f','key-ready','ready',1,NULL,NULL,NULL,NULL,NULL,30,31,NULL),
                        ('run-error','work-a','artifact-a','revision-a','p','m','i','f','key-error','error',1,NULL,NULL,NULL,'PROVIDER','safe',40,41,42),
                        ('run-cancel','work-a','artifact-a','revision-a','p','m','i','f','key-cancel','cancel_requested',1,NULL,NULL,NULL,NULL,NULL,50,51,NULL),
                        ('run-broken-running','work-b','artifact-b','revision-b','p','m','i','f','key-broken-running','running',1,NULL,NULL,NULL,NULL,NULL,55,56,NULL),
                        ('run-unknown','work-b','artifact-b','revision-b','p','m','i','f','key-unknown','unknown_manual',1,NULL,NULL,NULL,NULL,NULL,60,61,NULL);",
        ).unwrap();

        run_migration(&connection, MIGRATION_V10, 10, "v10").unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 10);
        let run_counters = connection
            .prepare("SELECT attempt_no FROM novel_artifact_optimization_runs ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(run_counters, vec![1; 7]);
        let attempts = connection.prepare("SELECT novel_artifact_optimization_run_id,attempt_no,parent_attempt_id,status,safe_error_code,finished_at FROM novel_artifact_optimization_attempts ORDER BY novel_artifact_optimization_run_id").unwrap()
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, String>(3)?, row.get::<_, Option<String>>(4)?, row.get::<_, Option<i64>>(5)?)))
            .unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(
            attempts,
            vec![
                (
                    "run-broken-running".into(),
                    1,
                    None,
                    "abandoned".into(),
                    Some("LEASE_UNAVAILABLE".into()),
                    Some(56)
                ),
                (
                    "run-cancel".into(),
                    1,
                    None,
                    "cancelled".into(),
                    None,
                    Some(51)
                ),
                (
                    "run-error".into(),
                    1,
                    None,
                    "error".into(),
                    Some("PROVIDER".into()),
                    Some(42)
                ),
                ("run-queued".into(), 1, None, "queued".into(), None, None),
                (
                    "run-ready".into(),
                    1,
                    None,
                    "succeeded".into(),
                    None,
                    Some(31)
                ),
                ("run-running".into(), 1, None, "running".into(), None, None),
                (
                    "run-unknown".into(),
                    1,
                    None,
                    "abandoned".into(),
                    Some("UNKNOWN_MANUAL".into()),
                    Some(61)
                ),
            ]
        );
        connection.execute(
            "INSERT INTO novel_artifact_optimization_attempts (id,novel_artifact_optimization_run_id,attempt_no,parent_attempt_id,status,created_at) VALUES ('run-a-attempt-2','run-ready',2,'noptattempt_run-ready','queued',70)",
            [],
        ).unwrap();
        assert!(connection.execute(
            "INSERT INTO novel_artifact_optimization_attempts (id,novel_artifact_optimization_run_id,attempt_no,parent_attempt_id,status,created_at) VALUES ('cross-run-parent','run-unknown',2,'noptattempt_run-ready','queued',71)",
            [],
        ).is_err());
        assert!(connection.execute(
            "INSERT INTO novel_artifact_optimization_attempts (id,novel_artifact_optimization_run_id,attempt_no,parent_attempt_id,status,created_at) VALUES ('skipped-number','run-ready',4,'run-a-attempt-2','queued',72)",
            [],
        ).is_err());
    }

    #[test]
    fn v11_comic_adaptation_apply_is_scoped_and_receipt_mapped() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (sql, version, name) in [
            (MIGRATION_V1, 1, "v1"),
            (MIGRATION_V2, 2, "v2"),
            (MIGRATION_V3, 3, "v3"),
            (MIGRATION_V4, 4, "v4"),
            (MIGRATION_V5, 5, "v5"),
            (MIGRATION_V6, 6, "v6"),
            (MIGRATION_V7, 7, "v7"),
            (MIGRATION_V8, 8, "v8"),
            (MIGRATION_V9, 9, "v9"),
            (MIGRATION_V10, 10, "v10"),
        ] {
            run_migration(&connection, sql, version, name).unwrap();
        }
        connection.execute_batch(
            "INSERT INTO novel_works (id,project_id,title,description,status,created_at,updated_at) VALUES
                 ('work-a','project-a','A','','active',1,1), ('work-b','project-b','B','','active',1,1);
             INSERT INTO novel_chapters (id,novel_work_id,volume_id,sequence_no,chapter_no,title,current_revision_id,created_at,updated_at) VALUES
                 ('chapter-a','work-a',NULL,1,1,'A',NULL,1,1), ('chapter-b','work-b',NULL,1,1,'B',NULL,1,1);
             INSERT INTO novel_chapter_revisions (id,novel_chapter_id,version,content,content_hash,asset_id,requested_parent_context_revision_id,source_kind,created_at) VALUES
                 ('source-a','chapter-a',1,'abcdef','hash-a',NULL,NULL,'paste',1), ('source-b','chapter-b',1,'abcdef','hash-b',NULL,NULL,'paste',1);
             UPDATE novel_chapters SET current_revision_id='source-a' WHERE id='chapter-a';
             UPDATE novel_chapters SET current_revision_id='source-b' WHERE id='chapter-b';
             INSERT INTO novel_canon_versions (id,novel_work_id,version,parent_version_id,body_json,rendered_markdown,status,created_at) VALUES
                 ('canon-a','work-a',0,NULL,'{}','','published',1), ('canon-b','work-b',0,NULL,'{}','','published',1);
             INSERT INTO novel_analysis_lineages (id,novel_work_id,name,base_published_canon_version_id,base_published_novel_state_version_id,current_context_revision_id,continuous_through_sequence_no,status,optimistic_version,created_at,updated_at) VALUES
                 ('lineage-a','work-a','main','canon-a',NULL,NULL,0,'active',0,1,1);
             INSERT INTO source_analysis_runs (id,novel_chapter_revision_id,novel_analysis_lineage_id,base_working_context_revision_id,base_canon_version_id,base_novel_state_version_id,frozen_input_fingerprint,provider_id,model_id,status,review_status,current_stage,prompt_version,schema_version,idempotency_key,progress_json,safe_error_code,safe_user_message,created_at,updated_at,completed_at) VALUES
                 ('source-run-a','source-a','lineage-a',NULL,'canon-a',NULL,'fixture-source-fingerprint','fixture','fixture','ready_for_review','pending','done','v1','novel-analysis.v1','fixture-source-run-a','{}',NULL,NULL,1,1,1);
             INSERT INTO comic_adaptations (id,project_id,novel_work_id,title,status,config_json,current_continuity_version_id,optimistic_version,created_at,updated_at) VALUES
                 ('adapt-a','project-a','work-a','A','active','{}',NULL,0,1,1), ('adapt-b','project-b','work-b','B','active','{}',NULL,0,1,1);
             INSERT INTO continuity_state_versions (id,comic_adaptation_id,version,parent_version_id,through_comic_chapter_id,body_json,created_at) VALUES
                 ('continuity-a-0','adapt-a',0,NULL,NULL,'{}',1), ('continuity-b-0','adapt-b',0,NULL,NULL,'{}',1);
             UPDATE comic_adaptations SET current_continuity_version_id='continuity-a-0' WHERE id='adapt-a';
             UPDATE comic_adaptations SET current_continuity_version_id='continuity-b-0' WHERE id='adapt-b';
             INSERT INTO comic_adaptation_chapters (id,comic_adaptation_id,novel_chapter_revision_id,sequence_no,created_at) VALUES
                 ('adapt-chapter-a','adapt-a','source-a',1,1);
             INSERT INTO analysis_artifacts (id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,candidate_head_revision_id,adopted_head_revision_id,status,optimistic_version,created_at,updated_at) VALUES
                 ('proposal-a','source-run-a',NULL,'adaptation_proposal','work-a','source-a','adapt-a',NULL,NULL,NULL,'active',0,1,1),
                 ('chapter-plan-a','source-run-a',NULL,'comic_chapter_plan','work-a','source-a','adapt-a','adapt-chapter-a',NULL,NULL,'active',0,1,1),
                 ('scene-plan-a','source-run-a',NULL,'scene_plan','work-a','source-a','adapt-a','adapt-chapter-a',NULL,NULL,'active',0,1,1),
                 ('page-plan-a','source-run-a',NULL,'page_panel_plan','work-a','source-a','adapt-a','adapt-chapter-a',NULL,NULL,'active',0,1,1),
                 ('proposal-b',NULL,'run-b','adaptation_proposal','work-b',NULL,'adapt-b',NULL,NULL,NULL,'active',0,1,1),
                 ('page-plan-b',NULL,'run-b','page_panel_plan','work-b',NULL,'adapt-b',NULL,NULL,NULL,'active',0,1,1);
             INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES
                 ('proposal-rev-a','proposal-a',1,NULL,'{}','','seed','{}','{}','adopted',1),
                 ('chapter-plan-rev-a','chapter-plan-a',1,NULL,'{}','','seed','{}','{}','adopted',1),
                 ('scene-plan-rev-a','scene-plan-a',1,NULL,'{}','','seed','{}','{}','adopted',1),
                 ('page-plan-rev-a','page-plan-a',1,NULL,'{}','','seed','{}','{}','adopted',1),
                 ('proposal-rev-b','proposal-b',1,NULL,'{}','','seed','{}','{}','adopted',1),
                 ('page-plan-rev-b','page-plan-b',1,NULL,'{}','','seed','{}','{}','adopted',1);
             UPDATE analysis_artifacts SET adopted_head_revision_id=CASE id
                 WHEN 'proposal-a' THEN 'proposal-rev-a' WHEN 'chapter-plan-a' THEN 'chapter-plan-rev-a'
                 WHEN 'scene-plan-a' THEN 'scene-plan-rev-a' WHEN 'page-plan-a' THEN 'page-plan-rev-a'
                 WHEN 'proposal-b' THEN 'proposal-rev-b' WHEN 'page-plan-b' THEN 'page-plan-rev-b' END;
             INSERT INTO analysis_apply_operations (id,operation_type,novel_work_id,comic_adaptation_id,base_target_version_id,idempotency_key,preview_fingerprint,approval_token_hash,status,safe_error_code,safe_user_message,created_at,updated_at,completed_at,approval_expires_at) VALUES
                 ('accept-a','accept_adaptation',NULL,'adapt-a',NULL,'accept-key-a','preview','token','approved',NULL,NULL,1,1,NULL,100),
                 ('apply-a','apply_comic_plan',NULL,'adapt-a',NULL,'apply-key-a','preview','token','approved',NULL,NULL,1,1,NULL,100),
                 ('apply-b','apply_comic_plan',NULL,'adapt-b',NULL,'apply-key-b','preview','token','approved',NULL,NULL,1,1,NULL,100),
                 ('accept-legacy-null','accept_adaptation',NULL,'adapt-a',NULL,'accept-legacy-key','preview','token','approved',NULL,NULL,1,1,NULL,100);",
        ).unwrap();

        run_migration(&connection, MIGRATION_V11, 11, "v11").unwrap();
        assert_eq!(schema_version(&connection).unwrap(), 11);
        connection.execute(
            "UPDATE analysis_apply_operations SET expected_adaptation_version=0 WHERE id IN ('accept-a','apply-a','apply-b')",
            [],
        ).unwrap();
        assert!(connection.execute(
            "UPDATE analysis_apply_operations SET expected_adaptation_version=NULL WHERE id='apply-a'", [],
        ).is_err(), "adaptation operations must retain a CAS baseline");
        assert!(connection.execute(
            "UPDATE analysis_apply_operations SET expected_adaptation_version=-1 WHERE id='apply-a'", [],
        ).is_err(), "adaptation CAS baseline cannot be negative");
        assert!(connection.execute(
            "INSERT INTO analysis_apply_operations (id,operation_type,novel_work_id,comic_adaptation_id,base_target_version_id,idempotency_key,preview_fingerprint,approval_token_hash,status,created_at,updated_at,approval_expires_at) VALUES ('bad-adaptation','accept_adaptation',NULL,'adapt-a',NULL,'bad-adaptation-key','preview','token','approved',1,1,100)", [],
        ).is_err(), "new adaptation operation requires expected version");
        assert!(connection.execute(
            "INSERT INTO analysis_apply_operations (id,operation_type,novel_work_id,comic_adaptation_id,base_target_version_id,idempotency_key,preview_fingerprint,approval_token_hash,status,created_at,updated_at,approval_expires_at,expected_adaptation_version) VALUES ('bad-publish','publish_canon','work-a',NULL,NULL,'bad-publish-key','preview','token','approved',1,1,100,0)", [],
        ).is_err(), "NovelWork operation cannot carry adaptation CAS version");
        assert!(connection.execute(
            "INSERT INTO analysis_apply_receipts (analysis_apply_operation_id,idempotency_key,operation_type,result_novel_canon_version_id,result_novel_state_version_id,created_at) VALUES ('accept-legacy-null','accept-legacy-key','accept_adaptation',NULL,NULL,5)", [],
        ).is_err(), "legacy adaptation operation cannot bypass the CAS baseline at receipt");
        for table in [
            "comic_adaptation_plan_heads",
            "comic_planning_chapters",
            "comic_scene_context_snapshot_approvals",
            "comic_production_chapters",
            "analysis_apply_receipt_entity_maps",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing {table}");
        }
        assert!(connection.execute(
            "INSERT INTO comic_adaptation_plan_heads (id,comic_adaptation_id,adaptation_proposal_revision_id,comic_chapter_plan_revision_id,scene_plan_revision_id,accept_apply_operation_id,status,created_at,updated_at) VALUES ('cross-head','adapt-a','proposal-rev-b','chapter-plan-rev-a','scene-plan-rev-a','accept-a','active',2,2)", [],
        ).is_err(), "cross-adaptation proposal must fail closed");
        connection.execute_batch(
            "INSERT INTO comic_adaptation_plan_heads (id,comic_adaptation_id,adaptation_proposal_revision_id,comic_chapter_plan_revision_id,scene_plan_revision_id,accept_apply_operation_id,status,created_at,updated_at) VALUES
                 ('head-a','adapt-a','proposal-rev-a','chapter-plan-rev-a','scene-plan-rev-a','accept-a','active',2,2);
             INSERT INTO comic_planning_chapters (id,comic_adaptation_plan_head_id,comic_adaptation_id,planning_chapter_stable_key,status,created_at,updated_at) VALUES
                 ('planning-a','head-a','adapt-a','chapter-001','planning',2,2);",
        ).unwrap();
        assert!(
            connection
                .execute(
                    "UPDATE comic_adaptation_plan_heads SET status='active' WHERE id='head-a'",
                    [],
                )
                .is_err(),
            "completed planning head cannot be replayed or rewritten as active"
        );
        assert!(connection.execute(
            "INSERT INTO comic_planning_chapter_sources (comic_planning_chapter_id,novel_chapter_revision_id,source_order,source_start,source_end) VALUES ('planning-a','source-b',0,0,2)", [],
        ).is_err(), "cross-NovelWork source must fail closed");
        connection.execute(
            "INSERT INTO comic_planning_chapter_sources (comic_planning_chapter_id,novel_chapter_revision_id,source_order,source_start,source_end) VALUES ('planning-a','source-a',0,0,2)", [],
        ).unwrap();
        connection.execute(
            "INSERT INTO comic_planning_chapter_sources (comic_planning_chapter_id,novel_chapter_revision_id,source_order,source_start,source_end) VALUES ('planning-a','source-a',1,3,5)", [],
        ).unwrap();
        for (sql, version, name) in [
            (MIGRATION_V12, 12, "v12"),
            (MIGRATION_V13, 13, "v13"),
            (MIGRATION_V14, 14, "v14"),
            (MIGRATION_V15, 15, "v15"),
            (MIGRATION_V16, 16, "v16"),
            (MIGRATION_V17, 17, "v17"),
            (MIGRATION_V18, 18, "v18"),
            (MIGRATION_V19, 19, "v19"),
            (MIGRATION_V20, 20, "v20"),
            (MIGRATION_V21, 21, "v21"),
        ] {
            run_migration(&connection, sql, version, name).unwrap();
        }
        assert_eq!(schema_version(&connection).unwrap(), 21);
        connection.execute(
            "INSERT INTO comic_scene_context_snapshots (id,novel_work_id,comic_adaptation_id,scene_plan_revision_id,planning_scene_stable_key,working_context_revision_id,novel_canon_version_id,novel_state_version_id,continuity_state_version_id,adaptation_plan_revision_id,resolver_contract_version,prompt_compiler_contract_version,context_fingerprint,resolved_context_json,resolved_context_hash,status,created_at) VALUES ('snapshot-a','work-a','adapt-a','scene-plan-rev-a','scene-001',NULL,'canon-a',NULL,'continuity-a-0','proposal-rev-a','v1','v1','fingerprint-a','{}','hash','provisional',3)", [],
        ).unwrap();
        assert!(connection.execute(
            "UPDATE comic_scene_context_snapshots SET novel_canon_version_id='canon-b' WHERE id='snapshot-a'", [],
        ).is_err(), "snapshot facts cannot cross NovelWork scope on update");
        assert!(connection.execute("UPDATE comic_scene_context_snapshots SET status='approved' WHERE id='snapshot-a'", []).is_err(), "approval row is mandatory");
        assert!(connection.execute(
            "INSERT INTO comic_scene_context_snapshot_approvals (scene_context_snapshot_id,approved_context_fingerprint,approved_by,approved_at) VALUES ('snapshot-a','wrong','reviewer',4)", [],
        ).is_err(), "approval fingerprint must match");
        connection.execute_batch(
            "INSERT INTO comic_scene_context_snapshot_approvals (scene_context_snapshot_id,approved_context_fingerprint,approved_by,approved_at) VALUES ('snapshot-a','fingerprint-a','reviewer',4);
             UPDATE comic_scene_context_snapshots SET status='approved' WHERE id='snapshot-a';
             INSERT INTO analysis_apply_operation_sources (analysis_apply_operation_id,analysis_artifact_revision_id,source_role,source_order) VALUES ('apply-a','page-plan-rev-a','scene_input',0);",
        ).unwrap();
        assert!(connection.execute(
            "INSERT INTO analysis_apply_operation_sources (analysis_apply_operation_id,analysis_artifact_revision_id,source_role,source_order) VALUES ('apply-a','page-plan-rev-b','scene_input',1)", [],
        ).is_err(), "cross-adaptation apply source must fail closed");
        connection.execute(
            "INSERT INTO analysis_apply_scene_context_selections (apply_operation_id,planning_scene_stable_key,scene_context_snapshot_id,source_order) VALUES ('apply-a','scene-001','snapshot-a',0)", [],
        ).unwrap();
        assert!(connection.execute(
            "INSERT INTO analysis_apply_scene_context_selections (apply_operation_id,planning_scene_stable_key,scene_context_snapshot_id,source_order) VALUES ('apply-b','scene-001','snapshot-a',0)", [],
        ).is_err(), "cross-adaptation snapshot selection must fail closed");
        connection.execute_batch(
            "UPDATE comic_scene_context_snapshots SET status='frozen' WHERE id='snapshot-a';
             INSERT INTO analysis_apply_receipts (analysis_apply_operation_id,idempotency_key,operation_type,result_novel_canon_version_id,result_novel_state_version_id,created_at) VALUES
                 ('accept-a','accept-key-a','accept_adaptation',NULL,NULL,5), ('apply-a','apply-key-a','apply_comic_plan',NULL,NULL,5);
             INSERT INTO comic_production_chapters (id,comic_adaptation_id,comic_planning_chapter_id,page_panel_plan_revision_id,apply_operation_id,status,created_at) VALUES
                 ('production-chapter-a','adapt-a','planning-a','page-plan-rev-a','apply-a','active',5);
             INSERT INTO comic_production_scenes (id,comic_production_chapter_id,planning_scene_stable_key,scene_context_snapshot_id,scene_no,created_at) VALUES
                 ('production-scene-a','production-chapter-a','scene-001','snapshot-a',1,5);
             INSERT INTO comic_production_pages (id,comic_production_chapter_id,planning_page_stable_key,page_no,created_at) VALUES ('production-page-a','production-chapter-a','page-001',1,5);
             INSERT INTO comic_production_panels (id,comic_production_page_id,comic_production_scene_id,planning_panel_stable_key,panel_no,spec_json,created_at) VALUES ('production-panel-a','production-page-a','production-scene-a','panel-001',1,'{}',5);
             INSERT INTO analysis_apply_receipt_entity_maps (analysis_apply_operation_id,entity_kind,stable_key,entity_id,created_at) VALUES
                 ('accept-a','planning_chapter','chapter-001','planning-a',5),
                 ('apply-a','production_chapter','chapter-001','production-chapter-a',5),
                 ('apply-a','production_scene','scene-001','production-scene-a',5),
                 ('apply-a','production_page','page-001','production-page-a',5),
                 ('apply-a','production_panel','panel-001','production-panel-a',5);",
        ).unwrap();
        let mapped: i64 = connection.query_row(
            "SELECT COUNT(*) FROM analysis_apply_receipt_entity_maps WHERE analysis_apply_operation_id='apply-a'", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(mapped, 4);
        assert!(connection.execute(
            "INSERT INTO analysis_apply_receipt_entity_maps (analysis_apply_operation_id,entity_kind,stable_key,entity_id,created_at) VALUES ('apply-a','production_page','wrong-page-key','production-page-a',6)", [],
        ).is_err(), "page receipt map must retain its stable key");
        assert!(connection.execute(
            "INSERT INTO analysis_apply_receipt_entity_maps (analysis_apply_operation_id,entity_kind,stable_key,entity_id,created_at) VALUES ('apply-a','production_panel','wrong-panel-key','production-panel-a',6)", [],
        ).is_err(), "panel receipt map must retain its stable key");
        assert!(connection.execute(
            "INSERT INTO comic_production_chapters (id,comic_adaptation_id,comic_planning_chapter_id,page_panel_plan_revision_id,apply_operation_id,status,created_at) VALUES ('bad-production','adapt-a','planning-a','page-plan-rev-b','apply-a','active',6)", [],
        ).is_err(), "cross-adaptation page plan must fail closed");
        assert!(connection.execute(
            "UPDATE comic_scene_context_snapshots SET resolved_context_json='{\"changed\":true}' WHERE id='snapshot-a'", [],
        ).is_err(), "frozen snapshots are immutable");

        // Keep the original v11 acceptance fixture intact through its lineage
        // assertions.  The UTF-8 migration is verified only after that
        // fixture has been fully upgraded to v21.
        let unicode_source = "你🙂好";
        let utf8_end = unicode_source.len() as i64;
        assert!(utf8_end > unicode_source.chars().count() as i64);
        connection
            .execute(
                "UPDATE novel_chapter_revisions SET content=? WHERE id='source-a'",
                params![unicode_source],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO comic_planning_chapter_sources (comic_planning_chapter_id,novel_chapter_revision_id,source_order,source_start,source_end) VALUES ('planning-a','source-a',2,0,?)",
                params![utf8_end],
            )
            .unwrap();
        assert!(
            connection
                .execute(
                    "INSERT INTO comic_planning_chapter_sources (comic_planning_chapter_id,novel_chapter_revision_id,source_order,source_start,source_end) VALUES ('planning-a','source-a',3,0,?)",
                    params![utf8_end + 1],
                )
                .is_err(),
            "v21 must reject an end beyond the UTF-8 byte length"
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO comic_planning_chapter_sources (comic_planning_chapter_id,novel_chapter_revision_id,source_order,source_start,source_end) VALUES ('planning-a','source-b',4,0,1)",
                    [],
                )
                .is_err(),
            "v21 must preserve cross-NovelWork rejection"
        );
    }

    #[test]
    fn v12_adaptation_analysis_freezes_ten_inputs_and_emits_four_scoped_outputs() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (sql, version, name) in [
            (MIGRATION_V1, 1, "v1"),
            (MIGRATION_V2, 2, "v2"),
            (MIGRATION_V3, 3, "v3"),
            (MIGRATION_V4, 4, "v4"),
            (MIGRATION_V5, 5, "v5"),
            (MIGRATION_V6, 6, "v6"),
            (MIGRATION_V7, 7, "v7"),
            (MIGRATION_V8, 8, "v8"),
            (MIGRATION_V9, 9, "v9"),
            (MIGRATION_V10, 10, "v10"),
            (MIGRATION_V11, 11, "v11"),
        ] {
            run_migration(&connection, sql, version, name).unwrap();
        }
        connection.execute_batch(
            "INSERT INTO novel_works (id,project_id,title,description,status,created_at,updated_at) VALUES ('work-a','project-a','A','','active',1,1);
             INSERT INTO novel_chapters (id,novel_work_id,volume_id,sequence_no,chapter_no,title,current_revision_id,created_at,updated_at) VALUES ('chapter-a','work-a',NULL,1,1,'A',NULL,1,1);
             INSERT INTO novel_chapter_revisions (id,novel_chapter_id,version,content,content_hash,asset_id,requested_parent_context_revision_id,source_kind,created_at) VALUES ('source-text-a','chapter-a',1,'abcdef','hash',NULL,NULL,'paste',1);
             UPDATE novel_chapters SET current_revision_id='source-text-a' WHERE id='chapter-a';
             INSERT INTO novel_canon_versions (id,novel_work_id,version,parent_version_id,body_json,rendered_markdown,status,created_at) VALUES ('canon-a','work-a',0,NULL,'{}','','published',1);
             INSERT INTO novel_analysis_lineages (id,novel_work_id,name,base_published_canon_version_id,base_published_novel_state_version_id,current_context_revision_id,continuous_through_sequence_no,status,optimistic_version,created_at,updated_at) VALUES ('lineage-a','work-a','main','canon-a',NULL,NULL,0,'active',0,1,1);
             INSERT INTO comic_adaptations (id,project_id,novel_work_id,title,status,config_json,current_continuity_version_id,optimistic_version,created_at,updated_at) VALUES ('adapt-a','project-a','work-a','A','active','{}',NULL,0,1,1), ('adapt-b','project-a','work-a','B','active','{}',NULL,0,1,1);
             INSERT INTO continuity_state_versions (id,comic_adaptation_id,version,parent_version_id,through_comic_chapter_id,body_json,created_at) VALUES ('continuity-a','adapt-a',0,NULL,NULL,'{}',1), ('continuity-b','adapt-b',0,NULL,NULL,'{}',1);
             UPDATE comic_adaptations SET current_continuity_version_id='continuity-a' WHERE id='adapt-a';
             UPDATE comic_adaptations SET current_continuity_version_id='continuity-b' WHERE id='adapt-b';
             INSERT INTO comic_adaptation_chapters (id,comic_adaptation_id,novel_chapter_revision_id,sequence_no,created_at) VALUES ('adapt-chapter-a','adapt-a','source-text-a',1,1), ('adapt-chapter-b','adapt-b','source-text-a',1,1);
             INSERT INTO source_analysis_runs (id,novel_chapter_revision_id,novel_analysis_lineage_id,base_working_context_revision_id,base_canon_version_id,base_novel_state_version_id,frozen_input_fingerprint,provider_id,model_id,status,review_status,current_stage,prompt_version,schema_version,idempotency_key,progress_json,safe_error_code,safe_user_message,created_at,updated_at,completed_at,frozen_comic_adaptation_id,frozen_comic_chapter_id) VALUES
               ('source-run-a','source-text-a','lineage-a',NULL,'canon-a',NULL,'source-fingerprint','p','m','ready_for_review','pending','done','v1','novel-analysis.v1','source-key-a','{}',NULL,NULL,1,1,1,NULL,NULL),
               ('source-run-other','source-text-a','lineage-a',NULL,'canon-a',NULL,'source-fingerprint-other','p','m','ready_for_review','pending','done','v1','novel-analysis.v1','source-key-other','{}',NULL,NULL,1,1,1,NULL,NULL);",
        ).unwrap();
        let original_types = [
            "chapter_summary",
            "chapter_beats",
            "world_facts",
            "character_facts",
            "faction_facts",
            "location_facts",
            "prop_facts",
            "timeline_delta",
            "continuity_delta",
            "open_threads",
        ];
        for (index, artifact_type) in original_types.iter().enumerate() {
            let artifact_id = format!("input-artifact-{index}");
            let revision_id = format!("input-revision-{index}");
            connection.execute(
                "INSERT INTO analysis_artifacts (id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,candidate_head_revision_id,adopted_head_revision_id,status,optimistic_version,created_at,updated_at) VALUES (?, 'source-run-a',NULL,?, 'work-a','source-text-a',NULL,NULL,?,NULL,'active',0,1,1)",
                params![artifact_id, artifact_type, revision_id],
            ).unwrap();
            connection.execute(
                "INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?, ?,1,NULL,'{}','','seed','{}','{}','candidate',1)",
                params![revision_id, artifact_id],
            ).unwrap();
        }
        connection.execute_batch(
            "INSERT INTO analysis_artifacts (id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,candidate_head_revision_id,adopted_head_revision_id,status,optimistic_version,created_at,updated_at) VALUES ('other-artifact','source-run-other',NULL,'chapter_summary','work-a','source-text-a',NULL,NULL,'other-revision',NULL,'active',0,1,1);
             INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES ('other-revision','other-artifact',1,NULL,'{}','','seed','{}','{}','candidate',1);",
        ).unwrap();

        run_migration(&connection, MIGRATION_V12, 12, "v12").unwrap();
        assert_eq!(schema_version(&connection).unwrap(), 12);
        for table in [
            "adaptation_analysis_runs",
            "adaptation_analysis_run_inputs",
            "adaptation_analysis_run_attempts",
            "adaptation_analysis_run_events",
            "adaptation_analysis_run_artifacts",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing {table}");
        }
        connection.execute(
            "INSERT INTO adaptation_analysis_runs (id,project_id,novel_work_id,comic_adaptation_id,comic_adaptation_chapter_id,input_mode,source_analysis_run_id,base_canon_version_id,base_novel_state_version_id,base_continuity_state_version_id,provider_id,model_id,prompt_version,schema_version,frozen_input_fingerprint,idempotency_key,status,attempt_no,lease_owner,lease_expires_at,heartbeat_at,safe_error_code,safe_user_message,created_at,updated_at,finished_at) VALUES ('adapt-run-a','project-a','work-a','adapt-a','adapt-chapter-a','source_run','source-run-a','canon-a',NULL,'continuity-a','p','m','v1','novel-analysis.v1','frozen','adapt-run-key','draft',1,NULL,NULL,NULL,NULL,NULL,2,2,NULL)", [],
        ).unwrap();
        assert!(connection.execute(
            "INSERT INTO adaptation_analysis_run_inputs (adaptation_analysis_run_id,artifact_type,analysis_artifact_revision_id,source_order) VALUES ('adapt-run-a','chapter_summary','other-revision',0)", [],
        ).is_err(), "source-run input must freeze a revision from its exact source run");
        for (index, artifact_type) in original_types.iter().enumerate() {
            connection.execute(
                "INSERT INTO adaptation_analysis_run_inputs (adaptation_analysis_run_id,artifact_type,analysis_artifact_revision_id,source_order) VALUES ('adapt-run-a',?,?,?)",
                params![artifact_type, format!("input-revision-{index}"), index as i64],
            ).unwrap();
        }
        connection.execute(
            "INSERT INTO adaptation_analysis_run_attempts (id,adaptation_analysis_run_id,attempt_no,parent_attempt_id,status,lease_owner,lease_expires_at,heartbeat_at,safe_error_code,safe_user_message,created_at,finished_at) VALUES ('adapt-attempt-1','adapt-run-a',1,NULL,'queued','worker',100,NULL,NULL,NULL,3,NULL)", [],
        ).unwrap();
        connection.execute(
            "UPDATE adaptation_analysis_runs SET status='queued',lease_owner='worker',lease_expires_at=100,heartbeat_at=3,updated_at=3 WHERE id='adapt-run-a'", [],
        ).unwrap();
        connection.execute_batch(
            "UPDATE adaptation_analysis_run_attempts SET status='running',heartbeat_at=4 WHERE id='adapt-attempt-1';
             UPDATE adaptation_analysis_runs SET status='running',heartbeat_at=4,updated_at=4 WHERE id='adapt-run-a';
             INSERT INTO adaptation_analysis_run_events (id,adaptation_analysis_run_id,seq,event_type,payload_json,created_at) VALUES ('event-1','adapt-run-a',1,'submitted','{}',4);",
        ).unwrap();
        assert!(connection.execute(
            "UPDATE adaptation_analysis_runs SET status='ready_for_review',updated_at=5 WHERE id='adapt-run-a'", [],
        ).is_err(), "ready status requires all four output mappings and success attempt");
        assert!(connection.execute(
            "INSERT INTO analysis_artifacts (id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,status,optimistic_version,created_at,updated_at) VALUES ('bad-original',NULL,'adapt-run-a','world_facts','work-a',NULL,'adapt-a',NULL,'active',0,5,5)", [],
        ).is_err(), "adaptation analysis cannot emit an original fact type");
        assert!(connection.execute(
            "INSERT INTO analysis_artifacts (id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,status,optimistic_version,created_at,updated_at) VALUES ('bad-owner',NULL,'adapt-run-a','scene_plan','work-a',NULL,'adapt-b','adapt-chapter-b','active',0,5,5)", [],
        ).is_err(), "adaptation output cannot cross its adaptation owner");
        for (index, artifact_type) in [
            "adaptation_proposal",
            "comic_chapter_plan",
            "scene_plan",
            "page_panel_plan",
        ]
        .iter()
        .enumerate()
        {
            let artifact_id = format!("output-artifact-{index}");
            let chapter_id = if *artifact_type == "adaptation_proposal" {
                None
            } else {
                Some("adapt-chapter-a")
            };
            connection.execute(
                "INSERT INTO analysis_artifacts (id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,status,optimistic_version,created_at,updated_at) VALUES (?,NULL,'adapt-run-a',?,'work-a',NULL,'adapt-a',?,'active',0,5,5)",
                params![artifact_id, artifact_type, chapter_id],
            ).unwrap();
            connection.execute(
                "INSERT INTO adaptation_analysis_run_artifacts (adaptation_analysis_run_id,artifact_type,analysis_artifact_id) VALUES ('adapt-run-a',?,?)",
                params![artifact_type, artifact_id],
            ).unwrap();
        }
        connection.execute_batch(
            "UPDATE adaptation_analysis_run_attempts SET status='success',finished_at=6 WHERE id='adapt-attempt-1';
             UPDATE adaptation_analysis_runs SET status='ready_for_review',lease_owner=NULL,lease_expires_at=NULL,finished_at=6,updated_at=6 WHERE id='adapt-run-a';",
        ).unwrap();
        let outputs: i64 = connection.query_row("SELECT COUNT(*) FROM adaptation_analysis_run_artifacts WHERE adaptation_analysis_run_id='adapt-run-a'", [], |row| row.get(0)).unwrap();
        assert_eq!(outputs, 4);

        // An accepted head and a later production apply must retain one whole
        // analysis lineage.  A second v12 run may legitimately target the
        // same adaptation chapter, but its revisions must never be mixed into
        // this head or apply.
        for index in 0..4 {
            let artifact_id = format!("output-artifact-{index}");
            let revision_id = format!("output-revision-a-{index}");
            connection.execute(
                "INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?, ?,1,NULL,'{}','','seed','{}','{}','adopted',7)",
                params![revision_id, artifact_id],
            ).unwrap();
            connection
                .execute(
                    "UPDATE analysis_artifacts SET adopted_head_revision_id=? WHERE id=?",
                    params![revision_id, artifact_id],
                )
                .unwrap();
        }
        connection.execute(
            "INSERT INTO adaptation_analysis_runs (id,project_id,novel_work_id,comic_adaptation_id,comic_adaptation_chapter_id,input_mode,source_analysis_run_id,base_canon_version_id,base_novel_state_version_id,base_continuity_state_version_id,provider_id,model_id,prompt_version,schema_version,frozen_input_fingerprint,idempotency_key,status,attempt_no,lease_owner,lease_expires_at,heartbeat_at,safe_error_code,safe_user_message,created_at,updated_at,finished_at) VALUES ('adapt-run-b','project-a','work-a','adapt-a','adapt-chapter-a','source_run','source-run-a','canon-a',NULL,'continuity-a','p','m','v1','novel-analysis.v1','frozen-b','adapt-run-key-b','draft',1,NULL,NULL,NULL,NULL,NULL,7,7,NULL)", [],
        ).unwrap();
        for (index, artifact_type) in original_types.iter().enumerate() {
            connection.execute(
                "INSERT INTO adaptation_analysis_run_inputs (adaptation_analysis_run_id,artifact_type,analysis_artifact_revision_id,source_order) VALUES ('adapt-run-b',?,?,?)",
                params![artifact_type, format!("input-revision-{index}"), index as i64],
            ).unwrap();
        }
        connection.execute_batch(
            "INSERT INTO adaptation_analysis_run_attempts (id,adaptation_analysis_run_id,attempt_no,parent_attempt_id,status,lease_owner,lease_expires_at,heartbeat_at,safe_error_code,safe_user_message,created_at,finished_at) VALUES ('adapt-attempt-b-1','adapt-run-b',1,NULL,'queued','worker-b',100,NULL,NULL,NULL,7,NULL);
             UPDATE adaptation_analysis_runs SET status='queued',lease_owner='worker-b',lease_expires_at=100,heartbeat_at=7,updated_at=7 WHERE id='adapt-run-b';
             UPDATE adaptation_analysis_run_attempts SET status='running',heartbeat_at=8 WHERE id='adapt-attempt-b-1';
             UPDATE adaptation_analysis_runs SET status='running',heartbeat_at=8,updated_at=8 WHERE id='adapt-run-b';",
        ).unwrap();
        for (index, artifact_type) in [
            "adaptation_proposal",
            "comic_chapter_plan",
            "scene_plan",
            "page_panel_plan",
        ]
        .iter()
        .enumerate()
        {
            let artifact_id = format!("output-artifact-b-{index}");
            let chapter_id = if *artifact_type == "adaptation_proposal" {
                None
            } else {
                Some("adapt-chapter-a")
            };
            let revision_id = format!("output-revision-b-{index}");
            connection.execute(
                "INSERT INTO analysis_artifacts (id,source_analysis_run_id,adaptation_analysis_run_id,artifact_type,novel_work_id,novel_chapter_revision_id,comic_adaptation_id,comic_chapter_id,status,optimistic_version,created_at,updated_at) VALUES (?,NULL,'adapt-run-b',?,'work-a',NULL,'adapt-a',?,'active',0,8,8)",
                params![artifact_id, artifact_type, chapter_id],
            ).unwrap();
            connection.execute(
                "INSERT INTO adaptation_analysis_run_artifacts (adaptation_analysis_run_id,artifact_type,analysis_artifact_id) VALUES ('adapt-run-b',?,?)",
                params![artifact_type, artifact_id],
            ).unwrap();
            connection.execute(
                "INSERT INTO analysis_artifact_revisions (id,analysis_artifact_id,version,parent_revision_id,body_json,rendered_markdown,change_type,provenance_json,validation_json,status,created_at) VALUES (?, ?,1,NULL,'{}','','seed','{}','{}','adopted',8)",
                params![revision_id, artifact_id],
            ).unwrap();
            connection
                .execute(
                    "UPDATE analysis_artifacts SET adopted_head_revision_id=? WHERE id=?",
                    params![revision_id, artifact_id],
                )
                .unwrap();
        }
        connection.execute_batch(
            "UPDATE adaptation_analysis_run_attempts SET status='success',finished_at=9 WHERE id='adapt-attempt-b-1';
             UPDATE adaptation_analysis_runs SET status='ready_for_review',lease_owner=NULL,lease_expires_at=NULL,finished_at=9,updated_at=9 WHERE id='adapt-run-b';
             INSERT INTO analysis_apply_operations (id,operation_type,novel_work_id,comic_adaptation_id,base_target_version_id,idempotency_key,preview_fingerprint,approval_token_hash,status,created_at,updated_at,approval_expires_at,expected_adaptation_version) VALUES
               ('accept-a','accept_adaptation',NULL,'adapt-a',NULL,'accept-a-key','preview','token','approved',10,10,100,0),
               ('accept-b','accept_adaptation',NULL,'adapt-a',NULL,'accept-b-key','preview','token','approved',10,10,100,0),
               ('apply-a','apply_comic_plan',NULL,'adapt-a',NULL,'apply-a-key','preview','token','approved',10,10,100,0);
             INSERT INTO comic_adaptation_plan_heads (id,comic_adaptation_id,adaptation_proposal_revision_id,comic_chapter_plan_revision_id,scene_plan_revision_id,accept_apply_operation_id,status,created_at,updated_at) VALUES
               ('head-a','adapt-a','output-revision-a-0','output-revision-a-1','output-revision-a-2','accept-a','active',10,10);
             INSERT INTO comic_planning_chapters (id,comic_adaptation_plan_head_id,comic_adaptation_id,planning_chapter_stable_key,status,created_at,updated_at) VALUES
               ('planning-a','head-a','adapt-a','chapter-001','planning',10,10);
             INSERT INTO analysis_apply_operation_sources (analysis_apply_operation_id,analysis_artifact_revision_id,source_role,source_order) VALUES
               ('apply-a','output-revision-b-3','scene_input',0);",
        ).unwrap();
        assert!(connection.execute(
            "INSERT INTO comic_adaptation_plan_heads (id,comic_adaptation_id,adaptation_proposal_revision_id,comic_chapter_plan_revision_id,scene_plan_revision_id,accept_apply_operation_id,status,created_at,updated_at) VALUES ('cross-run-head','adapt-a','output-revision-b-0','output-revision-a-1','output-revision-a-2','accept-b','active',11,11)", [],
        ).is_err(), "same-adaptation head must not mix distinct v12 analysis runs");
        assert!(connection.execute(
            "INSERT INTO comic_production_chapters (id,comic_adaptation_id,comic_planning_chapter_id,page_panel_plan_revision_id,apply_operation_id,status,created_at) VALUES ('cross-run-production','adapt-a','planning-a','output-revision-b-3','apply-a','active',11)", [],
        ).is_err(), "same-adaptation production apply must not use a page plan from another v12 analysis run");
        assert!(connection.execute(
            "INSERT INTO adaptation_analysis_runs (id,project_id,novel_work_id,comic_adaptation_id,comic_adaptation_chapter_id,input_mode,source_analysis_run_id,base_canon_version_id,base_novel_state_version_id,base_continuity_state_version_id,provider_id,model_id,prompt_version,schema_version,frozen_input_fingerprint,idempotency_key,status,attempt_no,lease_owner,lease_expires_at,heartbeat_at,safe_error_code,safe_user_message,created_at,updated_at,finished_at) VALUES ('bad-scope','project-b','work-a','adapt-a','adapt-chapter-a','source_run','source-run-a','canon-a',NULL,'continuity-a','p','m','v1','novel-analysis.v1','frozen','bad-scope-key','draft',1,NULL,NULL,NULL,NULL,NULL,7,7,NULL)", [],
        ).is_err(), "project/NovelWork/adaptation owner scope must fail closed");
    }

    #[test]
    fn refuses_a_database_from_a_newer_application() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(migrate(&connection)
            .unwrap_err()
            .contains("高于当前程序支持"));
    }

    #[test]
    fn v5_merges_duplicate_references_before_creating_business_key_index() {
        let connection = Connection::open_in_memory().unwrap();
        run_migration(&connection, MIGRATION_V1, 1, "v1").unwrap();
        run_migration(&connection, MIGRATION_V2, 2, "v2").unwrap();
        run_migration(&connection, MIGRATION_V3, 3, "v3").unwrap();
        run_migration(&connection, MIGRATION_V4, 4, "v4").unwrap();
        connection.execute_batch(
            "INSERT INTO comic_references (id, comic_project_id, owner_type, owner_id, asset_id, role, sort_order, approved, created_at)
             VALUES ('later', 'comic', 'card', 'card', 'asset', 'portrait', 9, 0, 20);
             INSERT INTO comic_references (id, comic_project_id, owner_type, owner_id, asset_id, role, sort_order, approved, created_at)
             VALUES ('canonical', 'comic', 'card', 'card', 'asset', 'portrait', 1, 0, 10);
             INSERT INTO comic_references (id, comic_project_id, owner_type, owner_id, asset_id, role, sort_order, approved, created_at)
             VALUES ('approved-duplicate', 'comic', 'card', 'card', 'asset', 'portrait', 5, 1, 15);",
        ).unwrap();
        run_migration(&connection, MIGRATION_V5, 5, "v5").unwrap();
        let rows: (i64, String, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), MIN(id), MIN(sort_order), MAX(approved) FROM comic_references",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(rows, (1, "canonical".into(), 1, 1));
        assert!(connection.execute(
            "INSERT INTO comic_references (id, comic_project_id, owner_type, owner_id, asset_id, role, sort_order, approved, created_at)
             VALUES ('duplicate-again', 'comic', 'card', 'card', 'asset', 'portrait', 10, 0, 30)", [],
        ).is_err(), "v5 must enforce the deduplicated business key");
    }

    #[test]
    fn v1_through_v4_upgrade_to_v6_preserves_legacy_rows_and_is_atomic() {
        for source_version in 1..=4 {
            let connection = Connection::open_in_memory().unwrap();
            run_migration(&connection, MIGRATION_V1, 1, "v1").unwrap();
            if source_version >= 2 {
                run_migration(&connection, MIGRATION_V2, 2, "v2").unwrap();
                connection.execute_batch(
                    "INSERT INTO comic_references (id, comic_project_id, owner_type, owner_id, asset_id, role, sort_order, approved, created_at)
                     VALUES ('duplicate-late', 'comic', 'card', 'card', 'asset', 'portrait', 2, 0, 2);
                     INSERT INTO comic_references (id, comic_project_id, owner_type, owner_id, asset_id, role, sort_order, approved, created_at)
                     VALUES ('duplicate-canonical', 'comic', 'card', 'card', 'asset', 'portrait', 1, 1, 1);",
                ).unwrap();
            }
            if source_version >= 3 {
                run_migration(&connection, MIGRATION_V3, 3, "v3").unwrap();
                connection.execute_batch(
                    "INSERT INTO comic_page_runs (id, comic_project_id, page_no, asset_id, request_json, reference_snapshot_json, status, error, created_at, finished_at)
                     VALUES ('legacy-page-running', 'comic', 1, NULL, '{}', '[]', 'running', NULL, 1, NULL);
                     INSERT INTO comic_page_runs (id, comic_project_id, page_no, asset_id, request_json, reference_snapshot_json, status, error, created_at, finished_at)
                     VALUES ('legacy-page-error', 'comic', 2, NULL, '{}', '[]', 'error', 'legacy provider failure', 2, 2);",
                ).unwrap();
            }
            if source_version >= 4 {
                run_migration(&connection, MIGRATION_V4, 4, "v4").unwrap();
                connection.execute_batch(
                    "INSERT INTO comic_panel_runs (id, panel_id, task_id, asset_id, parent_run_id, strategy, request_json, reference_snapshot_json, score_json, status, created_at, finished_at, error)
                     VALUES ('legacy-panel-running', 'panel', NULL, NULL, NULL, 'legacy', '{}', '[]', NULL, 'running', 1, NULL, NULL);
                     INSERT INTO comic_panel_runs (id, panel_id, task_id, asset_id, parent_run_id, strategy, request_json, reference_snapshot_json, score_json, status, created_at, finished_at, error)
                     VALUES ('legacy-panel-error', 'panel', NULL, NULL, NULL, 'legacy', '{}', '[]', NULL, 'error', 2, 2, 'legacy panel failure');",
                ).unwrap();
            }

            migrate(&connection).unwrap();
            let version: i64 = connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(
                version, SCHEMA_VERSION,
                "v{source_version} must upgrade to v6"
            );
            let receipts: i64 = connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='comic_operation_receipts'",
                [],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(receipts, 1);

            if source_version >= 2 {
                let reference: (i64, String, i64) = connection
                    .query_row(
                        "SELECT COUNT(*), MIN(id), MAX(approved) FROM comic_references",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .unwrap();
                assert_eq!(reference, (1, "duplicate-canonical".into(), 1));
                assert!(connection.execute(
                    "INSERT INTO comic_references (id, comic_project_id, owner_type, owner_id, asset_id, role, sort_order, approved, created_at)
                     VALUES ('duplicate-after-upgrade', 'comic', 'card', 'card', 'asset', 'portrait', 3, 0, 3)", [],
                ).is_err());
            }
            if source_version >= 3 {
                let legacy: (Option<String>, Option<String>, Option<i64>, String) = connection.query_row(
                    "SELECT generation_attempt_id, owner_app_session_id, lease_expires_at, status FROM comic_page_runs WHERE id='legacy-page-running'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                ).unwrap();
                assert_eq!(legacy.0, None);
                assert_eq!(legacy.1, None);
                assert!(
                    legacy.2.is_some(),
                    "legacy running row needs an expired recovery lease"
                );
                assert_eq!(legacy.3, "running");
                let failure: String = connection
                    .query_row(
                        "SELECT failure_json FROM comic_page_runs WHERE id='legacy-page-error'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(failure.contains("LEGACY_ERROR"));
            }
            if source_version >= 4 {
                let failure: String = connection
                    .query_row(
                        "SELECT failure_json FROM comic_panel_runs WHERE id='legacy-panel-error'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(failure.contains("LEGACY_ERROR"));
            }
        }

        let connection = Connection::open_in_memory().unwrap();
        run_migration(&connection, MIGRATION_V1, 1, "v1").unwrap();
        run_migration(&connection, MIGRATION_V2, 2, "v2").unwrap();
        run_migration(&connection, MIGRATION_V3, 3, "v3").unwrap();
        run_migration(&connection, MIGRATION_V4, 4, "v4").unwrap();
        let invalid_v5 = format!("{MIGRATION_V5}\nTHIS IS INVALID SQL;");
        assert!(run_migration(&connection, &invalid_v5, 5, "v5-invalid").is_err());
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4, "failed v5 must not advance user_version");
        let v5_column_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('comic_page_runs') WHERE name='generation_attempt_id'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(
            v5_column_count, 0,
            "failed v5 must roll back schema changes"
        );
    }

    #[test]
    fn v24_and_v25_back_up_v23_and_preserve_markdown_history_with_empty_new_metadata() {
        let root=std::env::temp_dir().join(format!("comic-md-v24-migration-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();let path=root.join("test.db");
        let connection=Connection::open(&path).unwrap();connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (sql,version,name) in [(MIGRATION_V1,1,"v1"),(MIGRATION_V2,2,"v2"),(MIGRATION_V3,3,"v3"),(MIGRATION_V4,4,"v4"),(MIGRATION_V5,5,"v5"),(MIGRATION_V6,6,"v6"),(MIGRATION_V7,7,"v7"),(MIGRATION_V8,8,"v8"),(MIGRATION_V9,9,"v9"),(MIGRATION_V10,10,"v10"),(MIGRATION_V11,11,"v11"),(MIGRATION_V12,12,"v12"),(MIGRATION_V13,13,"v13"),(MIGRATION_V14,14,"v14"),(MIGRATION_V15,15,"v15"),(MIGRATION_V16,16,"v16"),(MIGRATION_V17,17,"v17"),(MIGRATION_V18,18,"v18"),(MIGRATION_V19,19,"v19"),(MIGRATION_V20,20,"v20"),(MIGRATION_V21,21,"v21")] {run_migration(&connection,sql,version,name).unwrap();}
        migrate_adaptation_chapter_revisions(&connection).unwrap();run_migration(&connection,MIGRATION_V23,23,"v23").unwrap();
        connection.execute_batch(include_str!("../fixtures/legacy-comic-retirement.sql")).unwrap();
        connection.execute_batch("INSERT INTO comic_md_documents(id,project_id,novel_work_id,chapter_id,kind,page_no,markdown,revision,dependencies,updated_at) VALUES('old-md','legacy-test-project','legacy-test-work','legacy-test-chapter','page_prompt',1,'旧版 Markdown 原文',1,'[]',1); INSERT INTO comic_md_revisions(document_id,revision,markdown,dependencies,created_at) VALUES('old-md',1,'旧版 Markdown 原文','[]',1); INSERT INTO comic_md_jobs(id,project_id,novel_work_id,chapter_id,kind,status,input_snapshot,completed_pages,total_pages,created_at) VALUES('old-md-job','legacy-test-project','legacy-test-work','legacy-test-chapter','images','succeeded','{}',1,1,1); INSERT INTO comic_md_images(id,job_id,document_id,document_revision,page_no,path,created_at) VALUES('old-image','old-md-job','old-md',1,1,'old.png',1);").unwrap();
        drop(connection);
        let db=DbState::open(path.clone()).unwrap();
        with_connection(&db,|c| {
            assert_eq!(schema_version(c).unwrap(),25);
            let metadata:(String,String,String,String,String)=c.query_row("SELECT d.markdown,d.optimization_instruction,r.optimization_instruction,i.prompt_injection,i.rerun_prompt_injection FROM comic_md_documents d JOIN comic_md_revisions r ON r.document_id=d.id JOIN comic_md_images i ON i.document_id=d.id WHERE d.id='old-md'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
            assert_eq!(metadata,("旧版 Markdown 原文".into(),String::new(),String::new(),String::new(),String::new()));
            let options:i64=c.query_row("SELECT count(*) FROM comic_md_render_options",[],|r|r.get(0)).unwrap();assert_eq!(options,0);
            let invalid:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",[],|r|r.get(0)).unwrap();assert!(!invalid);Ok(())
        }).unwrap();
        let backups=backup_files(&backup_directory(&path).unwrap(),backup_prefix(&path).unwrap()).unwrap();assert_eq!(backups.len(),1);validate_backup(&backups[0],23).unwrap();
        let backup=Connection::open(&backups[0]).unwrap();let original:String=backup.query_row("SELECT markdown FROM comic_md_documents WHERE id='old-md'",[],|r|r.get(0)).unwrap();assert_eq!(original,"旧版 Markdown 原文");drop(backup);drop(db);std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v25_backs_up_v24_and_preserves_existing_images_with_empty_rerun_injection() {
        let root =
            std::env::temp_dir().join(format!("comic-md-v25-migration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("test.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA user_version=24;
                 CREATE TABLE comic_md_images(
                   id TEXT PRIMARY KEY,
                   job_id TEXT NOT NULL,
                   document_id TEXT NOT NULL,
                   document_revision INTEGER NOT NULL,
                   page_no INTEGER NOT NULL,
                   path TEXT NOT NULL,
                   created_at INTEGER NOT NULL,
                   prompt_injection TEXT NOT NULL DEFAULT ''
                 );
                 INSERT INTO comic_md_images VALUES('old-image','old-job','old-doc',3,1,'old.png',9,'本章旧注入');",
            )
            .unwrap();
        drop(connection);

        let db = DbState::open(path.clone()).unwrap();
        with_connection(&db, |connection| {
            assert_eq!(schema_version(connection).unwrap(), 25);
            let image: (String, String, String) = connection
                .query_row(
                    "SELECT path,prompt_injection,rerun_prompt_injection FROM comic_md_images WHERE id='old-image'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(image, ("old.png".into(), "本章旧注入".into(), String::new()));
            Ok(())
        })
        .unwrap();

        let backups = backup_files(
            &backup_directory(&path).unwrap(),
            backup_prefix(&path).unwrap(),
        )
        .unwrap();
        assert_eq!(backups.len(), 1);
        validate_backup(&backups[0], 24).unwrap();
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v22_preserves_referenced_chapters_and_accepts_new_source_revisions() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (sql, version, name) in [
            (MIGRATION_V1, 1, "v1"),
            (MIGRATION_V2, 2, "v2"),
            (MIGRATION_V3, 3, "v3"),
            (MIGRATION_V4, 4, "v4"),
            (MIGRATION_V5, 5, "v5"),
            (MIGRATION_V6, 6, "v6"),
            (MIGRATION_V7, 7, "v7"),
            (MIGRATION_V8, 8, "v8"),
            (MIGRATION_V9, 9, "v9"),
            (MIGRATION_V10, 10, "v10"),
            (MIGRATION_V11, 11, "v11"),
            (MIGRATION_V12, 12, "v12"),
            (MIGRATION_V13, 13, "v13"),
            (MIGRATION_V14, 14, "v14"),
            (MIGRATION_V15, 15, "v15"),
            (MIGRATION_V16, 16, "v16"),
            (MIGRATION_V17, 17, "v17"),
            (MIGRATION_V18, 18, "v18"),
            (MIGRATION_V19, 19, "v19"),
            (MIGRATION_V20, 20, "v20"),
            (MIGRATION_V21, 21, "v21"),
        ] {
            run_migration(&connection, sql, version, name).unwrap();
        }
        connection.execute_batch("
            INSERT INTO novel_works(id,project_id,title,description,status,created_at,updated_at)
              VALUES('work','project','test','','active',1,1);
            INSERT INTO novel_chapters(id,novel_work_id,sequence_no,chapter_no,created_at,updated_at)
              VALUES('chapter','work',1,1,1,1);
            INSERT INTO novel_chapter_revisions(id,novel_chapter_id,version,content,content_hash,source_kind,created_at)
              VALUES('r1','chapter',1,'old','h1','paste',1),('r2','chapter',2,'new','h2','paste',2);
            INSERT INTO comic_adaptations(id,project_id,novel_work_id,title,status,config_json,optimistic_version,created_at,updated_at)
              VALUES('adapt','project','work','test','active','{}',0,1,1);
            INSERT INTO comic_adaptation_chapters VALUES('old-scope','adapt','r1',1,1);
            CREATE TABLE migration_reference(scope TEXT REFERENCES comic_adaptation_chapters(id) ON DELETE RESTRICT);
            INSERT INTO migration_reference VALUES('old-scope');
        ").unwrap();
        assert!(connection.execute("INSERT INTO comic_adaptation_chapters VALUES('new-scope','adapt','r2',1,2)", []).is_err());
        migrate(&connection).unwrap();
        assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
        assert!(connection.pragma_query_value::<bool, _>(None, "foreign_keys", |row| row.get(0)).unwrap());
        assert!(!connection.pragma_query_value::<bool, _>(None, "legacy_alter_table", |row| row.get(0)).unwrap());
        let old: String = connection.query_row("SELECT chapter.novel_chapter_revision_id FROM migration_reference ref JOIN comic_adaptation_chapters chapter ON chapter.id=ref.scope", [], |row| row.get(0)).unwrap();
        assert_eq!(old, "r1");
        connection.execute("INSERT INTO comic_adaptation_chapters VALUES('new-scope','adapt','r2',1,2)", []).unwrap();
        assert!(connection.execute("INSERT INTO comic_adaptation_chapters VALUES('duplicate','adapt','r2',1,3)", []).is_err());
        assert!(connection.execute("DELETE FROM comic_adaptation_chapters WHERE id='old-scope'", []).is_err());
        assert!(connection.execute("INSERT INTO migration_reference VALUES('missing')", []).is_err());
        let broken: i64 = connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| row.get(0)).unwrap();
        assert_eq!(broken, 0);
        migrate(&connection).unwrap();
    }

    #[test]
    fn v22_failure_rolls_back_schema_and_restores_foreign_keys() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA user_version=21;
            CREATE TABLE comic_adaptation_chapters(id TEXT PRIMARY KEY);
            INSERT INTO comic_adaptation_chapters VALUES('preserved');").unwrap();
        assert!(migrate_adaptation_chapter_revisions(&connection).is_err());
        assert_eq!(schema_version(&connection).unwrap(), 21);
        assert!(connection.pragma_query_value::<bool, _>(None, "foreign_keys", |row| row.get(0)).unwrap());
        assert!(!connection.pragma_query_value::<bool, _>(None, "legacy_alter_table", |row| row.get(0)).unwrap());
        let id: String = connection.query_row("SELECT id FROM comic_adaptation_chapters", [], |row| row.get(0)).unwrap();
        assert_eq!(id, "preserved");
        let temporary: i64 = connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='comic_adaptation_chapters_v22'", [], |row| row.get(0)).unwrap();
        assert_eq!(temporary, 0);
    }

    #[test]
    fn opens_a_wal_v1_file_and_upgrades_it_without_losing_settings() {
        let dir =
            std::env::temp_dir().join(format!("image-client-wal-v1-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.db");
        {
            let connection = Connection::open(&path).unwrap();
            run_migration(&connection, MIGRATION_V1, 1, "v1").unwrap();
            connection
                .execute(
                    "INSERT INTO settings (key, value) VALUES ('preupgrade', 'kept')",
                    [],
                )
                .unwrap();
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .unwrap();
        }
        let state = DbState::open(path).unwrap();
        let connection = state.connection.lock().unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let value: String = connection
            .query_row(
                "SELECT value FROM settings WHERE key='preupgrade'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(value, "kept");
        drop(connection);
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn create_v5_database(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let connection = Connection::open(&path).unwrap();
        run_migration(&connection, MIGRATION_V1, 1, "v1").unwrap();
        run_migration(&connection, MIGRATION_V2, 2, "v2").unwrap();
        run_migration(&connection, MIGRATION_V3, 3, "v3").unwrap();
        run_migration(&connection, MIGRATION_V4, 4, "v4").unwrap();
        run_migration(&connection, MIGRATION_V5, 5, "v5").unwrap();
        connection
            .execute(
                "INSERT INTO settings (key, value) VALUES ('backup-row', 'kept')",
                [],
            )
            .unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        path
    }

    fn create_v5_wal_database_with_open_writer(
        dir: &std::path::Path,
        name: &str,
    ) -> (std::path::PathBuf, Connection) {
        let path = dir.join(name);
        let connection = Connection::open(&path).unwrap();
        run_migration(&connection, MIGRATION_V1, 1, "v1").unwrap();
        run_migration(&connection, MIGRATION_V2, 2, "v2").unwrap();
        run_migration(&connection, MIGRATION_V3, 3, "v3").unwrap();
        run_migration(&connection, MIGRATION_V4, 4, "v4").unwrap();
        run_migration(&connection, MIGRATION_V5, 5, "v5").unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        connection
            .execute(
                "INSERT INTO settings (key, value) VALUES ('backup-wal-row', 'committed-in-wal')",
                [],
            )
            .unwrap();
        (path, connection)
    }

    #[test]
    fn v6_migration_adds_cross_page_history_index_without_temp_sort() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let mut statement = connection
            .prepare(
                "EXPLAIN QUERY PLAN SELECT id, created_at, status FROM comic_page_runs \
                 WHERE comic_project_id = ? ORDER BY created_at DESC, id DESC",
            )
            .unwrap();
        let plan = statement
            .query_map(["comic_1"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" | ");
        assert!(
            plan.contains("idx_comic_page_runs_history"),
            "cross-page history must use the v6 index: {plan}"
        );
        assert!(
            !plan.contains("USE TEMP B-TREE"),
            "cross-page history must not sort outside its index: {plan}"
        );
    }

    #[test]
    fn existing_wal_database_backup_contains_committed_uncheckpointed_wal_row() {
        let dir =
            std::env::temp_dir().join(format!("image-client-backup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (path, writer) = create_v5_wal_database_with_open_writer(&dir, "legacy.sqlite");
        let wal_path = std::path::PathBuf::from(format!("{}-wal", path.display()));
        assert!(wal_path.is_file(), "committed WAL must exist before backup");
        assert!(
            std::fs::metadata(&wal_path).unwrap().len() > 32,
            "WAL must contain committed frames before backup"
        );

        let state = DbState::open(path.clone()).unwrap();
        drop(state);

        let backups = backup_files(
            &backup_directory(&path).unwrap(),
            backup_prefix(&path).unwrap(),
        )
        .unwrap();
        assert_eq!(
            backups.len(),
            1,
            "existing v5 database needs exactly one pre-migration backup"
        );
        let backup = Connection::open(&backups[0]).unwrap();
        let integrity: String = backup
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        let backup_version: i64 = backup
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let value: String = backup
            .query_row(
                "SELECT value FROM settings WHERE key='backup-wal-row'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(integrity, "ok");
        assert_eq!(backup_version, 5);
        assert_eq!(value, "committed-in-wal");
        drop(backup);
        let upgraded = Connection::open(&path).unwrap();
        let version: i64 = upgraded
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        drop(upgraded);
        drop(writer);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn new_database_does_not_create_a_pre_migration_backup() {
        let dir =
            std::env::temp_dir().join(format!("image-client-new-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("new.sqlite");
        let state = DbState::open(path.clone()).unwrap();
        drop(state);
        let backup_dir = backup_directory(&path).unwrap();
        assert!(
            !backup_dir.exists()
                || backup_files(&backup_dir, backup_prefix(&path).unwrap())
                    .unwrap()
                    .is_empty()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pre_migration_backups_keep_only_the_latest_three_for_one_database_prefix() {
        let dir =
            std::env::temp_dir().join(format!("image-client-retention-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = create_v5_database(&dir, "retention.sqlite");
        let connection = Connection::open(&path).unwrap();
        for _ in 0..4 {
            create_migration_backup(&connection, &path, 5).unwrap();
        }
        let backups = backup_files(
            &backup_directory(&path).unwrap(),
            backup_prefix(&path).unwrap(),
        )
        .unwrap();
        assert_eq!(backups.len(), 3);
        drop(connection);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn backup_retention_orders_mixed_source_versions_by_backup_time_not_filename() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-backup-order-{}",
            uuid::Uuid::new_v4()
        ));
        let database = dir.join("mixed.sqlite");
        let backup_dir = backup_directory(&database).unwrap();
        std::fs::create_dir_all(&backup_dir).unwrap();
        let prefix = backup_prefix(&database).unwrap();
        for (version, timestamp, serial) in [
            (9, 100_u128, 1),
            (1, 400, 2),
            (8, 200, 3),
            (2, 300, 4),
            (-1, 500, 5),
        ] {
            let path = backup_dir.join(format!(
                "{prefix}{version}-{timestamp}-00000000-0000-4000-8000-{serial:012}.sqlite"
            ));
            std::fs::write(path, b"backup").unwrap();
        }

        let ordered = backup_files(&backup_dir, prefix.clone()).unwrap();
        let names = ordered
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                format!("{prefix}-1-500-00000000-0000-4000-8000-000000000005.sqlite"),
                format!("{prefix}1-400-00000000-0000-4000-8000-000000000002.sqlite"),
                format!("{prefix}2-300-00000000-0000-4000-8000-000000000004.sqlite"),
                format!("{prefix}8-200-00000000-0000-4000-8000-000000000003.sqlite"),
                format!("{prefix}9-100-00000000-0000-4000-8000-000000000001.sqlite"),
            ]
        );

        retain_recent_backups(&backup_dir, &prefix).unwrap();
        let retained = backup_files(&backup_dir, prefix).unwrap();
        assert_eq!(retained.len(), 3);
        assert!(retained.iter().all(|path| !path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("9-100")));
        assert!(retained.iter().any(|path| path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("-1-500-")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn backup_files_reports_unreadable_directory_instead_of_silently_ignoring_it() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-backup-read-dir-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("not-a-directory");
        std::fs::write(&file, b"file").unwrap();
        let error = backup_files(&file, "ignored-".to_string()).unwrap_err();
        assert!(error.contains("读取 SQLite 备份目录失败"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn online_backup_uses_a_short_temp_name_under_a_long_backup_parent() {
        let root = std::env::temp_dir().join(format!(
            "image-client-backup-long-path-{}",
            uuid::Uuid::new_v4()
        ));
        let mut database_dir = root.clone();
        while backup_directory(&database_dir.join("long-database.sqlite"))
            .unwrap()
            .to_string_lossy()
            .len()
            < 116
        {
            database_dir = database_dir.join("nested-backup-path");
        }
        std::fs::create_dir_all(&database_dir).unwrap();
        let path = create_v5_database(&database_dir, "long-database.sqlite");
        let backup_dir = backup_directory(&path).unwrap();
        assert!(backup_dir.to_string_lossy().len() >= 116);

        let source = Connection::open(&path).unwrap();
        create_migration_backup(&source, &path, 5).unwrap();
        let entries = std::fs::read_dir(&backup_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            backup_files(&backup_dir, backup_prefix(&path).unwrap())
                .unwrap()
                .len(),
            1
        );
        assert!(entries.iter().all(|name| !name.starts_with(".migration-")));
        assert!(entries.iter().all(|name| !name.ends_with("-journal")));
        drop(source);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn backup_failure_prevents_schema_advance_and_migration_failure_keeps_verified_backup() {
        let dir = std::env::temp_dir().join(format!(
            "image-client-backup-failure-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = create_v5_database(&dir, "failure.sqlite");
        let connection = Connection::open(&path).unwrap();
        let failing_backup = migrate_with_backup(
            &connection,
            &path,
            true,
            |_connection, _path, _version| Err("injected backup failure".to_string()),
            migrate,
        );
        assert!(failing_backup.is_err());
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5);

        let failing_migration =
            migrate_with_backup(&connection, &path, true, create_migration_backup, |conn| {
                run_migration(conn, "THIS IS INVALID SQL;", 6, "v6-invalid")
            });
        assert!(failing_migration.is_err());
        let backups = backup_files(
            &backup_directory(&path).unwrap(),
            backup_prefix(&path).unwrap(),
        )
        .unwrap();
        assert_eq!(
            backups.len(),
            1,
            "the verified backup must survive a migration failure"
        );
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5);
        drop(connection);
        let _ = std::fs::remove_dir_all(dir);
    }
}
