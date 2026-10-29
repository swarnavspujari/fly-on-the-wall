//! OAuth sign-in against a local fixture authorization server: the network
//! rules hold during discovery, and a full sign-in stores tokens apart.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fly_mcp_client::config::{AuthConfig, ServerConfig, TransportConfig};
use fly_mcp_client::netguard::NetPolicy;
use fly_mcp_client::oauth::{self, BrowserOpener, OAuthError};
use fly_mcp_client::registry::{Registry, ServerEntry};
use fly_mcp_client::secrets::get_chunked;
use fly_secrets::{MemorySecretStore, SecretStore};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
struct Req {
    method: String,
    target: String,
    body: String,
}

struct Resp {
    status: u16,
    location: Option<String>,
    body: String,
}

impl Resp {
    fn json(v: Value) -> Self {
        Self {
            status: 200,
            location: None,
            body: v.to_string(),
        }
    }
    fn redirect(to: &str) -> Self {
        Self {
            status: 302,
            location: Some(to.into()),
            body: String::new(),
        }
    }
    fn not_found() -> Self {
        Self {
            status: 404,
            location: None,
            body: "{}".into(),
        }
    }
}

type Route = Arc<dyn Fn(&Req, &str) -> Resp + Send + Sync>;

struct HttpFixture {
    origin: String,
    hits: Arc<Mutex<Vec<Req>>>,
}

async fn start(route: Route) -> HttpFixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let origin = format!("http://127.0.0.1:{}", addr.port());
    let hits = Arc::new(Mutex::new(Vec::new()));
    let (o, h) = (origin.clone(), hits.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (o, h, route) = (o.clone(), h.clone(), route.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break p + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())?
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let mut first = head.lines().next().unwrap_or("").split_whitespace();
                let req = Req {
                    method: first.next().unwrap_or("").into(),
                    target: first.next().unwrap_or("").into(),
                    body: String::from_utf8_lossy(&buf[head_end..]).to_string(),
                };
                h.lock().unwrap().push(req.clone());
                let resp = route(&req, &o);
                let mut out = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    resp.status,
                    resp.body.len()
                );
                if let Some(l) = resp.location {
                    out.push_str(&format!("Location: {l}\r\n"));
                }
                out.push_str("\r\n");
                out.push_str(&resp.body);
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    HttpFixture { origin, hits }
}

fn entry_for(origin: &str) -> ServerEntry {
    let mut reg = Registry::default();
    let id = reg
        .add_approved(
            ServerConfig {
                name: "Docs".into(),
                transport: TransportConfig::Http {
                    url: format!("{origin}/mcp"),
                    headers: vec![],
                },
                auth: AuthConfig::OAuth {
                    scopes: vec!["read".into()],
                    client_id: None,
                },
            },
            0,
        )
        .unwrap();
    reg.server(&id).unwrap().clone()
}

/// Records the URL; never follows it.
#[derive(Default)]
struct NoBrowser(Mutex<Vec<String>>);

impl BrowserOpener for NoBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(url.into());
        Ok(())
    }
}

fn as_metadata(origin: &str, authorize: &str, token: &str) -> Value {
    json!({
        "issuer": origin,
        "authorization_endpoint": authorize,
        "token_endpoint": token,
        "registration_endpoint": format!("{origin}/register"),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
        "grant_types_supported": ["authorization_code", "refresh_token"]
    })
}

fn prm(origin: &str, servers: Value) -> Value {
    json!({"resource": format!("{origin}/mcp"), "authorization_servers": servers})
}

#[tokio::test]
async fn the_guarded_client_refuses_private_redirect_hops() {
    use fly_mcp_client::netguard::{guarded_client, Redirects};
    let fx = start(Arc::new(|_: &Req, _: &str| {
        Resp::redirect("https://10.0.0.5/internal")
    }))
    .await;
    let client = guarded_client(NetPolicy::local_test(), Redirects::Follow).unwrap();
    let err = client
        .get(format!("{}/start", fx.origin))
        .send()
        .await
        .unwrap_err();
    let chain = format!("{err:?}");
    assert!(chain.contains("PrivateAddress"), "{chain}");
}

#[tokio::test]
async fn https_only_outside_tests() {
    let fx = start(Arc::new(|_: &Req, _: &str| Resp::not_found())).await;
    let browser = NoBrowser::default();
    let err = oauth::sign_in(
        &entry_for(&fx.origin),
        Arc::new(MemorySecretStore::default()),
        NetPolicy::strict(),
        &browser,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, OAuthError::Blocked(_)), "{err}");
    assert!(fx.hits.lock().unwrap().is_empty(), "nothing was contacted");
    assert!(browser.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn discovery_redirects_to_private_addresses_are_blocked() {
    let fx = start(Arc::new(|_: &Req, _: &str| {
        Resp::redirect("http://169.254.169.254/latest/meta-data/iam/security-credentials/")
    }))
    .await;
    let browser = NoBrowser::default();
    let res = oauth::sign_in(
        &entry_for(&fx.origin),
        Arc::new(MemorySecretStore::default()),
        NetPolicy::local_test(),
        &browser,
        Duration::from_secs(5),
    )
    .await;
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("169.254.169.254") && err.contains("rejected"),
        "{err}"
    );
    assert!(
        browser.0.lock().unwrap().is_empty(),
        "the browser never opened"
    );
    let hits: Vec<String> = fx
        .hits
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.target.clone())
        .collect();
    assert_eq!(hits, vec!["/mcp"], "discovery stopped at the redirect");
}

#[tokio::test]
async fn private_endpoints_in_discovered_metadata_are_refused() {
    let fx = start(Arc::new(|r: &Req, o: &str| {
        if r.target
            .starts_with("/.well-known/oauth-protected-resource")
        {
            Resp::json(prm(o, json!([o])))
        } else if r.target.starts_with("/.well-known/") {
            Resp::json(as_metadata(
                o,
                "https://10.0.0.8/authorize",
                "https://192.168.1.9/token",
            ))
        } else {
            Resp::not_found()
        }
    }))
    .await;
    let browser = NoBrowser::default();
    let err = oauth::sign_in(
        &entry_for(&fx.origin),
        Arc::new(MemorySecretStore::default()),
        NetPolicy::local_test(),
        &browser,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, OAuthError::BlockedEndpoint { .. }), "{err:?}");
    assert!(browser.0.lock().unwrap().is_empty());
    assert!(
        !fx.hits
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.target == "/register"),
        "nothing was registered"
    );
}

#[tokio::test]
async fn an_authorization_server_on_a_private_address_is_never_used() {
    let fx = start(Arc::new(|r: &Req, o: &str| {
        if r.target
            .starts_with("/.well-known/oauth-protected-resource")
        {
            Resp::json(prm(o, json!(["https://10.0.0.9"])))
        } else {
            Resp::not_found()
        }
    }))
    .await;
    let browser = NoBrowser::default();
    let res = oauth::sign_in(
        &entry_for(&fx.origin),
        Arc::new(MemorySecretStore::default()),
        NetPolicy::local_test(),
        &browser,
        Duration::from_secs(5),
    )
    .await;
    let err = res.unwrap_err().to_string();
    assert!(err.contains("none published usable metadata"), "{err}");
    assert!(
        browser.0.lock().unwrap().is_empty(),
        "the browser never opened"
    );
    // Only the fixture was contacted; the private server's metadata request
    // was refused by the guard before any connection.
    assert!(fx
        .hits
        .lock()
        .unwrap()
        .iter()
        .all(|r| !r.target.contains("10.0.0.9")));
}

/// Follows the authorization URL like a browser would after the user signs
/// in: the fixture's /authorize redirects back to FOTW's loopback listener.
struct FollowingBrowser(Mutex<Vec<String>>);

impl BrowserOpener for FollowingBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(url.into());
        let url = url.to_string();
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let resp = client.get(&url).send().await.unwrap();
            let back = resp.headers()["location"].to_str().unwrap().to_string();
            client.get(&back).send().await.unwrap();
        });
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_uses_pkce_and_stores_access_and_refresh_apart() {
    let fx = start(Arc::new(|r: &Req, o: &str| {
        let path = r.target.split('?').next().unwrap_or("");
        match (r.method.as_str(), path) {
            (_, p) if p.starts_with("/.well-known/oauth-protected-resource") => {
                Resp::json(prm(o, json!([o])))
            }
            (_, p) if p.starts_with("/.well-known/") => Resp::json(as_metadata(
                o,
                &format!("{o}/authorize"),
                &format!("{o}/token"),
            )),
            ("POST", "/register") => Resp::json(json!({
                "client_id": "fotw-test-client",
                "redirect_uris": [serde_json::from_str::<Value>(&r.body).unwrap()["redirect_uris"][0]]
            })),
            ("GET", "/authorize") => {
                let u = url::Url::parse(&format!("{o}{}", r.target)).unwrap();
                let q = |k: &str| {
                    u.query_pairs()
                        .find(|(key, _)| key == k)
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_default()
                };
                Resp::redirect(&format!(
                    "{}?code=c0de&state={}",
                    q("redirect_uri"),
                    q("state")
                ))
            }
            ("POST", "/token") => Resp::json(json!({
                "access_token": "A".repeat(1500),
                "token_type": "Bearer",
                "expires_in": 3600,
                "refresh_token": "R-refresh-1"
            })),
            _ => Resp::not_found(),
        }
    }))
    .await;
    let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
    let entry = entry_for(&fx.origin);
    let browser = FollowingBrowser(Mutex::new(Vec::new()));
    oauth::sign_in(
        &entry,
        secrets.clone(),
        NetPolicy::local_test(),
        &browser,
        Duration::from_secs(20),
    )
    .await
    .unwrap();

    // PKCE on the way out and back.
    let opened = browser.0.lock().unwrap()[0].clone();
    assert!(opened.contains("code_challenge="), "{opened}");
    assert!(opened.contains("code_challenge_method=S256"), "{opened}");
    let hits = fx.hits.lock().unwrap().clone();
    let token_req = hits
        .iter()
        .find(|r| r.target == "/token")
        .expect("token exchange");
    assert!(
        token_req.body.contains("code_verifier="),
        "{}",
        token_req.body
    );
    assert!(token_req.body.contains("code=c0de"));

    // Access and refresh tokens in separate keychain entries; the long
    // access token is chunked.
    let access = get_chunked(secrets.as_ref(), &format!("connector.{}.access", entry.id))
        .unwrap()
        .unwrap();
    assert!(!access.contains("R-refresh-1"));
    assert!(secrets
        .get(&format!("connector.{}.access.0", entry.id))
        .unwrap()
        .is_some());
    assert_eq!(
        secrets
            .get(&format!("connector.{}.refresh", entry.id))
            .unwrap()
            .as_deref(),
        Some("R-refresh-1")
    );

    // And the stored token is usable.
    let token = oauth::access_token(&entry, secrets.clone(), NetPolicy::local_test())
        .await
        .unwrap();
    assert_eq!(token, "A".repeat(1500));
}
