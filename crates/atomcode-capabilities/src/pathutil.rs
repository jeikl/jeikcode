//! Path helpers shared by the `tools` and `codeintel` tool families. Kept OUTSIDE
//! `tools/` and free of any feature `cfg` because `codeintel` is deliberately
//! independent of the `tools` feature (see `codeintel/mod.rs`) yet must resolve
//! model-supplied paths the SAME way — including leading-`~` expansion and
//! cross-platform absolute vs relative classification (POSIX `/…`, Windows
//! drive/UNC) so `read_file`, `code_explore`, `repo_map`, and `glob` agree with
//! the shell the `bash` tool runs.

use std::path::{Path, PathBuf};

/// The user's home directory, dependency-free (`HOME`, or `USERPROFILE` on Windows).
pub(crate) fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let var = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let var = std::env::var_os("HOME");
    var.map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

/// Expand a leading `~`/`~/` (and, on Windows, `~\`) against `home`; returns `None`
/// when `raw` is not a home-relative tilde form so the caller's absolute/relative
/// handling runs unchanged. Deliberately does NOT expand `~user/…` (needs a passwd
/// lookup), Windows 8.3 short names (`PROGRA~1`), or Office temp/lock files
/// (`~$doc.docx`).
pub(crate) fn expand_tilde_with_home(raw: &str, home: Option<&Path>) -> Option<PathBuf> {
    let rest = tilde_rest(raw)?;
    let home = home?;
    if rest.is_empty() {
        return Some(home.to_path_buf());
    }
    // `Path::join` REPLACES the whole path when its argument is absolute, so a `rest`
    // beginning with a separator (`~//etc` → rest `/etc`) would escape home entirely.
    // Strip leading separators so `rest` is ALWAYS joined as home-relative (the shell
    // keeps `~//etc` under `$HOME` too).
    let rest = rest.trim_start_matches(|c| c == '/' || (cfg!(windows) && c == '\\'));
    Some(home.join(rest))
}

/// [`expand_tilde_with_home`] using the process home. Checks the (cheap, alloc-free)
/// tilde shape BEFORE reading the environment, so a non-tilde path pays no env read.
pub(crate) fn expand_tilde(raw: &str) -> Option<PathBuf> {
    tilde_rest(raw)?; // fast-path: skip the env read entirely unless there's a `~`
    expand_tilde_with_home(raw, home_dir().as_deref())
}

/// Resolve a model-supplied path on every platform: leading `~`/`~/` → home;
/// POSIX `/…` and Windows drive/UNC → absolute (never joined onto `working_dir`);
/// relative → joined to `working_dir`.
///
/// On Windows, Git-Bash POSIX forms (`/tmp/foo`, `/c/Users/foo`) are mapped to the
/// native location so they do not become `{cwd_drive}:\tmp\foo`. On Unix those
/// strings are already native absolute paths and are left unchanged.
pub(crate) fn resolve_path(raw: &str, working_dir: &Path) -> PathBuf {
    if let Some(home) = expand_tilde(raw) {
        return home;
    }
    if let Some(translated) = maybe_translate_posix_absolute(raw) {
        return translated;
    }
    if is_absolute_path(raw) {
        PathBuf::from(raw)
    } else {
        working_dir.join(raw)
    }
}

/// Cross-platform absolute-path test for model-supplied paths.
///
/// `Path::is_absolute()` is platform-dependent:
/// - Unix rejects `G:\foo` (one relative name) → `working_dir.join` produces garbage.
/// - Windows rejects POSIX `/tmp/foo` (root, no drive) → join becomes `{drive}:\tmp\foo`.
///
/// Recognize POSIX `/…`, Windows `C:\` / `C:/`, and UNC `\\server\share` on every
/// build target so Linux CI, macOS, and Windows agree.
pub(crate) fn is_absolute_path(raw: &str) -> bool {
    if Path::new(raw).is_absolute() {
        return true;
    }
    let b = raw.as_bytes();
    if !b.is_empty() && b[0] == b'/' {
        return true;
    }
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        return true;
    }
    b.len() >= 2 && b[0] == b'\\' && b[1] == b'\\'
}

/// On Windows, map a POSIX-absolute path that Git Bash / a Linux-trained model
/// would emit onto a native path. No-op off Windows (POSIX `/tmp` is already native,
/// and remapping it to `$TMPDIR` would mis-resolve `/tmp` on macOS).
///
/// Windows `Path::join` treats `/tmp/foo` as "root, no drive prefix" and produces
/// `{cwd_drive}:\tmp\foo` (e.g. workspace on `E:` → `E:\tmp\foo`). Git for Windows
/// mounts `%TEMP%` at `/tmp`, and MSYS drive form is `/c/Users/...` → `C:\Users\...`.
pub(crate) fn maybe_translate_posix_absolute(raw: &str) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    translate_posix_absolute_for_windows(raw, &std::env::temp_dir())
}

/// Pure mapper used by [`maybe_translate_posix_absolute`]. `temp_dir` is injected so
/// unit tests run on Unix CI without reading the host `%TEMP%`. Returns `None` when
/// `raw` is not a POSIX-absolute form we know how to map.
pub(crate) fn translate_posix_absolute_for_windows(raw: &str, temp_dir: &Path) -> Option<PathBuf> {
    let s = raw.trim();
    if s.len() < 2 || !s.starts_with('/') || s.starts_with("//") {
        return None;
    }
    let s = s.replace('\\', "/");
    if let Some(mapped) = map_tmp_prefix(&s, temp_dir) {
        return Some(mapped);
    }
    if let Some(rest) = s.strip_prefix("/mnt/") {
        return map_msys_drive(&format!("/{rest}"));
    }
    if let Some(rest) = s.strip_prefix("/cygdrive/") {
        return map_msys_drive(&format!("/{rest}"));
    }
    map_msys_drive(&s)
}

const TMP_PREFIXES: &[&str] = &["/tmp", "/var/tmp", "/private/tmp"];

fn map_tmp_prefix(s: &str, temp_dir: &Path) -> Option<PathBuf> {
    for prefix in TMP_PREFIXES {
        if s == *prefix {
            return Some(temp_dir.to_path_buf());
        }
        if let Some(rest) = s.strip_prefix(prefix) {
            let rest = rest.strip_prefix('/')?;
            return Some(join_posix_under(temp_dir, rest));
        }
    }
    None
}

fn join_posix_under(base: &Path, rest: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for comp in rest.split('/') {
        if !comp.is_empty() {
            p.push(comp);
        }
    }
    p
}

/// `/c/Users/x` → `C:/Users/x`; `/e` → `E:/`. First component must be exactly one
/// ASCII letter so `/tmp`, `/dev/null`, `/etc/hosts` are not treated as drives.
fn map_msys_drive(s: &str) -> Option<PathBuf> {
    let rest = s.strip_prefix('/')?;
    let mut chars = rest.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    match chars.next() {
        None => Some(windows_drive_path(letter, "")),
        Some('/') => Some(windows_drive_path(letter, chars.as_str())),
        Some(_) => None,
    }
}

fn windows_drive_path(letter: char, rest: &str) -> PathBuf {
    let mut p = PathBuf::from(format!("{}:/", letter.to_ascii_uppercase()));
    for comp in rest.split('/') {
        if !comp.is_empty() {
            p.push(comp);
        }
    }
    p
}

/// The part of `raw` AFTER a leading `~` separator (empty for a bare `~`), or `None`
/// if `raw` is not a home-relative tilde form. Matches only `~`, `~/…`, and — on
/// Windows, where `\` is a separator — `~\…`.
fn tilde_rest(raw: &str) -> Option<&str> {
    if raw == "~" {
        return Some("");
    }
    raw.strip_prefix("~/").or_else(|| {
        if cfg!(windows) {
            raw.strip_prefix(r"~\")
        } else {
            None
        }
    })
}

/// The user's real home directory, resolving `SUDO_USER` first (so `atomcode`
/// launched under `sudo` still finds the invoking user's home, not root's).
/// Falls back to `dirs::home_dir()`. Ported from `core::tool::real_home_dir`
/// for the `plugin` feature (installer `~` expansion).
#[cfg(feature = "plugin")]
pub fn real_home_dir() -> Option<PathBuf> {
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        if let Some(home) = get_user_home(&sudo_user) {
            return Some(home);
        }
    }
    dirs::home_dir()
}

/// Look up a user's home via `getpwnam_r` (Unix). Returns `None` off-Unix.
#[cfg(all(feature = "plugin", unix))]
fn get_user_home(username: &str) -> Option<PathBuf> {
    use std::ffi::CString;
    use std::ptr;

    let username_c = CString::new(username).ok()?;
    // SAFETY: getpwnam_r is the thread-safe passwd lookup; buffers are sized and owned here.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut buf = vec![0u8; 4096];
        let mut result: *mut libc::passwd = ptr::null_mut();
        let ret = libc::getpwnam_r(
            username_c.as_ptr(),
            &mut pwd,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            &mut result,
        );
        if ret == 0 && !result.is_null() {
            let home = std::ffi::CStr::from_ptr(pwd.pw_dir)
                .to_string_lossy()
                .into_owned();
            return Some(PathBuf::from(home));
        }
    }
    None
}

#[cfg(all(feature = "plugin", not(unix)))]
fn get_user_home(_username: &str) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_leading_tilde_to_home() {
        let home = Path::new("/Users/csdn");
        assert_eq!(
            expand_tilde_with_home("~/.atomcode/x", Some(home)),
            Some(PathBuf::from("/Users/csdn/.atomcode/x"))
        );
        // A bare `~` (and `~/`) is the home dir itself.
        assert_eq!(
            expand_tilde_with_home("~", Some(home)),
            Some(PathBuf::from("/Users/csdn"))
        );
        assert_eq!(
            expand_tilde_with_home("~/", Some(home)),
            Some(PathBuf::from("/Users/csdn"))
        );
    }

    #[test]
    fn tilde_rest_with_leading_separators_stays_under_home() {
        // `Path::join` REPLACES on an absolute arg, so a `rest` beginning with a
        // separator (`~//etc/passwd` → rest `/etc/passwd`) must NOT escape home.
        let home = Path::new("/Users/csdn");
        assert_eq!(
            expand_tilde_with_home("~//etc/passwd", Some(home)),
            Some(PathBuf::from("/Users/csdn/etc/passwd"))
        );
        assert_eq!(
            expand_tilde_with_home("~//", Some(home)),
            Some(PathBuf::from("/Users/csdn"))
        );
    }

    #[test]
    fn non_home_forms_are_not_expanded() {
        let home = Path::new("/Users/csdn");
        // `~user/…`, 8.3 short names, Office temp files: not home-relative.
        assert_eq!(expand_tilde_with_home("~bob/notes.txt", Some(home)), None);
        assert_eq!(expand_tilde_with_home("PROGRA~1", Some(home)), None);
        assert_eq!(expand_tilde_with_home("~$report.docx", Some(home)), None);
        // Ordinary relative / absolute paths are not tilde forms.
        assert_eq!(expand_tilde_with_home("src/main.rs", Some(home)), None);
        assert_eq!(expand_tilde_with_home("/etc/hosts", Some(home)), None);
    }

    #[test]
    fn no_home_declines_expansion() {
        // No resolvable home → decline, so the caller degrades to its relative-join.
        assert_eq!(expand_tilde_with_home("~/.atomcode/x", None), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn backslash_is_literal_on_unix() {
        // On Unix `\` is an ordinary filename char, so `~\foo` is not a tilde form.
        assert_eq!(
            expand_tilde_with_home(r"~\foo", Some(Path::new("/Users/csdn"))),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn backslash_expands_on_windows() {
        let home = Path::new(r"C:\Users\csdn");
        assert_eq!(
            expand_tilde_with_home(r"~\.atomcode\x", Some(home)),
            Some(PathBuf::from(r"C:\Users\csdn\.atomcode\x"))
        );
        // Doubled separator must not escape to the drive root.
        assert_eq!(
            expand_tilde_with_home(r"~\\Windows", Some(home)),
            Some(PathBuf::from(r"C:\Users\csdn\Windows"))
        );
    }

    #[cfg(feature = "plugin")]
    #[test]
    fn real_home_dir_without_sudo_matches_dirs_home() {
        // In the normal (non-sudo) test process there is no SUDO_USER, so real_home_dir
        // must equal dirs::home_dir(). (We do not mutate SUDO_USER — env is process-global
        // and would race other tests.)
        if std::env::var("SUDO_USER").is_err() {
            assert_eq!(super::real_home_dir(), dirs::home_dir());
        }
    }

    fn fwd(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    #[test]
    fn posix_tmp_maps_onto_injected_temp_dir() {
        let tmp = Path::new("win-temp");
        assert_eq!(
            translate_posix_absolute_for_windows("/tmp/submit_comment.md", tmp),
            Some(tmp.join("submit_comment.md"))
        );
        assert_eq!(
            translate_posix_absolute_for_windows("/tmp", tmp),
            Some(tmp.to_path_buf())
        );
        assert_eq!(
            translate_posix_absolute_for_windows("/tmp/", tmp),
            Some(tmp.to_path_buf())
        );
        assert_eq!(
            translate_posix_absolute_for_windows("/var/tmp/x.log", tmp),
            Some(tmp.join("x.log"))
        );
        // `/tmpfoo` is not `/tmp/...`.
        assert_eq!(translate_posix_absolute_for_windows("/tmpfoo", tmp), None);
    }

    #[test]
    fn posix_msys_and_wsl_drive_forms_map_to_windows_drive() {
        let tmp = Path::new("win-temp");
        assert_eq!(
            fwd(
                &translate_posix_absolute_for_windows("/e/my/dingtalk-workspace-cli", tmp).unwrap()
            )
            .to_lowercase(),
            "e:/my/dingtalk-workspace-cli"
        );
        assert_eq!(
            fwd(&translate_posix_absolute_for_windows("/c/Users/x", tmp).unwrap()).to_lowercase(),
            "c:/Users/x".to_lowercase()
        );
        assert_eq!(
            fwd(&translate_posix_absolute_for_windows("/mnt/c/Users/x", tmp).unwrap())
                .to_lowercase(),
            "c:/Users/x".to_lowercase()
        );
        assert_eq!(
            fwd(&translate_posix_absolute_for_windows("/cygdrive/e/tmp/f", tmp).unwrap())
                .to_lowercase(),
            "e:/tmp/f"
        );
    }

    #[test]
    fn posix_translator_ignores_relative_unc_and_non_drive_roots() {
        let tmp = Path::new("win-temp");
        assert_eq!(
            translate_posix_absolute_for_windows("src/main.rs", tmp),
            None
        );
        assert_eq!(
            translate_posix_absolute_for_windows(r"C:\Windows", tmp),
            None
        );
        assert_eq!(
            translate_posix_absolute_for_windows("//server/share/f", tmp),
            None
        );
        assert_eq!(translate_posix_absolute_for_windows("/dev/null", tmp), None);
        assert_eq!(
            translate_posix_absolute_for_windows("/etc/hosts", tmp),
            None
        );
        assert_eq!(
            translate_posix_absolute_for_windows("/abs/dir/file.rs", tmp),
            None
        );
    }

    #[test]
    fn is_absolute_path_agrees_on_posix_and_windows_roots_on_every_os() {
        assert!(is_absolute_path("/tmp/submit_comment.md"));
        assert!(is_absolute_path("/usr/bin/ls"));
        assert!(is_absolute_path("G:/VR2024/keystore"));
        assert!(is_absolute_path(r"G:\VR2024\keystore"));
        assert!(is_absolute_path(r"\\server\share\f"));
        assert!(!is_absolute_path("src/main.rs"));
        assert!(!is_absolute_path("./foo"));
        assert!(!is_absolute_path("C:foo"));
    }

    #[test]
    fn resolve_path_never_joins_absolute_forms_onto_cwd() {
        // Runs on Linux, macOS, and Windows: POSIX `/…` and Windows drive/UNC must
        // not be treated as relative and joined under the working dir.
        let wd = Path::new("/work/proj");
        let tmp = resolve_path("/tmp/submit_comment.md", wd);
        #[cfg(windows)]
        {
            assert_eq!(tmp, std::env::temp_dir().join("submit_comment.md"));
            assert_ne!(
                tmp,
                wd.join("/tmp/submit_comment.md"),
                "POSIX /tmp must not become {{cwd_drive}}:/tmp/..."
            );
            let msys = resolve_path("/e/my/dingtalk-workspace-cli/src/main.rs", wd);
            assert_eq!(
                crate::pathnorm::to_display(&msys).to_lowercase(),
                "e:/my/dingtalk-workspace-cli/src/main.rs"
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(tmp, PathBuf::from("/tmp/submit_comment.md"));
            assert_ne!(tmp, wd.join("tmp/submit_comment.md"));
        }
        assert_eq!(
            resolve_path("/usr/bin/ls", wd),
            PathBuf::from("/usr/bin/ls")
        );
        assert_eq!(
            resolve_path("G:/VR2024/keystore", wd),
            PathBuf::from("G:/VR2024/keystore")
        );
        assert_eq!(
            resolve_path(r"G:\VR2024\keystore", wd),
            PathBuf::from(r"G:\VR2024\keystore")
        );
        assert_eq!(
            resolve_path(r"\\server\share\f", wd),
            PathBuf::from(r"\\server\share\f")
        );
        assert_eq!(resolve_path("src/main.rs", wd), wd.join("src/main.rs"));
    }
}
