#!/bin/bash
# bash 3.2 — génère un monorepo synthétique + zip de livraison + ship.toml à N groupes.
# Usage: benches/gen_fixture.sh <dest_dir> [n_groups] [files_per_crate]
set -euo pipefail
DEST="${1:?usage: gen_fixture.sh <dest_dir> [n_groups] [files_per_crate]}"
GROUPS="${2:-50}"
FPC="${3:-70}"
ROOT="$DEST/fixture"
rm -rf "$ROOT"
mkdir -p "$ROOT"
cd "$ROOT"
git init -q
git symbolic-ref HEAD refs/heads/main
git config user.email bench@ship.local
git config user.name bench
git config commit.gpgsign false
printf 'target/\nnode_modules/\nship.toml\n.github/pr-body.md\n' > .gitignore
c=0
while [ "$c" -lt 22 ]; do
  mkdir -p "crates/crate$c/src"
  f=0
  while [ "$f" -lt "$FPC" ]; do
    printf '// crate %s mod %s\npub fn f() {}\n' "$c" "$f" > "crates/crate$c/src/m$f.rs"
    f=$((f+1))
  done
  c=$((c+1))
done
mkdir -p target/debug/deps
i=0
while [ "$i" -lt 5000 ]; do printf 'x' > "target/debug/deps/a$i.rlib"; i=$((i+1)); done
git add -A
git commit -qm base
BASE=$(git rev-parse HEAD)
git update-ref refs/remotes/origin/main HEAD
# livraison: 3 fichiers par groupe + manifest
mkdir -p delivery/.github
printf '# bench PR\n' > delivery/.github/pr-body.md
{
  printf '[delivery]\n'
  printf 'branch = "feat/bench-delivery"\n'
  printf 'pr_title = "feat: bench delivery"\n'
  printf 'pr_body = ".github/pr-body.md"\n'
  printf 'base_commit = "%s"\n' "$BASE"
} > delivery/ship.toml
g=0
while [ "$g" -lt "$GROUPS" ]; do
  {
    printf '\n[[commit]]\n'
    printf 'message = "feat(bench): group %s"\n' "$g"
    printf 'files = ['
    k=0
    while [ "$k" -lt 3 ]; do
      idx=$((g*3+k)); cc=$((idx%22)); ff=$((idx%FPC))
      p="crates/crate$cc/src/m$ff.rs"
      mkdir -p "delivery/$(dirname "$p")"
      printf '// group %s\n' "$g" >> "delivery/$p"
      # snapshot complet requis: copier tous les fichiers trackés dans delivery une seule fois
      [ "$k" -gt 0 ] && printf ', '
      printf '"%s"' "$p"
      k=$((k+1))
    done
    printf ']\n'
  } >> delivery/ship.toml
  g=$((g+1))
done
# snapshot complet du repo dans delivery (le zip = état complet livré)
git ls-files | while IFS= read -r p; do
  mkdir -p "delivery/$(dirname "$p")"
  cp "$p" "delivery/$p"
done
cd delivery
zip -qr "$ROOT/delivery.zip" .
cd "$ROOT"
echo "fixture: $ROOT"
echo "zip    : $ROOT/delivery.zip ($GROUPS groups)"
echo "base   : $BASE"
