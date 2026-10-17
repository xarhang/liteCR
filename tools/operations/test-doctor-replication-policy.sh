#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077
fail() { printf 'liteCR doctor replication check failed [%s]\n' "$1" >&2; exit 1; }
for t in pg_config python3 cargo grep; do command -v "$t" >/dev/null || fail "MISSING_$t"; done
[[ "$(pg_config --version)" == 'PostgreSQL 16.15 (Ubuntu 16.15-0ubuntu0.24.04.1)' ]] || fail POSTGRES_VERSION
[[ "$(id -u)" != 0 ]] || fail MUST_NOT_RUN_AS_ROOT
pg_bin="$(pg_config --bindir)"
for t in initdb pg_ctl pg_isready psql; do [[ -x "$pg_bin/$t" ]] || fail "MISSING_$t"; done
root="$(mktemp -d /tmp/litecr-doctor-policy-v1.XXXXXXXX)"; chmod 700 "$root"; root="$(realpath -e "$root")"
[[ "$root" == /tmp/litecr-doctor-policy-v1.* && ! -L "$root" ]] || fail TEMP_ROOT
printf 'litecr-doctor-policy-v1\n' > "$root/OWNER"; chmod 600 "$root/OWNER"
data="$root/data"; socket="$root/socket"; mkdir -m 700 "$socket"
port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    print(sock.getsockname()[1])
PY
)"
db_user="$(id -un)"; db_url="postgresql://$db_user@127.0.0.1:$port/postgres"
stop_cluster() {
  local rc=0
  [[ -d "$data" && ! -L "$data" && "$(stat -c '%u:%a' "$data")" == "$(id -u):700" ]] || return 1
  if [[ -e "$data/postmaster.pid" || -L "$data/postmaster.pid" ]]; then
    [[ -f "$data/postmaster.pid" && ! -L "$data/postmaster.pid" && "$(sed -n '2p' "$data/postmaster.pid")" == "$data" ]] || return 1
    "$pg_bin/pg_ctl" -D "$data" -m immediate stop >/dev/null 2>&1 || true
  fi
  "$pg_bin/pg_ctl" -D "$data" status >/dev/null 2>&1 || rc=$?
  [[ "$rc" == 3 && ! -e "$data/postmaster.pid" && ! -L "$data/postmaster.pid" ]]
}
cleanup() {
  local rc=$?; trap - EXIT
  if [[ "$root" == /tmp/litecr-doctor-policy-v1.* && -d "$root" && ! -L "$root" \
    && "$(realpath -e "$root" 2>/dev/null)" == "$root" \
    && "$(stat -c '%u:%a' "$root" 2>/dev/null)" == "$(id -u):700" \
    && -f "$root/OWNER" && ! -L "$root/OWNER" \
    && "$(stat -c '%u:%a' "$root/OWNER" 2>/dev/null)" == "$(id -u):600" \
    && "$(cat "$root/OWNER" 2>/dev/null)" == litecr-doctor-policy-v1 \
    && "$data" == "$root/data" ]]; then
    if stop_cluster; then rm -rf -- "$root" || rc=1; else printf 'PRESERVED_FOR_INSPECTION=%s\n' "$root" >&2; rc=1; fi
  else printf 'OWNERSHIP_CHECK_FAILED_PRESERVED=%s\n' "$root" >&2; rc=1; fi
  exit "$rc"
}
trap cleanup EXIT; trap 'exit 129' HUP; trap 'exit 130' INT; trap 'exit 143' TERM

"$pg_bin/initdb" -D "$data" --username="$db_user" --auth-local=trust --auth-host=trust \
  --encoding=UTF8 --locale=C.UTF-8 --no-instructions > "$root/initdb.log"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" \
  -o "-h 127.0.0.1 -p $port -k $socket" start >/dev/null
ready=0
for _ in $(seq 1 100); do
  "$pg_bin/pg_isready" -h 127.0.0.1 -p "$port" -U "$db_user" >/dev/null 2>&1 && { ready=1; break; }
  sleep 0.1
done
[[ "$ready" == 1 ]] || fail POSTGRES_START

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
LITECR_TEST_DATABASE_URL="$db_url" cargo test --manifest-path "$repo/Cargo.toml" \
  -p litecr-infrastructure --test foundation --locked > "$root/foundation-test.log" 2>&1 || {
    cat "$root/foundation-test.log" >&2; fail FOUNDATION_TEST
  }
cargo build --manifest-path "$repo/Cargo.toml" -p litecr-server --locked > "$root/server-build.log" 2>&1 || {
  cat "$root/server-build.log" >&2; fail SERVER_BUILD
}
LITECR_MIGRATION_DATABASE_URL="$db_url" "$repo/target/debug/litecr" migrate > "$root/migrate.log" 2>&1 \
  || { cat "$root/migrate.log" >&2; fail MIGRATION; }
LITECR_DATABASE_URL="$db_url" "$repo/target/debug/litecr" doctor > "$root/doctor-local.txt" 2>&1 \
  || { cat "$root/doctor-local.txt" >&2; fail DOCTOR_LOCAL; }
grep -Fxq 'replication: no synchronous standby policy configured; commits rely on local WAL flush only' \
  "$root/doctor-local.txt" || fail LOCAL_DURABILITY_SUMMARY

"$pg_bin/psql" -X --no-psqlrc "$db_url" -v ON_ERROR_STOP=1 \
  -c "ALTER SYSTEM SET synchronous_standby_names='FIRST 1 (private_doctor_probe)'" >/dev/null
"$pg_bin/psql" -X --no-psqlrc "$db_url" -v ON_ERROR_STOP=1 -c 'SELECT pg_reload_conf()' >/dev/null
LITECR_DATABASE_URL="$db_url" "$repo/target/debug/litecr" doctor > "$root/doctor-configured.txt" 2>&1 \
  || { cat "$root/doctor-configured.txt" >&2; fail DOCTOR_CONFIGURED; }
grep -Fxq 'replication: synchronous standby policy configured; active standby, failure-domain independence, and fencing are not verified' \
  "$root/doctor-configured.txt" || fail CONFIGURED_POLICY_SUMMARY
if grep -Fq 'private_doctor_probe' "$root/doctor-configured.txt"; then fail STANDBY_NAME_DISCLOSED; fi

printf 'DOCTOR_REPLICATION_POLICY=passed\nLOCAL_ONLY_STATUS=reported\nCONFIGURED_POLICY_STATUS=reported\nSTANDBY_NAME=redacted\n'
