use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::{Duration as ChronoDuration, Local, NaiveDate, SecondsFormat};
use serde_json::{json, Map, Value};

const LOG_PREFIX: &str = "image-client-";
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 64 * 1024;
const RETENTION_DAYS: i64 = 10;

static LOGGER: OnceLock<FileLogger> = OnceLock::new();
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct WriterState {
    date: String,
    part: u32,
    size: u64,
    file: File,
}

struct PendingRecord {
    level: String,
    event: String,
    target: String,
    fields: Value,
}

pub struct FileLogger {
    dir: PathBuf,
    max_bytes: u64,
    retention_days: i64,
    state: Mutex<Option<WriterState>>,
}

impl FileLogger {
    fn new(dir: PathBuf) -> Result<Self, String> {
        Self::with_limits(dir, MAX_LOG_BYTES, RETENTION_DAYS)
    }

    fn with_limits(dir: PathBuf, max_bytes: u64, retention_days: i64) -> Result<Self, String> {
        fs::create_dir_all(&dir).map_err(|e| format!("创建日志目录失败: {e}"))?;
        crate::paths::secure_dir(&dir).map_err(|e| format!("设置日志目录权限失败: {e}"))?;
        let logger = Self {
            dir,
            max_bytes,
            retention_days,
            state: Mutex::new(None),
        };
        logger.cleanup_old_files(Local::now().date_naive())?;
        Ok(logger)
    }

    fn file_path(&self, date: &str, part: u32) -> PathBuf {
        if part == 0 {
            self.dir.join(format!("{LOG_PREFIX}{date}.log"))
        } else {
            self.dir.join(format!("{LOG_PREFIX}{date}.{part}.log"))
        }
    }

    fn open_state(&self, date: &str) -> Result<WriterState, String> {
        let mut part = 0;
        loop {
            let path = self.file_path(date, part);
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if !path.exists() || size < self.max_bytes {
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| format!("打开日志文件失败 {}: {e}", path.display()))?;
                crate::paths::secure_file(&path)
                    .map_err(|e| format!("设置日志文件权限失败 {}: {e}", path.display()))?;
                return Ok(WriterState {
                    date: date.to_string(),
                    part,
                    size,
                    file,
                });
            }
            part += 1;
        }
    }

    fn cleanup_old_files(&self, today: NaiveDate) -> Result<(), String> {
        let oldest = today - ChronoDuration::days(self.retention_days.saturating_sub(1));
        let entries = fs::read_dir(&self.dir).map_err(|e| format!("读取日志目录失败: {e}"))?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(LOG_PREFIX) || !name.ends_with(".log") {
                continue;
            }
            let date_text = name.get(LOG_PREFIX.len()..LOG_PREFIX.len() + 10);
            let Some(date) = date_text.and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            else {
                continue;
            };
            if date < oldest {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    fn write_record(
        &self,
        level: &str,
        event: &str,
        target: &str,
        fields: Value,
    ) -> Result<(), String> {
        self.write_records(vec![PendingRecord {
            level: level.to_string(),
            event: event.to_string(),
            target: target.to_string(),
            fields,
        }])
    }

    fn encode_record(record: PendingRecord) -> Result<(String, NaiveDate, Vec<u8>), String> {
        let now = Local::now();
        let date = now.format("%Y-%m-%d").to_string();
        let fields = sanitize_value(record.fields, None);
        let event = truncate(&record.event, 256);
        let target = truncate(&record.target, 64);
        let mut record = json!({
            "timestamp": now.to_rfc3339_opts(SecondsFormat::Millis, false),
            "sequence": SEQUENCE.fetch_add(1, Ordering::Relaxed),
            "level": record.level,
            "target": target,
            "event": event,
            "pid": std::process::id(),
            "thread": format!("{:?}", std::thread::current().id()),
            "fields": fields,
        });
        let mut line = serde_json::to_vec(&record).map_err(|e| format!("序列化日志失败: {e}"))?;
        if line.len() > MAX_EVENT_BYTES {
            record["fields"] = json!({
                "truncated": true,
                "originalEventBytes": line.len(),
            });
            line = serde_json::to_vec(&record).map_err(|e| format!("序列化截断日志失败: {e}"))?;
        }
        line.push(b'\n');
        Ok((date, now.date_naive(), line))
    }

    fn write_records(&self, records: Vec<PendingRecord>) -> Result<(), String> {
        let encoded = records
            .into_iter()
            .map(Self::encode_record)
            .collect::<Result<Vec<_>, _>>()?;
        if encoded.is_empty() {
            return Ok(());
        }
        let mut guard = self
            .state
            .lock()
            .map_err(|_| "日志写入锁已损坏".to_string())?;
        for (date, date_naive, line) in encoded {
            let needs_new_day =
                guard.as_ref().map(|state| state.date.as_str()) != Some(date.as_str());
            if needs_new_day {
                self.cleanup_old_files(date_naive)?;
                *guard = Some(self.open_state(&date)?);
            }
            let state = guard.as_mut().ok_or("日志文件未初始化")?;
            if state.size + line.len() as u64 >= self.max_bytes {
                let next_part = state.part + 1;
                let path = self.file_path(&date, next_part);
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| format!("创建日志分卷失败 {}: {e}", path.display()))?;
                crate::paths::secure_file(&path)
                    .map_err(|e| format!("设置日志分卷权限失败 {}: {e}", path.display()))?;
                *state = WriterState {
                    date: date.clone(),
                    part: next_part,
                    size: 0,
                    file,
                };
            }
            state
                .file
                .write_all(&line)
                .map_err(|e| format!("写入日志失败: {e}"))?;
            state.size += line.len() as u64;
        }
        guard
            .as_mut()
            .ok_or("日志文件未初始化")?
            .file
            .flush()
            .map_err(|e| format!("刷新日志失败: {e}"))?;
        Ok(())
    }
}

pub fn init() -> Result<PathBuf, String> {
    let dir = crate::paths::logs_dir();
    if LOGGER.get().is_none() {
        let logger = FileLogger::new(dir.clone())?;
        let _ = LOGGER.set(logger);
    }
    info(
        "logging.initialized",
        json!({
            "directory": dir.display().to_string(),
            "maxFileBytesExclusive": MAX_LOG_BYTES,
            "retentionDays": RETENTION_DAYS,
            "format": "jsonl",
        }),
    );
    Ok(dir)
}

pub fn log(level: &str, event: &str, target: &str, fields: Value) {
    let level = normalize_level(level);
    let event = normalize_event(event);
    if let Some(logger) = LOGGER.get() {
        if let Err(error) = logger.write_record(level, event, target, fields) {
            eprintln!("[logging] {error}");
        }
    }
}

fn normalize_level(level: &str) -> &'static str {
    match level.to_ascii_uppercase().as_str() {
        "DEBUG" => "DEBUG",
        "WARN" => "WARN",
        "ERROR" => "ERROR",
        _ => "INFO",
    }
}

fn normalize_event(event: &str) -> &str {
    if event.trim().is_empty() {
        "unnamed"
    } else {
        event
    }
}

pub fn debug(event: &str, fields: Value) {
    log("DEBUG", event, "backend", fields);
}

pub fn info(event: &str, fields: Value) {
    log("INFO", event, "backend", fields);
}

pub fn warn(event: &str, fields: Value) {
    log("WARN", event, "backend", fields);
}

pub fn error(event: &str, fields: Value) {
    log("ERROR", event, "backend", fields);
}

pub fn client_batch(entries: Vec<(String, String, Value)>) {
    let records = entries
        .into_iter()
        .map(|(level, event, fields)| PendingRecord {
            level: normalize_level(&level).to_string(),
            event: normalize_event(&event).to_string(),
            target: "frontend".to_string(),
            fields,
        })
        .collect();
    if let Some(logger) = LOGGER.get() {
        if let Err(error) = logger.write_records(records) {
            eprintln!("[logging] {error}");
        }
    }
}

pub fn text_stats(text: &str) -> Value {
    json!({
        "chars": text.chars().count(),
        "bytes": text.len(),
        "md5": format!("{:x}", md5::compute(text.as_bytes())),
    })
}

pub fn safe_url(raw: &str) -> String {
    reqwest::Url::parse(raw)
        .map(|url| {
            let host = url.host_str().unwrap_or_default();
            let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
            format!("{}://{}{}{}", url.scheme(), host, port, url.path())
        })
        .unwrap_or_else(|_| "<invalid-url>".to_string())
}

pub fn error_text(value: impl ToString) -> String {
    let value = truncate(&value.to_string(), 2_000);
    if contains_credential_assignment(&value) {
        "敏感错误详情已隐藏".into()
    } else {
        redact_error(&value)
    }
}

/// A credential label is sensitive only when used as an assignment, so an
/// ordinary diagnostic such as "Authorization failed" stays useful. Values
/// may be JSON quoted or separated by `:` / `=` and comparison is ASCII case
/// insensitive because upstream providers do not standardize field casing.
fn contains_credential_assignment(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    const FIELDS: &[&str] = &[
        "authorization",
        "api_key",
        "api-key",
        "apikey",
        "token",
        "access_token",
        "access-token",
        "accesstoken",
        "refresh_token",
        "refresh-token",
        "refreshtoken",
        "auth_token",
        "auth-token",
        "authtoken",
        "bearer_token",
        "bearer-token",
        "bearertoken",
        "access_key",
        "access-key",
        "accesskey",
        "client_secret",
        "client-secret",
        "clientsecret",
        "secret",
        "secret_key",
        "secret-key",
        "secretkey",
        "private_key",
        "private-key",
        "privatekey",
        "password",
    ];
    FIELDS.iter().any(|field| {
        lower.match_indices(field).any(|(offset, _)| {
            let before = lower[..offset].chars().next_back();
            if before.is_some_and(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            }) {
                return false;
            }
            let after = lower[offset + field.len()..].trim_start();
            let after = after
                .strip_prefix('"')
                .or_else(|| after.strip_prefix('\''))
                .unwrap_or(after)
                .trim_start();
            matches!(after.as_bytes().first(), Some(b':' | b'='))
        })
    })
}

fn redact_error(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &value[i..];
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("bearer ") {
            output.push_str(&rest[..7]);
            output.push_str("<redacted>");
            i += 7;
            while i < bytes.len()
                && !bytes[i].is_ascii_whitespace()
                && !matches!(bytes[i], b'"' | b'\'')
            {
                i += 1;
            }
            continue;
        }
        if lower.starts_with("sk-") {
            output.push_str(&rest[..3]);
            output.push_str("<redacted>");
            i += 3;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'_' | b'-'))
            {
                i += 1;
            }
            continue;
        }
        let Some(character) = rest.chars().next() else {
            break;
        };
        output.push(character);
        i += character.len_utf8();
    }
    output
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let text: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{text}…")
    } else {
        text
    }
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['-', '_'], "");
    if [
        "authorization",
        "password",
        "secret",
        "apikey",
        "accesskey",
        "privatekey",
    ]
    .iter()
    .any(|needle| key.contains(needle))
    {
        return true;
    }
    [
        "token",
        "accesstoken",
        "refreshtoken",
        "authtoken",
        "bearertoken",
    ]
    .contains(&key.as_str())
        || [
            "prompt",
            "system",
            "content",
            "text",
            "user",
            "data",
            "input",
            "result",
            "response",
            "body",
            "database64",
            "bindvalues",
        ]
        .contains(&key.as_str())
}

fn sanitize_value(value: Value, key: Option<&str>) -> Value {
    if key.is_some_and(sensitive_key) {
        return match value {
            Value::String(value) => json!({ "redacted": true, "chars": value.chars().count() }),
            Value::Array(value) => json!({ "redacted": true, "items": value.len() }),
            Value::Object(value) => json!({ "redacted": true, "keys": value.len() }),
            _ => Value::String("<redacted>".into()),
        };
    }
    match value {
        Value::String(value) => Value::String(truncate(&value, 2_048)),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .take(100)
                .map(|value| sanitize_value(value, None))
                .collect(),
        ),
        Value::Object(values) => {
            let mut output = Map::new();
            for (key, value) in values.into_iter().take(100) {
                output.insert(key.clone(), sanitize_value(value, Some(&key)));
            }
            Value::Object(output)
        }
        other => other,
    }
}

pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let location = panic.location().map(|location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        });
        let message = panic
            .payload()
            .downcast_ref::<&str>()
            .map(|value| (*value).to_string())
            .or_else(|| panic.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic payload".to_string());
        error(
            "application.panic",
            json!({ "message": message, "location": location }),
        );
        previous(panic);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("image-client-log-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn rotates_before_reaching_size_limit() {
        let dir = test_dir("rotate");
        let logger = FileLogger::with_limits(dir.clone(), 700, 10).unwrap();
        for index in 0..20 {
            logger
                .write_record(
                    "INFO",
                    "test.event",
                    "test",
                    json!({ "index": index, "value": "x".repeat(100) }),
                )
                .unwrap();
        }
        let files: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        assert!(files.len() > 1);
        assert!(files
            .iter()
            .all(|entry| entry.metadata().unwrap().len() < 700));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn removes_logs_older_than_retention_window() {
        let dir = test_dir("retention");
        fs::create_dir_all(&dir).unwrap();
        let today = Local::now().date_naive();
        let old = today - ChronoDuration::days(20);
        let recent = today - ChronoDuration::days(2);
        fs::write(
            dir.join(format!("{LOG_PREFIX}{}.log", old.format("%Y-%m-%d"))),
            b"old",
        )
        .unwrap();
        fs::write(
            dir.join(format!("{LOG_PREFIX}{}.log", recent.format("%Y-%m-%d"))),
            b"recent",
        )
        .unwrap();
        let logger = FileLogger::with_limits(dir.clone(), 1_024, 10).unwrap();
        logger.cleanup_old_files(today).unwrap();
        assert!(!dir
            .join(format!("{LOG_PREFIX}{}.log", old.format("%Y-%m-%d")))
            .exists());
        assert!(dir
            .join(format!("{LOG_PREFIX}{}.log", recent.format("%Y-%m-%d")))
            .exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn writes_batch_without_losing_entries() {
        let dir = test_dir("batch");
        let logger = FileLogger::with_limits(dir.clone(), 10_000, 10).unwrap();
        logger
            .write_records(
                (0..25)
                    .map(|index| PendingRecord {
                        level: "INFO".into(),
                        event: "batch.event".into(),
                        target: "test".into(),
                        fields: json!({ "index": index }),
                    })
                    .collect(),
            )
            .unwrap();
        let content = std::fs::read_to_string(
            std::fs::read_dir(&dir)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
        )
        .unwrap();
        assert_eq!(content.lines().count(), 25);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn redacts_sensitive_fields() {
        let value = sanitize_value(
            json!({ "apiKey": "secret", "prompt": "private text", "model": "safe-model" }),
            None,
        );
        assert_eq!(value["model"], "safe-model");
        assert_eq!(value["apiKey"]["redacted"], true);
        assert_eq!(value["prompt"]["chars"], 12);
        let usage = sanitize_value(json!({ "inputTokens": 12, "totalTokens": 20 }), None);
        assert_eq!(usage["inputTokens"], 12);
        let sql = sanitize_value(
            json!({ "query": "SELECT 1", "bindValues": ["secret"] }),
            None,
        );
        assert_eq!(sql["query"], "SELECT 1");
        assert_eq!(sql["bindValues"]["redacted"], true);
        assert_eq!(
            error_text("request failed: Bearer abc123 sk-live-secret"),
            "request failed: Bearer <redacted> sk-<redacted>"
        );
        assert_eq!(
            error_text("upstream rejected api_key=TOPSECRET"),
            "敏感错误详情已隐藏"
        );
        assert_eq!(
            error_text("Authorization failed for configured provider"),
            "Authorization failed for configured provider"
        );
        for field in [
            "token",
            "refresh_token",
            "auth_token",
            "bearer_token",
            "access_key",
            "client_secret",
            "secret",
        ] {
            assert_eq!(
                error_text(format!("provider rejected {field}=TOPSECRET")),
                "敏感错误详情已隐藏",
                "must redact {field} assignments"
            );
        }
        assert_eq!(error_text("secret not found"), "secret not found");
        assert_eq!(
            error_text("provider response: {'client_secret': 'TOPSECRET'}"),
            "敏感错误详情已隐藏"
        );
        assert_eq!(
            error_text("request failed: BEARER TOPSECRET SK-LIVE-SECRET"),
            "request failed: BEARER <redacted> SK-<redacted>"
        );
    }
}
