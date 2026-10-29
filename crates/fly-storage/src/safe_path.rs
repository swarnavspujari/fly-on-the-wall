//! Containment for data-dir-relative paths that arrive from outside the
//! backend (IPC arguments, `manifest.json` files copied in from elsewhere).
//!
//! Lexical checks only, deliberately strict: every component must be a plain
//! name that means the same thing on Windows and Unix. That rules out the
//! Win32 normalisation tricks a plain `Component::Normal` check misses —
//! trailing dots/spaces (`recordings/...` would collapse to `recordings`),
//! alternate data streams (`a:b`), reserved device names (`NUL`, `COM1`).

use std::path::{Component, Path, PathBuf};

/// One path component that is a plain name on both Windows and Unix: not
/// empty / `.` / `..`, no leading space, no trailing dot or space, no
/// separators, `:` (drive / alternate stream), `<>"|?*` or control chars,
/// and not a reserved device name (`NUL`, `com1.txt`, …). Also used by the
/// app's archive extractor to vet zip entry names.
pub fn is_plain_name(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.ends_with('.') || name.ends_with(' ') || name.starts_with(' ') {
        return false;
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*' | '\\' | '/')
    }) {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    !RESERVED.contains(&stem.as_str())
}

/// A note or meeting id that is safe to use as one path component: 1–64
/// lowercase ASCII letters, digits, `-` or `_`, and a plain name (not a
/// reserved device name). Every id this app creates is a lowercase UUID.
/// Lowercase only, so two different ids can never name the same folder on a
/// file system that ignores case. Ids read from a copied-in `manifest.json`
/// must pass this before they become rows, and paths are built only from
/// ids that pass it (`attachments/<note_id>/`, the scratch dir).
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && is_plain_name(id)
}

/// `rel` as a relative path whose first component is one of `roots` and
/// which has at least `min_depth` components, every one a plain name.
/// Returns the path joined onto `data_dir`, or `None`.
pub fn contained(data_dir: &Path, rel: &str, roots: &[&str], min_depth: usize) -> Option<PathBuf> {
    // Normalise separators first so `a\b` and `a/b` are judged the same on
    // every OS (a backslash is a literal name char on Unix).
    // Judge the raw segments: `Path::components` silently drops `.` and
    // empty segments, and a leading `/` or `//?/` (from `\\?\`) shows up here
    // as an empty first segment, which is rejected like any other.
    let unified = rel.replace('\\', "/");
    let segments: Vec<&str> = unified.split('/').collect();
    for (i, name) in segments.iter().enumerate() {
        if !is_plain_name(name) || (i == 0 && !roots.contains(name)) {
            return None;
        }
    }
    // Belt and braces: the parsed path must agree it's purely relative names.
    let all_normal = Path::new(&unified)
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    (all_normal && segments.len() >= min_depth).then(|| data_dir.join(unified))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paths_under_a_root_pass() {
        let base = Path::new("data");
        assert!(contained(base, "recordings/2026-10-23 Standup", &["recordings"], 2).is_some());
        assert!(contained(base, r"recordings\x\a.wav", &["recordings"], 2).is_some());
        assert!(contained(base, "attachments/n1/report.pdf", &["attachments"], 2).is_some());
    }

    #[test]
    fn windows_normalisation_tricks_are_rejected() {
        let base = Path::new("data");
        for bad in [
            "",
            "recordings",
            "recordings/...",
            "recordings/ ",
            "recordings/x.",
            "recordings/x /a.wav",
            "recordings/a:stream",
            "recordings/NUL",
            "recordings/com1.txt",
            "recordings/../x",
            "recordings/./x",
            "/etc/passwd",
            r"C:\Users\victim",
            "C:foo/bar",
            r"\\server\share\x",
            r"\\?\C:\x",
            "attachments/x",
            "recordings/a\u{0}b",
        ] {
            assert!(
                contained(base, bad, &["recordings"], 2).is_none(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn only_short_plain_ids_are_safe() {
        assert!(is_safe_id("862e2405-a77a-48ad-af38-fefcda797fa0"));
        assert!(is_safe_id("m-orig"));
        assert!(is_safe_id("n_1"));
        assert!(is_safe_id(&fly_core::new_id()), "the app's own ids pass");
        for bad in [
            "862E2405-A77A-48AD-AF38-FEFCDA797FA0",
            "M-orig",
            "con",
            "nul",
            "com1",
            "lpt9",
            "aux",
            "",
            ".",
            "..",
            "../x",
            "a/b",
            r"a\b",
            "/abs",
            r"C:\x",
            "C:x",
            r"\\server\share",
            "a.b",
            "a b",
            "NUL\u{0}",
            "é",
            &"x".repeat(65),
        ] {
            assert!(!is_safe_id(bad), "{bad:?} must not be a safe id");
        }
    }
}
