//! TOCTOU-safe upload primitives.
//!
//! ## Threat model
//!
//! The upload handler operates inside `<uploads_root>/`. The path-based
//! checks in `files.rs::stream_and_finalize` (`symlink_metadata` +
//! `ensure_path_inside`) reject pre-placed symlinks at check time, but
//! a sufficiently fast local attacker with write access to `uploads_root`
//! could swap a directory for a symlink between the check and the
//! subsequent file create or finalize. This module closes those windows on
//! Unix by performing the security-critical I/O via `openat(2)` and
//! `linkat(2)` with `O_NOFOLLOW` parent directory handles, so even if the
//! path resolves to a symlink at the moment of use the syscall fails rather
//! than escaping the jail.
//!
//! On Windows the module falls back to path-based ops (`tokio::fs`).
//! Reparse points (the Windows analogue of symlinks) require admin or
//! developer-mode privileges to create on most installations, so the
//! residual TOCTOU window is a much narrower concern. Hardening Windows
//! to the same level would require `CreateFile` with
//! `FILE_FLAG_OPEN_REPARSE_POINT` plus reparse-tag inspection, which is
//! out of scope for this round.
//!
//! ## What this module does NOT cover
//!
//! - `uploads_root` itself: if the root is a symlink to outside the
//!   intended jail, every operation here happens "inside" the symlink
//!   target. Validating `uploads_root` is the caller's responsibility
//!   (typically once at server startup).
//! - The path-based `mkdir`/`create_dir_all` of bucket and `.tmp`
//!   directories. Those are covered by `symlink_metadata` + canonicalize
//!   checks at the call site; the openat ops here only protect the
//!   subsequent file create and rename.

use std::io;
use std::path::Path;

#[cfg(unix)]
mod unix {
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::Path;

    fn cstr_from_path(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    fn cstr_from_name(name: &str) -> io::Result<CString> {
        if name.is_empty() || name.contains('/') || name.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "name must be non-empty and contain no '/' or NUL",
            ));
        }
        CString::new(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    /// Open `path` as a directory with `O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`.
    /// Fails with `ELOOP` if the final component is a symlink, `ENOTDIR`
    /// if it's not a directory, `ENOENT` if it doesn't exist.
    pub fn open_dir_nofollow(path: &Path) -> io::Result<OwnedFd> {
        let cpath = cstr_from_path(path)?;
        let fd = unsafe {
            libc::open(
                cpath.as_ptr(),
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
    }

    /// Create a regular file at `name` under `dir_fd` with
    /// `O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC`, mode 0600. Returns
    /// the file as a synchronous `std::fs::File` — wrap with
    /// `tokio::fs::File::from_std` for async use. `name` must contain no
    /// path separator.
    ///
    /// Fails with `EEXIST` if the name is already taken (including by a
    /// symlink), `ELOOP` if the resolved path is somehow a symlink,
    /// `ENOENT` if `dir_fd` no longer references a live directory.
    pub fn create_file_nofollow_at(dir_fd: &OwnedFd, name: &str) -> io::Result<std::fs::File> {
        let cname = cstr_from_name(name)?;
        let fd = unsafe {
            libc::openat(
                dir_fd.as_raw_fd(),
                cname.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { std::fs::File::from_raw_fd(fd) })
        }
    }

    /// Open an existing regular file for read without following a trailing
    /// symlink.
    pub fn open_file_read_nofollow_at(dir_fd: &OwnedFd, name: &str) -> io::Result<std::fs::File> {
        let cname = cstr_from_name(name)?;
        let fd = unsafe {
            libc::openat(
                dir_fd.as_raw_fd(),
                cname.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { std::fs::File::from_raw_fd(fd) })
        }
    }

    /// Atomically link `old_name` under `old_dir` to `new_name` under
    /// `new_dir` via `linkat(2)`, then best-effort unlink the old staging
    /// name. `linkat` fails with `EEXIST` instead of replacing an existing
    /// destination, which lets callers resolve filename collisions without a
    /// check-then-rename race. Neither directory is re-resolved through path
    /// lookup, so a concurrent symlink swap of either dir cannot redirect the
    /// destination. Both names must contain no path separator.
    pub fn link_at_no_replace(
        old_dir: &OwnedFd,
        old_name: &str,
        new_dir: &OwnedFd,
        new_name: &str,
    ) -> io::Result<()> {
        let old_c = cstr_from_name(old_name)?;
        let new_c = cstr_from_name(new_name)?;
        let ret = unsafe {
            libc::linkat(
                old_dir.as_raw_fd(),
                old_c.as_ptr(),
                new_dir.as_raw_fd(),
                new_c.as_ptr(),
                0,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            libc::unlinkat(old_dir.as_raw_fd(), old_c.as_ptr(), 0);
        }
        Ok(())
    }

    /// Compatibility fallback for filesystems that do not support hard links.
    /// Creates the destination with `O_EXCL|O_NOFOLLOW`, copies from the
    /// staging file opened with `O_NOFOLLOW`, then removes staging. This is
    /// not as atomic as `linkat`, but it still never replaces an existing
    /// destination and it preserves the jail guarantees of the openat path.
    pub fn copy_at_no_replace(
        old_dir: &OwnedFd,
        old_name: &str,
        new_dir: &OwnedFd,
        new_name: &str,
    ) -> io::Result<()> {
        let mut src = open_file_read_nofollow_at(old_dir, old_name)?;
        let mut dst = create_file_nofollow_at(new_dir, new_name)?;
        if let Err(e) = io::copy(&mut src, &mut dst) {
            unlink_at_best_effort(new_dir, new_name);
            return Err(e);
        }
        if let Err(e) = dst.sync_data() {
            unlink_at_best_effort(new_dir, new_name);
            return Err(e);
        }
        let old_c = cstr_from_name(old_name)?;
        let ret = unsafe { libc::unlinkat(old_dir.as_raw_fd(), old_c.as_ptr(), 0) };
        if ret < 0 {
            let err = io::Error::last_os_error();
            unlink_at_best_effort(new_dir, new_name);
            return Err(err);
        }
        Ok(())
    }

    /// Best-effort `unlinkat` for cleanup paths. Errors are ignored
    /// because the cleanup itself is best-effort: a stale temp file is
    /// not a security or correctness issue.
    pub fn unlink_at_best_effort(dir_fd: &OwnedFd, name: &str) {
        let Ok(cname) = cstr_from_name(name) else {
            return;
        };
        unsafe {
            libc::unlinkat(dir_fd.as_raw_fd(), cname.as_ptr(), 0);
        }
    }
}

/// Map a blocking-task `JoinError` into an `io::Error`. Only fires if the
/// closure panicked or the runtime is shutting down; both indicate the
/// process is in a bad state and the upload should fail loudly.
#[cfg(unix)]
fn map_join_err(e: tokio::task::JoinError) -> io::Error {
    io::Error::other(e)
}

/// Create the staging file at `<tmp_dir>/<staging_name>` in a way that
/// refuses to follow a symlink at any of the trailing path components
/// (Unix: `O_NOFOLLOW` via `openat`; Windows: `OpenOptions::create_new`,
/// which fails on existing reparse points but does not validate the
/// parent — see module docs).
///
/// The Unix syscalls (`open`/`openat`) are blocking, so they're hopped to
/// the blocking pool via `tokio::task::spawn_blocking`. This matches what
/// `tokio::fs::File::create` does internally and prevents a slow filesystem
/// (NFS, fuse, container overlayfs) from stalling the async executor on
/// the open path.
pub async fn create_staging_file(
    tmp_dir: &Path,
    staging_name: &str,
) -> io::Result<tokio::fs::File> {
    #[cfg(unix)]
    {
        let tmp_dir = tmp_dir.to_path_buf();
        let staging_name = staging_name.to_string();
        let std_file = tokio::task::spawn_blocking(move || -> io::Result<std::fs::File> {
            let tmp_fd = unix::open_dir_nofollow(&tmp_dir)?;
            unix::create_file_nofollow_at(&tmp_fd, &staging_name)
        })
        .await
        .map_err(map_join_err)??;
        Ok(tokio::fs::File::from_std(std_file))
    }
    #[cfg(not(unix))]
    {
        let path = tmp_dir.join(staging_name);
        tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
    }
}

/// Move a successfully-staged upload into its final bucket location without
/// replacing an existing file. On Unix this first uses `linkat` between
/// freshly-opened `O_NOFOLLOW` dirfds for both source and destination, then
/// falls back to an `O_EXCL` copy for filesystems that do not support hard
/// links. A concurrent symlink swap of either directory between the caller's
/// pre-checks and this call cannot land the file outside the jail (the swap
/// will instead surface as an `EEXIST`/`ENOENT`/`ELOOP` error). Windows uses
/// a path-based hard-link-then-copy strategy per module docs.
///
/// Routed through `spawn_blocking` for the same reason as
/// `create_staging_file`.
pub async fn finalize_into_bucket(
    tmp_dir: &Path,
    staging_name: &str,
    bucket_dir: &Path,
    final_name: &str,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let tmp_dir = tmp_dir.to_path_buf();
        let bucket_dir = bucket_dir.to_path_buf();
        let staging_name = staging_name.to_string();
        let final_name = final_name.to_string();
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            let tmp_fd = unix::open_dir_nofollow(&tmp_dir)?;
            let bucket_fd = unix::open_dir_nofollow(&bucket_dir)?;
            match unix::link_at_no_replace(&tmp_fd, &staging_name, &bucket_fd, &final_name) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
                Err(_) => unix::copy_at_no_replace(&tmp_fd, &staging_name, &bucket_fd, &final_name),
            }
        })
        .await
        .map_err(map_join_err)?
    }
    #[cfg(not(unix))]
    {
        let src = tmp_dir.join(staging_name);
        let dst = bucket_dir.join(final_name);
        match tokio::fs::hard_link(&src, &dst).await {
            Ok(()) => {
                let _ = tokio::fs::remove_file(&src).await;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
            Err(_) => {
                let mut src_file = tokio::fs::File::open(&src).await?;
                let mut dst_file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&dst)
                    .await?;
                if let Err(e) = tokio::io::copy(&mut src_file, &mut dst_file).await {
                    let _ = tokio::fs::remove_file(&dst).await;
                    return Err(e);
                }
                if let Err(e) = dst_file.sync_data().await {
                    let _ = tokio::fs::remove_file(&dst).await;
                    return Err(e);
                }
                if let Err(e) = tokio::fs::remove_file(&src).await {
                    let _ = tokio::fs::remove_file(&dst).await;
                    return Err(e);
                }
                Ok(())
            }
        }
    }
}

/// Best-effort cleanup of a staging file when the upload errored out.
/// Failures are intentional to ignore — the file may have already been
/// removed (or never created), and a stale temp is not a correctness
/// issue. On Unix this routes through `unlinkat` so the cleanup itself
/// can't be redirected by a swap of the parent directory.
///
/// Routed through `spawn_blocking` for the same reason as
/// `create_staging_file`.
pub async fn remove_staging_best_effort(tmp_dir: &Path, staging_name: &str) {
    #[cfg(unix)]
    {
        let tmp_dir = tmp_dir.to_path_buf();
        let staging_name = staging_name.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(tmp_fd) = unix::open_dir_nofollow(&tmp_dir) {
                unix::unlink_at_best_effort(&tmp_fd, &staging_name);
            }
        })
        .await;
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::fs::remove_file(tmp_dir.join(staging_name)).await;
    }
}

#[cfg(any(not(codeg_test_shard), codeg_test_shard = "6"))]
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn create_staging_file_succeeds_in_normal_dir() {
        let tmp = TempDir::new().unwrap();
        let mut f = create_staging_file(tmp.path(), "foo.part").await.unwrap();
        f.write_all(b"hello").await.unwrap();
        f.flush().await.unwrap();
        let bytes = tokio::fs::read(tmp.path().join("foo.part")).await.unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[tokio::test]
    async fn create_staging_file_rejects_symlinked_tmp() {
        let parent = TempDir::new().unwrap();
        let real = parent.path().join("real");
        let link = parent.path().join("link");
        std::fs::create_dir(&real).unwrap();
        symlink(&real, &link).unwrap();

        let err = create_staging_file(&link, "foo.part").await.unwrap_err();
        assert!(
            matches!(err.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENOTDIR)),
            "expected ELOOP/ENOTDIR, got {err:?} (errno={:?})",
            err.raw_os_error()
        );
        // And no file should have been created at the symlink target.
        assert!(!real.join("foo.part").exists());
    }

    #[tokio::test]
    async fn create_staging_file_rejects_symlink_at_target_name() {
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("target");
        std::fs::write(&target, b"").unwrap();
        // Pre-place a symlink at the staging name pointing outside.
        symlink(&target, tmp.path().join("foo.part")).unwrap();

        let err = create_staging_file(tmp.path(), "foo.part")
            .await
            .unwrap_err();
        // O_EXCL fires before O_NOFOLLOW gets a chance — both EEXIST and
        // ELOOP indicate "we did not write through the symlink", which is
        // what we want.
        assert!(
            matches!(err.raw_os_error(), Some(libc::EEXIST) | Some(libc::ELOOP)),
            "expected EEXIST/ELOOP, got {err:?} (errno={:?})",
            err.raw_os_error()
        );
        // Target outside must remain empty.
        let bytes = tokio::fs::read(&target).await.unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn create_staging_file_rejects_path_separator_in_name() {
        let tmp = TempDir::new().unwrap();
        let err = create_staging_file(tmp.path(), "../escape.part")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn finalize_into_bucket_moves_file_normally() {
        let root = TempDir::new().unwrap();
        let tmp = root.path().join("tmp");
        let bucket = root.path().join("bucket");
        std::fs::create_dir(&tmp).unwrap();
        std::fs::create_dir(&bucket).unwrap();
        std::fs::write(tmp.join("foo.part"), b"data").unwrap();

        finalize_into_bucket(&tmp, "foo.part", &bucket, "final.bin")
            .await
            .unwrap();

        assert!(!tmp.join("foo.part").exists());
        let bytes = tokio::fs::read(bucket.join("final.bin")).await.unwrap();
        assert_eq!(bytes, b"data");
    }

    #[tokio::test]
    async fn finalize_into_bucket_does_not_replace_existing_file() {
        let root = TempDir::new().unwrap();
        let tmp = root.path().join("tmp");
        let bucket = root.path().join("bucket");
        std::fs::create_dir(&tmp).unwrap();
        std::fs::create_dir(&bucket).unwrap();
        std::fs::write(tmp.join("foo.part"), b"new").unwrap();
        std::fs::write(bucket.join("final.bin"), b"old").unwrap();

        let err = finalize_into_bucket(&tmp, "foo.part", &bucket, "final.bin")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            tokio::fs::read(bucket.join("final.bin")).await.unwrap(),
            b"old"
        );
        assert_eq!(tokio::fs::read(tmp.join("foo.part")).await.unwrap(), b"new");
    }

    #[tokio::test]
    async fn copy_fallback_moves_file_without_hard_link() {
        let root = TempDir::new().unwrap();
        let tmp = root.path().join("tmp");
        let bucket = root.path().join("bucket");
        std::fs::create_dir(&tmp).unwrap();
        std::fs::create_dir(&bucket).unwrap();
        std::fs::write(tmp.join("foo.part"), b"data").unwrap();

        let tmp_fd = unix::open_dir_nofollow(&tmp).unwrap();
        let bucket_fd = unix::open_dir_nofollow(&bucket).unwrap();
        unix::copy_at_no_replace(&tmp_fd, "foo.part", &bucket_fd, "final.bin").unwrap();

        assert!(!tmp.join("foo.part").exists());
        let bytes = tokio::fs::read(bucket.join("final.bin")).await.unwrap();
        assert_eq!(bytes, b"data");
    }

    #[tokio::test]
    async fn copy_fallback_does_not_replace_existing_file() {
        let root = TempDir::new().unwrap();
        let tmp = root.path().join("tmp");
        let bucket = root.path().join("bucket");
        std::fs::create_dir(&tmp).unwrap();
        std::fs::create_dir(&bucket).unwrap();
        std::fs::write(tmp.join("foo.part"), b"new").unwrap();
        std::fs::write(bucket.join("final.bin"), b"old").unwrap();

        let tmp_fd = unix::open_dir_nofollow(&tmp).unwrap();
        let bucket_fd = unix::open_dir_nofollow(&bucket).unwrap();
        let err =
            unix::copy_at_no_replace(&tmp_fd, "foo.part", &bucket_fd, "final.bin").unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            tokio::fs::read(bucket.join("final.bin")).await.unwrap(),
            b"old"
        );
        assert_eq!(tokio::fs::read(tmp.join("foo.part")).await.unwrap(), b"new");
    }

    #[tokio::test]
    async fn finalize_into_bucket_rejects_symlinked_bucket() {
        let root = TempDir::new().unwrap();
        let tmp = root.path().join("tmp");
        let real = root.path().join("real_bucket");
        let link = root.path().join("link_bucket");
        std::fs::create_dir(&tmp).unwrap();
        std::fs::create_dir(&real).unwrap();
        symlink(&real, &link).unwrap();
        std::fs::write(tmp.join("foo.part"), b"data").unwrap();

        let err = finalize_into_bucket(&tmp, "foo.part", &link, "final.bin")
            .await
            .unwrap_err();
        assert!(
            matches!(err.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENOTDIR)),
            "expected ELOOP/ENOTDIR, got {err:?} (errno={:?})",
            err.raw_os_error()
        );
        // File must still be in tmp; symlink target unchanged.
        assert!(tmp.join("foo.part").exists());
        assert!(!real.join("final.bin").exists());
    }

    #[tokio::test]
    async fn finalize_into_bucket_rejects_symlinked_tmp() {
        let root = TempDir::new().unwrap();
        let real_tmp = root.path().join("real_tmp");
        let link_tmp = root.path().join("link_tmp");
        let bucket = root.path().join("bucket");
        std::fs::create_dir(&real_tmp).unwrap();
        symlink(&real_tmp, &link_tmp).unwrap();
        std::fs::create_dir(&bucket).unwrap();
        std::fs::write(real_tmp.join("foo.part"), b"data").unwrap();

        let err = finalize_into_bucket(&link_tmp, "foo.part", &bucket, "final.bin")
            .await
            .unwrap_err();
        assert!(
            matches!(err.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENOTDIR)),
            "expected ELOOP/ENOTDIR, got {err:?} (errno={:?})",
            err.raw_os_error()
        );
        assert!(real_tmp.join("foo.part").exists());
    }
}
