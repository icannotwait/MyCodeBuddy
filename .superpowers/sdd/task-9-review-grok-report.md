# Task 9 Review — Grok (HIGH dual reviewer)

- **Work unit:** Independent Task 9 HIGH reviewer (Grok)
- **reviewed_task_id:** `291c8192-3f96-43f6-b031-2b5511f7f8ee`
- **Producer code commit:** `bd011e818cec86c543744abf07df7f0e8c3ff6f5`
- **HEAD tip:** `180218a488d3fb95a61f9cc649fc2a7885b37932`
- **Plan:** `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md` — Task 9
- **Implementer report:** `.superpowers/sdd/task-9-report.md`
- **Reviewer:** Grok
- **Mode:** code review only (no implementation)

## Verdict

**`approve`**

**Ready to merge: Yes**

Task 9 removes the frontend legacy restart API, completion-protocol settings/rollout surface, and v1 workflow mutation affordances while retaining historical projection links, read-only copy, ordinary conversation deletion, valid v2 completion decisions, automatic root-wake status, and full 10-locale key validity.

No Critical, Important, or blocking Minor findings.

## Spec compliance (Task 9 only)

| Requirement | Status | Evidence |
| --- | --- | --- |
| Delete `restartLegacyWorkflow`, `getCompletionProtocolSettings`, settings snapshot DTO | Pass | Removed from `src/lib/api.ts`; `LegacyWorkflowRestartProjection` removed from `types.ts` |
| Delete web-transport restart mapping / root-capability restart path | Pass | `restart_legacy_workflow` out of `COMPLETION_MUTATION_COMMANDS`; `completionContextByRoot` and restart branch gone; transport tests expect `501 not_implemented` without capability replay |
| Remove rollout/settings status from delegation settings | Pass | Load is only `getDelegationSettings` + `getDelegationProfileCatalog`; `CompletionProtocolStatus` deleted |
| Make v1 graphs strictly read-only (no restart/resume/settle/recovery UI) | Pass | `legacyReadOnly = version === 1` suppresses root/node `CompletionDecisionCard`; restart/resume buttons, pending state, and protocol error UI removed |
| Keep historical links + read-only notice | Pass | `read_only_reason`, `legacy_source`, `v2_successor` still render with open-conversation handlers |
| Keep `CompletionProtocolWorkflowProjection` required fields | Pass | `version`, `mode`, `creation_mode`, links, `read_only_reason`, `automatic_root_wake` remain on the TS interface |
| Keep valid v2 decisions + automatic wake | Pass | v2 path still renders `CompletionDecisionCard` and `completionAutomaticWake`; overlay test submits `resolveCompletionDecision` and shows auto-wake copy |
| Keep ordinary conversation deletion outside graph mutations | Pass | `deleteConversation` API retained; `api.test.ts` asserts `delete_conversation` transport contract |
| Remove obsolete translations in all 10 locales; retain historical/v2 keys | Pass | Restart/manual-resume/rollout keys gone; retained four keys present in all locales; messages.test asserts them |
| 10 locale JSON valid + key parity | Pass | Independent parse of all 10 files OK; parity vs `en.json` (4281 keys each) |
| Plan dual-review focus: FE restart/settings/rollout gone | Pass | Plan-exact forbidden-symbol search over `src` has zero matches |

### Removal / retention map

```text
REMOVED
  api: restartLegacyWorkflow, getCompletionProtocolSettings,
       CompletionProtocolSettingsSnapshot
  types: LegacyWorkflowRestartProjection
  transport: restart_legacy_workflow mutation membership,
             completionContextByRoot + restart capability lookup
  settings: CompletionProtocolStatus, rollout load/format helpers
  graph: restart/resume buttons, pending state, protocol-error alerts,
         root/node completion mutation cards when protocol version === 1
  i18n: completionLegacyRestart, completionManualRootResume,
        completionDefaultMode/Creations/Samples/Minimum/Override,
        completionShadowDifference, completionRolloutDecision.*

RETAINED
  CompletionProtocolWorkflowProjection
    (version/mode/creation_mode/links/read_only_reason/automatic_root_wake)
  historical source/successor link buttons + read-only notice
  v2 CompletionDecisionCard + automatic_root_wake status copy
  deleteConversation / delete_conversation ordinary delete path
  10-locale retained historical + automatic-wake keys
```

## Independent verification

Re-ran on this worktree at HEAD `180218a4` (producer `bd011e81` + SDD report tip):

| Command / check | Result |
| --- | --- |
| `pnpm test --` focused 5 files (overlay, delegation-settings, api, web-transport, messages) | **pass** — 5 files, **88** tests, 0 failures |
| Plan-targeted `pnpm eslint` on production FE files | **pass** — no errors |
| Plan-exact forbidden-symbol `rg` on `src` | **no matches** |
| Required projection fields + retained locale keys | **present** (4 retained keys × 10 locales) |
| Locale JSON parse (all 10) | **pass** |
| Locale key parity vs `en.json` | **pass** (4281 keys each) |
| `git diff --check` on producer range | **clean** |
| Producer file allowlist | **20 files**; plan Files lists 19 + justified `src/i18n/messages.test.ts` |

### Focused test evidence (this review)

- v1 snapshot with both history links: read-only notice + link buttons present; restart/resume/Done/Retry artifact/`completion-decision-card` absent; supplied `onResumeRoot` untouched.
- v2 snapshot: automatic-wake copy renders; `Done` submits `resolveCompletionDecision` with CAS; resolved state appears; manual resume absent.
- Delegation settings: only delegation settings + profile catalog requested.
- Transport: removed commands reject as unknown without capability header.
- API: ordinary `delete_conversation` contract retained.
- Locales: retained historical/v2 control keys required by messages parity test.

## Strengths

1. Clean end-to-end FE removal: API DTOs, web transport capability path, settings panel, graph mutations, and all 10 locale packs land in one coherent producer commit (`bd011e81`).
2. TDD shape matches the plan: tests flip from restart/settings assertions to read-only/v2 retention and unknown-command rejection before production deletion is trusted.
3. Retention is explicit, not accidental — historical links/read-only copy, projection fields, v2 decision cards, automatic wake, and ordinary delete remain wired and covered.
4. v1 mutation suppression is complete at the graph surface: both root and per-node completion cards are gated, not only the restart button.
5. Scope stays frontend-only; no backend Task 10+ files in the producer commit.

## Findings

| id | severity | title | blocking |
| --- | --- | --- | --- |
| — | — | No Critical, Important, or Minor findings | — |

### Notes (non-findings)

- Producer includes `src/i18n/messages.test.ts`, which is outside the plan **Files** list (19 listed production/locale/test paths → 20 committed). Justified: plan Step 1/5 require locale retention assertions; absence/parity gates need the updated required-key set. Not a defect.
- `WorkflowGraphPanelProps` still declares unused `conversationId` and `onResumeRoot`, and `SubAgentOverlay` still threads them. Functional resume/restart UI is gone as required; residual prop plumbing is cosmetic dead surface, not a Task 9 acceptance failure.
- Manual-resume i18n deletion (Step 5) confirms the plan intends resume affordance removal globally, not only under v1; dual-review “v2 unchanged” is satisfied by retained decision cards + automatic wake, not by the removed manual-resume control.
- Vite CJS API deprecation warning during Vitest is pre-existing tooling noise outside this diff (also noted by the implementer).
- Full `pnpm eslint .` / `pnpm test` / `pnpm build` matrix is Task 11 territory; this review re-ran the plan-named focused suite and gates only.

## Scope notes

- Code commit `bd011e81` implements Task 9 frontend restart/settings/rollout removal only.
- Tip after code (`180218a4`) is SDD implementation report only.
- Task 10 aggregate/backend absence inventory is intentionally not started.
- No production code was changed by this review.

## Conclusion

**approve** — Task 9 fully removes frontend restart, settings, and rollout surfaces; keeps historical projection/links/read-only copy; keeps ordinary delete and valid v2 decision/auto-wake behavior; keeps all 10 locale files valid and key-aligned; and passes independent focused verification. Ready for Task 10.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"approve","summary":"Grok HIGH review: Task 9 removes FE restart/settings/rollout; v1 graphs stay read-only with history links; v2 decisions and auto-wake remain; delete remains; 10 locales valid. Ready to merge.","commits":[{"sha":"bd011e818cec86c543744abf07df7f0e8c3ff6f5","subject":"refactor: remove legacy completion controls"}],"tests":{"status":"passed","passed":88,"failed":0,"summary":"Independent re-run: 5 focused FE/locale files, 88 passed; ESLint, absence/retention gates, locale parse/parity clean."},"concerns":[],"report_file":".superpowers/sdd/task-9-review-grok-report.md","reviewed_task_id":"291c8192-3f96-43f6-b031-2b5511f7f8ee","findings":{"critical":0,"important":0,"minor":0},"ready_to_merge":true}
-->
