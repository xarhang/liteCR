#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

fail() { printf 'liteCR packaged API rehearsal failed [%s]\n' "$1" >&2; exit 1; }
[[ ( $# -eq 1 || $# -eq 2 ) && "$1" == /* && -f "$1" && ! -L "$1" \
  && $(basename -- "$1") =~ ^litecr-[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?-linux-x86_64\.tar\.gz$ ]] || fail RELEASE_ARCHIVE
schema_version="${2:-19}"
[[ "$schema_version" =~ ^(19|20|22|23|24|27)$ ]] || fail SUPPORTED_SCHEMA_VERSION_REQUIRED
archive="$(realpath -e -- "$1")"
archive_name="$(basename -- "$archive" .tar.gz)"
archive_version="${archive_name#litecr-}"
archive_version="${archive_version%-linux-x86_64}"
checksum="$archive.sha256"
[[ -f "$checksum" && ! -L "$checksum" ]] || fail CHECKSUM_SIDECAR
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
source "$repo/tools/operations/cleanup-owned-postgres-cluster.sh"
pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready psql createdb; do [[ -x "$pg_bin/$tool" ]] || fail "POSTGRESQL_TOOL_$tool"; done
for tool in systemd-run systemctl curl python3 tar sha256sum; do command -v "$tool" >/dev/null 2>&1 || fail "MISSING_$tool"; done
systemctl --user is-system-running >/dev/null 2>&1 || fail USER_SYSTEMD_REQUIRED
expected="$(awk 'NR==1 {print $1}' "$checksum")"
filename="$(awk 'NR==1 {sub(/^[^ ]+[[:space:]]+[*]?/, ""); print}' "$checksum")"
[[ "$expected" =~ ^[0-9a-f]{64}$ && "$filename" == "$(basename -- "$archive")" ]] || fail CHECKSUM_SIDECAR_INVALID
[[ "$(sha256sum -- "$archive" | awk '{print $1}')" == "$expected" ]] || fail ARCHIVE_CHECKSUM

root="$(mktemp -d "/tmp/litecr-p6-recovery-schema${schema_version}.XXXXXXXX")"
chmod 700 "$root"
root="$(realpath -e -- "$root")"
[[ "$root" == "/tmp/litecr-p6-recovery-schema${schema_version}."* && -d "$root" && ! -L "$root" ]] || fail TEMP_ROOT
printf 'litecr-p6-recovery-schema%s-v1\n' "$schema_version" > "$root/OWNER"
chmod 600 "$root/OWNER"
data="$root/postgres-data"
socket="$root/postgres-socket"
mkdir -m 700 "$socket"
service_unit="litecr-p6-api-schema${schema_version}-$$.service"
idp_pid=""
cleanup() {
  status=$?
  systemctl --user stop "$service_unit" >/dev/null 2>&1 || true
  systemctl --user reset-failed "$service_unit" >/dev/null 2>&1 || true
  if [[ -n "$idp_pid" ]]; then kill "$idp_pid" >/dev/null 2>&1 || true; wait "$idp_pid" 2>/dev/null || true; fi
  if ! litecr_cleanup_owned_postgres_cluster "$root" "$data" "$pg_bin/pg_ctl" "$schema_version"; then status=1; fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

tar -tzf "$archive" | while IFS= read -r entry; do
  [[ "$entry" != /* && "$entry" != .. && "$entry" != ../* && "$entry" != */../* && "$entry" != */.. ]] || fail ARCHIVE_PATH
done
tar -tvzf "$archive" | awk '$1 !~ /^d/ && $1 !~ /^-/ {exit 1}' || fail ARCHIVE_SPECIAL_FILE
mkdir -m 700 "$root/package"
tar -xzf "$archive" -C "$root/package" --no-same-owner
mapfile -t releases < <(find "$root/package" -mindepth 1 -maxdepth 1 -type d -name "$archive_name" -print)
(( ${#releases[@]} == 1 )) || fail PACKAGE_LAYOUT
release="${releases[0]}"
(cd "$release" && sha256sum --check --status -- SHA256SUMS) || fail INTERNAL_CHECKSUM
binary="$release/bin/litecr"
[[ -x "$binary" && ! -L "$binary" ]] || fail PACKAGE_BINARY
[[ -x "$release/share/tools/operations/backup-postgres.sh" && ! -L "$release/share/tools/operations/backup-postgres.sh" ]] || fail PACKAGE_BACKUP_TOOL
[[ -x "$release/share/tools/operations/restore-postgres-isolated.sh" && ! -L "$release/share/tools/operations/restore-postgres-isolated.sh" ]] || fail PACKAGE_RESTORE_TOOL
cmp -s -- "$release/share/tools/operations/backup-postgres.sh" "$repo/tools/operations/backup-postgres.sh" || fail PACKAGE_BACKUP_TOOL_CONTENT
cmp -s -- "$release/share/tools/operations/restore-postgres-isolated.sh" "$repo/tools/operations/restore-postgres-isolated.sh" || fail PACKAGE_RESTORE_TOOL_CONTENT
[[ "$("$binary" version --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["application_version"])')" == "$archive_version" ]] || fail APPLICATION_VERSION
[[ "$("$binary" version --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["schema_version"])')" == "$schema_version" ]] || fail SCHEMA_VERSION

port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"
idp_port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"
idp_issuer="http://127.0.0.1:$idp_port/realms/litecr"
python3 - "$idp_port" "$idp_issuer" >"$root/idp.log" 2>&1 <<'PY' &
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

port = int(sys.argv[1])
issuer = sys.argv[2]
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        print(self.path, flush=True)
        if self.path.endswith("/protocol/openid-connect/certs"):
            body = b'{"keys": []}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if not self.path.endswith("/.well-known/openid-configuration"):
            self.send_error(404)
            return
        metadata = {
            "issuer": issuer,
            "authorization_endpoint": issuer + "/protocol/openid-connect/auth",
            "token_endpoint": issuer + "/protocol/openid-connect/token",
            "jwks_uri": issuer + "/protocol/openid-connect/certs",
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        }
        body = json.dumps(metadata).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
HTTPServer(("127.0.0.1", port), Handler).serve_forever()
PY
idp_pid=$!
for _ in $(seq 1 50); do
  if curl --fail --silent --max-time 1 "$idp_issuer/.well-known/openid-configuration" >/dev/null; then break; fi
  sleep 0.1
done
curl --fail --silent --max-time 2 "$idp_issuer/.well-known/openid-configuration" >/dev/null || fail SYNTHETIC_IDP_NOT_READY

db_port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"
bootstrap_role="$(id -un)"
"$pg_bin/initdb" -D "$data" --username="$bootstrap_role" --auth-local=trust --auth-host=trust \
  --encoding=UTF8 --locale=C.UTF-8 --no-instructions > "$root/initdb.log"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" \
  -o "-h 127.0.0.1 -p $db_port -k $socket -c listen_addresses=127.0.0.1 -c unix_socket_directories=$socket" start >/dev/null
for _ in $(seq 1 50); do
  if "$pg_bin/pg_isready" -h 127.0.0.1 -p "$db_port" -U "$bootstrap_role" >/dev/null 2>&1; then break; fi
  sleep 0.1
done
"$pg_bin/pg_isready" -h 127.0.0.1 -p "$db_port" -U "$bootstrap_role" >/dev/null || fail POSTGRES_NOT_READY
[[ "$(sed -n '2p' "$data/postmaster.pid")" == "$data" ]] || fail CLUSTER_DATA_DIRECTORY
[[ "$(PGHOST=127.0.0.1 PGPORT="$db_port" PGUSER="$bootstrap_role" PGDATABASE=postgres "$pg_bin/psql" -XqAt -c 'SELECT host(inet_server_addr())')" == 127.0.0.1 ]] || fail CLUSTER_NOT_LOOPBACK
db="litecr_packaged_api_smoke"
PGHOST=127.0.0.1 PGPORT="$db_port" PGUSER="$bootstrap_role" PGDATABASE=postgres "$pg_bin/createdb" "$db"
database_url="postgresql://$bootstrap_role@127.0.0.1:$db_port/$db"
LITECR_MIGRATION_DATABASE_URL="$database_url" "$binary" migrate >/dev/null
[[ "$(PGHOST=127.0.0.1 PGPORT="$db_port" PGUSER="$bootstrap_role" PGDATABASE="$db" "$pg_bin/psql" -XqAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == "$schema_version" ]] || fail MIGRATION_SCHEMA_VERSION
LITECR_DATABASE_URL="$database_url" "$binary" doctor > "$root/doctor-local.txt" 2>&1 || fail DOCTOR_LOCAL
if [[ "$schema_version" =~ ^(24|27)$ ]]; then
  grep -Fq 'replication: no synchronous standby policy configured; commits rely on local WAL flush only; observed streaming standbys=' \
    "$root/doctor-local.txt" || fail DOCTOR_LOCAL_DURABILITY_SUMMARY
else
  grep -Fxq 'replication: no synchronous standby policy configured; commits rely on local WAL flush only' \
    "$root/doctor-local.txt" || fail DOCTOR_LOCAL_DURABILITY_SUMMARY
fi
grep -Fxq 'matching index: current' "$root/doctor-local.txt" || fail DOCTOR_MATCHING_INDEX_SUMMARY

bind_port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"
base_url="http://127.0.0.1:$bind_port"
systemd-run --user --quiet --no-block --unit="$service_unit" --property=Type=exec \
  --setenv="LITECR_DATABASE_URL=$database_url" --setenv=LITECR_ENV=development \
  --setenv="LITECR_BIND=127.0.0.1:$bind_port" --setenv="LITECR_OIDC_ISSUER_URL=$idp_issuer" \
  --setenv=LITECR_OIDC_CLIENT_ID=synthetic-client --setenv=LITECR_OIDC_CLIENT_SECRET=synthetic-secret \
  --setenv="LITECR_OIDC_REDIRECT_URI=$base_url/api/v1/access/oidc/callback" \
  --setenv="LITECR_WEB_ORIGIN=$base_url" --setenv=LITECR_COOKIE_SECURE=false "$binary" serve
ready=0
for _ in $(seq 1 100); do
  if curl --fail --silent --max-time 1 "$base_url/health/ready" > "$root/ready.json" 2>/dev/null; then ready=1; break; fi
  unit_state="$(systemctl --user show "$service_unit" --property=ActiveState --value 2>/dev/null || true)"
  # systemd-run --no-block can return before the transient unit is visible or
  # active; do not mistake that startup race for a service failure. Only stop
  # polling early once systemd reports an explicit failed state.
  [[ "$unit_state" == failed ]] && break
  sleep 0.2
done
[[ "$ready" == 1 ]] || { systemctl --user status "$service_unit" --no-pager >&2 || true; journalctl --user -u "$service_unit" -n 30 --no-pager >&2 || true; sed 's/^/synthetic OIDC: /' "$root/idp.log" >&2; fail API_SERVICE_NOT_READY; }
curl --fail --silent --show-error --max-time 5 "$base_url/health/live" > "$root/live.json" || fail API_LIVENESS
curl --fail --silent --show-error --max-time 5 "$base_url/" > "$root/index.html" || fail EMBEDDED_INDEX
asset="$(python3 - "$root/index.html" <<'PY'
from html.parser import HTMLParser
import sys
class Assets(HTMLParser):
    def __init__(self): super().__init__(); self.paths=[]
    def handle_starttag(self, tag, attrs):
        attrs=dict(attrs)
        for key in ("src", "href"):
            value=attrs.get(key, "")
            if value.startswith("/assets/") and value.endswith(".js"):
                self.paths.append(value)
p=Assets(); p.feed(open(sys.argv[1], encoding="utf-8").read())
if len(p.paths) != 1: raise SystemExit("expected one hashed JS entry asset")
print(p.paths[0])
PY
)" || fail ENTRY_ASSET_DISCOVERY
curl --fail --silent --show-error --max-time 5 "$base_url$asset" -o "$root/app.js" || fail EMBEDDED_ASSET
[[ -s "$root/app.js" ]] || fail EMBEDDED_ASSET_EMPTY
style_asset="$(python3 - "$root/index.html" <<'PY'
from html.parser import HTMLParser
import sys
class Assets(HTMLParser):
    def __init__(self): super().__init__(); self.paths=[]
    def handle_starttag(self, tag, attrs):
        attrs=dict(attrs)
        value=attrs.get("href", "")
        if tag == "link" and value.startswith("/assets/") and value.endswith(".css"):
            self.paths.append(value)
p=Assets(); p.feed(open(sys.argv[1], encoding="utf-8").read())
if len(p.paths) != 1: raise SystemExit("expected one hashed CSS asset")
print(p.paths[0])
PY
)" || fail STYLE_ASSET_DISCOVERY
curl --fail --silent --show-error --max-time 5 "$base_url$style_asset" -o "$root/app.css" || fail EMBEDDED_STYLE_ASSET
[[ -s "$root/app.css" ]] || fail EMBEDDED_STYLE_ASSET_EMPTY
if [[ "$schema_version" =~ ^(22|23|24|27)$ ]]; then
  grep -Fq '.nav-icon' "$root/app.css" || fail EMBEDDED_STYLE_ASSET_CONTENT
fi
mapfile -t locale_assets < <(grep -oE '(lo|th|vi|hi-IN)-[A-Za-z0-9_-]+\.js' "$root/app.js" | sort -u)
[[ "${#locale_assets[@]}" -eq 4 ]] || fail EMBEDDED_LOCALE_ASSET_REFERENCES
for locale_asset in "${locale_assets[@]}"; do
  curl --fail --silent --show-error --max-time 5 "$base_url/assets/$locale_asset" \
    -o "$root/$locale_asset" || fail "EMBEDDED_LOCALE_ASSET_$locale_asset"
  [[ -s "$root/$locale_asset" ]] || fail "EMBEDDED_LOCALE_ASSET_EMPTY_$locale_asset"
done
systemctl --user stop "$service_unit"
systemctl --user reset-failed "$service_unit" >/dev/null 2>&1 || true
printf 'schema%s package checksum/internal sums passed; migrations applied; transient systemd user service returned live/readiness 200 and served embedded UI/JS/CSS plus four built-in locale chunks; synthetic fixtures only.\n' "$schema_version"
