//! `edit_file` — replace an exact, UNIQUE text fragment in a file (or all of them
//! with `replace_all`). Mutates the filesystem ⇒ always `Risky`.
//!
//! Public schema matches mainstream agent editors: `old_string` / `new_string`,
//! optional same-file `edits` array, optional `replace_all`. Line numbers are
//! hints. Weak-model quirks (stringified arrays, stale line numbers, CRLF /
//! indent / blank-line drift) are repaired internally and are not advertised.

use super::{coerce_eol, err, ok, read::lenient_usize, resolve_path};
use crate::tool_feedback::{format_path_not_found, parse_tool_args};
use async_trait::async_trait;
use atomcode_kernel::tool::{RiskLevel, Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;

pub struct EditFileTool;

#[derive(Deserialize)]
struct Args {
    file_path: String,
    #[serde(default)]
    old_string: String,
    #[serde(default)]
    new_string: String,
    #[serde(default)]
    replace_all: bool,
    #[serde(default, deserialize_with = "lenient_usize")]
    start_line: Option<usize>,
    #[serde(default, deserialize_with = "lenient_usize")]
    end_line: Option<usize>,
    #[serde(default, deserialize_with = "deserialize_edits")]
    edits: Vec<EditHunk>,
}

#[derive(Deserialize, Clone)]
struct EditHunk {
    #[serde(default)]
    old_string: String,
    #[serde(default)]
    new_string: String,
    #[serde(default)]
    replace_all: bool,
    #[serde(default, deserialize_with = "lenient_usize")]
    start_line: Option<usize>,
    #[serde(default, deserialize_with = "lenient_usize")]
    end_line: Option<usize>,
}

/// Generous window (lines) for treating a model `start_line` as "near enough"
/// when the same `old_string` appears more than once.
const LINE_HINT_SLACK: usize = 80;

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn description(&self) -> &str {
        "Replace text in a file. Same-file multi-hunk: ONE call with \
         `edits:[{old_string,new_string},…]` (JSON array, applied serially on one \
         buffer, one write; a failed hunk leaves the file UNCHANGED). Independent \
         files: emit parallel `edit_file` calls. For one hunk, top-level \
         `old_string`/`new_string`. Each `old_string` must be UNIQUE unless \
         `replace_all` is true. Indentation, blank lines, and CRLF/LF differences \
         are tolerated. Optional `start_line`/`end_line` are 1-based hints from the \
         last read — not hard splices when `old_string` is present. Relative paths \
         resolve against the working directory."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path to edit (absolute, or relative to the working directory)" },
                "old_string": { "type": "string", "description": "Single-hunk: text to find and replace. Omit when using `edits`. Unique unless replace_all is true." },
                "new_string": { "type": "string", "description": "Single-hunk: replacement text. Omit when using `edits`." },
                "replace_all": { "type": "boolean", "description": "Single-hunk: replace ALL occurrences (default false)." },
                "edits": {
                    "type": "array",
                    "description": "Same-file multi-hunk batch as a JSON array of objects. Applied serially on one in-memory buffer; one write. Prefer this over N edit_file calls on the same file. Independent files should be parallel top-level calls.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string", "description": "Exact unique snippet to replace. Indentation / blank-line / CRLF drift is tolerated." },
                            "new_string": { "type": "string", "description": "Replacement snippet." },
                            "replace_all": { "type": "boolean" },
                            "start_line": { "type": "integer", "description": "Optional 1-based hint from the last read. Locating still prefers old_string; small drift and earlier-hunk offsets are handled internally." },
                            "end_line": { "type": "integer", "description": "Optional 1-based hint. Line-range splice is used only when old_string is omitted." }
                        }
                    }
                },
                "start_line": { "type": "integer", "description": "Optional 1-based hint from the last read. When old_string is present it only disambiguates / absorbs drift; it is not a hard splice." },
                "end_line": { "type": "integer", "description": "Optional 1-based hint. Range splice only when old_string is omitted." }
            },
            "required": ["file_path"]
        })
    }
    fn risk(&self, _args: &str) -> RiskLevel {
        RiskLevel::Risky // mutates an existing file
    }
    fn always_grant_scope(&self, _args: &str) -> String {
        // Tool-wide: "总是 / Always" approves every edit this session (v1 parity),
        // not just this one exact file/old/new triple.
        String::new()
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> ToolResult {
        let t0 = std::time::Instant::now();
        let a: Args = match parse_tool_args(
            "edit_file",
            args,
            r#"{"file_path":"<path>","old_string":"<exact>","new_string":"<replacement>"}"#,
        ) {
            Ok(a) => a,
            Err(e) => return e.into_tool_result(),
        };
        let hunks: Vec<EditHunk> = if !a.edits.is_empty() {
            a.edits
        } else {
            vec![EditHunk {
                old_string: a.old_string,
                new_string: a.new_string,
                replace_all: a.replace_all,
                start_line: a.start_line,
                end_line: a.end_line,
            }]
        };
        if hunks.is_empty()
            || hunks.iter().all(|h| {
                h.old_string.is_empty()
                    && h.new_string.is_empty()
                    && h.start_line.is_none()
                    && h.end_line.is_none()
            })
        {
            return err(
                "edit_file: provide `old_string`/`new_string`, `start_line`/`end_line`, or a non-empty `edits` array."
                    .to_string(),
            );
        }
        let path = resolve_path(&a.file_path, &ctx.working_dir);
        let raw = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    return err(format_path_not_found(
                        "edit_file",
                        &a.file_path,
                        &path,
                        &ctx.working_dir,
                    ));
                }
                return err(format!(
                    "edit_file: cannot read {}: {e}",
                    crate::pathnorm::to_display(&path)
                ));
            }
        };
        // Decode to UTF-8 for matching, remembering the on-disk encoding so the edit is
        // written back in the SAME encoding. A GBK/GB18030 file (Chinese Windows) is
        // edited in place; an ambiguous non-UTF-8 file is refused, not corrupted.
        let decoded = match crate::tools::encoding::decode_for_edit(&path, &raw) {
            Some(d) => d,
            None => {
                return err(format!(
                    "edit_file: cannot read {} as UTF-8 or a supported legacy text encoding \
                     (GBK/GB18030). Convert it to UTF-8 first. The file was NOT modified.",
                    crate::pathnorm::to_display(&path)
                ))
            }
        };
        let content = decoded.text;
        let file_encoding = decoded.encoding;

        let mut buf = content.clone();
        let mut total = 0usize;
        let mut kinds: Vec<&str> = Vec::new();
        // Model line numbers are relative to the file as last read (this call's
        // original). Map them onto the current buffer after earlier hunks so a
        // later hunk that still cites original coordinates does not splice the
        // wrong place. Text matching still wins when old_string is present.
        let mut line_map = OrigLineMap::default();
        for (i, h) in hunks.iter().enumerate() {
            let mapped_start = h.start_line.map(|s| line_map.map(s));
            let mapped_end = h.end_line.map(|e| line_map.map(e));
            match apply_hunk(
                &buf,
                &h.old_string,
                &h.new_string,
                h.replace_all,
                mapped_start,
                mapped_end,
            ) {
                Ok((next, n, kind)) => {
                    if let Some((cur_s, old_n, new_n)) = changed_span(&buf, &next) {
                        let orig_s = line_map.unmap(cur_s);
                        let orig_e = orig_s.saturating_add(old_n.saturating_sub(1)).max(orig_s);
                        line_map.record(orig_s, orig_e, new_n);
                    }
                    buf = next;
                    total += n;
                    kinds.push(kind);
                }
                Err(e) => {
                    return err(format!(
                        "edit_file: hunk {}/{} failed. The file was NOT modified. {e}",
                        i + 1,
                        hunks.len()
                    ));
                }
            }
        }
        if let Err(msg) = write_encoded(&path, &buf, file_encoding).await {
            return err(msg);
        }
        #[cfg(feature = "codeintel")]
        crate::codeintel::notify_code_index_file_changed(&path, Some(&buf));
        let diff = build_compact_diff(&content, &buf);
        let cost_time = t0.elapsed();
        let kind_note = if kinds.len() == 1 {
            if kinds[0] == "exact" {
                String::new()
            } else {
                format!(" ({})", kinds[0])
            }
        } else {
            format!(" ({} hunks: {})", kinds.len(), kinds.join(", "))
        };
        return ok(format!(
            "> ⏱️ **Cost Time**: {:.2?}ms\n\nEdited {} ({total} replacement{}{kind_note})\n{}",
            cost_time.as_millis(),
            crate::pathnorm::to_display(&path),
            if total == 1 { "" } else { "s" },
            diff,
        ));
    }
}

/// Hidden compatibility: the public schema types `edits` as a JSON array, but
/// some providers/models emit a stringified array (`"[{...}]"`). Decode one
/// layer; if that fails, run `repair_json` (raw newlines inside snippets) and
/// try again. A single object is wrapped as a one-element array. Dual types are
/// never advertised in the schema.
fn deserialize_edits<'de, D>(d: D) -> Result<Vec<EditHunk>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(d)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(v) => parse_edits_value(v).map_err(serde::de::Error::custom),
    }
}

fn parse_edits_value(value: serde_json::Value) -> Result<Vec<EditHunk>, String> {
    match value {
        serde_json::Value::Array(_) => {
            serde_json::from_value(value).map_err(|e| format!("edits array items: {e}"))
        }
        serde_json::Value::Object(_) => {
            let hunk: EditHunk =
                serde_json::from_value(value).map_err(|e| format!("edits object: {e}"))?;
            Ok(vec![hunk])
        }
        serde_json::Value::String(s) => parse_edits_string(&s),
        other => Err(format!(
            "edits must be a JSON array of {{old_string,new_string}} objects (got {other})"
        )),
    }
}

fn parse_edits_string(s: &str) -> Result<Vec<EditHunk>, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(Vec::new());
    }
    let parsed = serde_json::from_str::<serde_json::Value>(t).or_else(|_| {
        serde_json::from_str::<serde_json::Value>(&crate::tools::repair::repair_json(t))
    });
    match parsed {
        Ok(v) => match v {
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => parse_edits_value(v),
            _ => Err("stringified edits decoded but was not a JSON array or object".into()),
        },
        Err(e) => Err(format!(
            "edits was a string (expected a JSON array). Could not decode: {e}"
        )),
    }
}

/// Maps 1-based line numbers from the file as last read onto the current
/// in-memory buffer after earlier hunks in this same call.
#[derive(Default)]
struct OrigLineMap {
    /// `(original_start, original_end, new_line_count)` in application order.
    hunks: Vec<(usize, usize, usize)>,
}

impl OrigLineMap {
    fn map(&self, orig: usize) -> usize {
        let mut line = orig as isize;
        for &(s, e, new_n) in &self.hunks {
            if orig > e {
                line += new_n as isize - (e - s + 1) as isize;
            }
        }
        line.max(1) as usize
    }

    fn unmap(&self, current: usize) -> usize {
        let mut candidate = current as isize;
        for &(s, e, new_n) in self.hunks.iter().rev() {
            let delta = new_n as isize - (e - s + 1) as isize;
            if candidate > e as isize {
                candidate -= delta;
            }
        }
        candidate.max(1) as usize
    }

    fn record(&mut self, orig_s: usize, orig_e: usize, new_n: usize) {
        let s = orig_s.max(1);
        let e = orig_e.max(s);
        self.hunks.push((s, e, new_n));
    }
}

/// First contiguous changed region between two snapshots: (1-based start,
/// old line count, new line count). `None` if identical.
fn changed_span(old: &str, new: &str) -> Option<(usize, usize, usize)> {
    let ol: Vec<&str> = old.lines().collect();
    let nl: Vec<&str> = new.lines().collect();
    let mut i = 0usize;
    while i < ol.len() && i < nl.len() && ol[i] == nl[i] {
        i += 1;
    }
    let mut o_end = ol.len();
    let mut n_end = nl.len();
    while o_end > i && n_end > i && ol[o_end - 1] == nl[n_end - 1] {
        o_end -= 1;
        n_end -= 1;
    }
    if i == o_end && i == n_end {
        return None;
    }
    Some((i + 1, o_end - i, n_end - i))
}

fn byte_to_line(content: &str, byte: usize) -> usize {
    content.as_bytes()[..byte.min(content.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

fn match_start_lines(content: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut from = 0usize;
    while let Some(pos) = content[from..].find(needle) {
        let abs = from + pos;
        lines.push(byte_to_line(content, abs));
        from = abs + needle.len();
    }
    lines
}

fn replace_nth(content: &str, needle: &str, replacement: &str, n: usize) -> String {
    if needle.is_empty() {
        return content.to_string();
    }
    let mut from = 0usize;
    let mut seen = 0usize;
    while let Some(pos) = content[from..].find(needle) {
        let abs = from + pos;
        if seen == n {
            let mut out = String::with_capacity(content.len() + replacement.len());
            out.push_str(&content[..abs]);
            out.push_str(replacement);
            out.push_str(&content[abs + needle.len()..]);
            return out;
        }
        seen += 1;
        from = abs + needle.len();
    }
    content.to_string()
}

/// Pick the match nearest `hint` when it is uniquely nearer than the next, or
/// within [`LINE_HINT_SLACK`]. `hint` is 1-based in the current buffer.
fn pick_occurrence(match_lines: &[usize], hint: usize) -> Option<usize> {
    if match_lines.is_empty() {
        return None;
    }
    if match_lines.len() == 1 {
        return Some(0);
    }
    let mut indexed: Vec<(usize, usize)> = match_lines
        .iter()
        .copied()
        .enumerate()
        .map(|(i, line)| (i, line.abs_diff(hint)))
        .collect();
    indexed.sort_by_key(|(_, d)| *d);
    let (best_i, best_d) = indexed[0];
    let second_d = indexed[1].1;
    if best_d < second_d && (best_d <= LINE_HINT_SLACK || second_d - best_d >= 5) {
        Some(best_i)
    } else {
        None
    }
}

fn apply_hunk(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<(String, usize, &'static str), String> {
    // Text locate always wins. Line numbers are hints (disambiguation / drift)
    // when old_string is present; range splice is only the no-text fallback.
    if !old_string.is_empty() {
        return apply_text_hunk(
            content,
            old_string,
            new_string,
            replace_all,
            start_line.or(end_line),
        );
    }
    if let (Some(s), Some(e)) = (start_line, end_line) {
        return apply_line_range(content, new_string, s, e);
    }
    Err(
        "edit_file: provide `old_string`/`new_string`, `start_line`/`end_line`, or a non-empty `edits` array."
            .into(),
    )
}

fn apply_line_range(
    content: &str,
    new_string: &str,
    s: usize,
    e: usize,
) -> Result<(String, usize, &'static str), String> {
    if s == 0 || e == 0 || s > e {
        return Err(format!(
            "invalid line range: start_line ({s}) must be >= 1 and <= end_line ({e})."
        ));
    }
    let lines: Vec<&str> = content.lines().collect();
    if s > lines.len() {
        return Err(format!(
            "[Line Out of Range]: file has {} lines, but start_line is {s}.",
            lines.len()
        ));
    }
    let file_eol = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let end_bounded = e.min(lines.len());
    let prefix = if s > 1 {
        let mut p = lines[..s - 1].join(file_eol);
        p.push_str(file_eol);
        p
    } else {
        String::new()
    };
    let suffix = if end_bounded < lines.len() {
        let mut suf = String::from(file_eol);
        suf.push_str(&lines[end_bounded..].join(file_eol));
        suf
    } else {
        String::new()
    };
    let normalized_new = coerce_eol(new_string, file_eol);
    let mut result = format!("{prefix}{normalized_new}{suffix}");
    if content.ends_with('\n') && !result.ends_with('\n') {
        result.push_str(file_eol);
    }
    Ok((result, 1, "line-range replace"))
}

fn apply_text_hunk(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    line_hint: Option<usize>,
) -> Result<(String, usize, &'static str), String> {
    if old_string == new_string {
        return Err("old_string and new_string are identical — nothing to change.".into());
    }

    let literal = content.matches(old_string).count();
    let (old_match, new_match, count) = if literal > 0 {
        (old_string.to_string(), new_string.to_string(), literal)
    } else {
        let file_eol = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let old_c = coerce_eol(old_string, file_eol);
        let c = content.matches(&old_c).count();
        (old_c, coerce_eol(new_string, file_eol), c)
    };

    if count == 0 {
        if let Some((fuzzy_result, fuzzy_count)) =
            try_fuzzy_replace(content, old_string, new_string, replace_all, line_hint)
        {
            if fuzzy_result != content {
                return Ok((fuzzy_result, fuzzy_count, "line-trimmed whitespace match"));
            }
        }
        if let Some((token_result, token_count)) =
            try_token_normalized_replace(content, old_string, new_string, replace_all, line_hint)
        {
            if token_result != content {
                return Ok((token_result, token_count, "token-normalized match"));
            }
        }
        if let Some((comment_result, comment_count)) =
            try_comment_style_replace(content, old_string, new_string, replace_all)
        {
            if comment_result != content {
                return Ok((comment_result, comment_count, "comment-style match"));
            }
        }
        if let Some((anchor_result, _)) = try_block_anchor_replace(content, old_string, new_string)
        {
            if anchor_result != content {
                return Ok((anchor_result, 1, "anchored block match"));
            }
        }
        if let Some((bound_result, _)) =
            try_trimmed_boundary_replace(content, old_string, new_string)
        {
            if bound_result != content {
                return Ok((bound_result, 1, "trimmed boundary match"));
            }
        }
        let hint = find_closest_match_snippet(content, old_string).unwrap_or_default();
        return Err(format!("old_string not found in file.\n{hint}"));
    }
    if count > 1 && !replace_all {
        if let Some(hint) = line_hint {
            let lines = match_start_lines(content, &old_match);
            if let Some(idx) = pick_occurrence(&lines, hint) {
                let updated = replace_nth(content, &old_match, &new_match, idx);
                return Ok((updated, 1, "exact (line-hint)"));
            }
        }
        return Err(format!(
            "old_string appears {count} times — it must be unique. Add surrounding context, or set replace_all=true."
        ));
    }
    if old_match == new_match {
        return Err(
            "old_string and new_string are identical after line-ending normalization.".into(),
        );
    }
    let updated = if replace_all {
        content.replace(&old_match, &new_match)
    } else {
        content.replacen(&old_match, &new_match, 1)
    };
    let replaced = if replace_all { count } else { 1 };
    Ok((updated, replaced, "exact"))
}

/// Write edited text back to `path` in its original on-disk `encoding`. Refuses (Err
/// with a user-facing message) rather than write replacement bytes if the text cannot
/// be represented — so a failed re-encode leaves the file untouched, never corrupted.
async fn write_encoded(
    path: &std::path::Path,
    text: &str,
    encoding: crate::tools::encoding::FileEncoding,
) -> Result<(), String> {
    let bytes = crate::tools::encoding::encode(text, encoding).ok_or_else(|| {
        format!(
            "edit_file: cannot re-encode the edit to {}'s original encoding; the file was \
             NOT modified. Convert it to UTF-8 first.",
            crate::pathnorm::to_display(path)
        )
    })?;
    tokio::fs::write(path, bytes).await.map_err(|e| {
        format!(
            "edit_file: failed to write {}: {e}",
            crate::pathnorm::to_display(path)
        )
    })
}

/// A compact GIT UNIFIED DIFF (`@@` hunks, 3 lines of context) between the OLD
/// and NEW whole-file contents, capped so a large edit can't flood the model
/// context / transcript. The TUI re-parses this into a line-numbered, color-
/// coded diff block; the model reads it as a normal unified diff.
fn build_compact_diff(old_file: &str, new_file: &str) -> String {
    const MAX_DIFF_LINES: usize = 60;
    // Bound the Myers diff with a deadline: this runs synchronously on the async
    // executor thread over the WHOLE file, and two large, mostly-different files
    // (e.g. replacing a minified blob) can otherwise spin for a long time and
    // stall the event loop. On timeout `similar` returns a coarser-but-valid
    // diff instead of hanging (same guard codex uses).
    let mut config = similar::TextDiff::configure();
    config.timeout(std::time::Duration::from_millis(200));
    let full = config
        .diff_lines(old_file, new_file)
        .unified_diff()
        .context_radius(3)
        .to_string();
    let full = full.trim_end();
    let lines: Vec<&str> = full.lines().collect();
    if lines.len() <= MAX_DIFF_LINES {
        return full.to_string();
    }
    let mut out = lines[..MAX_DIFF_LINES].join("\n");
    out.push_str(&format!(
        "\n… ({} more diff lines)",
        lines.len() - MAX_DIFF_LINES
    ));
    out
}

/// Number of leading whitespace **characters** in `s`. Counts Unicode
/// whitespace consistently with `chars().take(n)` — both operate on
/// characters, not bytes. This is the correct unit for indent arithmetic:
/// `" ".repeat(n)` and `chars().take(n)` both count characters.
fn leading_ws_chars(s: &str) -> usize {
    s.chars().take_while(|c| c.is_whitespace()).count()
}

/// Re-anchor `new_lines` to the file's REAL indentation at `original_line`: the first
/// non-empty new line is the anchor, and each line's SIGNED indent offset from it is
/// re-applied on top of the matched file line's actual leading whitespace (tabs
/// preserved, multi-byte whitespace counted by CHARACTER). Shared by both fuzzy tiers
/// ([`try_fuzzy_replace`] and [`try_block_anchor_replace`]).
fn reanchored_replacement(new_lines: &[&str], original_line: &str) -> Vec<String> {
    // Anchor indent = the first non-empty line of new_string. Using the first non-empty
    // line (NOT the min indent) avoids the indent-drift an outdented closing `}` causes.
    let new_base_indent = new_lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| leading_ws_chars(l))
        .unwrap_or(0);
    let file_indent = leading_ws_chars(original_line);
    let file_indent_str: String = original_line.chars().take(file_indent).collect();
    new_lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                let line_indent = leading_ws_chars(l);
                let signed_relative = line_indent as isize - new_base_indent as isize;
                let total_indent = if signed_relative >= 0 {
                    // Same/deeper than anchor: keep the file's indent prefix (preserves the
                    // tab/space mix) and extend with plain spaces.
                    format!(
                        "{}{}",
                        file_indent_str,
                        " ".repeat(signed_relative as usize)
                    )
                } else {
                    // Outdented from anchor: drop chars from the tail of the file's indent.
                    let drop = (-signed_relative) as usize;
                    let keep = file_indent.saturating_sub(drop);
                    file_indent_str.chars().take(keep).collect()
                };
                format!("{}{}", total_indent, l.trim())
            }
        })
        .collect()
}

/// Whitespace-normalized fuzzy replace (faithful port of the v1 editor's
/// `try_fuzzy_replace`). Matches `old_string` against `content` line-by-line with each
/// line `.trim()`-ed, so a model that reproduced indentation with the wrong whitespace
/// (spaces vs the file's tabs, or a slightly-off indent) still matches. The replacement
/// is re-anchored to the file's REAL indentation: the first non-empty line of
/// `new_string` is the anchor, and each line's signed offset from it is re-applied on
/// top of the matched file line's actual leading whitespace (tabs preserved).
///
/// Returns `None` (caller falls back to the normal "not found" error) when: the old
/// string is empty, its trimmed content totals < 10 chars (too short to match safely),
/// no window matches, or `!replace_all` but more than one window matches (ambiguous).
fn try_fuzzy_replace(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    line_hint: Option<usize>,
) -> Option<(String, usize)> {
    // 1. Exact match (with optional leading/trailing blank line trimming)
    let old_normalized: Vec<&str> = old_string.lines().map(|l| l.trim()).collect();
    // Strip leading/trailing empty lines from the old_string pattern if they don't match the file boundary
    let old_trimmed_core: Vec<&str> = {
        let start = old_normalized
            .iter()
            .position(|l| !l.is_empty())
            .unwrap_or(0);
        let end = old_normalized
            .iter()
            .rposition(|l| !l.is_empty())
            .map(|p| p + 1)
            .unwrap_or(0);
        if start < end {
            old_normalized[start..end].to_vec()
        } else {
            old_normalized.clone()
        }
    };
    if old_trimmed_core.is_empty() || old_trimmed_core.iter().all(|l| l.is_empty()) {
        return None;
    }

    let content_lines: Vec<&str> = content.lines().collect();
    let has_trailing_newline = content.ends_with('\n');
    let mut matches: Vec<(usize, usize)> = Vec::new();

    // Only attempt a fuzzy match if old_string has substantial content (guards against
    // a short fragment matching the wrong place after trimming).
    let total_non_ws: usize = old_trimmed_core.iter().map(|l| l.len()).sum();
    if total_non_ws < 4 {
        return None;
    }

    // Pass 1: match exact old_normalized
    let mut i = 0;
    while i + old_normalized.len() <= content_lines.len() {
        let window: Vec<&str> = content_lines[i..i + old_normalized.len()]
            .iter()
            .map(|l| l.trim())
            .collect();
        if window == old_normalized {
            matches.push((i, i + old_normalized.len()));
            i += old_normalized.len();
        } else {
            i += 1;
        }
    }

    // Pass 2: If no matches, try matching with trimmed core (handles accidental leading/trailing blank lines emitted by LLM)
    if matches.is_empty() && old_trimmed_core.len() != old_normalized.len() {
        let mut i = 0;
        while i + old_trimmed_core.len() <= content_lines.len() {
            let window: Vec<&str> = content_lines[i..i + old_trimmed_core.len()]
                .iter()
                .map(|l| l.trim())
                .collect();
            if window == old_trimmed_core {
                matches.push((i, i + old_trimmed_core.len()));
                i += old_trimmed_core.len();
            } else {
                i += 1;
            }
        }
    }

    if matches.is_empty() {
        return None;
    }
    // Unique unless replace_all, or a line hint uniquely picks one nearby match.
    if !replace_all && matches.len() > 1 {
        let lines: Vec<usize> = matches.iter().map(|(s, _)| s + 1).collect();
        match line_hint.and_then(|h| pick_occurrence(&lines, h)) {
            Some(i) => {
                matches = vec![matches[i]];
            }
            None => return None,
        }
    }

    // Re-anchor the replacement to each match's REAL indentation (see `reanchored_replacement`).
    let new_lines: Vec<&str> = new_string.lines().collect();
    let mut result_lines: Vec<String> = content_lines.iter().map(|l| l.to_string()).collect();
    let to_replace = if replace_all {
        &matches[..]
    } else {
        &matches[..1]
    };
    for &(start, end) in to_replace.iter().rev() {
        let replacement = reanchored_replacement(&new_lines, content_lines[start]);
        result_lines.splice(start..end, replacement);
    }

    let mut result = result_lines.join("\n");
    if has_trailing_newline && !result.ends_with('\n') {
        result.push('\n');
    }
    // `lines()` stripped every `\r`, so a CRLF file would otherwise be rewritten to LF
    // across the WHOLE file (incl. untouched lines) — a silent whole-file EOL downgrade.
    // Restore the file's convention, mirroring the exact-match path's EOL preservation.
    if content.contains("\r\n") {
        result = coerce_eol(&result, "\r\n");
    }
    let count = if replace_all { matches.len() } else { 1 };
    Some((result, count))
}

/// BLOCK-ANCHOR fuzzy replace — the tier below [`try_fuzzy_replace`]. When the model
/// reproduced a multi-line block but got an INTERIOR line slightly wrong (a typo, a
/// reordered token, a comment tweak), the whitespace-normalized tier — which requires
/// EVERY trimmed line to match — fails, and a weak model then resorts to a shell script.
/// This tier anchors on the FIRST and LAST trimmed lines and tolerates interior drift,
/// replacing the whole window (re-anchored to the file's real indent via
/// [`reanchored_replacement`]).
///
/// Conservative guards so it can't clobber the wrong block: needs ≥ 3 lines; both
/// anchors non-empty and ≥ 3 trimmed chars (so a bare `{`/`}` can't anchor); the window
/// length equals the old block's; ALL BUT AT MOST ONE line still matches trimmed (so a
/// window that merely shares its first/last line with an unrelated region is rejected —
/// a plain "≥ half" rule would degenerate to "anchors only" for n ≤ 4); and the anchored
/// window must be UNIQUE (no `replace_all` at this tier — guessing which of several to
/// rewrite is unsafe). Returns `None` on any miss so the caller falls back to not-found.
fn try_block_anchor_replace(
    content: &str,
    old_string: &str,
    new_string: &str,
) -> Option<(String, usize)> {
    let raw_old_lines: Vec<&str> = old_string.lines().collect();
    let start_pos = raw_old_lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(0);
    let end_pos = raw_old_lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|p| p + 1)
        .unwrap_or(0);
    let old_lines: Vec<&str> = if start_pos < end_pos {
        raw_old_lines[start_pos..end_pos].to_vec()
    } else {
        raw_old_lines.clone()
    };

    let n = old_lines.len();
    if n < 3 {
        return None;
    }
    let first_norm = clean_token_normalize(old_lines[0]);
    let last_norm = clean_token_normalize(old_lines[n - 1]);
    if first_norm.chars().count() < 2 || last_norm.chars().count() < 2 {
        return None;
    }

    let content_lines: Vec<&str> = content.lines().collect();
    let has_trailing_newline = content.ends_with('\n');
    let mut matches: Vec<usize> = Vec::new();
    let mut i = 0;
    while i + n <= content_lines.len() {
        let f_c = clean_token_normalize(content_lines[i]);
        let l_c = clean_token_normalize(content_lines[i + n - 1]);
        if f_c == first_norm && l_c == last_norm {
            let matched = (0..n)
                .filter(|&k| {
                    let a = clean_token_normalize(content_lines[i + k]);
                    let b = clean_token_normalize(old_lines[k]);
                    a == b || strsim::normalized_levenshtein(&a, &b) >= 0.75
                })
                .count();
            let threshold = if n <= 4 {
                n.saturating_sub(1)
            } else {
                (n as f32 * 0.65).ceil() as usize
            };
            if matched >= threshold {
                matches.push(i);
            }
        }
        i += 1;
    }
    if matches.len() != 1 {
        return None;
    }

    let start = matches[0];
    let new_lines: Vec<&str> = new_string.lines().collect();
    let replacement = reanchored_replacement(&new_lines, content_lines[start]);
    let mut result_lines: Vec<String> = content_lines.iter().map(|l| l.to_string()).collect();
    result_lines.splice(start..start + n, replacement);

    let mut result = result_lines.join("\n");
    if has_trailing_newline && !result.ends_with('\n') {
        result.push('\n');
    }
    if content.contains("\r\n") {
        result = coerce_eol(&result, "\r\n");
    }
    Some((result, 1))
}

/// Filter invisible Unicode characters, normalize smart quotes/punctuation, and collapse whitespace.
fn clean_token_normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_ws = false;
    for c in s.chars() {
        if matches!(
            c,
            '\u{feff}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{fe0f}'
        ) {
            continue;
        }
        let norm_char = match c {
            '\u{00a0}' | '\u{2002}' | '\u{2003}' | '\u{2009}' | '\t' => ' ',
            '“' | '”' | '″' => '"',
            '‘' | '’' | '′' => '\'',
            '（' => '(',
            '）' => ')',
            '【' => '[',
            '】' => ']',
            '：' => ':',
            '；' => ';',
            '，' => ',',
            other => other,
        };
        if norm_char.is_whitespace() {
            if !last_was_ws {
                out.push(' ');
                last_was_ws = true;
            }
        } else {
            out.push(norm_char);
            last_was_ws = false;
        }
    }
    out.trim().to_string()
}

/// Token & inline-whitespace normalized fallback: matches line-by-line after collapsing internal spaces,
/// stripping zero-width characters, and normalizing unicode quotes/punctuation.
fn try_token_normalized_replace(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    line_hint: Option<usize>,
) -> Option<(String, usize)> {
    let old_normalized: Vec<String> = old_string
        .lines()
        .map(clean_token_normalize)
        .filter(|l| !l.is_empty())
        .collect();
    if old_normalized.is_empty() {
        return None;
    }

    let total_chars: usize = old_normalized.iter().map(|l| l.len()).sum();
    if total_chars < 4 {
        return None;
    }

    let content_lines: Vec<&str> = content.lines().collect();
    let content_normalized: Vec<String> = content_lines
        .iter()
        .map(|l| clean_token_normalize(l))
        .collect();

    let n = old_normalized.len();
    if n == 0 || n > content_normalized.len() {
        return None;
    }

    let mut matches: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i + n <= content_normalized.len() {
        let window = &content_normalized[i..i + n];
        if window == old_normalized.as_slice() {
            matches.push((i, i + n));
            i += n;
        } else {
            i += 1;
        }
    }

    if matches.is_empty() {
        return None;
    }
    if !replace_all && matches.len() > 1 {
        let lines: Vec<usize> = matches.iter().map(|(s, _)| s + 1).collect();
        match line_hint.and_then(|h| pick_occurrence(&lines, h)) {
            Some(idx) => {
                matches = vec![matches[idx]];
            }
            None => return None,
        }
    }

    let has_trailing_newline = content.ends_with('\n');
    let new_lines: Vec<&str> = new_string.lines().collect();
    let mut result_lines: Vec<String> = content_lines.iter().map(|l| l.to_string()).collect();
    let to_replace = if replace_all {
        &matches[..]
    } else {
        &matches[..1]
    };
    for &(start, end) in to_replace.iter().rev() {
        let replacement = reanchored_replacement(&new_lines, content_lines[start]);
        result_lines.splice(start..end, replacement);
    }

    let mut result = result_lines.join("\n");
    if has_trailing_newline && !result.ends_with('\n') {
        result.push('\n');
    }
    if content.contains("\r\n") {
        result = coerce_eol(&result, "\r\n");
    }
    let count = if replace_all { matches.len() } else { 1 };
    Some((result, count))
}

/// Boundary trimmed context match: when LLM emitted extra leading or trailing context lines.
fn try_trimmed_boundary_replace(
    content: &str,
    old_string: &str,
    new_string: &str,
) -> Option<(String, usize)> {
    let old_lines: Vec<&str> = old_string.lines().collect();
    let new_lines: Vec<&str> = new_string.lines().collect();
    if old_lines.len() < 3 || new_lines.len() < 3 {
        return None;
    }
    // Case 1: Drop first line from old_string and new_string if they match
    if clean_token_normalize(old_lines[0]) == clean_token_normalize(new_lines[0]) {
        let sub_old = old_lines[1..].join("\n");
        let sub_new = new_lines[1..].join("\n");
        if let Some(res) = try_fuzzy_replace(content, &sub_old, &sub_new, false, None) {
            return Some(res);
        }
        if let Some(res) = try_token_normalized_replace(content, &sub_old, &sub_new, false, None) {
            return Some(res);
        }
    }
    // Case 2: Drop last line from old_string and new_string if they match
    if let (Some(last_o), Some(last_n)) = (old_lines.last(), new_lines.last()) {
        if clean_token_normalize(last_o) == clean_token_normalize(last_n) {
            let sub_old = old_lines[..old_lines.len() - 1].join("\n");
            let sub_new = new_lines[..new_lines.len() - 1].join("\n");
            if let Some(res) = try_fuzzy_replace(content, &sub_old, &sub_new, false, None) {
                return Some(res);
            }
            if let Some(res) =
                try_token_normalized_replace(content, &sub_old, &sub_new, false, None)
            {
                return Some(res);
            }
        }
    }
    None
}

/// Collapse `/** foo */` and `/**\n * foo\n */` (and `// foo`) into one comparable token
/// so a model that re-wrapped a javadoc still matches the on-disk form.
fn collapse_comment_style_lines(lines: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut javadoc: Vec<String> = Vec::new();
    let mut in_javadoc = false;
    for raw in lines {
        let t = raw.trim();
        if t.starts_with("/**") && t.ends_with("*/") {
            let inner = t.trim_start_matches("/**").trim_end_matches("*/").trim();
            out.push(clean_token_normalize(&format!("/** {inner} */")));
            continue;
        }
        if t.starts_with("/**") {
            in_javadoc = true;
            javadoc.clear();
            let inner = t.trim_start_matches("/**").trim();
            if !inner.is_empty() {
                javadoc.push(inner.to_string());
            }
            continue;
        }
        if in_javadoc {
            if t.ends_with("*/") {
                let inner = t
                    .trim_end_matches("*/")
                    .trim()
                    .trim_start_matches('*')
                    .trim();
                if !inner.is_empty() {
                    javadoc.push(inner.to_string());
                }
                in_javadoc = false;
                out.push(clean_token_normalize(&format!(
                    "/** {} */",
                    javadoc.join(" ")
                )));
            } else {
                let inner = t.trim_start_matches('*').trim();
                if !inner.is_empty() {
                    javadoc.push(inner.to_string());
                }
            }
            continue;
        }
        if t.starts_with("//") {
            out.push(clean_token_normalize(&format!(
                "/** {} */",
                t.trim_start_matches('/').trim()
            )));
            continue;
        }
        let n = clean_token_normalize(raw);
        if !n.is_empty() {
            out.push(n);
        }
    }
    out
}

/// Map each collapsed token back to a half-open line range in `lines`.
fn collapse_comment_style_spans(lines: &[&str]) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut javadoc: Vec<String> = Vec::new();
    let mut in_javadoc = false;
    let mut javadoc_start = 0usize;
    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim();
        if t.starts_with("/**") && t.ends_with("*/") {
            let inner = t.trim_start_matches("/**").trim_end_matches("*/").trim();
            out.push((clean_token_normalize(&format!("/** {inner} */")), i, i + 1));
            continue;
        }
        if t.starts_with("/**") {
            in_javadoc = true;
            javadoc.clear();
            javadoc_start = i;
            let inner = t.trim_start_matches("/**").trim();
            if !inner.is_empty() {
                javadoc.push(inner.to_string());
            }
            continue;
        }
        if in_javadoc {
            if t.ends_with("*/") {
                let inner = t
                    .trim_end_matches("*/")
                    .trim()
                    .trim_start_matches('*')
                    .trim();
                if !inner.is_empty() {
                    javadoc.push(inner.to_string());
                }
                in_javadoc = false;
                out.push((
                    clean_token_normalize(&format!("/** {} */", javadoc.join(" "))),
                    javadoc_start,
                    i + 1,
                ));
            } else {
                let inner = t.trim_start_matches('*').trim();
                if !inner.is_empty() {
                    javadoc.push(inner.to_string());
                }
            }
            continue;
        }
        if t.starts_with("//") {
            out.push((
                clean_token_normalize(&format!("/** {} */", t.trim_start_matches('/').trim())),
                i,
                i + 1,
            ));
            continue;
        }
        let n = clean_token_normalize(raw);
        if !n.is_empty() {
            out.push((n, i, i + 1));
        }
    }
    out
}

/// Match `old_string` after collapsing javadoc wrapping. Does **not** drop
/// annotations or unrelated identifiers — a model that tried to replace
/// `createTime` with a different field must still fail.
fn try_comment_style_replace(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Option<(String, usize)> {
    let old_lines: Vec<&str> = old_string.lines().collect();
    let old_collapsed = collapse_comment_style_lines(&old_lines);
    if old_collapsed.len() < 2 {
        return None;
    }
    let total: usize = old_collapsed.iter().map(|s| s.len()).sum();
    if total < 8 {
        return None;
    }

    let content_lines: Vec<&str> = content.lines().collect();
    let content_spans = collapse_comment_style_spans(&content_lines);
    if content_spans.len() < old_collapsed.len() {
        return None;
    }

    let n = old_collapsed.len();
    let mut matches: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i + n <= content_spans.len() {
        let window: Vec<&String> = content_spans[i..i + n].iter().map(|(s, _, _)| s).collect();
        if window.iter().copied().eq(old_collapsed.iter()) {
            let start_line = content_spans[i].1;
            let end_line = content_spans[i + n - 1].2;
            matches.push((start_line, end_line));
            i += n;
        } else {
            i += 1;
        }
    }
    if matches.is_empty() || (!replace_all && matches.len() > 1) {
        return None;
    }

    let has_trailing_newline = content.ends_with('\n');
    let new_lines: Vec<&str> = new_string.lines().collect();
    let mut result_lines: Vec<String> = content_lines.iter().map(|l| l.to_string()).collect();
    let to_replace = if replace_all {
        &matches[..]
    } else {
        &matches[..1]
    };
    for &(start, end) in to_replace.iter().rev() {
        let replacement = reanchored_replacement(&new_lines, content_lines[start]);
        result_lines.splice(start..end, replacement);
    }

    let mut result = result_lines.join("\n");
    if has_trailing_newline && !result.ends_with('\n') {
        result.push('\n');
    }
    if content.contains("\r\n") {
        result = coerce_eol(&result, "\r\n");
    }
    let count = if replace_all { matches.len() } else { 1 };
    Some((result, count))
}

/// Computes the closest snippet in `content` to `old_string` using normalized Levenshtein similarity.
fn find_closest_match_snippet(content: &str, old_string: &str) -> Option<String> {
    let old_lines: Vec<&str> = old_string.lines().collect();
    let old_core: String = old_lines
        .iter()
        .map(|l| clean_token_normalize(l))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if old_core.is_empty() {
        return None;
    }
    let content_lines: Vec<&str> = content.lines().collect();
    let n = old_lines.len().max(1);

    let mut best_score = 0.0f32;
    let mut best_range = (0, 0);

    for i in 0..content_lines.len() {
        let end = (i + n).min(content_lines.len());
        let window: String = content_lines[i..end]
            .iter()
            .map(|l| clean_token_normalize(l))
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let sim = strsim::normalized_levenshtein(&old_core, &window) as f32;
        if sim > best_score {
            best_score = sim;
            best_range = (i, end);
        }
    }

    if best_score >= 0.30 {
        let (start, end) = best_range;
        let ctx = 3usize;
        let show_start = start.saturating_sub(ctx);
        let show_end = (end + ctx).min(content_lines.len());
        let snippet = content_lines[show_start..show_end].join("\n");
        Some(format!(
            "[Content Mismatch]: Closest matching block found around lines {}-{} (similarity {:.0}%):\n```\n{}\n```\n(Hint: verify line numbers or specify start_line/end_line to replace directly)",
            show_start + 1,
            show_end,
            best_score * 100.0,
            snippet
        ))
    } else {
        Some("[Content Mismatch]: Target old_string could not be located in the file. Please check line numbers or file contents.".to_string())
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
    async fn gbk_file_edits_in_place_and_stays_gbk() {
        // A GBK/GB18030-encoded file (common on Chinese Windows) must be editable
        // directly — matched in UTF-8 space, then written back in its ORIGINAL encoding,
        // never silently converted to UTF-8.
        let d = tempfile::tempdir().unwrap();
        let (gbk, _, had_err) = encoding_rs::GB18030.encode("第一行\n第二行\n第三行\n");
        assert!(!had_err);
        std::fs::write(d.path().join("notes.txt"), &gbk[..]).unwrap();

        let r = EditFileTool
            .execute(
                r#"{"file_path":"notes.txt","old_string":"第二行","new_string":"改过的第二行"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);

        let on_disk = std::fs::read(d.path().join("notes.txt")).unwrap();
        // Still GBK: the Chinese bytes are not valid UTF-8, and decode as GB18030.
        assert!(
            std::str::from_utf8(&on_disk).is_err(),
            "file must stay GBK, not be converted to UTF-8"
        );
        let (decoded, _, had_err) = encoding_rs::GB18030.decode(&on_disk);
        assert!(!had_err);
        assert_eq!(decoded, "第一行\n改过的第二行\n第三行\n");
    }

    #[tokio::test]
    async fn ambiguous_non_utf8_file_is_refused_and_left_untouched() {
        // A non-UTF-8 file that does not losslessly round-trip as GB18030 (here a stray
        // 0x80 byte) must be refused rather than corrupted — the file stays byte-identical.
        let d = tempfile::tempdir().unwrap();
        let mut bytes = b"plain text\n".to_vec();
        bytes.push(0x80);
        bytes.extend_from_slice(b"\n");
        std::fs::write(d.path().join("weird.txt"), &bytes).unwrap();

        let r = EditFileTool
            .execute(
                r#"{"file_path":"weird.txt","old_string":"plain","new_string":"changed"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(r.is_error, "{}", r.content);
        assert!(r.content.contains("UTF-8"), "{}", r.content);
        assert_eq!(
            std::fs::read(d.path().join("weird.txt")).unwrap(),
            bytes,
            "refused edit must leave the file byte-identical"
        );
    }

    #[tokio::test]
    async fn unique_replace_succeeds() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn main() {\n    let x = 1;\n}\n").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("-    let x = 1;"), "{}", r.content);
        assert!(r.content.contains("+    let x = 2;"), "{}", r.content);
        let on_disk = std::fs::read_to_string(d.path().join("a.rs")).unwrap();
        assert!(on_disk.contains("let x = 2;"), "{on_disk}");
    }

    #[tokio::test]
    async fn edits_array_applies_two_hunks_transactionally() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.rs"),
            "fn a() { 1 }\nfn b() { 2 }\nfn c() { 3 }\n",
        )
        .unwrap();
        let ok = EditFileTool
            .execute(
                r#"{"file_path":"a.rs","edits":[{"old_string":"fn a() { 1 }","new_string":"fn a() { 10 }"},{"old_string":"fn c() { 3 }","new_string":"fn c() { 30 }"}]}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!ok.is_error, "{}", ok.content);
        let on_disk = std::fs::read_to_string(d.path().join("a.rs")).unwrap();
        assert_eq!(on_disk, "fn a() { 10 }\nfn b() { 2 }\nfn c() { 30 }\n");

        let fail = EditFileTool
            .execute(
                r#"{"file_path":"a.rs","edits":[{"old_string":"fn a() { 10 }","new_string":"fn a() { 11 }"},{"old_string":"missing","new_string":"x"}]}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(fail.is_error, "{}", fail.content);
        assert!(fail.content.contains("hunk 2/2"), "{}", fail.content);
        let still = std::fs::read_to_string(d.path().join("a.rs")).unwrap();
        assert_eq!(still, "fn a() { 10 }\nfn b() { 2 }\nfn c() { 30 }\n");
    }

    #[tokio::test]
    async fn javadoc_wrapping_matches_single_line_comment() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Coupon.java"),
            "public class Coupon {\n    /** 创建时间 */\n    private LocalDateTime createTime;\n}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "file_path": "Coupon.java",
            "old_string": "    /**\n     * 创建时间\n     */\n    private LocalDateTime createTime;",
            "new_string": "    /**\n     * 创建时间\n     */\n    private LocalDateTime createTime;\n\n    /** 过期提前预警天数 */\n    private Integer expireWarningDays;"
        });
        let r = EditFileTool
            .execute(&args.to_string(), &ctx(d.path()))
            .await;
        assert!(!r.is_error, "javadoc wrap must match: {}", r.content);
        let on_disk = std::fs::read_to_string(d.path().join("Coupon.java")).unwrap();
        assert!(on_disk.contains("expireWarningDays"), "{on_disk}");
        assert!(
            on_disk.contains("createTime"),
            "must keep existing field: {on_disk}"
        );
    }

    #[tokio::test]
    async fn hallucinated_annotation_does_not_clobber_other_field() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Coupon.java"),
            "public class Coupon {\n    /** 创建时间 */\n    private LocalDateTime createTime;\n\n    /** 过期提前预警天数 (默认3天) */\n    private Integer expireWarningDays;\n}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "file_path": "Coupon.java",
            "old_string": "    /**\n     * 创建时间\n     */\n    @TableField(\"create_time\")\n    private LocalDateTime createTime;\n}",
            "new_string": "    /**\n     * 过期提前预警天数 (默认为3天)\n     */\n    @TableField(\"expire_warning_days\")\n    private Integer expireWarningDays;\n}"
        });
        let r = EditFileTool
            .execute(&args.to_string(), &ctx(d.path()))
            .await;
        assert!(
            r.is_error,
            "must refuse a structurally different old_string: {}",
            r.content
        );
        assert!(r.content.contains("not found"), "{}", r.content);
        let on_disk = std::fs::read_to_string(d.path().join("Coupon.java")).unwrap();
        assert!(
            on_disk.contains("createTime"),
            "createTime must survive: {on_disk}"
        );
        assert!(
            on_disk.matches("expireWarningDays").count() == 1,
            "must not duplicate expireWarningDays: {on_disk}"
        );
    }

    #[test]
    fn compact_diff_is_unified_with_line_numbers() {
        // Whole-file old vs new; a real diff must produce a `@@` hunk header whose
        // new-side start reflects the changed line's position in the file.
        let old = "fn main() {\n    let x = 1;\n}\n";
        let new = "fn main() {\n    let x = 2;\n}\n";
        let diff = build_compact_diff(old, new);
        assert!(
            diff.contains("@@"),
            "must be a unified diff with a hunk header: {diff}"
        );
        assert!(
            diff.contains("-    let x = 1;"),
            "removed line present: {diff}"
        );
        assert!(
            diff.contains("+    let x = 2;"),
            "added line present: {diff}"
        );
        // The change is on file line 2, which falls within lines 1-3 shown in the hunk header.
        assert!(
            diff.contains("@@ -1,3 +1,3 @@"),
            "hunk header shows lines 1-3: {diff}"
        );
    }

    #[test]
    fn compact_diff_caps_huge_diffs() {
        let old = String::new();
        let new: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let diff = build_compact_diff(&old, &new);
        assert!(
            diff.lines().count() <= 61,
            "capped: {} lines",
            diff.lines().count()
        );
        assert!(
            diff.contains("more diff lines"),
            "shows a truncation note: {diff}"
        );
    }

    #[tokio::test]
    async fn ambiguous_match_refuses() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "dup\ndup\n").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"a.txt","old_string":"dup","new_string":"x"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(r.is_error, "{}", r.content);
        assert!(r.content.contains("appears 2 times"), "{}", r.content);
        // file unchanged
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "dup\ndup\n"
        );
    }

    #[tokio::test]
    async fn replace_all_handles_duplicates() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "dup\ndup\ndup\n").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"a.txt","old_string":"dup","new_string":"x","replace_all":true}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("3 replacements"), "{}", r.content);
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "x\nx\nx\n"
        );
    }

    #[tokio::test]
    async fn missing_string_errors_and_keeps_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "hello\n").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"a.txt","old_string":"absent","new_string":"x"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(r.is_error, "{}", r.content);
        assert!(r.content.contains("not found"), "{}", r.content);
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "hello\n"
        );
    }

    #[tokio::test]
    async fn edit_is_risky() {
        assert_eq!(EditFileTool.risk("{}"), RiskLevel::Risky);
    }

    // A CRLF (Windows) file edited with a multi-line `old_string` whose line break is
    // `\n` — which is exactly what read_file shows the model, because read_file does
    // `text.lines()` and strips the `\r`. The edit must still succeed, and the file must
    // stay CRLF (no mixed line endings introduced).
    #[tokio::test]
    async fn crlf_file_matches_lf_oldstring_and_preserves_crlf() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("router.js"),
            "  path: '/help',\r\n  next: 1,\r\n",
        )
        .unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"router.js","old_string":"  path: '/help',\n  next: 1,","new_string":"  path: '/proxyCase',\n  next: 1,"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            !r.is_error,
            "CRLF file must match an LF old_string: {}",
            r.content
        );
        let on_disk = std::fs::read_to_string(d.path().join("router.js")).unwrap();
        assert_eq!(
            on_disk, "  path: '/proxyCase',\r\n  next: 1,\r\n",
            "must stay CRLF: {on_disk:?}"
        );
    }

    // A literal match must write new_string VERBATIM — never coerce its line endings.
    // Here a mostly-LF file has one stray CRLF line; editing an LF region must NOT force
    // the replacement to CRLF (that would inject mixed endings, the opposite of intent).
    #[tokio::test]
    async fn literal_match_writes_new_verbatim_no_crlf_injection() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("m.txt"), "head\r\nalpha\nbeta\n").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"m.txt","old_string":"alpha\nbeta","new_string":"alpha\nBETA"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        // The edited LF region stays LF; the unrelated CRLF line is untouched.
        assert_eq!(
            std::fs::read_to_string(d.path().join("m.txt")).unwrap(),
            "head\r\nalpha\nBETA\n"
        );
    }

    // old_string and new_string that differ ONLY by line-ending form collapse to the
    // same bytes after normalization → a no-op; it must be refused, not reported as a
    // successful edit.
    #[tokio::test]
    async fn eol_only_difference_is_rejected_as_noop() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("c.txt"), "a\r\nb\r\n").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"c.txt","old_string":"a\nb","new_string":"a\r\nb"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(r.is_error, "a no-op edit must be refused: {}", r.content);
        assert_eq!(
            std::fs::read_to_string(d.path().join("c.txt")).unwrap(),
            "a\r\nb\r\n",
            "unchanged"
        );
    }

    #[tokio::test]
    async fn empty_old_string_is_rejected() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "abc").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"a.txt","old_string":"","new_string":"X","replace_all":true}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            r.is_error,
            "empty old_string must be refused (would insert everywhere): {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "abc",
            "unchanged"
        );
    }

    #[tokio::test]
    async fn lf_file_is_unaffected_by_eol_tolerance() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "let x = 1;\nlet y = 2;\n").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"a.rs","old_string":"let x = 1;\nlet y = 2;","new_string":"let x = 9;\nlet y = 2;"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.rs")).unwrap(),
            "let x = 9;\nlet y = 2;\n"
        );
    }

    // The reported "改不动只能写脚本" case: the file is TAB-indented but the model
    // reproduced the body with SPACE indentation (read_file faithfully passes the tabs;
    // the model dropped them). Exact + EOL match both fail. The whitespace-normalized
    // fuzzy fallback must match line-by-line ignoring leading whitespace, and write back
    // using the file's REAL indentation (tabs preserved).
    #[tokio::test]
    async fn fuzzy_matches_tab_vs_space_indentation_and_preserves_tabs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("f.rs"),
            "fn f() {\n\tlet x = 1;\n\tlet y = 2;\n}\n",
        )
        .unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"f.rs","old_string":"    let x = 1;\n    let y = 2;","new_string":"    let x = 9;\n    let y = 2;"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            !r.is_error,
            "fuzzy whitespace match must succeed: {}",
            r.content
        );
        assert!(
            r.content.contains("line-trimmed") || r.content.contains("whitespace"),
            "should report a whitespace match: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("f.rs")).unwrap(),
            "fn f() {\n\tlet x = 9;\n\tlet y = 2;\n}\n",
            "the file's tab indentation must be preserved"
        );
    }

    // A fuzzy edit on a CRLF file must NOT rewrite the whole file to LF: `lines()`
    // strips every `\r`, so without restoring the file's EOL the entire file (incl.
    // untouched lines) would be silently downgraded to LF — a whole-file corruption.
    #[tokio::test]
    async fn fuzzy_match_preserves_crlf_line_endings() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("f.rs"),
            "fn f() {\r\n\tlet x = 1;\r\n\tlet y = 2;\r\n}\r\n",
        )
        .unwrap();
        // Model copied LF text (read_file strips \r) with SPACE indentation.
        let r = EditFileTool
            .execute(
                r#"{"file_path":"f.rs","old_string":"    let x = 1;\n    let y = 2;","new_string":"    let x = 9;\n    let y = 2;"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(
            std::fs::read_to_string(d.path().join("f.rs")).unwrap(),
            "fn f() {\r\n\tlet x = 9;\r\n\tlet y = 2;\r\n}\r\n",
            "CRLF must be preserved across the WHOLE file, not just the edited region"
        );
    }

    // Safety guard: a tiny fragment must NOT fuzzy-match (too ambiguous to be safe).
    #[tokio::test]
    async fn fuzzy_does_not_fire_for_short_fragments() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "\tx\n").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"a.txt","old_string":"  x","new_string":"  y"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            r.is_error,
            "a short fragment must not fuzzy-match: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "\tx\n",
            "unchanged"
        );
    }

    // Regression: indent arithmetic must count *characters*, not bytes. When the file
    // is indented with a multi-byte whitespace char (here U+3000 IDEOGRAPHIC SPACE,
    // 3 bytes / 1 char), the old byte-based `file_indent` fed into `chars().take(n)`
    // grabbed content chars into the indent prefix, producing corruption like
    // "\u{3000}x x = 99". The fix (leading_ws_chars) keeps exactly the whitespace.
    // BLOCK-ANCHOR tier: the model reproduced a multi-line block but got ONE interior
    // line slightly wrong (`let b = 20;` vs the file's `let b = 2;`) AND used spaces where
    // the file uses tabs. Exact + whitespace-normalized fuzzy both fail (fuzzy needs EVERY
    // trimmed line to match). Block-anchor matches on the first/last trimmed lines, replaces
    // the real window, and re-anchors to the file's tabs — so the model doesn't reach for sed.
    #[tokio::test]
    async fn block_anchor_matches_interior_drift_and_preserves_tabs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("f.rs"),
            "fn f() {\n\tlet a = 1;\n\tlet b = 2;\n\tlet c = 3;\n}\n",
        )
        .unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"f.rs","old_string":"    let a = 1;\n    let b = 20;\n    let c = 3;","new_string":"    let a = 1;\n    let b = 99;\n    let c = 3;"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "block-anchor must succeed: {}", r.content);
        assert!(
            r.content.contains("anchored block"),
            "should report an anchored match: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("f.rs")).unwrap(),
            "fn f() {\n\tlet a = 1;\n\tlet b = 99;\n\tlet c = 3;\n}\n",
            "the intended edit applies with the file's tab indentation preserved"
        );
    }

    // Guard: a block that merely SHARES its first/last line with an unrelated region (all
    // interior lines differ) must be REJECTED (< half match), not clobbered.
    #[tokio::test]
    async fn block_anchor_rejects_low_similarity_block() {
        let d = tempfile::tempdir().unwrap();
        let original = "start marker\nreal one\nreal two\nreal three\nend marker\n";
        std::fs::write(d.path().join("a.txt"), original).unwrap();
        let r = EditFileTool
            .execute(
                // first/last match, but all 3 interior lines are wrong → 2/5 < half → reject.
                r#"{"file_path":"a.txt","old_string":"start marker\nWRONG a\nWRONG b\nWRONG c\nend marker","new_string":"start marker\nX\nend marker"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            r.is_error,
            "a low-similarity block must be refused: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            original,
            "file must be unchanged"
        );
    }

    // Guard: at-most-ONE drifted line. A 4-line block whose BOTH interior lines differ
    // (only the anchors match) must be REJECTED — a plain "≥ half" rule would have passed
    // this (2/4), clobbering an unrelated region that happens to share first/last lines.
    #[tokio::test]
    async fn block_anchor_rejects_two_drifted_interior_lines() {
        let d = tempfile::tempdir().unwrap();
        let original = "region top\n\treal one\n\treal two\nregion bottom\n";
        std::fs::write(d.path().join("a.txt"), original).unwrap();
        let r = EditFileTool
            .execute(
                // first/last match; BOTH interior lines wrong → matched 2/4 → reject.
                r#"{"file_path":"a.txt","old_string":"region top\nWRONG one\nWRONG two\nregion bottom","new_string":"region top\nX\nregion bottom"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            r.is_error,
            "two drifted interior lines must be refused: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            original
        );
    }

    // Coverage: the OUTDENTED-line re-anchor path (`signed_relative < 0`) — a new line less
    // indented than the block's anchor (e.g. a top-level call after an indented statement).
    // The file uses tabs; the fuzzy tier matches and re-anchors, dropping indent for the
    // outdented line.
    #[tokio::test]
    async fn reanchor_handles_outdented_new_line() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("f.rs"),
            "fn f() {\n\tlet a = 1;\n\tlet b = 2;\n}\n",
        )
        .unwrap();
        let r = EditFileTool
            .execute(
                // Model copied with spaces; new_string's 2nd line is OUTDENTED to column 0.
                r#"{"file_path":"f.rs","old_string":"    let a = 1;\n    let b = 2;","new_string":"    let a = 1;\ndone();"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            !r.is_error,
            "outdented re-anchor must succeed: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("f.rs")).unwrap(),
            "fn f() {\n\tlet a = 1;\ndone();\n}\n",
            "the kept line stays tab-indented; the outdented line drops to column 0"
        );
    }

    // Guard: two windows share the same first/last anchors → ambiguous → refuse.
    #[tokio::test]
    async fn block_anchor_rejects_ambiguous_windows() {
        let d = tempfile::tempdir().unwrap();
        let original =
            "open block\n  middle here\nclose block\n\nopen block\n  other mid\nclose block\n";
        std::fs::write(d.path().join("a.txt"), original).unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"a.txt","old_string":"open block\n  drifted\nclose block","new_string":"open block\n  changed\nclose block"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            r.is_error,
            "ambiguous anchored windows must be refused: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            original
        );
    }

    // Guard: bare-brace anchors (`{` / `}`, < 3 trimmed chars) can't anchor a block.
    #[tokio::test]
    async fn block_anchor_ignores_short_anchors() {
        let d = tempfile::tempdir().unwrap();
        let original = "if x {\n\tfoo();\n}\n";
        std::fs::write(d.path().join("a.rs"), original).unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"a.rs","old_string":"{\n    bar();\n}","new_string":"{\n    baz();\n}"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            r.is_error,
            "short brace anchors must not fire: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.rs")).unwrap(),
            original
        );
    }

    #[tokio::test]
    async fn fuzzy_preserves_multibyte_whitespace_indentation() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("f.py"),
            "def f():\n\u{3000}x = 1\n\u{3000}y = 2\n",
        )
        .unwrap();
        // Model reproduced the body with plain-space indentation → exact match fails,
        // fuzzy path fires.
        let r = EditFileTool
            .execute(
                r#"{"file_path":"f.py","old_string":"    x = 1\n    y = 2","new_string":"    x = 99\n    y = 2"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "fuzzy match must succeed: {}", r.content);
        assert_eq!(
            std::fs::read_to_string(d.path().join("f.py")).unwrap(),
            "def f():\n\u{3000}x = 99\n\u{3000}y = 2\n",
            "the file's multi-byte whitespace indent must be preserved with no content leaking into it"
        );
    }

    #[tokio::test]
    async fn token_normalized_matches_invisible_unicode_and_inline_whitespace() {
        let d = tempfile::tempdir().unwrap();
        let content = "fn calculate_total(price: f64, tax_rate: f64) -> f64 {\n    let subtotal = price * (1.0 + tax_rate);\n    subtotal.round()\n}\n";
        std::fs::write(d.path().join("calc.rs"), content).unwrap();

        // Model emits with double spaces, NBSP, zero-width space, and smart quotes
        let old_str = "let  subtotal\u{200b} =\u{00a0}price * (1.0 + tax_rate);";
        let new_str = "let subtotal = price * (1.0 + tax_rate) + 5.0;";

        let r = EditFileTool
            .execute(
                &format!(
                    r#"{{"file_path":"calc.rs","old_string":{},"new_string":{}}}"#,
                    serde_json::to_string(old_str).unwrap(),
                    serde_json::to_string(new_str).unwrap()
                ),
                &ctx(d.path()),
            )
            .await;
        assert!(
            !r.is_error,
            "token normalized match must succeed: {}",
            r.content
        );
        assert!(r.content.contains("token-normalized match"));
        let updated = std::fs::read_to_string(d.path().join("calc.rs")).unwrap();
        assert!(updated.contains("let subtotal = price * (1.0 + tax_rate) + 5.0;"));
    }

    #[tokio::test]
    async fn boundary_trimmed_matches_when_llm_emits_extra_context_line() {
        let d = tempfile::tempdir().unwrap();
        let content = "fn main() {\n    let a = 10;\n    let b = 20;\n    let sum = a + b;\n    println!(\"{}\", sum);\n}\n";
        std::fs::write(d.path().join("main.rs"), content).unwrap();

        // Model copied leading context line "fn main() {" and modified sum line
        let old_str = "fn main() {\n    let a = 10;\n    let b = 20;\n    let sum = a + b;";
        let new_str = "fn main() {\n    let a = 10;\n    let b = 20;\n    let sum = a * b;";

        let r = EditFileTool
            .execute(
                &format!(
                    r#"{{"file_path":"main.rs","old_string":{},"new_string":{}}}"#,
                    serde_json::to_string(old_str).unwrap(),
                    serde_json::to_string(new_str).unwrap()
                ),
                &ctx(d.path()),
            )
            .await;
        assert!(
            !r.is_error,
            "boundary trimmed match must succeed: {}",
            r.content
        );
        let updated = std::fs::read_to_string(d.path().join("main.rs")).unwrap();
        assert!(updated.contains("let sum = a * b;"));
    }

    #[tokio::test]
    async fn not_found_returns_closest_match_snippet() {
        let d = tempfile::tempdir().unwrap();
        let content = "pub fn perform_action() {\n    let mut state = get_state();\n    state.validate_and_commit();\n}\n";
        std::fs::write(d.path().join("act.rs"), content).unwrap();

        let r = EditFileTool
            .execute(
                r#"{"file_path":"act.rs","old_string":"let state = get_state();\nstate.validate_and_rollback();","new_string":"let state = get_state();"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(r.is_error);
        assert!(
            r.content.contains("Closest matching block")
                || r.content.contains("[Content Mismatch]"),
            "{}",
            r.content
        );
    }

    #[tokio::test]
    async fn line_range_replace_succeeds_and_preserves_structure() {
        let d = tempfile::tempdir().unwrap();
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        std::fs::write(d.path().join("range.rs"), content).unwrap();

        let r = EditFileTool
            .execute(
                r#"{"file_path":"range.rs","start_line":2,"end_line":4,"new_string":"line TWO\nline THREE\nline FOUR"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(
            std::fs::read_to_string(d.path().join("range.rs")).unwrap(),
            "line 1\nline TWO\nline THREE\nline FOUR\nline 5\n"
        );
    }

    #[test]
    fn schema_advertises_edits_as_array_only() {
        let schema = EditFileTool.parameters_schema();
        let edits = &schema["properties"]["edits"];
        assert_eq!(
            edits["type"], "array",
            "schema must not advertise string|array: {edits}"
        );
        assert!(edits.get("oneOf").is_none());
        assert!(edits.get("anyOf").is_none());
    }

    #[tokio::test]
    async fn stringified_edits_array_is_accepted() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn a() { 1 }\nfn b() { 2 }\n").unwrap();
        let inner = r#"[{"old_string":"fn a() { 1 }","new_string":"fn a() { 10 }"}]"#;
        let args = serde_json::json!({
            "file_path": "a.rs",
            "edits": inner
        })
        .to_string();
        let r = EditFileTool.execute(&args, &ctx(d.path())).await;
        assert!(
            !r.is_error,
            "stringified edits must be accepted internally: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.rs")).unwrap(),
            "fn a() { 10 }\nfn b() { 2 }\n"
        );
    }

    #[tokio::test]
    async fn stringified_edits_with_raw_newlines_is_repaired() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "let x = 1;\nlet y = 2;\n").unwrap();
        // Outer JSON is valid; the edits *string* contains a real newline inside the
        // inner JSON snippet — the common provider/model double-encoding miss.
        let inner = "[{ \"old_string\": \"let x = 1;\nlet y = 2;\", \"new_string\": \"let x = 9;\nlet y = 2;\" }]";
        let args = serde_json::json!({
            "file_path": "a.rs",
            "edits": inner
        })
        .to_string();
        let r = EditFileTool.execute(&args, &ctx(d.path())).await;
        assert!(
            !r.is_error,
            "repair_json must salvage inner newlines: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.rs")).unwrap(),
            "let x = 9;\nlet y = 2;\n"
        );
    }

    #[tokio::test]
    async fn unique_old_string_ignores_stale_line_range() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("cfg.yaml"), "alpha: 1\nbeta: 2\ngamma: 3\n").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"cfg.yaml","start_line":99,"end_line":100,"old_string":"beta: 2","new_string":"beta: 20"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            !r.is_error,
            "unique text must win over stale line numbers: {}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(d.path().join("cfg.yaml")).unwrap(),
            "alpha: 1\nbeta: 20\ngamma: 3\n"
        );
    }

    #[tokio::test]
    async fn multi_hunk_original_line_numbers_after_insertion() {
        // Repro: first hunk grows the file; second hunk still cites original-file
        // line numbers (how models think after one read). Internal offset must
        // retarget the later range, and old_string must still win if present.
        let d = tempfile::tempdir().unwrap();
        let original = [
            "# header",
            "keep",
            "# block-a",
            "a1",
            "a2",
            "# block-b",
            "  jeikcode:",
            "    protocol: openai_chat",
            "# footer",
            "",
        ]
        .join("\n");
        std::fs::write(d.path().join("config.yaml"), original).unwrap();

        let hunk1_new = "# block-a\na1\na2\na3\na4\na5\n";
        let args = serde_json::json!({
            "file_path": "config.yaml",
            "edits": [
                {
                    "start_line": 3,
                    "end_line": 5,
                    "old_string": "# block-a\na1\na2",
                    "new_string": hunk1_new.trim_end()
                },
                {
                    "start_line": 7,
                    "end_line": 8,
                    "old_string": "  jeikcode:\n    protocol: openai_chat",
                    "new_string": "  jeikcode:\n    protocol: openai_chat\n    # extra"
                }
            ]
        });
        let r = EditFileTool
            .execute(&args.to_string(), &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        let on_disk = std::fs::read_to_string(d.path().join("config.yaml")).unwrap();
        assert!(
            on_disk.contains("a5\n# block-b\n  jeikcode:"),
            "second hunk must not splice into the grown first block:\n{on_disk}"
        );
        assert!(on_disk.contains("    # extra"), "{on_disk}");
        assert!(on_disk.contains("# footer"), "{on_disk}");
    }

    #[tokio::test]
    async fn multi_hunk_line_range_only_applies_original_coordinates() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("f.txt"), "L1\nL2\nL3\nL4\nL5\nL6\nL7\n").unwrap();
        // Replace L2-L3 (2 lines) with 4 lines (+2). Original L6 should land on
        // current L8 after the insertion — without offset it would hit L6 (old L4).
        let args = serde_json::json!({
            "file_path": "f.txt",
            "edits": [
                {"start_line": 2, "end_line": 3, "new_string": "A\nB\nC\nD"},
                {"start_line": 6, "end_line": 6, "new_string": "SIX"}
            ]
        });
        let r = EditFileTool
            .execute(&args.to_string(), &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(
            std::fs::read_to_string(d.path().join("f.txt")).unwrap(),
            "L1\nA\nB\nC\nD\nL4\nL5\nSIX\nL7\n"
        );
    }

    #[tokio::test]
    async fn line_hint_disambiguates_duplicate_old_string() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "dup\nkeep\ndup\n").unwrap();
        let r = EditFileTool
            .execute(
                r#"{"file_path":"a.txt","old_string":"dup","new_string":"x","start_line":3}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "dup\nkeep\nx\n"
        );
    }

    #[test]
    fn orig_line_map_shifts_later_ranges_only() {
        let mut m = OrigLineMap::default();
        m.record(10, 23, 36); // 14 lines → 36, delta +22
        assert_eq!(m.map(10), 10, "hunk start itself is unshifted");
        assert_eq!(m.map(46), 68);
        assert_eq!(m.unmap(68), 46);
        assert_eq!(m.map(5), 5, "earlier lines stay put");
    }
}
