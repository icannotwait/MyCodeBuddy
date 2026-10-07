//! Real crun measurements. A missing isolator, a skipped escape, or a failed
//! ACP turn becomes a failing check. Nothing here invents a pass.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use roundtable_protocol::Hash256;
use sha2::{Digest, Sha256};
use tokio::io::BufReader;
use tokio::process::Command;

use super::companion::ATTEMPT_TOKEN_ENV;
use super::host_model_auth::{resolve_model_upstream, unix_now, ResolveInput};
use super::installed_runtime::ProviderBinding;
use super::live_gateway::{LiveGatewayServer, LiveModelGateway};
use super::qualification::CertifiedBinary;
#[cfg(test)]
use super::qualification_experiment::CredentialCanary;
use super::qualification_experiment::{
    failed_observation, production_checks, production_prompt, ObserveInput, QualificationExperiment,
};
use super::qualification_probe::{ProbeCheck, ProbeFacts, ProbeRequest};
use super::qualification_profiles::{profile_by_id, profile_for_agent};
use super::relay::SANDBOX_ENDPOINT;
use super::sandbox::{AuthMount, QualifiedOciProfile};

pub async fn measure_host(request: &ProbeRequest) -> ProbeFacts {
    let mut facts = empty_facts(&request.agent);
    let Some(profile) = select_profile(request) else {
        facts.anomalies.push("unknown_adapter".into());
        return facts;
    };
    facts.profile_id = profile.exact_id.to_string();
    if !cfg!(target_os = "linux") {
        facts.platform_blocked = true;
        facts.anomalies.push("platform_blocked".into());
        return facts;
    }
    match read_os() {
        Some((name, version)) => {
            facts.os_name = name;
            facts.os_version = version;
        }
        None => {
            facts.platform_blocked = true;
            facts.anomalies.push("host_os_unknown".into());
            return facts;
        }
    }
    facts.kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    facts.arch = std::env::consts::ARCH.to_string();
    if facts.kernel.is_empty() {
        facts.anomalies.push("host_kernel_unknown".into());
    }

    let mut failures: Vec<ProbeCheck> = Vec::new();
    if !request.crun.is_file() {
        failures.push(fail("strict_isolation", "crun is not installed"));
        stamp_failures(&mut facts, &failures);
        return facts;
    }
    let state_root = request.runtime_root.join("state");
    let _ = fs::create_dir_all(&state_root);
    let state_arg = state_root.to_string_lossy().into_owned();
    // Bare `crun --version` prints "Failed to get state directory" when crun
    // has no default root. The key must store the version line from the same
    // state root the containers use.
    let crun_version = command_text(&request.crun, &["--root", &state_arg, "--version"])
        .await
        .as_deref()
        .and_then(recorded_crun_version);
    let crun_hash = hash_file(&request.crun);
    let (crun_version, crun_hash) = match (crun_version, crun_hash) {
        (Some(version), Some(hash)) if !version.is_empty() => (version, hash),
        _ => {
            failures.push(fail(
                "strict_isolation",
                "crun version or sha256 unreadable",
            ));
            stamp_failures(&mut facts, &failures);
            return facts;
        }
    };
    if let Some(reason) = super::sandbox::linux_cgroup_delegation_error(&request.cgroup_root) {
        failures.push(fail("strict_isolation", &reason));
    }
    if !userns_available() {
        failures.push(fail(
            "strict_isolation",
            "unprivileged user namespace is not available",
        ));
    }

    let image_hash = match super::sandbox::qualified_rootfs_digest_detail(&request.rootfs) {
        Ok(hash) => hash,
        Err(reason) => {
            failures.push(fail("strict_isolation", &reason));
            stamp_failures(&mut facts, &failures);
            return facts;
        }
    };
    if let Some(reason) = rootfs_layout_error(&request.rootfs) {
        failures.push(fail("strict_isolation", &reason));
        stamp_failures(&mut facts, &failures);
        return facts;
    }
    facts.image_digest = format!("sha256:{}", image_hash.to_hex());

    let cli_path = request
        .rootfs
        .join(profile.container_cli.trim_start_matches('/'));
    let mcp_path = request
        .rootfs
        .join(profile.container_mcp.trim_start_matches('/'));
    let cli_hash = hash_file(&cli_path);
    let mcp_hash = hash_file(&mcp_path);
    if cli_hash.is_none() {
        failures.push(fail(
            "actual_binary_match",
            "adapter binary missing from rootfs",
        ));
    }
    if mcp_hash.is_none() {
        failures.push(fail("roundtable_mcp", "codeg-mcp missing from rootfs"));
    }

    let mut host_held = Vec::new();
    for file in profile.auth_files {
        let source = request.home.join(file.home_relative);
        if !source.is_file() {
            if file.required {
                failures.push(fail(
                    "api_credential_scope",
                    &format!("auth file missing: {}", file.home_relative),
                ));
            }
            continue;
        }
        if path_inside(&source, &request.rootfs) {
            failures.push(fail(
                "model_credential_material_in_sandbox",
                "auth file is inside the image",
            ));
        }
        let placeholder = request
            .rootfs
            .join(file.destination.trim_start_matches('/'));
        if let Ok(metadata) = fs::symlink_metadata(&placeholder) {
            if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != 0 {
                failures.push(fail(
                    "model_credential_material_in_sandbox",
                    "auth bytes are baked into the image",
                ));
            }
        }
        host_held.push(AuthMount {
            source,
            destination: file.destination.to_string(),
        });
    }
    if let Some(filename) = profile.require_one_filename {
        if !host_held
            .iter()
            .any(|mount| mount.destination.rsplit('/').next() == Some(filename))
        {
            failures.push(fail(
                "api_credential_scope",
                &format!("auth file missing: need one {filename} from the profile"),
            ));
        }
    }

    let adapter_version = version_inside(request, profile.container_cli, &["--version"]).await;
    if adapter_version.is_none() {
        failures.push(not_tested(
            "cancel_and_reap",
            "version probe did not return a final cleanup proof",
        ));
    }
    let version_ok = adapter_version
        .as_deref()
        .is_some_and(|text| text.contains(profile.version_needle));

    if let (Some(cli_hash), Some(mcp_hash)) = (cli_hash, mcp_hash) {
        facts.binaries = vec![
            CertifiedBinary {
                role: "crun".into(),
                absolute_path: request.crun.display().to_string(),
                version: first_line(&crun_version),
                sha256: crun_hash,
            },
            CertifiedBinary {
                role: "cli".into(),
                absolute_path: profile.container_cli.into(),
                version: profile.adapter_version.into(),
                sha256: cli_hash,
            },
            CertifiedBinary {
                role: "mcp".into(),
                absolute_path: profile.container_mcp.into(),
                version: "codeg-mcp".into(),
                sha256: mcp_hash,
            },
        ];
        if !version_ok {
            failures.push(fail(
                "actual_binary_match",
                "adapter version does not match the profile pin",
            ));
        } else if hash_file(&cli_path) == Some(cli_hash)
            && hash_file(&request.crun) == Some(crun_hash)
        {
            facts.checks.push(pass_flag(
                "actual_binary_match",
                "re-hashed crun and the adapter inside the rootfs",
                true,
            ));
        }
    }

    facts.providers = read_providers(&request.provider_bindings).unwrap_or_else(|reason| {
        failures.push(fail("api_credential_scope", &reason));
        Vec::new()
    });
    let mut container_env = BTreeMap::new();
    for (key, value) in profile.container_env {
        container_env.insert((*key).to_string(), (*value).to_string());
    }
    facts.oci = Some(QualifiedOciProfile {
        runtime: facts
            .binaries
            .iter()
            .find(|binary| binary.role == "crun")
            .cloned()
            .unwrap_or(CertifiedBinary {
                role: "crun".into(),
                absolute_path: request.crun.display().to_string(),
                version: first_line(&crun_version),
                sha256: crun_hash,
            }),
        rootfs: request.rootfs.clone(),
        rootfs_sha256: image_hash,
        runtime_root: request.runtime_root.clone(),
        cgroup_root: request.cgroup_root.clone(),
        cli_args: profile
            .cli_args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect(),
        service_socket: None,
        gateway_socket: None,
        auth_mounts: Vec::new(),
        host_held_credentials: host_held.clone(),
        container_env,
    });

    if failures
        .iter()
        .any(|check| check.name == "strict_isolation")
    {
        stamp_failures(&mut facts, &failures);
        return facts;
    }

    match run_isolation(request).await {
        Ok(output) => {
            facts.isolation_marker = if output.contains("ISOLATION_OK") && required_denials(&output)
            {
                "ISOLATION_OK".into()
            } else {
                String::new()
            };
            let idle = if output.contains("REAPED") { 0 } else { 1 };
            facts.checks.push(ProbeCheck {
                name: "strict_isolation".into(),
                status: if facts.isolation_marker == "ISOLATION_OK" {
                    "passed".into()
                } else {
                    "failed".into()
                },
                evidence: redact_keep(&output),
                input: b"isolation-probe".to_vec(),
                output: output.into_bytes(),
                count: None,
                flag: None,
            });
            facts.checks.push(ProbeCheck {
                name: "cancel_and_reap".into(),
                status: if idle == 0 {
                    "passed".into()
                } else {
                    "failed".into()
                },
                evidence: "crun delete after the probe".into(),
                input: b"reap".to_vec(),
                output: idle.to_string().into_bytes(),
                count: Some(idle),
                flag: None,
            });
            // This shell probe measures host-path isolation only. It does not
            // exercise the adapter's native tools or its authentication files.
        }
        Err(reason) => {
            facts.checks.push(fail("strict_isolation", &reason));
            facts.checks.push(fail("cancel_and_reap", &reason));
            facts.checks.push(fail("native_read_boundary", &reason));
        }
    }

    let probe_token = mint_probe_token();
    match run_mcp(request, &probe_token).await {
        Ok(()) => {
            facts.checks.push(pass(
                "companion_socket_connection",
                "codeg-mcp connected to the mounted socket; no production broker submission was performed",
            ));
            facts.checks.push(pass(
                "companion_lifecycle",
                "companion exited and the socket closed",
            ));
        }
        Err(reason) => {
            facts.checks.push(not_tested(
                "cancel_and_reap",
                "MCP probe failure did not return a final cleanup proof",
            ));
            facts
                .checks
                .push(fail("companion_socket_connection", &reason));
            facts.checks.push(fail("companion_lifecycle", &reason));
        }
    }

    // Sandbox credential isolation is deferred. The ACP container bind-mounts
    // the host files the CLI already reads, and these checks do not fail the
    // certificate. A missing required host file is still a failure, stamped
    // below from `failures`.
    facts.checks.extend(deferred_host_auth_checks());

    match run_acp(
        request,
        profile.container_cli,
        profile.cli_args,
        &probe_token,
    )
    .await
    {
        Ok(probe) => {
            match probe.turn {
                Ok(turn) => {
                    facts
                        .checks
                        .push(pass("new_session", "ACP initialize and session/new"));
                    if model_binding_error(&request.agent, &turn.session, &facts.providers)
                        .is_none()
                    {
                        facts.checks.push(pass(
                            "advertised_model_binding",
                            &format!("protocolVersion 1; {}", turn.egress_note),
                        ));
                    }
                    facts.checks.push(pass_flag(
                        "ordered_turn_completion",
                        "session/prompt stopReason=end_turn",
                        turn.completed,
                    ));
                }
                Err(reason) => {
                    facts.checks.push(not_tested(
                        "cancel_and_reap",
                        "ACP probe failure did not return a final cleanup proof",
                    ));
                    facts.checks.push(fail("new_session", &reason));
                    facts.checks.push(fail("ordered_turn_completion", &reason));
                }
            }
            facts.checks.extend(production_checks(&probe.observation));
        }
        Err(reason) => {
            facts.checks.push(not_tested(
                "cancel_and_reap",
                "ACP probe failure did not return a final cleanup proof",
            ));
            facts.checks.push(fail("new_session", &reason));
            facts.checks.push(fail("ordered_turn_completion", &reason));
            facts
                .checks
                .extend(production_checks(&failed_observation()));
        }
    }
    for name in [
        "submit_receipt_completion",
        "roundtable_mcp",
        "bounded_context_delivery",
        "private_events",
        "sidebar_discovery",
        "global_body_events",
        "model_credentials_visible_to_agent",
        "model_credential_material_in_sandbox",
        "native_read_boundary",
        "api_credential_scope",
        "endpoint_compatibility",
    ] {
        if !facts.checks.iter().any(|check| check.name == name) {
            facts.checks.push(not_tested(
                name,
                "production path was not exercised by this smoke probe",
            ));
        }
    }
    stamp_failures(&mut facts, &failures);
    facts
}

fn empty_facts(agent: &str) -> ProbeFacts {
    ProbeFacts {
        agent: agent.to_string(),
        profile_id: String::new(),
        os_name: String::new(),
        os_version: String::new(),
        kernel: String::new(),
        arch: String::new(),
        platform_blocked: false,
        checks: Vec::new(),
        binaries: Vec::new(),
        image_digest: String::new(),
        oci: None,
        providers: Vec::new(),
        anomalies: Vec::new(),
        isolation_marker: String::new(),
        fake_broker: false,
        fake_fd: false,
    }
}

fn stamp_failures(facts: &mut ProbeFacts, failures: &[ProbeCheck]) {
    for failure in failures {
        if let Some(existing) = facts
            .checks
            .iter_mut()
            .find(|check| check.name == failure.name)
        {
            if existing.status != "failed" || failure.status == "failed" {
                *existing = failure.clone();
            }
        } else {
            facts.checks.push(failure.clone());
        }
    }
}

fn not_tested(name: &str, evidence: &str) -> ProbeCheck {
    let mut check = fail(name, evidence);
    check.status = "not_tested".into();
    check
}

fn fail(name: &str, evidence: &str) -> ProbeCheck {
    ProbeCheck {
        name: name.into(),
        status: "failed".into(),
        evidence: evidence.into(),
        input: name.as_bytes().to_vec(),
        output: evidence.as_bytes().to_vec(),
        count: None,
        flag: None,
    }
}

fn pass(name: &str, evidence: &str) -> ProbeCheck {
    ProbeCheck {
        name: name.into(),
        status: "passed".into(),
        evidence: evidence.into(),
        input: name.as_bytes().to_vec(),
        output: evidence.as_bytes().to_vec(),
        count: None,
        flag: None,
    }
}

fn pass_flag(name: &str, evidence: &str, flag: bool) -> ProbeCheck {
    let mut check = pass(name, evidence);
    check.flag = Some(flag);
    check
}

fn read_os() -> Option<(String, String)> {
    let release = fs::read_to_string("/etc/os-release").ok()?;
    let field = |key: &str| {
        release.lines().find_map(|line| {
            line.strip_prefix(&format!("{key}="))
                .map(|value| value.trim_matches('"').to_string())
        })
    };
    let id = field("ID")?;
    let version = field("VERSION_ID")?;
    Some(("linux".into(), format!("{id}-{version}")))
}

fn userns_available() -> bool {
    if let Ok(value) = fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone") {
        return value.trim() == "1";
    }
    fs::read_to_string("/proc/sys/user/max_user_namespaces")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .is_some_and(|value| value > 0)
}

fn hash_file(path: &Path) -> Option<Hash256> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Some(Hash256::from_bytes(hasher.finalize().into()))
}

fn path_inside(path: &Path, root: &Path) -> bool {
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    path.starts_with(root)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).trim().to_string()
}

/// The line stored on the qualification key. Ignores crun's state-directory
/// error so two hosts with the same crun binary record the same version.
fn recorded_crun_version(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("crun version") && !lower.contains("failed to get state directory")
        })
        .map(str::to_string)
        .filter(|line| !line.is_empty())
}

async fn command_text(bin: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(bin)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stderr).into_owned();
    }
    Some(text)
}

async fn version_inside(request: &ProbeRequest, cli: &str, args: &[&str]) -> Option<String> {
    let mut full = vec![cli.to_string()];
    full.extend(args.iter().map(|arg| (*arg).to_string()));
    run_container(
        request,
        &full,
        &[],
        None,
        Duration::from_secs(20),
        false,
        None,
    )
    .await
    .ok()
}

fn read_providers(path: &Path) -> Result<Vec<ProviderBinding>, String> {
    let bytes = fs::read(path).map_err(|_| "provider bindings file missing".to_string())?;
    serde_json::from_slice(&bytes).map_err(|_| "provider bindings unreadable".to_string())
}

fn required_denials(output: &str) -> bool {
    [
        "DENIED host_secret",
        "DENIED host_home",
        "DENIED rootfs_write",
        "DENIED proc_host",
        "DENIED network",
        "DENIED device",
        "PID1_OK",
        "ISOLATION_OK",
        "REAPED",
    ]
    .iter()
    .all(|token| output.contains(token))
}

#[cfg(test)]
const CREDENTIAL_CANARY_SCRIPT: &str = r#"#!/bin/sh
set -u
control=$(cat /scratch/control.txt 2>/dev/null || true)
printf 'CONTROL:%s\n' "$control"
for path in "$@"; do
  if [ -f "$path" ] && [ -s "$path" ]; then
    printf 'PRESENT %s\n' "$path"
  else
    printf 'ABSENT %s\n' "$path"
  fi
done
find /rt-home -type f -size +0c 2>/dev/null | while read -r file; do
  printf 'HOME_FILE %s\n' "$file"
done
"#;

#[cfg(test)]
fn secret_visible_in_env(held: &[AuthMount], endpoint: &str) -> bool {
    for mount in held {
        let Ok(bytes) = fs::read(&mount.source) else {
            continue;
        };
        if bytes.len() < 8 {
            continue;
        }
        if endpoint
            .as_bytes()
            .windows(bytes.len())
            .any(|window| window == bytes)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[allow(dead_code)]
async fn run_credential_canary(
    request: &ProbeRequest,
    held: &[AuthMount],
) -> Result<CredentialCanary, String> {
    let scratch = request.runtime_root.join("probe-scratch");
    fs::create_dir_all(&scratch).map_err(|error| error.to_string())?;
    let control_token = format!("control-{}", uuid::Uuid::new_v4().simple());
    let unmounted_canary = format!("canary-{}", uuid::Uuid::new_v4().simple());
    fs::write(scratch.join("control.txt"), &control_token).map_err(|error| error.to_string())?;
    fs::write(
        request.runtime_root.join("unmounted-canary"),
        &unmounted_canary,
    )
    .map_err(|error| error.to_string())?;
    let script = scratch.join("credential-canary.sh");
    fs::write(&script, CREDENTIAL_CANARY_SCRIPT).map_err(|error| error.to_string())?;
    let mut argv = vec!["/bin/sh".into(), "/scratch/credential-canary.sh".into()];
    let destinations: Vec<String> = held.iter().map(|mount| mount.destination.clone()).collect();
    argv.extend(destinations.iter().cloned());
    let output = run_container(
        request,
        &argv,
        &[(scratch, "/scratch".into(), false)],
        None,
        Duration::from_secs(60),
        false,
        None,
    )
    .await?;
    let _ = fs::remove_file(request.runtime_root.join("unmounted-canary"));
    Ok(CredentialCanary {
        output,
        mounted_destinations: 0,
        destinations,
        control_token,
        unmounted_canary,
        host_files_ready: held.iter().any(|mount| mount.source.is_file())
            && select_profile(request).is_some_and(|profile| {
                profile.require_one_filename.is_none_or(|filename| {
                    held.iter()
                        .any(|mount| mount.destination.rsplit('/').next() == Some(filename))
                }) && profile
                    .auth_files
                    .iter()
                    .filter(|file| file.required)
                    .all(|file| {
                        held.iter()
                            .any(|mount| mount.destination == file.destination)
                    })
            }),
        secret_visible_in_env: secret_visible_in_env(held, SANDBOX_ENDPOINT),
    })
}

async fn run_isolation(request: &ProbeRequest) -> Result<String, String> {
    let scratch = request.runtime_root.join("probe-scratch");
    fs::create_dir_all(&scratch).map_err(|error| error.to_string())?;
    let secret = request.runtime_root.join("probe-secret");
    fs::write(&secret, b"probe-canary").map_err(|error| error.to_string())?;
    let script = scratch.join("probe.sh");
    fs::write(&script, ISOLATION_SCRIPT).map_err(|error| error.to_string())?;
    let mounts = vec![(scratch.clone(), "/scratch".into(), false)];
    let output = run_container(
        request,
        &["/bin/sh".into(), "/scratch/probe.sh".into()],
        &mounts,
        Some(&secret),
        Duration::from_secs(60),
        false,
        None,
    )
    .await?;
    let _ = fs::remove_file(&secret);
    Ok(output)
}

async fn run_mcp(request: &ProbeRequest, token: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixListener;
        let dir = request.runtime_root.join("probe-mcp");
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let socket_path = dir.join("roundtable.sock");
        let _ = fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let _ = fs::create_dir_all(request.runtime_root.join("probe-scratch"));
        let mounts = vec![
            (
                request.runtime_root.join("probe-scratch"),
                "/scratch".into(),
                false,
            ),
            (socket_path, "/run/codeg/roundtable.sock".into(), false),
        ];
        let argv = [
            "/usr/local/bin/codeg-mcp".to_string(),
            "--service-roundtable".into(),
            "--socket-path".into(),
            "/run/codeg/roundtable.sock".into(),
            "--incarnation".into(),
            "qualify".into(),
        ];
        let prepared = prepare_bundle(request, &argv, &mounts, None, false, Some(token), &[], &[])?;
        let mut container = ProbeContainer {
            request,
            id: prepared.id.clone(),
            child: None,
            reaped: false,
            slirp_expected: false,
        };
        container.child = Some(
            Command::new(&request.crun)
                .args(crun_prefix(request))
                .arg("run")
                .arg("--bundle")
                .arg(&prepared.bundle)
                .arg(&prepared.id)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|error| error.to_string())?,
        );
        let start = std::time::Instant::now();
        let connected = loop {
            match listener.accept() {
                Ok(_) => break true,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > Duration::from_secs(15) {
                        break false;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(_) => break false,
            }
        };
        container.finish().await?;
        if connected {
            Ok(())
        } else {
            Err("mcp did not connect inside the isolator".into())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (request, token);
        Err("mcp socket requires unix".into())
    }
}

struct TurnOutcome {
    completed: bool,
    session_id: String,
    session: serde_json::Value,
    egress_note: String,
}

async fn run_acp(
    request: &ProbeRequest,
    cli: &str,
    args: &[&str],
    _smoke_token: &str,
) -> Result<AcpProbe, String> {
    let mut argv = vec![cli.to_string()];
    argv.extend(args.iter().map(|arg| (*arg).to_string()));
    let scratch = request.runtime_root.join("probe-scratch");
    let _ = fs::create_dir_all(&scratch);
    let mut mounts = vec![(scratch.clone(), "/scratch".into(), false)];
    let socket_path = super::companion::transport::short_socket_path("a");
    super::companion::transport::ensure_unix_socket_path(&socket_path.to_string_lossy()).map_err(
        |error| {
            error
                .details
                .reason
                .unwrap_or_else(|| "socket_path_too_long".into())
        },
    )?;
    let recipient = facts_model(request);
    let fixture = Hash256::sha256(request.agent.as_bytes());
    let experiment =
        QualificationExperiment::open(&request.data_dir, &socket_path, &recipient, fixture)
            .await
            .map_err(|error| {
                error
                    .details
                    .reason
                    .unwrap_or_else(|| "qualification_experiment".into())
            })?;
    let token = experiment.token();
    mounts.push((
        experiment.socket_path().to_path_buf(),
        "/run/codeg/roundtable.sock".into(),
        false,
    ));
    let egress_note = match run_egress(request, &mounts).await {
        Ok(note) => note,
        Err(reason) => return Err(reason),
    };
    let private_log = std::sync::Mutex::new(Vec::new());
    let attempt_bearer = uuid::Uuid::new_v4().simple().to_string();
    let model_env = profile_for_agent(&request.agent)
        .map(|profile| profile.model_gateway.entries(&attempt_bearer))
        .unwrap_or_default();
    let gateway = if model_env.is_empty() {
        None
    } else {
        let started = start_probe_gateway(request, &attempt_bearer).await?;
        mounts.push((started.path.clone(), "/run/codeg/gateway.sock".into(), true));
        Some(started)
    };
    let auth_overlays = host_auth_overlays(request);
    let output = run_acp_session(
        request,
        &argv,
        &mounts,
        &token,
        egress_note,
        Some(&private_log),
        &model_env,
        &auth_overlays,
    )
    .await;
    let gateway_exchanges = match gateway.as_ref() {
        Some(started) => started.server.exchange_log(),
        None => Vec::new(),
    };
    drop(gateway);
    let (session_id, endpoint_compatible) = match &output {
        Ok(turn) => (
            turn.session_id.clone(),
            model_binding_error(
                &request.agent,
                &turn.session,
                &read_providers(&request.provider_bindings).unwrap_or_default(),
            )
            .is_none(),
        ),
        Err(_) => (String::new(), false),
    };
    let frames = private_log
        .lock()
        .map(|log| log.clone())
        .unwrap_or_default();
    let (sandbox_endpoint, direct_provider_origin) = measured_endpoint(request, &model_env);
    let observation = experiment
        .observe(ObserveInput {
            session_id,
            agent: crate::models::AgentType::from_wire(&request.agent)
                .unwrap_or(crate::models::AgentType::Grok),
            private_frames: frames,
            endpoint_compatible,
            sandbox_endpoint,
            direct_provider_origin,
            gateway_exchanges,
            scratch_root: scratch,
        })
        .await;
    Ok(AcpProbe {
        turn: output,
        observation,
    })
}

struct AcpProbe {
    turn: Result<TurnOutcome, String>,
    observation: super::qualification_experiment::ProductionObservation,
}

/// Grok keeps the loopback gateway. Antigravity with no `AGY_*` variables
/// is measured against the provider origin the host oauth client calls.
fn measured_endpoint(request: &ProbeRequest, model_env: &[(String, String)]) -> (String, bool) {
    if request.agent == "antigravity" && model_env.is_empty() {
        let origin = read_providers(&request.provider_bindings)
            .ok()
            .and_then(|providers| {
                provider_for_agent(&providers, "antigravity")
                    .map(|provider| provider.origin.clone())
            })
            .unwrap_or_default();
        if origin.starts_with("https://") && !origin.contains("127.0.0.1") {
            return (origin, true);
        }
        return (String::new(), false);
    }
    (SANDBOX_ENDPOINT.to_string(), false)
}

fn facts_model(request: &ProbeRequest) -> String {
    read_providers(&request.provider_bindings)
        .ok()
        .and_then(|providers| {
            provider_for_agent(&providers, &request.agent).map(|provider| provider.model.clone())
        })
        .unwrap_or_else(|| request.agent.clone())
}

#[cfg(any(test, feature = "test-utils"))]
std::thread_local! {
    // The synchronous Drop fixture observes the real cleanup result without
    // replacing its cancellation path or trying cleanup again afterward.
    static PROBE_DROP_CLEANUP: std::cell::RefCell<Option<Result<(), String>>> = const { std::cell::RefCell::new(None) };
}

/// Stops the ACP `crun run` process, its slirp helper, and the container
/// when dropped. initialize and session/new return before the normal reap,
/// and those paths must not leave `cq-acp-*` running.
struct ProbeContainer<'a> {
    request: &'a ProbeRequest,
    id: String,
    child: Option<tokio::process::Child>,
    reaped: bool,
    slirp_expected: bool,
}

impl ProbeContainer<'_> {
    async fn finish(&mut self) -> Result<(), String> {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            child.wait().await.map_err(|error| error.to_string())?;
        }
        reap(self.request, &self.id, self.slirp_expected).await?;
        self.reaped = true;
        Ok(())
    }
}

impl Drop for ProbeContainer<'_> {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let pid = self.child.as_ref().and_then(|child| child.id());
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
        // Dropping a cancelled future cannot await. This is our own Child,
        // already sent SIGKILL; waitpid cannot target an unrelated process.
        #[cfg(unix)]
        if let Some(pid) = pid {
            loop {
                let result = unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), 0) };
                if result >= 0
                    || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    break;
                }
            }
        }
        #[cfg(not(unix))]
        let _ = pid;
        // Reap the launcher before the final helper sweep: it cannot create
        // another poststart hook after this point.
        let cleanup = reap_blocking(self.request, &self.id, self.slirp_expected);
        if let Err(reason) = &cleanup {
            tracing::warn!(container = %self.id, %reason, "qualification cleanup unproven");
        }
        #[cfg(any(test, feature = "test-utils"))]
        PROBE_DROP_CLEANUP.with(|result| *result.borrow_mut() = Some(cleanup));
    }
}

async fn run_acp_session(
    request: &ProbeRequest,
    argv: &[String],
    mounts: &[(PathBuf, String, bool)],
    token: &str,
    egress_note: String,
    private_log: Option<&std::sync::Mutex<Vec<String>>>,
    model_env: &[(String, String)],
    auth_overlays: &[(PathBuf, String)],
) -> Result<TurnOutcome, String> {
    let prepared = prepare_bundle(
        request,
        argv,
        mounts,
        None,
        true,
        None,
        model_env,
        auth_overlays,
    )?;
    let mut container = ProbeContainer {
        request,
        id: prepared.id.clone(),
        child: None,
        reaped: false,
        slirp_expected: true,
    };
    container.child = Some(
        Command::new(&request.crun)
            .args(crun_prefix(request))
            .arg("run")
            .arg("--bundle")
            .arg(&prepared.bundle)
            .arg(&prepared.id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?,
    );
    let child = container.child.as_mut().ok_or("acp child")?;
    let mut stdin = child.stdin.take().ok_or("acp stdin")?;
    let stdout = child.stdout.take().ok_or("acp stdout")?;
    let mut reader = BufReader::new(stdout);
    let init = rpc(
        &mut stdin,
        &mut reader,
        1,
        "initialize",
        super::live_runtime::roundtable_initialize_params("codeg-roundtable-qualify"),
        private_log,
    )
    .await?;
    if init["protocolVersion"] != 1 {
        return Err("acp protocol version".into());
    }
    let session = rpc(
        &mut stdin,
        &mut reader,
        2,
        "session/new",
        super::live_runtime::roundtable_session_params_for(
            crate::models::AgentType::from_wire(&request.agent)
                .unwrap_or(crate::models::AgentType::Cursor),
            &serde_json::json!([{
            "name": "roundtable",
            "command": "/usr/local/bin/codeg-mcp",
            "args": ["--service-roundtable", "--socket-path", "/run/codeg/roundtable.sock", "--incarnation", "qualify"],
            "env": mcp_env(token, !model_env.is_empty())
        }])),
        private_log,
    )
    .await?;
    let session_id = session["sessionId"]
        .as_str()
        .ok_or("acp session")?
        .to_string();
    let prompt = production_prompt();
    if prompt.len() > 8_192 {
        return Err("prompt over cap".into());
    }
    let result = rpc(
        &mut stdin,
        &mut reader,
        3,
        "session/prompt",
        serde_json::json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": prompt}]
        }),
        private_log,
    )
    .await?;
    container.finish().await?;
    Ok(TurnOutcome {
        completed: result["stopReason"] == "end_turn",
        session_id,
        session,
        egress_note,
    })
}

async fn rpc(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    id: u64,
    method: &str,
    params: serde_json::Value,
    private_log: Option<&std::sync::Mutex<Vec<String>>>,
) -> Result<serde_json::Value, String> {
    let mut seq = 0;
    let rejected = std::sync::atomic::AtomicBool::new(false);
    super::live_runtime::acp_exchange(
        stdin,
        stdout,
        &mut super::live_runtime::AcpExchange {
            id,
            method,
            params,
            seq: &mut seq,
            assistant: None,
            deadline: Some(Duration::from_secs(90)),
            rejected_permission: &rejected,
            private_log,
        },
    )
    .await
    .map_err(|error| match error.details.reason.as_deref() {
        Some("acp_timeout") => format!("{method} timed out"),
        Some("acp_closed") => format!("{method} closed"),
        Some("acp_rejected") => format!("{method} rejected\n{}", error.message),
        Some("acp_response") => format!("{method} response"),
        Some(reason) => format!("{method} {reason}"),
        None => method.to_string(),
    })
}

async fn run_container(
    request: &ProbeRequest,
    argv: &[String],
    mounts: &[(PathBuf, String, bool)],
    secret: Option<&Path>,
    timeout: Duration,
    slirp: bool,
    token: Option<&str>,
) -> Result<String, String> {
    let prepared = prepare_bundle(request, argv, mounts, secret, slirp, token, &[], &[])?;
    let mut container = ProbeContainer {
        request,
        id: prepared.id.clone(),
        child: None,
        reaped: false,
        slirp_expected: slirp,
    };
    container.child = Some(
        Command::new(&request.crun)
            .args(crun_prefix(request))
            .arg("run")
            .arg("--bundle")
            .arg(&prepared.bundle)
            .arg(&prepared.id)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?,
    );
    let child = container.child.as_mut().ok_or("container child")?;
    let stdout = child.stdout.take().ok_or("container stdout")?;
    let stderr = child.stderr.take().ok_or("container stderr")?;
    let result = tokio::time::timeout(timeout, async {
        let (stdout, stderr, status) = tokio::try_join!(
            bounded_probe_output(stdout),
            bounded_probe_output(stderr),
            async { child.wait().await.map_err(|error| error.to_string()) }
        )?;
        Ok::<_, String>(std::process::Output {
            status,
            stdout,
            stderr,
        })
    })
    .await
    .map_err(|_| "container timed out".to_string())
    .and_then(|result| result);
    let cleanup = container.finish().await;
    let output = match (result, cleanup) {
        (Ok(output), Ok(())) => output,
        (Err(reason), Ok(())) => return Err(reason),
        (result, Err(cleanup)) => {
            return Err(format!(
                "{}; cleanup unproven: {cleanup}",
                result.err().unwrap_or_else(|| "container completed".into())
            ))
        }
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text.push_str("\nREAPED\n");
    if !output.status.success() && !text.contains("ISOLATION_OK") {
        return Err(format!("container exit {}: {text}", output.status));
    }
    Ok(text)
}

async fn bounded_probe_output(
    reader: impl tokio::io::AsyncRead + Unpin,
) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    reader
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("container output limit".into());
    }
    Ok(bytes)
}

fn crun_prefix(request: &ProbeRequest) -> Vec<String> {
    vec![
        "--root".into(),
        request.runtime_root.join("state").display().to_string(),
        "--cgroup-manager".into(),
        "cgroupfs".into(),
    ]
}

struct PreparedBundle {
    bundle: PathBuf,
    id: String,
    home: PathBuf,
}

impl Drop for PreparedBundle {
    fn drop(&mut self) {
        remove_scratch_home(&self.home);
    }
}

/// Removes `path` if `prepare_bundle` returns before the bundle is live.
struct RemoveOnDrop {
    path: Option<PathBuf>,
}

impl RemoveOnDrop {
    fn arm(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(mut self) -> PathBuf {
        self.path.take().unwrap_or_default()
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            remove_scratch_home(&path);
        }
    }
}

fn mcp_env(token: &str, gateway: bool) -> Vec<serde_json::Value> {
    let mut env = vec![serde_json::json!({"name": ATTEMPT_TOKEN_ENV, "value": token})];
    if gateway {
        env.push(serde_json::json!({
            "name": "CODEG_RT_MODEL_SOCKET",
            "value": "/run/codeg/gateway.sock"
        }));
    }
    env
}

struct ProbeGateway {
    path: PathBuf,
    /// Kept alive until the ACP turn returns. Drop closes the socket.
    #[allow(dead_code)]
    server: LiveGatewayServer,
}

async fn start_probe_gateway(
    request: &ProbeRequest,
    attempt_bearer: &str,
) -> Result<ProbeGateway, String> {
    let profile = profile_for_agent(&request.agent).ok_or("adapter profile")?;
    let providers = read_providers(&request.provider_bindings).unwrap_or_default();
    let provider =
        provider_for_agent(&providers, &request.agent).ok_or("provider bindings missing")?;
    let files: Vec<PathBuf> = profile
        .auth_files
        .iter()
        .map(|file| request.home.join(file.home_relative))
        .collect();
    let upstream = resolve_model_upstream(ResolveInput {
        agent: &request.agent,
        binding_origin: &provider.origin,
        env_secret: std::env::var(&provider.credential_env).ok(),
        auth_files: &files,
        adapter_version: profile.version_needle,
        now_unix: unix_now(),
    })
    .await
    .map_err(|error| {
        error
            .details
            .reason
            .unwrap_or_else(|| "provider_credential_missing".into())
    })?;
    let gateway = std::sync::Arc::new(
        LiveModelGateway::native_probe(upstream, attempt_bearer.to_string()).map_err(|error| {
            error
                .details
                .reason
                .unwrap_or_else(|| "probe_gateway".into())
        })?,
    );
    let path = super::companion::transport::short_socket_path("g");
    super::companion::transport::ensure_unix_socket_path(&path.to_string_lossy()).map_err(
        |error| {
            error
                .details
                .reason
                .unwrap_or_else(|| "socket_path_too_long".into())
        },
    )?;
    let _ = fs::remove_file(&path);
    let server = LiveGatewayServer::bind(&path, gateway)
        .await
        .map_err(|error| {
            error
                .details
                .reason
                .unwrap_or_else(|| "probe_gateway".into())
        })?;
    Ok(ProbeGateway { path, server })
}

fn prepare_bundle(
    request: &ProbeRequest,
    argv: &[String],
    mounts: &[(PathBuf, String, bool)],
    secret: Option<&Path>,
    slirp: bool,
    token: Option<&str>,
    model_env: &[(String, String)],
    auth_overlays: &[(PathBuf, String)],
) -> Result<PreparedBundle, String> {
    let _ = fs::create_dir_all(request.runtime_root.join("state"));
    let id = if argv.iter().any(|arg| arg.contains("codeg-mcp")) {
        "cq-mcp".to_string()
    } else if argv.iter().any(|arg| arg.ends_with("probe.sh")) {
        "cq-isolation".to_string()
    } else if argv.iter().any(|arg| arg.ends_with("credential-canary.sh")) {
        "cq-canary".to_string()
    } else if argv.iter().any(|arg| arg.ends_with("egress.sh")) {
        "cq-egress".to_string()
    } else if argv.iter().any(|arg| arg == "--version") {
        "cq-version".to_string()
    } else {
        format!("cq-acp-{}", std::process::id())
    };
    // Concurrent probes must never reap one another's deterministic names.
    let id = format!("{id}-{}", uuid::Uuid::new_v4().simple());
    for path in [
        request.runtime_root.join("state").join(&id),
        request.cgroup_root.join(&id),
        request
            .runtime_root
            .join("slirp-pids")
            .join(format!("{id}.pid")),
    ] {
        if path.try_exists().map_err(|error| error.to_string())? {
            return Err("previous probe cleanup unproven".into());
        }
    }
    let bundle = request.runtime_root.join("bundles").join(&id);
    let _ = fs::remove_dir_all(&bundle);
    fs::create_dir_all(&bundle).map_err(|error| error.to_string())?;
    if mounts
        .iter()
        .any(|(_, destination, _)| destination.starts_with("/rt-home/"))
    {
        return Err("credential material cannot be mounted or copied into the sandbox".into());
    }
    let mut oci_mounts = super::sandbox::linux_runtime_mounts_json();
    let host_mounts = mounts;
    for (source, destination, read_only) in host_mounts {
        let mut options = vec!["bind", "nosuid", "nodev"];
        options.push(if *read_only { "ro" } else { "rw" });
        oci_mounts.push(serde_json::json!({
            "destination": destination,
            "type": "bind",
            "source": source,
            "options": options
        }));
    }
    let upper = scratch_home_dir(&request.runtime_root, &request.agent, &id);
    fs::create_dir_all(&upper).map_err(|error| error.to_string())?;
    let home_guard = RemoveOnDrop::arm(upper.clone());
    oci_mounts.push(serde_json::json!({
        "destination": "/rt-home",
        "type": "bind",
        "source": upper.clone(),
        "options": ["bind", "rw", "nosuid", "nodev"]
    }));
    append_host_auth_mounts(
        &mut oci_mounts,
        &upper,
        auth_overlays,
        select_profile(request),
    )?;
    let secret_note = secret
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let mut env = vec![
        "PATH=/usr/local/bin:/usr/bin:/bin".to_string(),
        "HOME=/rt-home".to_string(),
        "LANG=C".to_string(),
        format!("HOST_SECRET_PATH={secret_note}"),
        format!("HOST_HOME={}", request.home.display()),
    ];
    if let Some(token) = token {
        if token.is_empty() {
            return Err("missing_attempt_token".into());
        }
        env.push(format!("{ATTEMPT_TOKEN_ENV}={token}"));
    }
    if slirp {
        for (key, value) in model_env {
            if key != "PATH" && key != "HOME" && !value.is_empty() {
                env.push(format!("{key}={value}"));
            }
        }
    }
    if let Some(profile) = select_profile(request) {
        for (key, value) in profile.container_env {
            if *key != "PATH" && *key != "HOME" {
                env.push(format!("{key}={value}"));
            }
        }
    }
    let relative = request
        .cgroup_root
        .strip_prefix("/sys/fs/cgroup")
        .map_err(|_| "cgroup root must be under /sys/fs/cgroup".to_string())?;
    let cgroup_path = Path::new("/").join(relative).join(&id);
    let mut hooks = serde_json::json!({});
    let mut network = "isolated-deny-egress";
    if slirp {
        let slirp_bin = super::sandbox::linux_slirp_binary()
            .ok_or_else(|| "slirp4netns is not installed".to_string())?;
        let attached = super::sandbox::linux_attach_slirp(
            &request.runtime_root,
            &bundle.join("slirp-hook.sh"),
            &id,
            &slirp_bin,
        )
        .map_err(|error| {
            error
                .details
                .reason
                .unwrap_or_else(|| "slirp_hook".to_string())
        })?;
        hooks = attached.hooks;
        oci_mounts.push(attached.resolv_mount);
        network = attached.network;
    }
    let spec = serde_json::json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": {"uid": 0, "gid": 0},
            "args": argv,
            "env": env,
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
        "root": {"path": request.rootfs, "readonly": true},
        "mounts": oci_mounts,
        "hooks": hooks,
        "annotations": {"io.codeg.roundtable.network": network},
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
            "uidMappings": [{"containerID": 0, "hostID": current_uid(), "size": 1}],
            "gidMappings": [{"containerID": 0, "hostID": current_gid(), "size": 1}],
            "maskedPaths": ["/proc/acpi", "/proc/kcore", "/proc/keys"],
            "readonlyPaths": ["/proc/sys"],
            "cgroupsPath": cgroup_path,
            "seccomp": super::sandbox::linux_seccomp_json(),
            "resources": {
                "memory": {"limit": 512 * 1024 * 1024},
                "pids": {"limit": 64},
                "cpu": {"quota": 100000, "period": 100000},
                "devices": [{"allow": false, "access": "rwm"}]
            }
        }
    });
    fs::write(
        bundle.join("config.json"),
        serde_json::to_vec_pretty(&spec).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(PreparedBundle {
        bundle,
        id,
        home: home_guard.disarm(),
    })
}

fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn current_gid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getegid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn deferred_host_auth_checks() -> Vec<ProbeCheck> {
    [
        "model_credential_material_in_sandbox",
        "model_credentials_visible_to_agent",
        "native_read_boundary",
        "api_credential_scope",
    ]
    .into_iter()
    .map(|name| {
        let mut check = pass(
            name,
            "sandbox credential isolation is deferred; the CLI reads host auth files through a bind mount",
        );
        check.status = "not_applicable".into();
        check.flag = None;
        check
    })
    .collect()
}

fn host_auth_overlays(request: &ProbeRequest) -> Vec<(PathBuf, String)> {
    let Some(profile) = select_profile(request) else {
        return Vec::new();
    };
    profile
        .auth_files
        .iter()
        .filter_map(|file| {
            let source = request.home.join(file.home_relative);
            source
                .is_file()
                .then(|| (source, file.destination.to_string()))
        })
        .collect()
}

fn append_host_auth_mounts(
    oci_mounts: &mut Vec<serde_json::Value>,
    upper: &Path,
    overlays: &[(PathBuf, String)],
    profile: Option<&super::qualification_profiles::AdapterProfile>,
) -> Result<(), String> {
    let allowed: Vec<&str> = profile
        .map(|profile| {
            profile
                .auth_files
                .iter()
                .map(|file| file.destination)
                .collect()
        })
        .unwrap_or_default();
    for (source, destination) in overlays {
        if !allowed.iter().any(|item| *item == destination.as_str()) {
            return Err(format!(
                "auth destination is not in the adapter profile: {destination}"
            ));
        }
        if destination.contains("..") || !destination.starts_with("/rt-home/") {
            return Err("auth destination must be a file under /rt-home".into());
        }
        let source = source
            .canonicalize()
            .map_err(|error| format!("host auth file: {error}"))?;
        if !source.is_file() {
            return Err("host auth file is missing".into());
        }
        let relative = destination
            .strip_prefix("/rt-home/")
            .ok_or_else(|| "auth destination must be under /rt-home".to_string())?;
        let mount_point = upper.join(relative);
        if let Some(parent) = mount_point.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        if !mount_point.exists() {
            fs::write(&mount_point, b"").map_err(|error| error.to_string())?;
        }
        if fs::read(&mount_point)
            .map(|bytes| !bytes.is_empty())
            .unwrap_or(true)
        {
            return Err("auth mount point must stay an empty placeholder".into());
        }
        oci_mounts.push(serde_json::json!({
            "destination": destination,
            "type": "bind",
            "source": source,
            "options": ["bind", "rw", "nosuid", "nodev"]
        }));
    }
    Ok(())
}

fn select_profile(
    request: &ProbeRequest,
) -> Option<&'static super::qualification_profiles::AdapterProfile> {
    if let Some(id) = request.profile_id.as_deref() {
        return profile_by_id(id).filter(|profile| profile.agent == request.agent);
    }
    profile_for_agent(&request.agent)
}

fn mint_probe_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn rootfs_layout_error(rootfs: &Path) -> Option<String> {
    for relative in [
        "dev", "dev/pts", "dev/shm", "sys", "tmp", "proc", "scratch", "rt-home",
    ] {
        let path = rootfs.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => {
                return Some(format!(
                    "rootfs is missing directory {relative}; create it before the probe so crun does not write it into the image"
                ));
            }
        }
    }
    let resolv = rootfs.join("etc/resolv.conf");
    match fs::symlink_metadata(&resolv) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => None,
        _ => Some(
            "rootfs etc/resolv.conf must be an empty regular file, not a symlink, so the ACP turn can bind the slirp resolver over it"
                .to_string(),
        ),
    }
}

fn origin_host(origin: &str) -> Option<String> {
    let rest = origin.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split('@').next_back().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

async fn run_egress(
    request: &ProbeRequest,
    mounts: &[(PathBuf, String, bool)],
) -> Result<String, String> {
    let origin = request_origin_host(request)?;
    let scratch = request.runtime_root.join("probe-scratch");
    fs::create_dir_all(&scratch).map_err(|error| error.to_string())?;
    let script = scratch.join("egress.sh");
    fs::write(&script, EGRESS_SCRIPT).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&script, fs::Permissions::from_mode(0o755));
    }
    let mut egress_mounts = mounts.to_vec();
    if !egress_mounts
        .iter()
        .any(|(_, destination, _)| destination == "/scratch")
    {
        egress_mounts.insert(0, (scratch, "/scratch".into(), false));
    }
    let output = run_container(
        request,
        &[
            "/bin/bash".into(),
            "/scratch/egress.sh".into(),
            origin.clone(),
        ],
        &egress_mounts,
        None,
        Duration::from_secs(40),
        true,
        None,
    )
    .await?;
    let origin_ok = output.lines().any(|line| line == format!("REACH {origin}"));
    let other_open = output.lines().any(|line| line == "REACH 1.1.1.1");
    if !origin_ok {
        return Err(format!(
            "provider origin {origin}:443 was not reachable inside slirp; {output}"
        ));
    }
    let note = if other_open {
        format!(
            "slirp reached {origin}:443 and also 1.1.1.1:443; egress is not limited to the provider origin"
        )
    } else {
        format!(
            "slirp reached {origin}:443; 1.1.1.1:443 was blocked on this host. The probe does not claim an origin-only filter"
        )
    };
    Ok(note)
}

fn request_origin_host(request: &ProbeRequest) -> Result<String, String> {
    let bytes = fs::read(&request.provider_bindings)
        .map_err(|_| "provider bindings file missing".to_string())?;
    let providers: Vec<super::installed_runtime::ProviderBinding> =
        serde_json::from_slice(&bytes).map_err(|_| "provider bindings unreadable".to_string())?;
    let origin = provider_for_agent(&providers, &request.agent)
        .map(|provider| provider.origin.clone())
        .ok_or_else(|| "provider bindings missing".to_string())?;
    origin_host(&origin).ok_or_else(|| format!("provider origin is not an https host: {origin}"))
}

fn provider_for_agent<'a>(
    providers: &'a [ProviderBinding],
    agent: &str,
) -> Option<&'a ProviderBinding> {
    providers
        .iter()
        .find(|provider| {
            provider.provider_ref == format!("provider:{agent}")
                || provider.provider_ref.ends_with(&format!(":{agent}"))
        })
        .or_else(|| providers.first())
}

fn advertised_model_ids(session: &serde_json::Value) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    if let Some(id) = session
        .get("currentModelId")
        .and_then(|value| value.as_str())
    {
        if !id.is_empty() {
            ids.insert(id.to_string());
        }
    }
    if let Some(models) = session.get("models").and_then(|value| value.as_array()) {
        for model in models {
            for key in ["modelId", "id", "value"] {
                if let Some(id) = model.get(key).and_then(|value| value.as_str()) {
                    if !id.is_empty() {
                        ids.insert(id.to_string());
                    }
                }
            }
        }
    }
    if let Some(options) = session
        .get("configOptions")
        .and_then(|value| value.as_array())
    {
        for option in options {
            let id = option
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if !id.to_ascii_lowercase().contains("model") {
                continue;
            }
            if let Some(current) = option.get("currentValue").and_then(|value| value.as_str()) {
                if !current.is_empty() {
                    ids.insert(current.to_string());
                }
            }
            if let Some(values) = option.get("options").and_then(|value| value.as_array()) {
                for value in values {
                    if let Some(id) = value.get("value").and_then(|item| item.as_str()) {
                        if !id.is_empty() {
                            ids.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }
    ids
}

fn model_binding_error(
    agent: &str,
    session: &serde_json::Value,
    providers: &[ProviderBinding],
) -> Option<String> {
    let advertised = advertised_model_ids(session);
    if advertised.is_empty() {
        return Some("session/new did not advertise a model id".to_string());
    }
    let Some(provider) = provider_for_agent(providers, agent) else {
        return Some("provider bindings missing".to_string());
    };
    if advertised.contains(&provider.model) {
        None
    } else {
        Some(format!(
            "binding model {} is not advertised ({})",
            provider.model,
            advertised.into_iter().collect::<Vec<_>>().join(",")
        ))
    }
}

fn scratch_home_dir(runtime_root: &Path, agent: &str, container_id: &str) -> PathBuf {
    let agent: String = agent
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let agent = if agent.is_empty() {
        "adapter".to_string()
    } else {
        agent
    };
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    runtime_root
        .join("home-upper")
        .join(agent)
        .join(format!("{container_id}-{nonce}"))
}

fn remove_scratch_home(path: &Path) {
    if path.as_os_str().is_empty() {
        return;
    }
    let _ = fs::remove_dir_all(path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
}

/// `crun run` deletes a container that exits on its own. A later `crun delete`
/// then fails because the state directory is already gone. That is reaped
/// only when the container cgroup directory is gone too. A delete failure
/// while the state directory or the cgroup directory remains is not reaped.
fn classify_container_reap(
    delete_succeeded: bool,
    delete_stderr: &str,
    state_dir_exists: bool,
    cgroup_dir_exists: bool,
) -> Result<(), String> {
    let already_gone = !state_dir_exists && delete_reports_missing_container(delete_stderr);
    if !delete_succeeded && !already_gone {
        return Err(if delete_stderr.is_empty() {
            "crun delete failed".to_string()
        } else {
            delete_stderr.to_string()
        });
    }
    if state_dir_exists {
        return Err("container state directory still exists".into());
    }
    if cgroup_dir_exists {
        return Err("container cgroup directory still exists".to_string());
    }
    Ok(())
}

fn delete_reports_missing_container(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("cannot open directory")
        || lower.contains("no such file")
        || lower.contains("no such container")
        || lower.contains("container does not exist")
        || lower.contains("does not exist")
}

fn cgroup_dir_released(path: &Path) -> Result<bool, String> {
    for _ in 0..25 {
        if !path.try_exists().map_err(|error| error.to_string())? {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    Ok(!path.try_exists().map_err(|error| error.to_string())?)
}

fn bounded_reap_command(
    request: &ProbeRequest,
    tail: &[&str],
) -> Result<(std::process::ExitStatus, String), String> {
    use std::io::{Seek, SeekFrom};
    let mut stderr = tempfile::tempfile().map_err(|error| error.to_string())?;
    let mut child = std::process::Command::new(&request.crun)
        .args(crun_prefix(request))
        .args(tail)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr.try_clone().map_err(|error| error.to_string())?)
        .spawn()
        .map_err(|error| error.to_string())?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(result
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "container cleanup command timed out".into()));
            }
        }
    };
    stderr
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut text = String::new();
    stderr
        .take(64 * 1024)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    Ok((status, text))
}

fn reap_blocking(request: &ProbeRequest, id: &str, slirp_expected: bool) -> Result<(), String> {
    // The post-delete sweep is the proof and resumes an interrupted first
    // sweep. `crun delete` without `--force` refuses a container that is
    // still `running` (exec fifo already consumed) with "not in created or
    // stopped state". Kill is not ordered with that check.
    let _initial_helper_cleanup =
        super::sandbox::linux_stop_slirp(&request.runtime_root, id, slirp_expected);
    let _ = bounded_reap_command(request, &["kill", id, "KILL"]);
    let (status, stderr) = bounded_reap_command(request, &["delete", "--force", id])?;
    let state_dir = request.runtime_root.join("state").join(id);
    let cgroup_dir = request.cgroup_root.join(id);
    let cgroup_exists = !cgroup_dir_released(&cgroup_dir)?;
    let state_exists = state_dir.try_exists().map_err(|error| error.to_string())?;
    classify_container_reap(status.success(), &stderr, state_exists, cgroup_exists)?;
    super::sandbox::linux_stop_slirp(&request.runtime_root, id, slirp_expected).map_err(|error| {
        error
            .details
            .reason
            .unwrap_or_else(|| "slirp cleanup unproven".into())
    })
}

async fn reap(request: &ProbeRequest, id: &str, slirp_expected: bool) -> Result<(), String> {
    let request = request.clone();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || reap_blocking(&request, &id, slirp_expected))
        .await
        .unwrap_or_else(|error| Err(error.to_string()))
}

fn redact_keep(input: &str) -> String {
    input
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            !lower.contains("authorization") && !lower.contains("bearer") && !lower.contains("sk-")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(any(test, feature = "test-utils"))]
pub fn isolation_probe_script() -> &'static str {
    ISOLATION_SCRIPT
}

#[cfg(any(test, feature = "test-utils"))]
pub fn advertised_probe_models(session: &serde_json::Value) -> std::collections::BTreeSet<String> {
    advertised_model_ids(session)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn probe_model_binding_error(
    agent: &str,
    session: &serde_json::Value,
    providers: &[ProviderBinding],
) -> Option<String> {
    model_binding_error(agent, session, providers)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn probe_reap_classification(
    delete_succeeded: bool,
    delete_stderr: &str,
    state_dir_exists: bool,
    cgroup_dir_exists: bool,
) -> Result<(), String> {
    classify_container_reap(
        delete_succeeded,
        delete_stderr,
        state_dir_exists,
        cgroup_dir_exists,
    )
}

#[cfg(any(test, feature = "test-utils"))]
pub fn probe_scratch_home(runtime_root: &Path, agent: &str, container_id: &str) -> PathBuf {
    scratch_home_dir(runtime_root, agent, container_id)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn remove_probe_scratch_home(path: &Path) {
    remove_scratch_home(path);
}

#[cfg(any(test, feature = "test-utils"))]
pub fn probe_recorded_crun_version(text: &str) -> Option<String> {
    recorded_crun_version(text)
}

/// Same Drop as `run_acp_session`: returning `session/new rejected` still
/// stops slirp and deletes the container.
#[cfg(any(test, feature = "test-utils"))]
pub fn probe_acp_exit_cleans_container(request: &ProbeRequest, id: &str) -> Result<(), String> {
    PROBE_DROP_CLEANUP.with(|result| *result.borrow_mut() = None);
    drop(ProbeContainer {
        request,
        id: id.to_string(),
        child: None,
        reaped: false,
        slirp_expected: true,
    });
    let cleanup = PROBE_DROP_CLEANUP.with(|result| result.borrow_mut().take());
    let reason = match cleanup {
        Some(Ok(())) => "session/new rejected".into(),
        Some(Err(reason)) => format!("session/new rejected; cleanup: {reason}"),
        None => "session/new rejected; cleanup result missing".into(),
    };
    Err(reason)
}

const ISOLATION_SCRIPT: &str = r#"#!/bin/sh
fail() { echo "FAIL $1"; exit 1; }
if [ -r "$HOST_SECRET_PATH" ]; then fail host_secret; else echo "DENIED host_secret"; fi
if [ -d "$HOST_HOME" ] && [ -r "$HOST_HOME" ]; then fail host_home; else echo "DENIED host_home"; fi
if touch /etc/codeg-probe-write 2>/dev/null; then fail rootfs_write; else echo "DENIED rootfs_write"; fi
cmd=$(tr '\0' ' ' < /proc/1/cmdline 2>/dev/null || true)
case "$cmd" in
  *systemd*|*init*) fail proc_host ;;
  *) echo "PID1_OK"; echo "DENIED proc_host" ;;
esac
# crun masks /proc/kcore by bind-mounting /dev/null over it, so [ -r ] is true.
# A masked kcore is a char device 1:3 or a read that returns no bytes.
if [ -e /proc/kcore ]; then
  if [ -c /proc/kcore ]; then
    majmin=$(stat -c '%t:%T' /proc/kcore 2>/dev/null || true)
    [ "$majmin" = "1:3" ] || fail proc_kcore
  elif [ -r /proc/kcore ]; then
    bytes=$(dd if=/proc/kcore bs=1 count=1 2>/dev/null | wc -c | tr -d ' ')
    [ "$bytes" = "0" ] || fail proc_kcore
  fi
fi
if [ ! -d /sys/class/net ]; then fail network; fi
nets=$(ls -1 /sys/class/net 2>/dev/null || true)
case "$nets" in
  lo|lo\ *) echo "DENIED network" ;;
  *) fail network ;;
esac
if [ -r /dev/mem ] || [ -e /dev/sda ]; then fail device; else echo "DENIED device"; fi
echo canary > /scratch/canary || fail scratch
echo "ISOLATION_OK"
"#;

const EGRESS_SCRIPT: &str = r#"#!/bin/bash
origin="$1"
other="1.1.1.1"
try() {
  host="$1"
  i=0
  while [ "$i" -lt 20 ]; do
    if bash -c "echo >/dev/tcp/${host}/443" 2>/dev/null; then
      echo "REACH ${host}"
      return 0
    fi
    i=$((i + 1))
    sleep 0.5
  done
  echo "BLOCK ${host}"
  return 1
}
try "$origin" || true
try "$other" || true
exit 0
"#;

#[cfg(all(test, unix))]
mod cleanup_regressions {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct FakeRuntime {
        root: tempfile::TempDir,
        request: ProbeRequest,
    }

    impl FakeRuntime {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let crun = root.path().join("fake-crun");
            let log = root.path().join("calls");
            let pid = root.path().join("pid");
            fs::write(&crun, format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nfor arg in \"$@\"; do\n if [ \"$arg\" = run ]; then echo $$ > '{}'; exec sleep 30; fi\ndone\nexit 0\n", log.display(), pid.display()
            )).unwrap();
            fs::set_permissions(&crun, fs::Permissions::from_mode(0o700)).unwrap();
            let request = ProbeRequest {
                data_dir: root.path().to_path_buf(),
                agent: "grok".into(),
                rootfs: root.path().join("rootfs"),
                crun,
                // Only joined/read by the fake runner; never creates a cgroup.
                cgroup_root: PathBuf::from(format!(
                    "/sys/fs/cgroup/codeg-fake-{}",
                    uuid::Uuid::new_v4()
                )),
                runtime_root: root.path().join("runtime"),
                provider_bindings: root.path().join("bindings.json"),
                home: root.path().join("home"),
                profile_id: None,
            };
            Self { root, request }
        }
        fn calls(&self) -> String {
            fs::read_to_string(self.root.path().join("calls")).unwrap_or_default()
        }
        async fn wait_for_run(&self) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !self.root.path().join("pid").is_file() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        }
        fn assert_cleaned(&self, prefix: &str) {
            let calls = self.calls();
            let id = calls
                .lines()
                .find(|line| line.contains(" run --bundle "))
                .and_then(|line| line.split_whitespace().last())
                .expect("owned run id");
            assert!(id.starts_with(&format!("{prefix}-")), "{id}");
            assert!(calls.contains(&format!("kill {id} KILL")), "{calls}");
            assert!(calls.contains(&format!("delete --force {id}")), "{calls}");
            let pid = fs::read_to_string(self.root.path().join("pid"))
                .unwrap()
                .trim()
                .parse::<i32>()
                .unwrap();
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "launcher remains alive or unreaped"
            );
        }
    }
    impl Drop for FakeRuntime {
        fn drop(&mut self) {
            // The red test must not leak its stand-in even on assertion failure.
            if let Ok(pid) = fs::read_to_string(self.root.path().join("pid")) {
                if let Ok(pid) = pid.trim().parse::<i32>() {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn version_isolation_and_egress_timeouts_reap_the_owned_container() {
        for (arg, id) in [
            ("--version", "cq-version"),
            ("/scratch/probe.sh", "cq-isolation"),
            ("/scratch/egress.sh", "cq-egress"),
        ] {
            let fake = FakeRuntime::new();
            let error = run_container(
                &fake.request,
                &[arg.into()],
                &[],
                None,
                Duration::from_millis(50),
                false,
                None,
            )
            .await
            .unwrap_err();
            assert!(error.contains("timed out"), "{error}");
            fake.assert_cleaned(id);
        }
    }

    #[tokio::test]
    async fn cancelled_container_future_reaps_the_owned_container() {
        let fake = FakeRuntime::new();
        let request = fake.request.clone();
        let task = tokio::spawn(async move {
            run_container(
                &request,
                &["--version".into()],
                &[],
                None,
                Duration::from_secs(30),
                false,
                None,
            )
            .await
        });
        fake.wait_for_run().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        fake.assert_cleaned("cq-version");
    }

    #[tokio::test]
    async fn cancelled_mcp_future_reaps_the_owned_container() {
        let fake = FakeRuntime::new();
        let request = fake.request.clone();
        let task = tokio::spawn(async move { run_mcp(&request, "fake-only-token").await });
        fake.wait_for_run().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        fake.assert_cleaned("cq-mcp");
    }

    #[test]
    fn measured_failures_replace_unrelated_pass_observations() {
        let mut facts = empty_facts("grok");
        facts.checks.push(pass_flag(
            "model_credential_material_in_sandbox",
            "placeholder",
            false,
        ));
        stamp_failures(
            &mut facts,
            &[fail(
                "model_credential_material_in_sandbox",
                "nonempty placeholder",
            )],
        );
        assert_eq!(facts.checks.len(), 1);
        assert_eq!(facts.checks[0].status, "failed");
        stamp_failures(
            &mut facts,
            &[not_tested(
                "model_credential_material_in_sandbox",
                "later missing probe",
            )],
        );
        assert_eq!(
            facts.checks[0].status, "failed",
            "unknown cannot replace a measured failure"
        );
    }
}

#[cfg(test)]
mod host_auth_mounts {
    use super::*;

    #[test]
    fn host_auth_bind_follows_the_home_mount_and_does_not_copy_bytes() {
        let root = tempfile::tempdir().expect("temp");
        let upper = root.path().join("upper");
        fs::create_dir_all(&upper).expect("upper");
        let host = root.path().join("auth.json");
        let original = br#"{"access_token":"secret"}"#;
        fs::write(&host, original).expect("host auth");
        let profile = profile_for_agent("grok").expect("grok profile");
        let mut mounts = vec![serde_json::json!({"destination": "/rt-home"})];
        append_host_auth_mounts(
            &mut mounts,
            &upper,
            &[(host.clone(), "/rt-home/.grok/auth.json".into())],
            Some(profile),
        )
        .expect("overlay");
        assert_eq!(mounts[1]["destination"], "/rt-home/.grok/auth.json");
        assert_eq!(
            mounts[1]["source"],
            serde_json::json!(host.canonicalize().expect("canonical"))
        );
        assert_eq!(mounts[1]["options"][0], "bind");
        assert!(mounts[1]["options"]
            .as_array()
            .expect("options")
            .iter()
            .any(|option| option == "rw"));
        assert_eq!(
            fs::read(upper.join(".grok/auth.json")).expect("placeholder"),
            b""
        );
        assert_eq!(fs::read(&host).expect("host unchanged"), original);
        let error = append_host_auth_mounts(
            &mut mounts,
            &upper,
            &[(host, "/rt-home/.ssh/id_rsa".into())],
            Some(profile),
        )
        .expect_err("foreign destination");
        assert!(error.contains("not in the adapter profile"), "{error}");
    }

    #[test]
    fn antigravity_attempt_home_mounts_host_oauth_settings() {
        let root = tempfile::tempdir().expect("temp");
        let upper = root.path().join("upper");
        fs::create_dir_all(&upper).expect("upper");
        let host = root.path().join("settings.json");
        let oauth = br#"{"auth":{"type":"oauth-personal"}}"#;
        fs::write(&host, oauth).expect("host settings");
        let profile = profile_for_agent("antigravity").expect("antigravity");
        let destination = "/rt-home/.gemini/antigravity-acp/settings.json";
        let mut mounts = Vec::new();
        append_host_auth_mounts(
            &mut mounts,
            &upper,
            &[(host.clone(), destination.to_string())],
            Some(profile),
        )
        .expect("mount host settings");
        assert_eq!(mounts.len(), 1, "{mounts:?}");
        assert_eq!(mounts[0]["destination"], destination);
        assert_eq!(
            fs::read(upper.join(".gemini/antigravity-acp/settings.json")).expect("placeholder"),
            b""
        );
        assert_eq!(fs::read(&host).expect("host unchanged"), oauth);
        let token = root.path().join("acp_token.json");
        fs::write(&token, b"{\"access_token\":\"secret\"}").expect("token");
        append_host_auth_mounts(
            &mut mounts,
            &upper,
            &[(
                token,
                "/rt-home/.gemini/antigravity-acp/acp_token.json".into(),
            )],
            Some(profile),
        )
        .expect("token overlay");
        assert_eq!(mounts.len(), 2);
        assert_eq!(
            fs::read(upper.join(".gemini/antigravity-acp/settings.json")).expect("still empty"),
            b""
        );
    }
}
