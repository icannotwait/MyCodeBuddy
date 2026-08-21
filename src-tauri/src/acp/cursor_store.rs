#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    const COMPOUND_ID: &str = "call-cursor-1\nfc_abc_0";
    const SESSION: &str = "0198c9aa-1111-2222-3333-444455556666";
    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_cursor_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codeg-cursor-store-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_store(path: &Path, rows: &[serde_json::Value]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        for (i, row) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
                rusqlite::params![format!("row-{i}"), serde_json::to_vec(row).unwrap()],
            )
            .unwrap();
        }
    }

    fn write_raw_store(path: &Path, rows: &[Vec<u8>]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        for (i, row) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
                rusqlite::params![format!("row-{i}"), row],
            )
            .unwrap();
        }
    }

    fn push_varint(out: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            out.push((value as u8) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn length_delimited(field_number: u32, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        push_varint(&mut out, u64::from(field_number) << 3 | 2);
        push_varint(&mut out, value.len() as u64);
        out.extend_from_slice(value);
        out
    }

    fn varint_value(field_number: u32, value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        push_varint(&mut out, u64::from(field_number) << 3);
        push_varint(&mut out, value);
        out
    }

    fn string_value(value: &str) -> Vec<u8> {
        length_delimited(3, value.as_bytes())
    }

    fn bool_value(value: bool) -> Vec<u8> {
        varint_value(4, u64::from(value))
    }

    fn null_value() -> Vec<u8> {
        varint_value(1, 0)
    }

    fn number_value(value: f64) -> Vec<u8> {
        let mut out = vec![(2_u8 << 3) | 1];
        out.extend(value.to_bits().to_le_bytes());
        out
    }

    fn value_map_entry(key: &str, value: Vec<u8>) -> Vec<u8> {
        let mut entry = length_delimited(1, key.as_bytes());
        entry.extend(length_delimited(2, &value));
        entry
    }

    fn struct_value(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut structure = Vec::new();
        for (key, value) in entries {
            structure.extend(length_delimited(1, &value_map_entry(key, value)));
        }
        length_delimited(5, &structure)
    }

    fn list_value(values: Vec<Vec<u8>>) -> Vec<u8> {
        let mut list = Vec::new();
        for value in values {
            list.extend(length_delimited(1, &value));
        }
        length_delimited(6, &list)
    }

    fn protobuf_mcp_message_with_args(
        tool_call_id: &str,
        tool_name: &str,
        args: Vec<(&str, Vec<u8>)>,
    ) -> Vec<u8> {
        // Cursor's generated `agent.v1.McpArgs`: fields 2/3/5 are args,
        // tool_call_id and tool_name. The outer length-delimited messages
        // mirror the fact that McpArgs is nested in Cursor's persisted state.
        let mut mcp = length_delimited(1, b"codeg-mcp-delegate_to_agent");
        for (key, value) in args {
            mcp.extend(length_delimited(2, &value_map_entry(key, value)));
        }
        mcp.extend(length_delimited(3, tool_call_id.as_bytes()));
        mcp.extend(length_delimited(4, b"codeg-mcp"));
        mcp.extend(length_delimited(5, tool_name.as_bytes()));
        mcp.extend(length_delimited(9, b"codeg-mcp"));
        mcp
    }

    fn protobuf_mcp_blob_with_args(
        tool_call_id: &str,
        tool_name: &str,
        args: Vec<(&str, Vec<u8>)>,
    ) -> Vec<u8> {
        let mcp = protobuf_mcp_message_with_args(tool_call_id, tool_name, args);
        length_delimited(2, &length_delimited(7, &mcp))
    }

    fn nest_protobuf_message(mut message: Vec<u8>, depth: usize) -> Vec<u8> {
        for _ in 0..depth {
            message = length_delimited(7, &message);
        }
        message
    }

    fn protobuf_mcp_blob(tool_call_id: &str) -> Vec<u8> {
        protobuf_mcp_blob_with_args(
            tool_call_id,
            "delegate_to_agent",
            vec![
                ("agent_type", string_value("codex")),
                ("task", string_value("build it")),
                ("correlation_id", string_value("proto-corr-1")),
            ],
        )
    }

    /// Like [`write_store`] but switches the store to WAL journal mode and
    /// returns the writer connection instead of dropping it. Cursor's CLI
    /// keeps live stores in WAL mode with most data uncheckpointed in
    /// `-wal`; the caller must keep the returned connection alive for as
    /// long as the test wants that data to stay out of the main file
    /// (SQLite auto-checkpoints on last-connection-close by default).
    fn write_store_wal(path: &Path, rows: &[serde_json::Value]) -> Connection {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        for (i, row) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
                rusqlite::params![format!("row-{i}"), serde_json::to_vec(row).unwrap()],
            )
            .unwrap();
        }
        conn
    }

    fn tool_call_blob(
        tool_call_id: &str,
        tool_name: &str,
        args: serde_json::Value,
    ) -> serde_json::Value {
        json!({
            "content": [{
                "type": "tool-call",
                "toolCallId": tool_call_id,
                "toolName": tool_name,
                "args": args
            }]
        })
    }

    #[test]
    fn validate_session_id_rejects_traversal_and_absolute_paths() {
        for bad in [
            "",
            ".",
            "..",
            "foo/bar",
            "foo\\bar",
            "../x",
            "/tmp/sess",
            "C:\\sess",
            "\\\\server\\share",
        ] {
            assert_eq!(
                validate_cursor_session_id(bad),
                Err(CursorStoreError::InvalidSessionId),
                "{bad:?}"
            );
        }
        assert!(validate_cursor_session_id(SESSION).is_ok());
    }

    #[test]
    fn current_flat_path_wins_over_legacy() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let flat = root.join("acp-sessions").join(SESSION).join("store.db");
        let legacy = root
            .join("chats")
            .join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .join(SESSION)
            .join("store.db");
        write_store(
            &flat,
            &[tool_call_blob(
                COMPOUND_ID,
                "delegate_to_agent",
                json!({"agent_type":"codex","task":"from-flat","correlation_id":"c1"}),
            )],
        );
        write_store(
            &legacy,
            &[tool_call_blob(
                COMPOUND_ID,
                "delegate_to_agent",
                json!({"agent_type":"codex","task":"from-legacy","correlation_id":"c1"}),
            )],
        );
        assert_eq!(reader.resolve_store_path(SESSION).unwrap(), flat);
        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID).unwrap().args["task"],
            "from-flat"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn legacy_hashed_path_resolves_when_exactly_one_match() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let legacy = root
            .join("chats")
            .join("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .join(SESSION)
            .join("store.db");
        write_store(
            &legacy,
            &[tool_call_blob(
                COMPOUND_ID,
                "delegate_to_agent",
                json!({"agent_type":"codex","task":"legacy","correlation_id":"c1"}),
            )],
        );
        assert_eq!(reader.resolve_store_path(SESSION).unwrap(), legacy);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn multiple_legacy_matches_fail_closed() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        for hash in [
            "11111111111111111111111111111111",
            "22222222222222222222222222222222",
        ] {
            write_store(
                &root.join("chats").join(hash).join(SESSION).join("store.db"),
                &[json!({})],
            );
        }
        assert_eq!(
            reader.resolve_store_path(SESSION),
            Err(CursorStoreError::StoreAmbiguous)
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn lookup_matches_compound_id_skips_junk_and_ignores_tool_result() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        let args = json!({"agent_type":"codex","task":"build it","correlation_id":"c1"});
        write_store(
            &store,
            &[
                json!({"not":"content"}),
                json!({"content": "not-an-array"}),
                json!({"content": [{
                    "type": "tool-result",
                    "toolCallId": COMPOUND_ID,
                    "output": "SECRET_OUTPUT"
                }]}),
                json!({"content": [{
                    "type": "tool-call",
                    "toolCallId": "other-id",
                    "toolName": "delegate_to_agent",
                    "args": {"task": "nope"}
                }]}),
                tool_call_blob(COMPOUND_ID, "delegate_to_agent", args.clone()),
            ],
        );
        let conn = Connection::open(&store).unwrap();
        conn.execute(
            "INSERT INTO blobs (id, data) VALUES ('bin', ?1)",
            rusqlite::params![&[0xff_u8, 0x00, 0xfe][..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO blobs (id, data) VALUES ('bad-json', ?1)",
            rusqlite::params![b"{not json"],
        )
        .unwrap();
        drop(conn);
        let found = reader.lookup(SESSION, COMPOUND_ID).unwrap();
        assert_eq!(found.tool_name, "delegate_to_agent");
        assert_eq!(found.args, args);
        assert!(!found.args.to_string().contains("SECRET_OUTPUT"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn lookup_recovers_binary_mcp_args_before_json_tool_call_is_persisted() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        write_raw_store(&store, &[protobuf_mcp_blob(COMPOUND_ID)]);

        let found = reader.lookup(SESSION, COMPOUND_ID).unwrap();
        assert_eq!(found.tool_name, "delegate_to_agent");
        assert_eq!(
            found.args,
            json!({
                "agent_type": "codex",
                "task": "build it",
                "correlation_id": "proto-corr-1"
            })
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn lookup_decodes_nested_protobuf_structs_and_lists() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        let nested = struct_value(vec![
            ("enabled", bool_value(true)),
            ("threshold", number_value(2.5)),
            (
                "labels",
                list_value(vec![string_value("one"), null_value()]),
            ),
        ]);
        write_raw_store(
            &store,
            &[protobuf_mcp_blob_with_args(
                COMPOUND_ID,
                "delegate_to_agent",
                vec![("settings", nested)],
            )],
        );

        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID).unwrap().args,
            json!({
                "settings": {
                    "enabled": true,
                    "threshold": 2.5,
                    "labels": ["one", null]
                }
            })
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn malformed_anchored_protobuf_record_is_not_accepted() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        write_raw_store(
            &store,
            &[protobuf_mcp_blob_with_args(
                COMPOUND_ID,
                "delegate_to_agent",
                vec![("task", vec![0x1a, 0x05, b'x'])],
            )],
        );

        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID),
            Err(CursorStoreError::ConflictingRecords)
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn malformed_anchored_record_rejects_valid_sibling() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        let malformed = protobuf_mcp_blob_with_args(
            COMPOUND_ID,
            "delegate_to_agent",
            vec![("task", vec![0x1a, 0x05, b'x'])],
        );
        write_raw_store(&store, &[protobuf_mcp_blob(COMPOUND_ID), malformed]);

        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID),
            Err(CursorStoreError::ConflictingRecords)
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn protobuf_scan_accepts_max_depth_and_rejects_deeper_candidate() {
        let message = || {
            protobuf_mcp_message_with_args(
                COMPOUND_ID,
                "delegate_to_agent",
                vec![("task", string_value("depth"))],
            )
        };

        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        write_raw_store(&store, &[nest_protobuf_message(message(), 24)]);
        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID).unwrap().args["task"],
            "depth"
        );
        std::fs::remove_dir_all(root).ok();

        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        write_raw_store(&store, &[nest_protobuf_message(message(), 25)]);
        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID),
            Err(CursorStoreError::StoreUnreadable)
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn conflicting_json_and_protobuf_records_fail_closed() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        write_raw_store(
            &store,
            &[
                serde_json::to_vec(&tool_call_blob(
                    COMPOUND_ID,
                    "delegate_to_agent",
                    json!({
                        "agent_type": "codex",
                        "task": "different",
                        "correlation_id": "proto-corr-1"
                    }),
                ))
                .unwrap(),
                protobuf_mcp_blob(COMPOUND_ID),
            ],
        );

        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID),
            Err(CursorStoreError::ConflictingRecords)
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn identical_json_and_protobuf_records_are_accepted() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        let args = json!({
            "agent_type": "codex",
            "task": "build it",
            "correlation_id": "proto-corr-1"
        });
        write_raw_store(
            &store,
            &[
                serde_json::to_vec(&tool_call_blob(
                    COMPOUND_ID,
                    "delegate_to_agent",
                    args.clone(),
                ))
                .unwrap(),
                protobuf_mcp_blob(COMPOUND_ID),
            ],
        );

        assert_eq!(reader.lookup(SESSION, COMPOUND_ID).unwrap().args, args);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn identical_json_and_protobuf_integer_numbers_are_accepted() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        let args = json!({"attempt": 1});
        write_raw_store(
            &store,
            &[
                serde_json::to_vec(&tool_call_blob(
                    COMPOUND_ID,
                    "delegate_to_agent",
                    args.clone(),
                ))
                .unwrap(),
                protobuf_mcp_blob_with_args(
                    COMPOUND_ID,
                    "delegate_to_agent",
                    vec![("attempt", number_value(1.0))],
                ),
            ],
        );

        assert_eq!(reader.lookup(SESSION, COMPOUND_ID).unwrap().args, args);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn conflicting_same_id_records_fail_closed_independent_of_row_order() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        write_store(
            &store,
            &[
                tool_call_blob(
                    COMPOUND_ID,
                    "delegate_to_agent",
                    json!({"agent_type":"codex","task":"one","correlation_id":"c1"}),
                ),
                tool_call_blob(
                    COMPOUND_ID,
                    "delegate_to_agent",
                    json!({"agent_type":"codex","task":"two","correlation_id":"c1"}),
                ),
            ],
        );
        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID),
            Err(CursorStoreError::ConflictingRecords)
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn identical_repeated_records_are_accepted() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        let blob = tool_call_blob(
            COMPOUND_ID,
            "delegate_to_agent",
            json!({"agent_type":"codex","task":"same","correlation_id":"c1"}),
        );
        write_store(&store, &[blob.clone(), blob]);
        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID).unwrap().args["task"],
            "same"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_table_and_missing_store_are_classified() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID),
            Err(CursorStoreError::StoreNotFound)
        );
        let empty = root.join("acp-sessions").join(SESSION);
        std::fs::create_dir_all(&empty).unwrap();
        Connection::open(empty.join("store.db"))
            .unwrap()
            .execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID),
            Err(CursorStoreError::SchemaIncompatible)
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn lookup_does_not_mutate_store_bytes() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        write_store(
            &store,
            &[tool_call_blob(
                COMPOUND_ID,
                "delegate_to_agent",
                json!({"agent_type":"codex","task":"x","correlation_id":"c1"}),
            )],
        );
        let before = std::fs::read(&store).unwrap();
        reader.lookup(SESSION, COMPOUND_ID).unwrap();
        let after = std::fs::read(&store).unwrap();
        assert_eq!(before, after);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn lookup_reads_wal_mode_store_with_data_still_in_wal_segment() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        let args = json!({"agent_type":"codex","task":"wal read","correlation_id":"c1"});
        // Keep the writer connection open for the whole test: SQLite
        // auto-checkpoints WAL content into the main file when the last
        // connection closes, which would silently defeat the point of this
        // test (proving the reader can see rows that are *only* in `-wal`).
        let writer = write_store_wal(
            &store,
            &[tool_call_blob(
                COMPOUND_ID,
                "delegate_to_agent",
                args.clone(),
            )],
        );
        assert!(store.with_extension("db-wal").is_file());

        let found = reader.lookup(SESSION, COMPOUND_ID).unwrap();
        assert_eq!(found.args, args);

        drop(writer);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn lookup_maps_writer_held_lock_to_retryable_store_unreadable() {
        let root = temp_cursor_dir();
        let reader = CursorStoreReader::with_cursor_dir(root.clone());
        let store = root.join("acp-sessions").join(SESSION).join("store.db");
        write_store(
            &store,
            &[tool_call_blob(
                COMPOUND_ID,
                "delegate_to_agent",
                json!({"agent_type":"codex","task":"x","correlation_id":"c1"}),
            )],
        );

        // Hold an EXCLUSIVE lock on the rollback-journal-mode file so the
        // reader's own busy_timeout (50ms) expires while trying to acquire a
        // SHARED lock, reproducing the SQLITE_BUSY/SQLITE_LOCKED family a
        // live Cursor writer can transiently hold.
        let mut writer = Connection::open(&store).unwrap();
        let tx = writer
            .transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)
            .unwrap();

        assert_eq!(
            reader.lookup(SESSION, COMPOUND_ID),
            Err(CursorStoreError::StoreUnreadable)
        );

        drop(tx);
        std::fs::remove_dir_all(root).ok();
    }
}

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

use super::cursor_store_proto::{find_mcp_tool_calls, ProtobufScanBudget, ProtobufScanError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorStoredToolCall {
    pub tool_name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStoreError {
    InvalidSessionId,
    StoreNotFound,
    StoreAmbiguous,
    StoreUnreadable,
    SchemaIncompatible,
    NoExactMatch,
    ConflictingRecords,
}

pub struct CursorStoreReader {
    cursor_dir: PathBuf,
}

impl CursorStoreReader {
    pub fn with_cursor_dir(cursor_dir: PathBuf) -> Self {
        Self { cursor_dir }
    }

    pub fn production() -> Self {
        Self::with_cursor_dir(dirs::home_dir().unwrap_or_default().join(".cursor"))
    }

    pub fn resolve_store_path(&self, session_id: &str) -> Result<PathBuf, CursorStoreError> {
        validate_cursor_session_id(session_id)?;

        let flat = self
            .cursor_dir
            .join("acp-sessions")
            .join(session_id)
            .join("store.db");
        if flat.is_file() {
            return Ok(flat);
        }

        let chats = self.cursor_dir.join("chats");
        if !chats.is_dir() {
            return Err(CursorStoreError::StoreNotFound);
        }

        let entries = std::fs::read_dir(chats).map_err(|_| CursorStoreError::StoreUnreadable)?;
        let mut found = None;
        for entry in entries {
            let entry = entry.map_err(|_| CursorStoreError::StoreUnreadable)?;
            if !entry
                .file_type()
                .map_err(|_| CursorStoreError::StoreUnreadable)?
                .is_dir()
            {
                continue;
            }
            let candidate = entry.path().join(session_id).join("store.db");
            if candidate.is_file() {
                if found.is_some() {
                    return Err(CursorStoreError::StoreAmbiguous);
                }
                found = Some(candidate);
            }
        }
        found.ok_or(CursorStoreError::StoreNotFound)
    }

    pub fn lookup(
        &self,
        session_id: &str,
        tool_call_id: &str,
    ) -> Result<CursorStoredToolCall, CursorStoreError> {
        let path = self.resolve_store_path(session_id)?;
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&path, flags)
            .map_err(|_| CursorStoreError::StoreUnreadable)?;
        let _ = conn.busy_timeout(std::time::Duration::from_millis(50));

        let mut statement = conn
            .prepare("SELECT id, data FROM blobs")
            .map_err(|err| classify_sqlite_error(&err))?;
        let mut rows = statement
            .query([])
            .map_err(|err| classify_sqlite_error(&err))?;
        let mut found: Option<(String, Value, String)> = None;
        let mut protobuf_budget = ProtobufScanBudget::default();

        while let Some(row) = rows.next().map_err(|err| classify_sqlite_error(&err))? {
            protobuf_budget
                .record_store_row()
                .map_err(classify_protobuf_scan_error)?;
            let data = match row.get_ref(1) {
                Ok(rusqlite::types::ValueRef::Blob(data)) => data,
                Ok(rusqlite::types::ValueRef::Text(data)) => data,
                _ => continue,
            };
            protobuf_budget
                .record_store_bytes(data.len())
                .map_err(classify_protobuf_scan_error)?;
            if let Ok(text) = std::str::from_utf8(data) {
                if let Ok(value) = serde_json::from_str::<Value>(text) {
                    if let Some(content) = value.get("content").and_then(Value::as_array) {
                        for item in content {
                            if item.get("type").and_then(Value::as_str) != Some("tool-call")
                                || item.get("toolCallId").and_then(Value::as_str)
                                    != Some(tool_call_id)
                            {
                                continue;
                            }
                            let Some(tool_name) = item.get("toolName").and_then(Value::as_str)
                            else {
                                continue;
                            };
                            let Some(args) = item.get("args") else {
                                continue;
                            };
                            protobuf_budget
                                .record_json_candidate()
                                .map_err(classify_protobuf_scan_error)?;
                            merge_candidate(&mut found, tool_name.to_owned(), args.clone())?;
                        }
                    }
                }
            }

            let protobuf_candidates = find_mcp_tool_calls(data, tool_call_id, &mut protobuf_budget)
                .map_err(classify_protobuf_scan_error)?;
            for candidate in protobuf_candidates {
                merge_candidate(&mut found, candidate.tool_name, candidate.args)?;
            }
        }

        found
            .map(|(_, args, tool_name)| CursorStoredToolCall { tool_name, args })
            .ok_or(CursorStoreError::NoExactMatch)
    }
}

fn classify_protobuf_scan_error(err: ProtobufScanError) -> CursorStoreError {
    match err {
        ProtobufScanError::MalformedAnchoredRecord => CursorStoreError::ConflictingRecords,
        ProtobufScanError::WorkLimitExceeded => CursorStoreError::StoreUnreadable,
    }
}

fn merge_candidate(
    found: &mut Option<(String, Value, String)>,
    tool_name: String,
    args: Value,
) -> Result<(), CursorStoreError> {
    let normalized = normalize_tool_name(&tool_name);
    if let Some((existing_name, existing_args, _)) = found {
        if existing_name != &normalized || existing_args != &args {
            return Err(CursorStoreError::ConflictingRecords);
        }
    } else {
        *found = Some((normalized, args, tool_name));
    }
    Ok(())
}

/// Classifies a `prepare`/`query`/row-step failure against the reader's
/// read-only, no-recovery connection. `SQLITE_BUSY`/`SQLITE_LOCKED` (another
/// process holding the store) and `SQLITE_READONLY*` (most notably
/// `SQLITE_READONLY_RECOVERY`, which a read-only handle cannot service after
/// a Cursor CLI crash leaves the wal-index needing recovery) are transient —
/// the store may become readable once Cursor reopens it or the writer
/// releases the lock, so these map to the retryable [`CursorStoreError::StoreUnreadable`]
/// rather than the terminal [`CursorStoreError::SchemaIncompatible`]. Every
/// other failure (e.g. missing `blobs` table) stays `SchemaIncompatible`.
fn classify_sqlite_error(err: &rusqlite::Error) -> CursorStoreError {
    match err {
        rusqlite::Error::SqliteFailure(sqlite_err, _)
            if matches!(
                sqlite_err.code,
                rusqlite::ErrorCode::DatabaseBusy
                    | rusqlite::ErrorCode::DatabaseLocked
                    | rusqlite::ErrorCode::ReadOnly
            ) =>
        {
            CursorStoreError::StoreUnreadable
        }
        _ => CursorStoreError::SchemaIncompatible,
    }
}

fn normalize_tool_name(tool_name: &str) -> String {
    tool_name.to_ascii_lowercase().replace([' ', '-'], "_")
}

pub fn validate_cursor_session_id(session_id: &str) -> Result<(), CursorStoreError> {
    if session_id.is_empty() {
        return Err(CursorStoreError::InvalidSessionId);
    }
    if session_id.contains('/') || session_id.contains('\\') {
        return Err(CursorStoreError::InvalidSessionId);
    }
    let path = Path::new(session_id);
    if path.is_absolute() {
        return Err(CursorStoreError::InvalidSessionId);
    }
    let mut parts = path.components();
    match parts.next() {
        Some(Component::Normal(name))
            if name != "." && name != ".." && name == std::ffi::OsStr::new(session_id) => {}
        _ => return Err(CursorStoreError::InvalidSessionId),
    }
    if parts.next().is_some() {
        return Err(CursorStoreError::InvalidSessionId);
    }
    Ok(())
}
