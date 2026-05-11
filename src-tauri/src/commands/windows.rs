use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering as AtomicOrdering};
use std::sync::Mutex;

use sea_orm::DatabaseConnection;
use tauri::{AppHandle, LogicalPosition, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::app_error::AppCommandError;
use crate::db::service::app_metadata_service;
use crate::db::AppDatabase;
use crate::models::FolderDetail;

/// Base traffic-light position (logical px) at 100 % zoom.
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_X: f64 = 12.0;
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_Y: f64 = 14.0;

#[cfg(target_os = "macos")]
static CURRENT_ZOOM: AtomicU32 = AtomicU32::new(100);

#[cfg(target_os = "macos")]
fn traffic_light_position() -> tauri::LogicalPosition<f64> {
    let zoom = CURRENT_ZOOM.load(AtomicOrdering::Relaxed) as f64;
    // Only Y scales with zoom: overlay content shifts vertically with
    // font-size changes, but the horizontal inset remains constant.
    tauri::LogicalPosition::new(TRAFFIC_LIGHT_X, TRAFFIC_LIGHT_Y * zoom / 100.0)
}

const ZOOM_LEVEL_DB_KEY: &str = "appearance_zoom_level";

/// Load saved zoom level from DB and initialize CURRENT_ZOOM.
/// Called once at startup before any window is created.
pub async fn load_saved_zoom(conn: &DatabaseConnection) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(Some(raw)) = app_metadata_service::get_value(conn, ZOOM_LEVEL_DB_KEY).await {
            if let Ok(zoom) = raw.parse::<u32>() {
                let clamped = zoom.clamp(50, 300);
                CURRENT_ZOOM.store(clamped, AtomicOrdering::Relaxed);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = conn;
    }
}

// ---------------------------------------------------------------------------
// Appearance mode persistence (dark / light / system)
// ---------------------------------------------------------------------------

const APPEARANCE_MODE_DB_KEY: &str = "appearance_mode";

/// Encoded appearance mode: 0 = system (default), 1 = dark, 2 = light.
static CACHED_APPEARANCE_MODE: AtomicU8 = AtomicU8::new(0);

const MODE_SYSTEM: u8 = 0;
const MODE_DARK: u8 = 1;
const MODE_LIGHT: u8 = 2;

fn mode_from_str(s: &str) -> u8 {
    match s {
        "dark" => MODE_DARK,
        "light" => MODE_LIGHT,
        _ => MODE_SYSTEM,
    }
}

/// Load saved appearance mode from DB. Called once at startup.
pub async fn load_saved_appearance_mode(conn: &DatabaseConnection) {
    if let Ok(Some(raw)) = app_metadata_service::get_value(conn, APPEARANCE_MODE_DB_KEY).await {
        CACHED_APPEARANCE_MODE.store(mode_from_str(&raw), AtomicOrdering::Relaxed);
    }
}

pub struct SettingsWindowState {
    owner_window_label: Mutex<Option<String>>,
}

pub struct CommitWindowState {
    owner_by_commit_label: Mutex<HashMap<String, String>>,
}

/// Detect macOS system dark mode via `defaults read`.
/// Result is cached for the process lifetime via `OnceLock`.
#[cfg(target_os = "macos")]
fn is_system_dark_mode() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        crate::process::std_command("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .map(|o| o.status.success()) // key exists only in dark mode
            .unwrap_or(false)
    })
}

/// Detect Windows system dark mode via registry query.
/// `AppsUseLightTheme`: 0 = dark, 1 = light.
/// Uses `crate::process::std_command` to avoid flashing a console window.
/// On pre-1809 Windows where the key is absent, defaults to light mode.
#[cfg(target_os = "windows")]
fn is_system_dark_mode() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        crate::process::std_command("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .output()
            .ok()
            .and_then(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                // Output: "    AppsUseLightTheme    REG_DWORD    0x0"
                // Extract the last token on the matching line to avoid
                // substring false-positives (e.g. "0x00000001" contains "0x0").
                stdout
                    .lines()
                    .find(|l| l.contains("AppsUseLightTheme"))
                    .map(|line| {
                        line.split_whitespace()
                            .last()
                            .map(|val| val == "0x0" || val == "0x00000000")
                            .unwrap_or(false)
                    })
            })
            .unwrap_or(false)
    })
}

/// Detect Linux system dark mode via desktop environment settings.
/// Covers GNOME (gsettings) and KDE Plasma (kreadconfig5/6).
/// Falls back to light mode on unsupported desktops (XFCE, etc.).
#[cfg(target_os = "linux")]
fn is_system_dark_mode() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        // GNOME 42+: color-scheme = 'prefer-dark'
        if let Ok(output) = crate::process::std_command("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .output()
        {
            let s = String::from_utf8_lossy(&output.stdout);
            if s.contains("prefer-dark") {
                return true;
            }
        }
        // Older GNOME / GTK: theme name contains "dark"
        if let Ok(output) = crate::process::std_command("gsettings")
            .args(["get", "org.gnome.desktop.interface", "gtk-theme"])
            .output()
        {
            let s = String::from_utf8_lossy(&output.stdout).to_lowercase();
            if s.contains("dark") {
                return true;
            }
        }
        // KDE Plasma 5/6: ColorScheme name contains "dark"
        for cmd in ["kreadconfig6", "kreadconfig5"] {
            if let Ok(output) = crate::process::std_command(cmd)
                .args(["--group", "General", "--key", "ColorScheme"])
                .output()
            {
                let s = String::from_utf8_lossy(&output.stdout).to_lowercase();
                if s.contains("dark") {
                    return true;
                }
            }
        }
        false
    })
}

/// Determine whether the window should use a dark background, considering
/// both the user's explicit preference (from DB) and the OS appearance.
fn should_use_dark_background() -> bool {
    match CACHED_APPEARANCE_MODE.load(AtomicOrdering::Relaxed) {
        MODE_DARK => true,
        MODE_LIGHT => false,
        _ => is_system_dark_mode(), // "system" or unknown — follow OS
    }
}

pub(crate) fn apply_platform_window_style<'a, R, M>(
    builder: WebviewWindowBuilder<'a, R, M>,
) -> WebviewWindowBuilder<'a, R, M>
where
    R: tauri::Runtime,
    M: tauri::Manager<R>,
{
    #[cfg(target_os = "macos")]
    {
        let builder = if should_use_dark_background() {
            // oklch(0.145 0 0) ≈ rgb(9,9,11) — matches CSS --background in dark mode
            builder.background_color(tauri::window::Color(9, 9, 11, 255))
        } else {
            builder
        };
        builder
            .hidden_title(true)
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .traffic_light_position(traffic_light_position())
    }

    #[cfg(target_os = "windows")]
    {
        let builder = if should_use_dark_background() {
            builder.background_color(tauri::window::Color(9, 9, 11, 255))
        } else {
            builder
        };
        return builder.decorations(false);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if should_use_dark_background() {
            builder.background_color(tauri::window::Color(9, 9, 11, 255))
        } else {
            builder
        }
    }
}

#[cfg(target_os = "windows")]
fn ensure_windows_undecorated(window: &tauri::WebviewWindow) {
    let _ = window.set_decorations(false);
}

#[cfg(not(target_os = "windows"))]
fn ensure_windows_undecorated(_window: &tauri::WebviewWindow) {}

/// Apply platform-specific post-creation setup.
pub(crate) fn post_window_setup(window: &tauri::WebviewWindow) {
    ensure_windows_undecorated(window);
}

impl SettingsWindowState {
    pub fn new() -> Self {
        Self {
            owner_window_label: Mutex::new(None),
        }
    }

    fn set_owner(&self, label: String) {
        if let Ok(mut owner) = self.owner_window_label.lock() {
            *owner = Some(label);
        }
    }

    fn take_owner(&self) -> Option<String> {
        self.owner_window_label
            .lock()
            .ok()
            .and_then(|mut owner| owner.take())
    }
}

impl Default for SettingsWindowState {
    fn default() -> Self {
        Self::new()
    }
}

impl CommitWindowState {
    pub fn new() -> Self {
        Self {
            owner_by_commit_label: Mutex::new(HashMap::new()),
        }
    }

    fn set_owner(&self, commit_label: String, owner_label: String) {
        if let Ok(mut owners) = self.owner_by_commit_label.lock() {
            owners.insert(commit_label, owner_label);
        }
    }

    fn take_owner(&self, commit_label: &str) -> Option<String> {
        self.owner_by_commit_label
            .lock()
            .ok()
            .and_then(|mut owners| owners.remove(commit_label))
    }
}

impl Default for CommitWindowState {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_settings_route(section: Option<&str>) -> &'static str {
    match section {
        Some("appearance") => "settings/appearance",
        Some("agents") => "settings/agents",
        Some("mcp") => "settings/mcp",
        Some("skills") => "settings/skills",
        Some("shortcuts") => "settings/shortcuts",
        Some("system") => "settings/system",
        _ => "settings/appearance",
    }
}

fn normalize_agent_query(agent_type: Option<&str>) -> Option<String> {
    let raw = agent_type?.trim();
    if raw.is_empty() {
        return None;
    }
    if raw
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Some(raw.to_string());
    }
    None
}

fn resolve_settings_target(section: Option<&str>, agent_type: Option<&str>) -> String {
    let route = resolve_settings_route(section);
    if route == "settings/agents" {
        if let Some(agent) = normalize_agent_query(agent_type) {
            return format!("{route}?agent={agent}");
        }
    }
    route.to_string()
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn open_folder_window(
    app: AppHandle,
    db: tauri::State<'_, AppDatabase>,
    path: String,
) -> Result<FolderDetail, AppCommandError> {
    // Single-window workspace: upsert the folder (is_open = true), close any
    // legacy project-boot window, and return the full detail for the frontend
    // to add to its workspace state.
    let entry = crate::db::service::folder_service::add_folder(&db.conn, &path)
        .await
        .map_err(AppCommandError::from)?;

    if let Some(w) = app.get_webview_window("project-boot") {
        let _ = w.close();
    }

    let folder = crate::db::service::folder_service::get_folder_by_id(&db.conn, entry.id)
        .await
        .map_err(AppCommandError::from)?
        .ok_or_else(|| AppCommandError::not_found("Folder not found after add"))?;

    // Bring the main window to focus if it exists
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.unminimize();
        let _ = main.set_focus();
    }

    Ok(folder)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn open_commit_window(
    app: AppHandle,
    window: tauri::WebviewWindow,
    db: tauri::State<'_, AppDatabase>,
    state: tauri::State<'_, CommitWindowState>,
    folder_id: i32,
) -> Result<(), AppCommandError> {
    let owner_label = window.label().to_string();
    let label = format!("commit-{folder_id}");

    if let Some(existing) = app.get_webview_window(&label) {
        state.set_owner(label.clone(), owner_label);
        let _ = existing.unminimize();
        existing
            .set_focus()
            .map_err(|e| AppCommandError::window("Failed to focus commit window", e.to_string()))?;
        return Ok(());
    }

    let folder = crate::db::service::folder_service::get_folder_by_id(&db.conn, folder_id)
        .await
        .map_err(AppCommandError::from)?
        .ok_or_else(|| {
            AppCommandError::not_found(format!("Folder {folder_id} not found"))
                .with_detail(format!("folder_id={folder_id}"))
        })?;

    let url = WebviewUrl::App(format!("commit?folderId={folder_id}").into());
    let builder = WebviewWindowBuilder::new(&app, &label, url)
        .title(format!("提交代码 - {}", folder.name))
        .inner_size(1220.0, 820.0)
        .min_inner_size(980.0, 620.0)
        .center();
    let builder = builder
        .parent(&window)
        .map_err(|e| AppCommandError::window("Failed to attach commit window to parent", e.to_string()))?;
    let commit_window = apply_platform_window_style(builder)
        .build()
        .map_err(|e| AppCommandError::window("Failed to open commit window", e.to_string()))?;
    post_window_setup(&commit_window);
    state.set_owner(label, owner_label);
    commit_window
        .set_focus()
        .map_err(|e| AppCommandError::window("Failed to focus commit window", e.to_string()))?;

    Ok(())
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn open_settings_window(
    app: AppHandle,
    window: tauri::WebviewWindow,
    section: Option<String>,
    agent_type: Option<String>,
    state: tauri::State<'_, SettingsWindowState>,
) -> Result<(), AppCommandError> {
    let target_route = resolve_settings_target(section.as_deref(), agent_type.as_deref());
    let owner_label = window.label().to_string();
    if let Some(existing) = app.get_webview_window("settings") {
        post_window_setup(&existing);
        if section.is_some() || agent_type.is_some() {
            let target_path = format!("/{target_route}");
            let target_json = serde_json::to_string(&target_path).map_err(|e| {
                AppCommandError::window("Failed to build settings navigation target", e.to_string())
            })?;
            let nav_script = format!("window.location.replace({target_json});");
            existing.eval(&nav_script).map_err(|e| {
                AppCommandError::window("Failed to navigate settings window", e.to_string())
            })?;
        }
        let _ = state.take_owner();
        state.set_owner(owner_label);
        let _ = existing.unminimize();
        existing.set_focus().map_err(|e| {
            AppCommandError::window("Failed to focus settings window", e.to_string())
        })?;
        return Ok(());
    }

    let url = WebviewUrl::App(target_route.into());
    let builder = WebviewWindowBuilder::new(&app, "settings", url)
        .title("Settings")
        .inner_size(1080.0, 700.0)
        .min_inner_size(1080.0, 600.0)
        .center();
    let builder = builder
        .parent(&window)
        .map_err(|e| AppCommandError::window("Failed to attach settings window to parent", e.to_string()))?;
    let settings_window = apply_platform_window_style(builder)
        .build()
        .map_err(|e| AppCommandError::window("Failed to open settings window", e.to_string()))?;
    post_window_setup(&settings_window);
    state.set_owner(owner_label);
    settings_window
        .set_focus()
        .map_err(|e| AppCommandError::window("Failed to focus settings window", e.to_string()))?;
    Ok(())
}

pub fn restore_windows_after_settings(app: &AppHandle, state: &SettingsWindowState) {
    if let Some(owner_label) = state.take_owner() {
        if let Some(window) = app.get_webview_window(&owner_label) {
            let _ = window.set_focus();
        }
    }
}

pub fn restore_window_after_commit(
    app: &AppHandle,
    state: &CommitWindowState,
    commit_window_label: &str,
) {
    if let Some(owner_label) = state.take_owner(commit_window_label) {
        if let Some(window) = app.get_webview_window(&owner_label) {
            let _ = window.set_focus();
        }
    }
}

pub struct MergeWindowState {
    owner_by_merge_label: Mutex<HashMap<String, String>>,
}

impl MergeWindowState {
    pub fn new() -> Self {
        Self {
            owner_by_merge_label: Mutex::new(HashMap::new()),
        }
    }

    fn set_owner(&self, merge_label: String, owner_label: String) {
        if let Ok(mut owners) = self.owner_by_merge_label.lock() {
            owners.insert(merge_label, owner_label);
        }
    }

    fn take_owner(&self, merge_label: &str) -> Option<String> {
        self.owner_by_merge_label
            .lock()
            .ok()
            .and_then(|mut owners| owners.remove(merge_label))
    }
}

impl Default for MergeWindowState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn open_merge_window(
    app: AppHandle,
    window: tauri::WebviewWindow,
    db: tauri::State<'_, AppDatabase>,
    state: tauri::State<'_, MergeWindowState>,
    folder_id: i32,
    operation: String,
    upstream_commit: Option<String>,
) -> Result<(), AppCommandError> {
    let owner_label = window.label().to_string();
    let label = format!("merge-{folder_id}");

    if let Some(existing) = app.get_webview_window(&label) {
        state.set_owner(label.clone(), owner_label);
        let _ = existing.unminimize();
        existing
            .set_focus()
            .map_err(|e| AppCommandError::window("Failed to focus merge window", e.to_string()))?;
        return Ok(());
    }

    let folder = crate::db::service::folder_service::get_folder_by_id(&db.conn, folder_id)
        .await
        .map_err(AppCommandError::from)?
        .ok_or_else(|| {
            AppCommandError::not_found(format!("Folder {folder_id} not found"))
                .with_detail(format!("folder_id={folder_id}"))
        })?;

    let mut url_str = format!("merge?folderId={folder_id}&operation={operation}");
    if let Some(ref commit) = upstream_commit {
        url_str.push_str(&format!("&upstreamCommit={commit}"));
    }
    let url = WebviewUrl::App(url_str.into());
    let builder = WebviewWindowBuilder::new(&app, &label, url)
        .title(format!("解决冲突 - {}", folder.name))
        .inner_size(1400.0, 900.0)
        .min_inner_size(1100.0, 650.0)
        .center();
    let builder = builder
        .parent(&window)
        .map_err(|e| AppCommandError::window("Failed to attach merge window to parent", e.to_string()))?;
    let merge_window = apply_platform_window_style(builder)
        .build()
        .map_err(|e| AppCommandError::window("Failed to open merge window", e.to_string()))?;
    post_window_setup(&merge_window);
    state.set_owner(label, owner_label);
    merge_window
        .set_focus()
        .map_err(|e| AppCommandError::window("Failed to focus merge window", e.to_string()))?;

    Ok(())
}

pub fn restore_window_after_merge(
    app: &AppHandle,
    state: &MergeWindowState,
    merge_window_label: &str,
) {
    if let Some(owner_label) = state.take_owner(merge_window_label) {
        if let Some(window) = app.get_webview_window(&owner_label) {
            let _ = window.set_focus();
        }
    }
}

/// Clean up dangling merge state when a merge window is closed without
/// completing or aborting. Checks if MERGE_HEAD exists, aborts the merge,
/// and notifies the parent window.
pub async fn cleanup_dangling_merge(app: &AppHandle, merge_window_label: &str) {
    let folder_id: i32 = match merge_window_label
        .strip_prefix("merge-")
        .and_then(|s| s.parse().ok())
    {
        Some(id) => id,
        None => return,
    };

    let db = match app.try_state::<AppDatabase>() {
        Some(db) => db,
        None => return,
    };

    let folder =
        match crate::db::service::folder_service::get_folder_by_id(&db.conn, folder_id).await {
            Ok(Some(f)) => f,
            _ => return,
        };

    // Check if MERGE_HEAD exists
    let check = crate::process::tokio_command("git")
        .args(["rev-parse", "--verify", "MERGE_HEAD"])
        .current_dir(&folder.path)
        .output()
        .await;
    let has_merge_head = check.map(|o| o.status.success()).unwrap_or(false);

    if has_merge_head {
        let _ = crate::process::tokio_command("git")
            .args(["merge", "--abort"])
            .current_dir(&folder.path)
            .output()
            .await;

        let emitter = crate::web::event_bridge::EventEmitter::Tauri(app.clone());
        crate::web::event_bridge::emit_event(
            &emitter,
            "folder://merge-aborted",
            serde_json::json!({ "folder_id": folder_id }),
        );
    }
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn open_stash_window(
    app: AppHandle,
    db: tauri::State<'_, AppDatabase>,
    folder_id: i32,
) -> Result<(), AppCommandError> {
    let label = format!("stash-{folder_id}");

    if let Some(existing) = app.get_webview_window(&label) {
        post_window_setup(&existing);
        let _ = existing.unminimize();
        existing
            .set_focus()
            .map_err(|e| AppCommandError::window("Failed to focus stash window", e.to_string()))?;
        return Ok(());
    }

    let folder = crate::db::service::folder_service::get_folder_by_id(&db.conn, folder_id)
        .await
        .map_err(AppCommandError::from)?
        .ok_or_else(|| {
            AppCommandError::not_found(format!("Folder {folder_id} not found"))
                .with_detail(format!("folder_id={folder_id}"))
        })?;

    let url = WebviewUrl::App(format!("stash?folderId={folder_id}").into());
    let builder = WebviewWindowBuilder::new(&app, &label, url)
        .title(format!("Stash - {}", folder.name))
        .inner_size(1100.0, 700.0)
        .min_inner_size(800.0, 500.0)
        .center();
    let stash_window = apply_platform_window_style(builder)
        .build()
        .map_err(|e| AppCommandError::window("Failed to open stash window", e.to_string()))?;
    post_window_setup(&stash_window);

    Ok(())
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn open_push_window(
    app: AppHandle,
    window: tauri::WebviewWindow,
    db: tauri::State<'_, AppDatabase>,
    folder_id: i32,
) -> Result<(), AppCommandError> {
    let label = format!("push-{folder_id}");

    if let Some(existing) = app.get_webview_window(&label) {
        post_window_setup(&existing);
        let _ = existing.unminimize();
        existing
            .set_focus()
            .map_err(|e| AppCommandError::window("Failed to focus push window", e.to_string()))?;
        return Ok(());
    }

    let folder = crate::db::service::folder_service::get_folder_by_id(&db.conn, folder_id)
        .await
        .map_err(AppCommandError::from)?
        .ok_or_else(|| {
            AppCommandError::not_found(format!("Folder {folder_id} not found"))
                .with_detail(format!("folder_id={folder_id}"))
        })?;

    let url = WebviewUrl::App(format!("push?folderId={folder_id}").into());
    let builder = WebviewWindowBuilder::new(&app, &label, url)
        .title(format!("Push - {}", folder.name))
        .inner_size(1100.0, 700.0)
        .min_inner_size(800.0, 500.0)
        .center();
    let builder = builder
        .parent(&window)
        .map_err(|e| AppCommandError::window("Failed to attach push window to parent", e.to_string()))?;
    let push_window = apply_platform_window_style(builder)
        .build()
        .map_err(|e| AppCommandError::window("Failed to open push window", e.to_string()))?;
    post_window_setup(&push_window);

    Ok(())
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn open_project_boot_window(
    app: AppHandle,
    source: Option<String>,
) -> Result<(), AppCommandError> {
    let _ = source;
    if let Some(existing) = app.get_webview_window("project-boot") {
        post_window_setup(&existing);
        let _ = existing.unminimize();
        existing.set_focus().map_err(|e| {
            AppCommandError::window("Failed to focus project boot window", e.to_string())
        })?;
        return Ok(());
    }

    let url = WebviewUrl::App("project-boot".into());
    let builder = WebviewWindowBuilder::new(&app, "project-boot", url)
        .title("Project Boot")
        .inner_size(1400.0, 900.0)
        .min_inner_size(1100.0, 700.0)
        .center();
    let window = apply_platform_window_style(builder).build().map_err(|e| {
        AppCommandError::window("Failed to open project boot window", e.to_string())
    })?;
    post_window_setup(&window);

    Ok(())
}

// ─── Desktop pet window ─────────────────────────────────────────────────

const PET_WINDOW_LABEL: &str = "pet";
const PET_HOVER_ENTER_EVENT: &str = "pet://hover-enter";
const PET_HOVER_LEAVE_EVENT: &str = "pet://hover-leave";
/// Single-frame logical pixel dimensions, locked to the Codex sprite-sheet
/// contract. The window is sized as one frame × user scale, with no extra
/// chrome — DPR handling lives inside the webview.
const PET_BASE_WIDTH: f64 = 192.0;
const PET_BASE_HEIGHT: f64 = 208.0;

/// Process-global "cursor is currently inside the pet window" flag, owned by
/// the hover watcher but readable/writable by the context-menu command so
/// that dismissing the native menu can force a fresh `enter` event. Without
/// this, the cursor never appears to "leave" while the menu is up — the
/// watcher's transition detector then misses the post-dismiss enter and
/// the user has to wiggle off-pet-and-back to re-trigger waving.
static PET_HOVER_WAS_INSIDE: AtomicBool = AtomicBool::new(false);

/// Apply the pet-window-specific platform style. Deliberately separate from
/// `apply_platform_window_style`: that helper sets a solid background color
/// for the main / settings / git windows, which would defeat the
/// transparent + chromeless pet window. The pet builder needs only
/// borderless decoration; transparency itself is set by the caller.
fn apply_pet_window_style<'a, R, M>(
    builder: WebviewWindowBuilder<'a, R, M>,
) -> WebviewWindowBuilder<'a, R, M>
where
    R: tauri::Runtime,
    M: tauri::Manager<R>,
{
    #[cfg(target_os = "macos")]
    {
        builder
            .title_bar_style(tauri::TitleBarStyle::Transparent)
            .hidden_title(true)
    }

    #[cfg(target_os = "windows")]
    {
        builder.decorations(false)
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        builder
    }
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn open_pet_window(
    app: AppHandle,
    db: tauri::State<'_, AppDatabase>,
) -> Result<(), AppCommandError> {
    let mut config =
        crate::commands::pet::pet_get_settings_core(&db.conn).await?;
    let pet_id = config
        .active_pet_id
        .clone()
        .ok_or_else(|| AppCommandError::configuration_missing("No active pet selected."))?;

    // Validate the pet still exists; otherwise fail loudly so the caller
    // can route the user to the picker rather than open an empty window.
    {
        let id = pet_id.clone();
        tokio::task::spawn_blocking(move || crate::pets::get_pet(&id))
            .await
            .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))??;
    }

    if let Some(existing) = app.get_webview_window(PET_WINDOW_LABEL) {
        let _ = existing.unminimize();
        existing
            .set_focus()
            .map_err(|e| AppCommandError::window("Failed to focus pet window", e.to_string()))?;
        return Ok(());
    }

    let scale = config.scale.clamp(0.5, 3.0);
    config.scale = scale;
    config.enabled = true;
    crate::commands::pet::pet_save_window_state_core(
        &db.conn,
        crate::models::pet::PetWindowStatePatch {
            x: None,
            y: None,
            scale: Some(scale),
            always_on_top: None,
            enabled: Some(true),
        },
    )
    .await?;

    let url = WebviewUrl::App(format!("pet?petId={pet_id}").into());
    let mut builder = WebviewWindowBuilder::new(&app, PET_WINDOW_LABEL, url)
        .title("codeg pet")
        .inner_size(PET_BASE_WIDTH * scale, PET_BASE_HEIGHT * scale)
        .min_inner_size(PET_BASE_WIDTH * 0.5, PET_BASE_HEIGHT * 0.5)
        .max_inner_size(PET_BASE_WIDTH * 3.0, PET_BASE_HEIGHT * 3.0)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(config.always_on_top)
        .skip_taskbar(true)
        .shadow(false)
        // Don't steal focus from the user's IDE/terminal on summon, and let
        // the first click on an inactive pet window hit the webview directly
        // (so drag works without a "click once to activate" cycle).
        .focused(false)
        .accept_first_mouse(true);

    builder = builder.center();

    apply_pet_window_style(builder)
        .build()
        .map_err(|e| AppCommandError::window("Failed to open pet window", e.to_string()))?;

    spawn_pet_hover_watcher(app.clone());

    Ok(())
}

/// Polls the global cursor position and emits `pet://hover-enter` whenever
/// the cursor crosses into the pet window's bounds. Native webviews on
/// macOS don't reliably deliver mouse events to non-key windows, so we
/// detect "cursor over the pet" in Rust and let the frontend trigger the
/// waving animation in response. The task ends when the pet window is
/// closed.
fn spawn_pet_hover_watcher(app: AppHandle) {
    use std::time::Duration;
    use tauri::Emitter;

    // Bounds change only on drag or scale; refreshing every N ticks cuts
    // `outer_position`/`outer_size` IPC by ~80% in the steady state. The
    // false hover-enter that cache staleness produces during a drag is
    // suppressed on the JS side via a pointer-down guard (see PetWindow).
    const BOUNDS_REFRESH_TICKS: u8 = 5;

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(80));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Start fresh: prior pet sessions may have left the flag set to
        // true, and we must guarantee an enter event next time the cursor
        // actually crosses the bounds.
        PET_HOVER_WAS_INSIDE.store(false, AtomicOrdering::Relaxed);
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        let mut ticks_since_refresh: u8 = BOUNDS_REFRESH_TICKS;
        loop {
            interval.tick().await;
            let Some(window) = app.get_webview_window(PET_WINDOW_LABEL) else {
                break;
            };

            if ticks_since_refresh >= BOUNDS_REFRESH_TICKS {
                let Ok(pos) = window.outer_position() else {
                    continue;
                };
                let Ok(size) = window.outer_size() else {
                    continue;
                };
                let x_min = pos.x as f64;
                let y_min = pos.y as f64;
                bounds = Some((
                    x_min,
                    x_min + size.width as f64,
                    y_min,
                    y_min + size.height as f64,
                ));
                ticks_since_refresh = 0;
            } else {
                ticks_since_refresh += 1;
            }

            let Some((x_min, x_max, y_min, y_max)) = bounds else {
                continue;
            };
            let Ok(cursor) = app.cursor_position() else {
                continue;
            };
            let inside = cursor.x >= x_min
                && cursor.x < x_max
                && cursor.y >= y_min
                && cursor.y < y_max;
            let was_inside = PET_HOVER_WAS_INSIDE.load(AtomicOrdering::Relaxed);
            if inside && !was_inside {
                let _ = app.emit(PET_HOVER_ENTER_EVENT, ());
            } else if !inside && was_inside {
                let _ = app.emit(PET_HOVER_LEAVE_EVENT, ());
            }
            PET_HOVER_WAS_INSIDE.store(inside, AtomicOrdering::Relaxed);
        }
    });
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn close_pet_window(
    app: AppHandle,
    db: tauri::State<'_, AppDatabase>,
) -> Result<(), AppCommandError> {
    if let Some(existing) = app.get_webview_window(PET_WINDOW_LABEL) {
        let _ = existing.close();
    }
    let _ = crate::commands::pet::pet_save_window_state_core(
        &db.conn,
        crate::models::pet::PetWindowStatePatch {
            x: None,
            y: None,
            scale: None,
            always_on_top: None,
            enabled: Some(false),
        },
    )
    .await?;
    Ok(())
}

/// Persist the pet window's last-known position. Called by the pet renderer
/// when the user finishes dragging.
#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_window_record_position(
    db: tauri::State<'_, AppDatabase>,
    x: f64,
    y: f64,
) -> Result<(), AppCommandError> {
    crate::commands::pet::pet_save_window_state_core(
        &db.conn,
        crate::models::pet::PetWindowStatePatch {
            x: Some(x),
            y: Some(y),
            scale: None,
            always_on_top: None,
            enabled: None,
        },
    )
    .await?;
    Ok(())
}

// ─── Pet right-click context menu (native) ──────────────────────────────
//
// The pet window is intentionally tiny (a single sprite frame × user scale,
// e.g. 144×156 logical px at 0.75x). An HTML-rendered popup gets clipped to
// those bounds — items don't fit, and the user can't click "outside" because
// there is no outside inside the window. Popping a real OS menu via Tauri's
// `popup_menu_at` sidesteps the clip entirely and gets us native dismiss
// (Escape, click-elsewhere) for free. Item ids carry the action; the global
// `on_menu_event` listener wired up in `lib.rs` dispatches them.

/// Stable id namespace for pet menu items.
pub const PET_MENU_ID_PREFIX: &str = "pet:";
pub const PET_MENU_ID_OPEN_MANAGER: &str = "pet:open_manager";
pub const PET_MENU_ID_CLOSE: &str = "pet:close";
pub const PET_MENU_SCALE_PREFIX: &str = "pet:scale:";
/// Selectable scale steps. Display label is locale-independent (just digits +
/// "×"), so we don't translate it. The id `suffix` survives a round-trip
/// through the OS menu and back into our event dispatcher.
const PET_MENU_SCALE_STEPS: &[(f64, &str, &str)] = &[
    (0.5, "0.5×", "05"),
    (0.75, "0.75×", "075"),
    (1.0, "1×", "1"),
    (1.5, "1.5×", "15"),
    (2.0, "2×", "2"),
];

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetMenuLabels {
    pub scale: String,
    pub open_manager: String,
    pub close: String,
}

/// Map a menu item id back to its scale value. Used by the global menu event
/// dispatcher in `lib.rs` so the suffix→value table lives in one place.
pub fn pet_menu_scale_from_id(id: &str) -> Option<f64> {
    let suffix = id.strip_prefix(PET_MENU_SCALE_PREFIX)?;
    PET_MENU_SCALE_STEPS
        .iter()
        .find_map(|(value, _, s)| if *s == suffix { Some(*value) } else { None })
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn pet_show_context_menu(
    app: AppHandle,
    db: tauri::State<'_, AppDatabase>,
    labels: PetMenuLabels,
    x: f64,
    y: f64,
) -> Result<(), AppCommandError> {
    use tauri::menu::{CheckMenuItem, MenuBuilder, MenuItem, PredefinedMenuItem};

    let pet_window = app
        .get_webview_window(PET_WINDOW_LABEL)
        .ok_or_else(|| AppCommandError::window("Pet window not open", String::new()))?;

    let config = crate::commands::pet::pet_get_settings_core(&db.conn).await?;
    let current = config.scale;

    let menu_err =
        |stage: &str, e: tauri::Error| AppCommandError::window(stage, e.to_string());

    // Disabled header acts as a "Scale" section label. macOS renders this as
    // dimmed gray text, Linux/Windows as a non-clickable item — close enough
    // to a section heading without depending on platform-specific section
    // APIs that don't exist in Tauri's cross-platform menu wrapper.
    let header = MenuItem::with_id(
        &app,
        format!("{PET_MENU_ID_PREFIX}header"),
        &labels.scale,
        false,
        None::<&str>,
    )
    .map_err(|e| menu_err("Failed to build pet menu header", e))?;
    let sep1 = PredefinedMenuItem::separator(&app)
        .map_err(|e| menu_err("Failed to build pet menu separator", e))?;
    let sep2 = PredefinedMenuItem::separator(&app)
        .map_err(|e| menu_err("Failed to build pet menu separator", e))?;

    let mut scale_items = Vec::with_capacity(PET_MENU_SCALE_STEPS.len());
    for (value, label, suffix) in PET_MENU_SCALE_STEPS {
        let id = format!("{PET_MENU_SCALE_PREFIX}{suffix}");
        let checked = (current - *value).abs() < 0.01;
        let item = CheckMenuItem::with_id(&app, id, *label, true, checked, None::<&str>)
            .map_err(|e| menu_err("Failed to build pet menu scale item", e))?;
        scale_items.push(item);
    }

    let open_mgr = MenuItem::with_id(
        &app,
        PET_MENU_ID_OPEN_MANAGER,
        &labels.open_manager,
        true,
        None::<&str>,
    )
    .map_err(|e| menu_err("Failed to build pet menu manager item", e))?;
    let close_item = MenuItem::with_id(
        &app,
        PET_MENU_ID_CLOSE,
        &labels.close,
        true,
        None::<&str>,
    )
    .map_err(|e| menu_err("Failed to build pet menu close item", e))?;

    let mut builder = MenuBuilder::new(&app).item(&header).item(&sep1);
    for item in &scale_items {
        builder = builder.item(item);
    }
    let menu = builder
        .item(&sep2)
        .item(&open_mgr)
        .item(&close_item)
        .build()
        .map_err(|e| menu_err("Failed to build pet menu", e))?;

    pet_window
        .popup_menu_at(&menu, LogicalPosition::new(x, y))
        .map_err(|e| menu_err("Failed to popup pet menu", e))?;

    // Hover transition state needs a manual reset after the menu
    // dismisses — see `reset_pet_hover_after_native_menu`'s docs.
    reset_pet_hover_after_native_menu();

    Ok(())
}

/// Force the hover watcher to re-emit `enter` next tick if the cursor
/// is still over the pet.
///
/// `popup_menu_at` is modal on macOS / Windows, so this runs strictly
/// after the user has dismissed the menu. The cursor almost certainly
/// never appeared to leave the pet window from the watcher's view —
/// right-click happens *at* the pet, and the OS menu is just an
/// overlay sitting on top, so the polled cursor stays inside the
/// window's bounds the whole time. Without this reset, the watcher's
/// `was_inside == true` flag suppresses the next genuine hover-enter
/// and the wave animation silently stops working until the user moves
/// off-pet and back. Storing `false` makes the very next 80 ms tick
/// re-emit `enter` if the cursor is still over the pet, restoring
/// waving on the spot.
fn reset_pet_hover_after_native_menu() {
    PET_HOVER_WAS_INSIDE.store(false, AtomicOrdering::Relaxed);
}

/// Store the current zoom level and persist it to DB so the next launch
/// creates windows with the correct traffic-light position.
/// Existing windows are NOT repositioned at runtime.
#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn update_traffic_light_position(
    app: AppHandle,
    db: tauri::State<'_, AppDatabase>,
    zoom: f64,
) -> Result<(), AppCommandError> {
    let clamped = zoom.clamp(50.0, 300.0) as u32;

    #[cfg(target_os = "macos")]
    CURRENT_ZOOM.store(clamped, AtomicOrdering::Relaxed);

    // Persist to DB so the next launch reads the correct value.
    let _ =
        app_metadata_service::upsert_value(&db.conn, ZOOM_LEVEL_DB_KEY, &clamped.to_string()).await;

    let _ = app;
    Ok(())
}

/// Persist the user's appearance mode ("dark" / "light" / "system") to DB
/// and update the in-memory cache so that subsequent window creations use the
/// correct native background color.
#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn update_appearance_mode(
    db: tauri::State<'_, AppDatabase>,
    mode: String,
) -> Result<(), AppCommandError> {
    CACHED_APPEARANCE_MODE.store(mode_from_str(&mode), AtomicOrdering::Relaxed);

    let _ = app_metadata_service::upsert_value(&db.conn, APPEARANCE_MODE_DB_KEY, &mode).await;

    Ok(())
}

// ─── System tray icon ──────────────────────────────────────────────────

/// Monochrome template image for the macOS menu bar. AppKit treats an
/// `NSImage` with `isTemplate = true` as a mask: only the alpha channel
/// matters and the system tints it to match the menu-bar appearance
/// (light, dark, accent). The colored window icon won't get this
/// treatment and looks out of place next to other system icons.
#[cfg(all(feature = "tauri-runtime", target_os = "macos"))]
const MACOS_TRAY_TEMPLATE_PNG: &[u8] =
    include_bytes!("../../icons/tray-icon-template.png");

#[cfg(all(feature = "tauri-runtime", target_os = "macos"))]
fn load_macos_tray_template_icon() -> Result<tauri::image::Image<'static>, String> {
    let decoded = image::load_from_memory(MACOS_TRAY_TEMPLATE_PNG)
        .map_err(|e| format!("decode tray template png: {e}"))?
        .to_rgba8();
    let (w, h) = (decoded.width(), decoded.height());
    Ok(tauri::image::Image::new_owned(decoded.into_raw(), w, h))
}

/// Stable id namespace for tray menu items. Routed through the app-wide
/// `on_menu_event` handler in `lib.rs`.
pub const TRAY_MENU_ID_PREFIX: &str = "tray:";
pub const TRAY_MENU_ID_SHOW: &str = "tray:show";
pub const TRAY_MENU_ID_QUIT: &str = "tray:quit";
pub const TRAY_ICON_ID: &str = "codeg-tray";

/// True after `install_tray_icon` returns `Ok`. The hide-on-close path
/// in `lib.rs` consults this so we don't strand the user on systems
/// where the tray failed to install (Windows tray refused, etc.). On
/// Linux this is necessary-but-not-sufficient: the StatusNotifierWatcher
/// may be missing and the icon invisible even when build() returns Ok,
/// which is why `can_hide_to_tray()` reports false on Linux regardless.
#[cfg(feature = "tauri-runtime")]
static TRAY_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Whether hide-on-close is safe on this platform/session. When false,
/// the close handler in `lib.rs` forces a real app exit instead — both
/// `hide()` and `minimize()` would leave aux windows (pet, settings)
/// running without a recoverable workspace.
#[cfg(feature = "tauri-runtime")]
pub fn can_hide_to_tray() -> bool {
    // Linux: even with a successfully installed tray icon, modern GNOME
    // (45+) defaults ship without a StatusNotifierWatcher and the icon
    // is silently invisible. Refusing here forces the close to pass
    // through to a real exit on Linux — preferable to a phantom process
    // with no UI surface.
    if cfg!(target_os = "linux") {
        return false;
    }
    TRAY_AVAILABLE.load(AtomicOrdering::Relaxed)
}

/// Bring the hidden / minimized main workspace window back to the
/// foreground. Used by:
///   * single-instance plugin (second launch)
///   * tray icon left-click and "Show Workspace" menu item
///   * macOS dock-icon reopen
#[cfg(feature = "tauri-runtime")]
pub fn show_main_window(app: &AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
    }
}

#[cfg(feature = "tauri-runtime")]
struct TrayLabels {
    show_workspace: &'static str,
    quit: &'static str,
}

#[cfg(feature = "tauri-runtime")]
fn tray_labels_for(locale: crate::models::system::AppLocale) -> TrayLabels {
    use crate::models::system::AppLocale;
    match locale {
        AppLocale::ZhCn => TrayLabels {
            show_workspace: "显示工作台",
            quit: "退出 Codeg",
        },
        AppLocale::ZhTw => TrayLabels {
            show_workspace: "顯示工作臺",
            quit: "退出 Codeg",
        },
        AppLocale::Ja => TrayLabels {
            show_workspace: "ワークスペースを表示",
            quit: "Codeg を終了",
        },
        AppLocale::Ko => TrayLabels {
            show_workspace: "워크스페이스 표시",
            quit: "Codeg 종료",
        },
        AppLocale::Es => TrayLabels {
            show_workspace: "Mostrar el área de trabajo",
            quit: "Salir de Codeg",
        },
        AppLocale::De => TrayLabels {
            show_workspace: "Arbeitsbereich anzeigen",
            quit: "Codeg beenden",
        },
        AppLocale::Fr => TrayLabels {
            show_workspace: "Afficher l'espace de travail",
            quit: "Quitter Codeg",
        },
        AppLocale::Pt => TrayLabels {
            show_workspace: "Mostrar área de trabalho",
            quit: "Sair do Codeg",
        },
        AppLocale::Ar => TrayLabels {
            show_workspace: "إظهار مساحة العمل",
            quit: "إنهاء Codeg",
        },
        AppLocale::En => TrayLabels {
            show_workspace: "Show Workspace",
            quit: "Quit Codeg",
        },
    }
}

/// Install the system tray icon and its right-click menu. Left-click
/// (Linux/Windows) and dock-style activation behaviors map to
/// `show_main_window`. Menu wiring lives in the app-wide
/// `on_menu_event` callback in `lib.rs` so the tray and pet menus share
/// one dispatcher.
#[cfg(feature = "tauri-runtime")]
pub fn install_tray_icon(
    app: &AppHandle,
    locale: crate::models::system::AppLocale,
) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let labels = tray_labels_for(locale);
    let show_item = MenuItem::with_id(
        app,
        TRAY_MENU_ID_SHOW,
        labels.show_workspace,
        true,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item =
        MenuItem::with_id(app, TRAY_MENU_ID_QUIT, labels.quit, true, None::<&str>)?;
    let menu = MenuBuilder::new(app)
        .items(&[&show_item, &separator, &quit_item])
        .build()?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ICON_ID)
        .tooltip("Codeg")
        .menu(&menu)
        // `false` is required for `on_tray_icon_event::Click` to fire on
        // every platform we ship: the default `true` causes the OS to
        // consume left-click to pop the menu (notably on macOS — see
        // tauri-apps/tauri#11413). Right-click still shows the menu
        // because that's the OS's job, not ours.
        .show_menu_on_left_click(false);

    // macOS menu bar expects a monochrome template image that adapts to
    // light/dark mode and the user's accent settings. Other platforms
    // (Windows tray, Linux indicators) want the regular colored icon.
    #[cfg(target_os = "macos")]
    {
        match load_macos_tray_template_icon() {
            Ok(icon) => {
                builder = builder.icon(icon).icon_as_template(true);
            }
            Err(err) => {
                eprintln!("[Tray] failed to load template icon, falling back: {err}");
                if let Some(icon) = app.default_window_icon() {
                    builder = builder.icon(icon.clone());
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }
    }

    builder
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    TRAY_AVAILABLE.store(true, AtomicOrdering::Relaxed);
    Ok(())
}

/// Rebuild the tray menu in the supplied locale and swap it onto the
/// existing tray icon. No-op if the tray hasn't been installed yet
/// (e.g. the language change races setup, or the platform refused the
/// initial install).
#[cfg(feature = "tauri-runtime")]
pub fn refresh_tray_menu(
    app: &AppHandle,
    locale: crate::models::system::AppLocale,
) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItem, PredefinedMenuItem};

    let Some(tray) = app.tray_by_id(TRAY_ICON_ID) else {
        return Ok(());
    };

    let labels = tray_labels_for(locale);
    let show_item = MenuItem::with_id(
        app,
        TRAY_MENU_ID_SHOW,
        labels.show_workspace,
        true,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item =
        MenuItem::with_id(app, TRAY_MENU_ID_QUIT, labels.quit, true, None::<&str>)?;
    let menu = MenuBuilder::new(app)
        .items(&[&show_item, &separator, &quit_item])
        .build()?;

    tray.set_menu(Some(menu))?;
    Ok(())
}

/// Push the current effective UI locale to the system tray. Called by
/// the i18n provider whenever the resolved app locale changes — covers
/// both manual selection and OS-driven changes in system mode.
#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_tray_locale(
    app: AppHandle,
    locale: crate::models::system::AppLocale,
) -> Result<(), AppCommandError> {
    refresh_tray_menu(&app, locale)
        .map_err(|e| AppCommandError::window("Failed to refresh tray menu", e.to_string()))
}
