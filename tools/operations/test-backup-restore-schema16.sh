#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

fail() { printf 'liteCR recovery rehearsal failed [%s]\n' "$1" >&2; exit 1; }
[[ $# -ge 1 && $# -le 2 && "$1" == /* && -f "$1" && -x "$1" && ! -L "$1" && $(basename -- "$1") == litecr ]] || fail RELEASE_BINARY
release="$(realpath -e -- "$1")"
schema_version="${2:-16}"
[[ "$schema_version" =~ ^(16|17|18|19|20|22|23|24|25|26|27)$ ]] || fail SUPPORTED_SCHEMA_VERSION_REQUIRED
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
source "$repo/tools/operations/cleanup-owned-postgres-cluster.sh"
operations_dir="${LITECR_TEST_OPERATIONS_DIR:-$repo/tools/operations}"
[[ "$operations_dir" == /* && -d "$operations_dir" && ! -L "$operations_dir" ]] || fail OPERATIONS_DIRECTORY
operations_dir="$(realpath -e -- "$operations_dir")"
for operations_tool in backup-postgres.sh restore-postgres-isolated.sh; do
  [[ -f "$operations_dir/$operations_tool" && -x "$operations_dir/$operations_tool" && ! -L "$operations_dir/$operations_tool" ]] || fail "OPERATIONS_TOOL_$operations_tool"
done
pg_bin="$(pg_config --bindir)"
expected_postgres_version="${LITECR_TEST_POSTGRES_VERSION_PREFIX:-$(pg_config --version | awk '{print $2}')}"
[[ "$expected_postgres_version" =~ ^[0-9]+\.[0-9]+$ ]] || fail EXPECTED_POSTGRES_VERSION_PREFIX
for tool in initdb pg_ctl pg_isready psql pg_dump pg_restore createdb; do
  [[ -x "$pg_bin/$tool" ]] || fail "POSTGRESQL_TOOL_$tool"
done
[[ "$(pg_config --version)" == "PostgreSQL $expected_postgres_version"* ]] || fail POSTGRESQL_CLIENT_VERSION_MISMATCH
command -v python3 >/dev/null 2>&1 || fail PYTHON_REQUIRED
command -v sha256sum >/dev/null 2>&1 || fail SHA256SUM_REQUIRED
command -v diff >/dev/null 2>&1 || fail DIFF_REQUIRED
[[ "$($release version --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["schema_version"])')" == "$schema_version" ]] || fail SCHEMA_BINARY_VERSION_MISMATCH

export PATH="$pg_bin:$PATH"
unset PGHOST PGHOSTADDR PGPORT PGDATABASE PGUSER PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGOPTIONS \
  PGCONNECT_TIMEOUT PGSSLMODE PGREQUIRESSL PGSSLCERT PGSSLKEY PGSSLROOTCERT PGSSLCRL PGSSLCRLDIR \
  PGSSLCOMPRESSION PGSSLCERTMODE PGSSLMINPROTOCOLVERSION PGSSLMAXPROTOCOLVERSION PGSSLKEYLOGFILE \
  PGCHANNELBINDING PGGSSENCMODE PGTARGETSESSIONATTRS PGLOADBALANCEHOSTS PGCLIENTENCODING PGREALM \
  PGGSSDELEGATION PGKRBSRVNAME PGAPPNAME
bootstrap_role="$(id -un)"
[[ "$bootstrap_role" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || fail BOOTSTRAP_ROLE

root="$(mktemp -d "/tmp/litecr-p6-recovery-schema${schema_version}.XXXXXXXX")"
chmod 700 "$root"
root="$(realpath -e -- "$root")"
[[ "$root" == "/tmp/litecr-p6-recovery-schema${schema_version}."* && -d "$root" && ! -L "$root" ]] || fail TEMP_ROOT
printf 'litecr-p6-recovery-schema%s-v1\n' "$schema_version" > "$root/OWNER"
chmod 600 "$root/OWNER"
data="$root/postgres-data"
socket="$root/postgres-socket"
mkdir -m 700 "$socket"
port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"
cleanup() {
  status=$?
  # A failed pg_ctl stop or ambiguous status must preserve PGDATA for recovery.
  # The helper checks ownership, then requires pg_ctl status=3 and no PID file.
  if ! litecr_cleanup_owned_postgres_cluster "$root" "$data" "$pg_bin/pg_ctl" "$schema_version"; then
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

"$pg_bin/initdb" -D "$data" --username="$bootstrap_role" --auth-local=trust --auth-host=trust \
  --encoding=UTF8 --locale=C.UTF-8 --no-instructions > "$root/initdb.log"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" \
  -o "-h 127.0.0.1 -p $port -k $socket -c listen_addresses=127.0.0.1 -c unix_socket_directories=$socket" start >/dev/null
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
[[ "$(psql_as "$bootstrap_role" postgres -qAt -c 'SELECT host(inet_server_addr())')" == 127.0.0.1 ]] || fail CLUSTER_NOT_LOOPBACK
[[ "$(psql_as "$bootstrap_role" postgres -qAt -c 'SHOW port')" == "$port" ]] || fail CLUSTER_PORT_MISMATCH
[[ "$(sed -n '2p' "$data/postmaster.pid")" == "$data" ]] || fail CLUSTER_DATA_DIRECTORY_MISMATCH
[[ "$(psql_as "$bootstrap_role" postgres -qAt -c 'SHOW server_version')" == "$expected_postgres_version"* ]] || fail POSTGRESQL_SERVER_VERSION_MISMATCH

source_db=litecr_p6_recovery_source
restored_db=litecr_restore_p6_recovery
psql_as "$bootstrap_role" postgres -v ON_ERROR_STOP=1 <<SQL
CREATE ROLE litecr_migrator LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_runtime LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_backup LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE DATABASE $source_db OWNER litecr_migrator;
SQL

migration_url="postgresql://litecr_migrator@127.0.0.1:$port/$source_db"
LITECR_MIGRATION_DATABASE_URL="$migration_url" "$release" migrate
[[ "$(psql_as litecr_migrator "$source_db" -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == "$schema_version" ]] || fail SOURCE_SCHEMA_VERSION

# Seed a small coherent synthetic graph and append-only audit/outbox evidence.
psql_as litecr_migrator "$source_db" -v ON_ERROR_STOP=1 <<'SQL'
BEGIN;
WITH person AS (
    INSERT INTO litecr.person (public_id, registry_id, organization_id)
    VALUES ('p6-recovery-person', 'p6-recovery-registry', 'p6-recovery-org')
    RETURNING person_id
), source_record AS (
    INSERT INTO litecr.source_record
        (registry_id, organization_id, source_system_id, source_record_key, person_id)
    SELECT 'p6-recovery-registry', 'p6-recovery-org', 'synthetic-system', 'p6-recovery-source', person_id
    FROM person
    RETURNING source_record_id
)
INSERT INTO litecr.source_record_version
    (source_record_id, receive_version, source_version, payload_hash, version_state)
SELECT source_record_id, 1, 'fixture-v1', decode(repeat('ab', 32), 'hex'), 'accepted'
FROM source_record;

INSERT INTO litecr.source_assertion (source_record_id, receive_version, field_path, original_value, language_tag, script)
SELECT source_record_id, 1, 'name.given', 'Synthetic Recovery', 'en', 'Latn'
FROM litecr.source_record WHERE source_record_key = 'p6-recovery-source';
INSERT INTO litecr.person_alias (registry_id, organization_id, alias_value, person_id)
SELECT registry_id, organization_id, 'p6-recovery-alias', person_id
FROM litecr.person WHERE public_id = 'p6-recovery-person';
INSERT INTO litecr.mutation_audit
    (actor_kind, actor_id, registry_id, organization_id, action, resource_type, resource_ref,
     outcome, request_id, resource_version)
VALUES ('system', 'p6-recovery-fixture', 'p6-recovery-registry', 'p6-recovery-org',
        'fixture_seeded', 'Person', 'p6-recovery-person', 'accepted', 'p6-recovery-request', 1);
INSERT INTO litecr.outbox_event
    (event_type, schema_version, registry_id, organization_id, aggregate_type, aggregate_id,
     aggregate_version, actor_kind, actor_id, correlation_id, payload)
SELECT 'litecr.registry.person_registered', 1, registry_id, organization_id, 'Person', person_id,
       1, 'system', 'p6-recovery-fixture', 'p6-recovery-correlation', '{"fixture":"p6-recovery"}'::jsonb
FROM litecr.person WHERE public_id = 'p6-recovery-person';
COMMIT;
SQL

if [[ "$schema_version" =~ ^(17|18|19|20|22|23|24|25|26|27)$ ]]; then
  # Include a genuine matching evidence row with a complete API
  # snapshot and a linked candidate JSON document. All values are synthetic.
  psql_as litecr_migrator "$source_db" -v ON_ERROR_STOP=1 <<'SQL'
BEGIN;
WITH new_person AS (
    INSERT INTO litecr.person (public_id, registry_id, organization_id)
    VALUES ('p6-recovery-candidate-person', 'p6-recovery-registry', 'p6-recovery-org')
    RETURNING person_id
), source_record AS (
    INSERT INTO litecr.source_record
        (registry_id, organization_id, source_system_id, source_record_key, person_id)
    SELECT 'p6-recovery-registry', 'p6-recovery-org', 'synthetic-system',
           'p6-recovery-candidate', person_id
    FROM new_person
    RETURNING source_record_id
), source_version AS (
    INSERT INTO litecr.source_record_version
        (source_record_id, receive_version, source_version, payload_hash, version_state)
    SELECT source_record_id, 1, 'fixture-candidate-v1', decode(repeat('cd', 32), 'hex'), 'accepted'
    FROM source_record
    RETURNING source_record_id, receive_version
)
INSERT INTO litecr.source_assertion
    (source_record_id, receive_version, field_path, original_value, language_tag, script)
SELECT source_record_id, receive_version, 'name.given', 'Synthetic Recovery Candidate', 'en', 'Latn'
FROM source_version;

INSERT INTO matching.evidence_run
    (run_id, registry_id, organization_id, query_source_record_id, query_receive_version,
     query_source_version, engine_version, normalization_version, candidate_version,
     feature_version, policy_version, source_snapshot_digest, candidate_count,
     candidates_truncated, actor_id, request_id, result_snapshot)
SELECT '17000000-0000-4000-8000-000000000017'::uuid,
       'p6-recovery-registry', 'p6-recovery-org', query.source_record_id, 1, 'fixture-v1',
       'litecr-match-v1', 'synthetic-normalization-v1', 'synthetic-candidate-v1',
       'synthetic-feature-v1', 'synthetic-policy-v1', decode(repeat('ef', 32), 'hex'),
       1, FALSE, 'p6-recovery-fixture', 'p6-recovery-match-request',
       jsonb_build_object(
           'versions', jsonb_build_object(
               'engine', 'litecr-match-v1', 'normalization', 'synthetic-normalization-v1',
               'candidate', 'synthetic-candidate-v1', 'feature', 'synthetic-feature-v1',
               'policy', 'synthetic-policy-v1'),
           'evidence_run_id', '17000000-0000-4000-8000-000000000017',
           'query_record_id', query.source_record_id::text,
           'query_source_version', 'fixture-v1', 'query_receive_version', 1,
           'candidates', jsonb_build_array(jsonb_build_object(
               'candidate_record_id', candidate.source_record_id::text,
               'candidate_source_version', 'fixture-candidate-v1',
               'candidate_receive_version', 1, 'passes', jsonb_build_array('exact_name'),
               'score_points', 10, 'score_type', 'raw_heuristic_points_not_probability',
               'outcome', 'review', 'fields', '[]'::jsonb, 'hard_conflicts', '[]'::jsonb)),
           'retrieval', jsonb_build_object(
               'per_pass_candidates', jsonb_build_object('exact_name', 1),
               'union_before_limit', 1, 'returned_candidates', 1, 'truncated', FALSE,
               'limit', 20, 'complete', TRUE),
           'mutation_authority', 'registry-only; advisory output cannot link or merge identities',
           'snapshot_complete', TRUE)
FROM litecr.source_record query, litecr.source_record candidate
WHERE query.source_record_key = 'p6-recovery-source'
  AND candidate.source_record_key = 'p6-recovery-candidate';

INSERT INTO matching.evidence_candidate
    (run_id, candidate_source_record_id, candidate_receive_version,
     candidate_source_version, score_points, score_type, advisory_outcome, evidence)
SELECT run_id, candidate.source_record_id, 1, 'fixture-candidate-v1', 10,
       'raw_heuristic_points_not_probability', 'review',
       result_snapshot -> 'candidates' -> 0
FROM matching.evidence_run run
JOIN litecr.source_record candidate
  ON candidate.source_record_key = 'p6-recovery-candidate'
WHERE run.run_id = '17000000-0000-4000-8000-000000000017'::uuid;

COMMIT;
SQL
  if [[ "$schema_version" =~ ^(26|27)$ ]]; then
    # Schema 26 adds digest-only API replay rows; include a synthetic one so
    # the generic source/restore table digest proves it survives recovery.
    psql_as litecr_migrator "$source_db" -v ON_ERROR_STOP=1 <<'SQL'
INSERT INTO matching.request_idempotency
    (actor_digest, registry_id, organization_id, query_source_record_id,
     key_digest, request_digest, evidence_run_id)
SELECT decode(repeat('11', 32), 'hex'), run.registry_id, run.organization_id,
       run.query_source_record_id, decode(repeat('22', 32), 'hex'),
       decode(repeat('33', 32), 'hex'), run.run_id
FROM matching.evidence_run run
WHERE run.run_id = '17000000-0000-4000-8000-000000000017'::uuid;
SQL
    [[ "$(psql_as litecr_migrator "$source_db" -qAt -c \
      "SELECT count(*) FROM matching.request_idempotency WHERE evidence_run_id='17000000-0000-4000-8000-000000000017'::uuid")" == 1 ]] \
      || fail SOURCE_MATCH_IDEMPOTENCY_FIXTURE
  fi
fi

if [[ "$schema_version" =~ ^(19|20|22|23|24|25|26|27)$ ]]; then
  # Schema 19 introduced migration:write as an explicit grant. Preserve one
  # scoped synthetic grant in the archive so restoration proves this value is
  # accepted and retained by the upgraded permission constraint.
  psql_as litecr_migrator "$source_db" -v ON_ERROR_STOP=1 <<'SQL'
INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject)
VALUES ('human', 'https://synthetic.example.test/issuer', 'p6-migration-writer');
INSERT INTO litecr.access_grant
    (actor_kind, issuer_url, external_subject, registry_id, organization_id,
     record_ids, role_name, permission_name)
VALUES ('human', 'https://synthetic.example.test/issuer', 'p6-migration-writer',
        'p6-recovery-registry', 'p6-recovery-org', NULL, NULL, 'migration:write');
SQL
fi

if [[ "$schema_version" =~ ^(22|23|24|25|26|27)$ ]]; then
  # Schema 22 stores only the internal actor key on an access-denial event.
  psql_as litecr_migrator "$source_db" -v ON_ERROR_STOP=1 <<'SQL'
INSERT INTO litecr.access_security_audit
    (event_type, reason_code, route_family, scope_shape, actor_kind,
     actor_identity_key, request_id)
SELECT 'protected_route_denied', 'permission_denied', 'migration', 'none',
       identity.actor_kind, identity.identity_key, 'p6-internal-actor-restored-audit'
FROM litecr.access_identity AS identity
WHERE identity.actor_kind = 'human'
  AND identity.issuer_url = 'https://synthetic.example.test/issuer'
  AND identity.external_subject = 'p6-migration-writer';
SQL
  [[ "$(psql_as litecr_migrator "$source_db" -qAt -c \
    "SELECT count(*) FROM litecr.access_security_audit audit JOIN litecr.access_identity identity ON identity.identity_key=audit.actor_identity_key AND identity.actor_kind=audit.actor_kind WHERE audit.request_id='p6-internal-actor-restored-audit' AND identity.external_subject='p6-migration-writer'")" == 1 ]] || fail SOURCE_INTERNAL_ACTOR_AUDIT
fi

# Recreate the runtime's documented ACL and a separate read-only backup identity.
LITECR_MIGRATION_DATABASE_URL="$migration_url" bash "$repo/tools/operations/grant-runtime-database-access.sh" > "$root/runtime-acl-source.log"
grant_backup_access() {
  local database="$1"
  psql_as litecr_migrator "$database" -v ON_ERROR_STOP=1 <<'SQL'
DO $$BEGIN EXECUTE format('GRANT CONNECT ON DATABASE %I TO litecr_backup', current_database()); END$$;
GRANT USAGE ON SCHEMA litecr, matching TO litecr_backup;
GRANT SELECT ON ALL TABLES IN SCHEMA litecr, matching TO litecr_backup;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA litecr, matching TO litecr_backup;
GRANT SELECT ON TABLE public._sqlx_migrations TO litecr_backup;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA litecr GRANT SELECT ON TABLES TO litecr_backup;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA matching GRANT SELECT ON TABLES TO litecr_backup;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA litecr GRANT USAGE, SELECT ON SEQUENCES TO litecr_backup;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA matching GRANT USAGE, SELECT ON SEQUENCES TO litecr_backup;
SQL
}
grant_backup_access "$source_db"
psql_as "$bootstrap_role" "$source_db" -v ON_ERROR_STOP=1 <<'SQL'
DO $$
DECLARE item RECORD; next_value BIGINT := 50000;
BEGIN
  FOR item IN
    SELECT n.nspname, c.relname
    FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
    WHERE n.nspname IN ('litecr','matching','public') AND c.relkind='S'
    ORDER BY n.nspname,c.relname
  LOOP
    PERFORM setval(format('%I.%I', item.nspname, item.relname), next_value, true);
    next_value := next_value + 97;
  END LOOP;
END $$;
SQL

table_snapshot() {
  local database="$1" destination="$2" items schema table result
  : > "$destination"
  items="$(psql_as "$bootstrap_role" "$database" -v ON_ERROR_STOP=1 -qAt -F '|' -c \
    "SELECT n.nspname, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ('litecr','matching','public') AND c.relkind IN ('r','p') ORDER BY n.nspname,c.relname")" || fail TABLE_SNAPSHOT_LIST
  while IFS='|' read -r schema table; do
    [[ "$schema" =~ ^(litecr|matching|public)$ && "$table" =~ ^[a-z0-9_]+$ ]] || fail TABLE_SNAPSHOT_NAME
    result="$(psql_as "$bootstrap_role" "$database" -v ON_ERROR_STOP=1 -qAt -F '|' -c \
      "SELECT count(*), md5(coalesce(string_agg(to_jsonb(t)::text, E'\\n' ORDER BY to_jsonb(t)::text), '')) FROM \"$schema\".\"$table\" AS t")" || fail TABLE_SNAPSHOT_QUERY
    printf '%s|%s|%s\n' "$schema" "$table" "$result" >> "$destination"
  done <<< "$items"
}
matching_snapshot_snapshot() {
  local database="$1" destination="$2"
  if [[ "$schema_version" =~ ^(17|18|19|20|22|23|24|25|26|27)$ ]]; then
    psql_as "$bootstrap_role" "$database" -v ON_ERROR_STOP=1 -qAt -F '|' > "$destination" <<'SQL'
SELECT count(*),
       md5(coalesce(string_agg(er.run_id::text || ':' || er.result_snapshot::text,
                               E'\n' ORDER BY er.run_id), '')),
       bool_and(result_snapshot->>'snapshot_complete'='true'
                AND result_snapshot->'retrieval'->>'complete'='true'
                AND jsonb_array_length(result_snapshot->'candidates')=1
                AND result_snapshot->>'evidence_run_id'=er.run_id::text
                AND result_snapshot->>'query_record_id'=er.query_source_record_id::text
                AND (result_snapshot->>'query_receive_version')::bigint=er.query_receive_version
                AND ec.candidate_source_record_id::text=
                    result_snapshot->'candidates'->0->>'candidate_record_id'
                AND ec.evidence=result_snapshot->'candidates'->0),
       count(*) FILTER (WHERE ec.run_id IS NOT NULL)
FROM matching.evidence_run er
LEFT JOIN matching.evidence_candidate ec
  ON ec.run_id=er.run_id
WHERE er.run_id='17000000-0000-4000-8000-000000000017'::uuid
GROUP BY er.run_id;
SQL
    [[ -s "$destination" ]] || fail MATCHING_SNAPSHOT_MISSING
    [[ "$(cut -d'|' -f1 "$destination")" == 1 ]] || fail MATCHING_SNAPSHOT_COUNT
    [[ "$(cut -d'|' -f3 "$destination")" == t ]] || fail MATCHING_SNAPSHOT_SHAPE
    [[ "$(cut -d'|' -f4 "$destination")" == 1 ]] || fail MATCHING_CANDIDATE_COUNT
  else
    : > "$destination"
  fi
}
sequence_snapshot() {
  local database="$1" destination="$2" sequences sequence state
  : > "$destination"
  sequences="$(psql_as "$bootstrap_role" "$database" -v ON_ERROR_STOP=1 -qAt -c \
    "SELECT format('%I.%I', n.nspname, c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ('litecr','matching','public') AND c.relkind='S' ORDER BY n.nspname,c.relname")" || fail SEQUENCE_SNAPSHOT_LIST
  while IFS= read -r sequence; do
    [[ "$sequence" =~ ^(litecr|matching|public)\.[a-z0-9_]+$ ]] || fail SEQUENCE_SNAPSHOT_NAME
    state="$(psql_as "$bootstrap_role" "$database" -v ON_ERROR_STOP=1 -qAt -F '|' -c \
      "SELECT last_value, is_called FROM $sequence")" || fail SEQUENCE_SNAPSHOT_QUERY
    printf '%s|%s\n' "$sequence" "$state" >> "$destination"
  done <<< "$sequences"
}
owner_snapshot() {
  local database="$1" destination="$2"
  psql_as "$bootstrap_role" "$database" -v ON_ERROR_STOP=1 -qAt -F '|' > "$destination" <<'SQL'
SELECT 'database|' || pg_get_userbyid(datdba) FROM pg_database WHERE datname=current_database();
SELECT 'schema|' || nspname || '|' || pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname IN ('public','litecr','matching') ORDER BY nspname;
SELECT 'relation|' || n.nspname || '|' || c.relname || '|' || c.relkind::text || '|' || pg_get_userbyid(c.relowner)
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname IN ('public','litecr','matching') AND c.relkind IN ('r','p','S','v','m','f')
ORDER BY n.nspname,c.relname;
SELECT 'function|' || n.nspname || '|' || p.proname || '|' || pg_get_function_identity_arguments(p.oid)
       || '|' || pg_get_userbyid(p.proowner) || '|' || p.prosecdef || '|'
       || coalesce(p.proconfig::text,'NULL') || '|' || md5(pg_get_functiondef(p.oid))
FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
WHERE n.nspname IN ('public','litecr','matching') AND p.prokind='f'
ORDER BY n.nspname,p.proname,pg_get_function_identity_arguments(p.oid);
SQL
}
acl_snapshot() {
  local database="$1" destination="$2"
  psql_as "$bootstrap_role" "$database" -v ON_ERROR_STOP=1 -qAt -F '|' > "$destination" <<'SQL'
SELECT 'database|' || coalesce(datacl::text, 'NULL') FROM pg_database WHERE datname=current_database();
SELECT 'schema|' || nspname || '|' || coalesce(nspacl::text, 'NULL') FROM pg_namespace
WHERE nspname IN ('public','litecr','matching') ORDER BY nspname;
SELECT 'relation|' || n.nspname || '|' || c.relname || '|' || c.relkind::text || '|' || coalesce(c.relacl::text, 'NULL')
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname IN ('public','litecr','matching') AND c.relkind IN ('r','p','S')
ORDER BY n.nspname,c.relname;
SELECT 'default|' || pg_get_userbyid(d.defaclrole) || '|' || coalesce(n.nspname, '') || '|'
       || d.defaclobjtype::text || '|' || coalesce(d.defaclacl::text, 'NULL')
FROM pg_default_acl d LEFT JOIN pg_namespace n ON n.oid=d.defaclnamespace
WHERE pg_get_userbyid(d.defaclrole) IN ('litecr_migrator','litecr_runtime','litecr_backup')
ORDER BY pg_get_userbyid(d.defaclrole), coalesce(n.nspname, ''), d.defaclobjtype;
SELECT 'function|' || n.nspname || '|' || p.proname || '|' || pg_get_function_identity_arguments(p.oid)
       || '|' || coalesce(p.proacl::text,'NULL') || '|runtime|'
       || has_function_privilege('litecr_runtime',p.oid,'EXECUTE') || '|backup|'
       || has_function_privilege('litecr_backup',p.oid,'EXECUTE')
FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
WHERE n.nspname IN ('public','litecr','matching') AND p.prokind='f'
ORDER BY n.nspname,p.proname,pg_get_function_identity_arguments(p.oid);
SELECT 'runtime-db|' || has_database_privilege('litecr_runtime',current_database(),'CONNECT') || '|'
       || has_database_privilege('litecr_runtime',current_database(),'CREATE') || '|'
       || has_database_privilege('litecr_runtime',current_database(),'TEMP');
SELECT 'backup-db|' || has_database_privilege('litecr_backup',current_database(),'CONNECT') || '|'
       || has_database_privilege('litecr_backup',current_database(),'CREATE') || '|'
       || has_database_privilege('litecr_backup',current_database(),'TEMP');
SELECT 'effective-schema|' || nspname || '|runtime|' || has_schema_privilege('litecr_runtime',nspname,'USAGE') || '|'
       || has_schema_privilege('litecr_runtime',nspname,'CREATE') || '|backup|'
       || has_schema_privilege('litecr_backup',nspname,'USAGE') || '|' || has_schema_privilege('litecr_backup',nspname,'CREATE')
FROM pg_namespace WHERE nspname IN ('public','litecr','matching') ORDER BY nspname;
SELECT 'effective-table|' || n.nspname || '.' || c.relname || '|'
       || has_table_privilege('litecr_runtime',c.oid,'SELECT') || '|'
       || has_table_privilege('litecr_runtime',c.oid,'INSERT') || '|'
       || has_table_privilege('litecr_runtime',c.oid,'UPDATE') || '|'
       || has_table_privilege('litecr_runtime',c.oid,'DELETE') || '|'
       || has_table_privilege('litecr_backup',c.oid,'SELECT') || '|'
       || has_table_privilege('litecr_backup',c.oid,'INSERT') || '|'
       || has_table_privilege('litecr_backup',c.oid,'UPDATE') || '|'
       || has_table_privilege('litecr_backup',c.oid,'DELETE')
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname IN ('public','litecr','matching') AND c.relkind IN ('r','p')
ORDER BY n.nspname,c.relname;
SELECT 'effective-sequence|' || n.nspname || '.' || c.relname || '|'
       || has_sequence_privilege('litecr_runtime',c.oid,'USAGE') || '|'
       || has_sequence_privilege('litecr_runtime',c.oid,'SELECT') || '|'
       || has_sequence_privilege('litecr_backup',c.oid,'USAGE') || '|'
       || has_sequence_privilege('litecr_backup',c.oid,'SELECT')
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname IN ('public','litecr','matching') AND c.relkind='S'
ORDER BY n.nspname,c.relname;
SQL
}
role_snapshot() {
  local destination="$1"
  psql_as "$bootstrap_role" postgres -v ON_ERROR_STOP=1 -qAt -F '|' > "$destination" <<'SQL'
SELECT 'role|' || rolname || '|' || rolsuper || '|' || rolinherit || '|' || rolcreaterole || '|'
       || rolcreatedb || '|' || rolcanlogin || '|' || rolreplication || '|' || rolbypassrls
FROM pg_roles WHERE rolname IN ('litecr_migrator','litecr_runtime','litecr_backup') ORDER BY rolname;
SELECT 'membership|' || pg_get_userbyid(m.roleid) || '|' || pg_get_userbyid(m.member) || '|' || m.admin_option
FROM pg_auth_members m JOIN pg_roles granted ON granted.oid=m.roleid JOIN pg_roles member ON member.oid=m.member
WHERE granted.rolname IN ('litecr_migrator','litecr_runtime','litecr_backup')
   OR member.rolname IN ('litecr_migrator','litecr_runtime','litecr_backup')
ORDER BY granted.rolname,member.rolname;
SQL
}

source_data="$root/source-data.txt"
source_matching_snapshot="$root/source-matching-snapshot.txt"
source_sequences="$root/source-sequences.txt"
source_owners="$root/source-owners.txt"
source_acls="$root/source-acls.txt"
source_roles="$root/source-roles.txt"
table_snapshot "$source_db" "$source_data"
matching_snapshot_snapshot "$source_db" "$source_matching_snapshot"
sequence_snapshot "$source_db" "$source_sequences"
(( $(wc -l < "$source_sequences") > 0 )) || fail NO_APPLICATION_SEQUENCES
owner_snapshot "$source_db" "$source_owners"
acl_snapshot "$source_db" "$source_acls"
role_snapshot "$source_roles"
[[ "$(psql_as litecr_runtime "$source_db" -qAt -c "SELECT count(*) FROM litecr.person WHERE public_id='p6-recovery-person'")" == 1 ]] || fail RUNTIME_ROLE_READ
[[ "$(psql_as litecr_backup "$source_db" -qAt -c 'SELECT count(*) FROM litecr.schema_meta')" == 1 ]] || fail BACKUP_ROLE_READ
if [[ "$schema_version" =~ ^(19|20|22|23|24|25|26|27)$ ]]; then
  [[ "$(psql_as litecr_migrator "$source_db" -qAt -c \
    "SELECT count(*) FROM litecr.access_grant WHERE external_subject='p6-migration-writer' AND permission_name='migration:write' AND registry_id='p6-recovery-registry' AND organization_id='p6-recovery-org'")" == 1 ]] || fail SOURCE_MIGRATION_WRITE_GRANT
fi

cat > "$root/backup.conf" <<EOF
LITECR_PG_DATABASE=$source_db
LITECR_PG_USER=litecr_backup
LITECR_BACKUP_DIR=$root/backups
LITECR_RELEASE_ID=phase6-schema${schema_version}-recovery-check
LITECR_PGHOST=127.0.0.1
LITECR_PGPORT=$port
LITECR_MIN_FREE_KB=1
EOF
cat > "$root/restore.conf" <<EOF
LITECR_PG_USER=litecr_migrator
LITECR_PGHOST=127.0.0.1
LITECR_PGPORT=$port
LITECR_RESTORE_DB_OWNER=litecr_migrator
EOF
target_system_identifier="$(psql_as "$bootstrap_role" postgres -qAt -c 'SELECT system_identifier FROM pg_catalog.pg_control_system()')"
[[ "$target_system_identifier" =~ ^[0-9]{1,20}$ ]] || fail TARGET_SYSTEM_IDENTIFIER
printf 'format=litecr-isolated-postgres-target-v2\nsystem_identifier=%s\n' \
  "$target_system_identifier" > "$root/isolation-marker"
chmod 600 "$root/backup.conf" "$root/restore.conf" "$root/isolation-marker"
LITECR_CONFIG_FILE="$root/backup.conf" bash "$operations_dir/backup-postgres.sh" --archive-only
mapfile -t dumps < <(find "$root/backups" -maxdepth 1 -type f -name '*.dump' -print)
(( ${#dumps[@]} == 1 )) || fail BACKUP_ARCHIVE_COUNT
success_marker="${dumps[0]%.dump}.success"
[[ -f "$success_marker" && ! -L "$success_marker" ]] || fail BACKUP_SUCCESS_MARKER_MISSING
[[ "$(stat -c '%a' "$success_marker")" == 600 ]] || fail BACKUP_SUCCESS_MARKER_MODE
grep -Fxq 'format=litecr-backup-success-v1' "$success_marker" || fail BACKUP_SUCCESS_MARKER_FORMAT
grep -Fxq "backup_id=$(basename "${dumps[0]%.dump}")" "$success_marker" || fail BACKUP_SUCCESS_MARKER_ID
grep -Fxq "dump_file=$(basename "${dumps[0]}")" "$success_marker" || fail BACKUP_SUCCESS_MARKER_DUMP
expected_dump_hash="$(sha256sum -- "${dumps[0]}" | awk '{print $1}')"
grep -Fxq "sha256=$expected_dump_hash" "$success_marker" || fail BACKUP_SUCCESS_MARKER_DIGEST
mapfile -t markers < <(find "$root/backups" -maxdepth 1 -type f -name '*.success' -print)
(( ${#markers[@]} == 1 )) || fail BACKUP_SUCCESS_MARKER_COUNT

# Fault-inject only the final success-marker rename. The failed publication
# must remove its partial set and preserve the earlier completed backup.
mkdir -m 700 "$root/fail-marker-mv"
cat > "$root/fail-marker-mv/mv" <<'SH'
#!/usr/bin/env bash
for arg in "$@"; do last_arg="$arg"; done
if [[ "${last_arg:-}" == *.success ]]; then exit 97; fi
exec /usr/bin/mv "$@"
SH
chmod 700 "$root/fail-marker-mv/mv"
set +e
LITECR_CONFIG_FILE="$root/backup.conf" PATH="$root/fail-marker-mv:$PATH" \
  bash "$operations_dir/backup-postgres.sh" --archive-only > "$root/marker-failure.log" 2>&1
marker_failure_status=$?
set -e
(( marker_failure_status != 0 )) || fail MARKER_RENAME_FAILURE_NOT_PROPAGATED
mapfile -t markers_after_failure < <(find "$root/backups" -maxdepth 1 -type f -name '*.success' -print)
(( ${#markers_after_failure[@]} == 1 )) && [[ "${markers_after_failure[0]}" == "$success_marker" ]] || fail MARKER_PUBLISHED_AFTER_FAILURE
for extension in dump manifest sha256; do
  mapfile -t artifacts_after_failure < <(find "$root/backups" -maxdepth 1 -type f -name "*.$extension" -print)
  (( ${#artifacts_after_failure[@]} == 1 )) || fail FAILED_BACKUP_ARTIFACT_INSPECTION
  [[ "${artifacts_after_failure[0]}" == "${dumps[0]%.dump}.$extension" ]] || fail FAILED_BACKUP_ARTIFACT_INSPECTION
done
mapfile -t temporary_artifacts_after_failure < <(find "$root/backups" -maxdepth 1 -type f -name '.litecr-*' -print)
(( ${#temporary_artifacts_after_failure[@]} == 0 )) || fail FAILED_BACKUP_TEMPORARY_CLEANUP

# Simulate recovery to a fresh database/security catalog: the database dump
# does not contain global roles. Remove only the source database and test roles
# in this owned cluster, then recreate the frozen role baseline from the
# runbook. This makes role/ACL recovery explicit rather than inheriting roles
# accidentally from the source database.
psql_as "$bootstrap_role" postgres -v ON_ERROR_STOP=1 <<SQL
DROP DATABASE $source_db;
DROP ROLE litecr_backup, litecr_runtime, litecr_migrator;
CREATE ROLE litecr_migrator LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_runtime LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_backup LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC;
SQL
recreated_roles="$root/recreated-roles.txt"
role_snapshot "$recreated_roles"
diff -u "$source_roles" "$recreated_roles" || fail ROLE_BOOTSTRAP_RECONCILIATION_MISMATCH

# The restore script needs CREATEDB to create one new target DB. Grant it only
# inside this disposable cluster, and a direct EXECUTE grant for its cluster
# identity check. Revoke CREATEDB immediately after restore.
if psql_as litecr_migrator postgres -v ON_ERROR_STOP=1 -qAt -c \
    'SELECT system_identifier FROM pg_catalog.pg_control_system()' >"$root/restore-function-denied.txt" 2>&1; then
  fail RESTORE_FUNCTION_PRIVILEGE_NOT_RESTRICTED
fi
psql_as "$bootstrap_role" postgres -v ON_ERROR_STOP=1 \
  -c 'GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO litecr_migrator' >/dev/null
[[ "$(psql_as "$bootstrap_role" postgres -qAt -c \
  "SELECT has_function_privilege('litecr_migrator', 'pg_catalog.pg_control_system()', 'EXECUTE')")" == t ]] \
  || fail RESTORE_FUNCTION_PRIVILEGE_GRANT
[[ "$(psql_as "$bootstrap_role" postgres -qAt -c \
  "SELECT rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls FROM pg_roles WHERE rolname='litecr_migrator'")" == f ]] \
  || fail RESTORE_ROLE_PRIVILEGES_BEFORE_CREATE
psql_as "$bootstrap_role" postgres -v ON_ERROR_STOP=1 -c 'ALTER ROLE litecr_migrator CREATEDB' >/dev/null
[[ "$(psql_as "$bootstrap_role" postgres -qAt -c \
  "SELECT rolcreatedb AND NOT rolsuper AND NOT rolcreaterole AND NOT rolreplication AND NOT rolbypassrls FROM pg_roles WHERE rolname='litecr_migrator'")" == t ]] \
  || fail RESTORE_ROLE_PRIVILEGES_DURING_CREATE
LITECR_CONFIG_FILE="$root/restore.conf" bash "$operations_dir/restore-postgres-isolated.sh" \
  --dump "${dumps[0]}" --target-db "$restored_db" --isolation-marker "$root/isolation-marker"
psql_as "$bootstrap_role" postgres -v ON_ERROR_STOP=1 -c 'ALTER ROLE litecr_migrator NOCREATEDB' >/dev/null
psql_as "$bootstrap_role" postgres -v ON_ERROR_STOP=1 \
  -c 'REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM litecr_migrator' >/dev/null
[[ "$(psql_as "$bootstrap_role" postgres -qAt -c \
  "SELECT has_function_privilege('litecr_migrator', 'pg_catalog.pg_control_system()', 'EXECUTE')")" == f ]] \
  || fail RESTORE_FUNCTION_PRIVILEGE_NOT_REVOKED
[[ "$(psql_as "$bootstrap_role" postgres -qAt -c \
  "SELECT rolcreatedb FROM pg_roles WHERE rolname='litecr_migrator'")" == f ]] \
  || fail RESTORE_CREATEDB_NOT_REVOKED

[[ "$(psql_as litecr_migrator "$restored_db" -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == "$schema_version" ]] || fail RESTORED_SCHEMA_VERSION
[[ "$(psql_as litecr_migrator "$restored_db" -qAt -c "SELECT count(*) FROM litecr.person WHERE public_id='p6-recovery-person'")" == 1 ]] || fail RESTORED_FIXTURE_ROW
[[ "$(psql_as "$bootstrap_role" "$restored_db" -qAt -c 'SELECT count(*) FROM litecr.mutation_audit')" == 1 ]] || fail RESTORED_AUDIT_ROW
[[ "$(psql_as "$bootstrap_role" "$restored_db" -qAt -c 'SELECT count(*) FROM litecr.outbox_event')" == 1 ]] || fail RESTORED_OUTBOX_ROW
if [[ "$schema_version" =~ ^(19|20|22|23|24|25|26|27)$ ]]; then
  [[ "$(psql_as litecr_migrator "$restored_db" -qAt -c \
    "SELECT count(*) FROM litecr.access_grant WHERE external_subject='p6-migration-writer' AND permission_name='migration:write' AND registry_id='p6-recovery-registry' AND organization_id='p6-recovery-org'")" == 1 ]] || fail RESTORED_MIGRATION_WRITE_GRANT
fi

# pg_dump --no-owner --no-acl does not restore service grants. Reconcile them
# with the shipped runtime policy and the external read-only backup policy.
restored_migration_url="postgresql://litecr_migrator@127.0.0.1:$port/$restored_db"
LITECR_MIGRATION_DATABASE_URL="$restored_migration_url" \
  bash "$repo/tools/operations/grant-runtime-database-access.sh" > "$root/runtime-acl-restored.log"
grant_backup_access "$restored_db"

restored_data="$root/restored-data.txt"
restored_matching_snapshot="$root/restored-matching-snapshot.txt"
restored_sequences="$root/restored-sequences.txt"
restored_owners="$root/restored-owners.txt"
restored_acls="$root/restored-acls.txt"
restored_roles="$root/restored-roles.txt"

if [[ "$schema_version" == 27 ]]; then
  [[ "$(psql_as litecr_runtime "$restored_db" -qAt -c \
    "SELECT matching.name_edit_within_v1('ສົມໃຈ','ສົມໄຈ',1) \
      AND matching.name_prefix_within_v1('Somchay','Somchai') \
      AND matching.name_parts_within_v1('Somchay Phom','Somchai Phommasane') \
      AND NOT matching.name_parts_within_v1('Somchai UniqueSurname','Somchai DifferentSurname')")" == t ]] \
    || fail RESTORED_NAME_RETRIEVAL_HELPERS
fi
table_snapshot "$restored_db" "$restored_data"
if [[ "$schema_version" =~ ^(26|27)$ ]]; then
  [[ "$(psql_as litecr_migrator "$restored_db" -qAt -c \
    "SELECT count(*) FROM matching.request_idempotency WHERE evidence_run_id='17000000-0000-4000-8000-000000000017'::uuid")" == 1 ]] \
    || fail RESTORED_MATCH_IDEMPOTENCY_FIXTURE
fi
matching_snapshot_snapshot "$restored_db" "$restored_matching_snapshot"
sequence_snapshot "$restored_db" "$restored_sequences"
owner_snapshot "$restored_db" "$restored_owners"
acl_snapshot "$restored_db" "$restored_acls"
role_snapshot "$restored_roles"
diff -u "$source_data" "$restored_data" || fail TABLE_CONTENT_MISMATCH
diff -u "$source_matching_snapshot" "$restored_matching_snapshot" || fail MATCHING_SNAPSHOT_DIGEST_MISMATCH
diff -u "$source_sequences" "$restored_sequences" || fail SEQUENCE_STATE_MISMATCH
diff -u "$source_owners" "$restored_owners" || fail OBJECT_OWNER_MISMATCH
diff -u "$source_acls" "$restored_acls" || fail ACL_RECONCILIATION_MISMATCH
diff -u "$source_roles" "$restored_roles" || fail ROLE_RECONCILIATION_MISMATCH
while IFS='|' read -r sequence last_value is_called; do
  [[ "$is_called" == t ]] || fail TEST_SEQUENCE_NOT_CALLED
  expected_next=$((last_value + 1))
  actual_next="$(psql_as "$bootstrap_role" "$restored_db" -v ON_ERROR_STOP=1 -qAt -c \
    "SELECT nextval('$sequence'::regclass)")" || fail RESTORED_SEQUENCE_NEXTVAL_QUERY
  [[ "$actual_next" == "$expected_next" ]] || fail RESTORED_SEQUENCE_NEXTVAL_MISMATCH
done < "$source_sequences"

# Verify restored role permissions still permit expected reads and deny DDL.
[[ "$(psql_as litecr_runtime "$restored_db" -qAt -c "SELECT count(*) FROM litecr.person WHERE public_id='p6-recovery-person'")" == 1 ]] || fail RESTORED_RUNTIME_READ
[[ "$(psql_as litecr_backup "$restored_db" -qAt -c 'SELECT count(*) FROM litecr.schema_meta')" == 1 ]] || fail RESTORED_BACKUP_READ
if psql_as litecr_runtime "$restored_db" -v ON_ERROR_STOP=1 -c 'CREATE TABLE litecr.p6_recovery_must_be_denied (id integer)' > "$root/runtime-ddl.out" 2>&1; then
  fail RESTORED_RUNTIME_DDL_UNEXPECTEDLY_ALLOWED
fi
grep -Fq 'permission denied' "$root/runtime-ddl.out" || fail RESTORED_RUNTIME_DDL_WRONG_FAILURE
[[ "$(grep -c 'ROLE_ACL_VERIFICATION_OK' "$root/runtime-acl-restored.log")" == 1 ]] || fail RESTORED_RUNTIME_ACL_VERIFIER

matching_summary="not-applicable"
if [[ "$schema_version" =~ ^(17|18|19|20|22|23|24|25|26|27)$ ]]; then
  matching_summary="count=$(cut -d'|' -f1 "$source_matching_snapshot"),md5=$(cut -d'|' -f2 "$source_matching_snapshot")"
fi
if [[ "$schema_version" =~ ^(22|23|24|25|26|27)$ ]]; then
  [[ "$(psql_as litecr_migrator "$restored_db" -qAt -c \
    "SELECT count(*) FROM litecr.access_security_audit audit JOIN litecr.access_identity identity ON identity.identity_key=audit.actor_identity_key AND identity.actor_kind=audit.actor_kind WHERE audit.request_id='p6-internal-actor-restored-audit' AND identity.external_subject='p6-migration-writer'")" == 1 ]] || fail RESTORED_INTERNAL_ACTOR_AUDIT
fi
printf 'Schema-%s isolated backup/restore passed: PostgreSQL %s.x; source=%s; restored=%s; matching_result_snapshot=%s; application table digests, sequence states, object owners, global role flags, and reconciled runtime/backup ACLs matched.\n' \
  "$schema_version" "${expected_postgres_version%.*}" "$source_db" "$restored_db" "$matching_summary"
