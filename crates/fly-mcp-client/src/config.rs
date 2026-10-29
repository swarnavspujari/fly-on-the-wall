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
/// Longest web address; the approval dialog shows it in full.
pub const MAX_URL_CHARS: usize = 512;
/// Longest program line (program + arguments); shown in full.
pub const MAX_RUN_LINE_CHARS: usize = 1000;
/// Longest non-secret setting or header value; shown in full.
pub const MAX_SHOWN_VALUE_CHARS: usize = 200;

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

/// Variables a local connector inherits from FOTW's own environment
/// (everything else is cleared before it starts).
pub const INHERITED_ENV: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "LANG",
    "LC_ALL",
    "TZ",
    "USER",
    "LOGNAME",
    "XDG_RUNTIME_DIR",
];

/// Variables that change how a program loads code, where it finds its
/// config, where its runtime fetches packages from, how it reaches the
/// network or which certificates it trusts. A local connector can't set
/// them, as a plain or a secret setting (secret values are never shown).
///
/// A list of names to refuse rather than a list to allow: servers name their
/// own settings freely (`NOTION_TOKEN`, `GITHUB_PERSONAL_ACCESS_TOKEN`…), so
/// an allowlist would refuse ordinary connectors.
const RESERVED_ENV: &[&str] = &[
    "RUBYOPT",
    "RUBYLIB",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "CLASSPATH",
    "SSLKEYLOGFILE",
    "REQUESTS_CA_BUNDLE",
    "SHELL",
    "BASH_ENV",
    "ENV",
    "ZDOTDIR",
    "PSMODULEPATH",
    "HOMEDRIVE",
    "HOMEPATH",
    "HOMESHARE",
    "GOFLAGS",
    "GOPATH",
    "GOROOT",
    "GOENV",
    "GOBIN",
    "GOCACHE",
    "GOMODCACHE",
    "GOSUMDB",
    "GONOSUMDB",
    "GONOSUMCHECK",
    "GOPRIVATE",
    "GOINSECURE",
    "GOTOOLCHAIN",
    "GOEXPERIMENT",
];

/// Prefixes of the same kinds of variables.
const RESERVED_ENV_PREFIXES: &[&str] = &[
    "LD_",
    "DYLD_",
    "PYTHON",
    "PERL",
    "NODE_",
    "NPM_",
    "YARN_",
    "PNPM_",
    "BUN_",
    "DENO_",
    "PIP_",
    "UV_",
    "GEM_",
    "BUNDLE_",
    "CARGO_",
    "RUSTUP_",
    "GIT_",
    "XDG_",
    "ELECTRON_",
    "DOTNET_",
    "COMPLUS_",
    "CORECLR_",
    "COR_PROFILER",
    "SSL_",
    "OPENSSL_",
    "CURL_",
];

/// Whether a local connector may not set `name` (compared without case).
pub fn is_reserved_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    INHERITED_ENV.contains(&upper.as_str())
        || RESERVED_ENV.contains(&upper.as_str())
        || RESERVED_ENV_PREFIXES.iter().any(|p| upper.starts_with(p))
        || upper.ends_with("PROXY")
}

/// Text a person couldn't see in full in a dialog.
fn hidden(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_control() || crate::sanitize::is_hidden_char(c))
}

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
        if hidden(&self.name) {
            return Err(bad("The name can't contain hidden or control characters."));
        }
        match &self.transport {
            TransportConfig::Http { url, headers } => {
                if url.chars().count() > MAX_URL_CHARS {
                    return Err(bad("The address is too long."));
                }
                if hidden(url) {
                    return Err(bad("The address can't contain hidden characters."));
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
                        check_shown_value(v, &h.name)?;
                    }
                }
            }
            TransportConfig::Stdio { command, args, env } => {
                if command.is_empty() || command.chars().count() > MAX_COMMAND_CHARS {
                    return Err(bad("Choose the program to run."));
                }
                if hidden(command) {
                    return Err(bad(
                        "The program can't contain hidden or control characters.",
                    ));
                }
                let has_dir = command.contains(['/', '\\']);
                if has_dir && !std::path::Path::new(command).is_absolute() {
                    return Err(bad(
                        "Choose the program with Choose a file…, or use npx or uvx.",
                    ));
                }
                if args.len() > MAX_ARGS {
                    return Err(bad(format!("At most {MAX_ARGS} arguments.")));
                }
                for a in args {
                    if a.chars().count() > MAX_ARG_CHARS || hidden(a) {
                        return Err(bad(
                            "An argument is too long or contains hidden or control characters.",
                        ));
                    }
                }
                if self.will_run().unwrap_or_default().chars().count() > MAX_RUN_LINE_CHARS {
                    return Err(bad(format!(
                        "The program and its arguments must fit in {MAX_RUN_LINE_CHARS} characters, \
                         so the approval can show all of it."
                    )));
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
                    if is_reserved_env(&e.name) {
                        return Err(bad(format!("{} can't be set for a connector.", e.name)));
                    }
                    if !seen.insert(upper) {
                        return Err(bad(format!("{} is listed twice.", e.name)));
                    }
                    if let Some(v) = &e.value {
                        check_shown_value(v, &e.name)?;
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
                if id.len() > 256 || hidden(id) || id.chars().any(char::is_whitespace) {
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

    /// True when `other` points somewhere else: another address, another
    /// program line or settings (a setting can change what a runner like
    /// `npx` fetches), another transport or another kind of sign-in. Stored
    /// secrets, tokens and tool permissions must not carry over to it.
    pub fn moves_from(&self, other: &ServerConfig) -> bool {
        let identity = |c: &ServerConfig| {
            (
                match &c.transport {
                    TransportConfig::Http { url, .. } => format!("http {url}"),
                    TransportConfig::Stdio { env, .. } => format!(
                        "stdio {}\n{}",
                        c.will_run().unwrap_or_default(),
                        serde_json::to_string(env).unwrap_or_default()
                    ),
                },
                std::mem::discriminant(&c.auth),
            )
        };
        identity(self) != identity(other)
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

/// A value the approval dialog shows: short, and nothing hidden.
fn check_shown_value(v: &str, name: &str) -> Result<(), ConfigError> {
    if v.chars().count() > MAX_SHOWN_VALUE_CHARS {
        return Err(bad(format!(
            "The value for {name} is longer than {MAX_SHOWN_VALUE_CHARS} characters; make it a secret."
        )));
    }
    if hidden(v) {
        return Err(bad(format!(
            "The value for {name} has hidden characters or line breaks."
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
            "NPM_CONFIG_REGISTRY",
            "npm_config_registry",
            "PIP_INDEX_URL",
            "UV_INDEX_URL",
            "PYTHONPATH",
            "JAVA_TOOL_OPTIONS",
            "DOTNET_STARTUP_HOOKS",
            "NODE_EXTRA_CA_CERTS",
            "SSL_CERT_FILE",
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
    fn everything_the_dialog_shows_is_visible_and_whole() {
        let p = NetPolicy::strict();
        let zw = "\u{200B}";
        let tag = "\u{E0041}";
        let vs = "\u{FE0F}";
        let mut bad_name = http("https://mcp.example.com/mcp");
        bad_name.name = format!("Docs{zw}");
        assert!(bad_name.validate(&p).is_err());
        assert!(http(&format!("https://mcp.example.com/m{tag}cp"))
            .validate(&p)
            .is_err());
        let with_arg = |a: String| ServerConfig {
            name: "Local".into(),
            transport: TransportConfig::Stdio {
                command: "npx".into(),
                args: vec![a],
                env: vec![],
            },
            auth: AuthConfig::None,
        };
        assert!(with_arg(format!("-y{vs}")).validate(&p).is_err());
        assert!(
            with_arg("x".repeat(MAX_RUN_LINE_CHARS))
                .validate(&p)
                .is_err(),
            "too long to show"
        );
        let with_value = |v: String| {
            local(vec![EnvConfig {
                name: "REGION".into(),
                value: Some(v),
            }])
        };
        assert!(with_value(format!("eu{zw}")).validate(&p).is_err());
        assert!(with_value("e".repeat(MAX_SHOWN_VALUE_CHARS + 1))
            .validate(&p)
            .is_err());
        assert!(with_value("eu".into()).validate(&p).is_ok());
        // Fake line breaks and other format or ignorable characters, by
        // category: in the name, address, program, arguments and values.
        for c in [
            '\u{2028}',
            '\u{2029}',
            '\u{180B}',
            '\u{FFF9}',
            '\u{1D173}',
            '\u{1BCA0}',
        ] {
            let mut named = http("https://mcp.example.com/mcp");
            named.name = format!("Docs{c}");
            assert!(named.validate(&p).is_err(), "name U+{:04X}", c as u32);
            assert!(http(&format!("https://mcp.example.com/m{c}cp"))
                .validate(&p)
                .is_err());
            assert!(with_arg(format!("--dir{c}x")).validate(&p).is_err());
            assert!(with_value(format!("eu{c}")).validate(&p).is_err());
            let mut prog = with_arg("x".into());
            if let TransportConfig::Stdio { command, .. } = &mut prog.transport {
                *command = format!("/opt/srv{c}/bin");
            }
            assert!(prog.validate(&p).is_err(), "program U+{:04X}", c as u32);
        }
        assert!(http(&format!(
            "https://mcp.example.com/{}",
            "a".repeat(MAX_URL_CHARS)
        ))
        .validate(&p)
        .is_err());
    }

    #[test]
    fn inherited_and_config_variables_are_reserved() {
        let p = NetPolicy::strict();
        for name in [
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
            "XDG_CONFIG_HOME",
            "NODE_ENV",
            "GIT_SSH_COMMAND",
            "HTTPS_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "GOPROXY",
            "GOFLAGS",
            "SSLKEYLOGFILE",
            "OPENSSL_CONF",
            "COMPlus_EnableDiagnostics",
            "DOTNET_gcServer",
            "PERLLIB",
            "GEM_HOME",
            "BUNDLE_GEMFILE",
            "HOMEPATH",
            "HOMEDRIVE",
            "ELECTRON_RUN_AS_NODE",
            "NPM_TOKEN",
            "CARGO_HOME",
        ] {
            let cfg = local(vec![EnvConfig {
                name: name.into(),
                value: None,
            }]);
            assert!(cfg.validate(&p).is_err(), "{name} (secret) must be refused");
        }
        assert!(local(vec![EnvConfig {
            name: "ACME_TOKEN".into(),
            value: None
        }])
        .validate(&p)
        .is_ok());
    }

    #[test]
    fn relative_program_paths_are_refused() {
        let p = NetPolicy::strict();
        for cmd in ["./server", "bin/server", "..\\tools\\srv.exe"] {
            let mut cfg = local(vec![]);
            cfg.transport = TransportConfig::Stdio {
                command: cmd.into(),
                args: vec![],
                env: vec![],
            };
            assert!(cfg.validate(&p).is_err(), "{cmd}");
        }
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
    fn a_new_address_program_or_sign_in_is_a_move() {
        let a = http("https://mcp.example.com/mcp");
        let mut renamed = a.clone();
        renamed.name = "Docs 2".into();
        assert!(!renamed.moves_from(&a));
        assert!(http("https://mcp.example.com/other").moves_from(&a));
        let mut keyed = a.clone();
        keyed.auth = AuthConfig::ApiKey;
        assert!(keyed.moves_from(&a));
        let l = local(vec![]);
        let mut other_pkg = l.clone();
        other_pkg.transport = TransportConfig::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "@example/other-server".into()],
            env: vec![],
        };
        assert!(other_pkg.moves_from(&l), "same runner, different package");
        let mut with_setting = l.clone();
        with_setting.transport = TransportConfig::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "@example/server".into()],
            env: vec![EnvConfig {
                name: "REGION".into(),
                value: Some("eu".into()),
            }],
        };
        assert!(
            with_setting.moves_from(&l),
            "a setting can change what the runner fetches"
        );
        let mut renamed = l.clone();
        renamed.name = "Local 2".into();
        assert!(!renamed.moves_from(&l));
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
