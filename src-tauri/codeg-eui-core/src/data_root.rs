use std::env;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use thiserror::Error;

static STARTUP_WORKING_DIRECTORY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
static PINNED_EUI_DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EuiRootInputs {
    pub codeg_eui_data_dir: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub cwd: PathBuf,
}

impl EuiRootInputs {
    pub fn from_process_environment() -> Result<Self, DataRootError> {
        Ok(Self {
            codeg_eui_data_dir: env::var_os("CODEG_EUI_DATA_DIR").map(PathBuf::from),
            xdg_data_home: env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            home: env::var_os("HOME").map(PathBuf::from),
            cwd: startup_working_directory()?,
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DataRootError {
    #[error("neither CODEG_EUI_DATA_DIR, XDG_DATA_HOME, nor HOME is available")]
    HomeUnavailable,
    #[error("could not determine the startup working directory: {0}")]
    CurrentDirectory(String),
    #[error("the EUI data root contains an embedded NUL byte")]
    EmbeddedNul,
    #[error("the EUI data root is already pinned to {pinned:?}, not {requested:?}")]
    AlreadyPinned { pinned: PathBuf, requested: PathBuf },
}

pub fn resolve_eui_data_root(input: &EuiRootInputs) -> Result<PathBuf, DataRootError> {
    let candidate = input
        .codeg_eui_data_dir
        .as_ref()
        .filter(|path| !path.as_os_str().is_empty())
        .cloned()
        .or_else(|| {
            input
                .xdg_data_home
                .as_ref()
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| path.join("codeg-eui"))
        })
        .or_else(|| {
            input
                .home
                .as_ref()
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| path.join(".local/share/codeg-eui"))
        })
        .ok_or(DataRootError::HomeUnavailable)?;

    Ok(absolutize_from(&candidate, &input.cwd))
}

pub fn pin_eui_data_root(root: PathBuf) -> Result<(), DataRootError> {
    let absolute = absolutize_without_requiring_existence(&root)?;
    if absolute.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(DataRootError::EmbeddedNul);
    }
    verify_or_set_process_pin(&absolute)?;

    // This function is a startup-only trust-boundary operation. Callers must
    // invoke it before starting worker threads or environment-reading helpers.
    env::remove_var("CODEG_HOME");
    env::set_var("CODEG_DATA_DIR", &absolute);
    Ok(())
}

pub(crate) fn absolutize_from(path: &Path, cwd: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    lexically_normalize(&absolute)
}

pub(crate) fn startup_working_directory() -> Result<PathBuf, DataRootError> {
    STARTUP_WORKING_DIRECTORY
        .get_or_init(|| env::current_dir().map_err(|error| error.to_string()))
        .clone()
        .map_err(DataRootError::CurrentDirectory)
}

fn absolutize_without_requiring_existence(root: &Path) -> Result<PathBuf, DataRootError> {
    Ok(absolutize_from(root, &startup_working_directory()?))
}

fn verify_or_set_process_pin(requested: &PathBuf) -> Result<(), DataRootError> {
    if let Some(pinned) = PINNED_EUI_DATA_ROOT.get() {
        return roots_match(pinned, requested);
    }

    match PINNED_EUI_DATA_ROOT.set(requested.clone()) {
        Ok(()) => Ok(()),
        Err(_) => roots_match(
            PINNED_EUI_DATA_ROOT
                .get()
                .expect("EUI data root is set after a failed OnceLock set"),
            requested,
        ),
    }
}

fn roots_match(pinned: &PathBuf, requested: &PathBuf) -> Result<(), DataRootError> {
    if pinned == requested {
        Ok(())
    } else {
        Err(DataRootError::AlreadyPinned {
            pinned: pinned.clone(),
            requested: requested.clone(),
        })
    }
}

fn lexically_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}
