//! Model-facing tool feedback helpers — structured parameter errors and
//! path-not-found enrichment ("Did you mean?").
//!
//! Inspired by Grok Build's `params_validation` + path suggestions and
//! OpenCode's clear `InvalidArgumentsError` rewrite prompts. Goal: when the
//! model emits bad tool args or wrong paths, feed back **actionable** detail
//! so the next turn fixes the call instead of retrying blindly.

use atomcode_kernel::tool::ToolResult;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};

// ── Structured invalid-arguments ──────────────────────────────────────────

/// Category of a tool-argument failure (model-facing, stable strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgErrorCategory {
    /// JSON itself is not parseable.
    ParseError,
    /// A required field is absent.
    MissingField,
    /// A field has the wrong JSON type.
    TypeMismatch,
    /// An unexpected extra field (when the schema rejects unknowns).
    UnknownField,
    /// Catch-all for other serde validation failures.
    InvalidValue,
}

impl ArgErrorCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ParseError => "parse_error",
            Self::MissingField => "missing_field",
            Self::TypeMismatch => "type_mismatch",
            Self::UnknownField => "unknown_field",
            Self::InvalidValue => "invalid_value",
        }
    }
}

/// Structured argument error returned to the model as a tool result.
#[derive(Debug, Clone)]
pub struct ParamError {
    pub tool: String,
    pub message: String,
    pub category: ArgErrorCategory,
    pub field_path: Option<String>,
    pub expected: Option<String>,
    pub bad_value: Option<Value>,
    /// Compact schema hint, e.g. `{"file_path":"<path>","content":"<text>"}`.
    pub expected_shape: String,
}

impl ParamError {
    /// Render a multi-line, model-actionable tool error.
    pub fn format_for_model(&self) -> String {
        let mut out = format!(
            "{}: invalid arguments [{}]\n  detail: {}",
            self.tool,
            self.category.as_str(),
            self.message
        );
        if let Some(ref field) = self.field_path {
            out.push_str(&format!("\n  field: {field}"));
        }
        if let Some(ref expected) = self.expected {
            out.push_str(&format!("\n  expected: {expected}"));
        }
        if let Some(ref bad) = self.bad_value {
            let rendered = serde_json::to_string(bad).unwrap_or_else(|_| bad.to_string());
            let clipped = if rendered.len() > 120 {
                format!("{}…", &rendered[..117])
            } else {
                rendered
            };
            out.push_str(&format!("\n  bad_value: {clipped}"));
        }
        out.push_str(&format!("\n  expected_shape: {}", self.expected_shape));
        out.push_str(
            "\nPlease rewrite the tool input so it satisfies the expected schema, then retry once.",
        );
        out
    }

    pub fn into_tool_result(self) -> ToolResult {
        ToolResult {
            call_id: String::new(),
            content: self.format_for_model(),
            is_error: true,
            images: vec![],
        }
    }
}

/// Parse tool arguments into `T`, returning a structured model-facing error on failure.
pub fn parse_tool_args<T: DeserializeOwned>(
    tool: &str,
    args: &str,
    expected_shape: &str,
) -> Result<T, ParamError> {
    match serde_json::from_str::<T>(args) {
        Ok(v) => Ok(v),
        Err(e) => Err(build_param_error(tool, args, expected_shape, &e)),
    }
}

fn build_param_error(
    tool: &str,
    args: &str,
    expected_shape: &str,
    err: &serde_json::Error,
) -> ParamError {
    let message = err.to_string();
    let category = classify_serde_message(&message);
    let field_path = extract_field_path(&message);
    let expected = extract_expected(&message);
    let bad_value = field_path
        .as_deref()
        .and_then(|p| value_at_path(args, p))
        .or_else(|| {
            // For top-level parse failures, show a clipped raw snippet.
            if category == ArgErrorCategory::ParseError {
                let t = args.trim();
                if t.is_empty() {
                    Some(Value::String(String::new()))
                } else {
                    let clipped = if t.len() > 80 {
                        format!("{}…", &t[..77])
                    } else {
                        t.to_string()
                    };
                    Some(Value::String(clipped))
                }
            } else {
                None
            }
        });

    ParamError {
        tool: tool.to_string(),
        message,
        category,
        field_path,
        expected,
        bad_value,
        expected_shape: expected_shape.to_string(),
    }
}

fn classify_serde_message(message: &str) -> ArgErrorCategory {
    let lower = message.to_ascii_lowercase();
    if lower.contains("missing field") {
        ArgErrorCategory::MissingField
    } else if lower.contains("unknown field") {
        ArgErrorCategory::UnknownField
    } else if lower.contains("invalid type") || lower.contains("invalid number") {
        ArgErrorCategory::TypeMismatch
    } else if lower.contains("eof while parsing")
        || lower.contains("expected value")
        || lower.contains("trailing characters")
        || lower.contains("key must be a string")
        || lower.contains("control character")
        || (lower.contains("expected") && lower.contains("at line"))
    {
        ArgErrorCategory::ParseError
    } else {
        ArgErrorCategory::InvalidValue
    }
}

/// Pull `missing field \`foo\`` / `unknown field \`bar\`` style names out of serde text.
fn extract_field_path(message: &str) -> Option<String> {
    for marker in ["missing field `", "unknown field `", "missing field \"", "unknown field \""] {
        if let Some(rest) = message.find(marker).map(|i| &message[i + marker.len()..]) {
            let end = rest
                .find(['`', '"', ',', ' ', '\n'])
                .unwrap_or(rest.len());
            let name = rest[..end].trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    // `invalid type: …, expected a string` has no field; leave None.
    None
}

fn extract_expected(message: &str) -> Option<String> {
    // "invalid type: integer `1`, expected a string"
    if let Some(idx) = message.find("expected ") {
        let rest = message[idx + "expected ".len()..].trim();
        let end = rest.find(" at line").unwrap_or(rest.len());
        let expected = rest[..end].trim().trim_end_matches('.');
        if !expected.is_empty() {
            return Some(expected.to_string());
        }
    }
    None
}

fn value_at_path(args: &str, field: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(args).ok()?;
    match v {
        Value::Object(map) => map.get(field).cloned(),
        _ => None,
    }
}

// ── Path-not-found enrichment ─────────────────────────────────────────────

const MAX_SIMILAR: usize = 5;
const MIN_LEAF_LEN: usize = 2;
const MIN_REVERSE_STEM_LEN: usize = 4;

/// Enrichment for a path that does not exist on disk.
#[derive(Debug, Clone)]
pub struct PathNotFoundHint {
    pub suggestion: Option<PathBuf>,
    pub similar: Vec<PathBuf>,
    pub cwd_note: String,
}

impl PathNotFoundHint {
    /// Suffix to append after a "does not exist" line.
    pub fn format_suffix(&self) -> String {
        let mut out = String::new();
        if let Some(ref s) = self.suggestion {
            out.push_str(&format!(
                "\nDid you mean {}?",
                crate::pathnorm::to_display(s)
            ));
        } else if !self.similar.is_empty() {
            let names: Vec<String> = self
                .similar
                .iter()
                .map(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| crate::pathnorm::to_display(p))
                })
                .collect();
            out.push_str(&format!(
                "\nDid you mean one of these?\n  - {}",
                names.join("\n  - ")
            ));
        }
        out.push_str(&format!("\n{}", self.cwd_note));
        out
    }
}

/// Build path-not-found hints for `resolved` (absolute or joined path).
pub fn path_not_found_hint(resolved: &Path, cwd: &Path) -> PathNotFoundHint {
    let cwd_note = format!(
        "Note: your current working directory is {}",
        crate::pathnorm::to_display(cwd)
    );
    let (suggestion, similar) = collect_path_hints(resolved, cwd);
    PathNotFoundHint {
        suggestion,
        similar,
        cwd_note,
    }
}

/// Full model-facing "path does not exist" error with Did-you-mean enrichment.
pub fn format_path_not_found(tool: &str, display: &str, resolved: &Path, cwd: &Path) -> String {
    let base = format!(
        "{tool}: path does not exist: {display} (resolved to {})",
        crate::pathnorm::to_display(resolved)
    );
    let hint = path_not_found_hint(resolved, cwd);
    format!("{base}{}", hint.format_suffix())
}

fn collect_path_hints(path: &Path, cwd: &Path) -> (Option<PathBuf>, Vec<PathBuf>) {
    if let Some(corrected) = try_suggest_under_cwd(path, cwd) {
        return (Some(corrected), Vec::new());
    }
    // Relative typo: try under cwd with the leaf name only.
    if let Some(leaf) = path.file_name() {
        let under_cwd = cwd.join(leaf);
        if under_cwd.exists() && under_cwd != path {
            return (Some(under_cwd), Vec::new());
        }
    }
    (None, find_similar_entries(path, cwd))
}

/// If the model dropped the repo folder (`/parent/foo` while cwd is `/parent/repo`),
/// suggest `/parent/repo/foo` when it exists.
fn try_suggest_under_cwd(path: &Path, cwd: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || path.starts_with(cwd) {
        return None;
    }
    let cwd_parent = cwd.parent()?;
    let rel_from_parent = path.strip_prefix(cwd_parent).ok()?;
    if let Some(std::path::Component::Normal(first)) = rel_from_parent.components().next() {
        let sibling = cwd_parent.join(first);
        if sibling != *cwd && sibling.exists() {
            return None;
        }
    }
    let candidate = cwd.join(rel_from_parent);
    candidate.exists().then_some(candidate)
}

fn find_similar_entries(path: &Path, cwd: &Path) -> Vec<PathBuf> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty() && p.exists())
        .unwrap_or(cwd);
    let base = match path.file_name().and_then(|n| n.to_str()) {
        Some(b) if b.len() >= MIN_LEAF_LEN => b.to_lowercase(),
        _ => return Vec::new(),
    };
    let base_stem = Path::new(&base)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&base)
        .to_lowercase();

    let Ok(read_dir) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name == base {
            continue;
        }
        let name_stem = Path::new(&name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&name)
            .to_lowercase();
        let forward = name_stem.contains(&base_stem) || name.contains(&base);
        let reverse = !forward
            && name_stem.len() >= MIN_REVERSE_STEM_LEN
            && base_stem.contains(&name_stem);
        if forward || reverse {
            matches.push(entry.path());
            if matches.len() >= MAX_SIMILAR {
                break;
            }
        }
    }
    // Prefer shorter names (closer typos) then alpha.
    matches.sort_by(|a, b| {
        let la = a.file_name().map(|n| n.len()).unwrap_or(0);
        let lb = b.file_name().map(|n| n.len()).unwrap_or(0);
        la.cmp(&lb)
            .then_with(|| a.file_name().cmp(&b.file_name()))
    });
    matches.truncate(MAX_SIMILAR);
    matches
}

/// Suggest similar symbol names from a name index when an exact lookup misses.
pub fn similar_symbol_names<'a>(
    wanted: &str,
    known: impl Iterator<Item = &'a str>,
    max: usize,
) -> Vec<String> {
    let want = wanted.to_lowercase();
    if want.len() < 2 {
        return Vec::new();
    }
    let mut scored: Vec<(i32, String)> = Vec::new();
    for name in known {
        let lower = name.to_lowercase();
        if lower == want {
            continue;
        }
        let score = if lower.contains(&want) || want.contains(&lower) {
            3
        } else if lower.starts_with(&want) || want.starts_with(&lower) {
            2
        } else {
            // cheap char-overlap heuristic
            let overlap = want.chars().filter(|c| lower.contains(*c)).count();
            if overlap * 2 >= want.len() && overlap * 2 >= lower.len().min(12) {
                1
            } else {
                0
            }
        };
        if score > 0 {
            scored.push((score, name.to_string()));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.dedup_by(|a, b| a.1 == b.1);
    scored.into_iter().take(max).map(|(_, n)| n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Sample {
        #[allow(dead_code)]
        file_path: String,
        #[allow(dead_code)]
        content: String,
    }

    #[test]
    fn missing_field_is_structured() {
        let err = parse_tool_args::<Sample>("write_file", r#"{"file_path":"a"}"#, r#"{"file_path","content"}"#)
            .unwrap_err();
        assert_eq!(err.category, ArgErrorCategory::MissingField);
        assert_eq!(err.field_path.as_deref(), Some("content"));
        let text = err.format_for_model();
        assert!(text.contains("missing_field"), "{text}");
        assert!(text.contains("rewrite the tool input"), "{text}");
    }

    #[test]
    fn type_mismatch_reports_bad_value() {
        let err = parse_tool_args::<Sample>(
            "write_file",
            r#"{"file_path":1,"content":"x"}"#,
            r#"{"file_path":"<path>","content":"<text>"}"#,
        )
        .unwrap_err();
        assert_eq!(err.category, ArgErrorCategory::TypeMismatch);
        assert!(err.bad_value.is_some() || err.expected.is_some());
    }

    #[test]
    fn path_hint_finds_similar_leaf() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("CouponService.cs"), "class X {}").unwrap();
        let missing = d.path().join("CouponServic.cs"); // typo
        let hint = path_not_found_hint(&missing, d.path());
        let names: Vec<_> = hint
            .similar
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert!(
            names.iter().any(|n| n == "CouponService.cs"),
            "similar should include CouponService.cs, got {names:?}"
        );
    }

    #[test]
    fn similar_symbols_match_substring() {
        let known = ["CouponService", "OrderService", "ApplyCoupon", "User"];
        let hits = similar_symbol_names("coupon", known.into_iter(), 5);
        assert!(hits.iter().any(|h| h.contains("Coupon")), "{hits:?}");
    }
}
