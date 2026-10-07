//! In-image loopback relay. The sandbox base URL is fixed. The relay reaches
//! only the Unix socket this process mounted. A caller-supplied target is
//! refused. A socket that cannot be mounted is `not_tested`, not a pass, and
//! this module does not open a public listener.

use roundtable_protocol::{ErrorCode, RtResult};

use super::rt_error;

pub const SANDBOX_ORIGIN: &str = "http://127.0.0.1:39173";
pub const SANDBOX_ENDPOINT: &str = "http://127.0.0.1:39173/v1";

#[derive(Debug)]
pub enum SocketProbe {
    NotTested { reason: &'static str },
    Mounted(InstanceSocket),
}

impl SocketProbe {
    pub fn status(&self) -> &'static str {
        match self {
            Self::NotTested { .. } => "not_tested",
            Self::Mounted(_) => "mounted",
        }
    }

    /// Mounting a private socket is not a qualification pass.
    pub fn is_passed(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct InstanceSocket {
    label: String,
    #[cfg(target_os = "linux")]
    path: std::path::PathBuf,
    #[cfg(target_os = "linux")]
    listener: std::os::unix::net::UnixListener,
}

impl InstanceSocket {
    #[cfg(target_os = "linux")]
    fn label(&self) -> &str {
        &self.label
    }

    #[cfg(not(target_os = "linux"))]
    fn label(&self) -> &str {
        &self.label
    }
}

#[cfg(target_os = "linux")]
impl Drop for InstanceSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
pub struct LoopbackRelay {
    endpoint: &'static str,
    #[cfg(target_os = "linux")]
    path: std::path::PathBuf,
}

impl LoopbackRelay {
    /// Forwards to the mounted instance socket. There is no target argument.
    pub fn forward(&self, bytes: &[u8]) -> RtResult<()> {
        if self.endpoint != SANDBOX_ENDPOINT {
            return Err(rt_error(ErrorCode::PolicyUnenforceable, "sandbox_endpoint"));
        }
        #[cfg(target_os = "linux")]
        {
            use std::io::Write;
            let mut stream = std::os::unix::net::UnixStream::connect(&self.path)
                .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "relay_connect"))?;
            stream
                .write_all(bytes)
                .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "relay_write"))?;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = bytes;
            Err(rt_error(
                ErrorCode::PolicyUnenforceable,
                "socket_not_tested",
            ))
        }
    }
}

pub fn probe_instance_socket() -> SocketProbe {
    #[cfg(target_os = "linux")]
    {
        match mount_private_socket() {
            Ok(socket) => SocketProbe::Mounted(socket),
            Err(_) => SocketProbe::NotTested {
                reason: "socket_unmounted",
            },
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        SocketProbe::NotTested {
            reason: "unix_socket_unavailable",
        }
    }
}

/// Image-helper entry. The helper cannot pass a path or URL.
pub fn relay_from_helper(probe: &SocketProbe) -> RtResult<LoopbackRelay> {
    match probe {
        SocketProbe::NotTested { .. } => Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "socket_not_tested",
        )),
        SocketProbe::Mounted(socket) => {
            let _ = socket.label();
            #[cfg(target_os = "linux")]
            {
                let _keepalive = &socket.listener;
                Ok(LoopbackRelay {
                    endpoint: SANDBOX_ENDPOINT,
                    path: socket.path.clone(),
                })
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(rt_error(
                    ErrorCode::PolicyUnenforceable,
                    "socket_not_tested",
                ))
            }
        }
    }
}

/// Always refuses. Refusal does not mount a socket and is not a pass.
pub fn forward_to_caller_target(target: &str, bytes: &[u8]) -> RtResult<()> {
    let _ = (target, bytes);
    Err(rt_error(
        ErrorCode::PolicyUnenforceable,
        "caller_supplied_target",
    ))
}

#[cfg(target_os = "linux")]
fn mount_private_socket() -> std::io::Result<InstanceSocket> {
    let path =
        std::env::temp_dir().join(format!("codeg-roundtable-{}.sock", rand::random::<u128>()));
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    Ok(InstanceSocket {
        label: format!("roundtable-{}", rand::random::<u128>()),
        path,
        listener,
    })
}

/// Real per-companion relay. The socket and TCP address are fixed inside the
/// qualified network namespace; each accepted stream is owned until shutdown.
pub struct ServiceModelRelay {
    task: tokio::task::JoinHandle<()>,
    cancel: tokio_util::sync::CancellationToken,
}
impl ServiceModelRelay {
    pub async fn start() -> RtResult<Self> {
        #[cfg(unix)]
        {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:39173")
                .await
                .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "relay_bind"))?;
            let cancel = tokio_util::sync::CancellationToken::new();
            let stop = cancel.clone();
            let task = tokio::spawn(async move {
                let mut clients = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        _ = stop.cancelled() => break,
                        _ = clients.join_next(), if !clients.is_empty() => {},
                        accepted = listener.accept(), if clients.len() < 4 => {
                            let Ok((mut tcp,_)) = accepted else {break};
                            clients.spawn(async move {
                                let _ = tokio::time::timeout(std::time::Duration::from_secs(600),async {
                                    let mut unix = tokio::net::UnixStream::connect("/run/codeg/gateway.sock").await?;
                                    tokio::io::copy_bidirectional(&mut tcp,&mut unix).await
                                }).await;
                            });
                        }
                    }
                }
                clients.abort_all();
                while clients.join_next().await.is_some() {}
            });
            Ok(Self { task, cancel })
        }
        #[cfg(not(unix))]
        {
            Err(rt_error(
                ErrorCode::PolicyUnenforceable,
                "platform_unqualified",
            ))
        }
    }
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        let _ = (&mut self.task).await;
    }
}
impl Drop for ServiceModelRelay {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}
