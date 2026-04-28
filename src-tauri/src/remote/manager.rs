// Top-level scheduler for remote connections. Holds one `ConnectionTask`
// per connection id and dispatches `ControlMessage`s to it. Manifest is
// fetched lazily and cached for the desktop session.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use crate::models::connection::ConnectionConfig;
use crate::remote::connection::{ConnectionRuntime, ConnectionStatus, ConnectionTask, ControlMessage};
use crate::remote::http_client::DaemonClient;
use crate::remote::manifest::{self, RemoteDaemonManifest};
use crate::web::event_bridge::EventEmitter;

#[derive(Clone)]
pub struct RemoteConnectionManager {
    inner: Arc<Inner>,
}

struct Inner {
    tasks: RwLock<HashMap<String, ConnectionTask>>,
    manifest: RwLock<Option<Arc<RemoteDaemonManifest>>>,
    emitter: RwLock<EventEmitter>,
}

impl Default for RemoteConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteConnectionManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                tasks: RwLock::new(HashMap::new()),
                manifest: RwLock::new(None),
                emitter: RwLock::new(EventEmitter::Noop),
            }),
        }
    }

    /// Shallow clone sharing the same state, mirroring the pattern used by
    /// `ChatChannelManager` / ACP `ConnectionManager`.
    pub fn clone_ref(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }

    /// Replace the emitter (e.g. once Tauri's AppHandle is available at
    /// setup time). Existing tasks keep the snapshot they were spawned
    /// with — new tasks pick up the latest emitter.
    pub async fn set_emitter(&self, emitter: EventEmitter) {
        *self.inner.emitter.write().await = emitter;
    }

    /// Fetch and cache the manifest. Tolerant: failures are logged and the
    /// manager continues; `connect()` will retry on demand.
    pub async fn warm_up(&self) {
        let v = manifest::REMOTE_DAEMON_VERSION;
        match manifest::get_manifest(v).await {
            Ok(m) => {
                *self.inner.manifest.write().await = Some(Arc::new(m));
            }
            Err(e) => {
                eprintln!("[Remote] manifest warm-up failed: {e}");
            }
        }
    }

    pub async fn connect(&self, config: ConnectionConfig) -> Result<(), ConnectError> {
        let manifest = self.ensure_manifest().await?;
        let mut tasks = self.inner.tasks.write().await;
        if !tasks.contains_key(&config.id) {
            let emitter = self.inner.emitter.read().await.clone();
            let task = ConnectionTask::spawn(config.clone(), emitter, manifest);
            tasks.insert(config.id.clone(), task);
        }
        let task = tasks
            .get(&config.id)
            .expect("just inserted");
        task.control_tx
            .send(ControlMessage::Connect)
            .await
            .map_err(|_| ConnectError::TaskClosed)?;
        Ok(())
    }

    pub async fn disconnect(&self, connection_id: &str) -> Result<(), ConnectError> {
        let tasks = self.inner.tasks.read().await;
        if let Some(task) = tasks.get(connection_id) {
            task.control_tx
                .send(ControlMessage::Disconnect)
                .await
                .map_err(|_| ConnectError::TaskClosed)?;
        }
        Ok(())
    }

    pub async fn hard_reset(&self, connection_id: &str) -> Result<(), ConnectError> {
        let tasks = self.inner.tasks.read().await;
        if let Some(task) = tasks.get(connection_id) {
            task.control_tx
                .send(ControlMessage::HardReset)
                .await
                .map_err(|_| ConnectError::TaskClosed)?;
        }
        Ok(())
    }

    pub async fn resume_after_manual(&self, connection_id: &str) -> Result<(), ConnectError> {
        let tasks = self.inner.tasks.read().await;
        if let Some(task) = tasks.get(connection_id) {
            task.control_tx
                .send(ControlMessage::ResumeAfterManual)
                .await
                .map_err(|_| ConnectError::TaskClosed)?;
        }
        Ok(())
    }

    pub async fn current_runtime(&self, connection_id: &str) -> Option<ConnectionRuntime> {
        let tasks = self.inner.tasks.read().await;
        let task = tasks.get(connection_id)?;
        let s = task.state.read().await;
        Some(s.snapshot(connection_id))
    }

    /// Ensure the connection has reached `Live` and return a `DaemonClient`
    /// pointing at its tunnel. Triggers a `Connect` if no task exists yet
    /// or the current status isn't `Live`. Polls the runtime snapshot every
    /// 250 ms and gives up after `timeout`.
    ///
    /// Errors out early on `Error` (transmits `last_error`) or
    /// `AwaitingManual` (asks the user to finish manual install). M1 will
    /// add a reconnect supervisor; until then the caller is expected to
    /// surface the error and let the user retry.
    pub async fn ensure_live(
        &self,
        config: ConnectionConfig,
        timeout: Duration,
    ) -> Result<DaemonClient, EnsureLiveError> {
        // Take a snapshot first; if already Live we can short-circuit.
        if let Some(rt) = self.current_runtime(&config.id).await {
            if let ConnectionStatus::Live = rt.status {
                return rt_to_client(&rt).ok_or(EnsureLiveError::MissingHandshake);
            }
        }

        self.connect(config.clone())
            .await
            .map_err(EnsureLiveError::Connect)?;

        let deadline = Instant::now() + timeout;
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        loop {
            interval.tick().await;
            if let Some(rt) = self.current_runtime(&config.id).await {
                match &rt.status {
                    ConnectionStatus::Live => {
                        return rt_to_client(&rt).ok_or(EnsureLiveError::MissingHandshake);
                    }
                    ConnectionStatus::Error => {
                        return Err(EnsureLiveError::Failed(
                            rt.last_error
                                .unwrap_or_else(|| "unknown remote error".into()),
                        ));
                    }
                    ConnectionStatus::AwaitingManual => {
                        return Err(EnsureLiveError::AwaitingManual);
                    }
                    _ => {} // Probing/Deploying/Launching/Handshaking/etc — keep waiting
                }
            }
            if Instant::now() >= deadline {
                return Err(EnsureLiveError::Timeout);
            }
        }
    }

    /// Send Disconnect to every task. Used at desktop shutdown.
    pub async fn disconnect_all(&self) {
        let tasks = self.inner.tasks.read().await;
        for task in tasks.values() {
            let _ = task.control_tx.send(ControlMessage::Disconnect).await;
        }
    }

    async fn ensure_manifest(&self) -> Result<Arc<RemoteDaemonManifest>, ConnectError> {
        if let Some(m) = self.inner.manifest.read().await.clone() {
            return Ok(m);
        }
        let v = manifest::REMOTE_DAEMON_VERSION;
        let m = manifest::get_manifest(v)
            .await
            .map_err(|e| ConnectError::Manifest(e.to_string()))?;
        let arc = Arc::new(m);
        *self.inner.manifest.write().await = Some(arc.clone());
        Ok(arc)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("task channel closed")]
    TaskClosed,
}

#[derive(Debug, thiserror::Error)]
pub enum EnsureLiveError {
    #[error("connect: {0}")]
    Connect(ConnectError),
    #[error("timed out waiting for remote daemon to come online")]
    Timeout,
    #[error("remote daemon needs manual install (open settings)")]
    AwaitingManual,
    #[error("remote: {0}")]
    Failed(String),
    #[error("daemon handshake missing on Live runtime")]
    MissingHandshake,
}

fn rt_to_client(rt: &ConnectionRuntime) -> Option<DaemonClient> {
    let port = rt.local_port?;
    let token = rt.handshake.as_ref()?.token.clone();
    Some(DaemonClient::new(port, token))
}
