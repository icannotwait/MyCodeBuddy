//! Exclusive `save_translation_as` path validation and write.
//!
//! Workspace isolation: relative path only, no `..`, no absolute segments,
//! no symlink parents, exclusive create (never overwrite).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::app_error::{AppCommandError, AppErrorCode};
use crate::document_translate::types::{
    I18N_SAVE_ALREADY_EXISTS, I18N_SAVE_PATH_REJECTED, SaveTranslationAsResult,
};

/// Soft size guard aligned with editor save limits (bytes of UTF-8).
const SAVE_CONTENT_HARD_LIMIT: usize = 50_000_000;

/// Validate `relative_path` and resolve the exclusive-create target under `root`.
///
/// Parent directory must already exist. Rejects absolute paths, `..`, empty
/// names, and any symlink in the path chain from root through the parent.
pub fn resolve_save_target(
    root: &Path,
    relative_path: &str,
) -> Result<PathBuf, AppCommandError> {
    let trimmed = relative_path.trim();
    if trimmed.is_empty() {
        return Err(path_rejected("Relative path cannot be empty"));
    }

    let rel = Path::new(trimmed);
    if rel.is_absolute() {
        return Err(path_rejected("Path must be relative"));
    }

    let mut has_normal = false;
    for component in rel.components() {
        match component {
            Component::Normal(seg) => {
                let s = seg.to_string_lossy();
                if s.is_empty() || s == "." || s == ".." {
                    return Err(path_rejected("Invalid path component"));
                }
                has_normal = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(path_rejected("Path cannot contain '..'"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(path_rejected("Invalid path component"));
            }
        }
    }
    if !has_normal {
        return Err(path_rejected("Relative path must include a file name"));
    }

    if !root.exists() || !root.is_dir() {
        return Err(AppCommandError::not_found("Folder does not exist"));
    }

    let joined = root.join(rel);
    let parent = joined.parent().ok_or_else(|| {
        path_rejected("Cannot determine parent directory for target file")
    })?;

    // Walk root → parent; reject symlink components (do not follow).
    ensure_no_symlink_in_chain(root, parent)?;

    if !parent.exists() {
        return Err(AppCommandError::not_found("Parent directory does not exist")
            .with_detail(parent.display().to_string())
            .with_i18n(
                I18N_SAVE_PATH_REJECTED,
                std::collections::BTreeMap::new(),
            ));
    }

    let parent_meta = std::fs::symlink_metadata(parent).map_err(AppCommandError::io)?;
    if parent_meta.file_type().is_symlink() {
        return Err(path_rejected("Parent path must not be a symlink"));
    }
    if !parent_meta.is_dir() {
        return Err(path_rejected("Parent path is not a directory"));
    }

    let canonical_root = std::fs::canonicalize(root).map_err(AppCommandError::io)?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(AppCommandError::io)?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(path_rejected("Resolved path escapes workspace root"));
    }

    let file_name = joined.file_name().ok_or_else(|| {
        path_rejected("Relative path must include a file name")
    })?;
    Ok(canonical_parent.join(file_name))
}

/// Exclusive create + write under `root`. Returns absolute path of the new file.
pub fn save_translation_as_to_root(
    root: &Path,
    relative_path: &str,
    content: &str,
) -> Result<SaveTranslationAsResult, AppCommandError> {
    if content.len() > SAVE_CONTENT_HARD_LIMIT {
        return Err(AppCommandError::invalid_input(
            "File is too large to save",
        )
        .with_detail(format!("max_bytes={SAVE_CONTENT_HARD_LIMIT}")));
    }

    let target = resolve_save_target(root, relative_path)?;

    // Race-safe existence check: create_new is the authority.
    match std::fs::symlink_metadata(&target) {
        Ok(_) => {
            return Err(already_exists_err());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(AppCommandError::io(e)),
    }

    // Open exclusively first. Only remove on later write/sync failure after
    // *this* call created the file — never delete on AlreadyExists (another
    // exclusive creator may have won the race).
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(already_exists_err());
        }
        Err(e) => return Err(AppCommandError::io(e)),
    };

    let write_result = (|| -> Result<(), AppCommandError> {
        file.write_all(content.as_bytes())
            .map_err(AppCommandError::io)?;
        // Best-effort fsync; ignore failure on platforms where it is flaky.
        let _ = file.sync_all();
        Ok(())
    })();

    if let Err(err) = write_result {
        drop(file);
        let _ = std::fs::remove_file(&target);
        return Err(err);
    }
    drop(file);

    let absolute = std::fs::canonicalize(&target)
        .unwrap_or(target)
        .to_string_lossy()
        .to_string();

    Ok(SaveTranslationAsResult {
        absolute_path: absolute,
    })
}

fn path_rejected(message: impl Into<String>) -> AppCommandError {
    AppCommandError::new(AppErrorCode::InvalidInput, message)
        .with_i18n(I18N_SAVE_PATH_REJECTED, std::collections::BTreeMap::new())
}

fn already_exists_err() -> AppCommandError {
    AppCommandError::already_exists("A file already exists at the save path")
        .with_i18n(
            I18N_SAVE_ALREADY_EXISTS,
            std::collections::BTreeMap::new(),
        )
}

/// Walk from `root` toward `target` (inclusive of intermediate segments).
/// Rejects if any existing component is a symlink.
fn ensure_no_symlink_in_chain(root: &Path, target: &Path) -> Result<(), AppCommandError> {
    let rel = match target.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => {
            // target may equal root (save in workspace root) — no chain.
            if target == root {
                return Ok(());
            }
            // Non-canonical join: compare after we still walk by components.
            // Fall through using joined path components from root.
            return walk_joined(root, target);
        }
    };
    let mut current = root.to_path_buf();
    for component in rel.components() {
        let segment = match component {
            Component::Normal(s) => s,
            Component::CurDir => continue,
            _ => return Err(path_rejected("Invalid path component while validating save target")),
        };
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(md) => {
                if md.file_type().is_symlink() {
                    return Err(path_rejected(
                        "Save path traverses a symlink; refuse to follow it",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(e) => return Err(AppCommandError::io(e)),
        }
    }
    Ok(())
}

fn walk_joined(root: &Path, target: &Path) -> Result<(), AppCommandError> {
    // Best-effort: if target is not under root by strip_prefix (e.g. different
    // path forms), reject rather than risk escape.
    let _ = (root, target);
    Err(path_rejected("Target path is not under workspace root"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn rejects_parent_dir_traversal() {
        let root = temp_root();
        let err = resolve_save_target(root.path(), "../escape.txt").unwrap_err();
        assert_eq!(err.code, AppErrorCode::InvalidInput);
        assert_eq!(err.i18n_key.as_deref(), Some(I18N_SAVE_PATH_REJECTED));
    }

    #[test]
    fn rejects_nested_parent_dir_traversal() {
        let root = temp_root();
        std::fs::create_dir_all(root.path().join("sub")).unwrap();
        let err = resolve_save_target(root.path(), "sub/../../escape.txt").unwrap_err();
        assert_eq!(err.i18n_key.as_deref(), Some(I18N_SAVE_PATH_REJECTED));
    }

    #[test]
    fn rejects_absolute_path() {
        let root = temp_root();
        #[cfg(windows)]
        let abs = "C:\\Windows\\Temp\\out.md";
        #[cfg(not(windows))]
        let abs = "/etc/passwd";
        let err = resolve_save_target(root.path(), abs).unwrap_err();
        assert_eq!(err.i18n_key.as_deref(), Some(I18N_SAVE_PATH_REJECTED));
    }

    #[test]
    fn rejects_empty_relative_path() {
        let root = temp_root();
        let err = resolve_save_target(root.path(), "   ").unwrap_err();
        assert_eq!(err.i18n_key.as_deref(), Some(I18N_SAVE_PATH_REJECTED));
    }

    #[test]
    fn happy_path_exclusive_create_writes_content() {
        let root = temp_root();
        let result =
            save_translation_as_to_root(root.path(), "README.zh_cn.md", "你好").unwrap();
        let path = PathBuf::from(&result.absolute_path);
        assert!(path.is_absolute());
        assert!(path.starts_with(std::fs::canonicalize(root.path()).unwrap()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "你好");
    }

    #[test]
    fn rejects_when_file_already_exists() {
        let root = temp_root();
        let rel = "exists.md";
        std::fs::write(root.path().join(rel), b"old").unwrap();
        let err = save_translation_as_to_root(root.path(), rel, "new").unwrap_err();
        assert_eq!(err.code, AppErrorCode::AlreadyExists);
        assert_eq!(err.i18n_key.as_deref(), Some(I18N_SAVE_ALREADY_EXISTS));
        assert_eq!(std::fs::read_to_string(root.path().join(rel)).unwrap(), "old");
    }

    #[test]
    fn concurrent_two_creates_one_wins() {
        let root = temp_root();
        let root_path = root.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));
        let rel = "race.md";

        let b1 = Arc::clone(&barrier);
        let r1 = root_path.clone();
        let h1 = thread::spawn(move || {
            b1.wait();
            save_translation_as_to_root(&r1, rel, "first")
        });

        let b2 = Arc::clone(&barrier);
        let r2 = root_path.clone();
        let h2 = thread::spawn(move || {
            b2.wait();
            save_translation_as_to_root(&r2, rel, "second")
        });

        let a = h1.join().unwrap();
        let b = h2.join().unwrap();
        let wins = a.is_ok() as u8 + b.is_ok() as u8;
        let fails = a.is_err() as u8 + b.is_err() as u8;
        assert_eq!(
            wins, 1,
            "exactly one exclusive create must succeed; a={a:?} b={b:?}"
        );
        assert_eq!(
            fails, 1,
            "exactly one exclusive create must fail; a={a:?} b={b:?}"
        );
        let fail = if a.is_err() {
            a.as_ref().unwrap_err()
        } else {
            b.as_ref().unwrap_err()
        };
        assert_eq!(fail.code, AppErrorCode::AlreadyExists);
        assert_eq!(fail.i18n_key.as_deref(), Some(I18N_SAVE_ALREADY_EXISTS));
        // Prefer winner's absolute path (canonical) over joining the temp root.
        let path = a
            .as_ref()
            .or(b.as_ref())
            .map(|r| PathBuf::from(&r.absolute_path))
            .unwrap_or_else(|| root_path.join(rel));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content == "first" || content == "second");
    }

    #[test]
    fn nested_relative_path_under_existing_subdir() {
        let root = temp_root();
        std::fs::create_dir_all(root.path().join("docs")).unwrap();
        let result =
            save_translation_as_to_root(root.path(), "docs/guide.ja.md", "こんにちは").unwrap();
        assert!(PathBuf::from(&result.absolute_path).exists());
        assert_eq!(
            std::fs::read_to_string(root.path().join("docs/guide.ja.md")).unwrap(),
            "こんにちは"
        );
    }

    #[test]
    fn missing_parent_directory_is_rejected() {
        let root = temp_root();
        let err =
            save_translation_as_to_root(root.path(), "nope/nested.md", "x").unwrap_err();
        assert_eq!(err.code, AppErrorCode::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_parent() {
        let root = temp_root();
        let outside = tempfile::tempdir().unwrap();
        let link = root.path().join("linked");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let err =
            save_translation_as_to_root(root.path(), "linked/out.md", "x").unwrap_err();
        assert_eq!(err.i18n_key.as_deref(), Some(I18N_SAVE_PATH_REJECTED));
        assert!(!outside.path().join("out.md").exists());
    }
}
