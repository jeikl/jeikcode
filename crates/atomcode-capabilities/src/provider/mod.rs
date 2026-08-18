//! Real `LlmProvider` adapters (L1).
//!
//! The kernel's [`LlmProvider`](atomcode_kernel::provider::LlmProvider) trait is the
//! seam; these types implement it against real backends. Three adapters live here:
//!   - [`OpenAiCompatProvider`] — the **OpenAI-compatible** chat/completions surface
//!     (GLM / DeepSeek / any OpenAI-shaped endpoint);
//!   - [`AnthropicProvider`] — the **Anthropic Messages API** (`/v1/messages`, Claude),
//!     including the signed extended-thinking round-trip;
//!   - [`OllamaProvider`] — the **Ollama native** `/api/chat` (local models, NDJSON).
//!
//! Division of labour (mechanism vs policy):
//!   - the kernel owns the *mechanism* — neutral `Message`/`StreamEvent`/`ChatOptions`
//!     and lossless `reasoning` storage;
//!   - this adapter owns the *policy* — how each neutral knob maps onto the wire, how
//!     SSE deltas assemble into whole `ToolCall`s, and whether prior-turn reasoning is
//!     echoed back ([`ReasoningPolicy`]).

mod anthropic;
mod atomgit_sign;
mod ollama;
mod openai_compat;
mod pricing_catalog;
mod reasoning;
mod retry;
mod sign;

pub use anthropic::{AnthropicConfig, AnthropicProvider};
pub use atomgit_sign::{atomgit_request_signer, is_atomgit_gateway, signer_available};
pub use ollama::{OllamaConfig, OllamaProvider};
pub use openai_compat::{
    model_suggests_vision, reason_effort_applicable, OpenAiCompatConfig, OpenAiCompatProvider,
};
pub use pricing_catalog::{
    ensure_models_dev_catalog, resolve_models_dev_pricing, spawn_models_dev_catalog_refresh,
    CatalogPricing,
};
pub use reasoning::{ReasoningPolicy, REASONING_PLACEHOLDER};
pub use retry::RetryPolicy;
pub use sign::{RequestSigner, RequestSigningError, SignedAuth};

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

/// Fallback User-Agent when a provider config carries no explicit `user_agent`.
/// Bare (no version) on purpose: this crate is versioned independently of the
/// product (`0.0.0`), so a local `CARGO_PKG_VERSION` would be MISLEADING. The
/// host adapter injects the real `atomcode/<version>` via `*Config::user_agent`;
/// this fallback only applies to direct/test construction.
pub(crate) const DEFAULT_USER_AGENT: &str = "atomcode";

/// Process-local sequence so dumps sort in call order even when two land in the same
/// nanosecond (the timestamp alone isn't a tiebreaker under concurrency).
static WIRE_DUMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// BYTE-LEVEL outbound-request dump for wire diagnosis. No-op unless `ATOMCODE_WIRE_DUMP=1`.
/// Writes the EXACT JSON body an adapter built (post-projection, pre-send) to
/// `<config_dir>/wire-dump/<seq>-<ts>-<model>.req.json`. Best-effort: any failure (env unset,
/// unwritable dir) is silently ignored so diagnostics never break a real request.
///
/// This is the ADAPTER-level, provider-SPECIFIC counterpart to the neutral
/// [`WireLogHooks`](crate::hooks::WireLogHooks) (which logs the kernel `Message` view, not
/// these bytes). The kernel has NO byte seam by design — byte framing is intrinsically the
/// adapter's concern (each backend's JSON differs), so every adapter routes its built body
/// through here. Ported from core's v1 `ATOMCODE_WIRE_DUMP` (same env + `wire-dump/` dir),
/// but `config_dir()` honors `$ATOMCODE_HOME` (v1 used `$HOME`).
pub(crate) fn wire_dump_request(model: &str, body: &Value) {
    if std::env::var("ATOMCODE_WIRE_DUMP").ok().as_deref() != Some("1") {
        return;
    }
    wire_dump_to(&crate::paths::config_dir().join("wire-dump"), model, body);
}

/// The pure writer behind [`wire_dump_request`] — `dir`-injected so it's testable without
/// mutating the process-global `$ATOMCODE_HOME`/`$ATOMCODE_WIRE_DUMP`. Best-effort.
fn wire_dump_to(dir: &std::path::Path, model: &str, body: &Value) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()))
        .unwrap_or_default();
    let seq = WIRE_DUMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let safe_model: String = model
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{seq:06}-{ts}-{safe_model}.req.json"));
    if let Ok(s) = serde_json::to_string_pretty(body) {
        let _ = std::fs::write(&path, s);
    }
}

/// Push a `system` wire message, COALESCING it into the previous wire entry when that is
/// also a `system` message (joined with a blank line).
///
/// The kernel's neutral history can carry SEVERAL `Role::System` messages (persona +
/// `memory.md` + any future capability), but many OpenAI-compatible models / chat
/// templates accept only a SINGLE system message — extra ones are rejected outright or
/// silently honor just the first (dropping memory). Both `role:"system"`-on-the-wire
/// adapters (OpenAI-compatible and Ollama) route their `Role::System` arm through here so
/// a model never sees more than one. (The Anthropic adapter instead lifts+joins all System
/// messages into the top-level `system` field — same guarantee, different wire shape.)
///
/// Coalescing is over CONSECUTIVE system entries only; in practice every System message is
/// leading and contiguous, so this yields exactly one leading system block. It is pure and
/// deterministic, so the outgoing prefix stays byte-stable across rounds (cache-safe).
pub(crate) fn push_system_coalesced(out: &mut Vec<Value>, text: &str) {
    if let Some(last) = out.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some("system") {
            let prev = last.get("content").and_then(Value::as_str).unwrap_or("");
            let joined = if prev.is_empty() || text.is_empty() {
                format!("{prev}{text}")
            } else {
                format!("{prev}\n\n{text}")
            };
            last["content"] = json!(joined);
            return;
        }
    }
    out.push(json!({ "role": "system", "content": text }));
}

/// Map an HTTP error status to a plain-language headline so the TUI shows the
/// *cause*, not a bare `HTTP 401:` (which, when the server returns an empty
/// body, carried no hint at all). Shared by every provider protocol
/// (openai-compat, Anthropic/Claude, ollama, …) so the wording stays consistent
/// regardless of which wire format hit the error.
///
/// 401/402 get a headline, and for those the provider's raw `detail` is
/// deliberately DROPPED — the headline already says it and this short form folds
/// cleanly into the interrupted-turn summary (`✗ 已中断：账户余额不足（HTTP 402）`).
/// One explicit CodingPlan entitlement rejection also gets an actionable `/login`
/// hint. Other 403 responses stay raw because AtomGit reuses that status for
/// session-concurrency conflicts and their structured reason must survive. 429
/// must keep the literal `HTTP 429: ` prefix the kernel rate-limit path
/// (`rate_limit_server_message`) strips. Everything else keeps
/// `HTTP {code}: {detail}` (the detail is the only signal there).
pub(crate) fn friendly_http_error(code: u16, detail: &str) -> String {
    if code == 403
        && detail
            .to_ascii_lowercase()
            .contains("user has no codingplan")
    {
        return "CodingPlan 未领取或已失效（HTTP 403）。请运行 /login 重新登录并领取 CodingPlan。"
            .to_string();
    }
    let headline = match code {
        401 => "API key 未授权或已失效",
        402 => "账户余额不足",
        _ => return format!("HTTP {code}: {detail}"),
    };
    format!("{headline}（HTTP {code}）")
}

/// Recursively clean and normalize tool parameter schemas for universal LLM wire compatibility
/// (OpenAI, Anthropic, Gemini gateways, Ollama, Bedrock, etc.).
///
/// Converts Draft-6+ `const: "val"` to OpenAPI-3.0-standard `enum: ["val"]`,
/// flattens polymorphic `oneOf`/`anyOf` discriminator objects into clean flat schemas,
/// and strips meta keywords like `$schema`/`$id` that cause upstream 400s (e.g. Gemini Schema errors).
pub(crate) fn sanitize_schema_for_wire(val: &Value) -> Value {
    match val {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();

            // Check if this object is a oneOf / anyOf container that should be normalized
            if let Some(one_of) = map.get("oneOf").or_else(|| map.get("anyOf")).and_then(|v| v.as_array()) {
                if !one_of.is_empty() && one_of.iter().all(|item| item.is_object()) {
                    let mut merged_props = serde_json::Map::new();
                    let mut merged_desc = map.get("description").cloned();
                    let mut kinds = Vec::new();

                    for item in one_of {
                        if let Some(item_obj) = item.as_object() {
                            if merged_desc.is_none() {
                                merged_desc = item_obj.get("description").cloned();
                            }
                            if let Some(props) = item_obj.get("properties").and_then(|p| p.as_object()) {
                                for (k, v) in props {
                                    if k == "kind" || k == "type" || k == "action" {
                                        if let Some(c) = v.get("const").and_then(|c| c.as_str()) {
                                            if !kinds.contains(&c.to_string()) {
                                                kinds.push(c.to_string());
                                            }
                                        } else if let Some(e) = v.get("enum").and_then(|e| e.as_array()) {
                                            for ev in e {
                                                if let Some(s) = ev.as_str() {
                                                    if !kinds.contains(&s.to_string()) {
                                                        kinds.push(s.to_string());
                                                    }
                                                }
                                            }
                                        } else {
                                            merged_props.insert(k.clone(), sanitize_schema_for_wire(v));
                                        }
                                    } else {
                                        merged_props.insert(k.clone(), sanitize_schema_for_wire(v));
                                    }
                                }
                            }
                        }
                    }

                    if !kinds.is_empty() {
                        let kind_prop = json!({
                            "type": "string",
                            "enum": kinds,
                            "description": "Scope kind or discriminator."
                        });
                        merged_props.insert("kind".to_string(), kind_prop);
                    }

                    out.insert("type".to_string(), json!("object"));
                    if let Some(desc) = merged_desc {
                        out.insert("description".to_string(), desc);
                    }
                    if !merged_props.is_empty() {
                        out.insert("properties".to_string(), Value::Object(merged_props));
                    }
                    return Value::Object(out);
                }
            }

            for (k, v) in map {
                // Strip unsupported meta fields
                if k == "$schema" || k == "$id" {
                    continue;
                }
                // Convert const to enum
                if k == "const" {
                    out.insert("enum".to_string(), json!([v]));
                    continue;
                }
                out.insert(k.clone(), sanitize_schema_for_wire(v));
            }
            Value::Object(out)
        }
        Value::Array(arr) => {
            Value::Array(arr.iter().map(sanitize_schema_for_wire).collect())
        }
        _ => val.clone(),
    }
}

#[cfg(test)]
mod coalesce_tests {
    use super::push_system_coalesced;
    use serde_json::json;

    #[test]
    fn merges_runs_and_preserves_non_system_boundaries() {
        let mut out = Vec::new();
        push_system_coalesced(&mut out, "persona");
        push_system_coalesced(&mut out, "memory");
        assert_eq!(
            out,
            vec![json!({"role":"system","content":"persona\n\nmemory"})]
        );
        // A non-system entry breaks the run: a later system would start a fresh block.
        out.push(json!({"role":"user","content":"hi"}));
        push_system_coalesced(&mut out, "late");
        assert_eq!(
            out.len(),
            3,
            "system after a user is NOT merged into the leading block"
        );
        assert_eq!(out[2], json!({"role":"system","content":"late"}));
    }

    #[test]
    fn empty_text_does_not_inject_blank_separator() {
        let mut out = Vec::new();
        push_system_coalesced(&mut out, "");
        push_system_coalesced(&mut out, "real");
        assert_eq!(out, vec![json!({"role":"system","content":"real"})]);
    }
}

#[cfg(test)]
mod wire_dump_tests {
    use super::wire_dump_to;
    use serde_json::json;

    #[test]
    fn writes_body_as_req_json_into_dir() {
        let dir = tempfile::tempdir().unwrap();
        let body = json!({"model": "deepseek-v4", "messages": [{"role": "user", "content": "hi"}]});
        wire_dump_to(dir.path(), "deepseek-v4", &body);

        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 1, "one dump file written: {files:?}");
        let name = &files[0];
        assert!(name.ends_with(".req.json"), "req.json suffix: {name}");
        assert!(name.contains("deepseek-v4"), "model in filename: {name}");

        // The dumped bytes round-trip to the exact body (byte-level content preserved).
        let written = std::fs::read_to_string(dir.path().join(name)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(parsed, body, "dumped JSON equals the outbound body");
    }

    #[test]
    fn model_name_is_filename_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        // A slash / colon in a model id must not escape the dir or break the path.
        wire_dump_to(dir.path(), "org/model:v1", &json!({}));
        let name = std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(
            !name.contains('/') && !name.contains(':'),
            "unsafe chars stripped: {name}"
        );
        assert!(
            name.contains("org_model_v1"),
            "sanitized model retained: {name}"
        );
    }

    #[test]
    fn sanitize_schema_converts_const_and_normalizes_one_of() {
        use super::sanitize_schema_for_wire;

        // Test 1: const conversion
        let s = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "kind": { "const": "working_tree" }
            }
        });
        let sanitized = sanitize_schema_for_wire(&s);
        assert!(!sanitized.as_object().unwrap().contains_key("$schema"));
        assert_eq!(
            sanitized["properties"]["kind"]["enum"],
            json!(["working_tree"])
        );

        // Test 2: oneOf discriminator normalization
        let one_of_schema = json!({
            "oneOf": [
                { "type": "object", "properties": { "kind": { "const": "working_tree" } }, "required": ["kind"] },
                { "type": "object", "properties": { "kind": { "const": "staged" } }, "required": ["kind"] },
                { "type": "object", "properties": { "kind": { "const": "range" }, "base": { "type": "string" } }, "required": ["kind", "base"] }
            ],
            "description": "Scope selection"
        });
        let res = sanitize_schema_for_wire(&one_of_schema);
        assert_eq!(res["type"], "object");
        assert_eq!(res["description"], "Scope selection");
        assert_eq!(
            res["properties"]["kind"]["enum"],
            json!(["working_tree", "staged", "range"])
        );
        assert_eq!(res["properties"]["base"]["type"], "string");
    }
}

