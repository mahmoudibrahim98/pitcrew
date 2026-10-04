#!/usr/bin/env bash
# Checks the desktop installers packaging/desktop/build.sh wrote, against the helpers' manifest it
# compiled into the app (STAGE/helpers/manifest.json):
#
#   packaging/desktop/check.sh --manifest FILE [--budget-mb N] INSTALLER...
#
# For each installer, unpacked as its OS would install it (a .deb with dpkg-deb, an AppImage with
# --appimage-extract, a DMG with hdiutil on macOS; an NSIS installer is installed and removed by
# check-windows.ps1, on Windows):
#
# - its size against the budget (25 MB): over it is a warning in the log and the job's summary,
#   not a failure;
# - pitcrew-desktop, pitcrewd, pitcrew-ptyd and pitcrew-askpass side by side;
# - helpers/ where the app looks for it (the app's resources): the three helpers and
#   manifest.json, which is FILE, and each helper's sha256 is FILE's;
# - FILE, byte for byte, inside the desktop executable (PITCREW_HELPERS_MANIFEST);
# - what the app's own trust check (locate::check_trusted) needs: no file or folder others can
#   write, owned by root in a .deb and an AppImage;
# - pitcrew:// registered: the desktop entry's MimeType and %u (deb, AppImage), the app's
#   CFBundleURLSchemes (DMG); universal binaries in the DMG;
# - where this machine can run them: pitcrewd and pitcrew-ptyd answer --version with the
#   manifest's version, and pitcrew-askpass refuses to run outside ssh (exit 2).
#
# Exits 1 if any check fails. packaging/test.sh tests it on a stand-in .deb.
set -uo pipefail
export LC_ALL=C

usage() { sed -n '2,24p' "$0"; }

here=$(cd "$(dirname "$0")" && pwd)
product=PitCrew
identifier=org.pitcrew.desktop
app_exe=pitcrew-desktop
sidecars=(pitcrewd pitcrew-ptyd pitcrew-askpass)
budget_mb=25
manifest=""
installers=()
while [ $# -gt 0 ]; do
  case "$1" in
    --manifest) manifest=$2; shift 2 ;;
    --budget-mb) budget_mb=$2; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    -*) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    *) installers+=("$1"); shift ;;
  esac
done
if [ -z "$manifest" ] || [ ${#installers[@]} -eq 0 ]; then
  usage >&2
  exit 2
fi
[ -f "$manifest" ] || { echo "no such manifest: $manifest" >&2; exit 2; }

failures=0
fail() { echo "::error::$*" >&2; failures=$((failures + 1)); }
ok() { echo "  ok: $*"; }
summary() { if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then printf '%s\n' "$*" >>"$GITHUB_STEP_SUMMARY"; fi; }

sha256() {
  local sum
  if command -v sha256sum >/dev/null 2>&1; then
    sum=$(sha256sum -- "$1")
  else
    sum=$(shasum -a 256 -- "$1")
  fi
  printf '%s' "${sum%% *}"
}

expected=$(cat "$manifest")
version=$(printf '%s' "$expected" | sed -nE 's/^\{"version":"([^"]+)".*/\1/p')
# "<artefact> <hex>" per helper, from build.sh's compact JSON.
entries=$(printf '%s' "$expected" | grep -oE '"pitcrewd-[A-Za-z0-9_.-]+":"[0-9a-f]{64}"' |
  sed -E 's/^"([^"]+)":"([0-9a-f]+)"$/\1 \2/')
if [ -z "$version" ] || [ "$(printf '%s\n' "$entries" | grep -c .)" -ne 3 ]; then
  echo "$manifest is not a helpers' manifest from build.sh" >&2
  exit 2
fi

tmp=$(mktemp -d)
cleanup() {
  if [ -n "${mounted:-}" ]; then hdiutil detach -quiet "$mounted" || true; fi
  rm -rf "$tmp"
}
trap cleanup EXIT

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  summary "| Installer | Size | Budget ($budget_mb MB) |"
  summary "|---|---|---|"
fi

check_size() { # FILE
  local bytes mb
  bytes=$(wc -c <"$1" | tr -d ' ')
  mb=$(awk -v b="$bytes" 'BEGIN { printf "%.1f", b / 1048576 }')
  if [ "$bytes" -gt $((budget_mb * 1048576)) ]; then
    echo "::warning::$(basename "$1") is $mb MB, over the $budget_mb MB budget"
    summary "| $(basename "$1") | $mb MB | **over** |"
  else
    ok "$mb MB, within the $budget_mb MB budget"
    summary "| $(basename "$1") | $mb MB | within |"
  fi
}

# An `ls -l`-style mode: nobody but the owner may write it.
owner_only_writes() { [ "${1:5:1}" != w ] && [ "${1:8:1}" != w ]; }

check_tree() { # LABEL BIN_DIR HELPERS_DIR [EXE_SUFFIX]
  local label=$1 bin=$2 helpers=$3 exe=${4:-} name hex f listed
  for name in "$app_exe" "${sidecars[@]}"; do
    if [ -f "$bin/$name$exe" ]; then
      ok "$label: $name$exe next to the app"
    else
      fail "$label: $name$exe is not next to $app_exe$exe in $bin"
    fi
  done
  if cmp -s "$manifest" "$helpers/manifest.json"; then
    ok "$label: helpers/manifest.json is the compiled manifest"
  else
    fail "$label: $helpers/manifest.json is missing or differs from $manifest"
  fi
  while read -r name hex; do
    if [ ! -f "$helpers/$name" ]; then
      fail "$label: helpers/$name is missing"
    elif [ "$(sha256 "$helpers/$name")" = "$hex" ]; then
      ok "$label: helpers/$name matches its sha256"
    else
      fail "$label: helpers/$name does not match the manifest's sha256"
    fi
  done <<<"$entries"
  for f in "$helpers"/*; do
    name=$(basename "$f")
    listed=$(printf '%s\n' "$entries" | awk -v n="$name" '$1 == n')
    if [ "$name" != manifest.json ] && [ -z "$listed" ]; then
      fail "$label: helpers/$name is not in the manifest"
    fi
  done
  if [ -f "$bin/$app_exe$exe" ] && grep -qaF -- "$expected" "$bin/$app_exe$exe"; then
    ok "$label: the helpers' checksums are compiled into $app_exe$exe"
  else
    fail "$label: $app_exe$exe does not hold the manifest (built without PITCREW_HELPERS_MANIFEST?)"
  fi
}

run_sidecars() { # LABEL BIN_DIR [EXE_SUFFIX]
  local label=$1 bin=$2 exe=${3:-} line status
  line=$("$bin/pitcrewd$exe" --version 2>&1 | head -n 1)
  case "$line" in
    "pitcrewd $version "*) ok "$label: $line" ;;
    *) fail "$label: pitcrewd --version said \"$line\", not version $version" ;;
  esac
  line=$("$bin/pitcrew-ptyd$exe" --version 2>&1 | head -n 1)
  case "$line" in
    "pitcrew-ptyd $version "* | "pitcrew-ptyd $version") ok "$label: $line" ;;
    *) fail "$label: pitcrew-ptyd --version said \"$line\"" ;;
  esac
  line=$(env -u PITCREW_ASKPASS_ADDR "$bin/pitcrew-askpass$exe" 'Password:' 2>&1)
  status=$?
  if [ "$status" = 2 ] && [ "${line#*not started by PitCrew}" != "$line" ]; then
    ok "$label: pitcrew-askpass runs (and refuses to answer outside ssh)"
  else
    fail "$label: pitcrew-askpass exited $status: $line"
  fi
}

check_desktop_entry() { # LABEL FILE
  if [ ! -f "$2" ]; then
    fail "$1: no desktop entry at $2"
    return
  fi
  if grep -qE '^MimeType=(.*;)?x-scheme-handler/pitcrew;' "$2"; then
    ok "$1: the desktop entry handles x-scheme-handler/pitcrew"
  else
    fail "$1: the desktop entry does not register pitcrew:// ($(grep '^MimeType' "$2"))"
  fi
  if grep -qE "^Exec=$app_exe %u\$" "$2"; then
    ok "$1: Exec passes the link on (%u)"
  else
    fail "$1: Exec does not pass the link on: $(grep '^Exec' "$2")"
  fi
  if command -v desktop-file-validate >/dev/null 2>&1; then
    if desktop-file-validate "$2"; then ok "$1: desktop-file-validate"; else fail "$1: desktop-file-validate"; fi
  fi
}

can_run_linux() { [ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ]; }

check_listing() { # LABEL: `ls -l`-style lines on stdin (MODE OWNER SIZE DATE TIME PATH)
  local label=$1 mode owner rest bad=0
  while read -r mode owner rest; do
    case "$mode" in -* | d*) ;; *) continue ;; esac # links are judged by what they lead to
    case "$owner" in root/root | 0/0) ;; *) fail "$label: ${rest##* } is owned by $owner"; bad=1 ;; esac
    owner_only_writes "$mode" || { fail "$label: ${rest##* } is writable by others ($mode)"; bad=1; }
  done
  [ "$bad" = 0 ] && ok "$label: every file and folder is root's, and only root can write it"
}

check_deb() { # FILE
  local deb=$1 root="$tmp/deb"
  rm -rf "$root"
  if ! dpkg-deb -x "$deb" "$root"; then
    fail "deb: cannot unpack $deb"
    return
  fi
  # dpkg installs these owners and modes.
  check_listing deb < <(dpkg-deb -c "$deb")
  check_tree deb "$root/usr/bin" "$root/usr/lib/$product/helpers"
  check_desktop_entry deb "$root/usr/share/applications/$product.desktop"
  echo "  Depends: $(dpkg-deb -f "$deb" Depends)"
  echo "  Recommends: $(dpkg-deb -f "$deb" Recommends)"
  if can_run_linux; then run_sidecars deb "$root/usr/bin"; fi
}

check_rpm() { # FILE
  local rpm_file=$1 root="$tmp/rpm" listing="$tmp/rpm-listing" tool
  for tool in rpm rpm2cpio cpio; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      fail "rpm: $tool is required to check $rpm_file"
      return
    fi
  done
  # Query the package metadata, never the host's installed RPM database.
  if ! rpm -qp --qf '[%{FILEMODES:perms} %{FILEUSERNAME}/%{FILEGROUPNAME} %{FILESIZES} %{FILENAMES}\n]' \
    "$rpm_file" >"$listing"; then
    fail "rpm: cannot read owners and modes from $rpm_file"
    return
  fi
  rm -rf "$root"
  mkdir -p "$root"
  # rpm2cpio | cpio, else libarchive's bsdtar (which reads every payload compression rpm uses).
  # Either way the tree is checked the same; a failure says what the unpackers reported.
  local unpack_errors="$tmp/rpm-unpack-errors"
  if ! rpm2cpio "$rpm_file" 2>"$unpack_errors" |
    (cd "$root" && cpio -idm --no-absolute-filenames --quiet) 2>>"$unpack_errors"; then
    rm -rf "$root"
    mkdir -p "$root"
    if command -v bsdtar >/dev/null 2>&1 && bsdtar -xf "$rpm_file" -C "$root" 2>>"$unpack_errors"; then
      echo "  note: rpm2cpio | cpio failed ($(head -c 300 "$unpack_errors" | tr '\n' ' ')); unpacked with bsdtar"
    else
      fail "rpm: cannot unpack $rpm_file: $(head -c 300 "$unpack_errors" | tr '\n' ' ')"
      return
    fi
  fi
  check_listing rpm <"$listing"
  check_tree rpm "$root/usr/bin" "$root/usr/lib/$product/helpers"
  check_desktop_entry rpm "$root/usr/share/applications/$product.desktop"
  echo "  Requires: $(rpm -qp --requires "$rpm_file")"
  if can_run_linux; then run_sidecars rpm "$root/usr/bin"; fi
}

check_appimage() { # FILE
  local image root offset
  image=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
  chmod +x "$image"
  rm -rf "$tmp/squashfs-root"
  if ! (cd "$tmp" && "$image" --appimage-extract >/dev/null); then
    fail "AppImage: cannot extract $1"
    return
  fi
  root="$tmp/squashfs-root"
  if command -v unsquashfs >/dev/null 2>&1 && offset=$("$image" --appimage-offset); then
    # The image's own owners and modes: what the AppImage's mount shows the app.
    check_listing AppImage < <(unsquashfs -o "$offset" -lln "$image")
  else
    echo "::notice::unsquashfs is not installed: the AppImage's owners and modes are not checked"
  fi
  check_tree AppImage "$root/usr/bin" "$root/usr/lib/$product/helpers"
  check_desktop_entry AppImage "$root/$product.desktop"
  if can_run_linux; then run_sidecars AppImage "$root/usr/bin"; fi
}

check_dmg() { # FILE
  local app plist bin archs f
  if [ "$(uname -s)" != Darwin ]; then
    fail "DMG: check $1 on macOS"
    return
  fi
  mounted="$tmp/dmg"
  mkdir -p "$mounted"
  if ! hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mounted" "$1" >/dev/null; then
    mounted=""
    fail "DMG: cannot attach $1"
    return
  fi
  app="$mounted/$product.app"
  # Owners on a mounted image are ignored (uid 99): the person copies the app out, and owns it.
  local writable
  writable=$(find "$app" \( -perm -g+w -o -perm -o+w \) ! -type l)
  if [ -z "$writable" ]; then
    ok "DMG: nothing in $product.app is writable by others"
  else
    fail "DMG: writable by others: $writable"
  fi
  check_tree DMG "$app/Contents/MacOS" "$app/Contents/Resources/helpers"
  plist="$app/Contents/Info.plist"
  if [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleURLTypes:0:CFBundleURLSchemes:0' "$plist" 2>&1)" = pitcrew ]; then
    ok "DMG: CFBundleURLSchemes has pitcrew"
  else
    fail "DMG: $plist does not register pitcrew://"
  fi
  if [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist" 2>&1)" = "$identifier" ]; then
    ok "DMG: CFBundleIdentifier is $identifier (the notifications' sender)"
  else
    fail "DMG: CFBundleIdentifier is not $identifier"
  fi
  for f in "$app/Contents/MacOS"/* "$app/Contents/Resources/helpers/pitcrewd-universal-apple-darwin"; do
    archs=$(lipo -archs "$f" 2>/dev/null)
    # Each name is matched on its own: in " x86_64 arm64 " the two share the space between them.
    if [[ " $archs " == *" x86_64 "* && " $archs " == *" arm64 "* ]]; then
      ok "DMG: $(basename "$f"): $archs"
    else
      fail "DMG: $(basename "$f") is not universal ($archs)"
    fi
  done
  run_sidecars DMG "$app/Contents/MacOS"
  hdiutil detach -quiet "$mounted" && mounted=""
}

check_nsis() { # FILE
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) ;;
    *) fail "NSIS: check $1 on Windows"; return ;;
  esac
  if ! powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$here/check-windows.ps1")" \
    -Installer "$(cygpath -w "$1")" -Manifest "$(cygpath -w "$manifest")"; then
    fail "NSIS: check-windows.ps1 failed"
  fi
}

for installer in "${installers[@]}"; do
  echo "$(basename "$installer"):"
  if [ ! -f "$installer" ]; then
    fail "no such installer: $installer"
    continue
  fi
  check_size "$installer"
  case "$installer" in
    *.deb) check_deb "$installer" ;;
    *.rpm) check_rpm "$installer" ;;
    *.AppImage) check_appimage "$installer" ;;
    *.dmg) check_dmg "$installer" ;;
    *-setup.exe) check_nsis "$installer" ;;
    *) fail "unknown kind of installer: $installer" ;;
  esac
done

if [ "$failures" -gt 0 ]; then
  echo "desktop installers: $failures check(s) failed" >&2
  exit 1
fi
echo "desktop installers: every check passed"
