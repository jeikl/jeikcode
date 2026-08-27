//! System routing card for code intelligence.
//!
//! Tool descriptions are under-weighted by weak models, so a short
//! `=== CODE TOOLS ===` block lives in the leading System run. Keeping it out of
//! synthetic User context preserves the boundary between runtime guidance and the
//! user's actual request.

use async_trait::async_trait;
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::{Conversation, Message, Role};

pub const CODE_TOOLS_HEADER: &str = "=== CODE TOOLS ===";

const CARD: &str = "\
=== CODE TOOLS ===
Natural-language code question or precise symbol lookup \
→ `code_explore(path=<DIR/module>, query=<中文/English or symbol>)`
  GOOD: path=crates/atomcode-coding  query=CodeExploreTool
  GOOD: path=src/auth               query=用户登录如何校验
  GOOD: path=.                      query=会话压缩如何工作
  BAD:  path=src/auth.rs            (file → read_file)
Workspace layout only → `repo_map` (do not pair with list_directory)
Exact literals / error strings / TODO → `grep(pattern, path)`
Already-located file and line → `read_file` (default 1000 lines; if a footer remains, \
call again with that offset and omit `limit` to finish the file)
Do not wander with grep+read in place of `code_explore`. Never pass a file to `code_explore`.
A thin/empty `code_explore` result is not absence — read Coverage/CATALOG and retry synonyms \
or a broader directory.";

/// Injects the routing card into the leading System run.
pub struct CodeToolsHook {
    enabled: bool,
}

impl CodeToolsHook {
    pub fn new(code_explore_mounted: bool) -> Self {
        Self {
            enabled: code_explore_mounted,
        }
    }
}

#[async_trait]
impl LifecycleHooks for CodeToolsHook {
    async fn session_start(&self, convo: &mut Conversation, _resumed: bool) {
        let block = self.enabled.then(|| CARD.to_string());
        // Remove both the new System form and legacy synthetic-User copies, then
        // insert exactly one block at the end of the leading System run.
        convo.messages.retain(|m| {
            !(m.text.starts_with(CODE_TOOLS_HEADER)
                && (m.role == Role::System || (m.role == Role::User && m.synthetic)))
        });
        if let Some(block) = block {
            let at = convo
                .messages
                .iter()
                .take_while(|m| m.role == Role::System)
                .count();
            convo.messages.insert(at, Message::system(block));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn injects_system_card_when_mounted() {
        let hook = CodeToolsHook::new(true);
        let mut c = Conversation::default();
        c.push(Message::system("PERSONA"));
        c.push(Message::user("hi"));
        hook.session_start(&mut c, false).await;
        assert_eq!(c.messages[1].role, Role::System);
        assert!(!c.messages[1].synthetic);
        assert!(c.messages[1].text.starts_with(CODE_TOOLS_HEADER));
        assert!(c.messages[1].text.contains("code_explore"));
        assert!(
            c.messages[1].text.contains("src/auth")
                && c.messages[1].text.contains("src/auth.rs")
                && c.messages[1].text.contains("read_file"),
            "card must show directory GOOD / file BAD before the model calls: {}",
            c.messages[1].text
        );
        assert_eq!(c.messages[2].text, "hi");
    }

    #[tokio::test]
    async fn no_op_when_not_mounted() {
        let hook = CodeToolsHook::new(false);
        let mut c = Conversation::default();
        c.push(Message::user("hi"));
        hook.session_start(&mut c, false).await;
        assert_eq!(c.messages.len(), 1);
    }

    #[tokio::test]
    async fn resume_converts_legacy_synthetic_user_to_system() {
        let hook = CodeToolsHook::new(true);
        let mut c = Conversation::default();
        c.push(Message::system("PERSONA"));
        c.push(Message::synthetic_user(format!(
            "{CODE_TOOLS_HEADER}\nlegacy"
        )));
        c.push(Message::user("hi"));
        hook.session_start(&mut c, true).await;
        assert_eq!(c.messages.len(), 3);
        assert_eq!(c.messages[1].role, Role::System);
        assert!(c.messages[1].text.contains("path=."));
        assert_eq!(c.messages[2].text, "hi");
    }
}
