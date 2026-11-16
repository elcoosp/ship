#!/bin/bash
# bash 3.2 macOS — E2E drop->merged sur repo GitHub jetable auto-créé.
# Auth: GH_TOKEN/GITHUB_TOKEN sinon `gh auth token` (bypass keyring cassé, comme ship).
# Env: RUNS=3, KEEP=1 pour garder le repo même en cas de succès.
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
RUNS="${RUNS:-3}"
KEEP="${KEEP:-0}"
BIN="$HERE/target/release/ship"
[ -x "$BIN" ] || { echo "build release d'abord: cargo build --release"; exit 1; }
command -v gh >/dev/null 2>&1 || { echo "gh requis (brew install gh) — skip E2E"; exit 0; }
command -v zip >/dev/null 2>&1 || { echo "zip requis — skip E2E"; exit 0; }

# --- résolution du token (le keyring peut être cassé) ---
TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
if [ -z "$TOKEN" ]; then
  TOKEN="$(gh auth token 2>/dev/null || true)"
fi
if [ -z "$TOKEN" ]; then
  echo "aucun token GitHub (GH_TOKEN/GITHUB_TOKEN ou gh auth token). Lance: gh auth login — skip E2E"
  exit 0
fi
export GH_TOKEN="$TOKEN"
LOGIN="$(gh api user -q .login 2>/dev/null || true)"
if [ -z "$LOGIN" ]; then
  echo "token GitHub invalide/expiré (gh api user a échoué). Re-auth: gh auth login — skip E2E"
  exit 0
fi
gh auth setup-git >/dev/null 2>&1 || true
echo "authentifié en tant que: $LOGIN"

TS="$(date +%s)"
SLUG="ship-bench-$TS"
SUCCESS=0
echo "=== E2E: création repo privé jetable $SLUG ==="
if ! gh repo create "$SLUG" --private >/dev/null 2>&1; then
  echo "gh repo create a échoué (scope repo manquant ?) — skip E2E"
  exit 0
fi
REMOTE="https://github.com/${LOGIN}/${SLUG}.git"
echo "remote: $REMOTE"
cleanup() {
  if [ "$KEEP" = "1" ] || [ "$SUCCESS" != "1" ]; then
    echo "repo conservé pour inspection: $SLUG  (delete: gh repo delete $SLUG --yes)"
  else
    echo "cleanup: suppression $SLUG"
    gh repo delete "$SLUG" --yes >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

WORK="$(mktemp -d)"
bash "$HERE/benches/gen_fixture.sh" "$WORK" 50 70 >/dev/null
ROOT="$WORK/fixture"
cd "$ROOT"
BASE_SHA="$(git rev-parse HEAD)"
git remote add origin "$REMOTE"
git push -q -f origin main
git remote set-head origin -a || true
WATCH="$WORK/watch"; mkdir -p "$WATCH"
REPOS="$WORK/repos"; mkdir -p "$REPOS"
ln -s "$ROOT" "$REPOS/fixture"

echo ""
echo "run   drop->merged(s)   spawns   merged-sha"
TIMES=""
i=0
while [ "$i" -lt "$RUNS" ]; do
  BR="feat/bench-run$i"
  sed -i '' "s|^branch = .*|branch = \"$BR\"|" "$ROOT/delivery/ship.toml"
  rm -f "$ROOT/delivery.zip"
  ( cd "$ROOT/delivery" && zip -qr "$ROOT/delivery.zip" . )
  git switch -q main 2>/dev/null || git checkout -q main
  git reset -q --hard "$BASE_SHA"
  git push -q -f origin main
  git update-ref refs/remotes/origin/main "$BASE_SHA"
  cp "$ROOT/delivery.zip" "$WATCH/fixture-run$i.zip"

  START="$(python3 -c 'import time;print(time.time())')"
  if ! SHIP_TRACE=1 "$BIN" watch --once --watch-dir "$WATCH" --repos-root "$REPOS" > "$WORK/run$i.log" 2>&1; then
    echo "run $i ÉCHEC — extrait log (token masqué):"
    tail -n 45 "$WORK/run$i.log" | sed "s|${TOKEN}|***|g"
    echo "log complet: $WORK/run$i.log"
    exit 1
  fi
  END="$(python3 -c 'import time;print(time.time())')"
  DT="$(python3 -c "print(f'{$END-$START:.2f}')")"
  SP="$(grep -o 'spawns=[0-9]*' "$WORK/run$i.log" | tail -1 | cut -d= -f2)"
  SHA="$(grep -oE '[a-f0-9]{7,40}' "$WORK/run$i.log" | tail -1)"
  echo "$i     $DT               ${SP:-?}        ${SHA:-?}"
  TIMES="$TIMES $DT"
  i=$((i+1))
done

echo ""
python3 - "$TIMES" <<'PY'
import sys
vals = sorted(float(x) for x in sys.argv[1].split())
n = len(vals)
med = vals[n//2] if n % 2 else (vals[n//2-1]+vals[n//2])/2
print("================ E2E SUMMARY (réel GitHub, drop->merged) ================")
print(f"runs={n}  median={med:.2f}s  min={vals[0]:.2f}s  max={vals[-1]:.2f}s")
print(f"objectif < 10s : {'PASS' if med < 10 else 'FAIL'}")
print("détail phases: grep SHIP_TRACE dans les logs run<i> (fetch/push/pr/merge/sync)")
print("=========================================================================")
PY
SUCCESS=1
