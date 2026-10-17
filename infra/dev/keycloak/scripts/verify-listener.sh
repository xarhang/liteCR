#!/usr/bin/env bash
set -euo pipefail

fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
command -v ss >/dev/null || fail 'iproute2 ss is required.'

for port in 18081 19001; do
  lines="$(ss -H -ltn "sport = :$port")"
  [[ -n "$lines" ]] || fail "No TCP listener found on port $port."
  while IFS= read -r line; do
    local_addr="$(awk '{print $4}' <<< "$line")"
    case "$local_addr" in
      "127.0.0.1:$port"|"[::ffff:127.0.0.1]:$port") ;;
      *) fail "Listener on $port is not strictly loopback: $local_addr" ;;
    esac
  done <<< "$lines"
done
printf 'PASS: application and management listeners are restricted to IPv4 loopback (ports 18081 and 19001).\n'
