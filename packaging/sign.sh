#!/usr/bin/env bash
# Signing placeholders for the release workflow. Each kind reads its secrets from the
# environment, by name:
#
# - none of them set: skipped, with a notice;
# - any of them set: fails. There is no signing identity yet, and a partial set is a
#   misconfiguration, so the release stops rather than publish unsigned files as if they were
#   signed. Once a kind is implemented, it will need all of its secrets.
#
#   packaging/sign.sh macos DIR       APPLE_CERTIFICATE (base64 .p12), APPLE_CERTIFICATE_PASSWORD,
#                                     APPLE_SIGNING_IDENTITY; notarisation: APPLE_ID,
#                                     APPLE_TEAM_ID, APPLE_APP_PASSWORD
#   packaging/sign.sh windows DIR     WINDOWS_CERTIFICATE (base64 .pfx), WINDOWS_CERTIFICATE_PASSWORD
#   packaging/sign.sh checksums DIR   MINISIGN_SECRET_KEY, MINISIGN_PASSWORD
#                                     (writes DIR/SHA256SUMS.minisig)
#
# Only the names of the secrets are ever printed. packaging/test.sh tests this script.
set -euo pipefail

usage="usage: packaging/sign.sh macos|windows|checksums DIR"
kind=${1:?$usage}
dir=${2:?$usage}
[ -d "$dir" ] || { echo "no such directory: $dir" >&2; exit 2; }

case "$kind" in
  macos)
    secrets=(APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_ID
      APPLE_TEAM_ID APPLE_APP_PASSWORD) ;;
  windows) secrets=(WINDOWS_CERTIFICATE WINDOWS_CERTIFICATE_PASSWORD) ;;
  checksums) secrets=(MINISIGN_SECRET_KEY MINISIGN_PASSWORD) ;;
  *) echo "unknown kind: $kind" >&2; echo "$usage" >&2; exit 2 ;;
esac

set_names=()
for name in "${secrets[@]}"; do
  if [ -n "${!name:-}" ]; then set_names+=("$name"); fi
done

if [ ${#set_names[@]} -eq 0 ]; then
  echo "::notice::$kind signing skipped: none of ${secrets[*]} is set"
  exit 0
fi

# To implement: macOS `codesign --options runtime --timestamp` then `xcrun notarytool submit
# --wait` (a bare Mach-O cannot be stapled); Windows `signtool sign /fd sha256 /tr <RFC 3161
# timestamp URL> /td sha256`; checksums `minisign -S -m SHA256SUMS`.
echo "::error::$kind signing: ${set_names[*]} set, but signing is not implemented yet" >&2
exit 1
