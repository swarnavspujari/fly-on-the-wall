//! Read (never write) a Claude Desktop `claude_desktop_config.json` and turn
//! its servers into connector forms.
//!
//! Environment and header values become secrets bound for the keychain;
//! they're never handed to the webview (candidates carry names only).
//! Permissions aren't imported: every tool starts on Needs approval after
//! the user approves the connector in a native dialog.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::config::{
    AuthConfig, EnvConfig, HeaderConfig, ServerConfig, TransportConfig, MAX_NAME_CHARS,
};
use crate::hub::NewSecrets;

/// Largest config file read, in bytes.
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// One server found in the file, without secret values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    pub key: String,
    pub name: String,
    /// "local" or "web".
    pub kind: &'static str,
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Names only; the values move to the keychain on import.
    pub env_names: Vec<String>,
    pub url: Option<String>,
    pub header_names: Vec<String>,
}

fn servers(json: &str) -> Vec<(String, Value)> {
    if json.len() > MAX_CONFIG_BYTES {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(map) = v.get("mcpServers").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Value)> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn pairs(v: Option<&Value>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = v
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Fly on the Wall's own MCP server, which makes no sense as a connector.
fn is_ourselves(command: &str) -> bool {
    let file = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    file.starts_with("flyonthewall-mcp")
}

fn display_name(key: &str) -> String {
    let n: String = key
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    if n.is_empty() {
        "Imported connector".into()
    } else {
        n
    }
}

/// Servers in the file, with secret values left out.
pub fn candidates(json: &str) -> Vec<ImportCandidate> {
    servers(json)
        .into_iter()
        .filter_map(|(key, v)| {
            if let Some(url) = v.get("url").and_then(Value::as_str) {
                return Some(ImportCandidate {
                    name: display_name(&key),
                    key,
                    kind: "web",
                    command: None,
                    args: Vec::new(),
                    env_names: Vec::new(),
                    url: Some(url.to_string()),
                    header_names: pairs(v.get("headers"))
                        .into_iter()
                        .map(|(k, _)| k)
                        .collect(),
                });
            }
            let command = v.get("command").and_then(Value::as_str)?;
            if is_ourselves(command) {
                return None;
            }
            Some(ImportCandidate {
                name: display_name(&key),
                key,
                kind: "local",
                command: Some(command.to_string()),
                args: strings(v.get("args")),
                env_names: pairs(v.get("env")).into_iter().map(|(k, _)| k).collect(),
                url: None,
                header_names: Vec::new(),
            })
        })
        .collect()
}

/// The connector form for one server: every variable and header becomes a
/// secret whose value goes to the keychain.
pub fn to_config(json: &str, key: &str) -> Option<(ServerConfig, NewSecrets)> {
    let (_, v) = servers(json).into_iter().find(|(k, _)| k == key)?;
    let mut values = HashMap::new();
    let transport = if let Some(url) = v.get("url").and_then(Value::as_str) {
        let headers = pairs(v.get("headers"))
            .into_iter()
            .map(|(k, val)| {
                values.insert(k.clone(), val);
                HeaderConfig {
                    name: k,
                    value: None,
                }
            })
            .collect();
        TransportConfig::Http {
            url: url.to_string(),
            headers,
        }
    } else {
        let command = v.get("command").and_then(Value::as_str)?;
        if is_ourselves(command) {
            return None;
        }
        let env = pairs(v.get("env"))
            .into_iter()
            .map(|(k, val)| {
                values.insert(k.clone(), val);
                EnvConfig {
                    name: k,
                    value: None,
                }
            })
            .collect();
        TransportConfig::Stdio {
            command: command.to_string(),
            args: strings(v.get("args")),
            env,
        }
    };
    Some((
        ServerConfig {
            name: display_name(key),
            transport,
            auth: AuthConfig::None,
        },
        NewSecrets {
            api_key: None,
            client_secret: None,
            values,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "mcpServers": {
        "docs-search": {"command": "npx", "args": ["-y", "@example/docs-server"], "env": {"DOCS_TOKEN": "t0ken-value"}},
        "flyonthewall": {"command": "C:/Apps/Fly on the Wall/flyonthewall-mcp.exe"},
        "team-wiki": {"type": "http", "url": "https://mcp.example.com/mcp", "headers": {"X-Api-Key": "k3y"}},
        "broken": {"args": ["x"]}
      },
      "globalShortcut": "Ctrl+Space"
    }"#;

    #[test]
    fn candidates_carry_names_but_never_values() {
        let c = candidates(SAMPLE);
        let keys: Vec<&str> = c.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["docs-search", "team-wiki"],
            "ourselves and broken entries skipped"
        );
        assert_eq!(c[0].env_names, vec!["DOCS_TOKEN"]);
        assert_eq!(c[1].header_names, vec!["X-Api-Key"]);
        let shown = serde_json::to_string(&c).unwrap();
        assert!(
            !shown.contains("t0ken-value") && !shown.contains("k3y"),
            "{shown}"
        );
    }

    #[test]
    fn imported_values_become_secrets() {
        let (cfg, secrets) = to_config(SAMPLE, "docs-search").unwrap();
        assert_eq!(cfg.secret_names(), vec!["DOCS_TOKEN"]);
        assert_eq!(secrets.values["DOCS_TOKEN"], "t0ken-value");
        assert!(!serde_json::to_string(&cfg).unwrap().contains("t0ken-value"));
        let (web, s) = to_config(SAMPLE, "team-wiki").unwrap();
        assert!(matches!(web.transport, TransportConfig::Http { .. }));
        assert_eq!(s.values["X-Api-Key"], "k3y");
        assert!(to_config(SAMPLE, "flyonthewall").is_none());
        assert!(to_config("not json", "x").is_none());
    }
}
