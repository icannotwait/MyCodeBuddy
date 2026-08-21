# ACP fixtures

Sanitized wire shapes for unit tests. Replace if a live probe differs.

| File | Source | Notes |
| --- | --- | --- |
| `grok_auto_compact_completed.json` | Design live probe Grok **0.2.98** (provisional) | Private compact completed; method dual-accepted in mapper |
| `grok_autonomous_session_3806.jsonl` | Synthesized session-3806 shape (redacted) | Idle `task_completed` + hidden reminder + thought/message/tool + `turn_completed` |
| `grok_autonomous_monitor_completion.jsonl` | Sanitized Grok **4.6** task-wake shape | Dedicated live task carriers + persisted Monitor completion + `will_wake` + prompt-id terminal |
| `codex_goal_autonomous_two_cycles.jsonl` | Synthesized Codex CLI **0.146.0** / codex-acp **1.4.0** (redacted) | Foreground terminal + two Goal cycles with `rs_*` / `msg_*` ids |
