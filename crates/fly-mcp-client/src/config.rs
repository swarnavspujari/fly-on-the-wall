//! A connector's configuration and the hash its approval is bound to.
//!
//! The user approves one exact configuration (name, transport, command or
//! URL, auth). The approval stores [`ServerConfig::hash`]; any change to the
//! configuration changes the hash and voids the approval.

use serde::{Deserialize, Serialize};

use crate::canonical::domain_hash;
use crate::netguard::NetPolicy;

pub const MAX_NAME_CHARS: usize = 40;
pub const MAX_COMMAND_CHARS: usize = 1024;
pub const MAX_ARGS: usize = 32;
pub const MAX_ARG_CHARS: usize = 1024;
pub const MAX_ENV_NAMES: usize = 16;
pub const MAX_SCOPES: usize = 16;
pub const MAX_URL_CHARS: usize = 2048;

const CONFIG_HASH_DOMAIN: &str = "fotw.connector.config.v1";

/// How FOTW reaches the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TransportConfig {
    /// Streamable HTTP. https only.
    Http { url: String },
    /// A local program speaking MCP on stdin/stdout. It starts with a clean
    /// environment; `env_names` are variables whose values the user stores in
    /// the OS keychain (values are never part of the config).
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env_names: Vec<String>,
    },
}

/// How FOTW authenticates to an HTTP server.
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

/// Variables that change how a program loads code. A stdio connector can't
/// declare them (the user can still run a wrapper script if they must).
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

impl ServerConfig {
    /// Trim user-entered text. Validation runs on the result.
    pub fn normalized(mut self) -> Self {
        self.name = self.name.trim().to_string();
        match &mut self.transport {
            TransportConfig::Http { url } => *url = url.trim().to_string(),
            TransportConfig::Stdio {
                command, env_names, ..
            } => {
                *command = command.trim().to_string();
                for n in env_names.iter_mut() {
                    *n = n.trim().to_string();
                }
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
            TransportConfig::Http { url } => {
                if url.len() > MAX_URL_CHARS {
                    return Err(bad("The address is too long."));
                }
                let parsed = policy
                    .parse_and_check(url)
                    .map_err(|e| bad(format!("Server address: {e}.")))?;
                if parsed.fragment().is_some() {
                    return Err(bad("The address can't contain a #fragment."));
                }
            }
            TransportConfig::Stdio {
                command,
                args,
                env_names,
            } => {
                if command.is_empty() || command.chars().count() > MAX_COMMAND_CHARS {
                    return Err(bad("Enter the program to run."));
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
                if env_names.len() > MAX_ENV_NAMES {
                    return Err(bad(format!(
                        "At most {MAX_ENV_NAMES} environment variables."
                    )));
                }
                let mut seen = std::collections::HashSet::new();
                for name in env_names {
                    if !valid_env_name(name) {
                        return Err(bad(format!(
                            "{name:?} isn't a valid environment variable name."
                        )));
                    }
                    let upper = name.to_ascii_uppercase();
                    if RESERVED_ENV.contains(&upper.as_str())
                        || upper.starts_with("LD_")
                        || upper.starts_with("DYLD_")
                    {
                        return Err(bad(format!("{name} can't be set for a connector.")));
                    }
                    if !seen.insert(upper) {
                        return Err(bad(format!("{name} is listed twice.")));
                    }
                }
                if !matches!(self.auth, AuthConfig::None) {
                    return Err(bad("Sign-in is only available for web (https) connectors."));
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
                    return Err(bad("The client ID isn't valid."));
                }
            }
        }
        Ok(())
    }

    /// The hash an approval is bound to.
    pub fn hash(&self) -> String {
        let v = serde_json::to_value(self).expect("ServerConfig serializes");
        domain_hash(CONFIG_HASH_DOMAIN, &v)
    }

    /// Where the connector sends data, for dialogs and the call log: the
    /// host of an HTTP server, or the program of a stdio one.
    pub fn destination(&self) -> String {
        match &self.transport {
            TransportConfig::Http { url } => url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| url.clone()),
            TransportConfig::Stdio { command, .. } => {
                let file = command
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(command.as_str());
                format!("local program {file}")
            }
        }
    }

    pub fn uses_oauth(&self) -> bool {
        matches!(self.auth, AuthConfig::OAuth { .. })
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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
            transport: TransportConfig::Http { url: url.into() },
            auth: AuthConfig::None,
        }
    }

    #[test]
    fn hash_changes_with_any_field() {
        let a = http("https://mcp.example.com/mcp");
        let mut b = a.clone();
        assert_eq!(a.hash(), b.hash());
        b.transport = TransportConfig::Http {
            url: "https://mcp.example.com/mcp2".into(),
        };
        assert_ne!(a.hash(), b.hash());
        let mut c = a.clone();
        c.auth = AuthConfig::OAuth {
            scopes: vec![],
            client_id: None,
        };
        assert_ne!(a.hash(), c.hash());
        let mut d = a.clone();
        d.name = "Docs 2".into();
        assert_ne!(a.hash(), d.hash());
    }

    #[test]
    fn http_must_be_https_and_public() {
        let p = NetPolicy::strict();
        assert!(http("https://mcp.example.com/mcp").validate(&p).is_ok());
        assert!(http("http://mcp.example.com/mcp").validate(&p).is_err());
        assert!(http("https://192.168.1.4/mcp").validate(&p).is_err());
        assert!(http("https://mcp.example.com/mcp#x").validate(&p).is_err());
    }

    #[test]
    fn stdio_rules() {
        let p = NetPolicy::strict();
        let mut cfg = ServerConfig {
            name: "Local".into(),
            transport: TransportConfig::Stdio {
                command: "C:/Tools/server.exe".into(),
                args: vec!["--stdio".into()],
                env_names: vec!["API_TOKEN".into()],
            },
            auth: AuthConfig::None,
        };
        assert!(cfg.validate(&p).is_ok());
        for bad_name in [
            "PATH",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "node_options",
            "1X",
            "A-B",
        ] {
            cfg.transport = TransportConfig::Stdio {
                command: "srv".into(),
                args: vec![],
                env_names: vec![bad_name.into()],
            };
            assert!(cfg.validate(&p).is_err(), "{bad_name}");
        }
        cfg.transport = TransportConfig::Stdio {
            command: "srv".into(),
            args: vec!["a\nb".into()],
            env_names: vec![],
        };
        assert!(cfg.validate(&p).is_err());
        cfg.transport = TransportConfig::Stdio {
            command: "srv".into(),
            args: vec![],
            env_names: vec![],
        };
        cfg.auth = AuthConfig::OAuth {
            scopes: vec![],
            client_id: None,
        };
        assert!(cfg.validate(&p).is_err(), "OAuth is http-only");
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
        assert_eq!(slug_for("Notion"), "notion");
    }

    #[test]
    fn destination_names_host_or_program() {
        assert_eq!(
            http("https://mcp.example.com/mcp").destination(),
            "mcp.example.com"
        );
        let cfg = ServerConfig {
            name: "x".into(),
            transport: TransportConfig::Stdio {
                command: "C:\\Tools\\srv.exe".into(),
                args: vec![],
                env_names: vec![],
            },
            auth: AuthConfig::None,
        };
        assert_eq!(cfg.destination(), "local program srv.exe");
    }
}
