# Delegation Route Reliability

## Behavior Change

Codeg delegation remains disabled by default. When it is enabled, new Codex,
Grok, CodeBuddy, and Claude Code root connections default to the Codeg route,
which suppresses that platform's native sub-agent creation surface. Existing
connections keep their launch route until explicitly reconnected.

Codeg-created children remain on Codeg routing and keep the existing depth
limit semantics. A quiet child may be shown as stalled, but the soft watchdog
does not cancel or fail it.

## Rollback

Set Multi-agent routing to Native to use platform-native sub-agents on new
managed connections, or disable Codeg delegation entirely. Reconnect each
running connection to apply the change. No schema rollback is required; the
new nullable/additive conversation columns may remain in place.

## Pinned Smoke Matrix

| Platform | Pinned version | Codeg route check | Native route check |
| --- | --- | --- | --- |
| Codex | CLI 0.144.1 | `features.multi_agent=false`; Codeg tools listed | no Codeg override; Codeg delegation hidden |
| Grok | 0.2.98 | `--no-subagents` before `agent stdio`; Codeg tools listed | flag omitted; Codeg delegation hidden |
| CodeBuddy | 2.118.2 | `--disallowedTools Agent Task`; Codeg tools listed | Codeg denies omitted; Codeg delegation hidden |
| Claude Code | 2.1.205 / ACP 0.58.1 | `_meta` denies `Agent`,`Task`; Codeg tools listed | Codeg denies omitted; Codeg delegation hidden |

For every row, verify root safe fallback is visible, a managed Codeg child does
not fall back, a post-launch companion exit reports delegation unavailable
without changing route, and the mixed-route invariant counter remains zero.

## Scope Boundary

Other Agent types keep their existing behavior and are not included in the
hard mutual-exclusion claim. Native activity is observational and remains
owned by the native platform.
