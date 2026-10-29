//! File-backed diagnostics: everything tracing writes also lands under
//! `<data dir>/logs`, because a packaged app's stdout is unrecoverable —
//! the community bug reports that motivated this arrived as screenshots.
//! Local files only; nothing ever leaves the machine.
//!
//! **Log hygiene rule.** Log files get attached to bug reports, so no
//! `tracing` call anywhere in the workspace may carry user content: meeting
//! or note titles, folder names or paths under `recordings/` / `notes/`
//! (they are named after meetings), transcript, note or prompt text,
//! attendee names or emails, calendar ids, tokens or keys. Log ids
//! (`meeting_id`, `note_id`), counts, sizes, durations and bare file names
//! instead. `tests::no_user_content_in_log_calls` enforces a lexical version
//! of this over `crates/` and `src-tauri/src`.

use std::path::Path;

/// Directory under the app data dir the rolling log files live in
/// (`reveal_logs_dir` opens it from Settings → Technical).
pub const LOGS_DIR: &str = "logs";
/// Rotated daily; only the newest few files are kept — a few quiet MB at
/// info level, never a runaway disk hog.
const MAX_LOG_FILES: usize = 5;
const FILE_PREFIX: &str = "flyonthewall";

/// Install the global subscriber (stdout + rolling file when a data dir is
/// available) and a panic hook that logs before the process dies. Called once
/// at startup, before anything logs; file-logging failures degrade to
/// stdout-only — diagnostics must never block launch.
pub fn init(data_dir: Option<&Path>) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // Blocking writes to the file on purpose: volume is low (info level) and
    // a panic's final lines must reach disk before the abort — a non-blocking
    // writer would lose exactly the lines this module exists to keep.
    let file_layer = data_dir.and_then(rolling_appender).map(|appender| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(appender)
    });
    tracing_subscriber::registry()
        .with(
            // rmcp logs OAuth details (an authorization code) at debug;
            // keep it at warn unless RUST_LOG asks otherwise.
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rmcp=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(file_layer)
        .init();

    // First line of every session: which build produced this file.
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        "Fly on the Wall starting"
    );

    // A panic's message + location must reach the log before the process
    // dies — a flash-crash with empty logs is the failure mode this hook
    // closes. Chain to the previous hook (stderr backtrace, abort) after.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(ToString::to_string)
            .unwrap_or_else(|| "<unknown location>".into());
        tracing::error!(panic = payload_str(info.payload()), location, "panic");
        previous(info);
    }));
}

/// Daily-rotated appender under `<data dir>/logs`, capped at
/// [`MAX_LOG_FILES`]. `None` when the directory can't be created/opened.
fn rolling_appender(data_dir: &Path) -> Option<tracing_appender::rolling::RollingFileAppender> {
    tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(data_dir.join(LOGS_DIR))
        .map_err(|e| eprintln!("file logging unavailable: {e}"))
        .ok()
}

/// The human text of a panic payload (`panic!("…")` gives `&str`, formatted
/// panics give `String`; anything else has no message to extract).
fn payload_str(payload: &dyn std::any::Any) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The appender must create `<data dir>/logs` and land bytes in a
    /// prefix-named file — the invariant `reveal_logs_dir` and the smoke
    /// checklist rely on.
    #[test]
    fn rolling_appender_writes_into_logs_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut appender = rolling_appender(dir.path()).expect("appender builds");
        appender.write_all(b"probe line\n").unwrap();
        appender.flush().unwrap();
        let logs = dir.path().join(LOGS_DIR);
        let entries: Vec<_> = std::fs::read_dir(&logs)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries.iter().any(|n| n.starts_with(FILE_PREFIX)),
            "expected a {FILE_PREFIX}* file in {}: {entries:?}",
            logs.display()
        );
    }

    /// Every `trace!/debug!/info!/warn!/error!(…)` call in `src` (with its
    /// balanced argument list), as (file, call text).
    fn log_calls(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "target") {
                    log_calls(&path, out);
                }
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            for mac in ["trace!(", "debug!(", "info!(", "warn!(", "error!("] {
                let mut from = 0;
                while let Some(i) = src[from..].find(mac).map(|i| i + from) {
                    from = i + mac.len();
                    let word_start = src[..i]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                    if !word_start {
                        continue;
                    }
                    let mut depth = 0usize;
                    let mut end = src.len();
                    for (j, c) in src[i + mac.len() - 1..].char_indices() {
                        match c {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = i + mac.len() - 1 + j + 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    out.push((path.display().to_string(), src[i..end].to_string()));
                }
            }
        }
    }

    /// Lexical enforcement of the module's log hygiene rule. A hit here
    /// means a log call names user content; log an id or count instead.
    #[test]
    fn no_user_content_in_log_calls() {
        const BANNED: &[&str] = &[
            ".title",
            "title =",
            "email",
            ".phrase,",
            "phrase =",
            "prompt =",
            ".prompt",
            "access_token",
            "refresh_token",
            "api_key",
            ".text",
            "cal_id",
            "dir = %rel",
            "stderr,",
        ];
        // `.display()` of a path is banned except for app-owned locations
        // that never contain meeting names (model downloads, the data dir
        // root, the managed Ollama binary) — and this test itself.
        const DISPLAY_OK: &[&str] = &["models.rs", "state.rs", "ollama.rs", "logging.rs"];
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut calls = Vec::new();
        log_calls(&root.join("src"), &mut calls);
        log_calls(&root.join("../crates"), &mut calls);
        assert!(
            calls.len() > 50,
            "scanner found too few calls: {}",
            calls.len()
        );
        let offenders: Vec<String> = calls
            .iter()
            .filter(|(file, _)| !file.ends_with("logging.rs"))
            .filter(|(file, call)| {
                BANNED.iter().any(|b| call.contains(b))
                    || (call.contains(".display()")
                        && !DISPLAY_OK.iter().any(|ok| file.ends_with(ok)))
            })
            .map(|(file, call)| format!("{file}: {call}"))
            .collect();
        assert!(
            offenders.is_empty(),
            "log calls carrying user content:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn payload_str_extracts_both_panic_shapes() {
        let s: Box<dyn std::any::Any> = Box::new("plain &str panic");
        assert_eq!(payload_str(s.as_ref()), "plain &str panic");
        let owned: Box<dyn std::any::Any> = Box::new(String::from("formatted panic"));
        assert_eq!(payload_str(owned.as_ref()), "formatted panic");
        let other: Box<dyn std::any::Any> = Box::new(42u32);
        assert_eq!(payload_str(other.as_ref()), "<non-string panic payload>");
    }
}
