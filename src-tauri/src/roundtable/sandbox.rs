//! Sandbox plan and the pre-spawn launch journal.
//!
//! The plan is inspectable on every host. Windows spawn returns
//! `policy_unenforceable`. The isolator returns a process proof only; mailbox
//! and tool drain belong to the runtime layer. If owned-process enumeration
//! cannot be proven, discovery and reap stay blocked and do not release.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use roundtable_protocol::{Epoch, ErrorCode, Hash256, IncarnationId, ProcessTreeProof, RtResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::qualification::{CertifiedBinary, QualificationKey};
use super::rt_error;
use crate::roundtable::feature_gate::fsync_dir;

mod linux_oci;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DbIdentity(String);

impl DbIdentity {
    pub fn new(canonical: impl Into<String>) -> RtResult<Self> {
        let canonical = canonical.into();
        if canonical.is_empty() || canonical.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(rt_error(ErrorCode::InvalidArgument, "db_identity"));
        }
        Ok(Self(canonical))
    }

    pub fn canonical(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct SandboxInput {
    pub certificate: QualificationKey,
    pub db: DbIdentity,
    pub boot_epoch: Epoch,
    pub incarnation: IncarnationId,
    pub scratch: PathBuf,
    pub project: PathBuf,
    pub home: PathBuf,
    pub other_scratches: Vec<PathBuf>,
    pub decoy_paths: Vec<PathBuf>,
    pub inherited_env: BTreeMap<String, String>,
    pub env_allowlist: BTreeMap<String, String>,
    pub global_mcp: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceSet {
    pub user: bool,
    pub mount: bool,
    pub pid: bool,
    pub network: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CgroupLimits {
    pub memory_max_bytes: u64,
    pub pids_max: u64,
    pub cpu_quota_us: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanMount {
    pub source: PathBuf,
    pub destination: String,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
pub struct SandboxPlan {
    pub runtime_path: PathBuf,
    pub runtime_sha256: Hash256,
    pub argv: Vec<String>,
    pub namespaces: NamespaceSet,
    pub image_digest: String,
    pub image_readonly: bool,
    pub drop_all_caps: bool,
    pub no_new_privileges: bool,
    pub seccomp_default: String,
    pub cgroup: CgroupLimits,
    pub mounts: Vec<PlanMount>,
    pub host_network: bool,
    pub docker_socket_mounted: bool,
    pub home_mounted: bool,
    pub project_mounted: bool,
    pub host_proc_mounted: bool,
    pub inherited_fds: bool,
    pub devices_allowed: bool,
    pub host_sockets_mounted: bool,
    pub symlinks_followed: bool,
    pub global_mcp: bool,
    pub env_cleared_before_allowlist: bool,
    pub env: BTreeMap<String, String>,
    pub network_destinations: Vec<String>,
    pub owner_label: String,
    pub db: DbIdentity,
    pub boot_epoch: Epoch,
    pub incarnation: IncarnationId,
    pub plan_hash: Hash256,
    pub allowed_binaries: Vec<CertifiedBinary>,
    pub oci: Value,
}

pub fn build_sandbox_plan(input: &SandboxInput) -> RtResult<SandboxPlan> {
    linux_oci::build_plan(input)
}

pub(super) fn owner_label(db: &DbIdentity, boot: Epoch, incarnation: &IncarnationId) -> String {
    format!(
        "db={};boot={};incarnation={}",
        db.canonical(),
        boot.0,
        incarnation
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxInstance {
    pub runtime_id: String,
    pub incarnation: IncarnationId,
    pub boot_epoch: Epoch,
    pub image_digest: String,
    pub cgroup_handle: String,
    pub owner_label: String,
}

impl SandboxInstance {
    fn from_intent(intent: &LaunchIntent) -> Self {
        Self {
            runtime_id: format!("oci:{}", intent.owner_label),
            incarnation: intent.incarnation,
            boot_epoch: intent.boot_epoch,
            image_digest: intent.image_digest.clone(),
            cgroup_handle: format!("codeg:{}", intent.db.canonical()),
            owner_label: intent.owner_label.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchIntent {
    pub db: DbIdentity,
    pub boot_epoch: Epoch,
    pub incarnation: IncarnationId,
    pub owner_label: String,
    pub image_digest: String,
    pub plan_hash: Hash256,
    pub spawned: Option<SandboxInstance>,
    pub reaped: bool,
}

impl LaunchIntent {
    pub fn from_plan(plan: &SandboxPlan) -> Self {
        Self {
            db: plan.db.clone(),
            boot_epoch: plan.boot_epoch,
            incarnation: plan.incarnation,
            owner_label: plan.owner_label.clone(),
            image_digest: plan.image_digest.clone(),
            plan_hash: plan.plan_hash,
            spawned: None,
            reaped: false,
        }
    }
}

pub trait LaunchIntentStore {
    fn record(&self, intent: &LaunchIntent) -> RtResult<()>;
    fn mark_spawned(&self, incarnation: IncarnationId, instance: &SandboxInstance) -> RtResult<()>;
    fn list_unreaped(&self) -> RtResult<Vec<LaunchIntent>>;
    fn mark_reaped(&self, incarnation: IncarnationId) -> RtResult<()>;
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum JournalOp {
    Record {
        v: u8,
        intent: LaunchIntent,
    },
    Spawned {
        v: u8,
        incarnation: IncarnationId,
        instance: SandboxInstance,
    },
    Reaped {
        v: u8,
        incarnation: IncarnationId,
    },
}

struct JournalInner {
    file: File,
    intents: Vec<LaunchIntent>,
}

pub struct JournalLaunchIntentStore {
    dir: PathBuf,
    _lock: File,
    inner: Mutex<JournalInner>,
}

impl JournalLaunchIntentStore {
    pub fn open(dir: &Path) -> RtResult<Self> {
        fs::create_dir_all(dir)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_io"))?;
        let lock = lock_exclusive(dir)?;
        let path = dir.join("journal.jsonl");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_io"))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_io"))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_io"))?;
        file.seek(SeekFrom::End(0))
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_io"))?;
        let intents = replay(&bytes)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            _lock: lock,
            inner: Mutex::new(JournalInner { file, intents }),
        })
    }
}

impl LaunchIntentStore for JournalLaunchIntentStore {
    fn record(&self, intent: &LaunchIntent) -> RtResult<()> {
        let mut inner = self.inner();
        if let Some(existing) = inner
            .intents
            .iter()
            .find(|item| item.incarnation == intent.incarnation)
        {
            if same_identity(existing, intent) {
                return Ok(());
            }
            return Err(rt_error(ErrorCode::InvalidArgument, "journal_corrupt"));
        }
        append_op(
            &mut inner.file,
            &self.dir,
            &JournalOp::Record {
                v: 1,
                intent: intent.clone(),
            },
        )?;
        inner.intents.push(intent.clone());
        Ok(())
    }

    fn mark_spawned(&self, incarnation: IncarnationId, instance: &SandboxInstance) -> RtResult<()> {
        let mut inner = self.inner();
        let index = inner
            .intents
            .iter()
            .position(|item| item.incarnation == incarnation)
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "journal_corrupt"))?;
        if instance.incarnation != incarnation {
            return Err(rt_error(ErrorCode::InvalidArgument, "journal_corrupt"));
        }
        if let Some(existing) = &inner.intents[index].spawned {
            if existing != instance {
                return Err(rt_error(ErrorCode::InvalidArgument, "journal_corrupt"));
            }
            return Ok(());
        }
        append_op(
            &mut inner.file,
            &self.dir,
            &JournalOp::Spawned {
                v: 1,
                incarnation,
                instance: instance.clone(),
            },
        )?;
        inner.intents[index].spawned = Some(instance.clone());
        Ok(())
    }

    fn list_unreaped(&self) -> RtResult<Vec<LaunchIntent>> {
        Ok(self
            .inner()
            .intents
            .iter()
            .filter(|intent| !intent.reaped)
            .cloned()
            .collect())
    }

    fn mark_reaped(&self, incarnation: IncarnationId) -> RtResult<()> {
        let mut inner = self.inner();
        let index = inner
            .intents
            .iter()
            .position(|item| item.incarnation == incarnation)
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "journal_corrupt"))?;
        if inner.intents[index].reaped {
            return Ok(());
        }
        append_op(
            &mut inner.file,
            &self.dir,
            &JournalOp::Reaped { v: 1, incarnation },
        )?;
        inner.intents[index].reaped = true;
        Ok(())
    }
}

impl JournalLaunchIntentStore {
    fn inner(&self) -> std::sync::MutexGuard<'_, JournalInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn same_identity(left: &LaunchIntent, right: &LaunchIntent) -> bool {
    left.db == right.db
        && left.boot_epoch == right.boot_epoch
        && left.incarnation == right.incarnation
        && left.owner_label == right.owner_label
        && left.image_digest == right.image_digest
        && left.plan_hash == right.plan_hash
}

fn replay(bytes: &[u8]) -> RtResult<Vec<LaunchIntent>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.ends_with(b"\n") {
        return Err(rt_error(ErrorCode::InvalidArgument, "journal_truncated"));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "journal_corrupt"))?;
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let mut intents = Vec::new();
    for line in lines {
        if line.is_empty() {
            return Err(rt_error(ErrorCode::InvalidArgument, "journal_corrupt"));
        }
        let op: JournalOp = serde_json::from_str(line)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "journal_corrupt"))?;
        apply(&mut intents, op)?;
    }
    Ok(intents)
}

fn apply(intents: &mut Vec<LaunchIntent>, op: JournalOp) -> RtResult<()> {
    match op {
        JournalOp::Record { v, intent } => {
            if v != 1 {
                return Err(rt_error(ErrorCode::InvalidArgument, "journal_corrupt"));
            }
            if let Some(existing) = intents
                .iter()
                .find(|item| item.incarnation == intent.incarnation)
            {
                if same_identity(existing, &intent) {
                    return Ok(());
                }
                return Err(rt_error(ErrorCode::InvalidArgument, "journal_corrupt"));
            }
            intents.push(intent);
            Ok(())
        }
        JournalOp::Spawned {
            v,
            incarnation,
            instance,
        } => {
            if v != 1 || instance.incarnation != incarnation {
                return Err(rt_error(ErrorCode::InvalidArgument, "journal_corrupt"));
            }
            let slot = intents
                .iter_mut()
                .find(|item| item.incarnation == incarnation)
                .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "journal_corrupt"))?;
            if let Some(existing) = &slot.spawned {
                if existing != &instance {
                    return Err(rt_error(ErrorCode::InvalidArgument, "journal_corrupt"));
                }
            } else {
                slot.spawned = Some(instance);
            }
            Ok(())
        }
        JournalOp::Reaped { v, incarnation } => {
            if v != 1 {
                return Err(rt_error(ErrorCode::InvalidArgument, "journal_corrupt"));
            }
            let slot = intents
                .iter_mut()
                .find(|item| item.incarnation == incarnation)
                .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "journal_corrupt"))?;
            slot.reaped = true;
            Ok(())
        }
    }
}

fn append_op(file: &mut File, dir: &Path, op: &JournalOp) -> RtResult<()> {
    let mut line = serde_json::to_string(op)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "journal_corrupt"))?;
    line.push('\n');
    file.write_all(line.as_bytes())
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_io"))?;
    file.sync_all()
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_io"))?;
    fsync_dir(dir).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_io"))?;
    Ok(())
}

fn lock_exclusive(dir: &Path) -> RtResult<File> {
    let path = dir.join("LOCK");
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        return OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .share_mode(0)
            .open(&path)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_locked"));
    }
    #[cfg(not(windows))]
    {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_io"))?;
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(rt_error(ErrorCode::StorageUnavailable, "journal_locked"));
        }
        Ok(file)
    }
}

#[derive(Clone, Debug)]
pub struct PreparedSandbox {
    pub plan: SandboxPlan,
}

#[async_trait]
pub trait IsolationProvider {
    async fn prepare(&self, plan: &SandboxPlan) -> RtResult<PreparedSandbox>;
    async fn spawn(
        &self,
        prepared: &PreparedSandbox,
        intent: &LaunchIntent,
    ) -> RtResult<SandboxInstance>;
    async fn discover_owned(&self, db: &DbIdentity) -> RtResult<Vec<SandboxInstance>>;
    async fn reap(&self, instance: &SandboxInstance) -> RtResult<ProcessTreeProof>;
}

pub struct LinuxOciIsolator {
    intents: JournalLaunchIntentStore,
}

impl LinuxOciIsolator {
    pub fn new(intents: JournalLaunchIntentStore) -> Self {
        Self { intents }
    }

    pub fn list_unreaped(&self) -> RtResult<Vec<LaunchIntent>> {
        self.intents.list_unreaped()
    }

    /// Instances named by unreaped intents, including ones whose spawn ack was lost.
    /// This does not prove the OS process is still the same incarnation.
    pub fn recorded_instances(&self, db: &DbIdentity) -> RtResult<Vec<SandboxInstance>> {
        Ok(self
            .intents
            .list_unreaped()?
            .into_iter()
            .filter(|intent| intent.db.canonical() == db.canonical())
            .map(|intent| {
                intent
                    .spawned
                    .clone()
                    .unwrap_or_else(|| SandboxInstance::from_intent(&intent))
            })
            .collect())
    }

    /// The isolator does not observe mailbox drain.
    pub fn mailbox_drained(&self) -> Option<bool> {
        None
    }

    /// The isolator does not observe tool drain.
    pub fn tools_drained(&self) -> Option<bool> {
        None
    }
}

#[async_trait]
impl IsolationProvider for LinuxOciIsolator {
    async fn prepare(&self, plan: &SandboxPlan) -> RtResult<PreparedSandbox> {
        if !plan.runtime_path.is_absolute()
            || !plan.namespaces.user
            || !plan.namespaces.mount
            || !plan.namespaces.pid
            || !plan.namespaces.network
            || !plan.image_readonly
            || plan.host_network
            || plan.docker_socket_mounted
            || plan.home_mounted
            || plan.project_mounted
            || plan.host_proc_mounted
            || plan.inherited_fds
            || plan.devices_allowed
            || plan.global_mcp
            || !plan.env_cleared_before_allowlist
            || plan.seccomp_default.is_empty()
            || plan.cgroup.memory_max_bytes == 0
            || plan.cgroup.pids_max == 0
        {
            return Err(rt_error(ErrorCode::InvalidArgument, "plan_incomplete"));
        }
        Ok(PreparedSandbox { plan: plan.clone() })
    }

    async fn spawn(
        &self,
        prepared: &PreparedSandbox,
        intent: &LaunchIntent,
    ) -> RtResult<SandboxInstance> {
        // The intent is durable before any exec. A lost ack can still be found.
        self.intents.record(intent)?;
        let _ = prepared;
        Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "policy_unenforceable",
        ))
    }

    async fn discover_owned(&self, db: &DbIdentity) -> RtResult<Vec<SandboxInstance>> {
        // A readable journal is not a process listing. Empty Ok would release.
        let _ = self.recorded_instances(db)?;
        let _ = linux_oci::enumeration_proven();
        Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "enumeration_unproven",
        ))
    }

    async fn reap(&self, instance: &SandboxInstance) -> RtResult<ProcessTreeProof> {
        // No proven process tree, so this must not mark the intent reaped.
        let _ = (instance, linux_oci::enumeration_proven());
        Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "enumeration_unproven",
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscapeReport {
    pub denied_decoy_absolute_path: bool,
    pub denied_home: bool,
    pub denied_project: bool,
    pub denied_other_scratch: bool,
    pub denied_proc: bool,
    pub denied_inherited_fd: bool,
    pub denied_symlink: bool,
    pub denied_device: bool,
    pub denied_socket: bool,
    pub denied_global_mcp: bool,
    pub denied_arbitrary_network: bool,
}

/// Live escape attempts. A host that cannot run the pinned crun probe gets an
/// error, never a synthetic denial that would look like a pass.
pub async fn attempt_live_escapes(plan: &SandboxPlan) -> RtResult<EscapeReport> {
    let _ = plan;
    // Enumeration is not proven here, so this cannot report a denial as a pass.
    Err(rt_error(
        ErrorCode::PolicyUnenforceable,
        "live_probe_not_executed",
    ))
}
