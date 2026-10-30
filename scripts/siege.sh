#!/usr/bin/env bash
# Stress test: siege -b must keep availability >= 99.5%.
# ONLY run this against your own server.
set -euo pipefail
URL="${1:-http://127.0.0.1:8080/}"
DURATION="${2:-30S}"
CONC="${3:-50}"
command -v siege >/dev/null || { echo "install siege first (apt install siege)"; exit 1; }

PID="$(pgrep -f 'target/(release|debug)/localhost' | head -n1 || true)"
if [ -n "$PID" ]; then
  echo "server pid $PID, open fds before: $(ls /proc/$PID/fd | wc -l)"
fi
siege -b -c "$CONC" -t "$DURATION" "$URL"
if [ -n "$PID" ]; then
  sleep 2
  echo "open fds after (should be close to the 'before' value): $(ls /proc/$PID/fd | wc -l)"
  grep -E 'VmRSS' /proc/$PID/status || true
fi
