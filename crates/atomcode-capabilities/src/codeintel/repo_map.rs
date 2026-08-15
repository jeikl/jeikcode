//! `repo_map` — high-density, multi-language architectural codebase map.
//!
//! Uses Tree-sitter AST symbol extraction to build a global architecture summary of
//! key types, structs, traits, classes, interfaces, enums, and functions across all
//! 12 supported languages (Rust, Python, JS, TS, TSX, Go, Java, C, C++, C#, PHP, HTML).
//!
//! Designed to give an agent immediate full-scope situational awareness in 1 turn (~1.5k-3k tokens),
//! eliminating blind grep/list rounds when entering unfamiliar repositories.

use super::lang::Lang;
use super::symbols::extract_symbols;
use super::{err, ok};
use async_trait::async_trait;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

const DEFAULT_MAX_FILES: usize = 60;
const MAX_ALLOWED_FILES: usize = 200;
const MAX_SYMBOLS_PER_FILE: usize = 25;
const MAX_MAP_OUTPUT_BYTES: usize = 48 * 1024; // 48 KiB budget (~12k tokens max)

#[derive(Default)]
pub struct RepoMapTool;

impl RepoMapTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct Args {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    max_files: Option<usize>,
}

#[async_trait]
impl Tool for RepoMapTool {
    fn name(&self) -> &str {
        "repo_map"
    }

    fn description(&self) -> &str {
        "MANDATORY Round 1 architecture radar: generates a high-density, multi-language \
         outline of the codebase (key types, structs, interfaces, classes, functions, and module hierarchy) \
         using Tree-Sitter AST extraction. Supports Rust, Python, TS/JS, Go, Java, C, C++, C#, PHP, HTML. \
         ALWAYS call this in Round 1 on any unfamiliar project, multi-project workspace, or broad inquiry \
         to see global architecture in 1 round without blind searches."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Subdirectory or workspace path to map (default: working directory root)"
                },
                "max_files": {
                    "type": "integer",
                    "description": "Maximum number of source files to include in the architecture map (default 60, max 200)"
                }
            }
        })
    }

    fn read_only_hint(&self) -> bool {
        true
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> ToolResult {
        let a: Args = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(_) => Args {
                path: None,
                max_files: None,
            },
        };

        let target_dir = match a.path {
            Some(ref p) if !p.trim().is_empty() => {
                let resolved = if Path::new(p).is_absolute() {
                    PathBuf::from(p)
                } else {
                    ctx.working_dir.join(p)
                };
                resolved
            }
            _ => ctx.working_dir.clone(),
        };

        if !target_dir.exists() {
            return err(format!(
                "repo_map: target directory does not exist: {}",
                target_dir.display()
            ));
        }

        let max_files = a.max_files.unwrap_or(DEFAULT_MAX_FILES).min(MAX_ALLOWED_FILES);
        let working_dir = ctx.working_dir.clone();

        let result = tokio::task::spawn_blocking(move || {
            build_repo_map(&target_dir, &working_dir, max_files)
        })
        .await;

        match result {
            Ok(content) => ok(content),
            Err(e) => err(format!("repo_map execution failed: {e}")),
        }
    }
}

/// Score a relative path so key architectural files and entry points are prioritized.
fn file_priority_score(rel_path: &str) -> i32 {
    let lower = rel_path.to_ascii_lowercase();
    let mut score = 0;

    // Entry points and exports
    if lower.contains("main.") || lower.contains("lib.") || lower.contains("index.") || lower.contains("app.") || lower.contains("mod.rs") {
        score += 50;
    }
    // Core contracts and schemas
    if lower.contains("types.") || lower.contains("schema.") || lower.contains("models.") || lower.contains("protocol.") || lower.contains("interface.") {
        score += 40;
    }
    // High-signal architectural layers
    if lower.contains("service") || lower.contains("controller") || lower.contains("agent") || lower.contains("kernel") || lower.contains("engine") {
        score += 30;
    }
    // Shallow depth bonus
    let depth = rel_path.split('/').count();
    score += (10 - depth.min(10)) as i32 * 5;

    // Deprioritize tests, mocks, examples, and generated fixtures
    if lower.contains("test") || lower.contains("mock") || lower.contains("spec") || lower.contains("fixture") || lower.contains("example") {
        score -= 60;
    }

    score
}

fn is_skip_dir_component(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | "bin"
            | "obj"
            | ".atomcode"
            | ".claude"
            | ".opencode"
            | "vendor"
            | ".venv"
            | "__pycache__"
    )
}

fn build_repo_map(target_dir: &Path, working_dir: &Path, max_files: usize) -> String {
    let mut walker = WalkBuilder::new(target_dir);
    walker
        .standard_filters(true)
        .hidden(true)
        .parents(true)
        .git_ignore(true);

    let mut candidate_files: Vec<(i32, PathBuf, Lang)> = Vec::new();
    let mut lang_counts: HashMap<String, usize> = HashMap::new();

    for entry in walker.build().filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }

        // Check if any component is a skipped dir
        if path.components().any(|c| {
            c.as_os_str()
                .to_str()
                .map(is_skip_dir_component)
                .unwrap_or(false)
        }) {
            continue;
        }

        if let Some(lang) = Lang::detect(path) {
            let rel = path
                .strip_prefix(working_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");

            let lang_name = format!("{lang:?}");
            *lang_counts.entry(lang_name).or_default() += 1;

            let score = file_priority_score(&rel);
            candidate_files.push((score, path.to_path_buf(), lang));
        }
    }

    if candidate_files.is_empty() {
        return "(no supported source files found in target directory)".to_string();
    }

    // Sort by priority score descending
    candidate_files.sort_by(|a, b| b.0.cmp(&a.0));

    let selected_files = &candidate_files[..candidate_files.len().min(max_files)];

    let mut output = String::new();
    output.push_str("=== CODEBASE ARCHITECTURE MAP (Tree-Sitter AST) ===\n");
    output.push_str(&format!(
        "Overview: {} / {} candidate source files indexed by priority (Languages: {})\n\n",
        selected_files.len(),
        candidate_files.len(),
        lang_counts
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // Group files by top-level module/directory for clean mental models
    let mut module_map: BTreeMap<String, Vec<&(i32, PathBuf, Lang)>> = BTreeMap::new();
    for item in selected_files {
        let rel = item
            .1
            .strip_prefix(working_dir)
            .unwrap_or(&item.1)
            .to_string_lossy()
            .replace('\\', "/");

        let module_name = if let Some(idx) = rel.find('/') {
            rel[..idx].to_string()
        } else {
            "root".to_string()
        };
        module_map.entry(module_name).or_default().push(item);
    }

    for (module, files) in module_map {
        output.push_str(&format!("📦 [{module}]\n"));

        for (_, path, lang) in files {
            let rel = path
                .strip_prefix(working_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");

            let content = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let symbols = match extract_symbols(&content, *lang) {
                Some(s) if !s.is_empty() => s,
                _ => {
                    output.push_str(&format!("  📄 {rel} ({lang:?})\n"));
                    continue;
                }
            };

            output.push_str(&format!("  📄 {rel} ({lang:?})\n"));

            // Filter and compact key symbols
            let mut formatted_symbols = Vec::new();
            for sym in symbols.iter().take(MAX_SYMBOLS_PER_FILE) {
                let kind_label = match sym.kind.as_str() {
                    "struct_item" | "struct_declaration" | "struct_specifier" | "struct" => "struct",
                    "class_definition" | "class_declaration" | "class_specifier" | "class" => "class",
                    "trait_item" | "trait_definition" | "trait" => "trait",
                    "interface_declaration" | "interface_type" | "interface" | "protocol" => "interface",
                    "enum_item" | "enum_declaration" | "enum_specifier" | "enum" => "enum",
                    "function_item" | "function_definition" | "function_declaration" | "func_item" | "function" | "fn" | "def" => "fn",
                    "method_definition" | "method_declaration" | "method" => "method",
                    "type_item" | "type_alias_declaration" | "type_declaration" | "type_spec" | "typedef_declaration" | "type" => "type",
                    "mod_item" | "module" | "namespace_declaration" | "mod" => "mod",
                    "contract" => "contract",
                    "resource" => "resource",
                    "impl_item" | "impl" => "impl",
                    _ => {
                        if !sym.name.is_empty() {
                            sym.kind.split('_').next().unwrap_or("sym")
                        } else {
                            continue;
                        }
                    }
                };
                formatted_symbols.push(format!("{kind_label} {}:{}", sym.name, sym.start_line));
            }

            if !formatted_symbols.is_empty() {
                output.push_str(&format!("     └─ {}\n", formatted_symbols.join(", ")));
            }

            if output.len() > MAX_MAP_OUTPUT_BYTES {
                output.push_str("\n... [output capped at 48KB budget; remaining files omitted]\n");
                return output;
            }
        }
        output.push('\n');
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_file_priority_scoring() {
        assert!(file_priority_score("src/main.rs") > file_priority_score("tests/fixture.rs"));
        assert!(file_priority_score("crates/auth/src/types.rs") > file_priority_score("crates/auth/src/util_mock.rs"));
    }

    #[tokio::test]
    async fn test_multi_language_repo_map() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // 1. Rust file
        std::fs::write(
            root.join("main.rs"),
            "pub struct ServerConfig { port: u16 }\npub fn start_server() {}\n",
        )
        .unwrap();

        // 2. Python file
        std::fs::write(
            root.join("app.py"),
            "class UserModel:\n    def __init__(self):\n        pass\n\ndef run_app():\n    pass\n",
        )
        .unwrap();

        // 3. TypeScript file
        std::fs::write(
            root.join("index.ts"),
            "export interface AuthPayload { token: string; }\nexport function verifyToken() {}\n",
        )
        .unwrap();

        // 4. Go file
        std::fs::write(
            root.join("service.go"),
            "package main\ntype OrderService struct {}\nfunc ProcessOrder() {}\n",
        )
        .unwrap();

        let map_output = build_repo_map(root, root, 10);
        assert!(map_output.contains("CODEBASE ARCHITECTURE MAP"));
        assert!(map_output.contains("ServerConfig"));
        assert!(map_output.contains("UserModel"));
        assert!(map_output.contains("AuthPayload"));
        assert!(map_output.contains("OrderService"));
    }
}
