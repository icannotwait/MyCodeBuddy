# Roundtable product/evidence remediation, 2026-10-06

Base: `15313e62d3e76b8ad03932c7dd2766bf2e0deb75`. Rebased without conflicts; the upstream runtime, diagnostics and qualification changes are preserved.
Scope: F07, F19, F20 frontend/HTTP, F21. Implementation is prepared; Rust and React verification remains blocked by missing tools/dependencies. This is not a claim that the complete suite or real runtime qualifies.

## Revalidated findings and changes

- **F07 confirmed.** Public create had no evidence capture and the new-room UI always used empty source refs. `CreateRequest.selected_source_paths` is an optional explicit list. Omission/empty preserves topic-only behavior. The trusted service resolves `workspace_id` through the existing nondeleted regular `folder` record, never accepts an arbitrary root, and captures at most 32 individually selected files, 1 MiB each, 4 MiB combined. No workspace enumeration or Git command occurs. Explicit files use the new `selected` source class so no Git-status claim is invented.
- The capture runs before draft insertion; room, manifest, generated config refs and immutable projection commit together. A retry of the same command returns the stored response without rereading changed files. Database rollback can retain unreferenced content-addressed objects rather than deleting potentially shared objects; a 256 MiB object-storage ceiling bounds retained data. Automatic object GC is not implemented (`maintenance.rs` refuses it), so repeated database failures can exhaust capture capacity. Recovering that space requires separately reviewed maintenance that preserves referenced objects; this change does not provide such a workflow. Metadata-budget rejection now happens before source-object writes, avoiding retention for that rejection class. There is no partly captured room. The redirected room loads its persisted, server-generated config before the second preflight.
- Frozen names, sizes, hashes and actual text preview are shown before execution. Preview reads the immutable source object through authenticated paged object reads, verifies offsets/identity/size and the final hash, and never rereads the workspace. User source text is escaped by React and displayed verbatim rather than passing through the model-output redaction component. Binary files are labeled unavailable to text tools. Initial selection is a local-capture confirmation; starting requires a second confirmation of frozen data and resolved recipients.
- Snapshot reads now use descriptor-relative nofollow traversal on Unix, including all workspace ancestors, with `O_NONBLOCK` so FIFO replacement cannot block before opened-handle type checks. The public workspace-capture command fails closed on Windows with `source_capture_unqualified`. The library's full-path Windows helper is not qualified against in-place reparse mutation; no containment claim is made for it. Hard-link/type checks use the actual open handle. File reads are limited to `min(file limit, remaining total limit)+1`, with metadata checks before allocation and after reading. Changes exceeding the limit cannot cause unbounded `read_to_end` allocation. Symlinked workspace ancestors now fail closed; The Windows denial regression has not run locally.
- Source manifest, question and conservative per-file alias metadata must fit `input_byte_limit` and the strict parser ceiling during create, update and preflight. This complements the full-plan and exact runtime checks in the protocol and owned-runtime paths. File limits do not imply that every 32-file selection fits the smaller configured context budget.
- **F19 confirmed.** Moderator selection uses `participant.ordinal`, not array position. Editing a legally permuted config also sorts seats by ordinal and looks up saved model/effort by identity.
- **F20 confirmed.** The accompanying live-runtime change maps actual provider failures to `CapabilityUnqualified/provider_credential_missing`. This branch adds an actionable provider-settings explanation, HTTP/domain tests with a valid app token, and a transport regression that preserves app-token/WS state on HTTP 422. The production execution gate stays disabled. The HTTP test exercises preflight/read authentication, not a real paid resume; combined runtime-unit/HTTP/transport coverage must not be described as a live end-to-end provider test.
- **F21 confirmed.** Confirmation now shows resolved provider ref, origin, model, agent, effort, member/moderator identity and frozen sources. It does not substitute role labels for resolved recipients. Incomplete disclosure blocks confirmation-dependent actions. Changing configuration/selection invalidates the local confirmation. Stale asynchronous preflight results cannot enable actions for a different configuration key. Definitive paid-command rejections require reconfirmation. Network/unknown-outcome failures retain the entire serialized request, including its original UUID, revision and confirmation token, for exact replay; another paid intent is blocked until that outcome is recovered.
- Start, resume, restart-current and synthesis retry validate the principal, room, revision, requested config, frozen sources, capability/certificate hash and expiry. Paid controls carry the confirmed snapshot across cleanup and revalidate it immediately before successor dispatch. Optional DTO fields preserve parsing compatibility; actual paid commands now require a matching confirmation. Explicit fake test commands retain legacy control behavior when the optional confirmation is omitted, and validate supplied confirmations.

## Added regressions

Rust (`roundtable_transport`):
- `product_create_selected_files_freezes_only_registered_workspace_evidence`: starts from public command dispatch, selects one of two files, proves immutable bytes after workspace mutation and idempotent retry, rejects unsafe/duplicate/overquota paths, unknown workspace and metadata overflow without another draft
- `product_create_moderator_uses_ordinal_not_participant_position`
- `product_resume_rejects_changed_recipient_or_certificate_after_confirmation`
- `product_http_provider_failure_preserves_authenticated_application_access`

Rust (`roundtable_protocol_io`):
- `capture_rejects_oversized_file_before_reading_or_invoking_hook`
- `capture_rejects_a_symlink_in_workspace_ancestor`
- `capture_reads_pinned_file_when_parent_is_replaced_by_symlink`
- `capture_fifo_replacement_never_blocks_before_type_validation` (bounded test-thread timeout)
- Existing symlink, hard-link, FIFO, socket, unstable-read, immutable-bytes and rollback cases retained

Frontend:
- Resolved provider/model/origin/agent/effort and source hash/text disclosure
- Explicit selection passed to create and changed selection invalidation
- Legally permuted draft editing preserves ordinal identity
- Immutable source paging and content-hash verification
- Provider credential explanation and application session preservation

## Verification performed

- `git diff --check`: passed at review time
- All 10 edited translation files parsed as JSON
- Installed Node v24.19.0 native TypeScript parser: API/types/API tests/transport tests parsed with transform mode (not type checking)
- Native-Node execution of the actual source-preview API, with only its transport import stubbed: 5 assertions passed (UTF-8 success, incorrect content rejection, oversized reference rejection, two-page UTF-8 assembly and provider-domain explanation). The full Vitest run is still pending

## Blocked / never-run checks

- Test-first regression definitions preceded production edits, but RED/GREEN could not be established: `cargo` is absent from PATH, and frontend dependencies are absent
- `pnpm test ...` triggered pnpm's dependency-status installer automatically and failed immediately creating `/home/agent/.local/share/pnpm`; no dependency install completed. No further package installation was attempted while approval remained pending
- Rust check/test/clippy/rustfmt, Vitest, TypeScript project check, ESLint, Next build, and Windows runtime validation have not run
- Native parser strip mode rejected an existing TypeScript parameter-property syntax in the transport test; transform mode parsed it successfully. Native parser does not support TSX, so component parsing/type checking is not claimed
- New i18n keys have English fallbacks in eight non-English catalogues; Simplified Chinese and English are supplied. Existing translations are preserved

Suggested verification after prerequisites are available:

```
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features server,test-utils --test roundtable_transport product_
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features server,test-utils --test roundtable_protocol_io roundtable_snapshot
pnpm test src/components/roundtable src/lib/roundtable src/lib/transport/web-transport.test.ts
pnpm test
pnpm lint .
pnpm build
```

Integration requirements: include live-runtime credential/domain changes, resolved capability recipient fields, model persistence, the exported `validate_interjection_context` protocol helper, write-transaction serialization and local cleanup sequencing. This product change is not independently feature-complete without those changes. Preserve both confirmation and local cleanup logic in `product.rs`. No model call, credential injection, cloud task, execution-policy enablement, merge, deployment or push was performed.


## Follow-up correctness fixes

- Paused-room preflight permits only a prospective concurrency change. The room revision, all other config fields and the resulting capability/source snapshot remain bound. Resume validates and persists exactly that prospective config; changing concurrency again after confirmation is rejected without dispatch or mutation
- Paid-request replay now uses `src/lib/roundtable/mutation.ts`. Fresh preflight IDs do not create new intent identity. Unknown outcomes retain a deep copy of the original serialized body. The pending-operation retry button works after projection/preflight changes and resends that body; a new paid intent remains blocked until resolution; safe Pause/Stop controls remain available
- Late preflight responses are discarded by generation, in addition to configuration-key checks
- Complete selected-source coverage now includes create, saved-config reload, immutable object preview, second preflight and controlled fake start. Additional definitions cover changed concurrency, expired confirmation, certificate change during asynchronous cleanup, metadata rejection before object writes, clone rejection under reduced metadata budget, failed-create retained-object quota, Windows public-capture denial and paid ACK replay after a new projection/confirmation
- Encoded input-array admission now uses `roundtable_protocol::validate_interjection_context` before accepting a new interjection, and before preflight/start/resume/restart/successor execution. UUID/object overhead can no longer bypass the preflight reservation through many tiny inputs; raw-text quotas remain unchanged
- Native Node execution of the actual dependency-free mutation helper: 10 assertions passed for exact replay across new revision/token, per-room navigation retention, different-intent blocking and definitive-versus-uncertain errors. This is not React/Vitest or Rust verification
- Rust, React/Vitest, project typecheck, formatting, lint and build remain unrun locally. Existing PR CI is the verification route; no local dependency installation was performed in this correction pass


### Reproducible local checks after corrections

- `node scripts/roundtable-product-smoke.mjs`: passed on Node v24.19.0; 16 paid-request replay/navigation/cancellation assertions and 5 immutable-preview/provider-error assertions. Native TypeScript stripping emits its standard experimental warning. The source-preview transport I/O is stubbed; no provider/network call occurs
- `git diff --check`: passed
- `for file in src/i18n/messages/*.json; do python3 -m json.tool "$file" > /dev/null; done`: passed for all ten catalogues

CI still needs to execute the Rust regression targets and React/Vitest/project checks listed above. Locally defined tests are not recorded as passed until that evidence exists.

- Unresolved paid requests are retained in memory by workspace/room across client-side room navigation, projection refresh and re-preflight. The registry clears only for that request's definitive rejection or acknowledgment, and an older completion cannot clear a newer request. A full document reload or tab close clears this in-memory registry; the UI states that boundary and asks the operator to recover the outcome first. This is bounded idempotent replay, not a global exactly-once guarantee, and no private request is added to persistent client storage


### Cancellation during an unresolved paid request

- `prepareRoundtableMutation` now permits `roundtable_pause` and `roundtable_stop` while a paid acknowledgment is unresolved. These controls use separate request bodies and do not replace the original paid record in the room registry
- Cancellation acknowledgments and definitive rejections preserve the original paid record and pending-retry UI. The paid retry button explicitly reads that registry; cancellation retries retain their own current body. Another paid intent remains blocked until the original paid outcome is recovered
- Added helper regressions for both controls and parameterized the UI lost-paid-ACK/projection/preflight/navigation test to send Pause or Stop before verifying exact original-body replay
- RED: `node scripts/roundtable-product-smoke.mjs` exited 1 with `paid_outcome_unknown` at the newly added Pause preparation assertion before the fix
- GREEN: the same command exited 0 after the fix, with 16 replay/navigation/cancellation assertions plus 5 source/error assertions. `git diff --check` passed. Native TypeScript parsing passed for the mutation helper/test; React/Vitest and project type checking still require CI
