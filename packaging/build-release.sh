#!/usr/bin/env bash
# Builds release binaries of pitcrewd (the daemon and remote helper) and pitcrew (the agent CLI),
# then writes a SHA256SUMS manifest next to them.
#
#   packaging/build-release.sh [--out DIR] [--zig] TARGET...
#
#   TARGET   A Rust target triple, or universal-apple-darwin (the aarch64 and x86_64 macOS builds
#            joined with lipo; run it on macOS).
#   --zig    Build with cargo-zigbuild: the static musl helper from any Linux (or macOS) host.
#   --out    Where the binaries go (default: dist). Existing files there are kept.
#
# Output: DIR/<bin>-<TARGET>[.exe] for each binary and target, and DIR/SHA256SUMS over every
# file in DIR. See packaging/README.md.
set -euo pipefail

bins=(pitcrewd pitcrew)
packages=(-p pitcrew-daemon -p pitcrew-cli)

usage() { sed -n '2,14p' "$0"; }

root=$(cd "$(dirname "$0")/.." && pwd)
out=dist
zig=0
targets=()
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out=$2; shift 2 ;;
    --zig) zig=1; shift ;;
    -h | --help) usage; exit 0 ;;
    -*) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    *) targets+=("$1"); shift ;;
  esac
done
if [ ${#targets[@]} -eq 0 ]; then
  usage >&2
  exit 2
fi

cd "$root"
target_dir=${CARGO_TARGET_DIR:-$root/target}
mkdir -p "$out"

build() {
  if [ "$zig" = 1 ]; then
    cargo zigbuild --release --locked --target "$1" "${packages[@]}"
  else
    cargo build --release --locked --target "$1" "${packages[@]}"
  fi
}

for target in "${targets[@]}"; do
  case "$target" in
    universal-apple-darwin)
      build aarch64-apple-darwin
      build x86_64-apple-darwin
      for bin in "${bins[@]}"; do
        lipo -create -output "$out/$bin-$target" \
          "$target_dir/aarch64-apple-darwin/release/$bin" \
          "$target_dir/x86_64-apple-darwin/release/$bin"
      done
      ;;
    *)
      build "$target"
      exe=""
      case "$target" in *-windows-*) exe=.exe ;; esac
      for bin in "${bins[@]}"; do
        cp "$target_dir/$target/release/$bin$exe" "$out/$bin-$target$exe"
      done
      ;;
  esac
done

bash "$root/packaging/sha256sums.sh" "$out"
