#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077
fail() { printf 'liteCR API failover rehearsal failed [%s]\n' "$1" >&2; exit 1; }
[[ ( $# == 1 || $# == 2 ) && "$1" == /* && -f "$1" && ! -L "$1" \
  && $(basename -- "$1") =~ ^litecr-[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?-linux-x86_64\.tar\.gz$ ]] || fail RELEASE_ARCHIVE
expected_schema="${2:-18}"
[[ "$expected_schema" =~ ^[1-9][0-9]{0,2}$ ]] || fail EXPECTED_SCHEMA
archive="$(realpath -e "$1")"; checksum="$archive.sha256"
package_name="$(basename -- "$archive" .tar.gz)"
[[ -f "$checksum" && ! -L "$checksum" ]] || fail CHECKSUM_SIDECAR
pg_bin="$(pg_config --bindir)"
for t in initdb pg_ctl pg_isready pg_basebackup psql; do [[ -x "$pg_bin/$t" ]] || fail "MISSING_$t"; done
for t in python3 systemd-run systemctl curl tar sha256sum; do command -v "$t" >/dev/null || fail "MISSING_$t"; done
postgres_version="$(pg_config --version)"
[[ "$postgres_version" =~ ^PostgreSQL\ (16\.15|17\.6|18\.6)([[:space:]]|$) ]] || fail POSTGRES_VERSION
[[ "$(id -u)" != 0 ]] || fail MUST_NOT_RUN_AS_ROOT
systemctl --user is-system-running >/dev/null 2>&1 || fail USER_SYSTEMD_REQUIRED
unset PGHOST PGHOSTADDR PGPORT PGDATABASE PGUSER PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGOPTIONS PGSSLMODE PGAPPNAME
export PATH="$pg_bin:$PATH"
db_user="$(id -un)"; [[ "$db_user" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || fail DB_USER
root="$(mktemp -d /tmp/litecr-api-sync-ack-v1.XXXXXXXX)"; chmod 700 "$root"; root="$(realpath -e "$root")"
[[ "$root" == /tmp/litecr-api-sync-ack-v1.* && ! -L "$root" ]] || fail TEMP_ROOT
printf 'litecr-api-sync-ack-v1\n' > "$root/OWNER"; chmod 600 "$root/OWNER"
primary="$root/primary"; standby="$root/standby"
mkdir -m 700 "$primary" "$standby" "$root/primary-socket" "$root/standby-socket" "$root/package"
unit="litecr-api-sync-ack-$$.service"; retry_unit="litecr-api-sync-ack-retry-$$.service"; idp_pid=""; loss_curl_pid=""
port() { python3 - <<'PY'
import socket
with socket.socket() as s:
 s.bind(('127.0.0.1',0)); print(s.getsockname()[1])
PY
}
primary_port="$(port)"; standby_port="$(port)"; api_port="$(port)"; idp_port="$(port)"
[[ "$primary_port" != "$standby_port" ]] || fail DUPLICATE_PORT
for p in "$primary_port" "$standby_port"; do "$pg_bin/pg_isready" -h 127.0.0.1 -p "$p" >/dev/null 2>&1 && fail PORT_IN_USE || true; done
stop_cluster() {
 local d="$1" rc=0
 [[ "$d" == "$primary" || "$d" == "$standby" ]] || return 1
 [[ -d "$d" && ! -L "$d" && "$(stat -c '%u:%a' "$d")" == "$(id -u):700" ]] || return 1
 if [[ -e "$d/postmaster.pid" || -L "$d/postmaster.pid" ]]; then
  [[ -f "$d/postmaster.pid" && ! -L "$d/postmaster.pid" && "$(sed -n '2p' "$d/postmaster.pid")" == "$d" ]] || return 1
  "$pg_bin/pg_ctl" -D "$d" -m immediate stop >/dev/null 2>&1 || true
 fi
 "$pg_bin/pg_ctl" -D "$d" status >/dev/null 2>&1 || rc=$?
 [[ "$rc" == 3 && ! -e "$d/postmaster.pid" && ! -L "$d/postmaster.pid" ]]
}
cleanup() {
 local rc=$?; trap - EXIT
 systemctl --user stop "$unit" >/dev/null 2>&1 || true; systemctl --user reset-failed "$unit" >/dev/null 2>&1 || true
 systemctl --user stop "$retry_unit" >/dev/null 2>&1 || true; systemctl --user reset-failed "$retry_unit" >/dev/null 2>&1 || true
 if [[ -n "$loss_curl_pid" ]]; then kill "$loss_curl_pid" >/dev/null 2>&1 || true; wait "$loss_curl_pid" 2>/dev/null || true; fi
 if [[ -n "$idp_pid" ]]; then kill "$idp_pid" >/dev/null 2>&1 || true; wait "$idp_pid" 2>/dev/null || true; fi
 if [[ "$root" == /tmp/litecr-api-sync-ack-v1.* && -d "$root" && ! -L "$root" && "$(realpath -e "$root" 2>/dev/null)" == "$root" \
   && "$(stat -c '%u:%a' "$root" 2>/dev/null)" == "$(id -u):700" && -f "$root/OWNER" && ! -L "$root/OWNER" \
   && "$(stat -c '%u:%a' "$root/OWNER" 2>/dev/null)" == "$(id -u):600" && "$(cat "$root/OWNER" 2>/dev/null)" == litecr-api-sync-ack-v1 \
   && "$primary" == "$root/primary" && "$standby" == "$root/standby" ]]; then
  if stop_cluster "$primary" && stop_cluster "$standby"; then rm -rf -- "$root" || rc=1; else echo "PRESERVED_FOR_INSPECTION=$root" >&2; rc=1; fi
 else echo "OWNERSHIP_CHECK_FAILED_PRESERVED=$root" >&2; rc=1; fi
 exit "$rc"
}
trap cleanup EXIT; trap 'exit 129' HUP; trap 'exit 130' INT; trap 'exit 143' TERM
sum="$(awk 'NR==1{print $1}' "$checksum")"; file="$(awk 'NR==1{sub(/^[^ ]+[[:space:]]+[*]?/,"");print}' "$checksum")"
[[ "$sum" =~ ^[0-9a-f]{64}$ && "$file" == "$(basename "$archive")" && "$(sha256sum "$archive"|awk '{print $1}')" == "$sum" ]] || fail PACKAGE_CHECKSUM
tar -tzf "$archive" | while read -r e; do [[ "$e" != /* && "$e" != .. && "$e" != ../* && "$e" != */../* && "$e" != */.. ]] || fail ARCHIVE_PATH; done
tar -tvzf "$archive" | awk '$1 !~ /^d/ && $1 !~ /^-/ {exit 1}' || fail ARCHIVE_SPECIAL_FILE
tar -xzf "$archive" -C "$root/package" --no-same-owner
release="$root/package/$package_name"; [[ -d "$release" && ! -L "$release" ]] || fail PACKAGE_LAYOUT
(cd "$release" && sha256sum --check --status -- SHA256SUMS) || fail PACKAGE_INTERNAL_CHECKSUM
bin="$release/bin/litecr"; [[ -x "$bin" && ! -L "$bin" ]] || fail PACKAGE_BINARY
[[ "$("$bin" version --json|python3 -c 'import json,sys;print(json.load(sys.stdin)["schema_version"])')" == "$expected_schema" ]] || fail PACKAGE_SCHEMA

"$pg_bin/initdb" -D "$primary" --username="$db_user" --auth-local=trust --auth-host=reject --encoding=UTF8 --locale=C.UTF-8 --no-instructions > "$root/initdb.log"
sed -i "1ihost all $db_user 127.0.0.1/32 trust" "$primary/pg_hba.conf"
sed -i '1ihost replication litecr_api_repl 127.0.0.1/32 trust' "$primary/pg_hba.conf"
cat >> "$primary/postgresql.conf" <<EOF
listen_addresses='127.0.0.1'
port=$primary_port
unix_socket_directories='$root/primary-socket'
wal_level=replica
max_wal_senders=4
max_replication_slots=2
hot_standby=on
fsync=on
full_page_writes=on
synchronous_commit=on
EOF
"$pg_bin/pg_ctl" -D "$primary" -l "$root/primary.log" start >/dev/null
psqlp() { PGHOST=127.0.0.1 PGPORT="$primary_port" PGUSER="$db_user" PGDATABASE=postgres "$pg_bin/psql" -X --no-psqlrc "$@"; }
psqls() { PGHOST=127.0.0.1 PGPORT="$standby_port" PGUSER="$db_user" PGDATABASE=postgres "$pg_bin/psql" -X --no-psqlrc "$@"; }
ready() { for _ in $(seq 1 100); do "$pg_bin/pg_isready" -h 127.0.0.1 -p "$1" -U "$db_user" >/dev/null 2>&1 && return; sleep .1; done; return 1; }
ready "$primary_port" || fail PRIMARY_START
[[ "$(psqlp -qAt -c 'select host(inet_server_addr())')" == 127.0.0.1 ]] || fail LOOPBACK_CHECK
psqlp -v ON_ERROR_STOP=1 -c 'CREATE ROLE litecr_api_repl LOGIN REPLICATION' >/dev/null
LITECR_MIGRATION_DATABASE_URL="postgresql://$db_user@127.0.0.1:$primary_port/postgres" "$bin" migrate >/dev/null
[[ "$(psqlp -qAt -c 'select version from litecr.schema_meta where singleton')" == "$expected_schema" ]] || fail MIGRATION
credential='synthetic-api-sync-standby-credential-0001'; hash="$(printf %s "$credential"|sha256sum|awk '{print $1}')"
psqlp -v ON_ERROR_STOP=1 -v credential_hash="$hash" <<'SQL' >/dev/null
INSERT INTO litecr.access_identity(actor_kind,issuer_url,external_subject) VALUES('integration_client','','sync-api-client');
INSERT INTO litecr.access_grant(actor_kind,issuer_url,external_subject,registry_id,organization_id,role_name) VALUES('integration_client','','sync-api-client','sync-registry','sync-org','integration_client');
INSERT INTO litecr.access_grant(actor_kind,issuer_url,external_subject,registry_id,organization_id,permission_name) VALUES('integration_client','','sync-api-client','sync-registry','sync-org','source:write');
INSERT INTO litecr.access_integration_credential(token_hash,issuer_url,external_subject) VALUES(decode(:'credential_hash','hex'),'','sync-api-client');
SQL
PGHOST=127.0.0.1 PGPORT="$primary_port" PGUSER=litecr_api_repl "$pg_bin/pg_basebackup" --pgdata="$standby" --format=plain --wal-method=stream --write-recovery-conf --dbname="host=127.0.0.1 port=$primary_port user=litecr_api_repl application_name=litecr_api_sync_standby" > "$root/basebackup.log" 2>&1 || fail BASEBACKUP
[[ -f "$standby/standby.signal" ]] || fail STANDBY_CONFIG
cat >> "$standby/postgresql.conf" <<EOF
listen_addresses='127.0.0.1'
port=$standby_port
unix_socket_directories='$root/standby-socket'
hot_standby=on
EOF
"$pg_bin/pg_ctl" -D "$standby" -l "$root/standby.log" start >/dev/null; ready "$standby_port" || fail STANDBY_START
psqlp -v ON_ERROR_STOP=1 -c "ALTER SYSTEM SET synchronous_standby_names='FIRST 1 (litecr_api_sync_standby)'" >/dev/null
psqlp -v ON_ERROR_STOP=1 -c 'SELECT pg_reload_conf()' >/dev/null
sync=''; for _ in $(seq 1 100); do sync="$(psqlp -qAt -F '|' -c "select state,sync_state from pg_stat_replication where application_name='litecr_api_sync_standby'")"; [[ "$sync" == 'streaming|sync' ]] && break; sleep .1; done
[[ "$sync" == streaming\|sync && "$(psqlp -qAt -c 'show synchronous_commit')" == on ]] || fail SYNC_STANDBY

issuer="http://127.0.0.1:$idp_port/realms/litecr"
python3 - "$idp_port" "$issuer" >"$root/idp.log" 2>&1 <<'PY' &
import json,sys
from http.server import BaseHTTPRequestHandler,HTTPServer
p,i=int(sys.argv[1]),sys.argv[2]
class H(BaseHTTPRequestHandler):
 def do_GET(self):
  b=b'{"keys": []}' if self.path.endswith('/certs') else json.dumps({'issuer':i,'authorization_endpoint':i+'/auth','token_endpoint':i+'/token','jwks_uri':i+'/protocol/openid-connect/certs','response_types_supported':['code'],'subject_types_supported':['public'],'id_token_signing_alg_values_supported':['RS256']}).encode() if self.path.endswith('/.well-known/openid-configuration') else None
  if b is None:self.send_error(404);return
  self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(b)));self.end_headers();self.wfile.write(b)
HTTPServer(('127.0.0.1',p),H).serve_forever()
PY
idp_pid=$!
for _ in $(seq 1 50); do curl -fsS --max-time 1 "$issuer/.well-known/openid-configuration" >/dev/null 2>&1 && break; sleep .1; done
curl -fsS --max-time 2 "$issuer/.well-known/openid-configuration" >/dev/null || fail IDP_START
url="http://127.0.0.1:$api_port"
systemd-run --user --quiet --no-block --unit="$unit" --property=Type=exec --setenv="LITECR_DATABASE_URL=postgresql://$db_user@127.0.0.1:$primary_port/postgres" --setenv=LITECR_ENV=development --setenv="LITECR_BIND=127.0.0.1:$api_port" --setenv="LITECR_OIDC_ISSUER_URL=$issuer" --setenv=LITECR_OIDC_CLIENT_ID=synthetic-client --setenv=LITECR_OIDC_CLIENT_SECRET=synthetic-secret --setenv="LITECR_OIDC_REDIRECT_URI=$url/api/v1/access/oidc/callback" --setenv="LITECR_WEB_ORIGIN=$url" --setenv=LITECR_COOKIE_SECURE=false "$bin" serve
ok=0; for _ in $(seq 1 100); do
  curl -fsS --max-time 1 "$url/health/ready" >/dev/null 2>&1 && { ok=1; break; }
  state="$(systemctl --user show "$unit" --property=ActiveState --value 2>/dev/null || true)"
  [[ "$state" == inactive || "$state" == failed ]] && break
  sleep .2
done
[[ $ok == 1 ]] || { systemctl --user status "$unit" --no-pager >&2 || true; fail API_READY; }
body="$root/api-response.json"
payload='{"registry_id":"sync-registry","organization_id":"sync-org","source_system_id":"synthetic-api","source_record_key":"api-sync-ack-001","source_version":"v1","assertions":[{"field_path":"name.original","original_value":"Synthetic API Sync Patient","language_tag":"en","script":"Latn"}]}'
http="$(curl -sS --max-time 15 -o "$body" -w '%{http_code}' -X POST "$url/api/v1/source-records" -H "Authorization: Bearer $credential" -H 'Content-Type: application/json' -H 'Idempotency-Key: api-sync-ack-001' --data "$payload")"
[[ "$http" == 202 ]] || { cat "$body" >&2; fail API_ACK_HTTP; }
receipt="$(python3 - "$body" <<'PY'
import json,sys
d=json.load(open(sys.argv[1],encoding='utf8')); assert d['meta']['replayed'] is False and d['data']['outcome']=='accepted'; print(d['data']['source_record_id'])
PY
)" || fail API_ACK_BODY
[[ "$receipt" =~ ^[0-9a-f-]{36}$ ]] || fail API_ACK_ID
graph="$(psqlp -qAt -F '|' -c "select (select count(*) from litecr.source_record where source_record_id='$receipt'::uuid),(select count(*) from litecr.source_record_version where source_record_id='$receipt'::uuid),(select count(*) from litecr.mutation_audit where resource_ref='$receipt'),(select count(*) from litecr.outbox_event where payload::text like '%'||'$receipt'||'%'),(select count(*) from litecr.registry_idempotency where source_record_id='$receipt'::uuid)")"
[[ "$graph" == '1|1|1|1|1' ]] || fail API_GRAPH_NOT_COMMITTED

# Prove the API does not acknowledge a second write while the configured sync
# standby is absent. Observe the actual PostgreSQL SyncRep wait before letting
# the client time out; this avoids relying on timing or a guessed sleep alone.
"$pg_bin/pg_ctl" -D "$standby" -m immediate stop >/dev/null || fail STANDBY_STOP_FOR_ACK_TEST
rc=0; "$pg_bin/pg_ctl" -D "$standby" status >/dev/null 2>&1 || rc=$?
[[ "$rc" == 3 && ! -e "$standby/postmaster.pid" && ! -L "$standby/postmaster.pid" ]] || fail STANDBY_NOT_STOPPED_FOR_ACK_TEST
standby_replicas=""
for _ in $(seq 1 100); do
 standby_replicas="$(psqlp -qAt -c "select count(*) from pg_stat_replication where application_name='litecr_api_sync_standby'")"
 [[ "$standby_replicas" == 0 ]] && break
 sleep .1
done
[[ "$standby_replicas" == 0 ]] || fail STANDBY_STILL_CONNECTED_DURING_ACK_TEST
loss_key='api-sync-ack-standby-loss-001'
loss_payload='{"registry_id":"sync-registry","organization_id":"sync-org","source_system_id":"synthetic-api","source_record_key":"api-sync-ack-standby-loss-001","source_version":"v1","assertions":[{"field_path":"name.original","original_value":"Synthetic API Standby Loss Patient","language_tag":"en","script":"Latn"}]}'
loss_body="$root/api-standby-loss-response.json"; loss_http_file="$root/api-standby-loss-http.txt"
curl -sS --max-time 8 -o "$loss_body" -w '%{http_code}' -X POST "$url/api/v1/source-records" -H "Authorization: Bearer $credential" -H 'Content-Type: application/json' -H "Idempotency-Key: $loss_key" --data "$loss_payload" > "$loss_http_file" 2> "$root/api-standby-loss-curl.log" &
loss_curl_pid=$!
sync_waits=0
for _ in $(seq 1 50); do
 sync_waits="$(psqlp -qAt -c "select count(*) from pg_stat_activity where datname='postgres' and wait_event='SyncRep'")"
 [[ "$sync_waits" =~ ^[1-9][0-9]*$ ]] && break
 kill -0 "$loss_curl_pid" 2>/dev/null || break
 sleep .1
done
[[ "$sync_waits" =~ ^[1-9][0-9]*$ ]] || fail API_WRITE_NOT_OBSERVED_WAITING_FOR_SYNC
curl_rc=0; wait "$loss_curl_pid" || curl_rc=$?; loss_curl_pid=""
loss_http="$(cat "$loss_http_file")"
[[ "$curl_rc" != 0 && "$loss_http" == 000 ]] || { [[ -f "$loss_body" ]] && cat "$loss_body" >&2 || true; fail API_ACK_WITHOUT_SYNC_STANDBY; }
[[ ! -s "$loss_body" ]] || fail API_RETURNED_BODY_WITHOUT_SYNC_STANDBY

# Reconnect the same standby, then retry the identical request and key. The
# first attempt may have rolled back on client cancellation or committed after
# reconnect; both are safe if retry is accepted/replayed and effects occur once.
"$pg_bin/pg_ctl" -D "$standby" -l "$root/standby.log" start >/dev/null || fail STANDBY_RESTART_AFTER_ACK_TEST
ready "$standby_port" || fail STANDBY_READY_AFTER_ACK_TEST
sync=''; for _ in $(seq 1 100); do sync="$(psqlp -qAt -F '|' -c "select state,sync_state from pg_stat_replication where application_name='litecr_api_sync_standby'")"; [[ "$sync" == 'streaming|sync' ]] && break; sleep .1; done
[[ "$sync" == streaming\|sync ]] || fail SYNC_STANDBY_NOT_RESTORED_AFTER_ACK_TEST
http="$(curl -sS --max-time 15 -o "$root/api-standby-loss-retry-response.json" -w '%{http_code}' -X POST "$url/api/v1/source-records" -H "Authorization: Bearer $credential" -H 'Content-Type: application/json' -H "Idempotency-Key: $loss_key" --data "$loss_payload")"
[[ "$http" == 200 || "$http" == 202 ]] || { cat "$root/api-standby-loss-retry-response.json" >&2; fail RETRY_AFTER_STANDBY_RETURN_HTTP; }
loss_retry_http="$http"
loss_receipt="$(python3 - "$root/api-standby-loss-retry-response.json" "$http" <<'PY'
import json,sys
d=json.load(open(sys.argv[1],encoding='utf8'))
assert d['data']['outcome']=='accepted'
assert (sys.argv[2]=='200' and d['meta']['replayed'] is True) or (sys.argv[2]=='202' and d['meta']['replayed'] is False)
print(d['data']['source_record_id'])
PY
)" || fail RETRY_AFTER_STANDBY_RETURN_BODY
[[ "$loss_receipt" =~ ^[0-9a-f-]{36}$ ]] || fail RETRY_AFTER_STANDBY_RETURN_ID
loss_graph="$(psqlp -qAt -F '|' -c "select (select count(*) from litecr.source_record where registry_id='sync-registry' and organization_id='sync-org' and source_system_id='synthetic-api' and source_record_key='$loss_key'),(select count(*) from litecr.source_record_version where source_record_id='$loss_receipt'::uuid),(select count(*) from litecr.mutation_audit where resource_ref='$loss_receipt'),(select count(*) from litecr.outbox_event where payload::text like '%'||'$loss_receipt'||'%'),(select count(*) from litecr.registry_idempotency where source_record_id='$loss_receipt'::uuid)")"
[[ "$loss_graph" == '1|1|1|1|1' ]] || fail RETRY_AFTER_STANDBY_RETURN_DUPLICATE_EFFECTS
flush_lsn="$(psqlp -qAt -c 'select pg_current_wal_flush_lsn()')"; replay_lsn="$(psqls -qAt -c 'select pg_last_wal_replay_lsn()')"
[[ -n "$flush_lsn" && -n "$replay_lsn" ]] || fail REPLAY_LSN
PG_LSN_A="$replay_lsn" PG_LSN_B="$flush_lsn" python3 - <<'PY' || fail ACK_NOT_REPLAYED_BEFORE_FAILOVER
import os
def lsn(value):
 hi,lo=value.split('/'); return (int(hi,16)<<32)+int(lo,16)
raise SystemExit(0 if lsn(os.environ['PG_LSN_A']) >= lsn(os.environ['PG_LSN_B']) else 1)
PY
systemctl --user stop "$unit" >/dev/null || fail API_STOP
systemctl --user reset-failed "$unit" >/dev/null 2>&1 || true
"$pg_bin/pg_ctl" -D "$primary" -m immediate stop >/dev/null
rc=0; "$pg_bin/pg_ctl" -D "$primary" status >/dev/null 2>&1 || rc=$?
[[ "$rc" == 3 && ! -e "$primary/postmaster.pid" && ! -L "$primary/postmaster.pid" ]] || fail PRIMARY_NOT_FENCED
"$pg_bin/pg_ctl" -D "$standby" promote >/dev/null
promoted=0; for _ in $(seq 1 100); do [[ "$(psqls -qAt -c 'select not pg_is_in_recovery()' 2>/dev/null || true)" == t ]] && { promoted=1; break; }; sleep .1; done
[[ $promoted == 1 ]] || fail PROMOTION
after="$(psqls -qAt -F '|' -c "select (select count(*) from litecr.source_record where source_record_id='$receipt'::uuid),(select count(*) from litecr.source_record_version where source_record_id='$receipt'::uuid),(select count(*) from litecr.mutation_audit where resource_ref='$receipt'),(select count(*) from litecr.outbox_event where payload::text like '%'||'$receipt'||'%'),(select count(*) from litecr.registry_idempotency where source_record_id='$receipt'::uuid)")"
[[ "$after" == '1|1|1|1|1' ]] || fail ACK_GRAPH_MISSING_AFTER_PROMOTION
url="http://127.0.0.1:$api_port"
systemd-run --user --quiet --no-block --unit="$retry_unit" --property=Type=exec --setenv="LITECR_DATABASE_URL=postgresql://$db_user@127.0.0.1:$standby_port/postgres" --setenv=LITECR_ENV=development --setenv="LITECR_BIND=127.0.0.1:$api_port" --setenv="LITECR_OIDC_ISSUER_URL=$issuer" --setenv=LITECR_OIDC_CLIENT_ID=synthetic-client --setenv=LITECR_OIDC_CLIENT_SECRET=synthetic-secret --setenv="LITECR_OIDC_REDIRECT_URI=$url/api/v1/access/oidc/callback" --setenv="LITECR_WEB_ORIGIN=$url" --setenv=LITECR_COOKIE_SECURE=false "$bin" serve
ok=0; for _ in $(seq 1 100); do
  curl -fsS --max-time 1 "$url/health/ready" >/dev/null 2>&1 && { ok=1; break; }
  state="$(systemctl --user show "$retry_unit" --property=ActiveState --value 2>/dev/null || true)"
  [[ "$state" == inactive || "$state" == failed ]] && break
  sleep .2
done
if [[ $ok != 1 ]]; then
 curl -sS --max-time 2 -i "$url/health/ready" >&2 || true
 systemctl --user status "$retry_unit" --no-pager >&2 || true
 journalctl --user -u "$retry_unit" -n 30 --no-pager >&2 || true
 fail PROMOTED_API_READY
fi
http="$(curl -sS --max-time 15 -o "$root/api-retry-response.json" -w '%{http_code}' -X POST "$url/api/v1/source-records" -H "Authorization: Bearer $credential" -H 'Content-Type: application/json' -H 'Idempotency-Key: api-sync-ack-001' --data "$payload")"
[[ "$http" == 200 ]] || { cat "$root/api-retry-response.json" >&2; fail RETRY_AFTER_PROMOTION_HTTP; }
retry_receipt="$(python3 - "$root/api-retry-response.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1],encoding='utf8')); assert d['meta']['replayed'] is True and d['data']['outcome']=='accepted'; print(d['data']['source_record_id'])
PY
)" || fail RETRY_AFTER_PROMOTION_BODY
[[ "$retry_receipt" == "$receipt" ]] || fail RETRY_AFTER_PROMOTION_RECEIPT
retry_graph="$(psqls -qAt -F '|' -c "select (select count(*) from litecr.source_record where source_record_id='$receipt'::uuid),(select count(*) from litecr.source_record_version where source_record_id='$receipt'::uuid),(select count(*) from litecr.mutation_audit where resource_ref='$receipt'),(select count(*) from litecr.outbox_event where payload::text like '%'||'$receipt'||'%'),(select count(*) from litecr.registry_idempotency where source_record_id='$receipt'::uuid)")"
[[ "$retry_graph" == '1|1|1|1|1' ]] || fail RETRY_CREATED_DUPLICATE_EFFECTS
systemctl --user stop "$retry_unit" >/dev/null || fail PROMOTED_API_STOP
systemctl --user reset-failed "$retry_unit" >/dev/null 2>&1 || true
kill "$idp_pid" >/dev/null 2>&1 || true; wait "$idp_pid" 2>/dev/null || true; idp_pid=""
printf 'API_SYNC_ACK_REHEARSAL=passed\nPACKAGE_SCHEMA=%s\nPOSTGRES_VERSION=%s\nHTTP_ACK_STATUS=202\nAPI_RECEIPT=%s\nSYNC_STATE=%s\nPRIMARY_FLUSH_LSN=%s\nSTANDBY_REPLAY_LSN=%s\nSTANDBY_LOSS_CLIENT_HTTP=%s\nSTANDBY_LOSS_CLIENT_EXIT=%s\nSTANDBY_LOSS_RETRY_HTTP=%s\nSTANDBY_LOSS_RETRY_RECEIPT=%s\nSTANDBY_LOSS_RETRY_GRAPH=source,version,audit,outbox,idempotency_once\nPROMOTED_GRAPH=source,version,audit,outbox,idempotency_present\nRETRY_AFTER_PROMOTION=same_receipt_replayed_no_duplicate_effects\n' "$expected_schema" "$postgres_version" "$receipt" "$sync" "$flush_lsn" "$replay_lsn" "$loss_http" "$curl_rc" "$loss_retry_http" "$loss_receipt"
printf 'OLD_PRIMARY=stopped_and_confirmed_before_manual_promotion\nLIMITATION=same-host synthetic package; no automatic fencing, host-loss, RPO/RTO, or production acceptance\n'
