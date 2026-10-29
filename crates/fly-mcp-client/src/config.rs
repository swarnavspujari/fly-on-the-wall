//! A connector's configuration and the hash its approval is bound to.
//!
//! The user approves one exact configuration (name, transport, program or
//! address, arguments, header and variable names, non-secret values, auth).
//! The approval stores [`ServerConfig::hash`]; any change to the
//! configuration changes the hash and voids the approval. Secret values
//! (API keys, secret headers, secret variables) live in the keychain, never
//! in the configuration.

use serde::{Deserialize, Serialize};

use crate::canonical::domain_hash;
use crate::netguard::NetPolicy;

pub const MAX_NAME_CHARS: usize = 40;
pub const MAX_COMMAND_CHARS: usize = 1024;
pub const MAX_ARGS: usize = 32;
pub const MAX_ARG_CHARS: usize = 1024;
pub const MAX_ENV: usize = 16;
pub const MAX_HEADERS: usize = 16;
pub const MAX_VALUE_CHARS: usize = 4096;
pub const MAX_SCOPES: usize = 16;
pub const MAX_URL_CHARS: usize = 2048;

const CONFIG_HASH_DOMAIN: &str = "fotw.connector.config.v2";

/// A request header for a web connector. `value: None` means secret: the
/// value is in the keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderConfig {
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// An environment variable for a local connector. `value: None` means
/// secret: the value is in the keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvConfig {
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// How FOTW reaches the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TransportConfig {
    /// Streamable HTTP. https only.
    Http {
        url: String,
        #[serde(default)]
        headers: Vec<HeaderConfig>,
    },
    /// A local program speaking MCP on stdin/stdout, started without a shell
    /// and with a cleared environment plus `env`.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<EnvConfig>,
    },
}

/// How FOTW authenticates to a web connector.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AuthConfig {
    #[default]
    None,
    /// OAuth 2.1 with PKCE. The user signs in in their own browser. Without a
    /// `client_id`, FOTW registers itself (dynamic client registration).
    #[serde(rename = "oauth")]
    OAuth {
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default)]
        client_id: Option<String>,
    },
    /// An API key or token, sent as `Authorization: Bearer <key>`; the key is
    /// in the keychain.
    ApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub name: String,
    pub transport: TransportConfig,
    #[serde(default)]
    pub auth: AuthConfig,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct ConfigError(pub String);

fn bad(msg: impl Into<String>) -> ConfigError {
    ConfigError(msg.into())
}

/// Variables that change how a program loads code. A local connector can't
/// set them.
const RESERVED_ENV: &[&str] = &[
    "PATH",
    "PATHEXT",
    "NODE_OPTIONS",
    "NODE_PATH",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PYTHONHOME",
    "RUBYOPT",
    "PERL5OPT",
    "COMSPEC",
    "SYSTEMROOT",
];

/// Headers FOTW sets itself or that change how the request is framed.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
    "proxy-authorization",
    "authorization",
    "accept",
    "mcp-session-id",
    "mcp-protocol-version",
    "last-event-id",
];

impl ServerConfig {
    /// Trim user-entered text. Validation runs on the result.
    pub fn normalized(mut self) -> Self {
        self.name = self.name.trim().to_string();
        match &mut self.transport {
            TransportConfig::Http { url, headers } => {
                *url = url.trim().to_string();
                for h in headers.iter_mut() {
                    h.name = h.name.trim().to_string();
                }
                headers.retain(|h| !h.name.is_empty());
            }
            TransportConfig::Stdio { command, args, env } => {
                *command = command.trim().to_string();
                args.retain(|a| !a.is_empty());
                for e in env.iter_mut() {
                    e.name = e.name.trim().to_string();
                }
                env.retain(|e| !e.name.is_empty());
            }
        }
        if let AuthConfig::OAuth { scopes, client_id } = &mut self.auth {
            scopes.iter_mut().for_each(|s| *s = s.trim().to_string());
            scopes.retain(|s| !s.is_empty());
            if let Some(id) = client_id {
                *id = id.trim().to_string();
            }
            if client_id.as_deref() == Some("") {
                *client_id = None;
            }
        }
        self
    }

    pub fn validate(&self, policy: &NetPolicy) -> Result<(), ConfigError> {
        let n = self.name.chars().count();
        if n == 0 || n > MAX_NAME_CHARS {
            return Err(bad(format!(
                "The name must be 1–{MAX_NAME_CHARS} characters."
            )));
        }
        if self.name.chars().any(char::is_control) {
            return Err(bad("The name can't contain control characters."));
        }
        match &self.transport {
            TransportConfig::Http { url, headers } => {
                if url.len() > MAX_URL_CHARS {
                    return Err(bad("The address is too long."));
                }
                let parsed = policy.parse_and_check(url).map_err(|e| match e {
                    crate::netguard::NetError::NotHttps(_)
                    | crate::netguard::NetError::BadUrl(_) => {
                        bad("Use an address that starts with https://")
                    }
                    other => bad(format!("Web address: {other}.")),
                })?;
                if parsed.fragment().is_some() {
                    return Err(bad("The address can't contain a #fragment."));
                }
                if headers.len() > MAX_HEADERS {
                    return Err(bad(format!("At most {MAX_HEADERS} headers.")));
                }
                let mut seen = std::collections::HashSet::new();
                for h in headers {
                    let lower = h.name.to_ascii_lowercase();
                    if !valid_header_name(&h.name) {
                        return Err(bad(format!("{:?} isn't a valid header name.", h.name)));
                    }
                    if RESERVED_HEADERS.contains(&lower.as_str()) {
                        return Err(bad(format!(
                            "{} is set by Fly on the Wall; use the sign-in options instead.",
                            h.name
                        )));
                    }
                    if !seen.insert(lower) {
                        return Err(bad(format!("{} is listed twice.", h.name)));
                    }
                    if let Some(v) = &h.value {
                        check_value(v, &h.name)?;
                    }
                }
            }
            TransportConfig::Stdio { command, args, env } => {
                if command.is_empty() || command.chars().count() > MAX_COMMAND_CHARS {
                    return Err(bad("Choose the program to run."));
                }
                if command.chars().any(char::is_control) {
                    return Err(bad("The program can't contain control characters."));
                }
                if args.len() > MAX_ARGS {
                    return Err(bad(format!("At most {MAX_ARGS} arguments.")));
                }
                for a in args {
                    if a.chars().count() > MAX_ARG_CHARS || a.chars().any(char::is_control) {
                        return Err(bad(
                            "An argument is too long or contains control characters.",
                        ));
                    }
                }
                if env.len() > MAX_ENV {
                    return Err(bad(format!("At most {MAX_ENV} settings.")));
                }
                let mut seen = std::collections::HashSet::new();
                for e in env {
                    if !valid_env_name(&e.name) {
                        return Err(bad(format!("{:?} isn't a valid setting name.", e.name)));
                    }
                    let upper = e.name.to_ascii_uppercase();
                    if RESERVED_ENV.contains(&upper.as_str())
                        || upper.starts_with("LD_")
                        || upper.starts_with("DYLD_")
                    {
                        return Err(bad(format!("{} can't be set for a connector.", e.name)));
                    }
                    if !seen.insert(upper) {
                        return Err(bad(format!("{} is listed twice.", e.name)));
                    }
                    if let Some(v) = &e.value {
                        check_value(v, &e.name)?;
                    }
                }
                if !matches!(self.auth, AuthConfig::None) {
                    return Err(bad(
                        "Sign-in options are only for web connectors; use a secret setting.",
                    ));
                }
            }
        }
        if let AuthConfig::OAuth { scopes, client_id } = &self.auth {
            if scopes.len() > MAX_SCOPES {
                return Err(bad(format!("At most {MAX_SCOPES} scopes.")));
            }
            for s in scopes {
                // RFC 6749 §3.3 scope-token.
                let ok = !s.is_empty()
                    && s.len() <= 128
                    && s.bytes().all(|b| {
                        b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b)
                    });
                if !ok {
                    return Err(bad(format!("{s:?} isn't a valid scope.")));
                }
            }
            if let Some(id) = client_id {
                if id.len() > 256 || id.chars().any(|c| c.is_control() || c.is_whitespace()) {
                    return Err(bad("The OAuth client ID isn't valid."));
                }
            }
        }
        Ok(())
    }

    /// The hash an approval is bound to. Secret values aren't part of it
    /// (they aren't in the configuration); their names are.
    pub fn hash(&self) -> String {
        let v = serde_json::to_value(self).expect("ServerConfig serializes");
        domain_hash(CONFIG_HASH_DOMAIN, &v)
    }

    /// Where the connector sends data: the host of a web connector, or
    /// "this computer" for a local one.
    pub fn destination(&self) -> String {
        match &self.transport {
            TransportConfig::Http { url, .. } => url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| url.clone()),
            TransportConfig::Stdio { .. } => "this computer".into(),
        }
    }

    /// "npx -y @org/server" for a local connector (no variables).
    pub fn will_run(&self) -> Option<String> {
        match &self.transport {
            TransportConfig::Stdio { command, args, .. } => {
                let quote = |s: &str| {
                    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c == '"') {
                        format!("\"{}\"", s.replace('"', "\\\""))
                    } else {
                        s.to_string()
                    }
                };
                let mut parts = vec![quote(command)];
                parts.extend(args.iter().map(|a| quote(a)));
                Some(parts.join(" "))
            }
            TransportConfig::Http { .. } => None,
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self.transport, TransportConfig::Stdio { .. })
    }

    pub fn uses_oauth(&self) -> bool {
        matches!(self.auth, AuthConfig::OAuth { .. })
    }

    /// Names of the secret headers (web) or secret variables (local).
    pub fn secret_names(&self) -> Vec<String> {
        match &self.transport {
            TransportConfig::Http { headers, .. } => headers
                .iter()
                .filter(|h| h.value.is_none())
                .map(|h| h.name.clone())
                .collect(),
            TransportConfig::Stdio { env, .. } => env
                .iter()
                .filter(|e| e.value.is_none())
                .map(|e| e.name.clone())
                .collect(),
        }
    }
}

fn check_value(v: &str, name: &str) -> Result<(), ConfigError> {
    if v.chars().count() > MAX_VALUE_CHARS || v.contains(['\0', '\r', '\n']) {
        return Err(bad(format!(
            "The value for {name} is too long or has line breaks."
        )));
    }
    Ok(())
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// RFC 7230 token.
fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// A short identifier for tool names: lowercase ASCII letters, digits and
/// `_`, at most 16 characters. Uniqueness is the registry's job.
pub fn slug_for(name: &str) -> String {
    let mut out = String::new();
    let mut last_us = true;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_us = false;
        } else if !last_us {
            out.push('_');
            last_us = true;
        }
        if out.len() >= 16 {
            break;
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "server".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(url: &str) -> ServerConfig {
        ServerConfig {
            name: "Docs".into(),
            transport: TransportConfig::Http {
                url: url.into(),
                headers: vec![],
            },
            auth: AuthConfig::None,
        }
    }

    fn local(env: Vec<EnvConfig>) -> ServerConfig {
        ServerConfig {
            name: "Local".into(),
            transport: TransportConfig::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "@example/server".into()],
                env,
            },
            auth: AuthConfig::None,
        }
    }

    #[test]
    fn hash_changes_with_any_field_but_not_secret_values() {
        let a = http("https://mcp.example.com/mcp");
        assert_eq!(a.hash(), a.clone().hash());
        let mut b = a.clone();
        b.transport = TransportConfig::Http {
            url: "https://mcp.example.com/mcp2".into(),
            headers: vec![],
        };
        assert_ne!(a.hash(), b.hash());
        let mut c = a.clone();
        c.auth = AuthConfig::ApiKey;
        assert_ne!(a.hash(), c.hash());
        let mut d = a.clone();
        d.transport = TransportConfig::Http {
            url: "https://mcp.example.com/mcp".into(),
            headers: vec![HeaderConfig {
                name: "X-Team".into(),
                value: None,
            }],
        };
        assert_ne!(a.hash(), d.hash(), "a new secret header's name counts");
        let shown = local(vec![EnvConfig {
            name: "REGION".into(),
            value: Some("eu".into()),
        }]);
        let changed = local(vec![EnvConfig {
            name: "REGION".into(),
            value: Some("us".into()),
        }]);
        assert_ne!(shown.hash(), changed.hash(), "non-secret values count");
    }

    #[test]
    fn web_connectors_must_be_https_and_public() {
        let p = NetPolicy::strict();
        assert!(http("https://mcp.example.com/mcp").validate(&p).is_ok());
        assert_eq!(
            http("http://mcp.example.com/mcp")
                .validate(&p)
                .unwrap_err()
                .0,
            "Use an address that starts with https://"
        );
        assert!(http("https://192.168.1.4/mcp").validate(&p).is_err());
        assert!(http("https://mcp.example.com/mcp#x").validate(&p).is_err());
    }

    #[test]
    fn header_rules() {
        let p = NetPolicy::strict();
        let with = |name: &str, value: Option<&str>| ServerConfig {
            name: "Docs".into(),
            transport: TransportConfig::Http {
                url: "https://mcp.example.com/mcp".into(),
                headers: vec![HeaderConfig {
                    name: name.into(),
                    value: value.map(str::to_string),
                }],
            },
            auth: AuthConfig::None,
        };
        assert!(with("X-Api-Key", None).validate(&p).is_ok());
        assert!(with("X-Region", Some("eu")).validate(&p).is_ok());
        for bad_name in [
            "Authorization",
            "host",
            "Content-Length",
            "Mcp-Session-Id",
            "a b",
        ] {
            assert!(with(bad_name, None).validate(&p).is_err(), "{bad_name}");
        }
        assert!(with("X-A", Some("a\r\nInjected: 1")).validate(&p).is_err());
    }

    #[test]
    fn local_rules() {
        let p = NetPolicy::strict();
        assert!(local(vec![EnvConfig {
            name: "API_TOKEN".into(),
            value: None
        }])
        .validate(&p)
        .is_ok());
        for bad_name in [
            "PATH",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "node_options",
            "1X",
            "A-B",
        ] {
            let cfg = local(vec![EnvConfig {
                name: bad_name.into(),
                value: None,
            }]);
            assert!(cfg.validate(&p).is_err(), "{bad_name}");
        }
        let mut cfg = local(vec![]);
        cfg.transport = TransportConfig::Stdio {
            command: "srv".into(),
            args: vec!["a\nb".into()],
            env: vec![],
        };
        assert!(cfg.validate(&p).is_err());
        let mut cfg = local(vec![]);
        cfg.auth = AuthConfig::ApiKey;
        assert!(cfg.validate(&p).is_err(), "sign-in options are web-only");
    }

    #[test]
    fn scopes_are_scope_tokens() {
        let p = NetPolicy::strict();
        let mut cfg = http("https://mcp.example.com/mcp");
        cfg.auth = AuthConfig::OAuth {
            scopes: vec!["read:contacts".into()],
            client_id: None,
        };
        assert!(cfg.validate(&p).is_ok());
        cfg.auth = AuthConfig::OAuth {
            scopes: vec!["a b".into()],
            client_id: None,
        };
        assert!(cfg.validate(&p).is_err());
    }

    #[test]
    fn slugs() {
        assert_eq!(slug_for("HubSpot CRM"), "hubspot_crm");
        assert_eq!(slug_for("  ***  "), "server");
        assert_eq!(
            slug_for("A very long connector name here"),
            "a_very_long_conn"
        );
    }

    #[test]
    fn previews() {
        assert_eq!(
            http("https://mcp.example.com/mcp").destination(),
            "mcp.example.com"
        );
        let cfg = local(vec![]);
        assert_eq!(cfg.destination(), "this computer");
        assert_eq!(cfg.will_run().unwrap(), "npx -y @example/server");
        let mut spaced = local(vec![]);
        spaced.transport = TransportConfig::Stdio {
            command: "C:/Program Files/srv.exe".into(),
            args: vec!["--root".into(), "a b".into()],
            env: vec![],
        };
        assert_eq!(
            spaced.will_run().unwrap(),
            "\"C:/Program Files/srv.exe\" --root \"a b\""
        );
    }
}
