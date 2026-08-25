//! `UserWrapHook` — applies template wrapping to the user's latest query using `user-wrap.md`.
//!
//! # Purpose
//! Allows users or projects to define custom wrapper formatting (e.g. prompt structuring,
//! safety disclaimers, or project-specific query wrapping) around the user's raw input:
//!
//! ```markdown
//! 用户提问：【{{input}}】
//! 请你根据用户的信息，不能回答政治相关的问题。
//! ```
//!
//! # Resolution Hierarchy (Precedence)
//! 1. Project-level: `<working_dir>/.atomcode/user-wrap.md`
//! 2. Project-level: `<working_dir>/user-wrap.md`
//! 3. Global-level: `~/.atomcode/user-wrap.md` (or `$ATOMCODE_HOME/user-wrap.md`)
//!
//! If a project-level file exists, it overrides the global configuration completely.
//!
//! # Execution Guarantees
//! - **Last Real User Message Only**: Only executes when a user query is submitted (`user_prompt_submit`).
//! - **Prefix Stability**: Does not alter system messages, synthetic context, memory, skills, or tool outputs.
//! - **Hot Reload**: Re-reads the file from disk on each prompt submission; updates take effect instantly.

use async_trait::async_trait;
use atomcode_kernel::hook::LifecycleHooks;
use std::path::{Path, PathBuf};

/// Hook that wraps the user's latest prompt using `user-wrap.md`.
#[derive(Clone, Debug)]
pub struct UserWrapHook {
    working_dir: PathBuf,
}

impl UserWrapHook {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    /// Resolve the active `user-wrap.md` path according to precedence rules.
    pub fn resolve_wrap_file(&self) -> Option<PathBuf> {
        Self::resolve_wrap_file_for(&self.working_dir)
    }

    /// Resolve the active `user-wrap.md` path for a given working directory.
    pub fn resolve_wrap_file_for(working_dir: &Path) -> Option<PathBuf> {
        // 1. Project-level: <working_dir>/.atomcode/user-wrap.md
        let p1 = working_dir.join(".atomcode").join("user-wrap.md");
        if p1.is_file() {
            return Some(p1);
        }

        // 2. Project-level: <working_dir>/user-wrap.md
        let p2 = working_dir.join("user-wrap.md");
        if p2.is_file() {
            return Some(p2);
        }

        // 3. Global-level: ~/.atomcode/user-wrap.md
        let global = crate::session::config_dir().join("user-wrap.md");
        if global.is_file() {
            return Some(global);
        }

        None
    }

    /// Wrap the given user input text using the template from `user-wrap.md`.
    pub fn wrap_input(&self, input: &str) -> String {
        let Some(path) = self.resolve_wrap_file() else {
            return input.to_string();
        };

        let Ok(content) = std::fs::read_to_string(&path) else {
            return input.to_string();
        };

        let template = content.trim();
        if template.is_empty() {
            return input.to_string();
        }

        if template.contains("{{input}}") {
            template.replace("{{input}}", input)
        } else {
            format!("{template}\n\n{input}")
        }
    }
}

#[async_trait]
impl LifecycleHooks for UserWrapHook {
    async fn user_prompt_submit(&self, text: &mut String) -> Result<(), String> {
        let wrapped = self.wrap_input(text);
        *text = wrapped;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_wrap_input_no_file() {
        let temp = TempDir::new().unwrap();
        let hook = UserWrapHook::new(temp.path());
        assert_eq!(hook.wrap_input("hello world"), "hello world");
    }

    #[test]
    fn test_wrap_input_with_template() {
        let temp = TempDir::new().unwrap();
        let wrap_file = temp.path().join("user-wrap.md");
        std::fs::write(&wrap_file, "用户提问：【{{input}}】\n请严格遵守规则。").unwrap();

        let hook = UserWrapHook::new(temp.path());
        let res = hook.wrap_input("你好");
        assert_eq!(res, "用户提问：【你好】\n请严格遵守规则。");
    }

    #[test]
    fn test_wrap_input_project_atomcode_dir_precedence() {
        let temp = TempDir::new().unwrap();
        let atomcode_dir = temp.path().join(".atomcode");
        std::fs::create_dir_all(&atomcode_dir).unwrap();

        let p1 = atomcode_dir.join("user-wrap.md");
        std::fs::write(&p1, "Project AtomCode: {{input}}").unwrap();

        let p2 = temp.path().join("user-wrap.md");
        std::fs::write(&p2, "Project Root: {{input}}").unwrap();

        let hook = UserWrapHook::new(temp.path());
        assert_eq!(hook.wrap_input("test"), "Project AtomCode: test");
    }

    #[test]
    fn test_wrap_input_without_placeholder_appends() {
        let temp = TempDir::new().unwrap();
        let wrap_file = temp.path().join("user-wrap.md");
        std::fs::write(&wrap_file, "PREFIX HEADER").unwrap();

        let hook = UserWrapHook::new(temp.path());
        assert_eq!(hook.wrap_input("query"), "PREFIX HEADER\n\nquery");
    }

    #[tokio::test]
    async fn test_user_prompt_submit_lifecycle() {
        let temp = TempDir::new().unwrap();
        let wrap_file = temp.path().join("user-wrap.md");
        std::fs::write(&wrap_file, "Wrapped: [{{input}}]").unwrap();

        let hook = UserWrapHook::new(temp.path());
        let mut text = "original".to_string();
        hook.user_prompt_submit(&mut text).await.unwrap();
        assert_eq!(text, "Wrapped: [original]");
    }
}
