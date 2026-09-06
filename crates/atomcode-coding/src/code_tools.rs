//! Code intelligence lifecycle hook.
//!
//! Tool usage and few-shot examples (e.g. `code_explore` path/query patterns)
//! are now declared directly inside `code_explore`'s own parameters schema,
//! matching the Grok/OpenCode schema-first architecture.
//!
//! This hook remains active purely to clean up legacy `=== CODE TOOLS ===`
//! system/user blocks from resumed conversations, preserving a pristine system prompt.

use async_trait::async_trait;
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::{Conversation, Role};

pub const CODE_TOOLS_HEADER: &str = "=== CODE TOOLS ===";

/// Reconciles and cleans up legacy code tools card from conversations.
pub struct CodeToolsHook;

impl CodeToolsHook {
    pub fn new(_code_explore_mounted: bool) -> Self {
        Self
    }
}

#[async_trait]
impl LifecycleHooks for CodeToolsHook {
    async fn session_start(&self, convo: &mut Conversation, _resumed: bool) {
        // Remove legacy synthetic-User copies
        convo.messages.retain(|m| {
            !(m.text.starts_with(CODE_TOOLS_HEADER) && m.role == Role::User && m.synthetic)
        });
        // Purge legacy system card block
        convo.reconcile_system_block(CODE_TOOLS_HEADER, None);
    }

    async fn turn_start(&self, convo: &mut Conversation) {
        convo.reconcile_system_block(CODE_TOOLS_HEADER, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::message::Message;

    #[tokio::test]
    async fn purges_legacy_card_when_session_starts() {
        let hook = CodeToolsHook::new(true);
        let mut c = Conversation::default();
        c.push(Message::system("PERSONA"));
        c.push(Message::system(format!("{CODE_TOOLS_HEADER}\nold card")));
        c.push(Message::user("hi"));
        hook.session_start(&mut c, false).await;
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[0].text, "PERSONA");
        assert_eq!(c.messages[1].text, "hi");
        assert!(!c.messages.iter().any(|m| m.text.contains(CODE_TOOLS_HEADER)));
    }

    #[tokio::test]
    async fn purges_legacy_synthetic_user_card() {
        let hook = CodeToolsHook::new(true);
        let mut c = Conversation::default();
        c.push(Message::system("PERSONA"));
        c.push(Message::synthetic_user(format!(
            "{CODE_TOOLS_HEADER}\nlegacy"
        )));
        c.push(Message::user("hi"));
        hook.session_start(&mut c, true).await;
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[0].text, "PERSONA");
        assert_eq!(c.messages[1].text, "hi");
        assert!(!c.messages.iter().any(|m| m.text.contains(CODE_TOOLS_HEADER)));
    }

    #[tokio::test]
    async fn does_not_inject_card_into_clean_conversation() {
        let hook = CodeToolsHook::new(true);
        let mut c = Conversation::default();
        c.push(Message::system("PERSONA"));
        c.push(Message::user("hi"));
        hook.session_start(&mut c, false).await;
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[0].text, "PERSONA");
        assert_eq!(c.messages[1].text, "hi");
    }
}
