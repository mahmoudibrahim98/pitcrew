#!/usr/bin/env bash
# Opt-in remaining budgets. Run one stage, or all and produce the standard JSON budget report.
#   benches/more.sh [cpu|verbs|hook-cli|hook-live|more] [scale-tool options...]
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
stage=${1:-more}
if [ "$#" -gt 0 ]; then shift; fi
case "$stage" in cpu|verbs|hook-cli|hook-live|more) ;; *) echo "unknown stage: $stage" >&2; exit 2 ;; esac
cargo build --profile bench --locked -p pitcrew-daemon -p pitcrew-cli -p pitcrew-benches --bins
work=target/pitcrew-more
mkdir -p "$work"
# Record stdout only: tokens stay in child environments, never in command arguments/logs.
target/release/pitcrew-bench-scale "$stage" --probes 200 "$@" | tee "$work/$stage.log"
if [ "$stage" = more ]; then
  target/release/pitcrew-bench-report --only-more --tests "$work/$stage.log" \
    --out "$work/summary.json"
fi
