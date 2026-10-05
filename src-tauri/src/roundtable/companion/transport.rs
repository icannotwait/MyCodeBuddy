//! Attempt-scoped MCP broker over a local socket or Windows named pipe.
use std::sync::Arc;
use std::time::Duration;

use roundtable_protocol::{ErrorCode, RtResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::{JoinHandle, JoinSet};

use crate::roundtable::mcp::{invoke_scoped_tool, GateToolAuthority};
use crate::roundtable::rt_error;
use crate::roundtable::tool_core::{AttemptToken, RoundtableToolCall, ToolStore};

const FRAME_LIMIT: usize = 128 * 1024;
trait LocalStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> LocalStream for T {}
type Stream = Box<dyn LocalStream>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handshake {
    token: String,
    incarnation: String,
}

pub struct ServiceConnection {
    stream: Stream,
}

impl ServiceConnection {
    pub async fn request(&mut self, request: Value) -> RtResult<Value> {
        write(&mut self.stream, &request).await?;
        read(&mut self.stream).await
    }

    /// Proxy stdio without cancelling a partially read socket frame. The read
    /// future remains pinned while new stdin lines arrive.
    pub async fn serve_stdio<R, W>(self, input: R, mut output: W) -> RtResult<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (mut reader, mut writer) = tokio::io::split(self.stream);
        let mut input = BufReader::new(input);
        tokio::select! {
            result = async {
                loop {
                        let response: Value = read(&mut reader).await?;
                        let mut bytes = serde_json::to_vec(&response).map_err(|_| unavailable())?;
                        bytes.push(b'\n');
                        output.write_all(&bytes).await.map_err(|_| unavailable())?;
                        output.flush().await.map_err(|_| unavailable())?;
                }
            } => result,
            result = async {
                loop {
                        let mut line = Vec::new();
                        let count = (&mut input).take((FRAME_LIMIT + 1) as u64)
                            .read_until(b'\n', &mut line).await.map_err(|_| unavailable())?;
                        if count == 0 { return Ok(()); }
                        if line.len() > FRAME_LIMIT { return Err(rt_error(ErrorCode::InvalidArgument, "frame_limit")); }
                        let value: Value = serde_json::from_slice(&line)
                            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "invalid_json"))?;
                        write(&mut writer, &value).await?;
                }
            } => result,
        }
    }
}

pub(super) async fn connect(
    path: &str,
    incarnation: &str,
    token: &str,
) -> RtResult<ServiceConnection> {
    #[cfg(unix)]
    let mut stream: Stream = Box::new(
        tokio::net::UnixStream::connect(path)
            .await
            .map_err(|_| unavailable())?,
    );
    #[cfg(windows)]
    let mut stream: Stream = Box::new(
        tokio::net::windows::named_pipe::ClientOptions::new()
            .open(path)
            .map_err(|_| unavailable())?,
    );
    let hello = Handshake {
        token: token.to_owned(),
        incarnation: incarnation.to_owned(),
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        write(&mut stream, &hello).await?;
        let ack: Value = read(&mut stream).await?;
        if ack != json!({"ready":true}) {
            return Err(rt_error(ErrorCode::Unauthenticated, "broker_auth"));
        }
        Ok(())
    })
    .await
    .map_err(|_| unavailable())??;
    Ok(ServiceConnection { stream })
}

pub struct ServiceBroker {
    task: Option<JoinHandle<()>>,
    cancel: tokio_util::sync::CancellationToken,
    #[cfg(unix)]
    path: std::path::PathBuf,
}

impl ServiceBroker {
    pub async fn bind(
        path: &str,
        incarnation: &str,
        token: Arc<AttemptToken>,
        authority: Arc<GateToolAuthority>,
        store: Arc<dyn ToolStore>,
    ) -> RtResult<Self> {
        #[cfg(unix)]
        let listener = {
            use std::os::unix::fs::PermissionsExt;
            let listener = tokio::net::UnixListener::bind(path).map_err(|_| unavailable())?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| unavailable())?;
            listener
        };
        #[cfg(windows)]
        let mut listener = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(path)
            .map_err(|_| unavailable())?;
        let owned_path = path.to_owned();
        let incarnation = incarnation.to_owned();
        let cancel = tokio_util::sync::CancellationToken::new();
        let stop = cancel.clone();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                // Bound unauthenticated clients and reap completed handlers.
                if clients.len() >= 4 {
                    tokio::select! {_=stop.cancelled()=>break,_=clients.join_next()=>{}}
                }
                while clients.try_join_next().is_some() {}
                #[cfg(unix)]
                let stream: Stream = match tokio::select! {_=stop.cancelled()=>break,result=listener.accept()=>result}
                {
                    Ok((s, _)) => Box::new(s),
                    Err(_) => break,
                };
                #[cfg(windows)]
                let stream: Stream = {
                    if tokio::select! {_=stop.cancelled()=>break,result=listener.connect()=>result}
                        .is_err()
                    {
                        break;
                    }
                    let next = match tokio::net::windows::named_pipe::ServerOptions::new()
                        .reject_remote_clients(true)
                        .create(&owned_path)
                    {
                        Ok(next) => next,
                        Err(_) => break,
                    };
                    Box::new(std::mem::replace(&mut listener, next))
                };
                #[cfg(unix)]
                let _ = &owned_path;
                let (token, authority, store, incarnation) = (
                    Arc::clone(&token),
                    Arc::clone(&authority),
                    Arc::clone(&store),
                    incarnation.clone(),
                );
                clients.spawn(async move {
                    let _ = serve(stream, &incarnation, token, authority, store).await;
                });
            }
            clients.abort_all();
            while clients.join_next().await.is_some() {}
        });
        Ok(Self {
            task: Some(task),
            cancel,
            #[cfg(unix)]
            path: path.into(),
        })
    }

    pub async fn shutdown(mut self) {
        if let Some(task) = self.task.take() {
            self.cancel.cancel();
            let _ = task.await;
        }
    }
}

impl Drop for ServiceBroker {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn serve(
    mut stream: Stream,
    incarnation: &str,
    token: Arc<AttemptToken>,
    authority: Arc<GateToolAuthority>,
    store: Arc<dyn ToolStore>,
) -> RtResult<()> {
    let hello: Handshake = tokio::time::timeout(Duration::from_secs(5), read(&mut stream))
        .await
        .map_err(|_| unavailable())??;
    let left = hello.token.as_bytes();
    let right = token.reveal_for_same_sandbox().as_bytes();
    let matches = left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0;
    if !matches || hello.incarnation != incarnation {
        return Err(rt_error(ErrorCode::Unauthenticated, "broker_auth"));
    }
    write(&mut stream, &json!({"ready":true})).await?;
    loop {
        let request: Value = read(&mut stream).await?;
        if let Some(response) = dispatch(request, &token, &authority, store.as_ref()).await {
            write(&mut stream, &response).await?;
        }
    }
}

async fn dispatch(
    request: Value,
    token: &AttemptToken,
    authority: &GateToolAuthority,
    store: &dyn ToolStore,
) -> Option<Value> {
    let id = request.get("id")?.clone();
    let result = match request.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"codeg-roundtable","version":"1"}})
        }
        Some("ping") => json!({}),
        Some("tools/list") => json!({"tools":[
            {"name":"read_evidence","description":"Read frozen evidence lines","inputSchema":{"type":"object","additionalProperties":false,"required":["file_alias","start_line","end_line"],"properties":{"file_alias":{"type":"string"},"start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1}}}},
            {"name":"search_evidence","description":"Search frozen evidence literally","inputSchema":{"type":"object","additionalProperties":false,"required":["file_alias","query","limit"],"properties":{"file_alias":{"type":"string"},"query":{"type":"string","maxLength":256},"limit":{"type":"integer","minimum":1,"maximum":20}}}},
            {"name":"submit_result","description":"Submit a structured roundtable result","inputSchema":{"type":"object","additionalProperties":false,"required":["submission_id","result"],"properties":{"submission_id":{"type":"string"},"result":{"type":"object"}}}}
        ]}),
        Some("tools/call") => {
            let params = &request["params"];
            let call = RoundtableToolCall {
                name: params["name"].as_str().unwrap_or("").to_owned(),
                arguments: params.get("arguments").cloned().unwrap_or(Value::Null),
            };
            match invoke_scoped_tool(token, call, authority, store).await {
                Ok(response) => {
                    json!({"content":[{"type":"text","text":String::from_utf8_lossy(&response.body)}],"isError":false})
                }
                Err(error) => {
                    json!({"content":[{"type":"text","text":serde_json::to_string(&error).unwrap_or_default()}],"isError":true})
                }
            }
        }
        _ => {
            return Some(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
            )
        }
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn unavailable() -> roundtable_protocol::RtError {
    rt_error(ErrorCode::RuntimeUnavailable, "broker_unavailable")
}

async fn write<W: AsyncWrite + Unpin, T: Serialize>(stream: &mut W, value: &T) -> RtResult<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| unavailable())?;
    if bytes.len() > FRAME_LIMIT {
        return Err(rt_error(ErrorCode::InvalidArgument, "frame_limit"));
    }
    stream
        .write_u32_le(bytes.len() as u32)
        .await
        .map_err(|_| unavailable())?;
    stream.write_all(&bytes).await.map_err(|_| unavailable())?;
    stream.flush().await.map_err(|_| unavailable())
}

async fn read<R: AsyncRead + Unpin, T: for<'de> Deserialize<'de>>(stream: &mut R) -> RtResult<T> {
    let len = stream.read_u32_le().await.map_err(|_| unavailable())? as usize;
    if len > FRAME_LIMIT {
        return Err(rt_error(ErrorCode::InvalidArgument, "frame_limit"));
    }
    let mut bytes = vec![0; len];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| unavailable())?;
    serde_json::from_slice(&bytes).map_err(|_| rt_error(ErrorCode::InvalidArgument, "invalid_json"))
}
