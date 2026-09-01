//! `long_bash_keyword_add` and `bash_kill_by_id` — model-facing controls for a
//! short bash that printed then went silent. Both are parallel-safe so they
//! complete independently of the original (still-running) bash pane.

use super::bash_runtime::{
    add_live_long_keyword, kill_by_id, live_long_keywords, promote_matching, set_live_long_keywords,
};
use super::{err, ok};
use async_trait::async_trait;
use atomcode_kernel::tool::{RiskLevel, Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;

#[derive(Default)]
pub struct LongBashKeywordAddTool;

#[derive(Deserialize)]
struct AddArgs {
    bashkeyword: String,
}

#[async_trait]
impl Tool for LongBashKeywordAddTool {
    fn name(&self) -> &str {
        "long_bash_keyword_add"
    }
    fn description(&self) -> &str {
        "Add one bash command keyword to `[tools.bash] long_bash_command_keyword` \
         (deduped, case-insensitive) and hot-reload it so matching live bash tasks \
         are promoted to long jobs immediately — even if they are already running. \
         Use this when a short bash printed output then went silent and you received \
         `[bash-await-decision]` with a bashid. Pass the unknown long-job token \
         (compiler, bundler, script name), not the bashid. Output stays on the \
         original bash pane."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "bashkeyword": {
                    "type": "string",
                    "description": "One command token to treat as a long job (e.g. ninja, webpack, mvn). Whole-word match; overrides a built-in short classification."
                }
            },
            "required": ["bashkeyword"]
        })
    }
    fn risk(&self, _args: &str) -> RiskLevel {
        RiskLevel::Safe
    }
    fn parallel_safe(&self, _args: &str) -> bool {
        true
    }
    async fn execute(&self, args: &str, _ctx: &ToolContext) -> ToolResult {
        let a: AddArgs = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => {
                return err(format!(
                    "long_bash_keyword_add: invalid arguments: {e}. Expected {{\"bashkeyword\":\"<token>\"}}."
                ))
            }
        };
        let keyword = a.bashkeyword.trim();
        if keyword.is_empty() {
            return err("long_bash_keyword_add: bashkeyword must not be empty");
        }
        match atomcode_config::config::append_long_bash_command_keyword(keyword) {
            Ok(inserted) => {
                add_live_long_keyword(keyword);
                let mut merged = live_long_keywords();
                if !merged.iter().any(|k| k.eq_ignore_ascii_case(keyword)) {
                    merged.push(keyword.to_string());
                }
                set_live_long_keywords(merged);
                let promoted = promote_matching(keyword);
                if inserted {
                    ok(format!(
                        "added keyword `{keyword}` to long_bash_command_keyword; \
                         promoted {promoted} live bash task(s). Original pane keeps streaming."
                    ))
                } else {
                    ok(format!(
                        "keyword `{keyword}` already in long_bash_command_keyword; \
                         promoted {promoted} live bash task(s)."
                    ))
                }
            }
            Err(e) => err(format!("long_bash_keyword_add: failed to write config: {e}")),
        }
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
        "Stop a still-running bash task by its bashid (from `[bash-await-decision]`). \
         The original bash pane prints `[task was canceled by bash kill tool]`. \
         Do not start a new bash for the same command; this is the cancel path \
         after a short command went silent with output."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "bashid": {
                    "type": "string",
                    "description": "The bashid from the await-decision prompt (e.g. b-00000001)."
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
