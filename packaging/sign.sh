#!/usr/bin/env bash
# Signing placeholders for the release workflow. Each kind reads its secrets from the
# environment, by name, and is skipped with a notice when they are absent. There is no signing
# identity yet, so a kind whose secrets ARE set fails rather than publish unsigned files as if
# they were signed.
#
#   packaging/sign.sh macos DIR       APPLE_CERTIFICATE (base64 .p12), APPLE_CERTIFICATE_PASSWORD,
#                                     APPLE_SIGNING_IDENTITY; notarisation: APPLE_ID,
#                                     APPLE_TEAM_ID, APPLE_APP_PASSWORD
#   packaging/sign.sh windows DIR     WINDOWS_CERTIFICATE (base64 .pfx), WINDOWS_CERTIFICATE_PASSWORD
#   packaging/sign.sh checksums DIR   MINISIGN_SECRET_KEY, MINISIGN_PASSWORD
#                                     (writes DIR/SHA256SUMS.minisig)
set -euo pipefail

kind=${1:?usage: packaging/sign.sh macos|windows|checksums DIR}
dir=${2:?usage: packaging/sign.sh macos|windows|checksums DIR}
[ -d "$dir" ] || { echo "no such directory: $dir" >&2; exit 2; }

case "$kind" in
  macos) secret=APPLE_CERTIFICATE ;;
  windows) secret=WINDOWS_CERTIFICATE ;;
  checksums) secret=MINISIGN_SECRET_KEY ;;
  *) echo "unknown kind: $kind" >&2; exit 2 ;;
esac

if [ -z "${!secret:-}" ]; then
  echo "::notice::$kind signing skipped: $secret is not set"
  exit 0
fi

# To implement: macOS `codesign --options runtime --timestamp` then `xcrun notarytool submit
# --wait` (a bare Mach-O cannot be stapled); Windows `signtool sign /fd sha256 /tr <RFC 3161
# timestamp URL> /td sha256`; checksums `minisign -S -m SHA256SUMS`.
echo "::error::$kind signing: $secret is set but signing is not implemented yet" >&2
exit 1
