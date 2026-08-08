//! Stateless tree-sitter symbol extraction. Ported from production
//! `semantic/mod.rs::list_symbols_treesitter`, minus the parse cache and the
//! SemanticSearcher state — we parse fresh per call (single-file parsing is cheap, and
//! a neutral tool holds no shared index; the cross-file graph layer comes later).
//!
//! Queries are cached per-thread (compiling a tree-sitter query is expensive; a monorepo
//! index must not recompile them for every file).

use crate::codeintel::lang::Lang;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator, Tree};

/// Per-signature display cap in a skeleton — mirrors `read_file`'s line cap so the
/// skeleton never shows MORE of a (pathologically long, minified) line than a full read.
const SIG_MAX: usize = 2000;

/// A symbol definition found in a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    /// The tree-sitter node kind of the definition (e.g. `function_item`, `struct_item`).
    pub kind: String,
    /// 1-based inclusive line range of the definition.
    pub start_line: usize,
    pub end_line: usize,
    /// Byte range of the definition in the source.
    pub start_byte: usize,
    pub end_byte: usize,
}

thread_local! {
    static SYM_QUERIES: RefCell<HashMap<Lang, Query>> = RefCell::new(HashMap::new());
    static CALL_QUERIES: RefCell<HashMap<Lang, Query>> = RefCell::new(HashMap::new());
}

/// Parse `source` once. Shared by symbol outline and call extraction.
pub(crate) fn parse_source(source: &str, lang: Lang) -> Option<Tree> {
    let grammar = lang.grammar();
    let mut parser = Parser::new();
    parser.set_language(&grammar).ok()?;
    parser.parse(source, None)
}

fn with_sym_query<R>(lang: Lang, f: impl FnOnce(&Query) -> R) -> Option<R> {
    SYM_QUERIES.with(|cell| {
        let mut map = cell.borrow_mut();
        if !map.contains_key(&lang) {
            let q = Query::new(&lang.grammar(), lang.symbols_query()).ok()?;
            map.insert(lang, q);
        }
        Some(f(map.get(&lang)?))
    })
}

fn with_call_query<R>(lang: Lang, f: impl FnOnce(&Query) -> R) -> Option<R> {
    let Some(q_src) = lang.calls_query() else {
        return None;
    };
    CALL_QUERIES.with(|cell| {
        let mut map = cell.borrow_mut();
        if !map.contains_key(&lang) {
            let q = Query::new(&lang.grammar(), q_src).ok()?;
            map.insert(lang, q);
        }
        Some(f(map.get(&lang)?))
    })
}

/// Extract symbols from an already-parsed tree (no second parse).
pub(crate) fn extract_symbols_from_tree(
    source: &str,
    lang: Lang,
    tree: &Tree,
) -> Option<Vec<Symbol>> {
    with_sym_query(lang, |query| {
        let def_idx = match query.capture_index_for_name("definition") {
            Some(i) => i,
            None => return Vec::new(),
        };
        let name_idx = match query.capture_index_for_name("name") {
            Some(i) => i,
            None => return Vec::new(),
        };

        let mut cursor = QueryCursor::new();
        let mut symbols = Vec::new();
        let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();

        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
        loop {
            matches.advance();
            let m = match matches.get() {
                Some(m) => m,
                None => break,
            };

            let mut name = None;
            let (mut ds, mut de, mut dsr, mut der) = (0usize, 0usize, 0usize, 0usize);
            let mut kind = "";
            let mut has_def = false;
            for cap in m.captures {
                if cap.index == name_idx {
                    name = Some(source[cap.node.start_byte()..cap.node.end_byte()].to_string());
                }
                if cap.index == def_idx {
                    ds = cap.node.start_byte();
                    de = cap.node.end_byte();
                    dsr = cap.node.start_position().row;
                    der = cap.node.end_position().row;
                    kind = cap.node.kind();
                    has_def = true;
                }
            }
            if let (Some(name), true) = (name, has_def) {
                if seen.insert((ds, de)) {
                    symbols.push(Symbol {
                        name,
                        kind: kind.to_string(),
                        start_line: dsr + 1,
                        end_line: der + 1,
                        start_byte: ds,
                        end_byte: de,
                    });
                }
            }
        }
        symbols
    })
}

/// Parse `source` as `lang` and extract symbol definitions (functions, types, classes,
/// methods, …) via the language's tree-sitter query. `None` only if parsing or query
/// compilation fails; an empty `Vec` means a clean parse with no symbols.
pub fn extract_symbols(source: &str, lang: Lang) -> Option<Vec<Symbol>> {
    let tree = parse_source(source, lang)?;
    extract_symbols_from_tree(source, lang, &tree)
}

/// Call-site rows for the code graph (name + line). Kept here so one parse serves both
/// symbol outline and call edges.
#[derive(Clone, Debug)]
pub(crate) struct CallSite {
    pub callee_name: String,
    pub line: usize,
}

/// Extract `@callee` call sites from an already-parsed tree.
pub(crate) fn extract_call_sites_from_tree(
    source: &str,
    lang: Lang,
    tree: &Tree,
) -> Vec<CallSite> {
    with_call_query(lang, |query| {
        let Some(callee_idx) = query.capture_index_for_name("callee") else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        let mut calls = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
        loop {
            matches.advance();
            let m = match matches.get() {
                Some(m) => m,
                None => break,
            };
            for cap in m.captures {
                if cap.index != callee_idx {
                    continue;
                }
                calls.push(CallSite {
                    callee_name: source[cap.node.start_byte()..cap.node.end_byte()].to_string(),
                    line: cap.node.start_position().row + 1,
                });
            }
        }
        calls
    })
    .unwrap_or_default()
}

/// Extract the first symbol named `name` from `source`.
pub fn extract_symbol(source: &str, lang: Lang, name: &str) -> Option<Symbol> {
    extract_symbols(source, lang)?
        .into_iter()
        .find(|s| s.name == name)
}

/// Render a compact STRUCTURE skeleton of a source file: leading import lines + each
/// symbol's signature line annotated with its line range. `None` when the language is
/// unsupported or no symbols are found (the caller, e.g. `read_file`, then falls back to
/// a normal read). Lets a large file be outlined instead of dumped.
pub fn skeleton(path: &Path, source: &str) -> Option<String> {
    let lang = Lang::detect(path)?;
    let syms = extract_symbols(source, lang)?;
    if syms.is_empty() {
        return None;
    }
    let lines: Vec<&str> = source.lines().collect();
    let mut out = format!(
        "[File skeleton: {} lines, {} symbols. Read a symbol with read_symbol, or a line range with read_file offset/limit.]\n",
        lines.len(),
        syms.len()
    );
    // Leading import/use/include lines — cheap, high-value context.
    let mut shown_import = false;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("use ")
            || t.starts_with("import ")
            || t.starts_with("from ")
            || t.starts_with("#include")
            || t.starts_with("package ")
            || t.starts_with("require")
        {
            out.push_str(&format!("{:>6}| {}\n", i + 1, line.trim_end()));
            shown_import = true;
        }
    }
    if shown_import {
        out.push('\n');
    }
    for s in &syms {
        let mut sig = lines
            .get(s.start_line.saturating_sub(1))
            .map(|l| l.trim_end().to_string())
            .unwrap_or_else(|| s.name.clone());
        if sig.chars().count() > SIG_MAX {
            sig = sig.chars().take(SIG_MAX).collect::<String>() + "…";
        }
        let n = s.end_line.saturating_sub(s.start_line) + 1;
        out.push_str(&format!(
            "{:>6}| {}  // L{}-{} ({} lines)\n",
            s.start_line, sig, s.start_line, s.end_line, n
        ));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_symbols() {
        let src = "struct Foo {\n    x: i32,\n}\n\nfn bar(a: i32) -> i32 {\n    a + 1\n}\n";
        let syms = extract_symbols(src, Lang::Rust).expect("parse");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "{names:?}");
        assert!(names.contains(&"bar"), "{names:?}");
        let bar = syms.iter().find(|s| s.name == "bar").unwrap();
        assert_eq!(bar.start_line, 5);
        assert_eq!(bar.end_line, 7);
    }

    #[test]
    fn extracts_python_symbols() {
        let src = "class A:\n    def m(self):\n        pass\n\ndef top():\n    return 1\n";
        let syms = extract_symbols(src, Lang::Python).expect("parse");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"A"), "{names:?}");
        assert!(names.contains(&"top"), "{names:?}");
    }

    #[test]
    fn extract_symbol_finds_by_name() {
        let src = "fn alpha() {}\nfn beta() {}\n";
        let s = extract_symbol(src, Lang::Rust, "beta").expect("found");
        assert_eq!(s.name, "beta");
        assert_eq!(s.start_line, 2);
        assert!(extract_symbol(src, Lang::Rust, "missing").is_none());
    }

    #[test]
    fn empty_source_is_some_empty() {
        let syms = extract_symbols("", Lang::Rust).expect("parse");
        assert!(syms.is_empty());
    }

    #[test]
    fn skeleton_truncates_pathological_signature() {
        let long = "x".repeat(5000);
        let src = format!("fn f(/* {long} */) {{}}\n");
        let sk = skeleton(std::path::Path::new("a.rs"), &src).expect("skeleton");
        assert!(sk.contains("File skeleton"), "{}", &sk[..sk.len().min(120)]);
        assert!(sk.contains('…'), "long signature must be truncated");
        assert!(
            !sk.contains(&"x".repeat(2100)),
            "must not show the full 5000-char line"
        );
    }

    #[test]
    fn skeleton_none_for_symbolless_source() {
        // pure comments → no symbols → None (caller falls back to a normal read)
        assert!(skeleton(std::path::Path::new("a.rs"), "// just\n// comments\n").is_none());
        // unsupported language → None
        assert!(skeleton(std::path::Path::new("a.unknownext"), "anything").is_none());
    }

    #[test]
    fn extracts_symbols_for_every_language() {
        use Lang::*;
        // (lang, sample source, symbol names that MUST be found)
        let cases: &[(Lang, &str, &[&str])] = &[
            (Rust, "struct S;\nfn f() {}\n", &["S", "f"]),
            (
                Python,
                "class A:\n    def m(self):\n        pass\n\ndef g():\n    pass\n",
                &["A", "g"],
            ),
            (JavaScript, "function fn() {}\nclass C {}\n", &["fn", "C"]),
            (
                TypeScript,
                "function fn(): void {}\ninterface I {}\nclass C {}\n",
                &["fn", "I", "C"],
            ),
            (Tsx, "function App() { return null; }\n", &["App"]),
            (Go, "package p\nfunc F() {}\ntype T struct{}\n", &["F", "T"]),
            (Java, "class C {\n  void m() {}\n}\n", &["C", "m"]),
            (
                C,
                "struct S { int x; };\nint f(void) { return 0; }\n",
                &["S", "f"],
            ),
            (
                Cpp,
                "namespace N {}\nclass C {};\nvoid f() {}\n",
                &["N", "C", "f"],
            ),
            (CSharp, "class C {\n  void M() {}\n}\n", &["C", "M"]),
            (Php, "<?php\nfunction f() {}\nclass C {}\n", &["f", "C"]),
        ];
        for (lang, src, expected) in cases {
            let syms = extract_symbols(src, *lang).unwrap_or_default();
            let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
            for e in *expected {
                assert!(
                    names.contains(e),
                    "{lang:?}: expected symbol {e:?}, got {names:?}"
                );
            }
        }
        // HTML "symbols" are element-ish; just assert the query runs without error.
        assert!(extract_symbols("<div id=\"app\"></div>\n", Lang::Html).is_some());
    }

    #[test]
    fn byte_slicing_is_utf8_safe_with_multibyte_names() {
        // tree-sitter byte offsets align to token (char) boundaries, so name slicing
        // must not panic on multibyte identifiers / bodies.
        let src = "// 注释\nfn 计算(参数: i32) -> i32 {\n    参数 + 1\n}\n";
        let syms = extract_symbols(src, Lang::Rust).expect("parse");
        assert!(
            syms.iter().any(|s| s.name == "计算"),
            "{:?}",
            syms.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }
}
