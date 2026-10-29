//! Tools that live outside the Ask harness, such as connector (MCP) tools.
//!
//! The harness (`fly-ask`) and the connector client (`fly-mcp-client`) don't
//! depend on each other. The app builds an [`ExternalTools`] for each Ask turn
//! and hands it to the harness, which offers [`ExternalTools::specs`] to the
//! model and routes calls for those names to [`ExternalTools::call`].
//!
//! Contract for implementations:
//! * names in `specs()` never collide with the harness's own tools, and an
//!   empty list means "offer nothing" (for example on a small local model,
//!   or when the turn's scope includes Keep-local content);
//! * `call` does its own approval, validation and result fencing. It may wait
//!   minutes for the user and must be cancel-safe (dropping the future
//!   abandons the call);
//! * `Ok` is an observation for the model, `Err` a message for the model. A
//!   refusal is an `Err`, never a panic.

use async_trait::async_trait;
use serde_json::Value;

use crate::conversation::ToolSpec;

#[async_trait]
pub trait ExternalTools: Send + Sync {
    /// Tools to offer this turn.
    fn specs(&self) -> Vec<ToolSpec>;

    /// True when `name` is one of [`ExternalTools::specs`].
    fn owns(&self, name: &str) -> bool;

    /// Run one call the model proposed.
    async fn call(&self, name: &str, args: Value) -> Result<String, String>;

    /// Tripwire for the harness: Keep-local content entered this turn's
    /// context. Every later call is refused.
    fn mark_keep_local(&self);
}
