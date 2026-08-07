//! OpenAI- and Anthropic-compatible HTTP surface for `atomcode serve`.
//!
//! Endpoints (same host/port as WebUI, behind the same token gate):
//! - `GET  /v1/models`              — OpenAI model list (`id` = `account/model`)
//! - `GET  /v1/models/*id`          — OpenAI model retrieve (slash-safe)
//! - `POST /v1/chat/completions`    — OpenAI Chat Completions
//! - `POST /v1/responses`           — OpenAI Responses API (subset)
//! - `POST /v1/messages`            — Anthropic Messages
//! - `GET  /v1/sessions`            — list AtomCode sessions (filter by `user` title)
//! - `GET  /v1/sessions/:id`        — session detail
//!
//! Design (AtomCode core, not a dumb proxy):
//! - Client multi-turn history is **ignored**; only the latest user query is admitted.
//! - System prompts from the request are appended **after** AGENTS.md / glossary / db packs.
//! - OpenAI/Anthropic `user` is a client session key (`alice_1`, `chat-2`): same key
//!   resumes, different key creates a new session; omit for ephemeral.
//! - Request `model` is resolved every turn so model switches take effect immediately.
//! - Stream events include thinking, text, parallel tool calls, and subagent progress.
//! - Tools are parallel-safe (stable call `id`); `task` children surface as
//!   structured `progress` / `children` patches on the parent tool call.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::approval_mode::ApprovalMode;
use crate::{
    fanout_chat_events, process_chat_request, public_compat_model_id, resolve_chat_provider,
    ActiveChatAdmissionError, AppState, ChatEvent, ChatRequest, ImageInput, SessionSummary,
};
use atomcode_config::config::Config;
use atomcode_telemetry::{CurrentContext, SessionMode};

#[path = "compat_stream.rs"]
mod compat_stream;
use compat_stream::CompatProjector;

// ─── Shared turn intake ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CompatTurn {
    message: String,
    images: Vec<ImageInput>,
    system_append: Option<String>,
    /// Client-controlled session key from OpenAI/Anthropic `user`.
    ///
    /// Distinct non-empty keys map 1:1 to AtomCode sessions (exact name match).
    /// Clients typically use stable ids such as `tenant_alice`, `chat-42`,
    /// `user123_proj-x`. Different `user` ⇒ new session; same `user` ⇒ resume.
    /// Omitted / blank ⇒ ephemeral session (new UUID every request).
    session_key: Option<String>,
    /// Resolved AtomCode selection id (or None → config default).
    model_selection: Option<String>,
    /// Wire model string echoed back to the client.
    model_wire: String,
    stream: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionListQuery {
    /// Client session key filter (`user=xxx_xxx` / `user=xxx-xxx`). Exact match.
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

// ─── OpenAI Chat Completions request ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiChatRequest {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    messages: Vec<OpenAiMessage>,
    #[serde(default)]
    stream: Option<bool>,
    /// Client session key. AtomCode maps each distinct value to one session
    /// (exact name match). Prefer stable ids: `user_42`, `acct-7_chat-3`.
    /// Change the key to start a brand-new conversation; reuse it to continue.
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    role: String,
    #[serde(default)]
    content: Option<OpenAiContent>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

#[derive(Debug, Deserialize)]
struct OpenAiContentPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    image_url: Option<OpenAiImageUrl>,
    /// Some clients put base64 under `image_url` as data URL; others use Anthropic-like fields.
    #[serde(default)]
    source: Option<OpenAiImageSource>,
}

#[derive(Debug, Deserialize)]
struct OpenAiImageUrl {
    url: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiImageSource {
    #[serde(default)]
    #[serde(rename = "type")]
    #[allow(dead_code)]
    kind: Option<String>,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

// ─── OpenAI Responses API request (subset) ──────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiResponsesRequest {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    input: Option<ResponsesInput>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
    /// Full chat-style messages some SDKs still send.
    #[serde(default)]
    messages: Vec<OpenAiMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ResponsesInput {
    Text(String),
    Items(Vec<ResponsesInputItem>),
}

#[derive(Debug, Deserialize)]
struct ResponsesInputItem {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<OpenAiContent>,
    #[serde(default)]
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

// ─── Anthropic Messages request ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicMessagesRequest {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    system: Option<AnthropicSystem>,
    #[serde(default)]
    messages: Vec<AnthropicMessage>,
    #[serde(default)]
    stream: Option<bool>,
    /// Non-standard but useful: same as OpenAI `user` for session title.
    #[serde(default)]
    metadata: Option<AnthropicMetadata>,
    #[serde(default)]
    user: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicMetadata {
    #[serde(default)]
    user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AnthropicSystem {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    source: Option<AnthropicImageSource>,
}

#[derive(Debug, Deserialize)]
struct AnthropicImageSource {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

// ─── Model / session wire types ─────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct OpenAiModelList {
    object: &'static str,
    data: Vec<OpenAiModel>,
}

#[derive(Debug, Serialize)]
struct OpenAiModel {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: String,
    /// AtomCode extension: underlying wire model id.
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_type: Option<String>,
}

#[derive(Debug, Serialize)]
struct AnthropicModelList {
    data: Vec<AnthropicModel>,
    has_more: bool,
    first_id: Option<String>,
    last_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct AnthropicModel {
    #[serde(rename = "type")]
    kind: &'static str,
    id: String,
    display_name: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct CompatSessionList {
    object: &'static str,
    data: Vec<CompatSession>,
}

#[derive(Debug, Serialize)]
struct CompatSession {
    id: String,
    object: &'static str,
    /// Session title (= OpenAI `user` / user_title when set).
    title: String,
    created: u64,
    updated: u64,
    message_count: usize,
    project_hash: String,
    working_dir: String,
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: ApiErrorDetail,
}

#[derive(Debug, Serialize)]
struct ApiErrorDetail {
    message: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}

fn api_error(status: StatusCode, message: impl Into<String>, code: &str) -> Response {
    (
        status,
        Json(ApiErrorBody {
            error: ApiErrorDetail {
                message: message.into(),
                kind: "invalid_request_error".into(),
                code: Some(code.into()),
            },
        }),
    )
        .into_response()
}

pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── Models ─────────────────────────────────────────────────────────────────

/// One catalog row for compat `/v1/models` — public id is always `account/model`.
struct CompatModelRow {
    /// Stable external id: `{account}/{wire_model}`.
    public_id: String,
    /// Internal catalog selection id (may be hyphenated CodingPlan key).
    #[cfg_attr(not(test), allow(dead_code))]
    selection_id: String,
    account: String,
    wire_model: String,
    provider_type: String,
    #[allow(dead_code)]
    is_default: bool,
}

/// Build the compat model catalog with unified `account/model` public ids.
///
/// Internal selection keys stay as-is for runtime resolution; only the **exposed**
/// `id` is normalized so clients never mix `AtomGit-GLM-5.2` with `acc/ds`.
fn compat_models_from_config(config: &Config) -> Vec<CompatModelRow> {
    let default_selection = config.effective_model_selection().unwrap_or_default();
    let mut entries: Vec<(String, _)> = config.logical_models().into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
        .into_iter()
        .filter_map(|(selection_id, profile)| {
            let provider = config.provider_config_for_selection(&selection_id)?;
            let account = profile.account.clone();
            let wire_model = profile.model.clone();
            Some(CompatModelRow {
                public_id: public_compat_model_id(&account, &wire_model),
                selection_id: selection_id.clone(),
                account,
                wire_model,
                provider_type: provider.provider_type,
                is_default: selection_id == default_selection,
            })
        })
        .collect()
}

fn load_config() -> Result<Config, String> {
    Config::load(&Config::default_path()).map_err(|e| e.to_string())
}

/// GET /v1/models — OpenAI list (`id` = `account/model`).
pub(crate) async fn openai_list_models() -> Response {
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e, "config_error"),
    };
    let created = now_unix();
    let data = compat_models_from_config(&config)
        .into_iter()
        .map(|m| OpenAiModel {
            id: m.public_id,
            object: "model",
            created,
            owned_by: m.account,
            root: Some(m.wire_model),
            provider_type: Some(m.provider_type),
        })
        .collect();
    Json(OpenAiModelList {
        object: "list",
        data,
    })
    .into_response()
}

/// GET /v1/models/*id — OpenAI retrieve (supports slash in `account/model`).
pub(crate) async fn openai_get_model(Path(id): Path<String>) -> Response {
    // Catch-all may leave a leading slash depending on router; normalize.
    let id = id.trim_start_matches('/').trim().to_string();
    if id.is_empty() {
        return api_error(StatusCode::NOT_FOUND, "model id is empty", "model_not_found");
    }
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e, "config_error"),
    };
    match resolve_chat_provider(&config, Some(id)) {
        Ok((selection, provider)) => {
            // Prefer account from resolved catalog when available.
            let account = config
                .logical_models()
                .get(&selection)
                .map(|p| p.account.clone())
                .unwrap_or_else(|| selection.clone());
            let public_id = public_compat_model_id(&account, &provider.model);
            Json(OpenAiModel {
                id: public_id,
                object: "model",
                created: now_unix(),
                owned_by: account,
                root: Some(provider.model),
                provider_type: Some(provider.provider_type),
            })
            .into_response()
        }
        Err(e) => api_error(StatusCode::NOT_FOUND, e.to_string(), "model_not_found"),
    }
}

/// GET /v1/anthropic/models — Anthropic-shaped list; same `account/model` ids.
pub(crate) async fn anthropic_list_models() -> Response {
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e, "config_error"),
    };
    let models: Vec<AnthropicModel> = compat_models_from_config(&config)
        .into_iter()
        .map(|m| AnthropicModel {
            kind: "model",
            id: m.public_id.clone(),
            display_name: format!("{} / {}", m.account, m.wire_model),
            created_at: chrono_like_now(),
        })
        .collect();
    let first_id = models.first().map(|m| m.id.clone());
    let last_id = models.last().map(|m| m.id.clone());
    Json(AnthropicModelList {
        data: models,
        has_more: false,
        first_id,
        last_id,
    })
    .into_response()
}

fn chrono_like_now() -> String {
    // RFC3339-ish without chrono dep: epoch seconds as ISO-ish string is enough
    // for list display; clients rarely parse this strictly.
    format!("{}Z", now_unix())
}

// ─── Sessions ───────────────────────────────────────────────────────────────

fn list_project_sessions(working_dir: &std::path::Path) -> Vec<(String, SessionSummary)> {
    let root = atomcode_capabilities::session::SessionManager::sessions_root();
    let project_hash =
        atomcode_capabilities::session::SessionManager::project_hash(working_dir);
    let scan = atomcode_capabilities::session::SessionManager::scan_catalog(&root);
    let mut out = Vec::new();
    for entry in scan.entries {
        if entry.project_bucket != project_hash {
            continue;
        }
        let summary = SessionSummary {
            id: entry.id.clone(),
            name: entry.name.clone(),
            working_dir: entry.working_dir.clone(),
            created_at: u64::try_from(entry.created_at_ms.max(0)).unwrap_or(0) / 1_000,
            updated_at: u64::try_from(entry.updated_at_ms.max(0)).unwrap_or(0) / 1_000,
            message_count: entry.message_count,
            file_size: 0,
        };
        out.push((entry.project_bucket, summary));
    }
    out.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
    out
}

/// Normalize a client `user` session key.
///
/// - Trims only leading/trailing whitespace (preserves `_`, `-`, digits, unicode).
/// - Empty after trim ⇒ `None` (ephemeral: new session every request).
/// - Any other non-empty string is a stable key: `alice_proj1`, `chat-42`, `u1_t2`.
fn normalize_session_key(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Find the most recently updated session whose **name** equals the client session key.
fn find_session_id_by_key(working_dir: &std::path::Path, key: &str) -> Option<String> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    list_project_sessions(working_dir)
        .into_iter()
        .find(|(_, s)| s.name == key)
        .map(|(_, s)| s.id)
}

/// Resolve session for a client key:
/// - key present + existing name match → resume that session id
/// - key present + no match → create new session named exactly as the key
/// - key absent → ephemeral (no id / no name)
fn resolve_session_for_key(
    working_dir: &std::path::Path,
    session_key: Option<&str>,
) -> (Option<String>, Option<String>) {
    let Some(key) = normalize_session_key(session_key) else {
        return (None, None);
    };
    let existing = find_session_id_by_key(working_dir, &key);
    (existing, Some(key))
}

/// GET /v1/sessions?user=<session_key>
pub(crate) async fn list_sessions(
    State(state): State<AppState>,
    Query(q): Query<SessionListQuery>,
) -> Response {
    let working_dir = state.project.read().await.working_dir.clone();
    let limit = q.limit.unwrap_or(100).min(500);
    let filter = normalize_session_key(q.user.as_deref());
    let data: Vec<CompatSession> = list_project_sessions(&working_dir)
        .into_iter()
        .filter(|(_, s)| filter.as_ref().map(|t| s.name == *t).unwrap_or(true))
        .take(limit)
        .map(|(project_hash, s)| CompatSession {
            id: s.id,
            object: "session",
            title: s.name,
            created: s.created_at,
            updated: s.updated_at,
            message_count: s.message_count,
            project_hash,
            working_dir: s.working_dir.display().to_string(),
        })
        .collect();
    Json(CompatSessionList {
        object: "list",
        data,
    })
    .into_response()
}

/// GET /v1/sessions/:id  (UUID, prefix, or exact session key / `user` value)
pub(crate) async fn get_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let working_dir = state.project.read().await.working_dir.clone();
    let key = normalize_session_key(Some(id.as_str())).unwrap_or(id.clone());
    match list_project_sessions(&working_dir)
        .into_iter()
        .find(|(_, s)| s.id == key || s.id.starts_with(&key) || s.name == key)
    {
        Some((project_hash, s)) => Json(CompatSession {
            id: s.id,
            object: "session",
            title: s.name,
            created: s.created_at,
            updated: s.updated_at,
            message_count: s.message_count,
            project_hash,
            working_dir: s.working_dir.display().to_string(),
        })
        .into_response(),
        None => api_error(StatusCode::NOT_FOUND, "session not found", "session_not_found"),
    }
}

// ─── Content extraction ─────────────────────────────────────────────────────

async fn resolve_image_url(url: &str) -> Result<ImageInput, String> {
    let url = url.trim();
    if let Some(rest) = url.strip_prefix("data:") {
        // data:[<mediatype>][;base64],<data>
        let (meta, data) = rest
            .split_once(',')
            .ok_or_else(|| "invalid data URL".to_string())?;
        let media_type = meta
            .split(';')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("image/png")
            .to_string();
        let raw = if meta.contains(";base64") {
            data.to_string()
        } else {
            // percent-encoded payload — rare for images; keep as-is base64-looking
            data.to_string()
        };
        // Validate base64 lightly
        use base64::Engine;
        let _ = base64::engine::general_purpose::STANDARD
            .decode(raw.as_bytes())
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(raw.as_bytes()))
            .map_err(|e| format!("invalid base64 image: {e}"))?;
        return Ok(ImageInput {
            media_type,
            data: raw,
        });
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("fetch image_url failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("fetch image_url HTTP {}", resp.status()));
        }
        let media_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
            .filter(|s| s.starts_with("image/"))
            .unwrap_or_else(|| "image/png".into());
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("read image_url body failed: {e}"))?;
        if bytes.len() > 20 * 1024 * 1024 {
            return Err("image_url exceeds 20MB".into());
        }
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok(ImageInput { media_type, data });
    }
    Err("unsupported image_url scheme (use data: or http(s):)".into())
}

async fn collect_openai_content(
    content: &OpenAiContent,
) -> Result<(String, Vec<ImageInput>), String> {
    match content {
        OpenAiContent::Text(t) => Ok((t.clone(), Vec::new())),
        OpenAiContent::Parts(parts) => {
            let mut text = String::new();
            let mut images = Vec::new();
            for p in parts {
                match p.kind.as_str() {
                    "text" => {
                        if let Some(t) = &p.text {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                    }
                    "image_url" => {
                        if let Some(img) = &p.image_url {
                            images.push(resolve_image_url(&img.url).await?);
                        } else if let Some(src) = &p.source {
                            if let Some(data) = &src.data {
                                images.push(ImageInput {
                                    media_type: src
                                        .media_type
                                        .clone()
                                        .unwrap_or_else(|| "image/png".into()),
                                    data: data.clone(),
                                });
                            } else if let Some(url) = &src.url {
                                images.push(resolve_image_url(url).await?);
                            }
                        }
                    }
                    "image" => {
                        if let Some(src) = &p.source {
                            if let Some(data) = &src.data {
                                images.push(ImageInput {
                                    media_type: src
                                        .media_type
                                        .clone()
                                        .unwrap_or_else(|| "image/png".into()),
                                    data: data.clone(),
                                });
                            } else if let Some(url) = &src.url {
                                images.push(resolve_image_url(url).await?);
                            }
                        } else if let Some(img) = &p.image_url {
                            images.push(resolve_image_url(&img.url).await?);
                        }
                    }
                    _ => {}
                }
            }
            Ok((text, images))
        }
    }
}

async fn extract_openai_turn(
    messages: &[OpenAiMessage],
) -> Result<(Option<String>, String, Vec<ImageInput>), String> {
    let mut systems = Vec::new();
    let mut last_user: Option<&OpenAiMessage> = None;
    for m in messages {
        match m.role.to_ascii_lowercase().as_str() {
            "system" | "developer" => {
                if let Some(c) = &m.content {
                    let (t, _) = collect_openai_content(c).await?;
                    if !t.trim().is_empty() {
                        systems.push(t);
                    }
                }
            }
            "user" => last_user = Some(m),
            _ => {}
        }
    }
    let user = last_user.ok_or_else(|| "no user message in request".to_string())?;
    let content = user
        .content
        .as_ref()
        .ok_or_else(|| "user message has empty content".to_string())?;
    let (text, images) = collect_openai_content(content).await?;
    if text.trim().is_empty() && images.is_empty() {
        return Err("latest user message is empty".into());
    }
    let system = if systems.is_empty() {
        None
    } else {
        Some(systems.join("\n\n"))
    };
    Ok((system, text, images))
}

async fn extract_anthropic_turn(
    system: &Option<AnthropicSystem>,
    messages: &[AnthropicMessage],
) -> Result<(Option<String>, String, Vec<ImageInput>), String> {
    let mut system_text = None;
    if let Some(sys) = system {
        match sys {
            AnthropicSystem::Text(t) if !t.trim().is_empty() => system_text = Some(t.clone()),
            AnthropicSystem::Blocks(blocks) => {
                let mut parts = Vec::new();
                for b in blocks {
                    if b.kind == "text" {
                        if let Some(t) = &b.text {
                            if !t.trim().is_empty() {
                                parts.push(t.clone());
                            }
                        }
                    }
                }
                if !parts.is_empty() {
                    system_text = Some(parts.join("\n\n"));
                }
            }
            _ => {}
        }
    }

    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.role.eq_ignore_ascii_case("user"))
        .ok_or_else(|| "no user message in request".to_string())?;

    let (text, images) = match &last_user.content {
        AnthropicContent::Text(t) => (t.clone(), Vec::new()),
        AnthropicContent::Blocks(blocks) => {
            let mut text = String::new();
            let mut images = Vec::new();
            for b in blocks {
                match b.kind.as_str() {
                    "text" => {
                        if let Some(t) = &b.text {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                    }
                    "image" => {
                        if let Some(src) = &b.source {
                            match src.kind.as_str() {
                                "base64" => {
                                    images.push(ImageInput {
                                        media_type: src
                                            .media_type
                                            .clone()
                                            .unwrap_or_else(|| "image/png".into()),
                                        data: src.data.clone().unwrap_or_default(),
                                    });
                                }
                                "url" => {
                                    if let Some(url) = &src.url {
                                        images.push(resolve_image_url(url).await?);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            (text, images)
        }
    };
    if text.trim().is_empty() && images.is_empty() {
        return Err("latest user message is empty".into());
    }
    Ok((system_text, text, images))
}

/// Extract client session key from OpenAI/Anthropic `user` (preferred) or metadata.
///
/// Accepted shapes (all preserved exactly after trim):
/// - `user: "alice_proj1"`
/// - `user: "chat-42"`
/// - `metadata.user` / `metadata.user_id` / `metadata.session_key`
fn session_key_from_request(user: &Option<String>, meta: &Option<Value>) -> Option<String> {
    if let Some(key) = normalize_session_key(user.as_deref()) {
        return Some(key);
    }
    meta.as_ref().and_then(|m| {
        m.get("user")
            .or_else(|| m.get("user_id"))
            .or_else(|| m.get("user_title"))
            .or_else(|| m.get("session_key"))
            .or_else(|| m.get("session_title"))
            .and_then(|v| v.as_str())
            .and_then(|s| normalize_session_key(Some(s)))
    })
}

fn resolve_wire_model(config: &Config, requested: Option<String>) -> Result<(String, String), String> {
    let (selection, provider) =
        resolve_chat_provider(config, requested).map_err(|e| e.to_string())?;
    Ok((selection, provider.model))
}

// ─── Drive a turn and project events ────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub(super) enum WireFormat {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
}

async fn run_compat_turn(
    state: AppState,
    turn: CompatTurn,
    format: WireFormat,
) -> Response {
    let working_dir = state.project.read().await.working_dir.clone();
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, e, "config_error"),
    };
    // Model resolved every request so a client model-id change takes effect immediately.
    let (selection, wire_model) = match resolve_wire_model(&config, turn.model_selection.clone()) {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::BAD_REQUEST, e, "model_not_found"),
    };
    let model_wire = if turn.model_wire.is_empty() {
        selection.clone()
    } else {
        turn.model_wire
    };
    let _ = wire_model;

    // Client `user` is the session key:
    //   user=alice_a  → resume or create session named "alice_a"
    //   user=alice-b  → different key → different session
    //   (omit user)   → stable default key "default" (automation-friendly)
    let session_key_raw = turn
        .session_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    let (session_id, session_key) =
        resolve_session_for_key(&working_dir, Some(session_key_raw));

    // Always Auto for OpenAI/Anthropic API; serve --yolo additionally disables
    // interactive user-input modals process-wide via AppState.yolo.
    let chat_req = ChatRequest {
        message: turn.message,
        working_dir: Some(working_dir),
        provider: Some(selection.clone()),
        session_id,
        request_id: None,
        images: turn.images,
        approval_mode: Some(ApprovalMode::Auto),
        extra_system_append: turn.system_append,
        // On create, native runtime names the session exactly as this key
        // (`user_renamed=true`) so later lookups by `user` stay stable.
        session_title: session_key.clone(),
    };


    let admission = match state
        .active_chats
        .admit(chat_req.session_id.as_deref(), None)
        .await
    {
        Ok(a) => a,
        Err(ActiveChatAdmissionError::SessionBusy) => {
            return api_error(
                StatusCode::CONFLICT,
                "session is busy with another turn",
                "session_busy",
            );
        }
        Err(ActiveChatAdmissionError::RequestBusy) => {
            return api_error(
                StatusCode::CONFLICT,
                "request is busy",
                "request_busy",
            );
        }
    };

    let (client_tx, rx) = mpsc::unbounded_channel::<ChatEvent>();
    let operation_id = admission.operation_id.clone();
    let cancel_token = admission.cancellation;
    let event_bus = state
        .active_chats
        .event_bus(&admission.operation_id)
        .await
        .expect("just admitted operation always has an event bus");
    let replay = state
        .active_chats
        .event_bus_with_replay(&admission.operation_id)
        .await
        .map(|(_, r)| r);
    let fan_tx = fanout_chat_events(client_tx, event_bus, replay);
    let active_chats = state.active_chats.clone();
    let mcp_cache = state.mcp_cache.clone();
    let telemetry = state.telemetry.clone();
    let pending_permissions = state.pending_permissions.clone();
    let pending_user_inputs = state.pending_user_inputs.clone();
    let terminal_sent = Arc::new(AtomicBool::new(false));
    let terminal_sent_inner = terminal_sent.clone();
    let active_conns = state.active_connections.clone();
    active_conns.fetch_add(1, Ordering::Relaxed);

    // API-owned turn: low-confirm automation. WebUI /chat/watch only observes
    // the same event bus — no permission / user-input modals are emitted here.
    // (`serve --yolo` is identical for API; it also forces native WebUI/TUI paths.)
    let policy = crate::ChatTurnPolicy::resolve(
        state.yolo,
        crate::ChatTurnOrigin::Api,
        SessionMode::Channel,
        state.enforce_token,
        &state.bind_host,
    );
    debug_assert_eq!(
        policy.force_approval_mode,
        Some(crate::approval_mode::ApprovalMode::Auto),
        "API turns must force Auto approval"
    );
    debug_assert!(!policy.interactive_permission && !policy.interactive_user_input);
    tracing::info!(
        yolo = state.yolo,
        enforce_token = state.enforce_token,
        session_key = %session_key_raw,
        "compat turn: API policy (Auto approval, no permission/user-input modals)"
    );

    let ctx = CurrentContext {
        mode: Some(SessionMode::Channel),
        session_id: chat_req
            .session_id
            .as_deref()
            .and_then(|s| uuid::Uuid::parse_str(s).ok()),
        ..CurrentContext::current()
    };

    let cleanup_chats = active_chats.clone();
    let cleanup_op = operation_id.clone();
    let err_tx = fan_tx.clone();
    let interactive_permission = policy.interactive_permission;
    let interactive_user_input = policy.interactive_user_input;
    tokio::spawn(async move {
        let result = CurrentContext::scope(ctx, || async move {
            process_chat_request(
                chat_req,
                fan_tx,
                cancel_token,
                operation_id,
                active_chats,
                mcp_cache,
                telemetry,
                pending_permissions,
                pending_user_inputs,
                interactive_permission,
                interactive_user_input,
                terminal_sent_inner,
            )
            .await
        })
        .await;
        if let Err(e) = result {
            let _ = err_tx.send(ChatEvent::Error {
                message: e.to_string(),
            });
        }
        cleanup_chats.complete(&cleanup_op).await;
    });

    if turn.stream {
        stream_compat_response(rx, format, model_wire, session_key, active_conns).await
    } else {
        collect_compat_response(rx, format, model_wire, session_key, active_conns).await
    }
}

async fn stream_compat_response(
    rx: mpsc::UnboundedReceiver<ChatEvent>,
    format: WireFormat,
    model: String,
    session_key: Option<String>,
    active_conns: Arc<std::sync::atomic::AtomicUsize>,
) -> Response {
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let created = now_unix();
    let mut projector = match format {
        WireFormat::OpenAiChat => CompatProjector::openai_chat(id, model, created, session_key),
        WireFormat::OpenAiResponses => {
            CompatProjector::openai_responses(id, model, created, session_key)
        }
        WireFormat::Anthropic => CompatProjector::anthropic(id, model, session_key),
    };

    let stream = UnboundedReceiverStream::new(rx).map(move |event| {
        let chunks = projector.project(event);
        stream::iter(chunks.into_iter().map(|c| {
            let mut ev = Event::default().data(c.data);
            if let Some(name) = c.event {
                ev = ev.event(name);
            }
            Ok::<_, std::convert::Infallible>(ev)
        }))
    });
    let flat = stream.flatten();
    let guard = active_conns;
    let guarded = flat.chain(stream::once(async move {
        guard.fetch_sub(1, Ordering::Relaxed);
        Ok(Event::default().comment("bye"))
    }));

    Sse::new(guarded)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
        .into_response()
}

async fn collect_compat_response(
    mut rx: mpsc::UnboundedReceiver<ChatEvent>,
    format: WireFormat,
    model: String,
    session_key: Option<String>,
    active_conns: Arc<std::sync::atomic::AtomicUsize>,
) -> Response {
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let created = now_unix();
    let mut projector = match format {
        WireFormat::OpenAiChat => {
            CompatProjector::openai_chat(id.clone(), model.clone(), created, session_key.clone())
        }
        WireFormat::OpenAiResponses => CompatProjector::openai_responses(
            id.clone(),
            model.clone(),
            created,
            session_key.clone(),
        ),
        WireFormat::Anthropic => {
            CompatProjector::anthropic(id.clone(), model.clone(), session_key.clone())
        }
    };

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_trace = String::new();
    let mut session_id = String::new();
    let mut error: Option<String> = None;
    let mut tokens = 0usize;

    while let Some(event) = rx.recv().await {
        // Still run projector for any side effects / ordering consistency.
        let _ = projector.project(event.clone());
        match event {
            ChatEvent::TextDelta { content } => text.push_str(&content),
            ChatEvent::ReasoningDelta { content } => reasoning.push_str(&content),
            ChatEvent::ToolBatchStarted { calls } => {
                for c in calls {
                    tool_trace.push_str(&format!(
                        "\n[tool_start] {} ({}) parallel\n{}\n",
                        c.name, c.id, c.arguments
                    ));
                }
            }
            ChatEvent::ToolCallStarted { id, name, arguments } => {
                tool_trace.push_str(&format!("\n[tool_start] {name} ({id})\n{arguments}\n"));
            }
            ChatEvent::ToolOutputChunk { id, chunk } => {
                tool_trace.push_str(&format!("[{id}]{chunk}"));
            }
            ChatEvent::ToolProgress { id, progress } => {
                tool_trace.push_str(&format!("\n[progress {id}] {progress}\n"));
            }
            ChatEvent::ToolCallResult {
                id,
                name,
                output,
                success,
                ..
            } => {
                tool_trace.push_str(&format!(
                    "\n[tool_result] {name} ({id}) success={success}\n{output}\n"
                ));
            }
            ChatEvent::Done {
                tokens: t,
                session_id: sid,
                message,
                ..
            } => {
                tokens = t;
                session_id = sid;
                if let Some(m) = message {
                    if error.is_none() {
                        error = Some(m);
                    }
                }
            }
            ChatEvent::Error { message } => error = Some(message),
            _ => {}
        }
    }
    active_conns.fetch_sub(1, Ordering::Relaxed);

    if let Some(err) = error.filter(|_e| text.is_empty() && reasoning.is_empty()) {
        return api_error(StatusCode::BAD_GATEWAY, err, "turn_failed");
    }

    match format {
        WireFormat::OpenAiChat => {
            let mut message = json!({
                "role": "assistant",
                "content": text,
            });
            if !reasoning.is_empty() {
                message["reasoning_content"] = json!(reasoning);
            }
            if !tool_trace.is_empty() {
                message["atomcode_tools"] = json!(tool_trace);
            }
            Json(json!({
                "id": id,
                "object": "chat.completion",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": message,
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 0,
                    "completion_tokens": tokens,
                    "total_tokens": tokens
                },
                "atomcode": {
                    "session_id": session_id,
                    "user": session_key,
                }
            }))
            .into_response()
        }
        WireFormat::OpenAiResponses => {
            let mut output = Vec::new();
            if !reasoning.is_empty() {
                output.push(json!({
                    "type": "reasoning",
                    "content": [{"type": "output_text", "text": reasoning}]
                }));
            }
            if !tool_trace.is_empty() {
                output.push(json!({
                    "type": "atomcode_tools",
                    "content": [{"type": "output_text", "text": tool_trace}]
                }));
            }
            output.push(json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}]
            }));
            Json(json!({
                "id": id,
                "object": "response",
                "created_at": created,
                "model": model,
                "status": "completed",
                "output": output,
                "usage": { "total_tokens": tokens },
                "atomcode": { "session_id": session_id, "user": session_key }
            }))
            .into_response()
        }
        WireFormat::Anthropic => {
            let mut content = Vec::new();
            if !reasoning.is_empty() {
                content.push(json!({"type": "thinking", "thinking": reasoning}));
            }
            if !tool_trace.is_empty() {
                content.push(json!({"type": "text", "text": tool_trace}));
            }
            content.push(json!({"type": "text", "text": text}));
            Json(json!({
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": content,
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": { "input_tokens": 0, "output_tokens": tokens },
                "atomcode": { "session_id": session_id, "user": session_key }
            }))
            .into_response()
        }
    }
}

// ─── HTTP handlers ──────────────────────────────────────────────────────────

/// POST /v1/chat/completions
pub(crate) async fn openai_chat_completions(
    State(state): State<AppState>,
    Json(body): Json<OpenAiChatRequest>,
) -> Response {
    let (system, message, images) = match extract_openai_turn(&body.messages).await {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::BAD_REQUEST, e, "invalid_request"),
    };
    let session_key = session_key_from_request(&body.user, &body.metadata);
    let model_wire = body.model.clone().unwrap_or_default();
    let turn = CompatTurn {
        message,
        images,
        system_append: system,
        session_key,
        model_selection: body.model,
        model_wire,
        stream: body.stream.unwrap_or(false),
    };
    run_compat_turn(state, turn, WireFormat::OpenAiChat).await
}

/// POST /v1/responses
pub(crate) async fn openai_responses(
    State(state): State<AppState>,
    Json(body): Json<OpenAiResponsesRequest>,
) -> Response {
    let mut system = body.instructions.filter(|s| !s.trim().is_empty());
    let (message, images) = if !body.messages.is_empty() {
        match extract_openai_turn(&body.messages).await {
            Ok((sys, msg, imgs)) => {
                if system.is_none() {
                    system = sys;
                } else if let Some(s) = sys {
                    system = Some(format!("{}\n\n{s}", system.unwrap()));
                }
                (msg, imgs)
            }
            Err(e) => return api_error(StatusCode::BAD_REQUEST, e, "invalid_request"),
        }
    } else {
        match &body.input {
            Some(ResponsesInput::Text(t)) => (t.clone(), Vec::new()),
            Some(ResponsesInput::Items(items)) => {
                let mut last_user_text = String::new();
                let mut images = Vec::new();
                let mut systems = Vec::new();
                for item in items {
                    let role = item.role.as_deref().unwrap_or("user");
                    if role.eq_ignore_ascii_case("system") || role.eq_ignore_ascii_case("developer")
                    {
                        if let Some(c) = &item.content {
                            if let Ok((t, _)) = collect_openai_content(c).await {
                                if !t.trim().is_empty() {
                                    systems.push(t);
                                }
                            }
                        } else if let Some(t) = &item.text {
                            systems.push(t.clone());
                        }
                        continue;
                    }
                    if role.eq_ignore_ascii_case("user")
                        || item.kind.as_deref() == Some("message")
                        || item.kind.as_deref() == Some("input_text")
                    {
                        if let Some(c) = &item.content {
                            if let Ok((t, imgs)) = collect_openai_content(c).await {
                                last_user_text = t;
                                images = imgs;
                            }
                        } else if let Some(t) = &item.text {
                            last_user_text = t.clone();
                        }
                    }
                }
                if system.is_none() && !systems.is_empty() {
                    system = Some(systems.join("\n\n"));
                }
                if last_user_text.trim().is_empty() && images.is_empty() {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        "no user input in responses request",
                        "invalid_request",
                    );
                }
                (last_user_text, images)
            }
            None => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "responses request requires input or messages",
                    "invalid_request",
                );
            }
        }
    };

    let session_key = session_key_from_request(&body.user, &body.metadata);
    let model_wire = body.model.clone().unwrap_or_default();
    let turn = CompatTurn {
        message,
        images,
        system_append: system,
        session_key,
        model_selection: body.model,
        model_wire,
        stream: body.stream.unwrap_or(false),
    };
    run_compat_turn(state, turn, WireFormat::OpenAiResponses).await
}

/// POST /v1/messages
pub(crate) async fn anthropic_messages(
    State(state): State<AppState>,
    Json(body): Json<AnthropicMessagesRequest>,
) -> Response {
    let (system, message, images) =
        match extract_anthropic_turn(&body.system, &body.messages).await {
            Ok(v) => v,
            Err(e) => return api_error(StatusCode::BAD_REQUEST, e, "invalid_request"),
        };
    let meta_user = body.metadata.as_ref().and_then(|m| m.user_id.clone());
    let session_key = session_key_from_request(&body.user, &meta_user.map(|u| json!({"user_id": u})));
    let model_wire = body.model.clone().unwrap_or_default();
    let turn = CompatTurn {
        message,
        images,
        system_append: system,
        session_key,
        model_selection: body.model,
        model_wire,
        // Anthropic SDK defaults stream=false; many agent clients set true.
        stream: body.stream.unwrap_or(false),
    };
    run_compat_turn(state, turn, WireFormat::Anthropic).await
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compat_model_ids_are_account_slash_model() {
        // Catalog keys mix hyphen (CodingPlan) and slash (new schema) — public
        // ids must all be account/wire_model.
        let config: Config = serde_json::from_value(serde_json::json!({
            "default_model": "AtomGit-GLM-5.2",
            "provider_accounts": {
                "AtomGit": { "provider": "openai", "base_url": "https://llm-api.atomgit.com/v1" },
                "corp": { "provider": "openai", "base_url": "https://llm.corp/v1" }
            },
            "models": {
                "AtomGit-GLM-5.2": {
                    "account": "AtomGit",
                    "model": "GLM-5.2",
                    "context_window": 128000
                },
                "corp/code": {
                    "account": "corp",
                    "model": "corp-code",
                    "context_window": 200000
                }
            },
            "providers": {
                "claude": { "type": "claude", "model": "claude-opus-4-7" }
            }
        }))
        .unwrap();

        let rows = compat_models_from_config(&config);
        let ids: Vec<&str> = rows.iter().map(|r| r.public_id.as_str()).collect();
        assert!(ids.contains(&"AtomGit/GLM-5.2"), "{ids:?}");
        assert!(ids.contains(&"corp/corp-code"), "{ids:?}");
        assert!(ids.contains(&"claude/claude-opus-4-7"), "{ids:?}");
        // Never expose the raw hyphenated CodingPlan selection key as public id.
        assert!(!ids.iter().any(|id| *id == "AtomGit-GLM-5.2"), "{ids:?}");
        // No bare legacy provider name without model.
        assert!(!ids.iter().any(|id| *id == "claude"), "{ids:?}");

        let glm = rows.iter().find(|r| r.public_id == "AtomGit/GLM-5.2").unwrap();
        assert_eq!(glm.selection_id, "AtomGit-GLM-5.2");
        assert_eq!(glm.account, "AtomGit");
        assert_eq!(glm.wire_model, "GLM-5.2");
    }

    #[test]
    fn session_key_prefers_user_field_and_keeps_separators() {
        let meta = Some(json!({"user": "from_meta"}));
        assert_eq!(
            session_key_from_request(&Some("from_user".into()), &meta).as_deref(),
            Some("from_user")
        );
        assert_eq!(
            session_key_from_request(&None, &meta).as_deref(),
            Some("from_meta")
        );
        // Client-controlled keys: underscore / hyphen / mixed are preserved.
        assert_eq!(
            normalize_session_key(Some("  alice_proj1  ")).as_deref(),
            Some("alice_proj1")
        );
        assert_eq!(
            normalize_session_key(Some("chat-42")).as_deref(),
            Some("chat-42")
        );
        assert_eq!(
            normalize_session_key(Some("tenant_a-chat_3")).as_deref(),
            Some("tenant_a-chat_3")
        );
        assert_eq!(normalize_session_key(Some("   ")), None);
        assert_eq!(normalize_session_key(None), None);
    }

    #[test]
    fn different_user_keys_are_distinct_sessions() {
        // Same project: alice_a vs alice-a are different keys → different sessions.
        assert_ne!(
            normalize_session_key(Some("alice_a")),
            normalize_session_key(Some("alice-a"))
        );
        // resolve with no catalog → always "create" path (None id + Some key)
        let dir = std::path::Path::new(".");
        let (id1, k1) = resolve_session_for_key(dir, Some("user_1"));
        let (id2, k2) = resolve_session_for_key(dir, Some("user-2"));
        assert!(id1.is_none() && id2.is_none());
        assert_eq!(k1.as_deref(), Some("user_1"));
        assert_eq!(k2.as_deref(), Some("user-2"));
        // omit user → ephemeral
        let (id0, k0) = resolve_session_for_key(dir, None);
        assert!(id0.is_none() && k0.is_none());
    }

    #[test]
    fn openai_chunk_has_role_then_content() {
        let mut p = CompatProjector::openai_chat("id1".into(), "m".into(), 1, None);
        let chunks = p.project(ChatEvent::TextDelta {
            content: "hi".into(),
        });
        assert!(chunks[0].data.contains("\"role\":\"assistant\"") || chunks.len() >= 2);
        let joined: String = chunks.iter().map(|c| c.data.as_str()).collect();
        assert!(joined.contains("hi"));
        assert!(joined.contains("reasoning_content") || !joined.is_empty());
    }

    #[test]
    fn openai_reasoning_uses_reasoning_content() {
        let mut p = CompatProjector::openai_chat("id1".into(), "m".into(), 1, None);
        let chunks = p.project(ChatEvent::ReasoningDelta {
            content: "think".into(),
        });
        let joined: String = chunks.iter().map(|c| c.data.as_str()).collect();
        assert!(joined.contains("reasoning_content"));
        assert!(joined.contains("think"));
    }

    #[test]
    fn openai_tools_unified_under_tool_calls() {
        let mut p = CompatProjector::openai_chat("id1".into(), "m".into(), 1, None);
        let start = p.project(ChatEvent::ToolCallStarted {
            id: "call_1".into(),
            name: "bash".into(),
            arguments: r#"{"command":"ls"}"#.into(),
        });
        let out = p.project(ChatEvent::ToolOutputChunk {
            id: "call_1".into(),
            chunk: "a\n".into(),
        });
        let done = p.project(ChatEvent::ToolCallResult {
            id: "call_1".into(),
            name: "bash".into(),
            output: "a\n".into(),
            success: true,
            duration_ms: 3,
        });
        let joined: String = start
            .iter()
            .chain(out.iter())
            .chain(done.iter())
            .map(|c| c.data.as_str())
            .collect();
        // All phases live under tool_calls — no atomcode.tool_* for tools.
        assert!(joined.contains("\"tool_calls\""));
        assert!(joined.contains("\"status\":\"in_progress\""));
        assert!(joined.contains("\"output_delta\""));
        assert!(joined.contains("\"status\":\"completed\""));
        assert!(joined.contains("\"success\":true"));
        assert!(!joined.contains("atomcode.tool_"));
        assert!(!joined.contains("\"type\":\"tool_output\""));
        assert!(!joined.contains("\"type\":\"tool_result\""));
    }

    #[test]
    fn anthropic_thinking_then_text() {
        let mut p = CompatProjector::anthropic("msg_1".into(), "m".into(), None);
        let a = p.project(ChatEvent::ReasoningDelta {
            content: "r".into(),
        });
        let b = p.project(ChatEvent::TextDelta {
            content: "t".into(),
        });
        let joined: String = a.iter().chain(b.iter()).map(|c| c.data.as_str()).collect();
        assert!(joined.contains("thinking"));
        assert!(joined.contains("text_delta"));
    }

    #[tokio::test]
    async fn data_url_image_parses() {
        // "hi" as base64
        let url = "data:image/png;base64,aGk=";
        let img = resolve_image_url(url).await.unwrap();
        assert_eq!(img.media_type, "image/png");
        assert_eq!(img.data, "aGk=");
    }

    #[tokio::test]
    async fn extract_latest_user_only() {
        let messages = vec![
            OpenAiMessage {
                role: "system".into(),
                content: Some(OpenAiContent::Text("sys".into())),
            },
            OpenAiMessage {
                role: "user".into(),
                content: Some(OpenAiContent::Text("old".into())),
            },
            OpenAiMessage {
                role: "assistant".into(),
                content: Some(OpenAiContent::Text("reply".into())),
            },
            OpenAiMessage {
                role: "user".into(),
                content: Some(OpenAiContent::Text("latest".into())),
            },
        ];
        let (sys, text, imgs) = extract_openai_turn(&messages).await.unwrap();
        assert_eq!(sys.as_deref(), Some("sys"));
        assert_eq!(text, "latest");
        assert!(imgs.is_empty());
    }
}
