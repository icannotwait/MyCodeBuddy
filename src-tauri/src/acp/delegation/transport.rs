//! Wire format for `codeg-mcp` companion ↔ main process round-trip over UDS
//! (Unix) or named pipe (Windows).
//!
//! The frame is dead simple: a little-endian `u32` byte length followed by
//! that many bytes of UTF-8 JSON. One request, one response — the companion
//! reopens the socket per `tools/call`. This trades a few extra connects for
//! a wire that's trivial to test and that doesn't need multiplexing
//! (a parent makes at most one delegation call at a time from the LLM's
//! perspective — the broker handles concurrency at a higher level).
//!
//! Why length-prefix instead of newline-delimited JSON? The LLM-issued
//! `task` arguments can contain newlines, and we'd rather avoid escaping
//! them into a single line. JSON-RPC over stdio uses newlines because
//! Content-Length headers add complexity; for an internal UDS we can do
//! better.

use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One delegation call's worth of input forwarded from the companion to the
/// main process. The main process re-validates `token` and maps
/// `parent_connection_id` to the live ACP connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerRequest {
    /// Shared secret minted by the main process when it spawned the agent CLI;
    /// the agent passes it through to the companion via `--token`. Rejects
    /// anything else.
    pub token: String,
    /// codeg-internal ACP connection UUID for the parent session.
    pub parent_connection_id: String,
    /// The MCP `tool_use_id` for the LLM-issued `delegate_to_agent` call.
    /// Used to bind the eventual child outcome back to the parent's
    /// tool_use_id in the UI / DB.
    pub parent_tool_use_id: String,
    /// Raw `arguments` JSON from the MCP `tools/call` request, schema-shaped
    /// per [`super::tool_schema_json`]. The main process re-parses into
    /// [`super::types::DelegationRequest`].
    pub input: Value,
}

/// The wrapped outcome the main process returns over the same socket.
/// `outcome` is a serialized [`super::types::DelegationOutcome`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerResponse {
    pub outcome: Value,
}

/// Maximum allowed frame size, 16 MiB. Guards against a misbehaving peer
/// allocating gigabytes when reading the length prefix.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Write one length-prefixed JSON frame.
pub async fn write_frame<W, T>(stream: &mut W, value: &T) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("encode: {e}")))?;
    let len: u32 = bytes
        .len()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame > u32::MAX"))?;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// Read one length-prefixed JSON frame. Rejects frames larger than
/// [`MAX_FRAME_BYTES`].
pub async fn read_frame<R, T>(stream: &mut R) -> io::Result<T>
where
    R: AsyncReadExt + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame {len} bytes exceeds cap {MAX_FRAME_BYTES}"),
        ));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("decode: {e}")))
}

/// One-shot client round-trip: connect, write the request, read the response,
/// drop the connection.
#[cfg(unix)]
pub async fn client_round_trip(
    socket_path: &str,
    req: &BrokerRequest,
) -> io::Result<BrokerResponse> {
    use tokio::net::UnixStream;
    let mut stream = UnixStream::connect(socket_path).await?;
    write_frame(&mut stream, req).await?;
    read_frame(&mut stream).await
}

/// Windows path uses named pipes; the address format is `\\.\pipe\<name>`.
#[cfg(windows)]
pub async fn client_round_trip(
    socket_path: &str,
    req: &BrokerRequest,
) -> io::Result<BrokerResponse> {
    use tokio::net::windows::named_pipe::ClientOptions;
    let mut stream = ClientOptions::new()
        .open(socket_path)
        .map_err(|e| io::Error::other(format!("open pipe: {e}")))?;
    write_frame(&mut stream, req).await?;
    read_frame(&mut stream).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::duplex;

    #[tokio::test]
    async fn frame_round_trip_in_memory() {
        let (mut a, mut b) = duplex(8 * 1024);
        let req = BrokerRequest {
            token: "tok".into(),
            parent_connection_id: "p1".into(),
            parent_tool_use_id: "pt1".into(),
            input: json!({"agent_type": "codex", "task": "hi"}),
        };
        write_frame(&mut a, &req).await.unwrap();
        let got: BrokerRequest = read_frame(&mut b).await.unwrap();
        assert_eq!(got.token, "tok");
        assert_eq!(got.input["agent_type"], "codex");
    }

    #[tokio::test]
    async fn rejects_oversized_frame() {
        let (mut a, mut b) = duplex(8);
        // Write a length prefix larger than the cap, no body.
        let bad_len: u32 = (MAX_FRAME_BYTES as u32) + 1;
        a.write_all(&bad_len.to_le_bytes()).await.unwrap();
        a.flush().await.unwrap();
        let result: io::Result<BrokerRequest> = read_frame(&mut b).await;
        let err = result.expect_err("expected oversized frame to be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn uds_round_trip() {
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codeg-mcp.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server_path = path.to_string_lossy().to_string();

        let server = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            let req: BrokerRequest = read_frame(&mut conn).await.unwrap();
            assert_eq!(req.token, "tok");
            let resp = BrokerResponse {
                outcome: json!({"kind": "ok", "text": "hello"}),
            };
            write_frame(&mut conn, &resp).await.unwrap();
        });

        let req = BrokerRequest {
            token: "tok".into(),
            parent_connection_id: "p1".into(),
            parent_tool_use_id: "pt1".into(),
            input: json!({"agent_type": "codex", "task": "do x"}),
        };
        let resp = client_round_trip(&server_path, &req).await.unwrap();
        assert_eq!(resp.outcome["kind"], "ok");
        assert_eq!(resp.outcome["text"], "hello");
        server.await.unwrap();
    }
}
