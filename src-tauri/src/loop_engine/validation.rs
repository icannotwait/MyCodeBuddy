//! Deterministic validation (§4.6): run the issue's `validation_commands` in the
//! worktree after an implement checkpoint, with no agent involved.
//!
//! The runner draws a deliberate line between two failure shapes the gate treats
//! very differently:
//!
//! - **A command runs and exits non-zero** — a *code* problem (tests/lint fail).
//!   The agent can fix it, so the gate reworks (bumps the task attempt and
//!   re-dispatches implement with the failure output fed back in).
//! - **A command can't run at all** — missing tool, un-spawnable shell, or a
//!   timeout. That's an *environment/config* problem a human must resolve, so the
//!   gate blocks and files an inbox card rather than burning rework attempts.
//!
//! Missing-tool detection pre-flights the leading program with `which`; a typed
//! validation command (`cargo test`, `pnpm lint`) names a real program, so this
//! is reliable in practice. Execution itself goes through the platform shell so
//! arguments, quoting, and pipelines behave as written.

use std::path::Path;
use std::time::Duration;

use crate::loop_engine::error::LoopError;

/// How a validation pass concluded. The gate maps each variant to a distinct
/// next action (advance / rework / block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationOutcome {
    /// Every command ran and exited zero.
    Passed,
    /// A command ran and exited non-zero — a code problem the agent can fix.
    Failed,
    /// A command could not be run (missing tool, un-spawnable, or timed out) — an
    /// environment problem a human must resolve.
    Unrunnable,
}

/// The result of one validation pass: the per-command exit codes collected so
/// far, the combined transcript, and the classified outcome.
#[derive(Debug, Clone)]
pub struct ValidationReport {
    pub exit_codes: Vec<i32>,
    pub output: String,
    pub outcome: ValidationOutcome,
}

impl ValidationReport {
    pub fn passed(&self) -> bool {
        self.outcome == ValidationOutcome::Passed
    }
}

/// Build a shell command for `cmd` rooted at `dir`. `kill_on_drop` lets the
/// timeout path terminate the child simply by dropping the awaited future.
fn shell_command(cmd: &str, dir: &Path) -> tokio::process::Command {
    #[cfg(windows)]
    let mut command = {
        let mut c = crate::process::tokio_command("cmd");
        c.args(["/D", "/S", "/C", cmd]);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = crate::process::tokio_command("sh");
        c.args(["-c", cmd]);
        c
    };
    command.current_dir(dir).kill_on_drop(true);
    command
}

/// Run `commands` sequentially in `worktree_path`, stopping at the first failure.
///
/// Returns `Passed` when every command exits zero, `Failed` at the first
/// non-zero exit, and `Unrunnable` when a command's program is missing, the
/// shell can't be spawned, or `timeout` elapses (each command gets the full
/// `timeout`; `None` means unlimited). Never returns `Err` for ordinary process
/// trouble — infrastructure failures classify as `Unrunnable` so the caller can
/// surface them as a blocked inbox card rather than a hard engine error.
pub async fn run_validation(
    worktree_path: &Path,
    commands: &[String],
    timeout: Option<Duration>,
) -> Result<ValidationReport, LoopError> {
    let mut exit_codes = Vec::new();
    let mut output = String::new();

    for raw in commands {
        let cmd = raw.trim();
        if cmd.is_empty() {
            continue;
        }
        output.push_str(&format!("$ {cmd}\n"));

        // Pre-flight: a missing program is a config problem (block), not a code
        // failure (rework). A typed validation command leads with a real program.
        if let Some(program) = cmd.split_whitespace().next() {
            if which::which(program).is_err() {
                output.push_str(&format!("[validation] command not found: {program}\n"));
                return Ok(ValidationReport {
                    exit_codes,
                    output,
                    outcome: ValidationOutcome::Unrunnable,
                });
            }
        }

        let mut command = shell_command(cmd, worktree_path);
        let spawned = match timeout {
            Some(d) => match tokio::time::timeout(d, command.output()).await {
                Ok(result) => result,
                Err(_) => {
                    // The awaited future is dropped here; `kill_on_drop` reaps the
                    // child. A timeout is an environment problem → block.
                    output.push_str(&format!("[validation] timed out after {}s\n", d.as_secs()));
                    return Ok(ValidationReport {
                        exit_codes,
                        output,
                        outcome: ValidationOutcome::Unrunnable,
                    });
                }
            },
            None => command.output().await,
        };
        let out = match spawned {
            Ok(out) => out,
            Err(e) => {
                output.push_str(&format!("[validation] could not run: {e}\n"));
                return Ok(ValidationReport {
                    exit_codes,
                    output,
                    outcome: ValidationOutcome::Unrunnable,
                });
            }
        };

        output.push_str(&String::from_utf8_lossy(&out.stdout));
        output.push_str(&String::from_utf8_lossy(&out.stderr));
        let code = out.status.code().unwrap_or(-1);
        exit_codes.push(code);
        if code != 0 {
            return Ok(ValidationReport {
                exit_codes,
                output,
                outcome: ValidationOutcome::Failed,
            });
        }
    }

    Ok(ValidationReport {
        exit_codes,
        output,
        outcome: ValidationOutcome::Passed,
    })
}

// Exercises real coreutils, so it is gated to unix (the CI/dev platform). The
// engine code above compiles and runs on every platform via `shell_command`.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn cmds(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn all_zero_exit_passes() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_validation(dir.path(), &cmds(&["true", "true"]), None)
            .await
            .unwrap();
        assert_eq!(r.outcome, ValidationOutcome::Passed);
        assert_eq!(r.exit_codes, vec![0, 0]);
        assert!(r.passed());
    }

    #[tokio::test]
    async fn nonzero_exit_fails_and_stops_at_first() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_validation(dir.path(), &cmds(&["false", "true"]), None)
            .await
            .unwrap();
        assert_eq!(r.outcome, ValidationOutcome::Failed);
        assert_eq!(r.exit_codes, vec![1], "fail-fast: the second command never ran");
        assert!(!r.passed());
    }

    #[tokio::test]
    async fn missing_command_is_unrunnable() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_validation(
            dir.path(),
            &cmds(&["codeg-no-such-tool-xyzzy --version"]),
            None,
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, ValidationOutcome::Unrunnable);
        assert!(r.exit_codes.is_empty(), "nothing was executed");
        assert!(r.output.contains("command not found"));
    }

    #[tokio::test]
    async fn timeout_is_unrunnable() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_validation(
            dir.path(),
            &cmds(&["sleep 5"]),
            Some(Duration::from_millis(200)),
        )
        .await
        .unwrap();
        assert_eq!(r.outcome, ValidationOutcome::Unrunnable);
        assert!(r.output.contains("timed out"));
    }

    #[tokio::test]
    async fn commands_run_in_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "x").unwrap();
        // `test -f` exits 0 only when the cwd is the worktree holding the marker.
        let r = run_validation(dir.path(), &cmds(&["test -f marker.txt"]), None)
            .await
            .unwrap();
        assert_eq!(r.outcome, ValidationOutcome::Passed);
    }
}
