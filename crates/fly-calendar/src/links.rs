//! Meeting-link detection.
//!
//! Provider-native fields (Google Meet's `hangoutLink`/`conferenceData`,
//! Teams' `onlineMeeting.joinUrl`) cover the easy case, but Zoom and
//! GoToWebinar links usually live in the event's *location* or *body*. This
//! module scans arbitrary event text for join URLs across the four platforms
//! we support: Zoom, Microsoft Teams, Google Meet, and GoToWebinar (plus its
//! GoToMeeting sibling, which shares the same URL family).
//!
//! Everything here is a pure function with no I/O, so it is unit-tested
//! directly against real-world URL shapes (see the tests at the bottom).

/// Characters that end a URL when scanning free-form text. Event bodies are
/// often HTML, where links sit inside `href="…"` or `<https://…>`, so quotes
/// and angle brackets are boundaries too.
fn is_url_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '\\' | '|')
}

/// Strip trailing characters a URL never really ends with — sentence
/// punctuation and the closing half of `(https://…)` / `[https://…]`.
fn trim_url_end(url: &str) -> &str {
    url.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '"', '\''])
}

/// The meeting platform a single URL belongs to, or `None` if it is not a
/// join link we recognize. Host is matched against an allow-list and the path
/// must look like an actual meeting/join path, so ordinary marketing links
/// (`zoom.us/pricing`, `teams.microsoft.com/downloads`) are rejected.
pub fn meeting_platform(url: &str) -> Option<&'static str> {
    let lower = url.to_ascii_lowercase();
    // Case-insensitive scheme match, but slice the ORIGINAL url so path/query
    // tokens keep their case (join tokens are case-sensitive).
    let rest = if lower.starts_with("https://") {
        &url[8..]
    } else if lower.starts_with("http://") {
        &url[7..]
    } else {
        return None;
    };
    // host = up to the first '/', '?' or '#'; drop an optional ":port".
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host_raw = &rest[..host_end];
    let host = host_raw
        .split(':')
        .next()
        .unwrap_or(host_raw)
        .to_ascii_lowercase();
    let path = &rest[host_end..]; // starts with '/', '?', '#', or is empty
    let path_lower = path.to_ascii_lowercase();

    // Matches `host == suffix` or `*.suffix` (a real subdomain), never
    // `notzoom.us` for suffix `zoom.us`.
    let host_is = |suffix: &str| host == suffix || host.ends_with(&format!(".{suffix}"));

    // Zoom: zoom.us / *.zoom.us / zoomgov.com — only genuine join paths.
    if (host_is("zoom.us") || host_is("zoomgov.com"))
        && ZOOM_JOIN_PATHS.iter().any(|p| path_lower.starts_with(p))
    {
        return Some("zoom");
    }

    // Microsoft Teams: work/school (teams.microsoft.com, GCC .us) or personal
    // (teams.live.com). Join links carry a meetup-join / meet path.
    if (host_is("teams.microsoft.com") || host_is("teams.microsoft.us") || host_is("teams.live.com"))
        && (path_lower.contains("meetup-join")
            || path_lower.contains("/meet/")
            || path_lower.contains("/l/meeting"))
    {
        return Some("teams");
    }

    // Google Meet: meet.google.com/<code>. The native hangoutLink covers most
    // Google events; this catches Meet links pasted into other providers.
    if host_is("meet.google.com") {
        let code = path.trim_start_matches('/');
        let code = code.split(['?', '#']).next().unwrap_or("");
        if !code.is_empty() {
            return Some("meet");
        }
    }

    // GoToWebinar / GoToMeeting / gotomeet.me personal links.
    if host_is("gotowebinar.com") || host_is("gotomeeting.com") || host_is("gotomeet.me") {
        let has_path = !path.trim_matches(['/', '?', '#']).is_empty();
        if path_lower.contains("/join/")
            || path_lower.contains("/register/")
            || (host_is("gotomeet.me") && has_path)
        {
            return Some("gotowebinar");
        }
    }

    None
}

/// Zoom URL paths that indicate an actual meeting/webinar join (not a
/// marketing or download page).
const ZOOM_JOIN_PATHS: [&str; 6] = ["/j/", "/w/", "/s/", "/my/", "/wc/", "/meeting/"];

/// Scan the given text fields (e.g. location, description/body) in order and
/// return the first recognized meeting join URL, if any. Fields are searched
/// left-to-right, and within a field, URLs are scanned in appearance order —
/// so the result is deterministic.
pub fn detect_meeting_link(texts: &[&str]) -> Option<String> {
    texts.iter().find_map(|t| first_meeting_url(t))
}

/// First URL in `text` whose host+path classify as a known meeting link.
fn first_meeting_url(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(i) = lower[from..].find("http") {
        let start = from + i;
        let rest = &lower[start..];
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let tail = &text[start..];
            let end = tail.find(is_url_boundary).unwrap_or(tail.len());
            let candidate = trim_url_end(&tail[..end]);
            if meeting_platform(candidate).is_some() {
                return Some(candidate.to_string());
            }
            from = start + end.max(1);
        } else {
            from = start + 4; // advance past this "http" that wasn't a URL
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Zoom ------------------------------------------------------------

    #[test]
    fn zoom_join_urls_across_shapes() {
        // regional subdomain + password query
        assert_eq!(
            meeting_platform("https://us02web.zoom.us/j/89012345678?pwd=Qm5xZ1k9dz09"),
            Some("zoom")
        );
        // bare host
        assert_eq!(meeting_platform("https://zoom.us/j/123456789"), Some("zoom"));
        // company vanity subdomain
        assert_eq!(meeting_platform("https://acme.zoom.us/j/98765"), Some("zoom"));
        // personal room
        assert_eq!(meeting_platform("https://zoom.us/my/janedoe"), Some("zoom"));
        // webinar path
        assert_eq!(meeting_platform("https://zoom.us/w/1234567890"), Some("zoom"));
        // web-client join
        assert_eq!(
            meeting_platform("https://zoom.us/wc/join/1234567890"),
            Some("zoom")
        );
        // government
        assert_eq!(
            meeting_platform("https://agency.zoomgov.com/j/1616161616"),
            Some("zoom")
        );
    }

    #[test]
    fn zoom_marketing_links_are_not_meetings() {
        assert_eq!(meeting_platform("https://zoom.us/pricing"), None);
        assert_eq!(meeting_platform("https://zoom.us/download/client"), None);
        assert_eq!(meeting_platform("https://zoom.us/"), None);
        // look-alike host must not match
        assert_eq!(meeting_platform("https://notzoom.us/j/123"), None);
        assert_eq!(meeting_platform("https://evilzoom.us.attacker.com/j/1"), None);
    }

    // ---- Microsoft Teams -------------------------------------------------

    #[test]
    fn teams_join_urls() {
        let url = "https://teams.microsoft.com/l/meetup-join/19%3ameeting_ODUwZjM%40thread.v2/0?context=%7b%22Tid%22%3a%22abc%22%2c%22Oid%22%3a%22def%22%7d";
        assert_eq!(meeting_platform(url), Some("teams"));
        // GCC-High / government cloud host
        assert_eq!(
            meeting_platform("https://teams.microsoft.us/l/meetup-join/19%3ameeting_x%40thread.v2/0"),
            Some("teams")
        );
        // personal Teams
        assert_eq!(
            meeting_platform("https://teams.live.com/meet/9876543210?p=abc"),
            Some("teams")
        );
    }

    #[test]
    fn teams_non_meeting_links_rejected() {
        assert_eq!(meeting_platform("https://teams.microsoft.com/downloads"), None);
        assert_eq!(meeting_platform("https://www.microsoft.com/teams"), None);
    }

    // ---- Google Meet -----------------------------------------------------

    #[test]
    fn google_meet_urls() {
        assert_eq!(
            meeting_platform("https://meet.google.com/abc-defg-hij"),
            Some("meet")
        );
        assert_eq!(
            meeting_platform("https://meet.google.com/abc-defg-hij?authuser=0"),
            Some("meet")
        );
        // landing page with no code is not a meeting
        assert_eq!(meeting_platform("https://meet.google.com/"), None);
        assert_eq!(meeting_platform("https://meet.google.com"), None);
    }

    // ---- GoToWebinar / GoToMeeting --------------------------------------

    #[test]
    fn goto_urls() {
        assert_eq!(
            meeting_platform("https://global.gotomeeting.com/join/123456789"),
            Some("gotowebinar")
        );
        assert_eq!(
            meeting_platform("https://www.gotomeeting.com/join/987654321"),
            Some("gotowebinar")
        );
        assert_eq!(
            meeting_platform("https://attendee.gotowebinar.com/register/1234567890123456789"),
            Some("gotowebinar")
        );
        assert_eq!(
            meeting_platform("https://global.gotowebinar.com/join/8888888888"),
            Some("gotowebinar")
        );
        assert_eq!(meeting_platform("https://gotomeet.me/janedoe"), Some("gotowebinar"));
    }

    #[test]
    fn goto_marketing_rejected() {
        assert_eq!(meeting_platform("https://www.gotomeeting.com/features"), None);
        assert_eq!(meeting_platform("https://www.gotomeeting.com/"), None);
    }

    // ---- Non-meeting / junk ---------------------------------------------

    #[test]
    fn unrelated_urls_and_non_urls() {
        assert_eq!(meeting_platform("https://example.com/j/123"), None);
        assert_eq!(meeting_platform("https://calendar.google.com/event?id=1"), None);
        assert_eq!(meeting_platform("mailto:someone@example.com"), None);
        assert_eq!(meeting_platform("not a url at all"), None);
        assert_eq!(meeting_platform("ftp://zoom.us/j/1"), None);
    }

    // ---- Scanning free-form text ----------------------------------------

    #[test]
    fn detects_link_in_location_string() {
        // Google `location` often carries the raw Zoom URL.
        let loc = "https://us06web.zoom.us/j/85512345678?pwd=RHZ4bGtE";
        assert_eq!(
            detect_meeting_link(&[loc]).as_deref(),
            Some("https://us06web.zoom.us/j/85512345678?pwd=RHZ4bGtE")
        );
    }

    #[test]
    fn detects_link_inside_html_body() {
        let body = r#"<div>You are invited.<br>
            <a href="https://acme.zoom.us/j/99988877766?pwd=Tk5aQ2Rs">Join Zoom Meeting</a><br>
            Meeting ID: 999 888 777 66</div>"#;
        assert_eq!(
            detect_meeting_link(&[body]).as_deref(),
            Some("https://acme.zoom.us/j/99988877766?pwd=Tk5aQ2Rs")
        );
    }

    #[test]
    fn skips_non_meeting_urls_and_returns_the_meeting_one() {
        let text =
            "Agenda: https://acme.com/agenda-2026 \nJoin here: https://zoom.us/j/55554444 thanks";
        assert_eq!(
            detect_meeting_link(&[text]).as_deref(),
            Some("https://zoom.us/j/55554444")
        );
    }

    #[test]
    fn strips_trailing_punctuation_and_parens() {
        let text = "Dial in (https://global.gotomeeting.com/join/123456789).";
        assert_eq!(
            detect_meeting_link(&[text]).as_deref(),
            Some("https://global.gotomeeting.com/join/123456789")
        );
    }

    #[test]
    fn searches_fields_in_order() {
        // First field has no meeting link; second does.
        let location = "Conference Room B";
        let description = "Notes...\nTeams: https://teams.microsoft.com/l/meetup-join/19%3ameeting_a%40thread.v2/0";
        let got = detect_meeting_link(&[location, description]);
        assert_eq!(
            got.as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/19%3ameeting_a%40thread.v2/0")
        );
    }

    #[test]
    fn returns_none_when_no_meeting_link_present() {
        assert_eq!(detect_meeting_link(&["Lunch", "Bring your laptop"]), None);
        assert_eq!(detect_meeting_link(&[]), None);
    }

    #[test]
    fn preserves_case_sensitive_tokens() {
        // Join tokens are case-sensitive; the returned URL must not be lowercased.
        let text = "join https://zoom.us/j/123?pwd=AbCdEfGhIj";
        assert_eq!(
            detect_meeting_link(&[text]).as_deref(),
            Some("https://zoom.us/j/123?pwd=AbCdEfGhIj")
        );
    }
}
