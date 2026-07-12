use std::path::{Path, PathBuf};

use crate::acp::error::AcpError;

pub const CODEX_ACP_OVERRIDE_ENV: &str = "CODEG_CODEX_ACP_BIN";

pub fn locate_bundled_executable(
    cmd: &str,
    override_env_key: &str,
) -> Result<Option<PathBuf>, AcpError> {
    let explicit = std::env::var_os(override_env_key).map(PathBuf::from);
    let filename = platform_filename(cmd);
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&filename)));
    let on_path = which::which(&filename).ok();
    select_bundled_executable(explicit.as_deref(), sibling, on_path)
}

fn select_bundled_executable(
    explicit: Option<&Path>,
    sibling: Option<PathBuf>,
    on_path: Option<PathBuf>,
) -> Result<Option<PathBuf>, AcpError> {
    if let Some(path) = explicit {
        if is_executable_file(path) {
            return Ok(Some(path.to_path_buf()));
        }
        return Err(AcpError::protocol(format!(
            "{CODEX_ACP_OVERRIDE_ENV} does not point to an executable file: {}",
            path.display()
        )));
    }
    Ok(sibling
        .filter(|path| is_executable_file(path))
        .or_else(|| on_path.filter(|path| is_executable_file(path))))
}

fn platform_filename(cmd: &str) -> String {
    if cfg!(windows) {
        format!("{cmd}.exe")
    } else {
        cmd.to_string()
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable_fixture(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"test").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn explicit_override_wins_over_sibling_and_path() {
        let temp = tempfile::tempdir().unwrap();
        let explicit = executable_fixture(temp.path(), "explicit.exe");
        let sibling = executable_fixture(temp.path(), "sibling.exe");
        let on_path = executable_fixture(temp.path(), "path.exe");
        assert_eq!(
            select_bundled_executable(Some(&explicit), Some(sibling), Some(on_path)).unwrap(),
            Some(explicit)
        );
    }

    #[test]
    fn invalid_explicit_override_is_an_error() {
        let missing = PathBuf::from("Z:/missing/codex-acp.exe");
        let error = select_bundled_executable(Some(&missing), None, None).unwrap_err();
        assert!(error.to_string().contains(CODEX_ACP_OVERRIDE_ENV));
    }

    #[test]
    fn sibling_wins_over_path_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let sibling = executable_fixture(temp.path(), "sibling.exe");
        let on_path = executable_fixture(temp.path(), "path.exe");
        assert_eq!(
            select_bundled_executable(None, Some(sibling.clone()), Some(on_path)).unwrap(),
            Some(sibling)
        );
    }

    #[test]
    fn no_candidates_returns_none() {
        assert_eq!(select_bundled_executable(None, None, None).unwrap(), None);
    }
}
