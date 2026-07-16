//! Enumerate capturable top-level windows and resolve a requested title
//! against them.
//!
//! gdigrab matches a window by its EXACT current title, but exact titles are
//! not something users can reliably know or that stay put: VM/RDP clients
//! decorate them ("IDB VDI IT" is really "IDB VDI IT - VMware Horizon
//! Client"), apps append document names, and titles drift between picking
//! and recording. So the UI picks from [`list_windows`], and the recorder
//! re-resolves through [`resolve_window_title`] at start so a partial or
//! slightly stale title still lands on the right window instead of dying
//! inside ffmpeg with an I/O error.

/// Titles of top-level, visible, titled, non-tool, non-cloaked windows,
/// front-to-back (z-order). Empty on platforms without enumeration —
/// window capture is Windows-only today (see `grab_args`).
#[cfg(windows)]
pub fn list_windows() -> Vec<String> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongW, GetWindowTextW, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
    };

    /// Keep enumerating (callback return value).
    const CONTINUE: BOOL = BOOL(1);

    unsafe extern "system" fn on_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            let titles = &mut *(lparam.0 as *mut Vec<String>);
            if !IsWindowVisible(hwnd).as_bool() {
                return CONTINUE;
            }
            // Tool windows (floating palettes, hidden helpers) aren't
            // meeting-capture targets and clutter the picker.
            let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
                return CONTINUE;
            }
            // Cloaked windows (suspended UWP apps, other virtual desktops)
            // are "visible" to EnumWindows but paint nothing — gdigrab
            // would record black frames.
            let mut cloaked: u32 = 0;
            let _ = DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut _ as *mut _,
                std::mem::size_of::<u32>() as u32,
            );
            if cloaked != 0 {
                return CONTINUE;
            }
            let mut buf = [0u16; 512];
            let len = GetWindowTextW(hwnd, &mut buf);
            if len > 0 {
                titles.push(String::from_utf16_lossy(&buf[..len as usize]));
            }
            CONTINUE
        }
    }

    let mut titles: Vec<String> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(on_window),
            LPARAM(&mut titles as *mut Vec<String> as isize),
        );
    }
    titles
}

#[cfg(not(windows))]
pub fn list_windows() -> Vec<String> {
    Vec::new()
}

/// Resolve a requested title against the windows open RIGHT NOW; returns the
/// exact current title to hand to gdigrab, or `None` when nothing (or more
/// than one thing, ambiguously) matches.
#[cfg(windows)]
pub fn resolve_window_title(requested: &str) -> Option<String> {
    pick_title(requested, &list_windows())
}

/// The matching rule, pure for tests: exact title → case-insensitive title →
/// UNIQUE case-insensitive substring (so "IDB VDI IT" finds "IDB VDI IT -
/// VMware Horizon Client", but an ambiguous fragment matches nothing rather
/// than a random window).
pub fn pick_title(requested: &str, titles: &[String]) -> Option<String> {
    let req = requested.trim();
    if req.is_empty() {
        return None;
    }
    if let Some(t) = titles.iter().find(|t| t.as_str() == req) {
        return Some(t.clone());
    }
    let req_lower = req.to_lowercase();
    if let Some(t) = titles.iter().find(|t| t.to_lowercase() == req_lower) {
        return Some(t.clone());
    }
    let mut hits = titles
        .iter()
        .filter(|t| t.to_lowercase().contains(&req_lower));
    match (hits.next(), hits.next()) {
        (Some(t), None) => Some(t.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles() -> Vec<String> {
        vec![
            "Untitled - Notepad".into(),
            "IDB VDI IT - VMware Horizon Client".into(),
            "Budget – Zoom".into(),
            "budget notes.xlsx - Excel".into(),
        ]
    }

    /// The 2026-07-16 field failure: the user typed the taskbar name of a VM
    /// window; the real title carries the client's decoration. A unique
    /// substring must resolve to the exact current title.
    #[test]
    fn partial_vm_title_resolves_to_decorated_window() {
        assert_eq!(
            pick_title("IDB VDI IT", &titles()).as_deref(),
            Some("IDB VDI IT - VMware Horizon Client")
        );
        // case-insensitive too — titles are typed by hand
        assert_eq!(
            pick_title("idb vdi it", &titles()).as_deref(),
            Some("IDB VDI IT - VMware Horizon Client")
        );
    }

    #[test]
    fn exact_match_wins_over_substring() {
        let ts = vec!["Zoom".to_string(), "Budget – Zoom".to_string()];
        assert_eq!(pick_title("Zoom", &ts).as_deref(), Some("Zoom"));
    }

    #[test]
    fn ambiguous_fragment_matches_nothing() {
        // "budget" hits both the Zoom call and the spreadsheet — guessing
        // would record the wrong window; refuse instead.
        assert_eq!(pick_title("budget", &titles()), None);
    }

    #[test]
    fn unknown_and_empty_requests_match_nothing() {
        assert_eq!(pick_title("Slack", &titles()), None);
        assert_eq!(pick_title("   ", &titles()), None);
        assert_eq!(pick_title("x", &[]), None);
    }

    /// Enumeration smoke: must not crash; every returned title is non-empty.
    /// (Headless CI sessions may legitimately return few or no windows.)
    #[test]
    fn list_windows_returns_clean_titles() {
        let ws = list_windows();
        assert!(ws.iter().all(|t| !t.trim().is_empty()));
    }
}
