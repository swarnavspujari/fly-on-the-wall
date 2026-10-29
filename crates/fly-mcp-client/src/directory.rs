//! A short, built-in list of hosted connectors the user can add in one step.
//!
//! Each address was checked to answer as an MCP server (sign-in servers
//! answer 401 with OAuth metadata). Picking one only fills in the Add form;
//! the user still sees the preview and approves in a native dialog naming
//! the host.

use serde::Serialize;

use crate::config::{AuthConfig, ServerConfig, TransportConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryAuth {
    /// Browser sign-in.
    Oauth,
    /// A key or token the user pastes (kept in the keychain).
    ApiKey,
    None,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    pub id: &'static str,
    pub name: &'static str,
    /// Who runs the server ("Made by …").
    pub vendor: &'static str,
    pub description: &'static str,
    pub url: &'static str,
    pub auth: DirectoryAuth,
    /// What to call the key, for `ApiKey` entries.
    pub key_label: Option<&'static str>,
}

impl DirectoryEntry {
    pub fn config(&self) -> ServerConfig {
        ServerConfig {
            name: self.name.into(),
            transport: TransportConfig::Http {
                url: self.url.into(),
                headers: Vec::new(),
            },
            auth: match self.auth {
                DirectoryAuth::Oauth => AuthConfig::OAuth {
                    scopes: Vec::new(),
                    client_id: None,
                },
                DirectoryAuth::ApiKey => AuthConfig::ApiKey,
                DirectoryAuth::None => AuthConfig::None,
            },
        }
    }
}

const fn entry(
    id: &'static str,
    name: &'static str,
    vendor: &'static str,
    description: &'static str,
    url: &'static str,
    auth: DirectoryAuth,
) -> DirectoryEntry {
    DirectoryEntry {
        id,
        name,
        vendor,
        description,
        url,
        auth,
        key_label: None,
    }
}

pub fn directory() -> Vec<DirectoryEntry> {
    use DirectoryAuth::*;
    vec![
        entry(
            "notion",
            "Notion",
            "Notion",
            "Search and read your Notion pages and databases.",
            "https://mcp.notion.com/mcp",
            Oauth,
        ),
        entry(
            "linear",
            "Linear",
            "Linear",
            "Find and update issues and projects in Linear.",
            "https://mcp.linear.app/mcp",
            Oauth,
        ),
        entry(
            "hubspot",
            "HubSpot",
            "HubSpot",
            "Look up contacts, companies and deals in HubSpot.",
            "https://mcp.hubspot.com/",
            Oauth,
        ),
        DirectoryEntry {
            key_label: Some("GitHub personal access token"),
            ..entry(
                "github",
                "GitHub",
                "GitHub",
                "Search repositories, issues and pull requests on GitHub.",
                "https://api.githubcopilot.com/mcp/",
                ApiKey,
            )
        },
        entry(
            "stripe",
            "Stripe",
            "Stripe",
            "Look up customers, payments and invoices in Stripe.",
            "https://mcp.stripe.com",
            Oauth,
        ),
        entry(
            "intercom",
            "Intercom",
            "Intercom",
            "Search conversations and contacts in Intercom.",
            "https://mcp.intercom.com/mcp",
            Oauth,
        ),
        entry(
            "canva",
            "Canva",
            "Canva",
            "Find and create designs in Canva.",
            "https://mcp.canva.com/mcp",
            Oauth,
        ),
        entry(
            "paypal",
            "PayPal",
            "PayPal",
            "Look up transactions and invoices in PayPal.",
            "https://mcp.paypal.com/mcp",
            Oauth,
        ),
        entry(
            "deepwiki",
            "DeepWiki",
            "Cognition",
            "Ask questions about public GitHub repositories. No sign-in.",
            "https://mcp.deepwiki.com/mcp",
            None,
        ),
        entry(
            "huggingface",
            "Hugging Face",
            "Hugging Face",
            "Search models, datasets and papers on Hugging Face. No sign-in.",
            "https://huggingface.co/mcp",
            None,
        ),
        entry(
            "cloudflare-docs",
            "Cloudflare Docs",
            "Cloudflare",
            "Search Cloudflare's developer documentation. No sign-in.",
            "https://docs.mcp.cloudflare.com/mcp",
            None,
        ),
    ]
}

pub fn find(id: &str) -> Option<DirectoryEntry> {
    directory().into_iter().find(|e| e.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netguard::NetPolicy;

    #[test]
    fn every_entry_is_a_valid_https_connector() {
        let mut ids = std::collections::HashSet::new();
        for e in directory() {
            assert!(ids.insert(e.id), "duplicate id {}", e.id);
            e.config().validate(&NetPolicy::strict()).unwrap();
            assert!(e.url.starts_with("https://"));
            assert_eq!(
                e.key_label.is_some(),
                e.auth == DirectoryAuth::ApiKey,
                "{}",
                e.id
            );
        }
        assert!(find("notion").is_some());
    }
}
