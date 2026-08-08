//! `build_graph(root)` + the shared, lazily-built [`CodeIndex`] the graph tools hold.
//!
//! # Incremental model
//!
//! The index is a map of **per-file units** (parsed symbols + raw call sites). On each
//! refresh we walk the workspace, **re-parse only dirty/new files**, drop deleted ones,
//! then recompose the cross-file [`CodeGraph`] (symbol insert + call resolution).
//! Unchanged files keep their unit — so a `git pull` that touches a handful of sources
//! does not re-tree-sitter the whole monorepo.
//!
//! A background poller (started when codeintel tools register) keeps the last-used
//! workspace warm so the next tool call is usually already up to date.

use super::graph::{CodeGraph, Edge, EdgeKind, SymbolId, SymbolKind, SymbolNode, Visibility};
use super::lang::Lang;
use super::path_for_display;
use super::symbols::{
    extract_call_sites_from_tree, extract_symbols_from_tree, parse_source, Symbol,
};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// How often the background refresher re-walks the last workspace (cheap fingerprint;
/// only dirty files are re-parsed).
const BACKGROUND_REFRESH_SECS: u64 = 2;

/// On-disk cache layout version. Bump when `DiskCache` / unit schema changes.
const DISK_CACHE_VERSION: u32 = 1;

/// Relative path under the workspace root for the persisted unit+graph cache.
pub const DISK_CACHE_REL: &str = ".atomcode/codegraph/units.v1.json";

/// Map a tree-sitter node-kind string to a [`SymbolKind`]. From production
/// `classify_symbol_kind`.
fn classify_symbol_kind(ts: &str) -> SymbolKind {
    match ts {
        "function_item" | "function_definition" | "function_declaration" | "func_literal"
        | "local_function_statement" => SymbolKind::Function,
        "method_definition" | "method_declaration" | "constructor_declaration"
        | "destructor_declaration" | "operator_declaration" => SymbolKind::Method,
        "struct_item" | "struct_specifier" | "struct_type" | "struct_declaration"
        | "record_declaration" => SymbolKind::Struct,
        "class_definition" | "class_declaration" | "class_specifier" => SymbolKind::Class,
        "trait_item" => SymbolKind::Trait,
        "interface_declaration" | "interface_type" => SymbolKind::Interface,
        "enum_item" | "enum_declaration" | "enum_specifier" | "enum_member_declaration" => {
            SymbolKind::Enum
        }
        "const_item" | "const_declaration" | "property_declaration" | "event_declaration"
        | "delegate_declaration" => SymbolKind::Constant,
        "let_declaration" | "variable_declaration" | "static_item" => SymbolKind::Variable,
        "mod_item" | "module" | "namespace_declaration" | "file_scoped_namespace_declaration" => {
            SymbolKind::Module
        }
        "use_declaration" | "import_statement" | "import_declaration" => SymbolKind::Import,
        "type_item" | "type_alias_declaration" => SymbolKind::TypeAlias,
        "impl_item" => SymbolKind::Other("impl".to_string()),
        other => SymbolKind::Other(other.to_string()),
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct RawCall {
    caller_name: String,
    /// caller's start_line — lets the build reconstruct the caller's exact id via `make_id`
    /// instead of a name lookup (removes a scan and fixes wrong-caller attribution).
    caller_line: usize,
    callee_name: String,
    line: usize,
}

/// Attribute call sites to the innermost enclosing Function/Method symbol.
fn attribute_calls(syms: &[Symbol], sites: Vec<super::symbols::CallSite>) -> Vec<RawCall> {
    // Pre-filter callables once — attribution is O(sites × callables), not O(sites × all_syms).
    let callables: Vec<&Symbol> = syms
        .iter()
        .filter(|s| {
            matches!(
                classify_symbol_kind(&s.kind),
                SymbolKind::Function | SymbolKind::Method
            )
        })
        .collect();
    let mut calls = Vec::with_capacity(sites.len());
    for site in sites {
        let caller = callables
            .iter()
            .filter(|s| s.start_line <= site.line && site.line <= s.end_line)
            .max_by_key(|s| s.start_line);
        if let Some(caller) = caller {
            if caller.name != site.callee_name {
                calls.push(RawCall {
                    caller_name: caller.name.clone(),
                    caller_line: caller.start_line,
                    callee_name: site.callee_name,
                    line: site.line,
                });
            }
        }
    }
    calls
}

/// One tree-sitter parse → symbols + calls (was two full parses per file).
fn parse_file(path: &Path, source: &str) -> Option<(Vec<SymbolNode>, Vec<RawCall>)> {
    let lang = Lang::detect(path)?;
    if !lang.is_indexed() {
        return None;
    }
    let tree = parse_source(source, lang)?;
    let raw = extract_symbols_from_tree(source, lang, &tree)?;
    let sites = extract_call_sites_from_tree(source, lang, &tree);
    let calls = attribute_calls(&raw, sites);
    let nodes = raw
        .iter()
        .map(|s| SymbolNode {
            id: CodeGraph::make_id(path, &s.name, s.start_line),
            name: s.name.clone(),
            kind: classify_symbol_kind(&s.kind),
            visibility: Visibility::Unknown,
            file: path.to_path_buf(),
            start_line: s.start_line,
            end_line: s.end_line,
            signature: None,
        })
        .collect();
    Some((nodes, calls))
}

/// Extensions walked into the graph (matches production's INDEXED set + C#).
const INDEXED_EXTS: &[&str] = &[
    "rs", "py", "js", "jsx", "mjs", "cjs", "ts", "mts", "tsx", "go", "java", "c", "h", "cc", "cpp",
    "cxx", "hpp", "hh", "cs",
];

/// Directory basenames skipped even when not covered by `.gitignore` (common on
/// C#/Node/Rust monorepos that ship without ignore rules, or when agents open a
/// parent folder that contains build outputs).
const SKIP_DIR_NAMES: &[&str] = &[
    "bin",
    "obj",
    "node_modules",
    "target",
    "dist",
    "build",
    ".git",
    ".vs",
    ".idea",
    "packages", // NuGet restore folder
    "TestResults",
    "coverage",
    "wwwroot", // static assets; often minified JS
    "bower_components",
    "jspm_packages",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    "site-packages",
    ".nuget",
    ".next",
    ".turbo",
    ".cache",
    "publish",
    "out",
    "Debug",
    "Release",
];

/// Skip huge / minified / generated sources that blow up tree-sitter time.
const MAX_INDEX_FILE_BYTES: u64 = 768 * 1024;

/// Generated / designer C# sources — huge and low value for call-graph tools.
fn is_generated_source(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".designer.cs")
        || lower.ends_with(".g.cs")
        || lower.ends_with(".g.i.cs")
        || lower == "assemblyinfo.cs"
        || lower.ends_with(".assemblyattributes.cs")
        || lower.contains(".min.")
        || lower.ends_with(".min.js")
        || lower.ends_with(".min.css")
        || lower.ends_with(".bundle.js")
        || lower.ends_with(".map")
}

fn should_skip_dir(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .map(|n| SKIP_DIR_NAMES.iter().any(|s| s.eq_ignore_ascii_case(n)))
        .unwrap_or(false)
}

/// A walked source file + the inputs to its staleness fingerprint.
struct Walked {
    path: PathBuf,
    /// mtime in NANOSECONDS — coarse whole seconds would miss a same-second edit and
    /// serve a stale graph.
    mtime_ns: u128,
    /// file length — defends against a same-instant edit whose mtime didn't move (content
    /// length almost always changes on a real edit).
    len: u64,
}

/// Walk `root` (assumed already canonical) for indexable source files + staleness inputs.
fn collect_files(root: &Path) -> Vec<Walked> {
    let mut out = Vec::new();
    for entry in WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|e| {
            // Prune known build/vendor directories early (gitignore may be missing).
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                !should_skip_dir(e.file_name())
            } else {
                true
            }
        })
        .build()
        .flatten()
    {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let ext_ok = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| INDEXED_EXTS.contains(&e))
            .unwrap_or(false);
        if !ext_ok {
            continue;
        }
        if is_generated_source(p) {
            continue;
        }
        let md = entry.metadata().ok();
        let len = md.as_ref().map(|m| m.len()).unwrap_or(0);
        if len > MAX_INDEX_FILE_BYTES {
            continue;
        }
        let mtime_ns = md
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        out.push(Walked {
            path: p.to_path_buf(),
            mtime_ns,
            len,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn fingerprint(files: &[Walked]) -> u64 {
    let mut h = DefaultHasher::new();
    for w in files {
        w.path.hash(&mut h);
        w.mtime_ns.hash(&mut h);
        w.len.hash(&mut h);
    }
    h.finish()
}

fn top_component(p: &Path, root: &Path) -> Option<std::ffi::OsString> {
    p.strip_prefix(root)
        .ok()?
        .components()
        .next()
        .map(|c| c.as_os_str().to_os_string())
}

/// Resolve a callee name to a symbol id, preferring closer candidates (production
/// scoring): same file (4) > same dir (2) > same top-level component (1) > any (0).
/// (Import-based score 3 is omitted — like production, we do not parse imports yet.)
/// Ties are broken DETERMINISTICALLY by the smallest (file, start_line) — production's
/// tie-break depends on HashMap iteration order, which is not reproducible.
fn resolve_callee(
    g: &CodeGraph,
    callee: &str,
    caller_file: &Path,
    root: &Path,
) -> Option<SymbolId> {
    let score = |n: &SymbolNode| -> i32 {
        if n.file == caller_file {
            4
        } else if n.file.parent().is_some() && n.file.parent() == caller_file.parent() {
            2
        } else {
            let a = top_component(&n.file, root);
            if a.is_some() && a == top_component(caller_file, root) {
                1
            } else {
                0
            }
        }
    };
    let mut best: Option<&SymbolNode> = None;
    let mut best_score = i32::MIN;
    for n in g.find_by_name(callee) {
        let s = score(n);
        let better = match best {
            None => true,
            Some(b) => {
                s > best_score
                    || (s == best_score
                        && (n.file.as_path(), n.start_line) < (b.file.as_path(), b.start_line))
            }
        };
        if better {
            best = Some(n);
            best_score = s;
        }
    }
    best.map(|n| n.id)
}

/// Atomic index unit for one source file: parsed symbols + unresolved call sites.
/// A content change (mtime/len) replaces this whole unit; other units are untouched.
#[derive(Clone, Serialize, Deserialize)]
struct FileUnit {
    mtime_ns: u128,
    len: u64,
    nodes: Vec<SymbolNode>,
    calls: Vec<RawCall>,
}

/// Persisted workspace index written by `atomcode init` / after in-process builds.
#[derive(Serialize, Deserialize)]
struct DiskCache {
    version: u32,
    /// Canonical root path string (informational; validity is walk_fp).
    root: String,
    walk_fp: u64,
    /// path string → unit (PathBuf keys serialize poorly across OS separators).
    units: HashMap<String, FileUnit>,
    graph: CodeGraph,
}

/// Result of [`init_workspace_index`] / a successful index refresh.
#[derive(Debug, Clone)]
pub struct IndexReport {
    pub root: PathBuf,
    pub cache_path: PathBuf,
    pub files: usize,
    pub symbols: usize,
    pub reparsed: usize,
    pub removed: usize,
    pub kept: usize,
    pub elapsed: Duration,
    /// True when the disk cache was a perfect walk_fp hit (no re-parse).
    pub cache_hit: bool,
}

/// Absolute path of the on-disk codegraph cache for `root`.
pub fn disk_cache_path(root: &Path) -> PathBuf {
    super::canonical(root).join(DISK_CACHE_REL)
}

fn load_disk_cache(root: &Path) -> Option<DiskCache> {
    let path = disk_cache_path(root);
    let bytes = std::fs::read(&path).ok()?;
    let mut cache: DiskCache = serde_json::from_slice(&bytes).ok()?;
    if cache.version != DISK_CACHE_VERSION {
        return None;
    }
    // `by_name` is skip-serialized — rebuild before serving queries.
    cache.graph.rebuild_name_index();
    Some(cache)
}

fn save_disk_cache(
    root: &Path,
    walk_fp: u64,
    units: &HashMap<PathBuf, FileUnit>,
    graph: &CodeGraph,
) -> std::io::Result<PathBuf> {
    let path = disk_cache_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut unit_map = HashMap::with_capacity(units.len());
    for (p, u) in units {
        unit_map.insert(p.to_string_lossy().into_owned(), u.clone());
    }
    let cache = DiskCache {
        version: DISK_CACHE_VERSION,
        root: root.display().to_string(),
        walk_fp,
        units: unit_map,
        graph: graph.clone(),
    };
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec(&cache)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn units_from_disk(cache: DiskCache) -> HashMap<PathBuf, FileUnit> {
    cache
        .units
        .into_iter()
        .map(|(p, u)| (PathBuf::from(p), u))
        .collect()
}

/// Build or refresh the workspace code graph and **persist** it under
/// [`.atomcode/codegraph/`](DISK_CACHE_REL) so the next agent session can load it.
///
/// Used by `atomcode init`. `force` deletes any existing cache and re-parses every file.
pub fn init_workspace_index(
    root: &Path,
    force: bool,
    on_progress: &dyn Fn(&str),
) -> Result<IndexReport, String> {
    let root = super::canonical(root);
    let t0 = Instant::now();
    let cache_path = disk_cache_path(&root);

    if force {
        on_progress(&format!(
            "Code graph: --force, removing {}",
            path_for_display(&cache_path)
        ));
        let _ = std::fs::remove_file(&cache_path);
    }

    let idx = CodeIndex::new();
    let _g = idx.get_with_progress(&root, on_progress);

    let guard = idx.inner.lock().map_err(|e| e.to_string())?;
    // Always rewrite so CLI leaves a durable cache even on pure hits.
    if let Some(g) = guard.graph.as_ref() {
        let path = save_disk_cache(&root, guard.walk_fp, &guard.units, g)
            .map_err(|e| format!("failed to write {}: {e}", path_for_display(&cache_path)))?;
        on_progress(&format!("Code graph: wrote {}", path_for_display(&path)));
    }
    let stats = guard.last_stats.unwrap_or(RefreshStats {
        reparsed: 0,
        removed: 0,
        kept: guard.units.len(),
        cache_hit: false,
    });
    Ok(IndexReport {
        root: root.clone(),
        cache_path,
        files: guard.units.len(),
        symbols: guard.graph.as_ref().map(|g| g.node_count()).unwrap_or(0),
        reparsed: stats.reparsed,
        removed: stats.removed,
        kept: stats.kept,
        elapsed: t0.elapsed(),
        cache_hit: stats.cache_hit,
    })
}

#[derive(Debug, Clone, Copy)]
struct RefreshStats {
    reparsed: usize,
    removed: usize,
    kept: usize,
    cache_hit: bool,
}

fn parse_unit(w: &Walked) -> Option<FileUnit> {
    let source = std::fs::read_to_string(&w.path).ok()?;
    let (nodes, calls) = parse_file(&w.path, &source)?;
    Some(FileUnit {
        mtime_ns: w.mtime_ns,
        len: w.len,
        nodes,
        calls,
    })
}

/// Compose a cross-file graph from per-file units (symbols first, then call resolve).
/// Call resolution is global (names may resolve into other files) but cheap vs parse.
fn compose_graph(
    root: &Path,
    units: &HashMap<PathBuf, FileUnit>,
    on_progress: &dyn Fn(&str),
) -> CodeGraph {
    let mut g = CodeGraph::new();
    let mut raw_calls: Vec<(PathBuf, RawCall)> = Vec::new();
    for (path, unit) in units {
        for n in &unit.nodes {
            g.add_symbol(n.clone());
        }
        g.file_mtimes
            .insert(path.clone(), (unit.mtime_ns / 1_000_000_000) as u64);
        for c in &unit.calls {
            raw_calls.push((path.clone(), c.clone()));
        }
    }
    on_progress(&format!(
        "Code graph: resolving {} call sites across {} symbols ({} files)...",
        raw_calls.len(),
        g.node_count(),
        units.len()
    ));
    for (caller_file, rc) in raw_calls {
        let caller = CodeGraph::make_id(&caller_file, &rc.caller_name, rc.caller_line);
        if g.node(caller).is_none() {
            continue;
        }
        if let Some(callee) = resolve_callee(&g, &rc.callee_name, &caller_file, root) {
            g.add_edge(
                caller,
                Edge {
                    to: callee,
                    kind: EdgeKind::Calls,
                    line: rc.line,
                },
            );
        }
    }
    g
}

/// Diff `walked` against `units`: re-parse dirty/new, drop deleted. Returns
/// `(reparsed, removed, kept)`. Dirty files are parsed **in parallel** across
/// available CPU cores (each thread caches its own tree-sitter queries).
fn sync_units(
    units: &mut HashMap<PathBuf, FileUnit>,
    walked: &[Walked],
    on_progress: &dyn Fn(&str),
) -> (usize, usize, usize) {
    let walked_paths: std::collections::HashSet<PathBuf> =
        walked.iter().map(|w| w.path.clone()).collect();

    let before = units.len();
    units.retain(|p, _| walked_paths.contains(p));
    let removed = before - units.len();

    let mut dirty: Vec<&Walked> = Vec::new();
    let mut kept = 0usize;
    for w in walked {
        match units.get(&w.path) {
            Some(u) if u.mtime_ns == w.mtime_ns && u.len == w.len => kept += 1,
            _ => dirty.push(w),
        }
    }
    let dirty_total = dirty.len();
    if dirty_total == 0 {
        return (0, removed, kept);
    }

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16);
    on_progress(&format!(
        "Code graph: re-parsing {dirty_total} changed/new file(s) ({} unchanged) with {threads} threads...",
        walked.len().saturating_sub(dirty_total)
    ));

    // Chunk dirty list across worker threads. Progress is reported after each
    // chunk joins (on_progress is not Sync, so workers cannot call it).
    let chunk = (dirty_total + threads - 1) / threads;
    let mut parsed: Vec<(PathBuf, Option<FileUnit>)> = Vec::with_capacity(dirty_total);
    let mut finished = 0usize;

    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for piece in dirty.chunks(chunk.max(1)) {
            let piece: Vec<&Walked> = piece.to_vec();
            handles.push(scope.spawn(move || {
                let mut out = Vec::with_capacity(piece.len());
                for w in piece {
                    out.push((w.path.clone(), parse_unit(w)));
                }
                out
            }));
        }
        for h in handles {
            if let Ok(chunk_out) = h.join() {
                finished += chunk_out.len();
                on_progress(&format!(
                    "Code graph: re-parsed {finished}/{dirty_total} dirty files..."
                ));
                parsed.extend(chunk_out);
            }
        }
    });

    on_progress(&format!(
        "Code graph: finished parse of {dirty_total} dirty files ({threads} threads)."
    ));

    let mut reparsed = 0usize;
    for (path, unit) in parsed {
        reparsed += 1;
        match unit {
            Some(u) => {
                units.insert(path, u);
            }
            None => {
                units.remove(&path);
            }
        }
    }
    (reparsed, removed, kept)
}

/// Build a fresh code graph for `root` (walk → parse → resolve). O(repo), CPU-bound.
pub fn build_graph(root: &Path) -> CodeGraph {
    let root = super::canonical(root);
    let files = collect_files(&root);
    let mut units = HashMap::new();
    for w in &files {
        if let Some(u) = parse_unit(w) {
            units.insert(w.path.clone(), u);
        }
    }
    compose_graph(&root, &units, &|_| {})
}

/// Shared, lazily-built **incremental** code index the graph tools hold.
///
/// - Per-file [`FileUnit`]s: only dirty files are re-parsed.
/// - Graph is recomposed from units after unit-level changes (full call resolve is
///   still global; tree-sitter work is not).
/// - Concurrent callers **single-flight** one refresh.
/// - Optional background poller keeps the last workspace warm after registration.
pub struct CodeIndex {
    inner: Mutex<IndexState>,
    cv: Condvar,
    /// Background refresher already spawned for this index instance.
    watcher_started: AtomicBool,
}

struct IndexState {
    /// Last workspace root this index was built for (canonical).
    root: Option<PathBuf>,
    /// Walk fingerprint of the last successful sync (path+mtime+len of all files).
    walk_fp: u64,
    /// Per-file parse units — the incremental cache.
    units: HashMap<PathBuf, FileUnit>,
    /// Composed query graph (derived from `units`).
    graph: Option<Arc<CodeGraph>>,
    building: bool,
    /// Stats from the most recent refresh (for `atomcode init` reporting).
    last_stats: Option<RefreshStats>,
}

impl Default for IndexState {
    fn default() -> Self {
        Self {
            root: None,
            walk_fp: 0,
            units: HashMap::new(),
            graph: None,
            building: false,
            last_stats: None,
        }
    }
}

impl Default for CodeIndex {
    fn default() -> Self {
        Self {
            inner: Mutex::new(IndexState::default()),
            cv: Condvar::new(),
            watcher_started: AtomicBool::new(false),
        }
    }
}

impl CodeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a cheap background poller that incrementally refreshes the last-used
    /// workspace so `git pull` / editor saves land without waiting for the next tool.
    /// Safe to call multiple times; only the first starts the thread.
    pub fn start_background_refresh(self: &Arc<Self>) {
        if self
            .watcher_started
            .swap(true, Ordering::SeqCst)
        {
            return;
        }
        let this = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("codegraph-refresh".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(BACKGROUND_REFRESH_SECS));
                let root = {
                    let g = match this.inner.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    if g.building {
                        continue;
                    }
                    g.root.clone()
                };
                if let Some(root) = root {
                    // Silent incremental sync; tools still single-flight if racing.
                    let _ = this.get_with_progress(&root, &|_| {});
                }
            });
    }

    /// Cached graph lookup with no progress callbacks (tests / internal).
    pub fn get(&self, root: &Path) -> Arc<CodeGraph> {
        self.get_with_progress(root, &|_| {})
    }

    /// Ensure the index matches `root` on disk. Unchanged files keep their units;
    /// only dirty/new files are re-parsed, then the graph is recomposed if needed.
    ///
    /// Cold start path: if memory is empty, try loading
    /// [`.atomcode/codegraph/units.v1.json`](DISK_CACHE_REL) written by `atomcode init`.
    pub fn get_with_progress(&self, root: &Path, on_progress: &dyn Fn(&str)) -> Arc<CodeGraph> {
        let root = super::canonical(root);
        let files = collect_files(&root);
        let fp = fingerprint(&files);

        let mut guard = self.inner.lock().unwrap();
        loop {
            let same_root = guard.root.as_ref() == Some(&root);
            if same_root && guard.walk_fp == fp {
                if let Some(g) = guard.graph.as_ref() {
                    return g.clone();
                }
            }
            if guard.building {
                on_progress(
                    "Code graph: waiting for in-flight workspace index (shared with other tools)...",
                );
                guard = self.cv.wait(guard).unwrap();
                continue;
            }
            guard.building = true;
            // Move units out so we can work without holding the lock.
            let mut units = if same_root {
                std::mem::take(&mut guard.units)
            } else {
                guard.units.clear();
                HashMap::new()
            };
            let prev_graph = if same_root {
                guard.graph.clone()
            } else {
                None
            };
            drop(guard);

            // Seed from disk cache when memory has no units (new process / after force).
            let mut prev_graph = prev_graph;
            if units.is_empty() {
                if let Some(disk) = load_disk_cache(&root) {
                    if disk.walk_fp == fp {
                        let DiskCache {
                            units: disk_units,
                            graph,
                            ..
                        } = disk;
                        let n_files = disk_units.len();
                        let g = Arc::new(graph);
                        on_progress(&format!(
                            "Code graph: loaded {} from disk ({n_files} files, {} symbols) - up to date.",
                            path_for_display(&disk_cache_path(&root)),
                            g.node_count()
                        ));
                        let units: HashMap<PathBuf, FileUnit> = disk_units
                            .into_iter()
                            .map(|(p, u)| (PathBuf::from(p), u))
                            .collect();
                        let mut guard = self.inner.lock().unwrap();
                        guard.root = Some(root.clone());
                        guard.walk_fp = fp;
                        guard.units = units;
                        guard.graph = Some(g.clone());
                        guard.last_stats = Some(RefreshStats {
                            reparsed: 0,
                            removed: 0,
                            kept: n_files,
                            cache_hit: true,
                        });
                        guard.building = false;
                        self.cv.notify_all();
                        return g;
                    }
                    on_progress(&format!(
                        "Code graph: disk cache stale - incremental sync from {}...",
                        path_for_display(&disk_cache_path(&root))
                    ));
                    units = units_from_disk(disk);
                    prev_graph = None; // force recompose after unit sync
                }
            }

            let (reparsed, removed, kept) = sync_units(&mut units, &files, on_progress);
            let need_compose = reparsed > 0 || removed > 0 || prev_graph.is_none();
            let cache_hit = reparsed == 0 && removed == 0 && prev_graph.is_some();

            let g = if need_compose {
                if kept == 0 && reparsed > 0 {
                    on_progress(&format!(
                        "Code graph: full index of {} files (first build or workspace switch)...",
                        files.len()
                    ));
                } else if reparsed > 0 || removed > 0 {
                    on_progress(&format!(
                        "Code graph: unit update - reparsed {reparsed}, removed {removed}, kept {kept}."
                    ));
                }
                Arc::new(compose_graph(&root, &units, on_progress))
            } else {
                prev_graph.expect("need_compose false => graph present")
            };

            if need_compose {
                on_progress(&format!(
                    "Code graph: ready ({} symbols, {} files; reparsed {}, removed {}, kept {}).",
                    g.node_count(),
                    units.len(),
                    reparsed,
                    removed,
                    kept
                ));
                match save_disk_cache(&root, fp, &units, g.as_ref()) {
                    Ok(p) => on_progress(&format!("Code graph: saved {}", path_for_display(&p))),
                    Err(e) => on_progress(&format!("Code graph: disk save skipped ({e})")),
                }
            }

            let stats = RefreshStats {
                reparsed,
                removed,
                kept,
                cache_hit,
            };

            let mut guard = self.inner.lock().unwrap();
            guard.root = Some(root.clone());
            guard.walk_fp = fp;
            guard.units = units;
            guard.graph = Some(g.clone());
            guard.last_stats = Some(stats);
            guard.building = false;
            self.cv.notify_all();
            return g;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_cross_file_call_edges() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.rs"),
            "fn helper() {}\nfn main() {\n    helper();\n}\n",
        )
        .unwrap();
        let g = build_graph(d.path());
        let main = g.find_by_name("main").into_iter().next().expect("main");
        let helper = g.find_by_name("helper").into_iter().next().expect("helper");
        // main → helper edge exists
        let callees = g.callees(main.id).expect("callees");
        assert!(
            callees.iter().any(|e| e.to == helper.id),
            "main should call helper"
        );
        // reverse: helper has main as caller
        assert!(g
            .callers(helper.id)
            .unwrap()
            .iter()
            .any(|e| e.to == main.id));
    }

    #[test]
    fn resolves_calls_across_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("util.rs"), "pub fn compute() -> i32 { 42 }\n").unwrap();
        std::fs::write(
            d.path().join("main.rs"),
            "fn run() {\n    let _ = compute();\n}\n",
        )
        .unwrap();
        let g = build_graph(d.path());
        let run = g.find_by_name("run").into_iter().next().expect("run");
        let compute = g
            .find_by_name("compute")
            .into_iter()
            .next()
            .expect("compute");
        assert!(
            g.callees(run.id)
                .unwrap()
                .iter()
                .any(|e| e.to == compute.id),
            "run → compute across files"
        );
    }

    #[test]
    fn self_calls_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("r.rs"),
            "fn recur(n: i32) {\n    if n > 0 { recur(n - 1); }\n}\n",
        )
        .unwrap();
        let g = build_graph(d.path());
        let recur = g.find_by_name("recur").into_iter().next().expect("recur");
        assert!(
            g.callees(recur.id).map(|e| e.is_empty()).unwrap_or(true),
            "self-call must be skipped"
        );
    }

    #[test]
    fn csharp_symbols_and_cross_file_calls_are_indexed() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("CouponService.cs"),
            r#"
namespace Shop.Services;

public class CouponService
{
    public decimal Apply(decimal price) => price * 0.9m;
    public string Code { get; set; }
}

public record CouponDto(string Code, decimal Off);
"#,
        )
        .unwrap();
        std::fs::write(
            d.path().join("OrderController.cs"),
            r#"
namespace Shop.Api;

public class OrderController
{
    private readonly CouponService _coupons = new CouponService();

    public decimal Checkout(decimal total)
    {
        return _coupons.Apply(total);
    }
}
"#,
        )
        .unwrap();

        let g = build_graph(d.path());
        assert!(
            g.find_by_name("CouponService").into_iter().next().is_some(),
            "class CouponService must be indexed"
        );
        assert!(
            g.find_by_name("Apply").into_iter().next().is_some(),
            "method Apply must be indexed"
        );
        assert!(
            g.find_by_name("CouponDto").into_iter().next().is_some(),
            "record CouponDto must be indexed"
        );
        assert!(
            g.find_by_name("Code").into_iter().next().is_some(),
            "property Code must be indexed"
        );

        let checkout = g
            .find_by_name("Checkout")
            .into_iter()
            .next()
            .expect("Checkout");
        let apply = g.find_by_name("Apply").into_iter().next().expect("Apply");
        assert!(
            g.callees(checkout.id)
                .unwrap_or(&vec![])
                .iter()
                .any(|e| e.to == apply.id),
            "OrderController.Checkout should call CouponService.Apply"
        );

        // blast-radius style: Apply has Checkout as a dependent caller file
        let apply_file = &apply.file;
        let deps = g.file_dependents(apply_file, 2);
        assert!(
            deps.iter().any(|f| f.file_name().and_then(|n| n.to_str()) == Some("OrderController.cs")),
            "OrderController.cs must appear in blast radius of CouponService.cs: {deps:?}"
        );
    }

    #[test]
    fn same_second_edit_triggers_rebuild() {
        // Overwriting the SAME file (likely the same wall-clock second) must rebuild —
        // the fingerprint uses nanos + length, not coarse seconds.
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.rs");
        std::fs::write(&f, "fn one() {}\n").unwrap();
        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        assert!(g1.find_by_name("two").is_empty());
        std::fs::write(&f, "fn one() {}\nfn two() {}\n").unwrap();
        let g2 = idx.get(d.path());
        assert!(
            !g2.find_by_name("two").is_empty(),
            "same-second edit must rebuild (nanos/len changed)"
        );
    }

    #[test]
    fn tie_break_resolution_is_deterministic() {
        // Two same-named fns in the same dir → equal score for a same-dir caller → tie,
        // resolved deterministically to the smallest (file, line) = a_util.rs.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a_util.rs"), "pub fn dup() {}\n").unwrap();
        std::fs::write(d.path().join("z_util.rs"), "pub fn dup() {}\n").unwrap();
        std::fs::write(d.path().join("main.rs"), "fn run() { dup(); }\n").unwrap();
        let g = build_graph(d.path());
        let run = g.find_by_name("run").into_iter().next().unwrap();
        let target = g
            .callees(run.id)
            .and_then(|e| e.first())
            .and_then(|e| g.node(e.to))
            .map(|n| n.file.clone());
        assert!(
            target
                .as_ref()
                .map(|f| f.ends_with("a_util.rs"))
                .unwrap_or(false),
            "tie → a_util.rs, got {target:?}"
        );
        // stable across a rebuild
        let g2 = build_graph(d.path());
        let run2 = g2.find_by_name("run").into_iter().next().unwrap();
        let t2 = g2
            .callees(run2.id)
            .and_then(|e| e.first())
            .and_then(|e| g2.node(e.to))
            .map(|n| n.file.clone());
        assert_eq!(target, t2);
    }

    #[test]
    fn index_caches_then_rebuilds_on_change() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn one() {}\n").unwrap();
        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        let g2 = idx.get(d.path());
        assert!(
            Arc::ptr_eq(&g1, &g2),
            "unchanged repo → cached graph reused"
        );
        assert!(g1.find_by_name("two").is_empty());
        // change the repo (new mtime via a new file) → rebuild
        std::fs::write(d.path().join("b.rs"), "fn two() {}\n").unwrap();
        let g3 = idx.get(d.path());
        assert!(!Arc::ptr_eq(&g1, &g3), "changed repo → rebuilt");
        assert!(
            !g3.find_by_name("two").is_empty(),
            "rebuilt graph sees new symbol"
        );
    }

    #[test]
    fn skips_bin_obj_and_generated_csharp() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("bin")).unwrap();
        std::fs::create_dir_all(d.path().join("obj")).unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("bin/Junk.cs"), "class BinOnly {}\n").unwrap();
        std::fs::write(d.path().join("obj/Junk.cs"), "class ObjOnly {}\n").unwrap();
        std::fs::write(
            d.path().join("src/Form1.Designer.cs"),
            "partial class Form1 {}\n",
        )
        .unwrap();
        std::fs::write(d.path().join("src/Real.cs"), "class RealService {}\n").unwrap();
        let g = build_graph(d.path());
        assert!(
            g.find_by_name("RealService").into_iter().next().is_some(),
            "real source must be indexed"
        );
        assert!(
            g.find_by_name("BinOnly").is_empty(),
            "bin/ must be skipped"
        );
        assert!(
            g.find_by_name("ObjOnly").is_empty(),
            "obj/ must be skipped"
        );
        assert!(
            g.find_by_name("Form1").is_empty(),
            "Designer.cs must be skipped"
        );
    }

    #[test]
    fn concurrent_gets_single_flight_same_graph() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn shared() {}\n").unwrap();
        let idx = Arc::new(CodeIndex::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let idx = idx.clone();
            let root = d.path().to_path_buf();
            handles.push(std::thread::spawn(move || idx.get(&root)));
        }
        let graphs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for g in &graphs[1..] {
            assert!(
                Arc::ptr_eq(&graphs[0], g),
                "parallel cold gets must share one built graph"
            );
        }
        assert!(graphs[0].find_by_name("shared").into_iter().next().is_some());
    }

    #[test]
    fn unit_change_updates_only_that_file() {
        // Many stable files + one dirty file: after first index, editing one file must
        // surface new symbols without dropping the rest (unit-level reparse).
        let d = tempfile::tempdir().unwrap();
        for i in 0..20 {
            std::fs::write(
                d.path().join(format!("stable_{i}.rs")),
                format!("fn stable_{i}() {{}}\n"),
            )
            .unwrap();
        }
        let target = d.path().join("target.rs");
        std::fs::write(&target, "fn old_name() {}\n").unwrap();

        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        assert!(g1.find_by_name("old_name").into_iter().next().is_some());
        assert!(g1.find_by_name("stable_0").into_iter().next().is_some());
        assert!(g1.find_by_name("new_name").is_empty());
        let n1 = g1.node_count();

        std::fs::write(&target, "fn new_name() {}\n").unwrap();
        let g2 = idx.get(d.path());
        assert!(
            g2.find_by_name("new_name").into_iter().next().is_some(),
            "edited file unit must be reparsed"
        );
        assert!(
            g2.find_by_name("old_name").is_empty(),
            "old symbols from the replaced unit must be gone"
        );
        assert!(
            g2.find_by_name("stable_19").into_iter().next().is_some(),
            "unchanged units must remain"
        );
        // Symbol count: lost old_name, gained new_name → same count for this edit.
        assert_eq!(g2.node_count(), n1);

        // Unchanged second get → same Arc.
        let g3 = idx.get(d.path());
        assert!(Arc::ptr_eq(&g2, &g3), "no disk change → cached graph");
    }

    #[test]
    fn deleted_file_unit_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("keep.rs"), "fn keep() {}\n").unwrap();
        std::fs::write(d.path().join("gone.rs"), "fn gone() {}\n").unwrap();
        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        assert!(g1.find_by_name("gone").into_iter().next().is_some());
        std::fs::remove_file(d.path().join("gone.rs")).unwrap();
        let g2 = idx.get(d.path());
        assert!(g2.find_by_name("gone").is_empty());
        assert!(g2.find_by_name("keep").into_iter().next().is_some());
    }

    #[test]
    fn disk_cache_roundtrip_via_init() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let report = init_workspace_index(d.path(), false, &|_| {}).expect("init");
        assert!(report.cache_path.exists(), "cache file written");
        assert!(report.symbols >= 1);
        assert!(report.files >= 1);

        // New index instance (simulates new process) should load from disk.
        let idx = CodeIndex::new();
        let g = idx.get(d.path());
        assert!(
            g.find_by_name("hello").into_iter().next().is_some(),
            "loaded graph must contain hello"
        );
        let guard = idx.inner.lock().unwrap();
        assert!(
            guard.last_stats.map(|s| s.cache_hit).unwrap_or(false),
            "second process should report disk cache hit"
        );
    }

    #[test]
    fn cross_file_edge_updates_when_callee_file_changes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("util.rs"), "pub fn compute() -> i32 { 1 }\n").unwrap();
        std::fs::write(
            d.path().join("main.rs"),
            "fn run() {\n    let _ = compute();\n}\n",
        )
        .unwrap();
        let idx = CodeIndex::new();
        let g1 = idx.get(d.path());
        let run = g1.find_by_name("run").into_iter().next().unwrap();
        let compute = g1.find_by_name("compute").into_iter().next().unwrap();
        assert!(g1.callees(run.id).unwrap().iter().any(|e| e.to == compute.id));

        // Rename callee in util only — main unit unchanged; edge must re-resolve.
        std::fs::write(d.path().join("util.rs"), "pub fn compute_v2() -> i32 { 2 }\n").unwrap();
        let g2 = idx.get(d.path());
        assert!(g2.find_by_name("compute").is_empty());
        assert!(g2.find_by_name("compute_v2").into_iter().next().is_some());
        let run2 = g2.find_by_name("run").into_iter().next().unwrap();
        // main still calls "compute" textually → no resolve target → no edge (or empty).
        let callees = g2.callees(run2.id).map(|e| e.len()).unwrap_or(0);
        assert_eq!(callees, 0, "stale name must not keep old edge after unit recompose");
    }

    #[test]
    fn caller_attribution_is_per_file() {
        // Two files each define a function named `handler`, each calling a DISTINCT callee.
        // The old resolver picked the first same-named symbol as caller, so both edges hung
        // off ONE handler. Caller id must be reconstructed exactly, per file.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.rs"),
            "fn handler() {\n    alpha();\n}\nfn alpha() {}\n",
        )
        .unwrap();
        std::fs::write(
            d.path().join("b.rs"),
            "fn handler() {\n    beta();\n}\nfn beta() {}\n",
        )
        .unwrap();
        let g = build_graph(d.path());

        let a_handler = g
            .find_by_name("handler")
            .into_iter()
            .find(|n| n.file.ends_with("a.rs"))
            .expect("a.rs handler");
        let b_handler = g
            .find_by_name("handler")
            .into_iter()
            .find(|n| n.file.ends_with("b.rs"))
            .expect("b.rs handler");
        let alpha = g.find_by_name("alpha").into_iter().next().expect("alpha");
        let beta = g.find_by_name("beta").into_iter().next().expect("beta");

        let a_callees = g.callees(a_handler.id).cloned().unwrap_or_default();
        let b_callees = g.callees(b_handler.id).cloned().unwrap_or_default();

        assert!(
            a_callees.iter().any(|e| e.to == alpha.id),
            "a.rs::handler → alpha"
        );
        assert!(
            !a_callees.iter().any(|e| e.to == beta.id),
            "a.rs::handler must NOT call beta"
        );
        assert!(
            b_callees.iter().any(|e| e.to == beta.id),
            "b.rs::handler → beta"
        );
        assert!(
            !b_callees.iter().any(|e| e.to == alpha.id),
            "b.rs::handler must NOT call alpha"
        );
    }
}
