//! OCI plan for the rootless crun candidate.
//!
//! The document is data. Building it does not launch crun, mount a namespace,
//! or download an image. Spawn stays `policy_unenforceable` until a Linux host
//! can prove the pinned binary and the namespace set.

use std::collections::BTreeMap;
use std::path::PathBuf;

use roundtable_protocol::{ErrorCode, Hash256, RtResult};
use serde_json::{json, Value};

use super::{owner_label, CgroupLimits, NamespaceSet, PlanMount, SandboxInput, SandboxPlan};
use crate::roundtable::qualification::CertifiedBinary;
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
    reject_host_path(&input.scratch, input)?;
    let env = clear_then_allow(&input.inherited_env, &input.env_allowlist)?;
    let label = owner_label(&input.db, input.boot_epoch, &input.incarnation);
    let mounts = vec![
        PlanMount {
            source: PathBuf::from("proc"),
            destination: "/proc".to_string(),
            read_only: true,
        },
        PlanMount {
            source: input.scratch.clone(),
            destination: "/scratch".to_string(),
            read_only: false,
        },
    ];
    let cgroup = CgroupLimits {
        memory_max_bytes: MEMORY_MAX_BYTES,
        pids_max: PIDS_MAX,
        cpu_quota_us: CPU_QUOTA_US,
    };
    let oci = oci_document(input, cli, &env, &label, &cgroup);
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

/// P06a does not claim a proven process listing on any host. An empty list
/// would be read as "nothing left to reap".
pub(super) fn enumeration_proven() -> bool {
    false
}

fn binary<'a>(binaries: &'a [CertifiedBinary], role: &str) -> RtResult<&'a CertifiedBinary> {
    binaries
        .iter()
        .find(|binary| binary.role == role)
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "binary_missing"))
}

fn reject_host_path(scratch: &std::path::Path, input: &SandboxInput) -> RtResult<()> {
    let forbidden = forbidden_paths(input);
    if forbidden.iter().any(|path| *path == scratch) {
        return Err(rt_error(ErrorCode::InvalidArgument, "scratch_is_host_path"));
    }
    Ok(())
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

fn oci_document(
    input: &SandboxInput,
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
        "mounts": [
            {
                "destination": "/proc",
                "type": "proc",
                "source": "proc",
                "options": ["nosuid", "noexec", "nodev"]
            },
            {
                "destination": "/scratch",
                "type": "bind",
                "source": input.scratch,
                "options": ["rbind", "rw", "nosuid", "nodev", "nosymfollow"]
            }
        ],
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
