#!/usr/bin/env bash
set -euo pipefail
set +x

[[ $# == 1 && "$1" == /* && -x "$1" && ! -L "$1" && $(basename -- "$1") =~ ^litecr(\.exe)?$ ]] || {
  echo "usage: $0 /absolute/path/to/schema-22-litecr-binary" >&2
  exit 2
}
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
release="$(realpath -e -- "$1")"
pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready; do
  [[ -x "$pg_bin/$tool" ]] || { echo "POSTGRESQL_TOOL_MISSING:$tool" >&2; exit 2; }
done
schema_version="$("$release" version --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["schema_version"])')"
[[ "$schema_version" == 22 ]] || { echo 'SCHEMA_22_BINARY_REQUIRED' >&2; exit 2; }
export PATH="$pg_bin:$PATH"
unset PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGHOSTADDR PGOPTIONS PGDATABASE PGPORT PGHOST
bootstrap_role="$(id -un)"
[[ "$bootstrap_role" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || { echo 'UNSUPPORTED_BOOTSTRAP_ROLE' >&2; exit 2; }

root="$(mktemp -d /tmp/litecr-schema22-worker-upgrade.XXXXXXXX)"
chmod 700 "$root"
root="$(realpath -e -- "$root")"
[[ "$root" == /tmp/litecr-schema22-worker-upgrade.* && -d "$root" && ! -L "$root" ]] || { echo 'TEMP_PATH_UNSAFE' >&2; exit 3; }
printf 'litecr-schema22-worker-upgrade-v1\n' > "$root/OWNER"
chmod 600 "$root/OWNER"
data="$root/data"
socket="$root/socket"
mkdir -m 700 "$socket"
port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"
started=0
cleanup() {
  status=$?
  if (( started )); then "$pg_bin/pg_ctl" -D "$data" -m immediate stop >/dev/null 2>&1 || true; fi
  if [[ "$root" == /tmp/litecr-schema22-worker-upgrade.* && -f "$root/OWNER" && "$(cat "$root/OWNER")" == litecr-schema22-worker-upgrade-v1 ]]; then
    rm -rf -- "$root"
  else
    echo 'CLEANUP_REFUSED_UNOWNED_PATH' >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT

"$pg_bin/initdb" -D "$data" --username="$bootstrap_role" --auth-local=trust --auth-host=trust --encoding=UTF8 --locale=C.UTF-8 --no-instructions >"$root/initdb.log"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" -o "-h 127.0.0.1 -p $port -k $socket" start >/dev/null
started=1
for _ in $(seq 1 50); do
  if "$pg_bin/pg_isready" -h 127.0.0.1 -p "$port" -U "$bootstrap_role" >/dev/null 2>&1; then break; fi
  sleep 0.1
done
[[ "$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER="$bootstrap_role" PGDATABASE=postgres \
  "$pg_bin/psql" -X --no-psqlrc -qAt -c 'SELECT host(inet_server_addr())')" == 127.0.0.1 ]] || {
  echo 'CLUSTER_NOT_LOOPBACK' >&2; exit 3;
}

PGHOST=127.0.0.1 PGPORT="$port" PGUSER="$bootstrap_role" PGDATABASE=postgres \
  "$pg_bin/psql" -X --no-psqlrc -v ON_ERROR_STOP=1 <<'SQL'
CREATE ROLE litecr_migrator LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_worker LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE DATABASE litecr_test OWNER litecr_migrator;
SQL

migration_url="postgresql://litecr_migrator@127.0.0.1:$port/litecr_test"
worker_url="postgresql://litecr_worker@127.0.0.1:$port/litecr_test"
LITECR_MIGRATION_DATABASE_URL="$migration_url" "$release" migrate
[[ "$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_migrator PGDATABASE=litecr_test \
  "$pg_bin/psql" -X --no-psqlrc -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == 22 ]] || {
  echo 'SCHEMA_22_SETUP_FAILED' >&2; exit 1;
}

# Apply the checked-in least-privilege grants before the source test migrates this
# exact database to the current embedded schema. No runtime identity or application table access is
# granted to the worker.
LITECR_MIGRATION_DATABASE_URL="$migration_url" bash "$repo/tools/operations/grant-worker-database-access.sh"

cd "$repo"
LITECR_TEST_DATABASE_URL="$migration_url" \
LITECR_WORKER_DATABASE_URL="$worker_url" \
CARGO_BUILD_JOBS=1 cargo test --locked -p litecr-infrastructure \
  --test migration_queue_upgrade -- --test-threads=1 --ignored

[[ "$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_migrator PGDATABASE=litecr_test \
  "$pg_bin/psql" -X --no-psqlrc -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == 31 ]] || {
  echo 'CURRENT_SCHEMA_UPGRADE_NOT_RETAINED' >&2; exit 1;
}
echo 'SCHEMA22_TO_CURRENT_RESTRICTED_WORKER_UPGRADE=passed'
