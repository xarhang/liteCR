#!/usr/bin/env bash
set -euo pipefail
umask 077

fail() {
  printf 'liteCR backup failed [%s]\n' "$1" >&2
  exit 1
}

[[ $# -le 1 ]] || fail USAGE
mode="${1:---archive-only}"
[[ "$mode" == "--archive-only" || "$mode" == "--transfer" ]] || fail USAGE

# libpq accepts connection and session settings from the environment. Do not let
# an inherited variable override the explicit backup config or silently select
# another database, service file, TLS policy, or session behavior. PGPASSFILE is
# the sole exception: it is the documented credential source and is checked
# for ownership and permissions below.
libpq_environment=(
  PGHOST PGHOSTADDR PGPORT PGDATABASE PGUSER PGPASSWORD PGSERVICE PGSERVICEFILE
  PGSSLNEGOTIATION PGREQUIREAUTH PGSSLSNI PGREQUIREPEER PGGSSLIB
  PGOPTIONS PGCONNECT_TIMEOUT PGSSLMODE PGREQUIRESSL PGSSLCERT PGSSLKEY
  PGSSLROOTCERT PGSSLCRL PGSSLCRLDIR PGSSLCOMPRESSION PGSSLCERTMODE
  PGSSLMINPROTOCOLVERSION PGSSLMAXPROTOCOLVERSION PGSSLKEYLOGFILE
  PGCHANNELBINDING PGGSSENCMODE PGTARGETSESSIONATTRS
  PGLOADBALANCEHOSTS PGCLIENTENCODING PGREALM PGGSSDELEGATION PGKRBSRVNAME
  PGDATESTYLE PGTZ PGGEQO PGSYSCONFDIR PGLOCALEDIR
  PGAPPNAME
)
for variable in "${libpq_environment[@]}"; do
  if [[ -n "${!variable:-}" ]]; then fail LIBPQ_ENVIRONMENT_OVERRIDE; fi
done

: "${LITECR_CONFIG_FILE:?Set LITECR_CONFIG_FILE to a protected, root-owned key=value config file}"
[[ -f "$LITECR_CONFIG_FILE" && ! -L "$LITECR_CONFIG_FILE" ]] || fail CONFIG_FILE_NOT_REGULAR
config_owner="$(stat -c '%u' -- "$LITECR_CONFIG_FILE")"
config_mode="$(stat -c '%a' -- "$LITECR_CONFIG_FILE")"
(( config_owner == 0 || config_owner == EUID )) || fail CONFIG_FILE_OWNER
(( (8#$config_mode & 077) == 0 )) || fail CONFIG_FILE_MODE

LITECR_PG_DATABASE=""
LITECR_PG_USER=""
LITECR_BACKUP_DIR=""
LITECR_RELEASE_ID=""
LITECR_PGHOST=""
LITECR_PGPORT=""
LITECR_MIN_FREE_KB="1048576"
declare -A seen_config_keys=()

while IFS= read -r line || [[ -n "$line" ]]; do
  [[ -z "$line" || "$line" == \#* ]] && continue
  [[ "$line" == *=* ]] || fail CONFIG_SYNTAX
  key="${line%%=*}"
  value="${line#*=}"
  [[ -n "$value" && "$value" != *$'\r'* && "$value" != *$'\n'* ]] || fail CONFIG_VALUE
  case "$key" in
    LITECR_PG_DATABASE|LITECR_PG_USER|LITECR_BACKUP_DIR|LITECR_RELEASE_ID|LITECR_PGHOST|LITECR_PGPORT|LITECR_MIN_FREE_KB) ;;
    *) fail CONFIG_KEY_UNKNOWN ;;
  esac
  [[ -z "${seen_config_keys[$key]+present}" ]] || fail CONFIG_KEY_DUPLICATE
  seen_config_keys[$key]=1
  printf -v "$key" '%s' "$value"
done < "$LITECR_CONFIG_FILE"

[[ "$LITECR_PG_DATABASE" =~ ^[A-Za-z0-9_.-]{1,63}$ ]] || fail DATABASE_NAME
[[ "$LITECR_PG_USER" =~ ^[A-Za-z0-9_.-]{1,63}$ ]] || fail DATABASE_USER
[[ "$LITECR_RELEASE_ID" =~ ^[A-Za-z0-9_.+-]{1,128}$ ]] || fail RELEASE_ID
[[ "$LITECR_BACKUP_DIR" == /* && "$LITECR_BACKUP_DIR" != *$'\n'* ]] || fail BACKUP_PATH
[[ "$LITECR_MIN_FREE_KB" =~ ^[0-9]{1,12}$ ]] || fail FREE_SPACE_CONFIG
if [[ -n "$LITECR_PGHOST" ]]; then export PGHOST="$LITECR_PGHOST"; fi
if [[ -n "$LITECR_PGPORT" ]]; then
  [[ "$LITECR_PGPORT" =~ ^[0-9]{1,5}$ ]] && (( 10#$LITECR_PGPORT > 0 && 10#$LITECR_PGPORT <= 65535 )) || fail DATABASE_PORT
  export PGPORT="$LITECR_PGPORT"
fi
export PGDATABASE="$LITECR_PG_DATABASE" PGUSER="$LITECR_PG_USER"

check_secret_file() {
  local path="$1" owner mode
  [[ -f "$path" && ! -L "$path" ]] || fail SECRET_FILE_NOT_REGULAR
  owner="$(stat -c '%u' -- "$path")"
  mode="$(stat -c '%a' -- "$path")"
  (( owner == 0 || owner == EUID )) || fail SECRET_FILE_OWNER
  (( (8#$mode & 077) == 0 )) || fail SECRET_FILE_MODE
}
if [[ -n "${PGPASSFILE:-}" ]]; then
  check_secret_file "$PGPASSFILE"
  export PGPASSFILE
fi

mkdir -p -m 700 -- "$LITECR_BACKUP_DIR"
[[ -d "$LITECR_BACKUP_DIR" && ! -L "$LITECR_BACKUP_DIR" ]] || fail BACKUP_DIR_NOT_REGULAR
dir_owner="$(stat -c '%u' -- "$LITECR_BACKUP_DIR")"
dir_mode="$(stat -c '%a' -- "$LITECR_BACKUP_DIR")"
(( dir_owner == 0 || dir_owner == EUID )) || fail BACKUP_DIR_OWNER
(( (8#$dir_mode & 077) == 0 )) || fail BACKUP_DIR_MODE
exec 9>"$LITECR_BACKUP_DIR/.backup.lock"
flock -n 9 || fail BACKUP_ALREADY_RUNNING
available_kb="$(df -Pk -- "$LITECR_BACKUP_DIR" | awk 'NR==2 {print $4}')"
[[ "$available_kb" =~ ^[0-9]+$ ]] || fail FREE_SPACE_UNKNOWN
(( available_kb >= LITECR_MIN_FREE_KB )) || fail INSUFFICIENT_FREE_SPACE

created_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
final_base="litecr-${LITECR_PG_DATABASE}-${stamp}-$$"
dump_path="$LITECR_BACKUP_DIR/${final_base}.dump"
manifest_path="$LITECR_BACKUP_DIR/${final_base}.manifest"
checksum_path="$LITECR_BACKUP_DIR/${final_base}.sha256"
success_path="$LITECR_BACKUP_DIR/${final_base}.success"
[[ ! -e "$dump_path" && ! -e "$manifest_path" && ! -e "$checksum_path" && ! -e "$success_path" ]] || fail ARTIFACT_COLLISION
tmp_dump=""
tmp_manifest=""
tmp_checksum=""
tmp_success=""
temporary_files=()
published=0
published_files=()
cleanup() {
  if (( ${#temporary_files[@]} > 0 )); then
    rm -f -- "${temporary_files[@]}"
  fi
  # Remove only destinations that this process successfully moved into place.
  # This avoids deleting a file that appeared at a not-yet-published path after
  # the initial collision check (or any unrelated file in the backup folder).
  if (( published == 0 && ${#published_files[@]} > 0 )); then
    rm -f -- "${published_files[@]}"
  fi
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

tmp_dump="$(mktemp --tmpdir="$LITECR_BACKUP_DIR" '.litecr-dump.XXXXXXXX')"
temporary_files+=("$tmp_dump")
tmp_manifest="$(mktemp --tmpdir="$LITECR_BACKUP_DIR" '.litecr-manifest.XXXXXXXX')"
temporary_files+=("$tmp_manifest")
tmp_checksum="$(mktemp --tmpdir="$LITECR_BACKUP_DIR" '.litecr-checksum.XXXXXXXX')"
temporary_files+=("$tmp_checksum")
tmp_success="$(mktemp --tmpdir="$LITECR_BACKUP_DIR" '.litecr-success.XXXXXXXX')"
temporary_files+=("$tmp_success")

# pg_dump uses one MVCC snapshot by default. Check schema/server identity on
# both sides of that snapshot so the manifest cannot describe a different
# schema revision than the archive. Migrations are monotonic; any change fails
# closed and the EXIT trap removes incomplete artifacts.
schema_version_before="$(psql -X -A -t --no-password --dbname="$LITECR_PG_DATABASE" \
  -c 'SELECT version FROM litecr.schema_meta WHERE singleton = TRUE' 2>/dev/null | tr -d '[:space:]')" || fail SCHEMA_VERSION_QUERY
[[ "$schema_version_before" =~ ^[0-9]+$ ]] || fail SCHEMA_VERSION_MISSING
server_version_before="$(psql -X -A -t --no-password --dbname="$LITECR_PG_DATABASE" \
  -c 'SHOW server_version' 2>/dev/null | tr -d '\r')" || fail SERVER_VERSION_QUERY
[[ "$server_version_before" =~ ^[0-9]+[.][0-9]+([[:space:]]+\([A-Za-z0-9 ._+()-]+\))?$ ]] || fail SERVER_VERSION_INVALID

pg_dump --no-password --format=custom --no-owner --no-acl \
  --dbname="$LITECR_PG_DATABASE" --file="$tmp_dump" >/dev/null 2>&1 || fail PG_DUMP_FAILED
[[ -s "$tmp_dump" ]] || fail EMPTY_DUMP
# Parse the archive table of contents before publishing it. This catches a
# truncated or malformed custom archive even when pg_dump returned success;
# restore still performs its own validation before creating a target database.
pg_restore --list "$tmp_dump" >/dev/null 2>&1 || fail ARCHIVE_INVALID
schema_version_after="$(psql -X -A -t --no-password --dbname="$LITECR_PG_DATABASE" \
  -c 'SELECT version FROM litecr.schema_meta WHERE singleton = TRUE' 2>/dev/null | tr -d '[:space:]')" || fail SCHEMA_VERSION_QUERY
[[ "$schema_version_after" =~ ^[0-9]+$ ]] || fail SCHEMA_VERSION_MISSING
[[ "$schema_version_before" == "$schema_version_after" ]] || fail SCHEMA_CHANGED_DURING_BACKUP
server_version_after="$(psql -X -A -t --no-password --dbname="$LITECR_PG_DATABASE" \
  -c 'SHOW server_version' 2>/dev/null | tr -d '\r')" || fail SERVER_VERSION_QUERY
[[ "$server_version_after" =~ ^[0-9]+[.][0-9]+([[:space:]]+\([A-Za-z0-9 ._+()-]+\))?$ ]] || fail SERVER_VERSION_INVALID
[[ "$server_version_before" == "$server_version_after" ]] || fail SERVER_CHANGED_DURING_BACKUP
schema_version="$schema_version_after"
server_version="$server_version_after"
pg_dump_version="$(pg_dump --version 2>/dev/null | sed -E 's/[^A-Za-z0-9 ._-]/_/g' | head -c 120)" || fail PG_DUMP_VERSION
dump_bytes="$(stat -c '%s' -- "$tmp_dump")"
dump_hash="$(sha256sum -- "$tmp_dump" | awk '{print $1}')"
[[ "$dump_hash" =~ ^[0-9a-f]{64}$ && "$dump_bytes" =~ ^[0-9]+$ ]] || fail CHECKSUM_FAILED

{
  printf 'format=litecr-backup-v1\n'
  printf 'created_utc=%s\n' "$created_utc"
  printf 'database=%s\n' "$LITECR_PG_DATABASE"
  printf 'release_id=%s\n' "$LITECR_RELEASE_ID"
  printf 'schema_version=%s\n' "$schema_version"
  printf 'postgres_server_version=%s\n' "$server_version"
  printf 'pg_dump_version=%s\n' "$pg_dump_version"
  printf 'dump_file=%s.dump\n' "$final_base"
  printf 'dump_bytes=%s\n' "$dump_bytes"
  printf 'sha256=%s\n' "$dump_hash"
} > "$tmp_manifest"
printf '%s  %s.dump\n' "$dump_hash" "$final_base" > "$tmp_checksum"
completed_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
{
  printf 'format=litecr-backup-success-v1\n'
  printf 'backup_id=%s\n' "$final_base"
  printf 'dump_file=%s.dump\n' "$final_base"
  printf 'sha256=%s\n' "$dump_hash"
  printf 'completed_utc=%s\n' "$completed_utc"
} > "$tmp_success"
chmod 600 -- "$tmp_dump" "$tmp_manifest" "$tmp_checksum" "$tmp_success"
mv -- "$tmp_dump" "$dump_path"
published_files+=("$dump_path")
mv -- "$tmp_manifest" "$manifest_path"
published_files+=("$manifest_path")
mv -- "$tmp_checksum" "$checksum_path"
published_files+=("$checksum_path")
mv -- "$tmp_success" "$success_path"
published_files+=("$success_path")
published=1
printf 'liteCR backup success marker: %s\n' "$(basename -- "$success_path")"
printf 'liteCR backup created: %s (schema %s; %s bytes)\n' "$(basename -- "$dump_path")" "$schema_version" "$dump_bytes"

if [[ "$mode" == "--transfer" ]]; then
  : "${RESTIC_REPOSITORY:?Set RESTIC_REPOSITORY only after the organization approves an encrypted destination}"
  : "${RESTIC_PASSWORD_FILE:?Set RESTIC_PASSWORD_FILE to a separately protected recovery-key file}"
  check_secret_file "$RESTIC_PASSWORD_FILE"
  restic backup --repo "$RESTIC_REPOSITORY" --password-file "$RESTIC_PASSWORD_FILE" \
    --tag litecr --tag "schema-${schema_version}" \
    "$dump_path" "$manifest_path" "$checksum_path" "$success_path" >/dev/null 2>&1 || fail RESTIC_BACKUP_FAILED
  printf 'liteCR encrypted transfer completed; repository details suppressed\n'
fi
