//! Route simple shell file-ops onto dedicated tools (`read_file`, `list_directory`,
//! `grep`, `glob`, `write_file`) so a model that still emits `cat`/`ls`/`grep`
//! through the shell tool gets the builtin implementation plus a one-line hint.
//!
//! Also: when a command is clearly a builtin-equivalent but too complex to rewrite
//! (multi-segment `;`, unsupported flags), the real shell still runs and a soft
//! hint is prepended so the model is told to use dedicated tools next time.

use super::glob::GlobTool;
use super::grep::GrepTool;
use super::list::ListDirTool;
use super::read::ReadFileTool;
use super::write::WriteFileTool;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use serde_json::json;

/// Canonical name advertised to the model. `bash` remains an alias so old
/// transcripts and models trained to call `bash` still resolve.
pub const SHELL_TOOL_NAME: &str = "run_command";
pub const SHELL_TOOL_ALIASES: &[&str] = &["bash"];

pub fn is_shell_tool_name(name: &str) -> bool {
    name.eq_ignore_ascii_case(SHELL_TOOL_NAME)
        || SHELL_TOOL_ALIASES
            .iter()
            .any(|alias| name.eq_ignore_ascii_case(alias))
}

const ROUTE_HINT: &str = "检测到你正在用run_command运行内置工具存在的命令，已为你自动路由到内置工具，下次请全程使用内置工具，比如read_file、grep等。\n\n";

/// Soft hint when the command looks like a builtin file-op but was too complex to
/// auto-rewrite (pipes with non-head tails, `;` chains, unsupported flags, …).
const SOFT_HINT: &str = "检测到你正在用run_command运行内置工具可覆盖的命令（如 cat/ls/grep/find/head）。本次因管道/多段命令/复杂参数未能完整自动路由，下次请直接使用内置工具：read_file、list_directory、grep、glob 等。\n\n";

const BUILTIN_EQUIV_HEADS: &[&str] = &[
    "cat",
    "type",
    "get-content",
    "gc",
    "head",
    "ls",
    "dir",
    "get-childitem",
    "gci",
    "grep",
    "rg",
    "find",
    "echo",
    "printf",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuiltinRoute {
    ReadFile {
        file_path: String,
        offset: Option<usize>,
        limit: Option<usize>,
    },
    ListDirectory {
        target_directory: String,
    },
    Grep {
        pattern: String,
        path: Option<String>,
        case_insensitive: bool,
        glob: Option<String>,
    },
    Glob {
        pattern: String,
        path: Option<String>,
    },
    WriteFile {
        file_path: String,
        content: String,
    },
}

pub(crate) async fn maybe_route_shell_command(command: &str, ctx: &ToolContext) -> Option<ToolResult> {
    let route = try_route_shell_command(command)?;
    Some(with_route_hint(dispatch_route(route, ctx).await))
}

/// When auto-route misses but the command clearly duplicates a dedicated tool,
/// return the soft hint to prepend onto the real shell result.
pub(crate) fn soft_hint_for_unrouted_builtin_equivalent(command: &str) -> Option<&'static str> {
    if try_route_shell_command(command).is_some() {
        // Caller should have routed; no soft hint needed on the shell path.
        return None;
    }
    if looks_like_builtin_file_op(command) {
        Some(SOFT_HINT)
    } else {
        None
    }
}

pub(crate) fn annotate_with_soft_hint(hint: Option<&'static str>, mut result: ToolResult) -> ToolResult {
    if let Some(hint) = hint {
        result.content = format!("{hint}{}", result.content);
    }
    result
}

fn with_route_hint(mut result: ToolResult) -> ToolResult {
    result.content = format!("{ROUTE_HINT}{}", result.content);
    result
}

async fn dispatch_route(route: BuiltinRoute, ctx: &ToolContext) -> ToolResult {
    match route {
        BuiltinRoute::ReadFile {
            file_path,
            offset,
            limit,
        } => {
            let mut args = json!({ "file_path": file_path });
            if let Some(offset) = offset {
                args["offset"] = json!(offset);
            }
            if let Some(limit) = limit {
                args["limit"] = json!(limit);
            }
            ReadFileTool::new(false)
                .execute(&args.to_string(), ctx)
                .await
        }
        BuiltinRoute::ListDirectory { target_directory } => {
            let args = json!({ "target_directory": target_directory });
            ListDirTool.execute(&args.to_string(), ctx).await
        }
        BuiltinRoute::Grep {
            pattern,
            path,
            case_insensitive,
            glob,
        } => {
            let mut args = json!({ "pattern": pattern });
            if let Some(path) = path {
                args["path"] = json!(path);
            }
            if case_insensitive {
                args["case_insensitive"] = json!(true);
            }
            if let Some(glob) = glob {
                args["glob"] = json!(glob);
            }
            GrepTool.execute(&args.to_string(), ctx).await
        }
        BuiltinRoute::Glob { pattern, path } => {
            let mut args = json!({ "pattern": pattern });
            if let Some(path) = path {
                args["path"] = json!(path);
            }
            GlobTool.execute(&args.to_string(), ctx).await
        }
        BuiltinRoute::WriteFile { file_path, content } => {
            let args = json!({ "file_path": file_path, "content": content });
            WriteFileTool.execute(&args.to_string(), ctx).await
        }
    }
}

/// True when any shell segment's command head is a dedicated-tool equivalent.
pub(crate) fn looks_like_builtin_file_op(command: &str) -> bool {
    let cmd = strip_trailing_comment(command.trim());
    if cmd.is_empty() {
        return false;
    }
    for segment in split_shell_segments(cmd) {
        let tokens = match tokenize(segment) {
            Some(t) if !t.is_empty() => t,
            _ => continue,
        };
        let head = command_head(&tokens[0]);
        if BUILTIN_EQUIV_HEADS.contains(&head.as_str()) {
            return true;
        }
    }
    false
}

/// Parse a file-op command into a builtin route. `None` → run the shell.
pub(crate) fn try_route_shell_command(command: &str) -> Option<BuiltinRoute> {
    let normalized = normalize_shell_for_route(command);
    if normalized.is_empty() {
        return None;
    }
    // Do not peel off the first `;` / `&&` segment alone — that would drop the
    // rest of the compound command. Soft-hint covers those instead.
    try_route_normalized(&normalized)
}

fn try_route_normalized(cmd: &str) -> Option<BuiltinRoute> {
    let cmd = strip_trailing_comment(cmd.trim());
    if cmd.is_empty() {
        return None;
    }
    if let Some(route) = try_heredoc_write(cmd) {
        return Some(route);
    }
    if has_unquoted_operator(cmd, &['|', ';'])
        || cmd.contains("&&")
        || cmd.contains("||")
        || cmd.contains(">>")
    {
        return None;
    }
    if has_unquoted_char(cmd, '>') {
        return try_echo_write(cmd);
    }
    let tokens = tokenize(cmd)?;
    if tokens.is_empty() {
        return None;
    }
    let head = command_head(&tokens[0]);
    match head.as_str() {
        "cat" | "type" | "get-content" | "gc" => route_read(&tokens[1..], None, None),
        "head" => route_head(&tokens[1..]),
        "ls" | "dir" | "get-childitem" | "gci" => route_ls(&tokens[1..]),
        "grep" | "rg" => route_grep(&tokens[1..]),
        "find" => route_find(&tokens[1..]),
        _ => None,
    }
}

/// Strip noise the model often appends so a simple file-op can still route:
/// `2>/dev/null`, `2>&1`, and trailing `| head -N` / `| tail -N`.
fn normalize_shell_for_route(command: &str) -> String {
    let mut s = strip_trailing_comment(command.trim()).to_string();
    if s.is_empty() {
        return s;
    }
    loop {
        let trimmed = s.trim_end();
        let lower = trimmed.to_ascii_lowercase();
        let stripped = if let Some(rest) = strip_unquoted_suffix(&lower, trimmed, "2>/dev/null") {
            rest
        } else if let Some(rest) = strip_unquoted_suffix(&lower, trimmed, ">/dev/null") {
            rest
        } else if let Some(rest) = strip_unquoted_suffix(&lower, trimmed, "1>/dev/null") {
            rest
        } else if let Some(rest) = strip_unquoted_suffix(&lower, trimmed, "&>/dev/null") {
            rest
        } else if let Some(rest) = strip_unquoted_suffix(&lower, trimmed, "2>&1") {
            rest
        } else if let Some(rest) = strip_trailing_head_or_tail_pipe(trimmed) {
            rest
        } else {
            break;
        };
        s = stripped.trim_end().to_string();
    }
    s
}

fn strip_unquoted_suffix<'a>(lower: &str, original: &'a str, suffix: &str) -> Option<&'a str> {
    if !lower.ends_with(suffix) {
        return None;
    }
    let start = original.len().checked_sub(suffix.len())?;
    // Refuse if the suffix sits inside quotes (cheap: scan to start).
    if has_unquoted_char(&original[..start], '\'') || has_unquoted_char(&original[..start], '"') {
        // still ok if quotes are balanced before the suffix; only reject when the
        // cut point is inside an open quote.
    }
    if quote_state_open(&original[..start]) {
        return None;
    }
    Some(original[..start].trim_end())
}

fn quote_state_open(s: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    for c in s.chars() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            _ => {}
        }
    }
    in_single || in_double
}

fn strip_trailing_head_or_tail_pipe(cmd: &str) -> Option<&str> {
    let (left, right) = rsplit_unquoted(cmd, '|')?;
    let right = right.trim();
    let toks = tokenize(right)?;
    if toks.is_empty() {
        return None;
    }
    let head = command_head(&toks[0]);
    if head != "head" && head != "tail" {
        return None;
    }
    // Only swallow simple `head`/`tail` with optional `-n N` / `-N`.
    for t in &toks[1..] {
        if t == "-n" || t.starts_with('-') {
            continue;
        }
        if t.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        return None;
    }
    Some(left.trim_end())
}

fn split_shell_segments(cmd: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_single = false;
    let mut in_double = false;
    let bytes = cmd.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '|' | ';' if !in_single && !in_double => {
                let seg = cmd[start..i].trim();
                if !seg.is_empty() {
                    out.push(seg);
                }
                start = i + 1;
            }
            '&' if !in_single && !in_double && bytes.get(i + 1) == Some(&b'&') => {
                let seg = cmd[start..i].trim();
                if !seg.is_empty() {
                    out.push(seg);
                }
                // stop at && — later segments are not independently routed
                start = cmd.len();
                break;
            }
            _ => {}
        }
        i += 1;
    }
    let seg = cmd[start..].trim();
    if !seg.is_empty() {
        out.push(seg);
    }
    out
}

fn rsplit_unquoted(s: &str, sep: char) -> Option<(&str, &str)> {
    let mut in_single = false;
    let mut in_double = false;
    let mut last = None;
    for (i, c) in s.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if c == sep && !in_single && !in_double => last = Some(i),
            _ => {}
        }
    }
    let i = last?;
    Some((&s[..i], &s[i + sep.len_utf8()..]))
}

fn command_head(tok: &str) -> String {
    tok.rsplit(['/', '\\'])
        .next()
        .unwrap_or(tok)
        .trim_end_matches(".exe")
        .to_ascii_lowercase()
}

fn route_read(
    args: &[String],
    offset: Option<usize>,
    limit: Option<usize>,
) -> Option<BuiltinRoute> {
    let files: Vec<&str> = args
        .iter()
        .map(|s| s.as_str())
        .filter(|t| !t.starts_with('-') && *t != "--")
        .collect();
    if files.len() != 1 {
        return None;
    }
    if args
        .iter()
        .any(|t| t.starts_with('-') && t != "--" && t != "-n" && !t.starts_with("-n"))
    {
        return None;
    }
    Some(BuiltinRoute::ReadFile {
        file_path: files[0].to_string(),
        offset,
        limit,
    })
}

fn route_head(args: &[String]) -> Option<BuiltinRoute> {
    let mut limit = None;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let t = args[i].as_str();
        if t == "-n" {
            i += 1;
            limit = args.get(i)?.parse().ok();
        } else if let Some(n) = t.strip_prefix("-n") {
            if n.is_empty() {
                i += 1;
                limit = args.get(i)?.parse().ok();
            } else {
                limit = n.parse().ok();
            }
        } else if t.starts_with('-') && t != "--" {
            return None;
        } else if t != "--" {
            files.push(t);
        }
        i += 1;
    }
    if files.len() != 1 {
        return None;
    }
    Some(BuiltinRoute::ReadFile {
        file_path: files[0].to_string(),
        offset: None,
        limit,
    })
}

fn route_ls(args: &[String]) -> Option<BuiltinRoute> {
    let mut dir = ".";
    for t in args {
        if t.starts_with('-') {
            continue;
        }
        if dir != "." {
            return None; // multiple paths → shell
        }
        dir = t;
    }
    Some(BuiltinRoute::ListDirectory {
        target_directory: dir.to_string(),
    })
}

fn route_grep(args: &[String]) -> Option<BuiltinRoute> {
    let mut case_insensitive = false;
    let mut positional = Vec::new();
    let mut includes: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let t = args[i].as_str();
        if t == "-i" || t == "--ignore-case" {
            case_insensitive = true;
            i += 1;
            continue;
        }
        // Harmless / already-default flags our GrepTool covers implicitly.
        if matches!(
            t,
            "-n" | "-r"
                | "-R"
                | "-I"
                | "-H"
                | "-h"
                | "-E"
                | "-F"
                | "-G"
                | "-P"
                | "-w"
                | "-x"
                | "-a"
                | "--"
                | "--line-number"
                | "--with-filename"
                | "--no-filename"
                | "--extended-regexp"
                | "--fixed-strings"
                | "--perl-regexp"
                | "--basic-regexp"
        ) {
            i += 1;
            continue;
        }
        if t.starts_with("--color") {
            i += 1;
            continue;
        }
        if let Some(pat) = t.strip_prefix("--include=") {
            includes.push(pat.to_string());
            i += 1;
            continue;
        }
        if t == "--include" {
            i += 1;
            includes.push(args.get(i)?.clone());
            i += 1;
            continue;
        }
        if t == "-e" || t == "--regexp" {
            i += 1;
            positional.insert(0, args.get(i)?.clone());
            i += 1;
            continue;
        }
        // Unsupported context / binary / exclude flags → leave to shell (+ soft hint).
        if t.starts_with('-') {
            return None;
        }
        positional.push(t.to_string());
        i += 1;
    }
    let pattern = positional.first()?.clone();
    let path = positional.get(1).cloned();
    if positional.len() > 2 {
        return None;
    }
    Some(BuiltinRoute::Grep {
        pattern,
        path,
        case_insensitive,
        glob: merge_include_globs(&includes),
    })
}

fn merge_include_globs(globs: &[String]) -> Option<String> {
    if globs.is_empty() {
        return None;
    }
    if globs.len() == 1 {
        return Some(globs[0].clone());
    }
    let mut exts = Vec::new();
    for g in globs {
        if let Some(ext) = g.strip_prefix("*.") {
            if !ext.is_empty()
                && !ext.contains(['*', '?', '/', '\\', '{', '}'])
            {
                exts.push(ext.to_string());
                continue;
            }
        }
        // Non-uniform patterns: keep the first include only.
        return Some(globs[0].clone());
    }
    Some(format!("*.{{{}}}", exts.join(",")))
}

fn route_find(args: &[String]) -> Option<BuiltinRoute> {
    // `find PATH -name GLOB` only.
    if args.len() != 3 {
        return None;
    }
    if args[1] != "-name" && args[1] != "-iname" {
        return None;
    }
    Some(BuiltinRoute::Glob {
        pattern: args[2].clone(),
        path: Some(args[0].clone()),
    })
}

fn try_echo_write(cmd: &str) -> Option<BuiltinRoute> {
    let (left, right) = split_unquoted(cmd, '>')?;
    let file = right.trim();
    if file.is_empty() || file.contains(char::is_whitespace) {
        return None;
    }
    let left_toks = tokenize(left.trim())?;
    if left_toks.is_empty() {
        return None;
    }
    let head = command_head(&left_toks[0]);
    if head != "echo" && head != "printf" {
        return None;
    }
    if head == "printf" {
        // `printf '%s\n' content` — drop the format if present.
        let rest = if left_toks.len() >= 2 && left_toks[1].contains('%') {
            left_toks[2..].join(" ")
        } else {
            left_toks[1..].join(" ")
        };
        return Some(BuiltinRoute::WriteFile {
            file_path: file.to_string(),
            content: rest,
        });
    }
    let mut content = left_toks[1..].join(" ");
    if !content.ends_with('\n') {
        content.push('\n');
    }
    Some(BuiltinRoute::WriteFile {
        file_path: file.to_string(),
        content,
    })
}

fn try_heredoc_write(cmd: &str) -> Option<BuiltinRoute> {
    // `cat > FILE <<'EOF'\nbody\nEOF`
    let lower = cmd.to_ascii_lowercase();
    if !lower.starts_with("cat ") && !lower.starts_with("cat>") {
        return None;
    }
    let gt = cmd.find('>')?;
    let rest = cmd[gt + 1..].trim_start();
    let (file, after_file) = rest.split_once("<<")?;
    let file = file.trim();
    if file.is_empty() || file.contains(char::is_whitespace) {
        return None;
    }
    let after = after_file.trim_start();
    let (tag_tok, body_and_end) = after.split_once([' ', '\n', '\r'])?;
    let tag = tag_tok.trim_matches(|c| c == '\'' || c == '"' || c == '-');
    if tag.is_empty() {
        return None;
    }
    let body = body_and_end.trim_end();
    let body = body
        .strip_suffix(tag)
        .unwrap_or(body)
        .trim_end_matches(['\n', '\r']);
    Some(BuiltinRoute::WriteFile {
        file_path: file.to_string(),
        content: if body.is_empty() {
            String::new()
        } else {
            format!("{body}\n")
        },
    })
}

fn strip_trailing_comment(s: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in s.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return s[..i].trim_end(),
            _ => {}
        }
    }
    s
}

fn has_unquoted_operator(s: &str, ops: &[char]) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    for c in s.chars() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if !in_single && !in_double && ops.contains(&c) => return true,
            _ => {}
        }
    }
    false
}

fn has_unquoted_char(s: &str, target: char) -> bool {
    has_unquoted_operator(s, &[target])
}

fn split_unquoted(s: &str, sep: char) -> Option<(&str, &str)> {
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in s.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if c == sep && !in_single && !in_double => {
                return Some((&s[..i], &s[i + sep.len_utf8()..]));
            }
            _ => {}
        }
    }
    None
}

fn tokenize(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            '\\' if !in_single => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            _ => cur.push(c),
        }
    }
    if in_single || in_double {
        return None;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_shell_tool_name_accepts_canonical_and_alias() {
        assert!(is_shell_tool_name("run_command"));
        assert!(is_shell_tool_name("bash"));
        assert!(is_shell_tool_name("BASH"));
        assert!(!is_shell_tool_name("read_file"));
    }

    #[test]
    fn routes_cat_ls_grep_find_echo() {
        assert_eq!(
            try_route_shell_command("cat src/main.rs"),
            Some(BuiltinRoute::ReadFile {
                file_path: "src/main.rs".into(),
                offset: None,
                limit: None,
            })
        );
        assert_eq!(
            try_route_shell_command("head -n 20 README.md"),
            Some(BuiltinRoute::ReadFile {
                file_path: "README.md".into(),
                offset: None,
                limit: Some(20),
            })
        );
        assert_eq!(
            try_route_shell_command("ls -la src"),
            Some(BuiltinRoute::ListDirectory {
                target_directory: "src".into(),
            })
        );
        assert_eq!(
            try_route_shell_command("grep -n TODO src/lib.rs"),
            Some(BuiltinRoute::Grep {
                pattern: "TODO".into(),
                path: Some("src/lib.rs".into()),
                case_insensitive: false,
                glob: None,
            })
        );
        assert_eq!(
            try_route_shell_command("find . -name '*.rs'"),
            Some(BuiltinRoute::Glob {
                pattern: "*.rs".into(),
                path: Some(".".into()),
            })
        );
        assert_eq!(
            try_route_shell_command("echo hello > /tmp/x.txt"),
            Some(BuiltinRoute::WriteFile {
                file_path: "/tmp/x.txt".into(),
                content: "hello\n".into(),
            })
        );
    }

    #[test]
    fn does_not_route_pipelines_or_unknown_flags() {
        // `| head` is stripped, so simple `cat a | head` DOES route — that's intended.
        assert_eq!(
            try_route_shell_command("cat a | head"),
            Some(BuiltinRoute::ReadFile {
                file_path: "a".into(),
                offset: None,
                limit: None,
            })
        );
        assert!(try_route_shell_command("ls && pwd").is_none());
        assert!(try_route_shell_command("grep -A 3 foo bar").is_none());
        assert!(try_route_shell_command("sed -i s/a/b/ file").is_none());
        assert!(try_route_shell_command("cargo test").is_none());
        assert!(try_route_shell_command("cat a b").is_none());
        assert!(try_route_shell_command("echo hello").is_none());
    }

    #[test]
    fn routes_quoted_grep_pattern() {
        assert_eq!(
            try_route_shell_command(r#"grep -i "foo bar" ."#),
            Some(BuiltinRoute::Grep {
                pattern: "foo bar".into(),
                path: Some(".".into()),
                case_insensitive: true,
                glob: None,
            })
        );
    }

    #[test]
    fn routes_grep_with_include_and_head_pipe() {
        let cmd = r#"grep -n -H -E "def |class |import |password|login" --include="*.py" --include="*.js" . 2>/dev/null | head -80"#;
        assert_eq!(
            try_route_shell_command(cmd),
            Some(BuiltinRoute::Grep {
                pattern: "def |class |import |password|login".into(),
                path: Some(".".into()),
                case_insensitive: false,
                glob: Some("*.{py,js}".into()),
            })
        );
    }

    #[test]
    fn compound_ls_grep_gets_soft_hint_not_full_route() {
        let cmd = r#"ls -la *.py *.js 2>/dev/null; echo "===="; grep -n "def \|class \|import " jxtx_login.py jxtx_crypto.py login_jxtx.py login_component.js 2>&1 | head -80"#;
        // First segment is multi-glob ls → cannot fully route; soft hint must fire.
        assert!(try_route_shell_command(cmd).is_none());
        assert!(looks_like_builtin_file_op(cmd));
        assert_eq!(
            soft_hint_for_unrouted_builtin_equivalent(cmd),
            Some(SOFT_HINT)
        );
    }

    #[test]
    fn routed_result_always_prepends_builtin_hint() {
        let hinted = with_route_hint(ToolResult {
            call_id: String::new(),
            content: "file contents".into(),
            is_error: false,
            images: vec![],
        });
        assert!(
            hinted.content.starts_with(ROUTE_HINT),
            "builtin-equivalent shell commands must tell the model to use dedicated tools next time: {}",
            hinted.content
        );
        assert!(hinted.content.contains("read_file"));
        assert!(hinted.content.contains("grep"));
        assert!(hinted.content.contains("file contents"));
    }

    #[tokio::test]
    async fn cat_ls_grep_route_results_include_hint() {
        use atomcode_kernel::tool::{ProgressSink, ToolContext};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "hello\n").unwrap();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            cancel: Default::default(),
            progress: ProgressSink::noop(),
            requester: None,
        };
        for cmd in ["cat a.rs", "ls", "grep hello a.rs"] {
            let result = maybe_route_shell_command(cmd, &ctx)
                .await
                .unwrap_or_else(|| panic!("{cmd} must route to a builtin"));
            assert!(
                result.content.starts_with(ROUTE_HINT),
                "{cmd} routed without the required hint:\n{}",
                result.content
            );
        }
    }
}
