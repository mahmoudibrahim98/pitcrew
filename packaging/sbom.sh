#!/usr/bin/env bash
# Writes CycloneDX SBOMs (JSON, spec 1.5) for the shipped binaries into DIR:
#
#   DIR/pitcrewd.cdx.json   crate pitcrew-daemon
#   DIR/pitcrew.cdx.json    crate pitcrew-cli
#
#   packaging/sbom.sh DIR
#
# Needs cargo-cyclonedx (`cargo install cargo-cyclonedx --locked`). One SBOM covers every
# platform's build of a binary, so dependencies for all targets are listed. SOURCE_DATE_EPOCH,
# if set, fixes the timestamp; otherwise the commit time is used when git is available.
#
# cargo-cyclonedx writes one file next to every workspace member's Cargo.toml. This script
# gives them a name nothing else uses, keeps the two it needs and removes the rest.
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

cargo cyclonedx --manifest-path "$root/Cargo.toml" \
  --format json --spec-version 1.5 --target all --override-filename "$tmp" -q
mv "$root/crates/daemon/$tmp.json" "$dir/pitcrewd.cdx.json"
mv "$root/crates/cli/$tmp.json" "$dir/pitcrew.cdx.json"
echo "$dir/pitcrewd.cdx.json"
echo "$dir/pitcrew.cdx.json"
