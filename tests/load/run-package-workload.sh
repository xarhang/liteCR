#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

fail() { printf 'liteCR synthetic load rehearsal failed [%s]\n' "$1" >&2; exit 1; }
[[ $# -ge 1 && $# -le 3 && "$1" == /* && -f "$1" && ! -L "$1" ]] || fail USAGE
archive="$(realpath -e -- "$1")"
records="${2:-240}"
concurrency="${3:-8}"
[[ "$records" =~ ^[0-9]{2,4}$ && "$records" -ge 100 && "$records" -le 1000 ]] || fail RECORD_COUNT
[[ "$concurrency" =~ ^[1-9][0-9]?$ && "$concurrency" -le 32 ]] || fail CONCURRENCY
expected_archive_sha='f8989837896c28ba586cf18d0b13de3e7c6805df096cb08a12b4b3c29e81404a'
expected_binary_sha='ad6b114a395b36bafd2491c8b77a2cf1c056c7afdbe9ba3c722518683143582d'
[[ "$(sha256sum "$archive" | awk '{print $1}')" == "$expected_archive_sha" ]] || fail EXACT_ARCHIVE_SHA256
checksum="$archive.sha256"
[[ -f "$checksum" && ! -L "$checksum" ]] || fail CHECKSUM_SIDECAR
[[ "$(awk 'NR==1{print $1}' "$checksum")" == "$expected_archive_sha" ]] || fail CHECKSUM_SIDECAR_VALUE
[[ "$(awk 'NR==1{sub(/^[^ ]+[[:space:]]+[*]?/,"");print}' "$checksum")" == "$(basename "$archive")" ]] || fail CHECKSUM_SIDECAR_NAME

pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready psql createdb; do [[ -x "$pg_bin/$tool" ]] || fail "MISSING_POSTGRES_$tool"; done
for tool in python3 curl tar sha256sum; do command -v "$tool" >/dev/null 2>&1 || fail "MISSING_$tool"; done
[[ "$(pg_config --version)" =~ ^PostgreSQL\ 18\. ]] || fail POSTGRESQL_18_REQUIRED
[[ "$(id -u)" != 0 ]] || fail MUST_NOT_RUN_AS_ROOT
unset PGHOST PGHOSTADDR PGPORT PGDATABASE PGUSER PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGOPTIONS PGAPPNAME
export PATH="$pg_bin:$PATH"
db_user="$(id -un)"
[[ "$db_user" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || fail DB_USER
root="$(mktemp -d /tmp/litecr-load-v1.XXXXXXXX)"; chmod 700 "$root"; root="$(realpath -e "$root")"
[[ "$root" == /tmp/litecr-load-v1.* && ! -L "$root" ]] || fail TEMP_ROOT
printf 'litecr-load-v1\n' > "$root/OWNER"; chmod 600 "$root/OWNER"
data="$root/pgdata"; socket="$root/socket"; package="$root/package"
mkdir -m 700 "$data" "$socket" "$package"
api_pid=''; idp_pid=''
port() { python3 - <<'PY'
import socket
with socket.socket() as s:
    s.bind(('127.0.0.1', 0))
    print(s.getsockname()[1])
PY
}
db_port="$(port)"; api_port="$(port)"; idp_port="$(port)"
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
  [[ -z "$api_pid" ]] || { kill "$api_pid" >/dev/null 2>&1 || true; wait "$api_pid" 2>/dev/null || true; }
  [[ -z "$idp_pid" ]] || { kill "$idp_pid" >/dev/null 2>&1 || true; wait "$idp_pid" 2>/dev/null || true; }
  if [[ "$root" == /tmp/litecr-load-v1.* && -d "$root" && ! -L "$root" && "$(realpath -e "$root" 2>/dev/null)" == "$root" \
    && "$(stat -c '%u:%a' "$root" 2>/dev/null)" == "$(id -u):700" && -f "$root/OWNER" && ! -L "$root/OWNER" \
    && "$(stat -c '%u:%a' "$root/OWNER" 2>/dev/null)" == "$(id -u):600" && "$(cat "$root/OWNER" 2>/dev/null)" == litecr-load-v1 \
    && "$data" == "$root/pgdata" ]]; then
    if stop_cluster; then rm -rf -- "$root" || rc=1; else printf 'PRESERVED_FOR_INSPECTION=%s\n' "$root" >&2; rc=1; fi
  else printf 'OWNERSHIP_CHECK_FAILED_PRESERVED=%s\n' "$root" >&2; rc=1; fi
  exit "$rc"
}
trap cleanup EXIT; trap 'exit 129' HUP; trap 'exit 130' INT; trap 'exit 143' TERM

tar -tzf "$archive" | while IFS= read -r entry; do [[ "$entry" != /* && "$entry" != .. && "$entry" != ../* && "$entry" != */../* && "$entry" != */.. ]] || fail ARCHIVE_PATH; done
tar -tvzf "$archive" | awk '$1 !~ /^d/ && $1 !~ /^-/ {exit 1}' || fail ARCHIVE_SPECIAL_FILE
tar -xzf "$archive" -C "$package" --no-same-owner
release="$package/litecr-0.1.0-linux-x86_64"
[[ -d "$release" && ! -L "$release" ]] || fail PACKAGE_LAYOUT
(cd "$release" && sha256sum --check --status -- SHA256SUMS) || fail PACKAGE_INTERNAL_SHA256
bin="$release/bin/litecr"
[[ -x "$bin" && ! -L "$bin" && "$(sha256sum "$bin" | awk '{print $1}')" == "$expected_binary_sha" ]] || fail EXACT_BINARY_SHA256
version_json="$("$bin" version --json)"
python3 -c 'import json,sys; v=json.load(sys.stdin); assert v["application_version"]=="0.1.0" and v["schema_version"]==24' <<<"$version_json" || fail PACKAGE_VERSION

"$pg_bin/initdb" -D "$data" --username="$db_user" --auth-local=trust --auth-host=reject --encoding=UTF8 --locale=C.UTF-8 --no-instructions > "$root/initdb.log"
sed -i "1ihost all $db_user 127.0.0.1/32 trust" "$data/pg_hba.conf"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" -o "-h 127.0.0.1 -p $db_port -k $socket -c listen_addresses=127.0.0.1 -c unix_socket_directories=$socket -c max_connections=100" start >/dev/null
for _ in $(seq 1 100); do "$pg_bin/pg_isready" -h 127.0.0.1 -p "$db_port" -U "$db_user" >/dev/null 2>&1 && break; sleep .1; done
"$pg_bin/pg_isready" -h 127.0.0.1 -p "$db_port" -U "$db_user" >/dev/null || fail POSTGRES_NOT_READY
[[ "$(PGHOST=127.0.0.1 PGPORT="$db_port" PGUSER="$db_user" PGDATABASE=postgres "$pg_bin/psql" -XqAt -c 'select host(inet_server_addr())')" == 127.0.0.1 ]] || fail LOOPBACK_BIND
db=litecr_synthetic_load
PGHOST=127.0.0.1 PGPORT="$db_port" PGUSER="$db_user" PGDATABASE=postgres "$pg_bin/createdb" "$db"
database_url="postgresql://$db_user@127.0.0.1:$db_port/$db"
LITECR_MIGRATION_DATABASE_URL="$database_url" "$bin" migrate > "$root/migrate.log" 2>&1 || fail PACKAGE_MIGRATION
[[ "$(PGHOST=127.0.0.1 PGPORT="$db_port" PGUSER="$db_user" PGDATABASE="$db" "$pg_bin/psql" -XqAt -c 'select version from litecr.schema_meta where singleton')" == 24 ]] || fail SCHEMA_VERSION

token='synthetic-load-only-credential-not-for-reuse'; token_hash="$(printf %s "$token" | sha256sum | awk '{print $1}')"
credential_file="$root/credential"
printf '%s' "$token" > "$credential_file"; chmod 600 "$credential_file"
PGHOST=127.0.0.1 PGPORT="$db_port" PGUSER="$db_user" PGDATABASE="$db" "$pg_bin/psql" -X -v ON_ERROR_STOP=1 -v token_hash="$token_hash" >/dev/null <<'SQL'
INSERT INTO litecr.access_identity(actor_kind,issuer_url,external_subject) VALUES('integration_client','','load-runner');
INSERT INTO litecr.access_integration_credential(token_hash,issuer_url,external_subject) VALUES(decode(:'token_hash','hex'),'','load-runner');
INSERT INTO litecr.access_grant(actor_kind,issuer_url,external_subject,registry_id,organization_id,permission_name) VALUES
('integration_client','','load-runner','synthetic-registry','synthetic-org','source:write'),
('integration_client','','load-runner','synthetic-registry','synthetic-org','person:read'),
('integration_client','','load-runner','synthetic-registry','synthetic-org','person:search'),
('integration_client','','load-runner','synthetic-registry','synthetic-org','match:request'),
('integration_client','','load-runner','synthetic-registry','synthetic-org','match:evidence:read');
SQL

issuer="http://127.0.0.1:$idp_port/realms/litecr-load"
python3 - "$idp_port" "$issuer" >"$root/idp.log" 2>&1 <<'PY' &
import json,sys
from http.server import BaseHTTPRequestHandler,HTTPServer
p,i=int(sys.argv[1]),sys.argv[2]
class H(BaseHTTPRequestHandler):
 def do_GET(self):
  if self.path.endswith('/certs'): body=b'{"keys": []}'
  elif self.path.endswith('/.well-known/openid-configuration'):
   body=json.dumps({'issuer':i,'authorization_endpoint':i+'/auth','token_endpoint':i+'/token','jwks_uri':i+'/certs','response_types_supported':['code'],'subject_types_supported':['public'],'id_token_signing_alg_values_supported':['RS256']}).encode()
  else: self.send_error(404);return
  self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def log_message(self, *_): pass
HTTPServer(('127.0.0.1',p),H).serve_forever()
PY
idp_pid=$!
for _ in $(seq 1 50); do curl -fsS --max-time 1 "$issuer/.well-known/openid-configuration" >/dev/null 2>&1 && break; sleep .1; done
curl -fsS --max-time 2 "$issuer/.well-known/openid-configuration" >/dev/null || fail LOCAL_IDP_NOT_READY
url="http://127.0.0.1:$api_port"
LITECR_DATABASE_URL="$database_url" LITECR_DATABASE_MAX_CONNECTIONS=10 LITECR_ENV=development LITECR_BIND="127.0.0.1:$api_port" \
  LITECR_OIDC_ISSUER_URL="$issuer" LITECR_OIDC_CLIENT_ID=synthetic-load-client LITECR_OIDC_CLIENT_SECRET=synthetic-load-secret \
  LITECR_OIDC_REDIRECT_URI="$url/api/v1/access/oidc/callback" LITECR_WEB_ORIGIN="$url" LITECR_COOKIE_SECURE=false \
  "$bin" serve >"$root/api.log" 2>&1 &
api_pid=$!
ready=0
for _ in $(seq 1 100); do curl -fsS --max-time 1 "$url/health/ready" >/dev/null 2>&1 && { ready=1; break; }; kill -0 "$api_pid" >/dev/null 2>&1 || break; sleep .2; done
[[ "$ready" == 1 ]] || fail API_NOT_READY

read -r storage_type storage_size storage_used storage_free < <(df -B1 -T "$root" | awk 'NR==2{print $2,$3,$4,$5}')
printf 'LITECR_LOAD_RUN root=%s archive_sha256=%s binary_sha256=%s version=0.1.0 schema=24 postgres="%s" database_max_connections=100 api_pool_max_connections=10 concurrency=%s records=%s cpu="%s" cpu_logical=%s ram_total_kb=%s ram_available_kb=%s kernel="%s" storage_type=%s storage_total_bytes=%s storage_free_bytes=%s\n' \
  "$root" "$expected_archive_sha" "$expected_binary_sha" "$(pg_config --version)" "$concurrency" "$records" \
  "$(lscpu | awk -F: '/Model name/{gsub(/^[ \t]+/,"",$2);print $2;exit}')" "$(nproc)" "$(awk '/MemTotal/{print $2}' /proc/meminfo)" \
  "$(awk '/MemAvailable/{print $2}' /proc/meminfo)" "$(uname -srmo)" "$storage_type" "$storage_size" "$storage_free"
python3 "$(dirname "$0")/package-workload.py" "$url" "$credential_file" "$records" "$concurrency"
printf 'LITECR_LOAD_DATABASE_COUNTS source_records=%s evidence_runs=%s database_sessions_snapshot=%s\n' \
  "$(PGHOST=127.0.0.1 PGPORT="$db_port" PGUSER="$db_user" PGDATABASE="$db" "$pg_bin/psql" -XqAt -c "select count(*) from litecr.source_record where registry_id='synthetic-registry' and organization_id='synthetic-org'")" \
  "$(PGHOST=127.0.0.1 PGPORT="$db_port" PGUSER="$db_user" PGDATABASE="$db" "$pg_bin/psql" -XqAt -c 'select count(*) from matching.evidence_run')" \
  "$(PGHOST=127.0.0.1 PGPORT="$db_port" PGUSER="$db_user" PGDATABASE="$db" "$pg_bin/psql" -XqAt -c "select count(*) from pg_stat_activity where datname='litecr_synthetic_load'")"
printf 'cleanup=owner-checked-temporary-PG18-cluster; API_and_synthetic_IdP_stopped; loopback_only=true; no_system_install=true\n'
