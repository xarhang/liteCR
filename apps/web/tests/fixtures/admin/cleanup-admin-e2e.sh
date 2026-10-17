#!/usr/bin/env bash
set -euo pipefail
set +x

if [[ $# -ne 1 || ! $1 =~ ^[a-f0-9]{16}$ ]]; then
  echo "usage: $0 <16-hex-run-id>" >&2
  exit 2
fi
run_id=$1
home=${HOME:?}
state_root="$home/.litecr-dev/admin-e2e"
run_dir="$state_root/$run_id"
if [[ -L $state_root || -L $run_dir || ! -d $run_dir || ! -f $run_dir/OWNER || $(cat "$run_dir/OWNER") != "liteCR-admin-e2e-v1 $run_id" ]]; then
  echo "refusing to stop or remove a directory not owned by this admin E2E run" >&2
  exit 2
fi
expected="$(realpath -e "$run_dir")"
[[ "$expected" == "$state_root/$run_id" ]] || { echo 'fixture path escaped its private root' >&2; exit 2; }

stop_owned_process() {
  local role=$1 pid_file=$2 expected_exe=$3 expected_cwd=$4
  [[ -f $pid_file ]] || return 0
  local pid
  pid=$(cat "$pid_file")
  [[ $pid =~ ^[0-9]+$ ]] || { echo "invalid $role PID file; preserving run directory" >&2; return 1; }
  if [[ ! -d /proc/$pid ]]; then return 0; fi

  local executable working_directory process_environment
  executable=$(readlink "/proc/$pid/exe" 2>/dev/null || true)
  working_directory=$(readlink "/proc/$pid/cwd" 2>/dev/null || true)
  process_environment=$(tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null || true)
  if [[ $executable != *"/$expected_exe" || $working_directory != "$expected_cwd" || $process_environment != *"LITECR_E2E_ADMIN_RUN_ID=$run_id"* ]]; then
    echo "$role PID does not match this run's executable, working directory and private path; preserving run directory" >&2
    return 1
  fi
  kill -TERM "$pid"
  for _ in $(seq 1 30); do
    [[ ! -d /proc/$pid ]] && return 0
    sleep 1
  done
  echo "$role process did not stop; preserving run directory" >&2
  return 1
}

stop_owned_process keycloak "$run_dir/keycloak.pid" java "$run_dir/keycloak-install/keycloak-26.8.0"
stop_owned_process api "$run_dir/api.pid" litecr /mnt/d/liteCR

pg_data="$run_dir/postgres"
pg_ctl="$(pg_config --bindir)/pg_ctl"
if [[ -d $pg_data ]] && "$pg_ctl" -D "$pg_data" status >/dev/null 2>&1; then
  "$pg_ctl" -D "$pg_data" -m fast -w stop >/dev/null
fi
if [[ -e $pg_data/postmaster.pid ]]; then
  echo 'PostgreSQL postmaster PID remains after stop; preserving run directory' >&2
  exit 1
fi

rm -rf -- "$expected"
printf 'stopped private services and removed admin E2E run %s\n' "$run_id"
