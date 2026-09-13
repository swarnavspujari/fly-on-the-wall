//! Microsoft 365 / Outlook calendar via Microsoft Graph (public-client
//! PKCE — no client secret needed for desktop apps).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use fly_secrets::SecretStore;

use crate::google::OpenUrl;
use crate::links::detect_meeting_link;
use crate::oauth::{self, OAuthConfig, TokenSet};
use crate::{
    CalendarAttendee, CalendarError, CalendarEvent, CalendarInfo, CalendarProvider, Result,
};

pub struct MsGraphProvider {
    pub client_id: String,
    pub secrets: Arc<dyn SecretStore>,
    pub open_url: OpenUrl,
    /// Calendar ids the user toggled off; excluded from `upcoming()`. Empty
    /// means every calendar is merged.
    pub disabled_calendars: Vec<String>,
}

impl MsGraphProvider {
    fn oauth_config(&self) -> OAuthConfig {
        OAuthConfig {
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
            client_id: self.client_id.clone(),
            client_secret: None,
            scopes: "offline_access Calendars.Read".into(),
            extra_auth_params: vec![],
        }
    }

    async fn access_token(&self) -> Result<String> {
        let stored = self
            .secrets
            .get(fly_secrets::keys::MS_OAUTH_TOKEN)
            .map_err(|e| CalendarError::Auth(e.to_string()))?
            .ok_or(CalendarError::NotConnected)?;
        let tokens: TokenSet = serde_json::from_str(&stored)
            .map_err(|e| CalendarError::Auth(format!("stored token unreadable: {e}")))?;
        if !tokens.is_expired() {
            return Ok(tokens.access_token);
        }
        let refresh_token = tokens
            .refresh_token
            .as_deref()
            .ok_or(CalendarError::NotConnected)?;
        let renewed = oauth::refresh(&self.oauth_config(), refresh_token).await?;
        self.secrets
            .set(
                fly_secrets::keys::MS_OAUTH_TOKEN,
                &serde_json::to_string(&renewed).unwrap_or_default(),
            )
            .map_err(|e| CalendarError::Auth(e.to_string()))?;
        Ok(renewed.access_token)
    }

    /// Enumerate the user's Outlook calendars.
    async fn fetch_calendars(
        &self,
        client: &reqwest::Client,
        token: &str,
    ) -> Result<Vec<CalendarInfo>> {
        let url = "https://graph.microsoft.com/v1.0/me/calendars?$select=id,name,isDefaultCalendar&$top=100";
        let text = graph_get(client, url, token).await?;
        parse_graph_calendar_list(&text)
    }

    /// Upcoming events. `calendar_id: None` uses the default calendar view
    /// (the fallback when calendar enumeration is unavailable).
    async fn fetch_events(
        &self,
        client: &reqwest::Client,
        token: &str,
        calendar_id: Option<&str>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        self_addresses: &[String],
    ) -> Result<Vec<CalendarEvent>> {
        let base = match calendar_id {
            Some(id) => format!(
                "https://graph.microsoft.com/v1.0/me/calendars/{}/calendarView",
                urlencoding::encode(id)
            ),
            None => "https://graph.microsoft.com/v1.0/me/calendarView".to_string(),
        };
        let url = format!(
            "{base}?startDateTime={}&endDateTime={}&$orderby=start/dateTime&$top=25",
            urlencoding::encode(&from.to_rfc3339()),
            urlencoding::encode(&to.to_rfc3339()),
        );
        let text = graph_get(client, &url, token).await?;
        parse_graph_events(&text, self_addresses)
    }

    /// The signed-in account's addresses (`mail` + `userPrincipalName`,
    /// lowercased) so `is_self` can be set on Graph attendees — Graph has no
    /// per-attendee `self` flag. A failure yields an empty list (logged), so
    /// the user may appear as an attendee rather than the fetch failing.
    async fn fetch_self_addresses(&self, client: &reqwest::Client, token: &str) -> Vec<String> {
        let url = "https://graph.microsoft.com/v1.0/me?$select=mail,userPrincipalName";
        match graph_get(client, url, token).await {
            Ok(text) => parse_graph_self_addresses(&text),
            Err(e) => {
                tracing::warn!("graph /me lookup failed (attendee self-detection off): {e}");
                Vec::new()
            }
        }
    }
}

fn parse_graph_self_addresses(text: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    ["mail", "userPrincipalName"]
        .iter()
        .filter_map(|k| v.get(k).and_then(|s| s.as_str()))
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// One `attendees[]` (or the `organizer`) entry. Graph reports the reply in
/// `status.response`; `is_self` comes from comparing against `/me`.
fn parse_graph_attendee(
    a: &serde_json::Value,
    self_addresses: &[String],
) -> Option<CalendarAttendee> {
    let email = a.pointer("/emailAddress/address")?.as_str()?.trim();
    if email.is_empty() {
        return None;
    }
    Some(CalendarAttendee {
        email: email.to_string(),
        name: a
            .pointer("/emailAddress/name")
            .and_then(|n| n.as_str())
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string),
        is_self: self_addresses.iter().any(|s| s == &email.to_lowercase()),
        declined: a.pointer("/status/response").and_then(|s| s.as_str()) == Some("declined"),
    })
}

/// Authenticated Graph GET (UTC times requested via the `Prefer` header, which
/// `parse_graph_events` relies on). Maps 401 → NotConnected.
async fn graph_get(client: &reqwest::Client, url: &str, token: &str) -> Result<String> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .header("Prefer", "outlook.timezone=\"UTC\"")
        .send()
        .await
        .map_err(|e| CalendarError::Network(e.to_string()))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| CalendarError::Network(e.to_string()))?;
    if status.as_u16() == 401 {
        return Err(CalendarError::NotConnected);
    }
    if !status.is_success() {
        return Err(CalendarError::Provider(format!(
            "{status}: {}",
            text.chars().take(300).collect::<String>()
        )));
    }
    Ok(text)
}

#[async_trait::async_trait]
impl CalendarProvider for MsGraphProvider {
    fn id(&self) -> &'static str {
        "msgraph"
    }

    fn display_name(&self) -> &'static str {
        "Microsoft 365 / Outlook"
    }

    async fn is_connected(&self) -> bool {
        self.secrets
            .get(fly_secrets::keys::MS_OAUTH_TOKEN)
            .ok()
            .flatten()
            .is_some()
    }

    async fn connect(&self) -> Result<()> {
        let tokens = oauth::interactive_auth(&self.oauth_config(), self.open_url.as_ref()).await?;
        self.secrets
            .set(
                fly_secrets::keys::MS_OAUTH_TOKEN,
                &serde_json::to_string(&tokens).unwrap_or_default(),
            )
            .map_err(|e| CalendarError::Auth(e.to_string()))
    }

    async fn disconnect(&self) -> Result<()> {
        self.secrets
            .delete(fly_secrets::keys::MS_OAUTH_TOKEN)
            .map_err(|e| CalendarError::Auth(e.to_string()))
    }

    async fn list_calendars(&self) -> Result<Vec<CalendarInfo>> {
        let token = self.access_token().await?;
        let client = reqwest::Client::new();
        self.fetch_calendars(&client, &token).await
    }

    async fn upcoming(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<CalendarEvent>> {
        let token = self.access_token().await?;
        let client = reqwest::Client::new();

        let mut events = Vec::new();
        let me = self.fetch_self_addresses(&client, &token).await;
        match self.fetch_calendars(&client, &token).await {
            Ok(list) => {
                // Fan out across enabled calendars; one failing calendar must
                // not sink the rest.
                for cal in list {
                    if self.disabled_calendars.iter().any(|d| d == &cal.id) {
                        continue;
                    }
                    match self
                        .fetch_events(&client, &token, Some(&cal.id), from, to, &me)
                        .await
                    {
                        Ok(mut ev) => events.append(&mut ev),
                        Err(e) => {
                            tracing::warn!("graph events fetch for {} failed: {e}", cal.id)
                        }
                    }
                }
            }
            Err(e) => {
                // Enumeration failed — fall back to the default calendar view.
                tracing::warn!("graph calendars list failed, using default view: {e}");
                let mut ev = self
                    .fetch_events(&client, &token, None, from, to, &me)
                    .await?;
                events.append(&mut ev);
            }
        }
        events.sort_by_key(|e| e.start);
        Ok(events)
    }
}

/// Parse a Graph `me/calendars` response into `CalendarInfo`s.
pub fn parse_graph_calendar_list(json: &str) -> Result<Vec<CalendarInfo>> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CalendarError::Provider(format!("bad calendars JSON: {e}")))?;
    let mut out = Vec::new();
    for item in v.get("value").and_then(|i| i.as_array()).unwrap_or(&vec![]) {
        let Some(id) = item.get("id").and_then(|i| i.as_str()) else {
            continue;
        };
        let name = item.get("name").and_then(|s| s.as_str()).unwrap_or(id);
        let primary = item
            .get("isDefaultCalendar")
            .and_then(|p| p.as_bool())
            .unwrap_or(false);
        out.push(CalendarInfo {
            id: id.to_string(),
            name: name.to_string(),
            primary,
        });
    }
    Ok(out)
}

pub fn parse_graph_events(json: &str, self_addresses: &[String]) -> Result<Vec<CalendarEvent>> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CalendarError::Provider(format!("bad events JSON: {e}")))?;
    let mut events = Vec::new();
    for item in v.get("value").and_then(|i| i.as_array()).unwrap_or(&vec![]) {
        let parse_time = |field: &str| -> Option<DateTime<Utc>> {
            // Graph returns "2026-07-01T15:00:00.0000000" in the requested TZ (UTC)
            let dt = item.pointer(&format!("/{field}/dateTime"))?.as_str()?;
            let trimmed = dt.split('.').next().unwrap_or(dt);
            DateTime::parse_from_rfc3339(&format!("{trimmed}Z"))
                .ok()
                .map(|d| d.with_timezone(&Utc))
        };
        let (Some(start), Some(end)) = (parse_time("start"), parse_time("end")) else {
            continue;
        };
        // Graph lists the organizer separately from `attendees`; fold them in
        // (de-duped by address) so an externally organized meeting still names
        // its host.
        let mut attendees: Vec<CalendarAttendee> = item
            .get("attendees")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| parse_graph_attendee(a, self_addresses))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(org) = item
            .get("organizer")
            .and_then(|o| parse_graph_attendee(o, self_addresses))
        {
            if !attendees
                .iter()
                .any(|a| a.email.eq_ignore_ascii_case(&org.email))
            {
                attendees.insert(0, org);
            }
        }
        let join_url = item
            .pointer("/onlineMeeting/joinUrl")
            .and_then(|u| u.as_str())
            .map(str::to_string)
            .or_else(|| {
                // Legacy/Skype field, present on some events.
                item.get("onlineMeetingUrl")
                    .and_then(|u| u.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            // Fallback: scan the location + body for a Zoom / Teams / Meet /
            // GoToWebinar link the provider didn't surface natively.
            .or_else(|| {
                let location = item
                    .pointer("/location/displayName")
                    .and_then(|l| l.as_str())
                    .unwrap_or("");
                let body = item
                    .pointer("/body/content")
                    .and_then(|b| b.as_str())
                    .unwrap_or("");
                detect_meeting_link(&[location, body])
            });
        events.push(CalendarEvent {
            id: item
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or_default()
                .to_string(),
            provider: "msgraph".into(),
            title: item
                .get("subject")
                .and_then(|s| s.as_str())
                .unwrap_or("(untitled)")
                .to_string(),
            start,
            end,
            attendees,
            join_url,
        });
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attendees_carry_name_self_declined_and_organizer() {
        let json = r#"{"value": [{
            "id": "AAA=",
            "subject": "Standup",
            "start": {"dateTime": "2026-07-01T09:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-07-01T09:15:00.0000000", "timeZone": "UTC"},
            "organizer": {"emailAddress": {"address": "host@x.com", "name": "Host Person"}},
            "attendees": [
                {"emailAddress": {"address": "Me@X.com", "name": "Me"}, "status": {"response": "accepted"}},
                {"emailAddress": {"address": "dana@x.com", "name": "Dana Osei"}, "status": {"response": "declined"}},
                {"emailAddress": {"address": "host@x.com", "name": "Host Person"}, "status": {"response": "organizer"}}
            ]
        }]}"#;
        let events = parse_graph_events(json, &["me@x.com".to_string()]).unwrap();
        let a = &events[0].attendees;
        // organizer already listed → not duplicated
        assert_eq!(a.len(), 3, "{a:?}");
        assert!(a[0].is_self);
        assert_eq!(a[1].name.as_deref(), Some("Dana Osei"));
        assert!(a[1].declined);
        assert_eq!(a[2].email, "host@x.com");

        let json = r#"{"value": [{
            "id": "BBB=",
            "subject": "External",
            "start": {"dateTime": "2026-07-01T09:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-07-01T09:15:00.0000000", "timeZone": "UTC"},
            "organizer": {"emailAddress": {"address": "host@y.com", "name": "Host"}},
            "attendees": [{"emailAddress": {"address": "me@x.com"}}]
        }]}"#;
        let events = parse_graph_events(json, &[]).unwrap();
        let a = &events[0].attendees;
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].email, "host@y.com"); // organizer folded in first
        assert!(!a[1].is_self); // no /me data → nobody is self
        assert_eq!(
            parse_graph_self_addresses(r#"{"mail":"A@x.com","userPrincipalName":"a@x.com"}"#),
            vec!["a@x.com", "a@x.com"]
        );
    }

    #[test]
    fn parses_graph_calendar_view() {
        let json = r#"{"value": [{
            "id": "AAA=",
            "subject": "Standup",
            "start": {"dateTime": "2026-07-01T09:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-07-01T09:15:00.0000000", "timeZone": "UTC"},
            "attendees": [{"emailAddress": {"address": "team@x.com"}}],
            "onlineMeeting": {"joinUrl": "https://teams.microsoft.com/l/xyz"}
        }]}"#;
        let events = parse_graph_events(json, &[]).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Standup");
        assert_eq!(events[0].attendees.len(), 1);
        assert_eq!(events[0].attendees[0].email, "team@x.com");
        assert!(events[0].join_url.as_deref().unwrap().contains("teams"));
        assert_eq!(events[0].start.to_rfc3339(), "2026-07-01T09:00:00+00:00");
    }

    #[test]
    fn falls_back_to_zoom_link_in_body_or_location() {
        let json = r#"{"value": [{
            "id": "b1",
            "subject": "External sync",
            "start": {"dateTime": "2026-07-01T09:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-07-01T09:30:00.0000000", "timeZone": "UTC"},
            "location": {"displayName": "Zoom"},
            "body": {"contentType": "html", "content": "<div>Join <a href=\"https://acme.zoom.us/j/99988877766?pwd=Tk5aQ2Rs\">Zoom Meeting</a></div>"}
        }, {
            "id": "b2",
            "subject": "Desk booking",
            "start": {"dateTime": "2026-07-01T10:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-07-01T10:30:00.0000000", "timeZone": "UTC"},
            "location": {"displayName": "Room 2"}
        }]}"#;
        let events = parse_graph_events(json, &[]).unwrap();
        assert_eq!(
            events[0].join_url.as_deref(),
            Some("https://acme.zoom.us/j/99988877766?pwd=Tk5aQ2Rs")
        );
        assert_eq!(events[1].join_url, None);
    }

    #[test]
    fn parses_calendar_list() {
        let json = r#"{"value": [
            {"id": "AAA_default", "name": "Calendar", "isDefaultCalendar": true},
            {"id": "BBB_team", "name": "Team Events"}
        ]}"#;
        let cals = parse_graph_calendar_list(json).unwrap();
        assert_eq!(cals.len(), 2);
        assert!(cals[0].primary);
        assert_eq!(cals[0].name, "Calendar");
        assert!(!cals[1].primary);
        assert_eq!(cals[1].id, "BBB_team");
    }
}
