#!/usr/bin/env bash
# Explicit synthetic probe; owns a new loopback-only database cluster.
set -euo pipefail
set +x
umask 077
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
records="${1:-1000}"
report="${2:-$repo/scratch/matching-load-${records}.json}"
[[ $# -le 2 && "$records" =~ ^[1-9][0-9]{2,3}$ && "$records" -ge 100 && "$records" -le 9000 ]] || {
  echo 'usage: test-matching-load-postgres.sh [100..9000 distractors] [/absolute/report.json]' >&2; exit 2;
}
mkdir -p -- "$repo/scratch"
[[ "$report" == /* && -d "$(dirname -- "$report")" && ! -L "$report" ]] || {
  echo 'ABSOLUTE_REPORT_WITH_EXISTING_PARENT_REQUIRED' >&2; exit 2;
}
pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready createdb; do
  [[ -x "$pg_bin/$tool" ]] || { echo "POSTGRESQL_TOOL_MISSING:$tool" >&2; exit 2; }
done
[[ "$EUID" != 0 ]] || { echo 'NON_ROOT_TEST_USER_REQUIRED' >&2; exit 2; }
source "$repo/tools/operations/cleanup-owned-postgres-cluster.sh"
export PATH="$pg_bin:$PATH"
unset PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGHOSTADDR PGOPTIONS PGDATABASE PGPORT PGHOST
test_role="$(id -un)"
[[ "$test_role" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || { echo 'UNSUPPORTED_TEST_ROLE' >&2; exit 2; }
root="$(mktemp -d /tmp/litecr-p6-recovery-schema27.XXXXXXXX)"
root="$(realpath -e -- "$root")"
printf 'litecr-p6-recovery-schema27-v1\n' > "$root/OWNER"
data="$root/postgres-data"
socket="$root/socket"
mkdir -m 700 "$socket"
cleanup() {
  local status=$?
  if ((status != 0)) && [[ -f "$root/postgres.log" ]]; then tail -n 40 "$root/postgres.log" >&2; fi
  if ! litecr_cleanup_owned_postgres_cluster "$root" "$data" "$pg_bin/pg_ctl" 27; then status=1; fi
  exit "$status"
}
trap cleanup EXIT
port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"
"$pg_bin/initdb" -D "$data" --username="$test_role" --auth-local=trust --auth-host=trust \
  --encoding=UTF8 --locale=C.UTF-8 --no-instructions > "$root/initdb.log"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" -o "-h 127.0.0.1 -p $port -k $socket" start >/dev/null
"$pg_bin/createdb" -h 127.0.0.1 -p "$port" -U "$test_role" litecr_test
cd "$repo"
LITECR_TEST_DATABASE_URL="postgresql://$test_role@127.0.0.1:$port/litecr_test" \
  LITECR_MATCH_LOAD_RECORDS="$records" LITECR_MATCH_LOAD_REPORT="$report" \
  CARGO_BUILD_JOBS=1 cargo test --locked -p litecr-infrastructure --test matching_load -- --ignored --test-threads=1
printf 'Synthetic indexed matching load probe passed: distractors=%s report=%s\n' "$records" "$report"
