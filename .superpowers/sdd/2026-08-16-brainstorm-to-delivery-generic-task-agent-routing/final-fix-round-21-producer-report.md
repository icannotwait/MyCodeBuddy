# Final Fix Round 21 Producer Report

## Status

- Starting commit: `21401f42a993024fefc97b984c11196928e2dd74`
- Ending commit: `816dbe92e62e5cd6d4cd8f7026fe4af4b18791de`
- Branch: `codex/b2d-generic-task-agent-routing`
- Starting state: exact requested HEAD, with no tracked changes.
- Result: all three Important findings fixed in one commit. The two accepted
  Minor findings remain deferred and unchanged.

## Important 1: Admitted High-Task Route Binding

### Root cause

The high-Task implementer key is intentionally generation-invariant:
`task|N|implementer|codex|none`. The later-generation adoption check treated
an admitted run on that key as proof that the current Plan/progress generation
had been adopted. Rewriting the Plan and progress from generation 1/Grok to
generation 2/Gemini therefore left the Codex implementer run apparently valid,
even though it was admitted under the old auxiliary route.

### Fix

- Added `task_agent_generation` to every routed progress run fixture and to
  the operational Progress JSON shape.
- The Skill now requires the generation to be recorded with reserving intent
  and forbids rewriting it after admission.
- Routed progress validation requires every run's generation to match the
  Task's derived Plan generation.
- Same-key lineage validation freezes the generation alongside key, Agent,
  and profile.
- Historical later-generation adoption now requires the admitted implementer
  run itself to carry the boundary route's generation.

Because the authoritative route is derived from the referenced immutable
generation, this binds the admitted high implementer to the Task Agent
auxiliary identity/profile that existed at admission without changing the
canonical implementer key.

## Important 2: CommonMark Backtick Fence Info Strings

### Root cause

Both shared fence openers recognized any run of at least three backticks or
tildes without inspecting the info string. CommonMark rejects a backtick fence
opener when the remainder of its opening line contains a backtick. The
JavaScript visible-prose filter and Rust unfenced-comment extractor therefore
hid content that CommonMark renders as visible prose.

### Fix

- JavaScript `fenceStart` now rejects only backtick openers whose info-string
  remainder contains a backtick.
- Rust `markdown_fence_start` applies the same byte-level rule.
- Aligned tests prove that `````info`bad`` does not open a fence, while a
  normal backtick info string and a tilde info string containing a backtick
  remain valid fences.

## Important 3: Causative `lets it restart`

### Root cause

`stateSegmentHasExplicitNonTaskSubject` searched for an `it` object only after
the reactivation predicate. In `lets it restart`, the causative object occurs
immediately before `restart`, so the parser treated `server` as the explicit
reactivation subject and lost the Task reactivation.

### Fix

The state-relation path now recognizes an `it` immediately governed by a
causative purpose verb such as `lets` before the reactivation predicate. When
there is no closer explicit non-Task antecedent, the object remains bound to
the Task. The direct regression rejects the active-Task Agent switch; the
`lets itself restart` inverse remains accepted as a server-only restart.

## Strict RED Evidence

All new regressions were added before any production edit.

### Node RED for all three Important findings

Command:

```text
node --test --test-name-pattern='round-22' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Exit: `1`

Exact result summary:

```text
tests 3
suites 2
pass 0
fail 3
cancelled 0
skipped 0
todo 0
```

Exact intended failure evidence:

```text
round-22 treats a backtick-bearing backtick info string as visible prose
AssertionError [ERR_ASSERTION]: expected B2D-SKILL-005; got

round-22 binds causative restart objects to the active Task
expected rejection: The active Task is completed but the server, monitoring the Task, lets it restart and it is still running. Then switch the Task Agent.

round-22 rejects rewriting an admitted high Task to a new Task Agent generation
AssertionError [ERR_ASSERTION]: expected B2D-ROUTING-007; got
```

### Rust RED for the shared fence parser

Command, with default `tauri-runtime` disabled:

```text
cargo test --no-default-features --features server,test-utils --lib acp::delegation::workflow::simple_parse::tests::simple_parse_routing_applies_commonmark_backtick_info_rule -- --exact --nocapture
```

Exit: `101`

Exact intended failure evidence:

```text
thread 'acp::delegation::workflow::simple_parse::tests::simple_parse_routing_applies_commonmark_backtick_info_rule' panicked at src/acp/delegation/workflow/simple_parse.rs:937:9:
assertion failed: parsed.routing.is_some()
test acp::delegation::workflow::simple_parse::tests::simple_parse_routing_applies_commonmark_backtick_info_rule ... FAILED
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 4613 filtered out
```

The build also emitted the existing macOS linker warning that `__eh_frame`
was too large for compact unwind offsets. It did not cause the RED failure.

## GREEN and Full Verification Evidence

### Focused Node GREEN

Command:

```text
node --test --test-name-pattern='round-22' .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Exit: `0`

```text
tests 3
suites 2
pass 3
fail 0
cancelled 0
skipped 0
todo 0
```

### Focused Rust GREEN

Command, with default `tauri-runtime` disabled:

```text
cargo test --no-default-features --features server,test-utils --lib acp::delegation::workflow::simple_parse::tests::simple_parse_routing_applies_commonmark_backtick_info_rule -- --exact --nocapture
```

Exit: `0`

```text
running 1 test
test acp::delegation::workflow::simple_parse::tests::simple_parse_routing_applies_commonmark_backtick_info_rule ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4613 filtered out
```

The same non-fatal macOS `__eh_frame` linker warning was emitted. No broad
Rust suite was run, as required.

### Complete Node validator suite

Command:

```text
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Exit: `0`

```text
tests 307
suites 4
pass 307
fail 0
cancelled 0
skipped 0
todo 0
```

### Production validator

Command:

```text
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
```

Exit: `0`

```text
PASS: brainstorm-to-delivery Simple contract
  SKILL.md line count: 420

0 failures, 1 checks completed
```

### Formatting, syntax, and diff checks

Commands:

```text
pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
node --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs
node --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
node --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
git diff --check
git diff --cached --check
```

All exited `0`. Prettier reported:

```text
Checking formatting...
All matched files use Prettier code style!
```

The syntax and diff checks produced no diagnostics.

## Files Changed

- `.agents/skills/brainstorm-to-delivery/SKILL.md`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- `src-tauri/src/acp/delegation/workflow/simple_parse.rs`

This ignored report is intentionally outside the commit.

## Self-Review

- The final commit contains exactly the four expected tracked files.
- The high implementer key grammar remains unchanged; the fix adds
  admission-bound generation evidence instead of weakening route derivation.
- Legacy markerless progress remains readable because generation binding is
  required only when routed Plan/progress agreement is enforced.
- Rust remains a warning-only, non-authoritative Simple projection; only its
  shared Markdown fence recognition changed.
- The CommonMark change is restricted to backtick fences. Tilde fences still
  allow backticks in their info strings.
- The causative fix is limited to an object pronoun directly governed by a
  causative purpose verb, and the explicit reflexive inverse remains accepted.
- An initial formatting probe included `SKILL.md` and exposed its existing
  intentionally compact JSON formatting. Unrelated formatter churn was
  removed; the established JavaScript Prettier check is clean.
- `git diff --check` and the staged scope check were clean before commit.

## Minor Findings Disposition

1. `separate Task helper` false-positive: deferred unchanged. It is fail-closed
   and was not inseparable from the causative-object correction.
2. Isolated failed/canceled projection-locality coverage: deferred unchanged.
   No projection-locality production path was changed, and no broad Rust test
   expansion was made.

