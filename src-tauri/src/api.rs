use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
#[cfg(not(test))]
use tauri::Emitter;
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

use crate::agent_prompts;
use crate::commands;
use crate::config::ConfigState;
use crate::gateway;
use crate::model::RunNodeRequest;
use crate::providers::ProviderRegistry;
use crate::video::{self, VideoGenRequest};

#[derive(Clone)]
pub struct ApiState {
    #[cfg(not(test))]
    pub app: Option<tauri::AppHandle>,
    pub cfg: Arc<RwLock<ConfigState>>,
    pub registry: Arc<ProviderRegistry>,
    pub history_sync: Arc<crate::history::HistorySyncState>,
}

const API_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub(crate) struct ApiServerControl {
    inner: Arc<ApiServerControlInner>,
}

struct ApiServerControlInner {
    phase: Mutex<ApiServerPhase>,
    shutdown_sender: Mutex<Option<oneshot::Sender<()>>>,
    shutdown_receiver: Mutex<Option<oneshot::Receiver<()>>>,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApiServerPhase {
    Starting,
    Running,
    Draining,
    Exiting,
    RestartBypass,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitRequestAction {
    PreventAndDrain,
    PreventWhileDraining,
    AllowExit,
    BestEffortRestart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApiDrainResult {
    Graceful,
    TimedOut,
    TaskFailed,
    NoTask,
}

fn exit_request_action(
    phase: ApiServerPhase,
    is_restart: bool,
) -> (ApiServerPhase, ExitRequestAction) {
    if is_restart {
        return (
            ApiServerPhase::RestartBypass,
            ExitRequestAction::BestEffortRestart,
        );
    }
    match phase {
        ApiServerPhase::Starting | ApiServerPhase::Running => {
            (ApiServerPhase::Draining, ExitRequestAction::PreventAndDrain)
        }
        ApiServerPhase::Draining => (
            ApiServerPhase::Draining,
            ExitRequestAction::PreventWhileDraining,
        ),
        ApiServerPhase::Exiting | ApiServerPhase::Stopped | ApiServerPhase::RestartBypass => {
            (phase, ExitRequestAction::AllowExit)
        }
    }
}

impl ApiServerControl {
    pub(crate) fn new() -> Self {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        Self {
            inner: Arc::new(ApiServerControlInner {
                phase: Mutex::new(ApiServerPhase::Starting),
                shutdown_sender: Mutex::new(Some(shutdown_sender)),
                shutdown_receiver: Mutex::new(Some(shutdown_receiver)),
                task: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn set_task(&self, task: tauri::async_runtime::JoinHandle<()>) {
        if let Ok(mut stored) = self.inner.task.lock() {
            *stored = Some(task);
        }
    }

    pub(crate) fn request_exit(&self, is_restart: bool) -> ExitRequestAction {
        let mut phase = self
            .inner
            .phase
            .lock()
            .expect("API server phase lock poisoned");
        let (next_phase, action) = exit_request_action(*phase, is_restart);
        *phase = next_phase;
        action
    }

    pub(crate) fn signal_shutdown(&self) {
        if let Ok(mut sender) = self.inner.shutdown_sender.lock() {
            if let Some(sender) = sender.take() {
                let _ = sender.send(());
            }
        }
    }

    pub(crate) async fn drain_for_exit(&self) -> ApiDrainResult {
        self.signal_shutdown();
        let task = self.inner.task.lock().ok().and_then(|mut task| task.take());
        let result = match task {
            Some(mut task) => match tokio::time::timeout(API_SHUTDOWN_TIMEOUT, &mut task).await {
                Ok(Ok(())) => ApiDrainResult::Graceful,
                Ok(Err(_)) => ApiDrainResult::TaskFailed,
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    ApiDrainResult::TimedOut
                }
            },
            None => ApiDrainResult::NoTask,
        };
        if matches!(result, ApiDrainResult::TimedOut) {
            crate::logging::warn(
                "api.server.shutdown_timeout",
                json!({ "timeoutMs": API_SHUTDOWN_TIMEOUT.as_millis() }),
            );
        }
        if let Ok(mut phase) = self.inner.phase.lock() {
            *phase = ApiServerPhase::Exiting;
        }
        result
    }

    fn take_shutdown_receiver(&self) -> Option<oneshot::Receiver<()>> {
        self.inner
            .shutdown_receiver
            .lock()
            .ok()
            .and_then(|mut receiver| receiver.take())
    }

    fn mark_running(&self) {
        if let Ok(mut phase) = self.inner.phase.lock() {
            if *phase == ApiServerPhase::Starting {
                *phase = ApiServerPhase::Running;
            }
        }
    }

    fn mark_bind_failed(&self) {
        if let Ok(mut phase) = self.inner.phase.lock() {
            *phase = ApiServerPhase::Stopped;
        }
    }

    fn mark_server_finished(&self) {
        if let Ok(mut phase) = self.inner.phase.lock() {
            if matches!(*phase, ApiServerPhase::Starting | ApiServerPhase::Running) {
                *phase = ApiServerPhase::Stopped;
            }
        }
    }

    fn is_draining(&self) -> bool {
        self.inner
            .phase
            .lock()
            .is_ok_and(|phase| *phase == ApiServerPhase::Draining)
    }
}

pub async fn start(
    control: ApiServerControl,
    _app: tauri::AppHandle,
    cfg: Arc<RwLock<ConfigState>>,
    registry: Arc<ProviderRegistry>,
    history_sync: Arc<crate::history::HistorySyncState>,
) {
    let state = ApiState {
        #[cfg(not(test))]
        app: Some(_app),
        cfg,
        registry,
        history_sync,
    };
    let router = build_router(state);
    let host = std::env::var("API_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("API_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8123);
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        crate::logging::warn(
            "api.server.non_loopback_binding",
            json!({ "host": host, "authentication": "none", "risk": "network_exposure" }),
        );
    }
    let addr = format!("{host}:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(error) => {
            control.mark_bind_failed();
            crate::logging::error(
                "api.server.bind_failed",
                json!({ "address": addr, "error": error.to_string() }),
            );
            return;
        }
    };
    crate::logging::info(
        "api.server.started",
        json!({ "address": addr, "basePath": "/api/v1", "auth": "none" }),
    );
    control.mark_running();
    let receiver = control.take_shutdown_receiver();
    let result = match receiver {
        Some(receiver) => serve_listener(listener, router, receiver).await,
        None => Err(std::io::Error::other("API shutdown receiver unavailable")),
    };
    match result {
        Ok(()) if control.is_draining() => {
            crate::logging::info("api.server.stopped", json!({ "reason": "graceful" }));
        }
        Ok(()) => {
            crate::logging::warn(
                "api.server.stopped",
                json!({ "reason": "unexpected_return" }),
            );
        }
        Err(error) => {
            crate::logging::error(
                "api.server.stopped",
                json!({ "reason": "serve_error", "error": error.to_string() }),
            );
        }
    }
    control.mark_server_finished();
}

async fn serve_listener(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: oneshot::Receiver<()>,
) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = shutdown.await;
        })
        .await
}

#[cfg(not(test))]
fn emit_history_changed(state: &ApiState, operation: &str, asset_ids: &[String]) {
    let change = state.history_sync.publish(operation, asset_ids);
    if let Some(app) = &state.app {
        if let Err(error) = app.emit("history://changed", &change) {
            crate::logging::warn(
                "history.change_event_failed",
                json!({ "operation": operation, "error": error.to_string() }),
            );
        }
    }
}

#[cfg(test)]
fn emit_history_changed(state: &ApiState, operation: &str, asset_ids: &[String]) {
    state.history_sync.publish(operation, asset_ids);
}

fn build_router(state: ApiState) -> Router {
    let versioned = Router::new()
        .route("/health", get(health))
        .route("/system/info", get(system_info))
        .route("/system/config", get(config).put(save_config_api))
        .route("/system/llm", get(llm_config).put(save_llm_config))
        .route("/system/provider", axum::routing::put(set_provider))
        .route("/system/logs", get(logs_info))
        .route("/system/history-sync", get(history_sync_status))
        .route("/catalog/tools", get(tools))
        .route("/catalog/models", get(models))
        .route("/catalog/models/image", get(image_models))
        .route("/catalog/models/video", get(video_models))
        .route("/catalog/models/llm", get(llm_models))
        .route("/catalog/providers", get(providers))
        .route("/text/completions", post(text))
        .route("/agents/director/runs", post(director))
        .route("/agents/writer/runs", post(script_step))
        .route("/agents/storyboard/runs", post(storyboard_step))
        .route("/agents/consistency/runs", post(consistency_step))
        .route("/agents/qc/runs", post(qc_review))
        .route("/agents/orchestrations", post(orchestrate))
        .route("/media/images/generations", post(image))
        .route("/media/videos/generations", post(video))
        .route("/assets/text", post(save_text_asset))
        .route("/assets/media", post(save_media_asset));

    // Legacy aliases remain for existing automation. New integrations should use /api/v1.
    let legacy = Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(config).post(save_config_api))
        .route("/api/tools", get(tools))
        .route("/api/text", post(text))
        .route("/api/director", post(director))
        .route("/api/script", post(script_step))
        .route("/api/storyboard", post(storyboard_step))
        .route("/api/consistency", post(consistency_step))
        .route("/api/qc_review", post(qc_review))
        .route("/api/image", post(image))
        .route("/api/video", post(video));

    Router::new()
        .nest("/api/v1", versioned)
        .merge(legacy)
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(cors_layer())
        .layer(middleware::from_fn(request_log))
        .with_state(state)
}

fn cors_layer() -> CorsLayer {
    let configured = std::env::var("API_CORS_ORIGINS").unwrap_or_default();
    let mut origins = vec![
        "http://localhost:1420".to_string(),
        "http://127.0.0.1:1420".to_string(),
        "http://tauri.localhost".to_string(),
        "tauri://localhost".to_string(),
    ];
    origins.extend(
        configured
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(str::to_string),
    );
    let origins: Vec<HeaderValue> = origins
        .into_iter()
        .filter_map(|origin| HeaderValue::from_str(&origin).ok())
        .collect();
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::OPTIONS])
        .allow_headers(Any)
}

async fn request_log(request: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let method = request.method().to_string();
    let uri = request.uri().path().to_string();
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_string());
    let content_length = request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    crate::logging::info(
        "api.request.start",
        json!({ "requestId": request_id, "method": method, "path": uri, "route": matched, "contentLength": content_length }),
    );
    let started = Instant::now();
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    crate::logging::info(
        "api.request.end",
        json!({
            "requestId": request_id,
            "method": method,
            "path": uri,
            "route": matched,
            "status": response.status().as_u16(),
            "durationMs": started.elapsed().as_millis(),
        }),
    );
    response
}

type ApiError = (StatusCode, Json<Value>);

fn err(message: String) -> ApiError {
    let message = crate::logging::error_text(&message);
    crate::logging::warn("api.operation.error", json!({ "error": message }));
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": { "code": "bad_request", "message": message } })),
    )
}

const MAX_TEXT_INPUT_CHARS: usize = commands::MAX_AGENT_TEXT_CHARS;
const MAX_PROMPT_CHARS: usize = 200_000;
const MAX_MODEL_CHARS: usize = commands::MAX_AGENT_MODEL_CHARS;

fn required_text(value: &str, field: &str, max: usize) -> Result<(), ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(err(format!("{field} 不能为空")));
    }
    if trimmed.chars().count() > max {
        return Err(err(format!("{field} 超过 {max} 个字符")));
    }
    Ok(())
}

fn optional_text(value: Option<&str>, field: &str, max: usize) -> Result<(), ApiError> {
    if let Some(value) = value {
        if value.chars().count() > max {
            return Err(err(format!("{field} 超过 {max} 个字符")));
        }
    }
    Ok(())
}

fn validate_step_request(body: &StepReq) -> Result<(), ApiError> {
    required_text(&body.input, "input", MAX_TEXT_INPUT_CHARS)?;
    optional_text(body.system.as_deref(), "system", MAX_TEXT_INPUT_CHARS)?;
    optional_text(body.model.as_deref(), "model", MAX_MODEL_CHARS)
}

fn validate_text_request(body: &TextReq) -> Result<(), ApiError> {
    required_text(&body.system, "system", MAX_TEXT_INPUT_CHARS)?;
    required_text(&body.input, "input", MAX_TEXT_INPUT_CHARS)?;
    optional_text(body.model.as_deref(), "model", MAX_MODEL_CHARS)
}

async fn not_found() -> ApiError {
    (
        StatusCode::NOT_FOUND,
        Json(
            json!({ "error": { "code": "not_found", "message": "接口不存在，请读取 GET /api/v1/system/info" } }),
        ),
    )
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "image-client-api", "apiVersion": "v1" }))
}

async fn system_info() -> Json<Value> {
    Json(json!({
        "name": "image-client",
        "version": env!("CARGO_PKG_VERSION"),
        "apiVersion": "v1",
        "authentication": "none",
        "basePath": "/api/v1",
        "legacyBasePath": "/api",
        "domains": ["system", "catalog", "text", "agents", "media", "assets"],
        "historyPersistence": "sqlite",
        "historySynchronization": {
            "mode": "event_driven",
            "event": "history://changed",
            "polling": false,
            "manualRefresh": true,
        },
        "routes": route_catalog(),
    }))
}

async fn logs_info() -> Json<Value> {
    Json(json!({
        "directory": crate::paths::logs_dir().display().to_string(),
        "format": "jsonl",
        "dailyFiles": true,
        "maxFileBytesExclusive": 10 * 1024 * 1024,
        "retentionDays": 10,
    }))
}

async fn history_sync_status(State(state): State<ApiState>) -> Json<Value> {
    Json(state.history_sync.status())
}

fn route_catalog() -> Value {
    json!([
        {"method":"GET","path":"/api/v1/health","domain":"system","name":"health"},
        {"method":"GET","path":"/api/v1/system/info","domain":"system","name":"system_info"},
        {"method":"GET","path":"/api/v1/system/config","domain":"system","name":"get_config"},
        {"method":"PUT","path":"/api/v1/system/config","domain":"system","name":"update_config"},
        {"method":"GET","path":"/api/v1/system/llm","domain":"system","name":"get_llm_config"},
        {"method":"PUT","path":"/api/v1/system/llm","domain":"system","name":"update_llm_config"},
        {"method":"GET","path":"/api/v1/system/logs","domain":"system","name":"logs_info"},
        {"method":"GET","path":"/api/v1/system/history-sync","domain":"system","name":"history_sync_status"},
        {"method":"PUT","path":"/api/v1/system/provider","domain":"system","name":"set_active_provider"},
        {"method":"GET","path":"/api/v1/catalog/tools","domain":"catalog","name":"list_tools"},
        {"method":"GET","path":"/api/v1/catalog/models","domain":"catalog","name":"list_models"},
        {"method":"GET","path":"/api/v1/catalog/models/image","domain":"catalog","name":"list_image_models"},
        {"method":"GET","path":"/api/v1/catalog/models/video","domain":"catalog","name":"list_video_models"},
        {"method":"GET","path":"/api/v1/catalog/models/llm","domain":"catalog","name":"list_llm_models"},
        {"method":"GET","path":"/api/v1/catalog/providers","domain":"catalog","name":"list_providers"},
        {"method":"POST","path":"/api/v1/text/completions","domain":"text","name":"create_text_completion"},
        {"method":"POST","path":"/api/v1/agents/director/runs","domain":"agents","name":"run_director"},
        {"method":"POST","path":"/api/v1/agents/writer/runs","domain":"agents","name":"run_writer"},
        {"method":"POST","path":"/api/v1/agents/storyboard/runs","domain":"agents","name":"run_storyboard"},
        {"method":"POST","path":"/api/v1/agents/consistency/runs","domain":"agents","name":"run_consistency"},
        {"method":"POST","path":"/api/v1/agents/qc/runs","domain":"agents","name":"run_qc"},
        {"method":"POST","path":"/api/v1/agents/orchestrations","domain":"agents","name":"run_orchestration"},
        {"method":"POST","path":"/api/v1/media/images/generations","domain":"media","name":"generate_image"},
        {"method":"POST","path":"/api/v1/media/videos/generations","domain":"media","name":"generate_video_segments"},
        {"method":"POST","path":"/api/v1/assets/text","domain":"assets","name":"save_text_asset"},
        {"method":"POST","path":"/api/v1/assets/media","domain":"assets","name":"save_media_asset"}
    ])
}

async fn config(State(state): State<ApiState>) -> Json<Value> {
    let status = state.cfg.read().unwrap().status();
    Json(serde_json::to_value(status).unwrap_or(Value::Null))
}

async fn save_config_api(
    State(state): State<ApiState>,
    Json(config): Json<commands::SaveConfigRequest>,
) -> Result<Json<Value>, ApiError> {
    commands::validate_save_config(&config).map_err(err)?;
    let mut new = state.cfg.read().unwrap().clone();
    commands::merge_config(&mut new, &config);
    new.source = "api".into();
    new.persist_backend().map_err(err)?;
    *state.cfg.write().unwrap() = new.clone();
    let status = new.status();
    crate::logging::info(
        "configuration.updated",
        serde_json::to_value(&status).unwrap_or_default(),
    );
    Ok(Json(serde_json::to_value(status).unwrap_or(Value::Null)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LlmConfigReq {
    url: String,
    key: String,
    model: String,
}

fn validate_llm_config(body: &LlmConfigReq) -> Result<(), String> {
    let url =
        reqwest::Url::parse(body.url.trim()).map_err(|error| format!("LLM 地址无效: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("LLM 地址必须是 HTTP(S) 地址".into());
    }
    if body.key.trim().is_empty() || body.key.chars().count() > 16_384 {
        return Err("LLM Key 不能为空且不得超过 16384 个字符".into());
    }
    if body.model.trim().is_empty() || body.model.chars().count() > 200 {
        return Err("LLM 模型不能为空且不得超过 200 个字符".into());
    }
    Ok(())
}

async fn llm_config(State(state): State<ApiState>) -> Json<Value> {
    let cfg = state.cfg.read().unwrap();
    Json(json!({
        "ready": !cfg.llm_api_url.is_empty() && !cfg.llm_api_key.is_empty(),
        "url": if cfg.llm_api_url.is_empty() { Value::Null } else { json!(cfg.llm_api_url) },
        "model": cfg.llm_model,
        "source": cfg.source,
    }))
}

async fn save_llm_config(
    State(state): State<ApiState>,
    Json(body): Json<LlmConfigReq>,
) -> Result<Json<Value>, ApiError> {
    validate_llm_config(&body).map_err(err)?;
    let mut config = state.cfg.read().unwrap().clone();
    config.llm_api_url = body.url.trim().trim_end_matches('/').to_string();
    config.llm_api_key = body.key.trim().to_string();
    config.llm_model = body.model.trim().to_string();
    config.source = "api".into();
    config.persist_backend().map_err(err)?;
    *state.cfg.write().unwrap() = config.clone();
    crate::logging::info(
        "configuration.llm_updated",
        json!({
            "url": crate::logging::safe_url(&config.llm_api_url),
            "model": config.llm_model,
        }),
    );
    Ok(Json(
        json!({ "ready": true, "url": config.llm_api_url, "model": config.llm_model, "source": config.source }),
    ))
}

async fn image_models(State(state): State<ApiState>) -> Result<Json<Value>, ApiError> {
    let cfg = state.cfg.read().unwrap().clone();
    commands::list_image_models_with_config(&cfg)
        .await
        .map(|items| Json(json!({ "items": items })))
        .map_err(err)
}

async fn video_models(State(state): State<ApiState>) -> Result<Json<Value>, ApiError> {
    let cfg = state.cfg.read().unwrap().clone();
    let items = commands::list_video_models_with_config(&cfg)
        .await
        .map_err(err)?;
    Ok(Json(json!({
        "items": items,
        "capabilities": crate::video::model_capabilities(),
    })))
}

async fn llm_models() -> Json<Value> {
    Json(json!({ "items": crate::config::known_llm_models() }))
}

async fn providers(State(state): State<ApiState>) -> Json<Value> {
    Json(json!({ "items": state.registry.list() }))
}

#[derive(Deserialize)]
struct ProviderReq {
    id: String,
}

async fn set_provider(
    State(state): State<ApiState>,
    Json(body): Json<ProviderReq>,
) -> Result<Json<Value>, ApiError> {
    required_text(&body.id, "id", 100)?;
    state.registry.set_active(body.id).map_err(err)?;
    Ok(Json(json!({ "items": state.registry.list() })))
}

async fn models(State(state): State<ApiState>) -> Result<Json<Value>, ApiError> {
    let cfg = state.cfg.read().unwrap().clone();
    let image = commands::list_image_models_with_config(&cfg)
        .await
        .map_err(err)?;
    Ok(Json(json!({
        "image": image,
        "video": crate::config::known_video_models(),
        "llm": crate::config::known_llm_models(),
    })))
}

async fn tools() -> Json<Value> {
    let tool = |name: &str,
                description: &str,
                path: &str,
                properties: Value,
                required: Vec<&str>| {
        json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "endpoint": { "method": "POST", "path": path },
                "parameters": { "type": "object", "properties": properties, "required": required }
            }
        })
    };
    Json(json!({
        "name": "image-client",
        "apiVersion": "v1",
        "tools": [
            tool("director", "统筹导演", "/api/v1/agents/director/runs", json!({"input":{"type":"string"}}), vec!["input"]),
            tool("writer", "生成分场剧本", "/api/v1/agents/writer/runs", json!({"input":{"type":"string"}}), vec!["input"]),
            tool("storyboard", "生成 Markdown 视频分镜", "/api/v1/agents/storyboard/runs", json!({"input":{"type":"string"}}), vec!["input"]),
            tool("consistency", "建立固定锚定与剧情状态锚点", "/api/v1/agents/consistency/runs", json!({"input":{"type":"string"}}), vec!["input"]),
            tool("qc", "执行逐镜与内容质量检查", "/api/v1/agents/qc/runs", json!({"input":{"type":"string"}}), vec!["input"]),
            tool("generate_image", "生成图片", "/api/v1/media/images/generations", json!({"prompt":{"type":"string"},"referencePath":{"type":"string"},"model":{"type":"string"},"size":{"type":"string"}}), vec!["prompt"]),
            tool("generate_video", "生成视频分段", "/api/v1/media/videos/generations", json!({"prompt":{"type":"string"},"durationS":{"type":"integer"},"images":{"type":"array"},"videos":{"type":"array"},"audios":{"type":"array"},"model":{"type":"string"}}), vec!["prompt"])
        ]
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextReq {
    system: String,
    input: String,
    model: Option<String>,
}

async fn text(
    State(state): State<ApiState>,
    Json(body): Json<TextReq>,
) -> Result<Json<Value>, ApiError> {
    validate_text_request(&body)?;
    let cfg = state.cfg.read().unwrap().clone();
    if cfg.llm_api_url.is_empty() || cfg.llm_api_key.is_empty() {
        return Err(err("未配置文本 LLM".into()));
    }
    let url = format!("{}/chat/completions", cfg.llm_api_url.trim_end_matches('/'));
    let model = body.model.unwrap_or(cfg.llm_model.clone());
    crate::llm::complete_text(
        &url,
        &cfg.llm_api_key,
        &model,
        &body.system,
        &body.input,
        "rest_text_completion",
    )
    .await
    .map(|result| Json(json!({ "result": result })))
    .map_err(err)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StepReq {
    input: String,
    system: Option<String>,
    model: Option<String>,
    project_id: Option<String>,
}

async fn run_step(
    state: ApiState,
    body: StepReq,
    default_system: &'static str,
    operation: &'static str,
    agent_id: &'static str,
    label: &'static str,
    document_type: &'static str,
) -> Result<Json<Value>, ApiError> {
    validate_step_request(&body)?;
    optional_text(body.project_id.as_deref(), "projectId", 200)?;
    let cfg = state.cfg.read().unwrap().clone();
    if cfg.llm_api_url.is_empty() || cfg.llm_api_key.is_empty() {
        return Err(err("未配置文本 LLM".into()));
    }
    let url = format!("{}/chat/completions", cfg.llm_api_url.trim_end_matches('/'));
    let model = body.model.unwrap_or(cfg.llm_model.clone());
    let system = body.system.unwrap_or_else(|| default_system.to_string());
    let raw_result = crate::llm::complete_text(
        &url,
        &cfg.llm_api_key,
        &model,
        &system,
        &body.input,
        operation,
    )
    .await
    .map_err(err)?;
    let (result, normalized_reference_shots) = if agent_id == "storyboard" {
        agent_prompts::normalize_storyboard_reference_assets(&raw_result)
    } else {
        (raw_result, Vec::new())
    };
    agent_prompts::validate_agent_output(agent_id, &result).map_err(err)?;
    let asset = commands::save_text_with_config(&cfg, label, &result, Some(&model)).map_err(err)?;
    let now = chrono::Utc::now().timestamp_millis();
    let params_value = json!({
        "text": result,
        "title": label,
        "documentType": document_type,
        "documentId": Uuid::new_v4().to_string(),
        "version": 1,
        "changeType": "generated",
        "agentId": agent_id,
        "provenance": crate::history::generated_provenance(
            &body.input,
            &body.input,
            Some(&system),
            json!([]),
            json!([]),
        ),
        "updatedAt": now,
        "origin": "rest_api",
        "workflowStatus": "unreviewed",
        "normalization": {
            "correctedReferenceShots": normalized_reference_shots,
        },
    });
    crate::history::persist_assets(
        std::slice::from_ref(&asset),
        label,
        Some(&model),
        body.project_id.as_deref(),
        &params_value,
    )
    .map_err(err)?;
    emit_history_changed(&state, operation, std::slice::from_ref(&asset.id));
    Ok(Json(json!({
        "result": result,
        "asset": asset,
        "workflowStatus": "unreviewed",
        "normalization": { "correctedReferenceShots": normalized_reference_shots },
    })))
}

async fn director(
    State(state): State<ApiState>,
    Json(body): Json<StepReq>,
) -> Result<Json<Value>, ApiError> {
    run_step(
        state,
        body,
        agent_prompts::DIRECTOR,
        "rest_agent_director",
        "director",
        "导演规划",
        "director",
    )
    .await
}
async fn script_step(
    State(state): State<ApiState>,
    Json(body): Json<StepReq>,
) -> Result<Json<Value>, ApiError> {
    run_step(
        state,
        body,
        agent_prompts::WRITER,
        "rest_agent_writer",
        "writer",
        "剧本",
        "script",
    )
    .await
}
async fn storyboard_step(
    State(state): State<ApiState>,
    Json(body): Json<StepReq>,
) -> Result<Json<Value>, ApiError> {
    run_step(
        state,
        body,
        agent_prompts::STORYBOARD,
        "rest_agent_storyboard",
        "storyboard",
        "分镜",
        "storyboard",
    )
    .await
}
async fn consistency_step(
    State(state): State<ApiState>,
    Json(body): Json<StepReq>,
) -> Result<Json<Value>, ApiError> {
    run_step(
        state,
        body,
        agent_prompts::CONSISTENCY,
        "rest_agent_consistency",
        "consistency",
        "一致性",
        "consistency",
    )
    .await
}
async fn qc_review(
    State(state): State<ApiState>,
    Json(body): Json<StepReq>,
) -> Result<Json<Value>, ApiError> {
    run_step(
        state,
        body,
        agent_prompts::QC,
        "rest_agent_qc",
        "qc",
        "质检",
        "qc",
    )
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrchestrationReq {
    system: String,
    input: String,
    model: Option<String>,
    tools: Vec<commands::ToolDef>,
    project_id: Option<String>,
}

async fn orchestrate(
    State(state): State<ApiState>,
    Json(body): Json<OrchestrationReq>,
) -> Result<Json<Value>, ApiError> {
    required_text(&body.system, "system", MAX_TEXT_INPUT_CHARS)?;
    required_text(&body.input, "input", MAX_TEXT_INPUT_CHARS)?;
    optional_text(body.model.as_deref(), "model", MAX_MODEL_CHARS)?;
    optional_text(body.project_id.as_deref(), "projectId", 200)?;
    // 与 Tauri agent_run 共用同一套工具校验（数量/名称唯一/长度上限）。
    commands::validate_agent_tools(&body.tools).map_err(err)?;
    let cfg = state.cfg.read().unwrap().clone();
    let model = body.model.clone().unwrap_or_else(|| cfg.llm_model.clone());
    let system = body.system.clone();
    let input = body.input.clone();
    let result =
        commands::agent_run_with_config(&cfg, body.system, body.input, body.model, body.tools)
            .await
            .map_err(err)?;
    let asset =
        commands::save_text_with_config(&cfg, "Agent编排", &result, Some(&model)).map_err(err)?;
    let params_value = json!({
        "text": result,
        "title": "Agent编排",
        "documentType": "orchestration",
        "documentId": Uuid::new_v4().to_string(),
        "version": 1,
        "changeType": "generated",
        "provenance": crate::history::generated_provenance(&input, &input, Some(&system), json!([]), json!([])),
        "updatedAt": chrono::Utc::now().timestamp_millis(),
        "origin": "rest_api",
        "workflowStatus": "unreviewed",
    });
    crate::history::persist_assets(
        std::slice::from_ref(&asset),
        "Agent编排",
        Some(&model),
        body.project_id.as_deref(),
        &params_value,
    )
    .map_err(err)?;
    emit_history_changed(
        &state,
        "rest_agent_orchestration",
        std::slice::from_ref(&asset.id),
    );
    Ok(Json(json!({ "result": result, "asset": asset })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageReq {
    prompt: String,
    size: Option<String>,
    quality: Option<String>,
    background: Option<String>,
    reference_path: Option<String>,
    model: Option<String>,
    project_id: Option<String>,
}

async fn image(
    State(state): State<ApiState>,
    Json(body): Json<ImageReq>,
) -> Result<Json<Value>, ApiError> {
    required_text(&body.prompt, "prompt", MAX_PROMPT_CHARS)?;
    optional_text(body.size.as_deref(), "size", 64)?;
    optional_text(body.quality.as_deref(), "quality", 32)?;
    optional_text(body.background.as_deref(), "background", 32)?;
    optional_text(body.reference_path.as_deref(), "referencePath", 4_096)?;
    optional_text(body.model.as_deref(), "model", MAX_MODEL_CHARS)?;
    optional_text(body.project_id.as_deref(), "projectId", 200)?;
    if let Some(value) = body.quality.as_deref() {
        if !["low", "medium", "high"].contains(&value) {
            return Err(err("quality 必须是 low、medium 或 high".into()));
        }
    }
    if let Some(value) = body.background.as_deref() {
        if !["auto", "transparent", "opaque"].contains(&value) {
            return Err(err("background 必须是 auto、transparent 或 opaque".into()));
        }
    }
    let cfg = state.cfg.read().unwrap().clone();
    let prompt = body.prompt.clone();
    let reference_path = body.reference_path.clone();
    let model = body
        .model
        .clone()
        .unwrap_or_else(|| cfg.image_model.clone());
    let base = cfg.output_path();
    let dir = base.join("图片").join("api");
    let mut config = serde_json::Map::new();
    config.insert("prompt".into(), json!(prompt));
    if let Some(value) = body.size.clone() {
        config.insert("size".into(), json!(value));
    }
    if let Some(value) = body.quality.clone() {
        config.insert("quality".into(), json!(value));
    }
    if let Some(value) = body.background.clone() {
        config.insert("background".into(), json!(value));
    }
    if let Some(value) = reference_path.clone() {
        config.insert("referencePath".into(), json!(value));
    }
    config.insert("model".into(), json!(model));
    let request = RunNodeRequest {
        node_type: "textToImage".into(),
        category: "generate".into(),
        config: Value::Object(config),
        input_assets: vec![],
    };
    let assets = gateway::generate_image(&cfg, &request, &dir)
        .await
        .map_err(err)?;
    let source = if reference_path
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        "API图生图"
    } else {
        "API文生图"
    };
    let source_materials = reference_path.as_deref().map_or_else(
        || json!([]),
        |path| json!([{ "kind": "file", "label": "API参考图", "path": path }]),
    );
    let params_value = json!({
        "prompt": prompt,
        "referencePath": reference_path,
        "size": body.size,
        "quality": body.quality,
        "background": body.background,
        "model": model,
        "provenance": crate::history::generated_provenance(&prompt, &prompt, None, source_materials, json!([])),
        "origin": "rest_api",
    });
    crate::history::persist_assets(
        &assets,
        source,
        Some(&model),
        body.project_id.as_deref(),
        &params_value,
    )
    .map_err(err)?;
    let asset_ids = assets
        .iter()
        .map(|asset| asset.id.clone())
        .collect::<Vec<_>>();
    emit_history_changed(&state, "rest_image_generation", &asset_ids);
    Ok(Json(json!({ "assets": assets })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoReq {
    prompt: String,
    #[serde(alias = "duration_s")]
    duration_s: Option<u32>,
    aspect_ratio: Option<String>,
    resolution: Option<String>,
    model: Option<String>,
    mode: Option<String>,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    videos: Vec<String>,
    #[serde(default)]
    audios: Vec<String>,
    project_id: Option<String>,
}

async fn video(
    State(state): State<ApiState>,
    Json(body): Json<VideoReq>,
) -> Result<Json<Value>, ApiError> {
    required_text(&body.prompt, "prompt", MAX_PROMPT_CHARS)?;
    optional_text(body.aspect_ratio.as_deref(), "aspectRatio", 32)?;
    optional_text(body.resolution.as_deref(), "resolution", 32)?;
    optional_text(body.model.as_deref(), "model", MAX_MODEL_CHARS)?;
    optional_text(body.mode.as_deref(), "mode", 32)?;
    for (field, values) in [
        ("images", &body.images),
        ("videos", &body.videos),
        ("audios", &body.audios),
    ] {
        if values.len() > 30 {
            return Err(err(format!("{field} 最多允许 30 项")));
        }
        for value in values {
            optional_text(Some(value), field, 4_096)?;
        }
    }
    optional_text(body.project_id.as_deref(), "projectId", 200)?;
    if body
        .duration_s
        .is_some_and(|value| !(1..=3_600).contains(&value))
    {
        return Err(err("durationS 必须在 1 到 3600 秒之间".into()));
    }
    let cfg = state.cfg.read().unwrap().clone();
    let prompt = body.prompt.clone();
    let images = body.images.clone();
    let model = body
        .model
        .clone()
        .unwrap_or_else(|| cfg.video_model.clone());
    let base = cfg.output_path();
    let dir = base.join("视频").join("api");
    let request = VideoGenRequest {
        model: model.clone(),
        prompt: prompt.clone(),
        duration_s: body.duration_s.unwrap_or(5),
        aspect_ratio: body.aspect_ratio.clone(),
        resolution: body.resolution.clone(),
        mode: body.mode.clone(),
        images: images.clone(),
        videos: body.videos.clone(),
        audios: body.audios.clone(),
    };
    let assets = video::generate_segments(&cfg, &request, &dir)
        .await
        .map_err(err)?;
    let source = if images.is_empty() && body.videos.is_empty() && body.audios.is_empty() {
        "API文生视频"
    } else {
        "API参考素材视频"
    };
    let source_materials =
        images
            .iter()
            .map(|path| json!({ "kind": "file", "label": "API视频参考图 URL", "path": path }))
            .chain(body.videos.iter().map(
                |path| json!({ "kind": "file", "label": "API视频参考视频 URL", "path": path }),
            ))
            .chain(body.audios.iter().map(
                |path| json!({ "kind": "file", "label": "API视频参考音频 URL", "path": path }),
            ))
            .collect::<Vec<_>>();
    let params_value = json!({
        "prompt": prompt,
        "duration_s": request.duration_s,
        "aspectRatio": body.aspect_ratio,
        "resolution": body.resolution,
        "mode": body.mode,
        "images": images,
        "videos": body.videos,
        "audios": body.audios,
        "model": model,
        "providerTaskIds": assets.iter().filter_map(|asset| asset.id.strip_prefix("zzone:")).collect::<Vec<_>>(),
        "provenance": crate::history::generated_provenance(&prompt, &prompt, None, Value::Array(source_materials), json!([])),
        "origin": "rest_api",
        "workflowStatus": "direct_api_unreviewed",
    });
    crate::history::persist_assets(
        &assets,
        source,
        Some(&model),
        body.project_id.as_deref(),
        &params_value,
    )
    .map_err(err)?;
    let asset_ids = assets
        .iter()
        .map(|asset| asset.id.clone())
        .collect::<Vec<_>>();
    emit_history_changed(&state, "rest_video_generation", &asset_ids);
    Ok(Json(json!({ "assets": assets })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveTextReq {
    label: String,
    text: String,
    model: Option<String>,
    project_id: Option<String>,
}

async fn save_text_asset(
    State(state): State<ApiState>,
    Json(body): Json<SaveTextReq>,
) -> Result<Json<Value>, ApiError> {
    required_text(&body.label, "label", 100)?;
    if body.label.contains('/')
        || body.label.contains('\\')
        || body.label == "."
        || body.label == ".."
    {
        return Err(err("label 不能包含路径分隔符".into()));
    }
    if body.text.is_empty() || body.text.len() > 20 * 1024 * 1024 {
        return Err(err("text 必须非空且不超过 20 MiB".into()));
    }
    optional_text(body.model.as_deref(), "model", MAX_MODEL_CHARS)?;
    optional_text(body.project_id.as_deref(), "projectId", 200)?;
    let cfg = state.cfg.read().unwrap().clone();
    let asset =
        commands::save_text_with_config(&cfg, &body.label, &body.text, body.model.as_deref())
            .map_err(err)?;
    let params_value = json!({
        "text": body.text,
        "title": body.label,
        "documentType": "document",
        "documentId": Uuid::new_v4().to_string(),
        "version": 1,
        "changeType": "generated",
        "provenance": crate::history::generated_provenance(&body.text, &body.text, None, json!([]), json!([])),
        "updatedAt": chrono::Utc::now().timestamp_millis(),
        "origin": "rest_api",
    });
    crate::history::persist_assets(
        std::slice::from_ref(&asset),
        &body.label,
        body.model.as_deref(),
        body.project_id.as_deref(),
        &params_value,
    )
    .map_err(err)?;
    emit_history_changed(
        &state,
        "rest_text_asset_save",
        std::slice::from_ref(&asset.id),
    );
    Ok(Json(json!({ "asset": asset })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveMediaReq {
    kind: String,
    extension: String,
    data_base64: String,
    label: Option<String>,
    prompt: Option<String>,
    model: Option<String>,
    project_id: Option<String>,
}

async fn save_media_asset(
    State(state): State<ApiState>,
    Json(body): Json<SaveMediaReq>,
) -> Result<Json<Value>, ApiError> {
    if !["image", "video"].contains(&body.kind.as_str()) {
        return Err(err("kind 必须是 image 或 video".into()));
    }
    if body.data_base64.is_empty() || body.data_base64.len() > 700 * 1024 * 1024 {
        return Err(err("dataBase64 不能为空且长度过大".into()));
    }
    optional_text(body.label.as_deref(), "label", 100)?;
    optional_text(body.prompt.as_deref(), "prompt", MAX_PROMPT_CHARS)?;
    optional_text(body.model.as_deref(), "model", MAX_MODEL_CHARS)?;
    optional_text(body.project_id.as_deref(), "projectId", 200)?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.data_base64)
        .map_err(|error| err(format!("无效 base64: {error}")))?;
    let cfg = state.cfg.read().unwrap().clone();
    let asset = commands::save_media_asset_with_config(&cfg, &body.kind, &body.extension, &bytes)
        .map_err(err)?;
    let source = body.label.as_deref().unwrap_or(if body.kind == "image" {
        "API图片资产"
    } else {
        "API视频资产"
    });
    let prompt = body.prompt.as_deref().unwrap_or("");
    let params_value = json!({
        "prompt": body.prompt,
        "model": body.model,
        "provenance": crate::history::generated_provenance(prompt, prompt, None, json!([]), json!([])),
        "origin": "rest_api",
    });
    crate::history::persist_assets(
        std::slice::from_ref(&asset),
        source,
        body.model.as_deref(),
        body.project_id.as_deref(),
        &params_value,
    )
    .map_err(err)?;
    emit_history_changed(
        &state,
        "rest_media_asset_save",
        std::slice::from_ref(&asset.id),
    );
    Ok(Json(json!({ "asset": asset })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;
    use tokio::time::{timeout, Duration};
    use tower::ServiceExt;

    fn app() -> Router {
        build_router(ApiState {
            cfg: Arc::new(RwLock::new(ConfigState::load())),
            registry: {
                let registry = Arc::new(ProviderRegistry::new());
                registry.register(Arc::new(crate::providers::GatewayProvider::new(Arc::new(
                    RwLock::new(ConfigState::load()),
                ))));
                registry
            },
            history_sync: Arc::new(crate::history::HistorySyncState::default()),
        })
    }

    async fn get_json(path: &str) -> (StatusCode, Value) {
        let response = app()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value = serde_json::from_slice(&bytes).unwrap();
        (status, value)
    }

    #[tokio::test]
    async fn retired_comic_workflows_have_no_http_dispatch_route() {
        for path in [
            "/api/novel_analysis_start",
            "/api/novel_production_start",
            "/api/novel_adaptation_analysis_start",
            "/api/comic_visual_batch_resume",
            "/api/v1/novel/production/start",
            "/api/v1/comic/visual/render",
        ] {
            let response = app()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        // These are the separate, supported short-drama and media routes.
        // GET is rejected by these POST-only routes before any provider runs.
        for path in [
            "/api/director",
            "/api/script",
            "/api/storyboard",
            "/api/v1/agents/orchestrations",
            "/api/image",
            "/api/video",
        ] {
            let response = app()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
        }
    }

    #[tokio::test]
    async fn exposes_versioned_contract_and_legacy_health() {
        let (status, health) = get_json("/api/v1/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(health["apiVersion"], "v1");

        let (status, info) = get_json("/api/v1/system/info").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(info["authentication"], "none");
        assert_eq!(info["historyPersistence"], "sqlite");
        assert_eq!(info["historySynchronization"]["mode"], "event_driven");
        assert_eq!(info["historySynchronization"]["polling"], false);
        assert_eq!(info["routes"].as_array().unwrap().len(), 26);

        let (status, sync) = get_json("/api/v1/system/history-sync").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(sync["mode"], "event_driven");
        assert_eq!(sync["polling"], false);

        let (status, legacy) = get_json("/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(legacy["status"], "ok");
    }

    #[tokio::test]
    async fn reports_required_log_policy() {
        let (status, logs) = get_json("/api/v1/system/logs").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(logs["dailyFiles"], true);
        assert_eq!(logs["maxFileBytesExclusive"], 10 * 1024 * 1024);
        assert_eq!(logs["retentionDays"], 10);
    }

    #[tokio::test]
    async fn adds_request_id_and_cors_headers() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .header("origin", "http://localhost:1420")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.headers().get("x-request-id").is_some());
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "http://localhost:1420"
        );
    }

    #[tokio::test]
    async fn rejects_invalid_generation_inputs_before_calling_a_provider() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/media/images/generations")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"prompt":"   "}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn api_error_response_does_not_echo_upstream_credentials() {
        let (_, Json(body)) =
            err("upstream rejected Authorization: Bearer TOPSECRET client_secret=TOPSECRET".into());
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            !message.contains("TOPSECRET"),
            "unsafe API error: {message}"
        );
        assert!(
            !message.contains("client_secret"),
            "unsafe API error: {message}"
        );
    }

    #[test]
    fn exit_request_state_machine_prevents_until_the_final_exit_request() {
        assert_eq!(
            exit_request_action(ApiServerPhase::Running, false),
            (ApiServerPhase::Draining, ExitRequestAction::PreventAndDrain)
        );
        assert_eq!(
            exit_request_action(ApiServerPhase::Draining, false),
            (
                ApiServerPhase::Draining,
                ExitRequestAction::PreventWhileDraining
            )
        );
        assert_eq!(
            exit_request_action(ApiServerPhase::Exiting, false),
            (ApiServerPhase::Exiting, ExitRequestAction::AllowExit)
        );
        assert_eq!(
            exit_request_action(ApiServerPhase::Running, true),
            (
                ApiServerPhase::RestartBypass,
                ExitRequestAction::BestEffortRestart
            )
        );
        assert_eq!(
            exit_request_action(ApiServerPhase::Stopped, false),
            (ApiServerPhase::Stopped, ExitRequestAction::AllowExit)
        );
        assert_eq!(
            exit_request_action(ApiServerPhase::Draining, true),
            (
                ApiServerPhase::RestartBypass,
                ExitRequestAction::BestEffortRestart
            ),
            "restart must never request a second graceful drain"
        );
        assert_eq!(
            exit_request_action(ApiServerPhase::RestartBypass, false),
            (ApiServerPhase::RestartBypass, ExitRequestAction::AllowExit),
            "a queued normal exit must not revive draining after restart"
        );
    }

    #[tokio::test]
    async fn graceful_server_stops_accepting_connections_after_health_shutdown_and_join() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(serve_listener(listener, app(), receiver));

        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(
                b"GET /api/v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8_lossy(&response).contains("200 OK"));

        shutdown.send(()).unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .expect("graceful server must join")
            .unwrap()
            .unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
}
