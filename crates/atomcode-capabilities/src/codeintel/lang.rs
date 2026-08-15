//! Language detection + tree-sitter grammar/query mapping. Ported from production
//! `semantic/language.rs` — the SYMBOL-extraction subset only. (The calls-query and
//! Vue/Svelte dual-parse belong to the later graph layer and are intentionally omitted.)

use std::path::Path;
use tree_sitter::Language;

/// A source language with a tree-sitter grammar or semantic symbol extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Html,
    Php,
    // Modern Mobile, Web3, Scripting, and Systems
    Kotlin,
    Swift,
    Dart,
    Ruby,
    Scala,
    Solidity,
    Lua,
    Terraform,
    Erlang,
    R,
    Nix,
}

impl Lang {
    /// The tree-sitter grammar for this language, if available.
    pub fn grammar(&self) -> Option<Language> {
        Some(match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::C => tree_sitter_c::LANGUAGE.into(),
            Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Lang::Html => tree_sitter_html::LANGUAGE.into(),
            Lang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            _ => return None,
        })
    }

    /// The tree-sitter query that captures symbol definitions (`@name` + `@definition`).
    pub fn symbols_query(&self) -> Option<&'static str> {
        Some(match self {
            Lang::Rust => include_str!("queries/rust.scm"),
            Lang::Python => include_str!("queries/python.scm"),
            Lang::JavaScript => include_str!("queries/javascript.scm"),
            // TSX = typed JSX → the TS symbol query matches the TSX grammar's node types
            // (the JS query does NOT compile against the TSX grammar).
            Lang::TypeScript | Lang::Tsx => include_str!("queries/typescript.scm"),
            Lang::Go => include_str!("queries/go.scm"),
            Lang::Java => include_str!("queries/java.scm"),
            Lang::C => include_str!("queries/c.scm"),
            Lang::Cpp => include_str!("queries/cpp.scm"),
            Lang::CSharp => include_str!("queries/csharp.scm"),
            Lang::Html => include_str!("queries/html.scm"),
            Lang::Php => include_str!("queries/php.scm"),
            _ => return None,
        })
    }

    /// The tree-sitter query that captures call expressions (`@callee`). `None` for
    /// languages without a calls query (no call edges built for them).
    pub fn calls_query(&self) -> Option<&'static str> {
        Some(match self {
            Lang::Rust => include_str!("queries/rust_calls.scm"),
            Lang::Python => include_str!("queries/python_calls.scm"),
            Lang::JavaScript | Lang::TypeScript | Lang::Tsx => {
                include_str!("queries/javascript_calls.scm")
            }
            Lang::Java => include_str!("queries/java_calls.scm"),
            Lang::Go => include_str!("queries/go_calls.scm"),
            Lang::CSharp => include_str!("queries/csharp_calls.scm"),
            _ => return None,
        })
    }

    /// Whether this language is walked into the cross-file code graph (`build_graph`).
    /// Code languages with symbol + call extraction; HTML/PHP stay single-file only
    /// (outline via `list_symbols`, no cross-file edges).
    pub fn is_indexed(&self) -> bool {
        matches!(
            self,
            Lang::Rust
                | Lang::Python
                | Lang::JavaScript
                | Lang::TypeScript
                | Lang::Tsx
                | Lang::Go
                | Lang::Java
                | Lang::C
                | Lang::Cpp
                | Lang::CSharp
                | Lang::Kotlin
                | Lang::Swift
                | Lang::Dart
                | Lang::Ruby
                | Lang::Scala
                | Lang::Solidity
        )
    }

    /// Detect the language from a file path's extension. `None` = unsupported.
    pub fn detect(path: &Path) -> Option<Lang> {
        let ext = path.extension()?.to_str()?;
        Some(match ext {
            "rs" => Lang::Rust,
            "py" | "pyi" => Lang::Python,
            "js" | "mjs" | "cjs" => Lang::JavaScript,
            "ts" | "mts" | "ets" => Lang::TypeScript,
            "tsx" | "jsx" | "vue" | "svelte" | "astro" => Lang::Tsx,
            "go" => Lang::Go,
            "java" => Lang::Java,
            "c" | "h" => Lang::C,
            "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "cu" | "cuh" | "m" | "mm" => Lang::Cpp,
            "cs" => Lang::CSharp,
            "html" | "htm" => Lang::Html,
            "php" => Lang::Php,
            "kt" | "kts" => Lang::Kotlin,
            "swift" => Lang::Swift,
            "dart" => Lang::Dart,
            "rb" | "rake" => Lang::Ruby,
            "scala" | "sc" => Lang::Scala,
            "sol" => Lang::Solidity,
            "lua" | "luau" => Lang::Lua,
            "tf" | "tfvars" => Lang::Terraform,
            "erl" | "hrl" => Lang::Erlang,
            "r" | "R" => Lang::R,
            "nix" => Lang::Nix,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn detect_by_extension() {
        assert_eq!(Lang::detect(Path::new("a.rs")), Some(Lang::Rust));
        assert_eq!(Lang::detect(Path::new("x/y/z.py")), Some(Lang::Python));
        assert_eq!(Lang::detect(Path::new("a.tsx")), Some(Lang::Tsx));
        assert_eq!(Lang::detect(Path::new("App.vue")), Some(Lang::Tsx));
        assert_eq!(Lang::detect(Path::new("Header.svelte")), Some(Lang::Tsx));
        assert_eq!(Lang::detect(Path::new("Index.astro")), Some(Lang::Tsx));
        assert_eq!(Lang::detect(Path::new("EntryAbility.ets")), Some(Lang::TypeScript));
        assert_eq!(Lang::detect(Path::new("main.go")), Some(Lang::Go));
        assert_eq!(Lang::detect(Path::new("MainActivity.kt")), Some(Lang::Kotlin));
        assert_eq!(Lang::detect(Path::new("AppDelegate.swift")), Some(Lang::Swift));
        assert_eq!(Lang::detect(Path::new("widget.dart")), Some(Lang::Dart));
        assert_eq!(Lang::detect(Path::new("app.rb")), Some(Lang::Ruby));
        assert_eq!(Lang::detect(Path::new("Token.sol")), Some(Lang::Solidity));
        assert_eq!(Lang::detect(Path::new("kernel.cu")), Some(Lang::Cpp));
        assert_eq!(Lang::detect(Path::new("main.tf")), Some(Lang::Terraform));
        assert_eq!(Lang::detect(Path::new("init.lua")), Some(Lang::Lua));
        assert_eq!(Lang::detect(Path::new("a.hpp")), Some(Lang::Cpp));
        assert_eq!(Lang::detect(Path::new("CouponService.cs")), Some(Lang::CSharp));
        assert_eq!(Lang::detect(Path::new("a.unknownext")), None);
        assert_eq!(Lang::detect(Path::new("noext")), None);
    }

    #[test]
    fn csharp_is_fully_indexed_with_calls() {
        assert!(Lang::CSharp.is_indexed());
        assert!(Lang::CSharp.calls_query().is_some());
        let q = tree_sitter::Query::new(&Lang::CSharp.grammar().unwrap(), Lang::CSharp.calls_query().unwrap())
            .expect("csharp_calls.scm must compile");
        assert!(q.capture_index_for_name("callee").is_some());
    }

    #[test]
    fn every_lang_has_a_compilable_query() {
        let mut failures = Vec::new();
        for lang in [
            Lang::Rust,
            Lang::Python,
            Lang::JavaScript,
            Lang::TypeScript,
            Lang::Tsx,
            Lang::Go,
            Lang::Java,
            Lang::C,
            Lang::Cpp,
            Lang::CSharp,
            Lang::Html,
            Lang::Php,
        ] {
            let Some(grammar) = lang.grammar() else { continue; };
            let Some(query_str) = lang.symbols_query() else { continue; };
            match tree_sitter::Query::new(&grammar, query_str) {
                Ok(q) => {
                    if q.capture_index_for_name("name").is_none()
                        || q.capture_index_for_name("definition").is_none()
                    {
                        failures.push(format!("{lang:?}: missing @name/@definition"));
                    }
                }
                Err(e) => failures.push(format!("{lang:?}: {e:?}")),
            }
        }
        assert!(
            failures.is_empty(),
            "query compile failures:\n{}",
            failures.join("\n")
        );
    }
}
