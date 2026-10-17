#!/usr/bin/env bash
set -euo pipefail
set +x

[[ $# == 1 && "$1" == /* && -f "$1" && ! -L "$1" \
  && $(basename -- "$1") =~ ^litecr-[0-9]+\.[0-9]+\.[0-9]+-linux-x86_64\.tar\.gz$ ]] || {
  echo "usage: $0 /absolute/path/to/litecr-VERSION-linux-x86_64.tar.gz" >&2
  exit 2
}

expected_old_archive_sha256=2d001a4e18f3ca7bc0d9bda447e6ea263a4e76961261016cfd8208dcaf790a12
expected_old_binary_sha256=a9cc08e0968d4f3d6da2f56cf648b00e5b5419de8cf3b0063e541ad109204dc7
old_binary=/home/xarhang/litecr-p6-schema23-host-target-package-20261011/litecr-0.1.0-linux-x86_64/bin/litecr
new_archive="$(realpath -e -- "$1")"
archive_name="$(basename -- "$new_archive" .tar.gz)"
archive_version="${archive_name#litecr-}"
archive_version="${archive_version%-linux-x86_64}"
[[ "$archive_version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || {
  echo 'NEW_PACKAGE_VERSION_INVALID' >&2; exit 2;
}
[[ -f "$new_archive.sha256" && ! -L "$new_archive.sha256" ]] || { echo 'NEW_PACKAGE_CHECKSUM_MISSING' >&2; exit 2; }
(cd -- "$(dirname -- "$new_archive")" && sha256sum --check --strict "$(basename -- "$new_archive").sha256" >/dev/null) || {
  echo 'NEW_PACKAGE_ARCHIVE_CHECKSUM_FAILED' >&2; exit 2;
}
python3 - "$new_archive" "$archive_name" <<'PY'
import pathlib
import sys
import tarfile

archive = pathlib.Path(sys.argv[1])
expected_root = sys.argv[2]
with tarfile.open(archive, "r:gz") as bundle:
    members = bundle.getmembers()
assert members
for member in members:
    path = pathlib.PurePosixPath(member.name)
    assert not path.is_absolute() and ".." not in path.parts
    assert path.parts and path.parts[0] == expected_root
    assert member.isfile() or member.isdir()
PY
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"

verify_package() {
  local binary="$1" schema="$2" expected_archive="$3" expected_binary="$4"
  local package_root archive actual
  [[ -x "$binary" && ! -L "$binary" && "$(basename -- "$binary")" == litecr ]] || {
    echo "PACKAGE_BINARY_INVALID:$binary" >&2; exit 2;
  }
  package_root="$(dirname -- "$(dirname -- "$binary")")"
  archive="${package_root}.tar.gz"
  [[ -f "$archive" && ! -L "$archive" ]] || { echo "PACKAGE_ARCHIVE_MISSING:$schema" >&2; exit 2; }
  actual="$(sha256sum "$archive" | awk '{print $1}')"
  [[ "$actual" == "$expected_archive" ]] || { echo "PACKAGE_ARCHIVE_HASH_MISMATCH:$schema" >&2; exit 2; }
  (cd -- "$(dirname -- "$archive")" && sha256sum --check --strict "$(basename -- "$archive").sha256" >/dev/null) || {
    echo "PACKAGE_ARCHIVE_CHECKSUM_FILE_FAILED:$schema" >&2; exit 2;
  }
  actual="$(sha256sum "$binary" | awk '{print $1}')"
  [[ "$actual" == "$expected_binary" ]] || { echo "PACKAGE_BINARY_HASH_MISMATCH:$schema" >&2; exit 2; }
  (cd -- "$package_root" && sha256sum --check --strict SHA256SUMS >/dev/null) || {
    echo "PACKAGE_PAYLOAD_CHECKSUM_FAILED:$schema" >&2; exit 2;
  }
  "$binary" version --json | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d == {"application_version":"0.1.0","schema_version":int(sys.argv[1])}' "$schema" || {
    echo "PACKAGE_VERSION_MISMATCH:$schema" >&2; exit 2;
  }
  python3 - "$package_root/share/metadata/release.json" "$schema" <<'PY'
import json
import pathlib
import sys

release = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert release["application_version"] == "0.1.0"
assert release["schema_version"] == int(sys.argv[2])
assert release["platform"] == "linux-x86_64"
assert release["distribution_status"] == "review_required"
assert release["source_revision"] is None
assert release["source_revision_status"] == "no_git_metadata"
PY
}

verify_package "$old_binary" 23 "$expected_old_archive_sha256" "$expected_old_binary_sha256"

pg_version="$(pg_config --version)"
[[ "$pg_version" =~ ^PostgreSQL\ 18([.]|$) ]] || { echo "POSTGRESQL_18_REQUIRED:$pg_version" >&2; exit 2; }
pg_bin="$(pg_config --bindir)"
for tool in initdb pg_ctl pg_isready psql; do
  [[ -x "$pg_bin/$tool" ]] || { echo "POSTGRESQL_TOOL_MISSING:$tool" >&2; exit 2; }
done
export PATH="$pg_bin:$PATH"
unset PGPASSWORD PGPASSFILE PGSERVICE PGSERVICEFILE PGHOSTADDR PGOPTIONS PGDATABASE PGPORT PGHOST
bootstrap_role="$(id -un)"
[[ "$bootstrap_role" =~ ^[A-Za-z_][A-Za-z0-9_.-]{0,62}$ ]] || { echo 'UNSUPPORTED_BOOTSTRAP_ROLE' >&2; exit 2; }

root="$(mktemp -d /tmp/litecr-schema23-package-schema24-upgrade.XXXXXXXX)"
chmod 700 "$root"
root="$(realpath -e -- "$root")"
[[ "$root" == /tmp/litecr-schema23-package-schema24-upgrade.* && -d "$root" && ! -L "$root" ]] || {
  echo 'TEMP_PATH_UNSAFE' >&2; exit 3;
}
printf 'litecr-schema23-package-schema24-upgrade-v1\n' > "$root/OWNER"
chmod 600 "$root/OWNER"
data="$root/data"
socket="$root/socket"
started=0
cleanup() {
  status=$?
  if (( started )); then "$pg_bin/pg_ctl" -D "$data" -m immediate stop >/dev/null 2>&1 || true; fi
  if [[ "$root" == /tmp/litecr-schema23-package-schema24-upgrade.* && -f "$root/OWNER" && \
        "$(cat "$root/OWNER")" == litecr-schema23-package-schema24-upgrade-v1 ]]; then
    rm -rf -- "$root"
  else
    echo 'CLEANUP_REFUSED_UNOWNED_PATH' >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT

mkdir -m 700 "$root/package"
tar -xzf "$new_archive" -C "$root/package" --no-same-owner
new_package_root="$root/package/$archive_name"
new_binary="$new_package_root/bin/litecr"
[[ -d "$new_package_root" && ! -L "$new_package_root" && -x "$new_binary" && ! -L "$new_binary" ]] || {
  echo 'NEW_PACKAGE_LAYOUT_INVALID' >&2; exit 2;
}
(cd -- "$new_package_root" && sha256sum --check --strict SHA256SUMS >/dev/null) || {
  echo 'NEW_PACKAGE_PAYLOAD_CHECKSUM_FAILED' >&2; exit 2;
}
new_metadata="$new_package_root/share/metadata/release.json"
[[ -f "$new_metadata" && ! -L "$new_metadata" ]] || { echo 'NEW_PACKAGE_METADATA_MISSING' >&2; exit 2; }
new_identity="$(python3 - "$new_metadata" "$archive_version" <<'PY'
import json
import pathlib
import re
import sys

release = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
version = release["application_version"]
assert version == sys.argv[2]
assert release["schema_version"] == 24
assert release["platform"] == "linux-x86_64"
assert release["distribution_status"] == "review_required"
assert release["source_revision_status"] in {"clean_git_worktree", "no_git_metadata"}
assert release["source_revision"] is None or re.fullmatch(r"[0-9a-f]{40}", release["source_revision"])
print(version)
PY
)" || { echo 'NEW_PACKAGE_METADATA_INVALID' >&2; exit 2; }
"$new_binary" version --json | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d == {"application_version":sys.argv[1],"schema_version":24}' "$new_identity" || {
  echo 'NEW_PACKAGE_VERSION_MISMATCH' >&2; exit 2;
}
mkdir -m 700 "$socket"
port="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"

"$pg_bin/initdb" -D "$data" --username="$bootstrap_role" --auth-local=trust --auth-host=trust \
  --encoding=UTF8 --locale=C.UTF-8 --no-instructions >"$root/initdb.log"
"$pg_bin/pg_ctl" -D "$data" -l "$root/postgres.log" \
  -o "-h 127.0.0.1 -p $port -k $socket" start >/dev/null
started=1
for _ in $(seq 1 50); do
  if "$pg_bin/pg_isready" -h 127.0.0.1 -p "$port" -U "$bootstrap_role" >/dev/null 2>&1; then break; fi
  sleep 0.1
done
[[ "$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER="$bootstrap_role" PGDATABASE=postgres \
  "$pg_bin/psql" -X --no-psqlrc -qAt -c 'SELECT host(inet_server_addr())')" == 127.0.0.1 ]] || {
  echo 'CLUSTER_NOT_LOOPBACK' >&2; exit 3;
}
PGHOST=127.0.0.1 PGPORT="$port" PGUSER="$bootstrap_role" PGDATABASE=postgres \
  "$pg_bin/psql" -X --no-psqlrc -v ON_ERROR_STOP=1 <<'SQL'
CREATE ROLE litecr_migrator LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE DATABASE litecr_test OWNER litecr_migrator;
SQL

migration_url="postgresql://litecr_migrator@127.0.0.1:$port/litecr_test"
LITECR_MIGRATION_DATABASE_URL="$migration_url" "$old_binary" migrate
[[ "$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_migrator PGDATABASE=litecr_test \
  "$pg_bin/psql" -X --no-psqlrc -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == 23 ]] || {
  echo 'SCHEMA_23_SETUP_FAILED' >&2; exit 1;
}

# Migration 0024 must extend the permission constraint without losing the
# existing migration:write grant name.
LITECR_MIGRATION_DATABASE_URL="$migration_url" "$new_binary" migrate
for expected in 24 migration:read migration:write; do
  case "$expected" in
    24)
      actual="$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_migrator PGDATABASE=litecr_test \
        "$pg_bin/psql" -X --no-psqlrc -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')"
      [[ "$actual" == "$expected" ]] || { echo "SCHEMA_24_UPGRADE_FAILED:$actual" >&2; exit 1; }
      ;;
    *)
      PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_migrator PGDATABASE=litecr_test \
        "$pg_bin/psql" -X --no-psqlrc -qAt -v value="$expected" -c \
        "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='litecr.access_grant'::regclass AND conname='access_grant_permission_name_check'" \
        | grep -Fq "$expected" || { echo "PERMISSION_CONSTRAINT_MISSING:$expected" >&2; exit 1; }
      ;;
  esac
done
PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_migrator PGDATABASE=litecr_test \
  "$pg_bin/psql" -X --no-psqlrc -v ON_ERROR_STOP=1 <<'SQL'
INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject)
VALUES ('integration_client', '', 'schema24-read-operator');
INSERT INTO litecr.access_grant
  (actor_kind, issuer_url, external_subject, registry_id, organization_id, permission_name)
VALUES ('integration_client', '', 'schema24-read-operator', 'synthetic-registry', 'synthetic-org', 'migration:read');
INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject)
VALUES ('integration_client', '', 'schema24-write-operator');
INSERT INTO litecr.access_grant
  (actor_kind, issuer_url, external_subject, registry_id, organization_id, permission_name)
VALUES ('integration_client', '', 'schema24-write-operator', 'synthetic-registry', 'synthetic-org', 'migration:write');
SQL
if PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_migrator PGDATABASE=litecr_test \
  "$pg_bin/psql" -X --no-psqlrc -v ON_ERROR_STOP=1 -c \
  "INSERT INTO litecr.access_grant (actor_kind,issuer_url,external_subject,registry_id,organization_id,permission_name) VALUES ('integration_client','','schema24-read-operator','synthetic-registry','synthetic-org','migration:admin')" \
  >/dev/null 2>&1; then
  echo 'INVALID_PERMISSION_WAS_ACCEPTED' >&2; exit 1
fi
# A second run verifies schema-24 migration idempotency.
LITECR_MIGRATION_DATABASE_URL="$migration_url" "$new_binary" migrate
[[ "$(PGHOST=127.0.0.1 PGPORT="$port" PGUSER=litecr_migrator PGDATABASE=litecr_test \
  "$pg_bin/psql" -X --no-psqlrc -qAt -c 'SELECT version FROM litecr.schema_meta WHERE singleton')" == 24 ]] || {
  echo 'SCHEMA_24_IDEMPOTENCY_FAILED' >&2; exit 1;
}
echo 'SCHEMA23_TO_SCHEMA24_EXACT_PACKAGE_UPGRADE=passed'
