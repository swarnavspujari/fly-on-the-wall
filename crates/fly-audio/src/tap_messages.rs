//! User-facing text for the macOS system-audio tap's permission hazards.
//!
//! Lives outside the `cfg(target_os = "macos")` module so the wording — which
//! the UI pattern-matches on and support docs quote — is unit-tested on every
//! platform. The stale-TCC-grant remediation matters: TCC keys grants to the
//! app's code-signing requirement, so a Mac that ever ran a differently-signed
//! build (self-signed dev build, pre-Developer-ID release) shows the toggle ON
//! while silently denying the current build.

/// The bundle identifier TCC keys the grant to (tauri.conf.json).
pub const BUNDLE_ID: &str = "com.flyonthewall.app";

/// Where the grant lives in System Settings. The UI shows an "Open Settings"
/// action next to any warning containing this phrase.
pub const PRIVACY_PANE: &str = "Screen & System Audio Recording";

/// The Terminal escape hatch for a stale grant.
pub fn tccutil_reset_command() -> String {
    format!("tccutil reset ScreenCapture {BUNDLE_ID}")
}

/// The in-meeting banner shown when the tap delivers only zeros while the
/// output device is rendering. Must be actionable for the stale-grant case,
/// where the System Settings toggle ALREADY shows the app as allowed.
pub fn silence_warning_text() -> String {
    format!(
        "System audio is playing but its capture is recording only silence — macOS is \
         blocking system-audio capture for this build, so the other participants may be \
         missing from this recording. Open System Settings → Privacy & Security → \
         {PRIVACY_PANE} and allow Fly on the Wall. If it already shows as allowed, the \
         grant belongs to a previously installed build: remove Fly on the Wall from that \
         list with the − button and re-add it (or run `{}` in Terminal), then relaunch.",
        tccutil_reset_command()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tccutil_command_targets_screencapture_service_and_bundle() {
        assert_eq!(
            tccutil_reset_command(),
            "tccutil reset ScreenCapture com.flyonthewall.app"
        );
    }

    #[test]
    fn silence_warning_covers_the_stale_grant_dead_end() {
        let text = silence_warning_text();
        // Names the Settings pane (the UI keys its "Open Settings" action on this).
        assert!(text.contains(PRIVACY_PANE), "must name the privacy pane");
        // Tells the user the toggle may already look enabled…
        assert!(
            text.contains("already"),
            "must acknowledge the toggle can already show as allowed"
        );
        // …and gives both remediations: remove/re-add, and tccutil reset.
        assert!(text.contains("remove"), "must say to remove the entry");
        assert!(
            text.contains(&tccutil_reset_command()),
            "must include the exact tccutil command"
        );
        // Still says what the consequence is.
        assert!(text.contains("other participants"));
    }
}
