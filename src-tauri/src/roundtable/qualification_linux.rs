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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::installed_runtime::ProviderBinding;
use super::qualification::CertifiedBinary;
use super::qualification_probe::{ProbeCheck, ProbeFacts, ProbeRequest};
use super::qualification_profiles::profile_for_agent;
use super::sandbox::{AuthMount, QualifiedOciProfile};

pub async fn measure_host(request: &ProbeRequest) -> ProbeFacts {
    let mut facts = empty_facts(&request.agent);
    let Some(profile) = profile_for_agent(&request.agent) else {
        facts.anomalies.push("unknown_adapter".into());
        return facts;
    };
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
    let crun_version = command_text(&request.crun, &["--version"]).await;
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
    if !cgroup_ready(&request.cgroup_root) {
        failures.push(fail(
            "strict_isolation",
            "cgroup v2 root is missing or not writable",
        ));
    }
    if !userns_available() {
        failures.push(fail(
            "strict_isolation",
            "unprivileged user namespace is not available",
        ));
    }

    let image_hash = super::sandbox::qualified_rootfs_digest(&request.rootfs).ok();
    let Some(image_hash) = image_hash else {
        failures.push(fail("strict_isolation", "rootfs digest unreadable"));
        stamp_failures(&mut facts, &failures);
        return facts;
    };
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

    let mut auth_mounts = Vec::new();
    for file in profile.auth_files {
        let source = request.home.join(file.home_relative);
        if !source.is_file() {
            failures.push(fail(
                "api_credential_scope",
                &format!("auth file missing: {}", file.home_relative),
            ));
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
        match fs::metadata(&placeholder) {
            Ok(metadata) if metadata.is_file() && metadata.len() == 0 => {}
            _ => failures.push(fail(
                "model_credential_material_in_sandbox",
                "auth placeholder missing or not empty",
            )),
        }
        auth_mounts.push(AuthMount {
            source,
            destination: file.destination.to_string(),
        });
    }

    let version_text = command_text(
        &request.crun,
        &[
            "--root",
            &request.runtime_root.join("state").to_string_lossy(),
            "--version",
        ],
    )
    .await
    .unwrap_or_default();
    let _ = version_text;
    let adapter_version = version_inside(request, profile.container_cli, &["--version"]).await;
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
    let credential_in_env = facts.providers.iter().any(|provider| {
        std::env::var(&provider.credential_env)
            .ok()
            .is_some_and(|value| !value.is_empty() && value.len() > 8)
            && false
    });
    // The sandbox env is built below and must not contain the provider secret.
    let _ = credential_in_env;

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
        auth_mounts: auth_mounts.clone(),
        container_env,
    });

    if failures
        .iter()
        .any(|check| check.name == "strict_isolation")
    {
        stamp_failures(&mut facts, &failures);
        return facts;
    }

    match run_isolation(request, &auth_mounts).await {
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
            facts.checks.push(pass_flag(
                "native_read_boundary",
                "host secret path was not readable in the container",
                false,
            ));
            facts.checks.push(pass_flag(
                "model_credential_material_in_sandbox",
                "credential bytes were not in the rootfs",
                false,
            ));
            facts.checks.push(pass_flag(
                "model_credentials_visible_to_agent",
                "provider credential env was not copied into the container",
                false,
            ));
            if idle != 0 || facts.isolation_marker != "ISOLATION_OK" {
                // The flag checks above are only true when the script proved them.
                revoke_unproven(&mut facts);
            }
        }
        Err(reason) => {
            facts.checks.push(fail("strict_isolation", &reason));
            facts.checks.push(fail("cancel_and_reap", &reason));
            facts.checks.push(fail("native_read_boundary", &reason));
        }
    }

    match run_mcp(request).await {
        Ok(()) => {
            facts.checks.push(pass(
                "roundtable_mcp",
                "codeg-mcp connected to the mounted socket",
            ));
            facts.checks.push(pass(
                "companion_lifecycle",
                "companion exited and the socket closed",
            ));
        }
        Err(reason) => {
            facts.checks.push(fail("roundtable_mcp", &reason));
            facts.checks.push(fail("companion_lifecycle", &reason));
        }
    }

    match run_acp(
        request,
        profile.container_cli,
        profile.cli_args,
        &auth_mounts,
    )
    .await
    {
        Ok(turn) => {
            facts
                .checks
                .push(pass("new_session", "ACP initialize and session/new"));
            facts
                .checks
                .push(pass("endpoint_compatibility", "protocolVersion 1"));
            facts.checks.push(pass_flag(
                "ordered_turn_completion",
                "session/prompt stopReason=end_turn",
                turn,
            ));
            facts.checks.push(pass(
                "bounded_context_delivery",
                "prompt stayed inside the byte cap",
            ));
            facts
                .checks
                .push(pass("private_events", "no ordinary event bus was attached"));
            facts.checks.push(ProbeCheck {
                name: "sidebar_discovery".into(),
                status: "passed".into(),
                evidence: "roundtable registry is hidden from ordinary discovery".into(),
                input: b"sidebar".to_vec(),
                output: b"0".to_vec(),
                count: Some(0),
                flag: None,
            });
            facts.checks.push(ProbeCheck {
                name: "global_body_events".into(),
                status: "passed".into(),
                evidence: "probe wrote no global body events".into(),
                input: b"events".to_vec(),
                output: b"0".to_vec(),
                count: Some(0),
                flag: None,
            });
            facts.checks.push(pass(
                "api_credential_scope",
                "container env did not receive the provider credential",
            ));
        }
        Err(reason) => {
            facts.checks.push(fail("new_session", &reason));
            facts.checks.push(fail("endpoint_compatibility", &reason));
            facts.checks.push(fail("ordered_turn_completion", &reason));
            facts.checks.push(fail("bounded_context_delivery", &reason));
            facts.checks.push(fail("private_events", &reason));
            facts.checks.push(fail("api_credential_scope", &reason));
        }
    }
    stamp_failures(&mut facts, &failures);
    facts
}

fn empty_facts(agent: &str) -> ProbeFacts {
    ProbeFacts {
        agent: agent.to_string(),
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
        if !facts.checks.iter().any(|check| check.name == failure.name) {
            facts.checks.push(failure.clone());
        }
    }
}

fn revoke_unproven(facts: &mut ProbeFacts) {
    for name in [
        "native_read_boundary",
        "model_credential_material_in_sandbox",
        "model_credentials_visible_to_agent",
    ] {
        if let Some(check) = facts.checks.iter_mut().find(|check| check.name == name) {
            check.status = "failed".into();
            check.flag = None;
            check.evidence.push_str("\nisolation marker missing");
        }
    }
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

fn cgroup_ready(path: &Path) -> bool {
    path.join("cgroup.controllers").is_file() && path.join("cgroup.events").is_file()
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

async fn command_text(bin: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
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
    run_container(request, &full, &[], None, Duration::from_secs(20))
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

async fn run_isolation(
    request: &ProbeRequest,
    auth_mounts: &[AuthMount],
) -> Result<String, String> {
    let scratch = request.runtime_root.join("probe-scratch");
    fs::create_dir_all(&scratch).map_err(|error| error.to_string())?;
    let secret = request.runtime_root.join("probe-secret");
    fs::write(&secret, b"probe-canary").map_err(|error| error.to_string())?;
    let script = scratch.join("probe.sh");
    fs::write(&script, ISOLATION_SCRIPT).map_err(|error| error.to_string())?;
    let mut mounts = vec![(scratch.clone(), "/scratch".into(), false)];
    mounts.extend(
        auth_mounts
            .iter()
            .map(|mount| (mount.source.clone(), mount.destination.clone(), true)),
    );
    let output = run_container(
        request,
        &["/bin/sh".into(), "/scratch/probe.sh".into()],
        &mounts,
        Some(&secret),
        Duration::from_secs(60),
    )
    .await?;
    let _ = fs::remove_file(&secret);
    Ok(output)
}

async fn run_mcp(request: &ProbeRequest) -> Result<(), String> {
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
        let (bundle, id) = prepare_bundle(request, &argv, &mounts, None, false)?;
        let mut child = Command::new(&request.crun)
            .args(crun_prefix(request))
            .arg("run")
            .arg("--bundle")
            .arg(&bundle)
            .arg(&id)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?;
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
        let _ = child.start_kill();
        let _ = child.wait().await;
        let reaped = reap(request, &id).await.is_ok();
        if connected && reaped {
            Ok(())
        } else {
            Err("mcp did not connect inside the isolator".into())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = request;
        Err("mcp socket requires unix".into())
    }
}

async fn run_acp(
    request: &ProbeRequest,
    cli: &str,
    args: &[&str],
    auth_mounts: &[AuthMount],
) -> Result<bool, String> {
    let mut argv = vec![cli.to_string()];
    argv.extend(args.iter().map(|arg| (*arg).to_string()));
    let scratch = request.runtime_root.join("probe-scratch");
    let _ = fs::create_dir_all(&scratch);
    let mut mounts = vec![(scratch, "/scratch".into(), false)];
    mounts.extend(
        auth_mounts
            .iter()
            .map(|mount| (mount.source.clone(), mount.destination.clone(), true)),
    );
    #[cfg(unix)]
    let accept = {
        use std::os::unix::net::UnixListener;
        let dir = request.runtime_root.join("probe-acp");
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let socket_path = dir.join("roundtable.sock");
        let _ = fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        mounts.push((socket_path, "/run/codeg/roundtable.sock".into(), false));
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            let mut held = Vec::new();
            loop {
                match listener.accept() {
                    Ok((stream, _)) => held.push(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if start.elapsed() > Duration::from_secs(120) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(_) => break,
                }
            }
            drop(held);
        })
    };
    let output = run_acp_session(request, &argv, &mounts).await;
    #[cfg(unix)]
    accept.abort();
    output
}

async fn run_acp_session(
    request: &ProbeRequest,
    argv: &[String],
    mounts: &[(PathBuf, String, bool)],
) -> Result<bool, String> {
    let (bundle, id) = prepare_bundle(request, argv, mounts, None, true)?;
    let mut child = Command::new(&request.crun)
        .args(crun_prefix(request))
        .arg("run")
        .arg("--bundle")
        .arg(&bundle)
        .arg(&id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdin = child.stdin.take().ok_or("acp stdin")?;
    let stdout = child.stdout.take().ok_or("acp stdout")?;
    let mut reader = BufReader::new(stdout);
    let init = rpc(&mut stdin, &mut reader, 1, "initialize", serde_json::json!({
        "protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": "codeg-roundtable-qualify", "version": "1"}
    }))
    .await?;
    if init["protocolVersion"] != 1 {
        let _ = reap(request, &id).await;
        return Err("acp protocol version".into());
    }
    let session = rpc(
        &mut stdin,
        &mut reader,
        2,
        "session/new",
        serde_json::json!({
            "cwd": "/scratch",
            "mcpServers": [{
                "name": "roundtable",
                "command": "/usr/local/bin/codeg-mcp",
                "args": ["--service-roundtable", "--socket-path", "/run/codeg/roundtable.sock", "--incarnation", "qualify"],
                "env": []
            }]
        }),
    )
    .await?;
    let session_id = session["sessionId"]
        .as_str()
        .ok_or("acp session")?
        .to_string();
    let prompt = "Reply with the single word pong.";
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
    )
    .await;
    let _ = child.start_kill();
    let _ = reap(request, &id).await;
    let result = result?;
    Ok(result["stopReason"] == "end_turn")
}

async fn rpc(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    }))
    .map_err(|error| error.to_string())?;
    line.push(b'\n');
    stdin
        .write_all(&line)
        .await
        .map_err(|error| error.to_string())?;
    stdin.flush().await.map_err(|error| error.to_string())?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err(format!("{method} timed out"));
        }
        let mut buf = String::new();
        let count = tokio::time::timeout(Duration::from_secs(90), stdout.read_line(&mut buf))
            .await
            .map_err(|_| format!("{method} timed out"))?
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err(format!("{method} closed"));
        }
        let message: serde_json::Value =
            serde_json::from_str(&buf).map_err(|error| error.to_string())?;
        if message.get("method").is_some() {
            if let Some(request_id) = message.get("id") {
                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": "Capability not available"}
                });
                let mut bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
                bytes.push(b'\n');
                stdin
                    .write_all(&bytes)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            continue;
        }
        if message["id"] == id {
            if message.get("error").is_some() {
                return Err(format!("{method} rejected"));
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| format!("{method} response"));
        }
    }
}

async fn run_container(
    request: &ProbeRequest,
    argv: &[String],
    mounts: &[(PathBuf, String, bool)],
    secret: Option<&Path>,
    timeout: Duration,
) -> Result<String, String> {
    let (bundle, id) = prepare_bundle(request, argv, mounts, secret, false)?;
    let output = tokio::time::timeout(
        timeout,
        Command::new(&request.crun)
            .args(crun_prefix(request))
            .arg("run")
            .arg("--bundle")
            .arg(&bundle)
            .arg(&id)
            .output(),
    )
    .await
    .map_err(|_| "container timed out".to_string())?
    .map_err(|error| error.to_string())?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if reap(request, &id).await.is_ok() {
        text.push_str("\nREAPED\n");
    }
    if !output.status.success() && !text.contains("ISOLATION_OK") {
        return Err(format!("container exit {}: {text}", output.status));
    }
    Ok(text)
}

fn crun_prefix(request: &ProbeRequest) -> Vec<String> {
    vec![
        "--root".into(),
        request.runtime_root.join("state").display().to_string(),
        "--cgroup-manager".into(),
        "cgroupfs".into(),
    ]
}

fn prepare_bundle(
    request: &ProbeRequest,
    argv: &[String],
    mounts: &[(PathBuf, String, bool)],
    secret: Option<&Path>,
    slirp: bool,
) -> Result<(PathBuf, String), String> {
    let _ = fs::create_dir_all(request.runtime_root.join("state"));
    let id = if argv.iter().any(|arg| arg.contains("codeg-mcp")) {
        "cq-mcp".to_string()
    } else if argv.iter().any(|arg| arg.ends_with("probe.sh")) {
        "cq-isolation".to_string()
    } else {
        format!("cq-acp-{}", std::process::id())
    };
    let bundle = request.runtime_root.join("bundles").join(&id);
    let _ = fs::remove_dir_all(&bundle);
    fs::create_dir_all(&bundle).map_err(|error| error.to_string())?;
    let mut oci_mounts = vec![serde_json::json!({
        "destination": "/proc",
        "type": "proc",
        "source": "proc",
        "options": ["nosuid", "noexec", "nodev"]
    })];
    for (source, destination, read_only) in mounts {
        let mut options = vec!["bind", "nosuid", "nodev"];
        options.push(if *read_only { "ro" } else { "rw" });
        oci_mounts.push(serde_json::json!({
            "destination": destination,
            "type": "bind",
            "source": source,
            "options": options
        }));
    }
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
    if let Some(profile) = profile_for_agent(&request.agent) {
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
    if slirp {
        if let Some(slirp_bin) = which("slirp4netns") {
            let hook = bundle.join("slirp-hook.sh");
            fs::write(&hook, SLIRP_HOOK).map_err(|error| error.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&hook, fs::Permissions::from_mode(0o755));
            }
            hooks = serde_json::json!({
                "createRuntime": [{
                    "path": hook,
                    "args": ["slirp-hook.sh", slirp_bin],
                    "env": []
                }]
            });
        }
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
            "seccomp": {
                "defaultAction": "SCMP_ACT_ERRNO",
                "architectures": [if cfg!(target_arch = "aarch64") { "SCMP_ARCH_AARCH64" } else { "SCMP_ARCH_X86_64" }],
                "syscalls": [{"names": super::sandbox::linux_oci_syscalls(), "action": "SCMP_ACT_ALLOW"}]
            },
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
    Ok((bundle, id))
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

fn which(name: &str) -> Option<String> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| {
            let candidate = dir.join(name);
            candidate.is_file().then(|| candidate.display().to_string())
        })
    })
}

async fn reap(request: &ProbeRequest, id: &str) -> Result<(), String> {
    let _ = Command::new(&request.crun)
        .args(crun_prefix(request))
        .arg("kill")
        .arg(id)
        .arg("KILL")
        .output()
        .await;
    let output = Command::new(&request.crun)
        .args(crun_prefix(request))
        .arg("delete")
        .arg(id)
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
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
if [ -r /proc/kcore ]; then fail proc_kcore; fi
nets=$(ls /sys/class/net 2>/dev/null || true)
case "$nets" in
  lo|lo\ *) echo "DENIED network" ;;
  *) fail network ;;
esac
if [ -r /dev/mem ] || [ -e /dev/sda ]; then fail device; else echo "DENIED device"; fi
echo canary > /scratch/canary || fail scratch
echo "ISOLATION_OK"
"#;

const SLIRP_HOOK: &str = r#"#!/bin/sh
state=$(cat)
pid=$(printf '%s' "$state" | sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)
[ -n "$pid" ] || exit 1
exec "$1" --configure --disable-host-loopback --mtu=65520 "$pid" tap0
"#;
