#!/usr/bin/env bash
set -euo pipefail
set +x

[[ $# == 1 && "$1" == /* && -x "$1" && ! -L "$1" && $(basename -- "$1") =~ ^litecr(\.exe)?$ ]] || {
  echo "usage: $0 /absolute/path/to/litecr-schema-22,-23,-24,-27,-28,-29,-30-or-31-binary" >&2
  exit 2
}
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
release="$(realpath -e -- "$1")"
pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready psql; do
  [[ -x "$pg_bin/$tool" ]] || { echo "POSTGRESQL_TOOL_MISSING:$tool" >&2; exit 2; }
done
schema_version="$("$release" version --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["schema_version"])')"
[[ "$schema_version" =~ ^(22|23|24|27|28|29|30|31)$ ]] || {
  echo 'SCHEMA_22_23_24_27_28_29_30_OR_31_BINARY_REQUIRED' >&2
  exit 2
}
for file in grant-runtime-database-access.sh grant-runtime-database-access.sql grant-worker-database-access.sh grant-worker-database-access.sql; do
  [[ -f "$repo/tools/operations/$file" && ! -L "$repo/tools/operations/$file" ]] || {
    echo "ROLE_ACL_FILE_MISSING:$file" >&2; exit 2;
  }
done

export PATH="$pg_bin:$PATH"
unset PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGHOSTADDR PGOPTIONS PGDATABASE PGPORT PGHOST
bootstrap_role="$(id -un)"
[[ "$bootstrap_role" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || { echo 'UNSUPPORTED_BOOTSTRAP_ROLE' >&2; exit 2; }

root="$(mktemp -d /tmp/litecr-worker-rolecheck.XXXXXXXX)"
chmod 700 "$root"
root="$(realpath -e -- "$root")"
[[ "$root" == /tmp/litecr-worker-rolecheck.* && -d "$root" && ! -L "$root" ]] || { echo 'WORKER_ROLECHECK_TEMP_PATH_UNSAFE' >&2; exit 3; }
printf 'litecr-worker-postgres-rolecheck-v1\n' > "$root/OWNER"
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
systemd_unit=""
recovery_unit=""
blocker_pid=""
cleanup() {
  status=$?
  if [[ -n "$systemd_unit" ]]; then
    systemctl --user stop "$systemd_unit" >/dev/null 2>&1 || true
    systemctl --user reset-failed "$systemd_unit" >/dev/null 2>&1 || true
  fi
  if [[ -n "$recovery_unit" ]]; then
    systemctl --user stop "$recovery_unit" >/dev/null 2>&1 || true
    systemctl --user reset-failed "$recovery_unit" >/dev/null 2>&1 || true
  fi
  if [[ -n "$blocker_pid" ]]; then
    kill -TERM "$blocker_pid" 2>/dev/null || true
    wait "$blocker_pid" 2>/dev/null || true
  fi
  if (( started )); then "$pg_bin/pg_ctl" -D "$data" -m immediate stop >/dev/null 2>&1 || true; fi
  if [[ "$root" == /tmp/litecr-worker-rolecheck.* && -f "$root/OWNER" && "$(cat "$root/OWNER")" == litecr-worker-postgres-rolecheck-v1 ]]; then
    rm -rf -- "$root"
  else
    echo 'WORKER_ROLECHECK_CLEANUP_REFUSED_UNOWNED_PATH' >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT

"$pg_bin/initdb" -D "$data" --username="$bootstrap_role" --auth-local=trust --auth-host=trust --encoding=UTF8 --locale=C.UTF-8 --no-instructions >"$root/initdb.log"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" -o "-h 127.0.0.1 -p $port -k $socket" start >/dev/null
started=1
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
  echo 'WORKER_ROLECHECK_CLUSTER_NOT_LOOPBACK' >&2; exit 3;
}

# This cluster, its superuser, credentials and database are disposable. The
# script never probes or connects to a pre-existing PostgreSQL service.
psql_as "$bootstrap_role" postgres -v ON_ERROR_STOP=1 <<'SQL'
CREATE ROLE litecr_migrator LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_runtime LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_worker LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
-- Keep this synthetic crash drill bounded: a disconnected worker may leave a
-- PostgreSQL statement blocked until server-side cancellation releases locks.
ALTER ROLE litecr_worker SET statement_timeout = '5s';
CREATE DATABASE litecr_test OWNER litecr_migrator;
SQL

migration_url="postgresql://litecr_migrator@127.0.0.1:$port/litecr_test"
runtime_url="postgresql://litecr_runtime@127.0.0.1:$port/litecr_test"
worker_url="postgresql://litecr_worker@127.0.0.1:$port/litecr_test"
LITECR_MIGRATION_DATABASE_URL="$migration_url" "$release" migrate
[[ "$(psql_as litecr_migrator litecr_test -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == "$schema_version" ]] || {
  echo 'MIGRATION_SCHEMA_VERSION_MISMATCH' >&2; exit 1;
}

# Rehearse the existing API ACL unchanged first; current schema compatibility is an
# explicit gate for this worker role change.
LITECR_MIGRATION_DATABASE_URL="$migration_url" bash "$repo/tools/operations/grant-runtime-database-access.sh"
LITECR_MIGRATION_DATABASE_URL="$migration_url" bash "$repo/tools/operations/grant-worker-database-access.sh"

if [[ "$schema_version" =~ ^(27|28|29|30|31)$ ]]; then
  # Reapplying current policy must also remove the old column-level grant;
  # revoking table-level rights alone does not remove a column-level privilege.
  psql_as litecr_migrator litecr_test -v ON_ERROR_STOP=1 \
    -c 'GRANT UPDATE (result_snapshot) ON matching.evidence_run TO litecr_worker' >/dev/null
  LITECR_MIGRATION_DATABASE_URL="$migration_url" bash "$repo/tools/operations/grant-worker-database-access.sh"
fi

expect_denied() {
  local label="$1" role="$2" sql="$3"
  if psql_as "$role" litecr_test -v ON_ERROR_STOP=1 -c "$sql" >"$root/$label.out" 2>&1; then
    echo "${label}_UNEXPECTEDLY_SUCCEEDED" >&2; exit 1;
  fi
  if ! grep -Fq 'permission denied' "$root/$label.out"; then
    echo "${label}_FAILED_FOR_UNEXPECTED_REASON" >&2; exit 1;
  fi
  printf '%s denied as expected\n' "$label"
}
expect_denied worker_create_schema litecr_worker 'CREATE SCHEMA litecr_worker_must_not_create'
expect_denied worker_update_sqlx_ledger litecr_worker 'UPDATE public._sqlx_migrations SET success=success WHERE FALSE'
expect_denied worker_update_consumer_scope litecr_worker 'UPDATE litecr.consumer_job SET registry_id=registry_id WHERE FALSE'
expect_denied worker_insert_person litecr_worker "INSERT INTO litecr.person(public_id,registry_id,organization_id) VALUES('must-not-insert','x','y')"
expect_denied worker_insert_source_record litecr_worker "INSERT INTO litecr.source_record(registry_id,organization_id,source_system_id,source_record_key,person_id) VALUES('x','y','x','x',gen_random_uuid())"
expect_denied worker_update_source_record litecr_worker 'UPDATE litecr.source_record SET current_receive_version=current_receive_version WHERE FALSE'
expect_denied worker_insert_outbox litecr_worker "INSERT INTO litecr.outbox_event(event_type,schema_version,registry_id,organization_id,aggregate_type,aggregate_id,aggregate_version,actor_kind,actor_id,payload) VALUES('no',1,'r','o','x',gen_random_uuid(),1,'system','x','{}')"
expect_denied worker_insert_access litecr_worker "INSERT INTO litecr.access_audit(actor_kind,actor_id,action,outcome,request_id) VALUES('system','worker','test','denied','synthetic')"
expect_denied worker_select_person litecr_worker 'SELECT public_id FROM litecr.person LIMIT 1'
expect_denied worker_select_result_snapshot litecr_worker 'SELECT result_snapshot FROM matching.evidence_run LIMIT 1'
expect_denied worker_select_review litecr_worker 'SELECT case_id FROM litecr.review_case LIMIT 1'
expect_denied worker_select_catalog litecr_worker 'SELECT message_key FROM litecr.catalog_source LIMIT 1'
expect_denied worker_select_migration_package_v2 litecr_worker 'SELECT upload_id FROM litecr.migration_package_v2 LIMIT 1'
expect_denied worker_update_migration_package_v2 litecr_worker 'UPDATE litecr.migration_package_v2 SET state=state WHERE FALSE'
expect_denied worker_select_migration_package_v2_batch litecr_worker 'SELECT upload_id FROM litecr.migration_package_v2_batch LIMIT 1'
expect_denied worker_update_migration_package_v2_batch litecr_worker 'UPDATE litecr.migration_package_v2_batch SET batch_sequence=batch_sequence WHERE FALSE'
expect_denied runtime_update_consumer_job litecr_runtime "UPDATE litecr.consumer_job SET state='failed' WHERE FALSE"
expect_denied worker_update_evidence_metadata litecr_worker 'UPDATE matching.evidence_run SET actor_id=actor_id WHERE FALSE'

if [[ "$schema_version" =~ ^(27|28|29|30|31)$ ]]; then
  expect_denied worker_rewrite_result_snapshot litecr_worker "UPDATE matching.evidence_run SET result_snapshot='{}'::jsonb WHERE FALSE"
  expect_denied worker_select_matching_idempotency litecr_worker 'SELECT actor_digest FROM matching.request_idempotency LIMIT 1'
  expect_denied worker_select_mcp_credentials litecr_worker 'SELECT token_hash FROM litecr.access_mcp_stdio_credential LIMIT 1'
  expect_denied worker_select_mcp_audit litecr_worker 'SELECT outcome FROM litecr.mcp_invocation_audit LIMIT 1'
  [[ "$(psql_as litecr_worker litecr_test -v ON_ERROR_STOP=1 -qAt -c \
    "SELECT matching.name_edit_within_v1('ສົມໃຈ','ສົມໄຈ',1) \
      AND matching.name_prefix_within_v1('Somchay','Somchai') \
      AND matching.name_parts_within_v1('Somchay Phom','Somchai Phommasane') \
      AND NOT matching.name_parts_within_v1('Somchai UniqueSurname','Somchai DifferentSurname')")" == t ]] || {
    echo 'WORKER_NAME_RETRIEVAL_HELPERS_FAILED' >&2; exit 1;
  }
fi

# Runtime identity must retain the current API write path and trigger semantics.
cd "$repo"
LITECR_API_ROLE_TEST_DATABASE_URL="$runtime_url" CARGO_BUILD_JOBS=1 \
  cargo test --locked -p litecr-infrastructure --test api_worker_outbox_acl -- --test-threads=1

# Seed two entirely synthetic events as migration owner; the worker itself only
# claims, retries, reads source snapshots and writes matching evidence/receipts.
psql_as litecr_migrator litecr_test -v ON_ERROR_STOP=1 <<'SQL'
WITH persons AS (
  INSERT INTO litecr.person(person_id,public_id,registry_id,organization_id) VALUES
    ('00000000-0000-4000-8000-000000000013','synthetic-worker-query','synthetic-worker-reg','synthetic-worker-org'),
    ('00000000-0000-4000-8000-000000000014','synthetic-worker-candidate','synthetic-worker-reg','synthetic-worker-org')
  RETURNING person_id,public_id
), sources AS (
  INSERT INTO litecr.source_record(source_record_id,registry_id,organization_id,source_system_id,source_record_key,person_id)
  SELECT CASE public_id WHEN 'synthetic-worker-query' THEN '00000000-0000-4000-8000-000000000023'::uuid ELSE '00000000-0000-4000-8000-000000000024'::uuid END,
         'synthetic-worker-reg','synthetic-worker-org','synthetic-worker-acl',public_id,person_id FROM persons
  RETURNING source_record_id,source_record_key
), versions AS (
  INSERT INTO litecr.source_record_version(source_record_id,receive_version,source_version,payload_hash,version_state)
  SELECT source_record_id,1,'synthetic-v1',decode(repeat('a',64),'hex'),'accepted' FROM sources RETURNING source_record_id
)
INSERT INTO litecr.source_assertion(source_record_id,receive_version,field_path,original_value,language_tag,script)
SELECT source_record_id,1,'name.original','Synthetic Test Person','en','Latin' FROM versions
UNION ALL SELECT source_record_id,1,'birth_date','1986-04-19',NULL,NULL FROM versions;

INSERT INTO litecr.outbox_event(event_id,event_type,schema_version,registry_id,organization_id,aggregate_type,aggregate_id,aggregate_version,actor_kind,actor_id,correlation_id,payload)
VALUES
('00000000-0000-4000-8000-000000000033','registry.source_record.accepted',1,'synthetic-worker-reg','synthetic-worker-org','source_record','00000000-0000-4000-8000-000000000023',1,'system','synthetic-seed','synthetic-worker-query','{}'),
('00000000-0000-4000-8000-000000000034','registry.source_record.accepted',1,'synthetic-worker-reg','synthetic-worker-org','source_record','00000000-0000-4000-8000-000000000024',1,'system','synthetic-seed','synthetic-worker-retry','{}');
SQL

LITECR_WORKER_ROLE_TEST_DATABASE_URL="$worker_url" \
LITECR_WORKER_QUERY_EVENT_ID=00000000-0000-4000-8000-000000000033 \
LITECR_WORKER_RETRY_EVENT_ID=00000000-0000-4000-8000-000000000034 \
CARGO_BUILD_JOBS=1 cargo test --locked -p litecr-infrastructure --test worker_role_acl -- --test-threads=1

# Re-run registry replay, queue transaction, stale-snapshot race, and review
# outbox coverage against the same private supported-schema cluster as the migration
# owner; all test records are synthetic.
LITECR_TEST_DATABASE_URL="$migration_url" CARGO_BUILD_JOBS=1 \
  cargo test --locked -p litecr-infrastructure \
    --test registry --test worker_queue --test review --test review_evidence_provenance \
    -- --test-threads=1

[[ "$(psql_as litecr_worker litecr_test -qAt -c "SELECT state FROM litecr.consumer_job WHERE event_id='00000000-0000-4000-8000-000000000033'")" == completed ]] || {
  echo 'WORKER_EVIDENCE_COMPLETION_MISSING' >&2; exit 1;
}
[[ "$(psql_as litecr_migrator litecr_test -qAt -c "SELECT result_snapshot IS NOT NULL FROM matching.evidence_run WHERE run_id=(SELECT evidence_run_id FROM litecr.consumer_receipt WHERE event_id='00000000-0000-4000-8000-000000000033')")" == t ]] || {
  echo 'WORKER_RESULT_SNAPSHOT_MISSING' >&2; exit 1;
}
[[ "$(psql_as litecr_worker litecr_test -qAt -c "SELECT state FROM litecr.consumer_job WHERE event_id='00000000-0000-4000-8000-000000000034'")" == pending ]] || {
  echo 'WORKER_RETRY_MISSING' >&2; exit 1;
}

# Optional process-level check for environments with an active user systemd
# manager. This starts the real worker against the same disposable PostgreSQL
# cluster, forces an idle-process crash/restart, then sends SIGTERM through
# systemd and verifies a clean exit. Set LITECR_TEST_SYSTEMD_LEASE_RECOVERY=1
# to also hold a synthetic source row while a job is leased, kill the worker,
# then verify expiry/reclaim and exactly-once evidence/receipt completion. The
# hardened system service template is never installed or enabled here.
if [[ "${LITECR_TEST_SYSTEMD_WORKER:-0}" == 1 ]]; then
  command -v systemd-run >/dev/null 2>&1 && command -v systemctl >/dev/null 2>&1 || {
    echo 'SYSTEMD_USER_MANAGER_TOOLS_REQUIRED' >&2; exit 2;
  }
  systemctl --user is-system-running >/dev/null 2>&1 || {
    echo 'ACTIVE_SYSTEMD_USER_MANAGER_REQUIRED' >&2; exit 2;
  }
  systemd_event=00000000-0000-4000-8000-000000000035
  psql_as litecr_migrator litecr_test -v ON_ERROR_STOP=1 <<SQL
INSERT INTO litecr.outbox_event(
  event_id,event_type,schema_version,registry_id,organization_id,aggregate_type,
  aggregate_id,aggregate_version,actor_kind,actor_id,correlation_id,payload
) VALUES (
  '$systemd_event','registry.source_record.accepted',1,'synthetic-worker-reg',
  'synthetic-worker-org','source_record','00000000-0000-4000-8000-000000000023',
  1,'system','systemd-smoke','synthetic-worker-systemd-smoke','{}'
);
SQL
  systemd_unit="litecr-worker-systemd-smoke-$$.service"
  systemd-run --user --quiet --unit="$systemd_unit" --property=Type=exec \
    --property=Restart=on-failure --property=RestartSec=200ms \
    --setenv="LITECR_WORKER_DATABASE_URL=$worker_url" \
    --setenv=LITECR_WORKER_ID=synthetic-systemd-smoke "$release" worker
  completed=0
  for _ in $(seq 1 100); do
    if [[ "$(psql_as litecr_migrator litecr_test -qAt -c \
      "SELECT state FROM litecr.consumer_job WHERE event_id='$systemd_event'")" == completed ]]; then
      completed=1
      break
    fi
    if ! systemctl --user is-active --quiet "$systemd_unit"; then break; fi
    sleep 0.2
  done
  (( completed )) || { echo 'SYSTEMD_WORKER_DID_NOT_COMPLETE_SYNTHETIC_JOB' >&2; exit 1; }
  initial_pid="$(systemctl --user show "$systemd_unit" --property=MainPID --value)"
  [[ "$initial_pid" =~ ^[1-9][0-9]*$ ]] || { echo 'SYSTEMD_WORKER_MAIN_PID_MISSING' >&2; exit 1; }
  systemctl --user kill --kill-who=main --signal=SIGKILL "$systemd_unit"
  restarted=0
  for _ in $(seq 1 100); do
    restarted_pid="$(systemctl --user show "$systemd_unit" --property=MainPID --value)"
    if systemctl --user is-active --quiet "$systemd_unit" &&
       [[ "$restarted_pid" =~ ^[1-9][0-9]*$ && "$restarted_pid" != "$initial_pid" ]]; then
      restarted=1
      break
    fi
    sleep 0.2
  done
  (( restarted )) || { echo 'SYSTEMD_WORKER_DID_NOT_RESTART_AFTER_SIGKILL' >&2; exit 1; }
  systemctl --user stop "$systemd_unit"
  [[ "$(systemctl --user show "$systemd_unit" --property=Result --value)" == success ]] || {
    echo 'SYSTEMD_WORKER_SIGTERM_RESULT_NOT_SUCCESS' >&2; exit 1;
  }
  [[ "$(systemctl --user show "$systemd_unit" --property=ExecMainStatus --value)" == 0 ]] || {
    echo 'SYSTEMD_WORKER_SIGTERM_EXIT_NOT_ZERO' >&2; exit 1;
  }
  ! systemctl --user is-active --quiet "$systemd_unit" || {
    echo 'SYSTEMD_WORKER_RESTARTED_AFTER_ORDERLY_STOP' >&2; exit 1;
  }
  systemd_unit=""
  printf 'systemd user-unit worker completed a synthetic event, restarted after idle SIGKILL, and exited cleanly on SIGTERM.\n'
  [[ "$(psql_as litecr_worker litecr_test -qAt -c "SELECT state FROM litecr.consumer_job WHERE event_id='$systemd_event'")" == completed ]] || {
    echo 'SYSTEMD_WORKER_JOB_NOT_COMPLETED' >&2; exit 1;
  }

  if [[ "${LITECR_TEST_SYSTEMD_LEASE_RECOVERY:-0}" == 1 ]]; then
    recovery_event=00000000-0000-4000-8000-000000000036
    source_id=00000000-0000-4000-8000-000000000023
    psql_as litecr_migrator litecr_test -v ON_ERROR_STOP=1 <<SQL
INSERT INTO litecr.outbox_event(
  event_id,event_type,schema_version,registry_id,organization_id,aggregate_type,
  aggregate_id,aggregate_version,actor_kind,actor_id,correlation_id,payload
) VALUES (
  '$recovery_event','registry.source_record.accepted',1,'synthetic-worker-reg',
  'synthetic-worker-org','source_record','$source_id',1,'system',
  'systemd-recovery-smoke','synthetic-worker-recovery-smoke','{}'
);
SQL

    # Hold the query source row so matching blocks in its final freshness
    # transaction, after the queue lease has committed.
    PGHOST=127.0.0.1 PGPORT="$port" PGUSER="$bootstrap_role" PGDATABASE=litecr_test \
      "$pg_bin/psql" -X --no-psqlrc -v ON_ERROR_STOP=1 >"$root/recovery-blocker.log" 2>&1 <<SQL &
BEGIN;
SELECT source_record_id FROM litecr.source_record
WHERE source_record_id='$source_id'::uuid FOR UPDATE;
SELECT pg_sleep(600);
SQL
    blocker_pid=$!
    lock_ready=0
    for _ in $(seq 1 100); do
      if ! kill -0 "$blocker_pid" 2>/dev/null; then break; fi
      if [[ "$(psql_as "$bootstrap_role" litecr_test -qAt -c \
        "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND query LIKE '%pg_sleep(600)%' AND state='active'")" == 1 ]]; then
        lock_ready=1
        break
      fi
      sleep 0.1
    done
    (( lock_ready )) || { echo 'RECOVERY_SOURCE_BLOCKER_NOT_READY' >&2; exit 1; }

    recovery_unit="litecr-worker-lease-recovery-$$.service"
    systemd-run --user --quiet --unit="$recovery_unit" --property=Type=exec \
      --property=Restart=on-failure --property=RestartSec=200ms \
      --setenv="LITECR_WORKER_DATABASE_URL=$worker_url" \
      --setenv=LITECR_WORKER_ID=synthetic-lease-recovery "$release" worker
    recovery_initial_pid=""
    first_lease_token=""
    first_lease_seen=0
    for _ in $(seq 1 150); do
      recovery_initial_pid="$(systemctl --user show "$recovery_unit" --property=MainPID --value)"
      lease_row="$(psql_as litecr_migrator litecr_test -qAt -F '|' -c \
        "SELECT state,attempts,lease_owner,lease_token::text FROM litecr.consumer_job WHERE event_id='$recovery_event'")"
      if [[ "$lease_row" == leased\|1\|* ]]; then
        lock_wait="$(psql_as "$bootstrap_role" litecr_test -qAt -c \
          "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%lock_advisory_source_snapshot%'" )"
        if [[ "$lock_wait" -ge 1 ]]; then
          first_lease_token="${lease_row##*|}"
          first_lease_seen=1
          break
        fi
      fi
      sleep 0.1
    done
    (( first_lease_seen )) || { echo 'RECOVERY_FIRST_LEASE_NOT_BLOCKED_IN_FRESHNESS_CHECK' >&2; exit 1; }
    [[ "$recovery_initial_pid" =~ ^[1-9][0-9]*$ ]] || { echo 'RECOVERY_FIRST_WORKER_PID_MISSING' >&2; exit 1; }

    systemctl --user kill --kill-who=main --signal=SIGKILL "$recovery_unit"
    recovery_restarted_pid=""
    restarted=0
    for _ in $(seq 1 100); do
      recovery_restarted_pid="$(systemctl --user show "$recovery_unit" --property=MainPID --value)"
      if systemctl --user is-active --quiet "$recovery_unit" &&
         [[ "$recovery_restarted_pid" =~ ^[1-9][0-9]*$ && "$recovery_restarted_pid" != "$recovery_initial_pid" ]]; then
        restarted=1
        break
      fi
      sleep 0.1
    done
    (( restarted )) || { echo 'RECOVERY_WORKER_DID_NOT_RESTART_AFTER_LEASED_SIGKILL' >&2; exit 1; }

    reclaimed=0
    for _ in $(seq 1 800); do
      lease_row="$(psql_as litecr_migrator litecr_test -qAt -F '|' -c \
        "SELECT state,attempts,lease_owner,lease_token::text FROM litecr.consumer_job WHERE event_id='$recovery_event'")"
      if [[ "$lease_row" == leased\|2\|* ]]; then
        second_lease_token="${lease_row##*|}"
        if [[ "$second_lease_token" != "$first_lease_token" ]]; then
          reclaimed=1
          break
        fi
      fi
      sleep 0.1
    done
    (( reclaimed )) || { echo 'RECOVERY_EXPIRED_LEASE_NOT_RECLAIMED_WITH_NEW_TOKEN' >&2; exit 1; }

    # Terminate only the identified synthetic pg_sleep session; SIGTERM to the
    # psql client can cancel its statement without reliably closing the server
    # transaction that owns the row lock.
    blocker_terminated="$(psql_as "$bootstrap_role" litecr_test -qAt -c \
      "SELECT count(*) FROM pg_stat_activity WHERE pid<>pg_backend_pid() AND datname=current_database() AND query LIKE 'SELECT pg_sleep(600)%' AND pg_terminate_backend(pid)")"
    [[ "$blocker_terminated" == 1 ]] || { echo 'RECOVERY_SYNTHETIC_BLOCKER_NOT_TERMINATED' >&2; exit 1; }
    wait "$blocker_pid" 2>/dev/null || true
    completed=0
    for _ in $(seq 1 150); do
      if [[ "$(psql_as litecr_migrator litecr_test -qAt -c \
        "SELECT state FROM litecr.consumer_job WHERE event_id='$recovery_event'")" == completed ]]; then
        completed=1
        break
      fi
      sleep 0.1
    done
    (( completed )) || { echo 'RECOVERY_RECLAIMED_EVENT_DID_NOT_COMPLETE' >&2; exit 1; }
    evidence_receipt="$(psql_as litecr_migrator litecr_test -qAt -F '|' -c \
      "SELECT (SELECT count(*) FROM litecr.consumer_receipt WHERE event_id='$recovery_event' AND outcome='completed'), (SELECT count(*) FROM matching.evidence_run e JOIN litecr.consumer_receipt r ON r.evidence_run_id=e.run_id WHERE r.event_id='$recovery_event')")"
    [[ "$evidence_receipt" == '1|1' ]] || { echo 'RECOVERY_COMPLETION_NOT_SINGLE_EVIDENCE_AND_RECEIPT' >&2; exit 1; }
    systemctl --user stop "$recovery_unit"
    [[ "$(systemctl --user show "$recovery_unit" --property=Result --value)" == success ]] || {
      echo 'RECOVERY_WORKER_SIGTERM_RESULT_NOT_SUCCESS' >&2; exit 1;
    }
    [[ "$(systemctl --user show "$recovery_unit" --property=ExecMainStatus --value)" == 0 ]] || {
      echo 'RECOVERY_WORKER_SIGTERM_EXIT_NOT_ZERO' >&2; exit 1;
    }
    systemctl --user reset-failed "$recovery_unit" >/dev/null 2>&1 || true
    printf 'systemd worker recovered synthetic event %s after SIGKILL during a confirmed first lease; attempt 2 used a new lease token and committed exactly one evidence run/receipt.\n' "$recovery_event"
  fi
fi

LITECR_MIGRATION_DATABASE_URL="$migration_url" "$release" migrate
printf 'API/worker database-role verification passed (PostgreSQL %s; schema %s).\n' \
  "$(psql_as "$bootstrap_role" postgres -qAt -c 'SHOW server_version')" "$schema_version"
