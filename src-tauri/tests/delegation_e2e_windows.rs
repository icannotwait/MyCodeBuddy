//! Windows counterpart to `delegation_e2e_uds.rs`: drive a real named-pipe
//! round-trip through the listener → broker → mock spawner → `complete_call`
//! chain. Guards against regressions like generating a temp-file path
//! instead of a `\\.\pipe\...` address, or dropping the server instance
//! between accepts.

#![cfg(windows)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use codeg_lib::acp::delegation::broker::{
    ConversationDepthLookup, DelegationBroker, DelegationConfig,
};
use codeg_lib::acp::delegation::listener::{
    DelegationListener, ParentSessionLookup, TokenEntry, TokenRegistry,
};
use codeg_lib::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
use codeg_lib::acp::delegation::transport::{client_round_trip, BrokerRequest, BrokerResponse};
use codeg_lib::acp::delegation::types::{DelegationError, DelegationOutcome, DelegationSuccess};
use codeg_lib::models::AgentType;
use serde_json::json;

struct AlwaysRoot;
#[async_trait]
impl ConversationDepthLookup for AlwaysRoot {
    async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
        Ok(None)
    }
}

struct FixedParent(i32);
#[async_trait]
impl ParentSessionLookup for FixedParent {
    async fn current_conversation_id(&self, _: &str) -> Option<i32> {
        Some(self.0)
    }
}

fn unique_pipe(tag: &str) -> String {
    format!(
        r"\\.\pipe\codeg-e2e-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    )
}

/// Named pipes aren't file paths, so we can't `Path::exists` them. Retry the
/// round-trip a few times to ride out the brief window before the listener
/// task creates its first server instance.
async fn client_round_trip_with_retry(
    pipe: &str,
    req: &BrokerRequest,
) -> std::io::Result<BrokerResponse> {
    let mut last_err = None;
    for _ in 0..50 {
        match client_round_trip(pipe, req).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("client_round_trip retries exhausted")))
}

#[tokio::test]
async fn end_to_end_named_pipe_happy_path() {
    let mock = Arc::new(MockSpawner::new());
    mock.queue_spawn(Ok("child-conn-1".into())).await;
    mock.queue_send(Ok(77)).await;

    let broker = Arc::new(DelegationBroker::new(
        mock.clone() as Arc<dyn ConnectionSpawner>,
        Arc::new(AlwaysRoot) as Arc<dyn ConversationDepthLookup>,
    ));
    broker
        .set_config(DelegationConfig {
            enabled: true,
            depth_limit: 8,
            ..DelegationConfig::default()
        })
        .await;

    let tokens = Arc::new(TokenRegistry::default());
    tokens
        .register(
            "tok".into(),
            TokenEntry {
                parent_connection_id: "p1".into(),
                working_dir: PathBuf::from(r"C:\Windows\Temp"),
            },
        )
        .await;

    let listener = DelegationListener::new(
        broker.clone(),
        tokens,
        Arc::new(FixedParent(1)) as Arc<dyn ParentSessionLookup>,
    );

    let pipe = unique_pipe("happy");
    let pipe_for_listener = PathBuf::from(&pipe);
    let listener_task = tokio::spawn(async move {
        let _ = listener.run(pipe_for_listener).await;
    });

    let broker_for_completion = broker.clone();
    let completer = tokio::spawn(async move {
        loop {
            if let Some(call_id) = broker_for_completion.peek_first_pending_call_id().await {
                broker_for_completion
                    .complete_call(
                        &call_id,
                        DelegationOutcome::Ok(DelegationSuccess {
                            text: "pipe-result".into(),
                            child_conversation_id: 77,
                            child_agent_type: AgentType::Codex,
                            turn_count: 1,
                            duration_ms: 12,
                            token_usage: None,
                        }),
                    )
                    .await;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    let req = BrokerRequest {
        token: "tok".into(),
        parent_connection_id: "p1".into(),
        parent_tool_use_id: "pt-1".into(),
        external_handle: None,
        input: json!({"agent_type": "codex", "task": "do x"}),
    };
    let resp = client_round_trip_with_retry(&pipe, &req)
        .await
        .expect("client round-trip");

    completer.await.unwrap();
    listener_task.abort();

    assert_eq!(resp.outcome["kind"], "ok");
    assert_eq!(resp.outcome["text"], "pipe-result");
    assert_eq!(resp.outcome["child_conversation_id"], 77);
}

#[tokio::test]
async fn end_to_end_named_pipe_back_to_back_requests() {
    // Two sequential round-trips against the same listener. If the Windows
    // accept loop ever regresses to "create server only after handling a
    // connection", the second call will race against a missing pipe and the
    // client will see "system cannot find the file specified".
    let mock = Arc::new(MockSpawner::new());
    mock.queue_spawn(Ok("child-1".into())).await;
    mock.queue_send(Ok(1)).await;
    mock.queue_spawn(Ok("child-2".into())).await;
    mock.queue_send(Ok(2)).await;

    let broker = Arc::new(DelegationBroker::new(
        mock.clone() as Arc<dyn ConnectionSpawner>,
        Arc::new(AlwaysRoot) as Arc<dyn ConversationDepthLookup>,
    ));
    broker
        .set_config(DelegationConfig {
            enabled: true,
            depth_limit: 8,
            ..DelegationConfig::default()
        })
        .await;

    let tokens = Arc::new(TokenRegistry::default());
    tokens
        .register(
            "tok".into(),
            TokenEntry {
                parent_connection_id: "p1".into(),
                working_dir: PathBuf::from(r"C:\Windows\Temp"),
            },
        )
        .await;
    let listener = DelegationListener::new(
        broker.clone(),
        tokens,
        Arc::new(FixedParent(1)) as Arc<dyn ParentSessionLookup>,
    );

    let pipe = unique_pipe("repeat");
    let pipe_for_listener = PathBuf::from(&pipe);
    let listener_task = tokio::spawn(async move {
        let _ = listener.run(pipe_for_listener).await;
    });

    // A completer that resolves each call as it's registered.
    let broker_for_completion = broker.clone();
    let completer = tokio::spawn(async move {
        let mut completed = 0;
        while completed < 2 {
            if let Some(call_id) = broker_for_completion.peek_first_pending_call_id().await {
                broker_for_completion
                    .complete_call(
                        &call_id,
                        DelegationOutcome::Ok(DelegationSuccess {
                            text: format!("done-{completed}"),
                            child_conversation_id: completed + 1,
                            child_agent_type: AgentType::Codex,
                            turn_count: 1,
                            duration_ms: 5,
                            token_usage: None,
                        }),
                    )
                    .await;
                completed += 1;
            } else {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    });

    for i in 0..2 {
        let req = BrokerRequest {
            token: "tok".into(),
            parent_connection_id: "p1".into(),
            parent_tool_use_id: format!("pt-{i}"),
            external_handle: None,
            input: json!({"agent_type": "codex", "task": "x"}),
        };
        let resp = client_round_trip_with_retry(&pipe, &req)
            .await
            .unwrap_or_else(|e| panic!("round-trip {i} failed: {e}"));
        assert_eq!(resp.outcome["kind"], "ok", "round-trip {i}");
    }

    completer.await.unwrap();
    listener_task.abort();
}
