use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use atomcode_capabilities::mcp::registry::MCP_SERVER_INSTRUCTIONS_TAG;
use atomcode_capabilities::mcp::McpRegistry;
use atomcode_kernel::hook::LifecycleHooks;
use atomcode_kernel::message::Conversation;

pub const MCP_INSTRUCTIONS_HEADER: &str = "<mcp-server-instructions>";

/// Injects the connected servers' current instructions into the session's frozen
/// user prefix at `session_start` and `turn_start`.
///
/// Lands after the skill catalog, before the first real query. User-owned MCP
/// guidance sits inside `sacred_floor`, so compaction cannot drain it.
/// Unchanged bytes keep the prompt-cache prefix intact; the user's latest query
/// stays the final message.
pub(crate) struct McpInstructionsHook {
    registry: Arc<McpRegistry>,
    mounted_tools: Arc<RwLock<Vec<String>>>,
}

impl McpInstructionsHook {
    pub(crate) fn new(registry: Arc<McpRegistry>, mounted_tools: Arc<RwLock<Vec<String>>>) -> Self {
        Self {
            registry,
            mounted_tools,
        }
    }

    fn render_instructions(&self) -> Option<String> {
        let mounted = self
            .mounted_tools
            .read()
            .map(|names| names.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
        let instructions = self.registry.instructions_for_mounted_tools(&mounted)?;
        Some(format!(
            "<{MCP_SERVER_INSTRUCTIONS_TAG}>\n{instructions}\n</{MCP_SERVER_INSTRUCTIONS_TAG}>"
        ))
    }

    fn refresh_in_place(&self, convo: &mut Conversation) {
        convo.reconcile_frozen_user_block(MCP_INSTRUCTIONS_HEADER, self.render_instructions());
    }
}

#[async_trait]
impl LifecycleHooks for McpInstructionsHook {
    async fn session_start(&self, convo: &mut Conversation, _resumed: bool) {
        self.refresh_in_place(convo);
    }

    async fn turn_start(&self, convo: &mut Conversation) {
        self.refresh_in_place(convo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::message::Message;

    fn convo_with_persona() -> Conversation {
        let mut c = Conversation::default();
        c.push(Message::system("PERSONA"));
        c.push(Message::user("request"));
        c
    }

    #[tokio::test]
    async fn injects_mcp_guidance_into_leading_system_run() {
        let registry = Arc::new(McpRegistry::new());
        let mounted = Arc::new(RwLock::new(vec![]));
        let hook = McpInstructionsHook::new(registry, mounted);

        let mut convo = convo_with_persona();
        hook.refresh_in_place(&mut convo);
        assert_eq!(convo.messages.len(), 2);
    }
}
