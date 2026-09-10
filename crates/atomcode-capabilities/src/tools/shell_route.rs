//! Route simple shell file-ops onto dedicated tools (`read_file`, `list_directory`,
//! `grep`, `glob`, `write_file`) so a model that still emits `cat`/`ls`/`grep`
//! through the shell tool gets the builtin implementation plus a one-line hint.
//!
//! Conservative: pipelines, `&&` / `||` / `;`, unknown flags, and multi-file
//! `cat` fall through to the real shell.

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
        } => {
            let mut args = json!({ "pattern": pattern });
            if let Some(path) = path {
                args["path"] = json!(path);
            }
            if case_insensitive {
                args["case_insensitive"] = json!(true);
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

/// Parse a simple file-op command into a builtin route. `None` → run the shell.
pub(crate) fn try_route_shell_command(command: &str) -> Option<BuiltinRoute> {
    let cmd = strip_trailing_comment(command.trim());
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
    for t in args {
        if t == "-i" || t == "--ignore-case" {
            case_insensitive = true;
            continue;
        }
        if t == "-n" || t == "-r" || t == "-R" || t == "-I" || t == "--" {
            continue;
        }
        if t.starts_with("--color") {
            continue;
        }
        if t.starts_with('-') {
            return None;
        }
        positional.push(t.as_str());
    }
    let pattern = positional.first()?.to_string();
    let path = positional.get(1).map(|s| (*s).to_string());
    if positional.len() > 2 {
        return None;
    }
    Some(BuiltinRoute::Grep {
        pattern,
        path,
        case_insensitive,
    })
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
        assert!(try_route_shell_command("cat a | head").is_none());
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
            })
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
