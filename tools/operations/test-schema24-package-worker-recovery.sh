#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

fail() { printf 'liteCR package worker recovery failed [%s]\n' "$1" >&2; exit 1; }

[[ $# == 9 ]] || fail 'USAGE: ARCHIVE ARCHIVE_SHA256 APP_VERSION SCHEMA_VERSION PLATFORM DISTRIBUTION_STATUS SOURCE_REVISION SOURCE_REVISION_STATUS BINARY_SHA256'
archive_input="$1"
expected_archive_sha256="$2"
expected_app_version="$3"
expected_schema_version="$4"
expected_platform="$5"
expected_distribution_status="$6"
expected_source_revision="$7"
expected_source_revision_status="$8"
expected_binary_sha256="$9"
# Keep this harness specifically scoped to schema-24 Linux review archives,
# while requiring exact package identity and both hashes from the caller.
[[ "$expected_archive_sha256" =~ ^[a-f0-9]{64}$ && "$expected_binary_sha256" =~ ^[a-f0-9]{64}$ ]] || fail EXPECTED_SHA256_INVALID
[[ "$expected_app_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || fail EXPECTED_APP_VERSION_INVALID
[[ "$expected_schema_version" == 24 && "$expected_platform" == linux-x86_64 && \
   "$expected_distribution_status" == review_required ]] || fail EXPECTED_SCHEMA24_LINUX_REVIEW_PACKAGE_REQUIRED
if [[ "$expected_source_revision_status" == clean_git_worktree ]]; then
  [[ "$expected_source_revision" =~ ^[a-f0-9]{40}$ ]] || fail EXPECTED_SOURCE_REVISION_INVALID
elif [[ "$expected_source_revision_status" == no_git_metadata ]]; then
  [[ "$expected_source_revision" == null ]] || fail EXPECTED_SOURCE_REVISION_INVALID
else
  fail EXPECTED_SOURCE_REVISION_STATUS_INVALID
fi
package_name="litecr-$expected_app_version-$expected_platform"
[[ "$(basename -- "$archive_input")" == "$package_name.tar.gz" && -f "$archive_input" && ! -L "$archive_input" ]] || fail EXACT_CANDIDATE_ARCHIVE_REQUIRED
archive="$(realpath -e -- "$archive_input")"
[[ "$(sha256sum "$archive" | awk '{print $1}')" == "$expected_archive_sha256" ]] || fail ARCHIVE_SHA256_MISMATCH

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
pg_version="$(pg_config --version)"
[[ "$pg_version" =~ ^PostgreSQL\ 18([.]|$) ]] || fail POSTGRESQL_18_REQUIRED
pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready psql; do
  [[ -x "$pg_bin/$tool" ]] || fail "POSTGRESQL_TOOL_$tool"
done
command -v python3 >/dev/null 2>&1 || fail PYTHON_REQUIRED
command -v sha256sum >/dev/null 2>&1 || fail SHA256SUM_REQUIRED
command -v tar >/dev/null 2>&1 || fail TAR_REQUIRED

# Validate archive member names before extraction. Extraction is confined to a
# fresh owner-marked directory and never targets a shared package or release path.
tar -tzf "$archive" | python3 -c 'import pathlib,sys
root=sys.argv[1]
names=[line.rstrip("\n") for line in sys.stdin]
assert names
for name in names:
    path=pathlib.PurePosixPath(name)
    assert not path.is_absolute() and ".." not in path.parts and "" not in path.parts
assert all(name.startswith(root+"/") for name in names)' "$package_name" || fail ARCHIVE_MEMBER_PATH_INVALID

export PATH="$pg_bin:$PATH"
unset PGHOST PGHOSTADDR PGPORT PGDATABASE PGUSER PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGOPTIONS \
  PGCONNECT_TIMEOUT PGSSLMODE PGREQUIRESSL PGSSLCERT PGSSLKEY PGSSLROOTCERT PGSSLCRL PGSSLCRLDIR \
  PGSSLCOMPRESSION PGSSLCERTMODE PGSSLMINPROTOCOLVERSION PGSSLMAXPROTOCOLVERSION PGSSLKEYLOGFILE \
  PGCHANNELBINDING PGGSSENCMODE PGTARGETSESSIONATTRS PGLOADBALANCEHOSTS PGCLIENTENCODING PGREALM \
  PGGSSDELEGATION PGKRBSRVNAME PGAPPNAME
bootstrap_role="$(id -un)"
[[ "$bootstrap_role" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || fail BOOTSTRAP_ROLE

root="$(mktemp -d /tmp/litecr-schema24-package-worker-recovery.XXXXXXXX)"
chmod 700 "$root"
root="$(realpath -e -- "$root")"
[[ "$root" == /tmp/litecr-schema24-package-worker-recovery.* && -d "$root" && ! -L "$root" ]] || fail TEMP_ROOT
printf 'litecr-schema24-package-worker-recovery-v1\n' > "$root/OWNER"
chmod 600 "$root/OWNER"
package_dir="$root/package"
mkdir -m 700 "$package_dir"
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
worker_pid=''
cleanup() {
  status=$?
  if [[ -n "$worker_pid" ]] && kill -0 "$worker_pid" 2>/dev/null; then
    kill -TERM "$worker_pid" 2>/dev/null || true
    wait "$worker_pid" 2>/dev/null || true
  fi
  if [[ -f "$data/postmaster.pid" ]]; then
    if ! "$pg_bin/pg_ctl" -D "$data" -m fast stop >/dev/null 2>&1; then
      echo 'CLEANUP_POSTGRES_STOP_FAILED; preserving owned resources' >&2
      exit 1
    fi
  fi
  if [[ "$root" == /tmp/litecr-schema24-package-worker-recovery.* && -f "$root/OWNER" && \
        "$(cat "$root/OWNER")" == litecr-schema24-package-worker-recovery-v1 && ! -L "$root" && \
        ! -f "$data/postmaster.pid" ]]; then
    rm -rf -- "$root"
    echo 'CLEANUP=owned temporary package and PostgreSQL cluster removed'
  else
    echo 'CLEANUP_REFUSED_UNOWNED_OR_RUNNING_PATH' >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

tar -xzf "$archive" --no-same-owner --no-same-permissions -C "$package_dir"
package_root="$package_dir/$package_name"
binary="$package_root/bin/litecr"
[[ -d "$package_root" && ! -L "$package_root" && -f "$binary" && -x "$binary" && ! -L "$binary" ]] || fail PACKAGE_LAYOUT
[[ "$(sha256sum "$binary" | awk '{print $1}')" == "$expected_binary_sha256" ]] || fail BINARY_SHA256_MISMATCH
(cd -- "$package_root" && sha256sum --check --strict SHA256SUMS >/dev/null) || fail PACKAGE_PAYLOAD_CHECKSUM
[[ "$("$binary" version --json | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["application_version"] == sys.argv[1] and d["schema_version"] == int(sys.argv[2]); print(d["schema_version"])' "$expected_app_version" "$expected_schema_version")" == "$expected_schema_version" ]] || fail PACKAGE_VERSION_SCHEMA
python3 - "$package_root/share/metadata/release.json" "$expected_app_version" "$expected_schema_version" \
  "$expected_platform" "$expected_distribution_status" "$expected_source_revision" \
  "$expected_source_revision_status" <<'PY' || fail RELEASE_METADATA
import json, pathlib, sys
d=json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
expected=(sys.argv[2], int(sys.argv[3]), sys.argv[4], sys.argv[5], sys.argv[6], sys.argv[7])
actual=(d["application_version"], d["schema_version"], d["platform"],
        d["distribution_status"], d["source_revision"], d["source_revision_status"])
assert actual == expected, (actual, expected)
PY
[[ -f "$package_root/share/tools/operations/grant-worker-database-access.sh" && \
   -x "$package_root/share/tools/operations/grant-worker-database-access.sh" && \
   -f "$package_root/share/tools/operations/grant-worker-database-access.sql" ]] || fail PACKAGED_WORKER_GRANT_TOOL
python3 - "$package_root/share/tools/operations/grant-worker-database-access.sql" <<'PY' || fail PACKAGED_WORKER_GRANT_SCHEMA24_UNSUPPORTED
import pathlib, re, sys
sql=pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
match=re.search(r"version\s+IN\s*\(([^)]*)\)", sql, re.I)
assert match and [int(x.strip()) for x in match.group(1).split(",")] == [22,23,24], \
    "packaged ACL preflight must allow exactly schemas 22, 23 and 24"
PY
"$binary" worker --help >/dev/null 2>&1 || fail WORKER_SUBCOMMAND_UNAVAILABLE

"$pg_bin/initdb" -D "$data" --username="$bootstrap_role" --auth-local=trust --auth-host=trust \
  --encoding=UTF8 --locale=C.UTF-8 --no-instructions >"$root/initdb.log"
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
[[ "$(psql_as "$bootstrap_role" postgres -qAt -c 'SHOW server_version_num')" =~ ^180 ]] || fail POSTGRESQL_SERVER_VERSION

psql_as "$bootstrap_role" postgres -v ON_ERROR_STOP=1 <<'SQL'
CREATE ROLE litecr_migrator LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_worker LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE DATABASE litecr_test OWNER litecr_migrator;
SQL
migration_url="postgresql://litecr_migrator@127.0.0.1:$port/litecr_test"
worker_url="postgresql://litecr_worker@127.0.0.1:$port/litecr_test"
LITECR_MIGRATION_DATABASE_URL="$migration_url" "$binary" migrate
[[ "$(psql_as litecr_migrator litecr_test -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == 24 ]] || fail SCHEMA24_MIGRATION
# Prove the packaged privilege script fails closed for a future schema before
# exercising its supported schema-24 grant path. This changes only the isolated
# fixture's metadata and restores it immediately after the expected rejection.
psql_as litecr_migrator litecr_test -v ON_ERROR_STOP=1 -c \
  'UPDATE litecr.schema_meta SET version=25 WHERE singleton'
if LITECR_MIGRATION_DATABASE_URL="$migration_url" \
  bash "$package_root/share/tools/operations/grant-worker-database-access.sh" \
  >"$root/unsupported-schema-grant.log" 2>&1; then
  fail PACKAGED_WORKER_GRANT_ACCEPTED_UNSUPPORTED_SCHEMA
fi
grep -Fq 'WORKER_ROLE_PREFLIGHT_SCHEMA_VERSION' "$root/unsupported-schema-grant.log" || \
  fail PACKAGED_WORKER_GRANT_UNSUPPORTED_SCHEMA_WRONG_FAILURE
[[ "$(psql_as litecr_migrator litecr_test -qAt -c \
  "SELECT has_schema_privilege('litecr_worker','litecr','USAGE') OR has_schema_privilege('litecr_worker','matching','USAGE')")" == f ]] || \
  fail PACKAGED_WORKER_GRANT_UNSUPPORTED_SCHEMA_PARTIAL_GRANTS
psql_as litecr_migrator litecr_test -v ON_ERROR_STOP=1 -c \
  'UPDATE litecr.schema_meta SET version=24 WHERE singleton'
LITECR_MIGRATION_DATABASE_URL="$migration_url" \
  bash "$package_root/share/tools/operations/grant-worker-database-access.sh" >/dev/null

# Two synthetic accepted events are emitted through the real enqueue trigger.
# One remains pending; the other is changed to a deliberately expired lease.
psql_as litecr_migrator litecr_test -v ON_ERROR_STOP=1 <<'SQL'
BEGIN;
WITH fixtures(event_id, person_id, source_id, source_key, public_id, event_ref) AS (
    VALUES
      ('24000000-0000-4000-8000-000000000001'::uuid,
       '24000000-0000-4000-8000-000000000011'::uuid,
       '24000000-0000-4000-8000-000000000021'::uuid,
       'worker-pending', 'worker-pending-person', 'worker-pending-event'),
      ('24000000-0000-4000-8000-000000000002'::uuid,
       '24000000-0000-4000-8000-000000000012'::uuid,
       '24000000-0000-4000-8000-000000000022'::uuid,
       'worker-expired', 'worker-expired-person', 'worker-expired-event')
), people AS (
    INSERT INTO litecr.person(person_id,public_id,registry_id,organization_id)
    SELECT person_id,public_id,'p6-worker-registry','p6-worker-organization'
    FROM fixtures RETURNING person_id
), sources AS (
    INSERT INTO litecr.source_record(source_record_id,registry_id,organization_id,
                                     source_system_id,source_record_key,person_id)
    SELECT f.source_id,'p6-worker-registry','p6-worker-organization',
           'p6-worker-system',f.source_key,f.person_id
    FROM fixtures f JOIN people p USING(person_id)
    RETURNING source_record_id
)
INSERT INTO litecr.source_record_version(source_record_id,receive_version,source_version,
                                         payload_hash,version_state)
SELECT source_record_id,1,'fixture-v1',decode(repeat('a5',32),'hex'),'accepted' FROM sources;

INSERT INTO litecr.source_assertion(source_record_id,receive_version,field_path,original_value,language_tag,script)
VALUES ('24000000-0000-4000-8000-000000000021',1,'name.original','Synthetic Worker Pending','en','Latn'),
       ('24000000-0000-4000-8000-000000000022',1,'name.original','Synthetic Worker Expired','en','Latn');

INSERT INTO litecr.outbox_event(event_id,event_type,schema_version,registry_id,organization_id,
                               aggregate_type,aggregate_id,aggregate_version,actor_kind,actor_id,
                               correlation_id,payload)
VALUES ('24000000-0000-4000-8000-000000000001','registry.source_record.accepted',1,
        'p6-worker-registry','p6-worker-organization','source_record',
        '24000000-0000-4000-8000-000000000021',1,'system','p6-worker-fixture',
        'p6-worker-pending-event','{"fixture":"synthetic-pending"}'::jsonb),
       ('24000000-0000-4000-8000-000000000002','registry.source_record.accepted',1,
        'p6-worker-registry','p6-worker-organization','source_record',
        '24000000-0000-4000-8000-000000000022',1,'system','p6-worker-fixture',
        'p6-worker-expired-event','{"fixture":"synthetic-expired-lease"}'::jsonb);
COMMIT;
UPDATE litecr.consumer_job SET state='leased',attempts=1,lease_owner='synthetic-crashed-worker',
  lease_token='24000000-0000-4000-8000-000000000099'::uuid,
  lease_expires_at=now()-interval '1 minute'
WHERE event_id='24000000-0000-4000-8000-000000000002'::uuid AND state='pending';
SQL
[[ "$(psql_as litecr_migrator litecr_test -qAt -c "SELECT state||':'||attempts FROM litecr.consumer_job WHERE event_id='24000000-0000-4000-8000-000000000001'")" == pending:0 ]] || fail PENDING_FIXTURE
[[ "$(psql_as litecr_migrator litecr_test -qAt -c "SELECT state||':'||attempts||':'||(lease_expires_at<now()) FROM litecr.consumer_job WHERE event_id='24000000-0000-4000-8000-000000000002'")" == leased:1:true ]] || fail EXPIRED_LEASE_FIXTURE

LITECR_WORKER_DATABASE_URL="$worker_url" LITECR_WORKER_ID=p6-package-worker \
  "$binary" worker >"$root/worker-first.log" 2>&1 &
worker_pid=$!
completed=0
for _ in $(seq 1 180); do
  if [[ "$(psql_as litecr_migrator litecr_test -qAt -c \
      "SELECT count(*) FROM litecr.consumer_job WHERE event_id IN ('24000000-0000-4000-8000-000000000001','24000000-0000-4000-8000-000000000002') AND state='completed'")" == 2 ]]; then
    completed=1
    break
  fi
  kill -0 "$worker_pid" 2>/dev/null || fail WORKER_EXITED_EARLY
  sleep 0.5
done
(( completed == 1 )) || fail WORKER_DID_NOT_COMPLETE_FIXTURES
kill -TERM "$worker_pid"
if wait "$worker_pid"; then worker_status=0; else worker_status=$?; fi
worker_pid=''
[[ "$worker_status" == 0 ]] || fail WORKER_DRAIN_EXIT

expected_rows='24000000-0000-4000-8000-000000000001|completed|1
24000000-0000-4000-8000-000000000002|completed|2'
actual_rows="$(psql_as litecr_migrator litecr_test -qAt -F '|' -c \
  "SELECT event_id::text,state,attempts FROM litecr.consumer_job WHERE event_id IN ('24000000-0000-4000-8000-000000000001','24000000-0000-4000-8000-000000000002') ORDER BY event_id")"
[[ "$actual_rows" == "$expected_rows" ]] || fail COMPLETED_JOB_ATTEMPTS
before="$(psql_as litecr_migrator litecr_test -qAt -F '|' -c \
  "SELECT j.event_id::text,j.state,j.attempts,r.outcome,r.evidence_run_id::text,
          (SELECT count(*) FROM matching.evidence_run e WHERE e.run_id=r.evidence_run_id),
          (SELECT count(*) FROM litecr.consumer_receipt r2 WHERE r2.event_id=j.event_id)
   FROM litecr.consumer_job j JOIN litecr.consumer_receipt r USING(consumer_name,event_id)
   WHERE j.event_id IN ('24000000-0000-4000-8000-000000000001','24000000-0000-4000-8000-000000000002')
   ORDER BY j.event_id")"
[[ "$(printf '%s\n' "$before" | wc -l)" == 2 ]] || fail RECEIPT_CARDINALITY
[[ "$(printf '%s\n' "$before" | awk -F '|' '$2!="completed" || $4!="completed" || $6!=1 || $7!=1 {bad++} END {print bad+0}')" == 0 ]] || fail EVIDENCE_RECEIPT_ATOMICITY

# Restart the same exact package with no due work. Durable state must remain
# byte-for-byte identical, proving the completed jobs are not replayed by the
# normal worker claim path.
LITECR_WORKER_DATABASE_URL="$worker_url" LITECR_WORKER_ID=p6-package-worker-idle \
  "$binary" worker >"$root/worker-restart.log" 2>&1 &
worker_pid=$!
sleep 1
kill -0 "$worker_pid" 2>/dev/null || fail WORKER_RESTART_DID_NOT_STAY_HEALTHY
kill -TERM "$worker_pid"
if wait "$worker_pid"; then worker_status=0; else worker_status=$?; fi
worker_pid=''
[[ "$worker_status" == 0 ]] || fail WORKER_RESTART_DRAIN_EXIT
after="$(psql_as litecr_migrator litecr_test -qAt -F '|' -c \
  "SELECT j.event_id::text,j.state,j.attempts,r.outcome,r.evidence_run_id::text,
          (SELECT count(*) FROM matching.evidence_run e WHERE e.run_id=r.evidence_run_id),
          (SELECT count(*) FROM litecr.consumer_receipt r2 WHERE r2.event_id=j.event_id)
   FROM litecr.consumer_job j JOIN litecr.consumer_receipt r USING(consumer_name,event_id)
   WHERE j.event_id IN ('24000000-0000-4000-8000-000000000001','24000000-0000-4000-8000-000000000002')
   ORDER BY j.event_id")"
[[ "$after" == "$before" ]] || fail RESTART_CHANGED_COMPLETED_EVIDENCE

printf 'PACKAGE_ARCHIVE_SHA256=%s\n' "$expected_archive_sha256"
printf 'PACKAGE_BINARY_SHA256=%s\n' "$expected_binary_sha256"
printf 'POSTGRESQL_SERVER_VERSION=%s\n' "$(psql_as litecr_migrator litecr_test -qAt -c 'SHOW server_version')"
printf 'PACKAGE_WORKER_PENDING_AND_EXPIRED_LEASE_RECOVERY=passed\n'
printf 'COMPLETION_ATTEMPTS= pending:1 expired_lease:2\n'
printf 'EVIDENCE_RECEIPT_RESTART_IDEMPOTENCY=passed\n'
