use atomcode_config::config::provider::{
    default_context_window_for, ProviderConfig, ProviderPricing,
};
use axum::{extract::Path, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use crate::{
    api_config::{
        config_response, load_config, provider_info, update_config, validate_provider_name,
    },
    json_error, ProviderInfo,
};

// ============================================================================
// Request DTOs
// ============================================================================

/// POST /providers - Create or replace a provider.
#[derive(Debug, Deserialize)]
pub(crate) struct CreateProviderRequest {
    pub name: String,
    #[serde(rename = "type")]
    pub provider_type: String,
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub user_agent: Option<String>,
    pub context_window: Option<usize>,
    pub max_tokens: Option<usize>,
    pub thinking_type: Option<String>,
    pub thinking_keep: Option<String>,
    pub reasoning_history: Option<String>,
    pub reasoning_effort: Option<String>,
    pub thinking_enabled: Option<bool>,
    pub thinking_budget: Option<u32>,
    pub pricing: Option<ProviderPricing>,
    /// Whether the model accepts image inputs. Omitted → protocol default (opt-in false).
    pub supports_vision: Option<bool>,
    /// Whether the model is a reasoning model.
    pub reasoning_model: Option<bool>,
    #[serde(default)]
    pub skip_tls_verify: bool,
    #[serde(default)]
    pub set_default: bool,
}

/// PATCH /providers/:name - Partially update a provider.
#[derive(Debug, Deserialize)]
pub(crate) struct PatchProviderRequest {
    /// New name to rename this provider to. Omitted = keep current name.
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub provider_type: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<Option<String>>,
    #[serde(default)]
    pub clear_api_key: bool,
    pub base_url: Option<Option<String>>,
    #[serde(default)]
    pub clear_base_url: bool,
    pub user_agent: Option<Option<String>>,
    #[serde(default)]
    pub clear_user_agent: bool,
    pub context_window: Option<usize>,
    pub max_tokens: Option<Option<usize>>,
    #[serde(default)]
    pub clear_max_tokens: bool,
    pub thinking_enabled: Option<Option<bool>>,
    pub thinking_budget: Option<Option<u32>>,
    pub thinking_type: Option<Option<String>>,
    pub thinking_keep: Option<Option<String>>,
    pub reasoning_history: Option<Option<String>>,
    pub reasoning_effort: Option<Option<String>>,
    pub skip_tls_verify: Option<bool>,
    pub pricing: Option<Option<ProviderPricing>>,
    #[serde(default)]
    pub clear_pricing: bool,
    pub supports_vision: Option<Option<bool>>,
    #[serde(default)]
    pub clear_supports_vision: bool,
    pub reasoning_model: Option<Option<bool>>,
    #[serde(default)]
    pub clear_reasoning_model: bool,
}

/// PATCH /providers/:name/thinking - Update thinking settings.
#[derive(Debug, Deserialize)]
pub(crate) struct PatchThinkingRequest {
    pub enabled: Option<bool>,
    pub budget: Option<u32>,
    #[serde(rename = "type")]
    pub thinking_type: Option<Option<String>>,
    pub keep: Option<Option<String>>,
    pub reasoning_history: Option<Option<String>>,
    pub reasoning_effort: Option<Option<String>>,
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /providers - List all providers with sanitized info.
pub(crate) async fn get_providers() -> impl IntoResponse {
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    // List the unified catalog so new-schema / folded CodingPlan models (absent
    // from `config.providers`) remain visible and selectable.
    let default_selection = config.effective_model_selection().unwrap_or_default();
    let mut ids: Vec<String> = config.logical_models().into_keys().collect();
    ids.sort();
    let providers: Vec<ProviderInfo> = ids
        .iter()
        .filter_map(|id| {
            config
                .provider_config_for_selection(id)
                .map(|p| provider_info(id, &p, &default_selection))
        })
        .collect();
    Json(serde_json::json!({
        "default_provider": default_selection,
        "providers": providers,
    }))
    .into_response()
}

/// POST /providers - Create or replace a provider.
pub(crate) async fn create_provider(Json(req): Json<CreateProviderRequest>) -> impl IntoResponse {
    // Validate name
    let name = match validate_provider_name(&req.name) {
        Ok(n) => n,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e).into_response(),
    };
    // Validate required fields
    if req.provider_type.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Provider type cannot be empty")
            .into_response();
    }
    if req.model.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Model cannot be empty").into_response();
    }
    // Validate thinking budget
    if let Some(budget) = req.thinking_budget {
        if budget < 1024 {
            return json_error(StatusCode::BAD_REQUEST, "thinking_budget must be >= 1024")
                .into_response();
        }
    }
    if req
        .pricing
        .is_some_and(|pricing| pricing.validated().is_none())
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            "pricing values must be finite and non-negative",
        )
        .into_response();
    }

    let context_window = req
        .context_window
        .unwrap_or_else(|| default_context_window_for(&req.provider_type));

    let provider = ProviderConfig {
        provider_type: req.provider_type,
        api_key: req.api_key,
        model: req.model,
        base_url: req.base_url,
        system_prompt: None,
        user_agent: req.user_agent,
        context_window,
        max_tokens: req.max_tokens,
        thinking_type: req.thinking_type,
        thinking_keep: req.thinking_keep,
        reasoning_history: req.reasoning_history,
        reasoning_effort: req.reasoning_effort,
        reasoning_levels: None,
        thinking_enabled: req.thinking_enabled,
        thinking_budget: req.thinking_budget,
        skip_tls_verify: req.skip_tls_verify,
        ephemeral: false,
        capable_model: None,
        pricing: req.pricing,
        supports_vision: req.supports_vision,
        reasoning_model: req.reasoning_model,
    };

    let mut is_new = false;
    let config = match update_config(|config| {
        is_new = !config.providers.contains_key(&name);
        config.providers.insert(name.clone(), provider);
        // Only claim the default when there isn't already a valid one — check the
        // effective selection (new-schema `default_model` or legacy
        // `default_provider`) so a CodingPlan default isn't wrongly clobbered.
        let has_valid_default = config
            .effective_model_selection()
            .is_some_and(|s| config.selection_exists(&s));
        if req.set_default || !has_valid_default {
            config.default_model = Some(name.clone());
            config.default_provider = name.clone();
        }
        Ok(())
    }) {
        Ok(config) => config,
        Err(error) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };

    let p = config.providers.get(&name).unwrap();
    let status = if is_new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(provider_info(&name, p, &config.default_provider)),
    )
        .into_response()
}

/// PATCH /providers/:name - Partially update a provider.
pub(crate) async fn patch_provider(
    Path(name): Path<String>,
    Json(req): Json<PatchProviderRequest>,
) -> impl IntoResponse {
    if req
        .provider_type
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return json_error(StatusCode::BAD_REQUEST, "Provider type cannot be empty")
            .into_response();
    }
    if req
        .pricing
        .as_ref()
        .and_then(|pricing| *pricing)
        .is_some_and(|pricing| pricing.validated().is_none())
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            "pricing values must be finite and non-negative",
        )
        .into_response();
    }
    if req
        .model
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return json_error(StatusCode::BAD_REQUEST, "Model cannot be empty").into_response();
    }
    if req
        .thinking_budget
        .as_ref()
        .and_then(|budget| budget.as_ref())
        .is_some_and(|budget| *budget < 1024)
    {
        return json_error(StatusCode::BAD_REQUEST, "thinking_budget must be >= 1024")
            .into_response();
    }
    let final_name = match req.name.as_deref() {
        Some(new_name) if new_name.trim() != name => {
            match validate_provider_name(new_name.trim()) {
                Ok(name) => name,
                Err(error) => return json_error(StatusCode::BAD_REQUEST, error).into_response(),
            }
        }
        _ => name.clone(),
    };

    let mut missing = false;
    let mut conflict = false;
    let config = match update_config(|config| {
        if final_name != name && config.providers.contains_key(&final_name) {
            conflict = true;
            anyhow::bail!("provider {final_name:?} already exists");
        }
        let Some(existing) = config.providers.get_mut(&name) else {
            missing = true;
            anyhow::bail!("provider {name:?} not found");
        };
        if let Some(value) = req.provider_type {
            existing.provider_type = value;
        }
        if let Some(value) = req.model {
            existing.model = value;
        }
        if req.clear_api_key {
            existing.api_key = None;
        } else if let Some(value) = req.api_key {
            existing.api_key = value;
        }
        if req.clear_base_url {
            existing.base_url = None;
        } else if let Some(value) = req.base_url {
            existing.base_url = value;
        }
        if req.clear_user_agent {
            existing.user_agent = None;
        } else if let Some(value) = req.user_agent {
            existing.user_agent = value;
        }
        if let Some(value) = req.context_window {
            existing.context_window = value;
        }
        if req.clear_max_tokens {
            existing.max_tokens = None;
        } else if let Some(value) = req.max_tokens {
            existing.max_tokens = value;
        }
        if let Some(value) = req.thinking_enabled {
            existing.thinking_enabled = value;
        }
        if let Some(value) = req.thinking_budget {
            existing.thinking_budget = value;
        }
        if let Some(value) = req.thinking_type {
            existing.thinking_type = value;
        }
        if let Some(value) = req.thinking_keep {
            existing.thinking_keep = value;
        }
        if let Some(value) = req.reasoning_history {
            existing.reasoning_history = value;
        }
        if let Some(value) = req.reasoning_effort {
            existing.reasoning_effort = value;
        }
        if let Some(value) = req.skip_tls_verify {
            existing.skip_tls_verify = value;
        }
        if req.clear_pricing {
            existing.pricing = None;
        } else if let Some(value) = req.pricing {
            existing.pricing = value;
        }
        if req.clear_supports_vision {
            existing.supports_vision = None;
        } else if let Some(value) = req.supports_vision {
            existing.supports_vision = value;
        }
        if req.clear_reasoning_model {
            existing.reasoning_model = None;
        } else if let Some(value) = req.reasoning_model {
            existing.reasoning_model = value;
        }
        if final_name != name {
            let provider = config.providers.remove(&name).expect("validated above");
            config.providers.insert(final_name.clone(), provider);
            if config.default_provider == name {
                config.default_provider = final_name.clone();
            }
        }
        Ok(())
    }) {
        Ok(config) => config,
        Err(_) if missing => {
            return json_error(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found", name),
            )
            .into_response()
        }
        Err(_) if conflict => {
            return json_error(
                StatusCode::CONFLICT,
                format!("Provider '{}' already exists", final_name),
            )
            .into_response()
        }
        Err(error) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };

    let default_provider = config.default_provider.clone();
    let p = config.providers.get(&final_name).unwrap();
    Json(provider_info(&final_name, p, &default_provider)).into_response()
}

/// DELETE /providers/:name - Delete a provider.
pub(crate) async fn delete_provider(Path(name): Path<String>) -> impl IntoResponse {
    let mut missing = false;
    let config = match update_config(|config| {
        if config.providers.remove(&name).is_none() {
            missing = true;
            anyhow::bail!("provider {name:?} not found");
        }
        if config.default_provider == name {
            config.default_provider = config.providers.keys().min().cloned().unwrap_or_default();
        }
        Ok(())
    }) {
        Ok(config) => config,
        Err(_) if missing => {
            return json_error(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found", name),
            )
            .into_response()
        }
        Err(error) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };

    let providers: Vec<ProviderInfo> = config
        .providers
        .iter()
        .map(|(n, p)| provider_info(n, p, &config.default_provider))
        .collect();
    Json(serde_json::json!({
        "default_provider": config.default_provider,
        "providers": providers,
    }))
    .into_response()
}

/// POST /providers/:name/default - Set default provider.
pub(crate) async fn set_default_provider(Path(name): Path<String>) -> impl IntoResponse {
    let mut missing = false;
    let requested = name.clone();
    let config = match update_config(|config| {
        if !config.selection_exists(&requested) {
            missing = true;
            anyhow::bail!("provider {requested:?} not found");
        }
        // `default_model` is the canonical selection (`effective_model_selection`
        // prefers it); keep the legacy `default_provider` synced so a new-schema
        // selection actually takes effect.
        config.default_model = Some(requested.clone());
        config.default_provider = requested.clone();
        Ok(())
    }) {
        Ok(config) => config,
        Err(_) if missing => {
            return json_error(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found", name),
            )
            .into_response()
        }
        Err(error) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };

    Json(config_response(&config)).into_response()
}

/// PATCH /providers/:name/thinking - Update thinking settings.
pub(crate) async fn patch_thinking(
    Path(name): Path<String>,
    Json(req): Json<PatchThinkingRequest>,
) -> impl IntoResponse {
    if let Some(budget) = req.budget {
        if budget < 1024 {
            return json_error(StatusCode::BAD_REQUEST, "thinking_budget must be >= 1024")
                .into_response();
        }
    }
    let mut missing = false;
    let config = match update_config(|config| {
        // Schema-aware write so the webui thinking editor works on a new-schema
        // / folded CodingPlan model (which lives in `[models.*]`, not
        // `[providers.*]`). Cloned reads keep the closure re-runnable under CAS.
        let found = config.update_selection_reasoning(&name, |r| {
            if let Some(enabled) = req.enabled {
                *r.thinking_enabled = Some(enabled);
            }
            if let Some(budget) = req.budget {
                *r.thinking_budget = Some(budget);
            } else if req.enabled == Some(true) && r.thinking_budget.is_none() {
                *r.thinking_budget = Some(10000);
            }
            if let Some(tt) = req.thinking_type.clone() {
                *r.thinking_type = tt;
            }
            if let Some(tk) = req.keep.clone() {
                *r.thinking_keep = tk;
            }
            if let Some(rh) = req.reasoning_history.clone() {
                *r.reasoning_history = rh;
            }
            if let Some(re) = req.reasoning_effort.clone() {
                *r.reasoning_effort = re;
            }
        });
        if !found {
            missing = true;
            anyhow::bail!("provider {name:?} not found");
        }
        Ok(())
    }) {
        Ok(config) => config,
        Err(_) if missing => {
            return json_error(
                StatusCode::NOT_FOUND,
                format!("Provider '{}' not found", name),
            )
            .into_response()
        }
        Err(error) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };

    let default_selection = config.effective_model_selection().unwrap_or_default();
    let Some(p) = config.provider_config_for_selection(&name) else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Provider '{}' vanished after update", name),
        )
        .into_response();
    };
    Json(provider_info(&name, &p, &default_selection)).into_response()
}

// ============================================================================
// Upstream model catalog (WebUI / TUI parity)
// ============================================================================

/// POST /providers/upstream-models — list model ids from an upstream base_url.
/// Mirrors TUI `upstream_models::fetch_upstream_model_ids` so the WebUI model-id
/// field can offer a filterable catalog for openai / responses / anthropic / ollama.
#[derive(Debug, Deserialize)]
pub(crate) struct UpstreamModelsRequest {
    /// Wire protocol: `openai` / `responses` / `anthropic` / `claude` / `ollama`.
    pub protocol: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    /// When editing an existing selection, reuse its stored key if the form left
    /// api_key blank.
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub skip_tls_verify: bool,
}

pub(crate) async fn list_upstream_models(
    Json(req): Json<UpstreamModelsRequest>,
) -> impl IntoResponse {
    if req.base_url.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "base_url is required").into_response();
    }
    let mut api_key = req.api_key.unwrap_or_default();
    if api_key.trim().is_empty() {
        if let Some(name) = req
            .provider_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Ok(config) = load_config() {
                if let Some(p) = config.provider_config_for_selection(name) {
                    if let Some(key) = p.resolved_api_key() {
                        api_key = key;
                    }
                }
            }
        }
    }
    match fetch_upstream_model_ids(&req.protocol, &req.base_url, &api_key, req.skip_tls_verify)
        .await
    {
        Ok(models) => Json(serde_json::json!({ "models": models })).into_response(),
        Err(error) => json_error(StatusCode::BAD_GATEWAY, error).into_response(),
    }
}

fn models_endpoint(protocol: &str, base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    let p = protocol.to_ascii_lowercase();
    if p == "ollama" {
        let origin = base
            .strip_suffix("/v1")
            .unwrap_or(base)
            .trim_end_matches('/');
        return format!("{origin}/api/tags");
    }
    if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
}

fn parse_model_ids(body: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    if let Some(arr) = value.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                if !id.is_empty() {
                    ids.push(id.to_string());
                }
            } else if let Some(id) = item.as_str() {
                if !id.is_empty() {
                    ids.push(id.to_string());
                }
            }
        }
    }
    if ids.is_empty() {
        if let Some(arr) = value.get("models").and_then(|d| d.as_array()) {
            for item in arr {
                let id = item
                    .get("id")
                    .and_then(|x| x.as_str())
                    .or_else(|| item.get("name").and_then(|x| x.as_str()))
                    .or_else(|| item.get("model").and_then(|x| x.as_str()));
                if let Some(id) = id.filter(|s| !s.is_empty()) {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

async fn fetch_upstream_model_ids(
    protocol: &str,
    base_url: &str,
    api_key: &str,
    skip_tls_verify: bool,
) -> Result<Vec<String>, String> {
    let url = models_endpoint(protocol, base_url);
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .connect_timeout(std::time::Duration::from_secs(5));
    if skip_tls_verify {
        builder = builder.danger_accept_invalid_certs(true);
    }
    let client = builder.build().map_err(|e| e.to_string())?;
    let mut req = client.get(&url);
    let protocol = protocol.to_ascii_lowercase();
    if protocol == "anthropic" || protocol == "claude" {
        if !api_key.is_empty() {
            req = req
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01");
        }
    } else if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let text = resp.text().await.map_err(|e| e.to_string())?;
    Ok(parse_model_ids(&text))
}

#[cfg(test)]
mod upstream_tests {
    use super::{models_endpoint, parse_model_ids};

    #[test]
    fn openai_and_responses_use_v1_models() {
        assert_eq!(
            models_endpoint("openai", "https://api.openai.com/v1"),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            models_endpoint("responses", "http://127.0.0.1:8000/v1"),
            "http://127.0.0.1:8000/v1/models"
        );
    }

    #[test]
    fn parse_openai_data_ids() {
        let body = r#"{"object":"list","data":[{"id":"grok-4.6"},{"id":"grok-4.5"}]}"#;
        assert_eq!(
            parse_model_ids(body),
            vec!["grok-4.5".to_string(), "grok-4.6".to_string()]
        );
    }
}
