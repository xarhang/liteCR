#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
RUNTIME="$HERE/.runtime"
INSTALL="$RUNTIME/keycloak-26.8.0"
PID_FILE="$RUNTIME/keycloak.pid"
fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

[[ -f "$PID_FILE" ]] || fail "No fixture PID file found at $PID_FILE."
[[ -f "$INSTALL/.litecr-disposable-fixture" ]] || fail 'Install fixture marker missing; refusing to signal a process.'
pid="$(cat "$PID_FILE")"
[[ "$pid" =~ ^[0-9]+$ ]] || fail 'PID file is malformed.'
[[ -d "/proc/$pid" ]] || { rm -f "$PID_FILE"; printf 'Fixture process was already stopped.\n'; exit 0; }
cwd="$(readlink -f "/proc/$pid/cwd")"
cmd="$(tr '\0' ' ' < "/proc/$pid/cmdline")"
[[ "$cwd" == "$INSTALL" ]] || fail "PID $pid is not running from the fixture install directory."
[[ "$cmd" == *'start-dev'* && "$cmd" == *'127.0.0.1'* ]] || fail "PID $pid command does not match the fixture Keycloak start command."
kill -TERM "$pid"
for _ in $(seq 1 30); do
  [[ -d "/proc/$pid" ]] || break
  sleep 1
done
[[ ! -d "/proc/$pid" ]] || fail "Keycloak did not stop within 30 seconds; PID $pid remains."
rm -f "$PID_FILE"
printf 'Stopped disposable Keycloak process %s.\n' "$pid"
