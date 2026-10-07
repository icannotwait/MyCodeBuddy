//! Service and legacy companion entry.
//!
//! Legacy parent mode keeps the existing argv token and optional parent PID.
//! Service mode receives the attempt token only through the child environment,
//! watches broker EOF, and does not read the host parent PID.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use roundtable_protocol::{ErrorCode, QualificationStatus, RtResult, ServiceOwner};

use crate::acp::manager::ConnectionOwner;
use tokio::sync::Notify;

use crate::acp::delegation::companion::{CompanionContext, CompanionFeatures};
use crate::acp::delegation::transport::CompanionRole;

use super::capabilities::{self, LaunchCapabilityManifestV1};
use super::rt_error;
use super::tool_core::AttemptToken;

pub const ATTEMPT_TOKEN_ENV: &str = "CODEG_ROUNDTABLE_ATTEMPT_TOKEN";

pub(super) mod transport;
pub use transport::{ServiceBroker, ServiceConnection};

const LEGACY_HELP: &str = "codeg-mcp --parent-connection-id <uuid> --socket-path <path> --token <secret> [--parent-pid <pid>] [--features delegation,coordination_v1,feedback,ask,sessions,workflow_v2] [--role root|delegation_child] [--can-spawn-child true|false] [--disabled-agents <agent>,...] [--custom-agents <ignored>]";

const LEGACY_TOOL_CATALOG: &[&str] = &[
    "delegate_to_agent",
    "continue_delegation",
    "resume_delegation",
    "get_delegation_status",
    "cancel_delegation",
    "request_parent_decision",
    "complete_work",
    "reply_to_delegation",
    "get_delegation_orchestration_bindings",
    "register_simple_workflow",
    "request_recovery_authorization",
    "get_workflow_capabilities",
    "get_workflow_state",
    "publish_workflow_manifest",
    "settle_workflow_gate",
    "recover_workflow",
    "check_user_feedback",
    "ask_user_question",
    "get_session_info",
    "task_progress",
    "task_complete",
    "create_automation",
    "create_work_task",
    "browser_list_tabs",
    "browser_snapshot",
    "browser_console_messages",
    "browser_screenshot",
    "browser_click",
    "browser_hover",
    "browser_type",
    "browser_press_key",
    "browser_select_option",
    "browser_open_tab",
    "browser_navigate",
    "browser_close_tab",
    "browser_eval",
    "computer_list_apps",
    "computer_list_windows",
    "computer_screenshot",
    "computer_snapshot",
    "computer_verify",
    "computer_click",
    "computer_drag",
    "computer_scroll",
    "computer_type",
    "computer_press_key",
    "computer_hold_key",
    "computer_set_value",
    "computer_restore",
    "computer_invoke_menu",
    "computer_launch_app",
    "computer_set_window_frame",
    "computer_clipboard_read",
    "computer_clipboard_write",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompanionParse {
    Help(String),
    Launch(CompanionMode),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompanionMode {
    LegacyParent(LegacyParentArgs),
    ServiceRoundtable {
        socket_path: String,
        incarnation: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyParentArgs {
    pub parent_connection_id: String,
    pub socket_path: String,
    pub token: String,
    pub parent_pid: Option<u32>,
    pub features: Option<String>,
    pub role: CompanionRole,
    pub can_spawn_child: bool,
    pub connection_incarnation_id: String,
    pub disabled_agents: Option<String>,
}

pub fn parse_companion_args<I, S>(args: I) -> Result<CompanionParse, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--service-roundtable") {
        parse_service(&args)
    } else {
        parse_legacy(&args)
    }
}

pub fn legacy_companion_context(args: &LegacyParentArgs) -> CompanionContext {
    CompanionContext {
        parent_connection_id: args.parent_connection_id.clone(),
        socket_path: args.socket_path.clone(),
        token: args.token.clone(),
        features: CompanionFeatures::parse(args.features.as_deref()),
        role: args.role,
        can_spawn_child: args.can_spawn_child,
        connection_incarnation_id: args.connection_incarnation_id.clone(),
        disabled_agents: split_csv(args.disabled_agents.as_deref()),
    }
}

pub fn advertised_tools(mode: &CompanionMode) -> Vec<String> {
    match mode {
        CompanionMode::ServiceRoundtable { .. } => service_names(),
        CompanionMode::LegacyParent(args) => legacy_names(args),
    }
}

pub fn callable_tools(mode: &CompanionMode) -> Vec<String> {
    advertised_tools(mode)
}

pub fn tool_callable(mode: &CompanionMode, name: &str) -> bool {
    match mode {
        CompanionMode::ServiceRoundtable { .. } => service_names().iter().any(|tool| tool == name),
        CompanionMode::LegacyParent(args) => legacy_companion_context(args).allows_tool(name),
    }
}

pub fn service_tools_ignoring_host_flags(flags: &[&str]) -> Vec<String> {
    let ordinary = capabilities::OrdinarySessionFlags::from_names(flags);
    let manifest = capabilities::service_manifest_for_session(&ordinary);
    if capabilities::verify_service_manifest(&manifest, &capabilities::ManifestExtras::default())
        .is_err()
    {
        return Vec::new();
    }
    manifest.tool_names
}

pub struct ServiceLaunchInput<'a> {
    pub token: &'a AttemptToken,
    pub socket_path: &'a str,
    pub incarnation: &'a str,
    pub billing_credential: &'a str,
}

pub struct ServiceLaunchPlan {
    pub argv: Vec<String>,
    pub sandbox_env: BTreeMap<String, String>,
    pub prompt: String,
    pub log: String,
    pub request_url: String,
    pub manifest: LaunchCapabilityManifestV1,
}

impl std::fmt::Debug for ServiceLaunchPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys = self.sandbox_env.keys().collect::<Vec<_>>();
        formatter
            .debug_struct("ServiceLaunchPlan")
            .field("argv", &self.argv)
            .field("sandbox_env_keys", &keys)
            .field("prompt", &self.prompt)
            .field("log", &self.log)
            .field("request_url", &self.request_url)
            .field("manifest", &self.manifest)
            .finish()
    }
}

pub fn plan_service_launch(input: &ServiceLaunchInput<'_>) -> ServiceLaunchPlan {
    let mut sandbox_env = BTreeMap::new();
    let token = input.token.reveal_for_same_sandbox();
    if token != input.billing_credential {
        sandbox_env.insert(ATTEMPT_TOKEN_ENV.to_string(), token.to_string());
    }
    let manifest = capabilities::sealed_service_manifest();
    let verified =
        capabilities::verify_service_manifest(&manifest, &capabilities::ManifestExtras::default())
            .is_ok();
    ServiceLaunchPlan {
        argv: if verified {
            vec![
                "codeg-mcp".to_string(),
                "--service-roundtable".to_string(),
                "--socket-path".to_string(),
                input.socket_path.to_string(),
                "--incarnation".to_string(),
                input.incarnation.to_string(),
            ]
        } else {
            Vec::new()
        },
        sandbox_env,
        prompt: "roundtable service prompt".to_string(),
        log: "service companion broker watch".to_string(),
        request_url: String::new(),
        manifest,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceWatchState {
    Running,
    Terminated { reason: &'static str },
}

pub struct FakeBrokerTransport {
    socket_path: String,
    incarnation: String,
    open: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl FakeBrokerTransport {
    pub fn open(socket_path: impl Into<String>, incarnation: impl Into<String>) -> Self {
        Self {
            socket_path: socket_path.into(),
            incarnation: incarnation.into(),
            open: Arc::new(AtomicBool::new(true)),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn watch(&self) -> ServiceWatchdog {
        ServiceWatchdog {
            socket_path: self.socket_path.clone(),
            incarnation: self.incarnation.clone(),
            open: Arc::clone(&self.open),
            notify: Arc::clone(&self.notify),
        }
    }

    pub fn certificate_status(&self) -> QualificationStatus {
        QualificationStatus::NotTested
    }

    pub fn is_certificate(&self) -> bool {
        false
    }
}

pub struct ServiceWatchdog {
    socket_path: String,
    incarnation: String,
    open: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl ServiceWatchdog {
    pub fn reads_host_parent_pid(&self) -> bool {
        false
    }

    pub fn parent_pid(&self) -> Option<u32> {
        None
    }

    pub fn socket_path(&self) -> &str {
        &self.socket_path
    }

    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }

    pub fn poll(&self) -> ServiceWatchState {
        if self.open.load(Ordering::SeqCst) {
            ServiceWatchState::Running
        } else {
            ServiceWatchState::Terminated {
                reason: "broker_eof",
            }
        }
    }

    pub async fn closed(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            if !self.open.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

pub struct ServiceProcess {
    token: String,
    socket_path: String,
    incarnation: String,
}

impl ServiceProcess {
    #[cfg(test)]
    pub(crate) fn for_experiment(socket_path: String, incarnation: String, token: String) -> Self {
        Self {
            token,
            socket_path,
            incarnation,
        }
    }

    pub fn reads_host_parent_pid(&self) -> bool {
        false
    }

    pub fn parent_pid(&self) -> Option<u32> {
        None
    }

    pub fn token_is_present(&self) -> bool {
        !self.token.is_empty()
    }

    pub fn certificate_status(&self) -> QualificationStatus {
        QualificationStatus::NotTested
    }

    pub async fn connect(&self) -> RtResult<ServiceConnection> {
        transport::connect(&self.socket_path, &self.incarnation, &self.token).await
    }
}

impl std::fmt::Debug for ServiceProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceProcess")
            .field("socket_path", &self.socket_path)
            .field("incarnation", &self.incarnation)
            .field("reads_host_parent_pid", &false)
            .field("certificate", &"not_tested")
            .finish()
    }
}

pub fn bind_service_process(
    socket_path: &str,
    incarnation: &str,
    env: &HashMap<String, String>,
) -> RtResult<ServiceProcess> {
    let token = env
        .get(ATTEMPT_TOKEN_ENV)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| rt_error(ErrorCode::Unauthenticated, "missing_attempt_token"))?;
    let manifest = capabilities::sealed_service_manifest();
    capabilities::verify_service_manifest(&manifest, &capabilities::ManifestExtras::default())?;
    Ok(ServiceProcess {
        token,
        socket_path: socket_path.to_owned(),
        incarnation: incarnation.to_owned(),
    })
}

fn service_names() -> Vec<String> {
    let manifest = capabilities::sealed_service_manifest();
    if capabilities::verify_service_manifest(&manifest, &capabilities::ManifestExtras::default())
        .is_err()
    {
        return Vec::new();
    }
    manifest.tool_names
}

fn legacy_names(args: &LegacyParentArgs) -> Vec<String> {
    let context = legacy_companion_context(args);
    LEGACY_TOOL_CATALOG
        .iter()
        .filter(|name| context.allows_tool(name))
        .map(|name| (*name).to_string())
        .collect()
}

fn split_csv(raw: Option<&str>) -> Vec<String> {
    raw.map(|csv| {
        csv.split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

fn parse_role(raw: &str) -> Result<CompanionRole, String> {
    match raw {
        "root" => Ok(CompanionRole::Root),
        "delegation_child" => Ok(CompanionRole::DelegationChild),
        other => Err(format!(
            "--role must be root or delegation_child, got {other}"
        )),
    }
}

fn parse_service(args: &[String]) -> Result<CompanionParse, String> {
    let mut socket_path = None;
    let mut incarnation = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next().cloned() {
        match arg.as_str() {
            "--service-roundtable" => {}
            "--help" | "-h" => {
                return Ok(CompanionParse::Help(
                    "codeg-mcp --service-roundtable --socket-path <path> --incarnation <id>"
                        .to_string(),
                ));
            }
            "--socket-path" => {
                socket_path = Some(required_value(&mut iter, "--socket-path")?);
            }
            "--incarnation" => {
                incarnation = Some(required_value(&mut iter, "--incarnation")?);
            }
            "--token"
            | "--parent-pid"
            | "--parent-connection-id"
            | "--features"
            | "--role"
            | "--can-spawn-child"
            | "--disabled-agents"
            | "--custom-agents"
            | "--connection-incarnation-id" => {
                let _ = iter.next();
                return Err(format!("{arg} is not valid in service mode"));
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    let socket_path = socket_path.ok_or_else(|| "missing --socket-path".to_string())?;
    let incarnation = incarnation.ok_or_else(|| "missing --incarnation".to_string())?;
    if socket_path.is_empty() || incarnation.is_empty() {
        return Err("missing service identity".to_string());
    }
    Ok(CompanionParse::Launch(CompanionMode::ServiceRoundtable {
        socket_path,
        incarnation,
    }))
}

fn parse_legacy(args: &[String]) -> Result<CompanionParse, String> {
    let mut parent_connection_id = None;
    let mut socket_path = None;
    let mut token = None;
    let mut parent_pid = None;
    let mut features = None;
    let mut role = None;
    let mut can_spawn_child = None;
    let mut connection_incarnation_id = None;
    let mut disabled_agents = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next().cloned() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(CompanionParse::Help(LEGACY_HELP.to_string())),
            "--parent-connection-id" => {
                parent_connection_id = Some(required_value(&mut iter, "--parent-connection-id")?);
            }
            "--socket-path" => {
                socket_path = Some(required_value(&mut iter, "--socket-path")?);
            }
            "--token" => {
                token = Some(required_value(&mut iter, "--token")?);
            }
            "--parent-pid" => {
                let raw = required_value(&mut iter, "--parent-pid")?;
                parent_pid = Some(
                    raw.parse::<u32>()
                        .map_err(|err| format!("--parent-pid must be a u32: {err}"))?,
                );
            }
            "--features" => {
                features = Some(required_value(&mut iter, "--features")?);
            }
            "--role" => {
                role = Some(parse_role(&required_value(&mut iter, "--role")?)?);
            }
            "--can-spawn-child" => {
                let raw = required_value(&mut iter, "--can-spawn-child")?;
                can_spawn_child =
                    Some(raw.parse::<bool>().map_err(|err| {
                        format!("--can-spawn-child must be true or false: {err}")
                    })?);
            }
            "--connection-incarnation-id" => {
                connection_incarnation_id =
                    Some(required_value(&mut iter, "--connection-incarnation-id")?);
            }
            "--disabled-agents" => {
                disabled_agents = Some(required_value(&mut iter, "--disabled-agents")?);
            }
            "--custom-agents" => {
                let _ = required_value(&mut iter, "--custom-agents")?;
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    let parent_connection_id =
        parent_connection_id.ok_or_else(|| "missing --parent-connection-id".to_string())?;
    Ok(CompanionParse::Launch(CompanionMode::LegacyParent(
        LegacyParentArgs {
            connection_incarnation_id: connection_incarnation_id
                .unwrap_or_else(|| parent_connection_id.clone()),
            parent_connection_id,
            socket_path: socket_path.ok_or_else(|| "missing --socket-path".to_string())?,
            token: token.ok_or_else(|| "missing --token".to_string())?,
            parent_pid,
            features,
            role: role.unwrap_or(CompanionRole::Root),
            can_spawn_child: can_spawn_child.unwrap_or(true),
            disabled_agents,
        },
    )))
}

fn required_value<'a>(
    iter: &mut impl Iterator<Item = &'a String>,
    flag: &str,
) -> Result<String, String> {
    iter.next()
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

/// The tool channel admits only a service-owned room attempt.
/// A delegation companion lease is not a room lease and is not consulted.
pub fn service_channel_owner(owner: &ConnectionOwner) -> RtResult<ServiceOwner> {
    match owner {
        ConnectionOwner::Service {
            room_id,
            attempt_id,
            boot_epoch,
        } => Ok(ServiceOwner {
            room_id: *room_id,
            attempt_id: *attempt_id,
            boot_epoch: *boot_epoch,
        }),
        ConnectionOwner::Window { .. } => {
            Err(rt_error(ErrorCode::Forbidden, "service_owner_required"))
        }
    }
}
