//! looma-calendar: the `CalendarProvider` trait.
//!
//! Google Calendar and Microsoft Graph impls land in M5 (OAuth via the
//! system browser + loopback redirect; tokens in the OS keychain).

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
    /// Meeting link (Meet/Teams/Zoom URL) when present.
    pub join_url: Option<String>,
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
    /// Events in [from, to], sorted by start time.
    async fn upcoming(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<CalendarEvent>>;
}
