//! The stdio transport against the real fixture program.

use std::sync::Arc;

use async_trait::async_trait;
use fly_llm::external_tools::ExternalTools;
use fly_mcp_client::config::{AuthConfig, EnvConfig, ServerConfig, TransportConfig};
use fly_mcp_client::hub::{
    CallApprover, CallProposal, ConnectorHub, Decision, HubError, LiveConnector, NewSecrets,
    TurnOptions,
};
use fly_mcp_client::netguard::NetPolicy;
use fly_mcp_client::registry::{ConnectProblem, MemoryConnectorStore};
use fly_mcp_client::session::SessionError;
use fly_secrets::MemorySecretStore;
use serde_json::json;

struct RunAll;

#[async_trait]
impl CallApprover for RunAll {
    async fn decide(&self, p: CallProposal) -> Decision {
        Decision::Run { args: p.args }
    }
}

fn hub_for(command: &str, args: Vec<String>) -> (Arc<ConnectorHub>, String) {
    let secrets = Arc::new(MemorySecretStore::default());
    let policy = NetPolicy::local_test();
    let hub = Arc::new(ConnectorHub::new(
        Arc::new(MemoryConnectorStore::default()),
        secrets.clone(),
        policy,
        Arc::new(LiveConnector { secrets, policy }),
    ));
    let id = hub
        .add_approved(
            ServerConfig {
                name: "Fixture".into(),
                transport: TransportConfig::Stdio {
                    command: command.into(),
                    args,
                    env: vec![
                        EnvConfig {
                            name: "ACME_TOKEN".into(),
                            value: None,
                        },
                        EnvConfig {
                            name: "ACME_REGION".into(),
                            value: Some("eu".into()),
                        },
                    ],
                },
                auth: AuthConfig::None,
            },
            &NewSecrets {
                values: [("ACME_TOKEN".to_string(), "t0ken".to_string())].into(),
                ..Default::default()
            },
        )
        .unwrap();
    (hub, id)
}

const FIXTURE: &str = env!("CARGO_BIN_EXE_fly-mcp-fixture");

#[tokio::test(flavor = "multi_thread")]
async fn a_stdio_program_runs_with_a_clean_environment() {
    // A variable in FOTW's own environment must not leak to the connector.
    std::env::set_var("FOTW_PARENT_SECRET", "do-not-leak");
    let (hub, id) = hub_for(FIXTURE, vec!["--env-probe".into()]);
    let report = hub.sync_tools(&id).await.unwrap();
    assert_eq!(report.added.len(), 4);
    let turn = hub.turn(TurnOptions::for_tier("t", "C", false), Arc::new(RunAll));
    let out = turn
        .call("mcp_fixture__search_contacts", json!({"query": "x"}))
        .await
        .unwrap();
    assert!(
        out.contains("ACME_TOKEN") && out.contains("ACME_REGION"),
        "{out}"
    );
    assert!(!out.contains("FOTW_PARENT_SECRET"), "{out}");
    hub.close_all().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stdio_program_that_changes_its_tools_is_caught() {
    let (hub, id) = hub_for(FIXTURE, vec!["--swap".into()]);
    hub.sync_tools(&id).await.unwrap();
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

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_program_is_reported_by_name() {
    let (hub, id) = hub_for("fotw-no-such-program", vec![]);
    let err = hub.sync_tools(&id).await.unwrap_err();
    assert!(
        matches!(err, HubError::Session(SessionError::ProgramNotFound(ref p)) if p == "fotw-no-such-program"),
        "{err:?}"
    );
    let reg = hub.registry();
    assert_eq!(
        reg.server(&id).unwrap().last_problem,
        Some(ConnectProblem::ProgramNotFound)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_program_that_stops_at_once_reports_its_last_words() {
    let (hub, id) = hub_for(FIXTURE, vec!["--crash".into()]);
    let err = hub.sync_tools(&id).await.unwrap_err();
    match err {
        HubError::Session(SessionError::Exited(tail)) => {
            assert!(tail.contains("fatal: the token"), "{tail}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        hub.registry().server(&id).unwrap().last_problem,
        Some(ConnectProblem::CouldNotStart)
    );
}
