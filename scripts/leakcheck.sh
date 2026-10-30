#!/usr/bin/env bash
# Runs the server under valgrind, exercises it, stops it with SIGINT and
# prints the leak summary. (The server shuts down gracefully on SIGINT/SIGTERM.)
set -euo pipefail
cd "$(dirname "$0")/.."
command -v valgrind >/dev/null || { echo "install valgrind first"; exit 1; }
cargo build
valgrind --leak-check=full --show-leak-kinds=all --error-exitcode=1 \
  ./target/debug/localhost config/server.conf &
VG=$!
sleep 5
python3 tests/integration.py --external || true
kill -INT "$VG"
wait "$VG"
