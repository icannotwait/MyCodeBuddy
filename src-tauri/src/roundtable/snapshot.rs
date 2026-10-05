//! Immutable source evidence for one roundtable phase.
//!
//! Bytes are copied through a file handle the snapshot opens itself. The
//! delivery prompt is `DeliveryEncoder`; this module does not encode a second
//! prompt. `base_commit` is stored and is not part of `manifest_hash`.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use roundtable_protocol::{
    canonical_hash, ActorContext, BindingId, ContextFreshness, ContextStateV1, DeliveryEncoder,
    DeliveryManifestV1, ErrorCode, Hash256, InternalReason, MandatoryTargetV1, ManifestId,
    OrderedMemberV1, PhaseId, PhaseKind, PhaseSnapshotV1, PreflightRecordV1, PublishedMessageRef,
    QualifiedContextProfile, ResolvedRecipientV1, Revision, RoleSnapshot, RoomId, RtError,
    RtResult, SafeInt, TokenBound, ToolQuotaV1, SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::objects::{ObjectRef, ObjectStore};
use super::rt_error;
use super::store::{NewManifest, RoundtableStore};

/// Initial read plus one bounded re-read. A third sample is `snapshot_unstable`.
pub const MAX_SNAPSHOT_READS: u32 = 2;

pub const FRESH_CONTEXT_STATE: &str = "fresh";

pub type SnapshotReadHook = Arc<dyn Fn(&Path) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceClass {
    Tracked,
    Dirty,
    Untracked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotEncoding {
    Utf8,
    Binary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedFile {
    pub relative_path: String,
    pub class: SourceClass,
}

#[derive(Clone)]
pub struct SourceSelection {
    pub root: PathBuf,
    pub room_id: RoomId,
    pub version: i64,
    pub base_commit: Option<String>,
    pub files: Vec<SelectedFile>,
    /// Called after the before-read metadata sample, while the handle is open.
    pub mutate_while_open: Option<SnapshotReadHook>,
}

#[derive(Clone, Copy, Debug)]
pub struct SnapshotLimits {
    pub estimated_bytes: u64,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_files: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEntryV1 {
    pub path: String,
    pub class: SourceClass,
    pub encoding: SnapshotEncoding,
    pub size: u64,
    pub mode: u32,
    pub captured_at_ms: u64,
    pub content_hash: Hash256,
    pub line_offsets: Vec<u64>,
    pub text_admissible: bool,
    pub object: ObjectRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestV1 {
    pub schema_version: u32,
    pub manifest_id: ManifestId,
    pub room_id: RoomId,
    pub version: i64,
    pub base_commit: Option<String>,
    pub read_limit: u32,
    pub entries: Vec<SourceEntryV1>,
    pub manifest_hash: Hash256,
}

impl SourceManifestV1 {
    pub fn accounted_bytes(&self) -> u64 {
        let mut seen = std::collections::BTreeSet::new();
        let mut total = 0u64;
        for entry in &self.entries {
            if seen.insert(entry.object.object_id.clone()) {
                total = total.saturating_add(entry.object.total_bytes);
            }
        }
        total
    }

    pub fn object_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.object.object_id.clone())
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct PhaseInput {
    pub phase_id: PhaseId,
    pub phase_index: u32,
    pub revision: Revision,
    pub kind: PhaseKind,
    pub critique_round: Option<u32>,
    pub config_version: Revision,
    pub question_version: Revision,
    pub interjection_version: Revision,
    pub source_manifest_id: ManifestId,
    pub source_manifest_hash: Hash256,
    pub published_messages: Vec<PublishedMessageRef>,
    pub members: Vec<OrderedMemberV1>,
    pub mandatory_targets: Vec<MandatoryTargetV1>,
    pub policy_hash: Hash256,
    pub output_byte_limit: SafeInt,
    pub tool_quota: ToolQuotaV1,
}

#[derive(Clone, Debug)]
pub struct RoomDraft {
    pub room_id: Option<RoomId>,
    pub revision: Option<Revision>,
    pub config_hash: Hash256,
    pub policy_hash: Hash256,
    pub qualification_keys: Vec<String>,
    pub limits_hash: Hash256,
    pub confirmable: bool,
    /// Secret-name exclusion is not confirmation, even when `confirmable` is set.
    pub exclusion_only: bool,
    pub excluded_paths: Vec<String>,
    pub secret_names: Vec<String>,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRecipients {
    pub recipients: Vec<ResolvedRecipientV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmationEcho {
    pub excluded_paths: Vec<String>,
    pub secret_names: Vec<String>,
    pub recipients: Vec<ResolvedRecipientV1>,
    pub confirmable: bool,
}

struct StoredPreflight {
    record: PreflightRecordV1,
    manifest: SourceManifestV1,
}

#[derive(Default)]
pub struct PreflightLog {
    inner: std::sync::Mutex<std::collections::BTreeMap<String, StoredPreflight>>,
}

impl PreflightLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn confirm(&self, record: PreflightRecordV1, manifest: SourceManifestV1) -> RtResult<()> {
        if !record.confirmable {
            return Err(rt_error(ErrorCode::InvalidState, "not_confirmable"));
        }
        if record.source_manifest_hash != manifest.manifest_hash
            || record.source_manifest_id != manifest.manifest_id
        {
            return Err(rt_error(ErrorCode::InvalidArgument, "manifest_mismatch"));
        }
        self.inner
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(
                record.preflight_id.clone(),
                StoredPreflight { record, manifest },
            );
        Ok(())
    }

    /// Returns the confirmed manifest. A later source edit is not recaptured.
    /// A provider-mapping change requires confirmation again.
    pub fn start_confirmed_manifest(
        &self,
        preflight_id: &str,
        recipients: &ResolvedRecipients,
    ) -> RtResult<SourceManifestV1> {
        let guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        let stored = guard
            .get(preflight_id)
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "preflight_missing"))?;
        if stored.record.recipients != recipients.recipients {
            return Err(RtError::from_reason(InternalReason::ReconfirmationRequired));
        }
        Ok(stored.manifest.clone())
    }
}

struct PlannedFile {
    canonical: String,
    class: SourceClass,
}

pub fn validate_relative_path(raw: &str) -> RtResult<String> {
    if raw.is_empty() {
        return Err(rt_error(ErrorCode::InvalidArgument, "empty_path"));
    }
    let bytes = raw.as_bytes();
    if raw.starts_with('/') || raw.starts_with('\\') || raw.starts_with("//") {
        return Err(rt_error(ErrorCode::InvalidArgument, "absolute_path"));
    }
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(rt_error(ErrorCode::InvalidArgument, "absolute_path"));
    }
    let mut parts = Vec::new();
    for part in raw.split(|ch| ch == '/' || ch == '\\') {
        if part.is_empty() || part == "." {
            return Err(rt_error(ErrorCode::InvalidArgument, "invalid_path"));
        }
        if part == ".." {
            return Err(rt_error(ErrorCode::InvalidArgument, "parent_escape"));
        }
        if part.bytes().any(|byte| byte == 0) {
            return Err(rt_error(ErrorCode::InvalidArgument, "invalid_path"));
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(rt_error(ErrorCode::InvalidArgument, "empty_path"));
    }
    Ok(parts.join("/"))
}

pub fn canonical_path_bytes(raw: &str) -> RtResult<Vec<u8>> {
    Ok(validate_relative_path(raw)?.into_bytes())
}

pub fn path_within_root(root: &Path, candidate: &Path) -> bool {
    let root_parts = normalized_components(root);
    let candidate_parts = normalized_components(candidate);
    !root_parts.is_empty()
        && candidate_parts.len() > root_parts.len()
        && candidate_parts.starts_with(&root_parts)
}

pub fn ensure_within_root(root: &Path, candidate: &Path) -> RtResult<()> {
    if path_within_root(root, candidate) {
        Ok(())
    } else {
        Err(rt_error(ErrorCode::InvalidArgument, "cross_root"))
    }
}

/// Case-sensitive path check. `/` and `\` are both separators on every host
/// so Windows string fixtures do not depend on the process path parser.
pub fn lexical_within(root: &str, candidate: &str) -> bool {
    let root_parts = lexical_parts(root);
    let candidate_parts = lexical_parts(candidate);
    !root_parts.is_empty()
        && candidate_parts.len() > root_parts.len()
        && candidate_parts.starts_with(&root_parts)
}

fn lexical_parts(raw: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for part in raw.split(|ch| ch == '/' || ch == '\\') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if parts.last().is_some_and(|existing| existing != "..") {
                parts.pop();
            } else {
                parts.push("..".into());
            }
            continue;
        }
        parts.push(part.to_string());
    }
    parts
}

/// `None` means the bytes are not UTF-8 and must not be offered to a text tool.
/// Offsets are byte indexes of line starts. CRLF is one break. A missing final
/// newline does not invent an extra line, and a present final newline does not
/// either.
pub fn line_start_offsets(bytes: &[u8]) -> Option<Vec<u64>> {
    if std::str::from_utf8(bytes).is_err() {
        return None;
    }
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    let mut offsets = vec![0u64];
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            let next = index + 1;
            if next < bytes.len() {
                offsets.push(next as u64);
            }
            index = next;
        } else if bytes[index] == b'\r' {
            let next = if index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
                index + 2
            } else {
                index + 1
            };
            if next < bytes.len() {
                offsets.push(next as u64);
            }
            index = next;
        } else {
            index += 1;
        }
    }
    Some(offsets)
}

pub fn offer_text_tool<'a>(entry: &SourceEntryV1, bytes: &'a [u8]) -> RtResult<&'a str> {
    if !entry.text_admissible || entry.encoding != SnapshotEncoding::Utf8 {
        return Err(rt_error(ErrorCode::InvalidArgument, "binary_source"));
    }
    std::str::from_utf8(bytes).map_err(|_| rt_error(ErrorCode::InvalidArgument, "binary_source"))
}

pub async fn capture_snapshot(
    selection: SourceSelection,
    limits: SnapshotLimits,
    objects: &ObjectStore,
) -> RtResult<SourceManifestV1> {
    let planned = plan_selection(&selection, &limits)?;
    let lease = objects.reserve_estimated(limits.estimated_bytes)?;
    let mut created = Vec::new();
    match read_and_store(&selection, &limits, objects, &planned, &mut created).await {
        Ok(manifest) => {
            lease.commit_as_used(manifest.accounted_bytes());
            Ok(manifest)
        }
        Err(err) => {
            let removed = objects.discard_ids(&created) && objects.orphans_cleared();
            if removed {
                let _ = lease.release_after_partial_removed(true);
            }
            Err(err)
        }
    }
}

pub async fn commit_captured_manifest(
    store: &RoundtableStore,
    objects: &ObjectStore,
    manifest: &SourceManifestV1,
) -> RtResult<()> {
    if objects.manifest_commit_fault() {
        let removed = objects.discard_ids(&manifest.object_ids()) && objects.orphans_cleared();
        objects.release_accounted(manifest.accounted_bytes(), removed)?;
        return Err(rt_error(ErrorCode::StorageUnavailable, "manifest_commit"));
    }
    let body_json = match serde_json::to_string(manifest) {
        Ok(body) => body,
        Err(_) => {
            let removed = objects.discard_ids(&manifest.object_ids()) && objects.orphans_cleared();
            let _ = objects.release_accounted(manifest.accounted_bytes(), removed);
            return Err(rt_error(ErrorCode::InvalidArgument, "manifest_body"));
        }
    };
    let row = NewManifest {
        room_id: manifest.room_id.to_string(),
        manifest_id: manifest.manifest_id.to_string(),
        version: manifest.version,
        manifest_hash: manifest.manifest_hash.to_hex(),
        body_json,
    };
    if let Err(err) = store.insert_manifest(&row).await {
        let removed = objects.discard_ids(&manifest.object_ids()) && objects.orphans_cleared();
        if removed {
            let _ = objects.release_accounted(manifest.accounted_bytes(), true);
        }
        return Err(err);
    }
    objects.mark_committed(&manifest.object_ids());
    Ok(())
}

/// Copies the phase fields that already exist on `PhaseSnapshotV1`.
/// Deadline, resource waits, and C runtime counters are not snapshot fields.
pub fn freeze_phase(input: PhaseInput) -> RtResult<PhaseSnapshotV1> {
    Ok(PhaseSnapshotV1 {
        schema_version: SCHEMA_VERSION,
        phase_id: input.phase_id,
        phase_index: input.phase_index,
        revision: input.revision,
        kind: input.kind,
        critique_round: input.critique_round,
        config_version: input.config_version,
        question_version: input.question_version,
        interjection_version: input.interjection_version,
        source_manifest_id: input.source_manifest_id,
        source_manifest_hash: input.source_manifest_hash,
        published_messages: input.published_messages,
        members: input.members,
        mandatory_targets: input.mandatory_targets,
        policy_hash: input.policy_hash,
        output_byte_limit: input.output_byte_limit,
        tool_quota: input.tool_quota,
    })
}

pub fn build_delivery(
    snapshot: &PhaseSnapshotV1,
    role: &RoleSnapshot,
    binding: &BindingId,
    capacity: &dyn TokenBound,
    profile: &QualifiedContextProfile,
) -> RtResult<DeliveryManifestV1> {
    DeliveryEncoder::encode(snapshot, role, binding, capacity, profile)
}

pub fn fresh_binding_context(prompt_bytes: SafeInt) -> ContextStateV1 {
    ContextStateV1 {
        freshness: ContextFreshness::Fresh,
        delivered_prompt_bytes: prompt_bytes,
        tool_return_bytes: SafeInt(0),
        cli_hidden_context_limit: None,
        compression_signal: false,
    }
}

pub fn freeze_preflight(
    actor: &ActorContext,
    draft: &RoomDraft,
    manifest: &SourceManifestV1,
    recipients: &ResolvedRecipients,
) -> RtResult<PreflightRecordV1> {
    Ok(PreflightRecordV1 {
        preflight_id: Uuid::new_v4().as_hyphenated().to_string(),
        principal_id: actor.principal_id(),
        room_id: draft.room_id,
        revision: draft.revision,
        config_hash: draft.config_hash,
        source_manifest_id: manifest.manifest_id,
        source_manifest_hash: manifest.manifest_hash,
        recipients: recipients.recipients.clone(),
        policy_hash: draft.policy_hash,
        qualification_keys: draft.qualification_keys.clone(),
        limits_hash: draft.limits_hash,
        created_at: draft.created_at.clone(),
        expires_at: draft.expires_at.clone(),
        confirmable: draft.confirmable && !draft.exclusion_only,
    })
}

pub fn confirmation_echo(draft: &RoomDraft, recipients: &ResolvedRecipients) -> ConfirmationEcho {
    ConfirmationEcho {
        excluded_paths: draft.excluded_paths.clone(),
        secret_names: draft.secret_names.clone(),
        recipients: recipients.recipients.clone(),
        confirmable: draft.confirmable && !draft.exclusion_only,
    }
}

fn plan_selection(selection: &SourceSelection, limits: &SnapshotLimits) -> RtResult<Vec<PlannedFile>> {
    let mut planned = Vec::with_capacity(selection.files.len());
    for file in &selection.files {
        planned.push(PlannedFile {
            canonical: validate_relative_path(&file.relative_path)?,
            class: file.class,
        });
    }
    planned.sort_by(|left, right| left.canonical.as_bytes().cmp(right.canonical.as_bytes()));
    if planned.len() > usize::try_from(limits.max_files).unwrap_or(usize::MAX) {
        return Err(rt_error(ErrorCode::InvalidArgument, "source_limit"));
    }
    for pair in planned.windows(2) {
        if pair[0].canonical == pair[1].canonical {
            return Err(rt_error(ErrorCode::InvalidArgument, "duplicate_path"));
        }
    }
    Ok(planned)
}

async fn read_and_store(
    selection: &SourceSelection,
    limits: &SnapshotLimits,
    objects: &ObjectStore,
    planned: &[PlannedFile],
    created: &mut Vec<String>,
) -> RtResult<SourceManifestV1> {
    let captured_at_ms = now_ms();
    let mut copied = Vec::with_capacity(planned.len());
    let mut total = 0u64;
    for file in planned {
        let path = open_source(&selection.root, &file.canonical)?;
        let meta = fs::symlink_metadata(&path)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "source_stat"))?;
        ensure_regular_meta(&path, &meta)?;
        let mode = file_mode(&meta);
        let (bytes, _) = read_stable(&path, selection.mutate_while_open.as_ref())?;
        let size = u64::try_from(bytes.len())
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
        if size > limits.max_file_bytes {
            return Err(rt_error(ErrorCode::InvalidArgument, "source_limit"));
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
        copied.push((file, mode, bytes, size));
    }
    if total > limits.estimated_bytes {
        return Err(rt_error(ErrorCode::InvalidArgument, "estimate_too_small"));
    }
    if total > limits.max_total_bytes {
        return Err(rt_error(ErrorCode::InvalidArgument, "source_limit"));
    }
    let mut entries = Vec::with_capacity(copied.len());
    for (file, mode, bytes, size) in &copied {
        let object = match objects.put(bytes).await {
            Ok(object) => object,
            Err(err) => return Err(err),
        };
        created.push(object.object_id.clone());
        let (encoding, line_offsets, text_admissible) = match line_start_offsets(bytes) {
            Some(offsets) => (SnapshotEncoding::Utf8, offsets, true),
            None => (SnapshotEncoding::Binary, Vec::new(), false),
        };
        entries.push(SourceEntryV1 {
            path: file.canonical.clone(),
            class: file.class,
            encoding,
            size: *size,
            mode: *mode,
            captured_at_ms,
            content_hash: object.content_hash,
            line_offsets,
            text_admissible,
            object,
        });
    }
    let version = u64::try_from(selection.version)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "version"))?;
    let manifest_hash = canonical_hash(&ManifestIdentity {
        entries: entries.iter().map(EntryIdentity::from).collect(),
        read_limit: MAX_SNAPSHOT_READS,
        room_id: selection.room_id.to_string(),
        version,
    })?;
    Ok(SourceManifestV1 {
        schema_version: SCHEMA_VERSION,
        manifest_id: fresh_manifest_id()?,
        room_id: selection.room_id,
        version: selection.version,
        base_commit: selection.base_commit.clone(),
        read_limit: MAX_SNAPSHOT_READS,
        entries,
        manifest_hash,
    })
}

#[derive(Serialize)]
struct ManifestIdentity {
    entries: Vec<EntryIdentity>,
    read_limit: u32,
    room_id: String,
    version: u64,
}

#[derive(Serialize)]
struct EntryIdentity {
    class: SourceClass,
    content_hash: Hash256,
    encoding: SnapshotEncoding,
    line_offsets: Vec<u64>,
    mode: u32,
    path: String,
    size: u64,
    text_admissible: bool,
}

impl From<&SourceEntryV1> for EntryIdentity {
    fn from(entry: &SourceEntryV1) -> Self {
        Self {
            class: entry.class,
            content_hash: entry.content_hash,
            encoding: entry.encoding,
            line_offsets: entry.line_offsets.clone(),
            mode: entry.mode,
            path: entry.path.clone(),
            size: entry.size,
            text_admissible: entry.text_admissible,
        }
    }
}

fn fresh_manifest_id() -> RtResult<ManifestId> {
    let text = Uuid::new_v4().as_hyphenated().to_string();
    text.parse()
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "manifest_id"))
}

fn open_source(root: &Path, canonical: &str) -> RtResult<PathBuf> {
    let root_meta = fs::symlink_metadata(root)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "source_root"))?;
    if root_meta.file_type().is_symlink() {
        return Err(rt_error(ErrorCode::InvalidArgument, "symlink"));
    }
    let mut cursor = root.to_path_buf();
    let parts: Vec<&str> = canonical.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        cursor.push(part);
        let meta = fs::symlink_metadata(&cursor)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "source_missing"))?;
        if meta.file_type().is_symlink() {
            return Err(rt_error(ErrorCode::InvalidArgument, "symlink"));
        }
        let last = index + 1 == parts.len();
        if !last && !meta.is_dir() {
            return Err(rt_error(ErrorCode::InvalidArgument, "not_regular"));
        }
        if last {
            ensure_regular_meta(&cursor, &meta)?;
        }
    }
    ensure_within_root(root, &cursor)?;
    Ok(cursor)
}

fn ensure_regular_meta(path: &Path, meta: &fs::Metadata) -> RtResult<()> {
    let kind = meta.file_type();
    if kind.is_symlink() {
        return Err(rt_error(ErrorCode::InvalidArgument, "symlink"));
    }
    if !kind.is_file() {
        return Err(rt_error(ErrorCode::InvalidArgument, "not_regular"));
    }
    if link_count(path)? > 1 {
        return Err(rt_error(ErrorCode::InvalidArgument, "hard_link"));
    }
    Ok(())
}

fn read_stable(path: &Path, hook: Option<&SnapshotReadHook>) -> RtResult<(Vec<u8>, fs::Metadata)> {
    let mut file = open_read(path)?;
    for _ in 0..MAX_SNAPSHOT_READS {
        let before = file
            .metadata()
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "source_stat"))?;
        ensure_regular_meta(path, &before)?;
        if let Some(hook) = hook {
            (hook.as_ref())(path);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "source_read"))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "source_read"))?;
        let after = file
            .metadata()
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "source_stat"))?;
        let read_len = u64::try_from(bytes.len())
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
        let path_len = fs::symlink_metadata(path).ok().map(|meta| meta.len());
        let path_ok = path_len
            .map(|len| len == after.len() && len == read_len)
            .unwrap_or(true);
        if sample_stable(&before, &after, read_len) && path_ok {
            return Ok((bytes, after));
        }
    }
    Err(RtError::from_reason(InternalReason::SnapshotUnstable))
}

fn sample_stable(before: &fs::Metadata, after: &fs::Metadata, read_len: u64) -> bool {
    let len_ok = before.len() == after.len() && read_len == after.len();
    let mtime_ok = match (before.modified(), after.modified()) {
        (Ok(left), Ok(right)) => left == right,
        _ => true,
    };
    len_ok && mtime_ok
}

fn open_read(path: &Path) -> RtResult<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
        .open(path)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "source_open"))
}

fn link_count(path: &Path) -> RtResult<u64> {
    #[cfg(windows)]
    {
        use std::mem::MaybeUninit;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };

        let file = open_read(path)?;
        let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
        // SAFETY: `file` owns a valid handle for this call, and `info` is writable.
        let result = unsafe {
            GetFileInformationByHandle(file.as_raw_handle() as HANDLE, info.as_mut_ptr())
        };
        if result == 0 {
            return Err(rt_error(ErrorCode::StorageUnavailable, "source_stat"));
        }
        // SAFETY: a successful call initialized the complete structure.
        let info = unsafe { info.assume_init() };
        Ok(u64::from(info.nNumberOfLinks))
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::symlink_metadata(path)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "source_stat"))?;
        Ok(meta.nlink())
    }
}

fn file_mode(meta: &fs::Metadata) -> u32 {
    #[cfg(windows)]
    {
        if meta.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    }
}

fn normalized_components(path: &Path) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.last().is_some_and(|part| !is_structural(part)) {
                    parts.pop();
                } else {
                    parts.push("..".to_string());
                }
            }
            other => parts.push(other.as_os_str().to_string_lossy().into_owned()),
        }
    }
    parts
}

fn is_structural(part: &str) -> bool {
    part == "\\"
        || part == "/"
        || part.ends_with(':')
        || part.starts_with('\\')
        || part == ".."
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}
