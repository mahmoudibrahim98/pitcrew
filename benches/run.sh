#!/usr/bin/env bash
# Runs the budget benchmarks, writes a JSON summary and checks it against benches/baseline.json.
#
#   benches/run.sh [--quick | --full] [--out FILE] [--threshold FRACTION] [--retries N]
#                  [--write-baseline [--note TEXT]]
#
#   --quick           CI mode: 20 MiB transcripts only, few short samples.
#   --full            Local mode (the default): 20 and 200 MiB transcripts, more samples.
#   --out FILE        Where to write the summary (default: <target>/pitcrew-bench/summary.json).
#   --threshold F     Fail when a metric is worse than the baseline by more than F (default 0.10).
#   --retries N       Run a failing benchmark up to N more times before failing (default 2).
#   --write-baseline  Record this run as benches/baseline.json instead of comparing.
#   --note TEXT       With --write-baseline: what to record about the machine and conditions.
#
# Exits non-zero when a metric regressed by more than the threshold, is over its budget, or did
# not run, in every attempt. Generated inputs live in the system temp dir and are deleted as each
# benchmark finishes. Criterion's results go to <target>/pitcrew-bench/, cleared on each run.
set -euo pipefail

mode=full
report=()
out=""
retries=2
write=0
while [ $# -gt 0 ]; do
  case "$1" in
    --quick) mode=quick; shift ;;
    --full) mode=full; shift ;;
    --out) out=$2; shift 2 ;;
    --threshold) report+=(--threshold "$2"); shift 2 ;;
    --retries) retries=$2; shift 2 ;;
    --write-baseline) write=1; report+=(--write-baseline --recorded "$(date -u +%Y-%m-%d)"); shift ;;
    --note) report+=(--note "$2"); shift 2 ;;
    -h | --help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
target=${CARGO_TARGET_DIR:-$root/target}
work="$target/pitcrew-bench"
out=${out:-$work/summary.json}
rm -rf "$work"/criterion "$work"/criterion-retry-* "$work/retry-filter"
mkdir -p "$work"

bench() { # DIR [FILTER]
  CRITERION_HOME=$1 PITCREW_BENCH_MODE=$mode \
    cargo bench --locked -p pitcrew-benches --benches -- --noplot ${2:+"$2"}
}

bench "$work/criterion"
dirs=(--criterion "$work/criterion")
attempt=0
while :; do
  status=0
  # The bench profile, so the report reuses the dependencies the benchmarks just built.
  cargo run --locked --profile bench -q -p pitcrew-benches --bin pitcrew-bench-report -- \
    "${dirs[@]}" \
    --baseline "$root/benches/baseline.json" \
    --mode "$mode" \
    --out "$out" \
    --retry-filter "$work/retry-filter" \
    ${report[@]+"${report[@]}"} || status=$?
  # Noise on a busy machine can fail one attempt; a real regression fails every one.
  if [ "$status" -ne 1 ] || [ "$write" = 1 ] || [ "$attempt" -ge "$retries" ] ||
    [ ! -s "$work/retry-filter" ]; then
    break
  fi
  attempt=$((attempt + 1))
  filter=$(cat "$work/retry-filter")
  echo "retry $attempt of $retries: $filter"
  bench "$work/criterion-retry-$attempt" "$filter"
  dirs+=(--criterion "$work/criterion-retry-$attempt")
done
echo "summary: $out"
exit "$status"
