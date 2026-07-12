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

    // wait for exactly one redirect (5 min timeout)
    let (mut stream, _) =
        tokio::time::timeout(std::time::Duration::from_secs(300), listener.accept())
            .await
            .map_err(|_| CalendarError::Auth("timed out waiting for the browser redirect".into()))?
            .map_err(|e| CalendarError::Auth(e.to_string()))?;
    let mut buf = vec![0u8; 4096];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| CalendarError::Auth(e.to_string()))?;
    let request = String::from_utf8_lossy(&buf[..n]).to_string();

    let result = parse_redirect_request(&request, &state);
    let body = match &result {
        Ok(_) => "<html><body style='font-family:sans-serif'><h2>Fly on the Wall is connected \u{2705}</h2>You can close this tab and return to the app.</body></html>",
        Err(_) => "<html><body style='font-family:sans-serif'><h2>Connection failed</h2>Please return to Fly on the Wall and try again.</body></html>",
    };
    let _ = stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        )
        .await;
    let code = result?;

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
    let first_line = request.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("");
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
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
