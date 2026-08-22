//! OpenAI Responses API (`POST /v1/responses`) `LlmProvider` adapter.
//!
//! Sibling of [`openai_compat`](super::openai_compat). The kernel still speaks
//! `Message` / `ToolCall`; this adapter maps them onto Responses `input[]` items
//! (`message`, `function_call`, `function_call_output`, `reasoning`) so Grok Build
//! / OpenAI-style prefix cache sees an append-only item list instead of Chat
//! Completions `messages[]`.
//!
//! Cache levers (match Grok Build's high-hit wire):
//!   - `include: ["reasoning.encrypted_content"]` + replay of signed reasoning items
//!     with stable `id`s;
//!   - `prompt_cache_key` + `x-session-id` for gateway affinity;
//!   - consecutive user items are NOT merged;
//!   - `store: false` (full input is resent; cache is prefix-based, not stored-response).

use super::openai_compat::{build_http_client, resolve_wire_effort, SwappableClient};
use super::reasoning::ReasoningPolicy;
use super::retry::{self, RetryPolicy};
use super::sign::RequestSigner;
use async_trait::async_trait;
use atomcode_kernel::message::{Message, Role};
use atomcode_kernel::provider::{ChatOptions, LlmProvider, ToolChoice};
use atomcode_kernel::stream::{ProviderError, StreamEvent, TokenUsage};
use atomcode_kernel::tool::{ToolCall, ToolDef};
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Map, Value};
use std::time::Duration;

/// Attribution stamped onto [`atomcode_kernel::message::ReasoningBlock::provider`]
/// so a later Completions/Anthropic adapter will not echo this ciphertext.
const RESPONSES_PROVIDER: &str = "openai-responses";

#[derive(Clone)]
pub struct ResponsesConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub context_window: u32,
    pub max_tokens: Option<u32>,
    pub idle_timeout: Duration,
    pub connect_timeout: Duration,
    pub open_timeout: Duration,
    pub retry: RetryPolicy,
    pub request_signer: Option<std::sync::Arc<dyn RequestSigner>>,
    pub user_agent: Option<String>,
    pub skip_tls_verify: bool,
    pub supports_vision: bool,
    /// Explicit "this is a thinking model" flag. `Some(true)` ⇒ echo encrypted
    /// reasoning + send `reasoning.effort`; `Some(false)` ⇒ neither.
    pub reasoning_model: Option<bool>,
    /// Override for [`ReasoningPolicy`] from `reasoning_history = include|exclude`.
    /// `None` ⇒ derive from `reasoning_model` then model name / base URL.
    pub reasoning_policy: Option<ReasoningPolicy>,
}

impl ResponsesConfig {
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            model: model.into(),
            context_window: 128_000,
            max_tokens: None,
            idle_timeout: Duration::from_secs(300),
            connect_timeout: Duration::from_secs(10),
            open_timeout: Duration::from_secs(30),
            retry: RetryPolicy::default(),
            request_signer: None,
            user_agent: None,
            skip_tls_verify: false,
            supports_vision: false,
            reasoning_model: None,
            reasoning_policy: None,
        }
    }
}

pub struct ResponsesProvider {
    cfg: ResponsesConfig,
    policy: ReasoningPolicy,
    client: std::sync::Arc<SwappableClient>,
    url: String,
    session_id: std::sync::OnceLock<String>,
}

impl ResponsesProvider {
    pub fn new(cfg: ResponsesConfig) -> Result<Self, ProviderError> {
        let policy = cfg
            .reasoning_model
            .map(|rm| {
                if rm {
                    ReasoningPolicy::Include
                } else {
                    ReasoningPolicy::Exclude
                }
            })
            .or(cfg.reasoning_policy)
            .unwrap_or_else(|| ReasoningPolicy::derive(&cfg.model, &cfg.base_url));
        let connect_timeout = cfg.connect_timeout;
        let skip_tls_verify = cfg.skip_tls_verify;
        let user_agent = cfg.user_agent.clone();
        let url = format!("{}/responses", cfg.base_url.trim_end_matches('/'));
        let initial_tls12 = atomcode_config::tls::should_cap_url(&url);
        let client = std::sync::Arc::new(SwappableClient::new(initial_tls12, move |tls12| {
            build_http_client(connect_timeout, skip_tls_verify, user_agent.clone(), tls12)
        })?);
        Ok(Self {
            cfg,
            policy,
            client,
            url,
            session_id: std::sync::OnceLock::new(),
        })
    }
}

#[async_trait]
impl LlmProvider for ResponsesProvider {
    fn model_name(&self) -> &str {
        &self.cfg.model
    }

    fn context_window(&self) -> u32 {
        self.cfg.context_window
    }

    fn bind_session_id(&self, session_id: &str) {
        let _ = self.session_id.set(session_id.to_string());
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        options: &ChatOptions,
    ) -> Result<BoxStream<'static, StreamEvent>, ProviderError> {
        let session_id = self.session_id.get().cloned().unwrap_or_default();
        let body = build_request_body(
            &self.cfg.model,
            messages,
            tools,
            options,
            &self.cfg,
            self.policy,
            &session_id,
        );
        super::wire_dump_request(&self.cfg.model, &body);
        let body_bytes = serde_json::to_vec(&body).map_err(|e| ProviderError {
            retryable: false,
            message: format!("request body serialization failed: {e}"),
            ..Default::default()
        })?;
        let url = self.url.clone();
        let client = self.client.clone();
        let signer = self.cfg.request_signer.clone();
        let api_key = self.cfg.api_key.clone();
        let retry = self.cfg.retry.clone();
        let open_timeout = self.cfg.open_timeout;
        let idle_timeout = self.cfg.idle_timeout;
        let rate_limit_retry_owner = options.rate_limit_retry_owner;

        let s = async_stream::stream! {
            let mut dec = ResponsesSseDecoder::default();
            let resp = match open_responses_stream(
                &client,
                &url,
                &body_bytes,
                &signer,
                &api_key,
                &session_id,
                &retry,
                rate_limit_retry_owner,
                open_timeout,
            ).await {
                Ok(r) => r,
                Err(e) => {
                    yield StreamEvent::Error(e);
                    return;
                }
            };
            let mut bytes = resp.bytes_stream();
            loop {
                match tokio::time::timeout(idle_timeout, bytes.next()).await {
                    Err(_) => {
                        yield StreamEvent::Error(ProviderError {
                            retryable: true,
                            message: format!("stream idle timeout ({}s)", idle_timeout.as_secs()),
                            ..Default::default()
                        });
                        return;
                    }
                    Ok(None) => {
                        for ev in dec.finish() {
                            yield ev;
                        }
                        return;
                    }
                    Ok(Some(Err(e))) => {
                        yield StreamEvent::Error(ProviderError {
                            retryable: true,
                            message: format!("stream read failed: {}", retry::err_chain(&e)),
                            ..Default::default()
                        });
                        return;
                    }
                    Ok(Some(Ok(chunk))) => {
                        let mut saw_done = false;
                        for ev in dec.feed(chunk.as_ref()) {
                            if matches!(ev, StreamEvent::Done { .. }) {
                                saw_done = true;
                            }
                            yield ev;
                        }
                        if saw_done {
                            return;
                        }
                    }
                }
            }
        };
        Ok(s.boxed())
    }
}

async fn open_responses_stream(
    client: &SwappableClient,
    url: &str,
    body_bytes: &[u8],
    signer: &Option<std::sync::Arc<dyn RequestSigner>>,
    api_key: &str,
    session_id: &str,
    policy: &RetryPolicy,
    rate_limit_retry_owner: atomcode_kernel::provider::RateLimitRetryOwner,
    open_timeout: Duration,
) -> Result<reqwest::Response, ProviderError> {
    let _ = rate_limit_retry_owner;
    let mut attempt = 1u32;
    loop {
        let http = client.get();
        let mut req = http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body_bytes.to_vec());
        match signer {
            Some(signer) => {
                let auth = signer.sign(body_bytes).map_err(|error| ProviderError {
                    retryable: false,
                    message: error.to_string(),
                    code: Some(error.code().to_string()),
                    ..Default::default()
                })?;
                req = req.bearer_auth(auth.bearer.as_deref().unwrap_or(api_key));
                for (name, value) in &auth.headers {
                    req = req.header(name.as_str(), value.as_str());
                }
            }
            None => {
                req = req.bearer_auth(api_key);
            }
        }
        if !session_id.is_empty() {
            req = req.header("x-atomcode-session-id", session_id);
            req = req.header("x-session-id", session_id);
        }
        let sent = match tokio::time::timeout(open_timeout, req.send()).await {
            Ok(r) => r,
            Err(_) => {
                if attempt < policy.max_attempts {
                    tokio::time::sleep(retry::compute_backoff(attempt, policy)).await;
                    attempt += 1;
                    continue;
                }
                return Err(ProviderError {
                    retryable: true,
                    message: format!(
                        "open failed: 等待首字节超过 {}s(网关无响应)",
                        open_timeout.as_secs()
                    ),
                    ..Default::default()
                });
            }
        };
        match sent {
            Ok(resp) if resp.status().is_success() => return Ok(resp),
            Ok(resp) => {
                let status = resp.status().as_u16();
                let detail = resp.text().await.unwrap_or_default();
                let retryable = status == 429 || (500..600).contains(&status);
                if retryable && attempt < policy.max_attempts {
                    tokio::time::sleep(retry::compute_backoff(attempt, policy)).await;
                    attempt += 1;
                    continue;
                }
                return Err(ProviderError {
                    retryable,
                    message: super::friendly_http_error(status, &detail),
                    http_status: Some(status),
                    ..Default::default()
                });
            }
            Err(e) => {
                if attempt < policy.max_attempts {
                    let _ = client.rebuild(false);
                    tokio::time::sleep(retry::compute_backoff(attempt, policy)).await;
                    attempt += 1;
                    continue;
                }
                return Err(ProviderError {
                    retryable: true,
                    message: format!("open failed: {}", retry::err_chain(&e)),
                    ..Default::default()
                });
            }
        }
    }
}

fn build_request_body(
    model: &str,
    messages: &[Message],
    tools: &[ToolDef],
    options: &ChatOptions,
    cfg: &ResponsesConfig,
    policy: ReasoningPolicy,
    session_id: &str,
) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert(
        "input".into(),
        json!(format_input(
            messages,
            cfg.supports_vision,
            policy == ReasoningPolicy::Include,
        )),
    );
    body.insert("stream".into(), json!(true));
    body.insert("store".into(), json!(false));
    if policy == ReasoningPolicy::Include {
        // Ask the server for ciphertext we can echo next turn (Grok Build / OpenAI).
        body.insert(
            "include".into(),
            json!(["reasoning.encrypted_content"]),
        );
    }
    if !session_id.is_empty() {
        body.insert("prompt_cache_key".into(), json!(session_id));
    }
    if let Some(max) = options.max_tokens.or(cfg.max_tokens) {
        body.insert("max_output_tokens".into(), json!(max));
    }
    if let Some(t) = options.temperature {
        body.insert("temperature".into(), json!(t));
    }
    // Per-model custom thinking level (`reasoning.effort`). Independent of
    // whether we also ask for encrypted_content replay (`include`).
    if let Some(effort) = resolve_wire_effort(model, options) {
        body.insert("reasoning".into(), json!({ "effort": effort }));
    }
    match &options.tool_choice {
        ToolChoice::Auto => {}
        ToolChoice::Required => {
            body.insert("tool_choice".into(), json!("required"));
        }
        ToolChoice::None => {
            body.insert("tool_choice".into(), json!("none"));
        }
        ToolChoice::Specific(name) => {
            body.insert(
                "tool_choice".into(),
                json!({ "type": "function", "name": name }),
            );
        }
    }
    if !tools.is_empty() {
        let t: Vec<Value> = tools
            .iter()
            .map(|td| {
                json!({
                    "type": "function",
                    "name": td.name,
                    "description": td.description,
                    "parameters": super::sanitize_schema_for_wire(&td.parameters),
                })
            })
            .collect();
        body.insert("tools".into(), json!(t));
    }
    Value::Object(body)
}

/// Map kernel history onto Responses `input[]`. Consecutive users stay consecutive.
fn format_input(messages: &[Message], supports_vision: bool, echo_reasoning: bool) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        match m.role {
            Role::System => {
                if !m.text.is_empty() {
                    out.push(json!({
                        "type": "message",
                        "role": "system",
                        "content": m.text,
                    }));
                }
            }
            Role::User => {
                if m.images.is_empty() || !supports_vision {
                    out.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": m.text,
                    }));
                } else {
                    let mut parts: Vec<Value> = Vec::new();
                    if !m.text.is_empty() {
                        parts.push(json!({ "type": "input_text", "text": m.text }));
                    }
                    for img in &m.images {
                        if img.data.is_empty() {
                            continue;
                        }
                        let media_type = if img.media_type.is_empty() {
                            "application/octet-stream"
                        } else {
                            img.media_type.as_str()
                        };
                        parts.push(json!({
                            "type": "input_image",
                            "detail": "auto",
                            "image_url": format!("data:{media_type};base64,{}", img.data),
                        }));
                    }
                    if parts.is_empty() {
                        out.push(json!({
                            "type": "message",
                            "role": "user",
                            "content": m.text,
                        }));
                    } else {
                        out.push(json!({
                            "type": "message",
                            "role": "user",
                            "content": parts,
                        }));
                    }
                }
            }
            Role::Assistant => {
                if echo_reasoning {
                    for block in &m.reasoning_blocks {
                        if block.provider.as_deref() != Some(RESPONSES_PROVIDER) {
                            continue;
                        }
                        let Some(enc) = block.opaque.as_deref().filter(|s| !s.is_empty()) else {
                            continue;
                        };
                        let mut item = Map::new();
                        item.insert("type".into(), json!("reasoning"));
                        if let Some(id) = block.id.as_deref().filter(|s| !s.is_empty()) {
                            item.insert("id".into(), json!(id));
                        }
                        item.insert("encrypted_content".into(), json!(enc));
                        if !block.text.is_empty() {
                            item.insert(
                                "summary".into(),
                                json!([{ "type": "summary_text", "text": block.text }]),
                            );
                        }
                        out.push(Value::Object(item));
                    }
                }
                if !m.text.is_empty() {
                    out.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": m.text,
                    }));
                }
                for tc in &m.tool_calls {
                    let args = if serde_json::from_str::<Value>(&tc.arguments).is_ok() {
                        tc.arguments.clone()
                    } else {
                        json!({ "input": tc.arguments }).to_string()
                    };
                    out.push(json!({
                        "type": "function_call",
                        "call_id": tc.id,
                        "name": tc.name,
                        "arguments": args,
                    }));
                }
            }
            Role::Tool => {
                let Some(id) = m.tool_call_id.as_deref().filter(|s| !s.is_empty()) else {
                    continue;
                };
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": id,
                    "output": m.text,
                }));
            }
        }
    }
    out
}

#[derive(Default)]
struct ResponsesSseDecoder {
    buf: Vec<u8>,
    pending_calls: std::collections::BTreeMap<u32, (String, String, String)>,
    /// output_index → (item id, encrypted_content), filled across added/done.
    pending_reasoning: std::collections::BTreeMap<u32, (Option<String>, Option<String>)>,
    emitted_done: bool,
}

impl ResponsesSseDecoder {
    fn feed(&mut self, chunk: &[u8]) -> Vec<StreamEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some((pos, delim)) = find_event_end(&self.buf) {
            let raw: Vec<u8> = self.buf.drain(..pos).collect();
            let n = delim.min(self.buf.len());
            self.buf.drain(..n);
            if let Some(evs) = self.parse_event(&raw) {
                out.extend(evs);
            }
        }
        out
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        if self.emitted_done {
            return Vec::new();
        }
        self.emitted_done = true;
        self.flush_calls()
            .into_iter()
            .chain(std::iter::once(StreamEvent::Done { truncated: false }))
            .collect()
    }

    fn parse_event(&mut self, raw: &[u8]) -> Option<Vec<StreamEvent>> {
        let text = std::str::from_utf8(raw).ok()?;
        let mut data = String::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.trim_start());
            }
        }
        if data.is_empty() || data == "[DONE]" {
            if data == "[DONE]" {
                self.emitted_done = true;
                let mut evs = self.flush_calls();
                evs.push(StreamEvent::Done { truncated: false });
                return Some(evs);
            }
            return None;
        }
        let v: Value = serde_json::from_str(&data).ok()?;
        self.dispatch(&v)
    }

    fn dispatch(&mut self, v: &Value) -> Option<Vec<StreamEvent>> {
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        match ty {
            "response.created" => {
                let id = v
                    .pointer("/response/id")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                let model = v
                    .pointer("/response/model")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                let mut evs = Vec::new();
                if let Some(id) = id {
                    evs.push(StreamEvent::ResponseId(id));
                }
                if let Some(model) = model {
                    evs.push(StreamEvent::ResponseModel(model));
                }
                Some(evs)
            }
            "response.output_text.delta" => v
                .get("delta")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|s| vec![StreamEvent::TextDelta(s.to_string())]),
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => v
                .get("delta")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|s| vec![StreamEvent::Reasoning(s.to_string())]),
            "response.function_call_arguments.delta" => {
                let index = v.get("output_index").and_then(Value::as_u64).unwrap_or(0) as u32;
                let delta = v.get("delta").and_then(Value::as_str).unwrap_or("");
                let entry = self
                    .pending_calls
                    .entry(index)
                    .or_insert_with(|| (String::new(), String::new(), String::new()));
                entry.2.push_str(delta);
                Some(vec![StreamEvent::ToolCallDelta {
                    index,
                    id: None,
                    name: None,
                    arguments: delta.to_string(),
                }])
            }
            "response.output_item.added" | "response.output_item.done" => {
                let item = v.get("item")?;
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                match item_type {
                    "function_call" => {
                        let index = v.get("output_index").and_then(Value::as_u64).unwrap_or(0) as u32;
                        let id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let args = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let entry = self
                            .pending_calls
                            .entry(index)
                            .or_insert_with(|| (String::new(), String::new(), String::new()));
                        if !id.is_empty() {
                            entry.0 = id;
                        }
                        if !name.is_empty() {
                            entry.1 = name;
                        }
                        if !args.is_empty() {
                            entry.2 = args;
                        }
                        if ty == "response.output_item.done" {
                            if let Some((id, name, arguments)) = self.pending_calls.remove(&index) {
                                return Some(vec![StreamEvent::ToolCall(ToolCall {
                                    id,
                                    name,
                                    arguments,
                                })]);
                            }
                        }
                        Some(vec![])
                    }
                    "reasoning" => {
                        let index =
                            v.get("output_index").and_then(Value::as_u64).unwrap_or(0) as u32;
                        let id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                        let enc = item
                            .get("encrypted_content")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                        let entry = self
                            .pending_reasoning
                            .entry(index)
                            .or_insert((None, None));
                        if id.is_some() {
                            entry.0 = id;
                        }
                        if enc.is_some() {
                            entry.1 = enc;
                        }
                        if ty != "response.output_item.done" {
                            return Some(vec![]);
                        }
                        let (id, enc) = self.pending_reasoning.remove(&index).unwrap_or((None, None));
                        let Some(enc) = enc else {
                            return Some(vec![]);
                        };
                        Some(vec![StreamEvent::ReasoningSignature {
                            opaque: enc,
                            provider: RESPONSES_PROVIDER.to_string(),
                            id,
                        }])
                    }
                    _ => Some(vec![]),
                }
            }
            "response.completed" => {
                let mut evs = self.flush_calls();
                if let Some(usage) = v.pointer("/response/usage") {
                    evs.push(StreamEvent::Usage(parse_usage(usage)));
                }
                self.emitted_done = true;
                evs.push(StreamEvent::Done { truncated: false });
                Some(evs)
            }
            "response.failed" | "error" => {
                let msg = v
                    .pointer("/response/error/message")
                    .or_else(|| v.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("responses error")
                    .to_string();
                Some(vec![StreamEvent::Error(ProviderError {
                    retryable: false,
                    message: msg,
                    ..Default::default()
                })])
            }
            _ => None,
        }
    }

    fn flush_calls(&mut self) -> Vec<StreamEvent> {
        let keys: Vec<u32> = self.pending_calls.keys().copied().collect();
        keys.into_iter()
            .filter_map(|k| self.pending_calls.remove(&k))
            .filter(|(id, name, _)| !id.is_empty() || !name.is_empty())
            .map(|(id, name, arguments)| StreamEvent::ToolCall(ToolCall { id, name, arguments }))
            .collect()
    }
}

fn parse_usage(usage: &Value) -> TokenUsage {
    let prompt = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let completion = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let cached = usage
        .pointer("/input_tokens_details/cached_tokens")
        .or_else(|| usage.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    TokenUsage {
        prompt,
        completion,
        cached,
    }
}

fn find_event_end(buf: &[u8]) -> Option<(usize, usize)> {
    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((i, 4));
    }
    buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::message::{Message, ReasoningBlock};
    use atomcode_kernel::provider::ReasoningEffort;

    #[test]
    fn format_input_keeps_consecutive_users_and_native_tool_items() {
        let msgs = vec![
            Message::system("sys"),
            Message::synthetic_user("<system-reminder>\ndate\n</system-reminder>"),
            Message::user("query"),
            Message::assistant(
                "hi",
                vec![ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    arguments: "{\"cmd\":\"ls\"}".into(),
                }],
            ),
            Message::tool_result("c1", "ok", false),
        ];
        let out = format_input(&msgs, false, true);
        assert_eq!(out[0]["type"], "message");
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[2]["role"], "user");
        assert_eq!(out[2]["content"], "query");
        assert_eq!(out[3]["type"], "message");
        assert_eq!(out[4]["type"], "function_call");
        assert_eq!(out[4]["call_id"], "c1");
        assert_eq!(out[5]["type"], "function_call_output");
        assert_eq!(out[5]["call_id"], "c1");
    }

    #[test]
    fn format_input_replays_encrypted_reasoning_before_assistant() {
        let mut a = Message::assistant("done", vec![]);
        a.reasoning_blocks = vec![ReasoningBlock {
            text: "thought".into(),
            opaque: Some("cipher".into()),
            provider: Some(RESPONSES_PROVIDER.into()),
            id: Some("rs_6820f383d7c9a1b2".into()),
        }];
        let out = format_input(&[Message::user("q"), a], false, true);
        assert_eq!(out[1]["type"], "reasoning");
        assert_eq!(out[1]["id"], "rs_6820f383d7c9a1b2");
        assert_eq!(out[1]["encrypted_content"], "cipher");
        assert_eq!(out[2]["role"], "assistant");
        assert_eq!(out[2]["content"], "done");
    }

    #[test]
    fn prefix_is_append_only_across_turns() {
        let h1 = vec![
            Message::system("s"),
            Message::synthetic_user("<system-reminder>\nd\n</system-reminder>"),
            Message::user("u1"),
        ];
        let mut h2 = h1.clone();
        h2.push(Message::assistant("a1", vec![]));
        let f1 = format_input(&h1, false, true);
        let f2 = format_input(&h2, false, true);
        for i in 0..f1.len() {
            assert_eq!(
                serde_json::to_string(&f1[i]).unwrap(),
                serde_json::to_string(&f2[i]).unwrap()
            );
        }
    }

    #[test]
    fn body_includes_cache_key_and_encrypted_reasoning() {
        let cfg = ResponsesConfig::new("k", "https://api.x.ai/v1", "grok-4.6");
        let body = build_request_body(
            "grok-4.6",
            &[Message::user("hi")],
            &[],
            &ChatOptions::default(),
            &cfg,
            ReasoningPolicy::Include,
            "sess-1",
        );
        assert_eq!(body["prompt_cache_key"], "sess-1");
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn custom_effort_reaches_responses_wire_for_any_model() {
        let cfg = ResponsesConfig::new("k", "https://example.invalid/v1", "my-reasoner");
        let body = build_request_body(
            "my-reasoner",
            &[Message::user("hi")],
            &[],
            &ChatOptions {
                reasoning_effort: Some(ReasoningEffort::Custom("xhigh".into())),
                ..Default::default()
            },
            &cfg,
            ReasoningPolicy::Include,
            "sess-1",
        );
        assert_eq!(body["reasoning"]["effort"], "xhigh");
    }

    #[test]
    fn exclude_policy_omits_encrypted_replay_but_keeps_effort() {
        let mut cfg = ResponsesConfig::new("k", "https://api.x.ai/v1", "grok-4.6");
        cfg.reasoning_model = Some(false);
        let mut a = Message::assistant("done", vec![]);
        a.reasoning_blocks = vec![ReasoningBlock {
            text: "thought".into(),
            opaque: Some("cipher".into()),
            provider: Some(RESPONSES_PROVIDER.into()),
            id: Some("rs_1".into()),
        }];
        let body = build_request_body(
            "grok-4.6",
            &[Message::user("q"), a],
            &[],
            &ChatOptions {
                reasoning_effort: Some(ReasoningEffort::High),
                ..Default::default()
            },
            &cfg,
            ReasoningPolicy::Exclude,
            "sess-1",
        );
        assert!(body.get("include").is_none());
        assert_eq!(
            body["reasoning"]["effort"], "high",
            "custom/configured effort still rides the wire when echo is off"
        );
        let input = body["input"].as_array().unwrap();
        assert!(
            input.iter().all(|i| i["type"] != "reasoning"),
            "must not echo encrypted reasoning when not a thinking model: {input:?}"
        );
    }

    #[test]
    fn base64_images_go_out_as_input_image_when_vision_is_on() {
        use atomcode_kernel::message::ImageContent;
        let mut user = Message::user("look");
        user.images = vec![ImageContent {
            media_type: "image/png".into(),
            data: "QUJD".into(),
        }];
        let off = format_input(&[user.clone()], false, false);
        assert_eq!(off[0]["content"], "look", "vision-off drops image bytes");
        let on = format_input(&[user], true, false);
        let parts = on[0]["content"].as_array().expect("multimodal array");
        assert_eq!(parts[0]["type"], "input_text");
        assert_eq!(parts[1]["type"], "input_image");
        assert_eq!(
            parts[1]["image_url"],
            "data:image/png;base64,QUJD"
        );
    }

    #[test]
    fn sse_decoder_emits_text_tool_reasoning_and_usage() {
        let mut dec = ResponsesSseDecoder::default();
        let chunk = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"bash\",\"arguments\":\"{}\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_stream_01\",\"encrypted_content\":\"enc\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":3,\"input_tokens_details\":{\"cached_tokens\":8}}}}\n\n"
        );
        let evs = dec.feed(chunk.as_bytes());
        assert!(evs.iter().any(|e| matches!(e, StreamEvent::TextDelta(s) if s == "hi")));
        assert!(evs.iter().any(|e| matches!(e, StreamEvent::ToolCall(tc) if tc.id == "c1" && tc.name == "bash")));
        assert!(evs.iter().any(|e| matches!(e, StreamEvent::ReasoningSignature { opaque, provider, id } if opaque == "enc" && provider == RESPONSES_PROVIDER && id.as_deref() == Some("rs_stream_01"))));
        assert!(evs.iter().any(|e| matches!(e, StreamEvent::Usage(u) if u.prompt == 10 && u.cached == 8)));
        assert!(evs.iter().any(|e| matches!(e, StreamEvent::Done { .. })));
    }
}
