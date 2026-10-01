# PitCrew's side of deploying and starting the helper (pitcrewd) on a machine.
#
# It arrives on stdin and runs as `sh -c BOOTSTRAP sh <length> <command> <tag> <root> [args]`
# (see script.rs): the bootstrap reads exactly <length> bytes with dd, so what follows on stdin
# (the helper itself, for `install`) is left for this script.
#
# It prints a report between @@pitcrew-helper-begin-<tag> and @@pitcrew-helper-end-<tag>:
# key=value lines, and on failure error=<code> and detail=<text>. Whichever way it exits, it
# releases its lock and removes its temporary file.
#
# Needs a POSIX sh and dd, cat, ls, awk, sed, tr, cut, head, tail, wc, mkdir, rm, mv, ln, chmod,
# id, uname, date (+%s), find (-mmin), readlink and sleep; one of sha256sum, shasum or openssl to
# deploy; setsid or nohup (where they exist), or tmux, to start the helper; ps where there is no
# /proc.
#
# Character sets are spelled out rather than written as ranges, which depend on the locale.

pc_cmd=$1 pc_tag=$2 pc_root=$3
shift 3
umask 077
unset IFS TMUX
pc_bin=$pc_root/bin
pc_run=$pc_root/run
pc_ep=$pc_run/endpoint.json
pc_pidf=$pc_run/pitcrewd.pid
pc_log=$pc_run/pitcrewd.log
pc_held=
pc_tmp=

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

# Whether this run holds the lock directory $1.
pc_owns() { [ "$(cat "$1/owner" 2>/dev/null)" = "$pc_host $$ $pc_tag" ]; }

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

pc_cleanup() {
  if [ -n "$pc_tmp" ]; then rm -f "$pc_tmp"; fi
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
pc_host=$(uname -n 2>/dev/null)
case $pc_host in
  ''|*[!0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._-]*) pc_host=unknown ;;
esac

# pc_private DIR: creates DIR if missing (0700, from the umask), then fails unless it is a real
# directory owned by this user with no access for anyone else. One that is not is refused, never
# repaired: what it holds may have been changed while it was open.
pc_private() {
  if [ ! -e "$1" ] && [ ! -L "$1" ]; then
    mkdir "$1" 2>/dev/null || [ -d "$1" ] || pc_fail io "cannot create $1"
  fi
  if [ -L "$1" ]; then pc_fail unsafe_dir "$1 is a symbolic link"; fi
  if [ ! -d "$1" ]; then pc_fail unsafe_dir "$1 is not a directory"; fi
  pc_ls=$(ls -ldn "$1" 2>/dev/null | awk '{print $1, $3}')
  case $pc_ls in
    "drwx------ $pc_me"|"drwx------. $pc_me"|"drwx------@ $pc_me") ;;
    *) pc_fail unsafe_dir "$1 must be owned by uid $pc_me with mode drwx------ and no ACL; it is: $pc_ls" ;;
  esac
}

# pc_stale DIR MINUTES: whether the lock DIR is older than MINUTES, or was taken on this host by
# a process that is gone.
pc_stale() {
  if [ -n "$(find "$1" -prune -mmin +"$2" 2>/dev/null)" ]; then return 0; fi
  pc_owner=$(cat "$1/owner" 2>/dev/null) || return 1
  case $pc_owner in "$pc_host "*) ;; *) return 1 ;; esac
  pc_opid=${pc_owner#"$pc_host "}
  pc_opid=${pc_opid%% *}
  case $pc_opid in ''|*[!0123456789]*) return 1 ;; esac
  ! pc_alive "$pc_opid"
}

# pc_lock DIR WAIT MINUTES: takes the lock DIR (mkdir), waiting up to WAIT seconds for another
# holder, and breaking a stale one (see pc_stale). Moving a stale lock aside is atomic, so two
# waiters cannot both break it. A waiter that judged an old lock stale could still move aside
# a fresh one taken in between; so a holder checks it still owns its lock before each change,
# and the one that lost it stops (lock_lost) before changing anything.
pc_lock() {
  pc_waited=0
  while :; do
    if mkdir "$1" 2>/dev/null; then
      pc_held=$1
      if printf '%s %s %s\n' "$pc_host" "$$" "$pc_tag" > "$1/owner"; then return 0; fi
      rm -rf "$1"
      pc_held=
      pc_fail io "cannot write $1/owner"
    fi
    if [ -L "$1" ] || { [ -e "$1" ] && [ ! -d "$1" ]; }; then
      pc_fail unsafe_dir "$1 is not a directory"
    fi
    if [ -d "$1" ] && pc_stale "$1" "$3" && mv "$1" "$1.stale.$pc_tag.$pc_waited" 2>/dev/null; then
      rm -rf "$1.stale.$pc_tag.$pc_waited"
      continue
    fi
    if [ "$pc_waited" -ge "$2" ]; then
      if [ -d "$1" ]; then
        pc_fail busy "$1 is held by $(cut -d ' ' -f 1-2 "$1/owner" 2>/dev/null)"
      fi
      pc_fail io "cannot create $1"
    fi
    sleep 1
    pc_waited=$((pc_waited + 1))
  done
}
pc_still_locked() {
  pc_owns "$1" || pc_fail lock_lost "$1 was taken over by another run"
}

# --- Deploying ----------------------------------------------------------------------------

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

# pc_link NAME TARGET: points $pc_bin/NAME at TARGET by renaming a new link over it, which is
# atomic with GNU mv -T or BSD mv -h. With neither (busybox), the link is replaced in place.
pc_link() {
  pc_new=$pc_bin/$1.tmp.$pc_tag
  rm -f "$pc_new"
  ln -sfn "$2" "$pc_new" || pc_fail io "cannot create $pc_new"
  if mv -T "$pc_new" "$pc_bin/$1" 2>/dev/null || mv -h "$pc_new" "$pc_bin/$1" 2>/dev/null; then
    return 0
  fi
  rm -f "$pc_new"
  if [ -e "$pc_bin/$1" ] && [ ! -L "$pc_bin/$1" ]; then
    pc_fail switch_failed "$pc_bin/$1 is not a symbolic link"
  fi
  ln -sfn "$2" "$pc_bin/$1" || pc_fail switch_failed "cannot update $pc_bin/$1"
  pc_atomic=0
}

# Removes what interrupted runs left behind: nothing else can be writing under the lock.
pc_sweep() {
  pc_still_locked "$pc_bin/.lock"
  for pc_f in "$pc_bin"/*/pitcrewd.tmp.* "$pc_bin"/current.tmp.* "$pc_bin"/previous.tmp.* \
    "$pc_bin"/.lock.stale.*; do
    if [ -e "$pc_f" ] || [ -L "$pc_f" ]; then rm -rf "$pc_f"; fi
  done
}

# Removes every version directory but the ones `current` and `previous` point to.
pc_gc() {
  pc_prev=$(readlink "$pc_bin/previous" 2>/dev/null)
  for pc_d in "$pc_bin"/*; do
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
    pc_still_locked "$pc_bin/.lock"
    rm -rf "$pc_d" && pc_removed="$pc_removed $pc_n"
  done
}

# Points `current` at $pc_version (and `previous` at what it pointed to), removes old versions,
# and reports.
pc_switch() {
  pc_old=$(readlink "$pc_bin/current" 2>/dev/null)
  pc_still_locked "$pc_bin/.lock"
  if [ "$pc_old" != "$pc_version" ]; then
    pc_link current "$pc_version"
    if [ -n "$pc_old" ]; then pc_link previous "$pc_old"; fi
  fi
  pc_now=$(readlink "$pc_bin/current" 2>/dev/null)
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
  pc_say previous "$(readlink "$pc_bin/previous" 2>/dev/null)"
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
  pc_private "$pc_root"
  pc_private "$pc_bin"
  pc_lock "$pc_bin/.lock" "$4" "$5"
  pc_sweep
  pc_dir=$pc_bin/$pc_version
  pc_file=$pc_dir/pitcrewd
  if [ -e "$pc_dir" ] || [ -L "$pc_dir" ]; then pc_private "$pc_dir"; fi
  if [ -L "$pc_file" ] || { [ -e "$pc_file" ] && [ ! -f "$pc_file" ]; }; then
    pc_fail unsafe_dir "$pc_file is not a regular file"
  fi
  if [ -f "$pc_file" ]; then
    pc_sha256 "$pc_file" || pc_no_hash_tool
    if [ "$pc_sum" = "$pc_want" ]; then
      chmod 700 "$pc_file" || pc_fail io "cannot chmod $pc_file"
      pc_check_version "$pc_file"
      case $? in
        0) pc_switch ;;
        1) pc_say code "$pc_rc"; pc_fail not_runnable "$pc_vline" ;;
        *) pc_version_mismatch ;;
      esac
    fi
    # A damaged or different copy under this version's name: it is replaced.
    rm -f "$pc_file" || pc_fail io "cannot remove $pc_file"
    pc_say replaced 1
  fi
  if [ "$pc_cmd" = check ]; then
    pc_sha256 /dev/null || pc_no_hash_tool
    pc_say tool "$pc_tool"
    pc_say state absent
    pc_end
  fi
  pc_private "$pc_dir"
  pc_tmp=$pc_dir/pitcrewd.tmp.$pc_tag
  if pc_err=$( (set -C; cat > "$pc_tmp") 2>&1 ); then :; else
    pc_fail io "cannot write $pc_tmp: $pc_err"
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
  chmod 700 "$pc_tmp" || pc_fail io "cannot chmod $pc_tmp"
  pc_check_version "$pc_tmp"
  case $? in
    0) ;;
    1) rm -f "$pc_tmp"; pc_say code "$pc_rc"; pc_fail not_runnable "$pc_vline" ;;
    *) rm -f "$pc_tmp"; pc_version_mismatch ;;
  esac
  pc_still_locked "$pc_bin/.lock"
  mv -f "$pc_tmp" "$pc_file" || pc_fail io "cannot rename $pc_tmp"
  pc_tmp=
  pc_uploaded=1
  pc_switch
}

# --- Running ------------------------------------------------------------------------------

pc_tmux() { tmux -L pitcrew-helper -f /dev/null "$@"; }

# Run by the /bin/sh a launcher starts: records its pid, then becomes the helper with its output
# appended to the log. Its stdin is the launcher's: /dev/null, or under tmux the pane's terminal,
# since tmux takes a pane whose terminal nobody holds open for dead.
PC_EXEC='l=$1; shift; printf "%s\n" "$$" > "$0.tmp" && mv -f "$0.tmp" "$0" && exec "$@" >>"$l" 2>&1'

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

# pc_start LAUNCHER READY WAIT MINUTES TAKEOVER SOCKET SOCKET_JSON [ARGS...]: starts
# bin/current/pitcrewd ARGS unless the helper already runs here, waits up to READY seconds for
# SOCKET, and records endpoint.json. SOCKET_JSON is SOCKET as a JSON string.
pc_start() {
  pc_launcher=$1 pc_ready=$2 pc_takeover=$5 pc_sock=$6 pc_sockjson=$7
  pc_private "$pc_root"
  pc_private "$pc_run"
  pc_lock "$pc_run/.lock" "$3" "$4"
  shift 7
  pc_state
  case $? in
    0) pc_say started 0; pc_say endpoint "$pc_line"; pc_end ;;
    2) pc_say host "$pc_ehost"; pc_fail other_host "the helper is recorded on $pc_ehost" ;;
  esac
  pc_exe=$pc_bin/current/pitcrewd
  pc_ver=$(readlink "$pc_bin/current" 2>/dev/null)
  case $pc_ver in
    ''|*[!0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._+-]*)
      pc_fail not_deployed "$pc_bin/current" ;;
  esac
  if [ ! -f "$pc_exe" ] || [ ! -x "$pc_exe" ]; then pc_fail not_deployed "$pc_exe"; fi
  # Nothing of ours runs here, so these are left over.
  rm -f "$pc_ep" "$pc_pidf"
  if [ -S "$pc_sock" ]; then rm -f "$pc_sock"; fi
  if [ -f "$pc_log" ] && [ "$(wc -c < "$pc_log" | tr -d ' ')" -gt 1048576 ]; then
    mv -f "$pc_log" "$pc_log.old"
  fi
  case $pc_launcher in
    tmux)
      command -v tmux >/dev/null 2>&1 || pc_fail start_failed "tmux is not installed"
      pc_tmux kill-session -t =pitcrew-helper >/dev/null 2>&1
      pc_tmux new-session -d -s pitcrew-helper -- /bin/sh -c "$PC_EXEC" \
        "$pc_pidf" "$pc_log" "$pc_exe" "$@" </dev/null >/dev/null 2>&1 \
        || pc_fail start_failed "tmux new-session failed"
      ;;
    *)
      pc_detach=
      if command -v setsid >/dev/null 2>&1; then pc_detach=setsid; fi
      if command -v nohup >/dev/null 2>&1; then pc_detach="$pc_detach nohup"; fi
      # In a subshell that exits at once, so the helper is not this script's child.
      (
        trap - EXIT HUP INT PIPE TERM
        $pc_detach /bin/sh -c "$PC_EXEC" "$pc_pidf" "$pc_log" "$pc_exe" "$@" \
          </dev/null >/dev/null 2>&1 &
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
      if [ -S "$pc_sock" ]; then break; fi
    fi
    if [ "$(date +%s)" -ge "$pc_deadline" ]; then
      if [ -n "$pc_pid" ]; then kill -TERM "$pc_pid" 2>/dev/null; fi
      if [ "$pc_launcher" = tmux ]; then pc_tmux kill-session -t =pitcrew-helper >/dev/null 2>&1; fi
      pc_fail start_failed "no socket at $pc_sock after ${pc_ready}s; it was stopped"
    fi
    pc_nap
  done
  pc_line=$(printf '{"pid":%s,"host":"%s","version":"%s","started":%s000,"launcher":"%s","socket":%s}' \
    "$pc_pid" "$pc_host" "$pc_ver" "$pc_started" "$pc_launcher" "$pc_sockjson")
  if printf '%s\n' "$pc_line" > "$pc_ep.tmp.$pc_tag" && mv -f "$pc_ep.tmp.$pc_tag" "$pc_ep"; then :; else
    rm -f "$pc_ep.tmp.$pc_tag"
    pc_fail io "cannot write $pc_ep"
  fi
  pc_say started 1
  pc_say endpoint "$pc_line"
  pc_end
}

# pc_status LAUNCHER SOCKET: what is installed, and whether the recorded helper runs. Takes no
# lock and changes nothing.
pc_status() {
  pc_takeover=0
  for pc_d in "$pc_root" "$pc_run"; do
    if [ -e "$pc_d" ] || [ -L "$pc_d" ]; then pc_private "$pc_d"; fi
  done
  pc_say installed "$(readlink "$pc_bin/current" 2>/dev/null)"
  pc_state
  case $? in
    0) pc_say state running ;;
    2) pc_say state elsewhere ;;
    *) pc_say state stopped ;;
  esac
  if [ -n "$pc_line" ]; then pc_say endpoint "$pc_line"; fi
  if [ -S "$2" ]; then pc_say socket 1; else pc_say socket 0; fi
  if [ "$1" = tmux ]; then
    if command -v tmux >/dev/null 2>&1 && pc_tmux has-session -t =pitcrew-helper 2>/dev/null; then
      pc_say session 1
    else
      pc_say session 0
    fi
  fi
  pc_end
}

# pc_stop LAUNCHER WAIT MINUTES TAKEOVER STOP SOCKET: stops the recorded helper (SIGTERM, then
# SIGKILL after STOP seconds) and removes its records.
pc_stop() {
  pc_takeover=$4
  pc_private "$pc_root"
  pc_private "$pc_run"
  pc_lock "$pc_run/.lock" "$2" "$3"
  pc_state
  pc_st=$?
  if [ "$pc_st" -eq 2 ]; then
    pc_say host "$pc_ehost"
    pc_fail other_host "the helper is recorded on $pc_ehost"
  fi
  pc_forced=0
  if [ "$pc_st" -eq 0 ]; then
    kill -TERM "$pc_epid" 2>/dev/null
    pc_deadline=$(($(date +%s) + $5))
    while pc_is_helper "$pc_epid"; do
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
      pc_tmux kill-session -t =pitcrew-helper >/dev/null 2>&1
    fi
  fi
  rm -f "$pc_ep" "$pc_pidf"
  if [ -S "$6" ]; then rm -f "$6"; fi
  pc_say forced "$pc_forced"
  pc_end
}

case $pc_cmd in
  check|install) pc_deploy "$@" ;;
  start) pc_start "$@" ;;
  status) pc_status "$@" ;;
  stop) pc_stop "$@" ;;
esac
pc_fail usage "unknown command: $pc_cmd"
# pitcrew-helper-script-end
