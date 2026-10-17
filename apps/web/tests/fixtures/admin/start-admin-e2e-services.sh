#!/usr/bin/env bash
set -euo pipefail
set +x

if [[ $# -ne 1 || ! $1 =~ ^[a-f0-9]{16}$ ]]; then
  echo "usage: $0 <16-hex-run-id>" >&2
  exit 2
fi
run_id=$1
home=${HOME:?}
run_dir="$home/.litecr-dev/admin-e2e/$run_id"
if [[ -L $run_dir || ! -d $run_dir || ! -f $run_dir/OWNER || $(cat "$run_dir/OWNER") != "liteCR-admin-e2e-v1 $run_id" ]]; then
  echo "missing exact run-owned admin fixture directory" >&2
  exit 2
fi

for port in 18080 18081 19001; do
  if ss -H -ltn "sport = :$port" | grep -q .; then
    echo "loopback fixture port $port is already occupied; refusing to start" >&2
    exit 2
  fi
done

root=/mnt/d/liteCR
install_dir="$run_dir/keycloak-install/keycloak-26.8.0"
source "$run_dir/runtime.env"
source "$run_dir/runtime-secrets.env"
source "$run_dir/keycloak-bootstrap.env"
export KC_BOOTSTRAP_ADMIN_USERNAME KC_BOOTSTRAP_ADMIN_PASSWORD
export LITECR_OIDC_CLIENT_SECRET="$LITECR_E2E_ADMIN_OIDC_CLIENT_SECRET"

cd "$install_dir"
JAVA_OPTS_APPEND="${JAVA_OPTS_APPEND:-} -Djava.net.preferIPv4Stack=true" nohup ./bin/kc.sh start-dev \
  --health-enabled=true \
  --http-host=127.0.0.1 \
  --http-port=18081 \
  --http-management-host=127.0.0.1 \
  --http-management-port=19001 \
  --hostname=http://127.0.0.1:18081 \
  --import-realm \
  > "$run_dir/keycloak.log" 2>&1 < /dev/null &
echo "$!" > "$run_dir/keycloak.pid"
chmod 600 "$run_dir/keycloak.pid" "$run_dir/keycloak.log"

ready=0
for _ in $(seq 1 90); do
  if curl --fail --silent --max-time 2 http://127.0.0.1:18081/realms/litecr/.well-known/openid-configuration >/dev/null; then
    ready=1
    break
  fi
  sleep 1
done
if [[ $ready -ne 1 ]]; then
  echo 'private Keycloak fixture did not become ready; cleanup will stop only this marked run' >&2
  exit 1
fi

python3 "$root/apps/web/tests/fixtures/admin/set-keycloak-passwords.py" "$run_dir" http://127.0.0.1:18081

cd "$root"
api_binary=${LITECR_E2E_BINARY:-}
if [[ -z $api_binary ]]; then
  cargo build --locked -p litecr-server
  api_binary="$root/target/debug/litecr"
fi
if [[ $api_binary != /* || ! -x $api_binary || -L $api_binary || $(basename -- "$api_binary") != litecr ]]; then
  echo 'configured fixture API binary must be an absolute, executable, non-symlink litecr file' >&2
  exit 2
fi
nohup "$api_binary" serve \
  > "$run_dir/api.log" 2>&1 < /dev/null &
echo "$!" > "$run_dir/api.pid"
chmod 600 "$run_dir/api.pid" "$run_dir/api.log"

ready=0
for _ in $(seq 1 45); do
  if curl --fail --silent --max-time 2 http://127.0.0.1:18080/health/ready >/dev/null; then
    ready=1
    break
  fi
  sleep 1
done
if [[ $ready -ne 1 ]]; then
  echo 'private liteCR API did not become ready; cleanup will stop only this marked run' >&2
  exit 1
fi
printf 'started private loopback Keycloak and liteCR API for run %s\n' "$run_id"
