//! fly-mcp-client: connectors (beta). FOTW's Ask agent calls tools on MCP
//! servers the user connects (spec §4.3).
//!
//! FOTW holds private meeting data, reads attacker-controllable input
//! (anyone can say anything in a meeting) and, with connectors, gains an
//! outbound channel. So every connector runs under these rules:
//!
//! | Rule | Where |
//! |---|---|
//! | Approval is bound to a hash of the server config | [`config::ServerConfig::hash`], [`registry::ServerEntry::is_approved`] |
//! | Each tool definition is pinned by hash; a change disables it and keeps a diff | [`pin`] |
//! | Annotations only make a tool stricter: a tool is read-only only when the server says so (`readOnlyHint`, and not destructive); the user's permission decides whether it asks | [`pin::kind`] |
//! | Exact, editable arguments on a confirm card for every call on Needs approval (Always allow is set only in Settings, through a native dialog) | [`hub::CallApprover`] |
//! | A connector result can't lead to another proposal in the same turn | [`hub::ConnectorTurn`] |
//! | Results fenced, schema-validated and size-capped | [`fence`] |
//! | https only; private addresses blocked (OAuth discovery included) | [`netguard`], [`oauth`] |
//! | No sampling, roots or elicitation | [`session`] |
//! | Keep-local content blocks all connectors | [`hub::TurnOptions`], [`hub::ConnectorTurn`] |
//! | Tokens in the keychain, access and refresh apart | [`secrets`] |
//!
//! Configuration is written only by the app's backend after an explicit
//! Settings action confirmed in a native dialog; nothing here can be reached
//! by the model.

pub mod canonical;
pub mod claude_import;
pub mod config;
pub mod directory;
pub mod fence;
pub mod hub;
pub mod netguard;
pub mod oauth;
pub mod pin;
pub mod registry;
pub mod sanitize;
pub mod schema;
pub mod secrets;
pub mod session;

#[cfg(any(test, feature = "test-support"))]
pub mod fixture;

pub use config::{AuthConfig, EnvConfig, HeaderConfig, ServerConfig, TransportConfig};
pub use hub::{
    CallApprover, CallProposal, ConnectorHub, ConnectorTurn, Decision, NewSecrets, TurnOptions,
};
pub use pin::Permission;
pub use pin::ToolKind;
pub use registry::{CallLogEntry, ConnectorStore, PermissionChange, Registry, ServerEntry};
