#!/usr/bin/env bash
# Checks a release directory against its manifest: every file listed in DIR/SHA256SUMS exists
# with that hash, and every file in DIR (except the manifest and its signatures) is listed.
# The release workflow runs it after each download, so nothing is attested or released that
# changed, or was added, after the manifest was written.
#
#   packaging/verify.sh DIR
#
# Exits 1 on any mismatch, missing file or unlisted file. packaging/test.sh tests it.
set -euo pipefail
export LC_ALL=C

dir=${1:?usage: packaging/verify.sh DIR}
cd "$dir"
[ -f SHA256SUMS ] || { echo "::error::no SHA256SUMS in $dir" >&2; exit 1; }

if command -v sha256sum >/dev/null 2>&1; then
  check=(sha256sum --check --strict)
else
  check=(shasum -a 256 --check --strict)
fi
if ! "${check[@]}" SHA256SUMS; then
  echo "::error::files in $dir do not match SHA256SUMS" >&2
  exit 1
fi

listed=$(sed -E 's/^[0-9a-f]{64} [ *]//' SHA256SUMS)
unlisted=0
for f in *; do
  [ -f "$f" ] || continue
  case "$f" in SHA256SUMS | SHA256SUMS.*) continue ;; esac
  if ! grep -qxF -- "$f" <<<"$listed"; then
    echo "::error::$f is not in SHA256SUMS" >&2
    unlisted=1
  fi
done
exit "$unlisted"
