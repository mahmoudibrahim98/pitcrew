#!/usr/bin/env bash
# Starts the desktop app once, as installed, in a throwaway home and state directory, and checks
# from its log that it works out of the box with what it was installed with:
#
# - it found pitcrewd next to itself and trusted it (locate::check_trusted), started it, and the
#   daemon became ready;
# - it found and trusted pitcrew-askpass (no "cannot ask for passwords" warning);
# - its window came up;
# - an AppImage registered pitcrew:// links (in the throwaway home).
#
#   packaging/desktop/smoke.sh [--seconds N] APP
#
#   APP   An installed app's executable (e.g. /usr/bin/pitcrew-desktop from the .deb), an AppImage
#         (set APPIMAGE_EXTRACT_AND_RUN=1 where there is no FUSE), or a DMG, whose app is copied
#         out first, as a person would (macOS).
#
# Linux (under xvfb-run when there is no DISPLAY) and macOS. The app and the daemon it started are
# stopped afterwards. It never touches the real home: it makes its own.
set -uo pipefail
export LC_ALL=C

usage() { sed -n '2,19p' "$0"; }

seconds=60
app=""
while [ $# -gt 0 ]; do
  case "$1" in
    --seconds) seconds=$2; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    -*) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    *) app=$1; shift ;;
  esac
done
if [ -z "$app" ] || [ ! -f "$app" ]; then
  usage >&2
  exit 2
fi

tmp=$(mktemp -d)
home="$tmp/home"
state="$tmp/state"
log="$tmp/app.log"
mkdir -p "$home"
pid=""
stop() {
  if [ -n "$pid" ]; then
    kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
  fi
  # The daemon the app started, by its state directory.
  pkill -TERM -f -- "$state" 2>/dev/null
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    pgrep -f -- "$state" >/dev/null 2>&1 || break
    sleep 1
  done
  pkill -KILL -f -- "$state" 2>/dev/null
  if [ -n "$pid" ]; then kill -KILL -- "-$pid" 2>/dev/null || kill -KILL "$pid" 2>/dev/null; fi
  pid=""
}
trap 'stop; rm -rf "$tmp"' EXIT

appimage=0
case "$app" in
  *.AppImage)
    appimage=1
    exe=$(cd "$(dirname "$app")" && pwd)/$(basename "$app")
    chmod +x "$exe"
    ;;
  *.dmg)
    mnt="$tmp/dmg"
    mkdir -p "$mnt" "$tmp/Applications"
    hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mnt" "$app" >/dev/null || exit 1
    bundle=$(find "$mnt" -maxdepth 1 -name '*.app' | head -n 1)
    ditto "$bundle" "$tmp/Applications/$(basename "$bundle")"
    hdiutil detach -quiet "$mnt"
    exe="$tmp/Applications/$(basename "$bundle")/Contents/MacOS/pitcrew-desktop"
    ;;
  *) exe=$app ;;
esac

# TMPDIR too: an AppImage run with APPIMAGE_EXTRACT_AND_RUN extracts itself there.
mkdir -p "$tmp/tmp"
run=(env -u PITCREW_PITCREWD -u PITCREW_ASKPASS -u PITCREW_HELPERS -u PITCREW_SSH
  HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share"
  XDG_CACHE_HOME="$home/.cache" XDG_STATE_HOME="$home/.local/state" TMPDIR="$tmp/tmp"
  PITCREW_STATE_DIR="$state" PITCREW_DESKTOP_LOG=info "$exe")
if [ "$(uname -s)" = Linux ] && [ -z "${DISPLAY:-}" ]; then
  run=(xvfb-run -a -s "-screen 0 1280x800x24" "${run[@]}")
fi
# Its own process group, so the app and everything it started can be stopped together.
if command -v setsid >/dev/null 2>&1; then
  setsid "${run[@]}" </dev/null >"$log" 2>&1 &
else
  "${run[@]}" </dev/null >"$log" 2>&1 &
fi
pid=$!

up=0
for _ in $(seq 1 "$seconds"); do
  if grep -q "the main window is up" "$log" && grep -q "pitcrewd is ready" "$log"; then
    up=1
    break
  fi
  kill -0 "$pid" 2>/dev/null || break
  sleep 1
done
# A moment more for whatever it logs right after.
[ "$up" = 1 ] && sleep 3
stop

echo "--- the app's log"
cat "$log"
echo "---"

failures=0
expect() { # NAME PATTERN (extended regex)
  if grep -qE -- "$2" "$log"; then
    echo "  ok: $1"
  else
    echo "::error::$1: not in the log" >&2
    failures=$((failures + 1))
  fi
}
refuse() { # NAME PATTERN
  if grep -qE -- "$2" "$log"; then
    echo "::error::$1: $(grep -E -- "$2" "$log" | head -n 1)" >&2
    failures=$((failures + 1))
  else
    echo "  ok: no $1"
  fi
}
if [ "$appimage" = 1 ]; then
  # Inside the AppImage's own folder (mounted, or extracted).
  expect "pitcrewd found next to the app" "local daemon pitcrewd=/[^ ]*/usr/bin/pitcrewd "
else
  # Next to the app: the folder as named, or as the system resolves it (/var is /private/var on
  # macOS).
  named=$(cd "$(dirname "$exe")" && pwd -L)
  real=$(cd "$(dirname "$exe")" && pwd -P)
  if grep -qF -e "local daemon pitcrewd=$named/pitcrewd " -e "local daemon pitcrewd=$real/pitcrewd " "$log"; then
    echo "  ok: pitcrewd found next to the app"
  else
    echo "::error::pitcrewd found next to the app ($named): not in the log" >&2
    failures=$((failures + 1))
  fi
fi
expect "pitcrewd started" "starting pitcrewd"
expect "pitcrewd ready" "pitcrewd is ready"
expect "the window came up" "the main window is up"
refuse "missing or refused pitcrewd" "no pitcrewd to start|not running .*pitcrewd"
refuse "missing or refused pitcrew-askpass" "remote machines cannot ask for passwords"
if [ "$appimage" = 1 ]; then
  expect "pitcrew:// registered" "registered pitcrew:// links with the desktop"
  if grep -q '^x-scheme-handler/pitcrew=org.pitcrew.desktop-url-handler.desktop' \
    "$home/.config/mimeapps.list" 2>/dev/null; then
    echo "  ok: mimeapps.list names the AppImage's handler"
  else
    echo "::error::mimeapps.list does not name the AppImage's handler" >&2
    failures=$((failures + 1))
  fi
fi

if [ "$failures" -gt 0 ]; then
  echo "smoke: $failures check(s) failed" >&2
  exit 1
fi
echo "smoke: the app started with its own pitcrewd and askpass"
