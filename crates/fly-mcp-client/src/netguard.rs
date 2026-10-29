//! Network rules for remote connectors: https only, and no private addresses.
//!
//! Applied to the MCP transport and to every OAuth request (discovery,
//! registration, token exchange, refresh), at three points:
//! * the URL of every request and every redirect hop ([`NetPolicy::check_url`]):
//!   https, no credentials in the URL, no private or reserved IP literal, no
//!   `localhost`;
//! * DNS ([`GuardedResolver`]): names that resolve only to private, loopback,
//!   link-local or reserved addresses fail, and such addresses are dropped
//!   from mixed answers, so a public name can't be rebound to an internal host;
//! * redirects: at most [`MAX_REDIRECTS`], each hop checked as above. The MCP
//!   transport follows none.
//!
//! Limit: when a system proxy is configured, the proxy resolves names, so the
//! DNS filter can't see the final address (URL checks still apply).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use rmcp::transport::auth::{
    OAuthHttpClient, OAuthHttpClientError, OAuthHttpClientFuture, OAuthHttpRedirectPolicy,
    OAuthHttpRequest,
};
use url::{Host, Url};

/// Most redirect hops followed for an OAuth request.
pub const MAX_REDIRECTS: usize = 5;
/// Largest OAuth response body read, in bytes.
pub const MAX_OAUTH_BODY_BYTES: usize = 512 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum NetError {
    #[error("only https addresses can be used (got {0})")]
    NotHttps(String),
    #[error("the address can't contain a user name or password")]
    Credentials,
    #[error("the address has no host")]
    NoHost,
    #[error("{0} is a private, local or reserved address and can't be used")]
    PrivateAddress(String),
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("the response was larger than {0} bytes")]
    TooLarge(usize),
    #[error("couldn't parse the address: {0}")]
    BadUrl(String),
}

/// Which addresses a connection may reach. The app always uses
/// [`NetPolicy::strict`]; tests can allow plain http to a loopback IP literal
/// so a fixture server can run without TLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetPolicy {
    loopback_http: bool,
}

impl NetPolicy {
    pub const fn strict() -> Self {
        Self {
            loopback_http: false,
        }
    }

    /// Plain http to `127.0.0.1` / `[::1]` (IP literals only). Private,
    /// link-local and reserved addresses stay blocked.
    #[cfg(any(test, feature = "test-support"))]
    pub const fn local_test() -> Self {
        Self {
            loopback_http: true,
        }
    }

    pub fn allows_loopback(&self) -> bool {
        self.loopback_http
    }

    pub fn parse_and_check(&self, raw: &str) -> Result<Url, NetError> {
        let url = Url::parse(raw.trim()).map_err(|e| NetError::BadUrl(e.to_string()))?;
        self.check_url(&url)?;
        Ok(url)
    }

    /// The URL rule applied to every request and redirect hop.
    pub fn check_url(&self, url: &Url) -> Result<(), NetError> {
        if !url.username().is_empty() || url.password().is_some() {
            return Err(NetError::Credentials);
        }
        let host = url.host().ok_or(NetError::NoHost)?;
        let loopback_literal = match &host {
            Host::Ipv4(ip) => ip.is_loopback(),
            Host::Ipv6(ip) => ip.is_loopback(),
            Host::Domain(_) => false,
        };
        match url.scheme() {
            "https" => {}
            "http" if self.loopback_http && loopback_literal => {}
            other => return Err(NetError::NotHttps(other.to_string())),
        }
        match host {
            Host::Ipv4(ip) => self.check_ip(IpAddr::V4(ip)),
            Host::Ipv6(ip) => self.check_ip(IpAddr::V6(ip)),
            Host::Domain(d) => {
                let d = d.trim_end_matches('.').to_ascii_lowercase();
                if d == "localhost" || d.ends_with(".localhost") {
                    Err(NetError::PrivateAddress(d))
                } else {
                    Ok(())
                }
            }
        }
    }

    fn check_ip(&self, ip: IpAddr) -> Result<(), NetError> {
        if self.allows_ip(ip) {
            Ok(())
        } else {
            Err(NetError::PrivateAddress(ip.to_string()))
        }
    }

    fn allows_ip(&self, ip: IpAddr) -> bool {
        is_public_ip(ip) || (self.loopback_http && ip.is_loopback())
    }

    /// Keep only the addresses this policy may connect to.
    pub fn filter_addrs(&self, addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
        addrs
            .into_iter()
            .filter(|a| self.allows_ip(a.ip()))
            .collect()
    }
}

/// Globally routable unicast address: not private, loopback, link-local,
/// shared (CGNAT), documentation, benchmarking, multicast, broadcast or
/// reserved. IPv6 forms that embed an IPv4 address are judged by it.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0 // "this network"
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 shared (CGNAT)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 IETF
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15 benchmarking
        || o[0] >= 240) // 240.0.0.0/4 reserved
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = embedded_v4(ip) {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
        || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        || (s[0] & 0xffc0) == 0xfec0 // fec0::/10 site-local (deprecated)
        || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32 documentation
        || (s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0)) // 100::/64 discard
}

/// IPv4 inside IPv6: mapped (::ffff:a.b.c.d), compatible (::a.b.c.d),
/// NAT64 (64:ff9b::/96) and 6to4 (2002:aabb:ccdd::/48).
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    let s = ip.segments();
    let tail = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
    if s[..6].iter().all(|&x| x == 0) && !ip.is_loopback() && !ip.is_unspecified() {
        return Some(tail);
    }
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0) {
        return Some(tail);
    }
    if s[0] == 0x2002 {
        return Some(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    None
}

/// DNS resolver that drops addresses the policy forbids.
#[derive(Debug, Clone, Copy)]
pub struct GuardedResolver {
    policy: NetPolicy,
}

impl GuardedResolver {
    pub fn new(policy: NetPolicy) -> Self {
        Self { policy }
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = self.policy;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let found: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let allowed = policy.filter_addrs(found);
            if allowed.is_empty() {
                tracing::warn!(host = %host, "connector host resolves only to blocked addresses");
                return Err(Box::new(NetError::PrivateAddress(host))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Whether a client follows redirects (each hop checked) or returns them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redirects {
    Follow,
    Stop,
}

/// A reqwest client that enforces `policy` on DNS and redirects.
pub fn guarded_client(policy: NetPolicy, redirects: Redirects) -> reqwest::Result<reqwest::Client> {
    let redirect = match redirects {
        Redirects::Stop => reqwest::redirect::Policy::none(),
        Redirects::Follow => reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > MAX_REDIRECTS {
                return attempt.error(NetError::TooManyRedirects);
            }
            match policy.check_url(attempt.url()) {
                Ok(()) => attempt.follow(),
                Err(e) => {
                    tracing::warn!(error = %e, "blocked a connector redirect");
                    attempt.error(e)
                }
            }
        }),
    };
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(redirect)
        .https_only(!policy.allows_loopback())
        .dns_resolver(GuardedResolver::new(policy))
        .user_agent(concat!(
            "FlyOnTheWall-Connectors/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
}

/// The OAuth HTTP client handed to rmcp's `AuthorizationManager`: every
/// discovery, registration and token request goes through the policy.
pub struct GuardedOAuthClient {
    policy: NetPolicy,
    follow: reqwest::Client,
    stop: reqwest::Client,
}

impl GuardedOAuthClient {
    pub fn new(policy: NetPolicy) -> reqwest::Result<Self> {
        Ok(Self {
            policy,
            follow: guarded_client(policy, Redirects::Follow)?,
            stop: guarded_client(policy, Redirects::Stop)?,
        })
    }

    pub fn into_arc(self) -> Arc<dyn OAuthHttpClient> {
        Arc::new(self)
    }
}

impl OAuthHttpClient for GuardedOAuthClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            let client = match request.redirect_policy {
                OAuthHttpRedirectPolicy::Follow => &self.follow,
                _ => &self.stop,
            };
            let req = reqwest::Request::try_from(request.request)
                .map_err(|e| Box::new(e) as OAuthHttpClientError)?;
            if let Err(e) = self.policy.check_url(req.url()) {
                tracing::warn!(error = %e, "blocked an OAuth request");
                return Err(Box::new(e) as OAuthHttpClientError);
            }
            let mut resp = client
                .execute(req)
                .await
                .map_err(|e| Box::new(e) as OAuthHttpClientError)?;
            let mut builder = http::Response::builder()
                .status(resp.status())
                .version(resp.version());
            for (name, value) in resp.headers() {
                builder = builder.header(name, value);
            }
            let mut body = Vec::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| Box::new(e) as OAuthHttpClientError)?
            {
                if body.len() + chunk.len() > MAX_OAUTH_BODY_BYTES {
                    return Err(
                        Box::new(NetError::TooLarge(MAX_OAUTH_BODY_BYTES)) as OAuthHttpClientError
                    );
                }
                body.extend_from_slice(&chunk);
            }
            builder
                .body(body)
                .map_err(|e| Box::new(e) as OAuthHttpClientError)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strict(raw: &str) -> Result<Url, NetError> {
        NetPolicy::strict().parse_and_check(raw)
    }

    #[test]
    fn https_only() {
        assert!(strict("https://mcp.example.com/mcp").is_ok());
        assert!(matches!(
            strict("http://mcp.example.com/mcp"),
            Err(NetError::NotHttps(_))
        ));
        assert!(matches!(
            strict("ftp://example.com"),
            Err(NetError::NotHttps(_))
        ));
        assert!(matches!(
            strict("https://user:pw@example.com/"),
            Err(NetError::Credentials)
        ));
    }

    #[test]
    fn private_literals_and_localhost_are_blocked() {
        for raw in [
            "https://10.0.0.5/",
            "https://172.16.3.4/",
            "https://192.168.1.10/",
            "https://169.254.169.254/latest/meta-data",
            "https://127.0.0.1/",
            "https://0.0.0.0/",
            "https://100.64.1.1/",
            "https://[::1]/",
            "https://[fd00::1]/",
            "https://[fe80::1]/",
            "https://[::ffff:10.0.0.1]/",
            "https://[64:ff9b::a9fe:a9fe]/",
            "https://localhost/",
            "https://api.localhost/",
        ] {
            assert!(
                matches!(strict(raw), Err(NetError::PrivateAddress(_))),
                "{raw} should be blocked"
            );
        }
        assert!(strict("https://8.8.8.8/").is_ok());
        assert!(strict("https://[2606:4700:4700::1111]/").is_ok());
    }

    #[test]
    fn test_policy_allows_only_loopback_http() {
        let p = NetPolicy::local_test();
        assert!(p.parse_and_check("http://127.0.0.1:8080/mcp").is_ok());
        assert!(p.parse_and_check("http://[::1]:8080/mcp").is_ok());
        assert!(p.parse_and_check("http://example.com/").is_err());
        assert!(matches!(
            p.parse_and_check("http://10.0.0.1/"),
            Err(NetError::NotHttps(_))
        ));
        assert!(matches!(
            p.parse_and_check("https://169.254.169.254/"),
            Err(NetError::PrivateAddress(_))
        ));
    }

    #[test]
    fn resolver_filter_drops_private_answers() {
        let addrs: Vec<SocketAddr> = vec![
            "10.1.2.3:443".parse().unwrap(),
            "93.184.216.34:443".parse().unwrap(),
            "[::1]:443".parse().unwrap(),
        ];
        let kept = NetPolicy::strict().filter_addrs(addrs.clone());
        assert_eq!(
            kept,
            vec!["93.184.216.34:443".parse::<SocketAddr>().unwrap()]
        );
        let only_private: Vec<SocketAddr> = vec!["192.168.0.2:443".parse().unwrap()];
        assert!(NetPolicy::strict().filter_addrs(only_private).is_empty());
    }

    #[tokio::test]
    async fn resolver_refuses_names_that_resolve_to_loopback() {
        use reqwest::dns::Resolve;
        // "localhost" resolves without network access.
        let r = GuardedResolver::new(NetPolicy::strict());
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        let err = match r.resolve(name).await {
            Ok(_) => panic!("localhost must not resolve"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("private"), "{err}");
    }
}
