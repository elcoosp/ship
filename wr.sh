#!/usr/bin/env bash
# fix108.sh — Add missing Shell trait import to workflow tests.
set -euo pipefail
cd "$(dirname "$0")"

echo "→ Fixing imports in tests/workflow.rs..."
python3 - << 'PY'
import pathlib

p = pathlib.Path("tests/workflow.rs")
src = p.read_text()

if "use ship::shell::Shell;" not in src:
    src = src.replace(
        "use ship::mock_shell::MockShell;",
        "use ship::mock_shell::MockShell;\nuse ship::shell::Shell;"
    )
    p.write_text(src)
    print("  Added Shell trait import.")
else:
    print("  Shell trait already imported.")
PY

echo ""
echo "→ Running integration tests..."
cargo test --test workflow

echo ""
echo "→ cargo fmt --all"
cargo fmt --all

echo ""
echo "→ cargo clippy --workspace --all-targets --all-features -- -D warnings"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo ""
echo "→ cargo build"
cargo build

echo ""
echo "→ cargo install --path ."
cargo install --path .

echo ""
echo "→ Kill old watcher & launch"
pkill -f "ship watch" || true
echo "✓ done."
