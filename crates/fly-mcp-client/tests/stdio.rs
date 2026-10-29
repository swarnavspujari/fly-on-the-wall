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
    let missing = std::env::temp_dir().join("fotw-no-such-program.exe");
    let missing = missing.to_string_lossy().to_string();
    let (hub, id) = hub_for(&missing, vec![]);
    let err = hub.sync_tools(&id).await.unwrap_err();
    assert!(
        matches!(err, HubError::Session(SessionError::ProgramNotFound(ref p)) if *p == missing),
        "{err:?}"
    );
    let reg = hub.registry();
    assert_eq!(
        reg.server(&id).unwrap().last_problem,
        Some(ConnectProblem::ProgramNotFound)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_program_that_was_never_pinned_does_not_run() {
    // Approval pins a bare name like `npx` to a full path first; a bare name
    // that reaches the transport (never shown in full) is refused.
    let (hub, id) = hub_for("fotw-bare-name", vec![]);
    let err = hub.sync_tools(&id).await.unwrap_err();
    assert!(err.to_string().contains("pinned"), "{err}");
    assert!(matches!(
        hub.pin_program(ServerConfig {
            name: "x".into(),
            transport: TransportConfig::Stdio {
                command: "fotw-bare-name".into(),
                args: vec![],
                env: vec![],
            },
            auth: AuthConfig::None,
        }),
        Err(HubError::Session(SessionError::ProgramNotFound(_)))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnecting_stops_the_whole_process_tree() {
    let beat =
        std::env::temp_dir().join(format!("fotw-heartbeat-{}", uuid::Uuid::new_v4().simple()));
    let (hub, id) = hub_for(
        FIXTURE,
        vec!["--grandchild".into(), beat.to_string_lossy().to_string()],
    );
    hub.sync_tools(&id).await.unwrap();
    let read = || std::fs::read_to_string(&beat).unwrap_or_default();
    let mut alive = false;
    for _ in 0..50 {
        let a = read();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        if !a.is_empty() && read() != a {
            alive = true;
            break;
        }
    }
    assert!(alive, "the grandchild started");
    hub.close_all().await;
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let a = read();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    assert_eq!(read(), a, "the grandchild was stopped with the connector");
    let _ = std::fs::remove_file(&beat);
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
