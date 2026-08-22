//! `use_skill` (invoke a named skill, returns its expanded content) + `list_skills`.
//! Both `Safe` — skills are trusted, user-authored content.

use super::registry::SkillRegistry;
use super::{err, ok};
use async_trait::async_trait;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub struct UseSkillTool {
    registry: Arc<SkillRegistry>,
}

impl UseSkillTool {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        Self { registry }
    }
}

#[derive(Deserialize)]
struct Args {
    name: String,
    #[serde(default)]
    arguments: Option<String>,
}

#[async_trait]
impl Tool for UseSkillTool {
    fn name(&self) -> &str {
        "use_skill"
    }
    fn description(&self) -> &str {
        "Invoke a named skill (a reusable prompt/workflow template) and return its content \
         with your arguments substituted, plus the skill's install path and bundled file list. \
         The name must exactly match a skill listed under '=== AVAILABLE SKILLS ===' in the \
         user-prefix catalog or returned by list_skills. Never invent or guess a skill name. Trigger \
         a skill when the task matches its listed description — not only when the user names it. \
         list_skills shows any lower-priority skills omitted from the prompt catalog. For \
         instruction-only skills, omit `arguments` or pass an empty string."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Exact skill name from AVAILABLE SKILLS or list_skills; never invent a name" },
                "arguments": {
                    "type": "string",
                    "description": "Optional parameters only when the skill template uses $ARGUMENTS / $0. Leave empty for instruction-only skills — do not paste the whole user task here."
                }
            },
            "required": ["name"]
        })
    }
    // skills are trusted user content → risk() defaults to Safe.
    async fn execute(&self, args: &str, _ctx: &ToolContext) -> ToolResult {
        let a: Args = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => {
                return err(format!(
                    "use_skill: invalid arguments: {e}. Expected {{\"name\":\"<skill>\"}}."
                ))
            }
        };
        let skill = match self.registry.get(&a.name) {
            Some(s) => s,
            None => {
                let names: Vec<String> = self.registry.list().into_iter().map(|(n, _)| n).collect();
                return err(format!(
                    "use_skill: skill '{}' not found. Available: {}. Do not guess another skill \
                     name; use an exact available name or continue without a skill",
                    a.name,
                    if names.is_empty() {
                        "(none)".to_string()
                    } else {
                        names.join(", ")
                    }
                ));
            }
        };
        let arguments = a.arguments.unwrap_or_default();
        // expand_for_injection may run `!`cmd`` shell blocks → keep off the async runtime.
        // Must use expand_for_injection (not bare expand) so the model always receives the
        // skill base directory + bundled file list — matching Grok/OpenCode and the TUI
        // slash-command path (expand_for_injection).
        match tokio::task::spawn_blocking(move || skill.expand_for_injection(&arguments, "")).await
        {
            Ok(content) => ok(content),
            Err(_) => err("use_skill: expansion task failed"),
        }
    }
}

pub struct ListSkillsTool {
    registry: Arc<SkillRegistry>,
}

impl ListSkillsTool {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for ListSkillsTool {
    fn name(&self) -> &str {
        "list_skills"
    }
    fn description(&self) -> &str {
        "List the available skills (name + description). Invoke one with use_skill."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: &str, _ctx: &ToolContext) -> ToolResult {
        let skills: Vec<_> = self.registry.all().collect();
        if skills.is_empty() {
            return ok("No skills are loaded.".to_string());
        }
        let mut out = format!("Available skills ({}):\n", skills.len());
        for s in skills {
            let loc = if s.is_directory_skill() {
                format!(" @ {}", s.display_location())
            } else {
                String::new()
            };
            if s.description.is_empty() {
                out.push_str(&format!("- {}{loc}\n", s.name));
            } else {
                out.push_str(&format!("- {}{loc}: {}\n", s.name, s.description));
            }
        }
        ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::tool::ToolContext;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> ToolContext {
        ToolContext {
            working_dir: std::path::PathBuf::from("."),
            cancel: CancellationToken::new(),
            progress: atomcode_kernel::tool::ProgressSink::noop(),
            requester: None,
        }
    }
    fn registry_with(skills: &[(&str, &str)]) -> Arc<SkillRegistry> {
        let d = Box::leak(Box::new(tempfile::tempdir().unwrap())); // keep alive for the test
        for (name, body) in skills {
            std::fs::write(d.path().join(format!("{name}.md")), body).unwrap();
        }
        Arc::new(SkillRegistry::load(&[d.path().to_path_buf()]))
    }

    #[tokio::test]
    async fn use_skill_expands() {
        let tool = UseSkillTool::new(registry_with(&[("greet", "Hello $ARGUMENTS!")]));
        let r = tool
            .execute(r#"{"name":"greet","arguments":"world"}"#, &ctx())
            .await;
        assert!(!r.is_error, "{}", r.content);
        // Envelope wraps the expanded body (path-bearing injection).
        assert!(r.content.contains("Hello world!"), "{}", r.content);
        assert!(r.content.contains("<skill name="), "{}", r.content);
    }

    #[tokio::test]
    async fn use_skill_directory_skill_returns_base_dir_and_files() {
        let base = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let skill_dir = base.path().join("multi-db");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: >\n  连接公司内部数据库\n---\nRun ${SKILL_DIR}/scripts/db_executor.py\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("scripts/db_executor.py"), "print(1)\n").unwrap();
        let reg = Arc::new(SkillRegistry::load(&[base.path().to_path_buf()]));
        let tool = UseSkillTool::new(reg);
        let r = tool.execute(r#"{"name":"multi-db"}"#, &ctx()).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("Base directory for this skill:"), "{}", r.content);
        assert!(r.content.contains("db_executor.py"), "{}", r.content);
        assert!(
            r.content.contains("连接公司内部数据库")
                || r.content.contains("description=\"连接"),
            "description must not be bare '>': {}",
            r.content
        );
        // ${SKILL_DIR} expanded inside body (absolute path + scripts/…).
        assert!(
            r.content.contains("scripts/db_executor.py")
                || r.content.contains("scripts\\db_executor.py"),
            "{}",
            r.content
        );
        // System-reminder documents the tokens with backticks; only the body must expand.
        let body_start = r.content.find("Run ").unwrap_or(0);
        let body = &r.content[body_start..];
        assert!(
            !body.contains("${SKILL_DIR}"),
            "body must expand SKILL_DIR: {}",
            body
        );
    }

    #[tokio::test]
    async fn use_skill_not_found_lists_available() {
        let tool = UseSkillTool::new(registry_with(&[("a", "x"), ("b", "y")]));
        let r = tool.execute(r#"{"name":"nope"}"#, &ctx()).await;
        assert!(r.is_error);
        assert!(r.content.contains("not found"), "{}", r.content);
        assert!(
            r.content.contains("a") && r.content.contains("b"),
            "{}",
            r.content
        );
        assert!(
            r.content.contains("Do not guess another skill name"),
            "{}",
            r.content
        );
    }

    #[test]
    fn use_skill_schema_requires_an_exact_available_name() {
        let tool = UseSkillTool::new(Arc::new(SkillRegistry::new()));
        let schema = tool.parameters_schema().to_string();
        assert!(schema.contains("Exact skill name"), "{schema}");
        assert!(schema.contains("never invent"), "{schema}");
        assert!(
            tool.description().contains("must exactly match"),
            "{}",
            tool.description()
        );
    }

    #[tokio::test]
    async fn list_skills_formats() {
        let tool = ListSkillsTool::new(registry_with(&[(
            "greet",
            "---\ndescription: say hi\n---\nHello",
        )]));
        let r = tool.execute("{}", &ctx()).await;
        assert!(r.content.contains("Available skills (1)"), "{}", r.content);
        assert!(r.content.contains("- greet: say hi"), "{}", r.content);
    }

    #[tokio::test]
    async fn list_skills_includes_location_for_directory_skills() {
        let base = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let skill_dir = base.path().join("review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: review code\n---\nbody\n",
        )
        .unwrap();
        let tool = ListSkillsTool::new(Arc::new(SkillRegistry::load(&[base
            .path()
            .to_path_buf()])));
        let r = tool.execute("{}", &ctx()).await;
        assert!(r.content.contains("review"), "{}", r.content);
        assert!(
            r.content.contains("@ ") || r.content.contains("path"),
            "directory skills should surface location: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn list_skills_empty() {
        let tool = ListSkillsTool::new(Arc::new(SkillRegistry::new()));
        let r = tool.execute("{}", &ctx()).await;
        assert!(r.content.contains("No skills"), "{}", r.content);
    }

    // Regression for issue-use-skill-plugin-not-loaded: plugin skills MUST be reachable
    // when the driver feeds them into the registry with a namespace (the capabilities crate
    // cannot reach the core plugin loader by design — the bridge/driver feeds plugin dirs).
    // This is the L1 contract `atomcode-coding::parts` relies on via `load_dir(dir, Some(ns))`.
    #[tokio::test]
    async fn use_skill_finds_plugin_namespaced_skill() {
        let base = Box::leak(Box::new(tempfile::tempdir().unwrap())); // loose user skill
        std::fs::write(base.path().join("setup.md"), "built-in setup body\n").unwrap();

        let plugin_ns = "plugin-total-design";
        let plugin = Box::leak(Box::new(tempfile::tempdir().unwrap())); // plugin install dir
        let skills_dir = plugin.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("td-explore")).unwrap();
        std::fs::write(
            skills_dir.join("td-explore").join("SKILL.md"),
            "---\ndescription: explore a subsystem\n---\nExplore body $ARGUMENTS\n",
        )
        .unwrap();

        let mut reg = SkillRegistry::load(&[base.path().to_path_buf()]);
        reg.load_dir(&skills_dir, Some(plugin_ns));

        let tool = UseSkillTool::new(Arc::new(reg));
        // qualified name `<plugin>:<skill-name>` resolves
        let r = tool
            .execute(&format!(r#"{{"name":"{plugin_ns}:td-explore"}}"#), &ctx())
            .await;
        assert!(!r.is_error, "qualified lookup failed: {}", r.content);
        assert!(
            r.content.contains("Explore body"),
            "expanded body missing: {}",
            r.content
        );
        assert!(
            r.content.contains("<skill name=") && r.content.contains("path=\""),
            "injection must include path-bearing envelope: {}",
            r.content
        );

        // the loose user skill is still separately reachable (no namespace collision)
        let r2 = tool.execute(r#"{"name":"setup"}"#, &ctx()).await;
        assert!(!r2.is_error, "loose skill missing: {}", r2.content);
    }

    #[tokio::test]
    async fn use_skill_plugin_namespace_shows_in_available_list() {
        let plugin_ns = "my-plugin";
        let plugin = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let skills_dir = plugin.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("alpha")).unwrap();
        std::fs::write(
            skills_dir.join("alpha").join("SKILL.md"),
            "---\ndescription: alpha plugin skill\n---\nalpha body\n",
        )
        .unwrap();

        let mut reg = SkillRegistry::new();
        reg.load_dir(&skills_dir, Some(plugin_ns));

        let tool = UseSkillTool::new(Arc::new(reg));
        // asking for a non-existent skill must list `my-plugin:alpha` among available —
        // the bug from issue was that available NEVER showed any `<plugin>:<skill>` entry.
        let r = tool.execute(r#"{"name":"nope"}"#, &ctx()).await;
        assert!(r.is_error, "{}", r.content);
        assert!(
            r.content.contains("my-plugin:alpha"),
            "available list missing plugin entry: {}",
            r.content
        );
    }
}
