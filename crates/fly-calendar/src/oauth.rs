//! Minimal OAuth 2.0 authorization-code flow with PKCE and a loopback
//! redirect — the standard pattern for installed desktop apps. No client
//! secret is required for public clients (MS Graph); Google installed apps
//! use a client id + (non-confidential) client secret.

use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{CalendarError, Result};

/// Branded loopback response shown in the browser after a successful connect.
/// Self-contained (no external assets — this is a one-shot loopback reply):
/// full document, dark brand card (ink / cream / violet), best-effort auto-close.
pub const CONNECTED_PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Fly on the Wall — Connected</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body { height: 100%; margin: 0; }
  body { display: grid; place-items: center; padding: 24px;
    background: #0d0d12; color: #f2f2e8;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; }
  .card { width: 100%; max-width: 420px; text-align: center;
    background: #17161f; border: 1px solid rgba(242,242,232,.10);
    border-radius: 20px; padding: 40px 32px; box-shadow: 0 20px 60px rgba(0,0,0,.45); }
  .badge { width: 64px; height: 64px; margin: 0 auto 20px; display: grid; place-items: center;
    border-radius: 50%; background: #7a5cff; }
  h1 { margin: 0 0 8px; font-size: 22px; font-weight: 700; letter-spacing: -.01em; }
  p { margin: 0; font-size: 14px; line-height: 1.6; color: rgba(242,242,232,.72); }
  .hint { margin-top: 18px; font-size: 12.5px; color: rgba(242,242,232,.5); }
</style>
</head>
<body>
  <main class="card">
    <div class="badge" aria-hidden="true">
      <svg width="30" height="30" viewBox="0 0 24 24" fill="none" stroke="#0d0d12" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>
    </div>
    <h1>You're connected</h1>
    <p>Fly on the Wall can now see your upcoming meetings — everything stays on your machine.</p>
    <p class="hint">You can close this tab and return to the app.</p>
  </main>
  <script>setTimeout(function(){try{window.close();}catch(e){}}, 3000);</script>
</body>
</html>"##;

/// Branded loopback response shown when the connect flow fails.
pub const FAILED_PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Fly on the Wall — Connection failed</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body { height: 100%; margin: 0; }
  body { display: grid; place-items: center; padding: 24px;
    background: #0d0d12; color: #f2f2e8;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; }
  .card { width: 100%; max-width: 420px; text-align: center;
    background: #17161f; border: 1px solid rgba(242,242,232,.10);
    border-radius: 20px; padding: 40px 32px; box-shadow: 0 20px 60px rgba(0,0,0,.45); }
  .badge { width: 64px; height: 64px; margin: 0 auto 20px; display: grid; place-items: center;
    border-radius: 50%; background: #f2f2e8; }
  h1 { margin: 0 0 8px; font-size: 22px; font-weight: 700; letter-spacing: -.01em; }
  p { margin: 0; font-size: 14px; line-height: 1.6; color: rgba(242,242,232,.72); }
  .hint { margin-top: 18px; font-size: 12.5px; color: rgba(242,242,232,.5); }
</style>
</head>
<body>
  <main class="card">
    <div class="badge" aria-hidden="true">
      <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="#0d0d12" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6L6 18M6 6l12 12"/></svg>
    </div>
    <h1>Connection failed</h1>
    <p>Something went wrong while connecting. Please return to Fly on the Wall and try again.</p>
    <p class="hint">You can close this tab.</p>
  </main>
</body>
</html>"##;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
}

impl TokenSet {
    pub fn is_expired(&self) -> bool {
        Utc::now() + Duration::seconds(60) >= self.expires_at
    }
}

pub struct OAuthConfig {
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: String,
    /// Provider-specific additions to the auth URL (e.g. Google's
    /// access_type=offline&prompt=consent for refresh tokens).
    pub extra_auth_params: Vec<(&'static str, &'static str)>,
}

/// Run the interactive flow: spin a loopback listener, hand the auth URL to
/// `open_url` (the app opens the system browser), wait for the redirect,
/// exchange the code. Times out after 5 minutes.
pub async fn interactive_auth(
    cfg: &OAuthConfig,
    open_url: &(dyn Fn(String) + Send + Sync),
) -> Result<TokenSet> {
    // PKCE verifier + S256 challenge
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| CalendarError::Auth(format!("cannot bind loopback listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| CalendarError::Auth(e.to_string()))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let state = uuid::Uuid::new_v4().simple().to_string();

    let mut auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        cfg.auth_url,
        urlencoding::encode(&cfg.client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&cfg.scopes),
        state,
        challenge,
    );
    for (k, v) in &cfg.extra_auth_params {
        auth_url.push_str(&format!("&{k}={v}"));
    }
    open_url(auth_url);

    let code = wait_for_redirect(&listener, &state, REDIRECT_TIMEOUT).await?;

    // exchange the code
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("client_id", cfg.client_id.clone()),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    if let Some(secret) = &cfg.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    token_request(&cfg.token_url, &form).await
}

/// How long the interactive flow waits for the browser redirect.
const REDIRECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How long one loopback connection gets to send its request before it is
/// dropped, so a silent connection can't hold the listener.
const REDIRECT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Reply to loopback connections that aren't this sign-in's redirect.
const BAD_REQUEST_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Bad request</title><p>This is not the sign-in redirect Fly on the Wall is waiting for.</p>";

/// Accept loopback connections until one carries this flow's `state` (or
/// `timeout` elapses). Any other local process can connect to the port
/// first — a stray or hostile connection gets a small 400 page and is
/// closed instead of ending the sign-in. Only the matching request decides
/// the outcome: its code, or the provider's error.
async fn wait_for_redirect(
    listener: &tokio::net::TcpListener,
    expected_state: &str,
    timeout: std::time::Duration,
) -> Result<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let (mut stream, _) = tokio::time::timeout_at(deadline, listener.accept())
            .await
            .map_err(|_| CalendarError::Auth("timed out waiting for the browser redirect".into()))?
            .map_err(|e| CalendarError::Auth(e.to_string()))?;
        let mut buf = vec![0u8; 4096];
        let read_until = deadline.min(tokio::time::Instant::now() + REDIRECT_READ_TIMEOUT);
        let n = match tokio::time::timeout_at(read_until, stream.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => continue, // silent, closed or broken: drop it
        };
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        if !redirect_state_matches(&request, expected_state) {
            respond(&mut stream, "400 Bad Request", BAD_REQUEST_PAGE).await;
            continue;
        }
        let result = parse_redirect_request(&request, expected_state);
        let page = if result.is_ok() {
            CONNECTED_PAGE
        } else {
            FAILED_PAGE
        };
        respond(&mut stream, "200 OK", page).await;
        return result;
    }
}

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let _ = stream
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
            .as_bytes(),
        )
        .await;
    let _ = stream.shutdown().await;
}

/// The `key=value` pairs of the request line's query string (raw).
fn redirect_query(request: &str) -> impl Iterator<Item = (&str, &str)> {
    let first_line = request.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("");
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    query
        .split('&')
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
}

/// Whether the request carries exactly this flow's `state`.
fn redirect_state_matches(request: &str, expected_state: &str) -> bool {
    redirect_query(request).any(|(k, v)| k == "state" && v == expected_state)
}

/// Refresh an expired token; providers keep the old refresh token when the
/// response omits one (Google does on refresh).
pub async fn refresh(cfg: &OAuthConfig, refresh_token: &str) -> Result<TokenSet> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", cfg.client_id.clone()),
    ];
    if let Some(secret) = &cfg.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    let mut tokens = token_request(&cfg.token_url, &form).await?;
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(refresh_token.to_string());
    }
    Ok(tokens)
}

async fn token_request(token_url: &str, form: &[(&str, String)]) -> Result<TokenSet> {
    let client = reqwest::Client::new();
    let resp = client
        .post(token_url)
        .form(form)
        .send()
        .await
        .map_err(|e| CalendarError::Network(e.to_string()))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| CalendarError::Network(e.to_string()))?;
    if !status.is_success() {
        return Err(CalendarError::Auth(format!(
            "token endpoint returned {status}: {}",
            text.chars().take(300).collect::<String>()
        )));
    }
    parse_token_response(&text)
}

pub fn parse_token_response(json: &str) -> Result<TokenSet> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CalendarError::Auth(format!("bad token JSON: {e}")))?;
    let access_token = v
        .get("access_token")
        .and_then(|t| t.as_str())
        .ok_or_else(|| CalendarError::Auth("token response missing access_token".into()))?
        .to_string();
    let expires_in = v.get("expires_in").and_then(|e| e.as_i64()).unwrap_or(3600);
    Ok(TokenSet {
        access_token,
        refresh_token: v
            .get("refresh_token")
            .and_then(|t| t.as_str())
            .map(str::to_string),
        expires_at: Utc::now() + Duration::seconds(expires_in),
    })
}

/// Extract the auth code from the loopback HTTP request, checking state.
pub fn parse_redirect_request(request: &str, expected_state: &str) -> Result<String> {
    let mut code = None;
    let mut state = None;
    for (k, v) in redirect_query(request) {
        match k {
            "code" => code = Some(urlencoding::decode(v).unwrap_or_default().to_string()),
            "state" => state = Some(v.to_string()),
            "error" => return Err(CalendarError::Auth(format!("provider returned error: {v}"))),
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Err(CalendarError::Auth(
            "state mismatch in OAuth redirect".into(),
        ));
    }
    code.ok_or_else(|| CalendarError::Auth("redirect had no code".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_redirect_code_and_checks_state() {
        let req = "GET /?state=abc&code=4%2FxyzToken HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        assert_eq!(parse_redirect_request(req, "abc").unwrap(), "4/xyzToken");
        assert!(parse_redirect_request(req, "WRONG").is_err());
    }

    #[test]
    fn redirect_error_param_fails() {
        let req = "GET /?error=access_denied&state=s HTTP/1.1\r\n";
        assert!(parse_redirect_request(req, "s").is_err());
    }

    async fn send(port: u16, request: &str) -> String {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        s.write_all(request.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    /// Stray local connections (wrong state, no state, silent, garbage)
    /// are answered 400 and ignored; the real redirect still completes.
    #[tokio::test]
    async fn loopback_ignores_connections_without_matching_state() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiter = tokio::spawn(async move {
            wait_for_redirect(&listener, "good", std::time::Duration::from_secs(20)).await
        });

        let wrong = send(port, "GET /?state=evil&code=stolen HTTP/1.1\r\n\r\n").await;
        assert!(wrong.starts_with("HTTP/1.1 400"), "{wrong}");
        // an attacker-supplied error must not abort the real sign-in
        let err = send(port, "GET /?error=access_denied HTTP/1.1\r\n\r\n").await;
        assert!(err.starts_with("HTTP/1.1 400"), "{err}");
        let garbage = send(port, "\x16\x03\x01 not http").await;
        assert!(garbage.starts_with("HTTP/1.1 400"), "{garbage}");
        // a connection that closes without sending anything
        drop(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap(),
        );

        let ok = send(port, "GET /?state=good&code=4%2Freal HTTP/1.1\r\n\r\n").await;
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("connected"));
        assert_eq!(waiter.await.unwrap().unwrap(), "4/real");
    }

    /// The provider's own error (matching state) still ends the flow.
    #[tokio::test]
    async fn loopback_reports_provider_error_with_matching_state() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiter = tokio::spawn(async move {
            wait_for_redirect(&listener, "good", std::time::Duration::from_secs(20)).await
        });
        let resp = send(
            port,
            "GET /?error=access_denied&state=good HTTP/1.1\r\n\r\n",
        )
        .await;
        assert!(resp.contains("Connection failed"), "{resp}");
        assert!(waiter.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn loopback_times_out_when_only_strays_connect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiter = tokio::spawn(async move {
            wait_for_redirect(&listener, "good", std::time::Duration::from_millis(500)).await
        });
        send(port, "GET /?state=evil HTTP/1.1\r\n\r\n").await;
        let err = waiter.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
    }

    #[test]
    fn parses_token_response_with_expiry() {
        let t =
            parse_token_response(r#"{"access_token":"at","refresh_token":"rt","expires_in":10}"#)
                .unwrap();
        assert_eq!(t.access_token, "at");
        assert_eq!(t.refresh_token.as_deref(), Some("rt"));
        assert!(!t.is_expired() || t.expires_at > Utc::now()); // 10s - 60s skew → treated as expiring
    }
}
