#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
RUNTIME="$HERE/.runtime"
ARCHIVE="$RUNTIME/keycloak-26.8.0.zip"
INSTALL="$RUNTIME/keycloak-26.8.0"
REALM="$HERE/realm/litecr-realm.json"
EXPECTED_SHA256="7ed1de3fda2598369262613bf682aab7e233d80a38c405e91588f7a7454370a1"

fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

[[ "${OSTYPE:-}" == linux* ]] || fail 'Run from Linux/WSL Bash; the helper intentionally uses Linux loopback listener checks.'
command -v java >/dev/null || fail 'OpenJDK 25 is required; install it using your approved OS package process.'
command -v unzip >/dev/null || fail 'unzip is required.'
command -v sha256sum >/dev/null || fail 'sha256sum is required.'
command -v ss >/dev/null || fail 'iproute2 ss is required for loopback verification.'
command -v curl >/dev/null || fail 'curl is required for readiness verification.'
[[ -n "${KC_BOOTSTRAP_ADMIN_USERNAME:-}" ]] || fail 'Set KC_BOOTSTRAP_ADMIN_USERNAME in this shell; do not put it in the repository.'
[[ -n "${KC_BOOTSTRAP_ADMIN_PASSWORD:-}" ]] || fail 'Set KC_BOOTSTRAP_ADMIN_PASSWORD in this shell; do not put it in the repository.'
[[ -f "$ARCHIVE" ]] || fail "Place the official keycloak-26.8.0.zip at $ARCHIVE and verify its SHA-256 first."

JAVA_VERSION="$(java -version 2>&1 | sed -n '1s/.*version "\([0-9]*\).*/\1/p')"
[[ "$JAVA_VERSION" == 25 ]] || fail "Expected OpenJDK 25, found Java ${JAVA_VERSION:-unknown}."
ACTUAL_SHA256="$(sha256sum "$ARCHIVE" | awk '{print $1}')"
[[ "$ACTUAL_SHA256" == "$EXPECTED_SHA256" ]] || fail "Keycloak archive SHA-256 mismatch: $ACTUAL_SHA256"

mkdir -p "$RUNTIME"
if [[ ! -x "$INSTALL/bin/kc.sh" ]]; then
  [[ ! -e "$INSTALL" ]] || fail "Unexpected existing install directory: $INSTALL (move it aside manually)."
  unzip -q "$ARCHIVE" -d "$RUNTIME"
fi
[[ -x "$INSTALL/bin/kc.sh" ]] || fail "Expected extracted install at $INSTALL."
mkdir -p "$INSTALL/data/import"
install -m 600 "$REALM" "$INSTALL/data/import/litecr-realm.json"
touch "$INSTALL/.litecr-disposable-fixture"

if ss -H -ltn 'sport = :18081' | grep -q .; then
  fail 'TCP 18081 is already occupied; refusing to start against an unknown listener.'
fi
if ss -H -ltn 'sport = :19001' | grep -q .; then
  fail 'TCP 19001 is already occupied; refusing to start against an unknown management listener.'
fi

cd "$INSTALL"
nohup ./bin/kc.sh start-dev \
  --health-enabled=true \
  --http-host=127.0.0.1 \
  --http-port=18081 \
  --http-management-host=127.0.0.1 \
  --http-management-port=19001 \
  --hostname=http://127.0.0.1:18081 \
  --import-realm \
  > "$RUNTIME/keycloak.log" 2>&1 < /dev/null &
echo "$!" > "$RUNTIME/keycloak.pid"
chmod 600 "$RUNTIME/keycloak.pid" "$RUNTIME/keycloak.log"

for _ in $(seq 1 60); do
  if curl --fail --silent --show-error --max-time 2 \
    http://127.0.0.1:18081/realms/litecr/.well-known/openid-configuration \
    -o "$RUNTIME/discovery.json" 2>/dev/null; then
    break
  fi
  sleep 1
done
if [[ ! -s "$RUNTIME/discovery.json" ]]; then
  "$HERE/scripts/stop.sh" || true
  fail "Keycloak did not become ready; inspect $RUNTIME/keycloak.log."
fi
if ! grep -Fq '"issuer":"http://127.0.0.1:18081/realms/litecr"' "$RUNTIME/discovery.json"; then
  "$HERE/scripts/stop.sh" || true
  fail 'Discovery issuer differs from the required exact issuer; fixture process stopped.'
fi
if ! "$HERE/scripts/verify-listener.sh"; then
  "$HERE/scripts/stop.sh" || true
  fail 'Loopback listener verification failed; fixture process stopped.'
fi
printf 'Keycloak is ready at http://127.0.0.1:18081. Stop with %s/scripts/stop.sh\n' "$HERE"
