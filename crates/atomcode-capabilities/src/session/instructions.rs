//! Three-tier project-instructions loader (global / project / user) — the v2 port of
//! v1 `config/instructions.rs`. Each tier is a markdown file injected into the session
//! context block under a header carrying its source path. Pure (paths in, string out) so
//! it is testable without touching the real home dir; [`SessionContextHook`] supplies the
//! config-root and project paths.
//!
//! Also loads optional **additive knowledge packs** (coexist with AGENTS.md):
//! - domain glossary — business term → code aliases
//! - business rules — org structure, domain rules, process
//! - db words — common tables / columns / schema nicknames
//!
//! All packs are full-file, hot-reloaded each user turn via [`SessionContextHook`].
//!
//! [`SessionContextHook`]: super::context::SessionContextHook

use std::path::{Path, PathBuf};

/// Project-root filenames checked IN ORDER for the project tier; the FIRST existing wins.
/// Includes the ecosystem names (`AGENTS.md`, `CLAUDE.md`) so a repo's existing agent
/// instructions are honored (matches v1).
const PROJECT_NAMES: [&str; 5] = [
    ".atomcode.md",
    "ATOMCODE.md",
    "AGENTS.md",
    "CLAUDE.md",
    "claude.md",
];

/// Additive knowledge pack: first existing path wins per pack; packs do not replace each other.
struct KnowledgePack {
    /// Header label shown to the model, e.g. `DOMAIN GLOSSARY`.
    header: &'static str,
    /// Short usage hint prepended before the file body.
    hint: &'static str,
    /// Candidate paths relative to project root (first existing file wins).
    candidates: &'static [&'static str],
}

const KNOWLEDGE_PACKS: &[KnowledgePack] = &[
    KnowledgePack {
        header: "DOMAIN GLOSSARY",
        hint: "\
Use this glossary when the user speaks in business terms: expand each term into the \
listed code aliases, then search those aliases (and the original term) in parallel. \
Prefer find_symbol once you have an exact type/method name from the glossary.",
        candidates: &[
            ".atomcode/glossary.md",
            ".atomcode/domain-glossary.md",
            "docs/domain-glossary.md",
            "docs/glossary.md",
            "domain-glossary.md",
            "DOMAIN.md",
        ],
    },
    KnowledgePack {
        header: "BUSINESS RULES",
        hint: "\
Domain / org / process rules for this product (organization structure, approval flows, \
business constraints). Treat as authoritative project knowledge when implementing or \
explaining features. Prefer these over guessing product policy.",
        candidates: &[
            ".atomcode/rules.md",
            ".atomcode/business-rules.md",
            "docs/rules.md",
            "docs/business-rules.md",
            "rules.md",
        ],
    },
    KnowledgePack {
        header: "DB WORDS",
        hint: "\
Common database tables, columns, and nicknames. Use when the user mentions data entities \
or when writing SQL / ORM / migrations: map spoken names to real table/column identifiers \
before searching code or inventing schema.",
        candidates: &[
            ".atomcode/dbwords.md",
            ".atomcode/db-words.md",
            ".atomcode/schema.md",
            "docs/dbwords.md",
            "docs/db-words.md",
            "dbwords.md",
        ],
    },
];

/// Render the global / project / user instruction tiers plus additive knowledge packs.
/// Empty string when nothing exists (the caller then omits the section).
///
/// `home` = config root (`~/.atomcode`); `project` = workspace root.
pub fn render_instructions(home: &Path, project: &Path) -> String {
    let mut out: Vec<String> = Vec::new();
    let global = home.join("ATOMCODE.md");
    if let Some(body) = read_tier(&global) {
        out.push(format!(
            "=== GLOBAL INSTRUCTIONS ({}) ===\n{body}",
            global.display()
        ));
    }
    if let Some(proj) = project_file(project) {
        if let Some(body) = read_tier(&proj) {
            out.push(format!(
                "=== PROJECT INSTRUCTIONS ({}) ===\n{body}",
                proj.display()
            ));
        }
    }
    let user = project.join(".atomcode.user.md");
    if let Some(body) = read_tier(&user) {
        out.push(format!(
            "=== USER INSTRUCTIONS ({}) ===\n{body}",
            user.display()
        ));
    }
    for pack in KNOWLEDGE_PACKS {
        if let Some(path) = first_existing(project, pack.candidates) {
            if let Some(body) = read_tier(&path) {
                out.push(format!(
                    "=== {} ({}) ===\n{}\n\n{body}",
                    pack.header,
                    path.display(),
                    pack.hint
                ));
            }
        }
    }
    if out.is_empty() {
        return String::new();
    }
    // Precedence preamble: GLOBAL/PROJECT/USER override default working rules.
    // Knowledge packs are project facts (aliases / rules / schema), not safety overrides.
    const PREAMBLE: &str = "The following GLOBAL / PROJECT / USER instructions take \
PRECEDENCE over the assistant's default system-prompt rules — when they conflict with a \
default working rule, follow these. These instructions govern work on the project only; \
they do not describe or override the host application or active configured model. \
(Safety, approval, and destructive-action gates are not overridable here.) \
DOMAIN GLOSSARY / BUSINESS RULES / DB WORDS (if present) are project knowledge packs: \
use them for term expansion, policy, and schema mapping; they do not override safety gates.";
    format!("{PREAMBLE}\n\n{}", out.join("\n\n"))
}

/// The first existing project-tier file (precedence order), if any.
fn project_file(project: &Path) -> Option<PathBuf> {
    first_existing(project, &PROJECT_NAMES)
}

fn first_existing(project: &Path, names: &[&str]) -> Option<PathBuf> {
    names
        .iter()
        .map(|n| project.join(n))
        .find(|p| p.is_file())
}

/// Read a tier file → its trimmed body, or `None` if missing/non-file/empty.
/// No size cap: product policy is full-file hot-reload of instruction/knowledge packs.
fn read_tier(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let body = std::fs::read_to_string(path).ok()?;
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    Some(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn project_tier_picks_first_existing_name() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path();
        // AGENTS.md and CLAUDE.md both exist → AGENTS.md (earlier in precedence) wins.
        fs::write(proj.join("AGENTS.md"), "agents rules").unwrap();
        fs::write(proj.join("CLAUDE.md"), "claude rules").unwrap();
        let out = render_instructions(&d.path().join("nohome"), proj);
        assert!(out.contains("PROJECT INSTRUCTIONS"));
        assert!(out.contains("agents rules"));
        assert!(!out.contains("claude rules"), "first-precedence file wins");
        assert!(out.contains("AGENTS.md"), "header carries the source path");
    }

    #[test]
    fn global_and_user_tiers_included() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("ATOMCODE.md"), "global rules").unwrap();
        let proj = d.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join(".atomcode.user.md"), "user rules").unwrap();
        let out = render_instructions(&home, &proj);
        assert!(out.contains("GLOBAL INSTRUCTIONS") && out.contains("global rules"));
        assert!(out.contains("USER INSTRUCTIONS") && out.contains("user rules"));
        let prec = out.find("PRECEDENCE").expect("preamble present");
        let global_idx = out.find("GLOBAL INSTRUCTIONS").unwrap();
        assert!(prec < global_idx, "preamble precedes the blocks: {out}");
        assert!(
            out.contains("OVERRIDE") || out.contains("take PRECEDENCE"),
            "states override: {out}"
        );
    }

    #[test]
    fn no_precedence_preamble_when_no_instructions() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        let out = render_instructions(&d.path().join("nohome"), &proj);
        assert!(
            out.is_empty(),
            "no tiers → fully empty, no preamble: {out:?}"
        );
    }

    #[test]
    fn missing_and_empty_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("AGENTS.md"), "   \n  ").unwrap();
        let out = render_instructions(&d.path().join("nohome"), &proj);
        assert!(out.is_empty(), "no non-empty tiers → empty: {out:?}");
    }

    #[test]
    fn large_file_is_not_truncated() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        let big = format!("prefix-{}-suffix", "x".repeat(1_100_000));
        fs::write(proj.join("AGENTS.md"), &big).unwrap();
        let out = render_instructions(&d.path().join("nohome"), &proj);
        assert!(!out.contains("[truncated:"));
        assert!(out.contains("prefix-") && out.contains("-suffix"));
        assert!(out.len() > 1_100_000);
    }

    #[test]
    fn domain_glossary_is_additive_with_agents_md() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("proj");
        fs::create_dir_all(proj.join(".atomcode")).unwrap();
        fs::write(proj.join("AGENTS.md"), "build with cargo").unwrap();
        fs::write(
            proj.join(".atomcode/glossary.md"),
            "- 优惠券 → Coupon, PromoCode, Voucher\n- 结算 → Checkout, Settlement\n",
        )
        .unwrap();
        let out = render_instructions(&d.path().join("nohome"), &proj);
        assert!(out.contains("PROJECT INSTRUCTIONS") && out.contains("build with cargo"));
        assert!(out.contains("DOMAIN GLOSSARY") && out.contains("优惠券"));
        assert!(out.contains("expand each term into the listed code aliases"));
        fs::write(proj.join("DOMAIN.md"), "should not win").unwrap();
        let out2 = render_instructions(&d.path().join("nohome"), &proj);
        assert!(!out2.contains("should not win"));
    }

    #[test]
    fn domain_glossary_falls_back_to_docs_path() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("proj");
        fs::create_dir_all(proj.join("docs")).unwrap();
        fs::write(
            proj.join("docs/domain-glossary.md"),
            "invoice → Fapiao, InvoiceService\n",
        )
        .unwrap();
        let out = render_instructions(&d.path().join("nohome"), &proj);
        assert!(out.contains("DOMAIN GLOSSARY"));
        assert!(out.contains("InvoiceService"));
    }

    #[test]
    fn rules_and_dbwords_are_additive() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("proj");
        fs::create_dir_all(proj.join(".atomcode")).unwrap();
        fs::write(proj.join("AGENTS.md"), "use cargo").unwrap();
        fs::write(
            proj.join(".atomcode/rules.md"),
            "## 组织架构\n- 中台 / 交易中心 / 营销中心\n",
        )
        .unwrap();
        fs::write(
            proj.join(".atomcode/dbwords.md"),
            "| 业务 | 表名 |\n| 优惠券 | t_coupon |\n",
        )
        .unwrap();
        let out = render_instructions(&d.path().join("nohome"), &proj);
        assert!(out.contains("PROJECT INSTRUCTIONS") && out.contains("use cargo"));
        assert!(
            out.contains("BUSINESS RULES") && out.contains("组织架构") && out.contains("营销中心"),
            "rules.md injected: {out}"
        );
        assert!(
            out.contains("DB WORDS") && out.contains("t_coupon"),
            "dbwords.md injected: {out}"
        );
        assert!(out.contains("organization structure") || out.contains("org"));
        assert!(out.contains("database tables") || out.contains("schema"));
    }

    #[test]
    fn rules_prefers_atomcode_dir_over_root() {
        let d = tempfile::tempdir().unwrap();
        let proj = d.path().join("proj");
        fs::create_dir_all(proj.join(".atomcode")).unwrap();
        fs::write(proj.join(".atomcode/rules.md"), "nested-rules").unwrap();
        fs::write(proj.join("rules.md"), "root-rules-should-lose").unwrap();
        let out = render_instructions(&d.path().join("nohome"), &proj);
        assert!(out.contains("nested-rules"));
        assert!(!out.contains("root-rules-should-lose"));
    }
}
