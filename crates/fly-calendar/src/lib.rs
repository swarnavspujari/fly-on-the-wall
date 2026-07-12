//! fly-calendar: the `CalendarProvider` trait.
//!
//! Google Calendar and Microsoft Graph impls land in M5 (OAuth via the
//! system browser + loopback redirect; tokens in the OS keychain).

pub mod google;
pub mod links;
pub mod msgraph;
pub mod oauth;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum CalendarError {
    #[error("calendar is not connected")]
    NotConnected,
    #[error("OAuth flow failed: {0}")]
    Auth(String),
    #[error("provider returned an error: {0}")]
    Provider(String),
    #[error("network error: {0}")]
    Network(String),
}

pub type Result<T> = std::result::Result<T, CalendarError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    /// Which provider this came from ("google", "msgraph").
    pub provider: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub attendees: Vec<String>,
    /// Meeting link (Meet/Teams/Zoom/GoToWebinar URL) when present.
    pub join_url: Option<String>,
}

/// One of a user's calendars, as listed by a provider. Provider-agnostic; the
/// caller knows which provider it queried. The on/off toggle lives in app
/// settings, not here — this crate only reports what exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarInfo {
    pub id: String,
    pub name: String,
    /// The provider's primary/default calendar.
    pub primary: bool,
}

/// Combine events from every provider and calendar into the final "Up next"
/// list: drop events with no join link, de-dupe by (provider, id) in case the
/// same event surfaces through overlapping calendars, then sort by start time.
pub fn merge_upcoming(mut events: Vec<CalendarEvent>) -> Vec<CalendarEvent> {
    events.retain(|e| e.join_url.as_deref().is_some_and(|u| !u.is_empty()));
    let mut seen = std::collections::HashSet::new();
    events.retain(|e| seen.insert((e.provider.clone(), e.id.clone())));
    events.sort_by_key(|e| e.start);
    events
}

#[async_trait::async_trait]
pub trait CalendarProvider: Send + Sync {
    /// Stable id: "google", "msgraph".
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    async fn is_connected(&self) -> bool;
    /// Run the interactive OAuth flow (opens the system browser); stores
    /// tokens in the keychain on success.
    async fn connect(&self) -> Result<()>;
    async fn disconnect(&self) -> Result<()>;
    /// The user's calendars (all of them — the primary plus any secondary,
    /// shared, or subscribed calendars). Used to populate the settings
    /// toggles; the caller decides which are enabled.
    async fn list_calendars(&self) -> Result<Vec<CalendarInfo>>;
    /// Events in [from, to] across the user's enabled calendars, sorted by
    /// start time.
    async fn upcoming(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<CalendarEvent>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(provider: &str, id: &str, start: &str, join: Option<&str>) -> CalendarEvent {
        CalendarEvent {
            id: id.into(),
            provider: provider.into(),
            title: id.into(),
            start: DateTime::parse_from_rfc3339(start)
                .unwrap()
                .with_timezone(&Utc),
            end: DateTime::parse_from_rfc3339(start)
                .unwrap()
                .with_timezone(&Utc),
            attendees: vec![],
            join_url: join.map(str::to_string),
        }
    }

    #[test]
    fn merge_drops_linkless_events_and_sorts_by_start() {
        let merged = merge_upcoming(vec![
            ev(
                "google",
                "b",
                "2026-07-01T10:00:00Z",
                Some("https://zoom.us/j/2"),
            ),
            ev("google", "a", "2026-07-01T09:00:00Z", None), // no link → dropped
            ev(
                "msgraph",
                "c",
                "2026-07-01T08:00:00Z",
                Some("https://teams.microsoft.com/l/meetup-join/x"),
            ),
            ev("google", "d", "2026-07-01T11:00:00Z", Some("")), // empty link → dropped
        ]);
        let ids: Vec<_> = merged.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "b"]); // sorted by start, linkless gone
    }

    #[test]
    fn merge_dedupes_same_provider_and_id() {
        let merged = merge_upcoming(vec![
            ev(
                "google",
                "x",
                "2026-07-01T09:00:00Z",
                Some("https://zoom.us/j/1"),
            ),
            ev(
                "google",
                "x",
                "2026-07-01T09:00:00Z",
                Some("https://zoom.us/j/1"),
            ),
            // same id, different provider is a genuinely different event
            ev(
                "msgraph",
                "x",
                "2026-07-01T09:30:00Z",
                Some("https://zoom.us/j/1"),
            ),
        ]);
        assert_eq!(merged.len(), 2);
    }
}
