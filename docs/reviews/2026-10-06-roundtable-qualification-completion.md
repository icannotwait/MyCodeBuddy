# Roundtable qualification: repaired claims and remaining implementation

Scope: F02, F03 and the qualification evidence needed by F14 in the PR #37 repair.
The repair preserves upstream `a470c4aeedd3c6f8e8c999e8d4ec47b37b171870`.
Probe findings below include the separately reviewed isolation repair, which
reports unmeasured capabilities as `not_tested` rather than issuing false passes.

## Decision

Correcting false evidence and refusing unknown capacity are necessary bug fixes.
They do not implement usable qualification. Running the current smoke probe on
another host cannot fill the missing production experiment, credential boundary
or request-envelope measurement. Do not describe all qualification functionality
as complete, synthesize a passed certificate, or enable the product gate.

## Concrete evidence and missing functionality

### F02: receipt, drain and product visibility

- `qualification_linux::run_acp` accepts and holds a socket; its session helper
  sends `Reply with the single word pong.`. That cannot demonstrate a validated
  `submit_result`, a returned durable receipt, or product completion/drain
- `qualification_harness::QualificationHarness` uses `FakeBrokerTransport` and
  `InMemoryToolStore`. Its `deliver_receipt_to_cli` only records a receipt in
  memory; `certificate_status` correctly returns `NotTested`
- The repaired host probe leaves `submit_receipt_completion`, `roundtable_mcp`,
  product privacy/discovery observations and other unmeasured checks untested
- `companion/transport.rs::ServiceBroker::bind` requires `GateToolAuthority`;
  `mcp.rs::GateToolAuthority::begin_admission` invokes the execution gate for
  tool calls. `feature_gate.rs::ExecutionGate::check` requires a passed
  certificate even for `ExecutionScope::Qualification`

The missing feature is an authorized, non-product experiment that drives the
actual adapter, companion, broker, durable store, completion boundary, private
event/discovery observers and cleanup. Replacing the socket holder alone does
not solve its admission dependency. Existing component privacy tests, such as
`private_ingress::private_sink_receives_raw_events_only`, are useful controls
but cannot be relabeled as observations of that complete experiment.

### F03: credential boundary

`sandbox/linux_oci.rs::apply_auth_mounts` binds native auth files into the adapter
sandbox or copies Grok auth into its attempt-local home. Adapter profiles require
these files. Read-only binds expose their contents. General slirp egress remains
explicitly recorded as `slirp-egress-not-origin-filtered`.

`installed_runtime.rs` requires both `model_credential_material_in_sandbox` and
`model_credentials_visible_to_agent` to be false. The repaired probe correctly
reports material present and unknown native-tool visibility. Denied ACP requests,
absent credential environment variables or a host-path canary do not establish a
separate native-tool trust boundary.

The missing feature is host-held credentials with an enforced gateway route for
compatible adapters, or a genuinely separate authentication/tool execution
boundary. The supported-adapter contract must be decided before implementation;
changing the meaning of the certificate fields to bless the current shared
container would not preserve the existing guarantee.

### F14: admission versus evidence production

The integrated `owned_runtime.rs::validate_plan_context` is called by preflight
and room startup, covers selected seat profiles and moderator phases, reserves
future context growth and refuses an unknown request envelope. The integrated
`live_runtime.rs::verified_request_envelope_bound` reads a bound from the exact
verified report bytes. Existing scheduler tests cover late context growth,
missing envelope evidence and changed confirmation-bound proof.

The repaired `qualification_probe.rs::render_report` emits an unmeasured
`request_envelope`. There is no production measurement producer yet. Its future
implementation must cover the adapter's final request serialization and enforce
the measured domain. A short ACP prompt size or arbitrary nonzero evidence hash
is insufficient; evidence must be traceable to the observed bounded request path.

## Safe test-only increment

Only `src-tauri/tests/roundtable_cases/roundtable_mcp.rs` and this memo change.
The new tests use real local `ServiceBroker`, real `DurableToolStore`/SQLite and
the production dispatcher. Every authority uses `ExecutionScope::Fake`,
`QualificationStatus::NotTested` and gate-disabled assertions.

1. `socket_receipt_survives_disconnect_after_durable_commit`: invalid submission
   has no seal; a valid submission pauses after SQLite commits but before its
   broker reply. The test reads and verifies the persisted receipt and canonical
   payload through an independent database query, disconnects the client and
   cancels the paused broker handler before its in-memory ledger records the
   seal. A new broker/authority/token then retries through the durable store;
   receipt identity matches and a changed payload is `result_already_sealed`.
   Exactly one candidate remains staged, never accepted. The invalid fixture is
   structurally valid and refers to an unavailable evidence alias
2. `socket_stop_waits_for_the_actual_admitted_handler`: stop captures a real
   admitted evidence read; releasing that call drains the production handler.
   Later submission is refused without a seal. Shutdown produces socket EOF
3. `socket_shutdown_cancels_and_drains_a_blocked_handler`: a real admitted read
   remains paused. Shutdown must cancel and join the handler, with zero pending
   handlers and client EOF, without manually releasing the pause or calling
   `complete_inflight()`

The forwarding store wrapper only inserts deterministic scheduling pauses and
otherwise delegates storage operations to `DurableToolStore`. It does not fake
receipts, validation, SQL or handler-drain results. These assertions cover
production component contracts; they do not establish live adapter, provider,
native-tool, visibility, process-tree or host qualification.

## Minimum remaining stages

1. Run the focused socket tests and required Rust/CI checks on the integrated
   repair SHA. Passing these only establishes their component contracts
2. Decide a separate non-product-convertible qualification authority, bound to
   exact binaries/configuration, fixture, recipient, expiration, durable
   attempt/spend accounting and cancellation. Keep product admission unchanged;
   do not bootstrap by providing fake `Passed` facts
3. Decide and implement the credential boundary. A gateway-only approach must
   establish adapter compatibility and enforce that route, with no raw account
   auth inside the model-tool sandbox. A split-domain approach must demonstrate
   the independent isolation boundary. General egress plus readable auth cannot
   satisfy the current absence/unreachability certificate
4. Implement evidence collection through production plan construction, durable
   broker receipt, completion/drain, private visibility and cleanup. Capture the
   actual request envelope. Probe actual native tools using synthetic credential
   canaries, with positive controls proving the observation paths are live
5. Obtain separate authorization for bounded host validation: exact host/profile,
   image, provider/account, fixture data, attempt/time/spend limits and credential
   handling. Collect both positive and negative traces; issue a certificate only
   when its measured requirements hold. Gate enablement and merge remain
   separate actions

No provider call, real credential injection, security setting, dependency install,
external cloud task, product gate change or merge is performed by this increment.

## Verification

- Passed: `git diff --check` and source-scope/invariant checks. These only check
  whitespace, changed-file scope and explicit fake/non-certifying test setup
- Independent static review found a missing `RtResult` import and a mismatch
  between durable-store and wire-level conflict names; both were corrected. It
  also prompted the fresh-authority restart so replay cannot use the old ledger
- UNRUN: focused Rust tests, Rust compilation and formatting, because `cargo`,
  `rustc` and `rustfmt` are absent in this executor. The attempted focused test
  command exits 127 with `cargo: command not found`; no RED/GREEN is claimed
- Correct focused command:

  ```sh
  cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
    --features test-utils --test roundtable_protocol_io roundtable_mcp::socket_ \
    -- --nocapture
  ```

`roundtable_mcp.rs` belongs to `roundtable_protocol_io`, not `roundtable_runtime`.
Required broader verification remains the repository's documented Rust/CI suite
against the final integration SHA. Static review does not substitute for it.
