#!/usr/bin/env bash
# Builds the desktop installers for one OS with Tauri's bundler, carrying everything the app needs
# to work out of the box:
#
# - pitcrewd, pitcrew-ptyd and pitcrew-askpass next to the app's executable (Tauri's
#   `externalBin`: /usr/bin in the .deb and the AppImage, Contents/MacOS, the install folder);
# - helpers/ in the app's resources: the remote helpers under their Platform::artefact() names
#   and their manifest.json;
# - the same manifest compiled into the app (PITCREW_HELPERS_MANIFEST), the only checksums a
#   release build trusts.
#
#   packaging/desktop/build.sh [--dist DIR] [--stage DIR] [--out DIR] [--bundles LIST]
#                              [--stage-only] TARGET
#
#   TARGET        x86_64-unknown-linux-gnu (deb, AppImage), universal-apple-darwin (app, DMG) or
#                 x86_64-pc-windows-msvc (NSIS). Run it on that OS.
#   --dist        build-release.sh's output, from the same commit (default: dist). It holds
#                 pitcrewd, pitcrew-ptyd and pitcrew-askpass for TARGET's OS (on Linux the static
#                 x86_64 musl builds), and the three helpers: pitcrewd-x86_64-unknown-linux-musl,
#                 pitcrewd-aarch64-unknown-linux-musl and pitcrewd-universal-apple-darwin.
#   --stage       Where the bundle's inputs go (default: dist/desktop-stage). It must be inside
#                 the repository: Tauri's configuration names them relative to
#                 apps/desktop/src-tauri.
#   --out         Where the installers are copied (default: dist/desktop).
#   --bundles     Tauri's bundle formats, comma-separated (default: deb,appimage | app,dmg | nsis).
#   --stage-only  Stage the inputs and write the configuration, then stop (no Tauri, no UI).
#
# Relative paths are taken from the repository's root, as in build-release.sh. Needs the UI built
# (apps/ui/dist) and the Tauri CLI: `cargo tauri`, or the command in TAURI_CLI. Writes
# STAGE/helpers/manifest.json and STAGE/tauri.bundle.json, then OUT/<installer>. See
# packaging/README.md, "The desktop installers".
set -euo pipefail
export LC_ALL=C

usage() { sed -n '2,31p' "$0"; }

root=$(cd "$(dirname "$0")/../.." && pwd)
dist=dist
stage=dist/desktop-stage
out=dist/desktop
bundles=""
stage_only=0
target=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dist) dist=$2; shift 2 ;;
    --stage) stage=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --bundles) bundles=$2; shift 2 ;;
    --stage-only) stage_only=1; shift ;;
    -h | --help) usage; exit 0 ;;
    -*) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    *)
      if [ -n "$target" ]; then echo "one TARGET only" >&2; exit 2; fi
      target=$1; shift ;;
  esac
done
if [ -z "$target" ]; then
  usage >&2
  exit 2
fi

# The sidecars' own target in DIST, their suffix, and the OS's installers.
case "$target" in
  x86_64-unknown-linux-gnu) from=x86_64-unknown-linux-musl exe="" default_bundles=deb,appimage ;;
  universal-apple-darwin) from=universal-apple-darwin exe="" default_bundles=app,dmg ;;
  x86_64-pc-windows-msvc) from=x86_64-pc-windows-msvc exe=.exe default_bundles=nsis ;;
  *) echo "unsupported desktop target: $target" >&2; exit 2 ;;
esac
bundles=${bundles:-$default_bundles}

sidecars=(pitcrewd pitcrew-ptyd pitcrew-askpass)
# pitcrew_remote::Platform::artefact(), in the order `sort` gives.
helpers=(pitcrewd-aarch64-unknown-linux-musl pitcrewd-universal-apple-darwin
  pitcrewd-x86_64-unknown-linux-musl)

cd "$root"
# Whatever umask the runner has, the staged files and folders are writable by their owner only:
# the app refuses a pitcrewd, askpass or helper that others can write (locate::check_trusted).
umask 022

missing=()
for bin in "${sidecars[@]}"; do
  [ -f "$dist/$bin-$from$exe" ] || missing+=("$dist/$bin-$from$exe")
done
for helper in "${helpers[@]}"; do
  [ -f "$dist/$helper" ] || missing+=("$dist/$helper")
done
if [ ${#missing[@]} -gt 0 ]; then
  echo "::error::missing from $dist (build them with packaging/build-release.sh): ${missing[*]}" >&2
  exit 1
fi

sha256() {
  local sum
  if command -v sha256sum >/dev/null 2>&1; then
    sum=$(sha256sum -- "$1")
  else
    sum=$(shasum -a 256 -- "$1")
  fi
  printf '%s' "${sum%% *}"
}

# The stage, named relative to apps/desktop/src-tauri: Tauri drops a Windows drive from absolute
# paths, and a relative one reads the same in Git Bash and in Tauri.
rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/helpers"
stage=$(cd "$stage" && pwd)
case "$stage" in
  "$root"/*) rel="../../../${stage#"$root"/}" ;;
  *) echo "the stage must be inside the repository ($root): $stage" >&2; exit 2 ;;
esac

# Sidecars, named as Tauri's externalBin wants them (<name>-<target>[.exe]); it drops the target.
for bin in "${sidecars[@]}"; do
  cp "$dist/$bin-$from$exe" "$stage/bin/$bin-$target$exe"
  chmod 0755 "$stage/bin/$bin-$target$exe"
done

# The helpers are data here: uploaded to remote machines, never run on this one.
for helper in "${helpers[@]}"; do
  cp "$dist/$helper" "$stage/helpers/$helper"
  chmod 0644 "$stage/helpers/$helper"
done

# `pitcrewd --version`'s second word, which every helper reports (same commit).
line=$("$stage/bin/pitcrewd-$target$exe" --version)
version=$(printf '%s\n' "$line" | awk 'NR == 1 { print $2 }')
# pitcrew_remote::helper::validate_version: 1 to 64 of 0-9 A-Z a-z . _ + -, starting with a digit.
if ! printf '%s' "$version" | grep -Eq '^[0-9][0-9A-Za-z._+-]{0,63}$'; then
  echo "::error::cannot read the helpers' version from \"$line\"" >&2
  exit 1
fi
# The installers are named after the app's version: say so when it is not pitcrewd's.
app_version=$(sed -nE 's/^  "version": "([^"]+)".*/\1/p' apps/desktop/src-tauri/tauri.conf.json)
if [ "$app_version" != "$version" ]; then
  echo "::warning::the desktop's version ($app_version, tauri.conf.json) is not pitcrewd's ($version)"
fi

# { "version", "sha256": { "<artefact>": "<hex>" } }, compact, no trailing newline: the bytes
# compiled into the app and the bytes of helpers/manifest.json are the same.
manifest="{\"version\":\"$version\",\"sha256\":{"
sep=""
for helper in "${helpers[@]}"; do
  manifest+="$sep\"$helper\":\"$(sha256 "$stage/helpers/$helper")\""
  sep=","
done
manifest+="}}"
printf '%s' "$manifest" >"$stage/helpers/manifest.json"
chmod 0644 "$stage/helpers/manifest.json"

# Merged over tauri.conf.json by `tauri build --config` (JSON merge patch). The sidecars and the
# helpers are named here only: tauri-build checks that they exist, so tauri.conf.json cannot.
# Paths are relative to apps/desktop/src-tauri, where Tauri reads them.
cat >"$stage/tauri.bundle.json" <<EOF
{
  "bundle": {
    "externalBin": [
      "$rel/bin/pitcrewd",
      "$rel/bin/pitcrew-ptyd",
      "$rel/bin/pitcrew-askpass"
    ],
    "resources": { "$rel/helpers": "helpers" },
    "linux": {
      "deb": { "desktopTemplate": "../../../packaging/desktop/pitcrew.desktop.hbs" }
    },
    "windows": {
      "nsis": { "installerHooks": "../../../packaging/desktop/installer-hooks.nsh" }
    }
  }
}
EOF

echo "staged in $stage: pitcrewd $version"
echo "PITCREW_HELPERS_MANIFEST=$manifest"
if [ "$stage_only" = 1 ]; then
  exit 0
fi

if [ ! -f apps/ui/dist/index.html ]; then
  echo "::error::the UI is not built: run corepack pnpm --filter @pitcrew/ui build" >&2
  exit 1
fi

target_dir=${CARGO_TARGET_DIR:-$root/apps/desktop/src-tauri/target}
bundle_dir="$target_dir/$target/release/bundle"
rm -rf "$bundle_dir"
read -r -a tauri <<<"${TAURI_CLI:-cargo tauri}"
# `custom-protocol` is the app's own feature, which a release build needs (src/app.rs): Tauri 2's
# CLI turns on only tauri/custom-protocol.
(
  cd apps/desktop/src-tauri
  PITCREW_HELPERS_MANIFEST=$manifest "${tauri[@]}" build --ci --target "$target" \
    --features custom-protocol --bundles "$bundles" --config "$rel/tauri.bundle.json"
)

mkdir -p "$out"
found=0
for f in "$bundle_dir"/deb/*.deb "$bundle_dir"/appimage/*.AppImage "$bundle_dir"/dmg/*.dmg \
  "$bundle_dir"/nsis/*-setup.exe; do
  [ -f "$f" ] || continue
  cp "$f" "$out/"
  echo "$out/$(basename "$f")"
  found=$((found + 1))
done
if [ "$found" -eq 0 ]; then
  echo "::error::Tauri produced no installer in $bundle_dir" >&2
  exit 1
fi
