#!/usr/bin/env bash
set -euo pipefail
umask 077

fail() {
  printf 'liteCR restore failed [%s]\n' "$1" >&2
  exit 1
}

[[ $# == 6 && "$1" == "--dump" && "$3" == "--target-db" && "$5" == "--isolation-marker" ]] || fail USAGE
dump_path="$2"
target_db="$4"
marker_path="$6"

# Restore runs pg_restore against a highly privileged, newly created database.
# Reject inherited libpq settings so ambient session behavior, credentials,
# service files, TLS policy, or an alternate endpoint cannot weaken the
# explicit restore configuration. PGPASSFILE is the only permitted override;
# it is validated as a protected file below.
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

[[ "$dump_path" == /* && -f "$dump_path" && ! -L "$dump_path" ]] || fail DUMP_PATH
[[ "$target_db" =~ ^litecr_restore_[a-z0-9_]{1,48}$ ]] || fail TARGET_NAME
[[ "$marker_path" == /* && -f "$marker_path" && ! -L "$marker_path" ]] || fail ISOLATION_MARKER
marker_owner="$(stat -c '%u' -- "$marker_path")"
marker_mode="$(stat -c '%a' -- "$marker_path")"
(( marker_owner == 0 || marker_owner == EUID )) || fail ISOLATION_MARKER_OWNER
(( (8#$marker_mode & 077) == 0 )) || fail ISOLATION_MARKER_MODE
declare -A marker_values=()
while IFS= read -r line || [[ -n "$line" ]]; do
  [[ "$line" == *=* ]] || fail ISOLATION_MARKER_CONTENT
  key="${line%%=*}"; value="${line#*=}"
  [[ -n "$value" && -z "${marker_values[$key]+present}" ]] || fail ISOLATION_MARKER_CONTENT
  case "$key" in format|system_identifier) ;;
    *) fail ISOLATION_MARKER_CONTENT ;;
  esac
  marker_values[$key]="$value"
done < "$marker_path"
[[ "${#marker_values[@]}" == 2 && "${marker_values[format]}" == litecr-isolated-postgres-target-v2 \
  && "${marker_values[system_identifier]}" =~ ^[0-9]{1,20}$ ]] || fail ISOLATION_MARKER_CONTENT

: "${LITECR_CONFIG_FILE:?Set LITECR_CONFIG_FILE to a protected key=value config file}"
[[ -f "$LITECR_CONFIG_FILE" && ! -L "$LITECR_CONFIG_FILE" ]] || fail CONFIG_FILE_NOT_REGULAR
config_owner="$(stat -c '%u' -- "$LITECR_CONFIG_FILE")"
config_mode="$(stat -c '%a' -- "$LITECR_CONFIG_FILE")"
(( config_owner == 0 || config_owner == EUID )) || fail CONFIG_FILE_OWNER
(( (8#$config_mode & 077) == 0 )) || fail CONFIG_FILE_MODE

LITECR_PG_USER=""
LITECR_PGHOST=""
LITECR_PGPORT=""
LITECR_RESTORE_DB_OWNER=""
declare -A seen_config_keys=()
while IFS= read -r line || [[ -n "$line" ]]; do
  [[ -z "$line" || "$line" == \#* ]] && continue
  [[ "$line" == *=* ]] || fail CONFIG_SYNTAX
  key="${line%%=*}"
  value="${line#*=}"
  [[ -n "$value" && "$value" != *$'\r'* && "$value" != *$'\n'* ]] || fail CONFIG_VALUE
  case "$key" in
    LITECR_PG_USER|LITECR_PGHOST|LITECR_PGPORT|LITECR_RESTORE_DB_OWNER) ;;
    *) fail CONFIG_KEY_UNKNOWN ;;
  esac
  [[ -z "${seen_config_keys[$key]+present}" ]] || fail CONFIG_KEY_DUPLICATE
  seen_config_keys[$key]=1
  printf -v "$key" '%s' "$value"
done < "$LITECR_CONFIG_FILE"
[[ "$LITECR_PG_USER" =~ ^[A-Za-z0-9_.-]{1,63}$ ]] || fail DATABASE_USER
if [[ -n "$LITECR_PGHOST" ]]; then export PGHOST="$LITECR_PGHOST"; fi
if [[ -n "${PGHOST:-}" ]]; then
  case "$PGHOST" in
    localhost|127.0.0.1|::1|/var/run/postgresql|/run/postgresql) ;;
    *) fail RESTORE_HOST_MUST_BE_LOCAL ;;
  esac
fi
if [[ -n "$LITECR_PGPORT" ]]; then
  [[ "$LITECR_PGPORT" =~ ^[0-9]{1,5}$ ]] && (( 10#$LITECR_PGPORT > 0 && 10#$LITECR_PGPORT <= 65535 )) || fail DATABASE_PORT
  export PGPORT="$LITECR_PGPORT"
fi
export PGUSER="$LITECR_PG_USER"
LITECR_RESTORE_DB_OWNER="${LITECR_RESTORE_DB_OWNER:-$LITECR_PG_USER}"
[[ "$LITECR_RESTORE_DB_OWNER" =~ ^[A-Za-z0-9_.-]{1,63}$ ]] || fail DATABASE_OWNER

if [[ -n "${PGPASSFILE:-}" ]]; then
  [[ -f "$PGPASSFILE" && ! -L "$PGPASSFILE" ]] || fail SECRET_FILE_NOT_REGULAR
  pass_owner="$(stat -c '%u' -- "$PGPASSFILE")"
  pass_mode="$(stat -c '%a' -- "$PGPASSFILE")"
  (( pass_owner == 0 || pass_owner == EUID )) || fail SECRET_FILE_OWNER
  (( (8#$pass_mode & 077) == 0 )) || fail SECRET_FILE_MODE
  export PGPASSFILE
fi

dump_dir="$(dirname -- "$dump_path")"
dump_base="$(basename -- "$dump_path")"
stem="${dump_base%.dump}"
manifest="$dump_dir/$stem.manifest"
checksum="$dump_dir/$stem.sha256"
success="$dump_dir/$stem.success"
[[ "$dump_base" == "$stem.dump" && -f "$manifest" && ! -L "$manifest" && -f "$checksum" && ! -L "$checksum" && -f "$success" && ! -L "$success" ]] || fail SIDECAR_MISSING
for input_file in "$dump_path" "$manifest" "$checksum" "$success"; do
  input_owner="$(stat -c '%u' -- "$input_file")"
  input_mode="$(stat -c '%a' -- "$input_file")"
  (( input_owner == 0 || input_owner == EUID )) || fail BACKUP_INPUT_OWNER
  (( (8#$input_mode & 077) == 0 )) || fail BACKUP_INPUT_MODE
done
grep -Fxq 'format=litecr-backup-v1' "$manifest" || fail MANIFEST_FORMAT
manifest_value() { awk -F= -v key="$1" '$1 == key {sub(/^[^=]*=/, ""); print; count++} END {if (count != 1) exit 1}' "$manifest"; }
expected_file="$(manifest_value dump_file)" || fail MANIFEST_DUMP_FILE
expected_hash="$(manifest_value sha256)" || fail MANIFEST_CHECKSUM
expected_bytes="$(manifest_value dump_bytes)" || fail MANIFEST_LENGTH
expected_version="$(manifest_value schema_version)" || fail MANIFEST_SCHEMA_VERSION
source_pg_version="$(manifest_value postgres_server_version)" || fail MANIFEST_POSTGRES_VERSION
[[ "$expected_file" == "$dump_base" && "$expected_hash" =~ ^[0-9a-f]{64}$ && "$expected_bytes" =~ ^[0-9]+$ && "$expected_version" =~ ^[0-9]+$ ]] || fail MANIFEST_INVALID

declare -A success_values=()
while IFS= read -r line || [[ -n "$line" ]]; do
  [[ "$line" == *=* ]] || fail SUCCESS_MARKER_SYNTAX
  key="${line%%=*}"
  value="${line#*=}"
  [[ -n "$value" && "$value" != *"="* ]] || fail SUCCESS_MARKER_SYNTAX
  case "$key" in
    format|backup_id|dump_file|sha256|completed_utc) ;;
    *) fail SUCCESS_MARKER_KEY_UNKNOWN ;;
  esac
  [[ -z "${success_values[$key]+present}" ]] || fail SUCCESS_MARKER_KEY_DUPLICATE
  success_values[$key]="$value"
done < "$success"
[[ ${#success_values[@]} == 5 ]] || fail SUCCESS_MARKER_FIELD_MISSING
[[ "${success_values[format]}" == litecr-backup-success-v1 ]] || fail SUCCESS_MARKER_FORMAT
[[ "${success_values[backup_id]}" == "$stem" ]] || fail SUCCESS_MARKER_BACKUP_ID
[[ "${success_values[dump_file]}" == "$dump_base" ]] || fail SUCCESS_MARKER_DUMP_FILE
[[ "${success_values[sha256]}" =~ ^[0-9a-f]{64}$ && "${success_values[sha256]}" == "$expected_hash" ]] || fail SUCCESS_MARKER_CHECKSUM
[[ "${success_values[completed_utc]}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] || fail SUCCESS_MARKER_TIMESTAMP
[[ "$(stat -c '%s' -- "$dump_path")" == "$expected_bytes" ]] || fail DUMP_SIZE_MISMATCH
( cd "$dump_dir" && sha256sum --check --status -- "$checksum" ) || fail DUMP_CHECKSUM_MISMATCH
manifest_hash="$(awk -F= '$1 == "sha256" {print $2; count++} END {if (count != 1) exit 1}' "$manifest")" || fail MANIFEST_CHECKSUM
[[ "$manifest_hash" == "$expected_hash" ]] || fail MANIFEST_CHECKSUM_MISMATCH

target_version="$(psql -X -A -t --no-password --dbname=postgres -c 'SHOW server_version' 2>/dev/null | tr -d '\r')" || fail TARGET_VERSION_QUERY
source_major="${source_pg_version%%.*}"
target_major="${target_version%%.*}"
[[ "$source_major" =~ ^[0-9]+$ && "$target_major" == "$source_major" ]] || fail POSTGRES_MAJOR_MISMATCH
target_system_identifier="$(psql -X -A -t --no-password --dbname=postgres \
  -c 'SELECT system_identifier FROM pg_catalog.pg_control_system()' 2>/dev/null | tr -d '[:space:]')" || fail TARGET_SYSTEM_IDENTIFIER_QUERY
[[ "$target_system_identifier" =~ ^[0-9]{1,20}$ ]] || fail TARGET_SYSTEM_IDENTIFIER_INVALID
[[ "$target_system_identifier" == "${marker_values[system_identifier]}" ]] || fail TARGET_CLUSTER_IDENTITY_MISMATCH
exists="$(psql -X -A -t --no-password --dbname=postgres -c "SELECT 1 FROM pg_database WHERE datname = '$target_db'" 2>/dev/null | tr -d '[:space:]')" || fail TARGET_DATABASE_QUERY
[[ -z "$exists" ]] || fail TARGET_DATABASE_ALREADY_EXISTS
pg_restore --list "$dump_path" >/dev/null 2>&1 || fail ARCHIVE_INVALID

createdb --no-password --maintenance-db=postgres --owner="$LITECR_RESTORE_DB_OWNER" "$target_db" >/dev/null 2>&1 || fail TARGET_DATABASE_CREATE
# Restore only into the newly created, explicitly marked local database. Never drop it on failure.
pg_restore --no-password --exit-on-error --single-transaction --no-owner --no-acl \
  --dbname="$target_db" "$dump_path" >/dev/null 2>&1 || fail PG_RESTORE_FAILED
restored_version="$(psql -X -A -t --no-password --dbname="$target_db" \
  -c 'SELECT version FROM litecr.schema_meta WHERE singleton = TRUE' 2>/dev/null | tr -d '[:space:]')" || fail RESTORED_SCHEMA_QUERY
[[ "$restored_version" == "$expected_version" ]] || fail RESTORED_SCHEMA_VERSION_MISMATCH
printf 'liteCR isolated restore completed: database=%s schema_version=%s postgres_major=%s\n' \
  "$target_db" "$restored_version" "$target_major"
