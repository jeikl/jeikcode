//! Custom prompt loader and in-memory cache with hot-reloading for AtomCode.
//!
//! Loads configuration from `$ATOMCODE_HOME/prompts/` (or `~/.atomcode/prompts/`):
//! - `init.yaml`: Identity, Precedence, Platform, Environment.
//! - `rules.yaml`: Workflow, Tools Discipline, Doing Tasks, Task Tracking, Delegation, etc.
//! - `内置工具.yaml`: Tool parameter definitions and guidance.
//! - `内置技能.yaml`: Skills catalog and trigger rules.
//!
//! Utilizes an in-memory cache with modification timestamp (`mtime`) validation:
//! - If the files have not been modified: returns cached in-memory structure with 0 parsing cost.
//! - If the files have been edited by the user: automatically hot-reloads the changes into memory.
//! - If the files do not exist: falls back seamlessly to the built-in default templates.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CustomPromptConfig {
    pub version: Option<String>,
    pub identity: Option<IdentityConfig>,
    pub precedence: Option<PrecedenceConfig>,
    pub platform: Option<PlatformConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct IdentityConfig {
    pub agent_name: Option<String>,
    pub provider: Option<String>,
    pub description: Option<String>,
    pub role_summary: Option<String>,
    pub template: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PrecedenceConfig {
    pub rule: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PlatformConfig {
    pub windows: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CustomRulesConfig {
    pub version: Option<String>,
    pub workflow: Option<WorkflowConfig>,
    pub tools_discipline: Option<ToolsDisciplineConfig>,
    pub locating_code: Option<LocatingCodeConfig>,
    pub doing_tasks: Option<Vec<String>>,
    pub when_commands_fail: Option<String>,
    pub risky_actions: Option<String>,
    pub scope: Option<String>,
    pub output: Option<OutputConfig>,
    pub chinese_support: Option<String>,
    pub task_tracking: Option<String>,
    pub user_communication: Option<String>,
    pub asking_the_user: Option<String>,
    pub delegation: Option<String>,
    pub code_review: Option<String>,
    pub skills: Option<String>,
    pub firm_execution_discipline: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct WorkflowConfig {
    pub first_round_reflex: Option<String>,
    pub surgical_context: Option<String>,
    pub never_negative_conclusion: Option<String>,
    pub batched_parallel_exploration: Option<String>,
    pub general_phases: Option<HashMap<String, String>>,
    pub guidelines: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ToolsDisciplineConfig {
    pub concurrency_principle: Option<String>,
    pub mandatory_parallel: Option<Vec<String>>,
    pub tool_preferences: Option<HashMap<String, String>>,
    pub firm_tool_discipline: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct LocatingCodeConfig {
    pub repo_map_rule: Option<String>,
    pub business_concepts: Option<String>,
    pub upgrade_rule: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct OutputConfig {
    pub signposts: Option<String>,
    pub conciseness: Option<String>,
    pub language_match: Option<String>,
    pub structured_data: Option<String>,
    pub content_transformation: Option<String>,
}

#[derive(Default)]
struct PromptCacheState {
    last_init_mtime: Option<SystemTime>,
    init_config: Option<CustomPromptConfig>,
    last_rules_mtime: Option<SystemTime>,
    rules_config: Option<CustomRulesConfig>,
}

static PROMPT_CACHE: OnceLock<RwLock<PromptCacheState>> = OnceLock::new();

fn cache() -> &'static RwLock<PromptCacheState> {
    PROMPT_CACHE.get_or_init(|| RwLock::new(PromptCacheState::default()))
}

fn resolve_prompts_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ATOMCODE_HOME") {
        if !p.is_empty() {
            return Some(PathBuf::from(p).join("prompts"));
        }
    }
    Some(dirs::home_dir()?.join(".atomcode").join("prompts"))
}

/// Returns the active `init.yaml` custom prompt configuration.
pub fn get_custom_prompt_config() -> Option<CustomPromptConfig> {
    let init_path = resolve_prompts_dir()?.join("init.yaml");
    let current_mtime = std::fs::metadata(&init_path)
        .ok()
        .and_then(|m| m.modified().ok());

    let c = cache();
    {
        if let Ok(r) = c.read() {
            if r.last_init_mtime == current_mtime && r.last_init_mtime.is_some() {
                return r.init_config.clone();
            }
            if current_mtime.is_none() && r.last_init_mtime.is_none() {
                return None;
            }
        }
    }

    let mut w = c.write().ok()?;
    let config = if init_path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&init_path) {
            serde_yaml::from_str::<CustomPromptConfig>(&content).ok()
        } else {
            None
        }
    } else {
        None
    };

    w.last_init_mtime = current_mtime;
    w.init_config = config.clone();
    config
}

/// Returns the active `rules.yaml` custom rules configuration.
pub fn get_custom_rules_config() -> Option<CustomRulesConfig> {
    let rules_path = resolve_prompts_dir()?.join("rules.yaml");
    let current_mtime = std::fs::metadata(&rules_path)
        .ok()
        .and_then(|m| m.modified().ok());

    let c = cache();
    {
        if let Ok(r) = c.read() {
            if r.last_rules_mtime == current_mtime && r.last_rules_mtime.is_some() {
                return r.rules_config.clone();
            }
            if current_mtime.is_none() && r.last_rules_mtime.is_none() {
                return None;
            }
        }
    }

    let mut w = c.write().ok()?;
    let config = if rules_path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&rules_path) {
            serde_yaml::from_str::<CustomRulesConfig>(&content).ok()
        } else {
            None
        }
    } else {
        None
    };

    w.last_rules_mtime = current_mtime;
    w.rules_config = config.clone();
    config
}

/// Render the identity and precedence section based on custom config, or default if absent.
pub fn render_identity_and_precedence(model: &str) -> (String, Option<String>) {
    if let Some(cfg) = get_custom_prompt_config() {
        let identity = if let Some(id) = &cfg.identity {
            if let Some(template) = &id.template {
                let agent_name = id.agent_name.as_deref().unwrap_or("AtomCode");
                let provider = id.provider.as_deref().unwrap_or("AtomGit");
                let desc = id.description.as_deref().unwrap_or("an AI coding agent by AtomGit running the {model} model");
                let role = id.role_summary.as_deref().unwrap_or("You help users with software engineering tasks within the current project.");
                
                template
                    .replace("{agent_name}", agent_name)
                    .replace("{provider}", provider)
                    .replace("{description}", &desc.replace("{model}", model))
                    .replace("{role_summary}", role)
                    .replace("{model}", model)
            } else {
                format!("You are AtomCode, an AI coding agent by AtomGit running the {model} model. You help users with software engineering tasks within the current project.")
            }
        } else {
            format!("You are AtomCode, an AI coding agent by AtomGit running the {model} model. You help users with software engineering tasks within the current project.")
        };

        let precedence = cfg.precedence.as_ref().and_then(|p| p.rule.clone());
        (identity, precedence)
    } else {
        (
            format!("You are AtomCode, an AI coding agent by AtomGit running the {model} model. You help users with software engineering tasks within the current project."),
            None,
        )
    }
}

/// Render rules from `rules.yaml` if available, formatted as markdown sections.
pub fn render_custom_rules() -> Option<String> {
    let cfg = get_custom_rules_config()?;
    let mut out = String::new();

    // Context management & system reminders prefix
    out.push_str("## CONTEXT MANAGEMENT:\nThe context window is managed for you: as it fills, older turns are automatically compacted (tool results are stubbed, then summarized). Do NOT tell the user to start a new conversation, clear the history, or that you are \"running low on context\" in order to manage it — that is handled automatically. Keep working; if some earlier detail was condensed and you need it, re-read the source.\n\n");

    // Workflow
    if let Some(wf) = &cfg.workflow {
        out.push_str("## WORKFLOW:\n");
        if let Some(r) = &wf.first_round_reflex {
            out.push_str(&format!("- {r}\n"));
        }
        if let Some(s) = &wf.surgical_context {
            out.push_str(&format!("- {s}\n"));
        }
        if let Some(n) = &wf.never_negative_conclusion {
            out.push_str(&format!("- {n}\n"));
        }
        if let Some(b) = &wf.batched_parallel_exploration {
            out.push_str(&format!("- {b}\n"));
        }
        if let Some(phases) = &wf.general_phases {
            for (k, v) in phases {
                out.push_str(&format!("- Phase ({k}): {v}\n"));
            }
        }
        if let Some(guide) = &wf.guidelines {
            out.push_str("\nGuidelines:\n");
            for (_, v) in guide {
                out.push_str(&format!("- {v}\n"));
            }
        }
        out.push('\n');
    }

    // Tools & parallel execution
    if let Some(td) = &cfg.tools_discipline {
        out.push_str("## TOOLS & PARALLEL EXECUTION (CRITICAL EFFICIENCY):\n");
        if let Some(p) = &td.concurrency_principle {
            out.push_str(&format!("{p}\n\n"));
        }
        if let Some(mand) = &td.mandatory_parallel {
            out.push_str("MANDATORY parallel scenarios (MUST emit all in ONE response):\n");
            for item in mand {
                out.push_str(&format!("- {item}\n"));
            }
            out.push('\n');
        }
        if let Some(prefs) = &td.tool_preferences {
            for (k, v) in prefs {
                out.push_str(&format!("- {k}: {v}\n"));
            }
            out.push('\n');
        }
    }

    // Locating code
    if let Some(lc) = &cfg.locating_code {
        out.push_str("## LOCATING CODE:\n");
        if let Some(r) = &lc.repo_map_rule {
            out.push_str(&format!("1. {r}\n"));
        }
        if let Some(b) = &lc.business_concepts {
            out.push_str(&format!("2. {b}\n"));
        }
        if let Some(u) = &lc.upgrade_rule {
            out.push_str(&format!("3. UPGRADE: {u}\n"));
        }
        out.push('\n');
    }

    // Doing tasks
    if let Some(dt) = &cfg.doing_tasks {
        out.push_str("## DOING TASKS:\n");
        for rule in dt {
            out.push_str(&format!("- {rule}\n"));
        }
        out.push('\n');
    }

    // When commands fail
    if let Some(wcf) = &cfg.when_commands_fail {
        out.push_str(&format!("## WHEN COMMANDS FAIL:\n{wcf}\n\n"));
    }

    // Risky actions
    if let Some(ra) = &cfg.risky_actions {
        out.push_str(&format!("## RISKY ACTIONS:\n{ra}\n\n"));
    }

    // Scope
    if let Some(sc) = &cfg.scope {
        out.push_str(&format!("## SCOPE:\n{sc}\n\n"));
    }

    // Output
    if let Some(out_cfg) = &cfg.output {
        out.push_str("## OUTPUT:\n");
        if let Some(s) = &out_cfg.signposts {
            out.push_str(&format!("- Progress Signposts: {s}\n"));
        }
        if let Some(c) = &out_cfg.conciseness {
            out.push_str(&format!("- Conciseness: {c}\n"));
        }
        if let Some(l) = &out_cfg.language_match {
            out.push_str(&format!("- Language: {l}\n"));
        }
        if let Some(t) = &out_cfg.structured_data {
            out.push_str(&format!("- Tables: {t}\n"));
        }
        if let Some(ct) = &out_cfg.content_transformation {
            out.push_str(&format!("- Transformation: {ct}\n"));
        }
        out.push('\n');
    }

    // Chinese code support
    if let Some(cs) = &cfg.chinese_support {
        out.push_str(&format!("## CHINESE CODE SUPPORT:\n{cs}\n\n"));
    }

    Some(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_prompts_yaml_deserialize_matches_schema() {
        let yaml = r#"
version: "2.0.0"
identity:
  agent_name: "TestAgent"
  provider: "TestProvider"
  description: "a custom agent running {model}"
  role_summary: "Custom role."
  template: "You are {agent_name} by {provider} on {model}. {role_summary}"
precedence:
  rule: "Custom precedence."
"#;
        let cfg: CustomPromptConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.identity.as_ref().unwrap().agent_name.as_deref(), Some("TestAgent"));
        assert_eq!(cfg.precedence.as_ref().unwrap().rule.as_deref(), Some("Custom precedence."));
    }

    #[test]
    fn custom_rules_yaml_deserialize_matches_schema() {
        let yaml = r#"
version: "2.0.0"
workflow:
  first_round_reflex: "Structure then dive."
doing_tasks:
  - "Read before modify."
"#;
        let cfg: CustomRulesConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.workflow.as_ref().unwrap().first_round_reflex.as_deref(), Some("Structure then dive."));
        assert_eq!(cfg.doing_tasks.as_ref().unwrap().len(), 1);
    }
}
