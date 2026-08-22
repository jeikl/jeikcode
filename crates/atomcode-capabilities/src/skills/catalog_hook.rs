//! `SkillCatalogHook` — injects the `=== AVAILABLE SKILLS ===` catalog as a
//! frozen synthetic User message at session start.
//!
//! Why this exists: the v2 coding path registered the `use_skill` / `list_skills`
//! tools but NEVER told the model which skills are installed — so a skill that
//! should trigger on a description match (brainstorming before creative work, …)
//! was effectively invisible and "basically never fired". The catalog is what the
//! daemon path already injected inline; this hook brings the coding path to parity
//! (via the budget-gated, source-ranked [`super::render`]).
//!
//! Identified by [`super::render::CATALOG_HEADER`] so `--resume` reconciles it
//! in place; lands in the frozen user prefix after persona + session context +
//! memory. User-owned skills sit inside `sacred_floor`, so compaction cannot
//! drain them. The catalog is frozen at session start (skills don't change
//! mid-session; `/cd` is a new session).

use super::render::CATALOG_HEADER;
use async_trait::async_trait;
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::Conversation;

/// Injects the pre-rendered skill catalog. `None` catalog (no skills installed) →
/// the hook is a no-op on a fresh session and prunes a stale block on resume.
pub struct SkillCatalogHook {
    catalog: Option<String>,
}

impl SkillCatalogHook {
    /// Build from a rendered catalog (see [`super::SkillRegistry::render_catalog`]).
    pub fn new(catalog: Option<String>) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl LifecycleHooks for SkillCatalogHook {
    async fn session_start(&self, convo: &mut Conversation, _resumed: bool) {
        convo.reconcile_frozen_user_block(CATALOG_HEADER, self.catalog.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::message::{Message, Role};

    fn convo_with_persona() -> Conversation {
        let mut c = Conversation::default();
        c.push(Message::system("PERSONA"));
        c.push(Message::user("hi"));
        c
    }

    #[tokio::test]
    async fn fresh_inserts_after_leading_system_run() {
        let hook = SkillCatalogHook::new(Some(format!("{CATALOG_HEADER}\n- x: y")));
        let mut c = convo_with_persona();
        hook.session_start(&mut c, false).await;
        assert_eq!(c.messages[0].text, "PERSONA");
        assert_eq!(c.messages[1].role, Role::User);
        assert!(c.messages[1].synthetic, "catalog is a frozen user prefix");
        assert!(
            c.messages[1].text.starts_with(CATALOG_HEADER),
            "catalog after persona"
        );
        assert_eq!(c.messages[2].role, Role::User, "before the user message");
        assert!(!c.messages[2].synthetic);
    }

    #[tokio::test]
    async fn none_catalog_is_noop_on_fresh() {
        let hook = SkillCatalogHook::new(None);
        let mut c = convo_with_persona();
        let before = c.messages.len();
        hook.session_start(&mut c, false).await;
        assert_eq!(c.messages.len(), before, "no skills → nothing injected");
    }

    #[tokio::test]
    async fn resume_refreshes_in_place_no_growth() {
        let hook = SkillCatalogHook::new(Some(format!("{CATALOG_HEADER}\n- fresh: v")));
        let mut c = Conversation::default();
        c.push(Message::system("PERSONA"));
        c.push(Message::system(format!("{CATALOG_HEADER}\n- stale: old")));
        c.push(Message::user("hi"));
        hook.session_start(&mut c, true).await;
        assert_eq!(c.messages.len(), 3, "reconciled in place, no growth");
        assert_eq!(c.messages[1].role, Role::User);
        assert!(c.messages[1].synthetic);
        assert!(c.messages[1].text.contains("- fresh: v"));
        assert!(!c.messages[1].text.contains("stale"));
    }

    #[tokio::test]
    async fn resume_prunes_stale_when_no_skills_left() {
        let hook = SkillCatalogHook::new(None);
        let mut c = Conversation::default();
        c.push(Message::system("PERSONA"));
        c.push(Message::system(format!("{CATALOG_HEADER}\n- gone: x")));
        c.push(Message::user("hi"));
        hook.session_start(&mut c, true).await;
        assert_eq!(c.messages.len(), 2, "stale catalog pruned");
        assert!(c
            .messages
            .iter()
            .all(|m| !m.text.starts_with(CATALOG_HEADER)));
    }
}
