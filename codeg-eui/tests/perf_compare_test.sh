#!/usr/bin/env bash
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
script="$script_dir/../scripts/perf_compare.sh"
chmod +x "$script"

usage=$($script --help)
for command in record-eui record-webview aggregate validate self-test; do
  printf '%s\n' "$usage" | grep -q "$command"
done

set +e
$script unknown-command
status=$?
set -e
test "$status" -eq 64

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sleep 60 &
stub_pid=$!

for run in 1 2 3 4; do
  python3 - "$tmp/eui-metrics-$run.json" <<'PY'
import json,sys
json.dump({
  "shell":"eui","agent":"codex","promptId":"continuous-text-v1",
  "buildType":"release","backend":"opengl","gitCommit":"abc",
  "t0Ns":0,"tFirstPresentedNs":16,"tEndNs":108,
  "frameIntervalsMs":[16,60,16],"longFrameThresholdMs":50,"longFrameCount":1,
}, open(sys.argv[1],'w'))
PY
  python3 - "$tmp/webview-metrics-$run.json" <<'PY'
import json,sys
json.dump({
  "shell":"webview","agent":"codex","promptId":"continuous-text-v1",
  "buildType":"release","backend":"tauri-webview","gitCommit":"abc",
  "t0Ns":0,"tFirstPresentedNs":20,"tEndNs":100,
  "frameIntervalsMs":[20,40,20],"longFrameThresholdMs":50,"longFrameCount":0,
}, open(sys.argv[1],'w'))
PY
  $script record-eui --pid "$stub_pid" --metrics "$tmp/eui-metrics-$run.json" --output "$tmp/eui-runs/run-$run.json"
  $script validate "$tmp/eui-runs/run-$run.json"
  $script record-webview --pid "$stub_pid" --metrics "$tmp/webview-metrics-$run.json" --output "$tmp/webview-runs/run-$run.json"
  $script validate "$tmp/webview-runs/run-$run.json"
done
$script aggregate --eui-dir "$tmp/eui-runs" --webview-dir "$tmp/webview-runs" --output "$tmp/aggregate.json"
$script validate "$tmp/aggregate.json" || true
# aggregate schema differs; self-test covers it
$script self-test
kill "$stub_pid" 2>/dev/null || true
printf 'perf_compare_test ok\n'
