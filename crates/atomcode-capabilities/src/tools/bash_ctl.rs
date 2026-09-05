//! `long_bash_keyword_actions` / `bash_kill_by_id`.
//!
//! `action=add` with `global=false` (default) writes the session sidecar
//! (`<id>.bashkw.json`) so a JeikCode restart + `/resume` still sees it.
//! `global=true` also appends `[tools.bash] long_bash_command_keyword`.

use super::bash_runtime::{
    add_live_long_keyword, kill_by_id, live_long_keywords, promote_matching,
    remove_live_long_keyword, session_long_keywords,
};
use super::{err, ok};
use async_trait::async_trait;
use atomcode_kernel::tool::{RiskLevel, Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;

#[derive(Default)]
pub struct LongBashKeywordActionsTool;

#[derive(Deserialize)]
struct ActionsArgs {
    action: String,
    #[serde(alias = "bashkeyword")]
    keyword: String,
    #[serde(default, alias = "persist")]
    global: bool,
}

#[async_trait]
impl Tool for LongBashKeywordActionsTool {
    fn name(&self) -> &str {
        "long_bash_keyword_actions"
    }
    fn description(&self) -> &str {
        "Add or remove command keywords treated as long-running batch jobs (e.g. `mvn`, `cargo`, `ninja`). \
         When to use: When whitelisting slow build or test commands to prevent idle timeouts. \
         When NOT to use: Do NOT use for resident services or background daemons."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "delete"],
                    "description": "Action to perform: 'add' to register a keyword, 'delete' to remove it."
                },
                "keyword": {
                    "type": "string",
                    "description": "Command keyword token (e.g. 'mvn', 'cargo', 'ninja')."
                },
                "global": {
                    "type": "boolean",
                    "default": false,
                    "description": "Persist in config.toml for all future sessions (default false, current session only)."
                }
            },
            "required": ["action", "keyword"]
        })
    }
    fn risk(&self, _args: &str) -> RiskLevel {
        RiskLevel::Safe
    }
    fn parallel_safe(&self, _args: &str) -> bool {
        true
    }
    async fn execute(&self, args: &str, _ctx: &ToolContext) -> ToolResult {
        let a: ActionsArgs = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => {
                return err(format!(
                    "long_bash_keyword_actions: invalid arguments: {e}. \
                     Expected {{\"action\":\"add|delete\",\"keyword\":\"<token>\"}}."
                ))
            }
        };
        let action = a.action.trim().to_ascii_lowercase();
        let keyword = a.keyword.trim();
        if keyword.is_empty() {
            return err("long_bash_keyword_actions: keyword must not be empty");
        }
        match action.as_str() {
            "add" => execute_add(keyword, a.global),
            "delete" | "remove" => execute_delete(keyword, a.global),
            other => err(format!(
                "long_bash_keyword_actions: unknown action `{other}`. Use add or delete."
            )),
        }
    }
}

fn execute_add(keyword: &str, global: bool) -> ToolResult {
    add_live_long_keyword(keyword);
    let promoted = promote_matching(keyword);
    if global {
        match atomcode_config::config::append_long_bash_command_keyword(keyword) {
            Ok(inserted) => ok(format!(
                "session keyword `{keyword}` on; global={} (new on disk); \
                 promoted {promoted} live bash task(s). Original pane keeps streaming. \
                 Session list: {}",
                if inserted {
                    "written"
                } else {
                    "already in config"
                },
                session_list_preview()
            )),
            Err(e) => err(format!(
                "session keyword `{keyword}` on and promoted {promoted} task(s), \
                 but failed to persist config: {e}"
            )),
        }
    } else {
        ok(format!(
            "session keyword `{keyword}` on (not written to config.toml; \
             kept with this session across JeikCode restart). \
             promoted {promoted} live bash task(s). Pass global=true to keep it for every session. \
             Original pane keeps streaming. Keywords now: {}",
            live_long_keywords().join(", ")
        ))
    }
}

fn execute_delete(keyword: &str, global: bool) -> ToolResult {
    let session = remove_live_long_keyword(keyword);
    if global {
        match atomcode_config::config::remove_long_bash_command_keyword(keyword) {
            Ok(disk) => ok(format!(
                "removed `{keyword}` from session={session} config={disk}."
            )),
            Err(e) => err(format!(
                "removed `{keyword}` from session={session}, but config write failed: {e}"
            )),
        }
    } else {
        ok(format!(
            "removed `{keyword}` from session overlay={session}. \
             Pass global=true to also edit config.toml. Session list: {}",
            session_list_preview()
        ))
    }
}

fn session_list_preview() -> String {
    let v = session_long_keywords();
    if v.is_empty() {
        "(empty)".to_string()
    } else {
        v.join("、")
    }
}

#[derive(Default)]
pub struct BashKillByIdTool;

#[derive(Deserialize)]
struct KillArgs {
    bashid: String,
}

#[async_trait]
impl Tool for BashKillByIdTool {
    fn name(&self) -> &str {
        "bash_kill_by_id"
    }
    fn description(&self) -> &str {
        "Terminate a running background bash command by its `bashid`. \
         When to use: When stopping a stuck, timed-out, or unwanted background bash task."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "bashid": {
                    "type": "string",
                    "description": "The bashid of the running background bash command."
                }
            },
            "required": ["bashid"]
        })
    }
    fn risk(&self, _args: &str) -> RiskLevel {
        RiskLevel::Safe
    }
    fn parallel_safe(&self, _args: &str) -> bool {
        true
    }
    async fn execute(&self, args: &str, _ctx: &ToolContext) -> ToolResult {
        let a: KillArgs = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => {
                return err(format!(
                    "bash_kill_by_id: invalid arguments: {e}. Expected {{\"bashid\":\"b-…\"}}."
                ))
            }
        };
        let id = a.bashid.trim();
        if id.is_empty() {
            return err("bash_kill_by_id: bashid must not be empty");
        }
        if kill_by_id(id) {
            ok(format!(
                "signaled {id} to stop. The original bash pane will show \
                 `[task was canceled by bash kill tool]`."
            ))
        } else {
            err(format!(
                "bash_kill_by_id: no live bash with bashid `{id}`. It may have already exited."
            ))
        }
    }
}
