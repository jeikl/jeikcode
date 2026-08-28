//! `jeikcode_config_reload` — Ask the running JeikCode process to remount
//! `config.toml`, MCP servers, and skills after the agent finishes writing them.
//!
//! The tool itself only records a process-wide request. Drivers apply it at an
//! idle boundary (after the current turn): TUI `/reload`+`/mcp reload` path,
//! daemon `POST /mcp/reload` + live `reload_capabilities`. New MCP tools become
//! visible on the **next** user turn because the tool catalog is a cache prefix.

use super::ok;
use crate::config_reload::request_config_reload;
use crate::tool_feedback::parse_tool_args;
use async_trait::async_trait;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;

#[derive(Default)]
pub struct JeikcodeConfigReloadTool;

impl JeikcodeConfigReloadTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize, Default)]
struct Args {
    /// Optional scope hint for the model; ignored by the runtime (always full reload).
    #[serde(default)]
    #[allow(dead_code)]
    scope: Option<String>,
}

#[async_trait]
impl Tool for JeikcodeConfigReloadTool {
    fn name(&self) -> &str {
        "jeikcode_config_reload"
    }

    fn description(&self) -> &str {
        "Reload JeikCode configuration after you have written `~/.atomcode/config.toml`, \
         `~/.atomcode/mcp.json`, `<workspace>/.mcp.json`, or skills. Call this once the files \
         are saved so the running session remounts models, MCP servers, and skills. \
         The reload is applied after THIS turn completes; newly connected MCP tools become \
         available on the NEXT user message. Do not ask the user to restart JeikCode. \
         Equivalent to the user running `/reload` plus `/mcp reload`."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["all", "mcp", "config"],
                    "description": "Optional hint. The runtime always reloads config.toml, MCP, and skills together."
                }
            }
        })
    }

    fn read_only_hint(&self) -> bool {
        false
    }

    fn never_truncate_result(&self) -> bool {
        true
    }

    async fn execute(&self, args: &str, _ctx: &ToolContext) -> ToolResult {
        let args = if args.trim().is_empty() { "{}" } else { args };
        let _parsed: Args = match parse_tool_args("jeikcode_config_reload", args, r#"{}"#) {
            Ok(a) => a,
            Err(e) => return e.into_tool_result(),
        };
        request_config_reload();
        ok(
            "Configuration reload requested. `config.toml`, MCP servers (`mcp.json`), and skills \
             will remount after this turn completes. Newly connected MCP tools are available on \
             the next user message. You can also tell the user to click the MCP refresh button \
             or run `/mcp reload` / `/reload` if they want to refresh the panel immediately.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_reload::{
        clear_pending_config_reload, pending_config_reload, take_pending_live_reload,
        take_pending_mcp_cache_reload,
    };
    use atomcode_kernel::tool::{ProgressSink, Tool};
    use std::path::PathBuf;

    fn ctx() -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from("."),
            cancel: tokio_util::sync::CancellationToken::new(),
            progress: ProgressSink::noop(),
            requester: None,
        }
    }

    #[tokio::test]
    #[serial_test::serial(config_reload)]
    async fn reload_tool_sets_pending_flags() {
        clear_pending_config_reload();
        let tool = JeikcodeConfigReloadTool::new();
        let res = tool.execute("{}", &ctx()).await;
        assert!(!res.is_error);
        assert!(res.content.contains("reload"));
        assert!(pending_config_reload());
        assert!(take_pending_live_reload());
        assert!(take_pending_mcp_cache_reload());
        assert!(!pending_config_reload());
        clear_pending_config_reload();
    }

    #[tokio::test]
    #[serial_test::serial(config_reload)]
    async fn reload_tool_accepts_empty_and_scope() {
        clear_pending_config_reload();
        let tool = JeikcodeConfigReloadTool::new();
        let empty = tool.execute("", &ctx()).await;
        assert!(!empty.is_error);
        assert!(take_pending_live_reload());
        assert!(take_pending_mcp_cache_reload());
        let res = tool.execute(r#"{"scope":"mcp"}"#, &ctx()).await;
        assert!(!res.is_error);
        assert!(take_pending_live_reload());
        assert!(take_pending_mcp_cache_reload());
        clear_pending_config_reload();
    }
}
