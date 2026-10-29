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
//! * `Ok` is an observation for the model and `Err` a message for the model;
//!   both are already fenced and size-capped when they carry anything an
//!   outside service wrote. A refusal is an `Err`, never a panic.
//!
//! Contract for the harness:
//! * offer `specs()` only on tiers that may call tools, and only when the
//!   turn's explicit scope has no Keep-local note; retrieve with Keep-local
//!   content excluded for the whole turn, and call
//!   [`ExternalTools::mark_keep_local`] if any reaches the context anyway;
//! * once [`ExternalTools::sealed`] is true, an outside service's words are in
//!   the context: make no further proposals of any kind in that turn (no
//!   connector calls, no note inserts, no action items).

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

    /// True once a result or error from an outside service has been returned
    /// in this turn (see the module docs for what the harness must do).
    fn sealed(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// A stand-in with the shape a connector tool set has.
    #[derive(Default)]
    struct Lookup {
        sealed: AtomicBool,
        keep_local: AtomicBool,
    }

    #[async_trait]
    impl ExternalTools for Lookup {
        fn specs(&self) -> Vec<ToolSpec> {
            vec![ToolSpec {
                name: "mcp_docs__search".into(),
                description: "[Docs connector] Search pages".into(),
                input_schema: serde_json::json!({"type": "object"}),
                strict: false,
            }]
        }
        fn owns(&self, name: &str) -> bool {
            name == "mcp_docs__search"
        }
        async fn call(&self, _name: &str, args: Value) -> Result<String, String> {
            if self.keep_local.load(Ordering::SeqCst) {
                return Err("off for Keep-local content".into());
            }
            if self.sealed.swap(true, Ordering::SeqCst) {
                return Err("one result per turn".into());
            }
            Ok(format!("<connector_result>{args}</connector_result>"))
        }
        fn mark_keep_local(&self) {
            self.keep_local.store(true, Ordering::SeqCst);
        }
        fn sealed(&self) -> bool {
            self.sealed.load(Ordering::SeqCst)
        }
    }

    /// The routing a harness does in its tool executor.
    async fn route(
        ext: &Arc<dyn ExternalTools>,
        name: &str,
        args: Value,
    ) -> Result<String, String> {
        if ext.owns(name) {
            ext.call(name, args).await
        } else {
            Ok(format!("builtin {name}"))
        }
    }

    #[tokio::test]
    async fn a_trait_object_routes_owned_names_and_seals() {
        let ext: Arc<dyn ExternalTools> = Arc::new(Lookup::default());
        assert_eq!(ext.specs()[0].name, "mcp_docs__search");
        assert_eq!(
            route(&ext, "search_passages", serde_json::json!({})).await,
            Ok("builtin search_passages".into())
        );
        assert!(!ext.sealed());
        let out = route(&ext, "mcp_docs__search", serde_json::json!({"q": "x"}))
            .await
            .unwrap();
        assert!(out.starts_with("<connector_result>"));
        assert!(ext.sealed(), "the harness can see a result is in the turn");
        assert!(route(&ext, "mcp_docs__search", serde_json::json!({}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn the_keep_local_tripwire_refuses_later_calls() {
        let ext: Arc<dyn ExternalTools> = Arc::new(Lookup::default());
        ext.mark_keep_local();
        assert!(ext
            .call("mcp_docs__search", serde_json::json!({}))
            .await
            .is_err());
        assert!(!ext.sealed());
    }
}
