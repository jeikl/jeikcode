//! `find_symbol` — workspace-wide symbol lookup by name (code graph index).
//! Complements file-local `list_symbols` / `read_symbol` for NL queries like
//! "find CouponService" without grepping the whole tree first.

use super::index::CodeIndex;
use super::{canonical, display_path, err, graph_tool_desc, load_graph, ok};
use crate::tool_feedback::{parse_tool_args, similar_symbol_names};
use async_trait::async_trait;
use atomcode_kernel::tool::{ProgressSink, Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

pub struct FindSymbolTool {
    index: Arc<CodeIndex>,
}

impl FindSymbolTool {
    pub fn new(index: Arc<CodeIndex>) -> Self {
        Self { index }
    }
}

#[derive(Deserialize)]
struct Args {
    /// Exact symbol name (e.g. `CouponService`, `Apply`).
    #[serde(default)]
    name: Option<String>,
    /// Multiple symbol names to query in one batch.
    #[serde(default)]
    symbols: Option<Vec<String>>,
    /// Multiple symbol names alias.
    #[serde(default)]
    names: Option<Vec<String>>,
    /// Cap on listed matches per symbol (default 20, max 50).
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for FindSymbolTool {
    fn name(&self) -> &str {
        "find_symbol"
    }
    fn description(&self) -> &str {
        graph_tool_desc!(
            "Find definitions of symbols across the workspace code graph (classes, structs, \
             methods, functions, traits, records, …). Supports single symbol `name` or \
             batch `symbols: [\"Name1\", \"Name2\"]`. Prefer this over grep when you know \
             the symbol name(s). Supports Rust, Python, JS/TS, Go, Java, C/C++, C#. \
             Next: trace_callers / blast_radius / file_dependencies for impact."
        )
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Exact symbol name to look up" },
                "symbols": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of multiple symbol names to look up in one batch"
                },
                "limit": { "type": "integer", "description": "Max matches to list per symbol (default 20, max 50)" }
            }
        })
    }
    fn read_only_hint(&self) -> bool {
        true
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> ToolResult {
        let a: Args = match parse_tool_args("find_symbol", args, r#"{"name":"<SymbolName>"}"#) {
            Ok(a) => a,
            Err(e) => return e.into_tool_result(),
        };

        let mut query_names: Vec<String> = Vec::new();
        if let Some(ref list) = a.symbols {
            query_names.extend(
                list.iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            );
        }
        if let Some(ref list) = a.names {
            query_names.extend(
                list.iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            );
        }
        if let Some(ref n) = a.name {
            let trimmed = n.trim().to_string();
            if !trimmed.is_empty() && !query_names.contains(&trimmed) {
                query_names.push(trimmed);
            }
        }

        if query_names.is_empty() {
            return err(
                "find_symbol: no symbol name provided. Pass `name: \"<Name>\"` or `symbols: [\"Name1\", \"Name2\"]`."
                    .to_string(),
            );
        }

        let limit = a.limit.unwrap_or(20).clamp(1, 50);
        let index = self.index.clone();
        let root = ctx.working_dir.clone();
        let progress = ctx.progress.clone();
        tokio::task::spawn_blocking(move || {
            render_symbols(&index, &root, &query_names, limit, &progress)
        })
        .await
        .unwrap_or_else(|_| err("find_symbol: task failed"))
    }
}

fn render_symbols(
    index: &CodeIndex,
    root: &Path,
    names: &[String],
    limit: usize,
    progress: &ProgressSink,
) -> ToolResult {
    let g = load_graph(index, root, progress);
    let croot = canonical(root);
    let root: &Path = &croot;

    if names.len() == 1 {
        let name = &names[0];
        let matches = g.find_by_name(name);
        if matches.is_empty() {
            let known = g.by_name.keys().map(|s| s.as_str());
            let suggestions = similar_symbol_names(name, known, 8);
            let mut msg = format!(
                "Symbol '{name}' not found in the code graph ({} symbols indexed).",
                g.node_count()
            );
            if !suggestions.is_empty() {
                msg.push_str("\nDid you mean one of these?\n  - ");
                msg.push_str(&suggestions.join("\n  - "));
            } else {
                msg.push_str(
                    "\nTip: try grep for the name, or list_symbols on a candidate file path.",
                );
            }
            return err(msg);
        }

        let total = matches.len();
        let mut out = format!("Found {total} definition(s) of '{name}':\n\n");
        for (i, sym) in matches.iter().take(limit).enumerate() {
            out.push_str(&format!(
                "{}. {:?}  {}  L{}-L{}\n",
                i + 1,
                sym.kind,
                display_path(&sym.file, root),
                sym.start_line,
                sym.end_line
            ));
        }
        if total > limit {
            out.push_str(&format!(
                "\n… and {} more (raise `limit`, max 50).\n",
                total - limit
            ));
        }
        out.push_str(
            "\nNext: read_symbol / read_file for body; trace_callers / blast_radius for impact.\n",
        );
        return ok(out);
    }

    // Multi-symbol batch output
    let mut out = format!("Batch symbol lookup for {} names:\n\n", names.len());
    let mut found_any = false;

    for name in names {
        let matches = g.find_by_name(name);
        if matches.is_empty() {
            out.push_str(&format!("### '{name}': (not found)\n\n"));
        } else {
            found_any = true;
            out.push_str(&format!("### '{name}' ({} match(es)):\n", matches.len()));
            for (i, sym) in matches.iter().take(limit).enumerate() {
                out.push_str(&format!(
                    "  {}. {:?}  {}  L{}-L{}\n",
                    i + 1,
                    sym.kind,
                    display_path(&sym.file, root),
                    sym.start_line,
                    sym.end_line
                ));
            }
            out.push('\n');
        }
    }

    if !found_any {
        return err(format!(
            "None of the {} requested symbols were found in the code graph.",
            names.len()
        ));
    }

    ok(out)
}
