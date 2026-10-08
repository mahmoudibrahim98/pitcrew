#!/usr/bin/env bash
# Builds the portable Windows zip: PitCrew to unzip into a folder and run from there, with no
# installer, no administrator and no registry keys, and with the remote helpers it deploys to
# Linux machines.
#
#   packaging/portable/build.sh [--dist DIR] [--out DIR] [--desktop EXE] [--notices FILE]
#                               [--channel release|main] [--version VERSION]
#
#   --dist     From the same commit (default: dist): build-release.sh's Windows programs,
#              pitcrewd, pitcrew, pitcrew-ptyd and pitcrew-askpass as
#              <name>-x86_64-pc-windows-msvc.exe, and its static Linux helpers,
#              pitcrewd-x86_64-unknown-linux-musl and pitcrewd-aarch64-unknown-linux-musl.
#   --out      Where the folder and the zip go (default: dist/portable).
#   --desktop  pitcrew-desktop.exe, already built with this zip's helper manifest compiled in. By
#              default it is built here: cargo build --release --features custom-protocol for the
#              target with PITCREW_HELPERS_MANIFEST, which needs the UI built (apps/ui/dist) and
#              runs on Windows.
#   --notices  THIRD-PARTY-NOTICES.txt, already written. By default packaging/notices.mjs writes
#              it, which needs cargo and the UI's node_modules.
#   --channel  `release` for a zip built from a release tag, whose app is offered newer releases;
#              `main` (the default) for a development build, which is offered none. Written into
#              portable.txt.
#   --version  The app's version (a release tag's, without the v); default tauri.conf.json's.
#
# PORTABLE_COMMIT names the commit in README-portable.txt (default: GITHUB_SHA, else HEAD).
# Relative paths are taken from the repository's root, as in build-release.sh. Writes
# OUT/pitcrew-windows-x64-portable/ (the files, as zipped) and
# OUT/pitcrew-windows-x64-portable.zip, with the files at its root:
#
#   pitcrew-desktop.exe pitcrewd.exe pitcrew-ptyd.exe pitcrew-askpass.exe pitcrew.exe
#   helpers/manifest.json helpers/pitcrewd-{x86_64,aarch64}-unknown-linux-musl.xz
#   LICENSE NOTICE THIRD-PARTY-NOTICES.txt README-portable.txt portable.txt SHA256SUMS
#
# The helpers are XZ-compressed as in the installers; manifest.json holds their version and the
# sha256 of their decoded bytes, and the same bytes are compiled into the app, the only checksums
# a release build trusts. SHA256SUMS lists every other file, helpers/ included, by its path. The
# zip is made with zip, or with 7-Zip (7z) where zip is missing (Git Bash on Windows). See
# packaging/README.md, "The portable Windows zip".
set -euo pipefail
export LC_ALL=C

usage() { sed -n '2,39p' "$0"; }

root=$(cd "$(dirname "$0")/../.." && pwd)
target=x86_64-pc-windows-msvc
name=pitcrew-windows-x64-portable
dist=dist
out=dist/portable
desktop=""
notices=""
channel=main
version=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dist) dist=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --desktop) desktop=$2; shift 2 ;;
    --notices) notices=$2; shift 2 ;;
    --channel) channel=$2; shift 2 ;;
    --version) version=$2; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
case "$channel" in
  release | main) ;;
  *) echo "--channel is release or main, not $channel" >&2; exit 2 ;;
esac

cd "$root"
sidecars=(pitcrewd pitcrew pitcrew-ptyd pitcrew-askpass)
# pitcrew_remote::Platform::artefact() for Linux, in the order `sort` gives. Windows has no
# native remote helper; the macOS one needs a Mac to build, so a portable copy has none.
helpers=(pitcrewd-aarch64-unknown-linux-musl pitcrewd-x86_64-unknown-linux-musl)
missing=()
for bin in "${sidecars[@]}"; do
  [ -f "$dist/$bin-$target.exe" ] || missing+=("$dist/$bin-$target.exe")
done
for helper in "${helpers[@]}"; do
  [ -f "$dist/$helper" ] || missing+=("$dist/$helper")
done
for f in ${desktop:+"$desktop"} ${notices:+"$notices"}; do
  [ -f "$f" ] || missing+=("$f")
done
if [ ${#missing[@]} -gt 0 ]; then
  echo "::error::missing (build them with packaging/build-release.sh): ${missing[*]}" >&2
  exit 1
fi
zipper=""
for tool in zip 7z; do
  if command -v "$tool" >/dev/null 2>&1; then zipper=$tool; break; fi
done
[ -n "$zipper" ] || { echo "::error::zip or 7z is needed to make the zip" >&2; exit 1; }
command -v xz >/dev/null 2>&1 || { echo "::error::xz is needed to compress the helpers" >&2; exit 1; }

sha256() {
  local sum
  if command -v sha256sum >/dev/null 2>&1; then
    sum=$(sha256sum -- "$1")
  else
    sum=$(shasum -a 256 -- "$1")
  fi
  printf '%s' "${sum%% *}"
}

# The app's version: the release's, or tauri.conf.json's.
if [ -z "$version" ]; then
  version=$(sed -nE 's/^  "version": "([^"]+)".*/\1/p' apps/desktop/src-tauri/tauri.conf.json)
fi
if ! printf '%s' "$version" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'; then
  echo "::error::not a version: \"$version\"" >&2
  exit 1
fi
commit=${PORTABLE_COMMIT:-${GITHUB_SHA:-$(git rev-parse HEAD 2>/dev/null || echo unknown)}}
if ! printf '%s' "$commit" | grep -Eq '^([0-9a-f]{7,64}|unknown)$'; then
  echo "::error::not a commit: \"$commit\"" >&2
  exit 1
fi

# The helpers' manifest, as desktop/build.sh writes it: `pitcrewd --version`'s second word (every
# program and helper of one commit reports it), and each helper's sha256. Compact, no trailing
# newline: the bytes compiled into the app and the bytes of helpers/manifest.json are the same.
line=$("$dist/pitcrewd-$target.exe" --version)
helper_version=$(printf '%s\n' "$line" | awk 'NR == 1 { print $2 }')
# pitcrew_remote::helper::validate_version: 1 to 64 of 0-9 A-Z a-z . _ + -, starting with a digit.
if ! printf '%s' "$helper_version" | grep -Eq '^[0-9][0-9A-Za-z._+-]{0,63}$'; then
  echo "::error::cannot read the helpers' version from \"$line\"" >&2
  exit 1
fi
manifest="{\"version\":\"$helper_version\",\"sha256\":{"
sep=""
for helper in "${helpers[@]}"; do
  manifest+="$sep\"$helper\":\"$(sha256 "$dist/$helper")\""
  sep=","
done
manifest+="}}"
echo "PITCREW_HELPERS_MANIFEST=$manifest"

if [ -z "$desktop" ]; then
  if [ ! -f apps/ui/dist/index.html ]; then
    echo "::error::the UI is not built: run corepack pnpm --filter @pitcrew/ui build" >&2
    exit 1
  fi
  # `custom-protocol` is the app's own feature, which a release build needs (src/app.rs). The
  # desktop links the C runtime statically itself (tauri-build's staticVCRuntime). TAURI_CONFIG
  # gives the app the version (a release's tag), as `tauri build --config` would.
  PITCREW_HELPERS_MANIFEST=$manifest TAURI_CONFIG="{\"version\":\"$version\"}" \
    cargo build --release --locked --target "$target" --features custom-protocol \
    --manifest-path apps/desktop/src-tauri/Cargo.toml
  desktop="${CARGO_TARGET_DIR:-$root/apps/desktop/src-tauri/target}/$target/release/pitcrew-desktop.exe"
fi
# The app trusts only the checksums compiled into it: they must be these helpers'.
if ! grep -qaF -- "$manifest" "$desktop"; then
  echo "::error::$desktop does not hold this zip's helper manifest (PITCREW_HELPERS_MANIFEST)" >&2
  exit 1
fi

mkdir -p "$out"
stage="$out/$name"
rm -rf "$stage"
mkdir -p "$stage/helpers"
stage=$(cd "$stage" && pwd)
zip_path="$(cd "$out" && pwd)/$name.zip"
rm -f "$zip_path"

cp "$desktop" "$stage/pitcrew-desktop.exe"
for bin in "${sidecars[@]}"; do
  cp "$dist/$bin-$target.exe" "$stage/$bin.exe"
done
for helper in "${helpers[@]}"; do
  xz --compress --stdout -6 -- "$dist/$helper" >"$stage/helpers/$helper.xz"
done
printf '%s' "$manifest" >"$stage/helpers/manifest.json"
cp LICENSE NOTICE "$stage/"
if [ -n "$notices" ]; then
  cp "$notices" "$stage/THIRD-PARTY-NOTICES.txt"
else
  node packaging/notices.mjs --target "$target" --out "$stage/THIRD-PARTY-NOTICES.txt" \
    --title "PitCrew $version: third-party notices"
fi
# Windows' own tools read these: CRLF line ends.
sed -e 's/\r$//' -e "s/@VERSION@/$version/g" -e "s/@COMMIT@/$commit/g" -e 's/$/\r/' \
  packaging/portable/README-portable.txt >"$stage/README-portable.txt"
sed -e 's/\r$//' -e "s/@CHANNEL@/$channel/g" -e 's/$/\r/' \
  packaging/portable/portable.txt >"$stage/portable.txt"
if grep -q '@[A-Z]*@' "$stage/README-portable.txt" "$stage/portable.txt"; then
  echo "::error::a placeholder is left in README-portable.txt or portable.txt" >&2
  exit 1
fi

# SHA256SUMS: every file but itself, by its path in the zip ("helpers/…" included), sorted, as
# `sha256sum --check` reads it. Then checked: each one matches, and none is unlisted.
(
  cd "$stage"
  find . -type f ! -name SHA256SUMS | sed 's|^\./||' | sort | while IFS= read -r f; do
    printf '%s  %s\n' "$(sha256 "$f")" "$f"
  done >SHA256SUMS
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum --check --strict --quiet SHA256SUMS
  else
    shasum -a 256 --check --strict --quiet SHA256SUMS
  fi
  listed=$(sed -E 's/^[0-9a-f]{64}  //' SHA256SUMS)
  present=$(find . -type f ! -name SHA256SUMS | sed 's|^\./||' | sort)
  [ "$listed" = "$present" ] || { echo "::error::SHA256SUMS does not list every file" >&2; exit 1; }
)

# Files at the zip's root, so Windows' "Extract All" makes one folder named after the zip.
(
  cd "$stage"
  files=(*)
  if [ "$zipper" = zip ]; then
    zip -q -X -9 -r "$zip_path" "${files[@]}"
  else
    7z a -tzip -mx=9 "$zip_path" "${files[@]}" >/dev/null
  fi
)

echo "$zip_path: PitCrew $version ($channel), $(wc -c <"$zip_path" | tr -d ' ') bytes"
(cd "$stage" && find . -type f | sed 's|^\./||' | sort | while IFS= read -r f; do
  printf '%10s %s\n' "$(wc -c <"$f" | tr -d ' ')" "$f"
done)
