//! `SkillFirstHook` — a DeepSeek-only opening-turn `<system-reminder>` that forces a
//! skill-first check before the model explores or proposes a solution.
//!
//! A weak model (DeepSeek) under-weights the soft `## SKILLS:` guidance and the static
//! `SKILL/PROCESS FIRST` persona line (both proved insufficient on real hardware): it
//! opens by exploring the codebase and pre-solutioning instead of loading a matching
//! process skill. This injects the skill-first directive immediately ABOVE the
//! opening user query (Grok Build order).
//!
//! Gated to DeepSeek (via `model_needs_firm_execution`) AND a non-empty skill catalog
//! (never nudge `use_skill` when no skills are installed). One-shot: opening turn only.
//!
//! Unlike a tail reminder, this sits ABOVE the user query (Grok Build order). We fire
//! on the opening user turn — the reminder must preempt the
//! model's very first action. Consecutive user messages are kept on OpenAI/Responses
//! wires; Anthropic merges them with the query last.

use async_trait::async_trait;
use atomcode_capabilities::reminder::{insert_before_last_real_user, system_reminder};
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::{Conversation, Message, Role};

/// Injects a one-shot skill-first `<system-reminder>` on the opening turn, for DeepSeek only.
pub struct SkillFirstHook {
    /// Precomputed at construction: DeepSeek AND at least one skill installed.
    enabled: bool,
}

impl SkillFirstHook {
    /// Enabled only for a weak model needing firm steering (DeepSeek) AND when the skill
    /// catalog is non-empty (`has_skills`). Anything else yields a no-op hook.
    pub fn new(model: &str, has_skills: bool) -> Self {
        Self {
            enabled: has_skills && crate::persona::model_needs_firm_execution(model),
        }
    }

    /// The forceful skill-first reminder body (pure, testable). Wrapped by
    /// `system_reminder` before injection.
    fn body() -> &'static str {
        "Before you explore the codebase, plan, or propose a solution: check the \
\"=== AVAILABLE SKILLS ===\" catalog above. If this request matches a skill's description \
shown in that catalog, you MUST call `use_skill` with that exact listed name NOW and let it \
drive. Never infer a skill name merely from the task type. If no listed description matches, \
proceed normally without `use_skill`."
    }
}

#[async_trait]
impl LifecycleHooks for SkillFirstHook {
    async fn turn_start(&self, convo: &mut Conversation) {
        if !self.enabled {
            return;
        }
        let real_users = convo
            .messages
            .iter()
            .filter(|m| m.role == Role::User && !m.synthetic)
            .count();
        if real_users != 1 {
            return;
        }
        insert_before_last_real_user(
            &mut convo.messages,
            Message::synthetic_user(system_reminder(Self::body())),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convo(msgs: Vec<Message>) -> Conversation {
        let mut c = Conversation::default();
        c.messages = msgs;
        c
    }

    #[test]
    fn body_requires_an_exact_catalog_match_without_naming_a_skill() {
        let b = SkillFirstHook::body();
        assert!(b.contains("use_skill"), "{b}");
        assert!(b.contains("exact listed name"), "{b}");
        assert!(b.contains("Never infer a skill name"), "{b}");
        assert!(!b.contains("brainstorming"), "{b}");
    }

    #[tokio::test]
    async fn deepseek_opening_turn_injects_one_wrapped_reminder() {
        let hook = SkillFirstHook::new("deepseek-v4-flash", true);
        let mut c = convo(vec![Message::system("s"), Message::user("hi")]);
        hook.turn_start(&mut c).await;
        assert_eq!(c.messages.len(), 3, "opening turn inserts exactly one reminder");
        assert!(
            c.messages[1].synthetic
                && c.messages[1].text.starts_with("<system-reminder>")
                && c.messages[1].text.contains("use_skill"),
            "wrapped skill-first reminder above the query: {:?}",
            c.messages[1].text
        );
        assert_eq!(c.messages[2].text, "hi");
    }

    #[tokio::test]
    async fn does_not_fire_after_the_opening_turn() {
        let hook = SkillFirstHook::new("deepseek-v4-flash", true);
        let mut a = convo(vec![
            Message::user("hi"),
            Message::assistant("a", vec![]),
            Message::user("again"),
        ]);
        let before_a = a.messages.clone();
        hook.turn_start(&mut a).await;
        assert_eq!(
            a.messages, before_a,
            "must not fire when more than one real user is present"
        );
    }

    #[tokio::test]
    async fn disabled_for_glm_frontier_and_empty_catalog() {
        for (model, has_skills) in [("glm-5.2", true), ("m", true), ("deepseek-v4-flash", false)] {
            let hook = SkillFirstHook::new(model, has_skills);
            let mut c = convo(vec![Message::user("hi")]);
            let before = c.messages.clone();
            hook.turn_start(&mut c).await;
            assert_eq!(
                c.messages, before,
                "must be a no-op for (model={model}, has_skills={has_skills})"
            );
        }
    }
}
