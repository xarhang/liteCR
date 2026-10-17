#!/usr/bin/env bash
set -euo pipefail

[[ $# == 1 && "$1" == /* && -x "$1" && ! -L "$1" ]] || {
  echo "usage: $0 /absolute/path/to/litecr-release-binary" >&2
  exit 2
}
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
release="$1"
pg_bin="$(pg_config --bindir)"
[[ -x "$pg_bin/initdb" ]] || { echo "PostgreSQL server binaries are missing" >&2; exit 2; }
export PATH="$pg_bin:$PATH"
[[ -x "$repo/tools/operations/backup-postgres.sh" && -x "$repo/tools/operations/restore-postgres-isolated.sh" ]] || { echo "backup/restore scripts missing or not executable" >&2; exit 2; }
command -v initdb >/dev/null
command -v pg_ctl >/dev/null
command -v pg_dump >/dev/null
command -v pg_restore >/dev/null
command -v psql >/dev/null
command -v createdb >/dev/null
unset PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGHOSTADDR PGOPTIONS PGDATABASE PGPORT PGHOST
db_user="$(id -un)"
[[ "$db_user" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || { echo "local PostgreSQL user name is unsupported" >&2; exit 2; }

root=$(mktemp -d /tmp/litecr-seqrestore.XXXXXXXX)
chmod 700 "$root"
root=$(realpath -e "$root")
[[ "$root" == /tmp/litecr-seqrestore.* ]] || { echo "unexpected temp root" >&2; exit 3; }
data="$root/data"
socket="$root/socket"
mkdir -m 700 "$socket"
port=$(python3 - <<'PY'
import socket
s=socket.socket()
s.bind(('127.0.0.1',0))
print(s.getsockname()[1])
s.close()
PY
)
started=0
cleanup() {
  if (( started )); then pg_ctl -D "$data" -m immediate stop >/dev/null 2>&1 || true; fi
  [[ "$root" == /tmp/litecr-seqrestore.* ]] || return 0
  rm -rf -- "$root"
}
trap cleanup EXIT

initdb -D "$data" --username="$db_user" --auth-local=trust --auth-host=trust --no-instructions >"$root/initdb.log"
pg_ctl -D "$data" -l "$root/postgres.log" -o "-h 127.0.0.1 -p $port -k $socket" start >/dev/null
started=1
export PGHOST=127.0.0.1 PGPORT="$port" PGUSER="$db_user" PGDATABASE=postgres
for _ in $(seq 1 50); do
  if pg_isready -h 127.0.0.1 -p "$port" -U "$db_user" >/dev/null 2>&1; then break; fi
  sleep 0.1
done
pg_isready -h 127.0.0.1 -p "$port" -U "$db_user"
[[ "$(psql -X -A -t -c 'SELECT host(inet_server_addr())')" == 127.0.0.1 ]]
[[ "$(sed -n '2p' "$data/postmaster.pid")" == "$data" ]]
createdb litecr
psql -X -v ON_ERROR_STOP=1 -d postgres <<'SQL'
CREATE ROLE litecr_restore_probe LOGIN INHERIT NOSUPERUSER CREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC;
SQL
if PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_restore_probe \
    psql -X -v ON_ERROR_STOP=1 -A -t --no-password -d postgres \
    -c 'SELECT system_identifier FROM pg_catalog.pg_control_system()' \
    >"$root/restore-privilege-before.txt" 2>&1; then
  echo "restore role unexpectedly has pg_control_system before its explicit grant" >&2
  exit 1
fi
psql -X -v ON_ERROR_STOP=1 -d postgres \
  -c 'GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO litecr_restore_probe' >/dev/null
[[ "$(psql -X -A -t -d postgres -c \
  "SELECT has_function_privilege('litecr_restore_probe', 'pg_catalog.pg_control_system()', 'EXECUTE')")" == t ]]
[[ "$(psql -X -A -t -d postgres -c \
  "SELECT rolsuper OR rolcreaterole OR rolreplication OR rolbypassrls FROM pg_roles WHERE rolname='litecr_restore_probe'")" == f ]]
export LITECR_MIGRATION_DATABASE_URL="postgres://$db_user@127.0.0.1:$port/litecr"
"$release" migrate
expected_schema="$("$release" version --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["schema_version"])')"
[[ "$(psql -X -A -t -d litecr -c 'SELECT version FROM litecr.schema_meta WHERE singleton=true')" == "$expected_schema" ]]

# Give all application sequences distinct, called states and capture the exact snapshot.
psql -X -v ON_ERROR_STOP=1 -d litecr <<'SQL'
DO $$
DECLARE item RECORD; next_value BIGINT := 41000;
BEGIN
  FOR item IN
    SELECT n.nspname, c.relname
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'litecr' AND c.relkind = 'S'
    ORDER BY c.relname
  LOOP
    PERFORM setval(format('%I.%I', item.nspname, item.relname), next_value, true);
    next_value := next_value + 101;
  END LOOP;
END $$;
SQL
cat >"$root/sequence-state.sql" <<'SQL'
SELECT format('%I.%I', n.nspname, c.relname)
FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = 'litecr' AND c.relkind = 'S'
ORDER BY n.nspname, c.relname;
SQL
snapshot_sequences() {
  local database="$1" output="$2" sequence
  : > "$output"
  while IFS= read -r sequence; do
    [[ "$sequence" =~ ^litecr\.[a-z0-9_]+$ ]] || { echo "unexpected sequence name" >&2; return 1; }
    psql -X -qAt -F '|' -d "$database" \
      -c "SELECT '$sequence', last_value, is_called FROM $sequence" >> "$output"
  done < <(psql -X -qAt -d "$database" -f "$root/sequence-state.sql")
}
snapshot_sequences litecr "$root/source-sequences.txt"
sequence_count="$(wc -l <"$root/source-sequences.txt")"
(( sequence_count > 0 ))
cat >"$root/backup.conf" <<EOF
LITECR_PG_DATABASE=litecr
LITECR_PG_USER=$db_user
LITECR_BACKUP_DIR=$root/backup
LITECR_RELEASE_ID=synthetic-schema12-sequence-check
LITECR_PGHOST=127.0.0.1
LITECR_PGPORT=$port
LITECR_MIN_FREE_KB=1
EOF
cat >"$root/restore.conf" <<EOF
LITECR_PG_USER=litecr_restore_probe
LITECR_PGHOST=127.0.0.1
LITECR_PGPORT=$port
LITECR_RESTORE_DB_OWNER=litecr_restore_probe
EOF
target_system_identifier="$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_restore_probe \
  psql -X -A -t -q -d postgres -c 'SELECT system_identifier FROM pg_catalog.pg_control_system()')"
[[ "$target_system_identifier" =~ ^[0-9]{1,20}$ ]]
printf 'format=litecr-isolated-postgres-target-v2\nsystem_identifier=%s\n' \
  "$target_system_identifier" >"$root/isolation-marker"
chmod 600 "$root/backup.conf" "$root/restore.conf" "$root/isolation-marker"
env -u PGHOST -u PGPORT -u PGUSER -u PGDATABASE \
  LITECR_CONFIG_FILE="$root/backup.conf" \
  bash "$repo/tools/operations/backup-postgres.sh" --archive-only
mapfile -t dumps < <(find "$root/backup" -maxdepth 1 -type f -name '*.dump' -print)
[[ ${#dumps[@]} -eq 1 ]]
env -u PGHOST -u PGPORT -u PGUSER -u PGDATABASE \
  LITECR_CONFIG_FILE="$root/restore.conf" \
  bash "$repo/tools/operations/restore-postgres-isolated.sh" --dump "${dumps[0]}" --target-db litecr_restore_seqcheck --isolation-marker "$root/isolation-marker"
psql -X -v ON_ERROR_STOP=1 -d postgres <<'SQL'
ALTER ROLE litecr_restore_probe NOCREATEDB;
REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM litecr_restore_probe;
SQL
[[ "$(psql -X -A -t -d litecr_restore_seqcheck -c 'SELECT version FROM litecr.schema_meta WHERE singleton=true')" == "$expected_schema" ]]
snapshot_sequences litecr_restore_seqcheck "$root/restored-sequences.txt"
diff -u "$root/source-sequences.txt" "$root/restored-sequences.txt"
printf 'Sequence snapshots identical (%s sequences):\n' "$sequence_count"
cat "$root/source-sequences.txt"
while IFS= read -r sequence; do
  [[ "$sequence" =~ ^litecr\.[a-z0-9_]+$ ]] || { echo "unexpected sequence name" >&2; exit 1; }
  expected_next="$(awk -F'|' -v name="$sequence" '$1 == name {print $2 + 1}' "$root/source-sequences.txt")"
  actual_next="$(psql -X -qAt -d litecr_restore_seqcheck -c "SELECT nextval('$sequence'::regclass)")"
  [[ "$actual_next" == "$expected_next" ]] || { echo "restored sequence did not advance: $sequence" >&2; exit 1; }
  printf '%s nextval=%s\n' "$sequence" "$actual_next"
done < <(psql -X -qAt -d litecr_restore_seqcheck -f "$root/sequence-state.sql")
printf 'Synthetic schema-%s backup/restore sequence-state verification passed on PostgreSQL %s.\n' "$expected_schema" "$(psql -X -A -t -d postgres -c 'SHOW server_version')"




