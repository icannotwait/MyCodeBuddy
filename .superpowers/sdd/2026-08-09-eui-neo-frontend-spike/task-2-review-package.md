# Task 2 Review Package
BASE: 6fcfd6999d69d16d829b0410c1e828069aec0628 HEAD: 8bac8d78bcdf7f189304fa714d068e2d73ddb541
8bac8d78 feat(eui): add isolated core bootstrap profile
 src-tauri/codeg-eui-core/src/abi.rs                |  66 +++++-
 src-tauri/codeg-eui-core/src/bootstrap.rs          | 141 +++++++++++++
 src-tauri/codeg-eui-core/src/data_root.rs          | 141 +++++++++++++
 src-tauri/codeg-eui-core/src/lib.rs                |   4 +
 src-tauri/codeg-eui-core/tests/abi_smoke.rs        |   8 +-
 .../codeg-eui-core/tests/bootstrap_profile.rs      |  48 +++++
 .../codeg-eui-core/tests/data_root_isolation.rs    | 227 +++++++++++++++++++++
 src-tauri/src/app_state.rs                         |  80 ++++++++
 src-tauri/src/document_translate/service.rs        |  10 +-
 src-tauri/src/logging/init.rs                      |  25 ++-
 10 files changed, 741 insertions(+), 9 deletions(-)
diff --git a/src-tauri/codeg-eui-core/src/abi.rs b/src-tauri/codeg-eui-core/src/abi.rs
index 01e33c26..66cc050d 100644
--- a/src-tauri/codeg-eui-core/src/abi.rs
+++ b/src-tauri/codeg-eui-core/src/abi.rs
@@ -1,5 +1,9 @@
 use std::panic::{catch_unwind, AssertUnwindSafe};
+use std::path::PathBuf;
 use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
+use std::sync::{Mutex, OnceLock};
+
+use crate::EuiBootstrap;
 
 pub const CODEG_EUI_API_VERSION: u32 = 1;
 pub const CODEG_EUI_OK: i32 = 0;
@@ -12,10 +16,12 @@ const LIFECYCLE_STARTING: u32 = 1;
 const LIFECYCLE_RUNNING: u32 = 2;
 const LIFECYCLE_STOPPING: u32 = 3;
 const LIFECYCLE_STOPPED: u32 = 4;
+const CODEG_EUI_MAX_PATH_BYTES: usize = 32_768;
 
 static LIFECYCLE: AtomicU32 = AtomicU32::new(LIFECYCLE_UNINITIALIZED);
 static GENERATION: AtomicU64 = AtomicU64::new(0);
 static SHUTDOWN_READY: AtomicBool = AtomicBool::new(false);
+static BOOTSTRAP: OnceLock<Mutex<Option<EuiBootstrap>>> = OnceLock::new();
 
 #[repr(C)]
 #[derive(Clone, Copy, Default)]
@@ -38,7 +44,7 @@ pub extern "C" fn codeg_eui_api_version() -> u32 {
 
 #[no_mangle]
 pub extern "C" fn codeg_eui_init(data_dir_utf8: *const u8, data_dir_len: usize) -> i32 {
-    ffi_status(|| {
+    let status = ffi_status(|| {
         if data_dir_utf8.is_null() && data_dir_len > 0 {
             return CODEG_EUI_ERR_NULL_POINTER;
         }
@@ -51,9 +57,26 @@ pub extern "C" fn codeg_eui_init(data_dir_utf8: *const u8, data_dir_len: usize)
         LIFECYCLE.store(LIFECYCLE_STARTING, Ordering::Release);
         GENERATION.store(0, Ordering::Release);
         SHUTDOWN_READY.store(false, Ordering::Release);
+
+        let argument_root = match parse_data_root_argument(data_dir_utf8, data_dir_len) {
+            Ok(argument_root) => argument_root,
+            Err(error) => return error,
+        };
+        let bootstrap = match EuiBootstrap::start_with_data_root_argument(argument_root) {
+            Ok(bootstrap) => bootstrap,
+            Err(_) => return CODEG_EUI_ERR_INVALID_STATE,
+        };
+        *bootstrap_slot()
+            .lock()
+            .unwrap_or_else(|error| error.into_inner()) = Some(bootstrap);
         LIFECYCLE.store(LIFECYCLE_RUNNING, Ordering::Release);
         CODEG_EUI_OK
-    })
+    });
+
+    if status != CODEG_EUI_OK && LIFECYCLE.load(Ordering::Acquire) == LIFECYCLE_STARTING {
+        LIFECYCLE.store(LIFECYCLE_STOPPED, Ordering::Release);
+    }
+    status
 }
 
 #[no_mangle]
@@ -114,8 +137,47 @@ pub extern "C" fn codeg_eui_shutdown() -> i32 {
             return CODEG_EUI_ERR_INVALID_STATE;
         }
 
+        let bootstrap = bootstrap_slot()
+            .lock()
+            .unwrap_or_else(|error| error.into_inner())
+            .take()
+            .ok_or(CODEG_EUI_ERR_INVALID_STATE);
+        let bootstrap = match bootstrap {
+            Ok(bootstrap) => bootstrap,
+            Err(error) => return error,
+        };
+        bootstrap.shutdown();
+
         SHUTDOWN_READY.store(false, Ordering::Release);
         LIFECYCLE.store(LIFECYCLE_STOPPED, Ordering::Release);
         CODEG_EUI_OK
     })
 }
+
+fn bootstrap_slot() -> &'static Mutex<Option<EuiBootstrap>> {
+    BOOTSTRAP.get_or_init(|| Mutex::new(None))
+}
+
+fn parse_data_root_argument(
+    data_dir_utf8: *const u8,
+    data_dir_len: usize,
+) -> Result<Option<PathBuf>, i32> {
+    if data_dir_len == 0 {
+        return Ok(None);
+    }
+    if data_dir_utf8.is_null() {
+        return Err(CODEG_EUI_ERR_NULL_POINTER);
+    }
+    if data_dir_len > CODEG_EUI_MAX_PATH_BYTES {
+        return Err(CODEG_EUI_ERR_INVALID_STATE);
+    }
+
+    // The ABI contract guarantees `data_dir_utf8` is readable for exactly
+    // `data_dir_len` bytes. Bounds and nullness are checked before this read.
+    let bytes = unsafe { std::slice::from_raw_parts(data_dir_utf8, data_dir_len) };
+    if bytes.contains(&0) {
+        return Err(CODEG_EUI_ERR_INVALID_STATE);
+    }
+    let path = std::str::from_utf8(bytes).map_err(|_| CODEG_EUI_ERR_INVALID_STATE)?;
+    Ok(Some(PathBuf::from(path)))
+}
diff --git a/src-tauri/codeg-eui-core/src/bootstrap.rs b/src-tauri/codeg-eui-core/src/bootstrap.rs
new file mode 100644
index 00000000..e575ee7f
--- /dev/null
+++ b/src-tauri/codeg-eui-core/src/bootstrap.rs
@@ -0,0 +1,141 @@
+use std::path::{Path, PathBuf};
+
+use codeg_lib::app_state::AppState;
+use codeg_lib::logging::init::LogGuard;
+use thiserror::Error;
+use tokio::runtime::{Builder, Runtime};
+
+use crate::data_root::{absolutize_from, startup_working_directory};
+use crate::{pin_eui_data_root, resolve_eui_data_root, DataRootError, EuiRootInputs};
+
+#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
+pub struct StartedServices {
+    pub web_server: bool,
+    pub auto_title: bool,
+    pub automation: bool,
+    pub chat_channels: bool,
+    pub pet_mapper: bool,
+    pub document_translation: bool,
+    pub reference_search: bool,
+    pub delegation_listener: bool,
+    pub delegation_supervisor: bool,
+    pub completion_outbox_dispatcher: bool,
+    pub updater: bool,
+}
+
+#[derive(Debug, Error)]
+pub enum BootstrapError {
+    #[error(transparent)]
+    DataRoot(#[from] DataRootError),
+    #[error("could not create EUI data root {path:?}: {source}")]
+    CreateDataRoot {
+        path: PathBuf,
+        #[source]
+        source: std::io::Error,
+    },
+    #[error("could not create the EUI Tokio runtime: {0}")]
+    Runtime(#[source] std::io::Error),
+    #[error("EUI database initialization failed: {0}")]
+    Database(String),
+    #[error("EUI AppState initialization failed: {0}")]
+    AppState(String),
+    #[error("EUI runtime initialization task failed: {0}")]
+    RuntimeTask(String),
+}
+
+pub struct EuiBootstrap {
+    pub state: AppState,
+    pub started_services: StartedServices,
+    runtime: Option<Runtime>,
+    _log_guard: Option<LogGuard>,
+}
+
+impl EuiBootstrap {
+    pub fn start() -> Result<Self, BootstrapError> {
+        Self::start_with_data_root_argument(None)
+    }
+
+    pub(crate) fn start_with_data_root_argument(
+        argument_root: Option<PathBuf>,
+    ) -> Result<Self, BootstrapError> {
+        let root = resolve_bootstrap_root(argument_root)?;
+        prepare_root(&root)?;
+        let log_guard = codeg_lib::logging::init::init_eui();
+        let runtime = build_runtime()?;
+        let state = runtime.block_on(initialize_state(root))?;
+
+        Ok(Self::new(state, runtime, log_guard))
+    }
+
+    pub async fn start_for_test(root: impl AsRef<Path>) -> Result<Self, BootstrapError> {
+        let root = absolutize_from(root.as_ref(), &startup_working_directory()?);
+        pin_eui_data_root(root.clone())?;
+        prepare_root(&root)?;
+        let log_guard = codeg_lib::logging::init::init_eui();
+        let runtime = build_runtime()?;
+        let state = runtime
+            .spawn(initialize_state(root))
+            .await
+            .map_err(|error| BootstrapError::RuntimeTask(error.to_string()))??;
+
+        Ok(Self::new(state, runtime, log_guard))
+    }
+
+    /// Join the owned runtime before releasing the shared application state.
+    pub fn shutdown(mut self) {
+        if let Some(runtime) = self.runtime.take() {
+            drop(runtime);
+        }
+    }
+
+    fn new(state: AppState, runtime: Runtime, log_guard: LogGuard) -> Self {
+        Self {
+            state,
+            started_services: StartedServices::default(),
+            runtime: Some(runtime),
+            _log_guard: Some(log_guard),
+        }
+    }
+}
+
+impl Drop for EuiBootstrap {
+    fn drop(&mut self) {
+        if let Some(runtime) = self.runtime.take() {
+            runtime.shutdown_background();
+        }
+    }
+}
+
+fn resolve_bootstrap_root(argument_root: Option<PathBuf>) -> Result<PathBuf, DataRootError> {
+    let root = match argument_root.filter(|path| !path.as_os_str().is_empty()) {
+        Some(root) => absolutize_from(&root, &startup_working_directory()?),
+        None => resolve_eui_data_root(&EuiRootInputs::from_process_environment()?)?,
+    };
+    pin_eui_data_root(root.clone())?;
+    Ok(root)
+}
+
+fn prepare_root(root: &Path) -> Result<(), BootstrapError> {
+    std::fs::create_dir_all(root).map_err(|source| BootstrapError::CreateDataRoot {
+        path: root.to_path_buf(),
+        source,
+    })
+}
+
+fn build_runtime() -> Result<Runtime, BootstrapError> {
+    Builder::new_multi_thread()
+        .enable_all()
+        .thread_name("codeg-eui-core")
+        .build()
+        .map_err(BootstrapError::Runtime)
+}
+
+async fn initialize_state(root: PathBuf) -> Result<AppState, BootstrapError> {
+    let db = codeg_lib::db::init_database(&root, env!("CARGO_PKG_VERSION"))
+        .await
+        .map_err(|error| BootstrapError::Database(error.to_string()))?;
+    codeg_lib::logging::init::apply_persisted_level(&db.conn).await;
+    AppState::new_eui(db, root)
+        .await
+        .map_err(|error| BootstrapError::AppState(error.to_string()))
+}
diff --git a/src-tauri/codeg-eui-core/src/data_root.rs b/src-tauri/codeg-eui-core/src/data_root.rs
new file mode 100644
index 00000000..c2de94a6
--- /dev/null
+++ b/src-tauri/codeg-eui-core/src/data_root.rs
@@ -0,0 +1,141 @@
+use std::env;
+use std::path::{Component, Path, PathBuf};
+use std::sync::OnceLock;
+
+use thiserror::Error;
+
+static STARTUP_WORKING_DIRECTORY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
+static PINNED_EUI_DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();
+
+#[derive(Debug, Clone, PartialEq, Eq)]
+pub struct EuiRootInputs {
+    pub codeg_eui_data_dir: Option<PathBuf>,
+    pub xdg_data_home: Option<PathBuf>,
+    pub home: Option<PathBuf>,
+    pub cwd: PathBuf,
+}
+
+impl EuiRootInputs {
+    pub fn from_process_environment() -> Result<Self, DataRootError> {
+        Ok(Self {
+            codeg_eui_data_dir: env::var_os("CODEG_EUI_DATA_DIR").map(PathBuf::from),
+            xdg_data_home: env::var_os("XDG_DATA_HOME").map(PathBuf::from),
+            home: env::var_os("HOME").map(PathBuf::from),
+            cwd: startup_working_directory()?,
+        })
+    }
+}
+
+#[derive(Debug, Error, Clone, PartialEq, Eq)]
+pub enum DataRootError {
+    #[error("neither CODEG_EUI_DATA_DIR, XDG_DATA_HOME, nor HOME is available")]
+    HomeUnavailable,
+    #[error("could not determine the startup working directory: {0}")]
+    CurrentDirectory(String),
+    #[error("the EUI data root contains an embedded NUL byte")]
+    EmbeddedNul,
+    #[error("the EUI data root is already pinned to {pinned:?}, not {requested:?}")]
+    AlreadyPinned { pinned: PathBuf, requested: PathBuf },
+}
+
+pub fn resolve_eui_data_root(input: &EuiRootInputs) -> Result<PathBuf, DataRootError> {
+    let candidate = input
+        .codeg_eui_data_dir
+        .as_ref()
+        .filter(|path| !path.as_os_str().is_empty())
+        .cloned()
+        .or_else(|| {
+            input
+                .xdg_data_home
+                .as_ref()
+                .filter(|path| !path.as_os_str().is_empty())
+                .map(|path| path.join("codeg-eui"))
+        })
+        .or_else(|| {
+            input
+                .home
+                .as_ref()
+                .filter(|path| !path.as_os_str().is_empty())
+                .map(|path| path.join(".local/share/codeg-eui"))
+        })
+        .ok_or(DataRootError::HomeUnavailable)?;
+
+    Ok(absolutize_from(&candidate, &input.cwd))
+}
+
+pub fn pin_eui_data_root(root: PathBuf) -> Result<(), DataRootError> {
+    let absolute = absolutize_without_requiring_existence(&root)?;
+    if absolute.as_os_str().as_encoded_bytes().contains(&0) {
+        return Err(DataRootError::EmbeddedNul);
+    }
+    verify_or_set_process_pin(&absolute)?;
+
+    // This function is a startup-only trust-boundary operation. Callers must
+    // invoke it before starting worker threads or environment-reading helpers.
+    env::remove_var("CODEG_HOME");
+    env::set_var("CODEG_DATA_DIR", &absolute);
+    Ok(())
+}
+
+pub(crate) fn absolutize_from(path: &Path, cwd: &Path) -> PathBuf {
+    let absolute = if path.is_absolute() {
+        path.to_path_buf()
+    } else {
+        cwd.join(path)
+    };
+    lexically_normalize(&absolute)
+}
+
+pub(crate) fn startup_working_directory() -> Result<PathBuf, DataRootError> {
+    STARTUP_WORKING_DIRECTORY
+        .get_or_init(|| env::current_dir().map_err(|error| error.to_string()))
+        .clone()
+        .map_err(DataRootError::CurrentDirectory)
+}
+
+fn absolutize_without_requiring_existence(root: &Path) -> Result<PathBuf, DataRootError> {
+    Ok(absolutize_from(root, &startup_working_directory()?))
+}
+
+fn verify_or_set_process_pin(requested: &PathBuf) -> Result<(), DataRootError> {
+    if let Some(pinned) = PINNED_EUI_DATA_ROOT.get() {
+        return roots_match(pinned, requested);
+    }
+
+    match PINNED_EUI_DATA_ROOT.set(requested.clone()) {
+        Ok(()) => Ok(()),
+        Err(_) => roots_match(
+            PINNED_EUI_DATA_ROOT
+                .get()
+                .expect("EUI data root is set after a failed OnceLock set"),
+            requested,
+        ),
+    }
+}
+
+fn roots_match(pinned: &PathBuf, requested: &PathBuf) -> Result<(), DataRootError> {
+    if pinned == requested {
+        Ok(())
+    } else {
+        Err(DataRootError::AlreadyPinned {
+            pinned: pinned.clone(),
+            requested: requested.clone(),
+        })
+    }
+}
+
+fn lexically_normalize(path: &Path) -> PathBuf {
+    let mut normalized = PathBuf::new();
+    for component in path.components() {
+        match component {
+            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
+            Component::RootDir => normalized.push(component.as_os_str()),
+            Component::CurDir => {}
+            Component::ParentDir => {
+                normalized.pop();
+            }
+            Component::Normal(part) => normalized.push(part),
+        }
+    }
+    normalized
+}
diff --git a/src-tauri/codeg-eui-core/src/lib.rs b/src-tauri/codeg-eui-core/src/lib.rs
index 70943065..154846e3 100644
--- a/src-tauri/codeg-eui-core/src/lib.rs
+++ b/src-tauri/codeg-eui-core/src/lib.rs
@@ -1,3 +1,7 @@
 mod abi;
+mod bootstrap;
+mod data_root;
 
 pub use abi::*;
+pub use bootstrap::{BootstrapError, EuiBootstrap, StartedServices};
+pub use data_root::{pin_eui_data_root, resolve_eui_data_root, DataRootError, EuiRootInputs};
diff --git a/src-tauri/codeg-eui-core/tests/abi_smoke.rs b/src-tauri/codeg-eui-core/tests/abi_smoke.rs
index d67d5aa7..d2ef718c 100644
--- a/src-tauri/codeg-eui-core/tests/abi_smoke.rs
+++ b/src-tauri/codeg-eui-core/tests/abi_smoke.rs
@@ -6,6 +6,9 @@ use codeg_eui_core::{
 
 #[test]
 fn abi_version_and_null_poll_are_stable() {
+    let temp = tempfile::tempdir().expect("tempdir");
+    let data_dir = temp.path().to_str().expect("UTF-8 temp path").as_bytes();
+
     assert_eq!(codeg_eui_api_version(), CODEG_EUI_API_VERSION);
     assert_eq!(CODEG_EUI_API_VERSION, 1);
     assert_eq!(
@@ -13,7 +16,10 @@ fn abi_version_and_null_poll_are_stable() {
         CODEG_EUI_ERR_NULL_POINTER
     );
 
-    assert_eq!(codeg_eui_init(std::ptr::null(), 0), CODEG_EUI_OK);
+    assert_eq!(
+        codeg_eui_init(data_dir.as_ptr(), data_dir.len()),
+        CODEG_EUI_OK
+    );
     assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_INVALID_STATE);
     assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
 
diff --git a/src-tauri/codeg-eui-core/tests/bootstrap_profile.rs b/src-tauri/codeg-eui-core/tests/bootstrap_profile.rs
new file mode 100644
index 00000000..ff7d5a72
--- /dev/null
+++ b/src-tauri/codeg-eui-core/tests/bootstrap_profile.rs
@@ -0,0 +1,48 @@
+use codeg_eui_core::EuiBootstrap;
+use codeg_lib::web::event_bridge::EventEmitter;
+
+#[test]
+fn eui_profile_is_web_only_and_keeps_auxiliary_services_dormant() {
+    let test_runtime = tokio::runtime::Builder::new_current_thread()
+        .enable_all()
+        .build()
+        .expect("test runtime");
+
+    let (bootstrap, _temp) = test_runtime.block_on(async {
+        let temp = tempfile::tempdir().expect("tempdir");
+        let bootstrap = EuiBootstrap::start_for_test(temp.path())
+            .await
+            .expect("EUI bootstrap");
+
+        assert_eq!(bootstrap.state.data_dir, temp.path());
+        assert!(matches!(
+            &bootstrap.state.emitter,
+            EventEmitter::WebOnly { .. }
+        ));
+        assert_eq!(
+            bootstrap
+                .state
+                .connection_manager
+                .list_connections()
+                .await
+                .len(),
+            0
+        );
+        assert!(!bootstrap.state.delegation_socket_path.exists());
+        assert!(!bootstrap.started_services.web_server);
+        assert!(!bootstrap.started_services.auto_title);
+        assert!(!bootstrap.started_services.automation);
+        assert!(!bootstrap.started_services.chat_channels);
+        assert!(!bootstrap.started_services.pet_mapper);
+        assert!(!bootstrap.started_services.document_translation);
+        assert!(!bootstrap.started_services.reference_search);
+        assert!(!bootstrap.started_services.delegation_listener);
+        assert!(!bootstrap.started_services.delegation_supervisor);
+        assert!(!bootstrap.started_services.completion_outbox_dispatcher);
+        assert!(!bootstrap.started_services.updater);
+
+        (bootstrap, temp)
+    });
+
+    bootstrap.shutdown();
+}
diff --git a/src-tauri/codeg-eui-core/tests/data_root_isolation.rs b/src-tauri/codeg-eui-core/tests/data_root_isolation.rs
new file mode 100644
index 00000000..b0fc9e59
--- /dev/null
+++ b/src-tauri/codeg-eui-core/tests/data_root_isolation.rs
@@ -0,0 +1,227 @@
+use std::path::{Path, PathBuf};
+use std::process::Command;
+use std::sync::Mutex;
+
+use codeg_eui_core::{
+    codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_shutdown,
+    pin_eui_data_root, resolve_eui_data_root, CodegEuiFrame, DataRootError, EuiBootstrap,
+    EuiRootInputs, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_OK,
+};
+use tempfile::TempDir;
+
+static PROCESS_ENV: Mutex<()> = Mutex::new(());
+const CHILD_CASE_ENV: &str = "CODEG_EUI_DATA_ROOT_TEST_CASE";
+
+#[test]
+fn ambient_main_data_dir_and_codeg_home_never_choose_the_eui_root() {
+    let inputs = EuiRootInputs {
+        codeg_eui_data_dir: None,
+        xdg_data_home: Some(PathBuf::from("/tmp/xdg")),
+        home: Some(PathBuf::from("/home/tester")),
+        cwd: PathBuf::from("/work"),
+    };
+
+    assert_eq!(
+        resolve_eui_data_root(&inputs).unwrap(),
+        PathBuf::from("/tmp/xdg/codeg-eui")
+    );
+}
+
+#[test]
+fn explicit_eui_root_is_absolutized() {
+    let inputs = EuiRootInputs {
+        codeg_eui_data_dir: Some(PathBuf::from("relative-eui")),
+        xdg_data_home: Some(PathBuf::from("/tmp/ignored")),
+        home: Some(PathBuf::from("/home/tester")),
+        cwd: PathBuf::from("/work"),
+    };
+
+    assert_eq!(
+        resolve_eui_data_root(&inputs).unwrap(),
+        PathBuf::from("/work/relative-eui")
+    );
+}
+
+#[test]
+fn empty_eui_root_falls_back_to_home_when_xdg_is_unavailable() {
+    let inputs = EuiRootInputs {
+        codeg_eui_data_dir: Some(PathBuf::new()),
+        xdg_data_home: None,
+        home: Some(PathBuf::from("/home/tester")),
+        cwd: PathBuf::from("/work"),
+    };
+
+    assert_eq!(
+        resolve_eui_data_root(&inputs).unwrap(),
+        PathBuf::from("/home/tester/.local/share/codeg-eui")
+    );
+}
+
+#[test]
+fn missing_eui_xdg_and_home_roots_is_an_error() {
+    let inputs = EuiRootInputs {
+        codeg_eui_data_dir: None,
+        xdg_data_home: None,
+        home: None,
+        cwd: PathBuf::from("/work"),
+    };
+
+    assert_eq!(
+        resolve_eui_data_root(&inputs),
+        Err(DataRootError::HomeUnavailable)
+    );
+}
+
+#[test]
+fn bootstrap_ignores_ambient_main_app_roots() {
+    let fixture = IsolationFixture::new();
+
+    run_child_case("bootstrap_from_environment", &fixture);
+
+    assert!(fixture.eui_root.join("codeg.db").is_file());
+    assert!(fixture.eui_root.join("logs").is_dir());
+    assert!(!fixture.main_data_root.join("codeg.db").exists());
+    assert!(!fixture.main_home_root.join("logs").exists());
+}
+
+#[test]
+fn abi_argument_root_overrides_eui_environment_and_remains_pinned() {
+    let fixture = IsolationFixture::new();
+
+    run_child_case("bootstrap_from_abi_argument", &fixture);
+
+    assert!(fixture.argument_root.join("codeg.db").is_file());
+    assert!(fixture.argument_root.join("logs").is_dir());
+    assert!(!fixture.eui_root.join("codeg.db").exists());
+    assert!(!fixture.main_data_root.join("codeg.db").exists());
+    assert!(!fixture.main_home_root.join("logs").exists());
+    assert!(!fixture.different_root.join("codeg.db").exists());
+}
+
+#[test]
+fn isolated_process_case() {
+    let Ok(case) = std::env::var(CHILD_CASE_ENV) else {
+        return;
+    };
+    let _env_guard = PROCESS_ENV
+        .lock()
+        .unwrap_or_else(|error| error.into_inner());
+
+    match case.as_str() {
+        "bootstrap_from_environment" => {
+            let eui_root = path_from_env("CODEG_EUI_DATA_DIR");
+            let bootstrap = EuiBootstrap::start().expect("bootstrap from EUI environment root");
+
+            assert_eq!(bootstrap.state.data_dir, eui_root);
+            assert_eq!(
+                std::env::var_os("CODEG_DATA_DIR"),
+                Some(eui_root.into_os_string())
+            );
+            assert!(std::env::var_os("CODEG_HOME").is_none());
+            bootstrap.shutdown();
+        }
+        "bootstrap_from_abi_argument" => {
+            let argument_root = path_from_env("CODEG_EUI_ARGUMENT_ROOT");
+            let different_root = path_from_env("CODEG_EUI_DIFFERENT_ROOT");
+            let argument = argument_root.to_str().expect("UTF-8 temp path").as_bytes();
+
+            assert_eq!(
+                pin_eui_data_root(PathBuf::from(String::from("invalid\0root"))),
+                Err(DataRootError::EmbeddedNul),
+                "an invalid environment value must not poison the process pin"
+            );
+
+            let invalid_utf8 = [0xff];
+            assert_eq!(
+                codeg_eui_init(invalid_utf8.as_ptr(), invalid_utf8.len()),
+                CODEG_EUI_ERR_INVALID_STATE
+            );
+            let oversized = vec![b'x'; 32_769];
+            assert_eq!(
+                codeg_eui_init(oversized.as_ptr(), oversized.len()),
+                CODEG_EUI_ERR_INVALID_STATE
+            );
+            let embedded_nul = b"invalid\0root";
+            assert_eq!(
+                codeg_eui_init(embedded_nul.as_ptr(), embedded_nul.len()),
+                CODEG_EUI_ERR_INVALID_STATE
+            );
+
+            assert_eq!(
+                codeg_eui_init(argument.as_ptr(), argument.len()),
+                CODEG_EUI_OK
+            );
+            assert_eq!(
+                std::env::var_os("CODEG_DATA_DIR"),
+                Some(argument_root.clone().into_os_string())
+            );
+            assert!(std::env::var_os("CODEG_HOME").is_none());
+            assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
+            let mut frame = CodegEuiFrame::default();
+            assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
+            assert_eq!(frame.shutdown_ready, 1);
+            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+
+            assert_eq!(
+                codeg_eui_init(argument.as_ptr(), argument.len()),
+                CODEG_EUI_OK,
+                "re-init with the same normalized root must remain legal"
+            );
+            assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
+            let mut second_frame = CodegEuiFrame::default();
+            assert_eq!(codeg_eui_poll(&mut second_frame), CODEG_EUI_OK);
+            assert_eq!(second_frame.shutdown_ready, 1);
+            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+
+            let different = different_root.to_str().expect("UTF-8 temp path").as_bytes();
+            assert_eq!(
+                codeg_eui_init(different.as_ptr(), different.len()),
+                CODEG_EUI_ERR_INVALID_STATE,
+                "a different root must return a stable init error"
+            );
+        }
+        other => panic!("unknown child case: {other}"),
+    }
+}
+
+struct IsolationFixture {
+    _temp: TempDir,
+    eui_root: PathBuf,
+    argument_root: PathBuf,
+    different_root: PathBuf,
+    main_data_root: PathBuf,
+    main_home_root: PathBuf,
+}
+
+impl IsolationFixture {
+    fn new() -> Self {
+        let temp = tempfile::tempdir().expect("tempdir");
+        Self {
+            eui_root: temp.path().join("eui"),
+            argument_root: temp.path().join("argument-eui"),
+            different_root: temp.path().join("different-eui"),
+            main_data_root: temp.path().join("main-data"),
+            main_home_root: temp.path().join("main-home"),
+            _temp: temp,
+        }
+    }
+}
+
+fn run_child_case(case: &str, fixture: &IsolationFixture) {
+    let status = Command::new(std::env::current_exe().expect("current test executable"))
+        .args(["--exact", "isolated_process_case", "--nocapture"])
+        .env(CHILD_CASE_ENV, case)
+        .env("CODEG_DATA_DIR", &fixture.main_data_root)
+        .env("CODEG_HOME", &fixture.main_home_root)
+        .env("CODEG_EUI_DATA_DIR", &fixture.eui_root)
+        .env("CODEG_EUI_ARGUMENT_ROOT", &fixture.argument_root)
+        .env("CODEG_EUI_DIFFERENT_ROOT", &fixture.different_root)
+        .status()
+        .expect("run isolated child test process");
+
+    assert!(status.success(), "child case {case} failed with {status}");
+}
+
+fn path_from_env(name: &str) -> PathBuf {
+    Path::new(&std::env::var_os(name).unwrap_or_else(|| panic!("{name} is set"))).to_path_buf()
+}
diff --git a/src-tauri/src/app_state.rs b/src-tauri/src/app_state.rs
index 9ea0c8b3..23ffcab7 100644
--- a/src-tauri/src/app_state.rs
+++ b/src-tauri/src/app_state.rs
@@ -10,6 +10,7 @@ use crate::acp::delegation::listener::TokenRegistry;
 use crate::acp::delegation::metrics::DelegationMetrics;
 use crate::acp::manager::ConnectionManager;
 use crate::acp::InternalEventBus;
+use crate::app_error::AppCommandError;
 use crate::auto_title::{AutoTitleCoordinator, InternalAgentSessionRegistry};
 use crate::chat_channel::manager::ChatChannelManager;
 use crate::commands::conversation_experience::ConversationExperienceMutationGate;
@@ -440,6 +441,85 @@ pub fn spawn_completion_outbox_dispatcher(dispatcher: Arc<CompletionOutboxDispat
 }
 
 impl AppState {
+    /// Build the shared-core profile used by the optional EUI native shell.
+    ///
+    /// Constructors required by shared command paths are present, but this
+    /// profile starts none of the auxiliary services excluded from the EUI
+    /// shell. The EUI bootstrap owns runtime startup and shutdown.
+    pub async fn new_eui(db: AppDatabase, data_dir: PathBuf) -> Result<Self, AppCommandError> {
+        let broadcaster = Arc::new(WebEventBroadcaster::new());
+        let metrics = Arc::new(crate::acp::EventBusMetrics::default());
+        let bus = Arc::new(InternalEventBus::new(metrics));
+        let emitter = EventEmitter::web_only(broadcaster.clone(), bus.clone());
+        let manager = ConnectionManager::new();
+        let internal_sessions = InternalAgentSessionRegistry::load(db.conn.clone(), &data_dir)
+            .await
+            .map_err(AppCommandError::from)?;
+        let chat_channel_manager = default_chat_channel_manager();
+        let conversation_experience_gate = Arc::new(ConversationExperienceMutationGate::default());
+        let db_handle = Arc::new(AppDatabase {
+            conn: db.conn.clone(),
+        });
+        let auto_title_coordinator = crate::auto_title::build_production_coordinator(
+            Arc::clone(&db_handle),
+            manager.clone_ref(),
+            chat_channel_manager.clone_ref(),
+            EventEmitter::Noop,
+            Arc::clone(&conversation_experience_gate),
+        );
+        let document_translation = DocumentTranslationService::new_disabled(Arc::clone(&db_handle));
+        let reference_search_registry = ReferenceSearchRegistry::new(
+            crate::commands::conversation_experience::DEFAULT_REFERENCE_SEARCH_LIMIT,
+            Arc::new(crate::reference_search::ProductionReferenceSourceFactory {
+                db: db.conn.clone(),
+            }),
+        );
+        let stack = build_delegation_stack(&manager, db.conn.clone(), data_dir.clone());
+        let completion_protocol_rollout =
+            Arc::new(crate::acp::delegation::workflow::CompletionProtocolRolloutConfig::default());
+        manager.install_completion_protocol_runtime(
+            Arc::clone(&completion_protocol_rollout),
+            Arc::clone(&stack.metrics),
+        );
+        let completion_outbox_dispatcher = Arc::new(
+            CompletionOutboxDispatcher::new(db_handle, emitter.clone())
+                .with_metrics(Arc::clone(&stack.metrics)),
+        );
+
+        Ok(Self {
+            db,
+            connection_manager: manager,
+            terminal_manager: default_terminal_manager(),
+            event_broadcaster: broadcaster,
+            acp_event_bus: bus,
+            emitter,
+            data_dir,
+            internal_sessions,
+            auto_title_coordinator,
+            document_translation,
+            conversation_experience_gate,
+            reference_search_registry,
+            web_server_state: WebServerState::new(),
+            chat_channel_manager,
+            workspace_transfer: Arc::new(WorkspaceTransferManager::new_from_env()),
+            pet_state: crate::pet_state_mapper::new_pet_state_handle(),
+            delegation_broker: stack.broker,
+            continuation_coordinator: stack.continuation_coordinator,
+            delegation_metrics: stack.metrics,
+            completion_protocol_rollout,
+            completion_outbox_dispatcher,
+            delegation_runtime_settings: stack.runtime_settings,
+            delegation_tokens: stack.tokens,
+            delegation_leases: stack.leases,
+            delegation_socket_path: stack.socket_path,
+            feedback_config: stack.feedback,
+            question_config: stack.ask,
+            session_info_config: stack.sessions,
+            system_op_lock: default_system_op_lock(),
+            update_state: default_update_state(),
+        })
+    }
+
     /// Test-only constructor: build an `AppState` wired to an in-memory
     /// database and a `WebOnly` event emitter. Suitable for axum-test driven
     /// HTTP integration tests where no Tauri runtime is available.
diff --git a/src-tauri/src/document_translate/service.rs b/src-tauri/src/document_translate/service.rs
index f74d20a6..1eeb0b30 100644
--- a/src-tauri/src/document_translate/service.rs
+++ b/src-tauri/src/document_translate/service.rs
@@ -21,7 +21,6 @@ use crate::auto_title::parse_supported_app_locale;
 use crate::commands::conversation_experience::load_document_translate_agent_from;
 use crate::db::AppDatabase;
 use crate::document_translate::protect::{protect_markdown, restore_markdown};
-#[cfg(any(test, feature = "test-utils"))]
 use crate::document_translate::runner::InertDocumentTranslateAgent;
 use crate::document_translate::runner::{
     DocumentConnectionDriver, DocumentTranslateAgent, DocumentTranslateRunner,
@@ -49,10 +48,15 @@ impl DocumentTranslationService {
         })
     }
 
-    /// Inert service for test AppState constructors that never call translate.
+    /// Disabled service for runtime profiles that never start translation.
+    pub fn new_disabled(db: Arc<AppDatabase>) -> Arc<Self> {
+        Self::new(db, Arc::new(InertDocumentTranslateAgent))
+    }
+
+    /// Backwards-compatible test alias for [`Self::new_disabled`].
     #[cfg(any(test, feature = "test-utils"))]
     pub fn new_inert(db: Arc<AppDatabase>) -> Arc<Self> {
-        Self::new(db, Arc::new(InertDocumentTranslateAgent))
+        Self::new_disabled(db)
     }
 
     /// Translate a document: validate → admit → protect → run → restore.
diff --git a/src-tauri/src/logging/init.rs b/src-tauri/src/logging/init.rs
index 5a736c4b..0173fcbf 100644
--- a/src-tauri/src/logging/init.rs
+++ b/src-tauri/src/logging/init.rs
@@ -16,6 +16,7 @@
 //!    `logs://appended` delivery to the viewer.
 
 use std::path::Path;
+use std::sync::{Arc, OnceLock};
 
 use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
 use tracing_appender::rolling::Rotation;
@@ -36,9 +37,11 @@ pub type ReloadHandle = reload::Handle<EnvFilter, Registry>;
 /// desktop `run()`); dropping it flushes and shuts down the writer thread.
 #[must_use = "hold the guard for the process lifetime so buffered logs flush on exit"]
 pub struct LogGuard {
-    _guard: Option<WorkerGuard>,
+    _guard: Option<Arc<WorkerGuard>>,
 }
 
+static EUI_LOG_GUARD: OnceLock<LogGuard> = OnceLock::new();
+
 /// Standing per-target backstops appended to EVERY constructed filter — the
 /// default/configured level, a persisted level, AND an explicit `RUST_LOG` /
 /// `CODEG_LOG` override. Two entries, both appended last so they win over a
@@ -240,11 +243,25 @@ pub fn init_server() -> LogGuard {
     init_with_file("codeg-server")
 }
 
+/// Phase 1 for the optional EUI native shell. The caller pins
+/// `CODEG_DATA_DIR` before invoking this function, so the file sink cannot
+/// inherit the main application's ambient root. Subscriber installation and
+/// its writer guard are process-wide because same-root EUI re-initialization
+/// is legal after an ABI shutdown.
+pub fn init_eui() -> LogGuard {
+    let process_guard = EUI_LOG_GUARD.get_or_init(|| init_with_file("codeg-eui"));
+    LogGuard {
+        _guard: process_guard._guard.clone(),
+    }
+}
+
 fn init_with_file(prefix: &str) -> LogGuard {
     let dir = crate::paths::codeg_logs_root();
     let (reload, guard) = build_subscriber(LogLevel::default(), Some(&dir), prefix);
     LogHub::install(reload);
-    LogGuard { _guard: guard }
+    LogGuard {
+        _guard: guard.map(Arc::new),
+    }
 }
 
 /// Install a **stderr-only** subscriber (no file / buffer / hub / emitter) for
@@ -259,7 +276,9 @@ fn init_with_file(prefix: &str) -> LogGuard {
 /// to buffer/emit, so `BufferEmitLayer` short-circuits.
 pub fn init_stderr_only() -> LogGuard {
     let (_reload, guard) = build_subscriber(LogLevel::default(), None, "");
-    LogGuard { _guard: guard }
+    LogGuard {
+        _guard: guard.map(Arc::new),
+    }
 }
 
 /// Phase 1 for `codeg-mcp`. See [`init_stderr_only`].
