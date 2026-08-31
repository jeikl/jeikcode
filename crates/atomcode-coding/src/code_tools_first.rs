//! Opening-turn reminder: prefer `code_explore` over grep+read.
//!
//! Gated to weak instruction-following families (GLM / DeepSeek) via
//! [`crate::persona::model_needs_firm_tool_steering`]. One-shot on the first
//! real user, above the query (same placement as [`crate::skill_first`]).

use async_trait::async_trait;
use atomcode_capabilities::reminder::{insert_before_last_real_user, system_reminder};
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::{Conversation, Message, Role};

pub struct CodeToolsFirstHook {
    enabled: bool,
}

impl CodeToolsFirstHook {
    pub fn new(model: &str, code_explore_mounted: bool) -> Self {
        Self {
            enabled: code_explore_mounted && crate::persona::model_needs_firm_tool_steering(model),
        }
    }

    fn body() -> &'static str {
        "For a natural-language code question or a symbol lookup, call `code_explore` \
NOW: path MUST be a directory/module (crates/atomcode-coding, src/auth), never a file \
(src/auth.rs). query is a precise symbol OR Chinese/English (鉴权怎么做). \
Do not start with grep+read_file. Grep is only for exact literals. If a read_file footer \
shows remaining lines, continue with that offset and omit `limit`."
    }
}

#[async_trait]
impl LifecycleHooks for CodeToolsFirstHook {
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

    #[tokio::test]
    async fn glm_opening_turn_injects() {
        let hook = CodeToolsFirstHook::new("glm-5.2", true);
        let mut c = convo(vec![Message::user("how does auth work")]);
        hook.turn_start(&mut c).await;
        assert_eq!(c.messages.len(), 2);
        assert!(c.messages[0].synthetic && c.messages[0].text.contains("code_explore"));
        assert!(
            c.messages[0].text.contains("src/auth")
                && c.messages[0].text.contains("src/auth.rs")
                && c.messages[0].text.contains("never a file"),
            "{}",
            c.messages[0].text
        );
        assert_eq!(c.messages[1].text, "how does auth work");
    }

    #[tokio::test]
    async fn frontier_is_noop() {
        let hook = CodeToolsFirstHook::new("claude-opus", true);
        let mut c = convo(vec![Message::user("hi")]);
        hook.turn_start(&mut c).await;
        assert_eq!(c.messages.len(), 1);
    }
}
