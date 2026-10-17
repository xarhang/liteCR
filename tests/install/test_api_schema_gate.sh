#!/usr/bin/env bash
set -euo pipefail

[[ $# == 1 && "$1" == /* && -x "$1" && ! -L "$1" ]] || {
  echo "usage: $0 /absolute/path/to/litecr-release-binary" >&2
  exit 2
}
binary="$1"
pg_bin="$(pg_config --bindir)"
for command in initdb pg_ctl createdb psql pg_isready python3; do
  [[ -x "$pg_bin/$command" || "$(command -v "$command" || true)" != "" ]] || {
    echo "PostgreSQL test command is missing: $command" >&2
    exit 2
  }
done
[[ "$(id -u)" != 0 ]] || { echo "run this disposable-cluster check as an unprivileged user" >&2; exit 2; }

root="$(mktemp -d /tmp/litecr-api-schema-gate.XXXXXXXX)"
root="$(realpath -e -- "$root")"
[[ "$root" == /tmp/litecr-api-schema-gate.* ]] || { echo "unexpected temporary root" >&2; exit 3; }
data="$root/data"
socket="$root/socket"
mkdir -m 700 "$socket"
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
api_port=$((port + 1))
started=0
cleanup() {
  if (( started )); then "$pg_bin/pg_ctl" -D "$data" -m immediate stop >/dev/null 2>&1 || true; fi
  [[ "$root" == /tmp/litecr-api-schema-gate.* ]] || return 0
  rm -rf -- "$root"
}
trap cleanup EXIT

"$pg_bin/initdb" -D "$data" --username="$(id -un)" --auth-local=trust --auth-host=trust --no-instructions >"$root/initdb.log"
if ! "$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" -o "-h 127.0.0.1 -p $port -k $socket" start >/dev/null; then
  cat "$root/postgres.log" >&2
  exit 1
fi
started=1
"$pg_bin/createdb" -h 127.0.0.1 -p "$port" -U "$(id -un)" litecr
"$pg_bin/psql" -X -v ON_ERROR_STOP=1 -h 127.0.0.1 -p "$port" -U "$(id -un)" -d litecr \
  -c 'CREATE SCHEMA litecr; CREATE TABLE litecr.schema_meta(singleton boolean PRIMARY KEY, version bigint NOT NULL); INSERT INTO litecr.schema_meta VALUES (true, 16);' >/dev/null

status=0
output="$(LITECR_DATABASE_URL="postgresql://$(id -un)@127.0.0.1:$port/litecr" \
  LITECR_BIND="127.0.0.1:$api_port" "$binary" serve 2>&1)" || status=$?
[[ "$status" == 2 ]] || { printf 'unexpected API exit status: %s\n%s\n' "$status" "$output" >&2; exit 1; }
[[ "$output" == *"database schema 16 is older than application schema"* ]] || {
  printf 'API did not report an incompatible schema: %s\n' "$output" >&2
  exit 1
}
if "$pg_bin/pg_isready" -h 127.0.0.1 -p "$api_port" -t 1 >/dev/null 2>&1; then
  echo "API unexpectedly bound its listener for an incompatible schema" >&2
  exit 1
fi
echo "API rejected the incompatible schema before binding its listener."
