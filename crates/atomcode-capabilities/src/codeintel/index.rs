//! `build_graph(root)` + the shared, lazily-built [`CodeIndex`] the graph tools hold.
//!
//! # Incremental model
//!
//! The index is a map of **per-file units** (parsed symbols + raw call sites). On each
//! refresh we walk the workspace, **re-parse only dirty/new files**, drop deleted ones,
//! then recompose the cross-file [`CodeGraph`] (symbol insert + call resolution).
//! Unchanged files keep their unit — so a `git pull` that touches a handful of sources
//! does not re-tree-sitter the whole monorepo.
//!
//! A background poller (started when codeintel tools register) keeps the last-used
//! workspace warm so the next tool call is usually already up to date.

use super::graph::{CodeGraph, Edge, EdgeKind, SymbolId, SymbolKind, SymbolNode, Visibility};
use super::lang::Lang;
use super::symbols::{
    extract_call_sites_from_tree, extract_symbols_from_tree, parse_source, CallSite, Symbol,
};
use super::{path_for_display, path_matches_scope};
use ignore::WalkBuilder;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// How often the background refresher re-walks the last workspace (cheap fingerprint;
/// only dirty files are re-parsed). Debounced when recent edits occurred.
const BACKGROUND_REFRESH_SECS: u64 = 5;

/// Query-time safety valve: never re-tree-sitter thousands of files on a single
/// `code_explore`. Explicit `atomcode init` / `--force` pass [`ReparseBudget::Unlimited`].
const MAX_REPARSE_PER_QUERY: usize = 128;

/// Parse-thread cap. Same as sibling `codegraph`'s `DEFAULT_PARSE_POOL_CAP`:
/// `clamp(max(3, cores) - 1, 1, 8)` — leave a core for the main thread / SQLite
/// writer, never more than 8, and a 2-core box still gets 2 parse workers.
const MAX_PARSE_THREADS: usize = 8;
/// Files parsed (and flushed to SQLite) per batch. Bounds in-flight tree-sitter
/// trees + source strings + prepared blobs.
const PARSE_BATCH_FILES: usize = 256;
/// SQLite upsert chunk. Sibling codegraph commits as results arrive; a single
/// 15k-row WAL transaction is the other half of the OOM.
const UNIT_WRITE_CHUNK: usize = 256;
/// Per-symbol caps so a Java/C# ERP method with a wall of SQL strings cannot
/// inflate a `FileUnit` to megabytes.
const MAX_STRING_LITERALS_PER_SYMBOL: usize = 32;
const MAX_LITERAL_CHARS: usize = 240;
const MAX_SQL_PREDICATES_PER_SYMBOL: usize = 16;

/// How many dirty files [`sync_units`] may re-parse in one pass.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ReparseBudget {
    /// `code_explore` / warm refresh: cap + prefer files under `path:`.
    Query,
    /// `atomcode init` (with or without `--force`): parse every dirty file.
    Unlimited,
}

/// Unified internal path normalization for the code index.
/// Strips verbatim prefixes (`\\?\`), unifies slashes, and on Windows uppercases the drive letter.
pub fn normalize_index_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    let stripped = s
        .strip_prefix(r"\\?\")
        .or_else(|| s.strip_prefix("//?/"))
        .unwrap_or(&s);

    #[cfg(windows)]
    {
        let mut unified = stripped.replace('/', "\\");
        let bytes = unified.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            let mut chars: Vec<char> = unified.chars().collect();
            chars[0] = chars[0].to_ascii_uppercase();
            unified = chars.into_iter().collect();
        }
        PathBuf::from(unified)
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(stripped)
    }
}

/// On-disk cache layout version. Bump when AST queries / DiskCache / unit schema changes.
const DISK_CACHE_VERSION: u32 = 3;

/// Relative path under the workspace root for the persisted unit+graph cache.
pub const DISK_CACHE_REL: &str = ".atomcode/codegraph/units.v3.json";

/// Relative path for the BINARY (bincode+zstd) index cache. Written whenever
/// possible; `units.v3.json` stays as a read fallback for older workspaces.
pub const DISK_CACHE_REL_BIN: &str = ".atomcode/codegraph/units.v4.bin";

/// Map a tree-sitter node-kind string to a [`SymbolKind`]. From production
/// `classify_symbol_kind`.
fn classify_symbol_kind(ts: &str) -> SymbolKind {
    match ts {
        "function_item"
        | "function_definition"
        | "function_declaration"
        | "func_literal"
        | "local_function_statement" => SymbolKind::Function,
        "method_definition"
        | "method_declaration"
        | "constructor_declaration"
        | "destructor_declaration"
        | "operator_declaration" => SymbolKind::Method,
        "struct_item" | "struct_specifier" | "struct_type" | "struct_declaration"
        | "record_declaration" => SymbolKind::Struct,
        "class_definition" | "class_declaration" | "class_specifier" => SymbolKind::Class,
        "trait_item" => SymbolKind::Trait,
        "interface_declaration" | "interface_type" => SymbolKind::Interface,
        "enum_item" | "enum_declaration" | "enum_specifier" | "enum_member_declaration" => {
            SymbolKind::Enum
        }
        "property_declaration" | "field_declaration" => SymbolKind::Property,
        "const_item" | "const_declaration" | "event_declaration" | "delegate_declaration" => {
            SymbolKind::Constant
        }
        "let_declaration" | "variable_declaration" | "static_item" => SymbolKind::Variable,
        "mod_item" | "module" | "namespace_declaration" | "file_scoped_namespace_declaration" => {
            SymbolKind::Module
        }
        "use_declaration" | "import_statement" | "import_declaration" => SymbolKind::Import,
        "type_item" | "type_alias_declaration" => SymbolKind::TypeAlias,
        "impl_item" => SymbolKind::Other("impl".to_string()),
        // JSX/TSX UI elements: `<el-button>`, `<CouponDialog />` → UiElement.
        "jsx_element" | "jsx_self_closing_element" | "element" | "self_closing_tag" => {
            SymbolKind::UiElement
        }
        other => SymbolKind::Other(other.to_string()),
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RawCall {
    pub caller_name: String,
    /// caller's start_line — lets the build reconstruct the caller's exact id via `make_id`
    /// instead of a name lookup (removes a scan and fixes wrong-caller attribution).
    pub caller_line: usize,
    pub callee_name: String,
    pub line: usize,
}

/// Attribute call sites to the innermost enclosing Function/Method symbol.
fn attribute_calls(syms: &[Symbol], sites: Vec<super::symbols::CallSite>) -> Vec<RawCall> {
    // Pre-filter callables once — attribution is O(sites × callables), not O(sites × all_syms).
    let callables: Vec<&Symbol> = syms
        .iter()
        .filter(|s| {
            matches!(
                classify_symbol_kind(&s.kind),
                SymbolKind::Function | SymbolKind::Method
            )
        })
        .collect();
    let mut calls = Vec::with_capacity(sites.len());
    for site in sites {
        let caller = callables
            .iter()
            .filter(|s| s.start_line <= site.line && site.line <= s.end_line)
            .max_by_key(|s| s.start_line);
        if let Some(caller) = caller {
            if caller.name != site.callee_name {
                calls.push(RawCall {
                    caller_name: caller.name.clone(),
                    caller_line: caller.start_line,
                    callee_name: site.callee_name,
                    line: site.line,
                });
            }
        }
    }
    calls
}

fn parse_yaml_config(path: &Path, source: &str) -> Option<(Vec<SymbolNode>, Vec<RawCall>)> {
    let mut nodes = Vec::new();
    let lines: Vec<&str> = source.lines().collect();

    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1;
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }

        if let Some(pos) = trimmed.find(':') {
            let key = trimmed[..pos]
                .trim()
                .trim_start_matches("- ")
                .trim_matches('"')
                .trim_matches('\'');
            if !key.is_empty() && key.len() >= 3 && !key.contains(' ') {
                let kind = if key.contains("plugin")
                    || key.contains("reminder")
                    || key.contains("middleware")
                    || key.contains("hook")
                {
                    SymbolKind::PluginDeclaration
                } else {
                    SymbolKind::ConfigProperty
                };
                nodes.push(SymbolNode {
                    id: CodeGraph::make_id(path, key, line_num),
                    name: key.to_string(),
                    kind,
                    visibility: Visibility::Public,
                    file: path.to_path_buf(),
                    start_line: line_num,
                    end_line: (line_num + 3).min(lines.len()),
                    signature: Some(trimmed.to_string()),
                    ..Default::default()
                });
            }
        } else if trimmed.starts_with("- ") {
            let val = trimmed[2..].trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() && val.len() >= 3 && !val.contains(' ') {
                nodes.push(SymbolNode {
                    id: CodeGraph::make_id(path, val, line_num),
                    name: val.to_string(),
                    kind: SymbolKind::PluginDeclaration,
                    visibility: Visibility::Public,
                    file: path.to_path_buf(),
                    start_line: line_num,
                    end_line: line_num,
                    signature: Some(trimmed.to_string()),
                    ..Default::default()
                });
            }
        }
    }

    if nodes.is_empty() {
        None
    } else {
        Some((nodes, Vec::new()))
    }
}

fn parse_toml_config(path: &Path, source: &str) -> Option<(Vec<SymbolNode>, Vec<RawCall>)> {
    let mut nodes = Vec::new();
    let lines: Vec<&str> = source.lines().collect();

    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1;
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section = trimmed.trim_matches('[').trim_matches(']').trim();
            if !section.is_empty() {
                nodes.push(SymbolNode {
                    id: CodeGraph::make_id(path, section, line_num),
                    name: section.to_string(),
                    kind: SymbolKind::ConfigProperty,
                    visibility: Visibility::Public,
                    file: path.to_path_buf(),
                    start_line: line_num,
                    end_line: line_num,
                    signature: Some(trimmed.to_string()),
                    ..Default::default()
                });
            }
        } else if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos]
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            if !key.is_empty() && key.len() >= 2 && !key.contains(' ') {
                nodes.push(SymbolNode {
                    id: CodeGraph::make_id(path, key, line_num),
                    name: key.to_string(),
                    kind: SymbolKind::ConfigProperty,
                    visibility: Visibility::Public,
                    file: path.to_path_buf(),
                    start_line: line_num,
                    end_line: line_num,
                    signature: Some(trimmed.to_string()),
                    ..Default::default()
                });
            }
        }
    }

    if nodes.is_empty() {
        None
    } else {
        Some((nodes, Vec::new()))
    }
}

fn parse_markdown_doc(path: &Path, source: &str) -> Option<(Vec<SymbolNode>, Vec<RawCall>)> {
    let mut nodes = Vec::new();
    let lines: Vec<&str> = source.lines().collect();

    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1;
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let title = trimmed.trim_start_matches('#').trim();
            if !title.is_empty() && title.len() >= 2 {
                nodes.push(SymbolNode {
                    id: CodeGraph::make_id(path, title, line_num),
                    name: title.to_string(),
                    kind: SymbolKind::Module,
                    visibility: Visibility::Public,
                    file: path.to_path_buf(),
                    start_line: line_num,
                    end_line: (line_num + 5).min(lines.len()),
                    signature: Some(trimmed.to_string()),
                    docstring: Some(title.to_string()),
                    ..Default::default()
                });
            }
        }
    }

    if nodes.is_empty() {
        None
    } else {
        Some((nodes, Vec::new()))
    }
}

fn parse_json_config(path: &Path, source: &str) -> Option<(Vec<SymbolNode>, Vec<RawCall>)> {
    let mut nodes = Vec::new();
    let lines: Vec<&str> = source.lines().collect();

    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1;
        let trimmed = line.trim();
        if let Some(colon) = trimmed.find(':') {
            let key = trimmed[..colon].trim().trim_matches('"').trim_matches('\'');
            if (key.contains("plugin")
                || key.contains("middleware")
                || key.contains("name")
                || key.contains("main")
                || key.contains("scripts"))
                && key.len() >= 3
            {
                let val_part = trimmed[colon + 1..]
                    .trim()
                    .trim_matches(',')
                    .trim_matches('"')
                    .trim_matches('\'');
                let sym_name = if !val_part.is_empty()
                    && val_part.len() >= 3
                    && !val_part.starts_with('{')
                    && !val_part.starts_with('[')
                {
                    format!("{key}::{val_part}")
                } else {
                    key.to_string()
                };
                nodes.push(SymbolNode {
                    id: CodeGraph::make_id(path, &sym_name, line_num),
                    name: sym_name.clone(),
                    kind: SymbolKind::PluginDeclaration,
                    visibility: Visibility::Public,
                    file: path.to_path_buf(),
                    start_line: line_num,
                    end_line: line_num,
                    signature: Some(trimmed.to_string()),
                    ..Default::default()
                });
            }
        }
    }

    if nodes.is_empty() {
        None
    } else {
        Some((nodes, Vec::new()))
    }
}

/// One tree-sitter parse → symbols + calls (was two full parses per file).
fn parse_file(path: &Path, source: &str) -> Option<(Vec<SymbolNode>, Vec<RawCall>)> {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_ascii_lowercase();
        if ext_lower == "xml" {
            if let Some(xml_res) = parse_xml_mapper(path, source) {
                return Some(xml_res);
            }
        } else if ext_lower == "yml" || ext_lower == "yaml" {
            if let Some(yaml_res) = parse_yaml_config(path, source) {
                return Some(yaml_res);
            }
        } else if ext_lower == "json" {
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if file_name.contains("package.json")
                || file_name.contains("config")
                || file_name.contains("plugin")
            {
                if let Some(json_res) = parse_json_config(path, source) {
                    return Some(json_res);
                }
            }
        } else if ext_lower == "toml" {
            if let Some(toml_res) = parse_toml_config(path, source) {
                return Some(toml_res);
            }
        } else if ext_lower == "md" || ext_lower == "markdown" {
            if let Some(md_res) = parse_markdown_doc(path, source) {
                return Some(md_res);
            }
        } else if matches!(ext_lower.as_str(), "vue" | "svelte" | "astro") {
            // SFC dual-parse: `<script>` block → TSX (component logic + imports),
            // `<template>` block → HTML element tags (buttons/titles/UI nodes).
            if let Some(sfc) = parse_sfc(path, source, &ext_lower) {
                return Some(sfc);
            }
        } else if matches!(ext_lower.as_str(), "css" | "scss" | "less" | "sass") {
            // Stylesheet: textual class/id/at-rule extraction (zero new deps).
            if let Some(css_res) = parse_css_styles(path, source) {
                return Some(css_res);
            }
        }
    }

    let lang = Lang::detect(path)?;
    if !lang.is_indexed() {
        return None;
    }
    let tree = parse_source(source, lang)?;
    let raw = extract_symbols_from_tree(source, lang, &tree)?;
    let sites = extract_call_sites_from_tree(source, lang, &tree);
    let calls = attribute_calls(&raw, sites);
    let mut nodes: Vec<SymbolNode> = raw
        .iter()
        .map(|s| SymbolNode {
            id: CodeGraph::make_id(path, &s.name, s.start_line),
            name: s.name.clone(),
            kind: classify_symbol_kind(&s.kind),
            visibility: Visibility::Unknown,
            file: path.to_path_buf(),
            start_line: s.start_line,
            end_line: s.end_line,
            ..Default::default()
        })
        .collect();

    // Extract comment blocks and bind to symbol nodes by physical line proximity with source inspection
    let comment_blocks = super::comment_index::extract_comment_blocks(source);
    super::comment_index::bind_comments_to_symbols_with_source(&mut nodes, &comment_blocks, source);

    // Enrich micro-structures (string literals, SQL/QS clauses, AST metrics)
    enrich_symbol_microstructure(&mut nodes, source);

    Some((nodes, calls))
}

/// SFC (Single-File Component) dual-parse for `.vue` / `.svelte` / `.astro`.
///
/// Splits the file into its `<script>…</script>` and `<template>…</template>`
/// blocks, parses the script as TSX (component logic, imports, methods) and the
/// template as HTML (element tags → `UiElement` symbols for buttons/titles/UI
/// nodes), then merges the two symbol sets with line offsets so line numbers
/// stay file-absolute. `<style>` blocks are intentionally skipped here (CSS
/// class extraction is a separate textual pass; see `parse_css_styles`).
///
/// Falls back to a plain TSX parse of the whole file when the script/template
/// split fails (non-SFC content, malformed blocks).
fn parse_sfc(path: &Path, source: &str, ext: &str) -> Option<(Vec<SymbolNode>, Vec<RawCall>)> {
    let script = extract_sfc_block(source, "script");
    let template = extract_sfc_block(source, "template");

    let mut nodes: Vec<SymbolNode> = Vec::new();
    let mut calls: Vec<RawCall> = Vec::new();

    // 1. `<script>` → TSX symbol extraction (with line offset).
    if let Some((body, start_line)) = script {
        if let Some((raw_syms, raw_calls)) = parse_sfc_tsx_block(&body) {
            // Shift symbol lines to file-absolute BEFORE call attribution, so
            // `attribute_calls` (line-enclosure matching) works on real lines.
            let shifted_syms: Vec<Symbol> = raw_syms
                .iter()
                .map(|s| Symbol {
                    name: s.name.clone(),
                    kind: s.kind.clone(),
                    start_line: s.start_line + start_line - 1,
                    end_line: s.end_line + start_line - 1,
                    start_byte: s.start_byte,
                    end_byte: s.end_byte,
                })
                .collect();
            let shifted_sites: Vec<CallSite> = raw_calls
                .iter()
                .map(|c| CallSite {
                    callee_name: c.callee_name.clone(),
                    line: c.line + start_line - 1,
                })
                .collect();
            calls.extend(attribute_calls(&shifted_syms, shifted_sites));
            for s in shifted_syms {
                nodes.push(SymbolNode {
                    id: CodeGraph::make_id(path, &s.name, s.start_line),
                    name: s.name.clone(),
                    kind: classify_symbol_kind(&s.kind),
                    visibility: Visibility::Unknown,
                    file: path.to_path_buf(),
                    start_line: s.start_line,
                    end_line: s.end_line,
                    ..Default::default()
                });
            }
        }
    }

    // 2. `<template>` → HTML element tags (`UiElement`).
    if let Some((body, start_line)) = template {
        if let Some((raw_syms, _)) = parse_sfc_html_block(&body) {
            for s in raw_syms {
                let l = s.start_line + start_line - 1;
                nodes.push(SymbolNode {
                    id: CodeGraph::make_id(path, &s.name, l),
                    name: s.name.clone(),
                    kind: SymbolKind::UiElement,
                    visibility: Visibility::Public,
                    file: path.to_path_buf(),
                    start_line: l,
                    end_line: s.end_line + start_line - 1,
                    ..Default::default()
                });
            }
        }
    }

    if nodes.is_empty() {
        return None;
    }
    // Dedup by (name, line) — script and template rarely collide, but a
    // `Component` variable in script vs `<Component>` tag in template would.
    nodes.sort_by(|a, b| a.start_line.cmp(&b.start_line).then(a.name.cmp(&b.name)));
    nodes.dedup_by(|a, b| a.name == b.name && a.start_line == b.start_line);

    // Bind comments across the whole file (both blocks).
    let mut comment_blocks = super::comment_index::extract_comment_blocks(source);
    comment_blocks.sort_by_key(|b| b.start_line);
    super::comment_index::bind_comments_to_symbols_with_source(&mut nodes, &comment_blocks, source);

    // Enrich micro-structures (string literals, SQL/QS clauses, AST metrics)
    enrich_symbol_microstructure(&mut nodes, source);

    Some((nodes, calls))
}

fn enrich_symbol_microstructure(nodes: &mut [SymbolNode], source: &str) {
    let lines: Vec<&str> = source.lines().collect();

    for sym in nodes.iter_mut() {
        let sym_start = sym.start_line.saturating_sub(1);
        let sym_end = sym.end_line.min(lines.len());
        if sym_start >= lines.len() || sym_start > sym_end {
            continue;
        }

        let sym_lines = &lines[sym_start..sym_end];
        let mut literals = Vec::new();
        let mut sqls = Vec::new();
        let mut branches = 0usize;

        for (idx, &line) in sym_lines.iter().enumerate() {
            let line_num = sym.start_line + idx;
            let trimmed = line.trim();

            // Branch detection
            if trimmed.contains("case ")
                || trimmed.contains("if ")
                || trimmed.contains("if(")
                || trimmed.contains("else if")
                || trimmed.contains("elif ")
                || trimmed.contains("switch")
                || trimmed.contains("match ")
                || trimmed.contains("=>")
            {
                branches += 1;
            }

            // String literal extraction (double quote and single quote)
            extract_literals_from_line(trimmed, &mut literals);

            // SQL / QS predicate detection
            if is_sql_or_qs_clause(trimmed) {
                let fields = extract_sql_fields(trimmed);
                sqls.push(super::graph::SqlPredicate {
                    raw_clause: trimmed.to_string(),
                    target_fields: fields,
                    line: line_num,
                });
            }
        }

        literals.sort();
        literals.dedup();
        if literals.len() > MAX_STRING_LITERALS_PER_SYMBOL {
            literals.truncate(MAX_STRING_LITERALS_PER_SYMBOL);
        }
        for lit in &mut literals {
            truncate_to_char_boundary(lit, MAX_LITERAL_CHARS);
        }
        if sqls.len() > MAX_SQL_PREDICATES_PER_SYMBOL {
            sqls.truncate(MAX_SQL_PREDICATES_PER_SYMBOL);
        }
        for pred in &mut sqls {
            truncate_to_char_boundary(&mut pred.raw_clause, MAX_LITERAL_CHARS);
        }

        let has_sql = !sqls.is_empty();
        let is_dto = matches!(
            sym.kind,
            SymbolKind::Property | SymbolKind::ConfigProperty | SymbolKind::Variable
        ) || (sym.kind == SymbolKind::Class
            && branches == 0
            && !has_sql
            && sym_lines
                .iter()
                .all(|l| !l.contains("while") && !l.contains("for")));

        let is_active = (branches >= 1
            || has_sql
            || matches!(
                sym.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::SqlStatement
            ))
            && !is_dto;

        sym.metrics = super::graph::AstMetrics {
            cyclomatic_complexity: 1 + branches,
            branch_count: branches,
            has_sql_or_qs: has_sql,
            is_pure_dto: is_dto,
            is_active_logic: is_active,
        };
        sym.string_literals = literals;
        sym.sql_predicates = sqls;
    }
}

fn extract_literals_from_line(line: &str, out: &mut Vec<String>) {
    let mut in_str = false;
    let mut quote_char = '"';
    let mut buf = String::new();
    let bytes = line.as_bytes();
    let len = bytes.len();

    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if in_str {
            if b == b'\\' && i + 1 < len {
                buf.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b as char == quote_char {
                in_str = false;
                let trimmed = buf.trim();
                if trimmed.len() >= 2 && !is_trivial_literal(trimmed) {
                    out.push(trimmed.to_string());
                }
                buf.clear();
            } else {
                buf.push(b as char);
            }
        } else if b == b'"' || (b == b'\'' && !is_char_literal_bytes(bytes, i)) {
            in_str = true;
            quote_char = b as char;
        }
        i += 1;
    }
}

#[inline(always)]
fn is_char_literal_bytes(bytes: &[u8], idx: usize) -> bool {
    idx + 2 < bytes.len() && bytes[idx + 2] == b'\''
}

/// Byte cap that never panics on CJK (ERP Chinese string literals).
/// `String::truncate(n)` requires a char boundary; a 240-byte cut can land
/// in the middle of a 3-byte 汉字 and abort `atomcode init`.
fn truncate_to_char_boundary(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

fn is_trivial_literal(s: &str) -> bool {
    const TRIVIAL: &[&str] = &[
        "",
        " ",
        "\n",
        "\t",
        "true",
        "false",
        "null",
        "undefined",
        "0",
        "1",
        "utf-8",
        "utf8",
    ];
    TRIVIAL.contains(&s) || s.chars().all(|c| c.is_ascii_punctuation())
}

fn is_sql_or_qs_clause(line: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    (upper.contains("SELECT ") && upper.contains("FROM "))
        || (upper.contains("WHERE ") || upper.contains(" AND ") || upper.contains(" OR "))
        || (upper.contains("EXISTS(") || upper.contains("EXISTS ("))
        || (upper.contains(" IN (") || upper.contains(" IN("))
        || (upper.contains("BETWEEN ") && upper.contains(" AND "))
        || (upper.contains("JOIN ") && upper.contains(" ON "))
        || line.contains(".Where(")
        || line.contains(".Select(")
        || line.contains(".OrderBy(")
        || line.contains("SugarParameter")
        || line.contains("SqlSugar")
}

fn extract_sql_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    for token in line.split(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_') {
        let t = token.trim();
        if let Some(pos) = t.find('.') {
            let field = &t[pos + 1..];
            if field.len() >= 3 && !field.chars().all(|c| c.is_ascii_digit()) {
                fields.push(field.to_string());
            }
        }
    }
    fields
}

/// Extract a `<tag>…</tag>` block's body + its 1-based start line.
/// Handles `<script setup>` / `<template lang="pug">` opening tags.
fn extract_sfc_block(source: &str, tag: &str) -> Option<(String, usize)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut start_line = None;
    let mut depth = 0usize;
    let mut body = String::new();
    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1;
        let trimmed = line.trim();
        if start_line.is_none() {
            if trimmed.starts_with(&format!("<{tag}")) && !trimmed.starts_with(&format!("</{tag}"))
            {
                start_line = Some(line_num);
                // After the `>` of the opening tag, the rest of the line is body.
                if let Some(gt) = trimmed.find('>') {
                    let rest = trimmed[gt + 1..].trim();
                    if !rest.is_empty() {
                        body.push_str(rest);
                        body.push('\n');
                    }
                } else {
                    // Multi-line opening tag: next line starts the body.
                    depth = 1;
                }
                continue;
            }
        } else if trimmed.starts_with(&format!("<{tag}"))
            && !trimmed.starts_with(&format!("</{tag}"))
        {
            depth += 1;
        } else if trimmed.starts_with(&format!("</{tag}")) {
            if depth == 0 {
                // This line is the closing tag; body ends before it.
                return Some((body, start_line?));
            }
            depth -= 1;
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    // Reached EOF without a close (malformed) — still return what we have.
    if start_line.is_some() {
        Some((body, start_line?))
    } else {
        None
    }
}

/// Parse an SFC script body as TSX (symbols + call sites).
fn parse_sfc_tsx_block(body: &str) -> Option<(Vec<Symbol>, Vec<CallSite>)> {
    let tree = parse_source(body, Lang::Tsx)?;
    let raw = extract_symbols_from_tree(body, Lang::Tsx, &tree)?;
    let sites = extract_call_sites_from_tree(body, Lang::Tsx, &tree);
    Some((raw, sites))
}

/// Parse an SFC template body as HTML (element tags only).
fn parse_sfc_html_block(body: &str) -> Option<(Vec<Symbol>, Vec<CallSite>)> {
    let tree = parse_source(body, Lang::Html)?;
    let raw = extract_symbols_from_tree(body, Lang::Html, &tree)?;
    Some((raw, Vec::new()))
}

/// Textual CSS/SCSS/LESS/SASS selector extraction (zero new tree-sitter deps).
///
/// Character-scan each line for:
/// - `.class` selectors (anywhere — handles `.a .b` spacing, `.a,.b` commas,
///   single-line nesting like `@media { .x { … } }`, and `:pseudo` stripping)
/// - `#id` selectors (kept as `#id`)
/// - `@keyframes name` animation names
///
/// Every hit becomes a `UiElement` symbol so a natural-language query for a
/// class name ("coupon-panel" / 优惠券面板 via thesaurus) can recall the
/// stylesheet. Property values like `0.5` or `color: red` produce no hits
/// (a `.` only starts a class when followed by a letter/underscore/CJK).
fn parse_css_styles(path: &Path, source: &str) -> Option<(Vec<SymbolNode>, Vec<RawCall>)> {
    let mut nodes: Vec<SymbolNode> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (idx, line) in source.lines().enumerate() {
        let line_num = idx + 1;
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            if (ch == '.' || ch == '#') && i + 1 < chars.len() && chars[i + 1].is_ascii_alphabetic()
            {
                let mut j = i + 1;
                while j < chars.len()
                    && (chars[j].is_ascii_alphanumeric() || chars[j] == '-' || chars[j] == '_')
                {
                    j += 1;
                }
                let ident: String = chars[i + 1..j].iter().collect();
                if ident.len() >= 2 {
                    let name = if ch == '#' {
                        format!("#{ident}")
                    } else {
                        ident
                    };
                    let key = format!("{name}@{line_num}");
                    if seen.insert(key) {
                        nodes.push(SymbolNode {
                            id: CodeGraph::make_id(path, &name, line_num),
                            name,
                            kind: SymbolKind::UiElement,
                            visibility: Visibility::Public,
                            file: path.to_path_buf(),
                            start_line: line_num,
                            end_line: line_num,
                            ..Default::default()
                        });
                    }
                }
                i = j;
            } else if ch == '@' {
                let s = &line[i..];
                if s.starts_with("@keyframes") {
                    let mut j = i + "@keyframes".len();
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    let start = j;
                    while j < chars.len()
                        && (chars[j].is_ascii_alphanumeric() || chars[j] == '-' || chars[j] == '_')
                    {
                        j += 1;
                    }
                    let name: String = chars[start..j].iter().collect();
                    if !name.is_empty() {
                        let key = format!("@{name}@{line_num}");
                        if seen.insert(key) {
                            nodes.push(SymbolNode {
                                id: CodeGraph::make_id(path, &name, line_num),
                                name,
                                kind: SymbolKind::UiElement,
                                visibility: Visibility::Public,
                                file: path.to_path_buf(),
                                start_line: line_num,
                                end_line: line_num,
                                ..Default::default()
                            });
                        }
                    }
                    i = j;
                } else {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
    }

    if nodes.is_empty() {
        None
    } else {
        Some((nodes, Vec::new()))
    }
}

fn parse_xml_mapper(path: &Path, source: &str) -> Option<(Vec<SymbolNode>, Vec<RawCall>)> {
    let mut nodes = Vec::new();
    let lines: Vec<&str> = source.lines().collect();

    let mut namespace = String::new();
    for line in &lines {
        if let Some(pos) = line.find("namespace=") {
            let rest = &line[pos + 10..];
            if let Some(quote) = rest.chars().next() {
                if quote == '"' || quote == '\'' {
                    if let Some(end) = rest[1..].find(quote) {
                        namespace = rest[1..=end].to_string();
                    }
                }
            }
        }
    }

    if namespace.is_empty()
        && !source.contains("<select")
        && !source.contains("<insert")
        && !source.contains("<update")
        && !source.contains("<delete")
    {
        return None;
    }

    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1;
        let trimmed = line.trim();
        for tag in &["<select", "<insert", "<update", "<delete"] {
            if trimmed.starts_with(tag) {
                if let Some(id_pos) = trimmed.find("id=") {
                    let rest = &trimmed[id_pos + 3..];
                    if let Some(quote) = rest.chars().next() {
                        if quote == '"' || quote == '\'' {
                            if let Some(end) = rest[1..].find(quote) {
                                let id = &rest[1..=end];
                                let sym_name = if !namespace.is_empty() {
                                    format!("{namespace}::{id}")
                                } else {
                                    id.to_string()
                                };
                                nodes.push(SymbolNode {
                                    id: CodeGraph::make_id(path, &sym_name, line_num),
                                    name: sym_name.clone(),
                                    kind: SymbolKind::SqlStatement,
                                    visibility: Visibility::Public,
                                    file: path.to_path_buf(),
                                    start_line: line_num,
                                    end_line: (line_num + 5).min(lines.len()),
                                    signature: Some(trimmed.to_string()),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    if nodes.is_empty() {
        None
    } else {
        Some((nodes, Vec::new()))
    }
}

/// Extensions walked into the graph (matches production's full language matrix).
pub const INDEXED_EXTS: &[&str] = &[
    "rs", "py", "pyi", "js", "jsx", "mjs", "cjs", "ts", "mts", "cts", "tsx", "vue", "go", "java",
    "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "cs", "php", "phtml", "kt", "kts", "swift", "dart",
    "rb", "scala", "sc", "sol", "lua", "tf", "tfvars", "erl", "hrl", "r", "nix", "xml", "sql",
    "yml", "yaml", "json", "toml", "md", "markdown",
    // Frontend stylesheets + SFC flavors: textual selector extraction (zero deps).
    "css", "scss", "less", "sass", "svelte", "astro", "html",
];

pub fn is_indexed_ext(ext: &str) -> bool {
    INDEXED_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// Directory basenames skipped even when not covered by `.gitignore` (common on
/// C#/Node/Rust monorepos that ship without ignore rules, or when agents open a
/// parent folder that contains build outputs).
const SKIP_DIR_NAMES: &[&str] = &[
    "bin",
    "obj",
    "node_modules",
    "target",
    "dist",
    "build",
    ".git",
    ".vs",
    ".idea",
    "TestResults",
    "coverage",
    "wwwroot", // static assets; often minified JS
    "bower_components",
    "jspm_packages",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    "site-packages",
    ".nuget",
    ".next",
    ".turbo",
    ".cache",
    "publish",
    "out",
    "Debug",
    "Release",
];

/// Skip huge / minified / generated sources that blow up tree-sitter time.
const MAX_INDEX_FILE_BYTES: u64 = 768 * 1024;

/// Generated / designer C# sources — huge and low value for call-graph tools.
fn is_generated_source(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".designer.cs")
        || lower.ends_with(".g.cs")
        || lower.ends_with(".g.i.cs")
        || lower == "assemblyinfo.cs"
        || lower.ends_with(".assemblyattributes.cs")
        || lower.contains(".min.")
        || lower.ends_with(".min.js")
        || lower.ends_with(".min.css")
        || lower.ends_with(".bundle.js")
        || lower.ends_with(".map")
}

fn should_skip_dir(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .map(|n| SKIP_DIR_NAMES.iter().any(|s| s.eq_ignore_ascii_case(n)))
        .unwrap_or(false)
}

/// A walked source file + the inputs to its staleness fingerprint.
struct Walked {
    path: PathBuf,
    /// mtime in NANOSECONDS — coarse whole seconds would miss a same-second edit and
    /// serve a stale graph.
    mtime_ns: u128,
    /// file length — defends against a same-instant edit whose mtime didn't move (content
    /// length almost always changes on a real edit).
    len: u64,
}

/// Walk `root` (assumed already canonical) for indexable source files + staleness inputs.
fn collect_files(root: &Path) -> Vec<Walked> {
    // Fast-path: If git repository, query `git ls-files -z --cached --others --exclude-standard`.
    // Directly reads Git binary index within ~20-30ms for tens of thousands of files.
    if root.join(".git").exists() {
        if let Some(git_files) = collect_files_via_git(root) {
            if !git_files.is_empty() {
                return git_files;
            }
        }
    }

    collect_files_fallback(root)
}

fn collect_files_via_git(root: &Path) -> Option<Vec<Walked>> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(&[
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ])
    .current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }

    use rayon::prelude::*;
    let raw = output.stdout;
    let paths: Vec<&[u8]> = raw.split(|&b| b == 0).filter(|p| !p.is_empty()).collect();

    let out: Vec<Walked> = paths
        .into_par_iter()
        .filter_map(|rel_bytes| {
            let rel_str = std::str::from_utf8(rel_bytes).ok()?;
            let full_p = root.join(rel_str);
            let ext_ok = full_p
                .extension()
                .and_then(|e| e.to_str())
                .map(is_indexed_ext)
                .unwrap_or(false);
            if !ext_ok {
                return None;
            }
            if is_generated_source(&full_p) {
                return None;
            }
            let md = std::fs::metadata(&full_p).ok()?;
            if !md.is_file() {
                return None;
            }
            let len = md.len();
            if len > MAX_INDEX_FILE_BYTES {
                return None;
            }
            let mtime_ns = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let norm_path = normalize_index_path(&full_p);
            Some(Walked {
                path: norm_path,
                mtime_ns,
                len,
            })
        })
        .collect();

    Some(out)
}

fn collect_files_fallback(root: &Path) -> Vec<Walked> {
    let mut out = Vec::new();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".codegraphignore")
        .add_custom_ignore_filename(".codegraignore");

    // Add global ignore files if present (~/.atomcode/.codegraphignore, ~/.atomcode/.codegraignore)
    let global_config = crate::paths::config_dir();
    let global_ignore1 = global_config.join(".codegraphignore");
    if global_ignore1.is_file() {
        builder.add_ignore(global_ignore1);
    }
    let global_ignore2 = global_config.join(".codegraignore");
    if global_ignore2.is_file() {
        builder.add_ignore(global_ignore2);
    }

    // Also check .atomcode/.codegraphignore inside project root
    let project_atomcode_ignore = root.join(".atomcode").join(".codegraphignore");
    if project_atomcode_ignore.is_file() {
        builder.add_ignore(project_atomcode_ignore);
    }

    for entry in builder
        .filter_entry(|e| {
            // Prune known build/vendor directories early (gitignore may be missing).
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                !should_skip_dir(e.file_name())
            } else {
                true
            }
        })
        .build()
        .flatten()
    {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let ext_ok = p
            .extension()
            .and_then(|e| e.to_str())
            .map(is_indexed_ext)
            .unwrap_or(false);
        if !ext_ok {
            continue;
        }
        if is_generated_source(p) {
            continue;
        }
        let md = entry.metadata().ok();
        let len = md.as_ref().map(|m| m.len()).unwrap_or(0);
        if len > MAX_INDEX_FILE_BYTES {
            continue;
        }
        let mtime_ns = md
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        out.push(Walked {
            path: normalize_index_path(p),
            mtime_ns,
            len,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn fingerprint(files: &[Walked]) -> u64 {
    let mut h = DefaultHasher::new();
    for w in files {
        w.path.hash(&mut h);
        w.mtime_ns.hash(&mut h);
        w.len.hash(&mut h);
    }
    h.finish()
}

/// Granular phase timing metrics for index performance visualization.
#[derive(Debug, Clone, Default)]
pub struct IndexPhaseTimings {
    pub parse_ast: Duration,
    pub compose_graph: Duration,
    pub save_disk: Duration,
}

/// Result of [`init_workspace_index`] / a successful index refresh.
#[derive(Debug, Clone)]
pub struct IndexReport {
    pub root: PathBuf,
    pub cache_path: PathBuf,
    pub files: usize,
    pub symbols: usize,
    pub reparsed: usize,
    pub removed: usize,
    pub kept: usize,
    pub elapsed: Duration,
    /// True when the disk cache was a perfect walk_fp hit (no re-parse).
    pub cache_hit: bool,
    /// Granular phase breakdown
    pub phases: IndexPhaseTimings,
}

/// Absolute path of the on-disk codegraph cache for `root`.
pub fn disk_cache_path(root: &Path) -> PathBuf {
    super::canonical(root).join(DISK_CACHE_REL)
}

/// Absolute path of the BINARY (bincode+zstd) codegraph cache for `root`.
pub fn disk_cache_path_bin(root: &Path) -> PathBuf {
    super::canonical(root).join(DISK_CACHE_REL_BIN)
}

fn top_component_str<'a>(p: &'a Path, root: &Path) -> Option<&'a str> {
    p.strip_prefix(root)
        .ok()?
        .components()
        .next()?
        .as_os_str()
        .to_str()
}

fn top_component(p: &Path, root: &Path) -> Option<std::ffi::OsString> {
    top_component_str(p, root).map(std::ffi::OsString::from)
}

/// Fast resolver context pre-computing caller path properties (eliminates allocations per candidate).
struct ResolveContext<'a> {
    caller_file: &'a Path,
    caller_parent: Option<&'a Path>,
    caller_top: Option<&'a str>,
    root: &'a Path,
}

impl<'a> ResolveContext<'a> {
    fn new(caller_file: &'a Path, root: &'a Path) -> Self {
        Self {
            caller_file,
            caller_parent: caller_file.parent(),
            caller_top: top_component_str(caller_file, root),
            root,
        }
    }

    #[inline(always)]
    fn score(&self, n: &SymbolNode) -> i32 {
        if n.file == self.caller_file {
            4
        } else if self.caller_parent.is_some() && n.file.parent() == self.caller_parent {
            2
        } else if self.caller_top.is_some()
            && top_component_str(&n.file, self.root) == self.caller_top
        {
            1
        } else {
            0
        }
    }
}

/// Resolve a callee name to a symbol id, preferring closer candidates (production
/// scoring): same file (4) > same dir (2) > same top-level component (1) > any (0).
/// (Import-based score 3 is omitted — like production, we do not parse imports yet.)
/// Ties are broken DETERMINISTICALLY by the smallest (file, start_line) — production's
/// tie-break depends on HashMap iteration order, which is not reproducible.
fn resolve_callee(
    g: &CodeGraph,
    callee: &str,
    caller_file: &Path,
    root: &Path,
) -> Option<SymbolId> {
    let ctx = ResolveContext::new(caller_file, root);
    resolve_callee_with_ctx(g, callee, &ctx)
}

#[inline(always)]
fn resolve_callee_with_ctx(g: &CodeGraph, callee: &str, ctx: &ResolveContext) -> Option<SymbolId> {
    let candidates = g.find_by_name(callee);
    if candidates.is_empty() {
        return None;
    }
    // Fast path: unique symbol globally.
    if candidates.len() == 1 {
        return Some(candidates[0].id);
    }
    // Optimization for super-hot common names (>64 candidates like toString, get, save):
    // Prioritize same-file or same-directory candidates first without scanning full 1000+ candidates.
    if candidates.len() > 64 {
        for n in &candidates {
            if n.file == ctx.caller_file {
                return Some(n.id);
            }
        }
        if ctx.caller_parent.is_some() {
            for n in &candidates {
                if n.file.parent() == ctx.caller_parent {
                    return Some(n.id);
                }
            }
        }
    }

    let mut best: Option<&SymbolNode> = None;
    let mut best_score = i32::MIN;
    for n in candidates {
        let s = ctx.score(n);
        let better = match best {
            None => true,
            Some(b) => {
                s > best_score
                    || (s == best_score
                        && (n.file.as_path(), n.start_line) < (b.file.as_path(), b.start_line))
            }
        };
        if better {
            best = Some(n);
            best_score = s;
            if s == 4 {
                break; // Score 4 is maximum possible score (same-file), early exit!
            }
        }
    }
    best.map(|n| n.id)
}

/// Atomic index unit for one source file: parsed symbols + unresolved call sites.
/// A content change (mtime/len) replaces this whole unit; other units are untouched.
#[derive(Clone, Serialize, Deserialize)]
pub struct FileUnit {
    pub mtime_ns: u128,
    pub len: u64,
    pub nodes: Vec<SymbolNode>,
    pub calls: Vec<RawCall>,
}

/// Persisted workspace index written by `atomcode init` / after in-process builds.
#[derive(Serialize, Deserialize)]
pub struct DiskCache {
    pub version: u32,
    /// Canonical root path string (informational; validity is walk_fp).
    pub root: String,
    pub walk_fp: u64,
    /// path string → unit (PathBuf keys serialize poorly across OS separators).
    pub units: HashMap<String, FileUnit>,
    pub graph: CodeGraph,
}

/// Load the binary cache (`units.v4.bin`): bincode-decode + zstd-decompress.
fn load_disk_cache_bin(root: &Path) -> Option<DiskCache> {
    let path = disk_cache_path_bin(root);
    let bytes = std::fs::read(&path).ok()?;
    let decompressed = zstd::stream::decode_all(&bytes[..]).ok()?;
    let mut cache: DiskCache = bincode::deserialize(&decompressed).ok()?;
    if cache.version != DISK_CACHE_VERSION {
        return None;
    }
    cache.graph.rebuild_name_index();
    Some(cache)
}

fn units_from_disk(cache: DiskCache) -> HashMap<PathBuf, FileUnit> {
    cache
        .units
        .into_iter()
        .map(|(p, u)| (normalize_index_path(&PathBuf::from(p)), u))
        .collect()
}

/// Re-key an in-memory unit map so lookups never miss due to slash / drive
/// letter drift (the classic "every file looks dirty → reparse 1297 files").
fn rekey_units(units: &mut HashMap<PathBuf, FileUnit>) {
    let old = std::mem::take(units);
    units.reserve(old.len());
    for (p, u) in old {
        units.insert(normalize_index_path(&p), u);
    }
}

/// Write the binary cache (`units.v4.bin`): zstd-compress + bincode-encode.
fn save_disk_cache_bin(root: &Path, cache: &DiskCache) -> std::io::Result<PathBuf> {
    let path = disk_cache_path_bin(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = bincode::serialize(cache)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let compressed = zstd::stream::encode_all(&bytes[..], 3)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, compressed)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn load_disk_cache(root: &Path) -> Option<DiskCache> {
    // 1. Prefer SQLite-backed cache (`index.v1.db`) — fast incremental row-level upserts.
    //    Units are sufficient to recompose a graph; the graph blob is optional.
    let db_path = super::index_db::disk_cache_path_db(root);
    if db_path.is_file() {
        if let Ok(db) = super::index_db::IndexDb::open_shared(root) {
            let units_map = db.load_units();
            if !units_map.is_empty() {
                let mut unit_str_map = HashMap::with_capacity(units_map.len());
                for (p, u) in units_map {
                    unit_str_map.insert(normalize_index_path(&p).to_string_lossy().into_owned(), u);
                }
                let graph = db.load_graph().unwrap_or_else(CodeGraph::new);
                let walk_fp = db.get_walk_fp().unwrap_or(0);
                return Some(DiskCache {
                    version: DISK_CACHE_VERSION,
                    root: root.display().to_string(),
                    walk_fp,
                    units: unit_str_map,
                    graph,
                });
            }
        }
    }

    // 2. Binary cache (`units.v4.bin`)
    if let Some(bin) = load_disk_cache_bin(root) {
        return Some(bin);
    }

    // 3. Fall back to the legacy JSON cache (old workspaces / downgrade path).
    let path = disk_cache_path(root);
    let bytes = std::fs::read(&path).ok()?;
    let mut cache: DiskCache = serde_json::from_slice(&bytes).ok()?;
    if cache.version != DISK_CACHE_VERSION {
        return None;
    }
    cache.graph.rebuild_name_index();
    Some(cache)
}

/// Persist only the dirty file-unit rows. Never serializes the whole graph or
/// rewrites the giant JSON/bin snapshots — those belong on first build / init.
///
/// Writes in [`UNIT_WRITE_CHUNK`]-sized transactions so the WAL cannot grow to
/// the size of the whole corpus (the 15k-file one-shot commit).
fn persist_units_incremental(root: &Path, upsert: &[(PathBuf, FileUnit)], deleted: &[PathBuf]) {
    if upsert.is_empty() && deleted.is_empty() {
        return;
    }
    let Ok(db) = super::index_db::IndexDb::open_shared(root) else {
        return;
    };
    if upsert.is_empty() {
        let _ = db.upsert_units_prepared(&[], deleted);
        return;
    }
    for (i, chunk) in upsert.chunks(UNIT_WRITE_CHUNK).enumerate() {
        let prepared: Vec<super::index_db::PreparedUnitWrite> = chunk
            .par_iter()
            .filter_map(|(p, u)| super::index_db::PreparedUnitWrite::from_unit(p.clone(), u))
            .collect();
        let dels = if i == 0 { deleted } else { &[] as &[PathBuf] };
        let _ = db.upsert_units_prepared(&prepared, dels);
    }
}

/// Persist `paths` looked up from `units` (no `FileUnit` clone) in chunks.
fn persist_paths(
    root: &Path,
    units: &HashMap<PathBuf, FileUnit>,
    paths: &[PathBuf],
    deleted: &[PathBuf],
) {
    if paths.is_empty() && deleted.is_empty() {
        return;
    }
    let Ok(db) = super::index_db::IndexDb::open_shared(root) else {
        return;
    };
    if paths.is_empty() {
        let _ = db.upsert_units_prepared(&[], deleted);
        return;
    }
    for (i, chunk) in paths.chunks(UNIT_WRITE_CHUNK).enumerate() {
        let prepared: Vec<super::index_db::PreparedUnitWrite> = chunk
            .par_iter()
            .filter_map(|p| {
                let u = units.get(p)?;
                super::index_db::PreparedUnitWrite::from_unit(p.clone(), u)
            })
            .collect();
        let dels = if i == 0 { deleted } else { &[] as &[PathBuf] };
        let _ = db.upsert_units_prepared(&prepared, dels);
    }
}

/// First-build persist of every unit, chunked. Does **not** serialize the graph
/// blob — caller writes that separately after compose, once unit buffers are gone.
fn persist_units_chunked(root: &Path, units: &HashMap<PathBuf, FileUnit>, deleted: &[PathBuf]) {
    if units.is_empty() && deleted.is_empty() {
        return;
    }
    let Ok(db) = super::index_db::IndexDb::open_shared(root) else {
        return;
    };
    if !deleted.is_empty() {
        let _ = db.upsert_units_prepared(&[], deleted);
    }
    let mut batch: Vec<(&PathBuf, &FileUnit)> = Vec::with_capacity(UNIT_WRITE_CHUNK);
    for (p, u) in units {
        batch.push((p, u));
        if batch.len() >= UNIT_WRITE_CHUNK {
            let prepared: Vec<super::index_db::PreparedUnitWrite> = batch
                .par_iter()
                .filter_map(|(p, u)| super::index_db::PreparedUnitWrite::from_unit((*p).clone(), u))
                .collect();
            let _ = db.upsert_units_prepared(&prepared, &[]);
            batch.clear();
        }
    }
    if !batch.is_empty() {
        let prepared: Vec<super::index_db::PreparedUnitWrite> = batch
            .par_iter()
            .filter_map(|(p, u)| super::index_db::PreparedUnitWrite::from_unit((*p).clone(), u))
            .collect();
        let _ = db.upsert_units_prepared(&prepared, &[]);
    }
}

/// Remove leftover JSON/bin sidecars from the pre-SQLite layout. Serializing
/// the whole graph to `units.v3.json` (hundreds of MB) is what made a 1-file
/// edit take tens of seconds.
const META_DIRINDEX: &str = "dirindex.v1";
const META_IDF_STATS: &str = "idf_stats.v1";

fn persist_derived_sidecars(
    root: &Path,
    dirindex: Option<&super::retrieval::DirIndex>,
    stats: Option<&super::retrieval::IdfStats>,
) {
    let Ok(db) = super::index_db::IndexDb::open_shared(root) else {
        return;
    };
    if let Some(di) = dirindex {
        if let Ok(bytes) = serde_json::to_vec(di) {
            let _ = db.put_meta_blob(META_DIRINDEX, &bytes);
        }
    }
    if let Some(st) = stats {
        if let Ok(bytes) = serde_json::to_vec(st) {
            let _ = db.put_meta_blob(META_IDF_STATS, &bytes);
        }
    }
}

fn load_idf_stats_sqlite(root: &Path) -> Option<super::retrieval::IdfStats> {
    let db = super::index_db::IndexDb::open_shared(root).ok()?;
    let bytes = db.get_meta_blob(META_IDF_STATS)?;
    serde_json::from_slice(&bytes).ok()
}

fn cleanup_legacy_sidecars(root: &Path) {
    let root = super::canonical(root);
    for rel in [
        DISK_CACHE_REL,
        DISK_CACHE_REL_BIN,
        super::retrieval::DIRINDEX_REL,
        super::retrieval::stats::STATS_REL,
        ".atomcode/codegraph/units.v3.json.tmp",
        ".atomcode/codegraph/units.v4.bin.tmp",
    ] {
        let _ = std::fs::remove_file(root.join(rel));
    }
}

fn save_disk_cache(
    root: &Path,
    walk_fp: u64,
    units: &HashMap<PathBuf, FileUnit>,
    _graph: &CodeGraph,
    changed_units: &[(PathBuf, FileUnit)],
    deleted_paths: &[PathBuf],
) -> std::io::Result<PathBuf> {
    let db_path = super::index_db::disk_cache_path_db(root);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // SQLite is the only on-disk store. Units are the source of truth; the
    // graph is recomposed in memory on cold start. No JSON, no bin snapshot.
    // Never compress the whole corpus + graph in one shot — that WAL+bincode
    // spike is what OOM-killed large `atomcode init` runs.
    let incremental = !changed_units.is_empty() || !deleted_paths.is_empty();
    if incremental {
        persist_units_incremental(root, changed_units, deleted_paths);
    } else {
        persist_units_chunked(root, units, &[]);
        if let Ok(db) = super::index_db::IndexDb::open_shared(root) {
            let _ = db.save_graph_only(walk_fp, _graph);
            let _ = db.set_walk_fp(walk_fp);
        }
    }
    cleanup_legacy_sidecars(root);
    Ok(db_path)
}

pub fn init_workspace_index(
    root: &Path,
    force: bool,
    on_progress: &dyn Fn(&str),
) -> Result<IndexReport, String> {
    let root = super::canonical(root);
    let t0 = Instant::now();
    let cache_path = super::index_db::disk_cache_path_db(&root);

    if force {
        on_progress(&format!(
            "Code graph: --force, removing {}",
            path_for_display(&cache_path)
        ));
        super::index_db::IndexDb::drop_shared(&root);
        let _ = std::fs::remove_file(&cache_path);
        let _ = std::fs::remove_file(cache_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(cache_path.with_extension("db-shm"));
        cleanup_legacy_sidecars(&root);
    }

    let idx = CodeIndex::new();
    let _log_guard = super::index_log::ToolCallGuard::enter(
        "atomcode_init",
        serde_json::json!({ "force": force, "root": path_for_display(&root) }),
    );
    // Explicit init must parse every dirty file. The query-time 128 cap is
    // what turned `atomcode init --force` into a 127-file stub index.
    let _g = idx.reconcile_workspace(&root, None, on_progress, ReparseBudget::Unlimited);

    let mut guard = idx.inner.lock().map_err(|e| e.to_string())?;
    // Reconcile has already saved the updated database and updated dirindex / idf_stats internally.
    let changed = guard
        .last_stats
        .as_ref()
        .map(|s| s.reparsed > 0 || s.removed > 0)
        .unwrap_or(true);
    if changed {
        let (new_dirindex, new_idf_stats) = if let Some(g) = guard.graph.as_ref() {
            let di = if guard.dirindex.is_none() {
                let dirindex = super::retrieval::DirIndex::build(g);
                persist_derived_sidecars(&root, Some(&dirindex), None);
                Some(Arc::new(dirindex))
            } else {
                None
            };
            let idf = if guard.idf_stats.is_none() {
                let stats = super::retrieval::IdfStats::build(g);
                persist_derived_sidecars(&root, None, Some(&stats));
                Some(Arc::new(stats))
            } else {
                None
            };
            (di, idf)
        } else {
            (None, None)
        };
        if let Some(di) = new_dirindex {
            guard.dirindex = Some(di);
        }
        if let Some(idf) = new_idf_stats {
            guard.idf_stats = Some(idf);
        }
    } else {
        on_progress("Code graph: unchanged, cache kept");
    }
    let stats = guard.last_stats.clone().unwrap_or(RefreshStats {
        reparsed: 0,
        removed: 0,
        kept: guard.units.len(),
        cache_hit: false,
        ..Default::default()
    });
    Ok(IndexReport {
        root: root.clone(),
        cache_path,
        files: guard.units.len(),
        symbols: guard.graph.as_ref().map(|g| g.node_count()).unwrap_or(0),
        reparsed: stats.reparsed,
        removed: stats.removed,
        kept: stats.kept,
        elapsed: t0.elapsed(),
        cache_hit: stats.cache_hit,
        phases: stats.phases.clone(),
    })
}

#[derive(Debug, Clone, Default)]
pub struct RefreshStats {
    pub reparsed: usize,
    pub removed: usize,
    pub kept: usize,
    pub cache_hit: bool,
    pub reparsed_files: Vec<PathBuf>,
    pub removed_files: Vec<PathBuf>,
    pub phases: IndexPhaseTimings,
}

fn parse_unit(w: &Walked) -> Option<FileUnit> {
    // Always produce a unit, even when the file has no extractable symbols
    // (empty, header-only, generic json/xml, …). Returning None used to drop
    // the path from `units`, so the next query rediscovered the same 64 files
    // forever (MAX_DISCOVER_NEW_ON_QUERY). An empty unit is a tombstone:
    // mtime/len stay, discover treats it as known.
    let (nodes, calls) = match std::fs::read_to_string(&w.path) {
        Ok(source) => {
            let source = super::strip_utf8_bom(&source);
            parse_file(&w.path, source).unwrap_or((Vec::new(), Vec::new()))
        }
        Err(_) => (Vec::new(), Vec::new()),
    };
    Some(FileUnit {
        mtime_ns: w.mtime_ns,
        len: w.len,
        nodes,
        calls,
    })
}

/// Compose a cross-file graph from per-file units (symbols first, then call resolve).
/// Call resolution is global (names may resolve into other files) but cheap vs parse.
///
/// **Moves** `nodes` and `calls` out of `units` so the in-memory working set stays
/// ~1× (graph) instead of ~2× (units + cloned graph). After this returns, `units`
/// keep only mtime/len fingerprints — SQLite already holds the full rows.
fn compose_graph(
    root: &Path,
    units: &mut HashMap<PathBuf, FileUnit>,
    on_progress: &dyn Fn(&str),
) -> CodeGraph {
    let t_start = Instant::now();
    let mut g = CodeGraph::new();
    let mut raw_calls_by_file: Vec<(PathBuf, Vec<RawCall>)> = Vec::with_capacity(units.len());
    let mut total_calls = 0;
    for (path, unit) in units.iter_mut() {
        for n in std::mem::take(&mut unit.nodes) {
            g.add_symbol(n);
        }
        unit.nodes.shrink_to_fit();
        g.file_mtimes
            .insert(path.clone(), (unit.mtime_ns / 1_000_000_000) as u64);
        if !unit.calls.is_empty() {
            total_calls += unit.calls.len();
            raw_calls_by_file.push((path.clone(), std::mem::take(&mut unit.calls)));
            unit.calls.shrink_to_fit();
        }
    }
    let t_syms = t_start.elapsed();
    on_progress(&format!(
        "Code graph: resolving {} call sites across {} symbols ({} files) in parallel (symbols loaded in {:?})...",
        total_calls,
        g.node_count(),
        units.len(),
        t_syms
    ));

    let t_resolve = Instant::now();
    let resolved_edges: Vec<(SymbolId, Edge)> = raw_calls_by_file
        .into_par_iter()
        .flat_map(|(caller_file, calls)| {
            let ctx = ResolveContext::new(&caller_file, root);
            let mut edges = Vec::with_capacity(calls.len());
            for rc in &calls {
                let caller = CodeGraph::make_id(&caller_file, &rc.caller_name, rc.caller_line);
                if g.node(caller).is_none() {
                    continue;
                }
                if let Some(callee) = resolve_callee_with_ctx(&g, &rc.callee_name, &ctx) {
                    edges.push((
                        caller,
                        Edge {
                            to: callee,
                            kind: EdgeKind::Calls,
                            line: rc.line,
                        },
                    ));
                }
            }
            edges
        })
        .collect();

    let resolve_dur = t_resolve.elapsed();
    let edge_count = resolved_edges.len();
    for (caller, edge) in resolved_edges {
        g.add_edge(caller, edge);
    }
    on_progress(&format!(
        "Code graph: resolved {} call edges in {:?} (total graph composition: {:?})",
        edge_count,
        resolve_dur,
        t_start.elapsed()
    ));
    g
}

/// Patch a live graph for one file unit: drop the old symbols/edges for that
/// file, insert the new ones, resolve only this file's call sites. Avoids
/// recomposing every call edge in the workspace ("branch index rebuild").
fn patch_graph_with_unit(g: &mut CodeGraph, path: &Path, unit: &FileUnit, root: &Path) {
    let path = normalize_index_path(path);
    g.remove_file(&path);
    for n in &unit.nodes {
        g.add_symbol(n.clone());
    }
    g.file_mtimes
        .insert(path.clone(), (unit.mtime_ns / 1_000_000_000) as u64);
    for rc in &unit.calls {
        let caller = CodeGraph::make_id(&path, &rc.caller_name, rc.caller_line);
        if g.node(caller).is_none() {
            continue;
        }
        if let Some(callee) = resolve_callee(g, &rc.callee_name, &path, root) {
            g.add_edge(
                caller,
                Edge {
                    to: callee,
                    kind: EdgeKind::Calls,
                    line: rc.line,
                },
            );
        }
    }
}

fn path_in_focus(path: &Path, focus: Option<&Path>) -> bool {
    let Some(focus) = focus else {
        return true;
    };
    path_matches_scope(&normalize_index_path(path), &normalize_index_path(focus))
}

fn restat_one(path: &Path, unit: &FileUnit, focus: Option<&Path>) -> Result<Walked, PathBuf> {
    let norm = normalize_index_path(path);
    if !path_in_focus(&norm, focus) {
        return Ok(Walked {
            path: norm,
            mtime_ns: unit.mtime_ns,
            len: unit.len,
        });
    }
    match std::fs::metadata(&norm) {
        Ok(md) => {
            let len = md.len();
            if len > MAX_INDEX_FILE_BYTES {
                return Err(norm);
            }
            let mtime_ns = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            Ok(Walked {
                path: norm,
                mtime_ns,
                len,
            })
        }
        Err(_) => Err(norm),
    }
}

/// Re-stat already-known unit paths. Avoids `ignore::WalkBuilder` over the
/// whole workspace (the 10s+ "Index" cost) when we only need to know which
/// of the files we already indexed changed.
///
/// When `focus` is set, only files under that directory are re-stated against
/// disk. Everything else is trusted from the stored fingerprint so a scoped
/// `code_explore(path: coupon-mall-demo)` does not restat 1万+ unrelated files.
/// Parallel `metadata()` — Windows NTFS serial restat of 1万+ files is seconds.
fn restat_known_units(
    units: &HashMap<PathBuf, FileUnit>,
    focus: Option<&Path>,
) -> (Vec<Walked>, Vec<PathBuf>) {
    let mut results: Vec<Result<Walked, PathBuf>> = units
        .par_iter()
        .map(|(path, unit)| restat_one(path, unit, focus))
        .collect();
    let mut walked = Vec::with_capacity(results.len());
    let mut missing = Vec::new();
    for r in results.drain(..) {
        match r {
            Ok(w) => walked.push(w),
            Err(p) => missing.push(p),
        }
    }
    walked.sort_by(|a, b| a.path.cmp(&b.path));
    (walked, missing)
}

fn walked_from_disk(path: &Path) -> Option<Walked> {
    let norm = normalize_index_path(path);
    if is_generated_source(&norm) {
        return None;
    }
    let ext_ok = norm
        .extension()
        .and_then(|e| e.to_str())
        .map(is_indexed_ext)
        .unwrap_or(false);
    if !ext_ok {
        return None;
    }
    let md = std::fs::metadata(&norm).ok()?;
    if !md.is_file() {
        return None;
    }
    let len = md.len();
    if len > MAX_INDEX_FILE_BYTES || len == 0 {
        return None;
    }
    let mtime_ns = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some(Walked {
        path: norm,
        mtime_ns,
        len,
    })
}

fn path_under_skip_dir(p: &Path) -> bool {
    p.components().any(|c| should_skip_dir(c.as_os_str()))
}

/// Git roots we already index: the workspace itself, its immediate children,
/// and the nearest `.git` ancestor of known unit paths.
#[allow(dead_code)] // reserved for explicit `atomcode init` full refresh
fn discover_git_roots(workspace: &Path, known: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if workspace.join(".git").exists() {
        roots.push(workspace.to_path_buf());
    }
    if let Ok(rd) = std::fs::read_dir(workspace) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && p.join(".git").exists() {
                roots.push(normalize_index_path(&p));
            }
        }
    }
    for file in known.iter().take(256) {
        let mut cur = file.parent();
        while let Some(dir) = cur {
            if dir.join(".git").exists() {
                let n = normalize_index_path(dir);
                if !roots.iter().any(|r| r == &n) {
                    roots.push(n);
                }
                break;
            }
            if dir == workspace {
                break;
            }
            cur = dir.parent();
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

#[allow(dead_code)] // reserved for explicit `atomcode init` full refresh
fn git_ls_indexable(git_root: &Path) -> Vec<PathBuf> {
    let mut cmd = std::process::Command::new("git");
    cmd.args([
        "-C",
        &git_root.to_string_lossy(),
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ]);
    crate::process_utils::suppress_console_window_sync(&mut cmd);
    let Ok(out) = cmd.output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut files = Vec::new();
    for rel in out.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let rel = match std::str::from_utf8(rel) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let abs = normalize_index_path(&git_root.join(rel));
        if path_under_skip_dir(&abs) {
            continue;
        }
        files.push(abs);
    }
    files
}

/// Hard cap on how many brand-new files a query-time discover will ingest.
/// A workspace-wide `git ls-files` / `collect_files` of sibling projects used
/// to dump 2万+ "new" files onto every cold start (224s). Query path only
/// looks at **direct children of already-indexed directories**.
const MAX_DISCOVER_NEW_ON_QUERY: usize = 64;

/// Discover **new** source files (create) next to files we already index.
/// Delete/modify of already-indexed files are handled by [`restat_known_units`].
///
/// Intentionally shallow: one `read_dir` of each known parent (and optional
/// `focus`). No `WalkBuilder`, no `git ls-files` — those walk the whole
/// monorepo and turn a SQLite hit into a full rebuild.
fn discover_new_files(
    root: &Path,
    units: &HashMap<PathBuf, FileUnit>,
    focus: Option<&Path>,
) -> Vec<Walked> {
    let known: HashSet<PathBuf> = units.keys().map(|p| normalize_index_path(p)).collect();
    let mut new_files = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    let mut push_new = |p: PathBuf| {
        if new_files.len() >= MAX_DISCOVER_NEW_ON_QUERY {
            return;
        }
        let n = normalize_index_path(&p);
        if known.contains(&n) || !seen.insert(n.clone()) {
            return;
        }
        if !path_in_focus(&n, focus) {
            return;
        }
        if let Some(w) = walked_from_disk(&n) {
            new_files.push(w);
        }
    };

    let mut indexed_dirs: HashSet<PathBuf> = HashSet::new();
    for p in &known {
        if !path_in_focus(p, focus) {
            continue;
        }
        if let Some(dir) = p.parent() {
            indexed_dirs.insert(dir.to_path_buf());
        }
    }
    if let Some(focus) = focus {
        let f = normalize_index_path(focus);
        if f.is_dir() {
            indexed_dirs.insert(f.clone());
        }
        if let Some(parent) = f.parent() {
            indexed_dirs.insert(parent.to_path_buf());
        }
    } else {
        indexed_dirs.insert(normalize_index_path(root));
    }

    for dir in &indexed_dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                push_new(p);
                continue;
            }
            if !p.is_dir() || should_skip_dir(p.file_name().unwrap_or_default()) {
                continue;
            }
            let pn = normalize_index_path(&p);
            if known.iter().any(|k| k.starts_with(&pn)) {
                continue;
            }
            // New folder next to indexed sources. Ingest only if it is a
            // small package (e.g. `pkg/new.rs`). A large unindexed sibling
            // project (20k files) is skipped — that was the 224s cold start.
            let found = collect_files(&pn);
            if found.len() > 16 {
                continue;
            }
            for w in found {
                push_new(w.path);
            }
        }
    }

    // A scoped query (`path: coupon-mall-demo`) may point at a dir we have
    // not fully indexed — walk just that subtree. Skip the WalkBuilder when
    // the focus already has indexed files: that path is what turned every
    // `code_explore(path: atomcode)` into an 8s Index miss.
    if let Some(focus) = focus {
        let f = normalize_index_path(focus);
        let already_indexed = known.iter().any(|k| path_in_focus(k, Some(&f)));
        if !already_indexed && f.is_dir() {
            for w in collect_files(&f) {
                push_new(w.path);
            }
        }
    }

    new_files
}

/// Rank dirty files so a scoped `code_explore(path: …)` still ingests that
/// subtree when the query-time cap fires. New-in-focus first, then dirty-in-focus,
/// then everything else (original walk order as the tie-break).
fn take_reparse_budget<'a>(
    dirty: Vec<&'a Walked>,
    units: &HashMap<PathBuf, FileUnit>,
    budget: ReparseBudget,
    focus: Option<&Path>,
    on_progress: &dyn Fn(&str),
) -> Vec<&'a Walked> {
    let ReparseBudget::Query = budget else {
        return dirty;
    };
    if dirty.len() <= MAX_REPARSE_PER_QUERY {
        return dirty;
    }
    let dirty_found = dirty.len();

    let mut ranked: Vec<(u8, usize)> = dirty
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let norm = normalize_index_path(&w.path);
            let focused = path_in_focus(&norm, focus);
            let cached = units.contains_key(&norm);
            let rank: u8 = match (focused, cached) {
                (true, false) => 0,
                (true, true) => 1,
                (false, false) => 2,
                (false, true) => 3,
            };
            (rank, i)
        })
        .collect();
    ranked.sort_by_key(|(rank, i)| (*rank, *i));

    let mut selected = Vec::with_capacity(MAX_REPARSE_PER_QUERY);
    let mut kept_idx = vec![false; dirty.len()];
    for (_, i) in ranked.into_iter().take(MAX_REPARSE_PER_QUERY) {
        kept_idx[i] = true;
        selected.push(dirty[i]);
    }

    let mut deferred_new = 0usize;
    let mut deferred_cached = 0usize;
    for (i, w) in dirty.iter().enumerate() {
        if kept_idx[i] {
            continue;
        }
        if units.contains_key(&normalize_index_path(&w.path)) {
            deferred_cached += 1;
        } else {
            deferred_new += 1;
        }
    }
    let detail = match (deferred_new, deferred_cached) {
        (0, c) => format!("{c} stay on cached units"),
        (n, 0) => format!("{n} new files not indexed this pass"),
        (n, c) => format!("{n} new files not indexed this pass, {c} stay on cached units"),
    };
    on_progress(&format!(
        "Code graph: {dirty_found} dirty files; reparsing {MAX_REPARSE_PER_QUERY} this query \
         ({detail}; run `atomcode init` to finish)."
    ));
    selected
}

/// Same formula as sibling `codegraph` `resolveParsePoolSize`:
/// `clamp(max(3, available_parallelism()) - 1, 1, 8)`.
fn parse_parallelism() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    resolve_parse_pool_size(cores)
}

/// Pure pool-size helper — same as codegraph `resolveParsePoolSize(unset, n)`
/// after the caller does `Math.max(3, availableParallelism())`.
fn resolve_parse_pool_size(cpu_count: usize) -> usize {
    cpu_count
        .max(3)
        .saturating_sub(1)
        .clamp(1, MAX_PARSE_THREADS)
}

/// Result of [`sync_units`]. `changed_paths` are keys in `units` (no cloned
/// `FileUnit`s). `units_flushed` means every changed row is already in SQLite.
struct UnitSync {
    reparsed: usize,
    removed: usize,
    kept: usize,
    changed_paths: Vec<PathBuf>,
    deleted_paths: Vec<PathBuf>,
    units_flushed: bool,
}

/// Diff `walked` against `units`: re-parse dirty/new, drop deleted.
/// Dirty files are parsed in bounded batches on a dedicated Rayon pool
/// (not the process-global pool) so tree-sitter TLS dies with the pool.
fn sync_units(
    units: &mut HashMap<PathBuf, FileUnit>,
    walked: &[Walked],
    on_progress: &dyn Fn(&str),
    budget: ReparseBudget,
    focus: Option<&Path>,
    root: &Path,
) -> UnitSync {
    rekey_units(units);

    let walked_paths: std::collections::HashSet<PathBuf> = walked
        .iter()
        .map(|w| normalize_index_path(&w.path))
        .collect();

    let mut deleted_paths = Vec::new();
    let before = units.len();
    units.retain(|p, _| {
        let keep = walked_paths.contains(p);
        if !keep {
            deleted_paths.push(p.clone());
        }
        keep
    });
    let removed = before - units.len();

    let mut dirty: Vec<&Walked> = Vec::new();
    let mut kept = 0usize;
    for w in walked {
        let norm_p = normalize_index_path(&w.path);
        match units.get(&norm_p) {
            Some(u) if u.mtime_ns == w.mtime_ns && u.len == w.len => kept += 1,
            _ => dirty.push(w),
        }
    }
    if dirty.is_empty() {
        return UnitSync {
            reparsed: 0,
            removed,
            kept,
            changed_paths: Vec::new(),
            deleted_paths,
            units_flushed: false,
        };
    }

    let dirty_found = dirty.len();
    let dirty = take_reparse_budget(dirty, units, budget, focus, on_progress);
    let dirty_total = dirty.len();
    let deferred = dirty_found.saturating_sub(dirty_total);

    let threads = parse_parallelism();
    on_progress(&format!(
        "Code graph: re-parsing {dirty_total} changed/new file(s) ({kept} unchanged{}) with {threads} threads in batches of {PARSE_BATCH_FILES}...",
        if deferred > 0 {
            format!(", {deferred} deferred")
        } else {
            String::new()
        }
    ));

    // Stream to SQLite once the dirty set is large enough that holding a
    // second copy (changed_units clone + one-shot WAL) would OOM.
    let stream_persist = dirty_total >= PARSE_BATCH_FILES;
    let mut reparsed = 0usize;
    let mut changed_paths = Vec::with_capacity(dirty_total);

    // Dedicated pool so per-thread tree-sitter Parser/Query TLS dies with
    // the pool — before compose / graph serialize — instead of living on
    // the process-global Rayon workers for the rest of the CLI run.
    {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("codegraph-parse-{i}"))
            .build()
            .ok();

        let parse_chunk = |chunk: &[&Walked]| -> Vec<(PathBuf, Option<FileUnit>)> {
            let job = || {
                chunk
                    .par_iter()
                    .map(|w| {
                        let norm_p = normalize_index_path(&w.path);
                        (norm_p, parse_unit(w))
                    })
                    .collect()
            };
            match pool.as_ref() {
                Some(p) => p.install(job),
                None => job(),
            }
        };

        for chunk in dirty.chunks(PARSE_BATCH_FILES) {
            let parsed = parse_chunk(chunk);
            let flush_from = changed_paths.len();
            for (path, unit) in parsed {
                reparsed += 1;
                match unit {
                    Some(u) => {
                        changed_paths.push(path.clone());
                        units.insert(path, u);
                    }
                    None => {
                        units.remove(&path);
                        deleted_paths.push(path);
                    }
                }
            }
            if stream_persist {
                persist_paths(root, units, &changed_paths[flush_from..], &[]);
            }
            on_progress(&format!(
                "Code graph: re-parsed {reparsed}/{dirty_total} dirty files..."
            ));
        }
    }

    on_progress(&format!(
        "Code graph: finished parse of {dirty_total} dirty files ({threads} threads)."
    ));

    UnitSync {
        reparsed,
        removed,
        kept,
        changed_paths,
        deleted_paths,
        units_flushed: stream_persist,
    }
}

/// Build a fresh code graph for `root` (walk → parse → resolve). O(repo), CPU-bound.
pub fn build_graph(root: &Path) -> CodeGraph {
    let root = super::canonical(root);
    let files = collect_files(&root);
    let mut units = HashMap::new();
    for w in &files {
        if let Some(u) = parse_unit(w) {
            units.insert(normalize_index_path(&w.path), u);
        }
    }
    compose_graph(&root, &mut units, &|_| {})
}

/// Shared, lazily-built **incremental** code index the graph tools hold.
///
/// - Per-file [`FileUnit`]s: only dirty files are re-parsed.
/// - Graph is recomposed from units after unit-level changes (full call resolve is
///   still global; tree-sitter work is not).
/// - Concurrent callers **single-flight** one refresh.
/// - Optional background poller keeps the last workspace warm after registration.
pub struct CodeIndex {
    inner: Mutex<IndexState>,
    cv: Condvar,
    /// Background refresher already spawned for this index instance.
    watcher_started: AtomicBool,
}

struct IndexState {
    /// Last workspace root this index was built for (canonical).
    root: Option<PathBuf>,
    /// Walk fingerprint of the last successful sync (path+mtime+len of all files).
    walk_fp: u64,
    /// Per-file parse units — the incremental cache.
    units: HashMap<PathBuf, FileUnit>,
    /// Composed query graph (derived from `units`).
    graph: Option<Arc<CodeGraph>>,
    /// Directory aggregate index (derived from `graph`), persisted as a
    /// sidecar `dirindex.v1.json` next to `units.v3.json`. `None` = not yet
    /// built / sidecar missing → tools fall back to a live graph walk.
    dirindex: Option<Arc<super::retrieval::DirIndex>>,
    /// Corpus statistics for BM25 (IDF / avgdl), persisted as
    /// `stats.v1.json` and reused across sessions. `None` = not yet built →
    /// the query path builds once and caches here.
    idf_stats: Option<Arc<super::retrieval::IdfStats>>,
    /// Per-symbol concept vectors (symbol name → concept projection), lazily
    /// built and cached so every query / session sharing this `CodeIndex`
    /// reuses them instead of re-projecting every symbol on every query.
    concept_vectors: Option<Arc<std::collections::HashMap<SymbolId, Vec<f32>>>>,
    building: bool,
    /// Stats from the most recent refresh (for `atomcode init` reporting).
    last_stats: Option<RefreshStats>,
    /// Last instant an edit or incremental update occurred (for background debouncing).
    last_update_instant: Option<Instant>,
    /// True after `update_single_file` patched memory. The next `get()` must
    /// return that graph without walking the workspace — otherwise every agent
    /// edit is followed by a 10s+ "incremental" reindex of every file.
    fast_patch_pending: bool,
}

impl Default for IndexState {
    fn default() -> Self {
        Self {
            root: None,
            walk_fp: 0,
            units: HashMap::new(),
            graph: None,
            dirindex: None,
            idf_stats: None,
            concept_vectors: None,
            building: false,
            last_stats: None,
            last_update_instant: None,
            fast_patch_pending: false,
        }
    }
}

impl Default for CodeIndex {
    fn default() -> Self {
        Self {
            inner: Mutex::new(IndexState::default()),
            cv: Condvar::new(),
            watcher_started: AtomicBool::new(false),
        }
    }
}

impl CodeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a cheap background poller that incrementally refreshes the last-used
    /// workspace so `git pull` / editor saves land without waiting for the next tool.
    /// Debounced when active agent edits or quick-updates have recently occurred.
    /// Safe to call multiple times; only the first starts the thread.
    pub fn start_background_refresh(self: &Arc<Self>) {
        // In the event-driven architecture, file writes directly trigger fast-path
        // in-memory updates via `update_single_file` with SQLite WAL write-through.
        // Periodic background disk-scanning polling is intentionally disabled to eliminate
        // lock contention and CPU/IO jitter.
    }

    /// Notify the code index that a file was modified, triggering a fast
    /// in-memory unit parse and local graph/vector patch in 1-3ms without
    /// full directory tree scanning or global recomposition.
    pub fn update_single_file(&self, path: &Path, content: Option<&str>) -> bool {
        let norm_path = normalize_index_path(path);
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };

        // Never invent a workspace root from the file's parent — that made the
        // next `code_explore` (which uses the real workspace root) look like a
        // different project and throw away the whole in-memory index.
        let Some(root) = guard.root.clone() else {
            return false;
        };

        let ext_ok = norm_path
            .extension()
            .and_then(|e| e.to_str())
            .map(is_indexed_ext)
            .unwrap_or(false);
        if !ext_ok || is_generated_source(&norm_path) {
            return false;
        }

        let (source, mtime_ns, len) = match content {
            Some(text) => {
                let md = std::fs::metadata(&norm_path).ok();
                let mtime_ns = md
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or_else(|| {
                        std::time::SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_nanos())
                            .unwrap_or(0)
                    });
                // Use on-disk byte length (not UTF-8 char len) so the next
                // restat/`collect_files` comparison does not mark this file dirty.
                let disk_len = md.as_ref().map(|m| m.len()).unwrap_or(text.len() as u64);
                (text.to_string(), mtime_ns, disk_len)
            }
            None => {
                let Ok(md) = std::fs::metadata(&norm_path) else {
                    guard.units.remove(&norm_path);
                    if let Some(graph) = guard.graph.as_mut() {
                        let g_mut = Arc::make_mut(graph);
                        g_mut.remove_file(&norm_path);
                    }
                    guard.last_stats = Some(RefreshStats {
                        reparsed: 0,
                        removed: 1,
                        kept: guard.units.len(),
                        cache_hit: false,
                        removed_files: vec![norm_path.clone()],
                        ..Default::default()
                    });
                    guard.last_update_instant = Some(Instant::now());
                    guard.fast_patch_pending = true;
                    drop(guard);
                    persist_units_incremental(&root, &[], &[norm_path]);
                    return true;
                };
                let mtime_ns = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let Ok(text) = std::fs::read_to_string(&norm_path) else {
                    return false;
                };
                let l = md.len();
                (text, mtime_ns, l)
            }
        };

        if len > MAX_INDEX_FILE_BYTES {
            return false;
        }

        let source = super::strip_utf8_bom(&source);
        let Some((nodes, calls)) = parse_file(&norm_path, source) else {
            guard.units.remove(&norm_path);
            if let Some(graph) = guard.graph.as_mut() {
                let g_mut = Arc::make_mut(graph);
                g_mut.remove_file(&norm_path);
            }
            guard.fast_patch_pending = true;
            drop(guard);
            persist_units_incremental(&root, &[], &[norm_path]);
            return true;
        };

        let unit = FileUnit {
            mtime_ns,
            len,
            nodes,
            calls,
        };

        if let Some(graph) = guard.graph.as_mut() {
            let g = Arc::make_mut(graph);
            patch_graph_with_unit(g, &norm_path, &unit, &root);
        }

        // Fast concept vectors patch for modified symbols — do NOT drop the
        // whole map (rebuilding it is the 50s retrieval stall).
        if let Some(concept_map) = guard.concept_vectors.as_mut() {
            let map = Arc::make_mut(concept_map);
            for node in &unit.nodes {
                map.insert(
                    node.id,
                    super::retrieval::concept_projection(&node.name, &HashSet::new()),
                );
            }
        }

        guard.units.insert(norm_path.clone(), unit.clone());
        guard.last_stats = Some(RefreshStats {
            reparsed: 1,
            removed: 0,
            kept: guard.units.len().saturating_sub(1),
            cache_hit: false,
            reparsed_files: vec![norm_path.clone()],
            ..Default::default()
        });
        guard.last_update_instant = Some(Instant::now());
        guard.fast_patch_pending = true;
        drop(guard);

        persist_units_incremental(&root, &[(norm_path, unit)], &[]);
        true
    }

    /// Mark that an edit occurred on `path`, recording the debounce timestamp.
    pub fn notify_file_changed(&self, path: &Path) {
        let _ = self.update_single_file(path, None);
    }

    /// Cached graph lookup with no progress callbacks (tests / internal).
    pub fn get(&self, root: &Path) -> Arc<CodeGraph> {
        self.reconcile_workspace(root, None, &|_| {}, ReparseBudget::Query)
    }

    /// Like [`get`], but restat/discover only under `focus` (a `code_explore`
    /// `path:` scope). SQLite + the rest of the workspace stay untouched.
    pub fn get_scoped(&self, root: &Path, focus: Option<&Path>) -> Arc<CodeGraph> {
        self.reconcile_workspace(root, focus, &|_| {}, ReparseBudget::Query)
    }

    /// Current graph fingerprint (walk fingerprint) for `root`, or `None` if
    /// no graph is loaded. Callers can key result caches on this.
    pub fn fingerprint(&self, root: &Path) -> Option<u64> {
        let root = super::canonical(root);
        let guard = self.inner.lock().unwrap();
        if guard.root.as_ref() == Some(&root) {
            Some(guard.walk_fp)
        } else {
            None
        }
    }

    /// Last refresh statistics for `root` (cache hit / reparsed count), if available.
    pub fn last_stats(&self, root: &Path) -> Option<RefreshStats> {
        let root = super::canonical(root);
        let guard = self.inner.lock().unwrap();
        if guard.root.as_ref() == Some(&root) {
            guard.last_stats.clone()
        } else {
            None
        }
    }

    /// Directory aggregate index for `root` (lazily built if the sidecar is
    /// missing). Returns `None` if no graph is loaded for `root`.
    pub fn get_dirindex(&self, root: &Path) -> Option<Arc<super::retrieval::DirIndex>> {
        let root = super::canonical(root);
        let guard = self.inner.lock().unwrap();
        if guard.root.as_ref() == Some(&root) {
            guard.dirindex.clone()
        } else {
            None
        }
    }

    /// Corpus statistics for BM25 (`IdfStats`) for `root`. Lazily loads the
    /// `stats.v1.json` sidecar; if missing/stale it builds from the graph once
    /// and caches in memory so every subsequent query (any session sharing this
    /// `CodeIndex`) reuses it instead of re-scanning all symbols.
    pub fn get_idf_stats(&self, root: &Path) -> Option<Arc<super::retrieval::IdfStats>> {
        let root = super::canonical(root);
        let mut guard = self.inner.lock().unwrap();
        if guard.root.as_ref() != Some(&root) {
            return None;
        }
        if let Some(stats) = guard.idf_stats.clone() {
            return Some(stats);
        }
        let Some(g) = guard.graph.clone() else {
            return None;
        };
        let stats = Arc::new(match load_idf_stats_sqlite(&root) {
            Some(loaded) if loaded.total_symbols as usize == g.node_count() => loaded,
            _ => {
                let t_idf = Instant::now();
                let built = super::retrieval::IdfStats::build(&g);
                persist_derived_sidecars(&root, None, Some(&built));
                super::index_log::log_derived_rebuild(
                    &root,
                    "idf_stats",
                    t_idf.elapsed(),
                    serde_json::json!({ "symbols": g.node_count() }),
                );
                built
            }
        });
        guard.idf_stats = Some(stats.clone());
        Some(stats)
    }

    /// Per-symbol concept vectors for `root`, lazily built once and cached in
    /// memory. Every query / session sharing this `CodeIndex` reuses them
    /// instead of re-projecting every symbol (34万级) on every query.
    /// Returns `None` if no graph is loaded for `root`.
    pub fn get_concept_vectors(
        &self,
        root: &Path,
    ) -> Option<Arc<std::collections::HashMap<SymbolId, Vec<f32>>>> {
        let root = super::canonical(root);
        let mut guard = self.inner.lock().unwrap();
        if guard.root.as_ref() != Some(&root) {
            return None;
        }
        if let Some(vectors) = guard.concept_vectors.clone() {
            return Some(vectors);
        }
        let Some(g) = guard.graph.clone() else {
            return None;
        };
        let t_cv = Instant::now();
        let mut map = std::collections::HashMap::with_capacity(g.nodes.len());
        for node in g.nodes.values() {
            map.insert(
                node.id,
                super::retrieval::concept_projection(&node.name, &HashSet::new()),
            );
        }
        let n = map.len();
        let vectors = Arc::new(map);
        guard.concept_vectors = Some(vectors.clone());
        super::index_log::log_derived_rebuild(
            &root,
            "concept_vectors",
            t_cv.elapsed(),
            serde_json::json!({ "symbols": n }),
        );
        Some(vectors)
    }

    /// Ensure the index matches `root` on disk. Unchanged files keep their units;
    /// only dirty/new files are re-parsed, then the graph is recomposed if needed.
    ///
    /// Cold start path: if memory is empty, try loading
    /// [`.atomcode/codegraph/units.v1.json`](DISK_CACHE_REL) written by `atomcode init`.
    pub fn get_with_progress(&self, root: &Path, on_progress: &dyn Fn(&str)) -> Arc<CodeGraph> {
        self.reconcile_workspace(root, None, on_progress, ReparseBudget::Query)
    }

    /// Reconcile workspace state with disk.
    ///
    /// Hot path after an agent edit: trust the in-memory patch (`fast_patch_pending`)
    /// and return immediately — no `WalkBuilder`, no graph recompose, no sidecar
    /// rebuild. Warm path: re-stat only already-known files (optionally only
    /// under `focus`). Cold path: load SQLite; full walk only when no db exists.
    ///
    /// `budget` is [`ReparseBudget::Query`] for tool calls (128-file cap) and
    /// [`ReparseBudget::Unlimited`] for explicit `atomcode init` / `--force`.
    pub(crate) fn reconcile_workspace(
        &self,
        root: &Path,
        focus: Option<&Path>,
        on_progress: &dyn Fn(&str),
        budget: ReparseBudget,
    ) -> Arc<CodeGraph> {
        let root = super::canonical(root);
        let started = Instant::now();

        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        loop {
            let same_root = guard.root.as_ref() == Some(&root);

            // 1. Agent just patched this file — serve memory, do not walk.
            if same_root && guard.graph.is_some() && guard.fast_patch_pending {
                guard.fast_patch_pending = false;
                if guard.last_stats.is_none() {
                    guard.last_stats = Some(RefreshStats {
                        reparsed: 1,
                        removed: 0,
                        kept: guard.units.len().saturating_sub(1),
                        cache_hit: false,
                        ..Default::default()
                    });
                }
                return guard.graph.clone().unwrap();
            }

            // 2. Warm memory: re-stat known files only (no ignore-walk of the tree).
            if same_root && guard.graph.is_some() && !guard.units.is_empty() {
                if guard.building {
                    on_progress(
                        "Code graph: waiting for in-flight workspace index (shared with other tools)...",
                    );
                    guard = self.cv.wait(guard).unwrap();
                    continue;
                }
                let snapshot: Vec<(PathBuf, u128, u64)> = guard
                    .units
                    .iter()
                    .map(|(p, u)| (p.clone(), u.mtime_ns, u.len))
                    .collect();
                let warm_graph = guard.graph.clone().unwrap();
                let kept_hint = guard.units.len();
                drop(guard);

                let (dirty, missing) = snapshot
                    .par_iter()
                    .filter(|(path, _, _)| path_in_focus(path, focus))
                    .map(|(path, mtime_ns, len)| match std::fs::metadata(path) {
                        Ok(md) => {
                            let disk_mtime = md
                                .modified()
                                .ok()
                                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                                .map(|d| d.as_nanos())
                                .unwrap_or(0);
                            if disk_mtime != *mtime_ns || md.len() != *len {
                                (1usize, 0usize)
                            } else {
                                (0, 0)
                            }
                        }
                        Err(_) => (0, 1),
                    })
                    .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));

                if dirty == 0 && missing == 0 {
                    // Known files are clean. Still look for *created* files
                    // (any source: editor, copy, pull, …) without a full tree walk.
                    let known_only: HashMap<PathBuf, FileUnit> = snapshot
                        .iter()
                        .map(|(p, m, l)| {
                            (
                                p.clone(),
                                FileUnit {
                                    mtime_ns: *m,
                                    len: *l,
                                    nodes: Vec::new(),
                                    calls: Vec::new(),
                                },
                            )
                        })
                        .collect();
                    let newcomers = discover_new_files(&root, &known_only, focus);
                    if newcomers.is_empty() {
                        let mut guard = match self.inner.lock() {
                            Ok(g) => g,
                            Err(p) => p.into_inner(),
                        };
                        if guard.root.as_ref() == Some(&root) && guard.graph.is_some() {
                            if guard.fast_patch_pending {
                                guard.fast_patch_pending = false;
                                return guard.graph.clone().unwrap();
                            }
                            guard.last_stats = Some(RefreshStats {
                                reparsed: 0,
                                removed: 0,
                                kept: kept_hint,
                                cache_hit: true,
                                ..Default::default()
                            });
                            guard.last_update_instant = Some(Instant::now());
                            if guard.dirindex.is_none() {
                                let di = Arc::new(super::retrieval::DirIndex::build(&warm_graph));
                                persist_derived_sidecars(&root, Some(di.as_ref()), None);
                                guard.dirindex = Some(di);
                            }
                            return guard.graph.clone().unwrap_or(warm_graph);
                        }
                        drop(guard);
                    } else {
                        return self.reconcile_known_files(&root, focus, on_progress, budget);
                    }
                } else {
                    return self.reconcile_known_files(&root, focus, on_progress, budget);
                }

                // Re-lock and continue into the cold path if the warm graph vanished.
                guard = match self.inner.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                continue;
            }

            if guard.building {
                on_progress(
                    "Code graph: waiting for in-flight workspace index (shared with other tools)...",
                );
                guard = self.cv.wait(guard).unwrap();
                continue;
            }
            guard.building = true;
            let mut units = if same_root {
                std::mem::take(&mut guard.units)
            } else {
                guard.units.clear();
                HashMap::new()
            };
            let prev_graph = if same_root { guard.graph.clone() } else { None };
            drop(guard);

            // Cold start: load SQLite FIRST. A full WalkBuilder over a 1万+ file
            // workspace is 80s+ on Windows and is what made every process restart
            // look like a guaranteed rebuild even when the db already existed.
            let mut prev_graph = prev_graph;
            if units.is_empty() {
                if let Some(disk) = load_disk_cache(&root) {
                    let disk_fp = disk.walk_fp;
                    let disk_graph_ok = disk.graph.node_count() > 0;
                    let disk_graph = disk.graph;
                    let mut loaded = disk
                        .units
                        .into_iter()
                        .map(|(p, u)| (normalize_index_path(&PathBuf::from(p)), u))
                        .collect::<HashMap<PathBuf, FileUnit>>();
                    if !loaded.is_empty() {
                        let n_files = loaded.len();
                        let (mut walked_known, missing) = restat_known_units(&loaded, focus);
                        let focused_dirty = walked_known
                            .iter()
                            .filter(|w| {
                                path_in_focus(&w.path, focus)
                                    && loaded.get(&w.path).map_or(true, |u| {
                                        u.mtime_ns != w.mtime_ns || u.len != w.len
                                    })
                            })
                            .count()
                            + missing.iter().filter(|p| path_in_focus(p, focus)).count();
                        let newcomers = discover_new_files(&root, &loaded, focus);
                        let g = if disk_graph_ok {
                            on_progress(&format!(
                                "Code graph: loaded SQLite snapshot ({n_files} files, {} symbols) — skipped tree walk.",
                                disk_graph.node_count()
                            ));
                            // Graph already holds the symbols. Drop node/call
                            // payloads so a 15k-file restart is not 2× RSS.
                            for u in loaded.values_mut() {
                                u.nodes.clear();
                                u.nodes.shrink_to_fit();
                                u.calls.clear();
                                u.calls.shrink_to_fit();
                            }
                            Arc::new(disk_graph)
                        } else {
                            on_progress(&format!(
                                "Code graph: composing {n_files} units from SQLite (one-time, no tree walk)...",
                            ));
                            let composed = Arc::new(compose_graph(&root, &mut loaded, on_progress));
                            if let Ok(db) = super::index_db::IndexDb::open_shared(&root) {
                                let _ = db.save_graph_only(disk_fp, composed.as_ref());
                            }
                            composed
                        };
                        if newcomers.is_empty() && focused_dirty == 0 {
                            let mut guard = match self.inner.lock() {
                                Ok(g) => g,
                                Err(p) => p.into_inner(),
                            };
                            guard.root = Some(root.clone());
                            guard.walk_fp = disk_fp;
                            guard.units = loaded;
                            guard.graph = Some(g.clone());
                            guard.fast_patch_pending = false;
                            guard.last_stats = Some(RefreshStats {
                                reparsed: 0,
                                removed: 0,
                                kept: n_files,
                                cache_hit: true,
                                ..Default::default()
                            });
                            guard.building = false;
                            self.cv.notify_all();
                            return g;
                        }
                        on_progress(&format!(
                            "Code graph: incremental after SQLite load ({} dirty, {} new).",
                            focused_dirty,
                            newcomers.len()
                        ));
                        walked_known.extend(newcomers);
                        let mut units = loaded;
                        for p in &missing {
                            units.remove(p);
                        }
                        let sync = sync_units(
                            &mut units,
                            &walked_known,
                            on_progress,
                            budget,
                            focus,
                            &root,
                        );
                        return self.finish_reconcile(
                            &root,
                            disk_fp,
                            units,
                            Some(g),
                            sync,
                            walked_known.len(),
                            on_progress,
                            started,
                        );
                    }
                    prev_graph = if disk_graph_ok {
                        Some(Arc::new(disk_graph))
                    } else {
                        None
                    };
                    units = loaded;
                }
            }

            // No on-disk index at all — first-ever build for this workspace.
            on_progress("Code graph: no SQLite index, walking workspace...");
            let files = collect_files(&root);
            let fp = fingerprint(&files);

            let sync = sync_units(&mut units, &files, on_progress, budget, focus, &root);
            let g = self.finish_reconcile(
                &root,
                fp,
                units,
                prev_graph,
                sync,
                files.len(),
                on_progress,
                started,
            );
            return g;
        }
    }

    /// Incremental reconcile against already-known unit paths (no tree walk).
    fn reconcile_known_files(
        &self,
        root: &Path,
        focus: Option<&Path>,
        on_progress: &dyn Fn(&str),
        budget: ReparseBudget,
    ) -> Arc<CodeGraph> {
        let started = Instant::now();
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if guard.building {
            guard = self.cv.wait(guard).unwrap();
            if guard.root.as_deref() == Some(root) {
                if let Some(g) = guard.graph.clone() {
                    return g;
                }
            }
        }
        if guard.root.as_deref() != Some(root) {
            guard.building = false;
            self.cv.notify_all();
            drop(guard);
            return self.reconcile_workspace(root, focus, on_progress, budget);
        }
        guard.building = true;
        let mut units = std::mem::take(&mut guard.units);
        let prev_graph = guard.graph.clone();
        drop(guard);

        rekey_units(&mut units);
        let (mut walked, missing) = restat_known_units(&units, focus);
        for p in &missing {
            units.remove(p);
        }
        let newcomers = discover_new_files(root, &units, focus);
        if !newcomers.is_empty() {
            on_progress(&format!(
                "Code graph: discovered {} new sibling file(s).",
                newcomers.len()
            ));
            walked.extend(newcomers);
        }
        let mut sync = sync_units(&mut units, &walked, on_progress, budget, focus, root);
        sync.removed += missing.len();
        sync.deleted_paths.extend(missing);
        if sync.reparsed == 0 && sync.removed == 0 {
            sync.changed_paths.clear();
        }
        let fp = fingerprint(&walked);
        self.finish_reconcile(
            root,
            fp,
            units,
            prev_graph,
            sync,
            walked.len(),
            on_progress,
            started,
        )
    }

    fn finish_reconcile(
        &self,
        root: &Path,
        fp: u64,
        mut units: HashMap<PathBuf, FileUnit>,
        prev_graph: Option<Arc<CodeGraph>>,
        sync: UnitSync,
        walked_len: usize,
        on_progress: &dyn Fn(&str),
        started: Instant,
    ) -> Arc<CodeGraph> {
        let UnitSync {
            reparsed,
            removed,
            kept,
            changed_paths,
            deleted_paths,
            units_flushed,
        } = sync;
        // If we already have a live graph, NEVER recompose all call edges.
        // Reparsing 2223 files used to still rebuild 38万 symbols — that is why
        // a "partial" increment cost the same as a full index.
        let can_patch = prev_graph.is_some() && (reparsed > 0 || removed > 0);
        let need_compose = prev_graph.is_none();
        let cache_hit = reparsed == 0 && removed == 0 && prev_graph.is_some();

        // Persist FULL units BEFORE compose moves nodes out of them. Streaming
        // parse already flushed large dirty sets; this covers the small-set
        // path and any leftover deletes.
        let mut save_dur = Duration::ZERO;
        if (reparsed > 0 || removed > 0) && !units_flushed {
            let t_save = Instant::now();
            if kept == 0 {
                persist_units_chunked(root, &units, &deleted_paths);
            } else {
                persist_paths(root, &units, &changed_paths, &deleted_paths);
            }
            save_dur += t_save.elapsed();
        } else if (reparsed > 0 || removed > 0) && units_flushed && !deleted_paths.is_empty() {
            persist_paths(root, &units, &[], &deleted_paths);
        }

        let t_compose = Instant::now();
        let mut compose_dur = Duration::ZERO;
        let g = if can_patch {
            on_progress(&format!(
                "Code graph: incremental patch - reparsed {reparsed}, removed {removed}, kept {kept}."
            ));
            let mut g = prev_graph.expect("can_patch => graph").as_ref().clone();
            for p in &deleted_paths {
                g.remove_file(p);
            }
            for path in &changed_paths {
                if let Some(unit) = units.get(path) {
                    patch_graph_with_unit(&mut g, path, unit, root);
                }
            }
            compose_dur = t_compose.elapsed();
            Arc::new(g)
        } else if need_compose {
            if kept == 0 && reparsed > 0 {
                if reparsed >= walked_len {
                    on_progress(&format!(
                        "Code graph: full index of {walked_len} files (first build or workspace switch)..."
                    ));
                } else {
                    on_progress(&format!(
                        "Code graph: partial first build — parsed {reparsed} of {walked_len} files \
                         (run `atomcode init` to finish)."
                    ));
                }
            } else if reparsed > 0 || removed > 0 {
                on_progress(&format!(
                    "Code graph: unit update - reparsed {reparsed}, removed {removed}, kept {kept}."
                ));
            }
            let res = Arc::new(compose_graph(root, &mut units, on_progress));
            compose_dur = t_compose.elapsed();
            res
        } else {
            prev_graph.expect("need_compose false => graph present")
        };

        // Units are already on disk. The graph blob is a cold-start shortcut
        // written only after a full compose — never on a 1-file incremental
        // patch (serializing the whole CodeGraph is the multi-second stall).
        if reparsed > 0 || removed > 0 {
            on_progress(&format!(
                "Code graph: ready ({} symbols, {} files; reparsed {}, removed {}, kept {}).",
                g.node_count(),
                units.len(),
                reparsed,
                removed,
                kept
            ));
            let t_save = Instant::now();
            if let Ok(db) = super::index_db::IndexDb::open_shared(root) {
                if need_compose {
                    let _ = db.save_graph_only(fp, g.as_ref());
                }
                let _ = db.set_walk_fp(fp);
            }
            cleanup_legacy_sidecars(root);
            save_dur += t_save.elapsed();
            on_progress(&format!(
                "Code graph: saved {} in {:?}",
                path_for_display(&super::index_db::disk_cache_path_db(root)),
                save_dur
            ));
        } else if need_compose {
            if let Ok(db) = super::index_db::IndexDb::open_shared(root) {
                let t_save = Instant::now();
                let _ = db.save_graph_only(fp, g.as_ref());
                save_dur = t_save.elapsed();
                on_progress(&format!(
                    "Code graph: saved graph snapshot in {:?}",
                    save_dur
                ));
            }
        }

        let reparsed_files = changed_paths;
        // Time spent from `started` up to graph composition is the parse & scan phase
        let parse_dur = started.elapsed().saturating_sub(compose_dur);
        let stats = RefreshStats {
            reparsed,
            removed,
            kept,
            cache_hit,
            reparsed_files: reparsed_files.clone(),
            removed_files: deleted_paths.clone(),
            phases: IndexPhaseTimings {
                parse_ast: parse_dur,
                compose_graph: compose_dur,
                save_disk: save_dur,
            },
        };
        let kind = if cache_hit {
            "hit"
        } else if kept == 0 {
            "full"
        } else if can_patch {
            "incremental"
        } else {
            "compose"
        };
        super::index_log::log_index_refresh(
            root,
            cache_hit,
            kind,
            started.elapsed(),
            reparsed,
            removed,
            kept,
            &reparsed_files,
            &deleted_paths,
            serde_json::json!({
                "symbols": g.node_count(),
                "files": units.len(),
                "walked": walked_len,
                "derived_invalidated": need_compose && !can_patch,
            }),
        );

        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.root = Some(root.to_path_buf());
        guard.walk_fp = fp;
        guard.units = units;
        guard.graph = Some(g.clone());
        guard.fast_patch_pending = false;
        if need_compose && !can_patch {
            let t_di = Instant::now();
            guard.dirindex = Some(Arc::new(super::retrieval::DirIndex::build(&g)));
            super::index_log::log_derived_rebuild(
                root,
                "dirindex",
                t_di.elapsed(),
                serde_json::json!({ "symbols": g.node_count() }),
            );
            guard.idf_stats = None;
            guard.concept_vectors = None;
        }
        guard.last_stats = Some(stats);
        guard.building = false;
        self.cv.notify_all();
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_cross_file_call_edges() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.rs"),
            "fn helper() {}\nfn main() {\n    helper();\n}\n",
        )
        .unwrap();
        let g = build_graph(d.path());
        let main = g.find_by_name("main").into_iter().next().expect("main");
        let helper = g.find_by_name("helper").into_iter().next().expect("helper");
        // main → helper edge exists
        let callees = g.callees(main.id).expect("callees");
        assert!(
            callees.iter().any(|e| e.to == helper.id),
            "main should call helper"
        );
        // reverse: helper has main as caller
        assert!(g
            .callers(helper.id)
            .unwrap()
            .iter()
            .any(|e| e.to == main.id));
    }

    #[test]
    fn resolves_calls_across_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("util.rs"), "pub fn compute() -> i32 { 42 }\n").unwrap();
        std::fs::write(
            d.path().join("main.rs"),
            "fn run() {\n    let _ = compute();\n}\n",
        )
        .unwrap();
        let g = build_graph(d.path());
        let run = g.find_by_name("run").into_iter().next().expect("run");
        let compute = g
            .find_by_name("compute")
            .into_iter()
            .next()
            .expect("compute");
        assert!(
            g.callees(run.id)
                .unwrap()
                .iter()
                .any(|e| e.to == compute.id),
            "run → compute across files"
        );
    }

    #[test]
    fn sfc_vue_dual_parse_extracts_script_and_template() {
        let d = tempfile::tempdir().unwrap();
        let src = r#"<template>
  <div class="coupon-panel">
    <el-button @click="claimCoupon">领取优惠券</el-button>
    <el-dialog title="优惠券详情"></el-dialog>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue'
const loading = ref(false)
function claimCoupon() {
  loading.value = true
}
</script>

<style scoped>
.coupon-panel { color: red; }
</style>
"#;
        std::fs::write(d.path().join("CouponPanel.vue"), src).unwrap();
        let g = build_graph(d.path());

        // Script block: function + variable symbols are indexed.
        let claim = g
            .find_by_name("claimCoupon")
            .into_iter()
            .next()
            .expect("claimCoupon must be extracted from <script>");
        assert_eq!(claim.kind, SymbolKind::Function);

        // Template block: element tags are indexed as UiElement symbols —
        // the exact "按钮/标题/UI 节点" recall the user asked for.
        let button = g
            .nodes
            .values()
            .find(|n| n.name == "el-button")
            .expect("el-button must be extracted from <template>");
        assert_eq!(button.kind, SymbolKind::UiElement);
        assert!(
            g.nodes.values().any(|n| n.name == "el-dialog"),
            "el-dialog must also be extracted"
        );
        // No double-count from re-parse: exactly one claimCoupon.
        assert_eq!(g.find_by_name("claimCoupon").len(), 1);
    }

    #[test]
    fn tsx_jsx_elements_are_extracted_as_uielements() {
        let d = tempfile::tempdir().unwrap();
        let src = r#"import { Button, Dialog } from 'ui'
export function CouponPanel() {
  return (
    <div className="coupon-panel">
      <el-button onClick={claimCoupon}>领取优惠券</el-button>
      <CouponDialog visible={show} />
      <span>优惠券标题</span>
    </div>
  )
}
"#;
        std::fs::write(d.path().join("CouponPanel.tsx"), src).unwrap();
        let g = build_graph(d.path());

        // Function still extracted normally.
        assert!(g.find_by_name("CouponPanel").len() >= 1, "CouponPanel fn");

        // JSX elements (pair + self-closing) extracted as UiElement.
        let names: Vec<String> = g
            .nodes
            .values()
            .filter(|n| n.kind == SymbolKind::UiElement)
            .map(|n| n.name.clone())
            .collect();
        assert!(
            names.contains(&"el-button".to_string()),
            "el-button: {names:?}"
        );
        assert!(
            names.contains(&"CouponDialog".to_string()),
            "CouponDialog: {names:?}"
        );
        assert!(names.contains(&"span".to_string()), "span: {names:?}");
    }

    #[test]
    fn css_scss_selectors_are_extracted_as_uielements() {
        let d = tempfile::tempdir().unwrap();
        let src = r#".coupon-panel {
  color: red;
}
.coupon-panel .btn, .coupon-btn:hover {
  padding: 4px;
}
#coupon-app { margin: 0; }
@keyframes fadeIn { from { opacity: 0; } }
@media (max-width: 600px) { .mobile-only { display: none; } }
"#;
        std::fs::write(d.path().join("coupon.scss"), src).unwrap();
        let g = build_graph(d.path());

        let names: Vec<String> = g
            .nodes
            .values()
            .filter(|n| n.kind == SymbolKind::UiElement)
            .map(|n| n.name.clone())
            .collect();
        assert!(
            names.contains(&"coupon-panel".to_string()),
            "class: {names:?}"
        );
        assert!(
            names.contains(&"btn".to_string()),
            "comma-split class: {names:?}"
        );
        assert!(
            names.contains(&"coupon-btn".to_string()),
            "pseudo-stripped: {names:?}"
        );
        assert!(names.contains(&"#coupon-app".to_string()), "id: {names:?}");
        assert!(
            names.contains(&"fadeIn".to_string()),
            "keyframes: {names:?}"
        );
        assert!(
            names.contains(&"mobile-only".to_string()),
            "media-nested: {names:?}"
        );
    }

    #[test]
    fn restart_loads_sqlite_without_rewalk_or_rewrite() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "pub fn hello() {}\n").unwrap();
        let report = init_workspace_index(d.path(), false, &|_| {}).expect("init");
        assert!(report.cache_path.exists());
        let db_path = crate::codeintel::index_db::disk_cache_path_db(d.path());
        let before = std::fs::metadata(&db_path).unwrap();
        let before_mtime = before.modified().unwrap();
        let before_len = before.len();

        // New process: must load the existing SQLite index, not rebuild it.
        let idx = CodeIndex::new();
        let g = idx.get(d.path());
        assert!(g.find_by_name("hello").into_iter().next().is_some());
        let stats = idx.last_stats(d.path()).unwrap();
        assert!(
            stats.cache_hit && stats.reparsed == 0,
            "restart must be a SQLite cache hit, not a rebuild: {stats:?}"
        );

        let after = std::fs::metadata(&db_path).unwrap();
        assert_eq!(
            after.len(),
            before_len,
            "restart must not rewrite the SQLite db"
        );
        assert_eq!(
            after.modified().unwrap(),
            before_mtime,
            "restart must not touch the SQLite db mtime"
        );
    }

    #[test]
    fn new_file_create_is_ingested_without_git() {
        // Create (not just modify/delete) must be incremental even with no VCS.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("old.rs"), "pub fn old_sym() {}\n").unwrap();
        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        assert!(!g1.find_by_name("old_sym").is_empty());
        assert!(g1.find_by_name("brand_new").is_empty());

        std::fs::create_dir_all(d.path().join("pkg")).unwrap();
        std::fs::write(d.path().join("pkg/new.rs"), "pub fn brand_new() {}\n").unwrap();
        std::fs::write(d.path().join("sibling.rs"), "pub fn sibling() {}\n").unwrap();

        let g2 = idx.get(d.path());
        assert!(
            !g2.find_by_name("brand_new").is_empty(),
            "new file in new subdir"
        );
        assert!(!g2.find_by_name("sibling").is_empty(), "new sibling file");
        assert!(!g2.find_by_name("old_sym").is_empty());
        let stats = idx.last_stats(d.path()).unwrap();
        assert!(
            stats.reparsed <= 3,
            "create must be incremental, not a full rebuild: {stats:?}"
        );
    }

    #[test]
    fn git_new_file_is_ingested_incrementally() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        assert!(std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "init"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false));
        let _ = std::process::Command::new("git")
            .args([
                "-C",
                &root.to_string_lossy(),
                "config",
                "user.email",
                "t@t.t",
            ])
            .status();
        let _ = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "config", "user.name", "t"])
            .status();
        std::fs::write(root.join("old.rs"), "pub fn old_sym() {}\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C", &root.to_string_lossy(), "add", "old.rs"])
            .status();
        let _ = std::process::Command::new("git")
            .args([
                "-C",
                &root.to_string_lossy(),
                "commit",
                "-m",
                "init",
                "--no-gpg-sign",
            ])
            .status();

        let idx = CodeIndex::new();
        let g1 = idx.get(root);
        assert!(!g1.find_by_name("old_sym").is_empty());
        assert!(g1.find_by_name("new_sym").is_empty());

        std::fs::write(root.join("new.rs"), "pub fn new_sym() {}\n").unwrap();
        let g2 = idx.get(root);
        assert!(
            !g2.find_by_name("new_sym").is_empty(),
            "git-visible new file must be incrementally indexed"
        );
        assert!(!g2.find_by_name("old_sym").is_empty());
        let stats = idx.last_stats(root).unwrap();
        assert!(
            stats.reparsed <= 2,
            "must not rebuild the whole index for one new file: {stats:?}"
        );
    }

    #[test]
    fn sqlite_is_the_only_on_disk_store() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "pub fn alpha() {}\n").unwrap();
        let g = build_graph(d.path());
        assert!(!g.nodes.is_empty());

        let file = d.path().join("a.rs");
        let meta = std::fs::metadata(&file).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let unit = FileUnit {
            mtime_ns: mtime,
            len: meta.len(),
            nodes: g.nodes.values().cloned().collect(),
            calls: vec![],
        };
        let units = HashMap::from([(file.clone(), unit.clone())]);
        // Plant leftover sidecar junk — save must delete them.
        let json_path = disk_cache_path(d.path());
        let bin_path = disk_cache_path_bin(d.path());
        std::fs::create_dir_all(json_path.parent().unwrap()).unwrap();
        std::fs::write(&json_path, b"stale").unwrap();
        std::fs::write(&bin_path, b"stale").unwrap();

        let _ = save_disk_cache(d.path(), 1, &units, &g, &[], &[]);
        assert!(
            crate::codeintel::index_db::disk_cache_path_db(d.path()).is_file(),
            "SQLite db must be written"
        );
        assert!(!json_path.exists(), "units.v3.json must not be written");
        assert!(!bin_path.exists(), "units.v4.bin must not be written");

        let loaded = load_disk_cache(d.path()).expect("sqlite cache must load");
        assert_eq!(loaded.units.len(), 1);
    }

    #[test]
    fn self_calls_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("r.rs"),
            "fn recur(n: i32) {\n    if n > 0 { recur(n - 1); }\n}\n",
        )
        .unwrap();
        let g = build_graph(d.path());
        let recur = g.find_by_name("recur").into_iter().next().expect("recur");
        assert!(
            g.callees(recur.id).map(|e| e.is_empty()).unwrap_or(true),
            "self-call must be skipped"
        );
    }

    #[test]
    fn csharp_symbols_and_cross_file_calls_are_indexed() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("CouponService.cs"),
            r#"
namespace Shop.Services;

public class CouponService
{
    public decimal Apply(decimal price) => price * 0.9m;
    public string Code { get; set; }
}

public record CouponDto(string Code, decimal Off);
"#,
        )
        .unwrap();
        std::fs::write(
            d.path().join("OrderController.cs"),
            r#"
namespace Shop.Api;

public class OrderController
{
    private readonly CouponService _coupons = new CouponService();

    public decimal Checkout(decimal total)
    {
        return _coupons.Apply(total);
    }
}
"#,
        )
        .unwrap();

        let g = build_graph(d.path());
        assert!(
            g.find_by_name("CouponService").into_iter().next().is_some(),
            "class CouponService must be indexed"
        );
        assert!(
            g.find_by_name("Apply").into_iter().next().is_some(),
            "method Apply must be indexed"
        );
        assert!(
            g.find_by_name("CouponDto").into_iter().next().is_some(),
            "record CouponDto must be indexed"
        );
        assert!(
            g.find_by_name("Code").into_iter().next().is_some(),
            "property Code must be indexed"
        );

        let checkout = g
            .find_by_name("Checkout")
            .into_iter()
            .next()
            .expect("Checkout");
        let apply = g.find_by_name("Apply").into_iter().next().expect("Apply");
        assert!(
            g.callees(checkout.id)
                .unwrap_or(&vec![])
                .iter()
                .any(|e| e.to == apply.id),
            "OrderController.Checkout should call CouponService.Apply"
        );

        // blast-radius style: Apply has Checkout as a dependent caller file
        let apply_file = &apply.file;
        let deps = g.file_dependents(apply_file, 2);
        assert!(
            deps.iter()
                .any(|f| f.file_name().and_then(|n| n.to_str()) == Some("OrderController.cs")),
            "OrderController.cs must appear in blast radius of CouponService.cs: {deps:?}"
        );
    }

    #[test]
    fn same_second_edit_triggers_rebuild() {
        // Overwriting the SAME file (likely the same wall-clock second) must rebuild —
        // the fingerprint uses nanos + length, not coarse seconds.
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.rs");
        std::fs::write(&f, "fn one() {}\n").unwrap();
        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        assert!(g1.find_by_name("two").is_empty());
        std::fs::write(&f, "fn one() {}\nfn two() {}\n").unwrap();
        let g2 = idx.get(d.path());
        assert!(
            !g2.find_by_name("two").is_empty(),
            "same-second edit must rebuild (nanos/len changed)"
        );
    }

    #[test]
    fn tie_break_resolution_is_deterministic() {
        // Two same-named fns in the same dir → equal score for a same-dir caller → tie,
        // resolved deterministically to the smallest (file, line) = a_util.rs.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a_util.rs"), "pub fn dup() {}\n").unwrap();
        std::fs::write(d.path().join("z_util.rs"), "pub fn dup() {}\n").unwrap();
        std::fs::write(d.path().join("main.rs"), "fn run() { dup(); }\n").unwrap();
        let g = build_graph(d.path());
        let run = g.find_by_name("run").into_iter().next().unwrap();
        let target = g
            .callees(run.id)
            .and_then(|e| e.first())
            .and_then(|e| g.node(e.to))
            .map(|n| n.file.clone());
        assert!(
            target
                .as_ref()
                .map(|f| f.ends_with("a_util.rs"))
                .unwrap_or(false),
            "tie → a_util.rs, got {target:?}"
        );
        // stable across a rebuild
        let g2 = build_graph(d.path());
        let run2 = g2.find_by_name("run").into_iter().next().unwrap();
        let t2 = g2
            .callees(run2.id)
            .and_then(|e| e.first())
            .and_then(|e| g2.node(e.to))
            .map(|n| n.file.clone());
        assert_eq!(target, t2);
    }

    #[test]
    fn index_caches_then_rebuilds_on_change() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn one() {}\n").unwrap();
        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        let g2 = idx.get(d.path());
        assert!(
            Arc::ptr_eq(&g1, &g2),
            "unchanged repo → cached graph reused"
        );
        assert!(g1.find_by_name("two").is_empty());
        // New files arrive via the edit/write notify path (no full tree walk).
        let new_src = "fn two() {}\n";
        std::fs::write(d.path().join("b.rs"), new_src).unwrap();
        assert!(idx.update_single_file(&d.path().join("b.rs"), Some(new_src)));
        let g3 = idx.get(d.path());
        assert!(!Arc::ptr_eq(&g1, &g3), "changed repo → rebuilt");
        assert!(
            !g3.find_by_name("two").is_empty(),
            "rebuilt graph sees new symbol"
        );
    }

    #[test]
    fn skips_bin_obj_and_generated_csharp() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("bin")).unwrap();
        std::fs::create_dir_all(d.path().join("obj")).unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("bin/Junk.cs"), "class BinOnly {}\n").unwrap();
        std::fs::write(d.path().join("obj/Junk.cs"), "class ObjOnly {}\n").unwrap();
        std::fs::write(
            d.path().join("src/Form1.Designer.cs"),
            "partial class Form1 {}\n",
        )
        .unwrap();
        std::fs::write(d.path().join("src/Real.cs"), "class RealService {}\n").unwrap();
        let g = build_graph(d.path());
        assert!(
            g.find_by_name("RealService").into_iter().next().is_some(),
            "real source must be indexed"
        );
        assert!(g.find_by_name("BinOnly").is_empty(), "bin/ must be skipped");
        assert!(g.find_by_name("ObjOnly").is_empty(), "obj/ must be skipped");
        assert!(
            g.find_by_name("Form1").is_empty(),
            "Designer.cs must be skipped"
        );
    }

    #[test]
    fn concurrent_gets_single_flight_same_graph() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn shared() {}\n").unwrap();
        let idx = Arc::new(CodeIndex::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let idx = idx.clone();
            let root = d.path().to_path_buf();
            handles.push(std::thread::spawn(move || idx.get(&root)));
        }
        let graphs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for g in &graphs[1..] {
            assert!(
                Arc::ptr_eq(&graphs[0], g),
                "parallel cold gets must share one built graph"
            );
        }
        assert!(graphs[0]
            .find_by_name("shared")
            .into_iter()
            .next()
            .is_some());
    }

    #[test]
    fn unit_change_updates_only_that_file() {
        // Many stable files + one dirty file: after first index, editing one file must
        // surface new symbols without dropping the rest (unit-level reparse).
        let d = tempfile::tempdir().unwrap();
        for i in 0..20 {
            std::fs::write(
                d.path().join(format!("stable_{i}.rs")),
                format!("fn stable_{i}() {{}}\n"),
            )
            .unwrap();
        }
        let target = d.path().join("target.rs");
        std::fs::write(&target, "fn old_name() {}\n").unwrap();

        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        assert!(g1.find_by_name("old_name").into_iter().next().is_some());
        assert!(g1.find_by_name("stable_0").into_iter().next().is_some());
        assert!(g1.find_by_name("new_name").is_empty());
        let n1 = g1.node_count();

        std::fs::write(&target, "fn new_name() {}\n").unwrap();
        let g2 = idx.get(d.path());
        assert!(
            g2.find_by_name("new_name").into_iter().next().is_some(),
            "edited file unit must be reparsed"
        );
        assert!(
            g2.find_by_name("old_name").is_empty(),
            "old symbols from the replaced unit must be gone"
        );
        assert!(
            g2.find_by_name("stable_19").into_iter().next().is_some(),
            "unchanged units must remain"
        );
        // Symbol count: lost old_name, gained new_name → same count for this edit.
        assert_eq!(g2.node_count(), n1);

        // Unchanged second get → same Arc.
        let g3 = idx.get(d.path());
        assert!(Arc::ptr_eq(&g2, &g3), "no disk change → cached graph");
    }

    #[test]
    fn deleted_file_unit_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("keep.rs"), "fn keep() {}\n").unwrap();
        std::fs::write(d.path().join("gone.rs"), "fn gone() {}\n").unwrap();
        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        assert!(g1.find_by_name("gone").into_iter().next().is_some());
        std::fs::remove_file(d.path().join("gone.rs")).unwrap();
        let g2 = idx.get(d.path());
        assert!(g2.find_by_name("gone").is_empty());
        assert!(g2.find_by_name("keep").into_iter().next().is_some());
    }

    #[test]
    fn disk_cache_roundtrip_via_init() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let report = init_workspace_index(d.path(), false, &|_| {}).expect("init");
        assert!(report.cache_path.exists(), "cache file written");
        assert!(report.symbols >= 1);
        assert!(report.files >= 1);

        // New index instance (simulates new process) should load from disk.
        let idx = CodeIndex::new();
        let g = idx.get(d.path());
        assert!(
            g.find_by_name("hello").into_iter().next().is_some(),
            "loaded graph must contain hello"
        );
        let guard = idx.inner.lock().unwrap();
        assert!(
            guard
                .last_stats
                .as_ref()
                .map(|s| s.cache_hit)
                .unwrap_or(false),
            "second process should report disk cache hit"
        );
    }

    #[test]
    fn cross_file_edge_updates_when_callee_file_changes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("util.rs"), "pub fn compute() -> i32 { 1 }\n").unwrap();
        std::fs::write(
            d.path().join("main.rs"),
            "fn run() {\n    let _ = compute();\n}\n",
        )
        .unwrap();
        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        let run = g1.find_by_name("run").into_iter().next().unwrap();
        let compute = g1.find_by_name("compute").into_iter().next().unwrap();
        assert!(g1
            .callees(run.id)
            .unwrap()
            .iter()
            .any(|e| e.to == compute.id));

        // Rename callee in util only — main unit unchanged; edge must re-resolve.
        std::fs::write(
            d.path().join("util.rs"),
            "pub fn compute_v2() -> i32 { 2 }\n",
        )
        .unwrap();
        let g2 = idx.get(d.path());
        assert!(g2.find_by_name("compute").is_empty());
        assert!(g2.find_by_name("compute_v2").into_iter().next().is_some());
        let run2 = g2.find_by_name("run").into_iter().next().unwrap();
        // main still calls "compute" textually → no resolve target → no edge (or empty).
        let callees = g2.callees(run2.id).map(|e| e.len()).unwrap_or(0);
        assert_eq!(
            callees, 0,
            "stale name must not keep old edge after unit recompose"
        );
    }

    #[test]
    fn caller_attribution_is_per_file() {
        // Two files each define a function named `handler`, each calling a DISTINCT callee.
        // The old resolver picked the first same-named symbol as caller, so both edges hung
        // off ONE handler. Caller id must be reconstructed exactly, per file.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.rs"),
            "fn handler() {\n    alpha();\n}\nfn alpha() {}\n",
        )
        .unwrap();
        std::fs::write(
            d.path().join("b.rs"),
            "fn handler() {\n    beta();\n}\nfn beta() {}\n",
        )
        .unwrap();
        let g = build_graph(d.path());

        let a_handler = g
            .find_by_name("handler")
            .into_iter()
            .find(|n| n.file.ends_with("a.rs"))
            .expect("a.rs handler");
        let b_handler = g
            .find_by_name("handler")
            .into_iter()
            .find(|n| n.file.ends_with("b.rs"))
            .expect("b.rs handler");
        let alpha = g.find_by_name("alpha").into_iter().next().expect("alpha");
        let beta = g.find_by_name("beta").into_iter().next().expect("beta");

        let a_callees = g.callees(a_handler.id).cloned().unwrap_or_default();
        let b_callees = g.callees(b_handler.id).cloned().unwrap_or_default();

        assert!(
            a_callees.iter().any(|e| e.to == alpha.id),
            "a.rs::handler → alpha"
        );
        assert!(
            !a_callees.iter().any(|e| e.to == beta.id),
            "a.rs::handler must NOT call beta"
        );
        assert!(
            b_callees.iter().any(|e| e.to == beta.id),
            "b.rs::handler → beta"
        );
        assert!(
            !b_callees.iter().any(|e| e.to == alpha.id),
            "b.rs::handler must NOT call alpha"
        );
    }

    #[test]
    fn test_codegraphignore_skips_configured_patterns() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("main.rs"), "pub fn main() {}").unwrap();
        std::fs::create_dir_all(d.path().join("custom_dist")).unwrap();
        std::fs::write(
            d.path().join("custom_dist").join("dist_code.rs"),
            "pub fn dist() {}",
        )
        .unwrap();
        std::fs::write(d.path().join("test_generated.rs"), "pub fn gen() {}").unwrap();

        // Write .codegraphignore
        std::fs::write(
            d.path().join(".codegraphignore"),
            "custom_dist/\n*_generated.rs\n",
        )
        .unwrap();

        let g = build_graph(d.path());
        assert!(!g.find_by_name("main").is_empty(), "main should be indexed");
        assert!(
            g.find_by_name("dist").is_empty(),
            "custom_dist should be ignored"
        );
        assert!(
            g.find_by_name("gen").is_empty(),
            "test_generated.rs should be ignored"
        );
    }

    #[test]
    fn test_yaml_and_json_plugin_config_indexing() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("cordis.patch.yml"),
            "plugins:\n  - repeat-tool-reminder\n  - auto-context-compaction\n",
        )
        .unwrap();

        std::fs::write(
            d.path().join("package.json"),
            "{\n  \"name\": \"opencode\",\n  \"plugin\": \"@opencode/telemetry\"\n}\n",
        )
        .unwrap();

        let g = build_graph(d.path());
        assert!(
            g.find_by_name("repeat-tool-reminder")
                .into_iter()
                .next()
                .is_some(),
            "YAML plugin should be indexed"
        );
        assert!(
            g.find_by_name("auto-context-compaction")
                .into_iter()
                .next()
                .is_some(),
            "YAML plugin should be indexed"
        );
    }

    #[test]
    fn test_quick_update_single_file_and_caching() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::write(root.join("a.rs"), "pub fn alpha() { beta(); }").unwrap();
        std::fs::write(root.join("b.rs"), "pub fn beta() {}").unwrap();

        let index = CodeIndex::new();
        let g = index.get(root);
        assert_eq!(g.node_count(), 2);
        assert!(!g.find_by_name("alpha").is_empty());
        assert!(!g.find_by_name("beta").is_empty());

        // Fast update a.rs
        let updated_a = "pub fn alpha_prime() { beta(); }";
        std::fs::write(root.join("a.rs"), updated_a).unwrap();
        let ok = index.update_single_file(&root.join("a.rs"), Some(updated_a));
        assert!(ok, "single file update must succeed");

        let stats = index.last_stats(root).unwrap();
        assert_eq!(stats.reparsed, 1);
        assert_eq!(stats.removed, 0);

        let g2 = index.get(root);
        assert!(
            g2.find_by_name("alpha").is_empty(),
            "old symbol should be removed"
        );
        assert!(
            !g2.find_by_name("alpha_prime").is_empty(),
            "new symbol should be present"
        );
        assert!(!g2.find_by_name("beta").is_empty());

        // A second get with no disk change must be a cache hit — not "reparse N files".
        let g3 = index.get(root);
        assert!(
            Arc::ptr_eq(&g2, &g3),
            "warm get after patch must reuse graph"
        );
        let stats2 = index.last_stats(root).unwrap();
        assert!(
            stats2.cache_hit || stats2.reparsed <= 1,
            "must not reparse the whole workspace: {stats2:?}"
        );
    }

    #[test]
    fn slash_mismatched_unit_keys_do_not_mark_every_file_dirty() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..12 {
            std::fs::write(
                d.path().join(format!("f{i}.rs")),
                format!("pub fn f{i}() {{}}\n"),
            )
            .unwrap();
        }
        let idx = CodeIndex::new();
        let _ = idx.get(d.path());

        // Poison in-memory keys with forward slashes (the SQLite/JSON round-trip
        // bug that made every file look new).
        {
            let mut guard = idx.inner.lock().unwrap();
            let old = std::mem::take(&mut guard.units);
            for (p, u) in old {
                let slashed = PathBuf::from(p.to_string_lossy().replace('\\', "/"));
                guard.units.insert(slashed, u);
            }
            guard.fast_patch_pending = false;
        }

        let g2 = idx.get(d.path());
        let stats = idx.last_stats(d.path()).unwrap();
        assert!(
            stats.reparsed <= 1,
            "slash-mismatched keys must be rekeyed, not fully reparsed: {stats:?}"
        );
        assert!(!g2.find_by_name("f0").is_empty());
        assert!(!g2.find_by_name("f11").is_empty());
    }

    #[test]
    fn test_normalize_index_path_consistency() {
        let p1 = Path::new("E:/code/agents/atomcode/foo.rs");
        let p2 = Path::new("e:\\code\\agents\\atomcode\\foo.rs");
        let p3 = Path::new(r"\\?\E:\code\agents\atomcode\foo.rs");

        let norm1 = normalize_index_path(p1);
        let norm2 = normalize_index_path(p2);
        let norm3 = normalize_index_path(p3);

        assert_eq!(norm1, norm2);
        assert_eq!(norm2, norm3);
    }

    #[test]
    fn sqlite_hit_does_not_ingest_unindexed_sibling_tree() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("keep.rs"), "fn keep() {}\n").unwrap();
        init_workspace_index(d.path(), false, &|_| {}).expect("init");

        // A huge unindexed sibling tree must not be walked on the next process start.
        let other = d.path().join("other-project");
        std::fs::create_dir_all(&other).unwrap();
        for i in 0..40 {
            std::fs::write(other.join(format!("n{i}.rs")), format!("fn n{i}() {{}}\n")).unwrap();
        }

        let idx = CodeIndex::new();
        let g = idx.get(d.path());
        assert!(
            g.find_by_name("keep").into_iter().next().is_some(),
            "sqlite snapshot must load"
        );
        assert!(
            g.find_by_name("n0").is_empty(),
            "unindexed sibling tree must not be ingested on cold start"
        );
        let stats = idx.last_stats(d.path()).unwrap();
        assert!(
            stats.cache_hit,
            "sqlite + no known-file change must be a cache hit: {stats:?}"
        );
        assert_eq!(
            stats.reparsed, 0,
            "must not reparse sibling tree: {stats:?}"
        );
    }

    #[test]
    fn scoped_get_ignores_dirty_files_outside_focus() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("proj_a");
        let b = d.path().join("proj_b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("a.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(b.join("b.rs"), "fn beta() {}\n").unwrap();

        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        assert!(g1.find_by_name("alpha").into_iter().next().is_some());
        assert!(g1.find_by_name("beta").into_iter().next().is_some());

        std::fs::write(a.join("a.rs"), "fn alpha_v2() {}\n").unwrap();
        let g_b = idx.get_scoped(d.path(), Some(&b));
        assert!(
            g_b.find_by_name("alpha").into_iter().next().is_some(),
            "scoped get of proj_b must not restat proj_a"
        );
        assert!(g_b.find_by_name("alpha_v2").is_empty());
        let stats_b = idx.last_stats(d.path()).unwrap();
        assert!(
            stats_b.cache_hit || stats_b.reparsed == 0,
            "focus proj_b was clean: {stats_b:?}"
        );

        let g_a = idx.get_scoped(d.path(), Some(&a));
        assert!(
            g_a.find_by_name("alpha_v2").into_iter().next().is_some(),
            "scoped get of proj_a must pick up the edit"
        );
        assert!(g_a.find_by_name("alpha").is_empty());
    }

    #[test]
    fn query_time_first_build_caps_reparse() {
        let d = tempfile::tempdir().unwrap();
        let n = MAX_REPARSE_PER_QUERY + 12;
        for i in 0..n {
            std::fs::write(
                d.path().join(format!("f{i:03}.rs")),
                format!("pub fn f{i}() {{}}\n"),
            )
            .unwrap();
        }
        let idx = CodeIndex::new();
        let _ = idx.get(d.path());
        let stats = idx.last_stats(d.path()).unwrap();
        let units = idx.inner.lock().unwrap().units.len();
        assert!(
            stats.reparsed <= MAX_REPARSE_PER_QUERY,
            "query-time must not re-tree-sitter the whole workspace: {stats:?}"
        );
        assert!(
            units <= MAX_REPARSE_PER_QUERY,
            "capped first build must not persist the deferred files as indexed: {units}"
        );
    }

    #[test]
    fn init_force_parses_every_file_past_query_cap() {
        let d = tempfile::tempdir().unwrap();
        let n = MAX_REPARSE_PER_QUERY + 12;
        for i in 0..n {
            std::fs::write(
                d.path().join(format!("f{i:03}.rs")),
                format!("pub fn f{i}() {{}}\n"),
            )
            .unwrap();
        }
        let report = init_workspace_index(d.path(), true, &|_| {}).expect("init --force");
        assert!(
            report.files >= n,
            "init --force must parse all {n} files, not cap at {}: {report:?}",
            MAX_REPARSE_PER_QUERY
        );
        assert!(
            report.reparsed >= n,
            "init --force must reparse every dirty file: {report:?}"
        );
    }

    #[test]
    fn init_force_rebuilds_after_query_time_truncated_index() {
        let d = tempfile::tempdir().unwrap();
        let n = MAX_REPARSE_PER_QUERY + 12;
        for i in 0..n {
            std::fs::write(
                d.path().join(format!("f{i:03}.rs")),
                format!("pub fn f{i}() {{}}\n"),
            )
            .unwrap();
        }
        let idx = CodeIndex::new();
        let _ = idx.get(d.path());
        assert!(
            idx.inner.lock().unwrap().units.len() <= MAX_REPARSE_PER_QUERY,
            "precondition: query-time index is truncated"
        );

        let report = init_workspace_index(d.path(), true, &|_| {}).expect("init --force");
        assert!(
            report.files >= n,
            "init --force must recover a truncated index: {report:?}"
        );
        assert!(
            report.reparsed >= n,
            "init --force must reparse the whole workspace, not 128: {report:?}"
        );
    }

    #[test]
    fn scoped_query_prioritizes_focus_when_reparse_capped() {
        let d = tempfile::tempdir().unwrap();
        let other = d.path().join("aaa_other");
        let focus = d.path().join("zzz_focus");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(&focus).unwrap();
        for i in 0..(MAX_REPARSE_PER_QUERY + 12) {
            std::fs::write(
                other.join(format!("o{i:03}.rs")),
                format!("pub fn other_{i}() {{}}\n"),
            )
            .unwrap();
        }
        std::fs::write(focus.join("hot.rs"), "pub fn focused_sym() {}\n").unwrap();

        let idx = CodeIndex::new();
        let g = idx.get_scoped(d.path(), Some(&focus));
        assert!(
            g.find_by_name("focused_sym").into_iter().next().is_some(),
            "focus files must be parsed even when dirty count exceeds the query cap"
        );
    }

    #[test]
    fn unparseable_sibling_is_tombstoned_not_rediscovered() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("keep.rs"), "pub fn keep() {}\n").unwrap();
        std::fs::write(d.path().join("empty.json"), "").unwrap();

        let idx = CodeIndex::new();
        let _ = idx.get(d.path());
        assert!(
            idx.inner.lock().unwrap().units.len() >= 2,
            "empty.json must stay in units as a tombstone"
        );

        let _ = idx.get(d.path());
        let stats = idx.last_stats(d.path()).unwrap();
        assert!(
            stats.cache_hit || stats.reparsed == 0,
            "tombstoned empty file must not be rediscovered: {stats:?}"
        );
    }

    #[test]
    fn scoped_query_skips_full_walk_when_focus_already_indexed() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("keep.rs"), "pub fn keep() {}\n").unwrap();
        init_workspace_index(d.path(), false, &|_| {}).expect("init");

        let vendor = src.join("vendor");
        std::fs::create_dir_all(&vendor).unwrap();
        for i in 0..20 {
            std::fs::write(vendor.join(format!("e{i}.h")), "").unwrap();
        }

        let idx = CodeIndex::new();
        let _ = idx.get_scoped(d.path(), Some(&src));
        let stats = idx.last_stats(d.path()).unwrap();
        assert_eq!(
            stats.reparsed, 0,
            "already-indexed focus must not WalkBuilder vendor/: {stats:?}"
        );
        assert!(
            idx.inner.lock().unwrap().units.len() < 5,
            "must not ingest the 20-file unindexed vendor tree"
        );
    }

    #[test]
    fn parse_parallelism_is_bounded() {
        let n = parse_parallelism();
        assert!(
            (1..=MAX_PARSE_THREADS).contains(&n),
            "parse threads must stay in 1..={MAX_PARSE_THREADS}, got {n}"
        );
    }

    #[test]
    fn parse_pool_size_matches_codegraph_formula() {
        // codegraph: resolveParsePoolSize(unset, Math.max(3, cores))
        //          = clamp(max(3, cores) - 1, 1, 8)
        assert_eq!(
            resolve_parse_pool_size(1),
            2,
            "1-core floored to 3 → 2 workers"
        );
        assert_eq!(
            resolve_parse_pool_size(2),
            2,
            "2-core floored to 3 → 2 workers"
        );
        assert_eq!(
            resolve_parse_pool_size(6),
            5,
            "6-core → 5 workers (leave 1)"
        );
        assert_eq!(resolve_parse_pool_size(8), 7);
        assert_eq!(resolve_parse_pool_size(16), 8, "cap at 8");
        assert_eq!(resolve_parse_pool_size(32), 8);
    }

    #[test]
    fn init_sqlite_keeps_full_units_after_compose_slims_memory() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..24 {
            std::fs::write(
                d.path().join(format!("f{i:02}.rs")),
                format!("pub fn f{i}() {{}}\n"),
            )
            .unwrap();
        }
        let report = init_workspace_index(d.path(), false, &|_| {}).expect("init");
        assert_eq!(report.files, 24);

        let db = crate::codeintel::index_db::IndexDb::open_shared(d.path()).unwrap();
        let loaded = db.load_units();
        assert_eq!(loaded.len(), 24);
        let with_nodes = loaded.values().filter(|u| !u.nodes.is_empty()).count();
        assert_eq!(
            with_nodes, 24,
            "SQLite must store full units, not post-compose fingerprints"
        );
        let graph = db.load_graph().expect("graph blob");
        assert!(
            graph.node_count() >= 24,
            "graph snapshot must be written: {}",
            graph.node_count()
        );

        let idx = CodeIndex::new();
        let g = idx.get(d.path());
        assert!(g.find_by_name("f0").into_iter().next().is_some());
        let stats = idx.last_stats(d.path()).unwrap();
        assert!(
            stats.cache_hit && stats.reparsed == 0,
            "restart after slim-on-load must be a cache hit: {stats:?}"
        );
    }

    #[test]
    fn init_streams_batches_without_dropping_units() {
        let d = tempfile::tempdir().unwrap();
        let n = PARSE_BATCH_FILES + 8;
        for i in 0..n {
            std::fs::write(
                d.path().join(format!("s{i:04}.rs")),
                format!("pub fn s{i}() {{}}\n"),
            )
            .unwrap();
        }
        let report = init_workspace_index(d.path(), false, &|_| {}).expect("init");
        assert_eq!(report.files, n, "{report:?}");
        assert!(report.reparsed >= n, "{report:?}");

        let db = crate::codeintel::index_db::IndexDb::open_shared(d.path()).unwrap();
        let loaded = db.load_units();
        assert_eq!(loaded.len(), n);
        assert_eq!(
            loaded.values().filter(|u| !u.nodes.is_empty()).count(),
            n,
            "streamed persist must write full units for every file"
        );
        let g = db.load_graph().expect("graph");
        assert!(g.node_count() >= n, "graph {}", g.node_count());
        assert!(g.find_by_name("s0").into_iter().next().is_some());
        assert!(g
            .find_by_name(&format!("s{}", n - 1))
            .into_iter()
            .next()
            .is_some());
    }

    #[test]
    fn string_literals_are_capped_per_symbol() {
        let d = tempfile::tempdir().unwrap();
        let mut src = String::from("pub fn many() {\n");
        for i in 0..80 {
            src.push_str(&format!("    let _s{i} = \"literal_value_{i:04}\";\n"));
        }
        src.push_str("}\n");
        std::fs::write(d.path().join("many.rs"), src).unwrap();
        let g = build_graph(d.path());
        let n = g.find_by_name("many").into_iter().next().expect("many");
        assert!(
            n.string_literals.len() <= MAX_STRING_LITERALS_PER_SYMBOL,
            "literals leaked past cap: {}",
            n.string_literals.len()
        );
        assert!(
            n.string_literals
                .iter()
                .all(|s| s.len() <= MAX_LITERAL_CHARS),
            "a literal exceeded the char cap"
        );
    }

    #[test]
    fn truncate_to_char_boundary_stops_before_cjk() {
        let mut s = "业绩统计".repeat(80);
        assert!(s.len() > MAX_LITERAL_CHARS);
        truncate_to_char_boundary(&mut s, MAX_LITERAL_CHARS);
        assert!(s.len() <= MAX_LITERAL_CHARS);
        assert!(s.is_char_boundary(s.len()));
        assert!(!s.is_empty());
    }

    #[test]
    fn cjk_literal_cap_does_not_panic_on_char_boundary() {
        let d = tempfile::tempdir().unwrap();
        // 汉字 = 3 bytes. 100 of them = 300 bytes > MAX_LITERAL_CHARS (240),
        // so a naive `truncate(240)` lands mid-character and panics.
        let cjk: String = "业绩统计报表客户回访".chars().cycle().take(100).collect();
        let src = format!("pub fn report() {{ let _s = \"{cjk}\"; }}\n");
        std::fs::write(d.path().join("cjk.rs"), src).unwrap();
        let g = build_graph(d.path());
        let n = g.find_by_name("report").into_iter().next().expect("report");
        assert!(
            n.string_literals
                .iter()
                .all(|s| s.len() <= MAX_LITERAL_CHARS),
            "CJK literal exceeded byte cap: {:?}",
            n.string_literals
                .iter()
                .map(|s| s.len())
                .collect::<Vec<_>>()
        );
        assert!(
            n.string_literals
                .iter()
                .all(|s| std::str::from_utf8(s.as_bytes()).is_ok()),
            "truncated literal must stay valid UTF-8"
        );
    }
}
