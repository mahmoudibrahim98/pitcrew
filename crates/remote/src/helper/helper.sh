# pitcrew-helper-script-begin
# PitCrew's side of deploying and starting the helper (pitcrewd) on a machine.
#
# It arrives on stdin and runs as
# `/bin/sh -c BOOTSTRAP sh <bytes> <length> <tool path> <command> <tag> <root> [args]` (see
# script.rs). The bootstrap reads exactly <bytes> with dd, so what follows on stdin (the helper
# itself, for `install`) is left for this script. It runs the script only when it has both its
# first and its last line and the expected length: a shell start-up file that ate some of stdin
# must not make the rest of it run.
#
# It prints a report between @@pitcrew-helper-begin-<tag> and @@pitcrew-helper-end-<tag>:
# key=value lines, and on failure error=<code> and detail=<text>. Whichever way it exits, it
# releases its lock (only if it still holds it) and removes its temporary file.
#
# The way to the root is checked as sshd checks the way to authorized_keys: every directory
# above it must belong to root or the user, and be writable by no one else unless sticky. Then
# the script works from inside the root with relative paths, so nothing renamed above it can
# redirect what it writes or runs.
#
# Needs a POSIX sh and dd, cat, ls, awk, sed, tr, cut, head, tail, wc, mkdir, rm, mv, ln, chmod,
# id, uname, date (+%s), find (-mmin), readlink, cksum and sleep; one of sha256sum, shasum or
# openssl to deploy; setsid or nohup (where they exist), or tmux, to start the helper; ps where
# there is no /proc; env, sbatch, squeue and scancel (sacct where there is one) for SLURM.
#
# Character sets are spelled out rather than written as ranges, which depend on the locale.

# Functions imported from the environment (bash exports them) must not stand in for tools.
unset -f awk cat cd chmod cksum command cut date dd echo env find head hostid id kill ln ls \
  mkdir mv nohup openssl printf ps pwd readlink rm sacct sbatch scancel sed setsid sha256sum \
  shasum sleep squeue sysctl tail test tmux tr umask uname wc 2>/dev/null
unset IFS TMUX ENV BASH_ENV CDPATH

pc_cmd=$1 pc_tag=$2 pc_root=$3
shift 3
# The caller's umask, kept for the helper: files agents make in shared directories keep the
# user's usual permissions. Everything this script makes is private.
pc_umask=$(umask)
case $pc_umask in ''|*[!01234567]*) pc_umask=0022 ;; esac
umask 077
pc_ep=run/endpoint.json
pc_pidf=run/pitcrewd.pid
pc_log=run/pitcrewd.log
pc_held=
pc_mine=
pc_tmp=
pc_tmp2=

pc_say() { printf '%s=%s\n' "$1" "$2"; }
# One line of at most 400 bytes, for details that quote command output.
pc_flat() { printf '%s' "$1" | tr '\r\n\t' '   ' | dd bs=400 count=1 2>/dev/null; }
pc_end() {
  printf '@@pitcrew-helper-end-%s\n' "$pc_tag"
  exit 0
}
pc_fail() {
  pc_say error "$1"
  pc_say detail "$(pc_flat "$2")"
  pc_end
}
pc_nap() { sleep 0.2 2>/dev/null || sleep 1; }
# A path as the user knows it: relative paths are inside the root.
pc_where() {
  case $1 in
    /*) printf '%s' "$1" ;;
    .) printf '%s' "$pc_root" ;;
    *) printf '%s/%s' "$pc_root" "$1" ;;
  esac
}

# Whether this run holds the lock directory $1: its owner line is the one this run wrote.
pc_owns() { [ -n "$pc_mine" ] && [ "$(cat "$1/owner" 2>/dev/null)" = "$pc_mine" ]; }

# pc_alive PID: whether PID is a live process. A zombie is not: where nothing reaps orphans (a
# container without an init), a dead helper stays one.
pc_alive() {
  kill -0 "$1" 2>/dev/null || return 1
  if [ -r "/proc/$1/stat" ]; then
    pc_pstate=$(sed 's/.*) //' "/proc/$1/stat" 2>/dev/null)
  else
    pc_pstate=$(ps -o stat= -p "$1" 2>/dev/null)
  fi
  case $pc_pstate in Z*|X*) return 1 ;; esac
}

# pc_started_at PID: when PID started (clock ticks since boot on Linux, else as ps prints it),
# to tell a reused pid apart. Empty when it is gone.
pc_started_at() {
  if [ -r "/proc/$1/stat" ]; then
    sed 's/.*) //' "/proc/$1/stat" 2>/dev/null | awk '{print $20}'
  else
    ps -o lstart= -p "$1" 2>/dev/null
  fi
}

pc_cleanup() {
  if [ -n "$pc_tmp" ]; then rm -f "$pc_tmp"; fi
  if [ -n "$pc_tmp2" ]; then rm -f "$pc_tmp2"; fi
  # Never a lock this run lost: the owner is checked right before removing it.
  if [ -n "$pc_held" ] && pc_owns "$pc_held"; then rm -rf "$pc_held"; fi
}
trap pc_cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 141' PIPE
trap 'exit 143' TERM

printf '@@pitcrew-helper-begin-%s\n' "$pc_tag"
pc_me=$(id -u 2>/dev/null)
case $pc_me in ''|*[!0123456789]*) pc_fail io "id -u failed" ;; esac
case $pc_root in /?*) ;; *) pc_fail usage "the root must be an absolute path" ;; esac

# This host, for lock owners and endpoint.json: its name (other characters made `_`, at most
# 40) and its machine's id. With the id, two hosts of one name (login nodes of federated sites
# sharing a home, or names made plain alike) never pass for one.
pc_names=0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._-
pc_id=
for pc_f in /etc/machine-id /var/lib/dbus/machine-id; do
  if [ -z "$pc_id" ] && [ -r "$pc_f" ]; then pc_id=$(cut -c 1-12 "$pc_f" 2>/dev/null); fi
done
if [ -z "$pc_id" ]; then pc_id=$(hostid 2>/dev/null); fi
if [ -z "$pc_id" ]; then pc_id=$(sysctl -n kern.uuid 2>/dev/null | cut -c 1-12); fi
if [ -z "$pc_id" ]; then pc_id=$(uname -a 2>/dev/null | cksum | cut -d ' ' -f 1); fi
pc_host=$(uname -n 2>/dev/null)
pc_host=$(printf '%s' "$pc_host" | tr -c "$pc_names" '_' | cut -c 1-40)
pc_id=$(printf '%s' "$pc_id" | tr -c "$pc_names" '_')
pc_host=${pc_host:-host}-$pc_id

# --- The way to the root ------------------------------------------------------------------

# pc_dir_ok DIR: DIR, on the way to $pc_goal, is a directory owned by root or this user, and
# writable by the group or others only if sticky (as /tmp). Anyone else could rename what is
# under it after the checks.
pc_dir_ok() {
  pc_ls=$(ls -ldn "$1" 2>/dev/null | awk '{print $1, $3}')
  pc_m=${pc_ls%% *} pc_u=${pc_ls#* }
  case $pc_m in
    d*) ;;
    *) pc_fail unsafe_dir "$1, on the way to $pc_goal, is not a directory" ;;
  esac
  case $pc_u in
    0|"$pc_me") ;;
    *) pc_fail unsafe_dir "$1, on the way to $pc_goal, belongs to uid $pc_u, who could replace what is under it" ;;
  esac
  case $pc_m in
    ?????w*|????????w*)
      case $pc_m in
        ?????????[tT]*) ;;
        *) pc_fail unsafe_dir "$1, on the way to $pc_goal, is writable by others ($pc_m)" ;;
      esac
      ;;
  esac
}

# pc_safe_way PATH: resolves PATH's parent one component at a time, as the kernel does, and
# checks every directory it goes through, from / down. A symbolic link must belong to root or
# this user (in a sticky directory its owner could swap it); it is followed, and the way to
# where it points is checked the same way. The directory holding the link already was.
pc_safe_way() {
  pc_goal=$1 pc_rest=${1%/*} pc_at= pc_hops=0
  pc_dir_ok /
  while [ -n "$pc_rest" ]; do
    pc_rest=${pc_rest#/}
    pc_c=${pc_rest%%/*}
    case $pc_rest in */*) pc_rest=/${pc_rest#*/} ;; *) pc_rest= ;; esac
    case $pc_c in
      ''|.) continue ;;
      ..) pc_at=${pc_at%/*}; continue ;;
    esac
    pc_next=$pc_at/$pc_c
    if [ -L "$pc_next" ]; then
      pc_hops=$((pc_hops + 1))
      if [ "$pc_hops" -gt 40 ]; then
        pc_fail unsafe_dir "too many symbolic links on the way to $pc_goal"
      fi
      pc_u=$(ls -ldn "$pc_next" 2>/dev/null | awk '{print $3}')
      case $pc_u in
        0|"$pc_me") ;;
        *) pc_fail unsafe_dir "the link $pc_next, on the way to $pc_goal, belongs to uid $pc_u" ;;
      esac
      pc_target=$(readlink "$pc_next") || pc_fail unsafe_dir "cannot read the link $pc_next"
      case $pc_target in /*) pc_at= ;; esac
      pc_rest=/$pc_target$pc_rest
      continue
    fi
    pc_dir_ok "$pc_next"
    pc_at=$pc_next
  done
}

# pc_private DIR: creates DIR if missing (0700, from the umask), then fails unless it is a real
# directory owned by this user with no access for anyone else (a set-group-ID bit inherited from
# the parent is fine). One that is not is refused, never repaired: what it holds may have been
# changed while it was open.
pc_private() {
  if [ ! -e "$1" ] && [ ! -L "$1" ]; then
    mkdir "$1" 2>/dev/null || [ -d "$1" ] || pc_fail io "cannot create $(pc_where "$1")"
  fi
  if [ -L "$1" ]; then pc_fail unsafe_dir "$(pc_where "$1") is a symbolic link"; fi
  if [ ! -d "$1" ]; then pc_fail unsafe_dir "$(pc_where "$1") is not a directory"; fi
  pc_ls=$(ls -ldn "$1" 2>/dev/null | awk '{print $1, $3}')
  case $pc_ls in
    "drwx------ $pc_me"|"drwx------. $pc_me"|"drwx--S--- $pc_me"|"drwx--S---. $pc_me") ;;
    "drwx------@ $pc_me"|"drwx--S---@ $pc_me")
      # macOS shows @ for extended attributes, which hides the + of an ACL; ls -le lists any.
      if [ "$(ls -lde "$1" 2>/dev/null | wc -l | tr -d ' ')" != 1 ]; then
        pc_fail unsafe_dir "$(pc_where "$1") has an access control list"
      fi
      ;;
    *) pc_fail unsafe_dir "$(pc_where "$1") must be owned by uid $pc_me with mode drwx------ and no ACL; it is: $pc_ls" ;;
  esac
}

# pc_enter CREATE: checks the way to the root, creates the root if CREATE is 1 (else returns 1
# when it is missing), checks it, and moves into it. From then on every path is relative.
pc_enter() {
  pc_safe_way "$pc_root"
  if [ "$1" != 1 ] && [ ! -e "$pc_root" ] && [ ! -L "$pc_root" ]; then return 1; fi
  pc_private "$pc_root"
  cd -P "$pc_root" 2>/dev/null || pc_fail io "cannot enter $pc_root"
  pc_private .
  pc_phys=$(pwd -P)
}

# --- Locks -------------------------------------------------------------------------------

# pc_stale DIR MINUTES: whether the lock DIR may be broken; its owner line goes to pc_judged.
# - Taken on this host: when its process is gone, or it is older than MINUTES by this host's
#   clock (the owner line records when it was taken).
# - Taken elsewhere, with no owner line yet, or with a pid or time that cannot be read: another
#   host's clock cannot be compared with this one, so only when the directory is older than
#   MINUTES plus 10. Hosts sharing a home must keep their clocks, and the file server's, within
#   10 minutes of each other.
pc_stale() {
  pc_judged=$(cat "$1/owner" 2>/dev/null)
  case $pc_judged in
    "$pc_host "*)
      pc_rest=${pc_judged#"$pc_host "}
      pc_opid=${pc_rest%% *}
      pc_rest=${pc_rest#* }
      pc_oat=${pc_rest#* }
      case $pc_opid in
        ''|*[!0123456789]*) ;;
        *)
          if ! pc_alive "$pc_opid"; then return 0; fi
          case $pc_oat in
            ''|*[!0123456789]*) ;;
            *)
              [ $(($(date +%s) - pc_oat)) -gt $(($2 * 60)) ]
              return
              ;;
          esac
          ;;
      esac
      ;;
  esac
  [ -n "$(find "$1" -prune -mmin +$(($2 + 10)) 2>/dev/null)" ]
}

# pc_lock DIR WAIT MINUTES: takes the lock DIR (mkdir), waiting up to WAIT seconds for another
# holder and breaking a stale one (see pc_stale). Breaking moves the lock aside atomically, so
# two waiters cannot both break it; if what was moved is not the lock judged stale (a fresh one
# was taken in between), it goes back while the name is free. Whatever happens, a holder checks
# that it still owns its lock before every change (pc_still_locked), and one that lost it stops.
pc_lock() {
  pc_waited=0
  while :; do
    if mkdir "$1" 2>/dev/null; then
      pc_held=$1
      pc_mine="$pc_host $$ $pc_tag $(date +%s)"
      if printf '%s\n' "$pc_mine" > "$1/owner"; then return 0; fi
      rm -rf "$1"
      pc_held=
      pc_fail io "cannot write $(pc_where "$1")/owner"
    fi
    if [ -L "$1" ] || { [ -e "$1" ] && [ ! -d "$1" ]; }; then
      pc_fail unsafe_dir "$(pc_where "$1") is not a directory"
    fi
    if [ -d "$1" ] && pc_stale "$1" "$3"; then
      pc_aside=$1.stale.$pc_tag.$pc_waited
      if mv "$1" "$pc_aside" 2>/dev/null; then
        if [ "$(cat "$pc_aside/owner" 2>/dev/null)" = "$pc_judged" ]; then
          rm -rf "$pc_aside"
        elif [ ! -e "$1" ]; then
          mv "$pc_aside" "$1" 2>/dev/null
        fi
        continue
      fi
    fi
    if [ "$pc_waited" -ge "$2" ]; then
      if [ -d "$1" ]; then
        pc_fail busy "$(pc_where "$1") is held by $(cut -d ' ' -f 1-2 "$1/owner" 2>/dev/null)"
      fi
      pc_fail io "cannot create $(pc_where "$1")"
    fi
    sleep 1
    pc_waited=$((pc_waited + 1))
  done
}

# Before every change: this run must still hold its lock.
pc_still_locked() {
  pc_owns "$pc_held" || pc_fail lock_lost "$(pc_where "$pc_held") was taken over by another run"
}

# pc_sweep_aside LOCK: removes locks that were moved aside and left. Only under that lock.
pc_sweep_aside() {
  for pc_f in "$1".stale.*; do
    if [ -e "$pc_f" ] || [ -L "$pc_f" ]; then rm -rf "$pc_f"; fi
  done
}

# --- Deploying ---------------------------------------------------------------------------

# pc_sha256 FILE: sets pc_sum (lower-case hex) and pc_tool from the first of sha256sum,
# shasum -a 256 and openssl dgst -sha256 that gives one.
pc_sha256() {
  for pc_tool in sha256sum shasum openssl; do
    command -v "$pc_tool" >/dev/null 2>&1 || continue
    case $pc_tool in
      sha256sum) pc_out=$(sha256sum < "$1" 2>/dev/null) ;;
      shasum) pc_out=$(shasum -a 256 < "$1" 2>/dev/null) ;;
      *) pc_out=$(openssl dgst -sha256 < "$1" 2>/dev/null) ;;
    esac || continue
    pc_sum=$(printf '%s\n' "$pc_out" \
      | sed -n 's/^.*\([0123456789abcdefABCDEF]\{64\}\).*$/\1/p' | head -n 1 | tr ABCDEF abcdef)
    if [ ${#pc_sum} -eq 64 ]; then return 0; fi
  done
  pc_sum= pc_tool=
  return 1
}
pc_no_hash_tool() {
  pc_fail no_hash_tool "none of sha256sum, shasum -a 256 or openssl dgst -sha256 gave a sha256"
}

# pc_check_version FILE: 0 when `FILE --version` exits 0 and the first line of its output has
# $pc_version as a word; 1 when it fails (exit code in pc_rc); 2 when it names something else.
# The first line is in pc_vline either way.
pc_check_version() {
  pc_out=$("$1" --version </dev/null 2>&1)
  pc_rc=$?
  pc_vline=$(pc_flat "$(printf '%s\n' "$pc_out" | head -n 1)")
  if [ "$pc_rc" -ne 0 ]; then return 1; fi
  case " $pc_vline " in *" $pc_version "*) return 0 ;; esac
  return 2
}
pc_version_mismatch() {
  pc_say version_line "$pc_vline"
  pc_say expected "$pc_version"
  pc_fail version_mismatch "$pc_vline"
}

# pc_link NAME TARGET: points bin/NAME at TARGET by renaming a new link over it, which is atomic
# with GNU mv -T or BSD mv -h. With neither (busybox), the link is replaced in place.
pc_link() {
  pc_new=bin/$1.tmp.$pc_tag
  rm -f "$pc_new"
  ln -sfn "$2" "$pc_new" || pc_fail io "cannot create $(pc_where "$pc_new")"
  if mv -T "$pc_new" "bin/$1" 2>/dev/null || mv -h "$pc_new" "bin/$1" 2>/dev/null; then
    return 0
  fi
  rm -f "$pc_new"
  if [ -e "bin/$1" ] && [ ! -L "bin/$1" ]; then
    pc_fail switch_failed "$(pc_where "bin/$1") is not a symbolic link"
  fi
  ln -sfn "$2" "bin/$1" || pc_fail switch_failed "cannot update $(pc_where "bin/$1")"
  pc_atomic=0
}

# Removes what interrupted runs left behind: nothing else can be writing under the lock.
pc_sweep() {
  pc_still_locked
  for pc_f in bin/*/pitcrewd.tmp.* bin/current.tmp.* bin/previous.tmp.*; do
    if [ -e "$pc_f" ] || [ -L "$pc_f" ]; then rm -rf "$pc_f"; fi
  done
  pc_sweep_aside bin/.lock
}

# Removes every version directory but the ones `current` and `previous` point to.
pc_gc() {
  pc_prev=$(readlink bin/previous 2>/dev/null)
  for pc_d in bin/*; do
    pc_n=${pc_d##*/}
    case $pc_n in
      [0123456789]*) ;;
      *) continue ;;
    esac
    case $pc_n in
      *[!0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._+-]*) continue ;;
    esac
    if [ -L "$pc_d" ] || [ ! -d "$pc_d" ]; then continue; fi
    if [ "$pc_n" = "$pc_now" ] || [ "$pc_n" = "$pc_prev" ]; then continue; fi
    pc_still_locked
    rm -rf "$pc_d" && pc_removed="$pc_removed $pc_n"
  done
}

# Points `current` at $pc_version (and `previous` at what it pointed to), removes old versions,
# and reports.
pc_switch() {
  pc_old=$(readlink bin/current 2>/dev/null)
  pc_still_locked
  if [ "$pc_old" != "$pc_version" ]; then
    pc_link current "$pc_version"
    if [ -n "$pc_old" ]; then pc_link previous "$pc_old"; fi
  fi
  pc_now=$(readlink bin/current 2>/dev/null)
  if [ "$pc_now" != "$pc_version" ]; then
    pc_fail switch_failed "current points to '$pc_now', not $pc_version"
  fi
  pc_gc
  pc_say state installed
  pc_say uploaded "$pc_uploaded"
  pc_say sha256 "$pc_sum"
  pc_say tool "$pc_tool"
  pc_say version_line "$pc_vline"
  pc_say current "$pc_now"
  pc_say previous "$(readlink bin/previous 2>/dev/null)"
  pc_say removed "${pc_removed# }"
  pc_say atomic "$pc_atomic"
  pc_end
}

# pc_deploy VERSION SHA256 SIZE WAIT MINUTES, as `check` or `install`. Both verify a copy
# already installed under VERSION and switch to it. Otherwise `check` reports it absent, and
# `install` reads SIZE bytes from stdin into a temporary file, verifies them and renames them
# into place.
pc_deploy() {
  pc_version=$1 pc_want=$2 pc_size=$3
  pc_atomic=1 pc_uploaded=0 pc_removed= pc_vline=
  pc_enter 1
  pc_private bin
  pc_lock bin/.lock "$4" "$5"
  pc_sweep
  pc_dir=bin/$pc_version
  pc_file=$pc_dir/pitcrewd
  if [ -e "$pc_dir" ] || [ -L "$pc_dir" ]; then pc_private "$pc_dir"; fi
  if [ -L "$pc_file" ] || { [ -e "$pc_file" ] && [ ! -f "$pc_file" ]; }; then
    pc_fail unsafe_dir "$(pc_where "$pc_file") is not a regular file"
  fi
  if [ -f "$pc_file" ]; then
    pc_sha256 "$pc_file" || pc_no_hash_tool
    if [ "$pc_sum" = "$pc_want" ]; then
      pc_still_locked
      chmod 700 "$pc_file" || pc_fail io "cannot chmod $(pc_where "$pc_file")"
      pc_check_version "$pc_file"
      case $? in
        0) pc_switch ;;
        1) pc_say code "$pc_rc"; pc_fail not_runnable "$pc_vline" ;;
        *) pc_version_mismatch ;;
      esac
    fi
    # A damaged or different copy under this version's name: it is replaced.
    pc_still_locked
    rm -f "$pc_file" || pc_fail io "cannot remove $(pc_where "$pc_file")"
    pc_say replaced 1
  fi
  if [ "$pc_cmd" = check ]; then
    pc_sha256 /dev/null || pc_no_hash_tool
    pc_say tool "$pc_tool"
    pc_say state absent
    pc_end
  fi
  pc_private "$pc_dir"
  pc_still_locked
  pc_tmp=$pc_dir/pitcrewd.tmp.$pc_tag
  if pc_err=$( (set -C; cat > "$pc_tmp") 2>&1 ); then :; else
    pc_fail io "cannot write $(pc_where "$pc_tmp"): $pc_err"
  fi
  pc_got=$(wc -c < "$pc_tmp" | tr -d ' ')
  if [ "$pc_got" != "$pc_size" ]; then
    rm -f "$pc_tmp"
    pc_say received "$pc_got"
    pc_say expected "$pc_size"
    pc_fail incomplete "$pc_got of $pc_size bytes arrived"
  fi
  pc_sha256 "$pc_tmp" || pc_no_hash_tool
  if [ "$pc_sum" != "$pc_want" ]; then
    rm -f "$pc_tmp"
    pc_say sha256 "$pc_sum"
    pc_say expected "$pc_want"
    pc_fail hash_mismatch "the upload's sha256 is $pc_sum"
  fi
  pc_still_locked
  chmod 700 "$pc_tmp" || pc_fail io "cannot chmod $(pc_where "$pc_tmp")"
  pc_check_version "$pc_tmp"
  case $? in
    0) ;;
    1) rm -f "$pc_tmp"; pc_say code "$pc_rc"; pc_fail not_runnable "$pc_vline" ;;
    *) rm -f "$pc_tmp"; pc_version_mismatch ;;
  esac
  pc_still_locked
  mv -f "$pc_tmp" "$pc_file" || pc_fail io "cannot rename $(pc_where "$pc_tmp")"
  pc_tmp=
  pc_uploaded=1
  pc_switch
}

# --- Running -----------------------------------------------------------------------------

pc_tmux() { tmux -L "$pc_tmux_name" -f /dev/null "$@"; }

# Run by the /bin/sh a launcher starts, inside the root, as
# `sh -c "$PC_EXEC" sh UMASK UID ROOT PIDFILE LOG HELPER ARGS...`. It checks that its directory
# is still the root the script entered (ROOT, its physical path: tmux enters it by name) and
# still private, records its pid, sends its output to the log (opened while the umask is still
# 077), restores the user's umask, and becomes the helper. Its stdin is the launcher's:
# /dev/null, or under tmux the pane's terminal, since tmux takes a pane whose terminal nobody
# holds open for dead.
PC_EXEC='m=$1 u=$2 w=$3 p=$4 l=$5
shift 5
[ "$(pwd -P)" = "$w" ] || exit 98
d=$(ls -ldn . 2>/dev/null | awk "{print \$1, \$3}")
case $d in
"drwx------ $u"|"drwx------. $u"|"drwx------@ $u"|"drwx--S--- $u"|"drwx--S---. $u"|"drwx--S---@ $u") ;;
*) exit 98 ;;
esac
printf "%s\n" "$$" > "$p.tmp" && mv -f "$p.tmp" "$p" || exit 98
exec >>"$l" 2>&1
umask "$m"
exec "$@"'

# Reads endpoint.json: the line into pc_line, and its pid, host and launcher into pc_epid,
# pc_ehost and pc_elauncher. Fails when there is none, or it is not in the form written below.
pc_recorded() {
  pc_line= pc_epid= pc_ehost= pc_elauncher=
  if [ ! -f "$pc_ep" ] || [ -L "$pc_ep" ]; then return 1; fi
  pc_line=$(head -n 1 "$pc_ep" 2>/dev/null)
  pc_fields=$(printf '%s\n' "$pc_line" | sed -n 's/^{"pid":\([0123456789][0123456789]*\),"host":"\([^" ]*\)","version":"[^"]*","started":[0123456789]*,"launcher":"\([^" ]*\)",.*$/\1 \2 \3/p')
  if [ -z "$pc_fields" ]; then return 1; fi
  pc_epid=${pc_fields%% *}
  pc_fields=${pc_fields#* }
  pc_ehost=${pc_fields%% *}
  pc_elauncher=${pc_fields#* }
}

# pc_is_helper PID: whether PID is a live process named pitcrewd.
pc_is_helper() {
  pc_alive "$1" || return 1
  if [ -r "/proc/$1/comm" ]; then
    pc_comm=$(cat "/proc/$1/comm" 2>/dev/null)
  else
    pc_comm=$(ps -o comm= -p "$1" 2>/dev/null)
  fi
  [ "${pc_comm##*/}" = pitcrewd ]
}

# 0: the recorded helper runs on this host; 1: none runs; 2: it was recorded on another host
# (one sharing this home), where it cannot be checked from here. With take-over set, a record
# from another host counts as gone.
pc_state() {
  pc_recorded || return 1
  if [ "$pc_ehost" != "$pc_host" ]; then
    if [ "$pc_takeover" = 1 ]; then return 1; fi
    return 2
  fi
  pc_is_helper "$pc_epid"
}

# pc_socket_here SOCKET: the socket as a path relative to the root, when it is inside it.
pc_socket_here() {
  pc_sock=$1
  case $1 in
    "$pc_root"/*) pc_sock_rel=${1#"$pc_root"/} ;;
    *) pc_sock_rel=$1 ;;
  esac
}

# pc_start LAUNCHER READY WAIT MINUTES TAKEOVER SOCKET SOCKET_JSON TMUX_NAME [ARGS...]: starts
# bin/<current version>/pitcrewd ARGS unless the helper already runs here, waits up to READY
# seconds for SOCKET, and records endpoint.json. SOCKET_JSON is SOCKET as a JSON string.
pc_start() {
  pc_launcher=$1 pc_ready=$2 pc_takeover=$5 pc_sockjson=$7 pc_tmux_name=$8
  pc_socket_here "$6"
  pc_enter 1
  pc_private run
  pc_lock run/.lock "$3" "$4"
  shift 8
  pc_state
  case $? in
    0) pc_say started 0; pc_say endpoint "$pc_line"; pc_end ;;
    2) pc_say host "$pc_ehost"; pc_fail other_host "the helper is recorded on $pc_ehost" ;;
  esac
  pc_ver=$(readlink bin/current 2>/dev/null)
  case $pc_ver in
    [0123456789]*) ;;
    *) pc_fail not_deployed "$(pc_where bin/current)" ;;
  esac
  case $pc_ver in
    *[!0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._+-]*)
      pc_fail not_deployed "$(pc_where bin/current)" ;;
  esac
  # The version itself, not `current`, so the version recorded is the one running even if a
  # deploy switches `current` meanwhile.
  pc_exe=bin/$pc_ver/pitcrewd
  if [ ! -f "$pc_exe" ] || [ ! -x "$pc_exe" ]; then
    pc_fail not_deployed "$(pc_where "$pc_exe")"
  fi
  # Nothing of ours runs here, so these are left over.
  pc_still_locked
  pc_sweep_aside run/.lock
  rm -f "$pc_ep" "$pc_pidf"
  if [ -S "$pc_sock_rel" ]; then rm -f "$pc_sock_rel"; fi
  if [ -f "$pc_log" ] && [ "$(wc -c < "$pc_log" | tr -d ' ')" -gt 1048576 ]; then
    mv -f "$pc_log" "$pc_log.old"
  fi
  pc_still_locked
  case $pc_launcher in
    tmux)
      command -v tmux >/dev/null 2>&1 || pc_fail start_failed "tmux is not installed"
      pc_tmux kill-session -t "=$pc_tmux_name" >/dev/null 2>&1
      # tmux expands formats (#{...}, #(...)) in -c: every # is doubled to stay a #.
      pc_tdir=$(printf '%s' "$pc_phys" | sed 's/#/##/g')
      pc_tmux new-session -d -s "$pc_tmux_name" -c "$pc_tdir" -- /bin/sh -c "$PC_EXEC" sh \
        "$pc_umask" "$pc_me" "$pc_phys" "$pc_pidf" "$pc_log" "$pc_exe" "$@" \
        </dev/null >/dev/null 2>&1 || pc_fail start_failed "tmux new-session failed"
      ;;
    *)
      pc_detach=
      if command -v setsid >/dev/null 2>&1; then pc_detach=setsid; fi
      if command -v nohup >/dev/null 2>&1; then pc_detach="$pc_detach nohup"; fi
      # In a subshell that exits at once, so the helper is not this script's child. It
      # inherits the working directory: the root itself, not its name.
      (
        trap - EXIT HUP INT PIPE TERM
        $pc_detach /bin/sh -c "$PC_EXEC" sh "$pc_umask" "$pc_me" "$pc_phys" "$pc_pidf" \
          "$pc_log" "$pc_exe" "$@" </dev/null >/dev/null 2>&1 &
      )
      ;;
  esac
  pc_started=$(date +%s)
  case $pc_started in ''|*[!0123456789]*) pc_fail io "date +%s printed '$pc_started'" ;; esac
  pc_deadline=$((pc_started + pc_ready))
  pc_pid=
  while :; do
    if [ -z "$pc_pid" ] && [ -s "$pc_pidf" ]; then
      pc_pid=$(cat "$pc_pidf" 2>/dev/null)
      case $pc_pid in *[!0123456789]*) pc_pid= ;; esac
    fi
    if [ -n "$pc_pid" ]; then
      if ! pc_alive "$pc_pid"; then
        pc_fail start_failed "it exited at once; its log ends: $(tail -n 3 "$pc_log" 2>/dev/null)"
      fi
      if [ -S "$pc_sock_rel" ]; then break; fi
    fi
    if [ "$(date +%s)" -ge "$pc_deadline" ]; then
      if [ -n "$pc_pid" ]; then kill -TERM "$pc_pid" 2>/dev/null; fi
      if [ "$pc_launcher" = tmux ]; then
        pc_tmux kill-session -t "=$pc_tmux_name" >/dev/null 2>&1
      fi
      pc_fail start_failed "no socket at $pc_sock after ${pc_ready}s; it was stopped"
    fi
    pc_nap
  done
  pc_line=$(printf '{"pid":%s,"host":"%s","version":"%s","started":%s000,"launcher":"%s","socket":%s}' \
    "$pc_pid" "$pc_host" "$pc_ver" "$pc_started" "$pc_launcher" "$pc_sockjson")
  pc_still_locked
  if printf '%s\n' "$pc_line" > "$pc_ep.tmp.$pc_tag" && mv -f "$pc_ep.tmp.$pc_tag" "$pc_ep"; then :; else
    rm -f "$pc_ep.tmp.$pc_tag"
    pc_fail io "cannot write $(pc_where "$pc_ep")"
  fi
  pc_say started 1
  pc_say endpoint "$pc_line"
  pc_end
}

# pc_status LAUNCHER SOCKET TMUX_NAME: what is installed, and whether the recorded helper runs.
# Takes no lock and changes nothing.
pc_status() {
  pc_takeover=0 pc_tmux_name=$3
  pc_socket_here "$2"
  if pc_enter 0; then
    if [ -e run ] || [ -L run ]; then pc_private run; fi
    pc_say installed "$(readlink bin/current 2>/dev/null)"
    pc_state
    case $? in
      0) pc_say state running ;;
      2) pc_say state elsewhere ;;
      *) pc_say state stopped ;;
    esac
    if [ -n "$pc_line" ]; then pc_say endpoint "$pc_line"; fi
    if [ -S "$pc_sock_rel" ]; then pc_say socket 1; else pc_say socket 0; fi
  else
    pc_say installed ''
    pc_say state stopped
    pc_say socket 0
  fi
  if [ "$1" = tmux ]; then
    if command -v tmux >/dev/null 2>&1 && pc_tmux has-session -t "=$pc_tmux_name" 2>/dev/null; then
      pc_say session 1
    else
      pc_say session 0
    fi
  fi
  pc_end
}

# pc_stop LAUNCHER WAIT MINUTES TAKEOVER STOP SOCKET TMUX_NAME: stops the recorded helper
# (SIGTERM, then SIGKILL after STOP seconds, each only while it is still the process that was
# recorded: same pid, same start time, still pitcrewd) and removes its records.
pc_stop() {
  pc_takeover=$4 pc_tmux_name=$7
  pc_socket_here "$6"
  pc_forced=0
  if ! pc_enter 0; then
    if [ "$1" = tmux ] && command -v tmux >/dev/null 2>&1; then
      pc_tmux kill-session -t "=$pc_tmux_name" >/dev/null 2>&1
    fi
    pc_say forced 0
    pc_end
  fi
  pc_private run
  pc_lock run/.lock "$2" "$3"
  pc_sweep_aside run/.lock
  pc_state
  pc_st=$?
  if [ "$pc_st" -eq 2 ]; then
    pc_say host "$pc_ehost"
    pc_fail other_host "the helper is recorded on $pc_ehost"
  fi
  if [ "$pc_st" -eq 0 ]; then
    pc_since=$(pc_started_at "$pc_epid")
    pc_still_locked
    kill -TERM "$pc_epid" 2>/dev/null
    pc_deadline=$(($(date +%s) + $5))
    while pc_is_helper "$pc_epid" && [ "$(pc_started_at "$pc_epid")" = "$pc_since" ]; do
      if [ "$(date +%s)" -ge "$pc_deadline" ]; then
        if [ "$pc_forced" = 1 ]; then pc_fail stop_failed "process $pc_epid outlived SIGKILL"; fi
        kill -KILL "$pc_epid" 2>/dev/null
        pc_forced=1
        pc_deadline=$(($(date +%s) + 5))
      fi
      pc_nap
    done
    pc_say pid "$pc_epid"
  fi
  if [ "$1" = tmux ] || [ "$pc_elauncher" = tmux ]; then
    if command -v tmux >/dev/null 2>&1; then
      pc_tmux kill-session -t "=$pc_tmux_name" >/dev/null 2>&1
    fi
  fi
  pc_still_locked
  rm -f "$pc_ep" "$pc_pidf"
  if [ -S "$pc_sock_rel" ]; then rm -f "$pc_sock_rel"; fi
  pc_say forced "$pc_forced"
  pc_end
}

# --- SLURM -------------------------------------------------------------------------------
#
# The helper as a batch job on a compute node (see slurm/mod.rs). run/slurm.json records the
# job PitCrew submitted, as one line:
#   {"job":<id>,"name":"<job name>","submitted":<ms>,"host":"<submitted from>","cluster":"<c>"}
# (the cluster only when sbatch named one: the job is then asked about with -M <c>). A job is
# acted on only while squeue shows it under that id, with that name and this user's uid. An id
# that names any other job (reused after a cluster restart, say) is never touched.

pc_rec=run/slurm.json

# Variables that change what sbatch, squeue, scancel and sacct do: SBATCH_* override the job
# script's directives, SQUEUE_STATES and its kin filter even `squeue -j`, SCANCEL_* can make
# scancel ask or skip, SACCT_* change sacct's output. Unset, so each does what its command line
# says.
pc_slurm_env() {
  for pc_v in $(env | sed -n \
    -e 's/^\(SBATCH_[ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_]*\)=.*$/\1/p' \
    -e 's/^\(SQUEUE_[ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_]*\)=.*$/\1/p' \
    -e 's/^\(SCANCEL_[ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_]*\)=.*$/\1/p' \
    -e 's/^\(SACCT_[ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_]*\)=.*$/\1/p'); do
    unset "$pc_v"
  done
}

# pc_on TOOL ARGS...: runs a SLURM tool on the recorded job's cluster, if sbatch named one.
pc_on() {
  pc_t=$1
  shift
  if [ -n "$pc_rcl" ]; then "$pc_t" -M "$pc_rcl" "$@"; else "$pc_t" "$@"; fi
}

# Reads run/slurm.json into pc_rjob, pc_rname, pc_rsub and pc_rcl. Fails when there is none, or
# it is not in the form pc_slurm_submit writes.
pc_job_recorded() {
  pc_rjob= pc_rname= pc_rsub= pc_rcl=
  if [ ! -f "$pc_rec" ] || [ -L "$pc_rec" ]; then return 1; fi
  pc_rline=$(head -n 1 "$pc_rec" 2>/dev/null)
  pc_fields=$(printf '%s\n' "$pc_rline" | sed -n 's/^{"job":\([0123456789][0123456789]*\),"name":"\([0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._+-]*\)","submitted":\([0123456789]*\),.*$/\1 \2 \3/p')
  if [ -z "$pc_fields" ]; then return 1; fi
  pc_rjob=${pc_fields%% *}
  pc_fields=${pc_fields#* }
  pc_rname=${pc_fields%% *}
  pc_rsub=${pc_fields#* }
  pc_rcl=$(printf '%s\n' "$pc_rline" | sed -n 's/^.*,"cluster":"\([0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._-]*\)"}$/\1/p')
  [ -n "$pc_rname" ]
}

# pc_terminal STATE: whether a job in STATE has finished for good.
pc_terminal() {
  case $1 in
    BOOT_FAIL|CANCELLED|COMPLETED|DEADLINE|FAILED|NODE_FAIL) return 0 ;;
    OUT_OF_MEMORY|PREEMPTED|REVOKED|SPECIAL_EXIT|TIMEOUT) return 0 ;;
  esac
  return 1
}

# pc_queue ID: what squeue says about job ID, in pc_q:
#   ours     it is listed under the recorded name and this user's uid; pc_qstate, pc_qreason,
#            pc_qleft, pc_qlimit and pc_qnode say how it is;
#   foreign  the id names another job: pc_qowner and pc_qname say whose;
#   gone     squeue no longer knows it;
#   error    squeue failed (pc_qerr): the scheduler may be unreachable, so nothing is concluded.
pc_queue() {
  pc_qout=$(pc_on squeue -h -j "$1" -o '%i|%U|%T|%r|%L|%l|%N|%j' 2>&1)
  pc_qrc=$?
  pc_qerr= pc_qowner= pc_qstate= pc_qreason= pc_qleft= pc_qlimit= pc_qnode= pc_qname=
  # The job's own line. Its name comes last, since it may hold anything.
  pc_qline=$(printf '%s\n' "$pc_qout" | awk -F '|' -v id="$1" '$1 == id')
  if [ -z "$pc_qline" ]; then
    # squeue ran but does not list it, or says the id is unknown (purged after it ended).
    if [ "$pc_qrc" -eq 0 ]; then pc_q=gone; return; fi
    case $pc_qout in *'Invalid job id'*) pc_q=gone; return ;; esac
    pc_q=error
    pc_qerr=$(pc_flat "squeue exited with $pc_qrc: $pc_qout")
    return
  fi
  pc_q=error
  # Every | in a pattern is quoted: a bare one separates case patterns, ksh93 reads one in
  # ${...} as alternation, and mksh mishandles [|] there.
  case $pc_qline in
    *'
'*) pc_qerr="squeue listed job $1 more than once"; return ;;
    *'|'*'|'*'|'*'|'*'|'*'|'*'|'*) ;;
    *) pc_qerr=$(pc_flat "squeue printed: $pc_qline"); return ;;
  esac
  pc_rest=${pc_qline#*"|"}
  pc_qowner=${pc_rest%%"|"*}
  pc_rest=${pc_rest#*"|"}
  pc_qstate=${pc_rest%%"|"*}
  pc_rest=${pc_rest#*"|"}
  pc_qreason=${pc_rest%%"|"*}
  pc_rest=${pc_rest#*"|"}
  pc_qleft=${pc_rest%%"|"*}
  pc_rest=${pc_rest#*"|"}
  pc_qlimit=${pc_rest%%"|"*}
  pc_rest=${pc_rest#*"|"}
  pc_qnode=${pc_rest%%"|"*}
  pc_qname=${pc_rest#*"|"}
  case $pc_qowner in
    ''|*[!0123456789]*) pc_qerr=$(pc_flat "squeue printed: $pc_qline"); return ;;
  esac
  case $pc_qstate in
    ''|*[!ABCDEFGHIJKLMNOPQRSTUVWXYZ_]*) pc_qerr=$(pc_flat "squeue printed: $pc_qline"); return ;;
  esac
  if [ "$pc_qowner" = "$pc_me" ] && [ "$pc_qname" = "$pc_rname" ]; then
    pc_q=ours
  else
    pc_q=foreign
  fi
}

# pc_acct ID: how the recorded job ID ended, from sacct where there is one: pc_astate (e.g.
# FAILED, CANCELLED by 1000) and pc_aexit (code:signal). sacct itself keeps only records with
# the recorded name and this user's uid, and prints no names, which could carry a forged line;
# with several (an id reused), the last one counts.
pc_acct() {
  pc_astate= pc_aexit=
  command -v sacct >/dev/null 2>&1 || return 1
  pc_aout=$(pc_on sacct -n -X -P -j "$1" --name="$pc_rname" -u "$pc_me" \
    -o JobID,State,ExitCode 2>/dev/null) || return 1
  pc_aline=$(printf '%s\n' "$pc_aout" | awk -F '|' -v id="$1" 'NF == 3 && $1 == id { last = $0 } END { print last }')
  if [ -z "$pc_aline" ]; then return 1; fi
  pc_astate=$(printf '%s\n' "$pc_aline" | cut -d '|' -f 2)
  pc_aexit=$(printf '%s\n' "$pc_aline" | cut -d '|' -f 3)
}

# pc_endpoint_of JOB: whether endpoint.json was written by job JOB (see pc_recorded).
pc_endpoint_of() {
  pc_recorded || return 1
  [ "$pc_elauncher" = slurm ] || return 1
  case $pc_line in *',"job":'"$1"'}') return 0 ;; esac
  return 1
}

# Reports the recorded job: what squeue says (pc_queue must have run), and once it has ended,
# what sacct says and the end of its output.
pc_job_report() {
  pc_say job "$pc_rjob"
  pc_say name "$pc_rname"
  pc_say submitted "$pc_rsub"
  pc_say cluster "$pc_rcl"
  pc_say queue "$pc_q"
  case $pc_q in
    ours)
      pc_say state "$pc_qstate"
      pc_say reason "$(pc_flat "$pc_qreason")"
      pc_say left "$(pc_flat "$pc_qleft")"
      pc_say limit "$(pc_flat "$pc_qlimit")"
      pc_say node "$(pc_flat "$pc_qnode")"
      ;;
    foreign)
      pc_say owner "$pc_qowner"
      pc_say other_name "$(pc_flat "$pc_qname")"
      ;;
    error) pc_say squeue_error "$pc_qerr" ;;
  esac
  if [ "$pc_q" = gone ] || { [ "$pc_q" = ours ] && pc_terminal "$pc_qstate"; }; then
    if pc_acct "$pc_rjob"; then
      pc_say acct_state "$(pc_flat "$pc_astate")"
      pc_say acct_exit "$(pc_flat "$pc_aexit")"
    fi
    if [ -f "run/slurm-$pc_rjob.out" ]; then
      pc_say output "$(tail -n 5 "run/slurm-$pc_rjob.out" 2>/dev/null | tail -c 400 | tr '\r\n\t' '   ')"
    fi
  fi
  if pc_endpoint_of "$pc_rjob"; then pc_say endpoint "$pc_line"; fi
}

# pc_forget JOB: removes the record of job JOB, and its endpoint and socket in the root.
pc_forget() {
  pc_still_locked
  if pc_endpoint_of "$1"; then
    rm -f "$pc_ep"
    case $pc_line in
      *'"socket":"'"$pc_root"'/run/pitcrewd.sock",'*)
        if [ -S run/pitcrewd.sock ]; then rm -f run/pitcrewd.sock; fi ;;
    esac
  fi
  rm -f "$pc_rec"
}

pc_need() {
  for pc_t in "$@"; do
    command -v "$pc_t" >/dev/null 2>&1 || pc_fail no_slurm "$pc_t is not on the PATH"
  done
}

# pc_slurm_wait SECONDS: waits up to SECONDS for the recorded job to run and write its
# endpoint, or to end, polling squeue every 2 seconds; then reports it.
pc_slurm_wait() {
  pc_deadline=$(($(date +%s) + $1))
  while :; do
    pc_queue "$pc_rjob"
    if [ "$pc_q" != ours ] || pc_terminal "$pc_qstate"; then break; fi
    if [ "$pc_qstate" = RUNNING ] && pc_endpoint_of "$pc_rjob"; then break; fi
    if [ "$(date +%s)" -ge "$pc_deadline" ]; then break; fi
    sleep 2
  done
  pc_job_report
  pc_end
}

# pc_slurm_submit WAIT MINUTES SIZE SHA256 NAME READY: submits the job script that follows on
# stdin (SIZE bytes with that sha256: exactly the one the user saw) as job NAME, unless the
# recorded job is still queued; then waits up to READY seconds for it to run, and reports it.
pc_slurm_submit() {
  pc_size=$3 pc_want=$4 pc_name=$5 pc_ready=$6
  case $pc_name in
    ''|-*|*[!0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._+-]*)
      pc_fail usage "the job name must be plain characters" ;;
  esac
  # The root goes into sbatch options, where % is a pattern: plain characters only.
  case $pc_root in
    *[!0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._/+-]*)
      pc_fail usage "the root must be plain characters for the SLURM launcher" ;;
  esac
  pc_enter 1
  pc_private run
  pc_lock run/.lock "$1" "$2"
  pc_sweep_aside run/.lock
  pc_slurm_env
  # What a submit that was killed left behind: nothing else writes these but under the lock.
  pc_still_locked
  for pc_f in run/job.*.sh run/sbatch.err.* "$pc_rec".tmp.*; do
    if [ -e "$pc_f" ] || [ -L "$pc_f" ]; then rm -f "$pc_f"; fi
  done
  # The job script comes first, whatever happens next: it is used whole or not at all.
  pc_tmp=run/job.$pc_tag.sh
  if pc_err=$( (set -C; cat > "$pc_tmp") 2>&1 ); then :; else
    pc_fail io "cannot write $(pc_where "$pc_tmp"): $pc_err"
  fi
  pc_got=$(wc -c < "$pc_tmp" | tr -d ' ')
  if [ "$pc_got" != "$pc_size" ] \
    || [ "$(head -n 2 "$pc_tmp")" != "#!/bin/sh
# pitcrew-job-script-begin" ] \
    || [ "$(tail -n 1 "$pc_tmp")" != '# pitcrew-job-script-end' ]; then
    pc_fail job_script "the job script did not arrive whole ($pc_got of $pc_size bytes)"
  fi
  pc_sha256 "$pc_tmp" || pc_no_hash_tool
  if [ "$pc_sum" != "$pc_want" ]; then
    pc_fail job_script "the job script did not arrive as sent (its sha256 is $pc_sum)"
  fi
  pc_need sbatch squeue
  pc_say installed "$(readlink bin/current 2>/dev/null)"
  if pc_job_recorded; then
    pc_queue "$pc_rjob"
    case $pc_q in
      error) pc_fail slurm "$pc_qerr" ;;
      ours) if ! pc_terminal "$pc_qstate"; then pc_say started 0; pc_slurm_wait "$pc_ready"; fi ;;
    esac
    # Gone, ended, or someone else's job now: forgotten, never touched.
    pc_forget "$pc_rjob"
  fi
  # The other launchers share run/ and its socket: not while one of their helpers runs, here
  # or on another host sharing this home. A record of one that is gone is left over.
  if pc_recorded && [ "$pc_elauncher" != slurm ]; then
    pc_takeover=0
    pc_state
    case $? in
      0) pc_fail in_use "the $pc_elauncher launcher's helper runs here (pid $pc_epid); stop it first" ;;
      2)
        pc_say host "$pc_ehost"
        pc_fail other_host "the $pc_elauncher launcher's helper is recorded on $pc_ehost"
        ;;
    esac
    pc_still_locked
    rm -f "$pc_ep" "$pc_pidf"
    if [ -S run/pitcrewd.sock ]; then rm -f run/pitcrewd.sock; fi
  fi
  pc_ver=$(readlink bin/current 2>/dev/null)
  case $pc_ver in
    [0123456789]*) ;;
    *) pc_fail not_deployed "$(pc_where bin/current)" ;;
  esac
  case $pc_ver in
    *[!0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._+-]*)
      pc_fail not_deployed "$(pc_where bin/current)" ;;
  esac
  if [ ! -f "bin/$pc_ver/pitcrewd" ] || [ ! -x "bin/$pc_ver/pitcrewd" ]; then
    pc_fail not_deployed "$(pc_where "bin/$pc_ver/pitcrewd")"
  fi
  # The name, directory and output again on the command line, where nothing overrides them.
  # Under umask 077, so the job's output file is private; the job gets the user's umask as
  # its argument, for the helper.
  pc_still_locked
  pc_tmp2=run/sbatch.err.$pc_tag
  pc_out=$(sbatch --parsable --job-name="$pc_name" --chdir="$pc_root" \
    --output="$pc_root/run/slurm-%j.out" "$pc_tmp" "$pc_umask" 2>"$pc_tmp2")
  pc_rc=$?
  # On stdout, `<id>` or `<id>;<cluster>`; warnings go to stderr.
  pc_jid=$(printf '%s\n' "$pc_out" | sed -n \
    -e '/^[0123456789][0123456789]*$/p' \
    -e '/^[0123456789][0123456789]*;[0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._-][0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._-]*$/p' \
    | tail -n 1)
  if [ "$pc_rc" -ne 0 ] || [ -z "$pc_jid" ]; then
    pc_fail submit_failed "$(tail -n 3 "$pc_tmp2" 2>/dev/null) $(printf '%s\n' "$pc_out" | tail -n 1)"
  fi
  pc_rcl=
  case $pc_jid in
    *';'*)
      pc_rcl=${pc_jid#*";"}
      pc_jid=${pc_jid%%";"*}
      ;;
  esac
  # Recorded at once: the job waits for this record and ends on its own without it.
  pc_now=$(date +%s)
  case $pc_now in ''|*[!0123456789]*) pc_fail io "date +%s printed '$pc_now'" ;; esac
  pc_rsub=${pc_now}000
  pc_line=$(printf '{"job":%s,"name":"%s","submitted":%s,"host":"%s"' \
    "$pc_jid" "$pc_name" "$pc_rsub" "$pc_host")
  if [ -n "$pc_rcl" ]; then pc_line=$pc_line$(printf ',"cluster":"%s"' "$pc_rcl"); fi
  pc_line=$pc_line'}'
  pc_still_locked
  if printf '%s\n' "$pc_line" > "$pc_rec.tmp.$pc_tag" && mv -f "$pc_rec.tmp.$pc_tag" "$pc_rec"; then :; else
    rm -f "$pc_rec.tmp.$pc_tag"
    pc_fail io "cannot write $(pc_where "$pc_rec")"
  fi
  rm -f "$pc_tmp" "$pc_tmp2"
  pc_tmp= pc_tmp2=
  pc_rjob=$pc_jid pc_rname=$pc_name
  pc_say started 1
  pc_slurm_wait "$pc_ready"
}

# pc_slurm_status: what is installed, and what the recorded job is doing. Takes no lock and
# changes nothing.
pc_slurm_status() {
  pc_slurm_env
  if pc_enter 0; then
    if [ -e run ] || [ -L run ]; then pc_private run; fi
    pc_say installed "$(readlink bin/current 2>/dev/null)"
    if pc_job_recorded; then
      pc_need squeue
      pc_queue "$pc_rjob"
      pc_job_report
    fi
  else
    pc_say installed ''
  fi
  pc_end
}

# pc_slurm_stop WAIT MINUTES STOP: cancels the recorded job if it is ours and still queued,
# waits up to STOP seconds for it to leave the queue, reports how it ended and forgets it.
pc_slurm_stop() {
  if ! pc_enter 0; then pc_end; fi
  pc_private run
  pc_lock run/.lock "$1" "$2"
  pc_sweep_aside run/.lock
  if ! pc_job_recorded; then pc_end; fi
  pc_slurm_env
  pc_need squeue scancel
  pc_queue "$pc_rjob"
  if [ "$pc_q" = error ]; then pc_fail slurm "$pc_qerr"; fi
  if [ "$pc_q" = ours ] && ! pc_terminal "$pc_qstate"; then
    if pc_endpoint_of "$pc_rjob"; then pc_say pid "$pc_epid"; fi
    pc_still_locked
    # The name and owner again, for scancel itself to check.
    pc_cout=$(pc_on scancel --user="$pc_me" --name="$pc_rname" "$pc_rjob" 2>&1)
    pc_crc=$?
    if [ "$pc_crc" -eq 0 ]; then pc_say cancelled 1; else pc_say scancel_error "$(pc_flat "$pc_cout")"; fi
    pc_deadline=$(($(date +%s) + $3))
    while :; do
      pc_queue "$pc_rjob"
      if [ "$pc_q" = error ]; then pc_fail slurm "$pc_qerr"; fi
      if [ "$pc_q" != ours ] || pc_terminal "$pc_qstate"; then break; fi
      if [ "$(date +%s)" -ge "$pc_deadline" ]; then
        if [ "$pc_crc" -ne 0 ]; then
          pc_fail slurm "scancel $pc_rjob failed: $(pc_flat "$pc_cout")"
        fi
        pc_fail stop_failed "job $pc_rjob is still $pc_qstate after ${3}s"
      fi
      sleep 1
    done
  fi
  pc_job_report
  pc_forget "$pc_rjob"
  pc_end
}

case $pc_cmd in
  check|install) pc_deploy "$@" ;;
  start) pc_start "$@" ;;
  status) pc_status "$@" ;;
  stop) pc_stop "$@" ;;
  slurm-submit) pc_slurm_submit "$@" ;;
  slurm-status) pc_slurm_status "$@" ;;
  slurm-stop) pc_slurm_stop "$@" ;;
esac
pc_fail usage "unknown command: $pc_cmd"
# pitcrew-helper-script-end
