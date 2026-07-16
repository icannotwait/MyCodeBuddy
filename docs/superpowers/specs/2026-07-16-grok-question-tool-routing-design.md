# Grok Question Tool Routing Design

## Status

Approved. This design supersedes
`2026-07-14-grok-native-ask-user-question-design.md` for Grok 0.2.98.

## Problem

Grok exposes a built-in `ask_user_question` tool while Codeg also injects the
blocking `codeg-mcp__ask_user_question` tool. The names describe the same user
intent but use different transports.

Conversation `019f6960-7bec-7ed3-b434-5252953ea0e9` demonstrates the failure:

- Grok selected its built-in tool (`x.ai/tool.namespace = grok_build`).
- The tool emitted a `tool_call` with a valid `questions` array.
- With Grok's `always-approve` mode, execution was approved immediately.
- No Codeg `QuestionRequest` or tool result followed. The turn remained blocked
  until it was cancelled roughly 19 minutes later.

The existing transcript card deliberately treats an in-flight question tool as
a compact read-only marker because the interactive card is owned by Codeg's
`QuestionRequest` flow. Since the built-in call never enters that flow, there is
neither an auto-open answer card nor a useful manual answer path.

The superseded design assumed Grok's built-in question ended its turn and could
be answered by the next normal prompt. The recorded Grok 0.2.98 behavior
contradicts that assumption, so Codeg must not depend on it.

## Decision

Codeg will remove Grok's built-in `ask_user_question` from every ACP launch and
leave Codeg's MCP question tool as the only structured question mechanism.

The Grok root command will include:

```text
grok --no-auto-update --disallowed-tools ask_user_question [--always-approve] agent stdio
```

`--disallowed-tools` is a root-level Grok flag, so it must be inserted before
the `agent stdio` subcommand alongside the existing `--no-auto-update` and
conditional `--always-approve` flags. Grok 0.2.98, the version pinned by Codeg,
advertises and accepts this flag.

The flag is applied regardless of permission mode. It is also applied when the
Codeg question feature is disabled: disabling that feature should remove
structured question UI, not silently fall back to a Grok-native prompt that
Codeg cannot answer.

## Resulting Flow

1. Codeg launches Grok with the built-in question tool disabled.
2. Grok discovers `codeg-mcp__ask_user_question` through its injected MCP
   server when it needs a user-owned choice.
3. Grok invokes the MCP tool through its normal `use_tool` envelope.
4. The Codeg companion registers a `QuestionRequest` and blocks the MCP call.
5. The frontend renders the existing interactive `AskQuestionCard` above the
   composer.
6. `acp_answer_question` resolves the pending request and returns the structured
   result to Grok, which continues the same turn.

No frontend question component, answer formatting, or transcript parser needs
to change.

## Scope

In scope:

- Grok ACP launch argument construction.
- A focused regression test proving the disable flag and argument order.
- Comments in the Grok registry/launch path that document the name collision.

Out of scope:

- Translating a native question answer into a normal user prompt.
- Retrofitting already-cancelled historical calls.
- Making an in-flight read-only transcript marker interactive.
- Changing Codeg's MCP question schema, state, or answer endpoint.

Existing Grok connections must be restarted before the new launch arguments
take effect.

## Error Handling

- Other agent launch commands are unchanged.
- Grok's existing `always-approve` behavior remains conditional on the user's
  setting.
- If a future pinned Grok version removes or changes `--disallowed-tools`, its
  launch/preflight verification must be updated with that version bump rather
  than silently restoring the incompatible native tool.
- If the MCP question feature is unavailable, Grok may ask in assistant text;
  Codeg will not expose the incompatible built-in tool as a fallback.

## Testing

Add a focused Rust unit test around Grok root launch flag construction. It must
prove:

- `--disallowed-tools` and `ask_user_question` are present as adjacent
  arguments;
- the pair occurs before `agent stdio`;
- `--always-approve` remains present only when selected;
- non-Grok argument construction is unaffected by the helper/change.

After the focused test passes, run the relevant backend test target, `cargo
check`, and Clippy for the desktop feature set specified by `AGENTS.md`.

## Acceptance Criteria

1. Every newly spawned Grok ACP process disables the built-in
   `ask_user_question` tool.
2. Grok can still discover and call `codeg-mcp__ask_user_question` when the
   Codeg question feature is enabled.
3. A Codeg MCP question automatically renders the existing interactive card
   and resolves through `acp_answer_question`.
4. Grok's `always-approve` setting and all non-Grok launch behavior remain
   unchanged.
5. The regression test fixes the launch argument order so a future refactor
   cannot move the root flag after `agent stdio`.
