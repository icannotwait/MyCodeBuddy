Shared ACP Multi-Client Session Broker Design
Status
Approved in the 2026-08-14 design discussion for implementation planning.

This specification defines process-local concurrent access to one root ACP conversation from a desktop browser, a phone, and multiple browser tabs that all connect to the same codeg-server process. It replaces client-side find-then-connect ownership for server-hosted root conversations with an atomic backend session broker.

No implementation plan has been approved yet. Implementation planning must preserve the invariants and wire behavior in this document.

Executive Decision
ConnectionManager gains a process-local SharedSessionBroker. The broker is the lifecycle owner of server-hosted root ACP connections. Browser clients are equal co-controllers represented by expiring leases; no browser is the process owner.

The public entry point is an atomic connect_or_attach operation keyed by the persisted conversation. It reserves a stable connection incarnation before agent bootstrap starts, returns without waiting for ACP or companion initialization, and lets every client observe bootstrap, ready, failure, queue, and turn state through the existing subscribe-with-snapshot WebSocket stream.

Every accepted prompt enters one process-local FIFO queue. At most one item is active on the ACP connection. Permission and question responses are one-shot: any attached client may answer, and the first valid response wins. Any attached client may stop the current turn when it supplies the exact current turn_id. Stopping a turn does not discard queued prompts.

An idle connection is retained for 15 minutes after it simultaneously becomes Ready, has no client leases, and has no work. Executing turns, pending user input, queued prompts, continuation waits, and background work are never reaped by the client-idle sweep. Broker state, leases, and queued prompts are not durable and are cleared by a server restart.

The design applies to every built-in ACP agent and standard custom ACP agent. Companion readiness is a route capability, not a Codex-specific branch. When a resolved route explicitly requires the Codeg companion, companion failure fails the whole connection and never falls back to a native route.

Motivation and Current Failure
The current web flow has useful multi-client pieces but no single authority for connection creation and lifetime:

AcpConnectionsProvider first calls acp_find_connection_for_conversation and then calls acp_connect if the lookup returns no live connection.
The lookup and spawn are separate operations. Two devices can both observe absence and both start a connection.
Existing spawn de-duplication is keyed by (agent_type, working_dir, external_session_id) and only applies when an external session id already exists. It does not reserve a persisted conversation during the bootstrap window.
/acp_connect waits for route bootstrap and returns only a connection-id string. A second device has no stable broker record to attach to while the first request is waiting for companion readiness.
Every ordinary web connection uses the owner label "web"; this does not identify a device or tab and incorrectly makes page teardown relevant to process ownership.
The existing WS attach protocol already provides atomic snapshot/replay and per-connection live events, but only after a connection can be discovered.
Prompt admission intentionally rejects a second prompt while turn_in_flight is true. That prevents silent loss inside the ACP connection loop, but it does not implement the shared FIFO product behavior.
The idle sweep uses frontend touches and a three-minute default. Normal owner teardown and abnormal tab loss therefore have different lifecycle behavior.
The observed mobile timeout was not a Cloudflare Tunnel failure. The ACP session initialized quickly and then waited for Codeg companion readiness. The request failed with CompanionInitializationFailed, followed by a bounded cleanup wait. A later fresh Codex session connected normally. The product bug is that bootstrap and cleanup are exposed as one long owner-scoped HTTP operation while another client has no atomic attach path.

Goals
Support concurrent control of one root conversation from multiple devices and tabs connected to one codeg-server process.
Guarantee one live root connection incarnation per persisted conversation and resolved launch identity.
Make connection reservation, bootstrap observation, and client attachment atomic from the callers' perspective.
Return connect requests promptly, including while ACP and companion bootstrap are still running.
Preserve stream state across refresh and transient network loss through the existing snapshot/replay protocol.
Serialize accepted user prompts through one observable FIFO without silent loss, reordering, or duplicate retry insertion.
Make permission, question, plan approval, queue cancellation, and turn stop operations race-safe across clients.
Let a sending device disconnect without deleting its accepted queued prompts.
Keep active, waiting, queued, and background-working sessions alive even when every client is offline.
Reap only genuinely idle, clientless sessions after a 15-minute grace.
Apply the same broker semantics to all current ACP agents and future standard custom agents.
Fail closed when an explicitly required Codeg companion is unavailable.
Retain current local-desktop handoff, delegated-child ownership, automation, probe, and hidden-generation behavior outside the shared root boundary.
Add bounded memory use, typed errors, secret-safe diagnostics, and tests for concurrency races.
Non-Goals
Multi-user tenancy, per-user ACLs, or independent write roles. All clients authenticated with the server operator token are one trusted operator.
Running the same conversation on multiple codeg-server replicas.
Persisting client leases, FIFO items, bootstrap state, event buffers, or in-flight turn state across process restart.
Automatically retrying a prompt that may have reached an agent before a process or transport failure.
Reordering or editing queued prompts in place.
Restoring an active ACP subprocess after the server process dies.
Replacing the ACP protocol or individual agent adapters.
Rewriting local Tauri pop-out ownership handoff in this iteration.
Giving normal root write access to delegated child, internal probe, title, translation, or other hidden connections.
Supporting old cached web clients indefinitely after the shared-session wire API is enabled.
Product Decisions


Area	Decision
Scope	One codeg-server, many devices/tabs
Control	Every attached client is an equal co-controller
Root connection owner	Server-side broker
Connection creation	Atomic connect_or_attach
Bootstrap response	Return after reservation, do not wait for ready
Prompt concurrency	Shared FIFO
Accepted prompt after sender disconnect	Keep it
Queue mutation	Cancel unstarted item only; no edit/reorder
User-input races	First valid response wins
Turn stop	Any attached client, exact turn_id required
Queue after stop	Preserve; resume after cancellation quarantine
Idle grace	15 minutes
Active/background session with zero clients	Never client-idle reap
Server restart	No queue or lease recovery
Required companion failure	Fail the connection; no native fallback
ACP coverage	All built-ins plus conforming custom ACP agents
Alternatives Considered
Client Election and Retry
Clients could retain the current lookup-then-connect flow and add discovery delays, BroadcastChannel, or an elected browser leader. BroadcastChannel does not coordinate a phone with a desktop, browser background scheduling is not a reliable lock, and every approach retains a server-side TOCTOU window. It also cannot authoritatively serialize prompts or lifecycle decisions.

Rejected.

Atomic Broker Inside ConnectionManager
A small broker owns logical root sessions while reusing AgentConnection, ACP adapters, SessionState, and the WS event stream. The broker supplies the missing single-flight bootstrap, client leases, queue, and idle predicate. It does not rewrite the connection loop.

Selected. It fixes the correctness boundary with a contained migration and can later evolve toward actors without making that refactor a prerequisite.

Full Session Actor Rewrite
One actor/mailbox per conversation could own every connection command, prompt, input response, and lifecycle transition. This produces a clean long-term model but would simultaneously replace established ownership handoff, delegation, cancellation, and connection-loop behavior. The regression surface is disproportionate to the immediate concurrency problem.

Deferred.

Scope and Terminology
Shared root session
A user-facing root conversation hosted by codeg-server. It may use any built-in agent (ClaudeCode, Codex, OpenCode, Gemini, Cline, Hermes, CodeBuddy, KimiCode, Pi, Grok, or Cursor) or a registered custom ACP agent.

Client instance
One loaded frontend document. A phone page, desktop page, and each browser tab are distinct client instances. A client instance id is diagnostic and idempotency context, not authentication.

Lease
A random, process-local capability proving that one authenticated client instance is currently attached to one shared root session. A lease influences presence and idle eligibility; it does not own the agent process.

Connection incarnation
One attempt to launch or resume an ACP process. It has one preallocated connection_id and one monotonically increasing broker generation. A failed retry creates a new incarnation and generation.

Broker phase
The root session's public bootstrap/lifecycle phase. This is distinct from the existing ACP ConnectionStatus, which describes the underlying connection loop.

Work
Any condition that makes client-idle reclamation unsafe: an active turn, cancellation quarantine, pending permission/question/plan approval, queued prompt, continuation wait, active delegation, unresolved background work, or other explicitly registered host-owned task.

Core Invariants
One conversation, one incarnation
For a persisted root conversation, at most one non-terminal connection incarnation is registered in the broker. The invariant covers Reserved, Bootstrapping, Ready, and Closing; it is not limited to connections that already reached SessionStarted.

Every creation path must reserve the broker key before spawning a subprocess. No public server root path may bypass this reservation by calling spawn_agent directly.

Server ownership
The broker, not a web page, owns the root ACP process. Releasing or expiring one lease never sends ACP disconnect. The last lease disappearing only starts the idle eligibility calculation.

The existing owner/viewer distinction is not exposed for broker-managed web roots. Existing ownership fields may remain internally for local Tauri handoff, delegated children, and compatibility, but they cannot authorize a browser to kill a shared root.

Reservation before bootstrap
The broker preallocates connection_id, creates the public shared-session record, and installs a Bootstrapping snapshot before the agent process can produce readiness or failure. Concurrent callers can therefore attach to the same attempt even if companion startup takes tens of seconds.

Accepted means inserted
A prompt enqueue succeeds only after the complete validated item has been inserted into the in-memory queue under the per-session lock and assigned an enqueue_seq. An accepted item is never silently discarded. Every later terminal outcome is represented by a queue/turn event: dispatched, cancelled, failed, or completed.

This guarantee is process-local. A server crash may lose every accepted but unpersisted item, as explicitly chosen for this version.

One active prompt
Only the FIFO dispatcher calls existing foreground linked-prompt admission for a broker-managed root. It claims exactly the queue head and does not claim a second item until the current turn and any cancellation quarantine have ended.

First valid interaction wins
Permission, question, and plan-approval responses are one-shot transitions keyed by their current interaction id. The first request that atomically moves the interaction from Pending to Resolving wins. Later requests cannot invoke the ACP responder and receive a stable already-resolved result.

Generation-fenced stop
A stop request must carry the current broker turn_id. A stop for an older turn is a no-op and returns a stale-turn error containing no prompt content. This prevents a suspended phone page from cancelling work submitted later on another device.

Idle is an intersection, not an activity timestamp
The 15-minute timer begins only when all idle predicates are true at the same time. It resets whenever a lease appears, work appears, the connection leaves Ready, or the ACP status leaves idle Connected. Time spent executing with zero clients does not consume the later 15-minute idle grace.

Architecture
text


desktop browser      phone       browser tabs
       |                |              |
       +------ connect_or_attach -------+
                        |
                        v
              SharedSessionBroker
              - key/generation
              - bootstrap single-flight
              - client leases
              - prompt FIFO
              - idle eligibility
                        |
                        v
                 AgentConnection
                 - ACP transport
                 - SessionState
                 - event ring/broadcast
                        |
                        v
             any registered ACP agent
Component ownership
ConnectionManager owns one SharedSessionBroker through the same shallow clone pattern as the existing connection map. The broker is process-local and contains:

rust


struct SharedSessionBroker {
    index: Mutex<SharedSessionIndex>,
}
struct SharedSessionIndex {
    sessions: HashMap<SharedSessionKey, Arc<Mutex<SharedSessionRecord>>>,
    by_connection: HashMap<String, SharedSessionKey>,
}
struct SharedSessionRecord {
    key: SharedSessionKey,
    generation: u64,
    connection_id: String,
    phase: SharedSessionPhase,
    launch_identity: LaunchIdentity,
    leases: HashMap<LeaseId, ClientLease>,
    queue: VecDeque<QueuedPrompt>,
    next_enqueue_seq: u64,
    active_turn: Option<ActiveSharedTurn>,
    idle_zero_since: Option<tokio::time::Instant>,
    bootstrap: Option<BootstrapControl>,
}
The exact Rust split may place queues and leases in sibling structs, but these facts must be changed atomically under one per-session synchronization domain. The global map lock is used only to find/reserve a record. No subprocess, database, channel, or WebSocket await may occur while holding it.

Session key
The canonical key for a persisted root is:

text


Conversation(conversation_id)
The broker loads the conversation row and validates its persisted agent and folder/workspace association. agent_type is not added to the key because one conversation must not acquire parallel connections merely by supplying a different agent value.

Before a connection has a persisted conversation, the broker may use:

text


ExternalSession(agent_type, normalized_working_dir, external_session_id)
Ephemeral(server_generated_id)
An ephemeral draft is not discoverable across devices. When the first prompt binds a real conversation, the broker atomically installs the canonical conversation index before publishing ConversationLinked. A collision with an existing non-terminal record fails closed and does not merge two live ACP processes.

Historical conversations should always call connect_or_attach with their positive conversation_id; external-session fallback exists for the existing pre-link window and migration compatibility, not as the preferred identity.

Launch identity
The first creator freezes immutable launch facts for the incarnation:

persisted conversation and agent type;
normalized working directory;
requested external session and attach mode;
resolved delegation-route fingerprint;
terminal-shell fingerprint; and
connection purpose (UserRoot).
Mode/config selector preferences are not connection identity. The creator may apply initial preferences once; later clients consume the authoritative snapshot and use normal session controls.

An attach with a conflicting immutable launch identity returns shared_session_config_conflict and includes the existing connection id and a secret-safe conflict kind. It never starts a second process or mutates the live incarnation.

Split spawn registration from readiness
The current spawn_agent call combines registration with waiting for route bootstrap. Broker creation splits it conceptually into:

reserve key and connection id;
register a Connecting SessionState and event stream;
return the broker handle to the API;
run ACP and route bootstrap asynchronously; and
settle the broker phase from the typed bootstrap outcome.
This can reuse the existing preallocated-connection-id machinery, but root broker registration must have an explicit API rather than depending on a task scheduling race. connect_or_attach does not return until step 2 is observable, but it does not wait for steps 4 or 5.

Broker phase
rust


enum SharedSessionPhase {
    Reserved,
    Bootstrapping,
    Ready,
    Failed { error_code: String, cleanup_complete: bool },
    Closing,
}
Wire snapshots normally expose bootstrapping, ready, failed, or closing; reserved is an internal sub-state that must settle before the create response is released.

The state machine is:

text


Reserved -> Bootstrapping -> Ready -> Closing -> Removed
                         \-> Failed -> cleanup -> retry/remove
A failed record remains as a short process-local tombstone so every concurrent lease observes the same typed failure. An explicit retry supplies the failed generation and may CAS-replace it only after cleanup is complete. The retry creates a new connection_id; an old WebSocket receives a terminal replacement detach and cannot mutate the new generation.

Agent capability and route readiness
The broker does not switch on AgentType::Codex for companion behavior. It uses the already resolved route plan and its capability snapshot:

text


standard/native route
  ready = ACP session ready
route that explicitly requires Codeg companion
  ready = ACP session ready AND companion registered/ready
When an explicit Codeg route reports CompanionInitializationFailed, missing companion binary, invalid mandatory suppression, or another route-specific readiness failure, the broker enters Failed, tears down the unexposed incarnation, and reports the typed error. It does not call safe_native_fallback.

Automatic or native route policies may retain fallback only when the resolved policy explicitly permits fallback. The rule is based on route policy, so it works uniformly for every ACP capable of receiving the Codeg MCP companion. Agents that do not support or request the companion use standard ACP readiness.

Client Identity and Leases
Identity fields
The frontend creates:

device_id: random and stored in local storage, useful only for coarse diagnostics and "this device" UX; and
client_instance_id: random for each loaded document, kept in memory for that document's lifetime.
Neither value grants access. The existing Codeg bearer token remains the authentication boundary. Raw bearer tokens and lease tokens are never logged.

Lease creation
Every successful connect_or_attach creates or idempotently renews one random lease scoped to:

text


(shared_session_generation, client_instance_id, authenticated_operator)
The response returns lease_id and lease_expires_at. Mutation endpoints for the shared session require an active lease. An expired page must connect_or_attach again; it cannot revive a lease by sending a prompt.

Heartbeat
WS attach binds a subscription to a lease. A socket ping renews every lease currently bound to that socket. The default client cadence is 30 seconds and the default lease TTL is 90 seconds. These are presence values, not session reclamation values.

Mobile timer suspension is expected. When a lease expires, the active session continues, and an idle session still receives its separate 15-minute grace. Opening the page again obtains a new lease and a cold snapshot or replay.

Release
Normal page/tab teardown sends best-effort release_lease. WebSocket close does not immediately release presence; it lets the 90-second TTL absorb a network flap. Correctness never depends on beforeunload or sendBeacon.

acp_touch_connection is no longer an idle authority for broker-managed roots. It may remain for legacy/local connection purposes during migration.

Wire Contracts
Wire names below use snake case in Rust and camel case in TypeScript according to existing project conventions.

Connect or attach
Add a shared core command and server route rather than composing discovery and spawn in the frontend:

http


POST /acp_connect_or_attach
Request:

ts


type AcpConnectOrAttachRequest = {
  conversationId?: number
  agentType: AgentType
  workingDir?: string
  externalSessionId?: string
  delegationRouteOverride?: DelegationRoutePolicy
  preferredModeId?: string
  preferredConfigValues?: Record<string, string>
  deviceId: string
  clientInstanceId: string
  requestId: string
  retryFailedGeneration?: number
}
requestId makes a transport retry idempotent for the same client instance. The broker keeps the bounded idempotency result for the record lifetime.

Response:

ts


type AcpConnectOrAttachResponse = {
  connectionId: string
  generation: number
  leaseId: string
  leaseExpiresAt: string
  disposition: "created" | "attached"
  phase: "bootstrapping" | "ready" | "failed" | "closing"
  eventSeq: number
  error?: {
    code: string
    retryable: boolean
    cleanupComplete: boolean
  }
}
The normal response is fast because phase: "bootstrapping" is a successful attachment, not an HTTP timeout. Transport/auth/validation failures remain normal non-2xx responses.

WebSocket attach
Extend the existing attach message:

ts


{
  action: "attach"
  subscriptionId: string
  connectionId: string
  generation: number
  leaseId: string
  sinceSeq?: number
}
The server validates that connection, generation, and lease belong together before returning snapshot/replay. Existing event-sequence and lagged-resync rules remain unchanged.

LiveSessionSnapshot gains a shared-session projection containing phase, current turn_id, queue summaries, and this subscription's lease expiry. The snapshot must contain every state required to reconstruct queue and pending interaction UI after missing one-shot events.

Prompt enqueue
/acp_prompt becomes queue admission for broker-managed roots. Its request adds generation, lease_id, and an idempotent client_request_id.

Successful response:

ts


type PromptEnqueueResult = {
  queueItemId: string
  enqueueSeq: number
  state: "queued" | "dispatching"
}
Returning success means the item is in memory. It does not mean the ACP agent has accepted or completed it.

Queue events
The per-connection stream adds:

text


PromptQueued
PromptQueueItemCancelled
PromptDispatchStarted
PromptQueueItemFailed
PromptQueueDepthChanged
PromptDispatchStarted carries the existing exact client_message_id, so each sender can reconcile optimistic UI without text/time heuristics. The existing UserMessage and turn events remain authoritative once dispatch begins.

Cancel queued item
http


POST /acp_cancel_queued_prompt
The request contains connection id, generation, lease id, and queue item id. Any active lease may cancel an item still in Queued. The atomic transition is:

text


Queued -> Cancelled
If the dispatcher already claimed it, the endpoint returns queue_item_already_dispatching; the user may stop the exact active turn instead. There is no edit or reorder endpoint. To modify text, cancel and enqueue a new item at the tail.

Stop current turn
The stop request adds generation, lease_id, and turn_id. Any active lease may stop the exact active turn. Concurrent stop requests are idempotent for the same turn. A stale turn id returns stale_turn; it never targets the current turn by connection id alone.

After stop, the active item settles as cancelled. Remaining FIFO items stay in place. Dispatch pauses until the existing cancellation acknowledgement or quarantine completes, preventing late frames from the stopped turn from being attributed to the next item.

Permission and question responses
Permission, ask_user_question, and plan-approval requests retain their existing globally unique interaction ids. Responses add generation and lease validation.

The first valid response claims the pending interaction. Every client receives the existing resolved event. A later response receives a typed interaction_already_resolved result and refreshes from snapshot; it is not reported as a generic transport failure.

Release versus terminate
release_lease removes one client presence record. It never sends ACP disconnect.

Explicit process termination is a separate, clearly named operation such as terminate_shared_session. It requires the server operator credential, the current generation, and an explicit UI command. Legacy acp_disconnect must not kill a broker-managed root in response to browser teardown.

Prompt FIFO
Queue item
rust


struct QueuedPrompt {
    queue_item_id: String,
    enqueue_seq: u64,
    client_request_id: String,
    submitted_by: ClientInstanceId,
    submitted_at: DateTime<Utc>,
    blocks: Vec<PromptInputBlock>,
    visible_text: Option<String>,
    locale: Option<String>,
    client_message_id: String,
    state: QueuedPromptState,
}
The full validated prompt stays in broker memory. Snapshots may expose a wire-friendly visible projection and attachment metadata rather than repeating large encoded image/blob bodies.

Admission and idempotency
Before insertion, queue admission performs the same auth, conversation, workflow-write, block, payload-size, and route guards required by direct prompt admission. A rejected request creates no queue item.

The key (generation, client_instance_id, client_request_id) is idempotent. A retry returns the original queue result. Reusing the key with different prompt content returns idempotency_key_conflict.

The queue has explicit process-memory bounds. The implementation plan must set and test fixed defaults of no more than 64 waiting items and 32 MiB total serialized waiting payload per session, in addition to existing per-request limits. Capacity rejection returns prompt_queue_full; it never drops an already accepted item. These constants may later become operator settings.

Dispatch
The dispatcher may claim the head only when:

broker phase is Ready;
underlying status is idle Connected;
no turn or turn-recovery quarantine is active;
no prior queue head is dispatching; and
the conversation remains writable.
It changes the head to Dispatching, allocates a unique turn_id, and calls the existing linked foreground prompt path. turn_in_flight remains the connection-loop safety fence, but public concurrent sends no longer race that fence because only the dispatcher reaches it.

Queue order is the order of successful insertion under the per-session lock, represented by enqueue_seq. Arrival order at different TCP connections is not meaningful before insertion; the assigned sequence is the authoritative global order and is broadcast to all clients.

Dispatch failures
A per-item validation failure should normally be caught before enqueue. If a race makes the conversation unwritable before dispatch, the item becomes Failed with a typed reason and the dispatcher re-evaluates the next item.
A synchronous provider rejection before a turn starts fails that item and may continue only if the connection remains healthy.
A connection/process/bootstrap failure marks every still-queued item failed with session_unavailable and broadcasts those transitions before retiring the record.
No active or failed item is automatically retried. The user may explicitly enqueue a new request after reconnecting.
Sender disconnect
Queue items belong to the shared session, not to a lease. Lease release or expiry does not alter them. A clientless session continues dispatching its accepted queue until it becomes idle, fails, or is explicitly terminated.

Shared Interactions
Waiting for input
While a turn is waiting for permission, question, or plan approval:

every attached client renders the same pending interaction from snapshot;
any active lease may respond;
additional prompts may be enqueued but cannot dispatch;
losing simultaneous responses receive interaction_already_resolved; and
zero client leases never make the session idle or reclaimable.
The interaction id is the compare-and-set token. Display text or tool-call ids are never used for correlation.

Stop authority
All attached clients share stop authority because the selected product model is shared co-control. The exact turn_id is mandatory even if only one turn can currently be active. This is the stale-screen safety boundary.

Stopping closes or resolves current input responders through the existing turn-finalization path. A late permission answer for that retired turn is already-resolved/stale and cannot affect the next queued turn.

Queue authority
All attached clients may cancel any unstarted queue item. The server records the winning transition and broadcasts it. The UI may label an item as submitted from this or another device, but authorization does not depend on its originating device.

Lifecycle and Reclamation
Lease expiry is not session expiry
Lease expiry only changes client count. It does not update an activity timestamp and does not immediately terminate anything.

Idle predicate
A broker-managed root is eligible to start or continue the 15-minute timer only when all conditions are true:

text


phase == Ready
underlying status == Connected
lease_count == 0
queue is empty
active_turn is none
turn recovery/quarantine is false
pending_permission is none
pending_question is none
pending_plan_approval is none
waiting_for_subagents is none
active_delegations is empty
background_outstanding == 0
no other registered host-owned work exists
If any condition becomes false, idle_zero_since is cleared. When all become true again, the full 15-minute grace starts again.

This differs deliberately from last_activity_at. Repeated internal events do not extend a clientless idle connection forever, and time spent working does not consume idle grace.

Background work
Client-idle sweep must never reap while authoritative background work is outstanding. The existing age-bounded background keepalive cannot be treated as permission for the client-idle broker to kill a still-declared task. Detecting a stale/lost background watcher is a separate watchdog concern that must emit an explicit terminal/lost transition first; only then may idle eligibility begin.

Bootstrap with no clients
Bootstrap is governed by its typed initialization deadlines, not by the idle sweep. If every lease disappears during bootstrap, the attempt is allowed to reach ready or fail. If it reaches ready with no work and no leases, the 15-minute idle timer starts at that transition.

Failed and closing records
Failed and closing records do not wait 15 minutes to clean their subprocesses. They follow bounded teardown immediately. A small broker tombstone may remain for error delivery and retry fencing, but it owns no live ACP process.

Server shutdown and restart
On graceful shutdown, the broker stops admission, broadcasts server shutdown, and uses existing bounded disconnect-all cleanup. On restart:

no lease is restored;
no FIFO item is restored;
no prior bootstrap/turn is assumed active; and
clients perform a normal connect-or-attach using persisted conversation and external session history.
The server must not automatically resubmit an interrupted or unknown prompt.

Configuration
CODEG_ACP_IDLE_TIMEOUT_SECS changes its default from 180 to 900 for shared roots and represents the all-predicates idle grace, not frontend touch age. A zero value may continue to disable client-idle reclamation.

A separate CODEG_ACP_CLIENT_LEASE_TTL_SECS defaults to 90 seconds. Lease TTL must be materially shorter than idle grace.

Frontend Behavior
Connect flow
For server-hosted roots, AcpConnectionsProvider removes discovery-before- connect from own_or_observe. It always calls connect_or_attach for a persisted conversation and treats both created and attached as a shared connection.

The client immediately installs a WS attachment, even when phase is bootstrapping. The connection surface renders its normal connecting state from snapshot/events. A companion delay therefore remains visible and does not hold an HTTP request open.

acp_find_connection_for_conversation may remain as a read-only diagnostic and migration endpoint, but it is not a creation decision input.

Shared state
Frontend isViewer is no longer the lifecycle decision for broker-managed roots. A shared connection record stores lease id, broker generation, and phase. Teardown releases the lease and WS subscription only.

Composer and queue
Submitting while idle may dispatch immediately. Submitting while another turn is active, waiting for input, cancelling, or bootstrapping adds an observable queue item.

The composer clears only after queue admission succeeds. The UI shows queued items in authoritative enqueue_seq order with cancel controls. It does not render a queued item as persisted/agent-accepted history before dispatch. On PromptDispatchStarted/UserMessage, exact message-id reconciliation moves it into the active transcript.

No visible feature-explanation text is required. Normal status, queue order, cancel, and stop controls communicate the state.

Interaction convergence
After one device answers a permission or question, every device removes the pending card from the resolved event. A stale submit response is handled as normal convergence, not a destructive error toast; the client applies the latest snapshot if needed.

Mobile reconnect
When a phone resumes after lease expiry, it calls connect_or_attach, obtains a new lease for the existing generation, and attaches with its last event cursor when valid. Existing replay/snapshot fallback handles cursor gaps and slow consumers.

Server API Compatibility
Add the versioned shared endpoint and migrate the bundled server frontend in the same release. Existing /acp_connect remains available for local Tauri bridges and non-shared internal connection purposes, but server root calls may not use it to bypass the broker.

For broker-managed connection ids:

legacy browser acp_disconnect cannot terminate the process;
legacy acp_touch_connection is not an idle authority; and
prompt/interaction mutations without generation and lease return shared_session_protocol_required.
Because the static frontend and server are shipped together, an old cached web bundle may be told to reload/upgrade rather than receiving ambiguous owner semantics. Do not emulate an owner lease without a client identity; that would reintroduce the teardown bug.

Local Tauri root ownership and desktop pop-out handoff retain their current commands unless they explicitly opt into a remote server shared session. Delegated children retain broker/delegation ownership and existing viewer-only rules. Internal probes, hidden title/translation sessions, automation workers, and chat-channel producers retain their purpose-specific admission paths; they must not accidentally enter a user root FIFO solely because they use an ACP agent.

Error and Cleanup Semantics
Stable errors
At minimum, the shared path exposes stable codes for:

text


shared_session_config_conflict
shared_session_protocol_required
shared_session_generation_stale
shared_session_closing
shared_session_cleanup_in_progress
client_lease_missing
client_lease_expired
prompt_queue_full
idempotency_key_conflict
queue_item_not_found
queue_item_already_dispatching
interaction_already_resolved
stale_turn
session_unavailable
companion_initialization_failed
Errors contain no bearer token, lease token, prompt content, environment, working-directory text, or agent stderr.

Bootstrap failure
On typed bootstrap failure, the broker:

atomically changes phase to Failed for the current generation;
broadcasts the typed failure to every attached client;
marks queued items failed with session_unavailable;
starts bounded teardown of the unexposed process, companion token, pending interaction responders, and manager connection entry;
records cleanup_complete only after map absence and process cleanup are observed; and
permits an explicit generation-fenced retry without waiting for a stale frontend owner guard.
Cleanup never falls back to a native route when companion was required. A failed cleanup cannot leave the broker claiming Ready, and a new generation cannot be created while the old process is still capable of emitting events.

Process exit while ready
Unexpected ACP exit changes phase to Failed, fails queued items, terminates the active turn through the existing exactly-once finalizer, and broadcasts a terminal snapshot/event. No prompt is automatically replayed.

Slow WebSocket client
Existing bounded outbound channels and Lagged detach remain valid. The client reattaches with lastAppliedSeq; replay or snapshot restores broker phase, queue, active turn, and pending interactions. A lagged socket does not release or mutate its lease until TTL/release rules do so.

Concurrent close and attach
Final idle removal performs a generation and predicate recheck under the same per-session synchronization domain used by lease addition. If a lease attaches before removal commits, removal loses and the session survives. If removal commits first, attach creates a new incarnation through normal connect_or_attach. No stale selected victim may kill the new incarnation.

Consistency and Locking
The implementation must use this lock order when more than one domain is needed:

text


broker session-map lock
  -> per-session lock
    -> ConnectionManager connection-map lock
      -> SessionState lock
Production code should avoid holding more than one domain whenever possible. In particular:

reserve/find under the broker map, clone the record, then release the map;
never await process bootstrap, database I/O, channel send, or WS send under the broker map or per-session lock;
use generation compare-and-set after each awaited operation;
never hold SessionState while acquiring the broker map; and
emit events after committing state, using snapshots copied under the lock.
The implementation plan must audit existing event callbacks for reverse lock order before introducing broker callbacks.

Security Model
The existing non-empty Codeg server bearer token authenticates the single operator. Every client with that token can view and control every conversation available to that server, matching current single-user server behavior.

Lease ids are random, session/generation scoped, and required in addition to authentication for shared mutations. They prevent stale pages from mutating a session after presence expiry, but they are not a replacement for auth.

device_id and client_instance_id are untrusted labels. The server bounds their length, validates their format, and never uses them to select filesystem paths, processes, conversations, or authorization scope.

Multi-user sharing requires a future principal/ACL design and is outside this specification.

Observability
Add secret-safe structured logs and counters at broker boundaries:

connect-or-attach totals by created/attached and phase;
live shared-session and active-lease gauges;
bootstrap duration and typed failure totals by agent/route capability;
queue depth, enqueue, cancel, dispatch, capacity rejection, and item-failure totals;
first-winner versus stale interaction-response totals;
stale turn-stop totals;
lease expiry/release totals;
idle candidate, CAS-lost, and reclaimed totals; and
cleanup duration and incomplete-cleanup totals.
Logs may include connection id, conversation id, generation, agent type, stable error code, queue depth, and durations. They must not include prompt blocks, visible text, answer content, paths, tokens, environment values, or raw agent output.

A diagnostic snapshot should expose broker phase, generation, lease count, queue depth, idle-eligibility blockers, and cleanup state without exposing lease ids or client identifiers.

Testing Strategy
Broker unit tests
Two, ten, and one hundred simultaneous creates for the same conversation produce one reserved connection id and one spawn invocation.
A caller arriving during Bootstrapping attaches to the same generation and receives the same terminal ready/failure outcome.
Distinct conversations and distinct agents can bootstrap concurrently.
Immutable launch conflicts reject without mutating or spawning.
Failed-generation retry cannot start before cleanup and creates exactly one new incarnation after cleanup.
A stale generation cannot enqueue, answer, stop, release, or remove a new incarnation.
Lease tests
Multiple tabs on one device receive independent leases.
WS ping renews only leases bound to that authenticated socket.
Explicit release and TTL expiry reduce presence without disconnecting ACP.
Mobile-style heartbeat gaps expire presence but preserve active work and the 15-minute grace.
Attach racing final idle removal has one linearizable winner.
Queue tests
Concurrent enqueues receive a unique contiguous enqueue_seq and dispatch in that exact order.
Retrying one client_request_id returns one queue item; changed content with the same id conflicts.
Sender lease expiry does not cancel its queued item.
Any active lease can cancel an unstarted item.
Dispatch-versus-cancel race yields either cancelled or dispatching, never both and never lost.
Stop preserves tail items and the next item starts only after cancellation quarantine.
Queue count/byte limits reject the new request without dropping old items.
Bootstrap/process failure emits a terminal outcome for every accepted item.
Interaction tests
Two permission answers race; one ACP responder call occurs and both clients converge.
The same first-winner rule covers questions and plan approvals.
A stale answer after stop cannot affect the next turn.
A stale turn_id cannot cancel a newer turn.
Concurrent exact-turn stop requests finalize once.
Idle matrix tests
Use paused Tokio time to cover every blocker independently:

lease present;
prompting/active turn;
cancellation quarantine;
pending permission;
pending question;
pending plan approval;
queued prompt;
continuation wait;
active delegation;
background work; and
non-ready broker phase.
Each blocker resets idle_zero_since. A long clientless turn receives a fresh full 15 minutes only after becoming idle. Reclamation revalidates generation and all predicates immediately before removal.

Route and ACP coverage tests
Standard ACP ready reaches broker Ready without companion.
Every explicitly required Codeg route waits for companion ready.
Required companion failure never calls native fallback.
Auto/native policies fall back only when their resolved policy permits it.
A registry-driven conformance test covers all eleven built-in agent types and a custom ACP definition without adding agent-specific broker branches.
Existing Codex, Grok, Claude, OpenCode, and custom-agent adapter tests remain unchanged except for the shared admission wrapper.
Axum/WebSocket integration tests
Two authenticated HTTP clients connect concurrently and receive one connection id with separate leases.
Both attach during bootstrap, receive snapshot, then receive the same ready event.
One client enqueues while another turn is active; both observe queue and dispatch order.
A lagged/reconnected client recovers phase, queue, active turn, and pending input from snapshot.
Closing both sockets and expiring leases does not stop work.
Required companion failure is delivered before bounded cleanup and retry is possible after cleanup.
Simulated server restart restores neither queue nor leases.
Frontend tests
Server root connect does not call discovery before connect_or_attach.
Created and attached dispositions build the same shared connection state.
Page teardown releases a lease and never calls process disconnect.
Bootstrapping renders from WS state without an HTTP timeout.
Queue acknowledgements, cancellation, dispatch, and exact message-id reconciliation converge across two providers.
Stale interaction/stop responses refresh state without corrupting the active turn.
Existing local Tauri owner/viewer and desktop pop-out tests remain green.
Manual/chaos verification
Desktop and phone open the same conversation through the deployed tunnel.
Both watch one long turn; each sends additional prompts; FIFO order matches enqueue sequence.
Lock and unlock the phone beyond lease TTL, then reconnect and inspect the current snapshot.
Answer one permission on the phone while submitting the same answer on the desktop.
Stop a turn from the non-originating device and verify queued work remains.
Close every client during a turn and during background work; verify no idle disconnect.
After work becomes idle, verify retention before 15 minutes and reclamation after the grace.
Rollout and Migration
Phase 1: broker primitives behind tests
Add broker record/state, reservation, leases, queue, generation fencing, and idle predicate without routing production web roots through it. Build fake ACP and paused-time race tests first.

Phase 2: structured connect and bootstrap stream
Split root spawn registration from readiness, add /acp_connect_or_attach, extend WS attach/snapshot, and route the bundled web frontend through shared connect. Keep prompt submission single-item until the connection and lease lifecycle is proven.

Phase 3: FIFO and shared mutations
Move broker-managed root prompt admission behind FIFO; add queue snapshot/events and generation-fenced interaction/stop commands. Remove frontend busy-send rejection for shared roots.

Phase 4: lifecycle convergence
Change shared-root idle default and predicate, disable owner disconnect/touch authority for broker-managed roots, and remove server frontend reliance on isViewer and discovery-before-connect.

Phase 5: compatibility cleanup
Retire unused server owner/viewer branches only after local Tauri, desktop pop-out, delegation viewer, automation, and custom ACP regression suites prove their separate ownership paths remain intact.

Each phase must be deployable only when old paths cannot bypass the current invariant. A feature flag may gate early development, but production must not allow some clients to use broker ownership while other server-root clients can kill the same process through legacy owner disconnect.

Expected File Boundaries
Implementation planning should prefer a focused broker module rather than making manager.rs larger:

new src-tauri/src/acp/shared_session.rs for keys, records, leases, FIFO, and lifecycle predicates;
src-tauri/src/acp/manager.rs for integration with spawn, prompt, stop, and teardown;
src-tauri/src/acp/session_state.rs and types.rs for shared snapshot/events;
src-tauri/src/acp/idle_sweep.rs for the new shared-root predicate/default;
src-tauri/src/web/handlers/acp.rs, router.rs, and ws_attach.rs for wire entry points and lease-bound subscription;
src/lib/api.ts, src/lib/tauri.ts, and src/contexts/acp-connections-context.tsx for shared client state; and
focused Rust and frontend tests adjacent to those modules.
Exact placement may change during planning, but broker logic must stay in the shared Rust core rather than being duplicated in the Axum handler or React provider.

Risks and Mitigations


Risk	Mitigation
Broker/manager lock inversion	Fixed lock order, clone-before-await, race tests
Old client kills shared process	Generation/lease required; legacy disconnect cannot kill broker root
Companion failure wedges retry	Explicit Failed phase, cleanup completion fence, generation-CAS retry
Mobile heartbeat throttling	Lease expiry is harmless; active protection plus 15-minute idle grace
Queue memory growth	Per-item validation, count and byte caps, explicit capacity error
Duplicate send after HTTP retry	Client request id with payload consistency check
Stop from stale page	Exact current turn_id and generation required
Two devices answer input	One-shot interaction CAS and resolved broadcast
Slow WS misses queue event	Queue and interactions included in snapshot
ACP-specific readiness drift	Route-capability policy and registry conformance test
Regression to local desktop/delegation	Scope broker to server user roots; retain separate ownership tests
Server crash loses queue	Explicit non-durable contract; never claim recovery or auto-retry
Acceptance Criteria
Two or more authenticated devices opening one persisted conversation concurrently receive one connection incarnation and separate leases.
A second device can attach while the first connection is bootstrapping; its HTTP request does not wait for companion initialization.
Every current built-in ACP and a conforming custom ACP use the same broker path without agent-specific concurrency code.
An explicitly required Codeg companion must be ready before broker phase is Ready; failure is typed, fully cleaned up, and never silently downgraded.
Concurrent accepted prompts receive one global enqueue_seq order and are dispatched exactly once in that order.
Disconnecting the submitting device does not remove its queued prompt.
Any attached client can cancel an unstarted queue item; no client can edit or reorder it.
Any attached client can answer pending user input; exactly one response reaches the ACP responder and all clients converge.
Any attached client can stop only the exact current turn_id; queued tail items survive and resume after cancellation quarantine.
Closing/releasing every client does not disconnect a prompting, waiting-input, queued, continuation-waiting, delegated, or background-active session.
A Ready + Connected + no leases + no work session remains alive for 15 minutes and is then reclaimed after a final generation/predicate recheck.
Restarting codeg-server restores neither leases nor FIFO items and never automatically resubmits an interrupted prompt.
Slow or reconnecting clients recover full phase, queue, active-turn, and pending-interaction state through snapshot/replay.
Browser teardown cannot terminate a broker-owned process through legacy acp_disconnect.
Automated race, route, lifecycle, Axum/WS, frontend, local-desktop, and delegation regression tests pass.