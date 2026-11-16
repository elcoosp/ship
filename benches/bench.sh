#!/bin/bash
# bash 3.2 — Full E2E drop->merged bench against a REAL GitHub test repo.
# Usage: BENCH_REMOTE=git@github.com:you/ship-bench.git benches/bench.sh [runs]
set -euo pipefail
RUNS="${1:-3}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
if [ -z "${BENCH_REMOTE:-}" ]; then
  echo "BENCH_REMOTE requis (repo GitHub de test). Sinon: cargo run --release --example bench_engine"
  exit 1
fi
BIN="$HERE/target/release/ship"
[ -x "$BIN" ] || { echo "build release d'abord: cargo build --release"; exit 1; }
WORK="$(mktemp -d)"; echo "workdir: $WORK"
bash "$HERE/benches/gen_fixture.sh" "$WORK" 50 70
ROOT="$WORK/fixture"; cd "$ROOT"
BASE_SHA="$(git rev-parse HEAD)"
git remote add origin "$BENCH_REMOTE"
git push -q -f origin main
git remote set-head origin -a || true
WATCH="$WORK/watch"; mkdir -p "$WATCH"
REPOS="$WORK/repos"; mkdir -p "$REPOS"
ln -s "$ROOT" "$REPOS/fixture"
i=0
while [ "$i" -lt "$RUNS" ]; do
  cp "$ROOT/delivery.zip" "$WATCH/fixture-bench$i.zip"
  echo "--- run $i ---"
  START=$(python3 -c 'import time;print(time.time())')
  SHIP_TRACE=1 "$BIN" watch --once --watch-dir "$WATCH" --repos-root "$REPOS" 2>&1 | grep -E 'SHIP_TRACE_SUMMARY|MERGED|merged|error|FAIL' || true
  END=$(python3 -c 'import time;print(time.time())')
  python3 -c "print(f'run $i drop->merged: {$END-$START:.2f}s')"
  git switch -q main 2>/dev/null || true
  git reset -q --hard "$BASE_SHA"
  git push -q -f origin main
  i=$((i+1))
done
echo "done."
