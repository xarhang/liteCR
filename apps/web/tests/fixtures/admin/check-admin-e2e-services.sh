#!/usr/bin/env bash
set -euo pipefail
set +x

if [[ $# -ne 1 || ! $1 =~ ^[a-f0-9]{16}$ ]]; then
  echo "usage: $0 <16-hex-run-id>" >&2
  exit 2
fi
run_id=$1
run_dir="$HOME/.litecr-dev/admin-e2e/$run_id"
if [[ -L $run_dir || ! -d $run_dir || ! -f $run_dir/OWNER || $(cat "$run_dir/OWNER") != "liteCR-admin-e2e-v1 $run_id" ]]; then
  echo 'fixture directory ownership marker does not match' >&2
  exit 2
fi

check_process() {
  local role=$1 pid_file=$2 expected_exe=$3 expected_cwd=$4 pid executable working_directory process_environment
  [[ -f $pid_file ]] || { echo "missing $role fixture PID file" >&2; return 1; }
  pid=$(cat "$pid_file")
  [[ $pid =~ ^[0-9]+$ && -d /proc/$pid ]] || { echo "$role fixture process is not running" >&2; return 1; }
  executable=$(readlink "/proc/$pid/exe" 2>/dev/null || true)
  working_directory=$(readlink "/proc/$pid/cwd" 2>/dev/null || true)
  process_environment=$(tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null || true)
  if [[ $executable != *"/$expected_exe" || $working_directory != "$expected_cwd" || $process_environment != *"LITECR_E2E_ADMIN_RUN_ID=$run_id"* ]]; then
    echo "$role PID does not match the exact private fixture process" >&2
    return 1
  fi
}

check_process keycloak "$run_dir/keycloak.pid" java "$run_dir/keycloak-install/keycloak-26.8.0"
check_process api "$run_dir/api.pid" litecr /mnt/d/liteCR
for port in 18080 18081 19001; do
  lines=$(ss -H -ltn "sport = :$port")
  [[ -n $lines ]] || { echo "expected private service port $port is not listening" >&2; exit 1; }
  while IFS= read -r line; do
    address=$(awk '{print $4}' <<< "$line")
    if [[ "$address" != "127.0.0.1:$port" ]]; then
      echo "fixture service listener on $port is not IPv4 loopback" >&2
      exit 1
    fi
  done <<< "$lines"
done
curl --fail --silent --max-time 2 http://127.0.0.1:18080/health/ready >/dev/null
curl --fail --silent --max-time 2 http://127.0.0.1:18081/realms/litecr/.well-known/openid-configuration >/dev/null
index_html=$(curl --fail --silent --max-time 2 http://127.0.0.1:18080/)
grep -q 'id="root"' <<< "$index_html" || { echo 'API embedded web index is missing its application root' >&2; exit 1; }
asset_path=$(grep -Eo 'src="/assets/[^" ]+\.js"' <<< "$index_html" | head -n 1 | cut -d'"' -f2)
[[ $asset_path == /assets/*.js ]] || { echo 'API embedded web index does not reference a bundled JavaScript asset' >&2; exit 1; }
curl --fail --silent --max-time 2 "http://127.0.0.1:18080$asset_path" >/dev/null
printf 'verified run-owned loopback API and Keycloak for %s\n' "$run_id"
