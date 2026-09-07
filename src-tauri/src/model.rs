use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRef {
    pub id: String,
    pub kind: String, // "image" | "video" | "text"
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIn {
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunNodeRequest {
    pub node_type: String,
    pub category: String,
    pub config: serde_json::Value,
    #[serde(default)]
    pub input_assets: Vec<AssetIn>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResult {
    pub assets: Vec<AssetRef>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub capabilities: Vec<String>,
}

/// Known image models (fallback when the gateway `/v1/models` can't be read).
pub const IMAGE_MODELS: &[&str] = &[
    "gpt-image-2",
    "gemini-3.1-flash-image",
    "gemini-2.5-flash-image",
    "gemini-3-pro-image",
    "gemini-2.5-flash-image-preview",
    "gemini-3-pro-image-preview",
    "gemini-3.1-flash-image-preview",
    "grok-imagine-image",
];

/// Offline video model catalogue used only before a video API is configured.
/// Once configured, `/v1/models` is authoritative and failures are surfaced.
pub const VIDEO_MODELS: &[&str] = &[
    "video-ds-2.5",
    "video-ds-2.5-480",
    "minimax-h3",
    "minimax-h3-2k",
    "minimax-h3-4k",
    "grok-imagine-video",
    "grok-imagine-video-1.5-preview",
    "kling-video-v3",
    "kling-video-v3-omni",
    "kling-video-v3-turbo",
    "drama-video-v2",
    "drama-video-v2-fast",
    "seedance2.5",
    "as-sd2.0-fast",
    "video-ds-2.0",
    "video-ds-2.0-fast",
    "wan3-720p",
];

pub const LLM_MODELS: &[&str] = &[
    "gemini-3.7-flash",
    "gemini-3.6-flash",
    "gemini-3.5-flash",
    "gemini-3-flash",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
    "gemini-3.1-flash-lite",
    "gemini-2.5-pro",
];
