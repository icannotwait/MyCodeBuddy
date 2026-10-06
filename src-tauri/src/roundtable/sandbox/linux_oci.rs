//! OCI plan for the rootless crun candidate.
//!
//! The document is data. Building it does not launch crun, mount a namespace,
//! or download an image. Spawn stays `policy_unenforceable` until a Linux host
//! can prove the pinned binary and the namespace set.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use roundtable_protocol::{ErrorCode, Hash256, ProcessTreeProof, RtResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    owner_label, CgroupLimits, DbIdentity, LaunchIntent, NamespaceSet, PlanMount,
    QualifiedOciProfile, SandboxInput, SandboxInstance, SandboxPlan,
};
use crate::roundtable::qualification::{CertifiedBinary, QualificationKey};
use crate::roundtable::rt_error;

const MEMORY_MAX_BYTES: u64 = 512 * 1024 * 1024;
const PIDS_MAX: u64 = 64;
const CPU_QUOTA_US: u64 = 100_000;
const CPU_PERIOD_US: u64 = 100_000;

pub(super) fn build_plan(input: &SandboxInput) -> RtResult<SandboxPlan> {
    let scratch = reject_host_path(&input.scratch, input)?;
    build_plan_with_scratch(input, scratch)
}

fn build_plan_with_scratch(input: &SandboxInput, scratch: PathBuf) -> RtResult<SandboxPlan> {
    if input.global_mcp {
        return Err(rt_error(ErrorCode::InvalidArgument, "global_mcp"));
    }
    let crun = binary(&input.certificate.binaries, "crun")?;
    let cli = binary(&input.certificate.binaries, "cli")?;
    if !std::path::Path::new(&crun.absolute_path).is_absolute()
        || !std::path::Path::new(&cli.absolute_path).is_absolute()
    {
        return Err(rt_error(ErrorCode::InvalidArgument, "binary_not_absolute"));
    }
    let env = clear_then_allow(&input.inherited_env, &input.env_allowlist)?;
    let label = owner_label(&input.db, input.boot_epoch, &input.incarnation);
    let mounts = standard_plan_mounts(&scratch);
    let cgroup = CgroupLimits {
        memory_max_bytes: MEMORY_MAX_BYTES,
        pids_max: PIDS_MAX,
        cpu_quota_us: CPU_QUOTA_US,
    };
    let oci = oci_document(input, &mounts, cli, &env, &label, &cgroup);
    let plan_hash = Hash256::sha256(
        &serde_json::to_vec(&oci)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "oci_encode"))?,
    );
    Ok(SandboxPlan {
        runtime_path: PathBuf::from(&crun.absolute_path),
        runtime_sha256: crun.sha256,
        argv: vec![
            crun.absolute_path.clone(),
            "run".to_string(),
            "--cgroup-manager".to_string(),
            "cgroupfs".to_string(),
            label.clone(),
        ],
        namespaces: NamespaceSet {
            user: true,
            mount: true,
            pid: true,
            network: true,
        },
        image_digest: input.certificate.image_digest.clone(),
        image_readonly: true,
        drop_all_caps: true,
        no_new_privileges: true,
        seccomp_default: "SCMP_ACT_ERRNO".to_string(),
        cgroup,
        mounts,
        host_network: false,
        docker_socket_mounted: false,
        home_mounted: false,
        project_mounted: false,
        host_proc_mounted: false,
        inherited_fds: false,
        devices_allowed: false,
        host_sockets_mounted: false,
        symlinks_followed: false,
        global_mcp: false,
        env_cleared_before_allowlist: true,
        env,
        network_destinations: Vec::new(),
        owner_label: label,
        db: input.db.clone(),
        boot_epoch: input.boot_epoch,
        incarnation: input.incarnation,
        plan_hash,
        allowed_binaries: input.certificate.binaries.clone(),
        oci,
    })
}

fn binary<'a>(binaries: &'a [CertifiedBinary], role: &str) -> RtResult<&'a CertifiedBinary> {
    binaries
        .iter()
        .find(|binary| binary.role == role)
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "binary_missing"))
}

fn canonical_directory(path: &Path) -> RtResult<PathBuf> {
    if !path.is_absolute() {
        return Err(rt_error(ErrorCode::InvalidArgument, "scratch_path"));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "scratch_path"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(rt_error(ErrorCode::InvalidArgument, "scratch_path"));
    }
    // Also reject symlinks/junctions in parents, rather than normalizing an
    // attacker-controlled ancestor just once and reopening it at mount time.
    for ancestor in path.ancestors() {
        if fs::symlink_metadata(ancestor)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "scratch_path"))?
            .file_type()
            .is_symlink()
        {
            return Err(rt_error(ErrorCode::InvalidArgument, "scratch_path"));
        }
    }
    path.canonicalize()
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "scratch_path"))
}
fn reject_host_path(scratch: &Path, input: &SandboxInput) -> RtResult<PathBuf> {
    let scratch = canonical_directory(scratch)?;
    let forbidden = forbidden_paths(input);
    for path in forbidden {
        let forbidden = path
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "forbidden_path"))?;
        if scratch.starts_with(&forbidden) || forbidden.starts_with(&scratch) {
            return Err(rt_error(ErrorCode::InvalidArgument, "scratch_is_host_path"));
        }
    }
    Ok(scratch)
}

/// The HOME exception is limited to the exact installed per-incarnation
/// subtree. The generic builder never has this authority.
fn owned_scratch(
    scratch: &Path,
    profile: &QualifiedOciProfile,
    incarnation: &str,
) -> RtResult<PathBuf> {
    let scratch = canonical_directory(scratch)?;
    let runtime_root = canonical_directory(&profile.runtime_root)?;
    let expected = runtime_root.join("runs").join(incarnation).join("scratch");
    if scratch != expected {
        return Err(rt_error(ErrorCode::InvalidArgument, "scratch_not_owned"));
    }
    let private_parent = scratch
        .parent()
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "scratch_not_owned"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(private_parent)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "scratch_not_owned"))?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(rt_error(ErrorCode::InvalidArgument, "scratch_not_owned"));
        }
    }
    #[cfg(not(unix))]
    let _ = private_parent;
    Ok(scratch)
}

fn qualified_scratch(input: &SandboxInput, profile: &QualifiedOciProfile) -> RtResult<PathBuf> {
    let scratch = owned_scratch(&input.scratch, profile, &input.incarnation.to_string())?;
    let runtime_root = canonical_directory(&profile.runtime_root)?;
    let home = canonical_directory(&input.home)?;
    if runtime_root == home || home.starts_with(&runtime_root) {
        return Err(rt_error(ErrorCode::InvalidArgument, "scratch_not_owned"));
    }
    for path in std::iter::once(&input.project)
        .chain(&input.other_scratches)
        .chain(&input.decoy_paths)
    {
        let forbidden = path
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "forbidden_path"))?;
        if scratch.starts_with(&forbidden) || forbidden.starts_with(&scratch) {
            return Err(rt_error(ErrorCode::InvalidArgument, "scratch_is_host_path"));
        }
    }
    Ok(scratch)
}

fn forbidden_paths(input: &SandboxInput) -> Vec<&std::path::Path> {
    let mut paths = vec![input.project.as_path(), input.home.as_path()];
    paths.extend(input.other_scratches.iter().map(PathBuf::as_path));
    paths.extend(input.decoy_paths.iter().map(PathBuf::as_path));
    paths
}

fn clear_then_allow(
    inherited: &BTreeMap<String, String>,
    allow: &BTreeMap<String, String>,
) -> RtResult<BTreeMap<String, String>> {
    let mut cleared = inherited.clone();
    cleared.clear();
    for (key, value) in allow {
        if forbidden_env(key) {
            return Err(rt_error(ErrorCode::InvalidArgument, "env_not_allowlisted"));
        }
        cleared.insert(key.clone(), value.clone());
    }
    Ok(cleared)
}

fn forbidden_env(key: &str) -> bool {
    matches!(
        key,
        "HOME" | "PATH" | "USERPROFILE" | "DOCKER_HOST" | "MCP_CONFIG"
    ) || key.starts_with("MCP_")
}

fn standard_plan_mounts(scratch: &Path) -> Vec<PlanMount> {
    vec![
        PlanMount {
            source: PathBuf::from("proc"),
            destination: "/proc".to_string(),
            read_only: true,
        },
        PlanMount {
            source: PathBuf::from("tmpfs"),
            destination: "/dev".to_string(),
            read_only: false,
        },
        PlanMount {
            source: PathBuf::from("devpts"),
            destination: "/dev/pts".to_string(),
            read_only: false,
        },
        PlanMount {
            source: PathBuf::from("shm"),
            destination: "/dev/shm".to_string(),
            read_only: false,
        },
        PlanMount {
            source: PathBuf::from("sysfs"),
            destination: "/sys".to_string(),
            read_only: true,
        },
        PlanMount {
            source: PathBuf::from("tmpfs"),
            destination: "/tmp".to_string(),
            read_only: false,
        },
        PlanMount {
            source: scratch.to_path_buf(),
            destination: "/scratch".to_string(),
            read_only: false,
        },
    ]
}

fn mount_json(mount: &PlanMount) -> Value {
    let (kind, options): (&str, &[&str]) = match mount.destination.as_str() {
        "/proc" => ("proc", &["nosuid", "noexec", "nodev"]),
        "/dev" => (
            "tmpfs",
            &["nosuid", "strictatime", "mode=755", "size=65536k", "rw"],
        ),
        "/dev/pts" => (
            "devpts",
            &[
                "nosuid",
                "noexec",
                "newinstance",
                "ptmxmode=0666",
                "mode=0620",
                "rw",
            ],
        ),
        "/dev/shm" => (
            "tmpfs",
            &[
                "nosuid",
                "noexec",
                "nodev",
                "mode=1777",
                "size=67108864",
                "rw",
            ],
        ),
        "/sys" => ("sysfs", &["nosuid", "noexec", "nodev", "ro"]),
        "/tmp" => (
            "tmpfs",
            &["nosuid", "nodev", "mode=1777", "size=268435456", "rw"],
        ),
        // Live `/scratch` stays stricter than the probe's scratch bind
        // (`bind` without `nosymfollow`). The attempt workspace must not
        // follow a symlink out of that directory. Resolver, hook, seccomp,
        // and auth staging below are the shared path.
        _ => ("bind", &["rbind", "rw", "nosuid", "nodev", "nosymfollow"]),
    };
    json!({
        "destination": mount.destination,
        "type": kind,
        "source": mount.source,
        "options": options,
    })
}

pub(crate) fn runtime_mount_json() -> Vec<Value> {
    standard_plan_mounts(Path::new("/unused"))
        .into_iter()
        .filter(|mount| mount.destination != "/scratch")
        .map(|mount| mount_json(&mount))
        .collect()
}

fn oci_document(
    input: &SandboxInput,
    mounts: &[PlanMount],
    cli: &CertifiedBinary,
    env: &BTreeMap<String, String>,
    label: &str,
    cgroup: &CgroupLimits,
) -> Value {
    let env_list: Vec<String> = env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": {"uid": 0, "gid": 0},
            "args": [cli.absolute_path],
            "env": env_list,
            "cwd": "/scratch",
            "noNewPrivileges": true,
            "capabilities": {
                "bounding": [],
                "effective": [],
                "permitted": [],
                "inheritable": [],
                "ambient": []
            }
        },
        "root": {"path": "rootfs", "readonly": true},
        "mounts": mounts.iter().map(mount_json).collect::<Vec<_>>(),
        "linux": {
            "namespaces": [
                {"type": "user"},
                {"type": "mount"},
                {"type": "pid"},
                {"type": "network"},
                {"type": "ipc"},
                {"type": "uts"},
                {"type": "cgroup"}
            ],
            "maskedPaths": ["/proc/acpi", "/proc/kcore", "/proc/keys"],
            "readonlyPaths": ["/proc/sys"],
            "seccomp": seccomp_json(),
            "resources": {
                "memory": {"limit": cgroup.memory_max_bytes},
                "pids": {"limit": cgroup.pids_max},
                "cpu": {"quota": cgroup.cpu_quota_us, "period": CPU_PERIOD_US},
                "devices": [{"allow": false, "access": "rwm"}]
            }
        },
        "annotations": {
            "io.codeg.roundtable.label": label,
            "io.codeg.roundtable.image": input.certificate.image_digest,
            "io.codeg.roundtable.network": "isolated-deny-egress"
        }
    })
}

// A frozen deny-default syscall contract. Filesystem access remains bounded by
// the read-only root, scratch mount and fixed per-attempt Unix socket mounts.
pub(crate) const SYSCALLS: &[&str] = &[
    "read",
    "write",
    "readv",
    "writev",
    "pread64",
    "pwrite64",
    "close",
    "close_range",
    "open",
    "openat",
    "openat2",
    "newfstatat",
    "fstat",
    "stat",
    "lstat",
    "statx",
    "access",
    "faccessat",
    "faccessat2",
    "lseek",
    "getdents64",
    "readlink",
    "readlinkat",
    "mmap",
    "mprotect",
    "munmap",
    "mremap",
    "madvise",
    "brk",
    "arch_prctl",
    "rt_sigaction",
    "rt_sigprocmask",
    "rt_sigreturn",
    "sigaltstack",
    "signalfd4",
    "futex",
    "futex_waitv",
    "set_tid_address",
    "set_robust_list",
    "rseq",
    "clock_gettime",
    "clock_nanosleep",
    "nanosleep",
    "gettimeofday",
    "getrandom",
    "getpid",
    "getppid",
    "gettid",
    "getuid",
    "geteuid",
    "getgid",
    "getegid",
    "getgroups",
    "getcwd",
    "chdir",
    "fchdir",
    "uname",
    "sysinfo",
    "sched_getaffinity",
    "sched_yield",
    "prlimit64",
    "getrlimit",
    "getrusage",
    "fcntl",
    "ioctl",
    "dup",
    "dup2",
    "dup3",
    "pipe",
    "pipe2",
    "poll",
    "ppoll",
    "select",
    "pselect6",
    "epoll_create1",
    "epoll_ctl",
    "epoll_wait",
    "epoll_pwait",
    "epoll_pwait2",
    "eventfd2",
    "socket",
    "socketpair",
    "connect",
    "bind",
    "listen",
    "accept",
    "accept4",
    "getsockname",
    "getpeername",
    "getsockopt",
    "setsockopt",
    "sendto",
    "recvfrom",
    "sendmsg",
    "recvmsg",
    // glibc and Node resolve AF_UNSPEC with these. Without them DNS
    // returns EAI_AGAIN even when the slirp resolver is up.
    "sendmmsg",
    "recvmmsg",
    "shutdown",
    "clone",
    "clone3",
    "fork",
    "vfork",
    "execve",
    "wait4",
    "waitid",
    "kill",
    "tgkill",
    "exit",
    "exit_group",
    "restart_syscall",
    "mkdir",
    "mkdirat",
    "unlink",
    "unlinkat",
    "rename",
    "renameat",
    "renameat2",
    "ftruncate",
    "truncate",
    "fsync",
    "fdatasync",
    "chmod",
    "fchmod",
    "fchmodat",
    "utimensat",
    "flock",
    "umask",
    "getresuid",
    "getresgid",
    "prctl",
    // bash, Node, and Go adapters issue these while starting. They do not
    // grant mount, ptrace, namespace, or kernel-module control.
    "getpgrp",
    "getpgid",
    "setpgid",
    "setsid",
    // Antigravity 1.3.0 aborts in its runtime when interval timers are denied.
    "getitimer",
    "setitimer",
    "timerfd_create",
    "timerfd_settime",
    "timerfd_gettime",
    "memfd_create",
    "statfs",
    "fstatfs",
    "inotify_init1",
    "inotify_add_watch",
    "inotify_rm_watch",
    "membarrier",
    "copy_file_range",
];

pub(crate) fn seccomp_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "SCMP_ARCH_AARCH64"
    } else {
        "SCMP_ARCH_X86_64"
    }
}

/// Deny-by-default seccomp document shared by the qualification probe and the
/// live plan. Architecture and the syscall allowlist cannot be edited on only
/// one of those paths.
pub(crate) fn seccomp_json() -> Value {
    json!({
        "defaultAction": "SCMP_ACT_ERRNO",
        "architectures": [seccomp_arch()],
        "syscalls": [{"names": SYSCALLS, "action": "SCMP_ACT_ALLOW"}]
    })
}

fn document_hash(oci: &Value) -> RtResult<Hash256> {
    Ok(Hash256::sha256(&serde_json::to_vec(oci).map_err(|_| {
        rt_error(ErrorCode::InvalidArgument, "oci_encode")
    })?))
}

pub(super) fn validate_document(plan: &SandboxPlan) -> RtResult<()> {
    if document_hash(&plan.oci)? != plan.plan_hash
        || plan.oci["root"]["readonly"] != true
        || plan.oci["process"]["noNewPrivileges"] != true
        || plan.oci["linux"]["seccomp"]["defaultAction"] != "SCMP_ACT_ERRNO"
        || plan.oci["annotations"]["io.codeg.roundtable.label"] != plan.owner_label
        || plan.oci["annotations"]["io.codeg.roundtable.image"] != plan.image_digest
        || plan.oci["linux"]["resources"]["memory"]["limit"] != plan.cgroup.memory_max_bytes
        || plan.oci["linux"]["resources"]["pids"]["limit"] != plan.cgroup.pids_max
        || plan.oci["linux"]["resources"]["cpu"]["quota"] != plan.cgroup.cpu_quota_us
        || plan.oci["linux"]["resources"]["devices"] != json!([{"allow":false,"access":"rwm"}])
    {
        return Err(rt_error(ErrorCode::InvalidArgument, "plan_hash_mismatch"));
    }
    let namespaces = plan.oci["linux"]["namespaces"]
        .as_array()
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "plan_incomplete"))?;
    for required in ["user", "mount", "pid", "network"] {
        if !namespaces
            .iter()
            .any(|entry| entry["type"] == required && entry.get("path").is_none())
        {
            return Err(rt_error(ErrorCode::InvalidArgument, "plan_incomplete"));
        }
    }
    for group in [
        "bounding",
        "effective",
        "permitted",
        "inheritable",
        "ambient",
    ] {
        if plan.oci["process"]["capabilities"][group]
            .as_array()
            .is_none_or(|caps| !caps.is_empty())
        {
            return Err(rt_error(ErrorCode::InvalidArgument, "plan_incomplete"));
        }
    }
    let mounts = plan.oci["mounts"]
        .as_array()
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "plan_incomplete"))?;
    if mounts.len() != plan.mounts.len()
        || plan.mounts.iter().any(|mount| {
            !mounts.iter().any(|entry| {
                entry["source"] == mount.source.to_string_lossy().as_ref()
                    && entry["destination"] == mount.destination
                    && entry["options"].as_array().is_some_and(|options| {
                        options.iter().any(|option| {
                            option
                                == if mount.read_only && mount.destination != "/proc" {
                                    "ro"
                                } else if mount.destination == "/proc" {
                                    "nodev"
                                } else {
                                    "rw"
                                }
                        })
                    })
            })
        })
    {
        return Err(rt_error(ErrorCode::InvalidArgument, "plan_incomplete"));
    }
    let env: Vec<String> = plan
        .env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    if plan.oci["process"]["env"] != json!(env) {
        return Err(rt_error(ErrorCode::InvalidArgument, "plan_incomplete"));
    }
    Ok(())
}

pub(super) fn profile_hash(
    profile: &QualifiedOciProfile,
    key: &QualificationKey,
) -> RtResult<Hash256> {
    roundtable_protocol::canonical_hash(&json!({
        "isolator_contract":"codeg-linux-oci-v2", "runtime":profile.runtime,
        "rootfs":profile.rootfs,"rootfs_sha256":profile.rootfs_sha256,
        "runtime_root":profile.runtime_root,"cgroup_root":profile.cgroup_root,
        "cli_args":profile.cli_args,"socket_destinations":["/run/codeg/roundtable.sock","/run/codeg/gateway.sock"],
        "auth_mounts":profile.auth_mounts,"container_env":profile.container_env,
        "binaries":key.binaries,"image_digest":key.image_digest,"policy_hash":key.policy_hash,
        "syscalls":SYSCALLS,"memory":MEMORY_MAX_BYTES,"pids":PIDS_MAX,"cpu":CPU_QUOTA_US
    }))
}

pub(super) fn build_qualified_plan(
    input: &SandboxInput,
    profile: &QualifiedOciProfile,
) -> RtResult<SandboxPlan> {
    if profile.service_socket.is_none() || profile.gateway_socket.is_none() {
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "attempt_sockets_required",
        ));
    }
    let scratch = qualified_scratch(input, profile)?;
    let mut plan = build_plan_with_scratch(input, scratch)?;
    if binary(&input.certificate.binaries, "crun")? != &profile.runtime
        || profile_hash(profile, &input.certificate)? != input.certificate.plan_hash
    {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "qualification_plan_drift",
        ));
    }
    let id = runtime_id(
        &input.db,
        input.boot_epoch.0,
        &input.incarnation.to_string(),
    );
    plan.oci["root"]["path"] = json!(profile.rootfs);
    let cli = binary(&input.certificate.binaries, "cli")?;
    let mut args = vec![cli.absolute_path.clone()];
    args.extend(profile.cli_args.clone());
    plan.oci["process"]["args"] = json!(args);
    plan.oci["linux"]["cgroupsPath"] = json!(oci_cgroup_path(profile, &id)?);
    plan.oci["linux"]["seccomp"] = seccomp_json();
    #[cfg(target_os = "linux")]
    {
        // The qualified rootless container maps only the launching operator.
        plan.oci["linux"]["uidMappings"] =
            json!([{"containerID":0,"hostID":unsafe {libc::geteuid()},"size":1}]);
        plan.oci["linux"]["gidMappings"] =
            json!([{"containerID":0,"hostID":unsafe {libc::getegid()},"size":1}]);
    }
    for (source, destination) in [
        (&profile.service_socket, "/run/codeg/roundtable.sock"),
        (&profile.gateway_socket, "/run/codeg/gateway.sock"),
    ] {
        if let Some(source) = source {
            if !source.is_absolute() {
                return Err(rt_error(ErrorCode::InvalidArgument, "socket_path"));
            }
            plan.mounts.push(PlanMount {
                source: source.clone(),
                destination: destination.into(),
                read_only: true,
            });
            plan.oci["mounts"].as_array_mut().ok_or_else(||rt_error(ErrorCode::InvalidArgument,"oci_mounts"))?
                .push(json!({"source":source,"destination":destination,"type":"bind","options":["bind","ro","nosuid","nodev","noexec"]}));
        }
    }
    install_home_upper(&mut plan)?;
    apply_auth_mounts(&mut plan, profile)?;
    install_slirp_hook(&mut plan, profile, &id)?;
    plan.plan_hash = document_hash(&plan.oci)?;
    verify_profile(&plan, profile)?;
    Ok(plan)
}

fn file_hash(path: &Path) -> RtResult<Hash256> {
    let mut file = fs::File::open(path)
        .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "binary_unreadable"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "binary_unreadable"))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(Hash256::from_bytes(hash.finalize().into()))
}

/// Deterministic expanded-rootfs digest (`codeg-rootfs-v2`).
///
/// Symlink targets are hashed as text and are not followed or required to
/// exist inside the image. Device nodes, fifos, and sockets contribute type
/// and device identity, not a read of the node. A regular file the probing
/// user cannot read fails with the path; `chown` the tree to that user so
/// mode bits, and therefore the digest, stay stable.
pub(super) fn rootfs_digest(root: &Path) -> RtResult<Hash256> {
    rootfs_digest_detail(root)
        .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "rootfs_unreadable"))
}

pub(super) fn rootfs_digest_detail(root: &Path) -> Result<Hash256, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("rootfs_unreadable: {}: {error}", root.display()))?;
    if !root.is_dir() {
        return Err(format!(
            "rootfs_unreadable: {} is not a directory",
            root.display()
        ));
    }
    let mut entries = Vec::new();
    for item in walkdir::WalkDir::new(&root).follow_links(false) {
        match item {
            Ok(entry) => entries.push(entry),
            Err(error) => {
                let path = error
                    .path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| root.display().to_string());
                return Err(format!(
                    "rootfs_unreadable: {path} ({error}). chown -R the rootfs to the probing user so root-only files and directories can be read without changing mode bits"
                ));
            }
        }
    }
    entries.sort_by(|a, b| a.path().cmp(b.path()));
    let mut hash = Sha256::new();
    hash.update(b"codeg-rootfs-v2\0");
    for entry in entries {
        let relative = entry
            .path()
            .strip_prefix(&root)
            .map_err(|_| format!("rootfs_path: {}", entry.path().display()))?;
        let path = relative
            .to_str()
            .ok_or_else(|| format!("rootfs_path: {}", entry.path().display()))?
            .replace('\\', "/");
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("rootfs_unreadable: {}: {error}", entry.path().display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            hash.update(metadata.mode().to_le_bytes());
        }
        if metadata.is_dir() {
            hash.update(b"d");
        } else if metadata.is_file() {
            hash.update(b"f");
            let digest = file_hash(entry.path()).map_err(|_| {
                format!(
                    "rootfs_unreadable: {}. chown -R the rootfs to the probing user; chmod a+rX also works but changes mode bits and the digest",
                    entry.path().display()
                )
            })?;
            hash.update(digest.to_hex().as_bytes());
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(entry.path())
                .map_err(|error| format!("rootfs_path: {}: {error}", entry.path().display()))?;
            let text = target.to_string_lossy();
            hash.update(b"l");
            hash.update((text.len() as u64).to_le_bytes());
            hash.update(text.as_bytes());
        } else {
            hash_special(&mut hash, entry.path(), &metadata)?;
        }
    }
    Ok(Hash256::from_bytes(hash.finalize().into()))
}

fn hash_special(hash: &mut Sha256, path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let kind = if metadata.file_type().is_char_device() {
            b'c'
        } else if metadata.file_type().is_block_device() {
            b'b'
        } else if metadata.file_type().is_fifo() {
            b'p'
        } else if metadata.file_type().is_socket() {
            b's'
        } else {
            return Err(format!("rootfs_special_file: {}", path.display()));
        };
        hash.update([kind]);
        hash.update(metadata.rdev().to_le_bytes());
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (hash, metadata);
        Err(format!("rootfs_special_file: {}", path.display()))
    }
}

fn apply_auth_mounts(plan: &mut SandboxPlan, profile: &QualifiedOciProfile) -> RtResult<()> {
    for (key, value) in &profile.container_env {
        if !container_env_allowed(key, value) {
            return Err(rt_error(ErrorCode::InvalidArgument, "container_env"));
        }
        plan.env.insert(key.clone(), value.clone());
    }
    let env_list: Vec<String> = plan
        .env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    plan.oci["process"]["env"] = json!(env_list);
    let upper = plan
        .mounts
        .iter()
        .find(|item| item.destination == "/rt-home")
        .map(|item| item.source.clone())
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "home_upper"))?;
    for mount in &profile.auth_mounts {
        if !auth_destination_allowed(&mount.destination) || !mount.source.is_absolute() {
            return Err(rt_error(ErrorCode::InvalidArgument, "auth_mount"));
        }
        let root = profile
            .rootfs
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "rootfs_unreadable"))?;
        let source = mount
            .source
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "auth_mount"))?;
        if source.starts_with(&root) || !source.is_file() {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "credential_baked_into_image",
            ));
        }
        // Grok refreshes `auth.json` in place. That destination is a writable
        // copy in the attempt home. Every other file, including Cursor, stays
        // a read-only bind and is never copied.
        if !prepare_attempt_auth(&upper, &mount.source, &mount.destination)? {
            continue;
        }
        plan.mounts.push(PlanMount {
            source: mount.source.clone(),
            destination: mount.destination.clone(),
            read_only: true,
        });
        plan.home_mounted = false;
        plan.oci["mounts"]
            .as_array_mut()
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "oci_mounts"))?
            .push(json!({
                "source": mount.source,
                "destination": mount.destination,
                "type": "bind",
                "options": ["bind", "ro", "nosuid", "nodev", "noexec", "nosymfollow"]
            }));
    }
    Ok(())
}

fn home_upper_allowed(plan: &SandboxPlan, mount: &PlanMount) -> bool {
    if mount.read_only || !mount.source.is_absolute() {
        return false;
    }
    let Some(scratch) = plan
        .mounts
        .iter()
        .find(|item| item.destination == "/scratch")
    else {
        return false;
    };
    mount.source.starts_with(&scratch.source) && mount.source != scratch.source
}

fn install_home_upper(plan: &mut SandboxPlan) -> RtResult<()> {
    let scratch = plan
        .mounts
        .iter()
        .find(|item| item.destination == "/scratch")
        .map(|item| item.source.clone())
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "home_upper"))?;
    let upper = scratch.join("rt-home");
    fs::create_dir_all(&upper)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "home_upper"))?;
    plan.mounts.push(PlanMount {
        source: upper.clone(),
        destination: "/rt-home".to_string(),
        read_only: false,
    });
    plan.oci["mounts"]
        .as_array_mut()
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "oci_mounts"))?
        .push(json!({
            "source": upper,
            "destination": "/rt-home",
            "type": "bind",
            "options": ["bind", "rw", "nosuid", "nodev"]
        }));
    Ok(())
}

/// OCI hook phase. `createRuntime` runs while container init is still
/// non-dumpable, so `/proc/<pid>/ns/user` does not exist yet. `poststart`
/// runs after the user process is executed and the namespace path is visible.
pub(crate) const SLIRP_HOOK_PHASE: &str = "poststart";

pub(crate) const SLIRP_HOOK_SCRIPT: &str = r#"#!/bin/sh
state=$(cat)
pid=$(printf '%s' "$state" | sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)
[ -n "$pid" ] || exit 1
[ "$CODEG_ROUNDTABLE_SLIRP_OWNER" = "$3" ] || exit 1
[ "$CODEG_ROUNDTABLE_SLIRP_ROOT" = "$4" ] || exit 1
# Cleanup holds this same lock while closing the startup gate. All helper
# children close fd 9 so the hook alone owns the startup critical section.
[ -f "$2.lock" ] && [ ! -L "$2.lock" ] || exit 1
exec 9<>"$2.lock" || exit 1
flock -x -w 10 9 || exit 1
[ "$(cat "$2.state")" = "prepared" ] || exit 1
printf 'starting\n' > "$2.state" && sync -f "$2.state" || exit 1
umask 077
ready="$2.ready"
exit_pipe="$2.exit"
mkfifo "$ready" "$exit_pipe" || exit 1
# The parent opens the ready FIFO before forking. A failed pin publication
# cannot strand a child blocked on opening a FIFO with no reader.
exec 8<>"$ready" || exit 1
container_start=$(sed 's/^.*) //' "/proc/$pid/stat" | cut -d ' ' -f20)
[ -n "$container_start" ] || exit 1
CODEG_ROUNDTABLE_SLIRP_ROLE=watcher /bin/sh -c '
  while kill -0 "$1" 2>/dev/null; do
    current=$(sed "s/^.*) //" "/proc/$1/stat" 2>/dev/null | cut -d " " -f20)
    [ "$current" = "$2" ] || break
    sleep 0.2
  done
' watcher "$pid" "$container_start" 8>&- 9>&- >"$exit_pipe" &
watcher=$!
CODEG_ROUNDTABLE_SLIRP_ROLE=slirp "$1" --configure --disable-host-loopback --mtu=65520 \
  --userns-path="/proc/$pid/ns/user" \
  --netns-type=path --ready-fd 3 --exit-fd=0 \
  "/proc/$pid/ns/net" tap0 8>&- 9>&- <"$exit_pipe" >/dev/null 2>&1 3>"$ready" &
slirp=$!
slirp_start=$(sed 's/^.*) //' "/proc/$slirp/stat" | cut -d ' ' -f20)
watcher_start=$(sed 's/^.*) //' "/proc/$watcher/stat" | cut -d ' ' -f20)
[ -n "$slirp_start" ] && [ -n "$watcher_start" ] || exit 1
printf 'slirp %s %s\nwatcher %s %s\n' "$slirp" "$slirp_start" "$watcher" "$watcher_start" > "$2" && sync -f "$2" || exit 1
ready_byte=$(timeout 8 dd bs=1 count=1 <&8 2>/dev/null || true)
exec 8>&-
rm -f "$ready"
[ "$ready_byte" = "1" ] || exit 1
printf 'running\n' > "$2.state" && sync -f "$2.state" || exit 1
exit 0
"#;

fn install_slirp_hook(
    plan: &mut SandboxPlan,
    profile: &QualifiedOciProfile,
    id: &str,
) -> RtResult<()> {
    let slirp = slirp_binary()
        .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "slirp4netns_missing"))?;
    apply_slirp_edits(
        &mut plan.oci,
        &mut plan.mounts,
        &profile.runtime_root,
        &profile.runtime_root.join("slirp-hook.sh"),
        id,
        &slirp,
    )
}

pub(crate) fn slirp_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| {
            let candidate = dir.join("slirp4netns");
            candidate.is_file().then_some(candidate)
        })
    }) {
        return Some(path);
    }
    let fallback = PathBuf::from("/usr/bin/slirp4netns");
    fallback.is_file().then_some(fallback)
}

pub(crate) const SLIRP_RESOLV_BODY: &[u8] = b"nameserver 10.0.2.3\n";
pub(crate) const SLIRP_NETWORK: &str = "slirp-egress-not-origin-filtered";
/// Grok replaces this file when it refreshes the OIDC login. Keyed by
/// destination so the certificate schema, and therefore `plan_hash`, stays put.
pub(crate) const GROK_AUTH_DESTINATION: &str = "/rt-home/.grok/auth.json";
const ATTEMPT_AUTH_COPY_LIMIT: u64 = 64 * 1024;

pub(crate) struct SlirpAttachment {
    pub hooks: Value,
    pub resolv_mount: Value,
    pub resolv_source: PathBuf,
    pub network: &'static str,
}

pub(crate) fn slirp_resolv_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join("slirp-resolv.conf")
}

/// Writes the poststart hook, its watcher script, and the slirp resolver file.
/// The probe and `install_slirp_hook` both call only this function.
pub(crate) fn attach_slirp(
    runtime_root: &Path,
    hook_path: &Path,
    id: &str,
    slirp_bin: &Path,
) -> RtResult<SlirpAttachment> {
    if let Some(parent) = hook_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "slirp_hook"))?;
    }
    fs::write(hook_path, SLIRP_HOOK_SCRIPT)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "slirp_hook"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(hook_path, fs::Permissions::from_mode(0o755))
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "slirp_hook"))?;
    }
    let dir = runtime_root.join("slirp-pids");
    secure_directory(&dir)?;
    let pidfile = dir.join(format!("{id}.pid"));
    initialize_slirp_lifecycle(&pidfile)?;
    let resolv = slirp_resolv_path(runtime_root);
    fs::write(&resolv, SLIRP_RESOLV_BODY)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "slirp_resolv"))?;
    Ok(SlirpAttachment {
        hooks: json!({
            SLIRP_HOOK_PHASE: [{
                "path": hook_path,
                "args": ["slirp-hook.sh", slirp_bin, pidfile, id, runtime_root],
                "env": [
                    format!("CODEG_ROUNDTABLE_SLIRP_OWNER={id}"),
                    format!("CODEG_ROUNDTABLE_SLIRP_ROOT={}", runtime_root.display()),
                    "CODEG_ROUNDTABLE_SLIRP_ROLE=hook"
                ]
            }]
        }),
        resolv_mount: json!({
            "destination": "/etc/resolv.conf",
            "type": "bind",
            "source": &resolv,
            "options": ["bind", "ro", "nosuid", "nodev", "noexec"]
        }),
        resolv_source: resolv,
        network: SLIRP_NETWORK,
    })
}

fn apply_slirp_edits(
    oci: &mut Value,
    mounts: &mut Vec<PlanMount>,
    runtime_root: &Path,
    hook_path: &Path,
    id: &str,
    slirp_bin: &Path,
) -> RtResult<()> {
    let attachment = attach_slirp(runtime_root, hook_path, id, slirp_bin)?;
    oci["annotations"]["io.codeg.roundtable.network"] = json!(attachment.network);
    oci["hooks"] = attachment.hooks;
    mounts.push(PlanMount {
        source: attachment.resolv_source,
        destination: "/etc/resolv.conf".to_string(),
        read_only: true,
    });
    oci["mounts"]
        .as_array_mut()
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "oci_mounts"))?
        .push(attachment.resolv_mount);
    Ok(())
}

/// `true` means the caller must bind `source` read-only at `destination`.
/// Grok's auth file returns `false`: the bytes live only in the attempt home.
pub(crate) fn prepare_attempt_auth(
    upper: &Path,
    source: &Path,
    destination: &str,
) -> RtResult<bool> {
    let relative = destination
        .strip_prefix("/rt-home/")
        .filter(|relative| {
            !relative.is_empty() && !relative.contains("..") && !relative.ends_with('/')
        })
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "auth_mount"))?;
    let path = upper.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "auth_copy"))?;
    }
    if destination == GROK_AUTH_DESTINATION {
        copy_regular_nofollow(source, &path, ATTEMPT_AUTH_COPY_LIMIT)?;
        Ok(false)
    } else {
        if !path.exists() {
            fs::write(&path, b"")
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "auth_copy"))?;
        }
        Ok(true)
    }
}

fn copy_regular_nofollow(source: &Path, dest: &Path, limit: u64) -> RtResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let input = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(source)
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "auth_copy"))?;
        let meta = input
            .metadata()
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "auth_copy"))?;
        if !meta.is_file() || meta.len() > limit {
            return Err(rt_error(ErrorCode::CapabilityUnqualified, "auth_copy"));
        }
        #[cfg(test)]
        control_tests::before_auth_read(&input);
        let mut bytes = Vec::new();
        input
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "auth_copy"))?;
        if bytes.len() as u64 > limit {
            return Err(rt_error(ErrorCode::CapabilityUnqualified, "auth_copy"));
        }
        let tmp_name = format!(
            ".{}.codeg-attempt",
            dest.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("auth")
        );
        let tmp = dest.with_file_name(tmp_name);
        let _ = fs::remove_file(&tmp);
        {
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&tmp)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "auth_copy"))?;
            output
                .write_all(&bytes)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "auth_copy"))?;
        }
        fs::rename(&tmp, dest).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "auth_copy"))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (source, dest, limit);
        Err(rt_error(ErrorCode::PolicyUnenforceable, "auth_copy"))
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub fn live_slirp_document(runtime_root: &Path, id: &str, slirp_bin: &Path) -> RtResult<Value> {
    let mut oci = json!({
        "mounts": [],
        "annotations": {"io.codeg.roundtable.network": "isolated-deny-egress"},
        "linux": {"seccomp": seccomp_json()}
    });
    let mut mounts = Vec::new();
    apply_slirp_edits(
        &mut oci,
        &mut mounts,
        runtime_root,
        &runtime_root.join("slirp-hook.sh"),
        id,
        slirp_bin,
    )?;
    if mounts.len() != 1 || mounts[0].destination != "/etc/resolv.conf" || !mounts[0].read_only {
        return Err(rt_error(ErrorCode::InvalidArgument, "slirp_resolv"));
    }
    Ok(oci)
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperBirthContext {
    version: u32,
    boot_id: String,
    pid_namespace: String,
    time_namespace: Option<String>,
    zero_boottime_offset: bool,
    creator_start_ticks: u64,
}

#[cfg(target_os = "linux")]
fn validate_proc_number_space(link: &str, pid: u32) -> RtResult<()> {
    if link.parse::<u32>().ok() != Some(pid) {
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "slirp_proc_number_space",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_proc_namespace_status(status: &str, pid: u32, tid: u32) -> RtResult<()> {
    let invalid = || {
        rt_error(
            ErrorCode::PolicyUnenforceable,
            "slirp_proc_namespace_domain",
        )
    };
    if status.len() > 4096 || pid == 0 || tid == 0 {
        return Err(invalid());
    }
    for (field, expected) in [("NStgid", pid), ("NSpid", tid)] {
        let mut lines = status
            .lines()
            .filter(|line| line.trim_start().starts_with(field));
        let line = lines.next().ok_or_else(invalid)?;
        if lines.next().is_some() {
            return Err(invalid());
        }
        let values = line
            .strip_prefix(field)
            .and_then(|rest| rest.strip_prefix(':'))
            .ok_or_else(invalid)?;
        let mut values = values.split_whitespace();
        let value = values.next().ok_or_else(invalid)?;
        if values.next().is_some()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || value.parse::<u32>().ok() != Some(expected)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn helper_start_ticks(stat: &str) -> RtResult<u64> {
    stat.rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            rt_error(
                ErrorCode::PolicyUnenforceable,
                "slirp_proc_start_unreadable",
            )
        })
}

#[cfg(target_os = "linux")]
fn zero_boottime_offset(offsets: &str) -> bool {
    let mut boot = offsets
        .lines()
        .filter(|line| line.split_whitespace().next() == Some("boottime"));
    let Some(line) = boot.next() else {
        return false;
    };
    let fields: Vec<_> = line.split_whitespace().collect();
    boot.next().is_none() && fields == ["boottime", "0", "0"]
}

#[cfg(target_os = "linux")]
fn usable_helper_boot_clock(
    current: Option<&str>,
    children: Option<&str>,
    offsets: Option<&str>,
) -> bool {
    current.is_some() && current == children && offsets.is_some_and(zero_boottime_offset)
}

#[cfg(target_os = "linux")]
impl HelperBirthContext {
    fn capture() -> RtResult<Self> {
        let unavailable = || {
            rt_error(
                ErrorCode::PolicyUnenforceable,
                "slirp_birth_context_unavailable",
            )
        };
        let read = |path: &str| -> RtResult<String> {
            let mut text = String::new();
            fs::File::open(path)
                .map_err(|_| unavailable())?
                .take(4097)
                .read_to_string(&mut text)
                .map_err(|_| unavailable())?;
            if text.len() > 4096 {
                return Err(unavailable());
            }
            Ok(text)
        };
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        if tid <= 0 {
            return Err(unavailable());
        }
        let thread_link = fs::read_link("/proc/thread-self").map_err(|_| unavailable())?;
        if thread_link.as_path() != Path::new(&format!("{}/task/{tid}", std::process::id())) {
            return Err(unavailable());
        }
        let namespace = |name: &str| -> RtResult<Option<String>> {
            match fs::read_link(format!("/proc/{tid}/ns/{name}")) {
                Ok(link) => link
                    .to_str()
                    .map(|link| Some(link.to_string()))
                    .ok_or_else(unavailable),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(_) => Err(unavailable()),
            }
        };
        let self_link = fs::read_link("/proc/self").map_err(|_| unavailable())?;
        validate_proc_number_space(
            self_link.to_str().ok_or_else(unavailable)?,
            std::process::id(),
        )?;
        // Equal caller IDs alone are insufficient: an ancestor-mounted proc
        // filesystem can coincidentally use the same numbers for this caller
        // while mapping another numeric PID to a different process. Kernel
        // status lists every namespace level, including duplicate equal IDs.
        validate_proc_namespace_status(
            &read("/proc/thread-self/status")?,
            std::process::id(),
            tid as u32,
        )?;
        let boot_id = uuid::Uuid::parse_str(read("/proc/sys/kernel/random/boot_id")?.trim())
            .map_err(|_| unavailable())?;
        if boot_id.is_nil() {
            return Err(unavailable());
        }
        let pid_namespace = namespace("pid")?.ok_or_else(unavailable)?;
        // Unavailable time context is explicitly unsupported, never evidence
        // for exclusion. Boot/PID identity errors above still prevent use of
        // numeric process identities altogether.
        let time_namespace = namespace("time").unwrap_or(None);
        let time_for_children = namespace("time_for_children").unwrap_or(None);
        // timens_offsets describes the children namespace, while proc stat
        // uses the reader's current time namespace. A negative offset can
        // wrap an old birth timestamp, so only an exact zero offset is usable.
        // Numeric TID lookup uses proc's top-level entry table, which
        // exposes timens_offsets; /proc/thread-self's task table does not.
        let offsets = if time_namespace.is_some() && time_namespace == time_for_children {
            read(&format!("/proc/{tid}/timens_offsets")).ok()
        } else {
            None
        };
        let zero_boottime_offset = usable_helper_boot_clock(
            time_namespace.as_deref(),
            time_for_children.as_deref(),
            offsets.as_deref(),
        );
        Ok(Self {
            version: 1,
            boot_id: boot_id.to_string(),
            pid_namespace,
            time_namespace,
            zero_boottime_offset,
            creator_start_ticks: helper_start_ticks(&read("/proc/self/stat")?)?,
        })
    }

    fn validate(&self, current: &Self) -> RtResult<()> {
        if self.version != 1
            || current.version != 1
            || self.boot_id != current.boot_id
            || self.pid_namespace != current.pid_namespace
            || self.time_namespace != current.time_namespace
            || self.zero_boottime_offset != current.zero_boottime_offset
            || self.creator_start_ticks == 0
            || self.creator_start_ticks > current.creator_start_ticks
            || (self.zero_boottime_offset && self.time_namespace.is_none())
        {
            return Err(rt_error(
                ErrorCode::PolicyUnenforceable,
                "slirp_birth_context_changed",
            ));
        }
        Ok(())
    }

    fn predates(&self, start_ticks: u64) -> bool {
        self.time_namespace.is_some()
            && self.zero_boottime_offset
            && start_ticks > 0
            && start_ticks < self.creator_start_ticks
    }
}

fn initialize_slirp_lifecycle(pidfile: &Path) -> RtResult<()> {
    let unavailable = || rt_error(ErrorCode::StorageUnavailable, "slirp_lifecycle");
    let parent = pidfile.parent().ok_or_else(unavailable)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let lock = options
        .open(pidfile.with_extension("pid.lock"))
        .map_err(|_| unavailable())?;
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(unavailable());
        }
    }
    // Never attach fresh birth evidence to a legacy/running artifact set.
    for path in [
        pidfile.to_path_buf(),
        pidfile.with_extension("pid.state"),
        pidfile.with_extension("pid.birth"),
    ] {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(unavailable()),
        }
    }
    let mut state = options
        .open(pidfile.with_extension("pid.state"))
        .map_err(|_| unavailable())?;
    state
        .write_all(b"preparing\n")
        .and_then(|()| state.sync_all())
        .map_err(|_| unavailable())?;
    #[cfg(target_os = "linux")]
    {
        let context = HelperBirthContext::capture()?;
        let body = serde_json::to_vec(&context).map_err(|_| unavailable())?;
        let mut birth = options
            .open(pidfile.with_extension("pid.birth"))
            .map_err(|_| unavailable())?;
        birth
            .write_all(&body)
            .and_then(|()| birth.sync_all())
            .map_err(|_| unavailable())?;
        crate::roundtable::feature_gate::fsync_dir(parent).map_err(|_| unavailable())?;
    }
    use std::io::{Seek, SeekFrom};
    state
        .seek(SeekFrom::Start(0))
        .and_then(|_| state.set_len(0))
        .and_then(|()| state.write_all(b"prepared\n"))
        .and_then(|()| state.sync_all())
        .map_err(|_| unavailable())?;
    lock.sync_all().map_err(|_| unavailable())?;
    crate::roundtable::feature_gate::fsync_dir(parent).map_err(|_| unavailable())
}

#[cfg(target_os = "linux")]
fn process_disappeared(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

#[cfg(target_os = "linux")]
fn helper_process_exited(fd: &std::os::fd::OwnedFd) -> bool {
    use std::os::fd::AsRawFd;
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut poll, 1, 0) };
    #[cfg(test)]
    {
        let errno = (result < 0)
            .then(|| std::io::Error::last_os_error().raw_os_error())
            .flatten();
        control_tests::record_exit_poll(fd.as_raw_fd(), result, poll.revents, errno);
    }
    result > 0 && poll.revents & libc::POLLIN != 0
}

#[cfg(target_os = "linux")]
fn retained_helper_is_live(retained: &BTreeMap<i32, std::os::fd::OwnedFd>, pid: i32) -> bool {
    use std::os::fd::AsRawFd;
    let Some(fd) = retained.get(&pid) else {
        #[cfg(test)]
        control_tests::record_retained_poll(pid, None);
        return false;
    };
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // Only a successful no-readiness poll proves this original identity is
    // still live. An exited, invalid or inconclusive fd cannot exempt a PID.
    let result = unsafe { libc::poll(&mut poll, 1, 0) };
    #[cfg(test)]
    {
        let errno = (result < 0)
            .then(|| std::io::Error::last_os_error().raw_os_error())
            .flatten();
        control_tests::record_retained_poll(pid, Some((result, poll.revents, errno)));
    }
    result == 0
}

#[cfg(target_os = "linux")]
fn resolve_helper_environment(
    fd: &std::os::fd::OwnedFd,
    result: std::io::Result<Vec<u8>>,
    age_proof: bool,
) -> RtResult<Option<Vec<u8>>> {
    match result {
        Ok(environment) => Ok(Some(environment)),
        Err(error) if process_disappeared(&error) || helper_process_exited(fd) => Ok(None),
        Err(error) => {
            // A zombie can deny environ even when its proc directory remains.
            // Only the identity pinned before this read can prove it exited.
            let reason = match (
                error.kind() == std::io::ErrorKind::PermissionDenied,
                age_proof,
            ) {
                (true, true) => "slirp_proc_environment_denied_within_scope",
                (true, false) => "slirp_proc_environment_denied_no_age_proof",
                (false, true) => "slirp_proc_environment_unreadable_within_scope",
                (false, false) => "slirp_proc_environment_unreadable_no_age_proof",
            };
            Err(rt_error(ErrorCode::PolicyUnenforceable, reason))
        }
    }
}

pub(super) fn stop_slirp(runtime_root: &Path, id: &str, expected: bool) -> RtResult<()> {
    if !expected {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::{Seek, SeekFrom};
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        fn unproven_at(reason: &'static str) -> roundtable_protocol::RtError {
            rt_error(ErrorCode::PolicyUnenforceable, reason)
        }
        fn unproven_syscall(
            operation: &'static str,
            errno: Option<i32>,
        ) -> roundtable_protocol::RtError {
            // Call sites supply fixed operation labels; only a numeric errno
            // is added, never a PID, path, environment, or syscall error text.
            let mut error = unproven_at(operation);
            error.details.reason = Some(match errno {
                Some(errno) => format!("{operation}_errno_{errno}"),
                None => format!("{operation}_errno_unknown"),
            });
            error
        }
        fn private_file(path: &Path, writable: bool) -> RtResult<fs::File> {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(writable)
                // A FIFO must not block before fstat can reject it.
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(path)
                .map_err(|_| unproven_at("slirp_evidence_file_unreadable"))?;
            let meta = file
                .metadata()
                .map_err(|_| unproven_at("slirp_evidence_metadata_unreadable"))?;
            if !meta.is_file()
                || meta.uid() != unsafe { libc::geteuid() }
                || meta.mode() & 0o077 != 0
            {
                return Err(unproven_at("slirp_evidence_ownership"));
            }
            Ok(file)
        }
        fn persist_phase(file: &mut fs::File, body: &[u8]) -> RtResult<()> {
            file.seek(SeekFrom::Start(0))
                .and_then(|_| file.set_len(0))
                .map_err(|_| unproven_at("slirp_lifecycle_state_write"))?;
            file.write_all(body)
                .and_then(|()| file.sync_all())
                .map_err(|_| unproven_at("slirp_lifecycle_state_write"))
        }
        fn pidfd(pid: i32, operation: &'static str) -> RtResult<Option<OwnedFd>> {
            if pid <= 1 {
                return Err(unproven_at("slirp_pidfd_invalid_pid"));
            }
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if raw >= 0 {
                return Ok(Some(unsafe { OwnedFd::from_raw_fd(raw as i32) }));
            }
            let errno = std::io::Error::last_os_error().raw_os_error();
            if errno == Some(libc::ESRCH) {
                Ok(None)
            } else {
                Err(unproven_syscall(operation, errno))
            }
        }
        fn marked(env: &[u8], runtime_root: &Path, id: &str, role: Option<&str>) -> bool {
            let owner = format!("CODEG_ROUNDTABLE_SLIRP_OWNER={id}");
            let root = format!("CODEG_ROUNDTABLE_SLIRP_ROOT={}", runtime_root.display());
            let has = |expected: &str| {
                env.split(|byte| *byte == 0)
                    .any(|value| value == expected.as_bytes())
            };
            has(&owner)
                && has(&root)
                && match role {
                    Some(role) => has(&format!("CODEG_ROUNDTABLE_SLIRP_ROLE={role}")),
                    None => ["hook", "watcher", "slirp"]
                        .iter()
                        .any(|role| has(&format!("CODEG_ROUNDTABLE_SLIRP_ROLE={role}"))),
                }
        }
        fn verify(
            fd: &OwnedFd,
            pid: i32,
            start: Option<u64>,
            runtime_root: &Path,
            id: &str,
            role: Option<&str>,
        ) -> RtResult<()> {
            if helper_process_exited(fd) {
                return Ok(());
            }
            let stage = |pin: &'static str, census: &'static str| {
                if role.is_some() {
                    pin
                } else {
                    census
                }
            };
            let inspect = || {
                let metadata = fs::metadata(format!("/proc/{pid}")).map_err(|error| {
                    unproven_syscall(
                        stage("slirp_pin_verify_metadata", "slirp_census_verify_metadata"),
                        error.raw_os_error(),
                    )
                })?;
                let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|error| {
                    unproven_syscall(
                        stage("slirp_pin_verify_stat", "slirp_census_verify_stat"),
                        error.raw_os_error(),
                    )
                })?;
                let observed = stat
                    .rsplit_once(") ")
                    .and_then(|(_, rest)| rest.split_whitespace().nth(19))
                    .and_then(|value| value.parse::<u64>().ok());
                let env = fs::read(format!("/proc/{pid}/environ")).map_err(|error| {
                    unproven_syscall(
                        stage(
                            "slirp_pin_verify_environment",
                            "slirp_census_verify_environment",
                        ),
                        error.raw_os_error(),
                    )
                })?;
                if metadata.uid() != unsafe { libc::geteuid() } {
                    return Err(unproven_at(stage(
                        "slirp_pin_verify_uid_mismatch",
                        "slirp_census_verify_uid_mismatch",
                    )));
                }
                if observed.is_none() {
                    return Err(unproven_at(stage(
                        "slirp_pin_verify_start_invalid",
                        "slirp_census_verify_start_invalid",
                    )));
                }
                if start.is_some_and(|expected| observed != Some(expected)) {
                    return Err(unproven_at(stage(
                        "slirp_pin_verify_start_mismatch",
                        "slirp_census_verify_start_mismatch",
                    )));
                }
                if !marked(&env, runtime_root, id, role) {
                    return Err(unproven_at(stage(
                        "slirp_pin_verify_marker_mismatch",
                        "slirp_census_verify_marker_mismatch",
                    )));
                }
                Ok(())
            };
            match inspect() {
                Err(_) if helper_process_exited(fd) => Ok(()),
                result => result,
            }
        }
        fn owned_helpers(
            runtime_root: &Path,
            id: &str,
            birth: Option<&HelperBirthContext>,
            retained: &BTreeMap<i32, OwnedFd>,
        ) -> (Vec<(i32, OwnedFd)>, RtResult<()>) {
            let mut found = Vec::new();
            let mut first_error = None;
            #[cfg(not(test))]
            let entries = fs::read_dir("/proc");
            #[cfg(test)]
            let entries = control_tests::helper_entries();
            let entries = match entries {
                Ok(entries) => entries,
                Err(_) => {
                    return (
                        found,
                        Err(unproven_at("slirp_proc_enumeration_unavailable")),
                    );
                }
            };
            for entry in entries {
                let inspect = || -> RtResult<Option<(i32, OwnedFd)>> {
                    let entry = entry.map_err(|_| unproven_at("slirp_proc_entry_unreadable"))?;
                    let Some(pid) = entry
                        .file_name()
                        .to_str()
                        .and_then(|name| name.parse::<i32>().ok())
                        // Host helpers are descendants of the launcher, never
                        // this proc namespace's init. PID 1 is never signalable.
                        .filter(|pid| *pid > 1)
                    else {
                        return Ok(None);
                    };
                    let retained_live = retained_helper_is_live(retained, pid);
                    #[cfg(test)]
                    let retained_live = retained_live && !control_tests::force_rediscovery();
                    if retained_live {
                        // Ownership is already pinned. Exec may hide or clear
                        // environ, but cannot replace this live identity. Its
                        // original fd remains the signal and exit authority.
                        return Ok(None);
                    }
                    let metadata = match entry.metadata() {
                        Ok(metadata) => metadata,
                        Err(error) if process_disappeared(&error) => return Ok(None),
                        Err(_) => return Err(unproven_at("slirp_proc_metadata_unreadable")),
                    };
                    if metadata.uid() != unsafe { libc::geteuid() } {
                        return Ok(None);
                    }
                    // Pin before reading environ, including for unreadable
                    // candidates. Numeric PID reuse cannot prove this read's
                    // identity exited or authorize signaling its replacement.
                    let Some(fd) = pidfd(pid, "slirp_census_pidfd_open")? else {
                        return Ok(None);
                    };
                    if let Some(birth) = birth.filter(|birth| birth.zero_boottime_offset) {
                        let stat = fs::read_to_string(entry.path().join("stat"));
                        if helper_process_exited(&fd) {
                            return Ok(None);
                        }
                        #[cfg(test)]
                        let parent_pid = stat.as_ref().ok().and_then(|stat| {
                            stat.rsplit_once(") ")
                                .and_then(|(_, rest)| rest.split_whitespace().nth(1))
                                .and_then(|value| value.parse::<i32>().ok())
                        });
                        let start = helper_start_ticks(
                            &stat.map_err(|_| unproven_at("slirp_proc_start_unreadable"))?,
                        )?;
                        #[cfg(test)]
                        control_tests::record_candidate_stat(
                            pid,
                            parent_pid,
                            start,
                            birth.creator_start_ticks,
                        );
                        // The pinned identity remained alive across the stat
                        // read, so the numeric path cannot name a replacement.
                        if birth.predates(start) {
                            return Ok(None);
                        }
                    }
                    let environment = fs::read(entry.path().join("environ"));
                    #[cfg(test)]
                    let read_errno = environment
                        .as_ref()
                        .err()
                        .and_then(|error| error.raw_os_error());
                    #[cfg(test)]
                    let environment = control_tests::helper_environment(pid, environment);
                    let environment = resolve_helper_environment(
                        &fd,
                        environment,
                        birth.is_some_and(|birth| birth.zero_boottime_offset),
                    );
                    #[cfg(test)]
                    if environment.is_err() {
                        control_tests::record_environment_denial(pid, fd.as_raw_fd(), read_errno);
                    }
                    let Some(environment) = environment? else {
                        return Ok(None);
                    };
                    if !marked(&environment, runtime_root, id, None) {
                        return Ok(None);
                    }
                    verify(&fd, pid, None, runtime_root, id, None)?;
                    Ok((!helper_process_exited(&fd)).then_some((pid, fd)))
                };
                match inspect() {
                    Ok(Some(identity)) => {
                        if found.len() >= 1024 {
                            first_error
                                .get_or_insert_with(|| unproven_at("slirp_discovery_limit"));
                            break;
                        }
                        found.push(identity);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        // Keep independently verified identities even if an
                        // unrelated process prevents a complete proof.
                        first_error.get_or_insert(error);
                    }
                }
            }
            (found, first_error.map_or(Ok(()), Err))
        }
        fn verify_pins(
            pidfile: &Path,
            runtime_root: &Path,
            id: &str,
            never_started: bool,
        ) -> RtResult<Vec<OwnedFd>> {
            if !pidfile
                .try_exists()
                .map_err(|_| unproven_at("slirp_pin_presence_unreadable"))?
            {
                return if never_started {
                    Ok(Vec::new())
                } else {
                    Err(unproven_at("slirp_pin_missing"))
                };
            }
            let mut body = String::new();
            private_file(pidfile, false)?
                .take(1025)
                .read_to_string(&mut body)
                .map_err(|_| unproven_at("slirp_pin_body_unreadable"))?;
            if body.len() > 1024 {
                return Err(unproven_at("slirp_pin_body_oversized"));
            }
            let mut roles = std::collections::BTreeSet::new();
            let mut roots = Vec::new();
            for line in body.lines() {
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields.len() != 3
                    || !matches!(fields[0], "slirp" | "watcher")
                    || !roles.insert(fields[0])
                {
                    return Err(unproven_at("slirp_pin_fields_invalid"));
                }
                let pid = fields[1]
                    .parse::<i32>()
                    .map_err(|_| unproven_at("slirp_pin_pid_invalid"))?;
                let start = fields[2]
                    .parse::<u64>()
                    .map_err(|_| unproven_at("slirp_pin_start_invalid"))?;
                if start == 0 {
                    return Err(unproven_at("slirp_pin_start_zero"));
                }
                if let Some(fd) = pidfd(pid, "slirp_pin_pidfd_open")? {
                    verify(&fd, pid, Some(start), runtime_root, id, Some(fields[0]))?;
                    roots.push(fd);
                }
            }
            if never_started {
                return Err(unproven_at("slirp_pin_phase_mismatch"));
            }
            if roles.len() != 2 {
                return Err(unproven_at("slirp_pin_roles_incomplete"));
            }
            Ok(roots)
        }

        let pidfile = runtime_root.join("slirp-pids").join(format!("{id}.pid"));
        let lock = private_file(&pidfile.with_extension("pid.lock"), true)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error().raw_os_error();
            if (error != Some(libc::EWOULDBLOCK) && error != Some(libc::EAGAIN))
                || std::time::Instant::now() >= deadline
            {
                let reason = if std::time::Instant::now() >= deadline {
                    "slirp_lifecycle_lock_timeout"
                } else {
                    "slirp_lifecycle_lock_unavailable"
                };
                return Err(unproven_at(reason));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut state = private_file(&pidfile.with_extension("pid.state"), true)?;
        let mut phase = String::new();
        (&mut state)
            .take(128)
            .read_to_string(&mut phase)
            .map_err(|_| unproven_at("slirp_lifecycle_state_unreadable"))?;
        let never_started = match phase.trim() {
            "preparing" => return Err(unproven_at("slirp_lifecycle_initializing")),
            "prepared" | "cleanup-proven-never-started" => true,
            "starting" | "running" | "cleanup-proven-started" => false,
            // Old cancelled states did not persist descendant obligations.
            // They are just as unproven as an interrupted new cleanup.
            "cancelled-never-started"
            | "cancelled-started"
            | "cleanup-in-progress-never-started"
            | "cleanup-in-progress-started" => {
                return Err(rt_error(
                    ErrorCode::PolicyUnenforceable,
                    "slirp_cleanup_interrupted",
                ));
            }
            _ => return Err(unproven_at("slirp_lifecycle_state_invalid")),
        };
        // Proc numbers and pidfds must refer to the same current namespace.
        // A missing/corrupt persisted record remains an error, never a new
        // birth cutoff inferred from this cleanup process.
        let current_context = HelperBirthContext::capture()?;
        let birth = (|| -> RtResult<HelperBirthContext> {
            let mut body = String::new();
            private_file(&pidfile.with_extension("pid.birth"), false)?
                .take(4097)
                .read_to_string(&mut body)
                .map_err(|_| unproven_at("slirp_birth_evidence_unreadable"))?;
            if body.len() > 4096 {
                return Err(unproven_at("slirp_birth_evidence_invalid"));
            }
            let value: Value = serde_json::from_str(&body)
                .map_err(|_| unproven_at("slirp_birth_evidence_invalid"))?;
            // An explicit null means unsupported. Omission is incomplete
            // evidence, not permission to infer the current clock domain.
            if value.get("time_namespace").is_none() {
                return Err(unproven_at("slirp_birth_evidence_invalid"));
            }
            let evidence: HelperBirthContext = serde_json::from_str(&body)
                .map_err(|_| unproven_at("slirp_birth_evidence_invalid"))?;
            evidence.validate(&current_context)?;
            Ok(evidence)
        })();
        // Persist quarantine before the first signal can make a process
        // erase its markers. Only this lock-owning invocation can discharge
        // its in-memory pidfd obligations and publish a successful proof.
        persist_phase(
            &mut state,
            if never_started {
                b"cleanup-in-progress-never-started\n"
            } else {
                b"cleanup-in-progress-started\n"
            },
        )?;
        // Invalid/missing pins after startup cannot prove cleanup, but safely
        // stop any independently owner-marked survivors before reporting it.
        let pin_proof = verify_pins(&pidfile, runtime_root, id, never_started);
        let began = std::time::Instant::now();
        let mut empty_observations = 0;
        let mut retained: BTreeMap<i32, OwnedFd> = BTreeMap::new();
        let (birth, mut discovery_error) = match birth {
            Ok(birth) => (Some(birth), None),
            Err(error) => (None, Some(error)),
        };
        loop {
            #[cfg(test)]
            control_tests::before_cleanup_sweep()?;
            let (found, discovery) = owned_helpers(runtime_root, id, birth.as_ref(), &retained);
            if let Err(error) = discovery {
                discovery_error.get_or_insert(error);
            }
            for (pid, fd) in found {
                match retained.entry(pid) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(fd);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        // A PID can be reused only after the old pinned
                        // process exits. Never replace a still-live identity.
                        if helper_process_exited(entry.get()) {
                            entry.insert(fd);
                        }
                    }
                }
            }
            retained.retain(|_, fd| !helper_process_exited(fd));
            // Bound handles by distinct live processes, not sweep count.
            if retained.len() > 1024 {
                return Err(unproven_at("slirp_retained_limit"));
            }
            let roots_exited = pin_proof
                .as_ref()
                .map(|roots| roots.iter().all(helper_process_exited))
                .unwrap_or(true);
            if retained.is_empty() && roots_exited {
                empty_observations += 1;
                if empty_observations >= 2 {
                    // Known children being gone cannot discharge a failed
                    // discovery interval. Keep its durable quarantine.
                    if let Some(error) = discovery_error {
                        return Err(error);
                    }
                    drop(pin_proof?);
                    persist_phase(
                        &mut state,
                        if never_started {
                            b"cleanup-proven-never-started\n"
                        } else {
                            b"cleanup-proven-started\n"
                        },
                    )?;
                    return Ok(());
                }
            } else {
                empty_observations = 0;
                // Every verified descendant stays owned until its pinned
                // process exits, even after exec clears its environment.
                for fd in retained
                    .values()
                    .chain(pin_proof.as_ref().ok().into_iter().flatten())
                {
                    let signal = if began.elapsed() < Duration::from_millis(200) {
                        libc::SIGTERM
                    } else {
                        libc::SIGKILL
                    };
                    let result = unsafe {
                        libc::syscall(
                            libc::SYS_pidfd_send_signal,
                            fd.as_raw_fd(),
                            signal,
                            std::ptr::null::<libc::siginfo_t>(),
                            0,
                        )
                    };
                    if result < 0 {
                        let errno = std::io::Error::last_os_error().raw_os_error();
                        if errno != Some(libc::ESRCH) {
                            return Err(unproven_syscall("slirp_pidfd_send_signal", errno));
                        }
                    }
                }
            }
            if began.elapsed() > Duration::from_secs(3) {
                return Err(
                    discovery_error.unwrap_or_else(|| unproven_at("slirp_cleanup_deadline")),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (runtime_root, id);
        Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "slirp_cleanup_unproven",
        ))
    }
}

pub(super) fn cgroup_delegation_error(cgroup_root: &Path) -> Option<String> {
    if !cgroup_root.join("cgroup.controllers").is_file()
        || !cgroup_root.join("cgroup.events").is_file()
    {
        return Some(format!(
            "cgroup v2 root {} is missing cgroup.controllers or cgroup.events",
            cgroup_root.display()
        ));
    }
    let subtree =
        fs::read_to_string(cgroup_root.join("cgroup.subtree_control")).unwrap_or_default();
    let enabled: Vec<&str> = subtree.split_whitespace().collect();
    for controller in ["memory", "pids", "cpu"] {
        if !enabled.contains(&controller) {
            return Some(format!(
                "cgroup {} subtree_control lacks {controller}. With no processes in that directory, run: echo '+memory +pids +cpu' | sudo tee {}/cgroup.subtree_control && sudo mkdir -p {}/launcher && sudo chown -R \"$USER:$USER\" {} && echo $$ | sudo tee {}/launcher/cgroup.procs",
                cgroup_root.display(),
                cgroup_root.display(),
                cgroup_root.display(),
                cgroup_root.display(),
                cgroup_root.display()
            ));
        }
    }
    let relative = fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("0::").map(str::to_string))
        });
    let Some(relative) = relative else {
        return Some("could not read /proc/self/cgroup".to_string());
    };
    let Ok(root_rel) = cgroup_root.strip_prefix("/sys/fs/cgroup") else {
        return Some("cgroup root must be under /sys/fs/cgroup".to_string());
    };
    let expected = format!("/{}", root_rel.to_string_lossy()).replace("//", "/");
    let expected = expected.trim_end_matches('/');
    if relative.starts_with(&format!("{expected}/")) {
        return None;
    }
    let launcher = cgroup_root.join("launcher");
    if fs::create_dir_all(&launcher).is_err() {
        return Some(cgroup_move_hint(cgroup_root));
    }
    let pid = std::process::id().to_string();
    if fs::write(launcher.join("cgroup.procs"), &pid).is_err() {
        return Some(cgroup_move_hint(cgroup_root));
    }
    let moved = fs::read_to_string("/proc/self/cgroup")
        .ok()
        .is_some_and(|text| {
            text.lines().any(|line| {
                line.strip_prefix("0::")
                    .is_some_and(|path| path.starts_with(&format!("{expected}/")))
            })
        });
    if moved {
        None
    } else {
        Some(cgroup_move_hint(cgroup_root))
    }
}

fn cgroup_move_hint(cgroup_root: &Path) -> String {
    format!(
        "crun cannot create a container cgroup unless this process already lives in a child of {} (for example {}/launcher). Run: sudo mkdir -p {}/launcher && echo $$ | sudo tee {}/launcher/cgroup.procs",
        cgroup_root.display(),
        cgroup_root.display(),
        cgroup_root.display(),
        cgroup_root.display()
    )
}

fn container_env_allowed(key: &str, value: &str) -> bool {
    match key {
        "HOME" => value == "/rt-home",
        "PATH" => value == "/usr/local/bin:/usr/bin:/bin",
        "GEMINI_HOME" => value == "/rt-home/.gemini",
        "CURSOR_CONFIG_DIR" => value == "/rt-home/.cursor",
        "XDG_CONFIG_HOME" => value == "/rt-home/.config",
        _ => false,
    }
}

fn auth_destination_allowed(destination: &str) -> bool {
    destination.starts_with("/rt-home/")
        && !destination.contains("..")
        && !destination.ends_with('/')
}

fn reject_baked_credential(profile: &QualifiedOciProfile, mount: &PlanMount) -> RtResult<()> {
    let root = profile
        .rootfs
        .canonicalize()
        .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "rootfs_unreadable"))?;
    let source = mount
        .source
        .canonicalize()
        .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "auth_mount"))?;
    if source.starts_with(&root) {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "credential_baked_into_image",
        ));
    }
    let target = profile
        .rootfs
        .join(mount.destination.trim_start_matches('/'));
    let metadata = fs::symlink_metadata(&target)
        .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "rootfs_mount_target"))?;
    if metadata.len() != 0 {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "credential_baked_into_image",
        ));
    }
    Ok(())
}

pub(super) fn verify_profile(plan: &SandboxPlan, profile: &QualifiedOciProfile) -> RtResult<()> {
    validate_document(plan)?;
    if !profile.runtime_root.is_absolute()
        || !profile.cgroup_root.is_absolute()
        || !profile.rootfs.is_absolute()
        || profile.runtime.role != "crun"
        || Path::new(&profile.runtime.absolute_path) != plan.runtime_path
        || profile.runtime.sha256 != plan.runtime_sha256
        || file_hash(&plan.runtime_path)? != plan.runtime_sha256
        || rootfs_digest(&profile.rootfs)? != profile.rootfs_sha256
        || plan.oci["root"]["path"] != json!(profile.rootfs)
    {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "qualification_component_drift",
        ));
    }
    let cli = binary(&plan.allowed_binaries, "cli")?;
    let mut args = vec![cli.absolute_path.clone()];
    args.extend(profile.cli_args.clone());
    let id = runtime_id(&plan.db, plan.boot_epoch.0, &plan.incarnation.to_string());
    if plan.oci["process"]["args"] != json!(args)
        || plan.oci["linux"]["cgroupsPath"] != json!(oci_cgroup_path(profile, &id)?)
        || plan.oci["linux"]["seccomp"] != seccomp_json()
        || plan.oci["annotations"]["io.codeg.roundtable.network"] != SLIRP_NETWORK
        || plan.oci["hooks"][SLIRP_HOOK_PHASE]
            .as_array()
            .is_none_or(|hooks| hooks.is_empty())
    {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "qualification_plan_drift",
        ));
    }
    for mount in &plan.mounts {
        let allowed = match mount.destination.as_str() {
            "/proc" => mount.source == Path::new("proc") && mount.read_only,
            "/dev" => mount.source == Path::new("tmpfs") && !mount.read_only,
            "/dev/pts" => mount.source == Path::new("devpts") && !mount.read_only,
            "/dev/shm" => mount.source == Path::new("shm") && !mount.read_only,
            "/sys" => mount.source == Path::new("sysfs") && mount.read_only,
            "/tmp" => mount.source == Path::new("tmpfs") && !mount.read_only,
            "/scratch" => {
                !mount.read_only
                    && owned_scratch(&mount.source, profile, &plan.incarnation.to_string())?
                        == mount.source
            }
            "/rt-home" => home_upper_allowed(plan, mount),
            "/run/codeg/roundtable.sock" => {
                profile.service_socket.as_ref() == Some(&mount.source) && mount.read_only
            }
            "/run/codeg/gateway.sock" => {
                profile.gateway_socket.as_ref() == Some(&mount.source) && mount.read_only
            }
            "/etc/resolv.conf" => {
                mount.read_only
                    && mount.source == slirp_resolv_path(&profile.runtime_root)
                    && fs::read(&mount.source).ok().as_deref() == Some(SLIRP_RESOLV_BODY)
            }
            destination if destination.starts_with("/rt-home/") => {
                mount.read_only
                    && profile
                        .auth_mounts
                        .iter()
                        .any(|item| item.destination == destination && item.source == mount.source)
            }
            _ => false,
        };
        if !allowed {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "qualification_mount_drift",
            ));
        }
        if mount.destination == "/scratch" && canonical_directory(&mount.source)? != mount.source {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "scratch_path_drift",
            ));
        }
        // Mount targets are part of the frozen image. Do not let crun create
        // placeholders in a shared rootfs and invalidate later launches.
        let target = profile
            .rootfs
            .join(mount.destination.trim_start_matches('/'));
        let metadata = fs::symlink_metadata(&target)
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "rootfs_mount_target"))?;
        let file_mount = mount.destination.starts_with("/run/codeg/")
            || mount.destination.starts_with("/rt-home/")
            || mount.destination == "/etc/resolv.conf";
        if metadata.file_type().is_symlink()
            || (file_mount && !metadata.is_file())
            || (!file_mount && !metadata.is_dir())
        {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "rootfs_mount_target",
            ));
        }
        if mount.destination.starts_with("/rt-home/") {
            reject_baked_credential(profile, mount)?;
        }
    }
    for required in [
        "/proc",
        "/dev",
        "/dev/pts",
        "/dev/shm",
        "/sys",
        "/tmp",
        "/scratch",
        "/rt-home",
        "/etc/resolv.conf",
    ] {
        if !plan
            .mounts
            .iter()
            .any(|mount| mount.destination == required)
        {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "qualification_mount_drift",
            ));
        }
    }
    for (key, value) in &profile.container_env {
        if !container_env_allowed(key, value)
            || plan.env.get(key).map(String::as_str) != Some(value)
        {
            return Err(rt_error(ErrorCode::CapabilityUnqualified, "container_env"));
        }
    }
    verify_container_binaries(profile, &plan.allowed_binaries)?;
    Ok(())
}

fn verify_container_binaries(
    profile: &QualifiedOciProfile,
    binaries: &[CertifiedBinary],
) -> RtResult<()> {
    for binary in binaries.iter().filter(|binary| binary.role != "crun") {
        if !binary.absolute_path.starts_with('/') {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "container_binary_path",
            ));
        }
        let path = profile
            .rootfs
            .join(binary.absolute_path.trim_start_matches('/'));
        if !path
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "binary_unreadable"))?
            .starts_with(
                profile
                    .rootfs
                    .canonicalize()
                    .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "rootfs_unreadable"))?,
            )
            || file_hash(&path)? != binary.sha256
        {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "qualification_component_drift",
            ));
        }
    }
    Ok(())
}

pub(super) fn verify_installed_profile(
    profile: &QualifiedOciProfile,
    key: &QualificationKey,
) -> RtResult<()> {
    if profile_hash(profile, key)? != key.plan_hash
        || binary(&key.binaries, "crun")? != &profile.runtime
        || !profile.rootfs.is_absolute()
        || !profile.runtime_root.is_absolute()
        || rootfs_digest(&profile.rootfs)? != profile.rootfs_sha256
    {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "qualification_component_drift",
        ));
    }
    verify_runtime(profile)?;
    verify_container_binaries(profile, &key.binaries)?;
    for (path, directory) in [
        ("proc", true),
        ("scratch", true),
        ("run/codeg/roundtable.sock", false),
        ("run/codeg/gateway.sock", false),
        ("etc/resolv.conf", false),
    ] {
        let metadata = fs::symlink_metadata(profile.rootfs.join(path))
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "rootfs_mount_target"))?;
        if metadata.file_type().is_symlink()
            || if directory {
                !metadata.is_dir()
            } else {
                !metadata.is_file()
            }
        {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "rootfs_mount_target",
            ));
        }
    }
    for mount in &profile.auth_mounts {
        if !auth_destination_allowed(&mount.destination) || !mount.source.is_absolute() {
            return Err(rt_error(ErrorCode::CapabilityUnqualified, "auth_mount"));
        }
        let target = profile
            .rootfs
            .join(mount.destination.trim_start_matches('/'));
        let metadata = fs::symlink_metadata(&target)
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "rootfs_mount_target"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != 0 {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "credential_baked_into_image",
            ));
        }
        let root = profile
            .rootfs
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "rootfs_unreadable"))?;
        let source = mount
            .source
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::CapabilityUnqualified, "auth_mount"))?;
        if source.starts_with(&root) || !source.is_file() {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "credential_baked_into_image",
            ));
        }
    }
    for (key, value) in &profile.container_env {
        if !container_env_allowed(key, value) {
            return Err(rt_error(ErrorCode::CapabilityUnqualified, "container_env"));
        }
    }
    Ok(())
}

fn runtime_id(db: &DbIdentity, boot: u64, incarnation: &str) -> String {
    format!("{}{}-{}", db_prefix(db), boot, incarnation)
}
fn oci_cgroup_path(profile: &QualifiedOciProfile, id: &str) -> RtResult<PathBuf> {
    let relative = profile
        .cgroup_root
        .strip_prefix("/sys/fs/cgroup")
        .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"))?;
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"));
    }
    Ok(Path::new("/").join(relative).join(id))
}
fn db_prefix(db: &DbIdentity) -> String {
    format!(
        "codeg-rt-{}-",
        &Hash256::sha256(db.canonical().as_bytes()).to_hex()[..24]
    )
}
pub(super) fn verify_intent_instance(
    intent: &LaunchIntent,
    instance: &SandboxInstance,
) -> RtResult<()> {
    if instance.runtime_id
        != runtime_id(
            &intent.db,
            intent.boot_epoch.0,
            &intent.incarnation.to_string(),
        )
    {
        return Err(rt_error(ErrorCode::PolicyUnenforceable, "instance_unowned"));
    }
    Ok(())
}
/// Recover the deterministic OS locator when the spawn acknowledgement was lost.
/// Only `reap` can prove that this owner no longer has processes.
pub(super) fn recorded_intent_instance(
    profile: &QualifiedOciProfile,
    intent: &LaunchIntent,
) -> SandboxInstance {
    let id = runtime_id(
        &intent.db,
        intent.boot_epoch.0,
        &intent.incarnation.to_string(),
    );
    SandboxInstance {
        runtime_id: id.clone(),
        incarnation: intent.incarnation,
        boot_epoch: intent.boot_epoch,
        image_digest: intent.image_digest.clone(),
        cgroup_handle: profile.cgroup_root.join(&id).to_string_lossy().into_owned(),
        owner_label: intent.owner_label.clone(),
    }
}
fn instance(plan: &SandboxPlan, profile: &QualifiedOciProfile) -> SandboxInstance {
    let id = runtime_id(&plan.db, plan.boot_epoch.0, &plan.incarnation.to_string());
    SandboxInstance {
        runtime_id: id.clone(),
        incarnation: plan.incarnation,
        boot_epoch: plan.boot_epoch,
        image_digest: plan.image_digest.clone(),
        cgroup_handle: profile.cgroup_root.join(&id).to_string_lossy().into_owned(),
        owner_label: plan.owner_label.clone(),
    }
}
fn platform() -> RtResult<()> {
    if !cfg!(target_os = "linux") {
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "policy_unenforceable",
        ));
    }
    Ok(())
}
fn verify_runtime(profile: &QualifiedOciProfile) -> RtResult<()> {
    platform()?;
    verify_cgroup_root(&profile.cgroup_root)?;
    if !Path::new(&profile.runtime.absolute_path).is_absolute()
        || profile.runtime.role != "crun"
        || file_hash(Path::new(&profile.runtime.absolute_path))? != profile.runtime.sha256
    {
        return Err(rt_error(ErrorCode::CapabilityUnqualified, "runtime_drift"));
    }
    Ok(())
}

fn verify_cgroup_root(path: &Path) -> RtResult<()> {
    let actual = path
        .canonicalize()
        .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"))?;
    if actual != path
        || !actual.starts_with("/sys/fs/cgroup")
        || !actual.join("cgroup.controllers").is_file()
        || !actual.join("cgroup.events").is_file()
    {
        return Err(rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"));
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(actual.as_os_str().as_bytes())
            .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"))?;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0
            || unsafe { stat.assume_init() }.f_type != 0x63677270
        {
            return Err(rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"));
        }
    }
    Ok(())
}
fn secure_directory(path: &Path) -> RtResult<()> {
    if !path.is_absolute() {
        return Err(rt_error(ErrorCode::InvalidArgument, "runtime_root"));
    }
    fs::create_dir_all(path).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "oci_bundle"))?;
    if fs::symlink_metadata(path)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "oci_bundle"))?
        .file_type()
        .is_symlink()
    {
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "runtime_root_symlink",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "oci_bundle"))?;
    }
    Ok(())
}
fn command_args(profile: &QualifiedOciProfile) -> Vec<String> {
    vec![
        "--root".into(),
        profile
            .runtime_root
            .join("state")
            .to_string_lossy()
            .into_owned(),
        "--cgroup-manager".into(),
        "cgroupfs".into(),
    ]
}
pub(super) fn launch_artifacts_present(
    profile: &QualifiedOciProfile,
    intent: &LaunchIntent,
) -> RtResult<bool> {
    let instance = recorded_intent_instance(profile, intent);
    for path in [
        profile
            .runtime_root
            .join("bundles")
            .join(&instance.runtime_id)
            .join("config.json"),
        profile
            .runtime_root
            .join("state")
            .join(&instance.runtime_id),
        profile
            .runtime_root
            .join("slirp-pids")
            .join(format!("{}.pid", instance.runtime_id)),
        profile.cgroup_root.join(&instance.runtime_id),
    ] {
        if path
            .try_exists()
            .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "instance_unowned"))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) struct PendingLaunch {
    instance: SandboxInstance,
    runtime_path: PathBuf,
    args: Vec<String>,
    bundle: PathBuf,
}

pub(super) fn prepare_spawn(
    plan: &SandboxPlan,
    profile: &QualifiedOciProfile,
) -> RtResult<PendingLaunch> {
    verify_runtime(profile)?;
    if let Some(reason) = cgroup_delegation_error(&profile.cgroup_root) {
        tracing::warn!(%reason, "roundtable cgroup delegation");
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "cgroup_delegation",
        ));
    }
    // IsolationProvider::prepare just checked current binary/rootfs pins on
    // the blocking pool. This step only launches the already verified plan.
    let instance = instance(plan, profile);
    secure_directory(&profile.runtime_root)?;
    secure_directory(&profile.runtime_root.join("state"))?;
    let bundle = profile
        .runtime_root
        .join("bundles")
        .join(&instance.runtime_id);
    secure_directory(&bundle)?;
    let config = bundle.join("config.json");
    if config.exists() {
        return Err(rt_error(ErrorCode::InvalidState, "incarnation_reused"));
    }
    crate::roundtable::feature_gate::atomic_write(
        &config,
        &serde_json::to_vec(&plan.oci)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "oci_encode"))?,
    )
    .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "oci_bundle"))?;
    let mut args = command_args(profile);
    args.extend([
        "run".into(),
        "--bundle".into(),
        bundle.to_string_lossy().into_owned(),
        instance.runtime_id.clone(),
    ]);
    Ok(PendingLaunch {
        instance,
        runtime_path: plan.runtime_path.clone(),
        args,
        bundle,
    })
}

pub(super) fn spawn_prepared(
    launch: PendingLaunch,
) -> RtResult<(SandboxInstance, tokio::process::Child)> {
    let child = crate::acp::agent_process::spawn_attached_roundtable_oci(
        &launch.runtime_path,
        &launch.args,
        &launch.bundle,
    )?;
    Ok((launch.instance, child))
}

async fn control(profile: &QualifiedOciProfile, tail: &[&str]) -> RtResult<std::process::Output> {
    verify_runtime(profile)?;
    secure_directory(&profile.runtime_root)?;
    secure_directory(&profile.runtime_root.join("state"))?;
    let mut args = command_args(profile);
    args.extend(tail.iter().map(|arg| (*arg).to_owned()));
    let mut child = crate::acp::agent_process::spawn_attached_roundtable_oci(
        Path::new(&profile.runtime.absolute_path),
        &args,
        &profile.runtime_root,
    )?;
    drop(child.stdin.take());
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "runtime_control"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "runtime_control"))?;
    let (stdout, stderr, status) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(
            bounded_control_read(stdout),
            bounded_control_read(stderr),
            async {
                child
                    .wait()
                    .await
                    .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "runtime_control"))
            }
        )
    })
    .await
    .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "runtime_control_timeout"))??;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

async fn bounded_control_read(reader: impl tokio::io::AsyncRead + Unpin) -> RtResult<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    reader
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "runtime_control"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "runtime_control_bytes",
        ));
    }
    Ok(bytes)
}
async fn list(profile: &QualifiedOciProfile) -> RtResult<Vec<Value>> {
    let output = control(profile, &["list", "--format", "json"]).await?;
    if !output.status.success() {
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "enumeration_unproven",
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "enumeration_unproven"))
}
fn verify_instance(profile: &QualifiedOciProfile, instance: &SandboxInstance) -> RtResult<()> {
    if instance.runtime_id.contains('/')
        || instance.runtime_id.contains('\\')
        || !instance.runtime_id.starts_with("codeg-rt-")
        || instance.cgroup_handle
            != profile
                .cgroup_root
                .join(&instance.runtime_id)
                .to_string_lossy()
                .as_ref()
    {
        return Err(rt_error(ErrorCode::PolicyUnenforceable, "instance_unowned"));
    }
    let path = profile
        .runtime_root
        .join("bundles")
        .join(&instance.runtime_id)
        .join("config.json");
    let body: Value = serde_json::from_slice(
        &fs::read(path)
            .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "instance_unowned"))?,
    )
    .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "instance_unowned"))?;
    if body["annotations"]["io.codeg.roundtable.label"] != instance.owner_label
        || body["annotations"]["io.codeg.roundtable.image"] != instance.image_digest
        || body["linux"]["cgroupsPath"] != json!(oci_cgroup_path(profile, &instance.runtime_id)?)
    {
        return Err(rt_error(ErrorCode::PolicyUnenforceable, "instance_unowned"));
    }
    Ok(())
}
fn cgroup_empty(path: &Path) -> RtResult<bool> {
    if !path
        .try_exists()
        .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"))?
    {
        return Ok(true);
    }
    if fs::symlink_metadata(path)
        .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"))?
        .file_type()
        .is_symlink()
    {
        return Err(rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"));
    }
    let events = fs::read_to_string(path.join("cgroup.events"))
        .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"))?;
    let processes = fs::read_to_string(path.join("cgroup.procs"))
        .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "cgroup_unproven"))?;
    Ok(events.lines().any(|line| line == "populated 0") && processes.trim().is_empty())
}

// A crun launcher may still be between exec and OCI state registration. Its
// pinned executable, private --root and incarnation ID must also disappear;
// a missing container list entry alone cannot prove that interval empty.
fn launcher_present(profile: &QualifiedOciProfile, id: &str) -> RtResult<bool> {
    platform()?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let operator = unsafe { libc::geteuid() };
        let expected_root = profile
            .runtime_root
            .join("state")
            .to_string_lossy()
            .into_owned();
        let runtime = Path::new(&profile.runtime.absolute_path)
            .canonicalize()
            .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "enumeration_unproven"))?;
        for entry in fs::read_dir("/proc")
            .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "enumeration_unproven"))?
        {
            let entry = entry
                .map_err(|_| rt_error(ErrorCode::PolicyUnenforceable, "enumeration_unproven"))?;
            if !entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
            {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err(rt_error(
                        ErrorCode::PolicyUnenforceable,
                        "enumeration_unproven",
                    ))
                }
            };
            if metadata.uid() != operator {
                continue;
            }
            let executable = match fs::read_link(entry.path().join("exe")) {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err(rt_error(
                        ErrorCode::PolicyUnenforceable,
                        "enumeration_unproven",
                    ))
                }
            };
            if executable != runtime {
                continue;
            }
            let bytes = match fs::read(entry.path().join("cmdline")) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err(rt_error(
                        ErrorCode::PolicyUnenforceable,
                        "enumeration_unproven",
                    ))
                }
            };
            let args = bytes
                .split(|byte| *byte == 0)
                .filter_map(|arg| std::str::from_utf8(arg).ok())
                .collect::<Vec<_>>();
            if args.contains(&id)
                && args
                    .windows(2)
                    .any(|pair| pair == ["--root", expected_root.as_str()])
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (profile, id);
        Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "enumeration_unproven",
        ))
    }
}
pub(super) async fn discover_owned(
    profile: &QualifiedOciProfile,
    db: &DbIdentity,
    intents: &[LaunchIntent],
) -> RtResult<Vec<SandboxInstance>> {
    let listed = list(profile).await?;
    let mut found = Vec::new();
    for entry in listed {
        let id = entry["id"]
            .as_str()
            .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "enumeration_unproven"))?;
        if !id.starts_with(&db_prefix(db)) {
            continue;
        }
        let intent = intents
            .iter()
            .find(|intent| {
                intent.db == *db
                    && runtime_id(db, intent.boot_epoch.0, &intent.incarnation.to_string()) == id
            })
            .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "unknown_owned_process"))?;
        let current = SandboxInstance {
            runtime_id: id.into(),
            incarnation: intent.incarnation,
            boot_epoch: intent.boot_epoch,
            image_digest: intent.image_digest.clone(),
            cgroup_handle: profile.cgroup_root.join(id).to_string_lossy().into_owned(),
            owner_label: intent.owner_label.clone(),
        };
        verify_instance(profile, &current)?;
        found.push(current);
    }
    for intent in intents.iter().filter(|intent| intent.db == *db) {
        let id = runtime_id(db, intent.boot_epoch.0, &intent.incarnation.to_string());
        if !found.iter().any(|instance| instance.runtime_id == id)
            && (!cgroup_empty(&profile.cgroup_root.join(&id))? || launcher_present(profile, &id)?)
        {
            let current = SandboxInstance {
                runtime_id: id.clone(),
                incarnation: intent.incarnation,
                boot_epoch: intent.boot_epoch,
                image_digest: intent.image_digest.clone(),
                cgroup_handle: profile.cgroup_root.join(&id).to_string_lossy().into_owned(),
                owner_label: intent.owner_label.clone(),
            };
            verify_instance(profile, &current)?;
            found.push(current);
        }
    }
    Ok(found)
}
pub(super) async fn reap(
    runtime: &Path,
    profile: &QualifiedOciProfile,
    instance: &SandboxInstance,
) -> RtResult<ProcessTreeProof> {
    if runtime != Path::new(&profile.runtime.absolute_path) {
        return Err(rt_error(ErrorCode::PolicyUnenforceable, "runtime_drift"));
    }
    verify_runtime(profile)?;
    verify_instance(profile, instance)?;
    let initial_helper_cleanup = stop_slirp(&profile.runtime_root, &instance.runtime_id, true);
    let exists = list(profile)
        .await?
        .iter()
        .any(|entry| entry["id"] == instance.runtime_id);
    if exists {
        let _ = control(profile, &["kill", "--all", &instance.runtime_id, "KILL"]).await?;
        let deleted = control(profile, &["delete", "--force", &instance.runtime_id]).await?;
        if !deleted.status.success() {
            return Err(rt_error(ErrorCode::PolicyUnenforceable, "cleanup_unproven"));
        }
    }
    for _ in 0..30 {
        if cgroup_empty(Path::new(&instance.cgroup_handle))?
            && !list(profile)
                .await?
                .iter()
                .any(|entry| entry["id"] == instance.runtime_id)
            && !launcher_present(profile, &instance.runtime_id)?
        {
            let final_helper_cleanup =
                stop_slirp(&profile.runtime_root, &instance.runtime_id, true);
            initial_helper_cleanup.and(final_helper_cleanup)?;
            return Ok(ProcessTreeProof {
                instance_id: instance.runtime_id.clone(),
                incarnation: instance.incarnation,
                process_tree_empty: true,
            });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(rt_error(ErrorCode::PolicyUnenforceable, "cleanup_unproven"))
}

#[cfg(test)]
mod control_tests {
    // Component-test inputs only. This module and its inventory overrides
    // are absent from production and test-utils integration library builds.
    #[cfg(target_os = "linux")]
    struct InventoryState {
        pids: Vec<i32>,
        force_rediscovery: bool,
    }

    #[cfg(target_os = "linux")]
    std::thread_local! {
        static CONTROLLED_INVENTORY: std::cell::RefCell<Option<InventoryState>> = const { std::cell::RefCell::new(None) };
    }

    #[cfg(target_os = "linux")]
    struct ControlledInventory {
        previous_denial: Option<i32>,
        _thread_local: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    #[cfg(target_os = "linux")]
    impl ControlledInventory {
        fn start(pids: &[u32], force_rediscovery: bool) -> Self {
            assert!(pids.iter().all(|pid| *pid > 1 && *pid <= i32::MAX as u32));
            let unique: std::collections::BTreeSet<_> = pids.iter().copied().collect();
            assert_eq!(
                unique.len(),
                pids.len(),
                "inventory must be explicit and unique"
            );
            CONTROLLED_INVENTORY.with(|state| {
                let mut state = state.borrow_mut();
                assert!(state.is_none(), "inventory scopes cannot overlap");
                *state = Some(InventoryState {
                    pids: pids.iter().map(|pid| *pid as i32).collect(),
                    force_rediscovery,
                });
            });
            Self {
                previous_denial: DENIED_ENVIRONMENT_PID.with(|denied| denied.get()),
                _thread_local: std::marker::PhantomData,
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for ControlledInventory {
        fn drop(&mut self) {
            CONTROLLED_INVENTORY.with(|state| *state.borrow_mut() = None);
            DENIED_ENVIRONMENT_PID.with(|denied| denied.set(self.previous_denial));
        }
    }

    #[cfg(target_os = "linux")]
    struct EnvironmentDenialGuard {
        previous: Option<i32>,
        _thread_local: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    #[cfg(target_os = "linux")]
    impl EnvironmentDenialGuard {
        fn start(pid: i32) -> Self {
            Self {
                previous: DENIED_ENVIRONMENT_PID.with(|denied| denied.replace(Some(pid))),
                _thread_local: std::marker::PhantomData,
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for EnvironmentDenialGuard {
        fn drop(&mut self) {
            DENIED_ENVIRONMENT_PID.with(|denied| denied.set(self.previous));
        }
    }

    #[cfg(target_os = "linux")]
    fn candidate_inventory() -> Option<Vec<i32>> {
        CONTROLLED_INVENTORY.with(|state| state.borrow().as_ref().map(|state| state.pids.clone()))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn force_rediscovery() -> bool {
        CONTROLLED_INVENTORY.with(|state| {
            state
                .borrow()
                .as_ref()
                .is_some_and(|state| state.force_rediscovery)
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) enum InventoryEntry {
        Host(std::fs::DirEntry),
        Fixture(i32),
    }

    #[cfg(target_os = "linux")]
    impl InventoryEntry {
        pub(super) fn file_name(&self) -> std::ffi::OsString {
            match self {
                Self::Host(entry) => entry.file_name(),
                Self::Fixture(pid) => pid.to_string().into(),
            }
        }

        pub(super) fn path(&self) -> std::path::PathBuf {
            match self {
                Self::Host(entry) => entry.path(),
                Self::Fixture(pid) => std::path::PathBuf::from(format!("/proc/{pid}")),
            }
        }

        pub(super) fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
            match self {
                Self::Host(entry) => entry.metadata(),
                Self::Fixture(_) => std::fs::symlink_metadata(self.path()),
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) type ProcessEntries = Box<dyn Iterator<Item = std::io::Result<InventoryEntry>>>;

    #[cfg(target_os = "linux")]
    pub(super) fn helper_entries() -> std::io::Result<ProcessEntries> {
        if let Some(pids) = candidate_inventory() {
            return Ok(Box::new(
                pids.into_iter().map(|pid| Ok(InventoryEntry::Fixture(pid))),
            ));
        }
        Ok(Box::new(
            std::fs::read_dir("/proc")?.map(|entry| entry.map(InventoryEntry::Host)),
        ))
    }

    #[cfg(target_os = "linux")]
    struct ReapedTestChild(std::process::Child);

    #[cfg(target_os = "linux")]
    impl std::ops::Deref for ReapedTestChild {
        type Target = std::process::Child;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    #[cfg(target_os = "linux")]
    impl std::ops::DerefMut for ReapedTestChild {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for ReapedTestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) type PollDiagnostic = (i32, i16, Option<i32>);

    #[cfg(target_os = "linux")]
    #[derive(Clone, Copy, Debug)]
    enum RetainedPollObservation {
        NotObserved,
        NotRetained,
        Polled {
            result: i32,
            revents: i16,
            errno: Option<i32>,
        },
    }

    #[cfg(target_os = "linux")]
    #[derive(Clone, Copy, Debug)]
    struct CandidateStatDiagnostic {
        pid: i32,
        parent_pid: Option<i32>,
        start_ticks: u64,
        creator_start_ticks: u64,
    }

    #[cfg(target_os = "linux")]
    #[derive(Debug)]
    struct CleanupDenialDiagnostic {
        sweep: u32,
        pid: i32,
        fixture_role: &'static str,
        parent_fixture_role: Option<&'static str>,
        stat: Option<CandidateStatDiagnostic>,
        retained: RetainedPollObservation,
        exit_poll: Option<PollDiagnostic>,
        read_errno: Option<i32>,
        injected: bool,
    }

    #[cfg(target_os = "linux")]
    #[derive(Debug)]
    struct CleanupDiagnostics {
        fixture_pids: [(&'static str, i32); 3],
        sweep: u32,
        denials: Vec<CleanupDenialDiagnostic>,
        omitted_denials: u32,
        last_retained: Option<(i32, RetainedPollObservation)>,
        last_exit: Option<(i32, PollDiagnostic)>,
        last_stat: Option<CandidateStatDiagnostic>,
    }

    #[cfg(target_os = "linux")]
    std::thread_local! {
        static CLEANUP_DIAGNOSTICS: std::cell::RefCell<Option<CleanupDiagnostics>> = const { std::cell::RefCell::new(None) };
    }

    #[cfg(target_os = "linux")]
    struct CleanupDiagnosticCapture;

    #[cfg(target_os = "linux")]
    impl CleanupDiagnosticCapture {
        fn start(fixture_pids: [(&'static str, i32); 3]) -> Self {
            CLEANUP_DIAGNOSTICS.with(|state| {
                *state.borrow_mut() = Some(CleanupDiagnostics {
                    fixture_pids,
                    sweep: 0,
                    denials: Vec::new(),
                    omitted_denials: 0,
                    last_retained: None,
                    last_exit: None,
                    last_stat: None,
                });
            });
            Self
        }

        fn finish(self) -> CleanupDiagnostics {
            CLEANUP_DIAGNOSTICS.with(|state| {
                let mut snapshot = state.borrow_mut().take().expect("diagnostics armed");
                // Only per-denial observations are meaningful in the report.
                snapshot.last_retained = None;
                snapshot.last_exit = None;
                snapshot.last_stat = None;
                snapshot
            })
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for CleanupDiagnosticCapture {
        fn drop(&mut self) {
            CLEANUP_DIAGNOSTICS.with(|state| *state.borrow_mut() = None);
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn record_retained_poll(pid: i32, observed: Option<PollDiagnostic>) {
        CLEANUP_DIAGNOSTICS.with(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                let poll = match observed {
                    Some((result, revents, errno)) => RetainedPollObservation::Polled {
                        result,
                        revents,
                        errno,
                    },
                    None => RetainedPollObservation::NotRetained,
                };
                state.last_retained = Some((pid, poll));
                state.last_exit = None;
                state.last_stat = None;
            }
        });
    }

    #[cfg(target_os = "linux")]
    pub(super) fn record_candidate_stat(
        pid: i32,
        parent_pid: Option<i32>,
        start_ticks: u64,
        creator_start_ticks: u64,
    ) {
        CLEANUP_DIAGNOSTICS.with(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                state.last_stat = Some(CandidateStatDiagnostic {
                    pid,
                    parent_pid,
                    start_ticks,
                    creator_start_ticks,
                });
            }
        });
    }

    #[cfg(target_os = "linux")]
    pub(super) fn record_exit_poll(fd: i32, result: i32, revents: i16, errno: Option<i32>) {
        CLEANUP_DIAGNOSTICS.with(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                state.last_exit = Some((fd, (result, revents, errno)));
            }
        });
    }

    #[cfg(target_os = "linux")]
    pub(super) fn record_environment_denial(pid: i32, fd: i32, read_errno: Option<i32>) {
        CLEANUP_DIAGNOSTICS.with(|state| {
            let mut state = state.borrow_mut();
            let Some(state) = state.as_mut() else {
                return;
            };
            if state.denials.len() == 16 {
                state.omitted_denials = state.omitted_denials.saturating_add(1);
                return;
            }
            let role = |pid| {
                state
                    .fixture_pids
                    .iter()
                    .find_map(|(role, fixture_pid)| (*fixture_pid == pid).then_some(*role))
                    .unwrap_or("other")
            };
            let stat = state.last_stat.filter(|stat| stat.pid == pid);
            let fixture_role = role(pid);
            let parent_fixture_role = stat.and_then(|stat| stat.parent_pid).map(role);
            state.denials.push(CleanupDenialDiagnostic {
                sweep: state.sweep,
                pid,
                fixture_role,
                parent_fixture_role,
                stat,
                retained: state
                    .last_retained
                    .filter(|(last_pid, _)| *last_pid == pid)
                    .map(|(_, poll)| poll)
                    .unwrap_or(RetainedPollObservation::NotObserved),
                exit_poll: state
                    .last_exit
                    .filter(|(last_fd, _)| *last_fd == fd)
                    .map(|(_, poll)| poll),
                read_errno,
                injected: DENIED_ENVIRONMENT_PID.with(|denied| denied.get() == Some(pid)),
            });
        });
    }

    #[cfg(target_os = "linux")]
    type SweepObserver = Box<dyn FnMut() -> roundtable_protocol::RtResult<()>>;

    #[cfg(target_os = "linux")]
    std::thread_local! {
        static SWEEP_OBSERVER: std::cell::RefCell<Option<SweepObserver>> = const { std::cell::RefCell::new(None) };
    }

    #[cfg(target_os = "linux")]
    pub(super) fn before_cleanup_sweep() -> roundtable_protocol::RtResult<()> {
        CLEANUP_DIAGNOSTICS.with(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                state.sweep = state.sweep.saturating_add(1);
                state.last_retained = None;
                state.last_exit = None;
                state.last_stat = None;
            }
        });
        SWEEP_OBSERVER.with(|observer| match observer.borrow_mut().as_mut() {
            Some(callback) => callback(),
            None => Ok(()),
        })
    }

    #[cfg(target_os = "linux")]
    struct SweepObserverGuard;

    #[cfg(target_os = "linux")]
    impl Drop for SweepObserverGuard {
        fn drop(&mut self) {
            SWEEP_OBSERVER.with(|observer| *observer.borrow_mut() = None);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controlled_inventory_is_explicit_thread_local_and_restored_on_unwind() {
        assert!(candidate_inventory().is_none());
        assert!(!force_rediscovery());
        let fault = std::panic::catch_unwind(|| {
            let _scope = ControlledInventory::start(&[10, 11, 99], true);
            DENIED_ENVIRONMENT_PID.with(|denied| denied.set(Some(99)));
            let entries: Vec<_> = helper_entries()
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(entries, ["10", "11", "99"].map(std::ffi::OsString::from));
            assert!(force_rediscovery());
            std::thread::spawn(|| {
                assert!(candidate_inventory().is_none());
                assert!(!force_rediscovery());
            })
            .join()
            .unwrap();
            panic!("controlled inventory unwind");
        });
        assert!(fault.is_err());
        assert!(candidate_inventory().is_none());
        assert!(!force_rediscovery());
        DENIED_ENVIRONMENT_PID.with(|denied| assert_eq!(denied.get(), None));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controlled_inventory_unknown_live_candidate_still_quarantines_cleanup() {
        exercise_unreadable_live_candidate(true);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controlled_forced_rediscovery_is_a_fail_closed_mutation_control() {
        exercise_retained_unreadability(true);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_diagnostics_are_bounded_and_distinguish_fixture_and_unknown_identity() {
        let capture =
            CleanupDiagnosticCapture::start([("helper", 10), ("watcher", 11), ("descendant", 12)]);
        before_cleanup_sweep().unwrap();
        record_retained_poll(12, Some((-1, 0, Some(libc::EINTR))));
        record_exit_poll(7, 0, 0, None);
        record_candidate_stat(12, Some(11), 30, 20);
        DENIED_ENVIRONMENT_PID.with(|denied| denied.set(Some(12)));
        record_environment_denial(12, 7, None);
        DENIED_ENVIRONMENT_PID.with(|denied| denied.set(None));
        record_retained_poll(99, None);
        // Even a reused fd number must not borrow the previous PID's data.
        record_environment_denial(99, 7, Some(libc::EACCES));
        record_exit_poll(8, 0, 0, None);
        record_candidate_stat(99, Some(77), 40, 20);
        for _ in 0..20 {
            record_environment_denial(99, 8, Some(libc::EACCES));
        }
        let diagnostics = capture.finish();
        assert_eq!(diagnostics.denials.len(), 16);
        assert_eq!(diagnostics.omitted_denials, 6);
        let target = &diagnostics.denials[0];
        assert_eq!(target.fixture_role, "descendant");
        assert_eq!(target.pid, 12);
        assert_eq!(target.parent_fixture_role, Some("watcher"));
        assert_eq!(target.stat.unwrap().start_ticks, 30);
        assert_eq!(target.stat.unwrap().creator_start_ticks, 20);
        assert_eq!(target.sweep, 1);
        assert!(target.injected);
        assert_eq!(target.read_errno, None);
        assert!(matches!(
            target.retained,
            RetainedPollObservation::Polled {
                result: -1,
                revents: 0,
                errno: Some(libc::EINTR),
            }
        ));
        assert_eq!(target.exit_poll, Some((0, 0, None)));
        let reset = &diagnostics.denials[1];
        assert!(reset.stat.is_none());
        assert!(reset.exit_poll.is_none());
        assert!(reset.parent_fixture_role.is_none());
        let unknown = &diagnostics.denials[2];
        assert_eq!(unknown.fixture_role, "other");
        assert_eq!(unknown.pid, 99);
        assert_eq!(unknown.parent_fixture_role, Some("other"));
        assert!(!unknown.injected);
        assert_eq!(unknown.read_errno, Some(libc::EACCES));
        assert!(matches!(
            unknown.retained,
            RetainedPollObservation::NotRetained
        ));
        CLEANUP_DIAGNOSTICS.with(|state| assert!(state.borrow().is_none()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_namespace_status_requires_one_matching_level_for_process_and_thread() {
        let valid = "Name:\tfake\nNStgid:\t500\nNSpid:\t501\nNSpgid:\t500\n";
        super::validate_proc_namespace_status(valid, 500, 501).unwrap();
        for status in [
            "NStgid: 500 500\nNSpid: 501 501\n",
            "NStgid: 900 500\nNSpid: 901 501\n",
            "NStgid: 500\nNSpid: 501 501\n",
            "NStgid: 500 500\nNSpid: 501\n",
            "NStgid: 500\nNSpid: 501\nNSpid: 501\n",
            "NStgid: 500\nNStgid: 500\nNSpid: 501\n",
            "NStgid: 501\nNSpid: 501\n",
            "NStgid: 500\nNSpid: 500\n",
            "NStgid: 500\n",
            "NSpid: 501\n",
            "NStgid: 500\nNSpid:\n",
            "NStgid: +500\nNSpid: 501\n",
            "NStgid: 500\nNSpid: -501\n",
            "NStgid: 500\nNSpid: 4294967296\n",
            "NStgid: 500\nNSpid: 501x\n",
            "NStgid: 500\nNSpid 501\nNSpid: 501\n",
            " NStgid: 500\nNSpid: 501\n",
            "NStgid: 500\n NSpid: 501\nNSpid: 501\n",
        ] {
            let error = super::validate_proc_namespace_status(status, 500, 501).unwrap_err();
            assert_eq!(
                error.details.reason.as_deref(),
                Some("slirp_proc_namespace_domain")
            );
        }
        assert!(super::validate_proc_namespace_status("NStgid: 0\nNSpid: 0\n", 0, 0).is_err());
        let oversized = format!("{valid}{}", "x".repeat(4097));
        assert!(super::validate_proc_namespace_status(&oversized, 500, 501).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn helper_birth_exclusion_is_strict_and_keeps_the_original_context() {
        let mut original = super::HelperBirthContext::capture().unwrap();
        original.creator_start_ticks = 100;
        original.time_namespace = Some("time:[123]".into());
        original.zero_boottime_offset = true;
        let mut recovered = original.clone();
        recovered.creator_start_ticks = 200;
        original.validate(&recovered).unwrap();
        assert!(original.predates(99));
        assert!(!original.predates(100));
        assert!(!original.predates(101));
        assert_eq!(
            original.creator_start_ticks, 100,
            "recovery cannot renew the bound"
        );
        let mut unavailable = original.clone();
        unavailable.time_namespace = None;
        assert!(
            !unavailable.predates(99),
            "unknown clock domain cannot exclude a live process"
        );
        let mut nonzero = original.clone();
        nonzero.zero_boottime_offset = false;
        assert!(
            !nonzero.predates(99),
            "shifted boot clock cannot exclude a process"
        );
        assert!(super::zero_boottime_offset("monotonic 0 0\nboottime 0 0\n"));
        assert!(super::usable_helper_boot_clock(
            Some("time:[123]"),
            Some("time:[123]"),
            Some("boottime 0 0"),
        ));
        assert!(!super::usable_helper_boot_clock(
            Some("time:[123]"),
            Some("time:[456]"),
            Some("boottime 0 0"),
        ));
        assert!(!super::usable_helper_boot_clock(
            Some("time:[123]"),
            Some("time:[123]"),
            None,
        ));
        assert!(!super::usable_helper_boot_clock(
            None,
            None,
            Some("boottime 0 0"),
        ));
        for offsets in [
            "boottime -100 0",
            "boottime 1 0",
            "boottime 0 1",
            "",
            "boottime 0 0\nboottime 0 0",
        ] {
            assert!(!super::zero_boottime_offset(offsets), "{offsets}");
        }
        for field in ["boot", "pid", "time", "offset", "future", "version"] {
            let mut changed = recovered.clone();
            match field {
                "boot" => changed.boot_id = uuid::Uuid::new_v4().to_string(),
                "pid" => changed.pid_namespace.push('x'),
                "time" => changed.time_namespace = Some("time:[456]".into()),
                "offset" => changed.zero_boottime_offset = false,
                "future" => changed.creator_start_ticks = 99,
                _ => changed.version += 1,
            }
            assert!(original.validate(&changed).is_err(), "{field}");
        }
        assert!(super::validate_proc_number_space("17", 17).is_ok());
        assert!(super::validate_proc_number_space("17", 18).is_err());
        assert!(super::validate_proc_number_space("self", 17).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn helper_birth_is_persisted_once_before_prepared_and_rejects_old_artifacts() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let pin = helper_fixture(root.path(), "birth-order", "prepared\n");
        let birth = pin.with_extension("pid.birth");
        let bytes = std::fs::read(&birth).unwrap();
        let evidence: super::HelperBirthContext = serde_json::from_slice(&bytes).unwrap();
        evidence
            .validate(&super::HelperBirthContext::capture().unwrap())
            .unwrap();
        assert_eq!(std::fs::metadata(&birth).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::read_to_string(pin.with_extension("pid.state")).unwrap(),
            "prepared\n"
        );
        assert!(super::initialize_slirp_lifecycle(&pin).is_err());
        assert_eq!(std::fs::read(&birth).unwrap(), bytes);
        for suffix in ["pid", "pid.state", "pid.birth"] {
            let path = root.path().join(format!("old-{suffix}.pid"));
            let artifact = if suffix == "pid" {
                path.clone()
            } else {
                path.with_extension(suffix)
            };
            std::fs::write(&artifact, b"old artifact").unwrap();
            assert!(
                super::initialize_slirp_lifecycle(&path).is_err(),
                "{suffix}"
            );
            assert_eq!(std::fs::read(&artifact).unwrap(), b"old artifact");
            if suffix != "pid.birth" {
                assert!(!path.with_extension("pid.birth").exists());
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_or_mismatched_birth_evidence_cannot_be_upgraded_during_cleanup() {
        for kind in [
            "missing",
            "malformed",
            "missing-time",
            "duplicate",
            "boot",
            "pid",
            "time",
        ] {
            let root = tempfile::tempdir().unwrap();
            let pin = helper_fixture(root.path(), kind, "prepared\n");
            let birth = pin.with_extension("pid.birth");
            let mut evidence: super::HelperBirthContext =
                serde_json::from_slice(&std::fs::read(&birth).unwrap()).unwrap();
            match kind {
                "missing" => std::fs::remove_file(&birth).unwrap(),
                "malformed" => std::fs::write(&birth, b"{}").unwrap(),
                "duplicate" => {
                    let body = serde_json::to_string(&evidence).unwrap();
                    std::fs::write(&birth, format!("{{\"version\":1,{}", &body[1..])).unwrap();
                }
                "missing-time" => {
                    let mut value = serde_json::to_value(&evidence).unwrap();
                    value.as_object_mut().unwrap().remove("time_namespace");
                    std::fs::write(&birth, serde_json::to_vec(&value).unwrap()).unwrap();
                }
                _ => {
                    match kind {
                        "boot" => evidence.boot_id = uuid::Uuid::new_v4().to_string(),
                        "pid" => evidence.pid_namespace.push('x'),
                        _ => evidence.time_namespace = Some("time:[123]".into()),
                    }
                    std::fs::write(&birth, serde_json::to_vec(&evidence).unwrap()).unwrap();
                }
            }
            let before = std::fs::read(&birth).ok();
            assert!(
                super::stop_slirp(root.path(), kind, true).is_err(),
                "{kind}"
            );
            assert_eq!(std::fs::read(&birth).ok(), before, "{kind}");
            assert_ne!(
                std::fs::read_to_string(pin.with_extension("pid.state")).unwrap(),
                "cleanup-proven-never-started\n"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn helper_birth_context_subprocess_fixture() {
        let Some(root) = std::env::var_os("CODEG_TEST_HELPER_BIRTH_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let action = std::env::var("CODEG_TEST_HELPER_BIRTH_ACTION").unwrap();
        if action == "prepare" {
            helper_fixture(&root, "restored-birth", "prepared\n");
        } else {
            let denied = std::env::var("CODEG_TEST_HELPER_DENIED_PID")
                .unwrap()
                .parse()
                .unwrap();
            let inventory: Vec<u32> = std::env::var("CODEG_TEST_HELPER_INVENTORY_PIDS")
                .unwrap()
                .split(',')
                .map(|pid| pid.parse().unwrap())
                .collect();
            let _inventory = ControlledInventory::start(&inventory, false);
            let denial = EnvironmentDenialGuard::start(denied);
            let result = super::stop_slirp(&root, "restored-birth", true);
            drop(denial);
            let reason = result
                .map(|()| "passed".to_string())
                .unwrap_or_else(|error| error.details.reason.unwrap());
            std::fs::write(root.join("cleanup-result"), reason).unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controlled_persisted_birth_excludes_only_an_older_unreadable_child_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut foreign = ReapedTestChild(
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        let foreign_start = super::helper_start_ticks(
            &std::fs::read_to_string(format!("/proc/{}/stat", foreign.id())).unwrap(),
        )
        .unwrap();
        // Cross at least two kernel userspace ticks before creating the
        // initializer process. No synthetic birth evidence is written.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        assert!(ticks > 0);
        std::thread::sleep(std::time::Duration::from_nanos(
            2_000_000_000u64.div_ceil(ticks as u64),
        ));
        let run = |action, denied_pid: u32, inventory: &[u32]| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "roundtable::sandbox::linux_oci::control_tests::helper_birth_context_subprocess_fixture",
                    "--nocapture",
                ])
                .env("CODEG_TEST_HELPER_BIRTH_ROOT", root.path())
                .env("CODEG_TEST_HELPER_BIRTH_ACTION", action)
                .env("CODEG_TEST_HELPER_DENIED_PID", denied_pid.to_string())
                .env(
                    "CODEG_TEST_HELPER_INVENTORY_PIDS",
                    inventory
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                )
                .output()
                .unwrap()
        };
        let prepared = run("prepare", foreign.id(), &[foreign.id()]);
        let birth_path = root.path().join("slirp-pids/restored-birth.pid.birth");
        let before = std::fs::read(&birth_path);
        let cleaned = run("cleanup", foreign.id(), &[foreign.id()]);
        let outcome = std::fs::read_to_string(root.path().join("cleanup-result"));
        let mut newer = ReapedTestChild(
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        let newer_start = super::helper_start_ticks(
            &std::fs::read_to_string(format!("/proc/{}/stat", newer.id())).unwrap(),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_nanos(
            2_000_000_000u64.div_ceil(ticks as u64),
        ));
        // A second recovery process is newer than this candidate. It must
        // still use the original creator's bound, never its own birth time.
        let retried = run("cleanup", newer.id(), &[foreign.id(), newer.id()]);
        let retry_outcome = std::fs::read_to_string(root.path().join("cleanup-result"));
        let after = std::fs::read(&birth_path);
        let survived = foreign.try_wait().unwrap().is_none();
        let newer_survived = newer.try_wait().unwrap().is_none();
        for child in [&mut foreign, &mut newer] {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(
            prepared.status.success(),
            "{}",
            String::from_utf8_lossy(&prepared.stderr)
        );
        assert!(
            cleaned.status.success(),
            "{}",
            String::from_utf8_lossy(&cleaned.stderr)
        );
        assert!(
            retried.status.success(),
            "{}",
            String::from_utf8_lossy(&retried.stderr)
        );
        let before = before.unwrap();
        let birth: super::HelperBirthContext = serde_json::from_slice(&before).unwrap();
        assert!(foreign_start < birth.creator_start_ticks);
        assert!(newer_start >= birth.creator_start_ticks);
        assert_eq!(after.unwrap(), before, "restart cannot refresh provenance");
        let expected = if birth.zero_boottime_offset {
            "passed"
        } else {
            "slirp_proc_environment_denied_no_age_proof"
        };
        assert_eq!(outcome.unwrap(), expected);
        assert!(
            survived && newer_survived,
            "age exclusion cannot authorize a foreign signal"
        );
        let expected_retry = if birth.zero_boottime_offset {
            "slirp_proc_environment_denied_within_scope"
        } else {
            // The first attempt already quarantined the unsupported domain.
            "slirp_cleanup_interrupted"
        };
        assert_eq!(retry_outcome.unwrap(), expected_retry);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_disappearance_does_not_hide_permission_or_live_read_errors() {
        for code in [libc::ENOENT, libc::ESRCH] {
            let error = std::io::Error::from_raw_os_error(code);
            assert!(super::process_disappeared(&error), "errno {code}");
        }
        for code in [libc::EACCES, libc::EPERM, libc::EIO, libc::EAGAIN] {
            let error = std::io::Error::from_raw_os_error(code);
            assert!(!super::process_disappeared(&error), "errno {code}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn denied_environment_is_gone_only_after_pidfd_proves_exit() {
        use std::os::fd::FromRawFd;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id() as i32, 0) };
        assert!(raw >= 0);
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw as i32) };
        let denied = || Err(std::io::Error::from_raw_os_error(libc::EACCES));
        let live = super::resolve_helper_environment(&fd, denied(), false);
        let scoped_live = super::resolve_helper_environment(&fd, denied(), true);
        let was_alive = child.try_wait().unwrap().is_none();
        child.kill().unwrap();
        // Keep the child unreaped: a zombie's numeric PID still exists.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let dead = loop {
            let observed = super::resolve_helper_environment(&fd, denied(), false);
            if observed.is_ok() || std::time::Instant::now() >= deadline {
                break observed;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        child.wait().unwrap();
        assert!(was_alive);
        assert_eq!(
            live.unwrap_err().details.reason.as_deref(),
            Some("slirp_proc_environment_denied_no_age_proof")
        );
        assert_eq!(
            scoped_live.unwrap_err().details.reason.as_deref(),
            Some("slirp_proc_environment_denied_within_scope")
        );
        assert_eq!(dead.unwrap(), None);
    }

    #[cfg(target_os = "linux")]
    std::thread_local! {
        static DENIED_ENVIRONMENT_PID: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
    }

    #[cfg(target_os = "linux")]
    pub(super) fn helper_environment(
        pid: i32,
        result: std::io::Result<Vec<u8>>,
    ) -> std::io::Result<Vec<u8>> {
        if DENIED_ENVIRONMENT_PID.with(|denied| denied.get() == Some(pid)) {
            Err(std::io::Error::from_raw_os_error(libc::EACCES))
        } else {
            result
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unreadable_live_process_quarantines_proof_but_does_not_prevent_owned_termination() {
        assert!(
            candidate_inventory().is_none(),
            "this test uses full host discovery"
        );
        exercise_unreadable_live_candidate(false);
    }

    #[cfg(target_os = "linux")]
    fn exercise_unreadable_live_candidate(controlled: bool) {
        let root = tempfile::tempdir().unwrap();
        let id = "fake-denied-discovery";
        let pin = helper_fixture(root.path(), id, "running\n");
        let mut helper = ReapedTestChild(fake_helper(root.path(), id, "slirp"));
        let mut watcher = ReapedTestChild(fake_helper(root.path(), id, "watcher"));
        let mut descendant = ReapedTestChild(fake_helper(root.path(), id, "watcher"));
        let mut foreign = ReapedTestChild(
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        write_pins(
            &pin,
            &(pin_line("slirp", helper.id()) + &pin_line("watcher", watcher.id())),
        );
        let _inventory = controlled.then(|| {
            ControlledInventory::start(
                &[helper.id(), watcher.id(), descendant.id(), foreign.id()],
                false,
            )
        });
        let denial = EnvironmentDenialGuard::start(foreign.id() as i32);
        let result = super::stop_slirp(root.path(), id, true);
        drop(denial);
        let owned_ended = [&mut helper, &mut watcher, &mut descendant]
            .into_iter()
            .all(|child| child.try_wait().unwrap().is_some());
        let foreign_survived = foreign.try_wait().unwrap().is_none();
        let phase = std::fs::read_to_string(pin.with_extension("pid.state")).unwrap();
        let retry = super::stop_slirp(root.path(), id, true);
        for child in [&mut helper, &mut watcher, &mut descendant, &mut foreign] {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert_eq!(
            result.unwrap_err().details.reason.as_deref(),
            Some(
                if super::HelperBirthContext::capture()
                    .unwrap()
                    .zero_boottime_offset
                {
                    "slirp_proc_environment_denied_within_scope"
                } else {
                    "slirp_proc_environment_denied_no_age_proof"
                }
            )
        );
        assert!(owned_ended, "verified helpers must still be terminated");
        assert!(
            foreign_survived,
            "unreadable identity cannot authorize a signal"
        );
        assert_eq!(phase, "cleanup-in-progress-started\n");
        assert_eq!(
            retry.unwrap_err().details.reason.as_deref(),
            Some("slirp_cleanup_interrupted")
        );
    }

    #[cfg(target_os = "linux")]
    fn helper_fixture(root: &std::path::Path, id: &str, phase: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = root.join("slirp-pids");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let pin = dir.join(format!("{id}.pid"));
        super::initialize_slirp_lifecycle(&pin).unwrap();
        std::fs::write(pin.with_extension("pid.state"), phase).unwrap();
        pin
    }

    #[cfg(target_os = "linux")]
    fn fake_helper(root: &std::path::Path, id: &str, role: &str) -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("30")
            .env("CODEG_ROUNDTABLE_SLIRP_OWNER", id)
            .env("CODEG_ROUNDTABLE_SLIRP_ROOT", root)
            .env("CODEG_ROUNDTABLE_SLIRP_ROLE", role)
            .spawn()
            .unwrap()
    }

    #[cfg(target_os = "linux")]
    fn pin_line(role: &str, pid: u32) -> String {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let start = stat
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap();
        format!("{role} {pid} {start}\n")
    }

    #[cfg(target_os = "linux")]
    fn write_pins(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn configured_missing_pin_cannot_prove_cleanup_even_after_owned_survivor_is_stopped() {
        let root = tempfile::tempdir().unwrap();
        let id = "fake-missing-pin";
        helper_fixture(root.path(), id, "running\n");
        let mut helper = fake_helper(root.path(), id, "slirp");
        let result = super::stop_slirp(root.path(), id, true);
        let ended = helper.try_wait().unwrap().is_some();
        let _ = helper.kill();
        let _ = helper.wait();
        assert!(
            result.is_err(),
            "missing configured-helper pin is not proof"
        );
        assert!(
            ended,
            "independently owner-marked helper should still be stopped"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controlled_helper_cleanup_waits_for_both_pinned_helper_and_watcher() {
        use std::os::fd::FromRawFd;
        let root = tempfile::tempdir().unwrap();
        let id = "fake-complete-tree";
        let pin = helper_fixture(root.path(), id, "running\n");
        let mut helper = ReapedTestChild(fake_helper(root.path(), id, "slirp"));
        let mut watcher = ReapedTestChild(fake_helper(root.path(), id, "watcher"));
        // Descendants inherit the watcher marker but do not get a root pin.
        let mut descendant = ReapedTestChild(fake_helper(root.path(), id, "watcher"));
        write_pins(
            &pin,
            &(pin_line("slirp", helper.id()) + &pin_line("watcher", watcher.id())),
        );
        let _inventory =
            ControlledInventory::start(&[helper.id(), watcher.id(), descendant.id()], false);
        let identities = [helper.id(), watcher.id(), descendant.id()].map(|pid| {
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as i32, 0) };
            assert!(raw >= 0);
            unsafe { std::os::fd::OwnedFd::from_raw_fd(raw as i32) }
        });
        let result = super::stop_slirp(root.path(), id, true);
        // Preserve first-call evidence without reaping the child PIDs. A
        // retry must not repair the first call's termination or state proof.
        let first_exited = identities.iter().all(super::helper_process_exited);
        let first_phase = std::fs::read_to_string(pin.with_extension("pid.state"));
        let retry = super::stop_slirp(root.path(), id, true);
        let helper_ended = helper.try_wait().unwrap().is_some();
        let watcher_ended = watcher.try_wait().unwrap().is_some();
        let descendant_ended = descendant.try_wait().unwrap().is_some();
        let _ = helper.kill();
        let _ = watcher.kill();
        let _ = descendant.kill();
        let _ = helper.wait();
        let _ = watcher.wait();
        let _ = descendant.wait();
        result.unwrap();
        assert!(
            first_exited,
            "first cleanup must terminate every fixture identity"
        );
        assert_eq!(first_phase.unwrap(), "cleanup-proven-started\n");
        assert!(helper_ended && watcher_ended && descendant_ended);
        assert_eq!(
            std::fs::read_to_string(pin.with_extension("pid.state")).unwrap(),
            "cleanup-proven-started\n"
        );
        // Retained pins and the closed startup gate support safe retries.
        retry.unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controlled_retained_live_identity_survives_unreadable_environment_on_later_sweeps() {
        exercise_retained_unreadability(false);
    }

    #[cfg(target_os = "linux")]
    fn exercise_retained_unreadability(force_rediscovery: bool) {
        use std::os::unix::process::ExitStatusExt;
        let root = tempfile::tempdir().unwrap();
        let id = "fake-retained-unreadable";
        let pin = helper_fixture(root.path(), id, "running\n");
        let mut helper = ReapedTestChild(fake_helper(root.path(), id, "slirp"));
        let mut watcher = ReapedTestChild(fake_helper(root.path(), id, "watcher"));
        let ready_path = root.path().join("retained-ready");
        let mut descendant = ReapedTestChild(
            std::process::Command::new("sh")
                .args([
                    "-c",
                    "trap '' TERM; : > \"$1\"; while :; do :; done",
                    "retained-helper",
                ])
                .arg(&ready_path)
                .env("CODEG_ROUNDTABLE_SLIRP_OWNER", id)
                .env("CODEG_ROUNDTABLE_SLIRP_ROOT", root.path())
                .env("CODEG_ROUNDTABLE_SLIRP_ROLE", "watcher")
                .spawn()
                .unwrap(),
        );
        let pid = descendant.id() as i32;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let ready = loop {
            let ready = ready_path.exists();
            if ready || std::time::Instant::now() >= deadline {
                break ready;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        write_pins(
            &pin,
            &(pin_line("slirp", helper.id()) + &pin_line("watcher", watcher.id())),
        );
        let sweeps = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = sweeps.clone();
        SWEEP_OBSERVER.with(|observer| {
            *observer.borrow_mut() = Some(Box::new(move || {
                observed.set(observed.get() + 1);
                if observed.get() >= 2 {
                    // First sweep verified and retained this marked identity.
                    // A subsequent proc read denial must not discard ownership.
                    DENIED_ENVIRONMENT_PID.with(|denied| denied.set(Some(pid)));
                }
                Ok(())
            }));
        });
        let observer = SweepObserverGuard;
        let _inventory = ControlledInventory::start(
            &[helper.id(), watcher.id(), descendant.id()],
            force_rediscovery,
        );
        let capture = CleanupDiagnosticCapture::start([
            ("helper", helper.id() as i32),
            ("watcher", watcher.id() as i32),
            ("descendant", descendant.id() as i32),
        ]);
        let result = super::stop_slirp(root.path(), id, true);
        let diagnostics = capture.finish();
        drop(observer);
        DENIED_ENVIRONMENT_PID.with(|denied| denied.set(None));
        let ended = descendant.try_wait().unwrap();
        for child in [&mut helper, &mut watcher, &mut descendant] {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(ready, "TERM-ignore must be installed before cleanup starts");
        assert!(
            sweeps.get() >= 2,
            "the denial must follow initial retention"
        );
        assert!(ended.is_some_and(|status| status.signal() == Some(libc::SIGKILL)));
        let phase = std::fs::read_to_string(pin.with_extension("pid.state")).unwrap();
        if force_rediscovery {
            let error = result.expect_err("forcing old rediscovery must fail closed");
            let expected = if super::HelperBirthContext::capture()
                .unwrap()
                .zero_boottime_offset
            {
                "slirp_proc_environment_denied_within_scope"
            } else {
                "slirp_proc_environment_denied_no_age_proof"
            };
            assert_eq!(error.details.reason.as_deref(), Some(expected));
            let first = diagnostics
                .denials
                .first()
                .expect("injected denial recorded");
            assert_eq!(first.pid, pid);
            assert!(first.injected);
            assert_eq!(
                first.read_errno, None,
                "the real controlled read must succeed"
            );
            assert!(matches!(
                first.retained,
                RetainedPollObservation::Polled { result: 0, .. }
            ));
            assert_eq!(phase, "cleanup-in-progress-started\n");
        } else {
            result
                .unwrap_or_else(|error| panic!("{error:?}; cleanup diagnostics: {diagnostics:?}"));
            assert!(diagnostics.denials.is_empty());
            assert_eq!(phase, "cleanup-proven-started\n");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exited_retained_pidfd_cannot_exempt_a_reused_numeric_pid() {
        use std::os::fd::FromRawFd;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        assert!(raw >= 0);
        let mut retained = std::collections::BTreeMap::new();
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw as i32) };
        retained.insert(pid, fd);
        let live = super::retained_helper_is_live(&retained, pid);
        let unknown = super::retained_helper_is_live(&retained, -1);
        child.kill().unwrap();
        child.wait().unwrap();
        let exited = super::retained_helper_is_live(&retained, pid);
        let mut foreign = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        // Model PID reuse without changing host PID allocation: the saved
        // identity is dead even if its numeric map key names a new process.
        let old = retained.remove(&pid).unwrap();
        retained.insert(foreign.id() as i32, old);
        let reused = super::retained_helper_is_live(&retained, foreign.id() as i32);
        let survived = foreign.try_wait().unwrap().is_none();
        let _ = foreign.kill();
        let _ = foreign.wait();
        assert!(live);
        assert!(!unknown && !exited && !reused);
        assert!(survived);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controlled_verified_descendant_remains_owned_after_term_clears_its_environment() {
        use std::os::unix::process::ExitStatusExt;
        let root = tempfile::tempdir().unwrap();
        let id = "fake-cleared-markers";
        let pin = helper_fixture(root.path(), id, "running\n");
        let mut helper = ReapedTestChild(fake_helper(root.path(), id, "slirp"));
        let mut watcher = ReapedTestChild(fake_helper(root.path(), id, "watcher"));
        let ready = root.path().join("descendant-ready");
        let changed = root.path().join("descendant-execed");
        let mut descendant = ReapedTestChild(
            std::process::Command::new("sh")
                .arg("-c")
                .arg("trap 'trap \"\" TERM; : > \"$2\"; exec /usr/bin/env -i /bin/sleep 30' TERM; : > \"$1\"; while :; do :; done")
                .arg("fake-descendant").arg(&ready).arg(&changed)
                .env("CODEG_ROUNDTABLE_SLIRP_OWNER", id)
                .env("CODEG_ROUNDTABLE_SLIRP_ROOT", root.path())
                .env("CODEG_ROUNDTABLE_SLIRP_ROLE", "watcher")
                .spawn().unwrap(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        write_pins(
            &pin,
            &(pin_line("slirp", helper.id()) + &pin_line("watcher", watcher.id())),
        );
        let capture = CleanupDiagnosticCapture::start([
            ("helper", helper.id() as i32),
            ("watcher", watcher.id() as i32),
            ("descendant", descendant.id() as i32),
        ]);
        let _inventory =
            ControlledInventory::start(&[helper.id(), watcher.id(), descendant.id()], false);
        let result = super::stop_slirp(root.path(), id, true);
        let diagnostics = capture.finish();
        let ended = descendant.try_wait().unwrap();
        let _ = helper.kill();
        let _ = watcher.kill();
        let _ = descendant.kill();
        let _ = helper.wait();
        let _ = watcher.wait();
        let _ = descendant.wait();
        result.unwrap_or_else(|error| panic!("{error:?}; cleanup diagnostics: {diagnostics:?}"));
        assert!(
            changed.exists(),
            "descendant must run its TERM-to-exec path"
        );
        assert!(
            ended.is_some_and(|status| status.signal() == Some(libc::SIGKILL)),
            "verified descendant escaped after clearing markers"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controlled_interrupted_cleanup_cannot_forget_a_descendant_on_retry() {
        let root = tempfile::tempdir().unwrap();
        let id = "fake-interrupted-cleanup";
        let pin = helper_fixture(root.path(), id, "running\n");
        let mut helper = ReapedTestChild(fake_helper(root.path(), id, "slirp"));
        let mut watcher = ReapedTestChild(fake_helper(root.path(), id, "watcher"));
        let ready = root.path().join("descendant-ready");
        let changed = root.path().join("descendant-execed");
        let mut descendant = ReapedTestChild(
            std::process::Command::new("sh")
                .arg("-c")
                .arg("trap 'trap \"\" TERM; : > \"$2\"; exec /usr/bin/env -i /bin/sleep 30' TERM; : > \"$1\"; while :; do :; done")
                .arg("fake-descendant").arg(&ready).arg(&changed)
                .env("CODEG_ROUNDTABLE_SLIRP_OWNER", id)
                .env("CODEG_ROUNDTABLE_SLIRP_ROOT", root.path())
                .env("CODEG_ROUNDTABLE_SLIRP_ROLE", "watcher")
                .spawn().unwrap(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        write_pins(
            &pin,
            &(pin_line("slirp", helper.id()) + &pin_line("watcher", watcher.id())),
        );
        let descendant_pid = descendant.id();
        let changed_marker = changed.clone();
        SWEEP_OBSERVER.with(|observer| {
            *observer.borrow_mut() = Some(Box::new(move || {
                let environment =
                    std::fs::read(format!("/proc/{descendant_pid}/environ")).unwrap_or_default();
                if changed_marker.exists()
                    && !environment
                        .windows(b"CODEG_ROUNDTABLE_SLIRP_OWNER=".len())
                        .any(|part| part == b"CODEG_ROUNDTABLE_SLIRP_OWNER=")
                {
                    return Err(super::rt_error(
                        roundtable_protocol::ErrorCode::PolicyUnenforceable,
                        "injected_helper_enumeration",
                    ));
                }
                Ok(())
            }));
        });
        let fault = SweepObserverGuard;
        let _inventory =
            ControlledInventory::start(&[helper.id(), watcher.id(), descendant.id()], false);
        let first = super::stop_slirp(root.path(), id, true);
        drop(fault);
        let alive_after_fault = descendant.try_wait().unwrap().is_none();
        // No observer/fault is installed for the second call. It reopens the
        // durable state exactly as a new cleanup process would after restart.
        let retry = super::stop_slirp(root.path(), id, true);
        let alive_after_retry = descendant.try_wait().unwrap().is_none();
        let phase = std::fs::read_to_string(pin.with_extension("pid.state")).unwrap();
        let _ = helper.kill();
        let _ = watcher.kill();
        let _ = descendant.kill();
        let _ = helper.wait();
        let _ = watcher.wait();
        let _ = descendant.wait();
        assert_eq!(
            first.unwrap_err().details.reason.as_deref(),
            Some("injected_helper_enumeration")
        );
        assert!(changed.exists() && alive_after_fault);
        assert_eq!(
            retry.unwrap_err().details.reason.as_deref(),
            Some("slirp_cleanup_interrupted")
        );
        assert!(
            alive_after_retry,
            "retry must not act on lost process identity"
        );
        assert_eq!(phase, "cleanup-in-progress-started\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fifo_pin_fails_promptly_without_a_writer_or_unrelated_signal() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let id = "fake-fifo-pin";
        let pin = helper_fixture(root.path(), id, "running\n");
        let path = std::ffi::CString::new(pin.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let mut foreign = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let runtime_root = root.path().to_path_buf();
        let cleanup = std::thread::spawn(move || {
            let _ = send.send(super::stop_slirp(&runtime_root, id, true));
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(2));
        if result.is_err() {
            // Rescue the red test's blocked reader so the suite never leaves
            // its stand-in thread/process behind after reporting the failure.
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&pin);
        }
        cleanup.join().unwrap();
        let survived = foreign.try_wait().unwrap().is_none();
        let _ = foreign.kill();
        let _ = foreign.wait();
        assert!(
            result.is_ok(),
            "FIFO evidence blocked outside the cleanup deadline"
        );
        assert!(result.unwrap().is_err());
        assert!(survived, "FIFO evidence authorized an unrelated signal");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controlled_no_helper_and_durable_never_started_are_distinct_from_missing_configuration() {
        let root = tempfile::tempdir().unwrap();
        assert!(super::stop_slirp(root.path(), "no-network", false).is_ok());
        assert!(super::stop_slirp(root.path(), "missing-network-state", true).is_err());
        let pin = helper_fixture(root.path(), "not-started", "prepared\n");
        let _inventory = ControlledInventory::start(&[], false);
        super::stop_slirp(root.path(), "not-started", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(pin.with_extension("pid.state")).unwrap(),
            "cleanup-proven-never-started\n"
        );
        assert!(!pin.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn legacy_cancelled_cleanup_cannot_be_upgraded_into_a_proof() {
        for phase in ["cancelled-started", "cancelled-never-started"] {
            let root = tempfile::tempdir().unwrap();
            helper_fixture(root.path(), "legacy-cleanup", phase);
            let error = super::stop_slirp(root.path(), "legacy-cleanup", true).unwrap_err();
            assert_eq!(
                error.details.reason.as_deref(),
                Some("slirp_cleanup_interrupted")
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn forged_legacy_and_reused_pid_pins_never_authorize_signals() {
        for kind in ["forged", "legacy", "reused"] {
            let root = tempfile::tempdir().unwrap();
            let pin = helper_fixture(root.path(), kind, "running\n");
            let mut foreign = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap();
            let body = match kind {
                "legacy" => format!("{}\n", foreign.id()),
                "reused" => format!("slirp {} 1\nwatcher {} 1\n", foreign.id(), foreign.id()),
                _ => pin_line("slirp", foreign.id()) + &pin_line("watcher", foreign.id()),
            };
            write_pins(&pin, &body);
            let result = super::stop_slirp(root.path(), kind, true);
            let survived = foreign.try_wait().unwrap().is_none();
            let _ = foreign.kill();
            let _ = foreign.wait();
            assert!(result.is_err(), "{kind}");
            assert!(survived, "{kind} pin signaled a foreign process");
        }
    }

    #[cfg(unix)]
    type AuthReadObserver = Box<dyn FnOnce(&std::fs::File)>;

    #[cfg(unix)]
    std::thread_local! {
        static AUTH_READ_OBSERVER: std::cell::RefCell<Option<AuthReadObserver>> = const { std::cell::RefCell::new(None) };
    }

    #[cfg(unix)]
    pub(super) fn before_auth_read(input: &std::fs::File) {
        AUTH_READ_OBSERVER.with(|observer| {
            if let Some(callback) = observer.borrow_mut().take() {
                callback(input);
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn auth_copy_rejects_fifo_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("controlled-fifo");
        let destination = root.path().join("attempt-copy");
        let path = std::ffi::CString::new(source.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::channel();
        let copy_source = source.clone();
        let copy_destination = destination.clone();
        let copy = std::thread::spawn(move || {
            let _ = send.send(super::copy_regular_nofollow(
                &copy_source,
                &copy_destination,
                4,
            ));
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(2));
        // Rescue the old blocking open so a red test never strands a thread.
        let rescue = if result.is_err() {
            Some(
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&source)
                    .unwrap(),
            )
        } else {
            None
        };
        copy.join().unwrap();
        drop(rescue);
        assert!(
            result.is_ok(),
            "FIFO source blocked before metadata validation"
        );
        assert_eq!(
            result.unwrap().unwrap_err().details.reason.as_deref(),
            Some("auth_copy")
        );
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn auth_copy_bounds_reads_when_regular_source_grows_after_metadata() {
        use std::io::Seek;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("controlled-source");
        let destination = root.path().join("attempt-copy");
        std::fs::write(&source, b"ok").unwrap();
        let shared_input = std::rc::Rc::new(std::cell::RefCell::new(None));
        let observed_input = shared_input.clone();
        let growing_source = source.clone();
        AUTH_READ_OBSERVER.with(|observer| {
            *observer.borrow_mut() = Some(Box::new(move |input| {
                // Metadata accepted two bytes. Grow the same regular inode
                // before its first read and retain its shared file offset.
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&growing_source)
                    .unwrap()
                    .set_len(4096)
                    .unwrap();
                *observed_input.borrow_mut() = Some(input.try_clone().unwrap());
            }));
        });
        let result = super::copy_regular_nofollow(&source, &destination, 4);
        AUTH_READ_OBSERVER.with(|observer| *observer.borrow_mut() = None);
        assert_eq!(
            result.unwrap_err().details.reason.as_deref(),
            Some("auth_copy")
        );
        let mut input = shared_input
            .borrow_mut()
            .take()
            .expect("post-metadata observer");
        assert_eq!(input.stream_position().unwrap(), 5, "read exceeds limit+1");
        assert_eq!(std::fs::metadata(&source).unwrap().len(), 4096);
        assert!(!destination.exists());
    }

    #[tokio::test]
    async fn oversized_control_output_stops_before_the_complete_stream() {
        let data = vec![b'x'; 2 * 1024 * 1024];
        let mut cursor = std::io::Cursor::new(data);
        let error = super::bounded_control_read(&mut cursor).await.unwrap_err();
        assert_eq!(
            error.details.reason.as_deref(),
            Some("runtime_control_bytes")
        );
        assert_eq!(cursor.position(), 1024 * 1024 + 1);
    }
}
