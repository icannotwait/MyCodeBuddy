use std::env;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use thiserror::Error;

static STARTUP_WORKING_DIRECTORY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
static PINNED_EUI_DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();
#[cfg(test)]
static ENVIRONMENT_WRITE_PHASES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static ENVIRONMENT_WRITE_PAUSE: OnceLock<EnvironmentWritePause> = OnceLock::new();

#[cfg(test)]
struct EnvironmentWritePause {
    entered: std::sync::Barrier,
    release: std::sync::Barrier,
}

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
    let pinned = PINNED_EUI_DATA_ROOT.get_or_init(|| {
        // OnceLock publishes the root only after this startup-only
        // trust-boundary phase completes, so equal callers cannot proceed
        // while ambient environment values are still effective.
        #[cfg(test)]
        if let Some(pause) = ENVIRONMENT_WRITE_PAUSE.get() {
            pause.entered.wait();
            pause.release.wait();
        }
        env::remove_var("CODEG_HOME");
        env::set_var("CODEG_DATA_DIR", &absolute);
        #[cfg(test)]
        ENVIRONMENT_WRITE_PHASES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        absolute.clone()
    });

    roots_match(pinned, &absolute)
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

#[cfg(test)]
mod tests {
    use super::{
        pin_eui_data_root, EnvironmentWritePause, ENVIRONMENT_WRITE_PAUSE,
        ENVIRONMENT_WRITE_PHASES, PINNED_EUI_DATA_ROOT,
    };
    use crate::{
        codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_shutdown,
        CodegEuiFrame, CODEG_EUI_OK,
    };
    use std::sync::atomic::Ordering;
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::Duration;

    #[test]
    fn pin_lifecycle_publishes_only_after_environment_write_phase() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root_path = temp.path().to_path_buf();
        let pause = EnvironmentWritePause {
            entered: Barrier::new(2),
            release: Barrier::new(2),
        };
        assert!(ENVIRONMENT_WRITE_PAUSE.set(pause).is_ok());

        let first_root = root_path.clone();
        let first = std::thread::spawn(move || pin_eui_data_root(first_root));
        let pause = ENVIRONMENT_WRITE_PAUSE.get().expect("pause installed");
        pause.entered.wait();
        let published_early = PINNED_EUI_DATA_ROOT.get().is_some();

        let second_started = Arc::new(Barrier::new(2));
        let second_started_in_thread = Arc::clone(&second_started);
        let (second_done_tx, second_done_rx) = mpsc::channel();
        let second_root = root_path.clone();
        let second = std::thread::spawn(move || {
            second_started_in_thread.wait();
            second_done_tx
                .send(pin_eui_data_root(second_root))
                .expect("send second pin result");
        });
        second_started.wait();

        let early_result = second_done_rx.recv_timeout(Duration::from_millis(250));
        pause.release.wait();
        assert_eq!(first.join().expect("first pin thread"), Ok(()));
        let (returned_early, second_result) = match early_result {
            Ok(result) => (true, result),
            Err(mpsc::RecvTimeoutError::Timeout) => (
                false,
                second_done_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("equal pin returns after first pin completes"),
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("equal pin result channel disconnected")
            }
        };
        second.join().expect("second pin thread");

        assert!(
            !published_early,
            "root published before env write completed"
        );
        assert!(
            !returned_early,
            "equal pin returned before env write completed"
        );
        assert_eq!(second_result, Ok(()));
        assert_eq!(ENVIRONMENT_WRITE_PHASES.load(Ordering::SeqCst), 1);

        let root = root_path.to_str().expect("UTF-8 temp path").as_bytes();
        assert_eq!(codeg_eui_init(root.as_ptr(), root.len()), CODEG_EUI_OK);
        assert_eq!(ENVIRONMENT_WRITE_PHASES.load(Ordering::SeqCst), 1);
        complete_shutdown();

        assert_eq!(codeg_eui_init(root.as_ptr(), root.len()), CODEG_EUI_OK);
        assert_eq!(
            ENVIRONMENT_WRITE_PHASES.load(Ordering::SeqCst),
            1,
            "same-root restart must verify the pin without rewriting process env"
        );
        complete_shutdown();
    }

    fn complete_shutdown() {
        assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
        let mut frame = CodegEuiFrame::default();
        assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
        assert_eq!(frame.shutdown_ready, 1);
        assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
    }
}
