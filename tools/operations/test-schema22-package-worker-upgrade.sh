#!/usr/bin/env bash
set -euo pipefail
set +x

[[ $# == 2 && "$1" == /* && "$2" == /* ]] || {
  echo "usage: $0 /absolute/path/to/schema-22-package/bin/litecr /absolute/path/to/schema-23-package/bin/litecr" >&2
  exit 2
}

expected_old_archive_sha256=73a79a685d978b41516f9cc021dd460c485ba85fd16647a426ef5d90833f55e8
expected_old_binary_sha256=20f50d508917e30fb628f34cd1b120585122e07533ac8c5cad6626ee29a757aa
expected_new_archive_sha256=2d001a4e18f3ca7bc0d9bda447e6ea263a4e76961261016cfd8208dcaf790a12
expected_new_binary_sha256=a9cc08e0968d4f3d6da2f56cf648b00e5b5419de8cf3b0063e541ad109204dc7
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
old_binary="$(realpath -e -- "$1")"
new_binary="$(realpath -e -- "$2")"

verify_package() {
  local binary="$1" schema="$2" archive_sha="$3" binary_sha="$4"
  local package_root archive actual
  [[ -x "$binary" && ! -L "$binary" && "$(basename -- "$binary")" == litecr ]] || {
    echo "PACKAGE_BINARY_INVALID:$binary" >&2; exit 2;
  }
  package_root="$(dirname -- "$(dirname -- "$binary")")"
  archive="${package_root}.tar.gz"
  [[ -f "$archive" && ! -L "$archive" ]] || { echo 'PACKAGE_ARCHIVE_MISSING' >&2; exit 2; }
  actual="$(sha256sum "$archive" | awk '{print $1}')"
  [[ "$actual" == "$archive_sha" ]] || { echo "PACKAGE_ARCHIVE_HASH_MISMATCH:$schema" >&2; exit 2; }
  actual="$(sha256sum "$binary" | awk '{print $1}')"
  [[ "$actual" == "$binary_sha" ]] || { echo "PACKAGE_BINARY_HASH_MISMATCH:$schema" >&2; exit 2; }
  (cd -- "$package_root" && sha256sum --check --strict SHA256SUMS >/dev/null) || {
    echo "PACKAGE_PAYLOAD_CHECKSUM_FAILED:$schema" >&2; exit 2;
  }
  "$binary" version --json | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d == {"application_version":"0.1.0","schema_version":int(sys.argv[1])}' "$schema" || {
    echo "PACKAGE_VERSION_MISMATCH:$schema" >&2; exit 2;
  }
  python3 - "$package_root/share/metadata/release.json" "$schema" <<'PY'
import json
import pathlib
import sys

release = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert release["application_version"] == "0.1.0"
assert release["schema_version"] == int(sys.argv[2])
assert release["platform"] == "linux-x86_64"
PY
}

verify_package "$old_binary" 22 "$expected_old_archive_sha256" "$expected_old_binary_sha256"
verify_package "$new_binary" 23 "$expected_new_archive_sha256" "$expected_new_binary_sha256"

pg_version="$(pg_config --version)"
[[ "$pg_version" =~ ^PostgreSQL\ 18([.]|$) ]] || {
  echo "POSTGRESQL_18_REQUIRED:$pg_version" >&2; exit 2;
}
pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready psql; do
  [[ -x "$pg_bin/$tool" ]] || { echo "POSTGRESQL_TOOL_MISSING:$tool" >&2; exit 2; }
done
for file in grant-worker-database-access.sh grant-worker-database-access.sql; do
  [[ -f "$repo/tools/operations/$file" && ! -L "$repo/tools/operations/$file" ]] || {
    echo "WORKER_ACL_FILE_MISSING:$file" >&2; exit 2;
  }
done

export PATH="$pg_bin:$PATH"
unset PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGHOSTADDR PGOPTIONS PGDATABASE PGPORT PGHOST
bootstrap_role="$(id -un)"
[[ "$bootstrap_role" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || {
  echo 'UNSUPPORTED_BOOTSTRAP_ROLE' >&2; exit 2;
}

root="$(mktemp -d /tmp/litecr-schema22-package-worker-upgrade.XXXXXXXX)"
chmod 700 "$root"
root="$(realpath -e -- "$root")"
[[ "$root" == /tmp/litecr-schema22-package-worker-upgrade.* && -d "$root" && ! -L "$root" ]] || {
  echo 'TEMP_PATH_UNSAFE' >&2; exit 3;
}
printf 'litecr-schema22-package-worker-upgrade-v1\n' > "$root/OWNER"
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
  if [[ "$root" == /tmp/litecr-schema22-package-worker-upgrade.* && -f "$root/OWNER" && \
        "$(cat "$root/OWNER")" == litecr-schema22-package-worker-upgrade-v1 ]]; then
    rm -rf -- "$root"
  else
    echo 'CLEANUP_REFUSED_UNOWNED_PATH' >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT

"$pg_bin/initdb" -D "$data" --username="$bootstrap_role" --auth-local=trust --auth-host=trust \
  --encoding=UTF8 --locale=C.UTF-8 --no-instructions >"$root/initdb.log"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" \
  -o "-h 127.0.0.1 -p $port -k $socket" start >/dev/null
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
LITECR_MIGRATION_DATABASE_URL="$migration_url" "$old_binary" migrate
[[ "$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_migrator PGDATABASE=litecr_test \
  "$pg_bin/psql" -X --no-psqlrc -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == 22 ]] || {
  echo 'SCHEMA_22_SETUP_FAILED' >&2; exit 1;
}
LITECR_MIGRATION_DATABASE_URL="$migration_url" \
  bash "$repo/tools/operations/grant-worker-database-access.sh"

cd "$repo"
LITECR_TEST_DATABASE_URL="$migration_url" \
LITECR_WORKER_DATABASE_URL="$worker_url" \
LITECR_SCHEMA23_PACKAGE_BINARY="$new_binary" \
CARGO_BUILD_JOBS=1 cargo +1.85.0 test --locked -p litecr-infrastructure \
  --test migration_queue_upgrade -- --test-threads=1 --ignored

[[ "$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_migrator PGDATABASE=litecr_test \
  "$pg_bin/psql" -X --no-psqlrc -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == 23 ]] || {
  echo 'SCHEMA_23_UPGRADE_NOT_RETAINED' >&2; exit 1;
}
echo 'SCHEMA22_TO23_EXACT_PACKAGE_WORKER_UPGRADE=passed'
