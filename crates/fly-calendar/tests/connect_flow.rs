//! Connection / disconnection / reconnection state-machine tests for both
//! providers, offline: everything here exercises the token lifecycle in the
//! secret store and the auth-error paths that surface the sidebar's
//! Reconnect button — no network calls (an expired token with no refresh
//! token fails before any HTTP request is made).

use std::sync::Arc;

use chrono::{Duration, Utc};
use fly_calendar::google::GoogleCalendarProvider;
use fly_calendar::msgraph::MsGraphProvider;
use fly_calendar::oauth::TokenSet;
use fly_calendar::{CalendarError, CalendarProvider};
use fly_secrets::{MemorySecretStore, SecretStore};

fn google(secrets: Arc<dyn SecretStore>) -> GoogleCalendarProvider {
    GoogleCalendarProvider {
        client_id: "cid".into(),
        client_secret: "csecret".into(),
        secrets,
        open_url: Arc::new(|_| {}),
        disabled_calendars: vec![],
    }
}

fn msgraph(secrets: Arc<dyn SecretStore>) -> MsGraphProvider {
    MsGraphProvider {
        client_id: "cid".into(),
        secrets,
        open_url: Arc::new(|_| {}),
        disabled_calendars: vec![],
    }
}

fn token(expired: bool, refresh: Option<&str>) -> String {
    serde_json::to_string(&TokenSet {
        access_token: "at".into(),
        refresh_token: refresh.map(str::to_string),
        expires_at: Utc::now() + Duration::hours(if expired { -1 } else { 1 }),
    })
    .unwrap()
}

#[tokio::test]
async fn google_connect_disconnect_reconnect_lifecycle() {
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
    let p = google(secrets.clone());

    // fresh install: not connected; fetching reports NotConnected (auth error)
    assert!(!p.is_connected().await);
    let e = p.upcoming(Utc::now(), Utc::now()).await.unwrap_err();
    assert!(matches!(e, CalendarError::NotConnected));

    // "connect": a token lands in the store (what connect() persists)
    secrets
        .set(
            fly_secrets::keys::GOOGLE_OAUTH_TOKEN,
            &token(false, Some("rt")),
        )
        .unwrap();
    assert!(p.is_connected().await);

    // disconnect: token gone, provider reports not connected again
    p.disconnect().await.unwrap();
    assert!(!p.is_connected().await);
    assert_eq!(
        secrets.get(fly_secrets::keys::GOOGLE_OAUTH_TOKEN).unwrap(),
        None
    );

    // disconnect is idempotent (double-click / already disconnected)
    p.disconnect().await.unwrap();

    // "reconnect": a fresh token restores the connection
    secrets
        .set(
            fly_secrets::keys::GOOGLE_OAUTH_TOKEN,
            &token(false, Some("rt2")),
        )
        .unwrap();
    assert!(p.is_connected().await);
}

#[tokio::test]
async fn google_expired_token_without_refresh_is_auth_error() {
    // The "timed out, needs reconnect" state: token expired and no refresh
    // token to renew with → NotConnected, which upcoming_meetings maps to
    // needs_reconnect (the sidebar's Reconnect row).
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
    secrets
        .set(fly_secrets::keys::GOOGLE_OAUTH_TOKEN, &token(true, None))
        .unwrap();
    let p = google(secrets);
    let e = p.upcoming(Utc::now(), Utc::now()).await.unwrap_err();
    assert!(matches!(e, CalendarError::NotConnected), "got: {e:?}");
    let e = p.list_calendars().await.unwrap_err();
    assert!(matches!(e, CalendarError::NotConnected));
}

#[tokio::test]
async fn google_unreadable_stored_token_is_auth_error() {
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
    secrets
        .set(fly_secrets::keys::GOOGLE_OAUTH_TOKEN, "not-json")
        .unwrap();
    let p = google(secrets);
    let e = p.upcoming(Utc::now(), Utc::now()).await.unwrap_err();
    assert!(matches!(e, CalendarError::Auth(_)), "got: {e:?}");
}

#[tokio::test]
async fn msgraph_connect_disconnect_reconnect_lifecycle() {
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
    let p = msgraph(secrets.clone());

    assert!(!p.is_connected().await);
    let e = p.upcoming(Utc::now(), Utc::now()).await.unwrap_err();
    assert!(matches!(e, CalendarError::NotConnected));

    secrets
        .set(fly_secrets::keys::MS_OAUTH_TOKEN, &token(false, Some("rt")))
        .unwrap();
    assert!(p.is_connected().await);

    p.disconnect().await.unwrap();
    assert!(!p.is_connected().await);
    assert_eq!(
        secrets.get(fly_secrets::keys::MS_OAUTH_TOKEN).unwrap(),
        None
    );
    p.disconnect().await.unwrap(); // idempotent

    secrets
        .set(
            fly_secrets::keys::MS_OAUTH_TOKEN,
            &token(false, Some("rt2")),
        )
        .unwrap();
    assert!(p.is_connected().await);
}

#[tokio::test]
async fn msgraph_expired_token_without_refresh_is_auth_error() {
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
    secrets
        .set(fly_secrets::keys::MS_OAUTH_TOKEN, &token(true, None))
        .unwrap();
    let p = msgraph(secrets);
    let e = p.upcoming(Utc::now(), Utc::now()).await.unwrap_err();
    assert!(matches!(e, CalendarError::NotConnected), "got: {e:?}");
}

#[test]
fn loopback_pages_are_branded_fly_on_the_wall() {
    // The browser page shown after the OAuth redirect must carry the app's
    // branding in both the success and failure variants.
    for page in [
        fly_calendar::oauth::CONNECTED_PAGE,
        fly_calendar::oauth::FAILED_PAGE,
    ] {
        assert!(page.contains("Fly on the Wall"), "page missing brand");
        assert!(page.contains("<!doctype html"));
    }
}
