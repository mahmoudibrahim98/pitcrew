#!/usr/bin/env bash
# Runs the budget benchmarks, writes a JSON summary and checks it against benches/baseline.json.
#
#   benches/run.sh [--quick | --full] [--write-baseline] [--out FILE] [--threshold FRACTION]
#
#   --quick           CI mode: 20 MiB transcripts only, few short samples.
#   --full            Local mode (the default): 20 and 200 MiB transcripts, more samples.
#   --write-baseline  Record this run as benches/baseline.json instead of comparing.
#   --out FILE        Where to write the summary (default: <target>/pitcrew-bench/summary.json).
#
# Exits non-zero when a metric regressed by more than the threshold (10%), is over its budget,
# or did not run. Generated inputs live in the system temp dir and are deleted as each benchmark
# finishes. Criterion's own results go to <target>/pitcrew-bench/criterion, cleared on each run.
set -euo pipefail

mode=full
write_baseline=()
out=""
threshold=()
while [ $# -gt 0 ]; do
  case "$1" in
    --quick) mode=quick; shift ;;
    --full) mode=full; shift ;;
    --write-baseline) write_baseline=(--write-baseline --recorded "$(date -u +%Y-%m-%d)"); shift ;;
    --out) out=$2; shift 2 ;;
    --threshold) threshold=(--threshold "$2"); shift 2 ;;
    -h | --help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
target=${CARGO_TARGET_DIR:-$root/target}
work="$target/pitcrew-bench"
out=${out:-$work/summary.json}

export CRITERION_HOME="$work/criterion"
rm -rf "$CRITERION_HOME"
mkdir -p "$CRITERION_HOME"

PITCREW_BENCH_MODE=$mode cargo bench --locked -p pitcrew-benches --benches -- --noplot
# The bench profile, so the report reuses the dependencies the benchmarks just built.
cargo run --locked --profile bench -q -p pitcrew-benches --bin pitcrew-bench-report -- \
  --criterion "$CRITERION_HOME" \
  --baseline "$root/benches/baseline.json" \
  --mode "$mode" \
  --out "$out" \
  ${threshold[@]+"${threshold[@]}"} ${write_baseline[@]+"${write_baseline[@]}"}
echo "summary: $out"
