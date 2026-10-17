#!/usr/bin/env bash
set -euo pipefail
set +x

[[ $# == 1 && "$1" == /* && -x "$1" && ! -L "$1" && $(basename -- "$1") == litecr ]] || {
  echo "usage: $0 /absolute/path/to/litecr-current-binary" >&2
  exit 2
}
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
release="$(realpath -e -- "$1")"
pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready psql createdb; do
  [[ -x "$pg_bin/$tool" ]] || { echo "POSTGRESQL16_TOOL_MISSING" >&2; exit 2; }
done
binary_schema_version="$("$release" version --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["schema_version"])')"
[[ "$binary_schema_version" =~ ^(27|28|29|30|31)$ ]] || {
  echo 'SCHEMA_27_28_29_30_OR_31_BINARY_REQUIRED' >&2
  exit 2
}
[[ -f "$repo/tools/operations/grant-runtime-database-access.sql" && ! -L "$repo/tools/operations/grant-runtime-database-access.sql" ]] || {
  echo 'ROLE_ACL_SQL_MISSING' >&2
  exit 2
}

export PATH="$pg_bin:$PATH"
unset PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGHOSTADDR PGOPTIONS PGDATABASE PGPORT PGHOST
bootstrap_role="$(id -un)"
[[ "$bootstrap_role" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || { echo 'UNSUPPORTED_BOOTSTRAP_ROLE' >&2; exit 2; }

root="$(mktemp -d /tmp/litecr-rolecheck.XXXXXXXX)"
chmod 700 "$root"
root="$(realpath -e -- "$root")"
[[ "$root" == /tmp/litecr-rolecheck.* && -d "$root" && ! -L "$root" ]] || { echo 'ROLECHECK_TEMP_PATH_UNSAFE' >&2; exit 3; }
printf 'litecr-postgres-rolecheck-v1\n' > "$root/OWNER"
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
  if [[ "$root" == /tmp/litecr-rolecheck.* && -f "$root/OWNER" && "$(cat "$root/OWNER")" == litecr-postgres-rolecheck-v1 ]]; then
    rm -rf -- "$root"
  else
    echo 'ROLECHECK_CLEANUP_REFUSED_UNOWNED_PATH' >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT

"$pg_bin/initdb" -D "$data" --username="$bootstrap_role" --auth-local=trust --auth-host=trust --encoding=UTF8 --locale=C.UTF-8 --no-instructions >"$root/initdb.log"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" -o "-h 127.0.0.1 -p $port -k $socket" start >/dev/null
started=1
admin_url="postgresql://$bootstrap_role@127.0.0.1:$port/postgres"
psql_as() {
  local role="$1" database="$2"
  shift 2
  PGHOST=127.0.0.1 PGPORT="$port" PGUSER="$role" PGDATABASE="$database" \
    "$pg_bin/psql" -X --no-psqlrc "$@"
}
for _ in $(seq 1 50); do
  if "$pg_bin/pg_isready" -h 127.0.0.1 -p "$port" -U "$bootstrap_role" >/dev/null 2>&1; then break; fi
  sleep 0.1
done
[[ "$(psql_as "$bootstrap_role" postgres -qAt -c 'SELECT host(inet_server_addr())')" == 127.0.0.1 ]] || {
  echo 'ROLECHECK_CLUSTER_NOT_LOOPBACK' >&2
  exit 3
}

# This bootstrap superuser exists only inside the temporary cluster. No shared
# PostgreSQL service, database, or pre-existing role is queried or changed.
psql_as "$bootstrap_role" postgres -v ON_ERROR_STOP=1 <<'SQL'
CREATE ROLE litecr_migrator LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_runtime LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE DATABASE litecr OWNER litecr_migrator;
CREATE DATABASE litecr_extension_probe OWNER litecr_migrator;
SQL

migration_url="postgresql://litecr_migrator@127.0.0.1:$port/litecr"
runtime_url="postgresql://litecr_runtime@127.0.0.1:$port/litecr"
extension_probe_url="postgresql://litecr_runtime@127.0.0.1:$port/litecr_extension_probe"
LITECR_MIGRATION_DATABASE_URL="$migration_url" "$release" migrate
schema_version="$(psql_as litecr_migrator litecr -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')"
[[ "$schema_version" == "$binary_schema_version" ]] || { echo 'MIGRATION_SCHEMA_VERSION_MISMATCH' >&2; exit 1; }

LITECR_MIGRATION_DATABASE_URL="$migration_url" bash "$repo/tools/operations/grant-runtime-database-access.sh"

expect_denied() {
  local label="$1" url="$2" sql="$3"
  local role database
  case "$url" in
    *"/litecr_extension_probe") role=litecr_runtime; database=litecr_extension_probe ;;
    *"/litecr") role=litecr_runtime; database=litecr ;;
    *) echo 'ROLECHECK_TARGET_DATABASE_INVALID' >&2; exit 2 ;;
  esac
  if psql_as "$role" "$database" -v ON_ERROR_STOP=1 -c "$sql" >"$root/$label.out" 2>&1; then
    echo "${label}_UNEXPECTEDLY_SUCCEEDED" >&2
    exit 1
  fi
  if ! grep -Fq 'permission denied' "$root/$label.out"; then
    echo "${label}_FAILED_FOR_UNEXPECTED_REASON" >&2
    exit 1
  fi
  printf '%s denied as expected\n' "$label"
}

# Prove that app credentials cannot perform DDL, install a new extension, or
# update the SQLx ledger. The extension command runs against a pristine probe
# database in this same throwaway cluster and requires CREATE on that database.
available_citext="$(psql_as litecr_migrator litecr_extension_probe -qAt -c "SELECT count(*) FROM pg_available_extensions WHERE name='citext'")"
[[ "$available_citext" == 1 ]] || { echo 'CITEXT_PROBE_EXTENSION_UNAVAILABLE' >&2; exit 2; }
assert_matching_idempotency_acl() {
  local label="$1"
  local privileges
  privileges="$(psql_as "$bootstrap_role" litecr -qAt -c \
    "SELECT has_table_privilege('litecr_runtime', 'matching.request_idempotency', 'SELECT') \
       AND has_table_privilege('litecr_runtime', 'matching.request_idempotency', 'INSERT') \
       AND NOT has_table_privilege('litecr_runtime', 'matching.request_idempotency', 'UPDATE') \
       AND NOT has_table_privilege('litecr_runtime', 'matching.request_idempotency', 'DELETE')")"
  [[ "$privileges" == t ]] || { echo "${label}_MATCH_IDEMPOTENCY_ACL_MISMATCH" >&2; exit 1; }
}
assert_matching_idempotency_acl runtime
expect_denied runtime_create_schema "$runtime_url" 'CREATE SCHEMA litecr_runtime_must_not_create'
expect_denied runtime_create_extension "$extension_probe_url" 'CREATE EXTENSION citext'
expect_denied runtime_update_sqlx_ledger "$runtime_url" 'UPDATE public._sqlx_migrations SET success = success WHERE FALSE'
expect_denied runtime_update_mcp_invocation_audit "$runtime_url" \
  "UPDATE litecr.mcp_invocation_audit SET outcome = outcome WHERE FALSE"
expect_denied runtime_delete_mcp_invocation_audit "$runtime_url" \
  "DELETE FROM litecr.mcp_invocation_audit WHERE FALSE"
expect_denied runtime_truncate_mcp_invocation_audit "$runtime_url" \
  "TRUNCATE litecr.mcp_invocation_audit"
expect_mcp_credential_mutations_denied() {
  local prefix="$1"
  expect_denied "${prefix}_revive_mcp_credential" "$runtime_url" \
    "UPDATE litecr.access_mcp_stdio_credential SET revoked_at = NULL WHERE FALSE"
  expect_denied "${prefix}_replace_mcp_token_hash" "$runtime_url" \
    "UPDATE litecr.access_mcp_stdio_credential SET token_hash = decode(repeat('00', 32), 'hex') WHERE FALSE"
  expect_denied "${prefix}_extend_mcp_credential" "$runtime_url" \
    "UPDATE litecr.access_mcp_stdio_credential SET expires_at = expires_at + interval '1 day' WHERE FALSE"
  expect_denied "${prefix}_delete_mcp_credential" "$runtime_url" \
    "DELETE FROM litecr.access_mcp_stdio_credential WHERE FALSE"
  expect_denied "${prefix}_truncate_mcp_credential" "$runtime_url" \
    "TRUNCATE litecr.access_mcp_stdio_credential"
  expect_denied "${prefix}_insert_mcp_credential" "$runtime_url" \
    "INSERT INTO litecr.access_mcp_stdio_credential (token_hash, external_subject, audience, client_id, purpose_code, expires_at) SELECT decode(repeat('00', 32), 'hex'), 'synthetic', 'urn:litecr:mcp:stdio', 'client', 'purpose', now() + interval '1 minute' WHERE FALSE"
  expect_denied "${prefix}_widen_mcp_tool_grant" "$runtime_url" \
    "UPDATE litecr.access_mcp_stdio_credential_tool SET tool_id = 'registry.search_records' WHERE FALSE"
  expect_denied "${prefix}_delete_mcp_tool_grant" "$runtime_url" \
    "DELETE FROM litecr.access_mcp_stdio_credential_tool WHERE FALSE"
  expect_denied "${prefix}_truncate_mcp_tool_grant" "$runtime_url" \
    "TRUNCATE litecr.access_mcp_stdio_credential_tool"
  expect_denied "${prefix}_insert_mcp_tool_grant" "$runtime_url" \
    "INSERT INTO litecr.access_mcp_stdio_credential_tool (credential_id, tool_id) SELECT 1, 'registry.search_records' WHERE FALSE"
}
expect_mcp_credential_mutations_denied runtime
queue_table_exists="$(psql_as litecr_migrator litecr -qAt -c "SELECT to_regclass('litecr.consumer_job') IS NOT NULL")"
if [[ "$queue_table_exists" == t ]]; then
  expect_denied runtime_update_consumer_job "$runtime_url" \
    'UPDATE litecr.consumer_job SET state = state WHERE FALSE'
fi

runtime_doctor="$(LITECR_DATABASE_URL="$runtime_url" "$release" doctor 2>&1)" || {
  echo 'RUNTIME_DOCTOR_FAILED' >&2
  exit 1
}
[[ "$runtime_doctor" == *"schema: compatible ($schema_version)"* ]] || { echo 'RUNTIME_DOCTOR_SCHEMA_MISMATCH' >&2; exit 1; }

assert_name_retrieval_helpers() {
  local name_helper_result
  name_helper_result="$("$pg_bin/psql" "$runtime_url" -v ON_ERROR_STOP=1 -qAt -c \
    "SELECT matching.name_edit_within_v1('ສົມໃຈ','ສົມໄຈ',1) \
      AND matching.name_prefix_within_v1('Somchay','Somchai') \
      AND matching.name_parts_within_v1('Somchay Phom','Somchai Phommasane') \
      AND NOT matching.name_parts_within_v1('Somchai UniqueSurname','Somchai DifferentSurname')")"
  [[ "$name_helper_result" == t ]] || { echo 'RUNTIME_NAME_RETRIEVAL_HELPERS_FAILED' >&2; exit 1; }
}
assert_name_retrieval_helpers

# This integration scenario uses the real PostgresRegistryStore implementation
# and intentionally skips migration bootstrapping: the migration role already
# applied the embedded migrations; the runtime role only performs app CRUD.
cd "$repo"
LITECR_TEST_DATABASE_URL="$runtime_url" LITECR_TEST_ASSUME_MIGRATED=1 \
  CARGO_BUILD_JOBS=1 cargo test --locked -p litecr-infrastructure --test registry -- --test-threads=1

# A repeat migration invocation remains possible only via the dedicated role.
LITECR_MIGRATION_DATABASE_URL="$migration_url" "$release" migrate
# Reapplying grants after migration is the upgraded-schema ACL regression: the
# broad grant is removed again and runtime cannot rewrite existing MCP audits.
LITECR_MIGRATION_DATABASE_URL="$migration_url" bash "$repo/tools/operations/grant-runtime-database-access.sh"
assert_matching_idempotency_acl upgraded_runtime
assert_name_retrieval_helpers
expect_denied upgraded_runtime_update_mcp_invocation_audit "$runtime_url" \
  "UPDATE litecr.mcp_invocation_audit SET outcome = outcome WHERE FALSE"
expect_denied upgraded_runtime_delete_mcp_invocation_audit "$runtime_url" \
  "DELETE FROM litecr.mcp_invocation_audit WHERE FALSE"
expect_denied upgraded_runtime_truncate_mcp_invocation_audit "$runtime_url" \
  "TRUNCATE litecr.mcp_invocation_audit"
expect_mcp_credential_mutations_denied upgraded_runtime
printf 'Runtime/migration database-role verification passed (PostgreSQL %s; schema %s).\n' \
  "$(psql_as "$bootstrap_role" postgres -qAt -c 'SHOW server_version')" "$schema_version"
