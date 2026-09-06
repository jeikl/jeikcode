use atomcode_config::config::provider::{
    default_context_window_for, ModelProfileConfig, ProviderAccountConfig, ProviderConfig,
    ProviderPricing,
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
    #[serde(default)]
    pub account: Option<String>,
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
    #[serde(default)]
    pub account: Option<String>,
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

/// POST /provider-accounts / PUT /provider-accounts/:id
#[derive(Debug, Deserialize)]
pub(crate) struct CreateOrUpdateAccountRequest {
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub provider_type: Option<String>,
    pub base_url: Option<Option<String>>,
    pub api_key: Option<Option<String>>,
    #[serde(default)]
    pub clear_api_key: bool,
    #[serde(default)]
    pub clear_base_url: bool,
    pub skip_tls_verify: Option<bool>,
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
    let logical_models = config.logical_models();
    let mut ids: Vec<String> = logical_models.keys().cloned().collect();
    ids.sort();
    let providers: Vec<ProviderInfo> = ids
        .iter()
        .filter_map(|id| {
            config
                .provider_config_for_selection(id)
                .map(|p| {
                    let mut info = provider_info(id, &p, &default_selection);
                    if let Some(m) = logical_models.get(id) {
                        info.account = Some(m.account.clone());
                    }
                    info
                })
        })
        .collect();

    let logical_accounts = config.logical_accounts();
    let mut account_ids: Vec<String> = logical_accounts.keys().cloned().collect();
    account_ids.sort();
    let accounts: Vec<crate::AccountInfo> = account_ids
        .into_iter()
        .map(|id| {
            let a = &logical_accounts[&id];
            crate::AccountInfo {
                id: id.clone(),
                provider_type: a.provider.clone(),
                base_url: a.base_url.clone(),
                has_api_key: a.api_key.as_ref().is_some_and(|k| !k.is_empty()),
                skip_tls_verify: a.skip_tls_verify,
            }
        })
        .collect();

    Json(serde_json::json!({
        "default_provider": default_selection,
        "providers": providers,
        "accounts": accounts,
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

    if let Some(ref acc_id) = req.account {
        let account_name = acc_id.trim().to_string();
        let mut is_new = false;
        let config = match update_config(|config| {
            is_new = !config.models.contains_key(&name);
            if !config.provider_accounts.contains_key(&account_name) {
                config.provider_accounts.insert(
                    account_name.clone(),
                    ProviderAccountConfig {
                        provider: req.provider_type.clone(),
                        display_name: None,
                        api_key: req.api_key.clone(),
                        base_url: req.base_url.clone(),
                        user_agent: req.user_agent.clone(),
                        skip_tls_verify: req.skip_tls_verify,
                        enterprise_url: None,
                        ephemeral: false,
                    },
                );
            } else if let Some(acc) = config.provider_accounts.get_mut(&account_name) {
                if !req.provider_type.trim().is_empty() {
                    acc.provider = req.provider_type.clone();
                }
                if req.api_key.is_some() {
                    acc.api_key = req.api_key.clone();
                }
                if req.base_url.is_some() {
                    acc.base_url = req.base_url.clone();
                }
                if req.user_agent.is_some() {
                    acc.user_agent = req.user_agent.clone();
                }
                acc.skip_tls_verify = req.skip_tls_verify;
            }

            let profile = ModelProfileConfig {
                account: account_name.clone(),
                model: req.model.clone(),
                display_name: None,
                system_prompt: None,
                context_window,
                max_tokens: req.max_tokens,
                capable_model: None,
                thinking_type: req.thinking_type.clone(),
                thinking_keep: req.thinking_keep.clone(),
                reasoning_history: req.reasoning_history.clone(),
                reasoning_effort: req.reasoning_effort.clone(),
                reasoning_levels: None,
                thinking_enabled: req.thinking_enabled,
                thinking_budget: req.thinking_budget,
                pricing: req.pricing.clone(),
                supports_vision: req.supports_vision,
                reasoning_model: req.reasoning_model,
            };
            config.models.insert(name.clone(), profile);

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

        let default_selection = config.effective_model_selection().unwrap_or_default();
        let p = config.provider_config_for_selection(&name).unwrap();
        let mut info = provider_info(&name, &p, &default_selection);
        info.account = Some(account_name);
        let status = if is_new {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        return (status, Json(info)).into_response();
    }

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

    let default_selection = config.effective_model_selection().unwrap_or_default();
    let p = config.provider_config_for_selection(&name).unwrap();
    let status = if is_new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(provider_info(&name, &p, &default_selection)),
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
        if final_name != name
            && (config.providers.contains_key(&final_name)
                || config.models.contains_key(&final_name)
                || config.provider_accounts.contains_key(&final_name))
        {
            conflict = true;
            anyhow::bail!("provider {final_name:?} already exists");
        }

        if let Some(existing) = config.providers.get_mut(&name) {
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
                if config.default_model.as_deref() == Some(&name) {
                    config.default_model = Some(final_name.clone());
                }
            }
            Ok(())
        } else if let Some(mut existing_model) = config.models.remove(&name) {
            if let Some(value) = req.model {
                existing_model.model = value;
            }
            if let Some(value) = req.context_window {
                existing_model.context_window = value;
            }
            if req.clear_max_tokens {
                existing_model.max_tokens = None;
            } else if let Some(value) = req.max_tokens {
                existing_model.max_tokens = value;
            }
            if let Some(value) = req.thinking_enabled {
                existing_model.thinking_enabled = value;
            }
            if let Some(value) = req.thinking_budget {
                existing_model.thinking_budget = value;
            }
            if let Some(value) = req.thinking_type {
                existing_model.thinking_type = value;
            }
            if let Some(value) = req.thinking_keep {
                existing_model.thinking_keep = value;
            }
            if let Some(value) = req.reasoning_history {
                existing_model.reasoning_history = value;
            }
            if let Some(value) = req.reasoning_effort {
                existing_model.reasoning_effort = value;
            }
            if req.clear_pricing {
                existing_model.pricing = None;
            } else if let Some(value) = req.pricing {
                existing_model.pricing = value;
            }
            if req.clear_supports_vision {
                existing_model.supports_vision = None;
            } else if let Some(value) = req.supports_vision {
                existing_model.supports_vision = value;
            }
            if req.clear_reasoning_model {
                existing_model.reasoning_model = None;
            } else if let Some(value) = req.reasoning_model {
                existing_model.reasoning_model = value;
            }
            if let Some(new_account) = req.account {
                existing_model.account = new_account;
            }

            let account_id = existing_model.account.clone();
            if let Some(acc) = config.provider_accounts.get_mut(&account_id) {
                if let Some(value) = req.provider_type {
                    acc.provider = value;
                }
                if req.clear_api_key {
                    acc.api_key = None;
                } else if let Some(value) = req.api_key {
                    acc.api_key = value;
                }
                if req.clear_base_url {
                    acc.base_url = None;
                } else if let Some(value) = req.base_url {
                    acc.base_url = value;
                }
                if req.clear_user_agent {
                    acc.user_agent = None;
                } else if let Some(value) = req.user_agent {
                    acc.user_agent = value;
                }
                if let Some(value) = req.skip_tls_verify {
                    acc.skip_tls_verify = value;
                }
            } else if req.provider_type.is_some() || req.base_url.is_some() || req.api_key.is_some() {
                config.provider_accounts.insert(
                    account_id,
                    ProviderAccountConfig {
                        provider: req.provider_type.unwrap_or_else(|| "openai".into()),
                        display_name: None,
                        api_key: req.api_key.flatten(),
                        base_url: req.base_url.flatten(),
                        user_agent: req.user_agent.flatten(),
                        skip_tls_verify: req.skip_tls_verify.unwrap_or(false),
                        enterprise_url: None,
                        ephemeral: false,
                    },
                );
            }

            config.models.insert(final_name.clone(), existing_model);
            if config.default_model.as_deref() == Some(&name) {
                config.default_model = Some(final_name.clone());
            }
            if config.default_provider == name {
                config.default_provider = final_name.clone();
            }
            Ok(())
        } else if let Some(acc) = config.provider_accounts.get_mut(&name) {
            if let Some(value) = req.provider_type {
                acc.provider = value;
            }
            if req.clear_api_key {
                acc.api_key = None;
            } else if let Some(value) = req.api_key {
                acc.api_key = value;
            }
            if req.clear_base_url {
                acc.base_url = None;
            } else if let Some(value) = req.base_url {
                acc.base_url = value;
            }
            if req.clear_user_agent {
                acc.user_agent = None;
            } else if let Some(value) = req.user_agent {
                acc.user_agent = value;
            }
            if let Some(value) = req.skip_tls_verify {
                acc.skip_tls_verify = value;
            }
            if final_name != name {
                let acc = config.provider_accounts.remove(&name).unwrap();
                config.provider_accounts.insert(final_name.clone(), acc);
                for m in config.models.values_mut() {
                    if m.account == name {
                        m.account = final_name.clone();
                    }
                }
            }
            Ok(())
        } else {
            missing = true;
            anyhow::bail!("provider {name:?} not found");
        }
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

    let default_provider = config.effective_model_selection().unwrap_or_default();
    if let Some(p) = config.provider_config_for_selection(&final_name) {
        let mut info = provider_info(&final_name, &p, &default_provider);
        if let Some(m) = config.logical_models().get(&final_name) {
            info.account = Some(m.account.clone());
        }
        Json(info).into_response()
    } else if let Some(acc) = config.logical_accounts().get(&final_name) {
        let info = ProviderInfo {
            name: final_name.clone(),
            provider_type: acc.provider.clone(),
            model: String::new(),
            base_url: acc.base_url.clone(),
            has_api_key: acc.api_key.as_ref().is_some_and(|k| !k.is_empty()),
            requires_login: false,
            is_default: false,
            context_window: 128_000,
            max_tokens: None,
            thinking_enabled: None,
            thinking_budget: None,
            thinking_type: None,
            thinking_keep: None,
            reasoning_history: None,
            reasoning_effort: None,
            skip_tls_verify: acc.skip_tls_verify,
            ephemeral: acc.ephemeral,
            pricing: None,
            supports_vision: None,
            reasoning_model: None,
            account: Some(final_name.clone()),
        };
        Json(info).into_response()
    } else {
        json_error(
            StatusCode::NOT_FOUND,
            format!("Provider '{}' not found", final_name),
        )
        .into_response()
    }
}

/// DELETE /providers/:name - Delete a provider.
pub(crate) async fn delete_provider(Path(name): Path<String>) -> impl IntoResponse {
    let mut missing = false;
    let config = match update_config(|config| {
        let removed_legacy = config.providers.remove(&name).is_some();
        let removed_model = config.models.remove(&name).is_some();
        let removed_account = if !removed_legacy && !removed_model {
            if config.provider_accounts.remove(&name).is_some() {
                config.models.retain(|_, m| m.account != name);
                true
            } else {
                false
            }
        } else {
            false
        };
        if !removed_legacy && !removed_model && !removed_account {
            missing = true;
            anyhow::bail!("provider {name:?} not found");
        }
        if config.default_provider == name || config.default_model.as_deref() == Some(&name) {
            config.default_model = None;
            config.default_provider = config
                .models
                .keys()
                .min()
                .cloned()
                .or_else(|| config.providers.keys().min().cloned())
                .unwrap_or_default();
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
    let logical_models = config.logical_models();
    let mut ids: Vec<String> = logical_models.keys().cloned().collect();
    ids.sort();
    let providers: Vec<ProviderInfo> = ids
        .iter()
        .filter_map(|id| {
            config
                .provider_config_for_selection(id)
                .map(|p| {
                    let mut info = provider_info(id, &p, &default_selection);
                    if let Some(m) = logical_models.get(id) {
                        info.account = Some(m.account.clone());
                    }
                    info
                })
        })
        .collect();

    let logical_accounts = config.logical_accounts();
    let mut account_ids: Vec<String> = logical_accounts.keys().cloned().collect();
    account_ids.sort();
    let accounts: Vec<crate::AccountInfo> = account_ids
        .into_iter()
        .map(|id| {
            let a = &logical_accounts[&id];
            crate::AccountInfo {
                id: id.clone(),
                provider_type: a.provider.clone(),
                base_url: a.base_url.clone(),
                has_api_key: a.api_key.as_ref().is_some_and(|k| !k.is_empty()),
                skip_tls_verify: a.skip_tls_verify,
            }
        })
        .collect();

    Json(serde_json::json!({
        "default_provider": default_selection,
        "providers": providers,
        "accounts": accounts,
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

/// POST /provider-accounts / PUT /provider-accounts/:id
pub(crate) async fn create_or_update_provider_account(
    path_id: Option<Path<String>>,
    Json(req): Json<CreateOrUpdateAccountRequest>,
) -> impl IntoResponse {
    let raw_id = path_id
        .map(|Path(id)| id)
        .or(req.id)
        .unwrap_or_default();
    let id = match validate_provider_name(&raw_id) {
        Ok(id) => id,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e).into_response(),
    };

    let config = match update_config(|config| {
        let acc = config
            .provider_accounts
            .entry(id.clone())
            .or_insert_with(|| ProviderAccountConfig {
                provider: req.provider_type.clone().unwrap_or_else(|| "openai".into()),
                display_name: None,
                api_key: None,
                base_url: None,
                user_agent: None,
                skip_tls_verify: false,
                enterprise_url: None,
                ephemeral: false,
            });

        if let Some(provider_type) = req.provider_type {
            if !provider_type.trim().is_empty() {
                acc.provider = provider_type;
            }
        }
        if req.clear_api_key {
            acc.api_key = None;
        } else if let Some(key) = req.api_key {
            acc.api_key = key;
        }
        if req.clear_base_url {
            acc.base_url = None;
        } else if let Some(url) = req.base_url {
            acc.base_url = url;
        }
        if let Some(skip) = req.skip_tls_verify {
            acc.skip_tls_verify = skip;
        }
        Ok(())
    }) {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };

    let acc = &config.provider_accounts[&id];
    Json(crate::AccountInfo {
        id,
        provider_type: acc.provider.clone(),
        base_url: acc.base_url.clone(),
        has_api_key: acc.api_key.as_ref().is_some_and(|k| !k.is_empty()),
        skip_tls_verify: acc.skip_tls_verify,
    })
    .into_response()
}

/// DELETE /provider-accounts/:id
pub(crate) async fn delete_provider_account(Path(id): Path<String>) -> impl IntoResponse {
    let mut missing = false;
    let config = match update_config(|config| {
        let removed_account = config.provider_accounts.remove(&id).is_some();
        let removed_legacy = config.providers.remove(&id).is_some();
        if !removed_account && !removed_legacy {
            missing = true;
            anyhow::bail!("account {id:?} not found");
        }
        config.models.retain(|_, m| m.account != id);
        if config.default_provider == id || config.default_model.as_deref() == Some(&id) {
            config.default_model = None;
            config.default_provider = config
                .models
                .keys()
                .min()
                .cloned()
                .or_else(|| config.providers.keys().min().cloned())
                .unwrap_or_default();
        }
        Ok(())
    }) {
        Ok(c) => c,
        Err(_) if missing => {
            return json_error(StatusCode::NOT_FOUND, format!("Account '{}' not found", id))
                .into_response()
        }
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };

    Json(serde_json::json!({
        "success": true,
        "default_provider": config.effective_model_selection().unwrap_or_default(),
    }))
    .into_response()
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

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_config::config::Config;

    #[tokio::test]
    async fn patch_provider_works_on_new_schema_model() {
        update_config(|config| {
            config.provider_accounts.insert(
                "gemini".into(),
                ProviderAccountConfig {
                    provider: "anthropic".into(),
                    display_name: None,
                    api_key: Some("test-key".into()),
                    base_url: Some("http://127.0.0.1:8046/v1".into()),
                    user_agent: None,
                    skip_tls_verify: false,
                    enterprise_url: None,
                    ephemeral: false,
                },
            );
            config.models.insert(
                "gemini-3.8.flash-high".into(),
                ModelProfileConfig {
                    account: "gemini".into(),
                    model: "gemini-3.8-flash-high".into(),
                    display_name: None,
                    system_prompt: None,
                    context_window: 1_000_000,
                    max_tokens: None,
                    capable_model: None,
                    thinking_type: None,
                    thinking_keep: None,
                    reasoning_history: Some("include".into()),
                    reasoning_effort: Some("high".into()),
                    reasoning_levels: None,
                    thinking_enabled: Some(true),
                    thinking_budget: Some(2048),
                    pricing: None,
                    supports_vision: Some(true),
                    reasoning_model: Some(true),
                },
            );
            config.default_model = Some("gemini-3.8.flash-high".into());
            config.default_provider = "gemini-3.8.flash-high".into();
            Ok(())
        })
        .unwrap();

        // Call patch_provider to update reasoning_effort and context_window
        let req = PatchProviderRequest {
            name: None,
            provider_type: Some("anthropic".into()),
            model: Some("gemini-3.8-flash-high-v2".into()),
            account: None,
            api_key: None,
            clear_api_key: false,
            base_url: Some(Some("http://127.0.0.1:8046/v2".into())),
            clear_base_url: false,
            user_agent: None,
            clear_user_agent: false,
            context_window: Some(2_000_000),
            max_tokens: None,
            clear_max_tokens: false,
            thinking_enabled: Some(Some(true)),
            thinking_budget: Some(Some(4096)),
            thinking_type: None,
            thinking_keep: None,
            reasoning_history: Some(Some("include".into())),
            reasoning_effort: Some(Some("max".into())),
            skip_tls_verify: Some(false),
            pricing: None,
            clear_pricing: false,
            supports_vision: Some(Some(true)),
            clear_supports_vision: false,
            reasoning_model: Some(Some(true)),
            clear_reasoning_model: false,
        };

        let resp = patch_provider(Path("gemini-3.8.flash-high".into()), Json(req)).await;
        let response = resp.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify that config was updated
        let loaded = load_config().unwrap();
        let model = loaded.models.get("gemini-3.8.flash-high").expect("model should still exist");
        assert_eq!(model.model, "gemini-3.8-flash-high-v2");
        assert_eq!(model.context_window, 2_000_000);
        assert_eq!(model.reasoning_effort.as_deref(), Some("max"));

        let acc = loaded.provider_accounts.get("gemini").expect("account should exist");
        assert_eq!(acc.base_url.as_deref(), Some("http://127.0.0.1:8046/v2"));
    }
}
