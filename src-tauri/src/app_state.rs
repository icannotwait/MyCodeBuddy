use std::path::PathBuf;
use std::sync::Arc;

use crate::acp::delegation::broker::DelegationBroker;
use crate::acp::delegation::continuation::store::{ContinuationStore, DbContinuationStore};
use crate::acp::delegation::continuation::coordinator::DelegationContinuationCoordinator;
use crate::acp::delegation::lease::CompanionLeaseRegistry;
use crate::acp::delegation::listener::TokenRegistry;
use crate::acp::delegation::metrics::DelegationMetrics;
use crate::acp::manager::ConnectionManager;
use crate::acp::InternalEventBus;
use crate::auto_title::{AutoTitleCoordinator, InternalAgentSessionRegistry};
use crate::chat_channel::manager::ChatChannelManager;
use crate::commands::conversation_experience::ConversationExperienceMutationGate;
use crate::commands::delegation::DelegationRuntimeSettings;
use crate::db::AppDatabase;
use crate::pet_state_mapper::PetStateHandle;
use crate::reference_search::ReferenceSearchRegistry;
use crate::terminal::manager::TerminalManager;
use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};
use crate::web::WebServerState;
use crate::workspace_transfer::WorkspaceTransferManager;

pub struct AppState {
    pub db: AppDatabase,
    pub connection_manager: ConnectionManager,
    pub terminal_manager: TerminalManager,
    pub event_broadcaster: Arc<WebEventBroadcaster>,
    /// Process-wide bus for typed `Arc<EventEnvelope>` delivery to
    /// in-process consumers (lifecycle, pet state mapper, chat-channel
    /// subscribers). Distinct from `event_broadcaster`, which carries
    /// JSON-shaped `WebEvent`s for transport-bound delivery.
    pub acp_event_bus: Arc<InternalEventBus>,
    pub emitter: EventEmitter,
    pub data_dir: PathBuf,
    /// Shared registry of internal-only agent sessions (auto-title runs, etc.).
    /// One instance is cloned into Tauri managed state and embedded Axum state.
    pub internal_sessions: Arc<InternalAgentSessionRegistry>,
    /// Durable automatic-title worker. Shared by lifecycle, Tauri commands, and Axum.
    pub auto_title_coordinator: Arc<AutoTitleCoordinator>,
    /// Process-local mutation gate for conversation-experience settings.
    pub conversation_experience_gate: Arc<ConversationExperienceMutationGate>,
    /// Process-wide guarded pull-job registry for incremental reference search.
    /// Desktop/server construct one factory+registry; embedded Axum clones the
    /// same managed instance so both transports share limit epochs.
    pub reference_search_registry: Arc<ReferenceSearchRegistry>,
    pub web_server_state: WebServerState,
    pub chat_channel_manager: ChatChannelManager,
    pub workspace_transfer: Arc<WorkspaceTransferManager>,
    /// Latest ambient `PetState` written by `pet_state_subscriber_task`.
    /// Read by `pet_get_current_state` so a freshly-opened pet window can
    /// pick up the current state without waiting for the next transition.
    pub pet_state: PetStateHandle,
    /// Multi-agent delegation broker. Spawned in both desktop and server
    /// mode at startup; the UDS listener task forwards incoming companion
    /// requests here. v1 uses the default `DelegationConfig`; settings UI
    /// hot-swaps via `delegation_broker.set_config`.
    pub delegation_broker: Arc<DelegationBroker>,
    pub continuation_coordinator: Arc<DelegationContinuationCoordinator>,
    /// Process-local delegation reliability metrics (route/accepted/terminal/
    /// wait/cancel). Shared with broker, supervisor, listener, and route launch.
    pub delegation_metrics: Arc<DelegationMetrics>,
    /// Live route_policy / stalled_after_seconds / enabled snapshot shared by
    /// route resolution and the soft-watchdog supervisor. Updated only after
    /// a successful settings transaction (or one clamped load at startup).
    pub delegation_runtime_settings: DelegationRuntimeSettings,
    /// Per-launch ephemeral tokens identifying parent ACP connections.
    /// Registered when `load_mcp_servers_for_agent` injects the
    /// `codeg-mcp` MCP entry, revoked on parent teardown.
    pub delegation_tokens: Arc<TokenRegistry>,
    /// Authenticated companion ready-lease registry. Shared with the
    /// listener and MCP injection so Codeg routes wait for ready before
    /// emitting Connected.
    pub delegation_leases: Arc<CompanionLeaseRegistry>,
    /// Absolute path of the UDS / named pipe the companion connects to.
    /// PID-scoped so multiple codeg processes on the same host don't fight.
    pub delegation_socket_path: PathBuf,
    /// Hot-swappable live-feedback (`check_user_feedback`) enable flag. Shared
    /// with the `DelegationInjection` so MCP injection reads it, and updated by
    /// the feedback settings command on save. Populated at startup by
    /// `apply_persisted_feedback_config`.
    pub feedback_config: crate::acp::feedback::FeedbackRuntimeConfig,
    /// Hot-swappable ask-user-question (`ask_user_question`) enable flag. Shared
    /// with the `DelegationInjection` so MCP injection reads it, and updated by
    /// the question settings command on save. Populated at startup by
    /// `apply_persisted_question_config`.
    pub question_config: crate::acp::question::QuestionRuntimeConfig,
    /// Hot-swappable get-session-info (`get_session_info`) enable flag. Shared
    /// with the `DelegationInjection` so MCP injection reads it, and updated by
    /// the session-info settings command on save. Populated at startup by
    /// `apply_persisted_session_info_config`.
    pub session_info_config: crate::acp::session_info::SessionInfoRuntimeConfig,
    /// Serializes mutually-exclusive system operations — in-place
    /// self-update, restart, rollback — so a second click can't race a
    /// download/swap already in flight. Handlers `try_lock` and reject when
    /// held (an upgrade is already running).
    pub system_op_lock: Arc<tokio::sync::Mutex<()>>,
    /// Source of truth for an in-flight / completed app self-update, shared by
    /// the desktop (tauri-plugin-updater) and server (in-place swap) paths.
    /// The upgrade UI subscribes to it and re-syncs from a snapshot on mount,
    /// so download progress survives settings-page navigation and reloads.
    pub update_state: crate::update::AppUpdateStateHandle,
}

pub fn default_system_op_lock() -> Arc<tokio::sync::Mutex<()>> {
    Arc::new(tokio::sync::Mutex::new(()))
}

pub fn default_update_state() -> crate::update::AppUpdateStateHandle {
    crate::update::new_update_state_handle()
}

pub fn default_connection_manager() -> ConnectionManager {
    ConnectionManager::new()
}

pub fn default_terminal_manager() -> TerminalManager {
    TerminalManager::new()
}

pub fn default_chat_channel_manager() -> ChatChannelManager {
    ChatChannelManager::new()
}

/// Named result of [`build_delegation_stack`]. Keeps the shared desktop/
/// server/test bootstrap surface readable without a 9-element tuple.
pub struct DelegationStack {
    pub broker: Arc<DelegationBroker>,
    pub tokens: Arc<TokenRegistry>,
    pub leases: Arc<CompanionLeaseRegistry>,
    pub socket_path: PathBuf,
    pub feedback: crate::acp::feedback::FeedbackRuntimeConfig,
    pub ask: crate::acp::question::QuestionRuntimeConfig,
    pub sessions: crate::acp::session_info::SessionInfoRuntimeConfig,
    pub runtime_settings: DelegationRuntimeSettings,
    pub metrics: Arc<DelegationMetrics>,
    pub continuation_store: Arc<dyn ContinuationStore>,
    pub continuation_coordinator: Arc<DelegationContinuationCoordinator>,
}

/// Build the delegation broker + token registry + per-process UDS socket
/// path. Shared between codeg-server bootstrap and the Tauri `setup` block
/// so both modes apply identical depth limit + timeout defaults.
///
/// The listener task is _not_ spawned here — callers spawn it after they
/// own an `Arc<AppState>` (or the relevant pieces) so the listener can
/// borrow the long-lived state without circular Arc shenanigans.
pub fn build_delegation_stack(
    connection_manager: &ConnectionManager,
    db_conn: sea_orm::DatabaseConnection,
    data_dir: PathBuf,
) -> DelegationStack {
    use crate::acp::connection::DelegationInjection;
    use crate::acp::delegation::attention::{DbDelegationAttentionStore, DelegationAttentionStore};
    use crate::acp::delegation::broker::{
        ChildStatusLookup, ConversationDepthLookup, DbChildStatusLookup, DbDepthLookup,
    };
    use crate::acp::delegation::event_emitter::{
        ConnectionManagerEventEmitter, DelegationEventEmitter,
    };
    use crate::acp::delegation::listener::default_socket_path;
    use crate::acp::delegation::live_reply::{
        ChildLiveReplyLookup, ConnectionManagerLiveReplyLookup,
    };
    use crate::acp::delegation::meta_writer::{ConnectionManagerMetaWriter, DelegationMetaWriter};
    use crate::acp::delegation::spawner::ConnectionSpawner;
    use crate::acp::delegation::store::{DbDelegationTaskStore, DelegationTaskStore};
    use crate::acp::manager::ConnectionManagerSpawner;

    let cm_arc = Arc::new(connection_manager.clone_ref());
    let db_arc = Arc::new(AppDatabase {
        conn: db_conn.clone(),
    });
    let continuation_store =
        Arc::new(DbContinuationStore::new(db_conn.clone())) as Arc<dyn ContinuationStore>;
    connection_manager.install_continuation_store(continuation_store.clone());
    // Create the shared runtime handle before the spawner so child launches
    // always resolve against the live watch snapshot (never a second DB load).
    let runtime_settings = DelegationRuntimeSettings::default();
    let spawner = Arc::new(ConnectionManagerSpawner {
        manager: cm_arc.clone(),
        db: db_arc.clone(),
        data_dir: Arc::new(data_dir),
        runtime: runtime_settings.clone(),
    }) as Arc<dyn ConnectionSpawner>;
    let depth_lookup =
        Arc::new(DbDepthLookup { db: db_arc.clone() }) as Arc<dyn ConversationDepthLookup>;
    let task_store =
        Arc::new(DbDelegationTaskStore::new(db_arc.clone())) as Arc<dyn DelegationTaskStore>;
    let attention_store = Arc::new(DbDelegationAttentionStore::new(db_arc.clone()))
        as Arc<dyn DelegationAttentionStore>;
    let status_lookup = Arc::new(DbChildStatusLookup { db: db_arc }) as Arc<dyn ChildStatusLookup>;
    let meta_writer = Arc::new(ConnectionManagerMetaWriter {
        manager: cm_arc.clone(),
    }) as Arc<dyn DelegationMetaWriter>;
    let live_reply_lookup = Arc::new(ConnectionManagerLiveReplyLookup {
        manager: cm_arc.clone(),
    }) as Arc<dyn ChildLiveReplyLookup>;
    let event_emitter = Arc::new(ConnectionManagerEventEmitter {
        manager: cm_arc.clone(),
    })
        as Arc<dyn DelegationEventEmitter>;
    let delegation_metrics = Arc::new(DelegationMetrics::default());
    let broker = Arc::new(
        DelegationBroker::with_writers(spawner, depth_lookup, meta_writer, event_emitter)
            .with_task_store(task_store)
            .with_attention_store(attention_store)
            .with_status_lookup(status_lookup)
            .with_live_reply_lookup(live_reply_lookup)
            .with_metrics(delegation_metrics.clone()),
    );
    let continuation_port = Arc::new(
        crate::acp::delegation::continuation::coordinator::ManagerContinuationPort::new(
            cm_arc,
        ),
    );
    let continuation_coordinator = Arc::new(DelegationContinuationCoordinator::new(
        continuation_store.clone(),
        broker.clone(),
        delegation_metrics.clone(),
        continuation_port,
        Arc::new(
            crate::acp::delegation::continuation::coordinator::SystemContinuationClock::new(),
        ),
    ));
    let tokens = Arc::new(TokenRegistry::with_continuation_coordinator(
        continuation_coordinator.clone(),
    ));
    let leases = Arc::new(CompanionLeaseRegistry::default());
    let socket_path = default_socket_path(&std::env::temp_dir());
    let feedback = crate::acp::feedback::FeedbackRuntimeConfig::new();
    let ask = crate::acp::question::QuestionRuntimeConfig::new();
    let sessions = crate::acp::session_info::SessionInfoRuntimeConfig::new();

    // Soft-supervisor wake channel: tx side is shared via SupervisorWake on
    // injection + broker; rx is taken once at desktop/server startup after
    // reconcile (see spawn_delegation_supervisor).
    let (wake_tx, wake_rx) = tokio::sync::mpsc::channel(64);
    let supervisor_wake = crate::acp::delegation::supervisor::SupervisorWake::new(wake_tx);
    broker.set_supervisor_wake(supervisor_wake.clone());

    // Install the injection on the manager so spawn_agent picks it up
    // without an extra parameter at every call site.
    connection_manager.install_delegation(DelegationInjection {
        broker: broker.clone(),
        continuation_coordinator: Arc::downgrade(&continuation_coordinator),
        parent_connection_exit_causes: Arc::new(
            crate::acp::connection::ParentConnectionExitCauses::default(),
        ),
        tokens: tokens.clone(),
        leases: leases.clone(),
        socket_path: socket_path.clone(),
        feedback: feedback.clone(),
        ask: ask.clone(),
        sessions: sessions.clone(),
        // Same backing manager as the listener's question lookup; used only by
        // the run_connection teardown guard to reclaim a parked ask.
        questions: Arc::new(crate::acp::manager::ConnectionManagerQuestionLookup {
            manager: Arc::new(connection_manager.clone_ref()),
        }) as Arc<dyn crate::acp::question::SessionQuestionAccess>,
        supervisor_wake,
        metrics: delegation_metrics.clone(),
    });

    // Park the wake receiver on the broker until startup takes it.
    broker.park_supervisor_wake_rx(wake_rx);

    DelegationStack {
        broker,
        tokens,
        leases,
        socket_path,
        feedback,
        ask,
        sessions,
        runtime_settings,
        metrics: delegation_metrics,
        continuation_store,
        continuation_coordinator,
    }
}

/// Spawn the soft supervisor after Task 8 reconcile and with/before the
/// delegation listener. Observe-only: no cancel/settle/route capability.
pub fn spawn_delegation_supervisor(
    broker: Arc<crate::acp::delegation::broker::DelegationBroker>,
    connection_manager: crate::acp::manager::ConnectionManager,
    runtime: &DelegationRuntimeSettings,
) {
    use crate::acp::delegation::broker::{BrokerObservationSink, BrokerObservationSource};
    use crate::acp::delegation::supervisor::{DelegationSupervisor, SystemClock};

    let Some(wake_rx) = broker.take_supervisor_wake_rx() else {
        tracing::warn!("[delegation] supervisor wake rx already taken; skipping soft supervisor");
        return;
    };

    let threshold_rx = {
        let (tx, rx) = tokio::sync::watch::channel(runtime.snapshot().stalled_after_seconds);
        let mut settings_rx = runtime.subscribe();
        let bridge = async move {
            loop {
                if settings_rx.changed().await.is_err() {
                    break;
                }
                let secs = settings_rx.borrow().stalled_after_seconds;
                if tx.send(secs).is_err() {
                    break;
                }
            }
        };
        #[cfg(feature = "tauri-runtime")]
        tauri::async_runtime::spawn(bridge);
        #[cfg(not(feature = "tauri-runtime"))]
        tokio::spawn(bridge);
        rx
    };

    let source = Arc::new(BrokerObservationSource {
        broker: broker.clone(),
        manager: Arc::new(connection_manager.clone_ref()),
    });
    let sink = Arc::new(BrokerObservationSink {
        broker: broker.clone(),
    });
    let supervisor = DelegationSupervisor::with_metrics(
        source,
        sink,
        Arc::new(SystemClock),
        threshold_rx,
        wake_rx,
        broker.metrics(),
    );
    let run = async move {
        supervisor.run().await;
        tracing::info!("[delegation] soft supervisor exited");
    };
    #[cfg(feature = "tauri-runtime")]
    tauri::async_runtime::spawn(run);
    #[cfg(not(feature = "tauri-runtime"))]
    tokio::spawn(run);
}

impl AppState {
    /// Test-only constructor: build an `AppState` wired to an in-memory
    /// database and a `WebOnly` event emitter. Suitable for axum-test driven
    /// HTTP integration tests where no Tauri runtime is available.
    ///
    /// `data_dir` is a temp directory; handlers that touch it must use
    /// `tempfile::tempdir()` and pass the resulting path in.
    ///
    /// Uses an inert title runner that panics if invoked. Prefer
    /// [`Self::new_for_test_with_title_runner`] for deterministic Task 9
    /// integration coverage.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn new_for_test(db: crate::db::AppDatabase, data_dir: PathBuf) -> Self {
        // Ordinary tests never construct a production manager driver.
        Self::new_for_test_with_title_runner(
            db,
            data_dir,
            Arc::new(crate::auto_title::coordinator::InertTitleAgentRunner),
        )
    }

    /// Test constructor that injects a deterministic [`crate::auto_title::TitleAgentRunner`].
    /// Shares the same process-local [`ConversationExperienceMutationGate`] wiring
    /// as desktop/server/embedded constructors (one gate per AppState).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn new_for_test_with_title_runner(
        db: crate::db::AppDatabase,
        data_dir: PathBuf,
        runner: Arc<dyn crate::auto_title::TitleAgentRunner>,
    ) -> Self {
        use crate::acp::{EventBusMetrics, InternalEventBus};
        use crate::web::event_bridge::WebEventBroadcaster;

        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let metrics = Arc::new(EventBusMetrics::default());
        let acp_event_bus = Arc::new(InternalEventBus::new(metrics));
        let emitter = EventEmitter::web_only(broadcaster.clone(), acp_event_bus.clone());

        let connection_manager = default_connection_manager();
        let stack = build_delegation_stack(&connection_manager, db.conn.clone(), data_dir.clone());
        // Freshly migrated fixture DB: empty registry is correct and keeps
        // this constructor synchronous for existing integration tests.
        let internal_sessions =
            InternalAgentSessionRegistry::new_empty_for_test(db.conn.clone(), &data_dir)
                .expect("empty internal session registry for tests");
        let title_db = Arc::new(crate::db::AppDatabase {
            conn: db.conn.clone(),
        });
        // Never start the production notification worker from test constructors.
        let auto_title_coordinator =
            AutoTitleCoordinator::new(title_db, runner, EventEmitter::Noop);
        let conversation_experience_gate = Arc::new(ConversationExperienceMutationGate::default());
        // Synchronous test constructor installs the production factory at the
        // default limit. Async fixtures that need another value call
        // `set_limit` before wrapping the state in Arc / sharing it.
        let reference_search_registry = crate::reference_search::ReferenceSearchRegistry::new(
            crate::commands::conversation_experience::DEFAULT_REFERENCE_SEARCH_LIMIT,
            Arc::new(crate::reference_search::ProductionReferenceSourceFactory {
                db: db.conn.clone(),
            }),
        );

        Self {
            db,
            connection_manager,
            terminal_manager: default_terminal_manager(),
            event_broadcaster: broadcaster,
            acp_event_bus,
            emitter,
            data_dir,
            internal_sessions,
            auto_title_coordinator,
            conversation_experience_gate,
            reference_search_registry,
            web_server_state: crate::web::WebServerState::new(),
            chat_channel_manager: default_chat_channel_manager(),
            workspace_transfer: Arc::new(
                crate::workspace_transfer::WorkspaceTransferManager::new_for_tests(
                    std::time::Duration::from_secs(60),
                ),
            ),
            pet_state: crate::pet_state_mapper::new_pet_state_handle(),
            delegation_broker: stack.broker,
            continuation_coordinator: stack.continuation_coordinator,
            delegation_metrics: stack.metrics,
            delegation_runtime_settings: stack.runtime_settings,
            delegation_tokens: stack.tokens,
            delegation_leases: stack.leases,
            delegation_socket_path: stack.socket_path,
            feedback_config: stack.feedback,
            question_config: stack.ask,
            session_info_config: stack.sessions,
            system_op_lock: default_system_op_lock(),
            update_state: default_update_state(),
        }
    }
}
