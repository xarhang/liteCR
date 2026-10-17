#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
RUNTIME="$HERE/.runtime"
INSTALL="$RUNTIME/keycloak-26.8.0"
DATA="$INSTALL/data"
fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

[[ -f "$INSTALL/.litecr-disposable-fixture" ]] || fail 'Fixture install marker missing; refusing reset.'
[[ ! -f "$RUNTIME/keycloak.pid" ]] || fail 'Stop Keycloak first with scripts/stop.sh.'
[[ -d "$DATA" ]] || { printf 'No fixture data directory exists; nothing to reset.\n'; exit 0; }
resolved_install="$(realpath -e "$INSTALL")"
resolved_data="$(realpath -e "$DATA")"
[[ "$resolved_data" == "$resolved_install/data" ]] || fail "Resolved data directory is unexpected: $resolved_data"
[[ "$resolved_install" == "$RUNTIME/keycloak-26.8.0" ]] || fail 'Install resolved outside the expected fixture path.'
printf 'This removes only the disposable Keycloak state at: %s\n' "$resolved_data"
read -r -p 'Type RESET-LITECR-KEYCLOAK to continue: ' confirmation
[[ "$confirmation" == RESET-LITECR-KEYCLOAK ]] || fail 'Reset cancelled.'
rm -rf -- "$resolved_data"
printf 'Removed disposable data. The next start imports the committed synthetic realm again.\n'
