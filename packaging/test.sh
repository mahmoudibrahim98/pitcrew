#!/usr/bin/env bash
# Tests for the packaging scripts that need no Rust toolchain: sha256sums.sh, verify.sh and
# sign.sh. Runs anywhere bash and sha256sum (or shasum) do, including Git Bash.
#
#   bash packaging/test.sh
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failed=0
passed=0

check() { # NAME EXPECTED-STATUS COMMAND...
  local name=$1 want=$2 got
  shift 2
  "$@" >"$tmp/out" 2>&1
  got=$?
  if [ "$got" = "$want" ]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL: $name: exit $got, wanted $want"
    sed 's/^/  | /' "$tmp/out"
  fi
}

output_has() { # NAME TEXT
  if grep -qF -- "$2" "$tmp/out"; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL: $1: output lacks \"$2\""
    sed 's/^/  | /' "$tmp/out"
  fi
}

output_lacks() { # NAME TEXT
  if grep -qF -- "$2" "$tmp/out"; then
    failed=$((failed + 1))
    echo "FAIL: $1: output shows \"$2\""
  else
    passed=$((passed + 1))
  fi
}

# --- sha256sums.sh and verify.sh
dist="$tmp/dist"
mkdir -p "$dist"
printf 'helper\n' >"$dist/pitcrewd-x86_64-unknown-linux-musl"
printf 'cli\n' >"$dist/pitcrew-x86_64-unknown-linux-musl"
printf '{}\n' >"$dist/pitcrewd.cdx.json"
check "sha256sums writes a manifest" 0 bash "$here/sha256sums.sh" "$dist"
check "the manifest lists three files" 0 test "$(wc -l <"$dist/SHA256SUMS")" -eq 3
check "a fresh directory verifies" 0 bash "$here/verify.sh" "$dist"

printf 'signature\n' >"$dist/SHA256SUMS.minisig"
check "the manifest's signature need not be listed" 0 bash "$here/verify.sh" "$dist"

cp "$dist/pitcrew-x86_64-unknown-linux-musl" "$tmp/saved"
printf 'tampered\n' >"$dist/pitcrew-x86_64-unknown-linux-musl"
check "a changed file fails" 1 bash "$here/verify.sh" "$dist"
cp "$tmp/saved" "$dist/pitcrew-x86_64-unknown-linux-musl"
check "restored, it verifies again" 0 bash "$here/verify.sh" "$dist"

printf 'extra\n' >"$dist/pitcrewd-unlisted"
check "an unlisted file fails" 1 bash "$here/verify.sh" "$dist"
output_has "and is named" "pitcrewd-unlisted is not in SHA256SUMS"
rm "$dist/pitcrewd-unlisted"

rm "$dist/pitcrewd.cdx.json"
check "a missing listed file fails" 1 bash "$here/verify.sh" "$dist"
rm "$dist/SHA256SUMS"
check "no manifest fails" 1 bash "$here/verify.sh" "$dist"

# --- sign.sh: none of a kind's secrets set skips, any set fails, values never printed
sign() { # [NAME=VALUE...] KIND DIR, in an otherwise empty environment
  local vars=()
  while [ $# -gt 2 ]; do vars+=("$1"); shift; done
  env -i PATH="$PATH" ${vars[@]+"${vars[@]}"} bash "$here/sign.sh" "$@"
}
check "macos, no secrets: skipped" 0 sign macos "$dist"
output_has "says it skipped" "macos signing skipped"
check "windows, no secrets: skipped" 0 sign windows "$dist"
check "checksums, no secrets: skipped" 0 sign checksums "$dist"
check "macos, one secret: fails" 1 sign APPLE_ID=someone-secret macos "$dist"
output_has "names the secret" "APPLE_ID"
output_lacks "never prints a value" "someone-secret"
check "macos, password only: fails" 1 sign APPLE_CERTIFICATE_PASSWORD=pw-value macos "$dist"
output_lacks "never prints a value" "pw-value"
check "windows, certificate only: fails" 1 sign WINDOWS_CERTIFICATE=cert-value windows "$dist"
check "windows, both: fails (not implemented)" 1 \
  sign WINDOWS_CERTIFICATE=c WINDOWS_CERTIFICATE_PASSWORD=p windows "$dist"
check "checksums, password only: fails" 1 sign MINISIGN_PASSWORD=p checksums "$dist"
check "another kind's secret does not count" 0 sign APPLE_ID=x windows "$dist"
check "an empty value counts as unset" 0 sign MINISIGN_SECRET_KEY= checksums "$dist"
check "an unknown kind is a usage error" 2 sign linux "$dist"
check "a missing directory is a usage error" 2 sign macos "$tmp/nope"

echo "packaging tests: $passed passed, $failed failed"
[ "$failed" -eq 0 ]
