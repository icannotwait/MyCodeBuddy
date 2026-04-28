use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;

use crate::models::connection::{ConnectionConfig, SshAuthMethod};
use crate::remote::connection::ConnectionStatus;
use crate::remote::manager::RemoteConnectionManager;
use crate::web::event_bridge::{emit_event, EventEmitter};

pub const TEST_PROGRESS_EVENT: &str = "connection://test_progress";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStage {
    DnsResolve,
    TcpConnect,
    SshAuth,
    RemoteShell,
    DaemonPathWritable,
    /// Real implementation lands in CG-002.4 (bootstrap orchestrator);
    /// CG-002.1 emits this stage as `Skipped` so the UI can render the
    /// progress timeline without surprises later.
    DaemonProbe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Pending,
    Running,
    Success,
    Failure,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    pub stage: TestStage,
    pub status: StageStatus,
    pub elapsed_ms: u64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct StageProgressPayload<'a> {
    pub test_id: &'a str,
    pub connection_id: &'a str,
    #[serde(flatten)]
    pub result: &'a StageResult,
}

/// Run the staged connection test and emit per-stage progress on
/// `connection://test_progress`. Returns the full result vector after the
/// test finishes (or aborts on the first hard failure).
pub async fn run_test(
    config: &ConnectionConfig,
    emitter: &EventEmitter,
    test_id: &str,
    rcm: Option<&RemoteConnectionManager>,
) -> Vec<StageResult> {
    let mut results: Vec<StageResult> = Vec::with_capacity(6);

    // Stage 1: DNS resolution (skip when alias mode — system ssh handles it).
    let dns_skip_reason = if config.ssh_alias.is_some() && config.ssh_host.is_none() {
        Some("alias mode — DNS handled by system ssh")
    } else if config.ssh_host.is_none() {
        Some("ssh_host or ssh_alias required")
    } else {
        None
    };
    if let Some(reason) = dns_skip_reason {
        record(
            &mut results,
            emitter,
            config,
            test_id,
            TestStage::DnsResolve,
            StageStatus::Skipped,
            0,
            Some(reason.to_string()),
        );
    } else if let Some(host) = config.ssh_host.clone() {
        let port = config.ssh_port.unwrap_or(22);
        let started = Instant::now();
        emit_running(emitter, config, test_id, TestStage::DnsResolve);
        let outcome = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map(|_| ())
            .map_err(|e| format!("DNS resolution failed: {e}"));
        let elapsed = started.elapsed().as_millis() as u64;
        let (status, message) = match outcome {
            Ok(()) => (StageStatus::Success, None),
            Err(e) => (StageStatus::Failure, Some(e)),
        };
        record(
            &mut results,
            emitter,
            config,
            test_id,
            TestStage::DnsResolve,
            status,
            elapsed,
            message,
        );
        if status == StageStatus::Failure {
            skip_remaining(&mut results, emitter, config, test_id, TestStage::TcpConnect);
            return results;
        }
    }

    // Stage 2: TCP (skip in alias mode — port handled by ssh_config).
    if config.ssh_host.is_some() {
        let host = config.ssh_host.clone().unwrap();
        let port = config.ssh_port.unwrap_or(22);
        let started = Instant::now();
        emit_running(emitter, config, test_id, TestStage::TcpConnect);
        let outcome = match timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(format!("TCP connect failed: {e}")),
            Err(_) => Err("TCP connect timeout (10s)".to_string()),
        };
        let elapsed = started.elapsed().as_millis() as u64;
        let (status, message) = match outcome {
            Ok(()) => (StageStatus::Success, None),
            Err(e) => (StageStatus::Failure, Some(e)),
        };
        record(
            &mut results,
            emitter,
            config,
            test_id,
            TestStage::TcpConnect,
            status,
            elapsed,
            message,
        );
        if status == StageStatus::Failure {
            skip_remaining(&mut results, emitter, config, test_id, TestStage::SshAuth);
            return results;
        }
    } else {
        record(
            &mut results,
            emitter,
            config,
            test_id,
            TestStage::TcpConnect,
            StageStatus::Skipped,
            0,
            Some("alias mode".to_string()),
        );
    }

    // Stage 3: SSH auth.
    let ssh_args_base = build_ssh_args(config);
    let mut auth_args = ssh_args_base.clone();
    auth_args.push("true".to_string());
    let started = Instant::now();
    emit_running(emitter, config, test_id, TestStage::SshAuth);
    let auth_outcome = run_ssh(&auth_args).await;
    let elapsed = started.elapsed().as_millis() as u64;
    let (status, message) = match &auth_outcome {
        Ok(()) => (StageStatus::Success, None),
        Err(e) => (StageStatus::Failure, Some(e.clone())),
    };
    record(
        &mut results,
        emitter,
        config,
        test_id,
        TestStage::SshAuth,
        status,
        elapsed,
        message,
    );
    if status == StageStatus::Failure {
        skip_remaining(&mut results, emitter, config, test_id, TestStage::RemoteShell);
        return results;
    }

    // Stage 4: Remote shell — implicit from SSH auth success.
    record(
        &mut results,
        emitter,
        config,
        test_id,
        TestStage::RemoteShell,
        StageStatus::Success,
        0,
        Some("implicit from SSH auth".to_string()),
    );

    // Stage 5: daemon path writable. Build a one-shot remote command that
    // creates and removes a probe directory. The path comes from user input,
    // so we pass it through POSIX single-quote escaping to defang shell
    // metacharacters.
    let daemon_path = expand_tilde_in_remote_path(&config.daemon_path);
    let probe_dir = format!("{}/.codeg-test-{}", daemon_path, Uuid::new_v4().simple());
    let probe_dir_quoted = posix_single_quote(&probe_dir);
    let remote_cmd = format!(
        "mkdir -p {q} && rmdir {q}",
        q = probe_dir_quoted
    );
    let mut write_args = ssh_args_base.clone();
    write_args.push(remote_cmd);
    let started = Instant::now();
    emit_running(emitter, config, test_id, TestStage::DaemonPathWritable);
    let write_outcome = run_ssh(&write_args).await;
    let elapsed = started.elapsed().as_millis() as u64;
    let (status, message) = match write_outcome {
        Ok(()) => (StageStatus::Success, None),
        Err(e) => (StageStatus::Failure, Some(e)),
    };
    record(
        &mut results,
        emitter,
        config,
        test_id,
        TestStage::DaemonPathWritable,
        status,
        elapsed,
        message,
    );

    // Stage 6: daemon probe — drives the real bootstrap pipeline (probe →
    // deploy → launch → tunnel → handshake → live) via the connection
    // manager, then immediately disconnects. The pipeline runs the same code
    // as `open_connection`, so we validate it end-to-end with no separate
    // mock path. If no manager was supplied (e.g. the standalone server
    // mode where SSH bootstrap doesn't apply), we skip the stage.
    let probe_result = run_daemon_probe(config, emitter, test_id, rcm).await;
    let (status, message, elapsed) = probe_result;
    record(
        &mut results,
        emitter,
        config,
        test_id,
        TestStage::DaemonProbe,
        status,
        elapsed,
        message,
    );

    results
}

async fn run_daemon_probe(
    config: &ConnectionConfig,
    emitter: &EventEmitter,
    test_id: &str,
    rcm: Option<&RemoteConnectionManager>,
) -> (StageStatus, Option<String>, u64) {
    let Some(rcm) = rcm else {
        return (
            StageStatus::Skipped,
            Some("connection manager unavailable in this build mode".into()),
            0,
        );
    };
    emit_running(emitter, config, test_id, TestStage::DaemonProbe);
    let started = Instant::now();

    // Kick off the connect; this is fire-and-forget at the channel level.
    if let Err(e) = rcm.connect(config.clone()).await {
        return (
            StageStatus::Failure,
            Some(format!("failed to start bootstrap: {e}")),
            started.elapsed().as_millis() as u64,
        );
    }

    // Poll the runtime state until it settles into Live or Error, with a 90s
    // ceiling that comfortably spans first-time deploys (~remote curl + tar).
    const POLL_INTERVAL: Duration = Duration::from_millis(250);
    const PROBE_TIMEOUT: Duration = Duration::from_secs(90);
    let outcome: (StageStatus, Option<String>) = loop {
        if started.elapsed() > PROBE_TIMEOUT {
            break (
                StageStatus::Failure,
                Some("daemon probe timed out (90s)".into()),
            );
        }
        match rcm.current_runtime(&config.id).await {
            Some(rt) => match rt.status {
                ConnectionStatus::Live => break (StageStatus::Success, None),
                ConnectionStatus::Error => {
                    break (
                        StageStatus::Failure,
                        rt.last_error
                            .clone()
                            .or_else(|| Some("daemon probe failed".into())),
                    )
                }
                ConnectionStatus::AwaitingManual => {
                    break (
                        StageStatus::Failure,
                        Some(
                            "daemon binary not present; manual deployment required".into(),
                        ),
                    )
                }
                _ => {}
            },
            None => break (StageStatus::Failure, Some("connection task not found".into())),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    // Always release the bootstrap once the test is over — this is just a
    // probe, the real lifetime is owned by `open_connection`.
    let _ = rcm.disconnect(&config.id).await;

    (outcome.0, outcome.1, started.elapsed().as_millis() as u64)
}

/// Build the common SSH args list (everything before the remote command).
fn build_ssh_args(config: &ConnectionConfig) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
    ];

    // Port: honor explicit config port unless we're using an alias (in which
    // case ~/.ssh/config supplies it).
    if let Some(port) = config.ssh_port {
        if config.ssh_alias.is_none() {
            args.push("-p".into());
            args.push(port.to_string());
        }
    }
    // Identity file: only meaningful for `Key` auth method.
    if config.ssh_auth_method == SshAuthMethod::Key {
        if let Some(key) = &config.ssh_key_path {
            let expanded = expand_tilde_in_local_path(key);
            crate::remote::ssh_process::ensure_key_permissions(&expanded);
            args.push("-i".into());
            args.push(expanded);
        }
    }
    // ProxyJump: pass through to system ssh.
    if let Some(jump) = &config.proxy_jump {
        if !jump.trim().is_empty() {
            args.push("-J".into());
            args.push(jump.clone());
        }
    }

    args.push(build_ssh_target(config));
    args
}

fn build_ssh_target(config: &ConnectionConfig) -> String {
    if let Some(alias) = &config.ssh_alias {
        return alias.clone();
    }
    let user = config.ssh_user.as_deref().unwrap_or("");
    let host = config.ssh_host.as_deref().unwrap_or("");
    if user.is_empty() {
        host.to_string()
    } else {
        format!("{user}@{host}")
    }
}

async fn run_ssh(args: &[String]) -> Result<(), String> {
    let output = Command::new("ssh")
        .args(args)
        .output()
        .await
        .map_err(|e| format!("spawn ssh failed: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "ssh exited with status {}: {}",
            output.status,
            stderr.trim()
        ))
    }
}

/// Resolve `~` / `~/...` against the local home directory.
/// We pass *resolved* paths to `-i` since system ssh expands tilde in
/// most environments but it's safer to pre-resolve.
fn expand_tilde_in_local_path(path: &str) -> String {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped).to_string_lossy().into_owned();
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

/// Leave `~` in remote paths intact — they are interpreted by the remote
/// shell, not the local one. Just trim leading whitespace.
fn expand_tilde_in_remote_path(path: &str) -> String {
    path.trim().to_string()
}

/// POSIX single-quote escape: wrap the string in `'...'`, replacing every
/// embedded `'` with `'\''`. Safe to embed in shell commands.
fn posix_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn emit_running(
    emitter: &EventEmitter,
    config: &ConnectionConfig,
    test_id: &str,
    stage: TestStage,
) {
    let result = StageResult {
        stage,
        status: StageStatus::Running,
        elapsed_ms: 0,
        message: None,
    };
    emit_event(
        emitter,
        TEST_PROGRESS_EVENT,
        StageProgressPayload {
            test_id,
            connection_id: &config.id,
            result: &result,
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn record(
    results: &mut Vec<StageResult>,
    emitter: &EventEmitter,
    config: &ConnectionConfig,
    test_id: &str,
    stage: TestStage,
    status: StageStatus,
    elapsed_ms: u64,
    message: Option<String>,
) {
    let result = StageResult {
        stage,
        status,
        elapsed_ms,
        message,
    };
    emit_event(
        emitter,
        TEST_PROGRESS_EVENT,
        StageProgressPayload {
            test_id,
            connection_id: &config.id,
            result: &result,
        },
    );
    results.push(result);
}

/// On hard failure, record `Skipped` for all subsequent stages so the UI
/// can render the timeline consistently.
fn skip_remaining(
    results: &mut Vec<StageResult>,
    emitter: &EventEmitter,
    config: &ConnectionConfig,
    test_id: &str,
    starting_at: TestStage,
) {
    let order = [
        TestStage::DnsResolve,
        TestStage::TcpConnect,
        TestStage::SshAuth,
        TestStage::RemoteShell,
        TestStage::DaemonPathWritable,
        TestStage::DaemonProbe,
    ];
    let from = order.iter().position(|s| *s == starting_at).unwrap_or(0);
    for stage in order.iter().skip(from) {
        record(
            results,
            emitter,
            config,
            test_id,
            *stage,
            StageStatus::Skipped,
            0,
            Some("skipped after earlier failure".to_string()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ConnectionConfig {
        ConnectionConfig {
            id: "conn_test".into(),
            name: "test".into(),
            kind: crate::models::connection::ConnectionKind::Ssh,
            ssh_host: Some("example.com".into()),
            ssh_user: Some("alice".into()),
            ssh_port: Some(2222),
            ssh_alias: None,
            ssh_key_path: Some("~/.ssh/id_ed25519".into()),
            ssh_auth_method: SshAuthMethod::Key,
            proxy_jump: None,
            daemon_path: "~/.codeg-remote".into(),
            daemon_version: None,
            auto_connect: false,
            last_connected_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn ssh_args_explicit_host_port_user() {
        let args = build_ssh_args(&cfg());
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"2222".to_string()));
        assert!(args.contains(&"-i".to_string()));
        assert!(args.last().unwrap() == "alice@example.com");
    }

    #[test]
    fn ssh_args_alias_skips_port() {
        let mut c = cfg();
        c.ssh_host = None;
        c.ssh_alias = Some("dev".into());
        c.ssh_port = Some(2222); // user typed but alias overrides
        let args = build_ssh_args(&c);
        assert!(!args.contains(&"-p".to_string()));
        assert_eq!(args.last().unwrap(), "dev");
    }

    #[test]
    fn ssh_args_password_method_skips_identity_file() {
        let mut c = cfg();
        c.ssh_auth_method = SshAuthMethod::Password;
        let args = build_ssh_args(&c);
        assert!(!args.contains(&"-i".to_string()));
    }

    #[test]
    fn ssh_args_proxy_jump_added() {
        let mut c = cfg();
        c.proxy_jump = Some("bastion".into());
        let args = build_ssh_args(&c);
        let i = args.iter().position(|a| a == "-J").unwrap();
        assert_eq!(args[i + 1], "bastion");
    }

    #[test]
    fn ssh_target_user_at_host() {
        let c = cfg();
        assert_eq!(build_ssh_target(&c), "alice@example.com");
    }

    #[test]
    fn ssh_target_alias_takes_precedence() {
        let mut c = cfg();
        c.ssh_alias = Some("dev".into());
        assert_eq!(build_ssh_target(&c), "dev");
    }

    #[test]
    fn ssh_target_no_user() {
        let mut c = cfg();
        c.ssh_user = None;
        assert_eq!(build_ssh_target(&c), "example.com");
    }

    #[test]
    fn posix_quote_basic() {
        assert_eq!(posix_single_quote("safe"), "'safe'");
        assert_eq!(
            posix_single_quote("/tmp/with space/path"),
            "'/tmp/with space/path'"
        );
    }

    #[test]
    fn posix_quote_escapes_apostrophe() {
        assert_eq!(posix_single_quote("ab'cd"), "'ab'\\''cd'");
    }

    #[test]
    fn local_tilde_expansion() {
        // We can't reliably assert the exact expanded path on every platform,
        // but it must NOT start with "~" any more.
        let expanded = expand_tilde_in_local_path("~/foo");
        assert!(!expanded.starts_with('~'));
    }

    #[test]
    fn remote_tilde_left_intact() {
        assert_eq!(expand_tilde_in_remote_path("~/.codeg-remote"), "~/.codeg-remote");
        assert_eq!(expand_tilde_in_remote_path("  /opt/foo "), "/opt/foo");
    }
}
