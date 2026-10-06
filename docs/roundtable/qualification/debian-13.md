# Debian 13 roundtable qualification

A roundtable turn runs only after this host has a passed certificate for
every adapter in the room and `execution-policy.json` allows those
certificates. The probe writes a certificate only when every check on
this machine passed. A missing isolator, a failed escape, a hash
mismatch, or a failed ACP turn is a failing report and no certificate.

Profiles accept Debian 12 and Debian 13 as profile parameters. The
issued key still pins the OS, kernel, and architecture that were
measured. A Debian 12 certificate does not verify on Debian 13.

`spike/probe.mjs` is not this probe. It never sets
`qualification_issued` to true.

## Profiles

| Agent | Profile id | Binary inside the rootfs | Version text must contain | Auth files (host home, read-only) |
| --- | --- | --- | --- | --- |
| Codex | `linux-codex-2.1.1` | `/usr/local/bin/codex-acp` | `2.1.1` | `.codex/auth.json` |
| Grok | `linux-grok-1.0.46` | `/usr/local/bin/grok` with args `--no-auto-update agent stdio` | `1.0.46` | `.grok/auth.json` |
| Cursor | `linux-cursor-acp-2026.09.28-64d2043` | `/usr/local/bin/cursor-agent` with arg `acp` | `2026.09.28-64d2043` | `.cursor/cli-config.json`, plus one of `.config/cursor/auth.json` (XDG) or `.cursor/auth.json` |
| Antigravity | `linux-antigravity-acp-1.3.0` (default) | `/usr/local/bin/agy_acp_server.par` with arg `--uid=` | `1.3.0` | `.gemini/antigravity-acp/settings.json` and `acp_token.json` |
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
that byte and fails unless it is `1`. It then leaves a watcher,
`( while kill -0 $pid; do sleep 0.2; done; kill $slirp ) &`, because
crun adopts the orphaned slirp process and `crun run` would otherwise
wait on it forever.

`crun run` deletes a container that exits on its own. The probe's later
`crun delete` then fails with `cannot open directory …/state/<id>`.
That still counts as reaped when the container state directory is gone
and its cgroup directory is gone. A delete error while the cgroup
directory remains is not `REAPED`.

## Rootfs

One rootfs directory can hold every adapter. Do not put credentials,
tokens, or `auth.json` contents in the image or in git. The probe
bind-mounts the host auth files read-only and rejects a placeholder that
is missing, non-empty, or a symlink.

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
(`home-upper/<agent>/<container-id>-<run>`), with each auth file
bind-mounted read-only on top of it. The probe removes the scratch
directory after the container is reaped, including when the run fails.
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

Auth stays on the host. The probe mounts these files read-only when
they exist and are not inside the rootfs:

- `$HOME/.grok/auth.json`
- `$HOME/.cursor/cli-config.json`
- Cursor's token: `$HOME/.config/cursor/auth.json` (what the Linux CLI
  reads under `XDG_CONFIG_HOME`, set to `/rt-home/.config`) or
  `$HOME/.cursor/auth.json` when that build stores it beside
  `CURSOR_CONFIG_DIR`. At least one of those two files must exist.
  `session/new` also needs the slirp path to the Cursor origin; a
  visible token with no network still returns `Authentication required`.
  On the Debian 13 host those files were still not enough: `cursor-agent
  acp` returned `Authentication required` against the real home as well,
  while `cursor-agent status` was logged in. ACP `session/new` succeeded
  on the host only with `CURSOR_API_KEY` (desktop
  `CURSOR_AUTH_MODE=custom`). This probe does not inject that key.
- `$HOME/.gemini/antigravity-acp/settings.json` and
  `$HOME/.gemini/antigravity-acp/acp_token.json`
  (`acp_business_token.json` is mounted when it exists)
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
`model`) and fails `endpoint_compatibility` when the selected binding
is missing from that list or when the adapter advertises no model. Do
not invent an id the adapter did not report. Origins must be `https`
with no user, path, or query.
File-auth adapters (Grok, Cursor, Antigravity) are admitted when the
mounted auth files exist even if that environment variable is unset.
Codex still uses the named variable as the host-side gateway
credential. The value is never copied into the image.

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

## Execution policy

The probe does not enable the gate. After the adapters you need have
passed, merge their qualification keys into one file:

`$CODEG_DATA_DIR/roundtable/execution-policy.json`

```json
{
  "enabled": true,
  "generation": 1,
  "allowed_qualification_keys": []
}
```

Replace `allowed_qualification_keys` with the key objects from each
`execution-policy.example.json` you intend to run. `enabled` must be
true. `generation` must match the file the server reads. A key that is
not in this list cannot start a turn.

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
    {"ordinal": 1, "role": "critic", "provider_ref": "provider:cursor", "model": "composer-2.5", "agent": "cursor"},
    {"ordinal": 2, "role": "critic", "provider_ref": "provider:antigravity", "model": "gemini-2.5-pro", "agent": "antigravity"}
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
Antigravity. Create and start use the same `config`:

```bash
curl -sS -X POST "$CODEG_ORIGIN/api/roundtable_create" \
  -H "content-type: application/json" \
  -H "authorization: Bearer $CODEG_TOKEN" \
  -d '{"request":{"request_id":"11111111-1111-4111-8111-111111111111","config": { ... }}}'
```

Then `POST /api/roundtable_start` with `room_id`, `request_id`, and
`expected_revision` from the create response. `workspace_id` is the
Codeg folder id. Each `provider_ref` and `model` must be on that
adapter's certificate, and that certificate's key must be in the
execution policy. An omitted `agent` still means Codex.
