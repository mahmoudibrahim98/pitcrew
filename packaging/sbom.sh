#!/usr/bin/env bash
# Writes CycloneDX SBOMs (JSON, spec 1.5) for the shipped binaries into DIR:
#
#   DIR/pitcrewd.cdx.json          crate pitcrew-daemon
#   DIR/pitcrew.cdx.json           crate pitcrew-cli
#   DIR/pitcrew-ptyd.cdx.json      crate pitcrew-ptyd
#   DIR/pitcrew-askpass.cdx.json   crate pitcrew-remote (its binary)
#   DIR/pitcrew-desktop.cdx.json   crate pitcrew-desktop (apps/desktop/src-tauri, its own
#                                  workspace): the app inside the desktop installers
#
#   packaging/sbom.sh DIR
#
# Needs cargo-cyclonedx (`cargo install cargo-cyclonedx --locked`). One SBOM covers every
# platform's build of a binary, so dependencies for all targets are listed. SOURCE_DATE_EPOCH,
# if set, fixes the timestamp; otherwise the commit time is used when git is available.
#
# cargo-cyclonedx writes one file next to every workspace member's Cargo.toml. This script
# gives them a name nothing else uses, keeps the ones it needs and removes the rest.
set -euo pipefail

dir=${1:?usage: packaging/sbom.sh DIR}
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
tmp=pitcrew-sbom-tmp.cdx

if [ -z "${SOURCE_DATE_EPOCH:-}" ] && epoch=$(git -C "$root" log -1 --format=%ct 2>/dev/null); then
  export SOURCE_DATE_EPOCH=$epoch
fi

cleanup() {
  find "$root" -path "$root/target" -prune -o -name "$tmp.json" -type f -exec rm -f {} +
}
trap cleanup EXIT

for manifest in Cargo.toml apps/desktop/src-tauri/Cargo.toml; do
  cargo cyclonedx --manifest-path "$root/$manifest" \
    --format json --spec-version 1.5 --target all --override-filename "$tmp" -q
done
while read -r crate name; do
  mv "$root/$crate/$tmp.json" "$dir/$name.cdx.json"
  echo "$dir/$name.cdx.json"
done <<'EOF'
crates/daemon pitcrewd
crates/cli pitcrew
crates/ptyd pitcrew-ptyd
crates/remote pitcrew-askpass
apps/desktop/src-tauri pitcrew-desktop
EOF
