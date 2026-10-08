# Debian 13 roundtable qualification

A roundtable turn runs only after this host has a passed certificate for
every adapter in the room and `execution-policy.json` allows those
certificates. The probe writes a certificate only when every check on
this machine passed. A missing isolator, a failed escape, a hash
mismatch, or a failed ACP turn is a failing report and no certificate.

## Current qualification limitation

The probe can issue a certificate only when every required check is measured
on this host. Sandbox credential isolation is deferred for this iteration:
the ACP container bind-mounts the host auth files the CLI already reads
(`~/.grok/auth.json` for Grok; Antigravity `settings.json` with
`auth.type` such as `oauth-personal`, plus `acp_token.json`). Those
files are not copied into the attempt home. Antigravity does not receive
`AGY_*` gateway variables. `AGY_ACP_CCPA_BASE_URL` sends
`fetchAvailableModels` to the loopback relay, which does not serve that
API, and `session/new` then fails with connection refused. With the host
oauth files mounted and no `AGY_*` variables, model listing and the
prompt use the same oauth client and leave through slirp.
`endpoint_compatibility` for Antigravity accepts that provider origin.
Grok still uses the loopback gateway. Each gateway request records its
method, path, and upstream status in the probe trace. The four
credential-boundary checks are `not_applicable` and do not block the
certificate. A non-empty `auth_mounts` list still fails the report. The
sandbox also receives a random attempt bearer and, for adapters that use
it, gateway variables pointed at `http://127.0.0.1:39173/v1`. Grok
1.0.46 reads `XAI_API_KEY` and `GROK_XAI_API_BASE_URL`. Codex still
reads `OPENAI_BASE_URL` and `OPENAI_API_KEY`. A rejected `session/new`
trace includes the ACP error `code`, `message`, and `data.details`. The
host gateway
accepts the attempt bearer from `Authorization: Bearer` or
`x-goog-api-key` / `x-api-key` / `api-key`, strips `key` query
parameters, and swaps in the host-held credential before the request
leaves the machine. Slirp cleanup still signals every pinned helper.
A same-uid process that denies `/proc/<pid>/environ` is ignored only
after a short retry, and only when it is not `slirp4netns` and not a
descendant of the cleanup process. Probe sockets live under the temp
dir so a long `--data-dir` cannot exceed the 107-byte `sun_path` limit;
a path that still does is `socket_path_too_long`. A Grok OIDC session
(`~/.grok/auth.json`) is forwarded to `cli-chat-proxy.grok.com` with the
CLI session headers,
and refreshed on the host. An `XAI_API_KEY` in the server environment,
with no session file, is forwarded to the binding origin without those
headers. The sandbox does not receive `CURSOR_API_KEY`.
Each qualification open uses its own database under
`qualification-runs/`, so grok and antigravity can be measured in one
`--data-dir`.

The production experiment binds the real broker and durable store under a
qualification-experiment authority. That authority requires
`certificate=not_tested`, expires, and fixture limits. It does not authorize
product scope and it does not enable `execution-policy.json`. `roundtable_mcp`
passes only after the broker seals one `submit_result`. Receipt, bounded
evidence, private ACP frames, sidebar hide, and global-body counts are read
from that same run. A smoke `pong` is not a certificate.

`request_envelope` stays `not_tested` until a host gateway records a real
model request body. Antigravity's model call leaves over slirp and is not
visible to that gateway, so a fresh certificate does not invent a `passed`
bound and does not fill one with the ACP prompt size. Live preflight admits
that unmeasured certificate by reserving 65536 wrapper bytes outside the
canonical prompt, on top of the twofold JSON escape, and still refuses the
room when the sum exceeds the profile request-body cap. A later `passed`
bound replaces the reserve. Do not edit a report to waive a failed check.

This repository build does not run `crun`. A passing unit test of the broker
is not a live adapter certificate. Re-run `roundtable-qualify` on the Debian
host after installing this build.

Profiles accept Debian 12 and Debian 13 as profile parameters. The
issued key still pins the OS, kernel, and architecture that were
measured. A Debian 12 certificate does not verify on Debian 13.

`spike/probe.mjs` is not this probe. It never sets
`qualification_issued` to true.

## Profiles

| Agent | Profile id | Binary inside the rootfs | Version text must contain | Auth files (host-held, not mounted) |
| --- | --- | --- | --- | --- |
| Codex | `linux-codex-2.1.1` | `/usr/local/bin/codex-acp` | `2.1.1` | `.codex/auth.json` |
| Grok | `linux-grok-1.0.46` | `/usr/local/bin/grok` with args `--no-auto-update agent stdio` | `1.0.46` | `.grok/auth.json` |
| Cursor | `linux-cursor-acp-2026.09.28-64d2043` | `/usr/local/bin/cursor-agent` with arg `acp` | `2026.09.28-64d2043` | `.cursor/cli-config.json`, plus one of `.config/cursor/auth.json` (XDG) or `.cursor/auth.json` |
| Antigravity | `linux-antigravity-acp-1.3.0` (default) | `/usr/local/bin/agy_acp_server.par` with arg `--uid=` | `1.3.0` | host `settings.json` and `acp_token.json` (no `AGY_*` gateway env) |
| Antigravity | `linux-antigravity-acp-1.2.1` | same binary and arg | `1.2.1` | same files |

`--agent antigravity` selects `linux-antigravity-acp-1.3.0`. Pass
`--profile linux-antigravity-acp-1.2.1` when the image still has 1.2.1.
Do not combine `--profile` with `--agent all`.

Every profile also requires `/usr/local/bin/codeg-mcp` and an ACP
initialize, `session/new`, and prompt inside the isolator. Grok needs
Node.js 20 or newer on the image `PATH`. Cursor and Antigravity are
directory trees: keep each entry's sibling files beside it
(`cursor-agent` with its bundled `node`; `agy_acp_server.par` with
`localharness_external`).

## Host packages and cgroup

```bash
sudo apt-get update
sudo apt-get install -y crun slirp4netns debootstrap
sudo mkdir -p /sys/fs/cgroup/codeg-roundtable/launcher
# Controllers can be enabled only while this directory contains no processes.
echo '+memory +pids +cpu' | sudo tee /sys/fs/cgroup/codeg-roundtable/cgroup.subtree_control
sudo chown -R "$USER:$USER" /sys/fs/cgroup/codeg-roundtable
# crun moves the container into a sibling of launcher. The probe and the
# live server must already be inside a child, or that move returns EPERM.
echo $$ | sudo tee /sys/fs/cgroup/codeg-roundtable/launcher/cgroup.procs
```

Start `codeg-server` from that same shell, or from a systemd unit whose
`Delegate=yes` slice puts the process in
`/sys/fs/cgroup/codeg-roundtable/launcher` before it launches crun. The
probe tries the same move and fails with the command above when the
kernel refuses it. A parent cgroup that still contains the launcher
process is not enough.

`crun` must be executable at the path you pass (default `/usr/bin/crun`).
The cgroup directory must contain `cgroup.controllers`,
`cgroup.events`, and `cgroup.subtree_control` listing `memory`, `pids`,
and `cpu`. Unprivileged user namespaces must already be allowed
(`/proc/sys/kernel/unprivileged_userns_clone` is not `0`).
`slirp4netns` (1.2.x) is the egress helper for the ACP turn. If it is
missing, the turn fails and no certificate is issued. The isolation
container does not start slirp, so it still has only `lo`. The ACP
container's slirp path is general egress: the probe checks that the
provider origin answers on port 443 and records whether `1.1.1.1:443`
also answers. It does not claim an origin-only filter.

The slirp helper is an OCI `poststart` hook. `createRuntime` runs while
container init is still non-dumpable, so `/proc/<pid>/ns/user` does not
exist and slirp exits before it writes the ready byte. The hook reads
that byte and fails unless it is `1`. A watcher compares the container's
PID/start time and holds a pipe open only while that same process lives.
Slirp reads the pipe through `--exit-fd=0` and exits on EOF, so the watcher
never signals a reusable numeric PID. This is the upstream
[slirp4netns exit-fd contract](https://github.com/rootless-containers/slirp4netns/blob/master/slirp4netns.1.md).
The launcher can then finish waiting for its adopted helper. Explicit
cleanup and the hook share a private startup lock and durable lifecycle
state. Cleanup closes that gate before signaling and checks again after the
container and launcher can no longer create hooks. Hook startup requires
successful durable publication of both helper and watcher PID/start-time
pins. Hook, watcher and all their descendants carry per-container owner
markers; cleanup enumerates that owned tree and signals only verified
Linux pidfds. An old plain PID file, missing configured-helper pin or
unprovable ownership leaves cleanup failed rather than signaling another
process. A durable never-started state is distinct from version/isolation/
MCP probes that intentionally configure no slirp helper.

Cleanup durably records an in-progress quarantine before signaling anything,
and writes each retained helper's PID and start time before the signal.
A later call, including startup after a crash, resumes `cleanup-in-progress-*`.
It re-opens those pidfds, signals only a start-time match, and publishes a
proven state when the pins, the persisted set, and a complete census have
all exited. A reused numeric PID is not signaled. A missing retained file
means no descendant was published yet, so a clean census can still prove
the sweep. Legacy `cancelled-*` markers did not persist those obligations
and remain `slirp_cleanup_interrupted`. Do not delete or reset a quarantine
marker by hand; bare saved PIDs are not proof.

The qualification probe and the live room write that hook, the watcher,
the seccomp allowlist, and the resolver through one shared path. The
image's `/etc/resolv.conf` stays an empty regular file. Both paths
bind-mount `<runtime-root>/slirp-resolv.conf` read-only over it. That
file contains `nameserver 10.0.2.3`, which is slirp4netns's DNS proxy
inside the container network namespace. The image digest does not change.
`plan_hash` binds the execution template, including runtime and rootfs
paths. The same paths retain that template hash when only the resolver
contents change. `core_hash` separately binds the shared implementation;
the changes between `2f57bcde4` and `a8ee4042f` therefore invalidate
previously issued certificates. Probe/profile implementation sources are
also bound into new certificates. Changed production or probe semantics,
including host-held credentials in `plan_hash`, require a fresh
qualification on this host. Keep the execution policy disabled.
A different `--runtime-root` or rootfs path also changes `plan_hash` even
when the implementation is unchanged.
The shared-core hash includes the validator, seat prompt, MCP broker and
live ACP client. The `submit_result` JSON Schema is answered by the
codeg-server broker on `tools/list`; the `codeg-mcp` process in the rootfs
only proxies that socket, so a schema-only broker change does not require
a new rootfs. A certificate is issued only when this host measures the
production broker path and the host-held credential boundary. `request_envelope`
stays unmeasured on the certificate. Live admission reserves a fixed wrapper
for that field instead of treating it as unknown capacity.
Without that mount the live container keeps the empty file, so
`auth.x.ai` and `cli-chat-proxy.grok.com` fail with
`dns error: failed to lookup address information` until the attempt
timeout.

`crun run` deletes a container that exits on its own. The probe's later
`crun delete` then fails with `cannot open directory …/state/<id>`.
That still counts as reaped when the container state directory is gone
and its cgroup directory is gone. A delete error while the cgroup
directory remains is not `REAPED`. An ACP `initialize` or `session/new`
failure drops the same cleanup: the `crun run` process is killed, slirp
is stopped, and the container is deleted.

The qualification key stores the first `crun version …` line from
`crun --root <runtime-root>/state --version` (for example
`crun version 1.21`). A bare `crun --version` can print
`Failed to get state directory`, and that text is not the version.

## Rootfs

One rootfs directory can hold every adapter. Do not put credentials,
tokens, or `auth.json` contents in the image or in git. The probe
rejects a placeholder that is missing, non-empty, or a symlink. Auth
files stay on the host. They are not bind-mounted and they are not
copied into the attempt home.

```bash
export ROOTFS=/var/lib/codeg/roundtable-rootfs
sudo debootstrap --variant=minbase trixie "$ROOTFS" http://deb.debian.org/debian
sudo chroot "$ROOTFS" apt-get update
sudo chroot "$ROOTFS" apt-get install -y nodejs ca-certificates
sudo mkdir -p "$ROOTFS/usr/local/bin" "$ROOTFS/proc" "$ROOTFS/scratch" \
  "$ROOTFS/dev/pts" "$ROOTFS/dev/shm" "$ROOTFS/sys" "$ROOTFS/tmp" \
  "$ROOTFS/run/codeg" \
  "$ROOTFS/rt-home/.codex" "$ROOTFS/rt-home/.grok" \
  "$ROOTFS/rt-home/.cursor" "$ROOTFS/rt-home/.config/cursor" \
  "$ROOTFS/rt-home/.gemini/antigravity-acp"
sudo touch "$ROOTFS/run/codeg/roundtable.sock" "$ROOTFS/run/codeg/gateway.sock"
# ACP binds the slirp resolver here. A symlink (systemd-resolved) is rejected.
sudo rm -f "$ROOTFS/etc/resolv.conf"
sudo touch "$ROOTFS/etc/resolv.conf"
for rel in .codex/auth.json .grok/auth.json .cursor/cli-config.json \
  .cursor/auth.json .config/cursor/auth.json \
  .gemini/antigravity-acp/settings.json \
  .gemini/antigravity-acp/acp_token.json \
  .gemini/antigravity-acp/acp_business_token.json
do
  sudo touch "$ROOTFS/rt-home/$rel"
done
# The digest reads every regular file. chown keeps mode bits, so the
# digest does not change when only the owner changes. chmod a+rX also
# makes the tree readable, but it changes modes and therefore the digest.
sudo chown -R "$USER:$USER" "$ROOTFS"
```

The digest (`codeg-rootfs-v2`) hashes symlink text without following it,
including absolute `/etc/alternatives` links and dangling targets. Device
nodes, fifos, and sockets contribute type and device numbers; they are
not opened. A file the probing user cannot read fails the probe and
names the path.

`/proc`, `/dev`, `/dev/pts`, `/dev/shm`, `/sys`, `/tmp`, `/scratch`, and
`/rt-home` are directories. The socket paths, `/etc/resolv.conf`, and
auth placeholders are empty regular files. The probe mounts a tmpfs on
`/dev` (with `/dev/pts` and `/dev/shm`) so rootless crun does not create
device nodes in the image and change the digest. `/sys` is mounted
read-only. `/tmp` is a writable tmpfs. `/rt-home` is a writable scratch
directory for that adapter and that container run
(`home-upper/<agent>/<container-id>-<run>` on the probe,
`scratch/rt-home` on a live attempt). That directory must not receive
auth bytes. The probe removes the scratch directory after the container
is reaped, including when the run fails. A live reap removes
`oci/runs/<incarnation>/scratch` before the attempt is marked confirmed. It
then keeps the eight newest real run directories under `oci/runs/` and
does not follow a symlink out of that directory. A scratch path or an
intermediate home path that is a symlink fails the reap instead of
deleting the link target. Pruning an older directory that cannot be
removed is logged and does not block the attempt whose scratch was
just deleted.

The image root stays read-only. `/dev/mem` and `/dev/sda` are still
denied.

Build `codeg-mcp` and copy that binary only:

```bash
cargo build --release --manifest-path src-tauri/Cargo.toml \
  --no-default-features --features mcp-bin --bin codeg-mcp
sudo cp src-tauri/target/release/codeg-mcp "$ROOTFS/usr/local/bin/codeg-mcp"
sudo chmod 755 "$ROOTFS/usr/local/bin/codeg-mcp"
```

Copy each adapter from the copy Codeg already runs for normal sessions.
Install the pinned package on the host first, then copy the tree into
the rootfs. Example for Grok (Node 20+ must already be in the rootfs):

```bash
npm install -g @xai-official/grok@1.0.46
# Confirm the pin before copying. The probe rejects any other text.
grok --version | grep 1.0.46
sudo cp "$(command -v grok)" "$ROOTFS/usr/local/bin/grok"
```

If `grok` is a Node shim, also copy the package tree it executes and
keep Node at `/usr/bin/node` or `/usr/local/bin/node` inside the image.
Do not copy `~/.grok`.

Cursor (keep the extracted tree intact so the shim finds its `node`):

```bash
# After Codeg has cached the linux package for 2026.09.28-64d2043:
sudo cp -a /path/to/dist-package/. "$ROOTFS/usr/local/cursor/"
sudo tee "$ROOTFS/usr/local/bin/cursor-agent" >/dev/null <<'EOF'
#!/bin/sh
exec /usr/local/cursor/dist-package/cursor-agent "$@"
EOF
sudo chmod 755 "$ROOTFS/usr/local/bin/cursor-agent"
sudo chroot "$ROOTFS" /usr/local/bin/cursor-agent --version | grep 2026.09.28-64d2043
```

Adjust the `exec` path to the real entry inside the archive
(`dist-package/cursor-agent`). Copy the whole archive, not only that
script.

Antigravity (both files must sit in the same directory):

```bash
sudo cp agy_acp_server.par localharness_external "$ROOTFS/usr/local/bin/"
sudo chmod 755 "$ROOTFS/usr/local/bin/agy_acp_server.par" \
  "$ROOTFS/usr/local/bin/localharness_external"
sudo chroot "$ROOTFS" /usr/local/bin/agy_acp_server.par --version | grep -E '1\.3\.0|1\.2\.1'
```

Use `--profile linux-antigravity-acp-1.3.0` (the default) or
`--profile linux-antigravity-acp-1.2.1` to match that text.

Codex, when you still want it:

```bash
npm install -g @agentclientprotocol/codex-acp@2.1.1
sudo cp "$(command -v codex-acp)" "$ROOTFS/usr/local/bin/codex-acp"
```

After the last `sudo cp` into the image, give the probing user read
access without changing modes:

```bash
sudo chown -R "$USER:$USER" "$ROOTFS"
```

Auth stays on the host. The gateway may read these files. The probe and
the live plan both refuse to bind-mount them or copy them into
`/rt-home`. Cursor still has no `CURSOR_API_KEY` injection. A missing
required file fails `api_credential_scope`; it is not replaced with an
invented key.
- `$HOME/.cursor/cli-config.json`
- Cursor's token: `$HOME/.config/cursor/auth.json` or
  `$HOME/.cursor/auth.json`. At least one of those two files must exist
  on the host. The sandbox does not receive either file, and this probe
  does not inject `CURSOR_API_KEY`.
- `$HOME/.gemini/antigravity-acp/settings.json` and
  `$HOME/.gemini/antigravity-acp/acp_token.json` are bind-mounted.
  `settings.json` keeps the host `auth.type` (for example
  `oauth-personal`). The probe does not inject `AGY_*` variables.
  `acp_business_token.json` is mounted when it exists.
- `$HOME/.codex/auth.json` for Codex

## Provider bindings

Write a JSON array. `credential_env` is the variable name, never the
secret. Do not commit this file if it sits next to real tokens; the
sample below has no secrets.

```bash
cat > "$HOME/roundtable-providers.json" <<'EOF'
[
  {
    "provider_ref": "provider:grok",
    "model": "grok-4.6",
    "origin": "https://api.x.ai",
    "credential_env": "XAI_API_KEY",
    "supported_efforts": ["low", "high"]
  },
  {
    "provider_ref": "provider:cursor",
    "model": "composer-2.5[fast=true]",
    "origin": "https://api2.cursor.sh",
    "credential_env": "CURSOR_API_KEY",
    "supported_efforts": ["low", "high"]
  },
  {
    "provider_ref": "provider:antigravity",
    "model": "gemini-3.8-flash-high",
    "origin": "https://cloudcode-pa.googleapis.com",
    "credential_env": "GEMINI_API_KEY",
    "supported_efforts": ["low", "high"]
  }
]
EOF
```

Use the same `provider_ref` and `model` the room will send. The probe
selects the row whose `provider_ref` is `provider:<agent>` (the same
lookup as the origin check). It does not use the first row for every
adapter. Grok 1.0.46's `session/new` advertises `grok-4.6`. Antigravity
1.3.0 advertises `gemini-3.8-flash-high`. Cursor
2026.09.28-64d2043 advertises `composer-2.5[fast=true]` once ACP
authentication succeeds. The probe reads ids from that response
(`currentModelId`, `models`, and `configOptions` whose id contains
`model`) and records the advertised model binding. This alone does not prove
production `session/set_config_option` selection or endpoint compatibility. Do
not invent an id the adapter did not report. Origins must be `https`
with no user, path, or query.
File-auth adapters (Grok, Cursor, Antigravity) keep those files on the host.
The sandbox does not receive them. The host gateway reads the named
environment variable when it is set, and otherwise the host file, and
injects that secret only on the provider side of the gateway. Codex uses
the same host-side path. The value is never copied into the image or the
attempt home. Grok and Antigravity can be certified only when their ACP
process completes the measured production broker path through that gateway.
If a CLI still requires the auth file inside the sandbox, the probe fails
the production checks instead of mounting the file.

## Probe

Build the server binary, then run the probe. The same subcommand works
on the desktop `codeg` binary.

```bash
cargo build --release --manifest-path src-tauri/Cargo.toml \
  --no-default-features --features server --bin codeg-server
export CODEG_DATA_DIR="${CODEG_DATA_DIR:-$HOME/.codeg}"
./src-tauri/target/release/codeg-server roundtable-qualify \
  --agent all \
  --data-dir "$CODEG_DATA_DIR" \
  --rootfs "$ROOTFS" \
  --provider-bindings "$HOME/roundtable-providers.json" \
  --crun /usr/bin/crun \
  --cgroup-root /sys/fs/cgroup/codeg-roundtable \
  --runtime-root "$CODEG_DATA_DIR/roundtable/oci" \
  --home "$HOME"
```

Add `--profile linux-antigravity-acp-1.2.1` only when that exact build
is the one in the rootfs. `--agent` is `grok`, `cursor`, `antigravity`,
`codex`, or `all`.
Exit 0 means every requested adapter passed. Exit 1 means at least one
adapter failed. Exit 2 is a usage error.

Outputs:

- `$CODEG_DATA_DIR/roundtable/qualification/<profile-id>/report.json`
- `$CODEG_DATA_DIR/roundtable/qualification/<profile-id>/traces/`
- `$CODEG_DATA_DIR/roundtable/qualification/<profile-id>/execution-policy.example.json`
  only when that adapter passed
- `$CODEG_DATA_DIR/roundtable/qualified-runtime.json` only when the
  verdict is `passed` (schema 2 catalog; a later failure removes only
  that adapter)

A failing report has `verdict` other than `passed`,
`qualification_issued: false`, and `reasons` on stdout. Do not hand-edit
a report into a pass. The product checks the report hash, the host OS
and kernel, the binary hashes, and the image digest again at launch.

The default production scratch is
`$CODEG_DATA_DIR/roundtable/oci/runs/<incarnation>/scratch`. A dedicated
per-incarnation subtree may live below HOME. Validation must bind it to the
installed runtime root and exact incarnation; mounting HOME, the runtime
parent, a project/decoy/other-attempt subtree, or a symlink alias remains
forbidden. The generic sandbox-plan builder has no HOME exception.

## Execution policy

Keep the execution gate disabled. A qualification experiment is not product
admission, and this probe does not enable the gate. The policy file is:

`$CODEG_DATA_DIR/roundtable/execution-policy.json`

```json
{
  "enabled": false,
  "generation": 1,
  "allowed_qualification_keys": []
}
```

Do not populate this allowlist from synthetic test fixtures or old reports.
A future release with complete measured production evidence will need fresh
certificates and an explicit operator decision to enable execution. The
policy generation and accepted keys must match what the server reads.

## Seat ACP client

Live seats and the qualification probe share one ACP loop. `initialize`
advertises `terminal: false` and filesystem read and write false.
`session/new` does not send a capabilities object. It sets `cwd` to
`/scratch` and mounts only the roundtable MCP server.

`session/request_permission` selects an option from that request.
`submit_result`, `read_evidence`, and `search_evidence`, including
`roundtable/<tool>`, `roundtable__<tool>` and `mcp__roundtable__<tool>`, select only `allow_once`.
If that option is absent, the request is rejected: `allow_always` is not
qualified as confined to the sealed tool and disposable attempt. An explicit
`use_tool` identity may wrap a scoped roundtable tool only when its `rawInput`
contains the exact `tool_name` and an object `tool_input`, with no other keys.
A conflicting machine name or a native-operation kind cannot be overridden by
a display title or argument. Antigravity 1.3.0 is allowed when
`toolCall._meta.is_mcp_tool_call` is true, `_meta.mcp.server` is
`roundtable`, and `_meta.mcp.tool` is one of those three names. Its title
`roundtable_submit_result` (one underscore) is accepted only together
with that meta. The same title without the meta is rejected. A terminal
command whose text contains `submit_result` is rejected. Every other tool,
including `run_terminal_command`, selects `reject_once`, or
`reject_always` when `reject_once` is absent. The reply is
`{"outcome":{"outcome":"selected","optionId":"..."}}`. The client never
sends `outcome: cancelled`. A rejected tool does not finish the attempt
by itself. If Grok still ends the turn with `stopReason` `cancelled`
after a rejection, the same session sends at most two follow-up prompts,
provided no fatal gateway or session failure has been observed:
`Only use roundtable__read_evidence, roundtable__search_evidence, and roundtable__submit_result. Call submit_result now.`
Other stop reasons are not retried. A request with no usable option gets
JSON-RPC `-32601`. Failure observations are checked again after gateway drain
and before a staged candidate may be accepted.

Proposal and critique prompts add an instruction when `context.sources[].entries`
and `context.aliases.evidence` are both empty: do not call `search_evidence`
or `read_evidence`; submit or abstain from the topic and `published_messages`
only; `evidence_aliases` may be `[]`. Those tools then return
`{"empty":true,"aliases":[],"hint":"no frozen evidence; submit without citations"}`
instead of `unknown_alias`. An attempt killed by its `attempt_timeout` records
`finish_reason` and diagnostic `error_class` `attempt_timeout` before the seat
is reaped. That row is not a permission cancel.

`tools/list` publishes the full `submit_result` JSON Schema for the
attempt's phase. Proposal and critique schemas declare `kind`, `summary`,
and `claims` of `local_key`, `text`, `evidence_aliases`, and `confidence`,
with conditional requirements for abstention. Synthesis omits `speaker_id`
and `coverage`. MCP schemas, prompt schemas and examples derive from the same
validator contract. A missing or invalid nested field is
reported at its path, for example `$.claims[0].local_key`. Pure
schema-shape mistakes stay open through 8 submissions and close on the
9th. Alias and consensus failures still close on the 4th.

Grok `session/new` sets `_meta.agentProfile.disallowedTools` to the
native tool catalog (`run_terminal_command`, `read_file`, and the rest).
`use_tool` and `search_tool` stay allowed, because that is how Grok calls
MCP. `maxTurns` and `permissionMode` are not set. grok-cli 1.0.46's
`--disallowed-tools` flag applies to headless mode only, and an empty
`tools` list inherits every tool, so the ACP profile is the denylist
this session uses. Antigravity and Cursor sessions omit that `_meta`.

ACP transport frames use a bounded lenient JSON parser, at most 1 MiB.
A float in a `session/update` does not fail the turn. The strict
no-float parser still applies only to the `submit_result` payload. A
frame or payload that is rejected is logged only as a bounded structural
diagnostic. Arbitrary values are redacted; malformed bytes retain only a
redacted marker and byte count.

## Three-member room

In the Roundtable page, a new discussion starts with three seats:
Grok, Cursor, and Antigravity. Pick the model provider for each seat,
choose which seat is the moderator, run "Check readiness and budget",
confirm, create the draft, then start it. The moderator is that seat's
adapter, not a separate Codex process.

The same config over the HTTP API (`Authorization: Bearer $CODEG_TOKEN`
when the server requires it):

```bash
curl -sS -X POST "$CODEG_ORIGIN/api/roundtable_preflight" \
  -H "content-type: application/json" \
  -H "authorization: Bearer $CODEG_TOKEN" \
  -d @- <<'EOF'
{"request":{"config":{
  "schema_version": 1,
  "topic": "Review the patch",
  "workspace_id": "1",
  "source_refs": [],
  "participants": [
    {"ordinal": 0, "role": "proposer", "provider_ref": "provider:grok", "model": "grok-4.6", "agent": "grok"},
    {"ordinal": 1, "role": "critic", "provider_ref": "provider:cursor", "model": "composer-2.5[fast=true]", "agent": "cursor"},
    {"ordinal": 2, "role": "critic", "provider_ref": "provider:antigravity", "model": "gemini-3.8-flash-high", "agent": "antigravity"}
  ],
  "moderator_ordinal": 0,
  "strategy": {"type": "phased_rounds", "version": 1, "critique_rounds": 1},
  "concurrency": 3,
  "strict_snapshot_v1": true,
  "budgets": {"room_budget": "1800000", "phase_budget": "450000"},
  "timeouts": {"attempt_timeout": "225000"},
  "quotas": {"output_byte_limit": 8192, "input_byte_limit": 16384, "interjection_byte_limit": 16384}
}}}
EOF
```

`moderator_ordinal: 0` makes the Grok seat the moderator. Use `2` for
Antigravity. `workspace_id` is the Codeg folder id. Each `provider_ref`
and `model` must be on that adapter's certificate, and that
certificate's key must be in the execution policy. An omitted `agent`
still means Codex.

Start needs four calls. The first preflight has no room, so its
`confirmed_preflight_id` is JSON `null` and cannot start the room. A
confirmation is stored only when `room_id` is present, the runtime
preflight succeeds, the execution gate is enabled, and readiness is
`ready`. Omit optional fields. JSON `null` is rejected. `revision` and
`expected_revision` are decimal strings (`"1"`), not numbers. `room_id`
is a UUID string.

1. Preflight the config only (the curl above). Read `config_hash`.
   `confirmed_preflight_id` is `null`.
2. Create the draft with the same `config`. The response `revision` is
   `"1"` and `status` is `draft`.

```bash
curl -sS -X POST "$CODEG_ORIGIN/api/roundtable_create" \
  -H "content-type: application/json" \
  -H "authorization: Bearer $CODEG_TOKEN" \
  -d '{"request":{"request_id":"11111111-1111-4111-8111-111111111111","config": { ... }}}'
```

3. Preflight again with `room_id`, `revision`, and the same `config`.
   This is the call that returns `confirmed_preflight_id`. The record
   expires in 15 minutes and must match the principal, room, revision,
   config hash, capability hash, and source hash. A config that no
   longer matches the stored room is `config_changed`. `room_id`
   without `revision` is `revision`. `revision` without `room_id` is
   `room_revision`.

```bash
curl -sS -X POST "$CODEG_ORIGIN/api/roundtable_preflight" \
  -H "content-type: application/json" \
  -H "authorization: Bearer $CODEG_TOKEN" \
  -d '{"request":{"room_id":"<room_id from create>","revision":"1","config": { ... same config ... }}}'
```

4. Start with `room_id`, a new `request_id`, `expected_revision` (not
   `revision`), and `confirmed_preflight_id`. The room must still be
   `draft` or `ready`. Start without that confirmation is
   `preflight_confirmation`.

```bash
curl -sS -X POST "$CODEG_ORIGIN/api/roundtable_start" \
  -H "content-type: application/json" \
  -H "authorization: Bearer $CODEG_TOKEN" \
  -d '{"request":{"room_id":"<room_id>","request_id":"22222222-2222-4222-8222-222222222222","expected_revision":"1","confirmed_preflight_id":"<id from the second preflight>"}}'
```
