#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::broker::{
        ConversationDepthLookup, DelegationBroker, DelegationMatchKey,
    };
    use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
    use crate::acp::delegation::types::DelegationError;
    use crate::acp::types::{AcpEvent, EventEnvelope};
    use crate::models::AgentType;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct RootDepth;
    #[async_trait::async_trait]
    impl ConversationDepthLookup for RootDepth {
        async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
            Ok(None)
        }
    }

    use super::test_support::MapSessions;

    struct ScriptedStore {
        inner: std::sync::Mutex<VecDeque<Result<CursorStoredToolCall, CursorStoreError>>>,
    }

    impl CursorStoreLookup for ScriptedStore {
        fn lookup(
            &self,
            _session_id: &str,
            _tool_call_id: &str,
        ) -> Result<CursorStoredToolCall, CursorStoreError> {
            self.inner
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(CursorStoreError::NoExactMatch))
        }
    }

    fn cursor_session() -> CursorEnrichmentSession {
        CursorEnrichmentSession {
            agent_type: AgentType::Cursor,
            external_session_id: "0198c9aa-1111-2222-3333-444455556666".into(),
        }
    }

    fn mcp_tool_envelope(
        connection_id: &str,
        tool_call_id: &str,
        raw_input: Option<&str>,
    ) -> EventEnvelope {
        EventEnvelope {
            seq: 1,
            connection_id: connection_id.into(),
            payload: AcpEvent::ToolCall {
                tool_call_id: tool_call_id.into(),
                title: CURSOR_IDENTITYLESS_MCP_TITLE.into(),
                kind: "other".into(),
                status: "pending".into(),
                content: None,
                raw_input: raw_input.map(|s| s.to_string()),
                raw_output: None,
                locations: None,
                meta: None,
                images: None,
            },
        }
    }

    #[tokio::test]
    async fn schedules_only_cursor_identityless_mcp_shape() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .register_identityless_tool_call("cursor-conn", "tc-1".into())
            .await;
        let sessions = Arc::new(MapSessions(std::sync::Mutex::new(
            [
                ("cursor-conn".into(), cursor_session()),
                (
                    "codex-conn".into(),
                    CursorEnrichmentSession {
                        agent_type: AgentType::Codex,
                        external_session_id: "s".into(),
                    },
                ),
            ]
            .into(),
        )));
        let store = Arc::new(ScriptedStore {
            inner: std::sync::Mutex::new(VecDeque::from([Ok(CursorStoredToolCall {
                tool_name: "delegate_to_agent".into(),
                args: serde_json::json!({
                    "agent_type": "codex",
                    "task": "build it",
                    "correlation_id": "corr-1"
                }),
            })])),
        });
        let enricher = CursorStoreEnricher::new(store, sessions, broker.clone(), metrics.clone());

        enricher.maybe_schedule(&mcp_tool_envelope("codex-conn", "tc-x", Some("{}")));
        enricher.maybe_schedule(&EventEnvelope {
            seq: 1,
            connection_id: "cursor-conn".into(),
            payload: AcpEvent::ToolCall {
                tool_call_id: "tc-1".into(),
                title: "MCP: weather".into(),
                kind: "other".into(),
                status: "pending".into(),
                content: None,
                raw_input: Some("{}".into()),
                raw_output: None,
                locations: None,
                meta: None,
                images: None,
            },
        });
        enricher.maybe_schedule(&mcp_tool_envelope(
            "cursor-conn",
            "tc-1",
            Some(r#"{"providerIdentifier":"x"}"#),
        ));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(metrics.snapshot().cursor_enrichment_scheduled, 0);

        enricher.maybe_schedule(&mcp_tool_envelope("cursor-conn", "tc-1", Some("{}")));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(metrics.snapshot().cursor_enrichment_scheduled, 1);
        assert_eq!(metrics.snapshot().cursor_enrichment_resolved, 1);
        assert_eq!(
            broker
                .take_matching_tool_call(
                    "cursor-conn",
                    &DelegationMatchKey::Delegate {
                        correlation_id: "corr-1".into(),
                        agent_type: AgentType::Codex,
                        task: "build it".into(),
                        working_dir: None,
                    }
                )
                .await
                .as_deref(),
            Some("tc-1")
        );
    }

    #[tokio::test]
    async fn repeated_frames_share_one_in_flight_lookup() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .register_identityless_tool_call("cursor-conn", "tc-1".into())
            .await;
        let store = Arc::new(BlockingStore);
        let sessions = Arc::new(MapSessions(std::sync::Mutex::new(
            [("cursor-conn".into(), cursor_session())].into(),
        )));
        let enricher = CursorStoreEnricher::new(store, sessions, broker, metrics.clone());
        let env = mcp_tool_envelope("cursor-conn", "tc-1", Some("{}"));
        enricher.maybe_schedule(&env);
        enricher.maybe_schedule(&env);
        assert_eq!(
            enricher.in_flight_len_for_test(),
            1,
            "second frame must join the same in-flight lookup"
        );
    }

    struct BlockingStore;
    impl CursorStoreLookup for BlockingStore {
        fn lookup(&self, _: &str, _: &str) -> Result<CursorStoredToolCall, CursorStoreError> {
            std::thread::sleep(Duration::from_millis(200));
            Err(CursorStoreError::NoExactMatch)
        }
    }

    #[tokio::test]
    async fn maybe_schedule_returns_before_blocking_scan_finishes() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .register_identityless_tool_call("cursor-conn", "tc-1".into())
            .await;
        let enricher = CursorStoreEnricher::new(
            Arc::new(BlockingStore),
            Arc::new(MapSessions(std::sync::Mutex::new(
                [("cursor-conn".into(), cursor_session())].into(),
            ))),
            broker,
            metrics,
        );
        let started = std::time::Instant::now();
        enricher.maybe_schedule(&mcp_tool_envelope("cursor-conn", "tc-1", Some("{}")));
        assert!(started.elapsed() < Duration::from_millis(20));
    }

    // Deliberately NOT `start_paused = true`: the retry sleep is governed by
    // tokio's timer, but the store lookup itself always runs inside a real
    // `spawn_blocking` OS-thread hop (production correctness requirement —
    // the coordinator can't special-case a scripted test store). Under a
    // paused virtual clock, `tokio::time::advance` jumps the clock forward
    // *before* yielding even once, so it can race ahead of the still-running
    // first attempt and strand the eventual retry sleep past the point the
    // clock will ever reach again. A real (short) wait sidesteps that
    // ordering hazard entirely and still exercises the same retry-then-
    // succeed path deterministically, since `CURSOR_STORE_RETRY_INITIAL` is
    // only 50ms.
    #[tokio::test]
    async fn retries_then_succeeds_inside_deadline() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .register_identityless_tool_call("cursor-conn", "tc-1".into())
            .await;
        let store = Arc::new(ScriptedStore {
            inner: std::sync::Mutex::new(VecDeque::from([
                Err(CursorStoreError::NoExactMatch),
                Ok(CursorStoredToolCall {
                    tool_name: "continue_delegation".into(),
                    args: serde_json::json!({
                        "task_id": "run-42",
                        "task": "review",
                        "correlation_id": "cont-1"
                    }),
                }),
            ])),
        });
        let enricher = CursorStoreEnricher::new(
            store,
            Arc::new(MapSessions(std::sync::Mutex::new(
                [("cursor-conn".into(), cursor_session())].into(),
            ))),
            broker.clone(),
            metrics.clone(),
        );
        enricher.maybe_schedule(&mcp_tool_envelope("cursor-conn", "tc-1", None));
        tokio::time::sleep(CURSOR_STORE_RETRY_INITIAL * 4).await;
        assert_eq!(
            broker
                .take_matching_tool_call(
                    "cursor-conn",
                    &DelegationMatchKey::Continue {
                        correlation_id: "cont-1".into(),
                        target_task_id: "run-42".into(),
                        task: "review".into(),
                    }
                )
                .await
                .as_deref(),
            Some("tc-1")
        );
        assert_eq!(metrics.snapshot().cursor_enrichment_resolved, 1);
    }

    #[tokio::test]
    async fn unsupported_tool_and_invalid_args_do_not_backfill() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .register_identityless_tool_call("cursor-conn", "tc-1".into())
            .await;
        let store = Arc::new(ScriptedStore {
            inner: std::sync::Mutex::new(VecDeque::from([Ok(CursorStoredToolCall {
                tool_name: "not_delegate_to_agent".into(),
                args: serde_json::json!({
                    "agent_type": "codex",
                    "task": "build it",
                    "correlation_id": "corr-1"
                }),
            })])),
        });
        let enricher = CursorStoreEnricher::new(
            store,
            Arc::new(MapSessions(std::sync::Mutex::new(
                [("cursor-conn".into(), cursor_session())].into(),
            ))),
            broker.clone(),
            metrics.clone(),
        );
        enricher.maybe_schedule(&mcp_tool_envelope("cursor-conn", "tc-1", Some("{}")));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            metrics
                .snapshot()
                .cursor_enrichment_failed
                .get("unsupported_tool")
                .copied(),
            Some(1)
        );
        assert!(broker
            .take_matching_tool_call(
                "cursor-conn",
                &DelegationMatchKey::Delegate {
                    correlation_id: "x".into(),
                    agent_type: AgentType::Codex,
                    task: "x".into(),
                    working_dir: None,
                }
            )
            .await
            .is_none());
    }

    #[tokio::test]
    async fn delegate_tool_with_continue_args_is_invalid_args() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .register_identityless_tool_call("cursor-conn", "tc-1".into())
            .await;
        let store = Arc::new(ScriptedStore {
            inner: std::sync::Mutex::new(VecDeque::from([Ok(CursorStoredToolCall {
                tool_name: "delegate_to_agent".into(),
                args: serde_json::json!({
                    "task_id": "run-42",
                    "task": "review",
                    "correlation_id": "cont-1"
                }),
            })])),
        });
        let enricher = CursorStoreEnricher::new(
            store,
            Arc::new(MapSessions(std::sync::Mutex::new(
                [("cursor-conn".into(), cursor_session())].into(),
            ))),
            broker.clone(),
            metrics.clone(),
        );
        enricher.maybe_schedule(&mcp_tool_envelope("cursor-conn", "tc-1", Some("{}")));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            metrics
                .snapshot()
                .cursor_enrichment_failed
                .get("invalid_args")
                .copied(),
            Some(1)
        );
        assert!(broker
            .take_matching_tool_call(
                "cursor-conn",
                &DelegationMatchKey::Continue {
                    correlation_id: "cont-1".into(),
                    target_task_id: "run-42".into(),
                    task: "review".into(),
                }
            )
            .await
            .is_none());
    }

    struct AfterDeadlineStore;
    impl CursorStoreLookup for AfterDeadlineStore {
        fn lookup(&self, _: &str, _: &str) -> Result<CursorStoredToolCall, CursorStoreError> {
            std::thread::sleep(CURSOR_STORE_LOOKUP_DEADLINE + Duration::from_millis(50));
            Ok(CursorStoredToolCall {
                tool_name: "delegate_to_agent".into(),
                args: serde_json::json!({
                    "agent_type": "codex",
                    "task": "late",
                    "correlation_id": "corr-late"
                }),
            })
        }
    }

    #[tokio::test]
    async fn lookup_finishing_after_deadline_does_not_backfill() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .register_identityless_tool_call("cursor-conn", "tc-late".into())
            .await;
        let enricher = CursorStoreEnricher::new(
            Arc::new(AfterDeadlineStore),
            Arc::new(MapSessions(std::sync::Mutex::new(
                [("cursor-conn".into(), cursor_session())].into(),
            ))),
            broker.clone(),
            metrics.clone(),
        );
        enricher.maybe_schedule(&mcp_tool_envelope("cursor-conn", "tc-late", Some("{}")));
        tokio::time::sleep(CURSOR_STORE_LOOKUP_DEADLINE + Duration::from_millis(200)).await;
        assert_eq!(
            metrics
                .snapshot()
                .cursor_enrichment_failed
                .get("deadline")
                .copied(),
            Some(1)
        );
        assert_eq!(metrics.snapshot().cursor_enrichment_resolved, 0);
        assert!(broker
            .take_matching_tool_call(
                "cursor-conn",
                &DelegationMatchKey::Delegate {
                    correlation_id: "corr-late".into(),
                    agent_type: AgentType::Codex,
                    task: "late".into(),
                    working_dir: None,
                }
            )
            .await
            .is_none());
    }

    struct CallRecordingStore {
        called: AtomicBool,
    }
    impl CursorStoreLookup for CallRecordingStore {
        fn lookup(&self, _: &str, _: &str) -> Result<CursorStoredToolCall, CursorStoreError> {
            self.called.store(true, Ordering::SeqCst);
            Err(CursorStoreError::NoExactMatch)
        }
    }

    #[tokio::test]
    async fn missing_session_lookup_does_not_schedule_or_call_store() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .register_identityless_tool_call("unknown-conn", "tc-1".into())
            .await;
        let store = Arc::new(CallRecordingStore {
            called: AtomicBool::new(false),
        });
        let sessions = Arc::new(MapSessions(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )));
        let enricher = CursorStoreEnricher::new(store.clone(), sessions, broker, metrics.clone());
        enricher.maybe_schedule(&mcp_tool_envelope("unknown-conn", "tc-1", Some("{}")));
        tokio::time::sleep(Duration::from_millis(30)).await;
        let snap = metrics.snapshot();
        assert_eq!(snap.cursor_enrichment_scheduled, 0);
        assert!(!snap
            .cursor_enrichment_failed
            .contains_key("invalid_session"));
        assert!(!store.called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn invalid_session_id_records_failure_without_scheduling_or_calling_store() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .register_identityless_tool_call("cursor-conn", "tc-1".into())
            .await;
        let store = Arc::new(CallRecordingStore {
            called: AtomicBool::new(false),
        });
        let sessions = Arc::new(MapSessions(std::sync::Mutex::new(
            [(
                "cursor-conn".into(),
                CursorEnrichmentSession {
                    agent_type: AgentType::Cursor,
                    external_session_id: "../evil".into(),
                },
            )]
            .into(),
        )));
        let enricher = CursorStoreEnricher::new(store.clone(), sessions, broker, metrics.clone());
        enricher.maybe_schedule(&mcp_tool_envelope("cursor-conn", "tc-1", Some("{}")));
        tokio::time::sleep(Duration::from_millis(30)).await;
        let snap = metrics.snapshot();
        assert_eq!(snap.cursor_enrichment_scheduled, 0);
        assert_eq!(
            snap.cursor_enrichment_failed
                .get("invalid_session")
                .copied(),
            Some(1)
        );
        assert!(!store.called.load(Ordering::SeqCst));
    }

    #[test]
    fn retryable_deadline_failure_uses_last_miss_class() {
        assert_eq!(
            retryable_deadline_failure(CursorStoreError::StoreNotFound),
            CursorEnrichmentFailure::NotFound
        );
        assert_eq!(
            retryable_deadline_failure(CursorStoreError::NoExactMatch),
            CursorEnrichmentFailure::NoMatch
        );
        assert_eq!(
            retryable_deadline_failure(CursorStoreError::StoreUnreadable),
            CursorEnrichmentFailure::Unreadable
        );
    }

    #[test]
    fn classify_store_tool_name_accepts_only_leaf_delegation_tools() {
        assert_eq!(
            classify_store_tool_name("mcp__codeg-mcp__delegate_to_agent"),
            Some(CursorStoreToolKind::Delegate)
        );
        assert_eq!(
            classify_store_tool_name("codeg-mcp:continue_delegation"),
            Some(CursorStoreToolKind::Continue)
        );
        assert_eq!(classify_store_tool_name("not_delegate_to_agent"), None);
        assert_eq!(classify_store_tool_name("Read"), None);
    }

    #[test]
    fn enrichment_snapshot_labels_are_closed_and_secret_free() {
        let metrics = DelegationMetrics::default();
        metrics.record_cursor_enrichment_scheduled();
        metrics.record_cursor_enrichment_failed(CursorEnrichmentFailure::NoMatch);
        metrics.record_cursor_enrichment_backfill(
            crate::acp::delegation::broker::IdentitylessBackfillResult::Applied,
        );
        let snap = metrics.snapshot();
        // Scoped to the fields this task adds: the full snapshot already
        // carries an unrelated pre-existing field
        // (`completion_tool_supersessions`) whose name substring-matches
        // "session" and would otherwise false-positive this check (see the
        // codebase convention of checking field names / a scoped slice
        // rather than the whole blob, e.g. the `metrics.rs` promote-log and
        // availability-audit secret tests).
        let json = serde_json::to_string(&serde_json::json!({
            "cursor_enrichment_scheduled": snap.cursor_enrichment_scheduled,
            "cursor_enrichment_resolved": snap.cursor_enrichment_resolved,
            "cursor_enrichment_failed": snap.cursor_enrichment_failed,
            "cursor_enrichment_backfill": snap.cursor_enrichment_backfill,
            "cursor_enrichment_duration_ms_count": snap.cursor_enrichment_duration_ms_count,
            "cursor_enrichment_duration_ms_total": snap.cursor_enrichment_duration_ms_total,
        }))
        .unwrap();
        for forbidden in [
            "session",
            "corr-",
            "delegate_to_agent",
            "task text",
            "acp-sessions",
            "toolCallId",
        ] {
            assert!(!json.contains(forbidden), "leaked {forbidden}: {json}");
        }
        assert_eq!(snap.cursor_enrichment_scheduled, 1);
        assert_eq!(snap.cursor_enrichment_failed.get("no_match"), Some(&1));
        assert_eq!(snap.cursor_enrichment_backfill.get("applied"), Some(&1));
    }

    // Real on-disk `store.db` coordinator tests.
    //
    // Everything above exercises `CursorStoreEnricher` against scripted
    // `CursorStoreLookup` fakes. These two instead wire the real
    // `CursorStoreReader` at a throwaway temp `cursor_dir`, proving the full
    // coordinator + reader integration handles a store file that doesn't
    // exist yet and only appears mid-flight from a concurrent writer —
    // exactly the ACP-vs-Cursor's-own-writer race the coordinator exists
    // to bridge.

    fn temp_cursor_store_root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "codeg-cursor-enrichment-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// Writes a fresh `store.db` at `store_path` containing exactly one
    /// `delegate_to_agent` blob for `tool_call_id`, matching the schema
    /// `CursorStoreReader::lookup` scans (see `cursor_store.rs`'s own
    /// `write_store` test helper, which this mirrors).
    fn write_real_delegate_blob(store_path: &std::path::Path, tool_call_id: &str) {
        if let Some(parent) = store_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = rusqlite::Connection::open(store_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        let blob = serde_json::json!({
            "content": [{
                "type": "tool-call",
                "toolCallId": tool_call_id,
                "toolName": "delegate_to_agent",
                "args": {
                    "agent_type": "codex",
                    "task": "build it",
                    "correlation_id": "real-store-corr"
                }
            }]
        });
        conn.execute(
            "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
            rusqlite::params!["row-0", serde_json::to_vec(&blob).unwrap()],
        )
        .unwrap();
    }

    fn push_proto_varint(out: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            out.push((value as u8) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn proto_bytes(field_number: u32, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        push_proto_varint(&mut out, u64::from(field_number) << 3 | 2);
        push_proto_varint(&mut out, value.len() as u64);
        out.extend_from_slice(value);
        out
    }

    fn proto_string_value(value: &str) -> Vec<u8> {
        proto_bytes(3, value.as_bytes())
    }

    fn proto_mcp_arg(key: &str, value: &str) -> Vec<u8> {
        let mut entry = proto_bytes(1, key.as_bytes());
        entry.extend(proto_bytes(2, &proto_string_value(value)));
        proto_bytes(2, &entry)
    }

    fn write_real_binary_delegate_blob(store_path: &std::path::Path, tool_call_id: &str) {
        if let Some(parent) = store_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = rusqlite::Connection::open(store_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();

        let mut mcp_args = proto_mcp_arg("agent_type", "codex");
        mcp_args.extend(proto_mcp_arg("task", "build it"));
        mcp_args.extend(proto_mcp_arg("correlation_id", "real-store-corr"));
        mcp_args.extend(proto_bytes(3, tool_call_id.as_bytes()));
        mcp_args.extend(proto_bytes(5, b"delegate_to_agent"));
        let blob = proto_bytes(2, &proto_bytes(7, &mcp_args));
        conn.execute(
            "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
            rusqlite::params!["row-0", blob],
        )
        .unwrap();
    }

    fn real_delegate_key() -> DelegationMatchKey {
        DelegationMatchKey::Delegate {
            correlation_id: "real-store-corr".into(),
            agent_type: AgentType::Codex,
            task: "build it".into(),
            working_dir: None,
        }
    }

    /// The store file doesn't exist at the first (and several subsequent)
    /// lookup attempts — `resolve_store_path` reports `StoreNotFound`, which
    /// is retryable — and only appears ~80 ms in from a concurrent writer
    /// thread, well inside `CURSOR_STORE_LOOKUP_DEADLINE`. The retry loop
    /// must pick it up and backfill.
    #[tokio::test]
    async fn real_store_backfill_succeeds_when_write_lands_before_deadline() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        let session_id = "0198c9aa-1111-2222-3333-444455556666";
        broker
            .register_identityless_tool_call("cursor-conn", "tc-real".into())
            .await;
        let cursor_dir = temp_cursor_store_root();
        let store_path = cursor_dir
            .join("acp-sessions")
            .join(session_id)
            .join("store.db");
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            write_real_delegate_blob(&store_path, "tc-real");
        });
        let store = Arc::new(CursorStoreReader::with_cursor_dir(cursor_dir.clone()));
        let sessions = Arc::new(MapSessions(std::sync::Mutex::new(
            [(
                "cursor-conn".into(),
                CursorEnrichmentSession {
                    agent_type: AgentType::Cursor,
                    external_session_id: session_id.into(),
                },
            )]
            .into(),
        )));
        let enricher = CursorStoreEnricher::new(store, sessions, broker.clone(), metrics.clone());
        enricher.maybe_schedule(&mcp_tool_envelope("cursor-conn", "tc-real", Some("{}")));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            broker
                .take_matching_tool_call("cursor-conn", &real_delegate_key())
                .await
                .as_deref(),
            Some("tc-real")
        );
        assert_eq!(metrics.snapshot().cursor_enrichment_resolved, 1);
        std::fs::remove_dir_all(&cursor_dir).ok();
    }

    #[tokio::test]
    async fn real_binary_store_backfills_before_json_tool_call_is_persisted() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        let session_id = "0198c9aa-aaaa-bbbb-cccc-111122223333";
        let tool_call_id = "call-cursor-proto\nfc_cursor_proto_0";
        broker
            .register_identityless_tool_call("cursor-conn", tool_call_id.into())
            .await;
        let cursor_dir = temp_cursor_store_root();
        let store_path = cursor_dir
            .join("acp-sessions")
            .join(session_id)
            .join("store.db");
        write_real_binary_delegate_blob(&store_path, tool_call_id);

        let enricher = CursorStoreEnricher::new(
            Arc::new(CursorStoreReader::with_cursor_dir(cursor_dir.clone())),
            Arc::new(MapSessions(std::sync::Mutex::new(
                [(
                    "cursor-conn".into(),
                    CursorEnrichmentSession {
                        agent_type: AgentType::Cursor,
                        external_session_id: session_id.into(),
                    },
                )]
                .into(),
            ))),
            broker.clone(),
            metrics.clone(),
        );
        enricher.maybe_schedule(&mcp_tool_envelope("cursor-conn", tool_call_id, Some("{}")));
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(
            broker
                .take_matching_tool_call("cursor-conn", &real_delegate_key())
                .await
                .as_deref(),
            Some(tool_call_id)
        );
        assert_eq!(metrics.snapshot().cursor_enrichment_resolved, 1);
        std::fs::remove_dir_all(&cursor_dir).ok();
    }

    /// Sibling of the above: the writer thread doesn't land the blob until
    /// 1200 ms in — past `CURSOR_STORE_LOOKUP_DEADLINE` (1000 ms) — so the
    /// coordinator must give up and record the last retryable class
    /// (`not_found`) instead of backfilling once the (now-existing) file
    /// would finally match.
    #[tokio::test]
    async fn real_store_write_after_deadline_fails_closed() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        let session_id = "0198c9aa-2222-3333-4444-555566667777";
        broker
            .register_identityless_tool_call("cursor-conn", "tc-late-real".into())
            .await;
        let cursor_dir = temp_cursor_store_root();
        let store_path = cursor_dir
            .join("acp-sessions")
            .join(session_id)
            .join("store.db");
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1200));
            write_real_delegate_blob(&store_path, "tc-late-real");
        });
        let store = Arc::new(CursorStoreReader::with_cursor_dir(cursor_dir.clone()));
        let sessions = Arc::new(MapSessions(std::sync::Mutex::new(
            [(
                "cursor-conn".into(),
                CursorEnrichmentSession {
                    agent_type: AgentType::Cursor,
                    external_session_id: session_id.into(),
                },
            )]
            .into(),
        )));
        let enricher = CursorStoreEnricher::new(store, sessions, broker.clone(), metrics.clone());
        enricher.maybe_schedule(&mcp_tool_envelope(
            "cursor-conn",
            "tc-late-real",
            Some("{}"),
        ));
        tokio::time::sleep(CURSOR_STORE_LOOKUP_DEADLINE + Duration::from_millis(300)).await;
        assert_eq!(
            metrics
                .snapshot()
                .cursor_enrichment_failed
                .get("not_found")
                .copied(),
            Some(1)
        );
        assert_eq!(metrics.snapshot().cursor_enrichment_resolved, 0);
        assert!(broker
            .take_matching_tool_call("cursor-conn", &real_delegate_key())
            .await
            .is_none());
        std::fs::remove_dir_all(&cursor_dir).ok();
    }

    #[tokio::test]
    async fn no_exact_match_until_deadline_records_no_match() {
        let metrics = Arc::new(DelegationMetrics::default());
        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .register_identityless_tool_call("cursor-conn", "tc-miss".into())
            .await;
        let enricher = CursorStoreEnricher::new(
            Arc::new(ScriptedStore {
                inner: std::sync::Mutex::new(VecDeque::new()),
            }),
            Arc::new(MapSessions(std::sync::Mutex::new(
                [("cursor-conn".into(), cursor_session())].into(),
            ))),
            broker,
            metrics.clone(),
        );
        enricher.maybe_schedule(&mcp_tool_envelope("cursor-conn", "tc-miss", Some("{}")));
        tokio::time::sleep(CURSOR_STORE_LOOKUP_DEADLINE + Duration::from_millis(300)).await;
        assert_eq!(
            metrics
                .snapshot()
                .cursor_enrichment_failed
                .get("no_match")
                .copied(),
            Some(1)
        );
        assert!(!metrics
            .snapshot()
            .cursor_enrichment_failed
            .contains_key("deadline"));
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{
        CursorEnrichmentSession, CursorEnrichmentSessionLookup, CursorStoreError,
        CursorStoreLookup, CursorStoredToolCall,
    };
    use std::collections::HashMap;
    use std::time::Duration;

    /// In-memory `CursorEnrichmentSessionLookup` keyed by connection id.
    pub(crate) struct MapSessions(pub std::sync::Mutex<HashMap<String, CursorEnrichmentSession>>);

    #[async_trait::async_trait]
    impl CursorEnrichmentSessionLookup for MapSessions {
        async fn lookup(&self, connection_id: &str) -> Option<CursorEnrichmentSession> {
            self.0.lock().unwrap().get(connection_id).cloned()
        }
    }

    /// A `CursorStoreLookup` that always succeeds with the same record —
    /// for tests that only care about the happy path resolving quickly.
    pub(crate) struct FixedStore(pub CursorStoredToolCall);

    impl CursorStoreLookup for FixedStore {
        fn lookup(
            &self,
            _session_id: &str,
            _tool_call_id: &str,
        ) -> Result<CursorStoredToolCall, CursorStoreError> {
            Ok(self.0.clone())
        }
    }

    /// A `CursorStoreLookup` that blocks the calling (blocking-pool) thread
    /// for `delay` before returning `result` — for tests exercising races
    /// between the lookup finishing and something else happening mid-flight
    /// (e.g. a terminal tool-call event tombstoning the pending entry).
    pub(crate) struct SleepThenStore {
        pub delay: Duration,
        pub result: Result<CursorStoredToolCall, CursorStoreError>,
    }

    impl CursorStoreLookup for SleepThenStore {
        fn lookup(
            &self,
            _session_id: &str,
            _tool_call_id: &str,
        ) -> Result<CursorStoredToolCall, CursorStoreError> {
            std::thread::sleep(self.delay);
            self.result.clone()
        }
    }
}

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

use crate::acp::cursor_store::{
    validate_cursor_session_id, CursorStoreError, CursorStoreReader, CursorStoredToolCall,
};
use crate::acp::delegation::broker::{DelegationBroker, DelegationMatchKey};
use crate::acp::delegation::metrics::{agent_type_label, DelegationMetrics};
use crate::acp::lifecycle::{
    extract_delegation_match_key_from_value, CURSOR_IDENTITYLESS_MCP_TITLE,
};
use crate::acp::manager::ConnectionManager;
use crate::acp::types::{AcpEvent, EventEnvelope};
use crate::models::AgentType;

/// Wall-clock budget for one Cursor-store enrichment lookup, from the first
/// gate-passing frame to a resolved/failed outcome. A store result that
/// becomes ready after this deadline is discarded — never backfilled.
pub const CURSOR_STORE_LOOKUP_DEADLINE: Duration = Duration::from_millis(1000);
/// First retry backoff; doubles per attempt, capped by the remaining budget.
/// The first store attempt itself is immediate (no sleep before it).
pub const CURSOR_STORE_RETRY_INITIAL: Duration = Duration::from_millis(50);
/// Bounded concurrency for blocking SQLite scans across all in-flight lookups.
pub const CURSOR_STORE_SCAN_PERMITS: usize = 2;
/// Minimum spacing between WARN lines for the same enricher instance's
/// terminal store failures (one-shot store misconfiguration shouldn't spam).
const CURSOR_STORE_TERMINAL_WARN_WINDOW: Duration = Duration::from_secs(60);

/// The live ACP session backing a parent connection, as far as this
/// coordinator needs to know: which agent it is, and (only meaningful for
/// `Cursor`) the external session id used to locate its on-disk store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorEnrichmentSession {
    pub agent_type: AgentType,
    pub external_session_id: String,
}

/// Abstracts the live connection/session registry so the coordinator can be
/// unit-tested without a real `ConnectionManager`.
#[async_trait::async_trait]
pub trait CursorEnrichmentSessionLookup: Send + Sync {
    async fn lookup(&self, connection_id: &str) -> Option<CursorEnrichmentSession>;
}

/// Production [`CursorEnrichmentSessionLookup`]: an agent type from the live
/// connection registry, paired with the session's bound external id (unset
/// until `SessionStarted` lands). Missing connection or unset external id
/// both fall through to `None` — [`CursorStoreEnricher::run_lookup`] treats
/// that as "skip silently, no store call, no metric".
#[async_trait::async_trait]
impl CursorEnrichmentSessionLookup for ConnectionManager {
    async fn lookup(&self, connection_id: &str) -> Option<CursorEnrichmentSession> {
        let agent_type = self.agent_type_for_connection(connection_id).await?;
        let state = self.get_state(connection_id).await?;
        let external_session_id = state.read().await.external_id.clone()?;
        Some(CursorEnrichmentSession {
            agent_type,
            external_session_id,
        })
    }
}

/// Abstracts a read-only Cursor store lookup so the coordinator can be
/// unit-tested without touching `~/.cursor`. [`CursorStoreReader`] is the
/// production implementation.
pub trait CursorStoreLookup: Send + Sync {
    fn lookup(
        &self,
        session_id: &str,
        tool_call_id: &str,
    ) -> Result<CursorStoredToolCall, CursorStoreError>;
}

impl CursorStoreLookup for CursorStoreReader {
    fn lookup(
        &self,
        session_id: &str,
        tool_call_id: &str,
    ) -> Result<CursorStoredToolCall, CursorStoreError> {
        CursorStoreReader::lookup(self, session_id, tool_call_id)
    }
}

/// Which delegation MCP tool a Cursor store record names, classified from
/// its leaf tool name only (see [`classify_store_tool_name`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStoreToolKind {
    Delegate,
    Continue,
}

/// Leaf tool name only: `delegate_to_agent` / `continue_delegation`, or the
/// same leaf after `:` / `__` namespace prefixes. `not_delegate_to_agent` is
/// `None` — this is a leaf match, never a substring match.
pub fn classify_store_tool_name(tool_name: &str) -> Option<CursorStoreToolKind> {
    let n = tool_name.to_ascii_lowercase().replace([' ', '-'], "_");
    let leaf = n
        .rsplit_once("__")
        .map(|(_, leaf)| leaf)
        .or_else(|| n.rsplit_once(':').map(|(_, leaf)| leaf))
        .unwrap_or(n.as_str());
    match leaf {
        "delegate_to_agent" => Some(CursorStoreToolKind::Delegate),
        "continue_delegation" => Some(CursorStoreToolKind::Continue),
        _ => None,
    }
}

/// True when the recovered [`DelegationMatchKey`] variant agrees with the
/// store record's classified tool (`Delegate` args for `delegate_to_agent`,
/// `Continue` args for `continue_delegation`). A mismatch is `InvalidArgs`.
fn key_matches_store_tool(key: &DelegationMatchKey, kind: CursorStoreToolKind) -> bool {
    matches!(
        (key, kind),
        (
            DelegationMatchKey::Delegate { .. },
            CursorStoreToolKind::Delegate
        ) | (
            DelegationMatchKey::Continue { .. },
            CursorStoreToolKind::Continue
        )
    )
}

/// Closed set of reasons a scheduled Cursor-store enrichment lookup can fail
/// to resolve. [`Self::as_str`] values are the exact metric-map keys and are
/// never combined with any secret-bearing text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorEnrichmentFailure {
    InvalidSession,
    NotFound,
    Ambiguous,
    Unreadable,
    Schema,
    NoMatch,
    StoreConflict,
    UnsupportedTool,
    InvalidArgs,
    Deadline,
}

impl CursorEnrichmentFailure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSession => "invalid_session",
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
            Self::Unreadable => "unreadable",
            Self::Schema => "schema",
            Self::NoMatch => "no_match",
            Self::StoreConflict => "store_conflict",
            Self::UnsupportedTool => "unsupported_tool",
            Self::InvalidArgs => "invalid_args",
            Self::Deadline => "deadline",
        }
    }
}

/// True for `CursorStoreError` variants that mean "not there yet" — the
/// coordinator retries these until the deadline rather than giving up.
fn is_retryable_store_error(err: CursorStoreError) -> bool {
    matches!(
        err,
        CursorStoreError::NoExactMatch
            | CursorStoreError::StoreNotFound
            | CursorStoreError::StoreUnreadable
    )
}

/// Maps a terminal (non-retryable) `CursorStoreError` to its closed failure
/// label. Retryable variants never reach this — see [`is_retryable_store_error`].
fn terminal_store_failure(err: CursorStoreError) -> CursorEnrichmentFailure {
    match err {
        CursorStoreError::InvalidSessionId => CursorEnrichmentFailure::InvalidSession,
        CursorStoreError::StoreAmbiguous => CursorEnrichmentFailure::Ambiguous,
        CursorStoreError::SchemaIncompatible => CursorEnrichmentFailure::Schema,
        CursorStoreError::ConflictingRecords => CursorEnrichmentFailure::StoreConflict,
        CursorStoreError::NoExactMatch
        | CursorStoreError::StoreNotFound
        | CursorStoreError::StoreUnreadable => CursorEnrichmentFailure::NoMatch,
    }
}

/// Maps the last retryable miss onto the closed failure label recorded when
/// the lookup budget expires. A deadline with no prior attempt (queue delay
/// consumed the whole window, or a scan itself overran before any miss)
/// stays `Deadline`.
fn retryable_deadline_failure(err: CursorStoreError) -> CursorEnrichmentFailure {
    match err {
        CursorStoreError::StoreNotFound => CursorEnrichmentFailure::NotFound,
        CursorStoreError::NoExactMatch => CursorEnrichmentFailure::NoMatch,
        CursorStoreError::StoreUnreadable => CursorEnrichmentFailure::Unreadable,
        CursorStoreError::InvalidSessionId
        | CursorStoreError::StoreAmbiguous
        | CursorStoreError::SchemaIncompatible
        | CursorStoreError::ConflictingRecords => CursorEnrichmentFailure::Deadline,
    }
}

/// Diagnostic-only label for a retryable miss (never enters metrics).
fn retry_error_label(err: CursorStoreError) -> &'static str {
    match err {
        CursorStoreError::NoExactMatch => "no_exact_match",
        CursorStoreError::StoreNotFound => "store_not_found",
        CursorStoreError::StoreUnreadable => "store_unreadable",
        CursorStoreError::InvalidSessionId
        | CursorStoreError::StoreAmbiguous
        | CursorStoreError::SchemaIncompatible
        | CursorStoreError::ConflictingRecords => "retryable",
    }
}

/// `min(CURSOR_STORE_RETRY_INITIAL * 2^attempt, remaining)`, saturating
/// rather than panicking if `attempt` is large enough to overflow the
/// exponent (the outer deadline loop stops well before that in practice).
fn backoff_for_attempt(attempt: u32, remaining: Duration) -> Duration {
    let multiplier = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
    let scaled = CURSOR_STORE_RETRY_INITIAL
        .checked_mul(multiplier)
        .unwrap_or(Duration::MAX);
    std::cmp::min(scaled, remaining)
}

fn is_terminal_tool_call_status(status: &str) -> bool {
    matches!(status, "completed" | "failed")
}

/// Cheap event-shape gate for [`CursorStoreEnricher::maybe_schedule`]. Only
/// `ToolCall` / `ToolCallUpdate` frames whose title is exactly
/// `CURSOR_IDENTITYLESS_MCP_TITLE` and whose status isn't terminal pass; a
/// `ToolCallUpdate` with `title: None` is not the Cursor shape (its title
/// never repeats after the announcing `ToolCall` — see `acp::lifecycle`).
fn cursor_identityless_shape(payload: &AcpEvent) -> Option<(&str, Option<&str>)> {
    match payload {
        AcpEvent::ToolCall {
            tool_call_id,
            title,
            status,
            raw_input,
            ..
        } => {
            if is_terminal_tool_call_status(status) || title != CURSOR_IDENTITYLESS_MCP_TITLE {
                return None;
            }
            Some((tool_call_id.as_str(), raw_input.as_deref()))
        }
        AcpEvent::ToolCallUpdate {
            tool_call_id,
            title,
            status,
            raw_input,
            ..
        } => {
            if status.as_deref().is_some_and(is_terminal_tool_call_status) {
                return None;
            }
            if title.as_deref() != Some(CURSOR_IDENTITYLESS_MCP_TITLE) {
                return None;
            }
            Some((tool_call_id.as_str(), raw_input.as_deref()))
        }
        _ => None,
    }
}

/// True when `raw_input` is the empty shape Cursor's first announcement of
/// an MCP tool call ships: absent, blank, or the literal `"{}"`.
fn raw_input_is_cursor_empty_shape(raw_input: Option<&str>) -> bool {
    raw_input.is_none_or(|s| matches!(s.trim(), "" | "{}"))
}

/// Removes `key` from the shared in-flight set on drop, so every exit path
/// out of the spawned lookup task (normal return, early `return`, or a
/// panic) releases it — never leaving a permanently-stuck dedupe entry.
struct InFlightGuard {
    set: Arc<Mutex<HashSet<(String, String)>>>,
    key: (String, String),
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut guard = self.set.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&self.key);
    }
}

/// Coordinates bounded, best-effort backfill of identity-less parent tool
/// calls (Cursor's `"MCP: tool"` announcement) by retrying a read-only
/// lookup into Cursor's own on-disk store until either a match resolves or
/// [`CURSOR_STORE_LOOKUP_DEADLINE`] expires.
///
/// [`Self::maybe_schedule`] is the only entry point and is synchronous and
/// non-blocking: it does a cheap event-shape check, an in-flight dedupe
/// insert, and a `tokio::spawn` — never a session lookup, SQLite read,
/// semaphore acquire, or retry sleep. All of that happens inside the spawned
/// task, off whatever serial worker called `maybe_schedule`.
#[derive(Clone)]
pub struct CursorStoreEnricher {
    store: Arc<dyn CursorStoreLookup>,
    sessions: Arc<dyn CursorEnrichmentSessionLookup>,
    broker: Arc<DelegationBroker>,
    metrics: Arc<DelegationMetrics>,
    in_flight: Arc<Mutex<HashSet<(String, String)>>>,
    semaphore: Arc<Semaphore>,
    terminal_warn_throttle: Arc<Mutex<Option<Instant>>>,
}

impl CursorStoreEnricher {
    pub fn new(
        store: Arc<dyn CursorStoreLookup>,
        sessions: Arc<dyn CursorEnrichmentSessionLookup>,
        broker: Arc<DelegationBroker>,
        metrics: Arc<DelegationMetrics>,
    ) -> Self {
        Self {
            store,
            sessions,
            broker,
            metrics,
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            semaphore: Arc::new(Semaphore::new(CURSOR_STORE_SCAN_PERMITS)),
            terminal_warn_throttle: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    pub fn in_flight_len_for_test(&self) -> usize {
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Cheap event-shape gate + in-flight insert + spawn. Must return
    /// without awaiting session locks, SQLite, the semaphore, or retry
    /// sleeps — see the type docs.
    pub fn maybe_schedule(&self, envelope: &EventEnvelope) {
        let Some((tool_call_id, raw_input)) = cursor_identityless_shape(&envelope.payload) else {
            return;
        };
        if !raw_input_is_cursor_empty_shape(raw_input) {
            return;
        }
        let key = (envelope.connection_id.clone(), tool_call_id.to_string());
        {
            let mut guard = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            if !guard.insert(key.clone()) {
                return;
            }
        }
        let started = Instant::now();
        let this = self.clone();
        tokio::spawn(async move {
            let (parent_connection_id, tool_call_id) = key.clone();
            let _guard = InFlightGuard {
                set: this.in_flight.clone(),
                key,
            };
            this.run_lookup(&parent_connection_id, &tool_call_id, started)
                .await;
        });
    }

    /// The bounded retry loop. Runs entirely inside the task `maybe_schedule`
    /// spawns; never called from the serial worker directly.
    async fn run_lookup(&self, parent_connection_id: &str, tool_call_id: &str, started: Instant) {
        let deadline = started + CURSOR_STORE_LOOKUP_DEADLINE;

        let Some(session) = self.sessions.lookup(parent_connection_id).await else {
            // Missing connection: skip silently, no store call, no metric.
            return;
        };
        if session.agent_type != AgentType::Cursor {
            // Non-Cursor connection: skip silently, no store call, no metric.
            return;
        }
        if validate_cursor_session_id(&session.external_session_id).is_err() {
            self.metrics
                .record_cursor_enrichment_failed(CursorEnrichmentFailure::InvalidSession);
            return;
        }
        // Only now — a live Cursor connection with a validated session id —
        // does this lookup count as scheduled.
        self.metrics.record_cursor_enrichment_scheduled();

        let mut attempt: u32 = 0;
        let mut last_retryable: Option<CursorStoreError> = None;
        loop {
            let now = Instant::now();
            if now >= deadline {
                self.record_deadline(last_retryable);
                return;
            }
            let remaining = deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                self.record_deadline(last_retryable);
                return;
            }

            let permit =
                match tokio::time::timeout(remaining, self.semaphore.clone().acquire_owned()).await
                {
                    Ok(Ok(permit)) => permit,
                    _ => {
                        self.record_deadline(last_retryable);
                        return;
                    }
                };

            let remaining_before_scan = deadline.saturating_duration_since(Instant::now());
            if remaining_before_scan.is_zero() {
                drop(permit);
                self.record_deadline(last_retryable);
                return;
            }

            let store = self.store.clone();
            let session_id = session.external_session_id.clone();
            let owned_tool_call_id = tool_call_id.to_string();
            let join = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                store.lookup(&session_id, &owned_tool_call_id)
            })
            .await;

            if Instant::now() >= deadline {
                self.record_deadline(last_retryable);
                return;
            }

            let result = match join {
                Ok(result) => result,
                Err(_join_error) => {
                    self.metrics
                        .record_cursor_enrichment_failed(CursorEnrichmentFailure::Unreadable);
                    return;
                }
            };

            match result {
                Ok(stored) => {
                    let Some(kind) = classify_store_tool_name(&stored.tool_name) else {
                        self.metrics.record_cursor_enrichment_failed(
                            CursorEnrichmentFailure::UnsupportedTool,
                        );
                        return;
                    };
                    let Some(match_key) = extract_delegation_match_key_from_value(&stored.args)
                    else {
                        self.metrics
                            .record_cursor_enrichment_failed(CursorEnrichmentFailure::InvalidArgs);
                        return;
                    };
                    if !key_matches_store_tool(&match_key, kind) {
                        self.metrics
                            .record_cursor_enrichment_failed(CursorEnrichmentFailure::InvalidArgs);
                        return;
                    }
                    let backfill_result = self
                        .broker
                        .backfill_identityless_match_key(
                            parent_connection_id,
                            tool_call_id,
                            match_key,
                        )
                        .await;
                    self.metrics
                        .record_cursor_enrichment_backfill(backfill_result);
                    self.metrics
                        .record_cursor_enrichment_resolved(started.elapsed());
                    return;
                }
                Err(err) if is_retryable_store_error(err) => {
                    last_retryable = Some(err);
                    tracing::trace!(
                        target: "cursor_enrichment",
                        agent_type = agent_type_label(session.agent_type),
                        parent_connection_id = %parent_connection_id,
                        tool_call_id = %tool_call_id,
                        failure_class = retry_error_label(err),
                        attempt,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "cursor store enrichment lookup miss; retrying"
                    );
                    let remaining_after = deadline.saturating_duration_since(Instant::now());
                    if remaining_after.is_zero() {
                        self.record_deadline(last_retryable);
                        return;
                    }
                    let backoff = backoff_for_attempt(attempt, remaining_after);
                    attempt = attempt.saturating_add(1);
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                Err(err) => {
                    let failure = terminal_store_failure(err);
                    self.warn_terminal_failure_throttled(
                        failure,
                        parent_connection_id,
                        tool_call_id,
                        attempt,
                        started.elapsed(),
                    );
                    self.metrics.record_cursor_enrichment_failed(failure);
                    return;
                }
            }
        }
    }

    fn record_deadline(&self, last_retryable: Option<CursorStoreError>) {
        let failure = last_retryable
            .map(retryable_deadline_failure)
            .unwrap_or(CursorEnrichmentFailure::Deadline);
        self.metrics.record_cursor_enrichment_failed(failure);
    }

    /// One WARN line per [`CURSOR_STORE_TERMINAL_WARN_WINDOW`] for terminal
    /// store failures — a misconfigured/unreadable store shouldn't spam.
    fn warn_terminal_failure_throttled(
        &self,
        failure: CursorEnrichmentFailure,
        parent_connection_id: &str,
        tool_call_id: &str,
        attempt: u32,
        elapsed: Duration,
    ) {
        let now = Instant::now();
        let should_warn = {
            let mut guard = self
                .terminal_warn_throttle
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let due = guard
                .map(|last| {
                    now.saturating_duration_since(last) >= CURSOR_STORE_TERMINAL_WARN_WINDOW
                })
                .unwrap_or(true);
            if due {
                *guard = Some(now);
            }
            due
        };
        if should_warn {
            tracing::warn!(
                target: "cursor_enrichment",
                parent_connection_id = %parent_connection_id,
                tool_call_id = %tool_call_id,
                failure_class = failure.as_str(),
                attempt,
                elapsed_ms = elapsed.as_millis() as u64,
                "cursor store enrichment terminal failure"
            );
        }
    }
}
