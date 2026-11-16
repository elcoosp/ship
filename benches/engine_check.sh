#!/bin/bash
# bash 3.2 — dit si le moteur gix s'engage sur un repo (sinon pourquoi il fallback).
R="${1:-.}"
cd "$R" 2>/dev/null || { echo "pas un repo: $R"; exit 0; }
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || { echo "pas un repo git: $R"; exit 0; }
echo "repo: $(pwd)"
AC="$(git config --get core.autocrlf 2>/dev/null || echo unset)"
FILTER=0; TEXTNORM=0
for f in $(git ls-files -- '*.gitattributes' 2>/dev/null); do
  [ -f "$f" ] || continue
  grep -Eq 'filter=' "$f" && FILTER=1
  grep -Eq '(^|[[:space:]])text(=auto)?([[:space:]]|$)' "$f" && TEXTNORM=1
done
SUB=0; [ -f .gitmodules ] && SUB=1
echo "  core.autocrlf=$AC  filter=$FILTER  text-norm=$TEXTNORM  submodules=$SUB"
if [ "$AC" = "true" ] || [ "$FILTER" = "1" ] || [ "$SUB" = "1" ]; then
  echo "  VERDICT: fallback CLI (autocrlf=true / filter LFS / submodules)"
else
  echo "  VERDICT: moteur gix S'ACTIVE (text=auto ok tant que le zip est en LF)"
fi
