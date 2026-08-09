use std::path::{Path, PathBuf};
use std::sync::Arc;

use codeg_lib::app_state::AppState;
use codeg_lib::logging::init::LogGuard;
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::runtime::{Builder, Runtime};

use crate::data_root::{absolutize_from, startup_working_directory};
use crate::{pin_eui_data_root, resolve_eui_data_root, DataRootError, EuiRootInputs};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StartedServices {
    pub web_server: bool,
    pub auto_title: bool,
    pub automation: bool,
    pub chat_channels: bool,
    pub pet_mapper: bool,
    pub document_translation: bool,
    pub reference_search: bool,
    pub delegation_listener: bool,
    pub delegation_supervisor: bool,
    pub completion_outbox_dispatcher: bool,
    pub updater: bool,
}

#[derive(Debug, Error)]
pub enum BootstrapError {
    #[error(transparent)]
    DataRoot(#[from] DataRootError),
    #[error("could not create EUI data root {path:?}: {source}")]
    CreateDataRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not create the EUI Tokio runtime: {0}")]
    Runtime(#[source] std::io::Error),
    #[error("EUI database initialization failed: {0}")]
    Database(String),
    #[error("EUI AppState initialization failed: {0}")]
    AppState(String),
    #[error("EUI runtime initialization task failed: {0}")]
    RuntimeTask(String),
}

pub struct EuiBootstrap {
    pub state: Arc<AppState>,
    pub started_services: StartedServices,
    runtime: Option<Runtime>,
    _log_guard: Option<LogGuard>,
}

impl EuiBootstrap {
    pub fn start() -> Result<Self, BootstrapError> {
        Self::start_with_data_root_argument(None)
    }

    pub(crate) fn start_with_data_root_argument(
        argument_root: Option<PathBuf>,
    ) -> Result<Self, BootstrapError> {
        let root = resolve_bootstrap_root(argument_root)?;
        prepare_root(&root)?;
        let log_guard = codeg_lib::logging::init::init_eui();
        let runtime = build_runtime()?;
        let state = runtime.block_on(initialize_state(root))?;

        Ok(Self::new(Arc::new(state), runtime, log_guard))
    }

    pub async fn start_for_test(root: impl AsRef<Path>) -> Result<Self, BootstrapError> {
        let root = absolutize_from(root.as_ref(), &startup_working_directory()?);
        pin_eui_data_root(root.clone())?;
        prepare_root(&root)?;
        let log_guard = codeg_lib::logging::init::init_eui();
        let runtime = build_runtime()?;
        let state = runtime
            .spawn(initialize_state(root))
            .await
            .map_err(|error| BootstrapError::RuntimeTask(error.to_string()))??;

        Ok(Self::new(Arc::new(state), runtime, log_guard))
    }

    /// Join the owned runtime before releasing the shared application state.
    pub fn shutdown(mut self) {
        if let Some(runtime) = self.runtime.take() {
            drop(runtime);
        }
    }

    pub(crate) fn runtime_handle(&self) -> Handle {
        self.runtime
            .as_ref()
            .expect("EUI runtime available before shutdown")
            .handle()
            .clone()
    }

    fn new(state: Arc<AppState>, runtime: Runtime, log_guard: LogGuard) -> Self {
        Self {
            state,
            started_services: StartedServices::default(),
            runtime: Some(runtime),
            _log_guard: Some(log_guard),
        }
    }
}

impl Drop for EuiBootstrap {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

fn resolve_bootstrap_root(argument_root: Option<PathBuf>) -> Result<PathBuf, DataRootError> {
    let root = match argument_root.filter(|path| !path.as_os_str().is_empty()) {
        Some(root) => absolutize_from(&root, &startup_working_directory()?),
        None => resolve_eui_data_root(&EuiRootInputs::from_process_environment()?)?,
    };
    pin_eui_data_root(root.clone())?;
    Ok(root)
}

fn prepare_root(root: &Path) -> Result<(), BootstrapError> {
    std::fs::create_dir_all(root).map_err(|source| BootstrapError::CreateDataRoot {
        path: root.to_path_buf(),
        source,
    })
}

fn build_runtime() -> Result<Runtime, BootstrapError> {
    Builder::new_multi_thread()
        .enable_all()
        .thread_name("codeg-eui-core")
        .build()
        .map_err(BootstrapError::Runtime)
}

async fn initialize_state(root: PathBuf) -> Result<AppState, BootstrapError> {
    let db = codeg_lib::db::init_database(&root, env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|error| BootstrapError::Database(error.to_string()))?;
    codeg_lib::logging::init::apply_persisted_level(&db.conn).await;
    AppState::new_eui(db, root)
        .await
        .map_err(|error| BootstrapError::AppState(error.to_string()))
}
