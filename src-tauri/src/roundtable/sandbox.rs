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
use std::sync::{Arc, Mutex};

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

/// Installed execution paths. This is configuration, never a qualification pass.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualifiedOciProfile {
    pub runtime: CertifiedBinary,
    pub rootfs: PathBuf,
    pub rootfs_sha256: Hash256,
    pub runtime_root: PathBuf,
    pub cgroup_root: PathBuf,
    pub cli_args: Vec<String>,
    #[serde(default)]
    pub service_socket: Option<PathBuf>,
    #[serde(default)]
    pub gateway_socket: Option<PathBuf>,
    /// Read-only bind mounts of specific auth files. Never a home directory.
    #[serde(default)]
    pub auth_mounts: Vec<AuthMount>,
    /// Fixed in-container environment. Host `HOME` and `PATH` are not copied.
    #[serde(default)]
    pub container_env: BTreeMap<String, String>,
}

/// One credential file mounted read-only at a fixed container path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthMount {
    pub source: PathBuf,
    pub destination: String,
}

pub fn qualified_rootfs_digest(rootfs: &Path) -> RtResult<Hash256> {
    linux_oci::rootfs_digest(rootfs)
}

pub fn qualified_rootfs_digest_detail(rootfs: &Path) -> Result<Hash256, String> {
    linux_oci::rootfs_digest_detail(rootfs)
}

/// Stable execution template to bind to QualificationKey.plan_hash. Per-attempt
/// scratch paths, socket sources and incarnation labels are excluded.
pub fn qualified_oci_profile_hash(
    profile: &QualifiedOciProfile,
    certificate: &QualificationKey,
) -> RtResult<Hash256> {
    linux_oci::profile_hash(profile, certificate)
}

/// Read-only preflight: unresolved per-attempt socket sources are allowed, but
/// the certificate still binds the fixed dual-endpoint execution template.
pub fn verify_qualified_oci_profile(
    profile: &QualifiedOciProfile,
    certificate: &QualificationKey,
) -> RtResult<()> {
    linux_oci::verify_installed_profile(profile, certificate)
}

pub(crate) fn linux_oci_syscalls() -> &'static [&'static str] {
    linux_oci::SYSCALLS
}

pub(crate) fn linux_runtime_mounts_json() -> Vec<serde_json::Value> {
    linux_oci::runtime_mount_json()
}

pub(crate) fn linux_slirp_hook_script() -> &'static str {
    linux_oci::SLIRP_HOOK_SCRIPT
}

pub(crate) fn linux_slirp_hook_phase() -> &'static str {
    linux_oci::SLIRP_HOOK_PHASE
}

pub(crate) fn linux_slirp_binary() -> Option<PathBuf> {
    linux_oci::slirp_binary()
}

pub(crate) fn linux_stop_slirp(runtime_root: &Path, id: &str) {
    linux_oci::stop_slirp(runtime_root, id)
}

pub(crate) fn linux_cgroup_delegation_error(path: &Path) -> Option<String> {
    linux_oci::cgroup_delegation_error(path)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn syscall_allowlist() -> &'static [&'static str] {
    linux_oci::SYSCALLS
}

#[cfg(any(test, feature = "test-utils"))]
pub fn slirp_hook_script() -> &'static str {
    linux_oci::SLIRP_HOOK_SCRIPT
}

#[cfg(any(test, feature = "test-utils"))]
pub fn slirp_hook_phase() -> &'static str {
    linux_oci::SLIRP_HOOK_PHASE
}

#[cfg(any(test, feature = "test-utils"))]
pub fn cgroup_delegation_failure(path: &Path) -> Option<String> {
    linux_oci::cgroup_delegation_error(path)
}

pub fn build_qualified_sandbox_plan(
    input: &SandboxInput,
    profile: &QualifiedOciProfile,
) -> RtResult<SandboxPlan> {
    linux_oci::build_qualified_plan(input, profile)
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
    fn known_intent(&self, incarnation: IncarnationId) -> Option<LaunchIntent> {
        self.inner()
            .intents
            .iter()
            .find(|intent| intent.incarnation == incarnation)
            .cloned()
    }

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
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "journal_locked"))
    }
    #[cfg(not(windows))]
    {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
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
    intents: Arc<JournalLaunchIntentStore>,
    profile: Option<QualifiedOciProfile>,
}

impl LinuxOciIsolator {
    pub fn new(intents: JournalLaunchIntentStore) -> Self {
        Self {
            intents: Arc::new(intents),
            profile: None,
        }
    }

    pub fn with_profile(intents: JournalLaunchIntentStore, profile: QualifiedOciProfile) -> Self {
        Self {
            intents: Arc::new(intents),
            profile: Some(profile),
        }
    }

    /// Resolve one attempt's sockets while retaining the installed pins and
    /// the journal lock/state used by discovery and cleanup.
    /// Another adapter image that shares this launch journal.
    pub fn for_profile(&self, profile: QualifiedOciProfile) -> Self {
        Self {
            intents: Arc::clone(&self.intents),
            profile: Some(profile),
        }
    }

    pub fn with_attempt_sockets(
        &self,
        service_socket: PathBuf,
        gateway_socket: PathBuf,
    ) -> RtResult<Self> {
        let mut profile = self
            .profile
            .clone()
            .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "policy_unenforceable"))?;
        profile.service_socket = Some(service_socket);
        profile.gateway_socket = Some(gateway_socket);
        Ok(Self {
            intents: Arc::clone(&self.intents),
            profile: Some(profile),
        })
    }

    /// Launch crun with attached ACP stdio. The durable intent precedes exec;
    /// only OS enumeration and cgroup evidence may later retire it.
    pub async fn spawn_attached(
        &self,
        prepared: &PreparedSandbox,
        intent: &LaunchIntent,
    ) -> RtResult<(SandboxInstance, tokio::process::Child)> {
        if intent != &LaunchIntent::from_plan(&prepared.plan) {
            return Err(rt_error(
                ErrorCode::InvalidArgument,
                "launch_intent_mismatch",
            ));
        }
        self.intents.record(intent)?;
        let profile = self
            .profile
            .as_ref()
            .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "policy_unenforceable"))?;
        let prepared = self.prepare(&prepared.plan).await?;
        let (instance, mut child) = linux_oci::spawn_attached(&prepared.plan, profile).await?;
        if let Err(error) = self.intents.mark_spawned(intent.incarnation, &instance) {
            // Keep the intent live, even when acknowledgement cannot be durable.
            let _ = linux_oci::reap(&prepared.plan.runtime_path, profile, &instance).await;
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error);
        }
        Ok((instance, child))
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
            .map(|intent| self.instance_from_intent(&intent))
            .collect())
    }

    /// A retained journal record locates a known owner after cleanup retirement.
    /// It is not proof that its process tree is empty; `reap` still verifies it.
    pub fn recorded_instance(
        &self,
        db: &DbIdentity,
        incarnation: IncarnationId,
    ) -> RtResult<Option<SandboxInstance>> {
        Ok(self
            .intents
            .known_intent(incarnation)
            .filter(|intent| intent.db.canonical() == db.canonical())
            .map(|intent| self.instance_from_intent(&intent)))
    }

    fn instance_from_intent(&self, intent: &LaunchIntent) -> SandboxInstance {
        intent.spawned.clone().unwrap_or_else(|| {
            self.profile.as_ref().map_or_else(
                || SandboxInstance::from_intent(intent),
                |profile| linux_oci::recorded_intent_instance(profile, intent),
            )
        })
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
        linux_oci::validate_document(plan)?;
        if !plan.runtime_path.is_absolute()
            || !plan.namespaces.user
            || !plan.namespaces.mount
            || !plan.namespaces.pid
            || !plan.namespaces.network
            || !plan.image_readonly
            || !plan.drop_all_caps
            || !plan.no_new_privileges
            || plan.host_network
            || plan.docker_socket_mounted
            || plan.home_mounted
            || plan.project_mounted
            || plan.host_proc_mounted
            || plan.inherited_fds
            || plan.devices_allowed
            || plan.host_sockets_mounted
            || plan.symlinks_followed
            || plan.global_mcp
            || !plan.env_cleared_before_allowlist
            || plan.seccomp_default.is_empty()
            || plan.cgroup.memory_max_bytes == 0
            || plan.cgroup.pids_max == 0
        {
            return Err(rt_error(ErrorCode::InvalidArgument, "plan_incomplete"));
        }
        if let Some(profile) = &self.profile {
            let owned_plan = plan.clone();
            let owned_profile = profile.clone();
            tokio::task::spawn_blocking(move || {
                linux_oci::verify_profile(&owned_plan, &owned_profile)
            })
            .await
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "profile_validation"))??;
        }
        Ok(PreparedSandbox { plan: plan.clone() })
    }

    async fn spawn(
        &self,
        prepared: &PreparedSandbox,
        intent: &LaunchIntent,
    ) -> RtResult<SandboxInstance> {
        let (instance, child) = self.spawn_attached(prepared, intent).await?;
        // The detached recovery surface has no ACP consumer. Close its input,
        // drain both outputs, and retain ownership until crun has been waited.
        tokio::spawn(async move {
            let _ = child.wait_with_output().await;
        });
        Ok(instance)
    }

    async fn discover_owned(&self, db: &DbIdentity) -> RtResult<Vec<SandboxInstance>> {
        let Some(profile) = &self.profile else {
            return Err(rt_error(
                ErrorCode::PolicyUnenforceable,
                "enumeration_unproven",
            ));
        };
        let intents = self.intents.list_unreaped()?;
        linux_oci::discover_owned(profile, db, &intents).await
    }

    async fn reap(&self, instance: &SandboxInstance) -> RtResult<ProcessTreeProof> {
        let Some(profile) = &self.profile else {
            return Err(rt_error(
                ErrorCode::PolicyUnenforceable,
                "enumeration_unproven",
            ));
        };
        let intent = self
            .intents
            .known_intent(instance.incarnation)
            .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "instance_unowned"))?;
        if instance.owner_label != intent.owner_label
            || instance.boot_epoch != intent.boot_epoch
            || instance.image_digest != intent.image_digest
        {
            return Err(rt_error(ErrorCode::PolicyUnenforceable, "instance_unowned"));
        }
        linux_oci::verify_intent_instance(&intent, instance)?;
        let proof =
            linux_oci::reap(Path::new(&profile.runtime.absolute_path), profile, instance).await?;
        self.intents.mark_reaped(instance.incarnation)?;
        Ok(proof)
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

#[cfg(test)]
mod shared_journal_tests {
    use super::*;

    fn cleanup_fixture(root: &Path) -> (QualifiedOciProfile, LaunchIntent, SandboxInstance) {
        let profile = QualifiedOciProfile {
            runtime: CertifiedBinary {
                role: "crun".into(),
                absolute_path: root.join("missing-crun").to_string_lossy().into_owned(),
                version: "pinned".into(),
                sha256: Hash256::from_bytes([1; 32]),
            },
            rootfs: root.join("rootfs"),
            rootfs_sha256: Hash256::from_bytes([2; 32]),
            runtime_root: root.join("runtime"),
            cgroup_root: root.join("unqualified-cgroup"),
            cli_args: vec!["--acp".into()],
            service_socket: None,
            gateway_socket: None,
            auth_mounts: Vec::new(),
            container_env: BTreeMap::new(),
        };
        let db = DbIdentity::new("cleanup-retry-review").unwrap();
        let incarnation: IncarnationId = "00000000-0000-4000-8000-00000000000f".parse().unwrap();
        let boot_epoch = Epoch(7);
        let intent = LaunchIntent {
            db: db.clone(),
            boot_epoch,
            incarnation,
            owner_label: owner_label(&db, boot_epoch, &incarnation),
            image_digest: "sha256:cleanup-retry-image".into(),
            plan_hash: Hash256::from_bytes([3; 32]),
            spawned: None,
            reaped: false,
        };
        let runtime_id = format!(
            "codeg-rt-{}-7-{incarnation}",
            &Hash256::sha256(db.canonical().as_bytes()).to_hex()[..24]
        );
        let instance = SandboxInstance {
            cgroup_handle: profile
                .cgroup_root
                .join(&runtime_id)
                .to_string_lossy()
                .into_owned(),
            runtime_id,
            incarnation,
            boot_epoch,
            image_digest: intent.image_digest.clone(),
            owner_label: intent.owner_label.clone(),
        };
        (profile, intent, instance)
    }

    #[tokio::test]
    async fn retired_marker_does_not_replace_a_fresh_os_cleanup_proof() {
        let dir = tempfile::tempdir().unwrap();
        let journal_dir = dir.path().join("journal");
        let (profile, intent, instance) = cleanup_fixture(dir.path());
        let journal = JournalLaunchIntentStore::open(&journal_dir).unwrap();
        journal.record(&intent).unwrap();
        journal.mark_spawned(intent.incarnation, &instance).unwrap();
        // Simulate retirement before a failed database acknowledgement.
        // This marker is deliberately not an OS cleanup proof.
        journal.mark_reaped(intent.incarnation).unwrap();
        drop(journal);
        let isolator = LinuxOciIsolator::with_profile(
            JournalLaunchIntentStore::open(&journal_dir).unwrap(),
            profile,
        );
        assert!(isolator.recorded_instances(&intent.db).unwrap().is_empty());
        assert_eq!(
            isolator
                .recorded_instance(&intent.db, intent.incarnation)
                .unwrap(),
            Some(instance.clone())
        );
        assert_eq!(
            isolator
                .recorded_instance(
                    &DbIdentity::new("foreign-database").unwrap(),
                    intent.incarnation
                )
                .unwrap(),
            None
        );
        let unknown = "00000000-0000-4000-8000-000000000010".parse().unwrap();
        assert_eq!(
            isolator.recorded_instance(&intent.db, unknown).unwrap(),
            None
        );
        for _ in 0..2 {
            let error = isolator.reap(&instance).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::PolicyUnenforceable);
            assert_eq!(
                error.details.reason.as_deref(),
                Some(if cfg!(target_os = "linux") {
                    "cgroup_unproven"
                } else {
                    "policy_unenforceable"
                })
            );
        }
        let mut foreign = instance;
        foreign.owner_label = "foreign-owner".into();
        assert_eq!(
            isolator
                .reap(&foreign)
                .await
                .unwrap_err()
                .details
                .reason
                .as_deref(),
            Some("instance_unowned")
        );
    }

    #[test]
    fn unacknowledged_launch_resolves_the_real_oci_identity() {
        let dir = tempfile::tempdir().unwrap();
        let (profile, intent, instance) = cleanup_fixture(dir.path());
        let journal = JournalLaunchIntentStore::open(&dir.path().join("journal")).unwrap();
        journal.record(&intent).unwrap();
        let isolator = LinuxOciIsolator::with_profile(journal, profile);
        assert_eq!(
            isolator.recorded_instances(&intent.db).unwrap(),
            vec![instance.clone()]
        );
        assert_eq!(
            isolator
                .recorded_instance(&intent.db, intent.incarnation)
                .unwrap(),
            Some(instance)
        );
    }

    #[test]
    fn attempt_socket_resolution_shares_intent_discovery_and_retirement() {
        let dir = tempfile::tempdir().unwrap();
        let journal_dir = dir.path().join("journal");
        let profile = QualifiedOciProfile {
            runtime: CertifiedBinary {
                role: "crun".into(),
                absolute_path: dir.path().join("crun").to_string_lossy().into_owned(),
                version: "pinned".into(),
                sha256: Hash256::from_bytes([1; 32]),
            },
            rootfs: dir.path().join("rootfs"),
            rootfs_sha256: Hash256::from_bytes([2; 32]),
            runtime_root: dir.path().join("runtime"),
            cgroup_root: dir.path().join("cgroup"),
            cli_args: vec!["--acp".into()],
            service_socket: None,
            gateway_socket: None,
            auth_mounts: Vec::new(),
            container_env: BTreeMap::new(),
        };
        let base = LinuxOciIsolator::with_profile(
            JournalLaunchIntentStore::open(&journal_dir).unwrap(),
            profile.clone(),
        );
        let service = dir.path().join("attempt/service.sock");
        let gateway = dir.path().join("attempt/gateway.sock");
        let attempt = base
            .with_attempt_sockets(service.clone(), gateway.clone())
            .unwrap();
        assert_eq!(base.profile.as_ref().unwrap().service_socket, None);
        assert_eq!(base.profile.as_ref().unwrap().gateway_socket, None);
        let mut expected = profile;
        expected.service_socket = Some(service);
        expected.gateway_socket = Some(gateway);
        assert_eq!(
            serde_json::to_value(attempt.profile.as_ref().unwrap()).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        let db = DbIdentity::new("shared-journal-review").unwrap();
        let intent = LaunchIntent {
            db: db.clone(),
            boot_epoch: Epoch(1),
            incarnation: "00000000-0000-4000-8000-00000000000e".parse().unwrap(),
            owner_label: "shared-intent".into(),
            image_digest: "sha256:shared-image".into(),
            plan_hash: Hash256::from_bytes([3; 32]),
            spawned: None,
            reaped: false,
        };
        attempt.intents.record(&intent).unwrap();
        let instance = SandboxInstance::from_intent(&intent);
        attempt
            .intents
            .mark_spawned(intent.incarnation, &instance)
            .unwrap();
        assert_eq!(base.recorded_instances(&db).unwrap(), vec![instance]);
        base.intents.mark_reaped(intent.incarnation).unwrap();
        assert!(attempt.list_unreaped().unwrap().is_empty());
        drop(base);
        assert!(JournalLaunchIntentStore::open(&journal_dir).is_err());
        drop(attempt);
        assert!(JournalLaunchIntentStore::open(&journal_dir)
            .unwrap()
            .list_unreaped()
            .unwrap()
            .is_empty());
    }
}
