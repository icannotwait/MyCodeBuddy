# Codex Official npm Native Suppression Design

**Status:** Approved in conversation on 2026-07-20

## Problem

The managed Codeg route currently sets `CODEX_ACP_MULTI_AGENT=0` for Codex.
The official `@agentclientprotocol/codex-acp@1.1.2` process does not consume
that private variable, so Codex still receives its native collaboration tools
and can call `collaboration.spawn_agent` instead of Codeg's
`delegate_to_agent`.

The official adapter already supports `CODEX_CONFIG`: it parses the value as a
JSON object and merges it into the App Server configuration used by
`thread/start` and `thread/resume`. Codex's supported feature switch for native
subagents is `features.multi_agent=false`.

## Goals

- Suppress Codex native collaboration tools on managed Codeg routes while
  continuing to use the official npm `codex-acp` package.
- Preserve every unrelated user-provided `CODEX_CONFIG` key, including sibling
  feature flags.
- Keep Native routes byte-for-byte neutral: they must not add, remove, or force
  a multi-agent setting.
- Fail closed with `NativeSuppressionInvalid` when a managed Codeg route cannot
  safely merge the official configuration.
- Cover absent, existing, conflicting, and malformed configuration with focused
  Rust tests.

## Non-Goals

- Supporting `CODEX_ACP_USE_CLI=1`. Official 1.1.2's CLI Runtime bypasses the
  App Server session config path; this known limitation is intentionally left
  unchanged.
- Editing `~/.codex/config.toml` or any other persistent user configuration.
- Changing Grok, CodeBuddy, or Claude route suppression.
- Changing route resolution, safe fallback policy, or the Codeg MCP companion.

## Selected Approach

At the existing process-route application boundary, replace the Codex-specific
write of `CODEX_ACP_MULTI_AGENT=0` with a structured merge into the child
process's `CODEX_CONFIG` environment value:

```json
{
  "features": {
    "multi_agent": false
  }
}
```

The merge operates on the complete per-launch environment after distribution,
provider, and user settings have been combined. It is therefore connection
scoped and reaches the official npm adapter without modifying user files.

## Merge Contract

1. If `CODEX_CONFIG` is absent, start from an empty object.
2. If present, parse it as JSON and require a top-level object.
3. Preserve all existing top-level keys.
4. If `features` is absent, create an object.
5. If `features` is present, require an object and preserve all sibling keys.
6. Set `features.multi_agent` to `false`, overriding any prior value on the
   managed Codeg launch only.
7. Serialize the merged object back into `CODEX_CONFIG` without logging its
   contents.
8. Invalid JSON, a non-object root, or a non-object `features` value returns
   `AcpError::RouteUnavailable { reason: NativeSuppressionInvalid }`.

Native and unrelated route plans do not parse or rewrite `CODEX_CONFIG`; even
malformed input remains untouched because no suppression is requested.

## Alternatives Rejected

### Persistent `config.toml`

Setting `[features] multi_agent = false` in the user's Codex home would disable
Native collaboration globally and prevent route switching from restoring the
user's native behavior.

### Continue the private environment variable

`CODEX_ACP_MULTI_AGENT` only works in the retired MyCodeBuddy adapter fork. It
does not satisfy the requirement to run the official npm package.

### Fork or patch the npm adapter again

The official `CODEX_CONFIG` hook already supplies the required App Server
configuration surface, so another fork would add distribution and upgrade
cost without adding capability for the supported mode.

## Testing

Focused unit tests at the Rust process-route boundary will prove:

- absent `CODEX_CONFIG` becomes an object containing
  `features.multi_agent=false`;
- existing top-level and feature keys survive the merge;
- an existing `multi_agent=true` is forced to `false` on Codeg routes;
- invalid JSON, non-object roots, and non-object `features` fail with the typed
  suppression error;
- Native routes preserve configuration byte-for-byte;
- non-Codex suppression plans do not touch `CODEX_CONFIG`.

Verification will run the focused route tests, the relevant Rust test target,
format checking, and desktop `cargo check`.
