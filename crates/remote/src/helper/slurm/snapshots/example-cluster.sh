#!/bin/sh
# pitcrew-job-script-begin
# PitCrew's helper (pitcrewd) as a SLURM batch job, from the site recipe "example-cluster".
# PitCrew submits exactly this script. The job name, working directory and output
# are given on sbatch's command line too, where nothing overrides them.
#SBATCH --job-name=pitcrew-helper-016f101f
#SBATCH --chdir=/home/someone/.pitcrew
#SBATCH --output=/home/someone/.pitcrew/run/slurm-%j.out
#SBATCH --partition=gpu
#SBATCH --account=proj0001
#SBATCH --qos=normal
#SBATCH --time=04:00:00
#SBATCH --cpus-per-task=2
#SBATCH --mem=8G
#SBATCH --gres=gpu:1
#SBATCH --constraint=a100
#SBATCH --nodes=1

pc_root=/home/someone/.pitcrew
pc_tool_path=/usr/bin:/bin:/usr/sbin:/sbin
pc_socket=node-local
pc_wait=60
pc_modules_init=/etc/profile.d/modules.sh
pc_modules='example-toolchain/1.0 nodejs/22'

# The rest is fixed text (crates/remote, src/helper/slurm/job.sh). SLURM runs it on the compute
# node as `sh <script> <umask>`, in the root, with its output in run/slurm-<job>.out. It:
# 1. checks the way to the root and the root itself, as the other launchers do, and moves in;
# 2. waits until run/slurm.json records this job (PitCrew writes it as soon as sbatch answers),
#    and ends otherwise, so a job whose submission was cut off does not linger;
# 3. loads the recipe's modules, starts bin/<current version>/pitcrewd on its socket, waits for
#    the socket, and writes run/endpoint.json with this node's name and the job id;
# 4. waits for the helper, passes SIGTERM on to it (scancel, the time limit), and removes the
#    endpoint and the socket on the way out.

# Functions imported from the environment (bash exports them) must not stand in for tools.
unset -f awk cat cd chmod command date head hostname id kill ls mkdir mv printf ps pwd \
  readlink rm rmdir sed sleep test tr umask uname wc 2>/dev/null
unset IFS ENV BASH_ENV CDPATH
PATH=$pc_tool_path${PATH:+:$PATH}
export PATH
# The user's umask, for the helper. Everything the script makes is private.
pc_umask=$1
case $pc_umask in ''|*[!01234567]*) pc_umask=077 ;; esac
umask 077
pc_pid= pc_sock= pc_sdir=

pc_fail() {
  printf 'pitcrew: %s: %s\n' "$1" "$2" >&2
  exit 1
}
pc_nap() { sleep 0.2 2>/dev/null || sleep 1; }

# These five are the same text as in helper.sh (a test checks it).

# A path as the user knows it: relative paths are inside the root.
pc_where() {
  case $1 in
    /*) printf '%s' "$1" ;;
    .) printf '%s' "$pc_root" ;;
    *) printf '%s/%s' "$pc_root" "$1" ;;
  esac
}

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

# pc_dir_ok DIR: DIR, on the way to the root, is a directory owned by root or this user, and
# writable by the group or others only if sticky (as /tmp). Anyone else could rename what is
# under it after the checks.
pc_dir_ok() {
  pc_ls=$(ls -ldn "$1" 2>/dev/null | awk '{print $1, $3}')
  pc_m=${pc_ls%% *} pc_u=${pc_ls#* }
  case $pc_m in
    d*) ;;
    *) pc_fail unsafe_dir "$1, on the way to $pc_root, is not a directory" ;;
  esac
  case $pc_u in
    0|"$pc_me") ;;
    *) pc_fail unsafe_dir "$1, on the way to $pc_root, belongs to uid $pc_u, who could replace what is under it" ;;
  esac
  case $pc_m in
    ?????w*|????????w*)
      case $pc_m in
        ?????????[tT]*) ;;
        *) pc_fail unsafe_dir "$1, on the way to $pc_root, is writable by others ($pc_m)" ;;
      esac
      ;;
  esac
}

# pc_safe_way PATH: resolves PATH's parent one component at a time, as the kernel does, and
# checks every directory it goes through, from / down. A symbolic link is followed, and the way
# to where it points is checked the same way; the directory holding the link already was.
pc_safe_way() {
  pc_rest=${1%/*} pc_at= pc_hops=0
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
        pc_fail unsafe_dir "too many symbolic links on the way to $pc_root"
      fi
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

# Only what this job made: the endpoint while it names this job, the socket, its directory.
pc_cleanup() {
  if [ -n "$pc_pid" ] && pc_alive "$pc_pid"; then kill -TERM "$pc_pid" 2>/dev/null; fi
  case $(head -n 1 run/endpoint.json 2>/dev/null) in
    *',"job":'"$pc_job"'}') rm -f run/endpoint.json ;;
  esac
  if [ -n "$pc_sock" ] && [ -S "$pc_sock" ]; then rm -f "$pc_sock"; fi
  if [ -n "$pc_sdir" ]; then rmdir "$pc_sdir" 2>/dev/null; fi
}

pc_job=${SLURM_JOB_ID:-}
case $pc_job in ''|*[!0123456789]*) pc_fail usage "not run by SLURM (no SLURM_JOB_ID)" ;; esac
pc_me=$(id -u 2>/dev/null)
case $pc_me in ''|*[!0123456789]*) pc_fail io "id -u failed" ;; esac

# 1. The way to the root, the root, and the version to start.
pc_safe_way "$pc_root"
if [ ! -d "$pc_root" ]; then pc_fail not_deployed "$pc_root does not exist"; fi
pc_private "$pc_root"
cd -P "$pc_root" 2>/dev/null || pc_fail io "cannot enter $pc_root"
pc_private .
pc_phys=$(pwd -P)
pc_private run
chmod 600 "run/slurm-$pc_job.out" 2>/dev/null
trap pc_cleanup EXIT
pc_private bin
pc_ver=$(readlink bin/current 2>/dev/null)
case $pc_ver in
  [0123456789]*) ;;
  *) pc_fail not_deployed "$(pc_where bin/current)" ;;
esac
case $pc_ver in
  *[!0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._+-]*)
    pc_fail not_deployed "$(pc_where bin/current)" ;;
esac
pc_private "bin/$pc_ver"
pc_exe=bin/$pc_ver/pitcrewd
if [ -L "$pc_exe" ] || [ ! -f "$pc_exe" ] || [ ! -x "$pc_exe" ]; then
  pc_fail not_deployed "$(pc_where "$pc_exe")"
fi

# 2. PitCrew's record of this job.
pc_deadline=$(($(date +%s) + pc_wait))
while :; do
  case $(head -n 1 run/slurm.json 2>/dev/null) in
    '{"job":'"$pc_job"',"name":'*) break ;;
    '{"job":'*) pc_fail not_recorded "run/slurm.json records another job, not this one ($pc_job)" ;;
  esac
  if [ "$(date +%s)" -ge "$pc_deadline" ]; then
    pc_fail not_recorded "run/slurm.json does not record this job ($pc_job) after ${pc_wait}s"
  fi
  sleep 1
done

# 3. This node's name as the login node reaches it: SLURM's name for it, else its own.
pc_name_ok() {
  case $1 in
    ''|-*|*[!0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._-]*) return 1 ;;
  esac
}
pc_node=${SLURMD_NODENAME:-}
pc_name_ok "$pc_node" || pc_node=$(hostname -f 2>/dev/null)
pc_name_ok "$pc_node" || pc_node=$(uname -n 2>/dev/null)
pc_name_ok "$pc_node" || pc_fail host "this node has no name of plain characters to be reached by"

# The socket: in run/, or on the node's own disk ($TMPDIR, else /tmp).
case $pc_socket in
  node-local)
    pc_tmp=${TMPDIR:-/tmp}
    case $pc_tmp in
      /?*) ;;
      *) pc_tmp=/tmp ;;
    esac
    case $pc_tmp in
      *[!0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz._/+-]*) pc_tmp=/tmp ;;
    esac
    pc_sdir=${pc_tmp%/}/pitcrew-$pc_job.$$
    pc_safe_way "$pc_sdir"
    pc_private "$pc_sdir"
    pc_sock=$pc_sdir/pitcrewd.sock
    ;;
  *) pc_sock=$pc_root/run/pitcrewd.sock ;;
esac
if [ "${#pc_sock}" -gt 100 ]; then
  pc_fail usage "the socket path $pc_sock is longer than 100 bytes"
fi
if [ -S "$pc_sock" ]; then rm -f "$pc_sock"; fi

# The recipe's modules. A set-up script may change the shell's settings: they are reset after.
if [ -n "$pc_modules_init" ]; then
  if [ ! -f "$pc_modules_init" ]; then pc_fail modules "there is no $pc_modules_init"; fi
  . "$pc_modules_init"
  set +eu
  unset IFS
fi
if [ -n "$pc_modules" ]; then
  command -v module >/dev/null 2>&1 \
    || pc_fail modules "there is no module command (a site recipe can name the script that sets it up, as modules_init)"
  set -f
  for pc_m in $pc_modules; do
    module load "$pc_m" || pc_fail modules "module load $pc_m failed"
  done
  set +f
fi
# Paths from here on are relative to the root, entered once above (never again by name).
if [ "$(pwd -P)" != "$pc_phys" ]; then
  pc_fail modules "setting up the modules changed the working directory"
fi
# The helper gets the PATH the modules made; this script keeps its tools first.
pc_env_path=$PATH
PATH=$pc_tool_path:$PATH

umask "$pc_umask"
PATH=$pc_env_path "$pc_exe" serve --listen "unix:$pc_sock" </dev/null &
pc_pid=$!
umask 077
trap 'kill -TERM "$pc_pid" 2>/dev/null' TERM INT HUP
pc_started=$(date +%s)
case $pc_started in ''|*[!0123456789]*) pc_fail io "date +%s printed '$pc_started'" ;; esac
pc_deadline=$((pc_started + pc_wait))
while :; do
  if ! pc_alive "$pc_pid"; then
    wait "$pc_pid"
    pc_fail start_failed "the helper exited at once (exit $?)"
  fi
  if [ -S "$pc_sock" ]; then break; fi
  if [ "$(date +%s)" -ge "$pc_deadline" ]; then
    pc_fail start_failed "no socket at $pc_sock after ${pc_wait}s; it was stopped"
  fi
  pc_nap
done
pc_line=$(printf '{"pid":%s,"host":"%s","version":"%s","started":%s000,"launcher":"slurm","socket":"%s","job":%s}' \
  "$pc_pid" "$pc_node" "$pc_ver" "$pc_started" "$pc_sock" "$pc_job")
if printf '%s\n' "$pc_line" > "run/endpoint.json.tmp.$pc_job" \
  && mv -f "run/endpoint.json.tmp.$pc_job" run/endpoint.json; then :; else
  rm -f "run/endpoint.json.tmp.$pc_job"
  pc_fail io "cannot write $(pc_where run/endpoint.json)"
fi
printf 'pitcrew: helper %s runs on %s as pid %s; socket %s\n' "$pc_ver" "$pc_node" "$pc_pid" "$pc_sock"

# 4. Until the helper ends. A trapped signal interrupts wait; 127 means it was already reaped.
while :; do
  wait "$pc_pid"
  pc_rc=$?
  if [ "$pc_rc" -eq 127 ] || ! pc_alive "$pc_pid"; then break; fi
done
printf 'pitcrew: the helper exited (%s)\n' "$pc_rc"
exit "$pc_rc"
# pitcrew-job-script-end
