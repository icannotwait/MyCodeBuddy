# Autonomous Background Turns Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface Grok background-task follow-ups and capability-qualified Codex Goal continuations as independent marked assistant turns through the existing `BackgroundActivity` overlay, without grafting them onto the previous foreground reply.

**Architecture:** A provider-neutral policy module selects one adapter per connection (`ClaudeTranscript`, `GrokIdleWire`, `CodexGoalTranscript`, or `Unsupported`). Claude keeps its transcript watcher. Grok and Codex observe idle ACP signals but emit overlays only after the matching transcript/rollout complete-byte scanner consumes the records. Origin is optional `MessageTurn` metadata; the frontend marker and merge boundary key off that field. Overlay retirement remains `detail.transcript_watermark >= overlay.watermark`.

**Tech Stack:** Rust 2021 (Tauri/server shared core), Tokio, serde, Next.js/React 19 + TypeScript, Vitest, next-intl.

**Spec:** `docs/superpowers/specs/2026-08-18-autonomous-background-turns-design.md`

## Global Constraints

- First release supports only Claude Code, Grok, and capability-qualified Codex Goal continuations. Cursor, OpenCode, Gemini, Cline, Hermes, CodeBuddy, Kimi, Pi, DeepSeek, and custom ACP agents stay `Unsupported`.
- Codex selects `CodexGoalTranscript` only when the built-in agent is `AgentType::Codex`, initialize advertises `_meta.goal.version == 1` (exactly 1, not `>= 1`), `loadSession == true`, and exactly one native rollout can be resolved by `session_meta.payload.id ==` ACP session id. Fail closed otherwise. Goal cards and foreground prompting remain usable after autonomous downgrade.
- Do not remove the frontend `status == "prompting"` streaming guard. Autonomous content enters the UI only through `BackgroundActivity`.
- Do not set `turn_in_flight`, allocate a prompt generation, create an optimistic user turn, or emit `StatusChanged(prompting)` / foreground `TurnComplete` for an autonomous episode.
- Hidden Grok `<system-reminder>` and Codex `<codex_internal_context source="goal">` never render, copy, persist in Codeg, appear in events, diagnostics, or metrics.
- Origin is backend-derived `MessageTurn.autonomous_origin`. Model text cannot set or spoof it. V1 UI label for every origin is `后台续写` / `Background continuation`.
- Overlay retirement uses only the provider's complete-byte transcript watermark. ACP sequence numbers, `eventId`, `item-N` replay ids, mtime, and file length never retire an overlay.
- Canonical live and cold ids: Grok `grok-autonomous:<episode-key>:assistant:0`; Codex `codex-goal-turn-<native-turn-id>`. Normal non-autonomous Grok/Codex positional ids stay unchanged.
- At most one open Grok episode and one open Codex episode per connection. Bounds: 16 awaiting/tombstoned episodes, 64 task ids, 1,024 remembered record identities, rotate at 512 / force-rotate at 1,024, 2 MiB retained payload, 1s active retry, 30s Codex rollout discovery, tombstones 10 minutes, keepalive via `background_keepalive_max_age()` (default 3600s).
- Daily Rust verification uses `cargo test --lib --features test-utils` from `src-tauri/` with the narrowest filter that proves the task. Do not compile the full integration-test binary unless a task names a `--test` target.
- Frontend verification uses the named Vitest file(s) via `pnpm exec vitest run <file>`. Do not run the full frontend suite unless a later task says so.
- Work only in this worktree. Never stage, rewrite, or revert unrelated or pre-existing user changes. No database migration. No ACP protocol change.
- Logs/metrics may include provider, connection id (opaque), state, failure class, offsets, record counts, elapsed ms. Never include reminder text, prompt text, task command, tool I/O, assistant content, paths, or session ids in metric labels.

## File Structure

- Create `src-tauri/src/acp/autonomous_activity.rs`: `AutonomousTurnOrigin` re-export is *not* here — origin lives on `MessageTurn`. This file owns `AutonomousActivityPolicy`, `AutonomousCapabilities`, `for_connection`, adapter lifecycle hook types, and policy unit tests.
- Create `src-tauri/src/acp/grok_autonomous.rs`: Grok task ledger, episode state machine, hidden-trigger classification, `updates.jsonl` tailer, connection-loop hooks, tests.
- Create `src-tauri/src/acp/codex_autonomous.rs`: Codex capability/authority gate, Goal-cycle observer, native rollout tailer, tests.
- Create `src-tauri/src/parsers/complete_line.rs` (or keep scanners private in each parser if sharing would pull Grok/Codex together): a tiny complete-line/complete-record byte cursor used by both live tailers and cold parsers. Prefer one shared helper if both parsers can use the same newline rule without importing each other.
- Modify `src-tauri/src/models/message.rs`: add `AutonomousTurnOrigin` and `MessageTurn.autonomous_origin`.
- Modify every `MessageTurn { ... }` literal in the crate to include `autonomous_origin: None` (mechanical).
- Modify `src-tauri/src/acp/mod.rs`: declare the new modules.
- Modify `src-tauri/src/acp/background_watch.rs`: start Claude via policy; annotate proven origins; do not add Grok/Codex logic.
- Modify `src-tauri/src/acp/connection.rs`: policy at initialize; raw-dispatch hooks with foreground/idle ownership; prompt gate while `autonomous_busy`; Grok/Codex idle terminals stay out of foreground finalize.
- Modify `src-tauri/src/acp/types.rs` and `session_state.rs`: provider-neutral `BackgroundActivity` / keepalive comments; Codex single outstanding unit.
- Modify `src-tauri/src/parsers/grok.rs`: complete-byte watermark, hidden-trigger cold recovery, canonical autonomous ids.
- Modify `src-tauri/src/parsers/codex.rs`: complete-byte watermark, Goal-owned native turn ids, `source="goal"` origin, keep internal-context suppression.
- Modify `src-tauri/src/parsers/claude.rs`: cold-parse origin for proven `<task-notification>` / automation shapes.
- Modify `src/lib/types.ts`: origin union + watermark docs.
- Modify `src/contexts/acp-connections-context.tsx`: provider-neutral drop-log wording; keep the prompting guard.
- Modify `src/stores/conversation-runtime-store.ts` + `src/stores/background-overlay.test.ts`: same-id display dedupe; origin preservation.
- Modify `src/components/message/message-list-view.tsx` + tests: merge hard boundary + marker chrome.
- Modify all ten `src/i18n/messages/*.json`: `messageList.backgroundContinuation`.

---

### Task 1: Origin Metadata on MessageTurn

**Files:**
- Modify: `src-tauri/src/models/message.rs`
- Modify: every Rust `MessageTurn {` literal (parsers, acp tests, event_stream tests) — add `autonomous_origin: None`
- Modify: `src/lib/types.ts`
- Test: add `src-tauri/src/models/message.rs` `#[cfg(test)]` module (or extend an existing tests module in that file)

**Interfaces:**
- Consumes: existing `MessageTurn` serde shape.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
  #[serde(rename_all = "snake_case")]
  pub enum AutonomousTurnOrigin {
      BackgroundTask,
      Automation,
      AgentAutonomous,
  }
  ```
  and on `MessageTurn`:
  ```rust
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub autonomous_origin: Option<AutonomousTurnOrigin>,
  ```
  TypeScript:
  ```ts
  export type AutonomousTurnOrigin =
    | "background_task"
    | "automation"
    | "agent_autonomous"

  export interface MessageTurn {
    // existing fields
    autonomous_origin?: AutonomousTurnOrigin | null
  }
  ```
  Later tasks must set this field only when origin is proven. Historical JSON without the field deserializes as `None` / omitted.

- [ ] **Step 1: Write the failing serde tests**

Add at the bottom of `src-tauri/src/models/message.rs`:

```rust
#[cfg(test)]
mod autonomous_origin_tests {
    use super::{AutonomousTurnOrigin, MessageTurn, TurnRole};
    use chrono::TimeZone;

    fn bare_json() -> serde_json::Value {
        serde_json::json!({
            "id": "t1",
            "role": "assistant",
            "blocks": [],
            "timestamp": "2026-08-18T00:00:00Z"
        })
    }

    #[test]
    fn missing_origin_deserializes_as_none() {
        let turn: MessageTurn = serde_json::from_value(bare_json()).unwrap();
        assert_eq!(turn.autonomous_origin, None);
    }

    #[test]
    fn origin_round_trips_snake_case() {
        let mut turn: MessageTurn = serde_json::from_value(bare_json()).unwrap();
        turn.autonomous_origin = Some(AutonomousTurnOrigin::BackgroundTask);
        let value = serde_json::to_value(&turn).unwrap();
        assert_eq!(value["autonomous_origin"], "background_task");
        let again: MessageTurn = serde_json::from_value(value).unwrap();
        assert_eq!(
            again.autonomous_origin,
            Some(AutonomousTurnOrigin::BackgroundTask)
        );
    }

    #[test]
    fn absent_origin_is_omitted_from_json() {
        let turn: MessageTurn = serde_json::from_value(bare_json()).unwrap();
        let value = serde_json::to_value(&turn).unwrap();
        assert!(value.get("autonomous_origin").is_none());
    }

    #[test]
    fn all_origin_wires_are_stable() {
        assert_eq!(
            serde_json::to_string(&AutonomousTurnOrigin::BackgroundTask).unwrap(),
            "\"background_task\""
        );
        assert_eq!(
            serde_json::to_string(&AutonomousTurnOrigin::Automation).unwrap(),
            "\"automation\""
        );
        assert_eq!(
            serde_json::to_string(&AutonomousTurnOrigin::AgentAutonomous).unwrap(),
            "\"agent_autonomous\""
        );
        let _ts = chrono::Utc.with_ymd_and_hms(2026, 8, 18, 0, 0, 0).unwrap();
    }
}
```

- [ ] **Step 2: Run the new tests and confirm they fail to compile**

From `src-tauri/`:

```powershell
cargo test --lib --features test-utils autonomous_origin_ -- --nocapture
```

Expected: compile error `cannot find type AutonomouTurnOrigin` / missing field (the enum does not exist yet).

- [ ] **Step 3: Add the enum and field; update every MessageTurn literal**

In `message.rs`, add the enum next to `TurnOutcome` and the optional field on `MessageTurn` exactly as specified above. Then add `autonomous_origin: None` to every `MessageTurn {` construction the compiler reports. Do not invent a Default impl that changes timestamps. Do not change existing ids, roles, or blocks.

In `src/lib/types.ts`, add `AutonomousTurnOrigin` and the optional field. Update the `ConversationDetail.transcript_watermark` comment from “Claude only” to: available for transcript-backed autonomous overlay providers (Claude, Grok `updates.jsonl`, Codex native rollout).

- [ ] **Step 4: Re-run the origin tests and a smoke parse test**

```powershell
cargo test --lib --features test-utils autonomous_origin_ -- --nocapture
```

Expected: PASS.

Also compile the crate so every literal was updated:

```powershell
cargo test --lib --features test-utils models::message:: -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/models/message.rs src/lib/types.ts
# plus every parser/acp file whose MessageTurn literals you updated
git commit -m "feat(acp): add optional autonomous origin on MessageTurn"
```

---

### Task 2: Provider Policy Selector

**Files:**
- Create: `src-tauri/src/acp/autonomous_activity.rs`
- Modify: `src-tauri/src/acp/mod.rs` (add `pub mod autonomous_activity;`)
- Test: unit tests inside `autonomous_activity.rs`

**Interfaces:**
- Consumes: `crate::models::agent::AgentType`.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum AutonomousActivityPolicy {
      ClaudeTranscript,
      GrokIdleWire,
      CodexGoalTranscript,
      Unsupported,
  }

  #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
  pub struct AutonomousCapabilities {
      pub goal_version: Option<u32>,
      pub load_session: bool,
  }

  impl AutonomousActivityPolicy {
      pub fn for_connection(agent: AgentType, caps: &AutonomousCapabilities) -> Self {
          match agent {
              AgentType::ClaudeCode => Self::ClaudeTranscript,
              AgentType::Grok => Self::GrokIdleWire,
              AgentType::Codex
                  if caps.goal_version == Some(1) && caps.load_session =>
              {
                  Self::CodexGoalTranscript
              }
              _ => Self::Unsupported,
          }
      }
  }
  ```
  This task does **not** wire the selector into `connection.rs`. Later tasks call `for_connection` at initialize. Custom agent named `"codex"` is `AgentType::Custom(_)` and must map to `Unsupported`.

- [ ] **Step 1: Write the failing policy tests**

Create `autonomous_activity.rs` with the tests first (types can be sketched as `todo!()` until Step 3, but prefer writing tests that do not compile until the API exists):

```rust
#[cfg(test)]
mod tests {
    use super::{AutonomousActivityPolicy, AutonomousCapabilities};
    use crate::models::agent::AgentType;

    #[test]
    fn claude_maps_to_transcript() {
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::ClaudeCode,
                &AutonomousCapabilities::default()
            ),
            AutonomousActivityPolicy::ClaudeTranscript
        );
    }

    #[test]
    fn grok_maps_to_idle_wire() {
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Grok,
                &AutonomousCapabilities::default()
            ),
            AutonomousActivityPolicy::GrokIdleWire
        );
    }

    #[test]
    fn codex_requires_goal_v1_and_load_session() {
        let qualified = AutonomousCapabilities {
            goal_version: Some(1),
            load_session: true,
        };
        assert_eq!(
            AutonomousActivityPolicy::for_connection(AgentType::Codex, &qualified),
            AutonomousActivityPolicy::CodexGoalTranscript
        );
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Codex,
                &AutonomousCapabilities {
                    goal_version: Some(1),
                    load_session: false,
                }
            ),
            AutonomousActivityPolicy::Unsupported
        );
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Codex,
                &AutonomousCapabilities {
                    goal_version: Some(2),
                    load_session: true,
                }
            ),
            AutonomousActivityPolicy::Unsupported
        );
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Codex,
                &AutonomousCapabilities::default()
            ),
            AutonomousActivityPolicy::Unsupported
        );
    }

    #[test]
    fn custom_codex_and_other_builtins_are_unsupported() {
        let qualified = AutonomousCapabilities {
            goal_version: Some(1),
            load_session: true,
        };
        for agent in [
            AgentType::Cursor,
            AgentType::OpenCode,
            AgentType::Gemini,
            AgentType::Cline,
            AgentType::Hermes,
            AgentType::CodeBuddy,
            AgentType::KimiCode,
            AgentType::Pi,
            AgentType::DeepSeek,
            AgentType::Custom("codex"),
        ] {
            assert_eq!(
                AutonomousActivityPolicy::for_connection(agent, &qualified),
                AutonomousActivityPolicy::Unsupported,
                "{agent:?}"
            );
        }
    }
}
```

Register the module in `mod.rs`.

- [ ] **Step 2: Run tests — expect compile failure or fail**

```powershell
cargo test --lib --features test-utils acp::autonomous_activity:: -- --nocapture
```

Expected: FAIL/compile error until the types exist.

- [ ] **Step 3: Implement `for_connection` exactly as specified**

No I/O, no watcher spawn, no heuristics beyond the match above.

- [ ] **Step 4: Re-run**

```powershell
cargo test --lib --features test-utils acp::autonomous_activity:: -- --nocapture
```

Expected: all four tests PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/acp/autonomous_activity.rs src-tauri/src/acp/mod.rs
git commit -m "feat(acp): select autonomous activity policy per agent"
```

---

### Task 3: Claude Adapter Behind Policy + Proven Origin

**Files:**
- Modify: `src-tauri/src/acp/background_watch.rs`
- Modify: `src-tauri/src/acp/connection.rs` (only the `spawn_if_claude` call site)
- Modify: `src-tauri/src/parsers/claude.rs` (cold-parse origin for `<task-notification>` / explicit automation marker)
- Modify: `src-tauri/src/acp/types.rs` (`BackgroundActivity` docs — provider-neutral wording)
- Test: existing `background_watch` tests plus new origin tests in `claude.rs` / `background_watch.rs`

**Interfaces:**
- Consumes: `AutonomousActivityPolicy::for_connection`, existing `spawn_if_claude` / `PromptLedger`.
- Produces: `pub(crate) fn spawn_for_policy(policy, conn_id, state, emitter, cwd, ledger) -> Option<BackgroundWatchGuard>` that spawns the existing watcher only for `ClaudeTranscript`. `spawn_if_claude` becomes a thin wrapper that maps `AgentType` through `for_connection(..., &AutonomousCapabilities::default())` **or** is deleted and the connection call site uses `spawn_for_policy`. Prefer keeping `spawn_if_claude` as a wrapper so existing tests that name it keep compiling.
- Claude task-notification episodes set `autonomous_origin = Some(BackgroundTask)`. Claude cron/loop records with an explicit transcript automation marker set `Automation`. Other out-of-turn Claude episodes proven only by the live prompt ledger set `AgentAutonomous` on the live overlay; the cold parser leaves origin absent unless it can prove the shape independently.
- No Grok or Codex code in `background_watch.rs`.

- [ ] **Step 1: Write failing Claude origin tests**

In `parsers/claude.rs` tests, add a fixture that contains a `<task-notification>` continuation (reuse the existing task-notification fixture shape if one already exists — extend it rather than inventing a new transcript dialect). Assert:

```rust
#[test]
fn task_notification_continuation_is_background_task_origin() {
    // build/parse the existing-style fixture that already proves a
    // <task-notification> assistant continuation
    let detail = /* parse fixture */;
    let continuation = detail
        .turns
        .iter()
        .find(|t| {
            t.role == TurnRole::Assistant
                && t.blocks.iter().any(|b| match b {
                    ContentBlock::Text { text } => !text.contains("<task-notification>"),
                    _ => true,
                })
        })
        .expect("assistant continuation after notification");
    assert_eq!(
        continuation.autonomous_origin,
        Some(crate::models::message::AutonomousTurnOrigin::BackgroundTask)
    );
}
```

If the existing Claude tests already have a task-notification transcript, add the origin assertion there instead of duplicating the fixture. Also add a test that a normal user-prompted assistant turn still has `autonomous_origin == None`.

- [ ] **Step 2: Run the new tests — expect FAIL**

```powershell
cargo test --lib --features test-utils task_notification_continuation_is_background_task_origin -- --nocapture --exact
```

Expected: FAIL (`autonomous_origin` is `None`).

- [ ] **Step 3: Implement origin annotation + policy spawn**

1. When the Claude watcher (and the Claude detail parser) recognizes a `<task-notification>`-owned continuation, set `BackgroundTask`.
2. When the transcript has an explicit automation/cron marker the existing watcher already treats as out-of-turn automation, set `Automation`.
3. Live overlay turns classified only by `PromptLedger` (no reconstructible marker) get `AgentAutonomous` on the overlay only.
4. Replace the connection call:

```rust
background_watch::spawn_if_claude(...)
```

with a policy-aware spawn that still no-ops for non-Claude. Hidden-generation connections still pass `None`.

5. Rewrite `BackgroundActivity` rustdoc in `types.rs` so it no longer says Claude-only.

Keep poll cadence, ledger, rotation, settlement, and watermark behavior unchanged.

- [ ] **Step 4: Run Claude regression + new origin tests**

```powershell
cargo test --lib --features test-utils background_watch -- --nocapture
cargo test --lib --features test-utils parsers::claude:: -- --nocapture
```

Expected: PASS, including previous prompt-ledger / outstanding / rotation tests.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/acp/background_watch.rs src-tauri/src/acp/connection.rs src-tauri/src/parsers/claude.rs src-tauri/src/acp/types.rs
git commit -m "feat(acp): start Claude watcher from autonomous policy"
```

---

### Task 4: Grok Parser Watermark and Cold Origin

**Files:**
- Modify: `src-tauri/src/parsers/grok.rs`
- Test: existing `hidden_user_chunk_does_not_split_the_reply` plus new tests in `grok.rs`

**Interfaces:**
- Consumes: current `parse_updates` / `GrokParser::get_conversation`.
- Produces:
  - `ConversationDetail.transcript_watermark = Some(consumed_complete_bytes)` where `consumed_complete_bytes` is the exact number of bytes of complete lines read from `updates.jsonl` (including each line's `\n`). A trailing partial line is **not** counted.
  - Malformed / non-UTF-8 complete lines follow the existing skip policy but still advance the byte cursor.
  - Hidden `_meta.hideFromScrollback` user chunks still produce no user turn and no content block.
  - A hidden trigger at an **idle turn boundary** (previous assistant already flushed; not inside an open foreground assistant) that matches the verified background-task reminder shape (`<system-reminder>` + background-task completion text, optionally naming a task id) marks the **following** independently opened assistant turn:
    - `autonomous_origin = Some(BackgroundTask)`
    - `id = grok-autonomous:<episode-key>:assistant:0`
    - episode-key is derived from external session id + referenced task id set (when present) + hidden-trigger complete-line **start** byte offset. When no task id is present, session id + trigger offset is the key. Use a deterministic hex digest of that material if the raw key would be an illegal id; document the exact format in a `pub(crate) fn grok_autonomous_turn_id(...)` next to the parser so Task 5 can call the same function.
  - A hidden reminder injected while a foreground assistant accumulator is still open keeps today's behavior: suppress the reminder, do not cut or relabel that assistant, do not assign an autonomous id.
  - The same task id notified twice (two trigger offsets) produces two distinct ids.
  - All other Grok turns keep `grok-turn-<index>` assigned after parse, except the recognized autonomous assistant turns which keep the canonical id (do not overwrite them in the positional loop).
  - Incomplete autonomous turns (no `turn_completed` yet) are still returned on cold load.

- [ ] **Step 1: Write failing parser tests**

Add next to `hidden_user_chunk_does_not_split_the_reply` in `grok.rs`:

```rust
#[test]
fn updates_watermark_is_complete_line_bytes_only() {
    let complete = concat!(
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"},"_meta":{"promptIndex":0}}},"timestamp":1}"#,
        "\n",
    );
    let partial = r#"{"method":"session/update""#;
    let (_tmp, sessions) = fixture(SUMMARY, &format!("{complete}{partial}"));
    let detail = GrokParser::with_base_dir(sessions)
        .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
        .unwrap();
    assert_eq!(
        detail.transcript_watermark,
        Some(complete.len() as u64)
    );
}

#[test]
fn idle_hidden_trigger_marks_following_assistant_background_task() {
    let updates = concat!(
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"run it"},"_meta":{"promptIndex":0}}},"timestamp":1}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"started"}}},"timestamp":2}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"turn_completed","stop_reason":"end_turn"}},"timestamp":3}"#,
        "\n",
        r#"{"method":"_x.ai/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"task_completed","task_id":"term_x"}},"timestamp":4}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"<system-reminder>\nBackground task \"term_x\" completed (exit code: 0).\n</system-reminder>"},"_meta":{"hideFromScrollback":true,"promptIndex":1}}},"timestamp":5}"#,
        "\n",
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}},"timestamp":6}"#,
        "\n",
    );
    let (_tmp, sessions) = fixture(SUMMARY, updates);
    let detail = GrokParser::with_base_dir(sessions)
        .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
        .unwrap();
    assert_eq!(
        detail.turns.iter().filter(|t| t.role == TurnRole::User).count(),
        1
    );
    assert!(!detail.turns.iter().any(|t| t.blocks.iter().any(
        |b| matches!(b, ContentBlock::Text { text } if text.contains("system-reminder"))
    )));
    let auto = detail
        .turns
        .iter()
        .find(|t| t.autonomous_origin == Some(AutonomousTurnOrigin::BackgroundTask))
        .expect("autonomous assistant");
    assert!(auto.id.starts_with("grok-autonomous:"));
    assert!(auto.id.ends_with(":assistant:0"));
    assert!(matches!(&auto.blocks[0], ContentBlock::Text { text } if text == "done"));
    let again = GrokParser::with_base_dir(
        // re-parse same fixture
        detail
            .summary
            .folder_path
            .as_ref()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| panic!("sessions dir")),
    );
    // Prefer a second get_conversation on the same parser/base:
    let detail2 = GrokParser::with_base_dir(sessions)
        .get_conversation("019f45e3-e1ef-7690-a29f-fe2554382b49")
        .unwrap();
    let auto2 = detail2
        .turns
        .iter()
        .find(|t| t.autonomous_origin.is_some())
        .unwrap();
    assert_eq!(auto.id, auto2.id);
}

#[test]
fn hidden_reminder_inside_open_assistant_does_not_relabel_it() {
    // keep hidden_user_chunk_does_not_split_the_reply green and additionally:
    let detail = /* same fixture as hidden_user_chunk_does_not_split_the_reply */;
    assert!(detail
        .turns
        .iter()
        .all(|t| t.autonomous_origin.is_none()));
}

#[test]
fn two_triggers_same_task_get_distinct_ids() {
    // two idle hidden reminders for term_x at different offsets, each followed
    // by an assistant message. Assert two BackgroundTask turns with different ids.
}
```

Fix the second-parse helper in the first test to reuse the same `sessions` `PathBuf` (clone it before the first parse if `GrokParser` takes ownership — `with_base_dir` takes `PathBuf`; clone).

- [ ] **Step 2: Run — expect FAIL**

```powershell
cargo test --lib --features test-utils updates_watermark_is_complete_line_bytes_only idle_hidden_trigger_marks_following_assistant_background_task -- --nocapture
```

Expected: FAIL (`transcript_watermark` is `None`; origin is `None`).

- [ ] **Step 3: Implement scanner + cold recovery**

Replace `BufReader::lines()` in `parse_updates` with a complete-line reader that:

1. Reads until `\n` (include the `\n` in the consumed count).
2. If the file ends without `\n`, leave those bytes uncounted.
3. On UTF-8/JSON failure of a complete line, skip the record but keep the bytes.

Track whether an assistant is open. On hidden trigger:

- if assistant is open → continue (suppress only);
- if assistant is flushed → record pending autonomous episode (trigger offset + extracted task ids from the exact reminder tag/shape already implied by `hidden_user_chunk_does_not_split_the_reply`);
- the next independently opened assistant receives the canonical id + `BackgroundTask`.

Extract task ids only from the verified reminder shape (quoted name after `Background task`). Do not classify by matching arbitrary English continuation text.

Assign positional `grok-turn-{i}` only to turns whose id is still empty.

Return `transcript_watermark: Some(consumed)` from `get_conversation`.

Keep `hidden_user_chunk_does_not_split_the_reply` passing.

- [ ] **Step 4: Re-run Grok parser tests**

```powershell
cargo test --lib --features test-utils parsers::grok:: -- --nocapture
```

Expected: PASS, including prior snapshots of normal `grok-turn-*` ids.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/parsers/grok.rs
git commit -m "feat(parser): recover Grok autonomous turns and watermarks"
```

---

### Task 5: Grok Idle Observer and Transcript Tailer

**Files:**
- Create: `src-tauri/src/acp/grok_autonomous.rs`
- Modify: `src-tauri/src/acp/mod.rs`
- Modify: `src-tauri/src/acp/connection.rs` (raw-dispatch observation + prompt gate + idle terminal)
- Modify: `src-tauri/src/acp/session_state.rs` (provider-neutral outstanding comments)
- Test: `grok_autonomous.rs` unit tests using temp `updates.jsonl` files

**Interfaces:**
- Consumes: `AutonomousActivityPolicy::GrokIdleWire`, `parsers::grok` scanner/id helper from Task 4, existing `AcpEvent::BackgroundActivity`, `SessionState`, `EventEmitter`.
- Produces a connection-scoped observer with these hooks (names may live on a `GrokAutonomousAdapter` struct stored on the connection loop):
  - `on_session_ready(session_id, updates_jsonl_path)` — establish complete-line baseline; do not emit a fabricated content watermark if the file is missing.
  - `on_foreground_started()` / `on_foreground_ended()` — ownership boundary.
  - `on_raw_dispatch(method, params, ownership)` — observe **before** private variants are discarded:
    - `_x.ai/session/update` `task_backgrounded` / `task_completed` (any ownership) update the task ledger and `outstanding`.
    - idle-only hidden `user_message_chunk` (`hideFromScrollback == true` + verified reminder) opens one episode.
    - idle-only assistant/thought/tool updates advance the open episode and trigger an immediate tail pass.
    - idle-only `turn_completed` (standard or namespaced) closes the wire episode.
  - `on_disconnect()` — drop live state; do not kill parser recovery.
- Task ledger: `task_backgrounded(task_id)` inserts; `task_completed` removes and remembers as recently settled; unknown completion never underflows; cap 64; TTL `background_keepalive_max_age()`.
- Episode SM: `Dormant → Opening → Open → AwaitingPersistedTerminal → Closed` plus `SuppressedForeground` / `Abandoned` as specified. One open episode per connection.
- Tailer: from last complete-byte baseline, consume complete `updates.jsonl` lines, assemble the current assistant via the same normalization as the Grok parser, emit whole-turn `BackgroundActivity` with stable id, `autonomous_origin: background_task`, `watermark = committed bytes`. Hidden trigger contributes no block. Retry immediately on wire update and every 1s while active. Do not emit an unwatermarked turn.
- Prompt gate: once an idle episode opens, `autonomous_busy == true`. The connection prompt-receive branch must not send `session/prompt` until terminal or stale close. Control-lane cancel/disconnect still wins. Do not set `turn_in_flight` or `StatusChanged(prompting)`.
- Idle `turn_completed` claimed by the adapter must **not** call `finalize_turn_terminal_with_permissions` or emit foreground `TurnComplete`. It emits the final upsert, schedules detail refetch, releases the prompt gate, tombstones the episode.
- `settled` stays empty for Grok V1 (no `task_snapshot` result cards).

- [ ] **Step 1: Write failing observer tests**

Put tests in `grok_autonomous.rs` that drive the adapter with constructed JSON values and a temp file — do not boot a full ACP connection.

```rust
#[tokio::test]
async fn task_completed_without_trigger_creates_no_turn() {
    let mut adapter = GrokAutonomousAdapter::new_for_test(tmp_updates(""));
    adapter.on_raw_dispatch(
        "_x.ai/session/update",
        &json!({"update":{"sessionUpdate":"task_completed","task_id":"t1"}}),
        Ownership::Idle,
    );
    assert!(adapter.take_emitted().turns.is_empty());
    assert_eq!(adapter.outstanding(), 0);
}

#[tokio::test]
async fn hidden_trigger_then_persisted_assistant_upserts_one_stable_turn() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.jsonl");
    std::fs::write(&path, "").unwrap();
    let mut adapter = GrokAutonomousAdapter::new_for_test(path.clone());
    adapter.on_session_ready("sess", &path);

    adapter.on_raw_dispatch(
        "_x.ai/session/update",
        &json!({"update":{"sessionUpdate":"task_completed","task_id":"term_x"}}),
        Ownership::Idle,
    );
    adapter.on_raw_dispatch(
        "session/update",
        &json!({"update":{
            "sessionUpdate":"user_message_chunk",
            "content":{"type":"text","text":"<system-reminder>\nBackground task \"term_x\" completed (exit code: 0).\n</system-reminder>"},
            "_meta":{"hideFromScrollback":true}
        }}),
        Ownership::Idle,
    );
    assert!(adapter.take_emitted().turns.is_empty(), "no emit before persist");

    append_line(&path, /* hidden trigger + agent_message_chunk "hello" */);
    adapter.tail_once();
    let first = adapter.take_emitted();
    assert_eq!(first.turns.len(), 1);
    assert_eq!(first.turns[0].autonomous_origin, Some(AutonomousTurnOrigin::BackgroundTask));
    let id = first.turns[0].id.clone();

    append_line(&path, /* more agent_message_chunk " world" */);
    adapter.tail_once();
    let second = adapter.take_emitted();
    assert_eq!(second.turns[0].id, id);
    assert!(matches!(&second.turns[0].blocks[0], ContentBlock::Text { text } if text.contains("hello")));
}

#[test]
fn visible_user_chunk_does_not_open_episode() { /* ... */ }

#[test]
fn foreground_ownership_suppresses_overlay() { /* hidden trigger under Foreground → no episode */ }

#[test]
fn duplicate_task_launch_is_idempotent() { /* outstanding stays 1 */ }

#[test]
fn unknown_completion_does_not_underflow() { /* outstanding stays 0 */ }

#[test]
fn prompt_gate_holds_until_terminal() {
    // after open episode, adapter.autonomous_busy() == true
    // after turn_completed + persisted terminal, false
}
```

Also add a focused connection-loop test only if there is an existing harness for injecting idle raw dispatches. If not, keep prompt-gate logic unit-tested on the adapter and a small `fn should_hold_prompt(adapter) -> bool` used by `connection.rs`.

- [ ] **Step 2: Run — expect FAIL**

```powershell
cargo test --lib --features test-utils acp::grok_autonomous:: -- --nocapture
```

- [ ] **Step 3: Implement adapter + wire it**

In `connection.rs`:

1. After initialize, if policy is `GrokIdleWire` and not hidden-generation, construct the adapter (store in the loop, e.g. `Option<GrokAutonomousAdapter>`).
2. On session ready, resolve the Grok session dir the same way `GrokParser` does and call `on_session_ready`.
3. In both foreground and idle dispatch arms, **before** `MatchDispatch` drops private methods, call `on_raw_dispatch`.
4. If the adapter claims the record as autonomous content, do not feed it to the ordinary foreground streaming reducer.
5. Before sending a queued `session/prompt`, skip while `adapter.autonomous_busy()`.
6. Idle `turn_completed` handled by the adapter must not take the foreground terminal path.

Reuse Grok parser helpers; do not duplicate block normalization.

- [ ] **Step 4: Re-run**

```powershell
cargo test --lib --features test-utils acp::grok_autonomous:: -- --nocapture
cargo test --lib --features test-utils parsers::grok:: -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/acp/grok_autonomous.rs src-tauri/src/acp/mod.rs src-tauri/src/acp/connection.rs src-tauri/src/acp/session_state.rs
git commit -m "feat(acp): observe Grok idle background-task follow-ups"
```

---

### Task 6: Codex Parser Watermark and Goal-Turn Identity

**Files:**
- Modify: `src-tauri/src/parsers/codex.rs`
- Test: existing Codex parser tests plus new Goal-cycle tests

**Interfaces:**
- Consumes: current rollout parse / `group_into_turns` / `is_codex_internal_context_message`.
- Produces:
  - `transcript_watermark = Some(consumed_complete_bytes)` of the rollout JSONL (complete lines only; trailing partial uncounted).
  - Rollout authority helper `pub(crate) fn rollout_session_id(path) -> Option<String>` reading `session_meta.payload.id`. Task 7 uses this to require exact ACP session id match and to reject missing/duplicate/mismatched files.
  - Recognize native `event_msg.task_started` / `task_complete` including `payload.turn_id` when present.
  - Goal ownership is structural: user input whose text is `<codex_internal_context source="goal">...</codex_internal_context>` (reuse/extend `is_codex_internal_context_message` to require `source="goal"` for *autonomous classification*; keep suppressing **every** text-only internal envelope from user turns and titles as today).
  - A native turn that contains that Goal context:
    - assistant `MessageTurn.id = format!("codex-goal-turn-{turn_id}")` where `turn_id` is the `task_started` turn id (if missing, do **not** invent Goal identity — leave positional `turn-N`).
    - `autonomous_origin = Some(AgentAutonomous)`
    - internal context still creates no user turn / no content block / no title.
  - Incomplete Goal turns (no `task_complete`) still return the partial assistant.
  - Repeated parses produce the same id and blocks.
  - Non-Goal Codex turns keep `turn-N` ids and `autonomous_origin: None`.
  - Internally retain provider `msg_*` / `rs_*` / call / tool ids for correlation (on `UnifiedMessage.id` or a side map). Do not put ACP `item-N` ids into canonical `MessageTurn.id`.

- [ ] **Step 1: Write failing tests**

Add in `codex.rs` tests, following the existing `session_meta` + `task_started` fixture style:

```rust
#[test]
fn rollout_watermark_ignores_trailing_partial_line() {
    let complete = concat!(
        r#"{"timestamp":"2026-08-18T00:00:00Z","type":"session_meta","payload":{"id":"sess-1","cwd":"/tmp"}}"#,
        "\n",
    );
    let path = write_rollout(&format!("{complete}{{\"type\":\""));
    let detail = parse_path(&path);
    assert_eq!(detail.transcript_watermark, Some(complete.len() as u64));
}

#[test]
fn goal_context_turn_uses_native_id_and_agent_autonomous_origin() {
    let jsonl = concat!(
        r#"{"timestamp":"2026-08-18T00:00:00Z","type":"session_meta","payload":{"id":"01abc","cwd":"/tmp"}}"#,
        "\n",
        r#"{"timestamp":"2026-08-18T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn_goal_1"}}"#,
        "\n",
        r#"{"timestamp":"2026-08-18T00:00:02Z","type":"response_item","payload":{"type":"message","role":"user","id":"msg_hidden","content":[{"type":"input_text","text":"<codex_internal_context source=\"goal\">\nContinue working toward the active thread goal.\n</codex_internal_context>"}]}}"#,
        "\n",
        r#"{"timestamp":"2026-08-18T00:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","id":"msg_live","content":[{"type":"output_text","text":"working"}]}}"#,
        "\n",
    );
    let detail = parse_path(&write_rollout(jsonl));
    assert!(detail.turns.iter().all(|t| t.role != TurnRole::User
        || !t.blocks.iter().any(|b| matches!(b, ContentBlock::Text { text } if text.contains("codex_internal_context")))));
    let auto = detail
        .turns
        .iter()
        .find(|t| t.role == TurnRole::Assistant)
        .unwrap();
    assert_eq!(auto.id, "codex-goal-turn-turn_goal_1");
    assert_eq!(auto.autonomous_origin, Some(AutonomousTurnOrigin::AgentAutonomous));
    assert!(matches!(&auto.blocks[0], ContentBlock::Text { text } if text.contains("working")));
}

#[test]
fn non_goal_codex_turn_keeps_positional_id() {
    // existing-style user + assistant without source=goal
    let detail = /* fixture */;
    let assistant = detail.turns.iter().find(|t| t.role == TurnRole::Assistant).unwrap();
    assert!(assistant.id.starts_with("turn-"));
    assert_eq!(assistant.autonomous_origin, None);
}

#[test]
fn goal_internal_context_never_becomes_title() {
    // extend the existing title-suppression test if one already covers this
}
```

Use the same temp-file helpers the Codex tests already use (`write` + parse). Do not invent a second parser entry point.

- [ ] **Step 2: Run — expect FAIL**

```powershell
cargo test --lib --features test-utils rollout_watermark_ignores_trailing_partial_line goal_context_turn_uses_native_id_and_agent_autonomous_origin -- --nocapture
```

- [ ] **Step 3: Implement**

Thread a complete-byte cursor through the rollout reader. Track the current `task_started.turn_id`. When a Goal `source="goal"` internal user envelope is seen for that native turn, mark the turn Goal-owned and suppress the envelope (already suppressed for titles; also skip adding a user `UnifiedMessage` if one would be created). When grouping assistant messages for a Goal-owned turn, set id + origin as specified.

Do not change positional ids for other turns. Do not classify by matching the English “Continue working…” sentence.

- [ ] **Step 4: Re-run Codex parser tests**

```powershell
cargo test --lib --features test-utils parsers::codex:: -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/parsers/codex.rs
git commit -m "feat(parser): identify Codex Goal turns and rollout watermarks"
```

---

### Task 7: Codex Goal Observer and Authority Gate

**Files:**
- Create: `src-tauri/src/acp/codex_autonomous.rs`
- Modify: `src-tauri/src/acp/mod.rs`
- Modify: `src-tauri/src/acp/connection.rs` (initialize policy, raw `session_info_update`, prompt gate, no ACP `session/load` reconciliation)
- Test: `codex_autonomous.rs`

**Interfaces:**
- Consumes: `AutonomousActivityPolicy::CodexGoalTranscript`, Task 6 scanner/id helpers, existing Goal-card path (do not replace it).
- Produces `CodexAutonomousAdapter`:
  - Provisional enable only when `for_connection` returned `CodexGoalTranscript`.
  - Authority: find **exactly one** regular-file rollout whose `session_meta.payload.id` equals the ACP session id, using the same Codex home resolution as `CodexParser`. File not yet created → provisionally armed; idle opening starts a 30s retry. Missing/ambiguous/mismatch/timeout → `UnsupportedForConnection` (no overlays; Goal cards + prompts continue).
  - Idle `session_info_update._meta.goal.status = active` is remembered; it does **not** open an episode.
  - Idle `session_info_update._meta.codex.threadStatus.type = active` under an observed active Goal opens one episode and arms the rollout tail at the last complete-byte baseline.
  - Foreground `threadStatus: active` does not open an overlay.
  - A Goal-control JSON-RPC still in flight is **not** foreground.
  - Goal status `complete` / `blocked` / `limited` updates the existing Goal card only; episode stays open.
  - Live ACP `msg_*` / tool ids are correlation hints only. Emit `BackgroundActivity` only after rollout records for that native turn are consumed. Canonical id from `task_started.turn_id`. Never admit `item-N` replay ids.
  - Idle `threadStatus: idle` → `AwaitingPersistedTerminal`; tail through matching `task_complete` when present; then final upsert, tombstone, release prompt gate, refetch. If wire idle beats `task_complete`, keep retrying; do not fabricate a terminal watermark.
  - Keepalive: outstanding = 1 while Goal is active **or** an episode is open; 0 only when Goal is non-active **and** no episode is open. `settled` is always empty.
  - Prompt gate identical to Grok: `autonomous_busy` holds `session/prompt`; cancel/disconnect/Goal update/clear still work.
  - ACP `session/load` completion is never an episode terminal and never retires overlays.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn goal_active_alone_does_not_open_episode() { /* ... */ }

#[test]
fn idle_thread_active_under_goal_opens_after_rollout_proves_turn() {
    // write rollout with session_meta.id == acp session, task_started turn_id,
    // source=goal context, assistant message
    // feed Goal active + idle threadStatus active
    // tail_once → one turn id codex-goal-turn-...
}

#[test]
fn foreground_thread_active_does_not_overlay() { /* ... */ }

#[test]
fn goal_complete_does_not_close_episode() {
    // open episode, send goal.status=complete, more assistant in rollout,
    // still Open; later threadStatus idle closes
}

#[test]
fn two_idle_cycles_get_distinct_native_ids() { /* ... */ }

#[test]
fn item_n_ids_are_not_canonical() { /* feed a fake session/load item-1; adapter ignores */ }

#[test]
fn missing_rollout_after_30s_downgrades_only_autonomous() {
    // use a test hook to expire the window immediately
}

#[test]
fn mismatched_session_id_is_unsupported() { /* ... */ }

#[test]
fn prompt_gate_and_keepalive_unit() { /* outstanding 1 while goal active */ }
```

- [ ] **Step 2: Run — expect FAIL**

```powershell
cargo test --lib --features test-utils acp::codex_autonomous:: -- --nocapture
```

- [ ] **Step 3: Implement adapter + connection wiring**

At initialize, build `AutonomousCapabilities { goal_version: meta.goal.version if == 1, load_session: init_resp.agent_capabilities.load_session }` and select policy. Only `AgentType::Codex` + qualified caps construct this adapter.

Observe raw `session_info_update` in both ownership branches. Do not treat Goal-control as a Codeg prompt.

Share record classification with the Codex parser. Do not parse ACP `session/load` output for overlay content.

- [ ] **Step 4: Re-run**

```powershell
cargo test --lib --features test-utils acp::codex_autonomous:: -- --nocapture
cargo test --lib --features test-utils parsers::codex:: -- --nocapture
cargo test --lib --features test-utils acp::autonomous_activity:: -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/acp/codex_autonomous.rs src-tauri/src/acp/mod.rs src-tauri/src/acp/connection.rs
git commit -m "feat(acp): observe capability-qualified Codex Goal cycles"
```

---

### Task 8: Frontend Marker, Merge Boundary, Overlay Dedupe

**Files:**
- Modify: `src/lib/types.ts` (already has origin from Task 1; watermark docs)
- Modify: `src/contexts/acp-connections-context.tsx` (drop-log wording only; keep prompting guard)
- Modify: `src/stores/conversation-runtime-store.ts`
- Modify: `src/stores/background-overlay.test.ts`
- Modify: `src/components/message/message-list-view.tsx`
- Modify: `src/components/message/message-list-view.test.tsx`
- Modify: `src/i18n/messages/{ar,de,en,es,fr,ja,ko,pt,zh-CN,zh-TW}.json`
- Test: Vitest files above

**Interfaces:**
- Consumes: `MessageTurn.autonomous_origin`, existing `applyBackgroundActivity` / overlay retirement.
- Produces:
  - Overlay retirement unchanged: retire when `detail.transcript_watermark >= entry.watermark`. Same autonomous id in detail + overlay renders **once**, preferring the newer overlay until retirement. Origin from overlay is preserved until retirement.
  - `mergeConsecutiveAssistantTurns`: autonomous origin is a hard grouping boundary. Distinct episode ids do not merge. Foreground assistants keep current merge behavior when neither side has origin.
  - `ResolvedMessageGroup` (or the assistant item/group used for memoization) carries optional origin so origin-only updates invalidate cached groups.
  - Marker: compact icon + `t("messageList.backgroundContinuation")` above assistant content, outside the bubble, excluded from copy. Only when `autonomous_origin` is set.
  - Locales (verbatim):
    - `zh-CN`: `后台续写`
    - `zh-TW`: `後台續寫`
    - `en`: `Background continuation`
    - `ja`: `バックグラウンド継続`
    - `ko`: `백그라운드 이어쓰기`
    - `de`: `Hintergrund-Fortsetzung`
    - `es`: `Continuación en segundo plano`
    - `fr`: `Suite en arrière-plan`
    - `pt`: `Continuação em segundo plano`
    - `ar`: `متابعة في الخلفية`
  - Change the sampled out-of-turn drop log from “the transcript overlay renders them” to “provider policy owns out-of-turn rendering.”

- [ ] **Step 1: Write failing frontend tests**

In `background-overlay.test.ts`:

```ts
it("same autonomous id prefers newer overlay until watermark covers it", () => {
  actions().applyBackgroundActivity(7, [turn("grok-autonomous:x:assistant:0", "v1")], 100)
  useConversationRuntimeStore.setState(/* detail with same id text "old" watermark 50 */)
  const rows = selectTimelineTurns(7)
  const matches = rows.filter((r) => r.id === "grok-autonomous:x:assistant:0")
  expect(matches).toHaveLength(1)
  expect(matches[0].blocks[0]).toMatchObject({ type: "text", text: "v1" })
})

it("equal grok watermark retires the overlay", () => {
  actions().applyBackgroundActivity(7, [turn("grok-autonomous:x:assistant:0", "v1")], 200)
  // apply detail watermark 200 with the same id
  expect(
    selectTimelineTurns(7).filter((r) => r.id === "grok-autonomous:x:assistant:0")
  ).toHaveLength(1) // the detail copy, not a leftover overlay
})
```

In `message-list-view.test.tsx`:

```ts
it("does not merge foreground into autonomous or autonomous into foreground", () => {
  const merged = mergeConsecutiveAssistantTurns([
    assistantItem("a", { autonomous_origin: undefined }),
    assistantItem("b", { autonomous_origin: "background_task" }),
    assistantItem("c", { autonomous_origin: undefined }),
  ])
  expect(merged).toHaveLength(3)
})

it("does not merge distinct autonomous episode ids", () => {
  const merged = mergeConsecutiveAssistantTurns([
    assistantItem("grok-autonomous:1:assistant:0", { autonomous_origin: "background_task" }),
    assistantItem("grok-autonomous:2:assistant:0", { autonomous_origin: "background_task" }),
  ])
  expect(merged).toHaveLength(2)
})
```

Add a render test that a turn with `autonomous_origin: "background_task"` shows the zh-CN string when locale is `zh-CN`, and that reminder text is absent. Historical turns without origin show no marker.

Extend `assistantItem` helper to accept optional origin (thread it onto the turn/group). If the helper does not yet have a place for turn metadata, add `autonomous_origin` on the underlying `MessageTurn` and read it in `mergeConsecutiveAssistantTurns`.

- [ ] **Step 2: Run — expect FAIL**

```powershell
pnpm exec vitest run src/stores/background-overlay.test.ts src/components/message/message-list-view.test.tsx
```

- [ ] **Step 3: Implement UI + overlay + i18n**

In `mergeConsecutiveAssistantTurns`, when deciding whether to keep buffering, treat a change in `autonomous_origin` **or** a change in autonomous episode id as a flush boundary.

Render the marker as chrome above `MessageContent`, not inside copyable text.

Keep `applyStreamingAction` guard: still drop ordinary streaming when `status !== "prompting"`.

- [ ] **Step 4: Re-run frontend tests**

```powershell
pnpm exec vitest run src/stores/background-overlay.test.ts src/components/message/message-list-view.test.tsx src/stores/conversation-runtime-store.test.ts
```

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/lib/types.ts src/contexts/acp-connections-context.tsx src/stores/conversation-runtime-store.ts src/stores/background-overlay.test.ts src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx src/i18n/messages
git commit -m "feat(ui): mark autonomous continuations and keep them unmerged"
```

---

### Task 9: Integration Fixtures

**Files:**
- Create: `src-tauri/src/acp/fixtures/grok_autonomous_session_3806.jsonl` (redacted)
- Create: `src-tauri/src/acp/fixtures/codex_goal_autonomous_two_cycles.jsonl` (redacted)
- Create/Modify: tests in `grok_autonomous.rs` and `codex_autonomous.rs` (or a focused `src-tauri/src/acp/autonomous_integration.rs` test module) that load those fixtures
- Do **not** add a `tests/*.rs` integration binary unless the in-crate tests cannot drive the adapters

**Interfaces:**
- Consumes: Tasks 5 and 7 adapters + Task 4/6 parsers.
- Produces fixture-driven proofs of the spec acceptance path.

Grok fixture (session-3806 shape, redacted):

```text
task_completed
hidden user_message_chunk
agent thought/message/tool updates
turn_completed
```

Assert: no foreground `TurnComplete` signal from the adapter; one marked assistant incrementally upserted (stable id); final refetch requested; parser watermark >= last overlay watermark; no `system-reminder` in any emitted block.

Codex fixture (CLI 0.146.0 / codex-acp 1.4.0 shape, redacted) with two cycles:

```text
foreground prompt terminal
Goal active
idle threadStatus active
native task_started + source="goal" context
thought/message updates with rs_*/msg_* ids
idle threadStatus idle + native task_complete
idle threadStatus active
Goal complete
more thought/message updates
idle threadStatus idle + native task_complete
session/load replay using item-N ids
```

Assert: two independent `agent_autonomous` turns; Goal complete does not truncate the second; native ids survive cold parse; internal context never renders; `item-N` ignored; rollout watermarks retire both overlays.

- [ ] **Step 1: Write failing fixture tests that load the files**

```rust
#[test]
fn grok_session_3806_fixture_emits_one_marked_turn_and_covering_watermark() { /* ... */ }

#[test]
fn codex_two_cycle_fixture_keeps_native_ids_after_replay() { /* ... */ }
```

Check the fixtures in as redacted JSONL (no real user content, no secrets). Prefer synthesizing from the shapes already used in Tasks 5–7 if a raw capture is not in-repo.

- [ ] **Step 2: Run — expect FAIL if wiring gaps remain**

```powershell
cargo test --lib --features test-utils grok_session_3806_fixture_emits_one_marked_turn_and_covering_watermark codex_two_cycle_fixture_keeps_native_ids_after_replay -- --nocapture
```

- [ ] **Step 3: Fill any remaining adapter/parser gaps the fixtures expose**

Do not expand to other providers. Do not add a database migration.

- [ ] **Step 4: Run the autonomous + parser + frontend slice**

```powershell
cargo test --lib --features test-utils acp::autonomous_activity:: acp::grok_autonomous:: acp::codex_autonomous:: parsers::grok:: parsers::codex:: background_watch -- --nocapture
pnpm exec vitest run src/stores/background-overlay.test.ts src/components/message/message-list-view.test.tsx
```

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/acp/fixtures src-tauri/src/acp/grok_autonomous.rs src-tauri/src/acp/codex_autonomous.rs
git commit -m "test(acp): cover Grok and Codex autonomous fixtures"
```

---

## Self-Review

**Spec coverage**

| Spec area | Task |
| --- | --- |
| `AutonomousTurnOrigin` + optional field | 1 |
| Policy table + fail-closed Codex caps | 2, 7 |
| Claude watcher + origin + no Grok/Codex in watcher | 3 |
| Grok watermark, hidden trigger, canonical id, cold load | 4 |
| Grok observer, ledger, prompt gate, no foreground terminal | 5 |
| Codex watermark, native Goal id, suppress internal context | 6 |
| Codex observer, 30s authority, Goal≠terminal, keepalive unit | 7 |
| Marker, locales, merge boundary, overlay retirement/dedupe, drop-log wording | 8 |
| Session-3806 + two-cycle Codex fixtures | 9 |
| Unsupported agents stay unsupported | 2 |
| No DB migration / no prompting-guard removal | Global + 8 |

**Placeholder scan:** no TBD/TODO left in task steps.

**Type consistency:** `AutonomousTurnOrigin` snake_case wires, Grok id `grok-autonomous:<episode-key>:assistant:0`, Codex id `codex-goal-turn-<turn_id>`, watermark = complete transcript/rollout bytes, `outstanding` provider-neutral, Codex settled always empty.
