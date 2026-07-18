# Grok Question Tool Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure every new Grok ACP process disables Grok's incompatible built-in `ask_user_question` so structured questions use Codeg's existing blocking MCP question flow.

**Architecture:** Extract the Npx registry-argument append step into a small pure helper in `connection.rs`. For Grok, prepend root-level `--no-auto-update --disallowed-tools ask_user_question` and the conditional `--always-approve` flag before appending the registry's `agent stdio` subcommand; for every other agent, append registry arguments unchanged. Keep the existing Codeg MCP question backend and frontend untouched.

**Tech Stack:** Rust 2021, sacp/sacp-tokio ACP launcher, Cargo unit tests and Clippy.

## Global Constraints

- Grok is pinned to `0.2.98`, whose root CLI accepts `--disallowed-tools <TOOLS>`.
- `--disallowed-tools` and `ask_user_question` must be adjacent arguments before `agent stdio`.
- `--always-approve` remains conditional on the persisted Grok permission mode.
- Apply the built-in-tool disable even when Codeg's MCP question feature is disabled.
- Do not change frontend question rendering, answer formatting, transcript parsing, or historical conversations.
- Do not change any non-Grok launch arguments.
- Preserve unrelated worktree changes and stage only the files named by this task.

---

### Task 1: Route Grok Questions Through Codeg MCP

**Files:**
- Modify: `src-tauri/src/acp/connection.rs:356-485,5980+`
- Modify: `src-tauri/src/acp/registry.rs:382-410`
- Test: inline `#[cfg(test)]` module in `src-tauri/src/acp/connection.rs`

**Interfaces:**
- Consumes: `AgentType`, registry `args: &[&str]`, the already-resolved `grok_always_approve: bool`, and the mutable command `Vec<String>`.
- Produces: `append_npx_launch_args(parts: &mut Vec<String>, agent_type: AgentType, args: &[&str], grok_always_approve: bool)`.
- Preserves: the existing `build_agent(AgentType, &BTreeMap<String, String>, &Path) -> Result<AcpAgent, AcpError>` interface.

- [ ] **Step 1: Add failing pure launch-argument tests**

Add these tests near the start of the existing `connection.rs` test module:

```rust
#[test]
fn grok_npx_launch_args_disable_native_question_before_subcommand() {
    let mut without_auto_approve = vec!["grok".to_string()];
    append_npx_launch_args(
        &mut without_auto_approve,
        AgentType::Grok,
        &["agent", "stdio"],
        false,
    );
    assert_eq!(
        without_auto_approve,
        vec![
            "grok",
            "--no-auto-update",
            "--disallowed-tools",
            "ask_user_question",
            "agent",
            "stdio",
        ]
    );

    let mut with_auto_approve = vec!["grok".to_string()];
    append_npx_launch_args(
        &mut with_auto_approve,
        AgentType::Grok,
        &["agent", "stdio"],
        true,
    );
    assert_eq!(
        with_auto_approve,
        vec![
            "grok",
            "--no-auto-update",
            "--disallowed-tools",
            "ask_user_question",
            "--always-approve",
            "agent",
            "stdio",
        ]
    );
}

#[test]
fn non_grok_npx_launch_args_remain_unchanged() {
    let mut parts = vec!["codex-acp".to_string()];
    append_npx_launch_args(
        &mut parts,
        AgentType::Codex,
        &["serve"],
        true,
    );
    assert_eq!(parts, vec!["codex-acp", "serve"]);
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils grok_npx_launch_args_disable_native_question_before_subcommand
```

Expected: compilation fails with `cannot find function append_npx_launch_args`.
The failure must be caused by the missing production helper, not a typo in the
test.

- [ ] **Step 3: Implement the minimal argument helper and use it**

Add this pure helper immediately before `build_agent`:

```rust
fn append_npx_launch_args(
    parts: &mut Vec<String>,
    agent_type: AgentType,
    args: &[&str],
    grok_always_approve: bool,
) {
    if agent_type == AgentType::Grok {
        // Grok's native ask_user_question waits outside Codeg's QuestionRequest
        // flow. Remove it so structured questions use codeg-mcp instead.
        for arg in [
            "--no-auto-update",
            "--disallowed-tools",
            "ask_user_question",
        ] {
            parts.push(arg.into());
        }
        if grok_always_approve {
            parts.push("--always-approve".into());
        }
    }
    for arg in args {
        parts.push((*arg).into());
    }
}
```

Replace the current Grok flag block plus the following `for a in args` loop in
the Npx branch of `build_agent` with:

```rust
let grok_always_approve = agent_type == AgentType::Grok
    && crate::commands::acp::grok_launch_always_approve();
append_npx_launch_args(
    &mut parts,
    agent_type,
    args,
    grok_always_approve,
);
```

Keep the surrounding explanation that Grok root flags precede `agent stdio`,
but update it to document `--disallowed-tools ask_user_question` and the native
tool/MCP name collision.

In `registry.rs`, update the Grok distribution comment so the enumerated root
flags are exactly `--no-auto-update`, `--disallowed-tools ask_user_question`,
and conditional `--always-approve`. Do not change the registry's actual
`args: &["agent", "stdio"]` value.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils npx_launch_args
```

Expected: both new tests pass. The Grok vectors contain the adjacent disable
pair before `agent stdio`; the Codex vector is unchanged.

- [ ] **Step 5: Format and run desktop verification**

Run from `src-tauri/`:

```powershell
cargo fmt
cargo test --features test-utils npx_launch_args
cargo check
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: formatting completes; both focused tests, desktop check, and Clippy
pass with no warnings.

- [ ] **Step 6: Verify the server build path**

Run from `src-tauri/`:

```powershell
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib npx_launch_args
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
```

Expected: the server check, both focused library tests, and server Clippy pass
with no warnings.

- [ ] **Step 7: Inspect and commit only the scoped fix**

Run from the repository root:

```powershell
git diff --check -- src-tauri/src/acp/connection.rs src-tauri/src/acp/registry.rs
git diff -- src-tauri/src/acp/connection.rs src-tauri/src/acp/registry.rs
git add src-tauri/src/acp/connection.rs src-tauri/src/acp/registry.rs
git commit -m "fix(grok): route questions through codeg-mcp"
```

Expected: the diff contains only the pure helper, its use, two focused tests,
and Grok launch comments. The commit does not include unrelated working-tree
changes.
