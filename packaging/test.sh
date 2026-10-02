#!/usr/bin/env bash
# Tests for the packaging scripts that need no Rust toolchain: sha256sums.sh, verify.sh, sign.sh,
# and the desktop's staging (desktop/build.sh --stage-only) and installer checks
# (desktop/check.sh, on a stand-in .deb where dpkg-deb is). Runs anywhere bash and sha256sum (or
# shasum) do, including Git Bash. Everything is synthetic: stand-in scripts, never real binaries.
#
#   bash packaging/test.sh
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
tmp=$(mktemp -d)
# desktop/build.sh stages inside the repository (dist/ is ignored by git).
stage_dir="$root/dist/packaging-test-$$"
trap 'rm -rf "$tmp" "$stage_dir"' EXIT
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

# --- desktop/build.sh --stage-only: the sidecars, the helpers and their manifest
bins="$tmp/bins"
mkdir -p "$bins"
stub() { # FILE VERSION-LINE [EXIT [STDERR]]: a stand-in program
  # shellcheck disable=SC2016 # the stand-in's own "$1"
  printf '#!/bin/sh\n[ "${1:-}" = --version ] && { echo "%s"; exit 0; }\necho "%s" >&2\nexit %s\n' \
    "$2" "${4:-}" "${3:-0}" >"$1"
  chmod 0775 "$1"
}
musl=x86_64-unknown-linux-musl
stub "$bins/pitcrewd-$musl" "pitcrewd 1.2.3 (protocol 1, oldest accepted 1)"
stub "$bins/pitcrew-ptyd-$musl" "pitcrew-ptyd 1.2.3 (protocol 1)"
stub "$bins/pitcrew-askpass-$musl" "" 2 "pitcrew-askpass: not started by PitCrew (PITCREW_ASKPASS_ADDR unset)"
printf 'aarch64 helper\n' >"$bins/pitcrewd-aarch64-unknown-linux-musl"
printf 'macOS helper\n' >"$bins/pitcrewd-universal-apple-darwin"
desktop_stage() { bash "$here/desktop/build.sh" --stage-only --dist "$bins" --stage "$stage_dir" "$@"; }
hex() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

check "staging the Linux desktop" 0 desktop_stage x86_64-unknown-linux-gnu
cp "$tmp/out" "$tmp/staged"
manifest="$stage_dir/helpers/manifest.json"
want="{\"version\":\"1.2.3\",\"sha256\":{\"pitcrewd-aarch64-unknown-linux-musl\":\"$(hex "$bins/pitcrewd-aarch64-unknown-linux-musl")\",\"pitcrewd-universal-apple-darwin\":\"$(hex "$bins/pitcrewd-universal-apple-darwin")\",\"pitcrewd-x86_64-unknown-linux-musl\":\"$(hex "$bins/pitcrewd-$musl")\"}}"
check "the manifest has pitcrewd's version and each helper's sha256" 0 test "$(cat "$manifest")" = "$want"
cp "$tmp/staged" "$tmp/out"
output_has "and is what gets compiled in" "PITCREW_HELPERS_MANIFEST=$want"
check "the sidecars are named for Tauri's target" 0 test -x "$stage_dir/bin/pitcrewd-x86_64-unknown-linux-gnu"
check "the Linux sidecars are the static musl builds" 0 \
  cmp -s "$stage_dir/bin/pitcrew-ptyd-x86_64-unknown-linux-gnu" "$bins/pitcrew-ptyd-$musl"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) ;; # no Unix modes
  *)
    # shellcheck disable=SC2016 # sh's own "$1"
    check "a staged sidecar is 0755 though its source was 0775" 0 \
      sh -c 'ls -l "$1" | grep -q "^-rwxr-xr-x"' _ "$stage_dir/bin/pitcrewd-x86_64-unknown-linux-gnu"
    # shellcheck disable=SC2016
    check "a staged helper is 0644" 0 \
      sh -c 'ls -l "$1" | grep -q "^-rw-r--r--"' _ "$stage_dir/helpers/pitcrewd-universal-apple-darwin"
    ;;
esac
check "the overlay names the stage relative to src-tauri" 0 \
  grep -qF "\"../../../dist/packaging-test-$$/bin/pitcrewd\"" "$stage_dir/tauri.bundle.json"
check "and the helpers as the resources' helpers/" 0 \
  grep -qF "\"../../../dist/packaging-test-$$/helpers\": \"helpers\"" "$stage_dir/tauri.bundle.json"
check "an unknown desktop target is a usage error" 2 desktop_stage aarch64-unknown-linux-gnu
check "a stage outside the repository is refused" 2 \
  bash "$here/desktop/build.sh" --stage-only --dist "$bins" --stage "$tmp/stage" x86_64-unknown-linux-gnu
mv "$bins/pitcrewd-universal-apple-darwin" "$tmp/saved-helper"
check "a missing helper fails" 1 desktop_stage x86_64-unknown-linux-gnu
output_has "and is named" "pitcrewd-universal-apple-darwin"
mv "$tmp/saved-helper" "$bins/pitcrewd-universal-apple-darwin"
stub "$bins/pitcrewd-$musl" "pitcrewd ../../escape"
check "a version that could name another folder fails" 1 desktop_stage x86_64-unknown-linux-gnu
stub "$bins/pitcrewd-$musl" "pitcrewd 1.2.3 (protocol 1, oldest accepted 1)"
check "staged again" 0 desktop_stage x86_64-unknown-linux-gnu

# --- desktop/check.sh on a stand-in .deb laid out as Tauri's
if command -v dpkg-deb >/dev/null 2>&1 && [ "$(uname -s)" = Linux ]; then
  pkg="$tmp/pkg"
  make_deb() { # the .deb from $pkg, as root:root
    mkdir -p "$pkg/DEBIAN"
    printf 'Package: pitcrew\nVersion: 1.2.3\nArchitecture: amd64\nMaintainer: The PitCrew Authors\nDescription: stand-in\n' >"$pkg/DEBIAN/control"
    dpkg-deb --root-owner-group --build "$pkg" "$tmp/PitCrew_1.2.3_amd64.deb" >/dev/null
  }
  mkdir -p "$pkg/usr/bin" "$pkg/usr/lib/PitCrew/helpers" "$pkg/usr/share/applications"
  for bin in pitcrewd pitcrew-ptyd pitcrew-askpass; do
    cp "$stage_dir/bin/$bin-x86_64-unknown-linux-gnu" "$pkg/usr/bin/$bin"
  done
  { printf '#!/bin/sh\n# '; cat "$manifest"; printf '\n'; } >"$pkg/usr/bin/pitcrew-desktop"
  cp "$stage_dir/helpers/"* "$pkg/usr/lib/PitCrew/helpers/"
  printf '[Desktop Entry]\nCategories=Development;\nExec=pitcrew-desktop %%u\nIcon=pitcrew-desktop\nName=PitCrew\nTerminal=false\nType=Application\nMimeType=x-scheme-handler/pitcrew;\n' \
    >"$pkg/usr/share/applications/PitCrew.desktop"
  chmod -R go-w "$pkg"
  deb="$tmp/PitCrew_1.2.3_amd64.deb"
  installer_check() { bash "$here/desktop/check.sh" --manifest "$manifest" "$@"; }

  make_deb
  check "a complete .deb passes" 0 installer_check "$deb"
  output_has "its helpers match" "helpers/pitcrewd-universal-apple-darwin matches its sha256"
  output_has "its sidecars run" "pitcrewd 1.2.3 (protocol 1"
  output_has "askpass runs" "pitcrew-askpass runs"
  check "over the budget is a warning, not a failure" 0 installer_check --budget-mb 0 "$deb"
  output_has "and says so" "over the 0 MB budget"

  printf 'tampered\n' >"$pkg/usr/lib/PitCrew/helpers/pitcrewd-aarch64-unknown-linux-musl"
  make_deb
  check "a helper that does not match the manifest fails" 1 installer_check "$deb"
  output_has "and is named" "helpers/pitcrewd-aarch64-unknown-linux-musl does not match"
  cp "$stage_dir/helpers/pitcrewd-aarch64-unknown-linux-musl" "$pkg/usr/lib/PitCrew/helpers/"

  chmod g+w "$pkg/usr/bin/pitcrewd"
  make_deb
  check "a program others can write fails" 1 installer_check "$deb"
  output_has "and is named" "./usr/bin/pitcrewd is writable by others"
  chmod g-w "$pkg/usr/bin/pitcrewd"

  sed -i 's/ %u$//' "$pkg/usr/share/applications/PitCrew.desktop"
  make_deb
  check "a desktop entry that drops the link fails" 1 installer_check "$deb"
  output_has "and says why" "Exec does not pass the link on"
  sed -i 's/^Exec=pitcrew-desktop$/Exec=pitcrew-desktop %u/' "$pkg/usr/share/applications/PitCrew.desktop"

  printf '#!/bin/sh\n' >"$pkg/usr/bin/pitcrew-desktop"
  rm "$pkg/usr/bin/pitcrew-askpass"
  make_deb
  check "an app without compiled checksums, or without askpass, fails" 1 installer_check "$deb"
  output_has "no compiled checksums" "does not hold the manifest"
  output_has "no askpass" "pitcrew-askpass is not next to pitcrew-desktop"
else
  echo "(desktop/check.sh not tested here: it needs dpkg-deb on Linux)"
fi

echo "packaging tests: $passed passed, $failed failed"
[ "$failed" -eq 0 ]
