//! In-memory version history and 3-Way Auto-Rebase state machine for `edit_file`.
//!
//! Provides a sliding `VersionRing` (up to 8 historical snapshots per canonical file)
//! to recover from cross-turn "Context Time-Travel" mismatches in rapid test/debug loops.
//! When a model's `old_string` fails against the current on-disk content ($V_{current}$)
//! but matches a recent historical version ($V_{old}$), a 3-way line merge attempts to
//! automatically rebase and apply the edit onto $V_{current}$ if the intervening changes
//! do not conflict.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Maximum historical snapshots retained per file in memory.
pub const MAX_VERSIONS_PER_FILE: usize = 32;

#[derive(Debug, Clone)]
pub struct FileVersion {
    pub content: String,
    pub timestamp: std::time::Instant,
}

#[derive(Debug, Default)]
pub struct VersionRing {
    /// Pinned initial base ($V_0$): the very first version of the file observed in this session.
    /// Never evicted, so models referencing the original file content can always 3-way rebase.
    initial_base: Option<FileVersion>,
    /// Sliding window of recent mutations ($V_1..V_k$).
    versions: VecDeque<FileVersion>,
}

impl VersionRing {
    pub fn push(&mut self, content: String) {
        if self.initial_base.is_none() {
            self.initial_base = Some(FileVersion {
                content: content.clone(),
                timestamp: std::time::Instant::now(),
            });
        }
        if self
            .versions
            .back()
            .map(|v| v.content == content)
            .unwrap_or(false)
        {
            return;
        }
        if self.versions.len() >= MAX_VERSIONS_PER_FILE {
            self.versions.pop_front();
        }
        self.versions.push_back(FileVersion {
            content,
            timestamp: std::time::Instant::now(),
        });
    }

    /// Iterates through historical versions from newest to oldest.
    /// Guarantees that `initial_base` is always present at the end even after many revisions.
    pub fn all_versions_reverse(&self) -> Vec<String> {
        let mut out = Vec::new();
        for v in self.versions.iter().rev() {
            out.push(v.content.clone());
        }
        if let Some(base) = &self.initial_base {
            if !out.iter().any(|c| c == &base.content) {
                out.push(base.content.clone());
            }
        }
        out
    }
}

static FILE_HISTORY: LazyLock<Mutex<HashMap<PathBuf, VersionRing>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record a version snapshot for `path`.
pub fn record_version(path: &Path, content: &str) {
    let Ok(canonical) = path.canonicalize().or_else(|_| Ok::<_, std::io::Error>(path.to_path_buf())) else {
        return;
    };
    let mut map = FILE_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(canonical).or_default().push(content.to_string());
}

/// Clear history for a file (useful in tests).
#[cfg(test)]
pub fn clear_history(path: &Path) {
    let Ok(canonical) = path.canonicalize().or_else(|_| Ok::<_, std::io::Error>(path.to_path_buf())) else {
        return;
    };
    let mut map = FILE_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    map.remove(&canonical);
}

/// Result of a successful 3-way rebase.
#[derive(Debug, Clone)]
pub struct RebaseSuccess {
    /// The merged file content after rebasing the edit onto $V_{current}$.
    pub merged_content: String,
    /// The actual text in $V_{current}$ corresponding to the rebased region.
    pub actual_old_string: String,
}

/// Attempt to rebase an edit that failed on `current` by searching historical versions of `path`.
///
/// If `old_string` matches in an older version $V_{old}$, and the model's change on $V_{old}$
/// does not conflict with lines modified between $V_{old}$ and $V_{current}$, this returns
/// the rebased merged content and the actual text in $V_{current}$.
pub fn try_history_rebase(
    path: &Path,
    current: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Option<RebaseSuccess> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let history: Vec<String> = {
        let map = FILE_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
        let ring = map.get(&canonical)?;
        ring.all_versions_reverse()
    };

    for base in &history {
        if base == current {
            continue; // Already tried against current
        }

        // Check if old_string matches on this historical base
        if let Ok((theirs, _count, _kind, _actual)) =
            super::edit::apply_hunk_direct(base, old_string, new_string, replace_all)
        {
            if theirs == *base {
                continue;
            }
            if let Some(res) = perform_3way_rebase(base, current, &theirs) {
                return Some(res);
            }
        }
    }

    None
}

/// Performs line-based 3-way merge between:
/// - `base`: common historical ancestor ($V_{old}$)
/// - `ours`: current on-disk content ($V_{current}$)
/// - `theirs`: historical ancestor with model's edit applied ($V_{branch}$)
pub fn perform_3way_rebase(base: &str, ours: &str, theirs: &str) -> Option<RebaseSuccess> {
    let base_lines: Vec<&str> = base.lines().collect();
    let ours_lines: Vec<&str> = ours.lines().collect();
    let theirs_lines: Vec<&str> = theirs.lines().collect();

    // 1. Identify what `theirs` changed relative to `base`.
    let diff_bt = similar::TextDiff::configure()
        .algorithm(similar::Algorithm::Patience)
        .diff_lines(base, theirs);

    let mut theirs_changes = Vec::new(); // Vec<(base_start, base_end, replacement_lines)>
    for op in diff_bt.ops() {
        match *op {
            similar::DiffOp::Equal { .. } => {}
            similar::DiffOp::Delete { old_index, old_len, .. } => {
                theirs_changes.push((old_index, old_index + old_len, Vec::<&str>::new()));
            }
            similar::DiffOp::Insert { old_index, new_index, new_len } => {
                let repl = theirs_lines[new_index..new_index + new_len].to_vec();
                theirs_changes.push((old_index, old_index, repl));
            }
            similar::DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                let repl = theirs_lines[new_index..new_index + new_len].to_vec();
                theirs_changes.push((old_index, old_index + old_len, repl));
            }
        }
    }

    if theirs_changes.is_empty() {
        return None;
    }

    // 2. Diff `base` vs `ours` to map line coordinates and detect conflicts.
    let diff_bo = similar::TextDiff::configure()
        .algorithm(similar::Algorithm::Patience)
        .diff_lines(base, ours);

    // Build a map of base line index -> ours line index and check for conflicts.
    // We check if any ours change overlaps with any theirs_changes.
    for &(t_start, t_end, _) in &theirs_changes {
        for op in diff_bo.ops() {
            match *op {
                similar::DiffOp::Equal { .. } => {}
                similar::DiffOp::Delete { old_index, old_len, .. } => {
                    let o_start = old_index;
                    let o_end = old_index + old_len;
                    if ranges_overlap(t_start, t_end, o_start, o_end) {
                        return None; // Conflict: both modified the same lines
                    }
                }
                similar::DiffOp::Insert { old_index, .. } => {
                    // An insert in ours strictly inside (t_start, t_end) conflicts
                    if t_start < t_end && old_index > t_start && old_index < t_end {
                        return None;
                    }
                }
                similar::DiffOp::Replace { old_index, old_len, .. } => {
                    let o_start = old_index;
                    let o_end = old_index + old_len;
                    if ranges_overlap(t_start, t_end, o_start, o_end) {
                        return None; // Conflict: both modified the same lines
                    }
                }
            }
        }
    }

    // 3. Map `theirs_changes` coordinates from `base` into `ours`.
    // For each (t_start, t_end), find corresponding (o_start, o_end) in `ours`.
    let mut mapped_changes = Vec::new();
    for (t_start, t_end, repl) in theirs_changes {
        let (o_start, o_end) = map_base_range_to_ours(t_start, t_end, diff_bo.ops(), base_lines.len(), ours_lines.len())?;
        mapped_changes.push((o_start, o_end, repl));
    }

    // Sort mapped changes by o_start descending so we can apply them safely from bottom to top
    mapped_changes.sort_by(|a, b| b.0.cmp(&a.0));

    let mut result_lines: Vec<String> = ours_lines.iter().map(|l| l.to_string()).collect();
    let mut actual_parts = Vec::new();

    for (o_start, o_end, repl) in mapped_changes {
        if o_start <= o_end && o_end <= result_lines.len() {
            let actual = result_lines[o_start..o_end].join("\n");
            actual_parts.push(actual);
            let replacement_strings: Vec<String> = repl.iter().map(|s| s.to_string()).collect();
            result_lines.splice(o_start..o_end, replacement_strings);
        } else {
            return None;
        }
    }

    let has_trailing_newline = ours.ends_with('\n');
    let mut merged = result_lines.join("\n");
    if has_trailing_newline && !merged.ends_with('\n') {
        merged.push('\n');
    }
    if ours.contains("\r\n") {
        merged = super::edit::coerce_eol(&merged, "\r\n");
    }

    let actual_old_string = if actual_parts.is_empty() {
        String::new()
    } else {
        actual_parts.join("\n---\n")
    };

    Some(RebaseSuccess {
        merged_content: merged,
        actual_old_string,
    })
}

fn ranges_overlap(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    if a_start == a_end {
        // Point insertion at a_start: overlaps if inside (b_start, b_end) or at b_start when b is replacement
        return a_start >= b_start && a_start < b_end;
    }
    if b_start == b_end {
        return b_start >= a_start && b_start < a_end;
    }
    a_start < b_end && b_start < a_end
}

fn map_base_range_to_ours(
    base_start: usize,
    base_end: usize,
    ops: &[similar::DiffOp],
    base_len: usize,
    ours_len: usize,
) -> Option<(usize, usize)> {
    let map_pos = |pos: usize| -> usize {
        if pos == 0 {
            return 0;
        }
        if pos >= base_len {
            return ours_len;
        }
        for op in ops {
            match *op {
                similar::DiffOp::Equal { old_index, new_index, len } => {
                    if pos >= old_index && pos <= old_index + len {
                        return new_index + (pos - old_index);
                    }
                }
                similar::DiffOp::Delete { old_index, old_len, new_index } => {
                    if pos >= old_index && pos <= old_index + old_len {
                        return new_index;
                    }
                }
                similar::DiffOp::Insert { old_index, new_index, .. } => {
                    if pos == old_index {
                        return new_index;
                    }
                }
                similar::DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                    if pos >= old_index && pos <= old_index + old_len {
                        return new_index + new_len;
                    }
                }
            }
        }
        ours_len
    };

    Some((map_pos(base_start), map_pos(base_end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_3way_rebase_clean_merge() {
        let base = "fn foo() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n";
        // Ours modified `let a = 1` to `let a = 100`
        let ours = "fn foo() {\n    let a = 100;\n    let b = 2;\n    let c = 3;\n}\n";
        // Model (on base) modified `let c = 3` to `let c = 999`
        let theirs = "fn foo() {\n    let a = 1;\n    let b = 2;\n    let c = 999;\n}\n";

        let res = perform_3way_rebase(base, ours, theirs);
        assert!(res.is_some(), "clean 3-way merge should succeed");
        let unwrapped = res.unwrap();
        assert_eq!(
            unwrapped.merged_content,
            "fn foo() {\n    let a = 100;\n    let b = 2;\n    let c = 999;\n}\n",
            "both non-overlapping changes should be preserved"
        );
        assert_eq!(unwrapped.actual_old_string, "    let c = 3;");
    }

    #[test]
    fn test_3way_rebase_detects_conflict() {
        let base = "fn foo() {\n    let a = 1;\n}\n";
        // Ours modified `let a = 1` to `let a = 10`
        let ours = "fn foo() {\n    let a = 10;\n}\n";
        // Theirs modified `let a = 1` to `let a = 20`
        let theirs = "fn foo() {\n    let a = 20;\n}\n";

        let res = perform_3way_rebase(base, ours, theirs);
        assert!(res.is_none(), "overlapping changes must trigger conflict");
    }

    #[test]
    fn test_initial_base_is_pinned_even_after_many_edits() {
        let mut ring = VersionRing::default();
        let v0 = "fn initial() { 0 }".to_string();
        ring.push(v0.clone());

        // Push 40 subsequent revisions (exceeding MAX_VERSIONS_PER_FILE = 32)
        for i in 1..=40 {
            ring.push(format!("fn step_{i}() {{ {i} }}"));
        }

        let all = ring.all_versions_reverse();
        // The most recent 32 plus the initial base
        assert_eq!(all.len(), 33, "should hold 32 recent versions + 1 pinned initial base");
        assert_eq!(all.first().unwrap(), "fn step_40() { 40 }", "newest version first");
        assert_eq!(all.last().unwrap(), &v0, "pinned initial base must remain at the end");
    }
}
