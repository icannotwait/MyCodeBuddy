// Per-connection state machine. One `ConnectionTask` per registered
// connection; the task owns the SSH children (master, daemon-exec, tunnel)
// and is driven by `ControlMessage`s sent through an mpsc channel.
//
// CG-002.4 M0 shipped the happy path. CG-002.7 (M1) adds the reconnect
// supervisor: after reaching Live, a child task pings /api/health every
// 10s; two consecutive failures escalate to a Reconnecting loop with
// exponential backoff (1s/3s/10s/30s/60s, max 10 attempts). User-issued
// control messages always win — the supervisor and the reconnect loop
// surrender immediately when one arrives.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::task::JoinHandle;

use crate::models::connection::ConnectionConfig;
use crate::remote::bootstrap::{
    deploy, instructions_for, launch_daemon, DaemonHandshake, DeployError, DeploymentTarget,
    LaunchedDaemon, ManualDeployInstructions,
};
use crate::remote::http_client::{CapabilitiesResponse, DaemonClient};
use crate::remote::manifest::RemoteDaemonManifest;
use crate::remote::platform::probe;
use crate::remote::ssh_process::{base_ssh_args, build_ssh_target};
use crate::remote::tunnel::{establish_forward, TunnelHandle};
use crate::web::event_bridge::{emit_event, EventEmitter};

pub const STATUS_EVENT: &str = "connection://status";
const SUPERVISOR_INTERVAL: Duration = Duration::from_secs(10);
const SUPERVISOR_FAILURE_THRESHOLD: u32 = 2;
const RECONNECT_MAX_ATTEMPTS: u32 = 10;
const SUPERVISOR_KILL_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionStatus {
    NotAttempted,
    Probing,
    Deploying,
    AwaitingManual,
    Launching,
    Handshaking,
    Live,
    Reconnecting { attempt: u32 },
    Cached,
    Error,
    Disconnected,
}

impl ConnectionStatus {
    fn channel_label(&self) -> &'static str {
        match self {
            Self::NotAttempted => "not_attempted",
            Self::Probing => "probing",
            Self::Deploying => "deploying",
            Self::AwaitingManual => "awaiting_manual",
            Self::Launching => "launching",
            Self::Handshaking => "handshaking",
            Self::Live => "live",
            Self::Reconnecting { .. } => "reconnecting",
            Self::Cached => "cached",
            Self::Error => "error",
            Self::Disconnected => "disconnected",
        }
    }
}

/// Snapshot of the runtime exposed to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConnectionRuntime {
    pub connection_id: String,
    pub status: ConnectionStatus,
    pub handshake: Option<DaemonHandshake>,
    pub capabilities: Option<CapabilitiesResponse>,
    pub local_port: Option<u16>,
    pub last_error: Option<String>,
    pub deployment_target: Option<DeploymentTarget>,
    pub manual_instructions: Option<ManualDeployInstructions>,
}

/// Live, mutable state. `ConnectionTask` keeps it behind an `RwLock` so the
/// manager can take read snapshots without blocking the task loop.
pub(super) struct RuntimeState {
    pub status: ConnectionStatus,
    pub handshake: Option<DaemonHandshake>,
    pub capabilities: Option<CapabilitiesResponse>,
    pub local_port: Option<u16>,
    pub last_error: Option<String>,
    pub last_status_change_at: Instant,
    pub deployment_target: Option<DeploymentTarget>,
    pub manual_instructions: Option<ManualDeployInstructions>,
    /// Handles owned by the running pipeline. Replaced on every connect.
    pub launched: Option<LaunchedDaemon>,
    pub tunnel: Option<TunnelHandle>,
}

impl RuntimeState {
    pub(super) fn snapshot(&self, connection_id: &str) -> ConnectionRuntime {
        ConnectionRuntime {
            connection_id: connection_id.to_string(),
            status: self.status.clone(),
            handshake: self.handshake.clone(),
            capabilities: self.capabilities.clone(),
            local_port: self.local_port,
            last_error: self.last_error.clone(),
            deployment_target: self.deployment_target.clone(),
            manual_instructions: self.manual_instructions.clone(),
        }
    }
}

#[derive(Debug)]
pub enum ControlMessage {
    Connect,
    Disconnect,
    ResumeAfterManual,
    HardReset,
}

pub struct ConnectionTask {
    pub config: ConnectionConfig,
    pub(super) state: Arc<RwLock<RuntimeState>>,
    pub control_tx: mpsc::Sender<ControlMessage>,
}

impl ConnectionTask {
    pub fn spawn(
        config: ConnectionConfig,
        emitter: EventEmitter,
        manifest: Arc<RemoteDaemonManifest>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(8);
        let state = Arc::new(RwLock::new(RuntimeState {
            status: ConnectionStatus::NotAttempted,
            handshake: None,
            capabilities: None,
            local_port: None,
            last_error: None,
            last_status_change_at: Instant::now(),
            deployment_target: None,
            manual_instructions: None,
            launched: None,
            tunnel: None,
        }));

        let cfg = config.clone();
        let st = state.clone();
        tokio::spawn(async move {
            run_loop(cfg, st, emitter, manifest, rx).await;
        });

        Self {
            config,
            state,
            control_tx: tx,
        }
    }
}

/// Internal event sent from the supervisor task back to the run loop.
#[derive(Debug)]
enum SupervisorEvent {
    OutageDetected { reason: String },
}

/// Mutable per-loop bookkeeping. Lives only inside `run_loop`; not exposed
/// to snapshots — keeps `RuntimeState` purely a data type.
struct LoopCtx {
    config: ConnectionConfig,
    state: Arc<RwLock<RuntimeState>>,
    emitter: EventEmitter,
    manifest: Arc<RemoteDaemonManifest>,
    sup_tx: mpsc::Sender<SupervisorEvent>,
    sup_killer: Option<oneshot::Sender<()>>,
    sup_handle: Option<JoinHandle<()>>,
    /// WebSocket event bridge (CG-002.8 A): mirrors daemon `acp://event`
    /// (and any other) frames into the local emitter so the frontend sees
    /// remote ACP events identically to local ones. Lives alongside the
    /// supervisor — same spawn/kill cadence.
    bridge_killer: Option<oneshot::Sender<()>>,
    bridge_handle: Option<JoinHandle<()>>,
}

impl LoopCtx {
    async fn kill_supervisor(&mut self) {
        if let Some(k) = self.sup_killer.take() {
            let _ = k.send(());
        }
        if let Some(h) = self.sup_handle.take() {
            let _ = tokio::time::timeout(SUPERVISOR_KILL_GRACE, h).await;
        }
        if let Some(k) = self.bridge_killer.take() {
            let _ = k.send(());
        }
        if let Some(h) = self.bridge_handle.take() {
            let _ = tokio::time::timeout(SUPERVISOR_KILL_GRACE, h).await;
        }
    }

    async fn spawn_supervisor(&mut self) {
        // Build a DaemonClient pointing at the current Live runtime. If we
        // somehow lack port/token we just skip spawning — the connection
        // will only be supervised next time it reaches Live cleanly.
        let (port, token) = {
            let s = self.state.read().await;
            let port = s.local_port;
            let token = s.handshake.as_ref().map(|h| h.token.clone());
            (port, token)
        };
        let (port, token) = match (port, token) {
            (Some(p), Some(t)) => (p, t),
            _ => return,
        };
        let client = DaemonClient::new(port, token.clone());
        let (kt, kr) = oneshot::channel();
        let sup_tx = self.sup_tx.clone();
        let h = tokio::spawn(async move {
            supervisor_loop(client, sup_tx, kr).await;
        });
        self.sup_killer = Some(kt);
        self.sup_handle = Some(h);

        // WS bridge mirrors daemon-emitted events into our local emitter.
        let (bkt, bkr) = oneshot::channel();
        let bridge_emitter = self.emitter.clone();
        let ssh_id = self.config.id.clone();
        let bh = tokio::spawn(async move {
            crate::remote::ws_bridge::bridge_loop(ssh_id, port, token, bridge_emitter, bkr)
                .await;
        });
        self.bridge_killer = Some(bkt);
        self.bridge_handle = Some(bh);
    }
}

async fn run_loop(
    config: ConnectionConfig,
    state: Arc<RwLock<RuntimeState>>,
    emitter: EventEmitter,
    manifest: Arc<RemoteDaemonManifest>,
    mut rx: mpsc::Receiver<ControlMessage>,
) {
    let (sup_tx, mut sup_rx) = mpsc::channel::<SupervisorEvent>(4);
    let mut ctx = LoopCtx {
        config,
        state,
        emitter,
        manifest,
        sup_tx,
        sup_killer: None,
        sup_handle: None,
        bridge_killer: None,
        bridge_handle: None,
    };

    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => {
                let Some(msg) = msg else { break; };
                handle_control(&mut ctx, msg).await;
            }
            Some(evt) = sup_rx.recv() => {
                handle_supervisor(&mut ctx, &mut rx, evt).await;
            }
        }
    }

    // Channel closed → drop supervisor and owned children.
    ctx.kill_supervisor().await;
    disconnect(&ctx.config, &ctx.state, &ctx.emitter).await;
}

async fn handle_control(ctx: &mut LoopCtx, msg: ControlMessage) {
    match msg {
        ControlMessage::Connect => {
            ctx.kill_supervisor().await;
            connect_pipeline(&ctx.config, &ctx.state, &ctx.emitter, &ctx.manifest).await;
            let is_live = matches!(
                ctx.state.read().await.status,
                ConnectionStatus::Live
            );
            if is_live {
                ctx.spawn_supervisor().await;
            }
        }
        ControlMessage::Disconnect => {
            ctx.kill_supervisor().await;
            disconnect(&ctx.config, &ctx.state, &ctx.emitter).await;
        }
        ControlMessage::HardReset => {
            ctx.kill_supervisor().await;
            disconnect(&ctx.config, &ctx.state, &ctx.emitter).await;
            connect_pipeline(&ctx.config, &ctx.state, &ctx.emitter, &ctx.manifest).await;
            let is_live = matches!(
                ctx.state.read().await.status,
                ConnectionStatus::Live
            );
            if is_live {
                ctx.spawn_supervisor().await;
            }
        }
        ControlMessage::ResumeAfterManual => {
            // No supervisor to kill — we got here from AwaitingManual.
            connect_pipeline(&ctx.config, &ctx.state, &ctx.emitter, &ctx.manifest).await;
            let is_live = matches!(
                ctx.state.read().await.status,
                ConnectionStatus::Live
            );
            if is_live {
                ctx.spawn_supervisor().await;
            }
        }
    }
}

async fn handle_supervisor(
    ctx: &mut LoopCtx,
    rx: &mut mpsc::Receiver<ControlMessage>,
    evt: SupervisorEvent,
) {
    let SupervisorEvent::OutageDetected { reason } = evt;

    // Supervisor task already exited after sending the event. Drop the now-stale handle.
    ctx.sup_killer = None;
    ctx.sup_handle = None;

    // Tear down stale children before we re-attempt the pipeline.
    disconnect(&ctx.config, &ctx.state, &ctx.emitter).await;
    {
        let mut s = ctx.state.write().await;
        s.last_error = Some(reason.clone());
    }

    let mut attempt: u32 = 1;
    while attempt <= RECONNECT_MAX_ATTEMPTS {
        set_status(
            &ctx.state,
            &ctx.emitter,
            &ctx.config.id,
            ConnectionStatus::Reconnecting { attempt },
        )
        .await;

        // Race the backoff sleep against any incoming user message. A user
        // intervention (Disconnect / HardReset / Connect / ResumeAfterManual)
        // always preempts the auto-reconnect.
        let backoff = compute_backoff(attempt);
        let interrupted: Option<ControlMessage> = tokio::select! {
            biased;
            msg = rx.recv() => msg,
            _ = tokio::time::sleep(backoff) => None,
        };
        if let Some(msg) = interrupted {
            handle_control(ctx, msg).await;
            return;
        }

        connect_pipeline(&ctx.config, &ctx.state, &ctx.emitter, &ctx.manifest).await;
        let outcome = ctx.state.read().await.status.clone();
        match outcome {
            ConnectionStatus::Live => {
                ctx.spawn_supervisor().await;
                return;
            }
            ConnectionStatus::AwaitingManual => return,
            ConnectionStatus::Error => {
                attempt += 1;
                continue;
            }
            _ => return, // unexpected — bail out and wait for user
        }
    }
    // Exhausted attempts: status remains Error from the last try; the user
    // must Hard Reset (or Disconnect) to break the cycle.
}

fn compute_backoff(attempt: u32) -> Duration {
    let s = match attempt {
        1 => 1,
        2 => 3,
        3 => 10,
        4 => 30,
        _ => 60,
    };
    Duration::from_secs(s)
}

async fn supervisor_loop(
    client: DaemonClient,
    sup_tx: mpsc::Sender<SupervisorEvent>,
    mut killer: oneshot::Receiver<()>,
) {
    let mut consecutive_failures: u32 = 0;
    loop {
        let sleep = tokio::time::sleep(SUPERVISOR_INTERVAL);
        tokio::pin!(sleep);
        tokio::select! {
            biased;
            _ = &mut killer => return,
            _ = &mut sleep => {}
        }
        match client.health().await {
            Ok(()) => {
                consecutive_failures = 0;
            }
            Err(e) => {
                consecutive_failures += 1;
                if consecutive_failures >= SUPERVISOR_FAILURE_THRESHOLD {
                    let _ = sup_tx
                        .send(SupervisorEvent::OutageDetected {
                            reason: format!("health probe failed: {e}"),
                        })
                        .await;
                    return;
                }
            }
        }
    }
}

async fn connect_pipeline(
    config: &ConnectionConfig,
    state: &Arc<RwLock<RuntimeState>>,
    emitter: &EventEmitter,
    manifest: &Arc<RemoteDaemonManifest>,
) {
    let ssh_args = base_ssh_args(config);

    set_status(state, emitter, &config.id, ConnectionStatus::Probing).await;
    let platform = match probe(&ssh_args).await {
        Ok(p) => p,
        Err(e) => {
            return set_error(state, emitter, &config.id, format!("probe: {e}")).await;
        }
    };

    set_status(state, emitter, &config.id, ConnectionStatus::Deploying).await;
    let target = match deploy(&ssh_args, &platform, manifest).await {
        Ok(t) => t,
        Err(DeployError::ManualRequired { target }) => {
            let instr = instructions_for(&target, &build_ssh_target(config));
            {
                let mut s = state.write().await;
                s.deployment_target = Some(*target);
                s.manual_instructions = Some(instr);
            }
            return set_status(state, emitter, &config.id, ConnectionStatus::AwaitingManual).await;
        }
        Err(e) => {
            return set_error(state, emitter, &config.id, format!("deploy: {e}")).await;
        }
    };
    {
        let mut s = state.write().await;
        s.deployment_target = Some(target.clone());
    }

    set_status(state, emitter, &config.id, ConnectionStatus::Launching).await;
    let launched = match launch_daemon(&ssh_args, &target).await {
        Ok(l) => l,
        Err(e) => {
            return set_error(state, emitter, &config.id, format!("launch: {e}")).await;
        }
    };
    let handshake = launched.handshake.clone();
    let remote_port = handshake.port;
    let token = handshake.token.clone();
    {
        let mut s = state.write().await;
        s.handshake = Some(handshake);
        s.launched = Some(launched);
    }

    let tunnel = match establish_forward(&ssh_args, remote_port).await {
        Ok(t) => t,
        Err(e) => {
            return set_error(state, emitter, &config.id, format!("tunnel: {e}")).await;
        }
    };
    let local_port = tunnel.local_port;
    {
        let mut s = state.write().await;
        s.local_port = Some(local_port);
        s.tunnel = Some(tunnel);
    }

    set_status(state, emitter, &config.id, ConnectionStatus::Handshaking).await;
    let client = DaemonClient::new(local_port, token);
    let caps = match client.capabilities().await {
        Ok(c) => c,
        Err(e) => {
            return set_error(state, emitter, &config.id, format!("capabilities: {e}")).await;
        }
    };

    if let Err(reason) = check_version_compat(&caps.version) {
        return set_error(state, emitter, &config.id, reason).await;
    }
    {
        let mut s = state.write().await;
        s.capabilities = Some(caps);
    }

    set_status(state, emitter, &config.id, ConnectionStatus::Live).await;
}

async fn disconnect(
    config: &ConnectionConfig,
    state: &Arc<RwLock<RuntimeState>>,
    emitter: &EventEmitter,
) {
    let (launched, tunnel) = {
        let mut s = state.write().await;
        (s.launched.take(), s.tunnel.take())
    };
    if let Some(t) = tunnel {
        t.shutdown().await;
    }
    if let Some(d) = launched {
        d.shutdown().await;
    }
    {
        let mut s = state.write().await;
        s.local_port = None;
        s.handshake = None;
        s.capabilities = None;
    }
    set_status(state, emitter, &config.id, ConnectionStatus::Disconnected).await;
}

async fn set_status(
    state: &Arc<RwLock<RuntimeState>>,
    emitter: &EventEmitter,
    id: &str,
    status: ConnectionStatus,
) {
    {
        let mut s = state.write().await;
        s.status = status.clone();
        s.last_status_change_at = Instant::now();
    }
    let payload = serde_json::json!({
        "connection_id": id,
        "status": status.channel_label(),
        "detail": status,
    });
    emit_event(emitter, STATUS_EVENT, payload);
}

async fn set_error(
    state: &Arc<RwLock<RuntimeState>>,
    emitter: &EventEmitter,
    id: &str,
    msg: String,
) {
    {
        let mut s = state.write().await;
        s.status = ConnectionStatus::Error;
        s.last_error = Some(msg.clone());
        s.last_status_change_at = Instant::now();
    }
    let payload = serde_json::json!({
        "connection_id": id,
        "status": "error",
        "error": msg,
    });
    emit_event(emitter, STATUS_EVENT, payload);
}

fn check_version_compat(daemon_version: &str) -> Result<(), String> {
    let desktop = env!("CARGO_PKG_VERSION");
    let d = parse_semver(desktop).map_err(|e| format!("desktop version invalid: {e}"))?;
    let r = parse_semver(daemon_version).map_err(|e| format!("daemon version invalid: {e}"))?;
    if d.0 != r.0 {
        return Err(format!(
            "Major version mismatch: desktop {desktop} vs daemon {daemon_version}. \
             Please align them on the same major version."
        ));
    }
    if d.1 != r.1 {
        eprintln!(
            "[Remote] minor version mismatch: desktop {desktop} vs daemon {daemon_version} (continuing)"
        );
    }
    Ok(())
}

fn parse_semver(s: &str) -> Result<(u32, u32, u32), String> {
    let trimmed = s.trim_start_matches('v');
    let parts: Vec<&str> = trimmed.splitn(3, '.').collect();
    if parts.len() != 3 {
        return Err(format!("expected MAJOR.MINOR.PATCH, got {s}"));
    }
    let major: u32 = parts[0].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    let minor: u32 = parts[1].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    let patch: u32 = parts[2]
        .split('-')
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    Ok((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_basic() {
        assert_eq!(parse_semver("0.12.0").unwrap(), (0, 12, 0));
        assert_eq!(parse_semver("v0.12.0").unwrap(), (0, 12, 0));
    }

    #[test]
    fn semver_with_prerelease() {
        assert_eq!(parse_semver("0.12.0-rc.1").unwrap(), (0, 12, 0));
    }

    #[test]
    fn semver_invalid() {
        assert!(parse_semver("foo").is_err());
        assert!(parse_semver("1.2").is_err());
    }

    #[test]
    fn version_compat_major_diff_rejected() {
        // desktop = env!("CARGO_PKG_VERSION") (e.g. 0.12.0); daemon = 1.0.0
        assert!(check_version_compat("1.0.0").is_err());
    }

    #[test]
    fn version_compat_minor_diff_ok() {
        let pkg = env!("CARGO_PKG_VERSION");
        let (maj, _min, _) = parse_semver(pkg).unwrap();
        let bumped = format!("{maj}.99.0");
        assert!(check_version_compat(&bumped).is_ok());
    }

    #[test]
    fn backoff_schedule_matches_table() {
        assert_eq!(compute_backoff(1), Duration::from_secs(1));
        assert_eq!(compute_backoff(2), Duration::from_secs(3));
        assert_eq!(compute_backoff(3), Duration::from_secs(10));
        assert_eq!(compute_backoff(4), Duration::from_secs(30));
        assert_eq!(compute_backoff(5), Duration::from_secs(60));
        assert_eq!(compute_backoff(99), Duration::from_secs(60));
    }

    /// Verifies that a single transient health failure does NOT escalate to
    /// an outage (consecutive-failures threshold is 2). We start the
    /// supervisor against a server that fails the first ping then succeeds,
    /// give it enough time to perform two ticks, and then assert no
    /// `OutageDetected` event was emitted.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn supervisor_tolerates_single_transient_failure() {
        use std::sync::atomic::{AtomicU32, Ordering};

        // Bind a real loopback port and accept a single connection that
        // returns 500, then accept further connections returning 200.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let n = counter_clone.fetch_add(1, Ordering::SeqCst);
                let resp = if n == 0 {
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n"
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
                };
                use tokio::io::AsyncWriteExt;
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        let client = DaemonClient::new(port, "test-token".into());
        let (sup_tx, mut sup_rx) = mpsc::channel::<SupervisorEvent>(4);
        let (_killer_tx, killer_rx) = oneshot::channel();

        let sup = tokio::spawn(supervisor_loop(client, sup_tx, killer_rx));

        // Two intervals worth of paused time → supervisor probes twice.
        tokio::time::advance(SUPERVISOR_INTERVAL * 2 + Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        // No outage should have been signalled (one failure, then a success
        // resets the counter).
        assert!(sup_rx.try_recv().is_err());
        sup.abort();
    }

    /// Two consecutive failures must escalate to OutageDetected.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn supervisor_fires_on_two_consecutive_failures() {
        // Always return 500.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                use tokio::io::AsyncWriteExt;
                let _ = stream
                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n")
                    .await;
                let _ = stream.shutdown().await;
            }
        });

        let client = DaemonClient::new(port, "test-token".into());
        let (sup_tx, mut sup_rx) = mpsc::channel::<SupervisorEvent>(4);
        let (_killer_tx, killer_rx) = oneshot::channel();

        let sup = tokio::spawn(supervisor_loop(client, sup_tx, killer_rx));

        // Allow up to 3 intervals so both failures land and the event is sent.
        for _ in 0..40 {
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            if let Ok(evt) = sup_rx.try_recv() {
                match evt {
                    SupervisorEvent::OutageDetected { .. } => {
                        sup.abort();
                        return;
                    }
                }
            }
        }
        panic!("supervisor did not emit OutageDetected within 40 ticks");
    }

    /// Killing the supervisor before any failure must let it return cleanly.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn supervisor_exits_on_killer_signal() {
        // Listener that never responds; we only care about the killer path.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        std::mem::forget(listener); // keep the port held; never accept

        let client = DaemonClient::new(port, "test-token".into());
        let (sup_tx, mut sup_rx) = mpsc::channel::<SupervisorEvent>(4);
        let (killer_tx, killer_rx) = oneshot::channel();

        let sup = tokio::spawn(supervisor_loop(client, sup_tx, killer_rx));
        // Kill before the first interval elapses.
        let _ = killer_tx.send(());

        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        let res = tokio::time::timeout(Duration::from_secs(1), sup).await;
        assert!(
            res.is_ok(),
            "supervisor did not exit promptly after killer signal"
        );
        // No event should have been emitted.
        assert!(sup_rx.try_recv().is_err());
    }
}
