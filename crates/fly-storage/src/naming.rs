//! Human-readable disk names. Meeting folders and note mirrors are named
//! `<YYYY-MM-DD> <title>` (local date, so Explorer's name sort is
//! chronological), sanitized to be legal on every filesystem Fly on the Wall targets —
//! Windows rules are the strictest — and deduped with ` (2)`, ` (3)`… when
//! two same-day artifacts share a title.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, Utc};

/// Longest sanitized title kept in a disk name. Keeps the whole path far
/// below Windows' 260-char default even with the data dir + suffixes.
const MAX_TITLE_CHARS: usize = 60;

/// Windows device names that are illegal as a file stem regardless of
/// extension. The date prefix in [`disk_label`] already avoids them; the
/// check here keeps `sanitize_title` safe standalone.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Make a title safe as a single Windows/macOS/Linux path component:
/// illegal characters become spaces, whitespace is collapsed, trailing
/// dots/spaces are trimmed (illegal on Windows), length is capped, and an
/// empty result falls back to "Untitled".
pub fn sanitize_title(title: &str) -> String {
    let mapped: String = title
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => ' ',
            c if (c as u32) < 0x20 => ' ',
            c => c,
        })
        .collect();
    let mut s = mapped.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > MAX_TITLE_CHARS {
        s = s.chars().take(MAX_TITLE_CHARS).collect();
    }
    let s = s.trim_end_matches(['.', ' ']);
    if s.is_empty() {
        return "Untitled".to_string();
    }
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(s)) {
        return format!("{s}_");
    }
    s.to_string()
}

/// `<YYYY-MM-DD> <sanitized title>` — the base name for a meeting folder or
/// note mirror. The timestamp is rendered in the machine's local timezone to
/// match the user's mental model of "the Tuesday meeting".
pub fn disk_label(when: DateTime<Utc>, title: &str) -> String {
    format!(
        "{} {}",
        when.with_timezone(&Local).format("%Y-%m-%d"),
        sanitize_title(title)
    )
}

/// Best-effort inverse of [`disk_label`]: strip the leading `YYYY-MM-DD `
/// (and a trailing ` (n)` dedup suffix) to recover a display title from a
/// folder name. Used when resurrecting a meeting whose database row is gone.
pub fn strip_date_prefix(name: &str) -> String {
    let mut s = name;
    let bytes = s.as_bytes();
    let dateish = bytes.len() >= 11
        && bytes[..10].iter().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
        && bytes[10] == b' ';
    if dateish {
        s = &s[11..];
    }
    // trailing " (n)" dedup suffix
    if let Some(idx) = s.rfind(" (") {
        let rest = &s[idx + 2..];
        if rest.ends_with(')') && rest[..rest.len() - 1].chars().all(|c| c.is_ascii_digit()) {
            s = &s[..idx];
        }
    }
    let s = s.trim();
    if s.is_empty() {
        "Recovered meeting".to_string()
    } else {
        s.to_string()
    }
}

/// Does `name` already carry `base` (allowing a ` (n)` dedup suffix)? Lets
/// rename paths and migration reruns skip artifacts already in shape.
pub(crate) fn already_labeled(name: &str, base: &str) -> bool {
    name == base
        || name
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with(" ("))
}

/// First free `<base><ext>`, `<base> (2)<ext>`, … under `parent`. `taken`
/// lets callers exclude names reserved elsewhere (e.g. rows in the DB whose
/// mirror file hasn't been written yet).
pub fn unique_path(parent: &Path, base: &str, ext: &str, taken: &dyn Fn(&Path) -> bool) -> PathBuf {
    for n in 1u32.. {
        let name = if n == 1 {
            format!("{base}{ext}")
        } else {
            format!("{base} ({n}){ext}")
        };
        let candidate = parent.join(name);
        if !candidate.exists() && !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!("u32 exhausted allocating a unique path")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_windows_illegal_chars() {
        assert_eq!(
            sanitize_title("TB<1:1>SSP July 2 2026"),
            "TB 1 1 SSP July 2 2026"
        );
        assert_eq!(sanitize_title("a/b\\c|d?e*f\"g"), "a b c d e f g");
        assert_eq!(sanitize_title("  spaced   out  "), "spaced out");
    }

    #[test]
    fn sanitize_trims_trailing_dots_and_handles_empty() {
        assert_eq!(sanitize_title("ends with dots..."), "ends with dots");
        assert_eq!(sanitize_title(""), "Untitled");
        assert_eq!(sanitize_title("///"), "Untitled");
    }

    #[test]
    fn sanitize_caps_length_on_char_boundary() {
        let long = "é".repeat(200);
        let s = sanitize_title(&long);
        assert_eq!(s.chars().count(), 60);
    }

    #[test]
    fn sanitize_avoids_reserved_device_names() {
        assert_eq!(sanitize_title("con"), "con_");
        assert_eq!(sanitize_title("LPT1"), "LPT1_");
        assert_eq!(sanitize_title("console"), "console");
    }

    #[test]
    fn strip_date_prefix_inverts_disk_label() {
        assert_eq!(strip_date_prefix("2026-07-13 Big meeting"), "Big meeting");
        assert_eq!(strip_date_prefix("2026-07-13 Standup (2)"), "Standup");
        assert_eq!(strip_date_prefix("no date here"), "no date here");
        assert_eq!(strip_date_prefix("2026-07-13 "), "Recovered meeting");
        assert_eq!(strip_date_prefix(""), "Recovered meeting");
    }

    #[test]
    fn disk_label_prefixes_local_date() {
        let when = "2026-07-02T16:06:00Z".parse::<DateTime<Utc>>().unwrap();
        let label = disk_label(when, "Tina 1-1");
        let date = when.with_timezone(&Local).format("%Y-%m-%d").to_string();
        assert_eq!(label, format!("{date} Tina 1-1"));
    }

    #[test]
    fn unique_path_dedupes_against_disk_and_taken() {
        let dir = tempfile::tempdir().unwrap();
        let first = unique_path(dir.path(), "2026-07-02 Standup", ".md", &|_| false);
        assert!(first.ends_with("2026-07-02 Standup.md"));
        std::fs::write(&first, "x").unwrap();

        let second = unique_path(dir.path(), "2026-07-02 Standup", ".md", &|_| false);
        assert!(second.ends_with("2026-07-02 Standup (2).md"));

        let third = unique_path(dir.path(), "2026-07-02 Standup", ".md", &|p| p == second);
        assert!(third.ends_with("2026-07-02 Standup (3).md"));
    }
}
