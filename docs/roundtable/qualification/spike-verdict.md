# P00 bounded feasibility spike verdict

Verdict: `blocked_platform`.

This document is not a qualification certificate. `qualification_issued=false`. It does not unblock G1. `g1_unblocked=false`. A later Linux run in which all four critical chains pass would still not issue a certificate. Choosing a different adapter or candidate requires a new decision; this spike does not make that choice.

`can_probe=false`. `model_request_count=0`. Endpoint compatibility is `not_tested`. No crun install, image download, container start, credential read, or model/network request was performed. `spike/` is disposable and is not imported by the product.

## Observed environment

| Field | Value |
| --- | --- |
| os_build | Windows 11 Pro 10.0.26200.9457 |
| kernel | 10.0.26200.9457 |
| crun_version | not_installed |
| crun_sha256 | unknown |
| image_digest | unknown |
| adapter_sha256 | unknown |
| codex_sha256 | unknown |
| node_sha256 | unknown |
| mcp_sha256 | unknown |

`os_build` comes from Node `os.version()` plus the NT version in `cmd.exe /c ver`. `kernel` is that Windows NT version, not a Linux kernel. `crun_version=not_installed` is the result of `where.exe crun` finding nothing; crun was not started and its bytes were not hashed. The other digests were not measured. Unknown is explicit. Caller-supplied digests are not copied into these fields.

The gate process was host Node `v24.14.0` on `win32/x64`. That version is not `node_sha256` and is not the candidate image's Node.

## Four critical chains

All four are `blocked_platform`. None is `passed`.

| Chain | Covers | Verdict |
| --- | --- | --- |
| rootless_process | rootless crun create/list/reap; namespace/cgroup | blocked_platform |
| model_gateway | Codex custom base_url | blocked_platform |
| companion_injection | codeg-mcp mounted socket | blocked_platform |
| native_tool_boundary | shell/fs escape | blocked_platform |

`blocked_platform` means this Windows host cannot execute the Linux candidate chain. It is not a finding that the chain fails on Debian 12, and it is not a pass.

## Per-case evidence

No case has an executed command. `command_sha256` is null and `exit_status` is null for every case. `actual_rejection` is `not_executed`. `planned_command_sha256` hashes only the unexecuted template in `spike/fixtures/cases.json`.

| Case | Status | command_sha256 | exit_status | expected_rejection | actual_rejection |
| --- | --- | --- | --- | --- | --- |
| rootless_crun_create_list_reap | blocked_platform | null | null | false | not_executed |
| namespace_cgroup_isolation | blocked_platform | null | null | true | not_executed |
| codex_custom_base_url | blocked_platform | null | null | false | not_executed |
| codeg_mcp_mounted_socket | blocked_platform | null | null | false | not_executed |
| shell_fs_escape | blocked_platform | null | null | true | not_executed |
| endpoint_compatibility | not_tested | null | null | false | not_executed |

Block reasons:

- rootless crun create/list/reap: Windows 11 Pro 10.0.26200.9457 (win32/x64) cannot run rootless crun create/list/reap. `where.exe` did not find crun. No image was downloaded and no container was started.
- namespace/cgroup: win32 cannot prove Linux user, mount, pid, or network namespaces, or cgroup limits. The isolation probe was not executed.
- Codex custom base_url: the rootless Linux sandbox path was not executed on win32. Host model credentials were not read and were not mounted.
- codeg-mcp socket: codeg-mcp was not launched and no Unix socket was mounted. win32 cannot host that candidate socket.
- shell/fs escape: shell startup and native fs escape were not executed. A shell starting would not by itself be a failure. Escape, egress, and tool bypass were not marked passed.
- endpoint compatibility: `not_tested`. Model-request approval is absent, and no model or network request was sent. ChatGPT account login stays `not_tested`. `auth.json` was not read.

## Untested

- `endpoint_compatibility` — no approved real model request, so compatibility was not measured and was not fabricated as a pass.

## G1

The four critical chains are not feasible on this host, so this spike does not unblock G1 and does not authorize the sandbox-backed qualification path. Protocol and other non-sandbox work is outside this verdict. P08 and any real Debian sandbox test stay blocked until a Linux candidate host exists.
