//! OCI plan for the rootless crun candidate.
//!
//! The document is data. Building it does not launch crun, mount a namespace,
//! or download an image. Spawn stays `policy_unenforceable` until a Linux host
//! can prove the pinned binary and the namespace set.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
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
    let scratch = reject_host_path(&input.scratch, input)?;
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
            "seccomp": {
                "defaultAction": "SCMP_ACT_ERRNO",
                "architectures": ["SCMP_ARCH_X86_64"]
            },
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
    let mut plan = build_plan(input)?;
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
    plan.oci["linux"]["seccomp"]["syscalls"] =
        json!([{"names":SYSCALLS,"action":"SCMP_ACT_ALLOW"}]);
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
    install_home_upper(&mut plan, profile)?;
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

fn install_home_upper(plan: &mut SandboxPlan, profile: &QualifiedOciProfile) -> RtResult<()> {
    let scratch = plan
        .mounts
        .iter()
        .find(|item| item.destination == "/scratch")
        .map(|item| item.source.clone())
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "home_upper"))?;
    let upper = scratch.join("rt-home");
    fs::create_dir_all(&upper)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "home_upper"))?;
    for mount in &profile.auth_mounts {
        let Some(relative) = mount.destination.strip_prefix("/rt-home/") else {
            return Err(rt_error(ErrorCode::InvalidArgument, "auth_mount"));
        };
        let path = upper.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "home_upper"))?;
        }
        if !path.exists() {
            fs::write(&path, b"")
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "home_upper"))?;
        }
    }
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
# poststart: the user namespace exists. A closed ready fifo is EOF, not the
# ready byte, so require "1". Do not exec slirp: crun is a child subreaper
# and waits on that orphan, so `crun run` would never return.
ready="${TMPDIR:-/tmp}/codeg-slirp-ready-$$"
rm -f "$ready"
mkfifo "$ready" || exit 1
"$1" --configure --disable-host-loopback --mtu=65520 \
  --userns-path="/proc/$pid/ns/user" \
  --netns-type=path \
  --ready-fd 3 \
  "/proc/$pid/ns/net" \
  tap0 >/dev/null 2>&1 3>"$ready" </dev/null &
slirp=$!
echo "$slirp" > "$2"
ready_byte=$(timeout 8 dd bs=1 count=1 <"$ready" 2>/dev/null || true)
rm -f "$ready"
if [ "$ready_byte" != "1" ]; then
  kill "$slirp" 2>/dev/null || true
  exit 1
fi
# Kill slirp when the container pid exits so crun can finish waiting.
( while kill -0 "$pid"; do sleep 0.2; done; kill "$slirp" ) &
exit 0
"#;

fn install_slirp_hook(
    plan: &mut SandboxPlan,
    profile: &QualifiedOciProfile,
    id: &str,
) -> RtResult<()> {
    let slirp = slirp_binary()
        .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "slirp4netns_missing"))?;
    let dir = profile.runtime_root.join("slirp-pids");
    fs::create_dir_all(&dir).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "slirp_hook"))?;
    let hook = profile.runtime_root.join("slirp-hook.sh");
    fs::write(&hook, SLIRP_HOOK_SCRIPT)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "slirp_hook"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "slirp_hook"))?;
    }
    let pidfile = dir.join(format!("{id}.pid"));
    plan.oci["annotations"]["io.codeg.roundtable.network"] =
        json!("slirp-egress-not-origin-filtered");
    plan.oci["hooks"] = json!({
        SLIRP_HOOK_PHASE: [{
            "path": hook,
            "args": ["slirp-hook.sh", slirp, pidfile],
            "env": []
        }]
    });
    Ok(())
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

pub(super) fn stop_slirp(runtime_root: &Path, id: &str) {
    let pidfile = runtime_root.join("slirp-pids").join(format!("{id}.pid"));
    if let Ok(text) = fs::read_to_string(&pidfile) {
        if let Ok(pid) = text.trim().parse::<i32>() {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            let _ = pid;
        }
    }
    let _ = fs::remove_file(&pidfile);
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
        || plan.oci["linux"]["seccomp"]["syscalls"]
            != json!([{"names":SYSCALLS,"action":"SCMP_ACT_ALLOW"}])
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
            "/scratch" => !mount.read_only && mount.source.is_absolute(),
            "/rt-home" => home_upper_allowed(plan, mount),
            "/run/codeg/roundtable.sock" => {
                profile.service_socket.as_ref() == Some(&mount.source) && mount.read_only
            }
            "/run/codeg/gateway.sock" => {
                profile.gateway_socket.as_ref() == Some(&mount.source) && mount.read_only
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
            || mount.destination.starts_with("/rt-home/");
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
        "/proc", "/dev", "/dev/pts", "/dev/shm", "/sys", "/tmp", "/scratch", "/rt-home",
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
pub(super) async fn spawn_attached(
    plan: &SandboxPlan,
    profile: &QualifiedOciProfile,
) -> RtResult<(SandboxInstance, tokio::process::Child)> {
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
    let child = crate::acp::agent_process::spawn_attached_roundtable_oci(
        &plan.runtime_path,
        &args,
        &bundle,
    )?;
    Ok((instance, child))
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
    if !path.exists() {
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
    stop_slirp(&profile.runtime_root, &instance.runtime_id);
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
