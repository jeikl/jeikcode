//! Frozen-user routing card for code intelligence.
//!
//! Tool descriptions are under-weighted by weak models; the skill catalog already
//! proved a sacred-floor User block lifts willingness. This is the same seam:
//! a short `=== CODE TOOLS ===` card that compaction cannot drain.

use async_trait::async_trait;
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::Conversation;

pub const CODE_TOOLS_HEADER: &str = "=== CODE TOOLS ===";

const CARD: &str = "\
=== CODE TOOLS ===
Natural-language code question or precise symbol lookup \
→ `code_explore(path=<DIR/module>, query=<中文/English or symbol>)`
  GOOD: path=crates/atomcode-coding  query=CodeExploreTool
  GOOD: path=src/auth               query=用户登录如何校验
  BAD:  path=src/auth.rs            (file → read_file)
  BAD:  path=.                      (workspace root → repo_map)
Workspace layout → `repo_map` (only this tool may use path `.`)
Exact literals / error strings / TODO → `grep(pattern, path)`
Already-located file and line → `read_file` (default 1000 lines; if a footer remains, \
call again with that offset and omit `limit` to finish the file)
Do not wander with grep+read in place of `code_explore`. Never pass path `.` or a file to `code_explore`.
A thin/empty `code_explore` result is not absence — read Coverage/CATALOG and retry synonyms \
or a broader directory.";

/// Injects the routing card as a frozen synthetic User after skills / memory.
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
        convo.reconcile_frozen_user_block(CODE_TOOLS_HEADER, block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::message::{Message, Role};

    #[tokio::test]
    async fn injects_frozen_user_card_when_mounted() {
        let hook = CodeToolsHook::new(true);
        let mut c = Conversation::default();
        c.push(Message::system("PERSONA"));
        c.push(Message::user("hi"));
        hook.session_start(&mut c, false).await;
        assert_eq!(c.messages[1].role, Role::User);
        assert!(c.messages[1].synthetic);
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
}
