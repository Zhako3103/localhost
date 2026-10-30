#!/usr/bin/env bash
# Prints status + headers of the same request from localhost-rs and a local
# NGINX so that behaviour can be compared side by side.
# Usage: scripts/compare_nginx.sh /path [ours_port] [nginx_port]
P="${1:-/}"; OURS="${2:-8080}"; NGX="${3:-80}"
for port in "$OURS" "$NGX"; do
  echo "=== 127.0.0.1:$port$P"
  curl -s -o /dev/null -D - "http://127.0.0.1:$port$P" | sed 's/\r$//'
done
