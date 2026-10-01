#!/usr/bin/env bash
# Writes DIR/SHA256SUMS for every regular file in DIR, except the manifest and its signatures,
# in the format `sha256sum --check` reads: "<hex>  <name>", sorted by name.
#
#   packaging/sha256sums.sh DIR
#
# Verify a download with: sha256sum --check --ignore-missing SHA256SUMS
set -euo pipefail
export LC_ALL=C

dir=${1:?usage: packaging/sha256sums.sh DIR}
cd "$dir"

files=()
for f in *; do
  [ -f "$f" ] || continue
  case "$f" in SHA256SUMS | SHA256SUMS.*) continue ;; esac
  files+=("$f")
done
if [ ${#files[@]} -eq 0 ]; then
  echo "no files to checksum in $dir" >&2
  exit 1
fi

for f in "${files[@]}"; do
  if command -v sha256sum >/dev/null 2>&1; then
    sum=$(sha256sum -- "$f")
  else
    sum=$(shasum -a 256 -- "$f")
  fi
  printf '%s  %s\n' "${sum%% *}" "$f"
done >SHA256SUMS.partial
mv SHA256SUMS.partial SHA256SUMS
cat SHA256SUMS
