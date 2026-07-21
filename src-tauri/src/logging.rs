//! File-backed diagnostics: everything tracing writes also lands under
//! `<data dir>/logs`, because a packaged app's stdout is unrecoverable —
//! the community bug reports that motivated this arrived as screenshots.
//! Local files only; nothing ever leaves the machine.

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
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
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
