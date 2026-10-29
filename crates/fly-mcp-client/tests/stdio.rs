//! The stdio transport against the real fixture program.

use std::sync::Arc;

use async_trait::async_trait;
use fly_llm::external_tools::ExternalTools;
use fly_mcp_client::config::{AuthConfig, ServerConfig, TransportConfig};
use fly_mcp_client::hub::{
    CallApprover, CallProposal, ConnectorHub, Decision, LiveConnector, TurnOptions,
};
use fly_mcp_client::netguard::NetPolicy;
use fly_mcp_client::registry::{MemoryConnectorStore, ToolPermission};
use fly_secrets::MemorySecretStore;
use serde_json::json;

struct RunAll;

#[async_trait]
impl CallApprover for RunAll {
    async fn decide(&self, p: CallProposal) -> Decision {
        Decision::Run { args: p.args }
    }
}

fn hub_for(args: Vec<String>) -> (Arc<ConnectorHub>, String) {
    let secrets = Arc::new(MemorySecretStore::default());
    let policy = NetPolicy::local_test();
    let hub = Arc::new(ConnectorHub::new(
        Arc::new(MemoryConnectorStore::default()),
        secrets.clone(),
        policy,
        Arc::new(LiveConnector { secrets, policy }),
    ));
    let id = hub
        .add_approved(ServerConfig {
            name: "Fixture".into(),
            transport: TransportConfig::Stdio {
                command: env!("CARGO_BIN_EXE_fly-mcp-fixture").into(),
                args,
                env_names: vec!["ACME_TOKEN".into()],
            },
            auth: AuthConfig::None,
        })
        .unwrap();
    (hub, id)
}

fn enable(hub: &ConnectorHub, id: &str, tool: &str) {
    let changes = vec![ToolPermission {
        tool: tool.into(),
        enabled: true,
        read_only: true,
    }];
    let plan = hub.plan_permissions(id, &changes).unwrap();
    hub.apply_permissions(id, &changes, &plan).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stdio_program_runs_with_a_clean_environment() {
    // A variable in FOTW's own environment must not leak to the connector.
    std::env::set_var("FOTW_PARENT_SECRET", "do-not-leak");
    let (hub, id) = hub_for(vec!["--env-probe".into()]);
    hub.set_env_secret(&id, "ACME_TOKEN", "t0ken").unwrap();
    let report = hub.sync_tools(&id).await.unwrap();
    assert_eq!(report.added.len(), 4);
    enable(&hub, &id, "search_contacts");
    let turn = hub.turn(TurnOptions::for_tier("t", "C", false), Arc::new(RunAll));
    let out = turn
        .call("mcp_fixture__search_contacts", json!({"query": "x"}))
        .await
        .unwrap();
    assert!(out.contains("ACME_TOKEN"), "{out}");
    assert!(!out.contains("FOTW_PARENT_SECRET"), "{out}");
    hub.close_all().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stdio_program_that_changes_its_tools_is_caught() {
    let (hub, id) = hub_for(vec!["--swap".into()]);
    hub.sync_tools(&id).await.unwrap();
    enable(&hub, &id, "search_contacts");
    let first = hub.turn(TurnOptions::for_tier("t1", "C", false), Arc::new(RunAll));
    first
        .call("mcp_fixture__search_contacts", json!({"query": "x"}))
        .await
        .unwrap();
    let second = hub.turn(TurnOptions::for_tier("t2", "C", false), Arc::new(RunAll));
    let err = second
        .call("mcp_fixture__search_contacts", json!({"query": "x"}))
        .await
        .unwrap_err();
    assert!(err.contains("changed on the server"), "{err}");
    hub.close_all().await;
}
