#!/usr/bin/env bash
# Builds the portable Windows zip: PitCrew to unzip into a folder and run from there, with no
# installer, no administrator and no registry keys.
#
#   packaging/portable/build.sh [--dist DIR] [--out DIR] [--desktop EXE] [--notices FILE]
#
#   --dist     build-release.sh's output for x86_64-pc-windows-msvc, from the same commit
#              (default: dist): pitcrewd, pitcrew, pitcrew-ptyd and pitcrew-askpass, each as
#              <name>-x86_64-pc-windows-msvc.exe.
#   --out      Where the folder and the zip go (default: dist/portable).
#   --desktop  pitcrew-desktop.exe, already built. By default it is built here: cargo build
#              --release --features custom-protocol for the target, which needs the UI built
#              (apps/ui/dist) and runs on Windows.
#   --notices  THIRD-PARTY-NOTICES.txt, already written. By default packaging/notices.mjs writes
#              it, which needs cargo and the UI's node_modules.
#
# Relative paths are taken from the repository's root, as in build-release.sh. Writes
# OUT/pitcrew-windows-x64-portable/ (the files, as zipped) and
# OUT/pitcrew-windows-x64-portable.zip, with the files at its root:
#
#   pitcrew-desktop.exe pitcrewd.exe pitcrew-ptyd.exe pitcrew-askpass.exe pitcrew.exe
#   LICENSE NOTICE THIRD-PARTY-NOTICES.txt README-portable.txt portable.txt SHA256SUMS
#
# portable.txt marks the copy as portable (apps/desktop/src-tauri/src/portable.rs). The zip is
# made with zip, or with 7-Zip (7z) where zip is missing (Git Bash on Windows). See
# packaging/README.md, "The portable Windows zip".
set -euo pipefail
export LC_ALL=C

usage() { sed -n '2,27p' "$0"; }

root=$(cd "$(dirname "$0")/../.." && pwd)
target=x86_64-pc-windows-msvc
name=pitcrew-windows-x64-portable
dist=dist
out=dist/portable
desktop=""
notices=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dist) dist=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --desktop) desktop=$2; shift 2 ;;
    --notices) notices=$2; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

cd "$root"
sidecars=(pitcrewd pitcrew pitcrew-ptyd pitcrew-askpass)
missing=()
for bin in "${sidecars[@]}"; do
  [ -f "$dist/$bin-$target.exe" ] || missing+=("$dist/$bin-$target.exe")
done
for f in ${desktop:+"$desktop"} ${notices:+"$notices"}; do
  [ -f "$f" ] || missing+=("$f")
done
if [ ${#missing[@]} -gt 0 ]; then
  echo "::error::missing (build them with packaging/build-release.sh $target): ${missing[*]}" >&2
  exit 1
fi
zipper=""
for tool in zip 7z; do
  if command -v "$tool" >/dev/null 2>&1; then zipper=$tool; break; fi
done
[ -n "$zipper" ] || { echo "::error::zip or 7z is needed to make the zip" >&2; exit 1; }

# The app's version, as the installers are named.
version=$(sed -nE 's/^  "version": "([^"]+)".*/\1/p' apps/desktop/src-tauri/tauri.conf.json)
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$'; then
  echo "::error::cannot read the desktop's version from tauri.conf.json" >&2
  exit 1
fi
commit=${GITHUB_SHA:-$(git rev-parse HEAD 2>/dev/null || echo unknown)}

if [ -z "$desktop" ]; then
  if [ ! -f apps/ui/dist/index.html ]; then
    echo "::error::the UI is not built: run corepack pnpm --filter @pitcrew/ui build" >&2
    exit 1
  fi
  # `custom-protocol` is the app's own feature, which a release build needs (src/app.rs). The
  # desktop links the C runtime statically itself (tauri-build's staticVCRuntime).
  cargo build --release --locked --target "$target" --features custom-protocol \
    --manifest-path apps/desktop/src-tauri/Cargo.toml
  desktop="${CARGO_TARGET_DIR:-$root/apps/desktop/src-tauri/target}/$target/release/pitcrew-desktop.exe"
fi

mkdir -p "$out"
stage="$out/$name"
rm -rf "$stage"
mkdir -p "$stage"
stage=$(cd "$stage" && pwd)
zip_path="$(cd "$out" && pwd)/$name.zip"
rm -f "$zip_path"

cp "$desktop" "$stage/pitcrew-desktop.exe"
for bin in "${sidecars[@]}"; do
  cp "$dist/$bin-$target.exe" "$stage/$bin.exe"
done
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
sed -e 's/\r$//' -e 's/$/\r/' packaging/portable/portable.txt >"$stage/portable.txt"
if grep -q '@[A-Z]*@' "$stage/README-portable.txt"; then
  echo "::error::README-portable.txt has a placeholder left" >&2
  exit 1
fi

bash packaging/sha256sums.sh "$stage" >/dev/null
bash packaging/verify.sh "$stage"

# Files at the zip's root, so Windows' "Extract All" makes one folder named after the zip.
(
  cd "$stage"
  files=(*)
  if [ "$zipper" = zip ]; then
    zip -q -X -9 "$zip_path" "${files[@]}"
  else
    7z a -tzip -mx=9 "$zip_path" "${files[@]}" >/dev/null
  fi
)

echo "$zip_path: PitCrew $version, $(wc -c <"$zip_path" | tr -d ' ') bytes"
(cd "$stage" && wc -c -- * | sed '$d')
