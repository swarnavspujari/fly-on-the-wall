//! Navigation guard: the app window may only ever show the app itself.
//!
//! Links in AI-written notes or imported content must never load a remote
//! page inside the trusted window (a phishing surface with no address bar, and
//! a page that could talk to the app). The frontend already routes link clicks
//! to the system browser; this is the backstop for anything that slips past.
//! External http(s)/mailto targets are handed to the default browser instead.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime, Url};
use tauri_plugin_opener::OpenerExt;

/// Whether `url` is the app's own frontend (production or `tauri dev`).
pub fn is_app_url(url: &Url) -> bool {
    match url.scheme() {
        // macOS/Linux production origin, and Tauri's internal schemes.
        "tauri" | "asset" | "ipc" => true,
        "about" => url.as_str() == "about:blank",
        "http" | "https" => match url.host_str() {
            // Windows production origin.
            Some("tauri.localhost") => true,
            // Vite dev server (debug builds only).
            Some("localhost") | Some("127.0.0.1") => {
                cfg!(debug_assertions) && url.port() == Some(1420)
            }
            _ => false,
        },
        _ => false,
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("nav-guard")
        .on_navigation(|webview, url| {
            if is_app_url(url) {
                return true;
            }
            if crate::commands::is_openable_external_url(url.as_str()) {
                let _ = webview
                    .app_handle()
                    .opener()
                    .open_url(url.as_str(), None::<&str>);
            }
            tracing::warn!(scheme = url.scheme(), "blocked in-app navigation");
            false
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn app_origins_are_allowed() {
        assert!(is_app_url(&u("tauri://localhost/index.html")));
        assert!(is_app_url(&u("http://tauri.localhost/")));
        assert!(is_app_url(&u("https://tauri.localhost/notes")));
        assert!(is_app_url(&u("about:blank")));
    }

    #[test]
    fn remote_and_odd_schemes_are_blocked() {
        for s in [
            "https://evil.example/phish",
            "http://tauri.localhost.evil.example/",
            "http://localhost:8080/",
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "ms-settings:privacy",
        ] {
            if let Ok(url) = Url::parse(s) {
                assert!(!is_app_url(&url), "{s} must be blocked");
            }
        }
    }

    #[test]
    fn only_web_and_mail_links_open_externally() {
        use crate::commands::is_openable_external_url as ok;
        assert!(ok("https://example.com/a?b=c"));
        assert!(ok("http://example.com"));
        assert!(ok("mailto:someone@example.com"));
        assert!(!ok("file:///etc/passwd"));
        assert!(!ok("javascript:alert(1)"));
        assert!(!ok("ms-settings:privacy"));
        assert!(!ok("\\\\server\\share\\x.exe"));
        assert!(!ok("not a url"));
    }
}
