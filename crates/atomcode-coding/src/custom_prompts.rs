//! Custom prompt loader and in-memory cache with hot-reloading for AtomCode.
//!
//! Loads configuration from `$ATOMCODE_HOME/prompts/` (or `~/.atomcode/prompts/`):
//! - `init.yaml` (**live**): identity, precedence, security, environment. Every key in
//!   the seed is deserialized and rendered into the System persona.
//! - `rules.yaml` (**live**): workflow, tools discipline, locating code, doing tasks,
//!   etc. When present, **replaces** the compiled-in `RULES` block (not merged).
//!   Every key in the seed is rendered; unknown keys are ignored.
//! - `root_docs_prompts.md` / `root_docs_内置工具.yaml` / `root_docs_内置技能.yaml`:
//!   human-facing seed documents. **Not loaded** into the model. Live tool schema
//!   comes from `Tool::description()` / `parameters_schema()`; live skills come
//!   from installed `SKILL.md` dirs.
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
    pub security: Option<SecurityConfig>,
    pub environment: Option<EnvironmentConfig>,
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
pub struct SecurityConfig {
    pub system_reminders: Option<String>,
    pub mcp_instructions: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct EnvironmentConfig {
    pub context_management: Option<String>,
    pub windows_platform: Option<String>,
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
    pub explore_first: Option<String>,
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
    // Unit tests must not leak the developer's `~/.atomcode/prompts` into persona
    // snapshots. Production still loads the user home; tests opt in via ATOMCODE_HOME.
    #[cfg(test)]
    {
        return None;
    }
    #[cfg(not(test))]
    {
        Some(dirs::home_dir()?.join(".atomcode").join("prompts"))
    }
}

fn strip_utf8_bom(content: &str) -> &str {
    content.strip_prefix('\u{feff}').unwrap_or(content)
}

/// Bundled first-run templates for `~/.atomcode/prompts/`. Existing files are never
/// overwritten — the user owns their copies after the first write.
const PROMPT_SEEDS: &[(&str, &str)] = &[
    ("init.yaml", include_str!("../assets/prompts/init.yaml")),
    ("rules.yaml", include_str!("../assets/prompts/rules.yaml")),
    (
        "root_docs_prompts.md",
        include_str!("../assets/prompts/root_docs_prompts.md"),
    ),
    (
        "root_docs_内置工具.yaml",
        include_str!("../assets/prompts/root_docs_内置工具.yaml"),
    ),
    (
        "root_docs_内置技能.yaml",
        include_str!("../assets/prompts/root_docs_内置技能.yaml"),
    ),
];

/// Idempotent first-run seed of `~/.atomcode/prompts/` (or `$ATOMCODE_HOME/prompts/`).
///
/// Called from persona assembly so a fresh install gets editable templates without
/// requiring a separate `atomcode setup`. Failures are silent (read-only home must
/// not break startup). Tests skip seeding so isolated `ATOMCODE_HOME` does not
/// leak bundled YAML into persona snapshots.
pub fn seed_default_prompts() {
    #[cfg(test)]
    {
        // Isolated ATOMCODE_HOME must not receive bundled YAML (persona snapshots).
        let _ = resolve_prompts_dir;
        return;
    }
    #[cfg(not(test))]
    {
        let Some(dir) = resolve_prompts_dir() else {
            return;
        };
        seed_prompts_into(&dir);
    }
}

fn seed_prompts_into(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    for (name, content) in PROMPT_SEEDS {
        let dest = dir.join(name);
        if dest.exists() {
            continue;
        }
        let _ = std::fs::write(&dest, content);
    }
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
            serde_yaml::from_str::<CustomPromptConfig>(strip_utf8_bom(&content)).ok()
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
            serde_yaml::from_str::<CustomRulesConfig>(strip_utf8_bom(&content)).ok()
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
                let agent_name = id.agent_name.as_deref().unwrap_or("JeikCode");
                let provider = id.provider.as_deref().unwrap_or("Jeik");
                let desc = id
                    .description
                    .as_deref()
                    .unwrap_or("an AI coding agent by JeikCode running the {model} model");
                let role = id.role_summary.as_deref().unwrap_or(
                    "You help users with software engineering tasks within the current project.",
                );

                template
                    .replace("{agent_name}", agent_name)
                    .replace("{provider}", provider)
                    .replace("{description}", &desc.replace("{model}", model))
                    .replace("{role_summary}", role)
                    .replace("{model}", model)
            } else {
                format!("You are JeikCode, an AI coding agent by JeikCode running the {model} model. You help users with software engineering tasks within the current project.")
            }
        } else {
            format!("You are JeikCode, an AI coding agent by JeikCode running the {model} model. You help users with software engineering tasks within the current project.")
        };

        let precedence = cfg.precedence.as_ref().and_then(|p| p.rule.clone());
        (identity, precedence)
    } else {
        (
            format!("You are JeikCode, an AI coding agent by JeikCode running the {model} model. You help users with software engineering tasks within the current project."),
            None,
        )
    }
}

/// Live `init.yaml` sections that sit after identity/precedence: system reminders,
/// MCP isolation, context management. `None` when no init file is loaded.
pub fn render_init_live_prefix() -> Option<String> {
    let cfg = get_custom_prompt_config()?;
    render_init_live_prefix_from(&cfg)
}

pub(crate) fn render_init_live_prefix_from(cfg: &CustomPromptConfig) -> Option<String> {
    let mut out = String::new();
    if let Some(sec) = &cfg.security {
        if let Some(s) = sec
            .system_reminders
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push_str("## SYSTEM REMINDERS:\n");
            out.push_str(s);
            out.push_str("\n\n");
        }
        if let Some(s) = sec
            .mcp_instructions
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push_str("## MCP SERVER INSTRUCTIONS:\n");
            out.push_str(s);
            out.push_str("\n\n");
        }
    }
    if let Some(env) = &cfg.environment {
        if let Some(s) = env
            .context_management
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push_str("## CONTEXT MANAGEMENT:\n");
            out.push_str(s);
            out.push_str("\n\n");
        }
    }
    let trimmed = out.trim_end().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Windows platform line from live `init.yaml`. `None` when unset or not loaded.
pub fn render_init_windows_platform() -> Option<String> {
    let cfg = get_custom_prompt_config()?;
    cfg.environment
        .as_ref()
        .and_then(|e| e.windows_platform.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Render rules from `rules.yaml` if available, formatted as markdown sections.
pub fn render_custom_rules() -> Option<String> {
    let cfg = get_custom_rules_config()?;
    let out = render_custom_rules_from(&cfg);
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub(crate) fn render_custom_rules_from(cfg: &CustomRulesConfig) -> String {
    let mut out = String::new();

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
        if let Some(firm) = td
            .firm_tool_discipline
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push_str("## TOOL DISCIPLINE (MANDATORY):\n");
            out.push_str(firm);
            out.push_str("\n\n");
        }
    }

    // Locating code
    if let Some(lc) = &cfg.locating_code {
        out.push_str("## LOCATING CODE:\n");
        let mut n = 1u32;
        if let Some(r) = &lc.repo_map_rule {
            out.push_str(&format!("{n}. {r}\n"));
            n += 1;
        }
        if let Some(e) = &lc.explore_first {
            out.push_str(&format!("{n}. {e}\n"));
            n += 1;
        }
        if let Some(b) = &lc.business_concepts {
            out.push_str(&format!("{n}. {b}\n"));
            n += 1;
        }
        if let Some(u) = &lc.upgrade_rule {
            out.push_str(&format!("{n}. UPGRADE: {u}\n"));
        }
        let _ = n;
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

    // Task tracking — custom rules replace the built-in RULES block AND skip
    // TODO_USAGE, so this section must be rendered here or todowrite guidance
    // never reaches the model.
    if let Some(tt) = &cfg.task_tracking {
        out.push_str(&format!("## TASK TRACKING:\n{tt}\n\n"));
    }

    if let Some(ask) = &cfg.asking_the_user {
        out.push_str(&format!("## ASKING THE USER:\n{ask}\n\n"));
    }

    if let Some(del) = &cfg.delegation {
        out.push_str(&format!("## DELEGATING WITH `task`:\n{del}\n\n"));
    }

    if let Some(rev) = &cfg.code_review {
        out.push_str(&format!("## CODE REVIEW:\n{rev}\n\n"));
    }

    if let Some(sk) = &cfg.skills {
        out.push_str(&format!("## SKILLS:\n{sk}\n\n"));
    }

    if let Some(firm) = &cfg.firm_execution_discipline {
        if !firm.is_empty() {
            out.push_str("## EXECUTION DISCIPLINE (MANDATORY):\n");
            for item in firm {
                out.push_str(&format!("- {item}\n"));
            }
            out.push('\n');
        }
    }

    out.trim_end().to_string()
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
        assert_eq!(
            cfg.identity.as_ref().unwrap().agent_name.as_deref(),
            Some("TestAgent")
        );
        assert_eq!(
            cfg.precedence.as_ref().unwrap().rule.as_deref(),
            Some("Custom precedence.")
        );
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
        assert_eq!(
            cfg.workflow.as_ref().unwrap().first_round_reflex.as_deref(),
            Some("Structure then dive.")
        );
        assert_eq!(cfg.doing_tasks.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn strip_utf8_bom_lets_yaml_parse() {
        let yaml = "\u{feff}version: \"2.0.0\"\nprecedence:\n  rule: bom-ok\n";
        assert!(
            serde_yaml::from_str::<CustomPromptConfig>(yaml).is_err(),
            "serde_yaml rejects a leading BOM — the loader must strip it first"
        );
        let cfg: CustomPromptConfig = serde_yaml::from_str(strip_utf8_bom(yaml)).unwrap();
        assert_eq!(
            cfg.precedence.as_ref().unwrap().rule.as_deref(),
            Some("bom-ok")
        );
    }

    #[test]
    fn bundled_prompt_seeds_parse() {
        for (name, body) in PROMPT_SEEDS {
            let stripped = strip_utf8_bom(body);
            assert!(!stripped.trim().is_empty(), "{name} seed is empty");
            if name.ends_with(".yaml") {
                let parsed: Result<serde_yaml::Value, _> = serde_yaml::from_str(stripped);
                assert!(parsed.is_ok(), "{name} must be valid YAML: {parsed:?}");
            }
        }
        let init: CustomPromptConfig =
            serde_yaml::from_str(strip_utf8_bom(include_str!("../assets/prompts/init.yaml")))
                .expect("init.yaml");
        assert_eq!(
            init.identity.as_ref().unwrap().agent_name.as_deref(),
            Some("JeikCode")
        );
        let rules: CustomRulesConfig =
            serde_yaml::from_str(strip_utf8_bom(include_str!("../assets/prompts/rules.yaml")))
                .expect("rules.yaml");
        let surgical = rules
            .workflow
            .as_ref()
            .unwrap()
            .surgical_context
            .as_deref()
            .unwrap();
        assert!(surgical.contains("NEVER pass"));
        assert!(
            surgical.contains("crates/atomcode-coding") && surgical.contains("src/auth.rs"),
            "surgical_context must show directory GOOD / file BAD: {surgical}"
        );
        let first_round = rules
            .workflow
            .as_ref()
            .unwrap()
            .first_round_reflex
            .as_deref()
            .unwrap();
        assert!(
            first_round.contains("CONDITIONAL CONTEXT ROUTING")
                && first_round.contains("do NOT call `repo_map` first")
                && !first_round.contains("repo_map` ONLY"),
            "concrete targets must not pay an obligatory repo_map round: {first_round}"
        );
        let batched = rules
            .workflow
            .as_ref()
            .unwrap()
            .batched_parallel_exploration
            .as_deref()
            .unwrap();
        assert!(
            batched.contains("2–6 independent")
                && batched.contains("complete functions")
                && !batched.contains("hot spans"),
            "batched exploration must favor broad useful context over tiny slices: {batched}"
        );
        assert!(
            rules
                .locating_code
                .as_ref()
                .unwrap()
                .explore_first
                .as_deref()
                .unwrap()
                .contains("code_explore"),
            "explore_first is a live rules.yaml field"
        );
    }

    #[test]
    fn seed_init_yaml_live_fields_all_render() {
        let init: CustomPromptConfig =
            serde_yaml::from_str(strip_utf8_bom(include_str!("../assets/prompts/init.yaml")))
                .expect("init.yaml");
        let prefix = render_init_live_prefix_from(&init).expect("init live prefix");
        assert!(prefix.contains("## SYSTEM REMINDERS:"), "{prefix}");
        assert!(prefix.contains("## MCP SERVER INSTRUCTIONS:"), "{prefix}");
        assert!(prefix.contains("## CONTEXT MANAGEMENT:"), "{prefix}");
        assert!(
            init.environment
                .as_ref()
                .unwrap()
                .windows_platform
                .as_deref()
                .unwrap()
                .contains("where"),
            "windows_platform is live"
        );
    }

    #[test]
    fn seed_rules_yaml_live_fields_all_render() {
        let rules: CustomRulesConfig =
            serde_yaml::from_str(strip_utf8_bom(include_str!("../assets/prompts/rules.yaml")))
                .expect("rules.yaml");
        let out = render_custom_rules_from(&rules);
        for needle in [
            "## WORKFLOW:",
            "PRIMARY EXPLORATION",
            "## TOOL DISCIPLINE (MANDATORY):",
            "## LOCATING CODE:",
            "## ASKING THE USER:",
            "## DELEGATING WITH `task`:",
            "## CODE REVIEW:",
            "## SKILLS:",
            "## EXECUTION DISCIPLINE (MANDATORY):",
            "## TASK TRACKING:",
        ] {
            assert!(
                out.contains(needle),
                "missing live field {needle} in:\n{out}"
            );
        }
        assert!(
            !out.contains("## CONTEXT MANAGEMENT:"),
            "context lives in init.yaml, not rules.yaml"
        );
    }

    #[test]
    fn prompt_seeds_are_live_yaml_or_root_docs() {
        let names: Vec<&str> = PROMPT_SEEDS.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec![
                "init.yaml",
                "rules.yaml",
                "root_docs_prompts.md",
                "root_docs_内置工具.yaml",
                "root_docs_内置技能.yaml",
            ]
        );
        for (name, _) in PROMPT_SEEDS {
            if *name != "init.yaml" && *name != "rules.yaml" {
                assert!(
                    name.starts_with("root_docs_"),
                    "non-live seed must be named root_docs_*: {name}"
                );
            }
        }
    }

    #[test]
    fn seed_default_prompts_writes_missing_and_keeps_existing() {
        let home = tempfile::tempdir().unwrap();
        let prompts = home.path().join("prompts");
        seed_prompts_into(&prompts);
        for (name, _) in PROMPT_SEEDS {
            assert!(prompts.join(name).is_file(), "missing seeded {name}");
        }
        let marker = "USER-OWNED-DO-NOT-OVERWRITE";
        std::fs::write(prompts.join("init.yaml"), marker).unwrap();
        seed_prompts_into(&prompts);
        let after = std::fs::read_to_string(prompts.join("init.yaml")).unwrap();
        assert_eq!(after, marker, "seed must not overwrite user edits");
    }
}
