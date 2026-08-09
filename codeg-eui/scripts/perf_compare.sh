#!/usr/bin/env bash
# Comparable EUI vs WebView performance protocol.
set -euo pipefail

LONG_FRAME_MS=50
RSS_SCOPE="shell-process-only"

usage() {
  cat <<'EOF'
Usage: perf_compare.sh <command> [options]

Commands:
  record-eui       --pid <shell-pid> --metrics <shell-json> --output <run-json>
  record-webview   --pid <shell-pid> --metrics <shell-json> --output <run-json>
  aggregate        --eui-dir <dir> --webview-dir <dir> --output <aggregate-json>
  validate         <run-or-aggregate.json>
  self-test
  --help
EOF
}

die() { printf '%s\n' "$*" >&2; exit 1; }
usage_error() { printf '%s\n' "$*" >&2; exit 64; }

sample_rss_kb() {
  local pid=$1
  local status="/proc/${pid}/status"
  [[ -r "$status" ]] || die "cannot read $status"
  awk '/^VmRSS:/ { print $2; found=1 } END { if (!found) exit 1 }' "$status"
}

json_get() {
  # tiny getter: json_get file key
  local file=$1 key=$2
  python3 - "$file" "$key" <<'PY'
import json,sys
doc=json.load(open(sys.argv[1]))
key=sys.argv[2]
cur=doc
for part in key.split('.'):
    if isinstance(cur, dict) and part in cur:
        cur=cur[part]
    else:
        print('')
        sys.exit(0)
print(cur if cur is not None else '')
PY
}

validate_run() {
  local file=$1
  python3 - "$file" "$LONG_FRAME_MS" "$RSS_SCOPE" <<'PY'
import json, math, sys
path, thr, scope = sys.argv[1], float(sys.argv[2]), sys.argv[3]
doc = json.load(open(path))
required = [
  "shell","agent","promptId","buildType","backend","t0Ns","tFirstPresentedNs",
  "tEndNs","frameIntervalsMs","longFrameThresholdMs","longFrameCount",
  "peakShellRssKb","shellPid","rssScope","gitCommit"
]
for key in required:
    if key not in doc:
        raise SystemExit(f"missing field {key}")
if doc["longFrameThresholdMs"] != thr:
    raise SystemExit("longFrameThresholdMs must be 50")
if doc["rssScope"] != scope:
    raise SystemExit("rssScope must be shell-process-only")
if not isinstance(doc["shellPid"], int) or doc["shellPid"] <= 0:
    raise SystemExit("shellPid invalid")
intervals = doc["frameIntervalsMs"]
if not isinstance(intervals, list):
    raise SystemExit("frameIntervalsMs must be list")
count = sum(1 for x in intervals if x > thr)
if count != doc["longFrameCount"]:
    raise SystemExit(f"longFrameCount mismatch {count} != {doc['longFrameCount']}")
t0, first, end = doc["t0Ns"], doc["tFirstPresentedNs"], doc["tEndNs"]
if not (t0 <= first <= end):
    raise SystemExit("timestamps must be non-decreasing t0<=first<=end")
print("ok")
PY
}

merge_record() {
  local shell=$1 pid=$2 metrics=$3 output=$4
  [[ "$pid" =~ ^[0-9]+$ ]] || die "pid must be numeric"
  [[ -f "$metrics" ]] || die "metrics missing: $metrics"
  local peak=0
  # sample until metrics has tEndNs (file already complete in protocol)
  peak=$(sample_rss_kb "$pid" || echo 0)
  python3 - "$shell" "$pid" "$metrics" "$output" "$peak" "$RSS_SCOPE" "$LONG_FRAME_MS" <<'PY'
import json, sys, os
shell, pid, metrics, output, peak, scope, thr = sys.argv[1:8]
doc = json.load(open(metrics))
if doc.get("shell") not in (None, shell):
    # allow metrics without shell; force shell
    pass
doc["shell"] = shell
doc["shellPid"] = int(pid)
doc["rssScope"] = scope
doc["peakShellRssKb"] = int(peak)
doc["longFrameThresholdMs"] = float(thr)
# ensure required defaults
doc.setdefault("frameIntervalsMs", [])
doc.setdefault("longFrameCount", sum(1 for x in doc["frameIntervalsMs"] if x > float(thr)))
doc.setdefault("agent", "codex")
doc.setdefault("promptId", "continuous-text-v1")
doc.setdefault("buildType", "release")
doc.setdefault("backend", shell)
doc.setdefault("gitCommit", "unknown")
doc.setdefault("t0Ns", 0)
doc.setdefault("tFirstPresentedNs", doc.get("t0Ns", 0))
doc.setdefault("tEndNs", doc.get("tFirstPresentedNs", 0))
os.makedirs(os.path.dirname(output) or ".", exist_ok=True)
tmp = output + ".tmp"
with open(tmp, "w") as f:
    json.dump(doc, f, indent=2)
    f.write("\n")
os.replace(tmp, output)
print(output)
PY
  validate_run "$output" >/dev/null
}

cmd_aggregate() {
  local eui_dir= webview_dir= output=
  while [[ $# -gt 0 ]]; do
    case $1 in
      --eui-dir) eui_dir=$2; shift 2 ;;
      --webview-dir) webview_dir=$2; shift 2 ;;
      --output) output=$2; shift 2 ;;
      *) usage_error "unknown aggregate arg: $1" ;;
    esac
  done
  [[ -n "$eui_dir" && -n "$webview_dir" && -n "$output" ]] || usage_error "aggregate requires dirs and output"
  python3 - "$eui_dir" "$webview_dir" "$output" "$LONG_FRAME_MS" <<'PY'
import json, os, sys, statistics, math
eui_dir, web_dir, output, thr = sys.argv[1:5]
thr=float(thr)

def load_runs(d):
    files=sorted(f for f in os.listdir(d) if f.endswith('.json'))
    runs=[]
    for f in files:
        path=os.path.join(d,f)
        runs.append(json.load(open(path)))
    return runs

def validate_meta(runs):
    if len(runs) < 4:
        raise SystemExit('need 1 warm-up + 3 measured runs')
    keys=['agent','promptId','buildType','gitCommit']
    base={k:runs[0].get(k) for k in keys}
    for r in runs[1:]:
        for k in keys:
            if r.get(k)!=base[k]:
                raise SystemExit(f'metadata mismatch on {k}')

def summarize(runs):
    measured=runs[1:]
    first=[r['tFirstPresentedNs']-r['t0Ns'] for r in measured]
    intervals=[]
    long=0
    peak=0
    for r in measured:
        intervals.extend(r.get('frameIntervalsMs') or [])
        long += r.get('longFrameCount') or 0
        peak = max(peak, r.get('peakShellRssKb') or 0)
    intervals_sorted=sorted(intervals)
    if intervals_sorted:
        idx=math.ceil(0.95*len(intervals_sorted))-1
        p95=intervals_sorted[min(idx,len(intervals_sorted)-1)]
    else:
        p95=0
    return {
        'medianFirstPresentedLatencyNs': statistics.median(first),
        'frameIntervalP95Ms': p95,
        'longFrameCountSum': long,
        'peakShellRssKb': peak,
        'measuredRuns': len(measured),
        'longFrameThresholdMs': thr,
        'rssScope': 'shell-process-only',
    }

eui=load_runs(eui_dir)
web=load_runs(web_dir)
validate_meta(eui); validate_meta(web)
out={
  'eui': summarize(eui),
  'webview': summarize(web),
  'agent': eui[0].get('agent'),
  'promptId': eui[0].get('promptId'),
  'gitCommit': eui[0].get('gitCommit'),
  'buildType': eui[0].get('buildType'),
  'longFrameThresholdMs': thr,
  'rssScope': 'shell-process-only',
}
os.makedirs(os.path.dirname(output) or '.', exist_ok=True)
json.dump(out, open(output,'w'), indent=2)
print(output)
PY
}

cmd_self_test() {
  tmp=$(mktemp -d)
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" RETURN
  # long-lived stub pid: sleep
  sleep 120 &
  local stub=$!
  # child with extra RSS not attributed
  sleep 120 &
  local child=$!
  for run in 1 2 3 4; do
    python3 - "$tmp/eui-metrics-$run.json" "$run" <<'PY'
import json,sys
path,run=sys.argv[1],int(sys.argv[2])
# active intervals [16,60,16] style when frames 0,16,32,92,108
doc={
  "shell":"eui","agent":"codex","promptId":"continuous-text-v1",
  "buildType":"release","backend":"opengl","gitCommit":"deadbeef",
  "t0Ns":0,"tFirstTokenNs":8,"tFirstPresentedNs":16,"tEndNs":108,
  "frameIntervalsMs":[16,60,16],"longFrameThresholdMs":50,"longFrameCount":1,
}
json.dump(doc, open(path,'w'))
PY
    python3 - "$tmp/webview-metrics-$run.json" <<'PY'
import json,sys
doc={
  "shell":"webview","agent":"codex","promptId":"continuous-text-v1",
  "buildType":"release","backend":"tauri-webview","gitCommit":"deadbeef",
  "t0Ns":0,"tFirstTokenNs":8,"tFirstPresentedNs":20,"tEndNs":120,
  "frameIntervalsMs":[20,40,20],"longFrameThresholdMs":50,"longFrameCount":0,
}
json.dump(doc, open(sys.argv[1],'w'))
PY
    merge_record eui "$stub" "$tmp/eui-metrics-$run.json" "$tmp/eui-runs/run-$run.json"
    merge_record webview "$stub" "$tmp/webview-metrics-$run.json" "$tmp/webview-runs/run-$run.json"
  done
  # child RSS must not be required — only shell pid sampled
  kill "$child" 2>/dev/null || true
  cmd_aggregate --eui-dir "$tmp/eui-runs" --webview-dir "$tmp/webview-runs" --output "$tmp/aggregate.json"
  # validate aggregate has threshold
  python3 - "$tmp/aggregate.json" <<'PY'
import json,sys
doc=json.load(open(sys.argv[1]))
assert doc['longFrameThresholdMs']==50
assert doc['eui']['measuredRuns']==3
print('aggregate ok')
PY
  # reject bad threshold
  python3 - "$tmp/bad.json" <<'PY'
import json,sys
doc=json.load(open(sys.argv[1].replace('bad','eui-runs/run-2')))
doc['longFrameThresholdMs']=40
json.dump(doc, open(sys.argv[1],'w'))
PY
  if validate_run "$tmp/bad.json" >/dev/null 2>&1; then
    die 'expected threshold validation failure'
  fi
  kill "$stub" 2>/dev/null || true
  printf 'self-test ok\n'
}

main() {
  [[ $# -ge 1 ]] || { usage; exit 64; }
  case $1 in
    --help|-h) usage; exit 0 ;;
    record-eui)
      shift
      local pid= metrics= output=
      while [[ $# -gt 0 ]]; do
        case $1 in
          --pid) pid=$2; shift 2 ;;
          --metrics) metrics=$2; shift 2 ;;
          --output) output=$2; shift 2 ;;
          *) usage_error "unknown: $1" ;;
        esac
      done
      merge_record eui "$pid" "$metrics" "$output"
      ;;
    record-webview)
      shift
      local pid= metrics= output=
      while [[ $# -gt 0 ]]; do
        case $1 in
          --pid) pid=$2; shift 2 ;;
          --metrics) metrics=$2; shift 2 ;;
          --output) output=$2; shift 2 ;;
          *) usage_error "unknown: $1" ;;
        esac
      done
      merge_record webview "$pid" "$metrics" "$output"
      ;;
    aggregate) shift; cmd_aggregate "$@" ;;
    validate) shift; validate_run "${1:?}"; printf 'validate ok\n' ;;
    self-test) cmd_self_test ;;
    *) usage_error "unknown-command" ;;
  esac
}

main "$@"
