#!/usr/bin/env bash
set -euo pipefail
set +x

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready createdb; do
  [[ -x "$pg_bin/$tool" ]] || { echo "POSTGRESQL_TOOL_MISSING:$tool" >&2; exit 2; }
done
export PATH="$pg_bin:$PATH"
unset PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGHOSTADDR PGOPTIONS PGDATABASE PGPORT PGHOST
test_role="$(id -un)"
[[ "$test_role" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || { echo 'UNSUPPORTED_TEST_ROLE' >&2; exit 2; }

root="$(mktemp -d /tmp/litecr-mcp-audit.XXXXXXXX)"
chmod 700 "$root"
root="$(realpath -e -- "$root")"
[[ "$root" == /tmp/litecr-mcp-audit.* && -d "$root" && ! -L "$root" ]] || {
  echo 'MCP_AUDIT_TEMP_PATH_UNSAFE' >&2; exit 3;
}
printf 'litecr-mcp-audit-postgres-test-v1\n' > "$root/OWNER"
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
  if (( status != 0 )) && [[ -f "$root/postgres.log" ]]; then cat "$root/postgres.log" >&2; fi
  if (( started )); then "$pg_bin/pg_ctl" -D "$data" -m fast stop >/dev/null 2>&1 || status=1; fi
  if [[ "$root" == /tmp/litecr-mcp-audit.* && -f "$root/OWNER" && \
        "$(cat "$root/OWNER")" == litecr-mcp-audit-postgres-test-v1 ]]; then
    rm -rf -- "$root"
  else
    echo 'MCP_AUDIT_CLEANUP_REFUSED_UNOWNED_PATH' >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT

"$pg_bin/initdb" -D "$data" --username="$test_role" --auth-local=trust --auth-host=trust \
  --encoding=UTF8 --locale=C.UTF-8 --no-instructions >"$root/initdb.log"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" \
  -o "-h 127.0.0.1 -p $port -k $socket" start >/dev/null
started=1
for _ in $(seq 1 50); do
  if "$pg_bin/pg_isready" -h 127.0.0.1 -p "$port" -U "$test_role" >/dev/null 2>&1; then break; fi
  sleep 0.1
done
"$pg_bin/createdb" -h 127.0.0.1 -p "$port" -U "$test_role" litecr_test
cd "$repo"
LITECR_TEST_DATABASE_URL="postgresql://$test_role@127.0.0.1:$port/litecr_test" \
  CARGO_BUILD_JOBS=1 cargo +1.85.0 test --locked -p litecr-infrastructure --test access -- --test-threads=1
printf 'MCP credential/audit PostgreSQL integration passed (PostgreSQL %s).\n' \
  "$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER="$test_role" PGDATABASE=litecr_test \
    "$pg_bin/psql" -qAt -c 'SHOW server_version')"
