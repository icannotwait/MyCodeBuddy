# Roundtable contract remediation

Latest upstream retained: `a470c4aeedd3c6f8e8c999e8d4ec47b37b171870`

The shared live/probe ACP exchange, lenient transport-frame parsing, explicit filesystem/terminal denial, permitted roundtable MCP tools, auth-copy deletion, run-directory handling, and upstream regressions are retained. Structured result payloads still use the strict protocol parser.

Status: implementation and regression tests prepared. Rust compilation, Rust tests, and rustfmt are blocked because the development environment has no Rust toolchain. This report does not claim that the Rust changes compile or pass. No model call, credentials injection, OCI container, execution-gate enablement, deployment, push, or merge was performed.

## Findings revalidated and changes

- **F06:** The production prompt contained a one-sentence result instruction and the broker exposed only `type: object`. Added complete versioned member/moderator JSON Schemas in the validation module; production delivery, service schema text, MCP `tools/list`, and qualification tool hashing share that schema function. Enum values and quota limits use validator types/constants. Field/required/type parity is maintained by tests; this is not an automatically derived schema. Dynamic ownership, alias, coverage and consensus constraints remain authoritative in the result validator and are documented in the delivered contract. Existing persisted/public DTO shapes are unchanged.
- **F06 identity and aliases:** Exact prompt bytes now include host-owned speaker ID, ordinal and that speaker's alias-based mandatory targets. Immutable public phase hashing is preserved. Frozen context includes a contentful claim/response alias catalog, not only opaque IDs. Production scheduler tests derive member results from the schema delivered in the prompt, verify moderator alias text, and stage results through the actual scoped tool store.
- **F08:** The shared production/probe `acp_exchange` now classifies AIR `sessionFailure` on updates and responses before returning a successful result. Error/unknown severity, malformed declared failure envelopes, JSON-RPC errors, and abnormal stop reasons fail closed. Warning-only records can finish normally. The shared initialize handshake preserves explicit filesystem/terminal denial and adds the sealed `sessionFailure` capability metadata. Exact approved roundtable tool identities retain one-shot approval; native-operation kinds, misleading titles and unrelated metadata cannot grant tool permission. The exact `use_tool` wrapper may name a scoped Roundtable capability in its validated `rawInput` object. If `allow_once` is unavailable, the request is rejected rather than granting an unqualified persistent `allow_always` option; the upstream fallback assertion and operator guide are intentionally updated. The existing execute-turn cleanup runs before an RPC error propagates and before any sealed candidate can become an accepted completion.
- **F14:** Admission and room execution check the required initial request for every future phase, member and moderator against the selected profile's request-body limit. Canonical result objects are counted without another sixfold string escape; the contentful alias catalog and bounded host wrappers are accounted separately. Optional inputs retain their raw-text quota and gain a separately reserved encoded-context ceiling, checked before acceptance and frozen delivery. An explicit hash-verified request-envelope byte proof is mandatory; missing/invalid evidence is unknown, never zero, and byte/evidence-report changes bind the confirmation limits hash. Within-attempt optional tool/generated growth is still subject to the actual request-body cap; preflight does not promise all independent maxima fit simultaneously. Preflight and actual `DeliveryEncoder` share `admit_qualified_delivery`: the larger of the admitted request-body token bound and the prompt plus result/evidence reserve, followed by hidden-token and generation reserves exactly once. Small profiles that cannot satisfy the existing delivery reserve are rejected before confirmation. All selected profiles are used in delivery and the limits hash. The generic token-bound fallback refuses witnesses above 16 MiB; the live byte-only proof needs no witness allocation.
- **F15:** Auth-file reads use a no-follow, nonblocking Unix open followed by descriptor metadata checks and stop at 65,537 bytes. They reject symlink/FIFO, oversized, unreadable, non-file, malformed JSON or unsupported UTF-8 data rather than silently losing redaction coverage. The denylist includes JSON string values and their JSON-escaped forms, plus the original file. This conservatively handles token-only and reordered output across adapter JSON formats. Streaming capture holds possible longer matching secrets across chunks and redacts interrupted overlapping prefixes at EOF. Rejected ACP/result payload logging now retains only bounded safe field/shape metadata, or a redacted marker and byte count for malformed input, rather than attempting to recognize short or unquoted credentials heuristically. This is retention hardening; it does not prove credential isolation from the model process.
- **F18:** Slot classification consults failed/absent status before the placeholder `Abstain` kind. Coverage takes the latest discussion phase, ignoring the empty synthesis member list. A production scheduler regression stages a failing third seat, publishes the two-member quorum, synthesizes, and checks `(valid, absent, abstained, invalid) == (2, 1, 0, 0)` before and after synthesis.
- **F20:** Both live capability and execution paths use a shared credential-readiness guard returning `CapabilityUnqualified/provider_credential_missing`, never application `Unauthenticated`. A fixture covers this guard with a unique absent environment key. Router and frontend coverage of domain error mapping/session preservation is separate from this runtime guard.
- **F21:** Resolved recipients include ordinal and effort. Runtime resolves model by participant ordinal, and persists only the display `model_id` so completed replay does not keep `default`; the requested configuration/model snapshot stays unchanged.

## Default bound and optional-input ceiling

For the unchanged three-seat, one-critique-round configuration with 8,192-byte results and 16,384-byte input/interjection quotas, there are six member results. The exact reserved canonical future portion is:

- result bodies plus the contentful catalog: `2 × 6 × 8,192 = 98,304`
- bounded alias/ID/message wrappers: `6 × ((20 + 30) × 192 + 1,024) = 63,744`
- initial source/clone metadata allowance: `4 × 16,384 = 65,536`
- admitted encoded optional-input context: `6 × 16,384 + 64 = 98,368`
- phase/seat metadata allowance: `4,096 + 512 × 3 = 5,632`
- total: **331,584 canonical bytes**

Let `K` be the exact canonical role/schema/config bytes returned by `preflight_exact_prompt`, and `E` the separately qualified complete request-envelope byte bound. The required request bound is **`663,168 + 2K + E`**, checked against the unchanged **1,048,576-byte** profile cap. It does not use a token count as `E`. The default fixture test computes and prints the exact `K`, fake-encoder `E`, and total for all three phase roles and asserts that the unchanged defaults fit. Its execution is blocked by the missing compiler, so no observed final total is claimed here.

Optional input admission checks the same canonical `[{input_id,text}]` array that will be delivered. Its encoded ceiling is `6 × raw_text_quota + 64`; the original raw-text ceiling remains separately enforced. A single maximum-escaped input still fits. Many tiny inputs may hit the encoded ceiling first and are rejected before insertion, without truncation. Persisted inputs are checked again before paid continuation. This avoids assuming that a text-only quota also bounds thousands of UUID wrappers.

The certificate field is `request_envelope: {status, max_bytes, evidence_hash}`. The live reader requires a passed bound within the profile cap, a valid non-placeholder evidence hash, and the exact verified report hash. Both the bound and verified report hash enter each seat’s confirmation limits hash. Missing/not-tested evidence produces unknown capacity. Current diagnostic qualification cannot produce this proof and must leave the field unavailable. Controlled fixture evidence is explicitly synthetic and does not qualify a live adapter.

## Round-one review corrections

- Declared AIR failures now share the classifier’s exact envelope/parser domain. If `sessionFailure` is present but cannot be decoded by `air_session_failure`, classification is incompatible instead of empty. Update and response regressions cover `version=9223372036854775808`, including already-staged durable candidates; version-1 warning completion remains a success control.
- Preflight and delivery now call the same qualified token-admission helper. With the unchanged default profile the shared admission is `max(1,048,576, K + 331,584 + 245,760) + 8,192`, which the default fixture verifies as **1,056,768 tokens**, below 2,000,000.
- The supplied small-profile counterexample retains request cap **196,608**, model capacity **204,800**, generation reserve **8,192**, output quota **8,192**, evidence quota **32,768**, input quota **2,048**, interjection quota **1**, and an explicit synthetic **4,096-byte** envelope. Its canonical future reserve is **67,398** and required request is **138,892 + 2K**. Tests verify this request fits, then require preflight rejection because the shared delivery reserve alone is **253,952 tokens before prompt bytes**. No capacity is increased and no attempt starts.
- Actual model-gateway failures now produce an attempt-local, monotonic fatal observation. The production forwarding path records policy, transport, upstream status and incomplete-response errors; router/body-extractor HTTP rejections are also observed. After gateway drain, live completion checks that observation before reading the sealed candidate. Owner revocation/shutdown is explicitly nonfatal and does not erase an earlier failure. Diagnostic finish reason reflects a drained gateway failure.
- Real loopback gateway regressions cover HTTP 400, incomplete HTTP 200, unsupported-route 404 and oversized-body 413, followed by actual ACP `end_turn` decoding and the production post-drain gate. A durable scoped-submission scheduler test verifies failed gateway completion accepts no message; a successful gateway/cancellation control completes normally. These are local fake-provider tests, not live-model evidence.
- Focused RED attempts still stop before discovery: `cargo` is absent (exit 127). These regressions have not been executed locally; shared lock parity and whitespace checks pass.

## Round-two race and upstream reconciliation corrections

- Phase-pinned MCP `submit_result_input_schema`, delivered result schemas, and compact prompt examples now use one authoritative `result_schema` implementation. Removed the parallel schema builder. The retained upstream shape/semantic submission budgets and nested validation diagnostics still apply. The synthesis example is validator-accepted (`inference: true` for its empty evidence list). Field-schema parity remains manually maintained and tested, not derived automatically from Rust types.
- Retained the upstream Grok native-tool session denylist, lenient ACP frames, and at most two denied-tool cancellation repairs. A `use_tool` permission is accepted only when its authoritative identity is exactly `use_tool`, its input contains only `tool_name` and object-valued `tool_input`, and the name is an exact scoped Roundtable tool. Conflicting machine names, malformed or unscoped wrappers and native-operation kinds are rejected. Unqualified `allow_always` remains refused.
- The production prompt-repair loop checks the attempt-local fatal gateway observation before every prompt and after each response. A fatal session failure exits before a cancelled-turn repair. Controlled tests exercise successful bounded repairs, exhausted repairs, warning-only carriers, terminal failures, durable staged candidates and actual gateway failures; a prior gateway failure sends no repair prompt.
- HTTP failure observation exempts only a typed owner-revocation response, rather than ignoring every error after the owner starts draining. Router and body-extractor rejections latch before returning their responses. A coordinated loopback test delays completed 404/413 responses across revocation and requires the fatal completion gate to reject them.
- The expiry predicate no longer conflates the separate revocation flag with lease expiry; absolute expiry and the shared execution lease remain enforced. A controlled clock revokes between admission checks and verifies normal cleanup is nonfatal. A typed owner-I/O marker survives the real body extractor and identifies interrupted owner cancellation without suppressing genuine HTTP errors.
- The upstream stream path classifies an already-yielded body error before examining revocation. A truncated loopback response followed by owner revocation must retain `upstream_transport`; the existing partial-body owner-cancel test remains a nonfatal control.
- Auth-file opening uses `O_NOFOLLOW | O_NONBLOCK` on Unix before `fstat`, so a FIFO cannot block before the byte ceiling applies. FIFO and symlink regressions have bounded waits and unblock the pre-fix reader before assertion. Platforms without a qualified equivalent fail closed for auth-file reads; environment-based redaction behavior is unchanged.

Focused commands attempted for this correction, each blocked before compilation by missing `cargo` (exit 127):

- `cargo test --manifest-path src-tauri/roundtable-protocol/Cargo.toml --test result_contract`
- `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --lib completion_drain_tests`
- `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --lib permission_cancel_repairs_are_bounded_and_cannot_hide_terminal_failure`
- `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --lib auth_fifo_and_symlink_are_rejected_without_waiting_for_a_writer`
- `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime staged_candidate_then_actual_rpc_failure_never_accepts`

These tests use synthetic credentials and controlled local transports. No live-provider, adapter or real-host qualification is inferred. Rust formatting and execution remain unverified; run the listed tests and formatter in the existing Rust verification environment before treating this delta as verified.

## Added regression coverage

Protocol crate:

- full schema nested member/moderator fields and accepted examples
- schema property names versus validator allowlists
- fixture-corpus shape parity, omitted versus null optional fields, all member confidence/stance/priority values, required moderator fields
- host-owned same-role speaker identity and filtered alias targets
- long-witness allocation refusal
- one maximum-escaped input and boundary/many-tiny-input context ceilings without truncation
- synthesis coverage and failed/absent placeholder semantics

Application paths:

- exact delivered schema/identity/catalog assertions in the existing controlled production scheduler test
- short plan admissible, late history growth rejected before execution
- every selected seat profile checked
- unchanged three-seat/one-round defaults fit the explicit fake envelope
- missing, malformed, tampered and changed envelope-byte evidence
- durable scoped submission followed by real prompt RPC error, with no accepted messages
- final coverage after a failed third seat and successful synthesis
- shared live/probe exchange update/response error carriers, malformed envelopes, abnormal stop and warning controls
- exact permitted tool identities versus native-operation names and misleading title/argument/metadata content
- short, unquoted and malformed credential-bearing rejected-payload logs
- bounded auth-file extraction, value-only/reordered/split/EOF/overlapping secrets
- live auth denylist with mounted JSON and no environment seed
- missing provider credential remains a capability error
- resolved model display persistence

The prompt-RPC tests exercise the exact production RPC reader through in-memory I/O. They do not launch the full OCI/session-new transport or constitute real-host/live-adapter qualification.

## Verification evidence

Executed:

- `git diff --check`: passed
- `node scripts/check-roundtable-locks.mjs`: passed, `shared lock packages match (35)`
- UTF-8 decoding of edited Rust source: passed (not a syntax or type check)

Attempted, blocked before test discovery (exit 127, `/bin/bash: cargo: command not found`):

- protocol strategy regression
- protocol result-contract schema regression
- protocol delivery context regression
- diagnostic unit tests
- live completion unit tests
- production runtime scheduler/delivery regression
- full-plan preflight regression
- staged-candidate/terminal-failure regression

No Rust RED/GREEN cycle has been observed. Tests were added before the corresponding production changes where feasible, but an unavailable compiler cannot establish that they fail for the intended reason. Some later coverage extends the already-prepared fix and likewise remains unexecuted.

Required once a toolchain is available:

1. `cargo fmt --manifest-path src-tauri/Cargo.toml --check` and protocol crate formatting
2. `cargo test --manifest-path src-tauri/roundtable-protocol/Cargo.toml`
3. `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features test-utils --test roundtable_runtime`
4. application `--lib` tests for `roundtable::diagnostics::tests`, `roundtable::live_runtime::completion_contract_tests`, and `roundtable::owned_runtime::plan_context_contract_tests`
5. integrated server/desktop checks and relevant full suites against the final repair commit

## Integration and qualification limits

- Preserve execution-lease and mandatory cleanup persistence guarantees when integrating the shared runtime changes.
- `prepare_turn` receives an executor reference to select the seat's qualified profile.
- Qualification schema hashes now call `service_result_schema()`.
- No dependency, Cargo lock, public request DTO, or stored input shape change is introduced.
- Missing request-envelope byte evidence fails closed. Existing token-count bounds are not interpreted as byte bounds.
- The diagnostic qualification report must leave request-envelope evidence unavailable until an actual production adapter measurement exists. A fake test envelope does not qualify a real adapter.
