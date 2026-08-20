//! Self-cleanup watchdog for `codeg-mcp`.
//!
//! On Windows, child processes don't die with their parent automatically.
//! On Unix the kernel closes the inherited pipe ends so `stdin` reads EOF
//! and the loop exits — usually. In both worlds a misbehaving intermediate
//! (agent CLI that hangs, parent codeg crash that orphans the agent) can
//! leave `codeg-mcp` running forever, holding open the binary file and a
//! companion connection that no one will ever read from.
//!
//! When the parent codeg / codeg-server passes `--parent-pid <pid>` on
//! the command line, `codeg-mcp` spawns this watchdog. Unix still polls
//! existence. Windows opens the parent **once** with `SYNCHRONIZE` and
//! waits on that handle so a recycled PID cannot look alive.
//!
//! Backward compatibility: the `--parent-pid` flag is optional. Older
//! parents that don't pass it get today's behavior (no watchdog).

use std::time::Duration;

/// Default polling cadence for Unix existence probes.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Return `true` if a process with `pid` is currently alive (running, not
/// a zombie awaiting reap on Unix; not exited on Windows).
///
/// Best-effort: any unexpected OS error is treated as "alive" so a
/// permission glitch can't cause the watchdog to kill `codeg-mcp` while
/// the parent is in fact still running.
pub fn parent_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unix_parent_alive(pid)
    }
    #[cfg(windows)]
    {
        windows_parent_alive(pid)
    }
}

#[cfg(unix)]
fn unix_parent_alive(pid: u32) -> bool {
    // `kill(pid, 0)` is the POSIX existence probe — no signal is sent, the
    // kernel just validates the target. Result == 0 means alive; ESRCH
    // means gone; EPERM means alive but inaccessible (treat as alive).
    if pid == 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if r == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(e) if e == libc::EPERM
    )
}

#[cfg(windows)]
fn windows_parent_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return false;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut code: u32 = 0;
    let read_ok = unsafe { GetExitCodeProcess(handle, &mut code as *mut u32) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    read_ok != 0 && code == STILL_ACTIVE as u32
}

/// Long-running task: wait until `pid` no longer exists, then return.
/// The caller is expected to terminate the process at that point — this
/// function itself does not call `process::exit` so it stays testable.
pub async fn wait_for_parent_exit(pid: u32, interval: Duration) {
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    {
        let _ = interval;
        windows_wait_for_parent_exit(pid).await;
    }
    #[cfg(unix)]
    {
        loop {
            if !parent_alive(pid) {
                return;
            }
            tokio::time::sleep(interval).await;
        }
    }
}

/// `SYNCHRONIZE` (0x00100000). Held for the lifetime of the wait so a
/// recycled PID cannot satisfy a later `OpenProcess`.
#[cfg(windows)]
const SYNCHRONIZE: u32 = 0x0010_0000;

#[cfg(windows)]
async fn windows_wait_for_parent_exit(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, INFINITE, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return;
    }
    // HANDLE is a raw pointer and is not `Send`; the integer identity is.
    let handle_bits = handle as usize;
    let wait_result = tokio::task::spawn_blocking(move || {
        let handle = handle_bits as windows_sys::Win32::Foundation::HANDLE;
        let status = unsafe { WaitForSingleObject(handle, INFINITE) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        status
    })
    .await;
    if let Err(e) = wait_result {
        let _ = e;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn own_pid_is_alive() {
        assert!(parent_alive(std::process::id()));
    }

    #[test]
    fn pid_zero_is_dead() {
        assert!(!parent_alive(0));
    }

    #[test]
    fn obviously_missing_pid_is_dead() {
        assert!(!parent_alive(0x7FFF_FFF0));
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn watcher_returns_immediately_for_dead_pid() {
        wait_for_parent_exit(0, Duration::from_secs(60)).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn watcher_returns_immediately_for_missing_pid() {
        tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_parent_exit(0x7FFF_FFF0, Duration::from_millis(10)),
        )
        .await
        .expect("missing pid must not block");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_wait_returns_after_child_exits() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .expect("spawn cmd");
        let pid = child.id();
        let wait = tokio::spawn(async move {
            wait_for_parent_exit(pid, Duration::from_secs(2)).await;
        });
        let _ = child.wait();
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .expect("WaitForSingleObject should observe child exit")
            .expect("join");
    }
}
