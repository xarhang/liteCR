#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

fail() { printf 'liteCR sync-standby rehearsal failed [%s]\n' "$1" >&2; exit 1; }
pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready pg_basebackup psql; do
  [[ -x "$pg_bin/$tool" ]] || fail "POSTGRESQL_TOOL_$tool"
done
command -v python3 >/dev/null 2>&1 || fail PYTHON_REQUIRED
[[ "$(pg_config --version)" == 'PostgreSQL 16.15 (Ubuntu 16.15-0ubuntu0.24.04.1)' ]] || fail POSTGRESQL_16_15_REQUIRED
[[ "$(id -u)" != 0 ]] || fail MUST_NOT_RUN_AS_ROOT

# Do not let ambient libpq settings redirect even a single fixture query.
unset PGHOST PGHOSTADDR PGPORT PGDATABASE PGUSER PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGOPTIONS \
  PGCONNECT_TIMEOUT PGSSLMODE PGREQUIRESSL PGSSLCERT PGSSLKEY PGSSLROOTCERT PGSSLCRL PGSSLCRLDIR \
  PGSSLCOMPRESSION PGSSLCERTMODE PGSSLMINPROTOCOLVERSION PGSSLMAXPROTOCOLVERSION PGSSLKEYLOGFILE \
  PGCHANNELBINDING PGGSSENCMODE PGTARGETSESSIONATTRS PGLOADBALANCEHOSTS PGCLIENTENCODING PGREALM \
  PGGSSDELEGATION PGKRBSRVNAME PGAPPNAME
export PATH="$pg_bin:$PATH"
bootstrap_role="$(id -un)"
[[ "$bootstrap_role" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || fail BOOTSTRAP_ROLE

root="$(mktemp -d /tmp/litecr-sync-standby-v1.XXXXXXXX)"
chmod 700 "$root"
root="$(realpath -e -- "$root")"
[[ "$root" == /tmp/litecr-sync-standby-v1.* && -d "$root" && ! -L "$root" ]] || fail TEMP_ROOT
printf '%s\n' litecr-sync-standby-v1 > "$root/OWNER"
chmod 600 "$root/OWNER"
primary_data="$root/primary"
standby_data="$root/standby"
primary_socket="$root/primary-socket"
standby_socket="$root/standby-socket"
mkdir -m 700 "$primary_socket" "$standby_socket"

allocate_port() {
  python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
}
primary_port="$(allocate_port)"
standby_port="$(allocate_port)"
[[ "$primary_port" != "$standby_port" ]] || fail DUPLICATE_PORT
for port in "$primary_port" "$standby_port"; do
  if "$pg_bin/pg_isready" -h 127.0.0.1 -p "$port" >/dev/null 2>&1; then fail PORT_ALREADY_IN_USE; fi
done

cleanup_one() {
  local data="$1" status=0
  [[ "$data" == "$root/primary" || "$data" == "$root/standby" ]] || return 1
  [[ -d "$data" && ! -L "$data" ]] || { [[ ! -e "$data" && ! -L "$data" ]] && return 0; return 1; }
  [[ "$(stat -c '%u:%a' "$data")" == "$(id -u):700" ]] || return 1
  if [[ -e "$data/postmaster.pid" || -L "$data/postmaster.pid" ]]; then
    [[ -f "$data/postmaster.pid" && ! -L "$data/postmaster.pid" \
      && "$(sed -n '2p' "$data/postmaster.pid" 2>/dev/null)" == "$data" ]] || return 1
    "$pg_bin/pg_ctl" -D "$data" -m immediate stop >/dev/null 2>&1 || true
  fi
  "$pg_bin/pg_ctl" -D "$data" status >/dev/null 2>&1 || status=$?
  [[ "$status" == 3 && ! -e "$data/postmaster.pid" && ! -L "$data/postmaster.pid" ]]
}
cleanup() {
  local status=$?
  trap - EXIT
  if [[ "$root" == /tmp/litecr-sync-standby-v1.* && -d "$root" && ! -L "$root" \
    && "$(realpath -e -- "$root" 2>/dev/null)" == "$root" \
    && "$(stat -c '%u:%a' "$root" 2>/dev/null)" == "$(id -u):700" \
    && -f "$root/OWNER" && ! -L "$root/OWNER" \
    && "$(stat -c '%u:%a' "$root/OWNER" 2>/dev/null)" == "$(id -u):600" \
    && "$(cat -- "$root/OWNER" 2>/dev/null)" == litecr-sync-standby-v1 \
    && "$primary_data" == "$root/primary" && "$standby_data" == "$root/standby" ]]; then
    if cleanup_one "$primary_data" && cleanup_one "$standby_data"; then
      rm -rf -- "$root" || status=1
    else
      printf 'SYNC_REHEARSAL_CLEANUP_PRESERVED: %s\n' "$root" >&2
      status=1
    fi
  else
    printf 'SYNC_REHEARSAL_OWNERSHIP_CHECK_FAILED_PRESERVED: %s\n' "$root" >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

"$pg_bin/initdb" -D "$primary_data" --username="$bootstrap_role" --auth-local=trust --auth-host=reject \
  --encoding=UTF8 --locale=C.UTF-8 --no-instructions > "$root/initdb-primary.log"
# The only TCP exceptions are loopback-only replication and this bootstrap user.
sed -i "1ihost all $bootstrap_role 127.0.0.1/32 trust" "$primary_data/pg_hba.conf"
sed -i '1ihost replication litecr_sync_repl 127.0.0.1/32 trust' "$primary_data/pg_hba.conf"
cat >> "$primary_data/postgresql.conf" <<EOF
listen_addresses = '127.0.0.1'
port = $primary_port
unix_socket_directories = '$primary_socket'
wal_level = replica
max_wal_senders = 4
max_replication_slots = 2
hot_standby = on
fsync = on
full_page_writes = on
synchronous_commit = on
EOF
"$pg_bin/pg_ctl" -D "$primary_data" -l "$root/primary.log" start >/dev/null

primary_psql() {
  local role="$1" database="${2:-postgres}"
  shift 2
  PGHOST=127.0.0.1 PGPORT="$primary_port" PGUSER="$role" PGDATABASE="$database" \
    "$pg_bin/psql" -X --no-psqlrc "$@"
}
standby_psql() {
  local database="${1:-postgres}"
  shift
  PGHOST=127.0.0.1 PGPORT="$standby_port" PGUSER="$bootstrap_role" PGDATABASE="$database" \
    "$pg_bin/psql" -X --no-psqlrc "$@"
}
wait_ready() {
  local port="$1" role="$2"
  for _ in $(seq 1 100); do
    if "$pg_bin/pg_isready" -h 127.0.0.1 -p "$port" -U "$role" >/dev/null 2>&1; then return 0; fi
    sleep 0.1
  done
  return 1
}
wait_ready "$primary_port" "$bootstrap_role" || fail PRIMARY_START
[[ "$(primary_psql "$bootstrap_role" postgres -qAt -c 'SELECT host(inet_server_addr())')" == 127.0.0.1 ]] || fail PRIMARY_NOT_LOOPBACK
[[ "$(primary_psql "$bootstrap_role" postgres -qAt -c 'SHOW port')" == "$primary_port" ]] || fail PRIMARY_PORT_MISMATCH
[[ "$(sed -n '2p' "$primary_data/postmaster.pid")" == "$primary_data" ]] || fail PRIMARY_DATA_DIRECTORY_MISMATCH
[[ "$(primary_psql "$bootstrap_role" postgres -qAt -c 'SHOW server_version')" == 16.15* ]] || fail PRIMARY_VERSION
primary_psql "$bootstrap_role" postgres -v ON_ERROR_STOP=1 <<'SQL' >/dev/null
CREATE ROLE litecr_sync_repl LOGIN REPLICATION;
CREATE TABLE public.litecr_synthetic_ack_ledger (
    request_id text PRIMARY KEY,
    payload_digest text NOT NULL,
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
SQL

# Seed only a new standby from the newly created isolated primary.
PGHOST=127.0.0.1 PGPORT="$primary_port" PGUSER=litecr_sync_repl \
  "$pg_bin/pg_basebackup" --pgdata="$standby_data" --format=plain --wal-method=stream \
  --write-recovery-conf --dbname="host=127.0.0.1 port=$primary_port user=litecr_sync_repl application_name=litecr_sync_standby" \
  > "$root/basebackup.log" 2>&1 || fail BASEBACKUP
[[ -f "$standby_data/standby.signal" && -f "$standby_data/postgresql.auto.conf" ]] || fail STANDBY_CONFIG_MISSING
cat >> "$standby_data/postgresql.conf" <<EOF
listen_addresses = '127.0.0.1'
port = $standby_port
unix_socket_directories = '$standby_socket'
hot_standby = on
EOF
"$pg_bin/pg_ctl" -D "$standby_data" -l "$root/standby.log" start >/dev/null
wait_ready "$standby_port" "$bootstrap_role" || fail STANDBY_START
[[ "$(standby_psql postgres -qAt -c 'SELECT host(inet_server_addr())')" == 127.0.0.1 ]] || fail STANDBY_NOT_LOOPBACK
[[ "$(standby_psql postgres -qAt -c 'SHOW port')" == "$standby_port" ]] || fail STANDBY_PORT_MISMATCH
[[ "$(sed -n '2p' "$standby_data/postmaster.pid")" == "$standby_data" ]] || fail STANDBY_DATA_DIRECTORY_MISMATCH

# Enable strict synchronous acknowledgement only after bootstrap and streaming
# are ready; this prevents the initial fixture setup from waiting for a peer.
primary_psql "$bootstrap_role" postgres -v ON_ERROR_STOP=1 <<'SQL' >/dev/null
ALTER SYSTEM SET synchronous_standby_names = 'FIRST 1 (litecr_sync_standby)';
SELECT pg_reload_conf();
SQL
sync_row=''
for _ in $(seq 1 100); do
  sync_row="$(primary_psql "$bootstrap_role" postgres -qAt -F '|' -c \
    "SELECT application_name, state, sync_state FROM pg_stat_replication WHERE application_name='litecr_sync_standby'")"
  [[ "$sync_row" == 'litecr_sync_standby|streaming|sync' ]] && break
  sleep 0.1
done
[[ "$sync_row" == 'litecr_sync_standby|streaming|sync' ]] || fail STANDBY_NOT_SYNCHRONOUS
[[ "$(primary_psql "$bootstrap_role" postgres -qAt -c 'SHOW synchronous_commit')" == on ]] || fail SYNC_COMMIT_NOT_ON
[[ "$(primary_psql "$bootstrap_role" postgres -qAt -c 'SHOW synchronous_standby_names')" == 'FIRST 1 (litecr_sync_standby)' ]] || fail SYNC_STANDBY_POLICY

# Successful return from this INSERT/implicit commit is the synthetic client
# acknowledgement boundary; the primary waits for the configured sync flush.
primary_psql "$bootstrap_role" postgres -v ON_ERROR_STOP=1 -c \
  "INSERT INTO public.litecr_synthetic_ack_ledger(request_id,payload_digest) VALUES ('ack-before-failover-001','sha256:synthetic-001')" >/dev/null
commit_lsn="$(primary_psql "$bootstrap_role" postgres -qAt -c 'SELECT pg_current_wal_flush_lsn()')"
for _ in $(seq 1 100); do
  replay_lsn="$(standby_psql postgres -qAt -c 'SELECT pg_last_wal_replay_lsn()')"
  if [[ -n "$replay_lsn" ]] && PG_LSN_A="$replay_lsn" PG_LSN_B="$commit_lsn" python3 - <<'PY'
import os
def lsn(value):
    hi, lo = value.split("/")
    return (int(hi, 16) << 32) + int(lo, 16)
raise SystemExit(0 if lsn(os.environ["PG_LSN_A"]) >= lsn(os.environ["PG_LSN_B"]) else 1)
PY
  then break; fi
  sleep 0.1
done
[[ "$(standby_psql postgres -qAt -c "SELECT count(*) FROM public.litecr_synthetic_ack_ledger WHERE request_id='ack-before-failover-001'")" == 1 ]] || fail ACK_NOT_REPLAYED

# Observe strict-commit backpressure when the required standby is absent. Cancel
# nothing: the test holds the pending writer until the standby returns, avoiding
# any claim about client cancellation at the uncertain commit boundary.
"$pg_bin/pg_ctl" -D "$standby_data" -m immediate stop >/dev/null
[[ "$(primary_psql "$bootstrap_role" postgres -qAt -c "SELECT count(*) FROM pg_stat_replication WHERE application_name='litecr_sync_standby'")" == 0 ]] || fail STANDBY_STILL_CONNECTED
PGHOST=127.0.0.1 PGPORT="$primary_port" PGUSER="$bootstrap_role" PGDATABASE=postgres \
  PGAPPNAME=litecr_sync_outage_probe PGOPTIONS='-c synchronous_commit=on' \
  "$pg_bin/psql" -X --no-psqlrc -v ON_ERROR_STOP=1 -c \
  "INSERT INTO public.litecr_synthetic_ack_ledger(request_id,payload_digest) VALUES ('ack-after-standby-restored-001','sha256:synthetic-after-restore')" \
  > "$root/no-standby-write.log" 2>&1 &
writer_client_pid=$!
writer_backend_pid=''
for _ in $(seq 1 100); do
  writer_backend_pid="$(primary_psql "$bootstrap_role" postgres -qAt -c \
    "SELECT pid FROM pg_stat_activity WHERE application_name='litecr_sync_outage_probe' AND wait_event='SyncRep'")"
  [[ -n "$writer_backend_pid" ]] && break
  sleep 0.1
done
[[ "$writer_backend_pid" =~ ^[0-9]+$ ]] || fail NO_STANDBY_WRITE_DID_NOT_WAIT_FOR_SYNC
kill -0 "$writer_client_pid" 2>/dev/null || fail NO_STANDBY_WRITER_ALREADY_RETURNED

# Reconnect, verify sync, then stop/fence the old primary before promoting.
"$pg_bin/pg_ctl" -D "$standby_data" -l "$root/standby.log" start >/dev/null
wait_ready "$standby_port" "$bootstrap_role" || fail STANDBY_RESTART
sync_row=''
for _ in $(seq 1 100); do
  sync_row="$(primary_psql "$bootstrap_role" postgres -qAt -F '|' -c \
    "SELECT application_name, state, sync_state FROM pg_stat_replication WHERE application_name='litecr_sync_standby'")"
  [[ "$sync_row" == 'litecr_sync_standby|streaming|sync' ]] && break
  sleep 0.1
done
[[ "$sync_row" == 'litecr_sync_standby|streaming|sync' ]] || fail STANDBY_DID_NOT_RESYNC
wait "$writer_client_pid" || fail BLOCKED_WRITE_DID_NOT_RESUME
[[ "$(primary_psql "$bootstrap_role" postgres -qAt -c "SELECT count(*) FROM public.litecr_synthetic_ack_ledger WHERE request_id='ack-after-standby-restored-001'")" == 1 ]] || fail RESUMED_WRITE_MISSING_ON_PRIMARY
[[ "$(standby_psql postgres -qAt -c "SELECT count(*) FROM public.litecr_synthetic_ack_ledger WHERE request_id='ack-after-standby-restored-001'")" == 1 ]] || fail RESUMED_WRITE_MISSING_ON_STANDBY

"$pg_bin/pg_ctl" -D "$primary_data" -m immediate stop >/dev/null
primary_status=0
"$pg_bin/pg_ctl" -D "$primary_data" status >/dev/null 2>&1 || primary_status=$?
[[ "$primary_status" == 3 && ! -e "$primary_data/postmaster.pid" && ! -L "$primary_data/postmaster.pid" ]] || fail OLD_PRIMARY_NOT_FENCED
"$pg_bin/pg_ctl" -D "$standby_data" promote >/dev/null
new_primary=''
for _ in $(seq 1 100); do
  new_primary="$(standby_psql postgres -qAt -c 'SELECT NOT pg_is_in_recovery()' 2>/dev/null || true)"
  [[ "$new_primary" == t ]] && break
  sleep 0.1
done
[[ "$new_primary" == t ]] || fail PROMOTION
[[ "$(standby_psql postgres -qAt -c "SELECT count(*) FROM public.litecr_synthetic_ack_ledger WHERE request_id='ack-before-failover-001'")" == 1 ]] || fail ACK_MISSING_AFTER_PROMOTION
[[ "$(standby_psql postgres -qAt -c "SELECT count(*) FROM public.litecr_synthetic_ack_ledger WHERE request_id='ack-after-standby-restored-001'")" == 1 ]] || fail RESUMED_ACK_MISSING_AFTER_PROMOTION

printf 'SYNC_REHEARSAL=passed\nPOSTGRES_VERSION=16.15\nPRIMARY_PORT=%s\nSTANDBY_PORT=%s\n' "$primary_port" "$standby_port"
printf 'SYNC_STATE=%s\nSYNCHRONOUS_COMMIT=on\nACK_LEDGER_ID=ack-before-failover-001\nCOMMIT_WAL_FLUSH_LSN=%s\nSTANDBY_REPLAY_LSN=%s\n' \
  "$sync_row" "$commit_lsn" "$(standby_psql postgres -qAt -c 'SELECT pg_last_wal_replay_lsn()')"
printf 'NO_STANDBY_WRITE=observed_waiting_in_SyncRep_until_standby_restored_then_acknowledged\nOLD_PRIMARY=stopped_and_fenced_before_manual_promotion\nPROMOTED_STANDBY_ACK_ROWS=2_present\n'
printf 'LIMITATION=same_host_controlled_mechanics_only; not production RPO/RTO or HA acceptance\n'
