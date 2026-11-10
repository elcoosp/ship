<div align="center">
  <h1>⚓ ship</h1>
  <p><strong>The ultra-fast, git-only delivery pipeline for Rust & Node monorepos.</strong></p>
  <p>
    <img src="https://img.shields.io/badge/Rust-1.99%2B-orange?style=flat-square&logo=rust" alt="Rust"/>
    <img src="https://img.shields.io/badge/License-MIT-blue?style=flat-square" alt="License MIT"/>
  </p>
</div>

---

`ship` is a declarative, idempotent CLI that turns a dirty working tree into a merged pull request and a released Git tag in under 30 seconds. No CI runs. No manual pushing. No ceremony.

## ✨ Features

- **Declarative Manifests**: Define your commits, version bump, and PR body in a simple `ship.toml` file.
- **3-Way Merge Safety**: If you push commits while the AI is working, `ship` automatically merges the AI's zip with your changes using the `base_commit` recorded in `ship.toml`. No lost work.
- **True Parallel Execution**: Drop multiple zips at once. `ship` processes them concurrently, rendering a clean, live dashboard with `indicatif`.
- **Premium UI**: Animated spinners, color-coded Conventional Commits, perfectly aligned prefixes, and clickable OSC 8 hyperlinks.
- **MacOS Notifications**: Native, clickable notifications with the "Glass" sound via `terminal-notifier` (or AppleScript fallback).
- **Bulletproof Git**: Stashes WIP, auto-heals stale locks, reverts spurious chmod changes, and validates everything before touching git.

## 🚀 Quick Start

```sh
# Install
cargo install --path .

# Start the watcher
ship watch
```

Drop a zip named `project-slug.zip` into `~/Downloads`. `ship` takes over from there.

## 🛠️ The Manifest (`ship.toml`)

The AI generates a `ship.toml` alongside their changes.

```toml
[delivery]
branch      = "feat/compile-seam"
pr_title    = "feat(compile): the lex-registry-IR seam"
pr_body     = ".github/pr-body.md"
base_commit = "a1b2c3d4e5f6..." # The HEAD commit the AI cloned

[version]
bump = "auto" # auto | patch | minor | major

[[commit]]
message = "feat(compile): wire the lex-registry-IR compile seam"
files   = ["Cargo.toml", "crates/aeolus-compile/**/*.rs"]

[[commit]]
message = "docs: align the surface to the P2 seam"
files   = ["README.md", "CHANGELOG.md"]
```

## 📦 The Workflow

1. **Extraction**: `ship` extracts the zip. If the zip has a wrapper folder (e.g., `project/`), it strips it.
2. **3-Way Merge**: If `base_commit` is present, `ship` applies the zip's changes on top of that commit, then merges the result into the latest `main`. Your concurrent pushes are preserved automatically.
3. **Manifest Check**: Verifies `ship.toml` covers every dirty file (the union check).
4. **Commits**: Creates conventional commits from the `[[commit]]` groups.
5. **Release**: Bumps `Cargo.toml`, `Cargo.lock`, and `package.json` (if present) using `cargo set-version`.
6. **Changelog**: Runs `git-cliff` and prepends the release notes.
7. **Push & PR**: Pushes the branch and opens the PR from `.github/pr-body.md`.
8. **Merge & Tag**: Merges via rebase (`gh pr merge --admin`), syncs `main`, verifies the author email, and tags the merge SHA.

## 🛡️ Safety First

- **Never `reset --hard`**: `ship` uses `git switch -C` and stash-pop to sync, preserving any WIP.
- **Force-with-lease**: Pushing uses `--force-with-lease` to prevent overwriting remote changes.
- **Stale Locks**: Automatically clears `.git/*.lock` files older than 30 seconds.
- **Unpushed Commits**: Refuses to run if your local `main` is ahead of `origin/main`.
- **Clean Tree**: Refuses to run if the working tree is dirty in a way the manifest doesn't expect.

## 🎨 The Experience

```
  ⚓  s h i p
     git-only delivery pipeline

  watching      /Users/adm/Downloads
  repos root    /Users/adm/Documents/Repos
  projects      186  (MetaCodable, NexusStratum, Rex, aeolus, alacer, …)

  ready.  drop a {project}-{slug}.zip to ship.

  ──────────────────────────────────────────────────────────────────
[catheo]     📦 catheo-v061-truth-sweep-and-hatch-demo.zip
[catheo]     · 3-way merge enabled (base: a1b2c3d)
[catheo]     ✓ [ 1/8]  fix(docs): the README carries the ui-count truth
[catheo]     ✓ [ 2/8]  feat(parser): the entry vocabulary names every spelling
[peregrine]  📦 peregrine-arm64e-auth-producer.zip
[peregrine]  ✓ [ 1/8]  feat(core): the arm64e producer plans auth words

⠋ [catheo]      fetching origin...
⠙ [peregrine]  committing [ 3/8]...
```

## 🔧 Recovery & Diagnostics

If something goes wrong, `ship` prints a beautiful diagnostic panel with the exact commands to fix it:

```
  ✗ ship failed   aeolus-p2-compile-seam.zip

  ╭─ why ──────────────────────────────────────────────────────────────╮
  │  .github/pr-body.md is TRACKED (in git's index).                   │
  │  .gitignore only applies to untracked files, so adding a pattern   │
  │  does NOT help here.                                               │
  ╰────────────────────────────────────────────────────────────────────╯

  ╭─ fix ──────────────────────────────────────────────────────────────╮
  │  $ cd ~/Documents/Repos/aeolus                                     │
  │  $ git rm --cached .github/pr-body.md  # and/or: ship.toml         │
  │  $ mv ~/Downloads/.failed/aeolus-p2-compile-seam.zip ~/Downloads/  │
  │  $ ship watch --once                                               │
  ╰────────────────────────────────────────────────────────────────────╯
```
