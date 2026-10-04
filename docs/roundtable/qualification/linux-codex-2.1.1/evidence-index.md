# linux-codex-2.1.1 qualification evidence

Verdict: `blocked_platform`. This file is not a certificate. `qualification_issued=false`. `g1_passed=false`. G1 is not passed.

No live experiment ran. Nothing was installed, logged in, or sent to a model. The ignored Rust entry `linux_codex_2_1_1_live_experiment` is not a pass. A fake broker or a fake FD is not a certificate. P08 did not execute Cargo and did not hash candidate binaries.

`host_os_version` is copied from `docs/roundtable/qualification/spike-verdict.md` (`Windows 11 Pro 10.0.26200.9457`). P08 did not measure it again. The candidate remains `linux` / `debian-12` / `codex-acp@2.1.1`. This Windows host cannot run that candidate, so the profile is `platform_blocked`.

## Not measured

Binary paths, versions, and sha256 values are null. `image_digest`, policy hash, tool-contract hash, core hash, plan hash, and the execution-tree hash are null. Shared-core hashes are null for `canonical`, `delivery_encoder`, `validator`, `tool_core`, `request_accounting`, and `ingress`. Probe input and output hashes are empty. No experiment clock was taken (`observed_at` is null). No approval was present.

`ordinary_sidebar_imports`, `global_body_events`, `model_credential_material_in_sandbox`, `model_credentials_visible_to_agent`, `actual_resolved_binary_matches_certificate`, `idle_processes_after_reap`, and `normal_completion_after_submit_receipt` are null. Null is not a measured zero and not a pass.

## Checks

| Check | Status |
| --- | --- |
| new_session | not_tested |
| roundtable_mcp | not_tested |
| ordered_turn_completion | not_tested |
| cancel_and_reap | not_tested |
| strict_isolation | not_tested |
| bounded_context_delivery | not_tested |
| private_events | not_tested |
| sidebar_discovery | not_tested |
| global_body_events | not_tested |
| credentials_unreachable | not_tested |
| api_credential_scope | not_tested |
| chatgpt_account_scope | not_tested |
| actual binary match | not_tested |
| endpoint_compatibility (P00) | not_tested |
| native_read_boundary (P00) | not_tested |
| companion_lifecycle | not_tested |

P00 endpoint compatibility and native read were not executed. They are `not_tested`, not an estimated pass and not rewritten as a failure. API credentials and ChatGPT account authentication are recorded separately; neither was tested. ChatGPT account login stays `not_tested`.

The machine record is `report.json`. Tokens, environment values, `Authorization` headers, and other private material are not stored.
