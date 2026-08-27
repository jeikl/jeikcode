//! Diagnostic JSONL under `.atomcode/codegraph/logs/`.
//!
//! One line per index rebuild / cache-miss / graph-tool call so a Windows
//! "why did code_explore take 9s?" session can be replayed without a debugger.

use serde_json::{json, Value};
use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

thread_local! {
    static TOOL_CTX: RefCell<Option<ToolCtx>> = RefCell::new(None);
}

#[derive(Clone)]
struct ToolCtx {
    tool: String,
    args: Value,
}

pub struct ToolCallGuard;

impl ToolCallGuard {
    pub fn enter(tool: &str, args: Value) -> Self {
        TOOL_CTX.with(|c| {
            *c.borrow_mut() = Some(ToolCtx {
                tool: tool.to_string(),
                args,
            });
        });
        Self
    }
}

impl Drop for ToolCallGuard {
    fn drop(&mut self) {
        TOOL_CTX.with(|c| {
            *c.borrow_mut() = None;
        });
    }
}

fn now_iso() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = dur.as_secs();
    let millis = dur.subsec_millis();
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

/// Civil UTC date from Unix seconds. Good enough for log filenames / stamps.
fn civil_from_unix(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let z = (secs / 86400) as i64;
    let tod = (secs % 86400) as u32;
    let h = tod / 3600;
    let mi = (tod % 3600) / 60;
    let s = tod % 60;
    // Howard Hinnant civil_from_days
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32, h, mi, s)
}

pub fn log_dir(root: &Path) -> PathBuf {
    super::canonical(root)
        .join(".atomcode")
        .join("codegraph")
        .join("logs")
}

fn append(root: &Path, event: Value) {
    let dir = log_dir(root);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let (y, mo, d, _, _, _) = civil_from_unix(dur.as_secs());
    let path = dir.join(format!("index-{y:04}-{mo:02}-{d:02}.jsonl"));
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let _ = writeln!(f, "{event}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_jsonl_under_codegraph_logs() {
        let d = tempfile::tempdir().unwrap();
        let _g = ToolCallGuard::enter("code_explore", json!({"query": "foo", "path": "src"}));
        log_index_refresh(
            d.path(),
            false,
            "incremental",
            Duration::from_millis(12),
            2,
            0,
            10,
            &[PathBuf::from("a.rs"), PathBuf::from("b.rs")],
            &[],
            json!({"symbols": 3}),
        );
        let dir = log_dir(d.path());
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(files.len(), 1);
        let body = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(body.contains("\"event\":\"index_refresh\""));
        assert!(body.contains("a.rs"));
        assert!(body.contains("code_explore"));
        assert!(body.contains("\"query\":\"foo\""));
    }
}

fn tool_fields() -> Value {
    TOOL_CTX.with(|c| match c.borrow().as_ref() {
        Some(t) => json!({ "tool": t.tool, "args": t.args }),
        None => Value::Null,
    })
}

fn paths_json(paths: &[PathBuf]) -> Value {
    const CAP: usize = 200;
    let shown: Vec<String> = paths
        .iter()
        .take(CAP)
        .map(|p| super::path_for_display(p))
        .collect();
    json!({
        "count": paths.len(),
        "truncated": paths.len() > CAP,
        "files": shown,
    })
}

/// Index refresh / miss / incremental rebuild.
pub fn log_index_refresh(
    root: &Path,
    cache_hit: bool,
    kind: &str,
    elapsed: Duration,
    reparsed: usize,
    removed: usize,
    kept: usize,
    reparsed_files: &[PathBuf],
    removed_files: &[PathBuf],
    extra: Value,
) {
    if cache_hit && reparsed == 0 && removed == 0 {
        return;
    }
    let mut ev = json!({
        "ts": now_iso(),
        "event": "index_refresh",
        "kind": kind,
        "cache_hit": cache_hit,
        "elapsed_ms": elapsed.as_millis() as u64,
        "reparsed": reparsed,
        "removed": removed,
        "kept": kept,
        "reparsed_files": paths_json(reparsed_files),
        "removed_files": paths_json(removed_files),
        "call": tool_fields(),
        "root": super::path_for_display(root),
    });
    if let Some(obj) = ev.as_object_mut() {
        if let Some(extra_obj) = extra.as_object() {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    append(root, ev);
}

/// Derived cache rebuild (IDF / concept vectors / dirindex).
pub fn log_derived_rebuild(root: &Path, what: &str, elapsed: Duration, extra: Value) {
    let mut ev = json!({
        "ts": now_iso(),
        "event": "derived_rebuild",
        "what": what,
        "elapsed_ms": elapsed.as_millis() as u64,
        "call": tool_fields(),
        "root": super::path_for_display(root),
    });
    if let Some(obj) = ev.as_object_mut() {
        if let Some(extra_obj) = extra.as_object() {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    append(root, ev);
}

/// Graph-tool call (params + outcome). Misses include the reparsed file list.
pub fn log_tool_call(root: &Path, result: Value) {
    let mut ev = json!({
        "ts": now_iso(),
        "event": "tool_call",
        "call": tool_fields(),
        "root": super::path_for_display(root),
        "result": result,
    });
    if let Some(obj) = ev.as_object_mut() {
        if result.get("cache_hit") == Some(&Value::Bool(false)) {
            obj.insert("note".into(), json!("index_miss"));
        }
    }
    append(root, ev);
}
