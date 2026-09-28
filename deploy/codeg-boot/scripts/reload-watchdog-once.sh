#!/bin/bash
set -uo pipefail
# Kill only processes whose argv0 path is exactly the watchdog script
for p in /proc/[0-9]*; do
  pid=${p#/proc/}
  # read first arg from cmdline
  mapfile -d '' -t args < "$p/cmdline" 2>/dev/null || continue
  # bash running the script: args like [bash, /workspace/codeg-boot/codeg-watchdog.sh]
  for a in "${args[@]}"; do
    if [ "$a" = "/workspace/codeg-boot/codeg-watchdog.sh" ]; then
      # skip if this is a one-shot editor shell that only mentions it in a long -c string
      if [ "${args[0]}" = "bash" ] || [ "${args[0]}" = "/bin/bash" ] || [ "${args[0]}" = "/usr/bin/bash" ]; then
        if [ "${#args[@]}" -ge 2 ] && [ "${args[1]}" = "/workspace/codeg-boot/codeg-watchdog.sh" ]; then
          echo "kill wd $pid"
          kill "$pid" 2>/dev/null || true
        elif [ "${args[0]}" = "/workspace/codeg-boot/codeg-watchdog.sh" ]; then
          echo "kill wd $pid"
          kill "$pid" 2>/dev/null || true
        fi
      elif [ "${args[0]}" = "/workspace/codeg-boot/codeg-watchdog.sh" ]; then
        echo "kill wd $pid"
        kill "$pid" 2>/dev/null || true
      fi
    fi
  done
done
sleep 2
# force remaining
for p in /proc/[0-9]*; do
  pid=${p#/proc/}
  mapfile -d '' -t args < "$p/cmdline" 2>/dev/null || continue
  if [ "${args[0]-}" = "/workspace/codeg-boot/codeg-watchdog.sh" ] || { [ "${#args[@]}" -ge 2 ] && [ "${args[1]-}" = "/workspace/codeg-boot/codeg-watchdog.sh" ]; }; then
    echo "force $pid"
    kill -9 "$pid" 2>/dev/null || true
  fi
done
sleep 1
nohup /workspace/codeg-boot/codeg-watchdog.sh >/dev/null 2>&1 &
echo "started $!"
echo $! > /workspace/heartbeat/watchdog.pid
# wait for webdav= log line (watchdog sleeps 60s after first loop... actually logs at end of first loop then sleeps)
# First loop runs checks then logs then sleep 60 — so first log should appear within ~few seconds
for i in $(seq 1 30); do
  line=$(tail -1 /workspace/heartbeat/watchdog.log 2>/dev/null || true)
  echo "try$i $line"
  case "$line" in
    *webdav=*) exit 0 ;;
  esac
  sleep 2
done
exit 1
