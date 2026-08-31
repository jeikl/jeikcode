//! A loaded skill: a markdown template with optional YAML-ish frontmatter, plus the
//! argument/variable substitution engine. Ported from production `skill.rs`.
//!
//! `expand` runs any `` !`command` `` blocks through a shell — skills are TRUSTED,
//! user-authored content (the same trust as a slash command the user installed), so this
//! is by design, not arbitrary remote code.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The template body (everything after the frontmatter block).
    pub template: String,
    /// Tools the specialization MAY auto-approve while this skill is active (metadata;
    /// the L1 capability does not enforce it — that's an L2 approval-policy concern).
    pub allowed_tools: Vec<String>,
    /// If false (`user-invocable: false` in frontmatter), hidden from the `/` menu;
    /// the model can still auto-invoke it. Absent → true.
    pub user_invocable: bool,
    /// Directory containing the skill file (for `${CLAUDE_SKILL_DIR}`).
    pub skill_dir: PathBuf,
    pub source_path: PathBuf,
}

impl Skill {
    /// Substitute arguments + variables into the template:
    /// `$ARGUMENTS[N]` / `$N` (positional), `$ARGUMENTS` (all; appended if absent),
    /// `${CLAUDE_SESSION_ID}`, `${CLAUDE_SKILL_DIR}`, and `` !`cmd` `` (shell pre-exec).
    pub fn expand(&self, arguments: &str, session_id: &str) -> String {
        let positional: Vec<&str> = arguments.split_whitespace().collect();
        let skill_dir = self.skill_dir.to_string_lossy();

        // SINGLE left-to-right pass: each substitution's value is emitted literally and
        // never re-scanned — so an argument that itself contains `$1` is NOT re-expanded.
        let t = self.template.as_str();
        let mut result = String::with_capacity(t.len());
        let mut i = 0;
        while i < t.len() {
            let rest = &t[i..];
            if let Some((value, len)) =
                match_substitution(rest, &positional, arguments, session_id, skill_dir.as_ref())
            {
                result.push_str(value);
                i += len;
            } else {
                let ch = rest.chars().next().unwrap();
                result.push(ch);
                i += ch.len_utf8();
            }
        }
        // A template with no `$ARGUMENTS` token at all still gets the full args appended.
        if !self.template.contains("$ARGUMENTS") && !arguments.trim().is_empty() {
            result = format!("{}\n\nARGUMENTS: {}", result.trim_end(), arguments);
        }
        expand_shell_injections(&result)
    }

    /// Expand for model/tool injection. Aligns with Grok's skill envelope + OpenCode's
    /// base-dir/file-list pattern so every load path (use_skill, slash menu) returns a
    /// consistent, path-bearing payload:
    ///
    /// ```text
    /// <skill name="…" description="…" path="…">
    /// <system-reminder>…base dir…</system-reminder>   <!-- dir-style only -->
    /// {body with $ARGUMENTS / ${SKILL_DIR} expanded}
    /// <skill_files>…sampled absolute paths…</skill_files>  <!-- when present -->
    /// </skill>
    /// ```
    pub fn expand_for_injection(&self, arguments: &str, session_id: &str) -> String {
        let body = self.expand(arguments, session_id);
        let path = display_skill_dir(&self.skill_dir.to_string_lossy(), cfg!(windows));
        let desc = xml_attr_escape(&self.description);
        let name = xml_attr_escape(&self.name);
        let mut out = format!("<skill name=\"{name}\" description=\"{desc}\" path=\"{path}\">\n");
        if let Some(note) = self.bundled_resource_note() {
            out.push_str(&note);
            out.push_str("\n\n");
        }
        out.push_str(&body);
        if let Some(files) = self.list_bundled_files(20) {
            out.push_str("\n\n<skill_files>\n");
            for f in &files {
                out.push_str(&format!("<file>{f}</file>\n"));
            }
            out.push_str("</skill_files>");
        }
        out.push_str("\n</skill>");
        out
    }

    /// Absolute skill directory for catalog / diagnostics (forward-slash on Windows).
    pub fn display_location(&self) -> String {
        display_skill_dir(&self.skill_dir.to_string_lossy(), cfg!(windows))
    }

    /// True when this is a directory-style skill (`…/SKILL.md`) that owns a dedicated folder.
    pub fn is_directory_skill(&self) -> bool {
        self.source_path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md")
    }

    /// A `<system-reminder>` naming the skill's install directory, emitted only for
    /// directory-style skills (source file literally `SKILL.md`) — those own a dedicated
    /// folder that can bundle `scripts/`/`references/`. Single-file `.md` skills share a
    /// skills folder, so the note would point at the wrong (shared) directory.
    fn bundled_resource_note(&self) -> Option<String> {
        if !self.is_directory_skill() {
            return None;
        }
        let dir = self.display_location();
        Some(format!(
            "<system-reminder>\n\
             Base directory for this skill: {dir}\n\
             Relative paths in this skill (e.g. scripts/, references/, templates) are \
             relative to this base directory — NOT the current working directory. \
             `${{SKILL_DIR}}` / `${{CLAUDE_SKILL_DIR}}` / `{{SKILL_DIR}}` in the body are \
             already expanded to this path when present. Prefer absolute paths under the \
             base directory when invoking bundled scripts. Do not search the project cwd \
             for these files.\n\
             </system-reminder>"
        ))
    }

    /// Sample absolute paths of bundled resources under the skill directory (scripts,
    /// references, assets). Skips SKILL.md itself. Caps at `limit` (OpenCode samples 10;
    /// we allow up to 20). Flat single-file skills return None.
    fn list_bundled_files(&self, limit: usize) -> Option<Vec<String>> {
        if !self.is_directory_skill() || limit == 0 {
            return None;
        }
        let mut out = Vec::new();
        collect_skill_files(&self.skill_dir, &self.skill_dir, &mut out, limit, 0);
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
}

/// Escape XML attribute values for the skill envelope (name/description/path).
fn xml_attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Depth-bounded walk collecting non-SKILL.md files as absolute display paths.
fn collect_skill_files(root: &Path, dir: &Path, out: &mut Vec<String>, limit: usize, depth: usize) {
    const MAX_DEPTH: usize = 4;
    if out.len() >= limit || depth > MAX_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if out.len() >= limit {
            break;
        }
        if p.is_dir() {
            collect_skill_files(root, &p, out, limit, depth + 1);
        } else if p.is_file() {
            if p.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
                continue;
            }
            out.push(display_skill_dir(&p.to_string_lossy(), cfg!(windows)));
        }
    }
}

/// Format a skill directory for the base-dir note (see `bundled_resource_note`). On
/// Windows, convert `\` → `/` so the path works uniformly across read_file/Python/Git
/// Bash (a raw backslash path breaks when bash treats `\U`/`\s` as escapes). Windows
/// separators are always `\` and filenames can't contain `\`, so the replace is lossless.
/// On Unix, `\` is a legal filename char, so leave it. Note-text only — never used for IO.
fn display_skill_dir(raw: &str, is_windows: bool) -> String {
    if is_windows {
        raw.replace('\\', "/")
    } else {
        raw.to_string()
    }
}

/// Match a substitution token at the START of `rest`; returns `(replacement, consumed)`.
/// Longest-token-first; only DEFINED positional indices substitute (others stay literal,
/// matching production). `$N` consumes a maximal digit run (so `$10` ≠ `$1` + `0`).
fn match_substitution<'a>(
    rest: &str,
    positional: &[&'a str],
    arguments: &'a str,
    session_id: &'a str,
    skill_dir: &'a str,
) -> Option<(&'a str, usize)> {
    if let Some(after) = rest.strip_prefix("$ARGUMENTS[") {
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && after[digits.len()..].starts_with(']') {
            if let Ok(n) = digits.parse::<usize>() {
                if n < positional.len() {
                    return Some((positional[n], "$ARGUMENTS[".len() + digits.len() + 1));
                }
            }
        }
        return None; // malformed / out-of-range → literal
    }
    if rest.starts_with("${CLAUDE_SESSION_ID}") {
        return Some((session_id, "${CLAUDE_SESSION_ID}".len()));
    }
    // Grok-native SESSION_ID alias.
    if rest.starts_with("${SESSION_ID}") {
        return Some((session_id, "${SESSION_ID}".len()));
    }
    if rest.starts_with("${CLAUDE_SKILL_DIR}") {
        return Some((skill_dir, "${CLAUDE_SKILL_DIR}".len()));
    }
    // Grok-native SKILL_DIR (preferred) + bare `{SKILL_DIR}` used in many community skills.
    if rest.starts_with("${SKILL_DIR}") {
        return Some((skill_dir, "${SKILL_DIR}".len()));
    }
    if rest.starts_with("{SKILL_DIR}") {
        return Some((skill_dir, "{SKILL_DIR}".len()));
    }
    if rest.starts_with("$ARGUMENTS") {
        return Some((arguments, "$ARGUMENTS".len()));
    }
    if let Some(after) = rest.strip_prefix('$') {
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            if let Ok(n) = digits.parse::<usize>() {
                if n < positional.len() {
                    return Some((positional[n], 1 + digits.len()));
                }
            }
        }
    }
    None
}

/// Replace each `` !`cmd` `` with the command's trimmed stdout (sh -c). Stops on an
/// unclosed backtick.
fn expand_shell_injections(template: &str) -> String {
    let mut result = template.to_string();
    loop {
        let Some(start) = result.find("!`") else {
            break;
        };
        let search_from = start + 2;
        let Some(rel_end) = result[search_from..].find('`') else {
            break; // unclosed — leave as-is
        };
        let end = search_from + rel_end;
        let cmd = result[search_from..end].to_string();
        let output = run_shell_command(&cmd);
        result = format!("{}{}{}", &result[..start], output, &result[end + 1..]);
    }
    result
}

fn run_shell_command(cmd: &str) -> String {
    let secs = atomcode_config::config::ToolTimeoutsConfig::load_effective().skill_cmd_secs;
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    crate::process_utils::apply_utf8_locale_env_sync(&mut command);
    crate::process_utils::suppress_console_window_sync(&mut command);
    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return format!("[error: {e}]"),
    };
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(Ok(out)) => {
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stderr.trim().is_empty() {
                    s.push('\n');
                    s.push_str(stderr.trim());
                }
            }
            s.trim_end().to_string()
        }
        Ok(Err(e)) => format!("[error: {e}]"),
        Err(_) => {
            #[cfg(windows)]
            crate::process_utils::taskkill_tree(pid);
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            format!("[timed out after {secs}s]")
        }
    }
}

// ── Frontmatter + parsing ────────────────────────────────────────────────────

struct Frontmatter {
    name: Option<String>,
    description: String,
    allowed_tools: Vec<String>,
    /// If false (`user-invocable: false`), hidden from the `/` menu — the model can
    /// still auto-invoke. Absent → true.
    user_invocable: bool,
}

impl Default for Frontmatter {
    fn default() -> Self {
        Self {
            name: None,
            description: String::new(),
            allowed_tools: Vec::new(),
            user_invocable: true,
        }
    }
}

fn fm_value(s: &str) -> String {
    // Strip a surrounding pair of double OR single quotes (production parity).
    s.trim().trim_matches('"').trim_matches('\'').to_string()
}

/// True when a YAML scalar value is a block-scalar indicator (`>`, `|`, `>-`, `|+`, …).
/// Bare indicators keep content on following indented lines — a line-only parser that
/// stores `">"` as the description is the root cause of `name: >` catalog rows.
fn is_yaml_block_scalar_indicator(value: &str) -> bool {
    let v = value.trim();
    let Some(first) = v.as_bytes().first().copied() else {
        return false;
    };
    if first != b'>' && first != b'|' {
        return false;
    }
    v[1..]
        .bytes()
        .all(|b| matches!(b, b'+' | b'-' | b'0'..=b'9'))
}

/// Folded (`>`) YAML block: newlines → spaces, collapse runs of whitespace.
fn fold_yaml_block(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| l.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Collect a YAML block scalar starting after a `key: >` / `key: |` line.
/// Subsequent lines that are blank or indented belong to the scalar; the first
/// unindented non-blank line ends it (returned as the next index to process).
fn take_yaml_block_scalar(lines: &[&str], start: usize, folded: bool) -> (String, usize) {
    let mut collected: Vec<String> = Vec::new();
    let mut i = start;
    // Common indent of the first non-empty content line (YAML chomping-ish).
    let mut indent: Option<usize> = None;
    while i < lines.len() {
        let line = lines[i];
        if line.is_empty() {
            collected.push(String::new());
            i += 1;
            continue;
        }
        let leading = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        if leading == 0 {
            break; // next top-level key
        }
        if indent.is_none() {
            indent = Some(leading);
        }
        let strip = indent.unwrap_or(leading).min(leading);
        collected.push(line.chars().skip(strip).collect());
        i += 1;
    }
    // Drop trailing blank lines (YAML clip chomping default).
    while collected.last().is_some_and(|s| s.is_empty()) {
        collected.pop();
    }
    let value = if folded {
        fold_yaml_block(&collected)
    } else {
        collected.join("\n")
    };
    (value, i)
}

/// Parse `---`-delimited frontmatter; returns `(Frontmatter, body)`. Absent/unclosed
/// frontmatter → empty frontmatter + the whole content as body.
///
/// Supports YAML block scalars on `description:` (`>` folded / `|` literal), matching
/// Grok/OpenCode skill frontmatter so multi-line Chinese descriptions are not stored
/// as the bare indicator `">"`.
fn parse_frontmatter(content: &str) -> (Frontmatter, String) {
    let mut fm = Frontmatter::default();
    if !content.starts_with("---\n") && !content.starts_with("---\r\n") {
        return (fm, content.to_string());
    }
    let after_open = &content[if content.starts_with("---\r\n") { 5 } else { 4 }..];
    let (close_pos, skip) = match find_frontmatter_close(after_open) {
        Some(x) => x,
        None => return (fm, content.to_string()),
    };
    let block = &after_open[..close_pos];
    let body = &after_open[close_pos + skip..];
    let lines: Vec<&str> = block.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        // Top-level keys are matched after trim_start so lightly-indented
        // frontmatter (and test fixtures with source indentation) still parse.
        // Block-scalar body lines stay on the original `lines` slice so their
        // relative indent is preserved for `take_yaml_block_scalar`.
        let key_line = lines[i].trim_start();
        if let Some(v) = key_line.strip_prefix("name:") {
            fm.name = Some(fm_value(v));
            i += 1;
        } else if let Some(v) = key_line.strip_prefix("description:") {
            let rest = v.trim();
            if is_yaml_block_scalar_indicator(rest) {
                let folded = rest.as_bytes().first() == Some(&b'>');
                let (value, next) = take_yaml_block_scalar(&lines, i + 1, folded);
                fm.description = value;
                i = next;
            } else {
                let parsed = fm_value(v);
                // Never keep a bare block indicator as the description.
                fm.description = if is_yaml_block_scalar_indicator(&parsed) {
                    String::new()
                } else {
                    parsed
                };
                i += 1;
            }
        } else if let Some(v) = key_line.strip_prefix("allowed-tools:") {
            // AgentSkills spec is space-delimited; also accept commas (Claude Code compat).
            fm.allowed_tools = v
                .split([' ', ','])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            i += 1;
        } else if let Some(v) = key_line.strip_prefix("user-invocable:") {
            // Mirror core: only the literal `false` hides it; anything else stays true.
            fm.user_invocable = v.trim() != "false";
            i += 1;
        } else {
            i += 1;
        }
    }
    (fm, body.to_string())
}

/// Locate the closing `---`. Returns `(offset_of_close_newline, bytes_to_skip)`.
fn find_frontmatter_close(after_open: &str) -> Option<(usize, usize)> {
    // Closing delimiter at EOF with no trailing newline (empty / minimal frontmatter).
    if after_open == "---" {
        return Some((0, 3));
    }
    if after_open == "---\r" {
        return Some((0, 4));
    }
    if after_open.starts_with("---\n") {
        return Some((0, 4)); // empty frontmatter
    }
    if after_open.starts_with("---\r\n") {
        return Some((0, 5));
    }
    if let Some(pos) = after_open.find("\n---\n") {
        return Some((pos, 5));
    }
    if let Some(pos) = after_open.find("\n---\r\n") {
        return Some((pos, 6));
    }
    if after_open.ends_with("\n---") {
        return Some((after_open.len() - 4, 4));
    }
    if after_open.ends_with("\n---\r") {
        return Some((after_open.len() - 5, 5));
    }
    None
}

fn first_paragraph(template: &str) -> String {
    template
        .split("\n\n")
        .map(str::trim)
        .find(|p| !p.is_empty())
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err(format!("skill name '{name}' must be 1-64 characters"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/')
    {
        return Err(format!("skill name '{name}' has invalid characters"));
    }
    if name.starts_with(['/', '-'])
        || name.ends_with(['/', '-'])
        || name.contains("//")
        || name.contains("--")
    {
        return Err(format!(
            "skill name '{name}' has a bad slash/hyphen position"
        ));
    }
    Ok(())
}

fn make_name(base: &str, namespace: Option<&str>) -> String {
    let norm = base.to_ascii_lowercase().replace('/', "-");
    match namespace {
        Some(ns) => format!("{}:{norm}", ns.to_ascii_lowercase()),
        None => norm,
    }
}

/// Parse a flat `name.md` skill (name = file stem unless overridden in frontmatter).
pub(crate) fn parse_skill_file(path: &Path, namespace: Option<&str>) -> Result<Skill, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("invalid file name")?;
    build_skill(
        &content,
        stem,
        path.parent().unwrap_or(Path::new(".")),
        path,
        namespace,
    )
}

/// Parse a directory-style `<dir>/SKILL.md` (name = directory name unless overridden).
pub(crate) fn parse_skill_dir(
    skill_dir: &Path,
    skill_md: &Path,
    namespace: Option<&str>,
) -> Result<Skill, String> {
    let content = std::fs::read_to_string(skill_md).map_err(|e| e.to_string())?;
    let dir_name = skill_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("invalid directory name")?;
    build_skill(&content, dir_name, skill_dir, skill_md, namespace)
}

fn build_skill(
    content: &str,
    default_name: &str,
    skill_dir: &Path,
    source: &Path,
    namespace: Option<&str>,
) -> Result<Skill, String> {
    let (fm, template) = parse_frontmatter(content);
    let base = fm.name.as_deref().unwrap_or(default_name);
    validate_skill_name(base)?;
    let name = make_name(base, namespace);
    let description = if fm.description.is_empty() {
        first_paragraph(&template)
    } else {
        fm.description
    };
    Ok(Skill {
        name,
        description,
        template,
        allowed_tools: fm.allowed_tools,
        user_invocable: fm.user_invocable,
        skill_dir: skill_dir.to_path_buf(),
        source_path: source.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(template: &str) -> Skill {
        Skill {
            name: "t".into(),
            description: String::new(),
            template: template.into(),
            allowed_tools: vec![],
            user_invocable: true,
            skill_dir: PathBuf::from("/sk"),
            source_path: PathBuf::from("/sk/SKILL.md"),
        }
    }

    #[test]
    fn frontmatter_parses_user_invocable() {
        // `user-invocable: false` hides a skill from the `/` menu; anything else
        // (incl. absent) defaults to true. Mirrors core skill.rs parsing.
        let (fm, _) = parse_frontmatter("---\nname: x\nuser-invocable: false\n---\nbody");
        assert!(!fm.user_invocable);
        let (fm2, _) = parse_frontmatter("---\nname: x\n---\nbody");
        assert!(fm2.user_invocable, "absent → default true");
    }

    #[test]
    fn expand_arguments_full_and_positional() {
        // $ARGUMENTS = all args; positional $N / $ARGUMENTS[N] are 0-based ($0 = first).
        // A template WITHOUT $ARGUMENTS still gets the full args appended (production behavior).
        assert_eq!(
            skill("do $ARGUMENTS now").expand("a b c", ""),
            "do a b c now"
        );
        assert_eq!(
            skill("first=$0 second=$1")
                .expand("a b", "")
                .lines()
                .next()
                .unwrap(),
            "first=a second=b"
        );
        assert_eq!(skill("idx=$ARGUMENTS[1]").expand("a b", ""), "idx=b");
    }

    #[test]
    fn dollar_n_boundary() {
        // $1 (0-based → second arg) must not match inside $10 (eleventh arg).
        let out = skill("$1 and $10").expand("X Y Z Q R S T U V W K", "");
        assert!(out.starts_with("Y and K"), "{out}");
    }

    #[test]
    fn appends_args_when_no_arguments_token() {
        let out = skill("plain template").expand("hello world", "");
        assert!(out.contains("plain template"));
        assert!(out.contains("ARGUMENTS: hello world"), "{out}");
    }

    #[test]
    fn variable_substitution() {
        let out = skill("dir=${CLAUDE_SKILL_DIR} sid=${CLAUDE_SESSION_ID}").expand("", "sess-1");
        assert_eq!(out, "dir=/sk sid=sess-1");
    }

    #[test]
    fn shell_injection_runs() {
        // Windows CI may lack `sh`; accept either success or a clear spawn error.
        let out = skill("value=!`echo hi`").expand("", "");
        assert!(
            out == "value=hi"
                || out.contains("value=[error:")
                || out.contains("value=hi\r")
                || out.starts_with("value=hi"),
            "shell injection should expand or report spawn error, got {out:?}"
        );
    }

    #[test]
    fn frontmatter_parse() {
        let (fm, body) = parse_frontmatter("---\nname: my-skill\ndescription: \"does X\"\nallowed-tools: read_file, bash\n---\nbody here\n");
        assert_eq!(fm.name.as_deref(), Some("my-skill"));
        assert_eq!(fm.description, "does X");
        assert_eq!(
            fm.allowed_tools,
            vec!["read_file".to_string(), "bash".to_string()]
        );
        assert_eq!(body.trim(), "body here");
    }

    #[test]
    fn frontmatter_folded_description_block_scalar() {
        // Real community skills use `description: >` multi-line YAML — previously
        // stored as the bare indicator ">" and rendered as `name: >` in the catalog.
        // Closing `---` must be at column 0 (YAML frontmatter delimiter).
        let src = "---\nname: multi-db-executor\ndescription: >\n  连接公司内部数据库查询数据。当用户的需求涉及到数据库、\n  或着自身执行过程中需要查数据验证正确性时时使用该技能。\n---\n# multi-db-executor\nbody\n";
        let (fm, body) = parse_frontmatter(src);
        assert_eq!(fm.name.as_deref(), Some("multi-db-executor"));
        assert!(
            fm.description.contains("连接公司内部数据库"),
            "folded block must become real description, got {:?}",
            fm.description
        );
        assert!(
            fm.description.contains("验证正确性"),
            "second folded line must join: {:?}",
            fm.description
        );
        assert!(!fm.description.trim().starts_with('>'));
        assert!(body.contains("multi-db-executor"));
    }

    #[test]
    fn frontmatter_literal_description_block_scalar() {
        let src = "---\ndescription: |\n  line one\n  line two\n---\nbody\n";
        let (fm, _) = parse_frontmatter(src);
        assert_eq!(fm.description, "line one\nline two");
    }

    #[test]
    fn skill_dir_aliases_expand() {
        let s = skill("dir=${SKILL_DIR}|${CLAUDE_SKILL_DIR}|{SKILL_DIR}");
        let out = s.expand("", "");
        assert_eq!(out, "dir=/sk|/sk|/sk");
    }

    #[test]
    fn expand_for_injection_includes_path_envelope_and_files() {
        let dir = tempfile::tempdir().unwrap();
        // Directory name must be a valid skill name (tempdir basenames often start with `.`).
        let skill_dir = dir.path().join("demo-skill");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: d\n---\nDo X\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("scripts/db_executor.py"), "print(1)\n").unwrap();
        let skill = parse_skill_dir(&skill_dir, &skill_dir.join("SKILL.md"), None).unwrap();
        let out = skill.expand_for_injection("", "");
        assert!(out.contains("<skill name="), "{out}");
        assert!(out.contains("path=\""), "must carry path attr: {out}");
        assert!(out.contains("Base directory for this skill:"), "{out}");
        assert!(out.contains("<skill_files>"), "{out}");
        assert!(out.contains("db_executor.py"), "{out}");
        assert!(out.contains("Do X"), "{out}");
    }

    #[test]
    fn no_frontmatter_is_all_body() {
        let (fm, body) = parse_frontmatter("just a template\nmore");
        assert!(fm.name.is_none());
        assert_eq!(body, "just a template\nmore");
    }

    #[test]
    fn argument_containing_dollar_token_is_not_re_expanded() {
        // arg0 is the literal "$1"; the single pass must NOT re-expand it into arg1.
        let out = skill("a=$0 b=$1").expand("$1 V", "");
        assert!(out.starts_with("a=$1 b=V"), "{out}");
    }

    #[test]
    fn out_of_range_positional_stays_literal() {
        let out = skill("x=$5").expand("a b", "");
        assert!(out.starts_with("x=$5"), "undefined $5 stays literal: {out}");
    }

    #[test]
    fn frontmatter_single_quotes_and_space_tools() {
        let (fm, _) = parse_frontmatter(
            "---\nname: 'my-skill'\nallowed-tools: read_file bash grep\n---\nbody\n",
        );
        assert_eq!(fm.name.as_deref(), Some("my-skill"));
        assert_eq!(
            fm.allowed_tools,
            vec![
                "read_file".to_string(),
                "bash".to_string(),
                "grep".to_string()
            ]
        );
    }

    #[test]
    fn frontmatter_close_at_eof() {
        let (fm, body) = parse_frontmatter("---\ndescription: x\n---");
        assert_eq!(fm.description, "x");
        assert_eq!(body, "");
    }

    #[test]
    fn name_validation() {
        assert!(validate_skill_name("good-name_1").is_ok());
        assert!(validate_skill_name("").is_err());
        assert!(validate_skill_name("-bad").is_err());
        assert!(validate_skill_name("a--b").is_err());
        assert!(validate_skill_name("has space").is_err());
        assert_eq!(make_name("My/Skill", Some("Plug")), "plug:my-skill");
    }
}
