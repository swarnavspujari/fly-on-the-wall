//! OAuth sign-in for remote connectors (OAuth 2.1 + PKCE, via rmcp).
//!
//! * Every request (discovery, registration, token, refresh) goes through
//!   [`GuardedOAuthClient`]: https only, private addresses blocked, redirects
//!   checked hop by hop.
//! * Discovered endpoints are checked before anything is sent to them, and
//!   the authorization URL is checked before the browser opens.
//! * The user signs in in their own browser; FOTW never sees a password. The
//!   browser returns to a one-shot listener on `127.0.0.1` (RFC 8252), and
//!   rmcp checks the `state`, the PKCE verifier and the reply's issuer
//!   (`iss`, RFC 9207) on the code exchange.
//! * The authorization server found at sign-in is pinned with the connector;
//!   token refreshes use it instead of discovering again.
//! * Tokens land in the keychain through [`KeychainCredentials`].

use std::sync::Arc;
use std::time::Duration;

use fly_secrets::SecretStore;
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationMetadata, OAuthClientConfig,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::{AuthConfig, TransportConfig};
use crate::netguard::{GuardedOAuthClient, NetError, NetPolicy};
use crate::registry::ServerEntry;
use crate::secrets::KeychainCredentials;

pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CLIENT_NAME: &str = "Fly on the Wall";
const MAX_CALLBACK_REQUEST: usize = 8 * 1024;
/// How long one connection to the sign-in listener may take to send its
/// request (a stalled connection can't hold the listener).
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("this connector doesn't use sign-in")]
    NotOAuth,
    #[error("sign in to this connector first")]
    SignInRequired,
    #[error("{0}")]
    Blocked(#[from] NetError),
    #[error("the server's sign-in setup uses a blocked address ({endpoint}): {reason}")]
    BlockedEndpoint {
        endpoint: &'static str,
        reason: NetError,
    },
    #[error("sign-in was cancelled or refused in the browser ({0})")]
    Denied(String),
    #[error("sign-in timed out")]
    TimedOut,
    #[error("couldn't open the browser: {0}")]
    Browser(String),
    #[error("sign-in failed: {0}")]
    Auth(String),
    #[error("couldn't listen for the sign-in reply: {0}")]
    Listener(String),
}

impl From<AuthError> for OAuthError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::AuthorizationRequired => OAuthError::SignInRequired,
            other => OAuthError::Auth(other.to_string()),
        }
    }
}

/// Opens the system browser. The app passes its opener plugin; tests follow
/// the URL themselves.
pub trait BrowserOpener: Send + Sync {
    fn open(&self, url: &str) -> Result<(), String>;
}

fn http_url(entry: &ServerEntry) -> Result<(&str, &[String], Option<&str>), OAuthError> {
    match (&entry.config.transport, &entry.config.auth) {
        (TransportConfig::Http { url, .. }, AuthConfig::OAuth { scopes, client_id }) => {
            Ok((url.as_str(), scopes.as_slice(), client_id.as_deref()))
        }
        _ => Err(OAuthError::NotOAuth),
    }
}

/// Endpoints from discovery must pass the same network rules.
pub fn check_metadata(m: &AuthorizationMetadata, policy: &NetPolicy) -> Result<(), OAuthError> {
    let check = |endpoint: &'static str, raw: &str| {
        policy
            .parse_and_check(raw)
            .map(|_| ())
            .map_err(|reason| OAuthError::BlockedEndpoint { endpoint, reason })
    };
    check("authorization endpoint", &m.authorization_endpoint)?;
    check("token endpoint", &m.token_endpoint)?;
    if let Some(r) = &m.registration_endpoint {
        check("registration endpoint", r)?;
    }
    Ok(())
}

async fn manager(
    entry: &ServerEntry,
    url: &str,
    secrets: Arc<dyn SecretStore>,
    policy: NetPolicy,
) -> Result<AuthorizationManager, OAuthError> {
    policy.parse_and_check(url)?;
    let client = GuardedOAuthClient::new(policy)
        .map_err(|e| OAuthError::Auth(e.to_string()))?
        .into_arc();
    let mut mgr = AuthorizationManager::new_with_oauth_http_client(url, client).await?;
    mgr.set_credential_store(KeychainCredentials::new(secrets, entry.id.clone()));
    Ok(mgr)
}

/// Run the browser sign-in for one connector and store its tokens. Returns
/// the authorization server metadata to pin with the connector.
pub async fn sign_in(
    entry: &ServerEntry,
    secrets: Arc<dyn SecretStore>,
    policy: NetPolicy,
    opener: &dyn BrowserOpener,
    timeout: Duration,
) -> Result<serde_json::Value, OAuthError> {
    let (url, scopes, client_id) = http_url(entry)?;
    let creds = KeychainCredentials::new(secrets.clone(), entry.id.clone());
    let mut mgr = manager(entry, url, secrets, policy).await?;

    let resolution = mgr.resolve_metadata().await?;
    check_metadata(&resolution.metadata, &policy)?;
    let pinned =
        serde_json::to_value(&resolution.metadata).map_err(|e| OAuthError::Auth(e.to_string()))?;
    mgr.set_metadata(resolution.metadata);

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| OAuthError::Listener(e.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|e| OAuthError::Listener(e.to_string()))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();

    match client_id {
        Some(id) => {
            let mut cfg =
                OAuthClientConfig::new(id, redirect_uri.clone()).with_scopes(scopes.to_vec());
            if let Some(secret) = creds.client_secret() {
                cfg = cfg.with_client_secret(secret);
            }
            mgr.configure_client(cfg)?;
        }
        None => {
            let registered = mgr
                .register_client(CLIENT_NAME, &redirect_uri, &scope_refs)
                .await?;
            creds
                .set_client_secret(registered.client_secret.as_deref())
                .map_err(|e| OAuthError::Auth(e.to_string()))?;
        }
    }

    let auth_url = mgr.get_authorization_url(&scope_refs).await?;
    policy
        .parse_and_check(&auth_url)
        .map_err(|reason| OAuthError::BlockedEndpoint {
            endpoint: "authorization endpoint",
            reason,
        })?;
    opener.open(&auth_url).map_err(OAuthError::Browser)?;

    let reply = tokio::time::timeout(timeout, wait_for_callback(listener))
        .await
        .map_err(|_| OAuthError::TimedOut)??;
    mgr.exchange_code_for_token_with_issuer(&reply.code, &reply.state, reply.iss.as_deref())
        .await?;
    tracing::info!(connector = %entry.id, "connector sign-in complete");
    Ok(pinned)
}

/// A current access token, refreshed when it's about to expire.
pub async fn access_token(
    entry: &ServerEntry,
    secrets: Arc<dyn SecretStore>,
    policy: NetPolicy,
) -> Result<String, OAuthError> {
    use rmcp::transport::auth::CredentialStore;
    let (url, scopes, client_id) = http_url(entry)?;
    let creds = KeychainCredentials::new(secrets.clone(), entry.id.clone());
    let Some(stored) = creds.load().await? else {
        return Err(OAuthError::SignInRequired);
    };
    let mut mgr = manager(entry, url, secrets, policy).await?;
    // The server pinned at sign-in; a connector signed in before pinning
    // existed discovers once more.
    let metadata = match &entry.oauth_metadata {
        Some(pinned) => serde_json::from_value::<AuthorizationMetadata>(pinned.clone())
            .map_err(|_| OAuthError::SignInRequired)?,
        None => mgr.resolve_metadata().await?.metadata,
    };
    check_metadata(&metadata, &policy)?;
    mgr.set_metadata(metadata);
    if !mgr.initialize_from_store().await? {
        return Err(OAuthError::SignInRequired);
    }
    // initialize_from_store forgets the client secret; restore it for refresh.
    if let Some(secret) = creds.client_secret() {
        let id = client_id.map(str::to_string).unwrap_or(stored.client_id);
        mgr.configure_client(
            OAuthClientConfig::new(id, url.to_string())
                .with_client_secret(secret)
                .with_scopes(scopes.to_vec()),
        )?;
    }
    Ok(mgr.get_access_token().await?)
}

struct CallbackReply {
    code: String,
    state: String,
    iss: Option<String>,
}

/// Read one request head, giving up on a connection that stalls.
async fn read_head(sock: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let read = async {
        loop {
            let n = match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > MAX_CALLBACK_REQUEST {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(CALLBACK_READ_TIMEOUT, read).await;
    buf
}

/// Accept connections until one hits `/callback` with a code (or an error).
async fn wait_for_callback(listener: TcpListener) -> Result<CallbackReply, OAuthError> {
    loop {
        let (mut sock, _) = listener
            .accept()
            .await
            .map_err(|e| OAuthError::Listener(e.to_string()))?;
        let buf = read_head(&mut sock).await;
        let head = String::from_utf8_lossy(&buf);
        let target = head
            .lines()
            .next()
            .and_then(|l| {
                let mut parts = l.split_whitespace();
                (parts.next() == Some("GET"))
                    .then(|| parts.next())
                    .flatten()
            })
            .unwrap_or("")
            .to_string();
        let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
            respond(&mut sock, 400, "Bad request.").await;
            continue;
        };
        if url.path() != "/callback" {
            respond(&mut sock, 404, "Not found.").await;
            continue;
        }
        let get = |k: &str| {
            url.query_pairs()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.into_owned())
        };
        if let Some(err) = get("error") {
            respond(
                &mut sock,
                200,
                "Sign-in was cancelled. You can close this tab.",
            )
            .await;
            let safe: String = err
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .take(40)
                .collect();
            return Err(OAuthError::Denied(safe));
        }
        match (get("code"), get("state")) {
            (Some(code), Some(state)) => {
                respond(
                    &mut sock,
                    200,
                    "Signed in. You can close this tab and go back to Fly on the Wall.",
                )
                .await;
                return Ok(CallbackReply {
                    code,
                    state,
                    iss: get("iss"),
                });
            }
            _ => respond(&mut sock, 400, "The sign-in reply was incomplete.").await,
        }
    }
}

async fn respond(sock: &mut tokio::net::TcpStream, status: u16, text: &str) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Fly on the Wall</title>\
         <p style=\"font-family:system-ui;margin:3em\">{text}</p>"
    );
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}
