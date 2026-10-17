#!/usr/bin/env bash
set -euo pipefail
umask 077

fail() {
  printf 'liteCR release activation failed [%s]\n' "$1" >&2
  exit 1
}

root=/opt/litecr
pending="$root/pending"
current="$root/current"
[[ -n "${LITECR_DATABASE_URL:-}" ]] || fail RUNTIME_DATABASE_URL_REQUIRED
[[ -L "$pending" ]] || fail PENDING_RELEASE_MISSING
candidate="$(readlink -e -- "$pending")" || fail PENDING_RELEASE_INVALID
[[ "$candidate" == "$root"/releases/* && -x "$candidate/bin/litecr" ]] || fail PENDING_RELEASE_OUTSIDE_RELEASES
[[ ! -e "$current" || -L "$current" ]] || fail CURRENT_IS_NOT_SYMLINK

# The target release must prove exact schema compatibility using the runtime
# credential before current is changed. Doctor emits no credential material.
"$candidate/bin/litecr" doctor >/dev/null || fail DATABASE_SCHEMA_INCOMPATIBLE

temporary="$root/.current.$$.new"
[[ ! -e "$temporary" && ! -L "$temporary" ]] || fail TEMPORARY_LINK_EXISTS
cleanup() { rm -f -- "$temporary"; }
trap cleanup EXIT
ln -s -- "$candidate" "$temporary" || fail CURRENT_LINK_CREATE
mv -Tf -- "$temporary" "$current" || fail CURRENT_LINK_SWITCH
trap - EXIT
printf 'liteCR release activated: %s\n' "${candidate##*/}"
