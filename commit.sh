#!/usr/bin/env bash
set -euo pipefail

# ─── Helper ───────────────────────────────────────────────────────
commit_file() {
    local file="$1"
    shift

    if [[ -n $(git status --porcelain -- "$file" 2>/dev/null) ]]; then
        git add "$file"
        git commit "$@"
        echo "✅ Committed: $file"
    else
        echo "⏭️  Skipped: $file (no changes)"
    fi
}

# ─── Commits ──────────────────────────────────────────────────────

# Configuration racine
commit_file ".gitignore" \
  -m "chore: add target directory to gitignore" \
  -m "Ignore the build artifacts folder to keep the repository clean."

commit_file "Cargo.lock" \
  -m "chore: lock project dependencies" \
  -m "Generate the lock file representing the current stable dependency graph."

commit_file "Cargo.toml" \
  -m "chore: define package manifest and dependencies" \
  -m "Configure the ship package metadata, binary targets, and production crates."

# Outils de commit et références de logs
commit_file "changed.txt" \
  -m "docs: add initial change reference log" \
  -m "Document the generated diff matrix snapshot."

commit_file "commit.sh" \
  -m "chore: add multi-commit automation workflow script"

commit_file "docs/logo.png" \
  -m "docs: add branding assets" \
  -m "Include the brand logo image within the workspace documentation folder."

commit_file "justfile" \
  -m "chore: add watch development command to justfile"

# Code source applicatif
commit_file "src/guards.rs" \
  -m "feat(core): implement robust git pre-flight guards" \
  -m "Add safety checks for repository status, active remote branch syncing,
lock detection, and stale pid isolation."

commit_file "src/lib.rs" \
  -m "feat(core): expose public interface modules"

commit_file "src/main.rs" \
  -m "feat(cli): orchestrate application command line interface" \
  -m "Map all subcommands (Prepare, Tag, All, Watch, Projects) to the engine."

commit_file "src/manifest.rs" \
  -m "feat(core): implement robust ship.toml manifest parser" \
  -m "Add schema enforcement, structural validation, and flexible glob expansion
against active dirty sets."

commit_file "src/mock_shell.rs" \
  -m "feat(test): add configurable mock shell test double"

commit_file "src/normalize.rs" \
  -m "feat(core): add executable file mode normalizer" \
  -m "Revert spurious executable permission changes enforced by non-unix zip engines."

commit_file "src/pipeline.rs" \
  -m "feat(core): orchestrate continuous deployment milestone pipeline" \
  -m "Implement end-to-end integration flow from code staging, version bumping,
changelog prepend generation, pull request routing, to final tag publishing."

commit_file "src/shell.rs" \
  -m "feat(core): abstract dynamic process execution layer"

commit_file "src/ui.rs" \
  -m "feat(ui): create rich beautiful terminal interface engine" \
  -m "Design advanced layout blocks using progress spinners, status indicators,
error analysis frames, and responsive metrics panels."

commit_file "src/version.rs" \
  -m "feat(core): implement auto-bumping semantic version logic"

commit_file "src/watch.rs" \
  -m "feat(watch): implement automatic daemonized zip hot-reloader" \
  -m "Deploy native filesystem watch notification monitors and fallback poll structures
to capture uploaded milestone archives, execute 3-way branching merges,
and trigger instant atomic validation pipelines asynchronously."

# Suites de tests et validation
commit_file "tests/manifest.rs" \
  -m "test(manifest): verify dynamic config structural parsing validation"

commit_file "tests/normalize.rs" \
  -m "test(normalize): ensure file mode correction works against mock states"

commit_file "tests/pipeline_preflight.rs" \
  -m "test(pipeline): assert authentication and duplicate tag guard blocks fail safely"

commit_file "tests/version.rs" \
  -m "test(version): validate semantic increments and conventional token analysis"

commit_file "tests/workflow.rs" \
  -m "test(workflow): benchmark integration logic and 3-way merge conflict scenarios"

# Scripts de cycle de vie et automatisation locale
commit_file "wr.sh" \
  -m "chore: add test compilation automation wrapper script"

# ─── Teardown ─────────────────────────────────────────────────────
echo ""
echo "🎉 Done. $(git log --oneline -10 | wc -l | tr -d ' ') recent commits."
git log --oneline -10
