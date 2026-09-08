use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use uuid::Uuid;

const MAX_LLM_RESPONSE_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub assistant_message: Value,
    /// Additive metadata: existing callers may keep accepting partial text.
    /// Document-producing callers must require normal completion before saving.
    pub finish_reason: Option<String>,
    pub completed: bool,
}

#[derive(Debug, Default)]
struct ToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Default)]
struct StreamAccumulator {
    content: String,
    tool_calls: BTreeMap<usize, ToolCallBuilder>,
    usage: Option<Value>,
    first_token_ms: Option<u128>,
}

#[derive(Clone, Copy)]
struct RequestMetrics {
    input_chars: usize,
    estimated_input_tokens: u64,
    header_ms: u128,
    started: Instant,
}

#[derive(Debug)]
struct SseDiagnostics {
    bytes: usize,
    lines: usize,
    data_events: usize,
    invalid_json_events: usize,
    unknown_events: usize,
    error_events: usize,
    done_received: bool,
    reasoning_delta_seen: bool,
    finish_reason: &'static str,
}

impl Default for SseDiagnostics {
    fn default() -> Self {
        Self {
            bytes: 0,
            lines: 0,
            data_events: 0,
            invalid_json_events: 0,
            unknown_events: 0,
            error_events: 0,
            done_received: false,
            reasoning_delta_seen: false,
            finish_reason: "none",
        }
    }
}

impl SseDiagnostics {
    fn as_json(&self) -> Value {
        json!({
            "bytes": self.bytes,
            "lines": self.lines,
            "dataEvents": self.data_events,
            "invalidJsonEvents": self.invalid_json_events,
            "unknownEvents": self.unknown_events,
            "errorEvents": self.error_events,
            "doneReceived": self.done_received,
            "reasoningDeltaSeen": self.reasoning_delta_seen,
            "finishReason": self.finish_reason,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum SseProtocolError {
    InvalidUtf8,
    InvalidJson,
    ApplicationError,
}

impl SseProtocolError {
    fn code(self) -> &'static str {
        match self {
            Self::InvalidUtf8 => "LLM_SSE_INVALID_UTF8",
            Self::InvalidJson => "LLM_SSE_INVALID_JSON_EVENT",
            Self::ApplicationError => "LLM_SSE_APPLICATION_ERROR",
        }
    }

    fn user_message(self) -> &'static str {
        match self {
            Self::ApplicationError => "文本流返回应用错误",
            Self::InvalidUtf8 | Self::InvalidJson => "文本流响应协议错误",
        }
    }
}

#[derive(Default)]
struct SseDecoder {
    bytes: Vec<u8>,
    data_lines: Vec<String>,
    event_name: Option<String>,
    first_line: bool,
    done: bool,
    skip_next_lf: bool,
    diagnostics: SseDiagnostics,
}

impl SseDecoder {
    fn push(
        &mut self,
        chunk: &[u8],
        accumulator: &mut StreamAccumulator,
        started: Instant,
    ) -> Result<bool, SseProtocolError> {
        if self.done {
            return Ok(true);
        }
        self.diagnostics.bytes = self.diagnostics.bytes.saturating_add(chunk.len());
        self.bytes.extend_from_slice(chunk);
        self.consume_lines(accumulator, started)
    }

    fn finish(
        &mut self,
        accumulator: &mut StreamAccumulator,
        started: Instant,
    ) -> Result<bool, SseProtocolError> {
        if self.done {
            return Ok(true);
        }
        let done = self.consume_lines(accumulator, started)?;
        if done {
            return Ok(true);
        }
        if !self.bytes.is_empty() {
            let line = std::mem::take(&mut self.bytes);
            self.consume_line(line, accumulator, started)?;
        }
        if !self.data_lines.is_empty() {
            return self.dispatch_event(accumulator, started);
        }
        Ok(false)
    }

    fn consume_lines(
        &mut self,
        accumulator: &mut StreamAccumulator,
        started: Instant,
    ) -> Result<bool, SseProtocolError> {
        while let Some(line) = self.take_line() {
            if self.consume_line(line, accumulator, started)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn consume_line(
        &mut self,
        line: Vec<u8>,
        accumulator: &mut StreamAccumulator,
        started: Instant,
    ) -> Result<bool, SseProtocolError> {
        self.diagnostics.lines = self.diagnostics.lines.saturating_add(1);
        let line = std::str::from_utf8(&line).map_err(|_| SseProtocolError::InvalidUtf8)?;
        let line = if !self.first_line {
            self.first_line = true;
            line.strip_prefix('\u{feff}').unwrap_or(line)
        } else {
            line
        };
        if line.is_empty() {
            return self.dispatch_event(accumulator, started);
        }
        if line.starts_with(':') {
            return Ok(false);
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => self.data_lines.push(value.to_owned()),
            "event" => self.event_name = Some(value.to_owned()),
            "id" | "retry" => {}
            _ => {
                self.diagnostics.unknown_events = self.diagnostics.unknown_events.saturating_add(1)
            }
        }
        Ok(false)
    }

    fn take_line(&mut self) -> Option<Vec<u8>> {
        if self.skip_next_lf {
            if self.bytes.is_empty() {
                return None;
            }
            if self.bytes.first() == Some(&b'\n') {
                self.bytes.drain(..1);
            }
            self.skip_next_lf = false;
        }
        let position = self
            .bytes
            .iter()
            .enumerate()
            .find_map(|(index, byte)| match byte {
                b'\n' => Some((index, 1)),
                b'\r' => Some((index, 1)),
                _ => None,
            })?;
        let (index, ending_len) = position;
        let was_cr = self.bytes.get(index) == Some(&b'\r');
        let line = self.bytes.drain(..index).collect::<Vec<_>>();
        self.bytes.drain(..ending_len);
        if was_cr {
            self.skip_next_lf = true;
        }
        Some(line)
    }

    fn dispatch_event(
        &mut self,
        accumulator: &mut StreamAccumulator,
        started: Instant,
    ) -> Result<bool, SseProtocolError> {
        let event_name = self.event_name.take();
        if event_name.as_deref() == Some("error") {
            self.diagnostics.error_events = self.diagnostics.error_events.saturating_add(1);
            self.data_lines.clear();
            return Err(SseProtocolError::ApplicationError);
        }
        if self.data_lines.is_empty() {
            return Ok(false);
        }
        self.diagnostics.data_events = self.diagnostics.data_events.saturating_add(1);
        let data = std::mem::take(&mut self.data_lines).join("\n");
        if data.trim() == "[DONE]" {
            self.done = true;
            self.diagnostics.done_received = true;
            return Ok(true);
        }
        let value = match serde_json::from_str::<Value>(&data) {
            Ok(value) => value,
            Err(_) => {
                self.diagnostics.invalid_json_events =
                    self.diagnostics.invalid_json_events.saturating_add(1);
                return Err(SseProtocolError::InvalidJson);
            }
        };
        if value.get("error").is_some_and(|error| !error.is_null()) {
            self.diagnostics.error_events = self.diagnostics.error_events.saturating_add(1);
            return Err(SseProtocolError::ApplicationError);
        }
        if event_name
            .as_deref()
            .is_some_and(|event_name| event_name != "message")
        {
            self.diagnostics.unknown_events = self.diagnostics.unknown_events.saturating_add(1);
        }
        if !process_sse_value(&value, accumulator, &mut self.diagnostics, started) {
            self.diagnostics.unknown_events = self.diagnostics.unknown_events.saturating_add(1);
        }
        Ok(false)
    }
}

pub async fn complete_text(
    url: &str,
    key: &str,
    model: &str,
    system: &str,
    user: &str,
    operation: &str,
) -> Result<String, String> {
    Ok(
        complete_text_result(url, key, model, system, user, operation)
            .await?
            .content,
    )
}

pub async fn complete_text_result(
    url: &str,
    key: &str,
    model: &str,
    system: &str,
    user: &str,
    operation: &str,
) -> Result<Completion, String> {
    let messages = vec![
        json!({"role": "system", "content": system}),
        json!({"role": "user", "content": user}),
    ];
    let completion = request(url, key, model, messages, None, operation).await?;
    if completion.content.is_empty() {
        return Err("文本接口未返回内容".into());
    }
    Ok(completion)
}

pub async fn request(
    url: &str,
    key: &str,
    model: &str,
    messages: Vec<Value>,
    tools: Option<&[Value]>,
    operation: &str,
) -> Result<Completion, String> {
    #[cfg(feature = "real-e2e-harness")]
    let client = crate::harness_transport::generation_client()?;
    #[cfg(not(feature = "real-e2e-harness"))]
    let client = client_for_endpoint(url)?;
    #[cfg(feature = "real-e2e-harness")]
    let allow_stream_fallback = crate::harness_transport::stream_fallback_allowed();
    #[cfg(not(feature = "real-e2e-harness"))]
    let allow_stream_fallback = true;
    request_with_client(
        &client,
        url,
        key,
        model,
        messages,
        tools,
        operation,
        allow_stream_fallback,
    )
    .await
}

#[cfg(not(feature = "real-e2e-harness"))]
fn client_for_endpoint(url: &str) -> Result<reqwest::Client, String> {
    crate::http::client_for_url(url)
}

async fn request_with_client(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    model: &str,
    messages: Vec<Value>,
    tools: Option<&[Value]>,
    operation: &str,
    allow_stream_fallback: bool,
) -> Result<Completion, String> {
    let request_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    let input_chars = message_chars(&messages);
    let estimated_input_tokens = message_token_estimate(&messages);
    crate::logging::info(
        "llm.request.start",
        json!({
            "requestId": request_id,
            "operation": operation,
            "model": model,
            "endpoint": crate::logging::safe_url(url),
            "messageCount": messages.len(),
            "inputChars": input_chars,
            "toolCount": tools.map(|items| items.len()).unwrap_or(0),
            "streamRequested": true,
        }),
    );

    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if let Some(tools) = tools {
        body["tools"] = json!(tools);
    }

    #[cfg(feature = "real-e2e-harness")]
    crate::harness_transport::reserve_llm_post(operation)?;

    let response = client
        .post(url)
        .bearer_auth(key)
        .json(&body)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|error| {
            log_failure(
                &request_id,
                operation,
                model,
                started,
                None,
                &error.to_string(),
            );
            format!("请求文本接口失败: {error}")
        })?;

    #[cfg(feature = "real-e2e-harness")]
    crate::harness_transport::mark_llm_http_received(operation)?;

    let status = response.status();
    let header_ms = started.elapsed().as_millis();
    if !status.is_success() {
        let text = read_limited_text(response, 1024 * 1024, "文本接口错误响应")
            .await
            .unwrap_or_default();
        if allow_stream_fallback && matches!(status.as_u16(), 400 | 422) {
            crate::logging::warn(
                "llm.stream.fallback",
                json!({
                    "requestId": request_id,
                    "operation": operation,
                    "model": model,
                    "status": status.as_u16(),
                    "reason": crate::logging::error_text(&text),
                }),
            );
            if let Some(object) = body.as_object_mut() {
                object.remove("stream");
                object.remove("stream_options");
            }
            return request_non_stream(
                &client,
                url,
                key,
                model,
                operation,
                request_id,
                input_chars,
                estimated_input_tokens,
                body,
                started,
            )
            .await;
        }
        log_failure(
            &request_id,
            operation,
            model,
            started,
            Some(status.as_u16()),
            &text,
        );
        return Err(format!(
            "文本接口返回 {status}: {}",
            text.chars().take(400).collect::<String>()
        ));
    }

    let is_stream = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));

    if !is_stream {
        let response_bytes = read_limited_bytes(response, MAX_LLM_RESPONSE_BYTES, "文本接口响应")
            .await
            .map_err(|error| {
                log_failure(
                    &request_id,
                    operation,
                    model,
                    started,
                    Some(status.as_u16()),
                    &error.to_string(),
                );
                format!("读取文本响应失败: {error}")
            })?;
        let value: Value = serde_json::from_slice(&response_bytes).map_err(|error| {
            log_failure(
                &request_id,
                operation,
                model,
                started,
                Some(status.as_u16()),
                &error.to_string(),
            );
            format!("解析文本响应失败: {error}")
        })?;
        return finish_non_stream(
            value,
            &request_id,
            operation,
            model,
            RequestMetrics {
                input_chars,
                estimated_input_tokens,
                header_ms,
                started,
            },
        );
    }

    let mut response = response;
    let mut accumulator = StreamAccumulator::default();
    let mut decoder = SseDecoder::default();
    let mut done = false;
    while !done {
        let Some(chunk) = response.chunk().await.map_err(|error| {
            log_failure(
                &request_id,
                operation,
                model,
                started,
                Some(status.as_u16()),
                &error.to_string(),
            );
            format!("读取文本流失败: {error}")
        })?
        else {
            break;
        };
        if decoder.diagnostics.bytes.saturating_add(chunk.len()) > MAX_LLM_RESPONSE_BYTES {
            log_failure(
                &request_id,
                operation,
                model,
                started,
                Some(status.as_u16()),
                "流式响应超过 100 MiB 大小限制",
            );
            return Err("文本流响应超过 100 MiB 大小限制".into());
        }
        done = decoder
            .push(&chunk, &mut accumulator, started)
            .map_err(|error| {
                log_stream_diagnostics(
                    &request_id,
                    operation,
                    model,
                    status.as_u16(),
                    &decoder.diagnostics,
                    error.code(),
                );
                log_failure(
                    &request_id,
                    operation,
                    model,
                    started,
                    Some(status.as_u16()),
                    error.code(),
                );
                error.user_message().to_string()
            })?;
    }
    if !done {
        decoder.finish(&mut accumulator, started).map_err(|error| {
            log_stream_diagnostics(
                &request_id,
                operation,
                model,
                status.as_u16(),
                &decoder.diagnostics,
                error.code(),
            );
            log_failure(
                &request_id,
                operation,
                model,
                started,
                Some(status.as_u16()),
                error.code(),
            );
            error.user_message().to_string()
        })?;
    }

    let finish_reason = match decoder.diagnostics.finish_reason {
        "none" => None,
        reason => Some(reason.to_string()),
    };
    let completed = normal_completion(finish_reason.as_deref(), decoder.diagnostics.done_received);
    let completion = build_completion(
        accumulator.content,
        accumulator.tool_calls,
        finish_reason,
        completed,
    );
    if completion.content.is_empty() && completion.tool_calls.is_empty() {
        log_stream_diagnostics(
            &request_id,
            operation,
            model,
            status.as_u16(),
            &decoder.diagnostics,
            "LLM_SSE_NO_CONTENT",
        );
        log_failure(
            &request_id,
            operation,
            model,
            started,
            Some(status.as_u16()),
            "流式响应没有内容",
        );
        return Err("文本接口未返回内容".into());
    }
    log_stream_diagnostics(
        &request_id,
        operation,
        model,
        status.as_u16(),
        &decoder.diagnostics,
        "LLM_SSE_COMPLETE",
    );
    log_success(
        &request_id,
        operation,
        model,
        input_chars,
        estimated_input_tokens,
        &completion.content,
        accumulator.usage.as_ref(),
        accumulator.first_token_ms,
        header_ms,
        started,
        "stream",
    );
    Ok(completion)
}

#[allow(clippy::too_many_arguments)]
async fn request_non_stream(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    model: &str,
    operation: &str,
    request_id: String,
    input_chars: usize,
    estimated_input_tokens: u64,
    body: Value,
    started: Instant,
) -> Result<Completion, String> {
    // This is normally reached only by the compatibility fallback.  Keep the
    // paid-request boundary here as well: a future caller cannot bypass the
    // harness' exact-one reservation merely by calling this helper directly.
    #[cfg(feature = "real-e2e-harness")]
    crate::harness_transport::reserve_llm_post(operation)?;
    let response = client
        .post(url)
        .bearer_auth(key)
        .json(&body)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|error| {
            log_failure(
                &request_id,
                operation,
                model,
                started,
                None,
                &error.to_string(),
            );
            format!("请求文本接口失败: {error}")
        })?;
    #[cfg(feature = "real-e2e-harness")]
    crate::harness_transport::mark_llm_http_received(operation)?;
    let status = response.status();
    let header_ms = started.elapsed().as_millis();
    if !status.is_success() {
        let text = read_limited_text(response, 1024 * 1024, "文本接口错误响应")
            .await
            .unwrap_or_default();
        log_failure(
            &request_id,
            operation,
            model,
            started,
            Some(status.as_u16()),
            &text,
        );
        return Err(format!(
            "文本接口返回 {status}: {}",
            text.chars().take(400).collect::<String>()
        ));
    }
    let response_bytes =
        read_limited_bytes(response, MAX_LLM_RESPONSE_BYTES, "文本接口响应").await?;
    let value: Value = serde_json::from_slice(&response_bytes)
        .map_err(|error| format!("解析文本响应失败: {error}"))?;
    finish_non_stream(
        value,
        &request_id,
        operation,
        model,
        RequestMetrics {
            input_chars,
            estimated_input_tokens,
            header_ms,
            started,
        },
    )
}

async fn read_limited_bytes(
    response: reqwest::Response,
    limit: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    crate::http::read_limited_bytes(response, limit, label).await
}

async fn read_limited_text(
    response: reqwest::Response,
    limit: usize,
    label: &str,
) -> Result<String, String> {
    let bytes = read_limited_bytes(response, limit, label).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn finish_non_stream(
    value: Value,
    request_id: &str,
    operation: &str,
    model: &str,
    metrics: RequestMetrics,
) -> Result<Completion, String> {
    if value.get("error").is_some_and(|error| !error.is_null()) {
        log_failure(
            request_id,
            operation,
            model,
            metrics.started,
            Some(200),
            "LLM_HTTP200_APPLICATION_ERROR",
        );
        return Err("文本接口返回应用错误".into());
    }
    let message = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .cloned()
        .unwrap_or(Value::Null);
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let calls = parse_tool_calls(message.get("tool_calls"));
    let finish_reason = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        .map(|reason| finish_reason_category(reason).to_string());
    // A fully parsed non-stream response is the completion envelope. Preserve
    // compatibility with providers omitting finish_reason, but never accept
    // an explicit length/filter/tool stop as a finished text document.
    let completed = normal_completion(finish_reason.as_deref(), true);
    let completion = Completion {
        content,
        tool_calls: calls,
        assistant_message: message,
        finish_reason,
        completed,
    };
    if completion.content.is_empty() && completion.tool_calls.is_empty() {
        log_failure(
            request_id,
            operation,
            model,
            metrics.started,
            Some(200),
            "非流式响应没有内容",
        );
        return Err("文本接口未返回内容".into());
    }
    log_success(
        request_id,
        operation,
        model,
        metrics.input_chars,
        metrics.estimated_input_tokens,
        &completion.content,
        value.get("usage"),
        None,
        metrics.header_ms,
        metrics.started,
        "non_stream",
    );
    Ok(completion)
}

fn process_sse_value(
    value: &Value,
    accumulator: &mut StreamAccumulator,
    diagnostics: &mut SseDiagnostics,
    started: Instant,
) -> bool {
    let has_usage = value.get("usage").is_some_and(|usage| !usage.is_null());
    if has_usage {
        accumulator.usage = value.get("usage").cloned();
    }
    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return has_usage;
    };
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(finish_reason_category)
        .unwrap_or("none");
    if finish_reason != "none" {
        diagnostics.finish_reason = finish_reason;
    }
    let Some(delta) = choice.get("delta") else {
        return finish_reason != "none";
    };
    if let Some(content) = delta.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            accumulator
                .first_token_ms
                .get_or_insert(started.elapsed().as_millis());
            accumulator.content.push_str(content);
        }
    }
    let reasoning_seen = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .and_then(Value::as_str)
        .is_some_and(|reasoning| !reasoning.is_empty());
    if reasoning_seen {
        // It is diagnostic-only: never append hidden reasoning to final text.
        // The decoder caller records the boolean without preserving content.
        diagnostics.reasoning_delta_seen = true;
    }
    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let builder = accumulator.tool_calls.entry(index).or_default();
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                builder.id.push_str(id);
            }
            if let Some(function) = call.get("function") {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    builder.name.push_str(name);
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    builder.arguments.push_str(arguments);
                }
            }
            accumulator
                .first_token_ms
                .get_or_insert(started.elapsed().as_millis());
        }
    }
    finish_reason != "none"
        || reasoning_seen
        || delta.get("content").is_some()
        || delta.get("tool_calls").is_some()
}

fn finish_reason_category(value: &str) -> &'static str {
    match value {
        "stop" => "stop",
        "length" => "length",
        "tool_calls" => "tool_calls",
        "content_filter" => "content_filter",
        _ => "other",
    }
}

fn normal_completion(finish_reason: Option<&str>, done_received: bool) -> bool {
    match finish_reason {
        Some("stop") => true,
        None => done_received,
        _ => false,
    }
}

fn build_completion(
    content: String,
    builders: BTreeMap<usize, ToolCallBuilder>,
    finish_reason: Option<String>,
    completed: bool,
) -> Completion {
    let tool_calls: Vec<ToolCall> = builders
        .into_iter()
        .map(|(index, builder)| ToolCall {
            id: if builder.id.is_empty() {
                format!("call_{index}")
            } else {
                builder.id
            },
            name: builder.name,
            arguments: builder.arguments,
        })
        .collect();
    let mut assistant_message = json!({ "role": "assistant", "content": content });
    if !tool_calls.is_empty() {
        assistant_message["tool_calls"] = json!(tool_calls
            .iter()
            .map(|call| json!({
                "id": call.id,
                "type": "function",
                "function": { "name": call.name, "arguments": call.arguments },
            }))
            .collect::<Vec<_>>());
    }
    Completion {
        content,
        tool_calls,
        assistant_message,
        finish_reason,
        completed,
    }
}

fn parse_tool_calls(value: Option<&Value>) -> Vec<ToolCall> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|call| ToolCall {
            id: call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            name: call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            arguments: call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}")
                .to_string(),
        })
        .collect()
}

fn usage_number(usage: Option<&Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        usage
            .and_then(|value| value.get(*key))
            .and_then(Value::as_u64)
    })
}

#[allow(clippy::too_many_arguments)]
fn log_success(
    request_id: &str,
    operation: &str,
    model: &str,
    input_chars: usize,
    estimated_input_tokens: u64,
    output: &str,
    usage: Option<&Value>,
    first_token_ms: Option<u128>,
    header_ms: u128,
    started: Instant,
    response_mode: &str,
) {
    let provider_input = usage_number(usage, &["prompt_tokens", "input_tokens"]);
    let provider_output = usage_number(usage, &["completion_tokens", "output_tokens"]);
    let provider_total = usage_number(usage, &["total_tokens"]);
    let estimated_output = estimate_text_tokens(output);
    crate::logging::info(
        "llm.request.end",
        json!({
            "requestId": request_id,
            "operation": operation,
            "model": model,
            "status": "success",
            "responseMode": response_mode,
            "durationMs": started.elapsed().as_millis(),
            "responseHeaderMs": header_ms,
            "firstTokenMs": first_token_ms,
            "inputChars": input_chars,
            "outputChars": output.chars().count(),
            "inputTokens": provider_input.unwrap_or(estimated_input_tokens),
            "outputTokens": provider_output.unwrap_or(estimated_output),
            "totalTokens": provider_total.unwrap_or_else(|| provider_input.unwrap_or(estimated_input_tokens) + provider_output.unwrap_or(estimated_output)),
            "tokenUsageSource": if provider_input.is_some() || provider_output.is_some() { "provider" } else { "estimate_ascii4_nonascii1" },
        }),
    );
}

fn log_failure(
    request_id: &str,
    operation: &str,
    model: &str,
    started: Instant,
    status: Option<u16>,
    error: &str,
) {
    crate::logging::error(
        "llm.request.end",
        json!({
            "requestId": request_id,
            "operation": operation,
            "model": model,
            "status": "error",
            "httpStatus": status,
            "durationMs": started.elapsed().as_millis(),
            "error": crate::logging::error_text(error),
        }),
    );
}

fn log_stream_diagnostics(
    request_id: &str,
    operation: &str,
    model: &str,
    status: u16,
    diagnostics: &SseDiagnostics,
    outcome: &str,
) {
    crate::logging::info(
        "llm.stream.diagnostics",
        json!({
            "requestId": request_id,
            "operation": operation,
            "model": model,
            "httpStatus": status,
            "outcome": outcome,
            "metrics": diagnostics.as_json(),
        }),
    );
}

fn message_chars(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(|message| {
            message
                .get("content")
                .and_then(Value::as_str)
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or(0)
        })
        .sum()
}

fn message_token_estimate(messages: &[Value]) -> u64 {
    messages
        .iter()
        .filter_map(|message| message.get("content").and_then(Value::as_str))
        .map(estimate_text_tokens)
        .sum()
}

fn estimate_text_tokens(text: &str) -> u64 {
    let (ascii, non_ascii) = text
        .chars()
        .fold((0u64, 0u64), |(ascii, non_ascii), character| {
            if character.is_ascii() {
                (ascii + 1, non_ascii)
            } else {
                (ascii, non_ascii + 1)
            }
        });
    ascii.div_ceil(4) + non_ascii
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
        task::JoinHandle,
        time::timeout,
    };

    fn decode_chunks(
        chunks: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<(StreamAccumulator, SseDiagnostics, bool), SseProtocolError> {
        let started = Instant::now();
        let mut decoder = SseDecoder::default();
        let mut accumulator = StreamAccumulator::default();
        for chunk in chunks {
            if decoder.push(&chunk, &mut accumulator, started)? {
                let diagnostics = std::mem::take(&mut decoder.diagnostics);
                return Ok((accumulator, diagnostics, true));
            }
        }
        let done = decoder.finish(&mut accumulator, started)?;
        let diagnostics = std::mem::take(&mut decoder.diagnostics);
        Ok((accumulator, diagnostics, done))
    }

    struct LoopbackServer {
        url: String,
        requests: Arc<AtomicUsize>,
        shutdown: Option<oneshot::Sender<()>>,
        task: Option<JoinHandle<()>>,
    }

    impl LoopbackServer {
        async fn start(content_type: &'static str, body: &'static [u8], hold_open: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(AtomicUsize::new(0));
            let request_count = Arc::clone(&requests);
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                request_count.fetch_add(1, Ordering::SeqCst);
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nConnection: keep-alive\r\n\r\n"
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(body).await.unwrap();
                stream.flush().await.unwrap();
                if hold_open {
                    let _ = &mut shutdown_rx.await;
                }
            });
            Self {
                url: format!("http://{address}/v1/chat/completions"),
                requests,
                shutdown: Some(shutdown_tx),
                task: Some(task),
            }
        }

        async fn start_status(
            status: &'static str,
            content_type: &'static str,
            body: &'static [u8],
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(AtomicUsize::new(0));
            let request_count = Arc::clone(&requests);
            let (shutdown_tx, _shutdown_rx) = oneshot::channel::<()>();
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                request_count.fetch_add(1, Ordering::SeqCst);
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                let headers = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(body).await.unwrap();
                stream.flush().await.unwrap();
            });
            Self {
                url: format!("http://{address}/v1/chat/completions"),
                requests,
                shutdown: Some(shutdown_tx),
                task: Some(task),
            }
        }

        async fn start_sequence(
            responses: Vec<(&'static str, &'static str, &'static [u8])>,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(AtomicUsize::new(0));
            let request_count = Arc::clone(&requests);
            let (shutdown_tx, _shutdown_rx) = oneshot::channel::<()>();
            let task = tokio::spawn(async move {
                for (status, content_type, body) in responses {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    request_count.fetch_add(1, Ordering::SeqCst);
                    let mut request = [0u8; 4096];
                    let _ = stream.read(&mut request).await.unwrap();
                    let headers = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
                    );
                    stream.write_all(headers.as_bytes()).await.unwrap();
                    stream.write_all(body).await.unwrap();
                    stream.flush().await.unwrap();
                }
            });
            Self {
                url: format!("http://{address}/v1/chat/completions"),
                requests,
                shutdown: Some(shutdown_tx),
                task: Some(task),
            }
        }

        async fn close(mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            let result = if let Some(task) = self.task.as_mut() {
                timeout(Duration::from_secs(1), task).await
            } else {
                return;
            };
            if result.is_err() {
                if let Some(task) = self.task.take() {
                    task.abort();
                    let _ = task.await;
                }
                panic!("loopback server should stop");
            }
            result
                .expect("timeout result was checked")
                .expect("loopback server task should succeed");
            self.task.take();
        }
    }

    impl Drop for LoopbackServer {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            if let Some(task) = self.task.take() {
                task.abort();
            }
        }
    }

    fn loopback_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    fn no_redirect_loopback_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .unwrap()
    }

    #[test]
    fn accumulates_stream_content_usage_and_tool_calls() {
        let started = Instant::now();
        let mut accumulator = StreamAccumulator::default();
        let mut diagnostics = SseDiagnostics::default();
        assert!(process_sse_value(
            &serde_json::from_str(r#"{"choices":[{"delta":{"content":"你好"}}]}"#).unwrap(),
            &mut accumulator,
            &mut diagnostics,
            started,
        ));
        assert!(process_sse_value(
            &serde_json::from_str(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"writer","arguments":"{\"input\":\"故事\"}"}}]}}]}"#).unwrap(),
            &mut accumulator,
            &mut diagnostics,
            started,
        ));
        assert!(process_sse_value(
            &serde_json::from_str(r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}}"#).unwrap(),
            &mut accumulator,
            &mut diagnostics,
            started,
        ));
        assert_eq!(accumulator.content, "你好");
        assert_eq!(accumulator.usage.unwrap()["total_tokens"], 14);
        let completion = build_completion(accumulator.content, accumulator.tool_calls, None, false);
        assert_eq!(completion.tool_calls[0].name, "writer");
        assert!(completion.assistant_message.get("tool_calls").is_some());
    }

    #[test]
    fn decoder_handles_bom_crlf_utf8_chunks_and_named_completion_event() {
        let frame = "\u{feff}event: completion\r\ndata: {\"choices\":[\r\ndata: {\"delta\":{\"content\":\"你好\"}}]}\r\n\r\ndata: [DONE]\r\n\r\n";
        let chunks = frame
            .as_bytes()
            .iter()
            .map(|byte| vec![*byte])
            .collect::<Vec<_>>();
        let (accumulator, diagnostics, done) = decode_chunks(chunks).unwrap();
        assert!(done);
        assert_eq!(accumulator.content, "你好");
        assert!(diagnostics.done_received);
        assert!(diagnostics.unknown_events >= 1);
    }

    #[test]
    fn cr_only_done_dispatches_without_eof_and_ignores_tail() {
        let (accumulator, diagnostics, done) = decode_chunks(vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\r\rdata: [DONE]\r\rdata: {\"choices\":[{\"delta\":{\"content\":\"tail\"}}]}\r\r".to_vec(),
        ])
        .unwrap();
        assert!(done);
        assert_eq!(accumulator.content, "first");
        assert!(diagnostics.done_received);
    }

    #[test]
    fn accepts_legacy_eof_final_sse_data_without_blank_line() {
        let (accumulator, _, done) = decode_chunks(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"最后一段\"}}]}"
                .as_bytes()
                .to_vec(),
        ])
        .unwrap();
        assert!(!done);
        assert_eq!(accumulator.content, "最后一段");
    }

    #[test]
    fn error_event_wins_over_partial_content_and_done_marker() {
        let error = decode_chunks(vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\nevent: error\ndata: [DONE]\n\n".to_vec(),
        ])
        .unwrap_err();
        assert!(matches!(error, SseProtocolError::ApplicationError));
        let empty_error = decode_chunks(vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\nevent: error\n\n"
                .to_vec(),
        ])
        .unwrap_err();
        assert!(matches!(empty_error, SseProtocolError::ApplicationError));
    }

    #[test]
    fn invalid_json_fails_closed() {
        let error = decode_chunks(vec![b"data: {not-json}\n\n".to_vec()]).unwrap_err();
        assert!(matches!(error, SseProtocolError::InvalidJson));
    }

    #[test]
    fn invalid_utf8_and_top_level_error_after_partial_fail_closed() {
        let invalid_utf8 = decode_chunks(vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\ndata: \xff\n\n"
                .to_vec(),
        ])
        .unwrap_err();
        assert!(matches!(invalid_utf8, SseProtocolError::InvalidUtf8));
        let top_level_error = decode_chunks(vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\ndata: {\"error\":{\"message\":\"synthetic provider detail\"}}\n\n"
                .to_vec(),
        ])
        .unwrap_err();
        assert!(matches!(
            top_level_error,
            SseProtocolError::ApplicationError
        ));
    }

    #[test]
    fn tracks_reasoning_and_finish_reason_without_emitting_reasoning() {
        let (accumulator, diagnostics, _) = decode_chunks(vec![
            b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"private\",\"content\":\"visible\"},\"finish_reason\":\"stop\"}]}\n\n".to_vec(),
        ])
        .unwrap();
        assert_eq!(accumulator.content, "visible");
        assert!(diagnostics.reasoning_delta_seen);
        assert_eq!(diagnostics.finish_reason, "stop");
    }

    #[test]
    fn diagnostics_are_fixed_metrics_not_stream_payloads() {
        let started = Instant::now();
        let mut decoder = SseDecoder::default();
        let mut accumulator = StreamAccumulator::default();
        decoder
            .push(
                b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"synthetic-secret-reasoning\"}}]}\n\n",
                &mut accumulator,
                started,
            )
            .unwrap();
        let error = decoder.push(
            b"data: {\"error\":{\"message\":\"synthetic-secret-error\"}}\n\n",
            &mut accumulator,
            started,
        );
        assert!(matches!(error, Err(SseProtocolError::ApplicationError)));
        let serialized = decoder.diagnostics.as_json().to_string();
        assert!(decoder.diagnostics.reasoning_delta_seen);
        assert!(!serialized.contains("synthetic-secret-reasoning"));
        assert!(!serialized.contains("synthetic-secret-error"));
    }

    #[test]
    fn usage_frame_does_not_classify_later_unknown_objects_as_recognized() {
        let (accumulator, diagnostics, _) = decode_chunks(vec![
            b"data: {\"usage\":{\"total_tokens\":1}}\n\ndata: {\"unrelated\":true}\n\n".to_vec(),
        ])
        .unwrap();
        assert_eq!(accumulator.usage.unwrap()["total_tokens"], 1);
        assert_eq!(diagnostics.unknown_events, 1);
    }

    #[tokio::test]
    async fn partial_finish_reproduction_preserves_returned_text() {
        for (label,content_type,body) in [
            ("stream_length","text/event-stream",b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n".as_slice()),
            ("stream_eof","text/event-stream",b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n".as_slice()),
            ("json_length","application/json",b"{\"choices\":[{\"message\":{\"content\":\"partial\"},\"finish_reason\":\"length\"}]}".as_slice()),
        ] {
            let server=LoopbackServer::start(content_type,body,false).await;
            let result=request_with_client(&loopback_client(),&server.url,"local-test-key","local-test-model",vec![json!({"role":"user","content":"test"})],None,"partial_finish_reproduction",true).await.unwrap();
            assert_eq!(result.content,"partial");
            assert!(!result.completed,"{label} must expose incomplete output to document callers");
            assert_eq!(result.finish_reason.as_deref(),if label=="stream_eof"{None}else{Some("length")});
            assert_eq!(server.requests.load(Ordering::SeqCst),1);
            println!("{label}: partial content retained with completed=false; no retry");
            server.close().await;
        }
    }

    #[test]
    fn stop_and_done_are_normal_but_explicit_truncation_wins() {
        assert!(normal_completion(Some("stop"), false));
        assert!(normal_completion(None, true));
        assert!(!normal_completion(None, false));
        for reason in ["length", "content_filter", "tool_calls", "other"] {
            assert!(!normal_completion(Some(reason), true));
        }
    }

    #[tokio::test]
    async fn done_stops_network_read_before_loopback_server_closes() {
        let server = LoopbackServer::start(
            "text/event-stream",
            b"data: {\"choices\":[{\"delta\":{\"content\":\"test\"}}]}\r\rdata: [DONE]\r\r",
            true,
        )
        .await;
        let result = timeout(
            Duration::from_secs(1),
            request_with_client(
                &loopback_client(),
                &server.url,
                "local-test-key",
                "local-test-model",
                vec![json!({"role":"user","content":"test"})],
                None,
                "llm_loopback_done",
                true,
            ),
        )
        .await
        .expect("DONE must return before the server closes")
        .unwrap();
        assert_eq!(result.content, "test");
        assert!(result.completed);
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        server.close().await;
    }

    #[tokio::test]
    async fn http200_partial_then_error_fails_without_a_second_request() {
        let server = LoopbackServer::start(
            "text/event-stream",
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\nevent: error\n\n",
            true,
        )
        .await;
        let error = timeout(
            Duration::from_secs(1),
            request_with_client(
                &loopback_client(),
                &server.url,
                "local-test-key",
                "local-test-model",
                vec![json!({"role":"user","content":"test"})],
                None,
                "llm_loopback_error",
                true,
            ),
        )
        .await
        .expect("error frame must return before the server closes")
        .unwrap_err();
        assert_eq!(error, "文本流返回应用错误");
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        server.close().await;
    }

    #[tokio::test]
    async fn disabled_stream_fallback_never_sends_a_second_post_for_400_or_422() {
        for status in ["400 Bad Request", "422 Unprocessable Content"] {
            let server =
                LoopbackServer::start_status(status, "application/json", b"{\"error\":{}}").await;
            let error = timeout(
                Duration::from_secs(1),
                request_with_client(
                    &no_redirect_loopback_client(),
                    &server.url,
                    "local-test-key",
                    "local-test-model",
                    vec![json!({"role":"user","content":"test"})],
                    None,
                    "novel.analysis",
                    false,
                ),
            )
            .await
            .expect("a disabled fallback must return promptly")
            .unwrap_err();
            assert!(error.starts_with("文本接口返回"));
            assert_eq!(server.requests.load(Ordering::SeqCst), 1, "{status}");
            server.close().await;
        }
    }

    #[tokio::test]
    async fn normal_stream_fallback_remains_a_two_post_compatibility_path() {
        let server = LoopbackServer::start_sequence(vec![
            ("400 Bad Request", "application/json", b"{\"error\":{}}"),
            (
                "200 OK",
                "application/json",
                b"{\"choices\":[{\"message\":{\"content\":\"fallback-ok\"}}]}",
            ),
        ])
        .await;
        let completion = timeout(
            Duration::from_secs(1),
            request_with_client(
                &loopback_client(),
                &server.url,
                "local-test-key",
                "local-test-model",
                vec![json!({"role":"user","content":"test"})],
                None,
                "normal_fallback_test",
                true,
            ),
        )
        .await
        .expect("normal fallback should return promptly")
        .unwrap();
        assert_eq!(completion.content, "fallback-ok");
        assert_eq!(server.requests.load(Ordering::SeqCst), 2);
        server.close().await;
    }

    #[tokio::test]
    async fn ordinary_http_json_and_error_envelopes_stay_supported() {
        let server = LoopbackServer::start(
            "application/json",
            b"{\"choices\":[{\"message\":{\"content\":\"json-test\"}}]}",
            false,
        )
        .await;
        let completion = timeout(
            Duration::from_secs(1),
            request_with_client(
                &loopback_client(),
                &server.url,
                "local-test-key",
                "local-test-model",
                vec![json!({"role":"user","content":"test"})],
                None,
                "llm_loopback_json",
                true,
            ),
        )
        .await
        .expect("JSON loopback should return promptly")
        .unwrap();
        assert_eq!(completion.content, "json-test");
        assert!(completion.completed);
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        server.close().await;

        let server = LoopbackServer::start(
            "application/json",
            b"{\"error\":{\"message\":\"synthetic error\"}}",
            false,
        )
        .await;
        let error = timeout(
            Duration::from_secs(1),
            request_with_client(
                &loopback_client(),
                &server.url,
                "local-test-key",
                "local-test-model",
                vec![json!({"role":"user","content":"test"})],
                None,
                "llm_loopback_json_error",
                true,
            ),
        )
        .await
        .expect("JSON error loopback should return promptly")
        .unwrap_err();
        assert_eq!(error, "文本接口返回应用错误");
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        server.close().await;
    }

    #[test]
    fn estimates_ascii_and_non_ascii_tokens_separately() {
        assert_eq!(estimate_text_tokens("abcdefgh"), 2);
        assert_eq!(estimate_text_tokens("你好世界"), 4);
        assert_eq!(estimate_text_tokens("abcd你好"), 3);
    }
}
