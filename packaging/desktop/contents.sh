#!/usr/bin/env bash
# Print the largest twenty payload files and raw/compressed bytes for each desktop installer.
# No install: unpack into a throwaway folder, or mount a DMG read-only on macOS.
set -euo pipefail
export LC_ALL=C
[ "$#" -gt 0 ] || { echo "usage: $0 INSTALLER..." >&2; exit 2; }
tmp=$(mktemp -d)
mounted=""
cleanup() {
  if [ -n "$mounted" ]; then hdiutil detach -quiet "$mounted" || true; fi
  rm -rf "$tmp"
}
trap cleanup EXIT
python=python3
command -v "$python" >/dev/null 2>&1 || python=python
for input in "$@"; do
  file=$(cd "$(dirname "$input")" && pwd)/$(basename "$input")
  root="$tmp/root"
  rm -rf "$root"
  mkdir -p "$root"
  case "$file" in
    *.deb) dpkg-deb -x "$file" "$root" ;;
    *.rpm) rpm2cpio "$file" | (cd "$root" && cpio -idm --no-absolute-filenames --quiet) ;;
    *.AppImage)
      (cd "$root" && "$file" --appimage-extract >/dev/null)
      root="$root/squashfs-root"
      ;;
    *.dmg)
      [ "$(uname -s)" = Darwin ] || { echo 'Inspect DMGs on macOS.' >&2; exit 1; }
      mounted="$root"
      hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mounted" "$file" >/dev/null
      ;;
    *-setup.exe) 7z x -y "-o$root" "$file" >/dev/null ;;
    *) echo "unknown installer: $input" >&2; exit 2 ;;
  esac
  "$python" - "$file" "$root" <<'PY'
from pathlib import Path
import sys
installer, root = map(Path, sys.argv[1:])
files = sorted(((p.stat().st_size, p.relative_to(root).as_posix())
                for p in root.rglob('*') if p.is_file() and not p.is_symlink()), reverse=True)
print(f'\n{installer.name}: {installer.stat().st_size:,} installer bytes; {sum(n for n,_ in files):,} payload bytes')
print('| Payload file | Bytes | MB (decimal) |\n| --- | ---: | ---: |')
for size, name in files[:20]:
    print(f'| `{name}` | {size:,} | {size/1_000_000:.3f} |')
PY
  if [ -n "$mounted" ]; then hdiutil detach -quiet "$mounted"; mounted=""; fi
done
