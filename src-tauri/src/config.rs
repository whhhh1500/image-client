use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::model::{IMAGE_MODELS, LLM_MODELS, VIDEO_MODELS};

pub const DEFAULT_OUTPUT_DIR: &str = "$DEFAULT_ASSETS";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigStatus {
    pub image_ready: bool,
    pub video_ready: bool,
    pub llm_ready: bool,
    pub image_api_url: Option<String>,
    pub video_api_url: Option<String>,
    pub image_model: String,
    pub video_model: String,
    pub llm_model: String,
    pub output_dir: String,
    pub source: String, // "db" | "env" | "api" | "none"
}

/// API configuration. One model per group (image / video / llm). Keys are held
/// server-side; only URLs, ready flags and the selected model are exposed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigState {
    pub image_api_url: String,
    pub image_api_key: String,
    pub image_model: String,
    pub video_api_url: String,
    pub video_api_key: String,
    pub video_model: String,
    pub llm_api_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub output_dir: String,
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    "none".into()
}

fn env(name: &str) -> String {
    let upper = name.to_uppercase().replace('-', "_");
    std::env::var(name)
        .or_else(|_| std::env::var(&upper))
        .unwrap_or_default()
}

impl ConfigState {
    /// Fallback load from `.env`. The source of truth is the SQLite `settings`
    /// table, which the frontend reads and pushes to Rust via `save_config`.
    pub fn load() -> Self {
        let persisted_path = backend_config_path();
        if persisted_path.exists() {
            match load_from_path(&persisted_path) {
                Ok(mut config) => {
                    config.output_dir = normalize_output_setting(&config.output_dir);
                    return config;
                }
                Err(error) => crate::logging::warn(
                    "configuration.backend_snapshot_invalid",
                    serde_json::json!({ "path": persisted_path.display().to_string(), "error": error }),
                ),
            }
        }
        if let Some(path) = find_env_file() {
            let _ = dotenvy::from_path(&path);
        }
        let image_model = env("image-api-model");
        let video_model = env("video-api-model");
        let llm_model = env("llm-api-model");
        let mut cfg = Self {
            image_api_url: env("image-api-url"),
            image_api_key: env("image-api-key"),
            image_model: if image_model.is_empty() {
                "gpt-image-2".into()
            } else {
                image_model
            },
            video_api_url: env("video-api-url"),
            video_api_key: env("video-api-key"),
            video_model: if video_model.is_empty() {
                "kling-video-v3".into()
            } else {
                video_model
            },
            llm_api_url: env("llm-api-url"),
            llm_api_key: env("llm-api-key"),
            llm_model: if llm_model.is_empty() {
                "gemini-3.7-flash".into()
            } else {
                llm_model
            },
            output_dir: {
                let v = env("output-dir");
                if v.is_empty() {
                    DEFAULT_OUTPUT_DIR.to_string()
                } else {
                    normalize_output_setting(&v)
                }
            },
            source: "env".into(),
        };
        if cfg.image_api_url.is_empty() && cfg.video_api_url.is_empty() {
            cfg.source = "none".into();
        }
        cfg
    }

    pub fn status(&self) -> ConfigStatus {
        ConfigStatus {
            image_ready: !self.image_api_url.is_empty() && !self.image_api_key.is_empty(),
            video_ready: !self.video_api_url.is_empty() && !self.video_api_key.is_empty(),
            llm_ready: !self.llm_api_url.is_empty() && !self.llm_api_key.is_empty(),
            image_api_url: if self.image_api_url.is_empty() {
                None
            } else {
                Some(self.image_api_url.clone())
            },
            video_api_url: if self.video_api_url.is_empty() {
                None
            } else {
                Some(self.video_api_url.clone())
            },
            image_model: self.image_model.clone(),
            video_model: self.video_model.clone(),
            llm_model: self.llm_model.clone(),
            output_dir: self.output_path().display().to_string(),
            source: self.source.clone(),
        }
    }

    /// Persist the backend snapshot so REST and early-start operations survive restarts.
    /// The frontend SQLite settings remain the editable source and will re-apply on startup.
    pub fn persist_backend(&self) -> Result<(), String> {
        persist_to_path(self, &backend_config_path())
    }

    pub fn output_path(&self) -> PathBuf {
        resolve_output_path(&self.output_dir)
    }
}

fn normalize_output_setting(raw: &str) -> String {
    let value = raw.trim();
    if value.is_empty()
        || value == DEFAULT_OUTPUT_DIR
        || crate::paths::assets_dir() == Path::new(value)
    {
        return DEFAULT_OUTPUT_DIR.to_string();
    }
    if looks_foreign_absolute(value, cfg!(windows)) {
        crate::logging::warn(
            "configuration.foreign_output_path",
            serde_json::json!({ "configuredPath": value, "fallback": crate::paths::assets_dir().display().to_string() }),
        );
        return DEFAULT_OUTPUT_DIR.to_string();
    }
    value.to_string()
}

fn resolve_output_path(raw: &str) -> PathBuf {
    let value = normalize_output_setting(raw);
    if value == DEFAULT_OUTPUT_DIR {
        return crate::paths::assets_dir();
    }
    if let Some(relative) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        // Expand against the real user home, not the data dir's parent: an
        // IMAGE_CLIENT_DATA_DIR override must not redirect `~/...` paths.
        return dirs::home_dir()
            .unwrap_or_else(|| crate::paths::data_dir())
            .join(relative);
    }
    let path = PathBuf::from(&value);
    if path.is_absolute() {
        path
    } else {
        crate::paths::data_dir().join(path)
    }
}

fn looks_foreign_absolute(value: &str, windows: bool) -> bool {
    let bytes = value.as_bytes();
    let windows_absolute = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    if windows {
        value.starts_with('/') && !value.starts_with("//")
    } else {
        windows_absolute
    }
}

fn backend_config_path() -> PathBuf {
    crate::paths::data_dir().join("backend-config.json")
}

fn load_from_path(path: &Path) -> Result<ConfigState, String> {
    if std::fs::metadata(path)
        .map(|metadata| metadata.len() > 1024 * 1024)
        .unwrap_or(false)
    {
        return Err("后端配置文件超过 1 MiB 大小限制".into());
    }
    let bytes = std::fs::read(path).map_err(|error| format!("读取后端配置失败: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("解析后端配置失败: {error}"))
}

fn persist_to_path(config: &ConfigState, path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or("后端配置路径缺少父目录")?;
    std::fs::create_dir_all(parent).map_err(|error| format!("创建配置目录失败: {error}"))?;
    let temp = parent.join(format!(".backend-config-{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(config)
        .map_err(|error| format!("序列化后端配置失败: {error}"))?;
    std::fs::write(&temp, bytes).map_err(|error| format!("写入临时配置失败: {error}"))?;
    crate::paths::secure_file(&temp).map_err(|error| format!("设置临时配置权限失败: {error}"))?;

    if !path.exists() {
        let result = std::fs::rename(&temp, path).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("提交后端配置失败: {error}")
        });
        if result.is_ok() {
            crate::paths::secure_file(path)
                .map_err(|error| format!("设置后端配置权限失败: {error}"))?;
        }
        return result;
    }

    // `std::fs::rename` replaces an existing file on Windows (MoveFileExW with
    // MOVEFILE_REPLACE_EXISTING), so the previous backup dance only introduced a
    // window in which no config file existed at all.
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("提交后端配置失败: {error}")
    })?;
    crate::paths::secure_file(path).map_err(|error| format!("设置后端配置权限失败: {error}"))?;
    Ok(())
}

/// Derive the `/v1/models` URL from an API URL like `.../v1/images/generations`.
pub fn models_endpoint(api_url: &str) -> Option<String> {
    if api_url.is_empty() {
        return None;
    }
    let idx = api_url.find("/v1/")?;
    let head = &api_url[..idx + "/v1/".len()];
    Some(format!("{head}models"))
}

#[allow(dead_code)]
pub fn known_image_models() -> Vec<String> {
    IMAGE_MODELS.iter().map(|s| s.to_string()).collect()
}

#[allow(dead_code)]
pub fn known_video_models() -> Vec<String> {
    VIDEO_MODELS.iter().map(|s| s.to_string()).collect()
}

pub fn known_llm_models() -> Vec<String> {
    LLM_MODELS.iter().map(|s| s.to_string()).collect()
}

fn find_env_file() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(".env"));
        candidates.push(cwd.join("../.env"));
        candidates.push(cwd.join("../../.env"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(".env"));
            candidates.push(dir.join("../../.env"));
            candidates.push(dir.join("../../../.env"));
        }
    }
    candidates.into_iter().find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_and_loads_backend_configuration_without_source_field() {
        let dir =
            std::env::temp_dir().join(format!("image-client-config-{}", uuid::Uuid::new_v4()));
        let path = dir.join("backend-config.json");
        let config = ConfigState {
            image_api_url: "https://example.com/v1/images/generations".into(),
            image_api_key: "image-secret".into(),
            image_model: "image-model".into(),
            video_api_url: "https://example.com/v1/videos".into(),
            video_api_key: "video-secret".into(),
            video_model: "video-model".into(),
            llm_api_url: "https://example.com/v1".into(),
            llm_api_key: "llm-secret".into(),
            llm_model: "llm-model".into(),
            output_dir: "output".into(),
            source: "db".into(),
        };
        persist_to_path(&config, &path).unwrap();
        let loaded = load_from_path(&path).unwrap();
        assert_eq!(loaded.llm_model, "llm-model");
        assert_eq!(loaded.llm_api_key, "llm-secret");
        assert_eq!(loaded.source, "db");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"source\": \"db\""));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolves_portable_and_foreign_output_paths() {
        assert_eq!(normalize_output_setting(""), DEFAULT_OUTPUT_DIR);
        assert_eq!(
            resolve_output_path(DEFAULT_OUTPUT_DIR),
            crate::paths::assets_dir()
        );
        assert_eq!(
            resolve_output_path("exports"),
            crate::paths::data_dir().join("exports")
        );
        assert!(looks_foreign_absolute(r"C:\\Users\\Alice\\output", false));
        assert!(looks_foreign_absolute("/Users/alice/output", true));
    }
}
