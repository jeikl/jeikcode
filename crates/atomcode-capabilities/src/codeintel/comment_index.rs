//! Comment & Docstring Extraction & Proximity Binder.
//!
//! Extracts block comments and line comments across languages, groups contiguous lines,
//! and binds them to AST SymbolNodes by physical line proximity:
//! - Leading Docstring / Header Comments: immediately preceding a symbol definition (within 2 lines)
//! - Inline Body Comments: comments inside the symbol's start_line..=end_line body range
//! - Trailing Comments: comment on the same line as the symbol definition

use super::graph::SymbolNode;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentBlock {
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
}

/// Extract all comment blocks from a source string.
pub fn extract_comment_blocks(source: &str) -> Vec<CommentBlock> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut in_multiline = false;
    let mut multiline_start = 0;
    let mut multiline_buf = Vec::new();

    let mut single_start = 0;
    let mut single_buf = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1;
        let trimmed = line.trim();

        if in_multiline {
            multiline_buf.push(strip_multiline_prefix(trimmed));
            if line.contains("*/") || (trimmed.ends_with("'''") || trimmed.ends_with("\"\"\"")) {
                in_multiline = false;
                blocks.push(CommentBlock {
                    start_line: multiline_start,
                    end_line: line_num,
                    text: multiline_buf.join(" "),
                });
                multiline_buf.clear();
            }
            continue;
        }

        // Multi-line comment start
        if trimmed.starts_with("/*") || trimmed.starts_with("/**") {
            if !single_buf.is_empty() {
                blocks.push(CommentBlock {
                    start_line: single_start,
                    end_line: line_num - 1,
                    text: single_buf.join(" "),
                });
                single_buf.clear();
            }

            if line.contains("*/") {
                // Single-line block comment
                let clean = strip_comment_markers(trimmed);
                if !clean.is_empty() {
                    blocks.push(CommentBlock {
                        start_line: line_num,
                        end_line: line_num,
                        text: clean,
                    });
                }
            } else {
                in_multiline = true;
                multiline_start = line_num;
                multiline_buf.push(strip_multiline_prefix(trimmed));
            }
            continue;
        }

        // Python docstring start (standalone)
        if (trimmed.starts_with("'''") || trimmed.starts_with("\"\"\"")) && trimmed.len() > 3 {
            let quote = &trimmed[..3];
            let rest = &trimmed[3..];
            if rest.contains(quote) {
                blocks.push(CommentBlock {
                    start_line: line_num,
                    end_line: line_num,
                    text: strip_comment_markers(trimmed),
                });
            } else {
                in_multiline = true;
                multiline_start = line_num;
                multiline_buf.push(strip_comment_markers(trimmed));
            }
            continue;
        }

        // Line comment prefixes: //, ///, #, --
        let is_line_comment = trimmed.starts_with("//")
            || trimmed.starts_with("///")
            || (trimmed.starts_with('#')
                && !trimmed.starts_with("#[")
                && !trimmed.starts_with("#include"))
            || trimmed.starts_with("--");

        if is_line_comment {
            let clean = strip_comment_markers(trimmed);
            if single_buf.is_empty() {
                single_start = line_num;
            }
            single_buf.push(clean);
        } else {
            if !single_buf.is_empty() {
                blocks.push(CommentBlock {
                    start_line: single_start,
                    end_line: line_num - 1,
                    text: single_buf.join(" "),
                });
                single_buf.clear();
            }
        }
    }

    if !single_buf.is_empty() {
        blocks.push(CommentBlock {
            start_line: single_start,
            end_line: lines.len(),
            text: single_buf.join(" "),
        });
    }

    blocks
}

fn strip_comment_markers(s: &str) -> String {
    let mut t = s.trim();
    if t.starts_with("///") {
        t = &t[3..];
    } else if t.starts_with("//") || t.starts_with("--") {
        t = &t[2..];
    } else if t.starts_with('#') {
        t = &t[1..];
    } else if t.starts_with("/*") {
        t = &t[2..];
        if t.ends_with("*/") {
            t = &t[..t.len() - 2];
        }
    } else if t.starts_with("\"\"\"") || t.starts_with("'''") {
        t = &t[3..];
        if t.ends_with("\"\"\"") || t.ends_with("'''") {
            t = &t[..t.len() - 3];
        }
    }
    t.trim().to_string()
}

fn strip_multiline_prefix(s: &str) -> String {
    let mut t = s.trim();
    if t.starts_with("/*") {
        t = &t[2..];
    }
    if t.starts_with('*') && !t.starts_with("*/") {
        t = &t[1..];
    }
    if t.ends_with("*/") {
        t = &t[..t.len() - 2];
    }
    if t.starts_with("\"\"\"") || t.starts_with("'''") {
        t = &t[3..];
    }
    if t.ends_with("\"\"\"") || t.ends_with("'''") {
        t = &t[..t.len() - 3];
    }
    t.trim().to_string()
}

use super::graph::{CommentScope, StructuredComment};

/// Attach extracted comments to symbol nodes by physical line proximity, inspecting source context.
pub fn bind_comments_to_symbols_with_source(
    symbols: &mut [SymbolNode],
    comments: &[CommentBlock],
    source: &str,
) {
    let source_lines: Vec<&str> = source.lines().collect();
    for sym in symbols.iter_mut() {
        let sym_start = sym.start_line;
        let sym_end = sym.end_line;

        let mut leading_docs = Vec::new();
        let mut inline_comments = Vec::new();
        let mut structured_comments = Vec::new();

        for cb in comments {
            // 1. Leading comment: ended right before symbol starts (within 3 lines)
            if cb.end_line < sym_start && sym_start <= cb.end_line + 3 {
                leading_docs.push(cb.text.clone());
                let scope = match sym.kind {
                    super::graph::SymbolKind::Property
                    | super::graph::SymbolKind::ConfigProperty
                    | super::graph::SymbolKind::Variable => CommentScope::PropertyDoc,
                    super::graph::SymbolKind::Method | super::graph::SymbolKind::Function => {
                        CommentScope::MethodHeader
                    }
                    _ => CommentScope::Docstring,
                };
                structured_comments.push(StructuredComment {
                    text: cb.text.clone(),
                    scope,
                    line: cb.start_line,
                });
            }
            // 2. Inline body comment: contained inside symbol body
            else if cb.start_line >= sym_start && cb.end_line <= sym_end {
                inline_comments.push(cb.text.clone());

                let line_idx = cb.start_line.saturating_sub(1);
                let line_text = source_lines.get(line_idx).copied().unwrap_or("");
                let prev_line_text = if line_idx > 0 {
                    source_lines.get(line_idx - 1).copied().unwrap_or("")
                } else {
                    ""
                };

                let is_branch = line_text.contains("case ")
                    || line_text.contains("if ")
                    || line_text.contains("if(")
                    || line_text.contains("else if")
                    || line_text.contains("elif ")
                    || line_text.contains("switch")
                    || line_text.contains("match ")
                    || line_text.contains("=>")
                    || prev_line_text.contains("case ")
                    || prev_line_text.contains("switch");

                let scope = if is_branch {
                    let kind = if line_text.contains("case ") || prev_line_text.contains("case ") {
                        "switch_case".to_string()
                    } else if line_text.contains("if") {
                        "if_condition".to_string()
                    } else {
                        "branch".to_string()
                    };
                    CommentScope::BranchInline { branch_kind: kind }
                } else {
                    CommentScope::PlainInline
                };

                structured_comments.push(StructuredComment {
                    text: cb.text.clone(),
                    scope,
                    line: cb.start_line,
                });
            }
        }

        if !leading_docs.is_empty() {
            sym.docstring = Some(leading_docs.join("\n"));
        }
        sym.inline_comments = inline_comments;
        sym.comments = structured_comments;
    }
}

/// Attach extracted comments to symbol nodes by physical line proximity.
pub fn bind_comments_to_symbols(symbols: &mut [SymbolNode], comments: &[CommentBlock]) {
    bind_comments_to_symbols_with_source(symbols, comments, "");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_comment_blocks() {
        let code = r#"
// 批量扣减商品库存
// 防止并发超卖
pub fn batch_deduct() {
    // 锁定分布式锁
    lock.acquire();
}
"#;
        let blocks = extract_comment_blocks(code);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].text.contains("批量扣减商品库存"));
        assert!(blocks[0].text.contains("防止并发超卖"));
        assert!(blocks[1].text.contains("锁定分布式锁"));
    }
}
