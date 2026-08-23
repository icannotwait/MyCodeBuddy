use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use tokio::sync::Semaphore;

use crate::app_error::{AppCommandError, AppErrorCode};

pub(crate) const FILE_BASE64_DEFAULT_MAX_BYTES: usize = 20_000_000;
pub(crate) const FILE_BASE64_MAX_BYTES: usize = 100_000_000;
const FILE_IO_MAX_CONCURRENT_OPS: usize = 8;

static FILE_IO_SEMAPHORE: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(FILE_IO_MAX_CONCURRENT_OPS));

#[derive(Debug)]
pub(crate) struct ConfinedRegularFile {
    pub canonical_path: PathBuf,
    pub metadata: std::fs::Metadata,
    pub file: std::fs::File,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Debug)]
pub(crate) enum ConfinedRead {
    Absent,
    Found(ConfinedRegularFile),
}

#[cfg(windows)]
pub(crate) fn metadata_is_symlink_or_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub(crate) fn metadata_is_symlink_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn validate_relative_path(path: &Path) -> Result<Vec<&std::ffi::OsStr>, AppCommandError> {
    let mut normal_components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normal_components.push(component),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(AppCommandError::invalid_input("Path must be relative"));
            }
        }
    }
    if normal_components.is_empty() {
        return Err(AppCommandError::invalid_input("Path must be relative"));
    }
    Ok(normal_components)
}

fn map_authority_root_io(error: std::io::Error) -> AppCommandError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return AppCommandError::new(AppErrorCode::IoError, "File authority root is unavailable")
            .with_detail(error.to_string());
    }
    AppCommandError::io(error)
}

fn invalid_alias() -> AppCommandError {
    AppCommandError::invalid_input("Path is outside workspace root")
}

fn is_definitive_alias_resolution_failure(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ELOOP) {
        return true;
    }
    #[cfg(windows)]
    if error.raw_os_error() == Some(1921) {
        return true;
    }
    false
}

fn is_alias_resolution_failure(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || is_definitive_alias_resolution_failure(error)
}

fn map_alias_resolution_error(error: std::io::Error, alias_observed: bool) -> AppCommandError {
    if is_definitive_alias_resolution_failure(&error)
        || (alias_observed && is_alias_resolution_failure(&error))
    {
        invalid_alias()
    } else {
        AppCommandError::io(error)
    }
}

fn size_error(max_bytes: usize) -> AppCommandError {
    AppCommandError::invalid_input("File is too large to attach")
        .with_detail(format!("max_bytes={max_bytes}"))
}

pub(crate) fn read_confined_regular_file(
    authority_root: &Path,
    validated_relative_path: &Path,
    required_direct_parent: Option<&Path>,
    max_bytes: usize,
    read_bytes: bool,
) -> Result<ConfinedRead, AppCommandError> {
    let relative_components = validate_relative_path(validated_relative_path)?;
    if let Some(required_parent) = required_direct_parent {
        validate_relative_path(required_parent)?;
    }

    let authority_lexical_metadata = match std::fs::symlink_metadata(authority_root) {
        Ok(metadata) => metadata,
        Err(error) => return Err(map_authority_root_io(error)),
    };
    let authority_is_alias = metadata_is_symlink_or_reparse(&authority_lexical_metadata);
    let canonical_root = std::fs::canonicalize(authority_root).map_err(|error| {
        if authority_is_alias && is_alias_resolution_failure(&error) {
            invalid_alias()
        } else {
            map_authority_root_io(error)
        }
    })?;
    let root_metadata = std::fs::metadata(&canonical_root).map_err(map_authority_root_io)?;
    if !root_metadata.is_dir() {
        return Err(invalid_alias());
    }

    let mut lexical_prefix = authority_root.to_path_buf();
    let mut alias_observed = false;
    for (index, component) in relative_components.iter().enumerate() {
        lexical_prefix.push(component);
        let is_final = index + 1 == relative_components.len();
        let lexical_metadata = match std::fs::symlink_metadata(&lexical_prefix) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ConfinedRead::Absent);
            }
            Err(error) => return Err(AppCommandError::io(error)),
        };
        let is_alias = metadata_is_symlink_or_reparse(&lexical_metadata);
        alias_observed |= is_alias;

        if !is_final {
            if is_alias {
                let canonical_prefix = std::fs::canonicalize(&lexical_prefix)
                    .map_err(|error| map_alias_resolution_error(error, true))?;
                let metadata = std::fs::metadata(&canonical_prefix)
                    .map_err(|error| map_alias_resolution_error(error, true))?;
                if !canonical_prefix.starts_with(&canonical_root) || !metadata.is_dir() {
                    return Err(invalid_alias());
                }
            } else if !lexical_metadata.is_dir() {
                return Err(AppCommandError::invalid_input("Path is not a file"));
            }
        }
    }

    let canonical_required_parent = if let Some(required_parent) = required_direct_parent {
        let required_parent_path = authority_root.join(required_parent);
        let canonical_parent = std::fs::canonicalize(&required_parent_path)
            .map_err(|error| map_alias_resolution_error(error, alias_observed))?;
        let parent_metadata = std::fs::metadata(&canonical_parent)
            .map_err(|error| map_alias_resolution_error(error, alias_observed))?;
        if !canonical_parent.starts_with(&canonical_root) || !parent_metadata.is_dir() {
            return Err(invalid_alias());
        }
        Some(canonical_parent)
    } else {
        None
    };

    let target = authority_root.join(validated_relative_path);
    let canonical_target = match std::fs::canonicalize(&target) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !alias_observed => {
            return Ok(ConfinedRead::Absent);
        }
        Err(error) => return Err(map_alias_resolution_error(error, alias_observed)),
    };
    if !canonical_target.starts_with(&canonical_root) {
        return Err(invalid_alias());
    }
    if let Some(canonical_parent) = canonical_required_parent.as_ref() {
        if canonical_target.parent() != Some(canonical_parent.as_path()) {
            return Err(invalid_alias());
        }
    }

    let target_metadata = match std::fs::symlink_metadata(&canonical_target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !alias_observed => {
            return Ok(ConfinedRead::Absent);
        }
        Err(error) => return Err(map_alias_resolution_error(error, alias_observed)),
    };
    if metadata_is_symlink_or_reparse(&target_metadata) || !target_metadata.is_file() {
        return Err(AppCommandError::invalid_input("Path is not a file"));
    }

    let mut file = match open_no_follow(&canonical_target) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !alias_observed => {
            return Ok(ConfinedRead::Absent);
        }
        Err(error) => return Err(map_alias_resolution_error(error, alias_observed)),
    };
    let metadata = file.metadata().map_err(AppCommandError::io)?;
    if metadata_is_symlink_or_reparse(&metadata) || !metadata.is_file() {
        return Err(AppCommandError::invalid_input("Path is not a file"));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(size_error(max_bytes));
    }
    let bytes = if read_bytes {
        Some(read_bounded(&mut file, max_bytes)?)
    } else {
        None
    };

    Ok(ConfinedRead::Found(ConfinedRegularFile {
        canonical_path: canonical_target,
        metadata,
        file,
        bytes,
    }))
}

fn read_bounded<R: std::io::Read>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Vec<u8>, AppCommandError> {
    let read_limit = (max_bytes as u64).saturating_add(1);
    let mut limited = std::io::Read::take(reader, read_limit);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut limited, &mut bytes).map_err(AppCommandError::io)?;
    if bytes.len() > max_bytes {
        return Err(size_error(max_bytes));
    }
    Ok(bytes)
}

pub(crate) async fn run_file_io<T, F>(f: F) -> Result<T, AppCommandError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, AppCommandError> + Send + 'static,
{
    let _permit = FILE_IO_SEMAPHORE
        .acquire()
        .await
        .map_err(|_| AppCommandError::task_execution_failed("File I/O runtime is unavailable"))?;

    tokio::task::spawn_blocking(f).await.map_err(|error| {
        AppCommandError::task_execution_failed("File I/O task failed")
            .with_detail(error.to_string())
    })?
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_error::AppErrorCode;

    fn write(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn bounded_read_returns_canonical_path_metadata_handle_and_bytes() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "images/a.png", b"abc");
        let ConfinedRead::Found(mut found) = read_confined_regular_file(
            temp.path(),
            Path::new("images/a.png"),
            Some(Path::new("images")),
            3,
            true,
        )
        .unwrap() else {
            panic!("expected file")
        };
        assert_eq!(found.bytes.take().unwrap(), b"abc");
        assert_eq!(found.metadata.len(), 3);
        assert_eq!(
            found.canonical_path,
            std::fs::canonicalize(temp.path().join("images/a.png")).unwrap()
        );
        assert!(found.file.metadata().unwrap().is_file());
    }

    #[test]
    fn read_bytes_false_keeps_the_open_handle_without_buffering() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "images/a.png", b"abc");
        let ConfinedRead::Found(found) = read_confined_regular_file(
            temp.path(),
            Path::new("images/a.png"),
            Some(Path::new("images")),
            3,
            false,
        )
        .unwrap() else {
            panic!("expected file")
        };
        assert!(found.bytes.is_none());
        assert_eq!(found.metadata.len(), 3);
    }

    #[test]
    fn missing_ordinary_component_is_absent() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("images")).unwrap();
        assert!(matches!(
            read_confined_regular_file(
                temp.path(),
                Path::new("images/missing.png"),
                Some(Path::new("images")),
                20,
                true
            )
            .unwrap(),
            ConfinedRead::Absent
        ));
    }

    #[test]
    fn missing_authority_root_is_io_error_not_candidate_absence() {
        let temp = tempfile::tempdir().unwrap();
        let missing_root = temp.path().join("missing-root");
        let error = read_confined_regular_file(
            &missing_root,
            Path::new("images/a.png"),
            Some(Path::new("images")),
            20,
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, AppErrorCode::IoError);
    }

    #[test]
    fn absolute_parent_and_non_regular_targets_reject() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("images/dir.png")).unwrap();
        for rel in [
            Path::new("../outside.png"),
            Path::new("/tmp/outside.png"),
            Path::new("images/dir.png"),
        ] {
            let error =
                read_confined_regular_file(temp.path(), rel, Some(Path::new("images")), 20, true)
                    .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[test]
    fn required_parent_must_be_a_nonempty_relative_normal_path() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "images/a.png", b"abc");
        for required_parent in [Path::new(""), Path::new("../images"), Path::new("/images")] {
            let error = read_confined_regular_file(
                temp.path(),
                Path::new("images/a.png"),
                Some(required_parent),
                20,
                true,
            )
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[test]
    fn non_directory_required_parent_rejects_without_opening_it() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("images"), b"not a directory").unwrap();
        let error = read_confined_regular_file(
            temp.path(),
            Path::new("images/a.png"),
            Some(Path::new("images")),
            20,
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[test]
    fn metadata_limit_enforces_the_cap() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "images/a.png", b"abcd");
        let error = read_confined_regular_file(
            temp.path(),
            Path::new("images/a.png"),
            Some(Path::new("images")),
            3,
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
        assert_eq!(error.detail.as_deref(), Some("max_bytes=3"));
    }

    #[test]
    fn bounded_read_rejects_growth_past_the_metadata_cap() {
        let mut bytes = std::io::Cursor::new(b"abcd");
        let error = read_bounded(&mut bytes, 3).unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
        assert_eq!(error.detail.as_deref(), Some("max_bytes=3"));
    }

    #[test]
    fn nondirectory_authority_root_rejects() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root-file");
        std::fs::write(&root, b"not a directory").unwrap();
        let error = read_confined_regular_file(
            &root,
            Path::new("images/a.png"),
            Some(Path::new("images")),
            20,
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn definitive_alias_failure_is_invalid_without_prior_observation() {
        #[cfg(unix)]
        let raw_os_error = libc::ELOOP;
        #[cfg(windows)]
        let raw_os_error = 1921;
        let error =
            map_alias_resolution_error(std::io::Error::from_raw_os_error(raw_os_error), false);
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{symlink, PermissionsExt};

        #[test]
        fn direct_file_symlink_within_same_canonical_images_directory_passes() {
            let temp = tempfile::tempdir().unwrap();
            write(temp.path(), "images/real.png", b"abc");
            symlink("real.png", temp.path().join("images/alias.png")).unwrap();
            let ConfinedRead::Found(found) = read_confined_regular_file(
                temp.path(),
                Path::new("images/alias.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap() else {
                panic!("expected file")
            };
            assert_eq!(found.bytes.unwrap(), b"abc");
        }

        #[test]
        fn valid_authority_root_symlink_passes() {
            let real = tempfile::tempdir().unwrap();
            write(real.path(), "images/a.png", b"abc");
            let aliases = tempfile::tempdir().unwrap();
            let alias = aliases.path().join("root-alias");
            symlink(real.path(), &alias).unwrap();
            let ConfinedRead::Found(found) = read_confined_regular_file(
                &alias,
                Path::new("images/a.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap() else {
                panic!("expected file")
            };
            assert_eq!(found.bytes.unwrap(), b"abc");
        }

        #[test]
        fn escaping_final_symlink_rejects() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("images")).unwrap();
            let outside = tempfile::NamedTempFile::new().unwrap();
            symlink(outside.path(), root.path().join("images/escape.png")).unwrap();
            let error = read_confined_regular_file(
                root.path(),
                Path::new("images/escape.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }

        #[test]
        fn dangling_final_symlink_rejects() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("images")).unwrap();
            symlink("missing.png", root.path().join("images/alias.png")).unwrap();
            let error = read_confined_regular_file(
                root.path(),
                Path::new("images/alias.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }

        #[test]
        fn escaping_images_alias_rejects() {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            write(outside.path(), "a.png", b"abc");
            symlink(outside.path(), root.path().join("images")).unwrap();
            let error = read_confined_regular_file(
                root.path(),
                Path::new("images/a.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }

        #[test]
        fn dangling_images_alias_rejects() {
            let root = tempfile::tempdir().unwrap();
            symlink("missing-images", root.path().join("images")).unwrap();
            let error = read_confined_regular_file(
                root.path(),
                Path::new("images/a.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }

        #[test]
        fn missing_ordinary_component_after_valid_directory_alias_is_absent() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("real-images")).unwrap();
            symlink("real-images", root.path().join("images")).unwrap();
            assert!(matches!(
                read_confined_regular_file(
                    root.path(),
                    Path::new("images/missing.png"),
                    None,
                    20,
                    true,
                )
                .unwrap(),
                ConfinedRead::Absent
            ));
        }

        #[test]
        fn dangling_authority_root_rejects() {
            let temp = tempfile::tempdir().unwrap();
            let alias = temp.path().join("root-alias");
            symlink("missing-root", &alias).unwrap();
            let error = read_confined_regular_file(
                &alias,
                Path::new("images/a.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }

        #[test]
        fn fifo_candidate_rejects() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("images")).unwrap();
            let fifo = root.path().join("images/a.png");
            let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
            // SAFETY: fifo_c is a valid, null-terminated path string.
            assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
            let error = read_confined_regular_file(
                root.path(),
                Path::new("images/a.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }

        struct RestoreMode {
            path: PathBuf,
            mode: u32,
        }

        impl Drop for RestoreMode {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(
                    &self.path,
                    std::fs::Permissions::from_mode(self.mode),
                );
            }
        }

        #[test]
        fn unreadable_images_directory_preserves_permission_denied() {
            let root = tempfile::tempdir().unwrap();
            write(root.path(), "images/a.png", b"abc");
            let images = root.path().join("images");
            let original_mode = std::fs::metadata(&images).unwrap().permissions().mode();
            let _restore = RestoreMode {
                path: images.clone(),
                mode: original_mode,
            };
            std::fs::set_permissions(&images, std::fs::Permissions::from_mode(0o0)).unwrap();
            let result = read_confined_regular_file(
                root.path(),
                Path::new("images/a.png"),
                Some(Path::new("images")),
                20,
                true,
            );
            let Err(error) = result else {
                return;
            };
            assert_eq!(error.code, AppErrorCode::PermissionDenied);
        }

        #[test]
        fn symlink_loop_is_rejected_not_absent() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("images")).unwrap();
            symlink("b.png", root.path().join("images/a.png")).unwrap();
            symlink("a.png", root.path().join("images/b.png")).unwrap();
            let error = read_confined_regular_file(
                root.path(),
                Path::new("images/a.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use std::os::windows::fs::{symlink_dir, symlink_file};

        fn symlink_or_skip(result: std::io::Result<()>) -> bool {
            match result {
                Ok(()) => true,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => false,
                Err(error) => panic!("failed to create symlink: {error}"),
            }
        }

        #[test]
        fn windows_in_images_file_symlink_passes_when_creation_is_available() {
            let root = tempfile::tempdir().unwrap();
            write(root.path(), "images/real.png", b"abc");
            if !symlink_or_skip(symlink_file(
                "real.png",
                root.path().join("images/alias.png"),
            )) {
                return;
            }
            let ConfinedRead::Found(found) = read_confined_regular_file(
                root.path(),
                Path::new("images/alias.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap() else {
                panic!("expected file")
            };
            assert_eq!(found.bytes.unwrap(), b"abc");
        }

        #[test]
        fn windows_reparse_escape_rejects_when_creation_is_available() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("images")).unwrap();
            let outside = tempfile::NamedTempFile::new().unwrap();
            if !symlink_or_skip(symlink_file(
                outside.path(),
                root.path().join("images/escape.png"),
            )) {
                return;
            }
            let error = read_confined_regular_file(
                root.path(),
                Path::new("images/escape.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }

        #[test]
        fn windows_dangling_authority_reparse_rejects_when_creation_is_available() {
            let temp = tempfile::tempdir().unwrap();
            let alias = temp.path().join("root-alias");
            if !symlink_or_skip(symlink_dir(temp.path().join("missing-root"), &alias)) {
                return;
            }
            let error = read_confined_regular_file(
                &alias,
                Path::new("images/a.png"),
                Some(Path::new("images")),
                20,
                true,
            )
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }
}
