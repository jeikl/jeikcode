//! `reasoning_content` round-trip POLICY for OpenAI-compatible providers.
//!
//! The kernel STORES reasoning losslessly on `Message.reasoning`; THIS module decides
//! the per-model *wire* behaviour — whether the prior turn's reasoning is echoed back
//! on the next request. (Mechanism lives in L0; policy lives here in L1.)
//!
//! Why per-model: OpenAI-compatible "reasoning models" disagree on the round-trip.
//!
//! ```text
//! deepseek-v4*           REQUIRES reasoning_content echoed on assistant tool-call
//!                        turns (HTTP 400 "must be passed back" otherwise); an empty
//!                        string is rejected, so a non-empty REASONING_PLACEHOLDER is
//!                        sent when no reasoning was captured.
//! deepseek-r1/reasoner   FORBIDS echoing reasoning_content (HTTP 400 if sent).
//! GLM / everything else  safe default: do not echo (GLM does not error either way;
//!                        omitting keeps requests minimal).
//! ```
//!
//! There is NO opaque signature on this path — reasoning is plain text — so the flat
//! kernel `reasoning: Option<String>` is fully sufficient (see its FUTURE doc note for
//! the signed-provider extension).

/// Placeholder echoed when a model REQUIRES `reasoning_content` on a historical
/// assistant message but none was captured (resumed/compacted history, or a turn
/// produced by a non-thinking model). DeepSeek-V4 rejects an *empty* `reasoning_content`
/// on tool-call messages, so a non-empty placeholder is mandatory under [`ReasoningPolicy::Include`].
///
/// It is a single NON-PROSE sentinel (`·`), not an English sentence: at high context a
/// history full of an English placeholder *sentence* led DeepSeek-V4-Flash to MIMIC it and
/// emit it as its only assistant text, stalling the turn. A bare middle-dot satisfies the
/// non-empty requirement without giving the model prose to echo (ported from core 54c9e4bb).
pub const REASONING_PLACEHOLDER: &str = "·";

/// Whether a model echoes prior-turn `reasoning_content` back on the next request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReasoningPolicy {
    /// Echo `reasoning_content` on assistant messages (placeholder if missing).
    Include,
    /// Never echo `reasoning_content`.
    Exclude,
}

impl ReasoningPolicy {
    /// Parse the user-facing `reasoning_history` config value into an explicit
    /// override. `None`/empty ⇒ `Ok(None)` (caller falls back to [`derive`]);
    /// `"include"`/`"exclude"` (case/space-insensitive) ⇒ the matching policy; any
    /// other value is a typo and fails fast — mirrors `atomcode-core`'s load-time
    /// validation so a bad config errors the same way on either engine.
    ///
    /// [`derive`]: ReasoningPolicy::derive
    pub fn from_config(value: Option<&str>) -> Result<Option<Self>, String> {
        match value.map(|s| s.trim().to_ascii_lowercase()) {
            None => Ok(None),
            Some(s) if s.is_empty() => Ok(None),
            Some(s) if s == "include" => Ok(Some(ReasoningPolicy::Include)),
            Some(s) if s == "exclude" => Ok(Some(ReasoningPolicy::Exclude)),
            Some(other) => Err(format!(
                "invalid `reasoning_history` value {other:?} — expected \"include\" or \
                 \"exclude\" (unset = auto-detect)"
            )),
        }
    }

    /// Resolve policy given optional explicit `reasoning_model` bool, optional `reasoning_history` string, model and base_url.
    pub fn resolve(
        reasoning_model: Option<bool>,
        reasoning_history: Option<&str>,
        model: &str,
        base_url: &str,
    ) -> Result<Self, String> {
        if let Some(explicit) = reasoning_model {
            return Ok(if explicit {
                ReasoningPolicy::Include
            } else {
                ReasoningPolicy::Exclude
            });
        }
        if let Some(parsed) = Self::from_config(reasoning_history)? {
            return Ok(parsed);
        }
        Ok(Self::derive(model, base_url))
    }

    /// Derive the default policy from the model name + base URL (some vendors are only
    /// identifiable by host, e.g. Moonshot/MiMo gateways). An explicit
    /// [`OpenAiCompatConfig::reasoning_policy`](super::OpenAiCompatConfig) override takes
    /// precedence over this. Faithfully ports `atomcode-core`'s `derive_reasoning_policy`.
    pub fn derive(model: &str, base_url: &str) -> Self {
        let m = model.to_ascii_lowercase();
        let u = base_url.to_ascii_lowercase();
        if m.contains("deepseek-reasoner") || m.contains("deepseek-r1") {
            // DeepSeek V3 family: rejects echoed reasoning_content (400).
            ReasoningPolicy::Exclude
        } else if m.contains("deepseek-v4") || m.contains("grok") {
            // DeepSeek V4 thinking mode / Grok 4.5/4.6: REQUIRES/expects reasoning_content on tool-call turns.
            ReasoningPolicy::Include
        } else if m.starts_with("kimi-")
            || m.starts_with("moonshot")
            || m.starts_with("mimo-")
            || u.contains("moonshot")
            || u.contains("kimi")
            || u.contains("xiaomimimo")
            || u.contains("mimo")
        {
            // Moonshot/Kimi/MiMo: require reasoning_content on every assistant tool_call.
            ReasoningPolicy::Include
        } else {
            // GLM / vanilla OpenAI-compat: safe default: do not echo.
            ReasoningPolicy::Exclude
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_v4_and_grok_include() {
        assert_eq!(
            ReasoningPolicy::derive("deepseek-v4-flash", ""),
            ReasoningPolicy::Include
        );
        assert_eq!(
            ReasoningPolicy::derive("DeepSeek-V4", ""),
            ReasoningPolicy::Include
        );
        assert_eq!(
            ReasoningPolicy::derive("grok-4.6", "https://api.x.ai/v1"),
            ReasoningPolicy::Include
        );
        assert_eq!(
            ReasoningPolicy::derive("grok-4.5", "https://gateway.example/v1"),
            ReasoningPolicy::Include
        );
    }

    #[test]
    fn deepseek_r1_excludes() {
        assert_eq!(
            ReasoningPolicy::derive("deepseek-r1", ""),
            ReasoningPolicy::Exclude
        );
        assert_eq!(
            ReasoningPolicy::derive("deepseek-reasoner", ""),
            ReasoningPolicy::Exclude
        );
        // r1 wins even if the URL looks like a moonshot host.
        assert_eq!(
            ReasoningPolicy::derive("deepseek-r1", "https://api.moonshot.cn/v1"),
            ReasoningPolicy::Exclude
        );
    }

    #[test]
    fn moonshot_kimi_mimo_include() {
        assert_eq!(
            ReasoningPolicy::derive("kimi-k2", ""),
            ReasoningPolicy::Include
        );
        assert_eq!(
            ReasoningPolicy::derive("moonshot-v1-8k", ""),
            ReasoningPolicy::Include
        );
        // MiMo by MODEL NAME (reuses DeepSeek-V4 thinking protocol) — even on a generic
        // gateway URL that doesn't contain "mimo".
        assert_eq!(
            ReasoningPolicy::derive("mimo-v2.5-pro", "https://generic-gateway.example/v1"),
            ReasoningPolicy::Include
        );
        // identifiable only by host:
        assert_eq!(
            ReasoningPolicy::derive("some-model", "https://api.moonshot.cn/v1"),
            ReasoningPolicy::Include
        );
        assert_eq!(
            ReasoningPolicy::derive("some-model", "https://api-inference.xiaomimimo.com/v1"),
            ReasoningPolicy::Include
        );
    }

    #[test]
    fn from_config_parses_override_or_errors() {
        assert_eq!(ReasoningPolicy::from_config(None), Ok(None));
        assert_eq!(ReasoningPolicy::from_config(Some("")), Ok(None));
        assert_eq!(ReasoningPolicy::from_config(Some("  ")), Ok(None));
        assert_eq!(
            ReasoningPolicy::from_config(Some("include")),
            Ok(Some(ReasoningPolicy::Include))
        );
        assert_eq!(
            ReasoningPolicy::from_config(Some(" Exclude ")),
            Ok(Some(ReasoningPolicy::Exclude))
        );
        assert!(ReasoningPolicy::from_config(Some("sometimes")).is_err());
    }

    #[test]
    fn resolve_prioritizes_reasoning_model_then_config_then_derive() {
        // Explicit reasoning_model = Some(true) forces Include even on deepseek-r1
        assert_eq!(
            ReasoningPolicy::resolve(Some(true), None, "deepseek-r1", ""),
            Ok(ReasoningPolicy::Include)
        );
        // Explicit reasoning_model = Some(false) forces Exclude even on grok
        assert_eq!(
            ReasoningPolicy::resolve(Some(false), None, "grok-4.6", ""),
            Ok(ReasoningPolicy::Exclude)
        );
        // reasoning_history override wins if reasoning_model is None
        assert_eq!(
            ReasoningPolicy::resolve(None, Some("include"), "gpt-4o", ""),
            Ok(ReasoningPolicy::Include)
        );
        assert_eq!(
            ReasoningPolicy::resolve(None, Some("exclude"), "grok-4.6", ""),
            Ok(ReasoningPolicy::Exclude)
        );
        // Fallback to derive
        assert_eq!(
            ReasoningPolicy::resolve(None, None, "grok-4.6", ""),
            Ok(ReasoningPolicy::Include)
        );
        assert_eq!(
            ReasoningPolicy::resolve(None, None, "gpt-4o", ""),
            Ok(ReasoningPolicy::Exclude)
        );
    }

    #[test]
    fn glm_and_default_exclude() {
        assert_eq!(
            ReasoningPolicy::derive("glm-5.1", "https://open.bigmodel.cn/api/paas/v4"),
            ReasoningPolicy::Exclude
        );
        assert_eq!(
            ReasoningPolicy::derive("gpt-4o", "https://api.openai.com/v1"),
            ReasoningPolicy::Exclude
        );
        assert_eq!(ReasoningPolicy::derive("", ""), ReasoningPolicy::Exclude);
    }
}
