#!/usr/bin/env bash

# Remove a rehearsal cluster only when its owned directory is valid and
# pg_ctl confirms that PostgreSQL is stopped. On uncertainty, preserve it.
litecr_cleanup_owned_postgres_cluster() {
  local root="$1" data="$2" pg_ctl="$3" schema_version="${4:-16}"
  local pid_file="$data/postmaster.pid" stop_rc=0 status_rc=0 reason=""

  if [[ ! "$schema_version" =~ ^(16|17|18|19|20|22|23|24|25|26|27)$ \
    || "$root" != "/tmp/litecr-p6-recovery-schema${schema_version}."* || ! -d "$root" || -L "$root" \
    || "$(realpath -e -- "$root" 2>/dev/null)" != "$root" \
    || ! -f "$root/OWNER" || -L "$root/OWNER" \
    || "$(cat -- "$root/OWNER" 2>/dev/null)" != "litecr-p6-recovery-schema${schema_version}-v1" \
    || "$data" != "$root/postgres-data" || ! -x "$pg_ctl" ]]; then
    reason=OWNERSHIP_OR_PATH_CHECK_FAILED
  elif [[ -e "$pid_file" || -L "$pid_file" ]]; then
    if [[ ! -f "$pid_file" || -L "$pid_file" || "$(sed -n '2p' "$pid_file" 2>/dev/null)" != "$data" ]]; then
      reason=POSTMASTER_PID_OWNERSHIP_CHECK_FAILED
    else
      if LC_ALL=C "$pg_ctl" -D "$data" -m immediate stop >"$root/cleanup-stop.log" 2>&1; then
        stop_rc=0
      else
        stop_rc=$?
      fi
    fi
  fi

  if [[ -z "$reason" ]]; then
    if LC_ALL=C "$pg_ctl" -D "$data" status >"$root/cleanup-status.log" 2>&1; then
      status_rc=0
    else
      status_rc=$?
    fi
    if [[ "$status_rc" != 3 || -e "$pid_file" || -L "$pid_file" ]]; then
      if [[ "$stop_rc" != 0 ]]; then
        reason="PG_CTL_STOP_FAILED_RC_${stop_rc}_AND_CLUSTER_NOT_CONFIRMED_STOPPED"
      else
        reason="CLUSTER_NOT_CONFIRMED_STOPPED_STATUS_RC_${status_rc}"
      fi
    fi
  fi

  if [[ -n "$reason" ]]; then
    printf 'RECOVERY_CLEANUP_PRESERVED_PATH: reason=%s root=%s data=%s\n' \
      "$reason" "$root" "$data" >&2
    if [[ -f "$root/cleanup-stop.log" ]]; then
      sed 's/^/pg_ctl stop: /' "$root/cleanup-stop.log" >&2
    fi
    if [[ -f "$root/cleanup-status.log" ]]; then
      sed 's/^/pg_ctl status: /' "$root/cleanup-status.log" >&2
    fi
    return 1
  fi

  if ! rm -rf -- "$root"; then
    printf 'RECOVERY_CLEANUP_REMOVE_FAILED: root=%s data=%s; cluster is confirmed stopped\n' \
      "$root" "$data" >&2
    return 1
  fi
}
