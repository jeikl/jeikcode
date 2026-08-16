//! `list_symbols` — outline a source file's symbols (name + kind + line range) via
//! tree-sitter. Non-destructive ⇒ always `Safe`.

use super::lang::Lang;
use super::symbols::extract_symbols;
use super::{err, ok, resolve_path};
use crate::tool_feedback::{format_path_not_found, parse_tool_args};
use async_trait::async_trait;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

pub struct ListSymbolsTool;

#[derive(Deserialize)]
struct Args {
    file_path: String,
    /// First symbol to show (0-based, by symbol ordinal — NOT a byte offset).
    #[serde(default)]
    offset: Option<usize>,
    /// Max symbols to show (default 300). Paginate by re-invoking with `offset`.
    #[serde(default)]
    limit: Option<usize>,
}

/// Default page size. Keeps a single response comfortably under the 16 KiB
/// artifact threshold for typical files (~90 B/line × 300 ≈ 27 KiB worst case
/// is still paginated; the bound is on symbol count, not bytes, so a huge file
/// can never blow up into a multi-hundred-KB listing).
const DEFAULT_PAGE: usize = 300;
/// Hard ceiling so even an explicit `limit` can't produce an unbounded listing.
const MAX_PAGE: usize = 1000;

#[async_trait]
impl Tool for ListSymbolsTool {
    fn name(&self) -> &str {
        "list_symbols"
    }
    fn description(&self) -> &str {
        "List the functions, classes, structs, methods and other symbols defined in a \
         source file, each with its line range. Faster and more precise than read_file \
         for understanding a file's structure before editing. Supports Rust, Python, \
         JS/TS/TSX, Go, Java, C/C++, C#, HTML, PHP. Paginated: pass `offset` to page \
         through a large symbol list (symbol ordinal, not byte offset). Relative paths \
         resolve against the working directory."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path to the source file (absolute, or relative to the working directory)" },
                "offset": { "type": "integer", "description": "First symbol to show (0-based, default 0)" },
                "limit": { "type": "integer", "description": "Max symbols to show (default 300, max 1000)" }
            },
            "required": ["file_path"]
        })
    }
    // read-only → risk() defaults to Safe.
    async fn execute(&self, args: &str, ctx: &ToolContext) -> ToolResult {
        let a: Args = match parse_tool_args(
            "list_symbols",
            args,
            r#"{"file_path":"<path>"}"#,
        ) {
            Ok(a) => a,
            Err(e) => return e.into_tool_result(),
        };
        let path = resolve_path(&a.file_path, &ctx.working_dir);
        let display = a.file_path.clone();
        let cwd = ctx.working_dir.clone();
        let offset = a.offset.unwrap_or(0);
        let limit = a.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
        // tree-sitter parsing is CPU-bound — keep it off the async runtime.
        tokio::task::spawn_blocking(move || render(&path, &display, &cwd, offset, limit))
            .await
            .unwrap_or_else(|_| err("list_symbols: task failed"))
    }
}

fn render(path: &Path, display: &str, cwd: &Path, offset: usize, limit: usize) -> ToolResult {
    let lang = match Lang::detect(path) {
        Some(l) => l,
        // NOT an error: the file is fine, this tool just has no grammar for it. Reporting it
        // as a failure paints a red card in the UI and inflates the tool error rate with
        // non-failures (an Android project full of .xml/.gradle drove this to 69%). Hand the
        // model the next step instead — but ONLY when the file actually exists. `detect`
        // runs before the read below, so this branch owns the existence check for
        // unsupported types: a missing/typo'd path must stay an error, or the model treats
        // it as "no symbols" and never corrects the path.
        None => {
            return if path.is_file() {
                ok(format!(
                    "no symbol index for {display} — no tree-sitter grammar is bundled for this \
                     file type. Read it with read_file instead."
                ))
            } else {
                err(format!("list_symbols: cannot read {display}: file not found"))
            }
        }
    };
    let source = match std::fs::read_to_string(path) {
        Ok(s) => super::strip_utf8_bom(&s).to_string(),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return err(format_path_not_found("list_symbols", display, path, cwd));
            }
            return err(format!("list_symbols: cannot read {}: {e}", path.display()));
        }
    };
    match extract_symbols(&source, lang) {
        Some(syms) if syms.is_empty() => ok(format!("No symbols found in {display}")),
        Some(syms) => {
            let total = syms.len();
            // Symbol-ORDINAL window — never a byte slice, so a row can never be
            // split mid-line and pagination is by count, not by guessing offsets.
            let start = offset.min(total);
            let end = (start + limit).min(total);
            let mut out = format!("Symbols in {display} ({total} total):\n\n");
            for s in &syms[start..end] {
                // Both line numbers right-aligned (matches production) so the columns
                // stay aligned across rows for easy scanning.
                out.push_str(&format!(
                    "  {:>4}-{:>4}  {}  ({})\n",
                    s.start_line, s.end_line, s.name, s.kind
                ));
            }
            if start == end {
                // Empty window (offset past the end): say so plainly instead of
                // emitting a nonsensical "51-50" range.
                out.push_str(&format!(
                    "\n(no symbols at offset {offset}: file has {total} symbols)"
                ));
            } else if end < total {
                out.push_str(&format!(
                    "\n(showing symbols {}-{} of {total}; pass offset={end} for more)",
                    start + 1,
                    end
                ));
            } else if start > 0 {
                out.push_str(&format!(
                    "\n(showing symbols {}-{} of {total}; end)",
                    start + 1,
                    end
                ));
            }
            out.push_str("\n[Use read_symbol to read any symbol's full source.]");
            ok(out)
        }
        None => err(format!("list_symbols: failed to parse {display}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::tool::ToolContext;
    use tokio_util::sync::CancellationToken;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            working_dir: dir.to_path_buf(),
            cancel: CancellationToken::new(),
            progress: atomcode_kernel::tool::ProgressSink::noop(),
            requester: None,
        }
    }

    #[tokio::test]
    async fn lists_rust_symbols() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("m.rs"),
            "struct S;\nfn alpha() {}\nfn beta() {}\n",
        )
        .unwrap();
        let r = ListSymbolsTool
            .execute(r#"{"file_path":"m.rs"}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("alpha"), "{}", r.content);
        assert!(r.content.contains("beta"), "{}", r.content);
        assert!(r.content.contains("S"), "{}", r.content);
    }

    /// 「这个类型没打包语法」是**能力边界**,不是故障:文件就在那儿、读得到,只是没有符号索引。
    /// 判成 `is_error` 会让它在 UI 里渲染成红色失败卡、在遥测里计进工具错误率 —— 实测一个
    /// Android 工程(.xml / .gradle / .kts 满地)能把 list_symbols 的失败率顶到 69%,而其中没有
    /// 一次是真的坏了。降级成正常结果并**明确指路 read_file**,模型才知道下一步该干什么。
    #[tokio::test]
    async fn unsupported_extension_is_not_an_error_and_points_to_read_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.xyzlang"), "stuff").unwrap();
        let r = ListSymbolsTool
            .execute(r#"{"file_path":"a.xyzlang"}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("no symbol index"), "{}", r.content);
        assert!(r.content.contains("read_file"), "{}", r.content);
    }

    /// 降级只针对"类型不支持"。文件真的不存在仍然是错误 —— 否则模型拿着一个不存在的路径
    /// 收到「正常结果」,会当成"这文件没符号"继续往下走,而不是去纠正路径。必须对**两类扩展名**
    /// 都成立:`nope.rs` 由下方的读取兜住;`nope.xyzlang`(不支持类型)必须由 None 分支的
    /// `is_file()` 守卫兜住,否则一个写错的 `.xml`/`.gradle` 路径会被降级成"没符号"。
    #[tokio::test]
    async fn missing_file_is_still_an_error() {
        let d = tempfile::tempdir().unwrap();
        for name in ["nope.rs", "nope.xyzlang"] {
            let r = ListSymbolsTool
                .execute(&format!(r#"{{"file_path":"{name}"}}"#), &ctx(d.path()))
                .await;
            assert!(r.is_error, "{name} must be an error: {}", r.content);
        }
    }

    #[tokio::test]
    async fn missing_file_errors() {
        let d = tempfile::tempdir().unwrap();
        let r = ListSymbolsTool
            .execute(r#"{"file_path":"nope.rs"}"#, &ctx(d.path()))
            .await;
        assert!(r.is_error);
        assert!(
            r.content.contains("does not exist") || r.content.contains("cannot read"),
            "{}",
            r.content
        );
    }

    /// Pagination is by SYMBOL ORDINAL, not byte offset: a page boundary can
    /// never split a row, and the hint tells the model exactly which offset to
    /// pass next.
    #[tokio::test]
    async fn paginates_by_symbol_ordinal() {
        let d = tempfile::tempdir().unwrap();
        let mut src = String::new();
        for i in 0..50 {
            src.push_str(&format!("fn alpha_{i}() {{}}\n"));
        }
        std::fs::write(d.path().join("many.rs"), src).unwrap();

        // First page: limit 10 → symbols 1-10 + a "pass offset=10 for more" hint.
        let r1 = ListSymbolsTool
            .execute(r#"{"file_path":"many.rs","limit":10}"#, &ctx(d.path()))
            .await;
        assert!(!r1.is_error, "{}", r1.content);
        assert!(r1.content.contains("alpha_0"), "{}", r1.content);
        assert!(r1.content.contains("alpha_9"), "{}", r1.content);
        assert!(!r1.content.contains("alpha_10"), "{}", r1.content);
        assert!(r1.content.contains("(50 total)"), "{}", r1.content);
        assert!(r1.content.contains("offset=10"), "{}", r1.content);

        // Second page: offset 10 → symbols 11-20.
        let r2 = ListSymbolsTool
            .execute(r#"{"file_path":"many.rs","offset":10,"limit":10}"#, &ctx(d.path()))
            .await;
        assert!(r2.content.contains("alpha_10"), "{}", r2.content);
        assert!(r2.content.contains("alpha_19"), "{}", r2.content);
        assert!(!r2.content.contains("alpha_20"), "{}", r2.content);
        assert!(r2.content.contains("offset=20"), "{}", r2.content);

        // Last page: offset 45 → symbols 46-50 + "(end)" — no dead "more" hint.
        let r3 = ListSymbolsTool
            .execute(r#"{"file_path":"many.rs","offset":45,"limit":10}"#, &ctx(d.path()))
            .await;
        assert!(r3.content.contains("alpha_49"), "{}", r3.content);
        assert!(r3.content.contains("46-50 of 50; end)"), "{}", r3.content);
        assert!(!r3.content.contains("for more"), "{}", r3.content);

        // offset past the end → empty window, no panic, coherent message.
        let r4 = ListSymbolsTool
            .execute(r#"{"file_path":"many.rs","offset":999,"limit":10}"#, &ctx(d.path()))
            .await;
        assert!(!r4.is_error, "{}", r4.content);
        assert!(r4.content.contains("(50 total)"), "{}", r4.content);
        assert!(r4.content.contains("no symbols at offset 999"), "{}", r4.content);
        assert!(!r4.content.contains("alpha_"), "{}", r4.content);
    }
}
