#!/bin/sh
set -eu

normal_rlib=$1
test -f "$normal_rlib"

probe_dir=$(mktemp -d)
trap 'rm -rf "$probe_dir"' EXIT HUP INT TERM
probe="$probe_dir/hook_probe.rs"
cat >"$probe" <<'EOF'
extern crate codeg_eui_core;

fn main() {
    let _hook: fn() -> Result<u64, i32> = codeg_eui_core::enqueue_blocked_for_test;
    let _ = _hook;
}
EOF

if rustc --edition=2021 --extern "codeg_eui_core=$normal_rlib" \
    -L "dependency=$(dirname "$normal_rlib")" -o "$probe_dir/hook_probe" "$probe" \
    >"$probe_dir/stdout" 2>"$probe_dir/stderr"; then
    printf 'normal-feature rlib unexpectedly exposes enqueue_blocked_for_test\n' >&2
    exit 1
fi

grep -F 'enqueue_blocked_for_test' "$probe_dir/stderr" >/dev/null
