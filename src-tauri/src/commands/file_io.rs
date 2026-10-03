use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::app_error::AppCommandError;

/// Write a base64-encoded binary blob to a user-chosen path on disk.
///
/// Used by the frontend's "download generated image" flow on desktop:
/// the renderer first invokes `tauri-plugin-dialog`'s `save()` to obtain
/// a destination path from the system save dialog, then calls this command
/// with the base64 payload. Web mode bypasses this command entirely and
/// uses an `<a download>` Blob link.
///
/// `path` must be an absolute filesystem path (the dialog returns one).
/// Parent directory must already exist (it does, since the OS dialog only
/// lets the user pick an existing folder + filename).
#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn save_binary_file(path: String, data_base64: String) -> Result<(), AppCommandError> {
    let bytes = STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|e| AppCommandError::invalid_input(format!("invalid base64 payload: {e}")))?;
    std::fs::write(&path, bytes).map_err(AppCommandError::io)?;
    Ok(())
}

/// Write a UTF-8 text payload to a user-chosen path on disk.
///
/// Used by the frontend's "export conversation as Markdown / HTML" flow on
/// desktop. The renderer first invokes `tauri-plugin-dialog`'s `save()`
/// to obtain a destination path from the system save dialog, then calls
/// this command with the text contents. Web mode bypasses this command
/// entirely and uses an `<a download>` Blob link.
///
/// Mirrors `save_binary_file`'s contract: `path` must be an absolute
/// filesystem path returned by the OS dialog, and the parent directory
/// is guaranteed to exist by the dialog. I/O failures (including macOS
/// TCC denials at write time) surface through `AppCommandError::io`,
/// which maps `PermissionDenied` so the caller can disambiguate.
#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn save_text_file(path: String, contents: String) -> Result<(), AppCommandError> {
    std::fs::write(&path, contents).map_err(AppCommandError::io)?;
    Ok(())
}

#[cfg(any(not(codeg_test_shard), codeg_test_shard = "9"))]
#[cfg(all(test, feature = "tauri-runtime"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn save_text_file_writes_utf8_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("out.md");
        let contents = "# Title\n\n中文内容 emoji 🎉\n".to_string();
        save_text_file(path.to_string_lossy().into_owned(), contents.clone())
            .await
            .expect("write");
        let read = std::fs::read_to_string(&path).expect("read");
        assert_eq!(read, contents);
    }

    #[tokio::test]
    async fn save_text_file_overwrites_existing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("out.md");
        std::fs::write(&path, "old").expect("seed");
        save_text_file(path.to_string_lossy().into_owned(), "new".into())
            .await
            .expect("overwrite");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "new");
    }

    #[tokio::test]
    async fn save_text_file_surfaces_io_error_on_missing_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bad = dir.path().join("does/not/exist/out.md");
        let err = save_text_file(bad.to_string_lossy().into_owned(), "x".into())
            .await
            .expect_err("must fail");
        assert!(matches!(err.code, crate::app_error::AppErrorCode::NotFound));
    }
}
