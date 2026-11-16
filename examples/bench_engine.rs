//! Realistic LOCAL bench of the delivery workflow (no GitHub required).
//!
//! Measures the whole local hot path on a synthetic monorepo:
//!   GIX  : zip read (zipmem) -> parallel blobs -> merge-tree -> N grouped
//!          commits in memory -> ONE checkout.
//!   CLI  : materialize changes + legacy per-group loop (log/add/status/commit).
//!
//! Run: cargo run --release --example bench_engine -- [groups] [files_per_crate] [runs]
//! Defaults: 50 groups, 70 files/crate (22 crates), 3 runs.

use anyhow::{Result, bail};
use ship::manifest::Manifest;
use ship::shell::{RealShell, Shell};
use ship::{engine, trace, zipmem};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

const CRATES: usize = 22;
const NOISE: usize = 5000;
const BRANCH: &str = "feat/bench-delivery";

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(dir).output()?;
    if !out.status.success() {
        bail!("git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_ignore(dir: &Path, args: &[&str]) {
    let _ = Command::new("git").args(args).current_dir(dir).output();
}

fn writef(path: &Path, content: &str) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn reset(repo: &Path, base: &str) {
    git_ignore(repo, &["switch", "-q", "main"]);
    git_ignore(repo, &["reset", "-q", "--hard", base]);
    git_ignore(repo, &["branch", "-D", BRANCH]);
    git_ignore(repo, &["update-ref", "refs/remotes/origin/main", base]);
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if s.is_empty() {
        return 0.0;
    }
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let groups: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(50);
    let fpc: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(70);
    let runs: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);

    if Command::new("zip").arg("--version").output().is_err() {
        bail!("`zip` binary not found (needed to build the delivery archive)");
    }

    let tmp = tempfile::tempdir()?;
    let repo = tmp.path().join("repo");
    let delivery = tmp.path().join("delivery");
    std::fs::create_dir_all(&repo)?;
    std::fs::create_dir_all(&delivery)?;

    // --- fixture: monorepo + ignored noise ---
    git(&repo, &["init", "-q"])?;
    git(&repo, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
    git(&repo, &["config", "user.email", "bench@ship.local"])?;
    git(&repo, &["config", "user.name", "bench"])?;
    git(&repo, &["config", "commit.gpgsign", "false"])?;
    writef(
        &repo.join(".gitignore"),
        "target/\nship.toml\n.github/pr-body.md\n",
    )?;
    writef(
        &delivery.join(".gitignore"),
        "target/\nship.toml\n.github/pr-body.md\n",
    )?;

    let mut tracked: Vec<(String, String)> = Vec::new();
    for c in 0..CRATES {
        for f in 0..fpc {
            let rel = format!("crates/crate{c}/src/m{f}.rs");
            let content = format!("// crate {c} mod {f}\npub fn f{f}() {{}}\n");
            writef(&repo.join(&rel), &content)?;
            writef(&delivery.join(&rel), &content)?;
            tracked.push((rel, content));
        }
    }
    for i in 0..NOISE {
        writef(&repo.join(format!("target/debug/deps/a{i}.rlib")), "x")?;
    }
    git(&repo, &["add", "-A"])?;
    git(&repo, &["commit", "-q", "-m", "base"])?;
    let base = git(&repo, &["rev-parse", "HEAD"])?;
    git(&repo, &["update-ref", "refs/remotes/origin/main", &base])?;

    // --- delivery: full snapshot + 3 modified files per group + manifest ---
    let mut group_data: Vec<(String, Vec<String>)> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    for g in 0..groups {
        let mut paths = Vec::new();
        for k in 0..3 {
            let idx = g * 3 + k;
            if idx >= tracked.len() {
                break;
            }
            let (rel, content) = &tracked[idx];
            let new = format!("{content}// group {g}\n");
            writef(&delivery.join(rel), &new)?;
            paths.push(rel.clone());
            modified.push(rel.clone());
        }
        group_data.push((format!("feat(bench): group {g}"), paths));
    }
    writef(&delivery.join(".github/pr-body.md"), "# bench PR\n")?;

    let mut toml = format!(
        "[delivery]\nbranch = \"{BRANCH}\"\npr_title = \"feat: bench\"\npr_body = \".github/pr-body.md\"\nbase_commit = \"{base}\"\n"
    );
    for (msg, paths) in &group_data {
        toml.push_str("\n[[commit]]\n");
        toml.push_str(&format!("message = \"{msg}\"\nfiles = ["));
        for (i, p) in paths.iter().enumerate() {
            if i > 0 {
                toml.push_str(", ");
            }
            toml.push_str(&format!("\"{p}\""));
        }
        toml.push_str("]\n");
    }
    writef(&delivery.join("ship.toml"), &toml)?;
    let manifest = Manifest::from_text(&toml)?;

    // --- build the delivery zip (as an AI agent would) ---
    let zip_path = tmp.path().join("delivery.zip");
    let z = Command::new("zip")
        .args(["-qr", &zip_path.to_string_lossy(), "."])
        .current_dir(&delivery)
        .output()?;
    if !z.status.success() {
        bail!("zip failed: {}", String::from_utf8_lossy(&z.stderr));
    }

    println!(
        "fixture: {} crates x {fpc} files = {} tracked, {NOISE} ignored, {groups} groups x 3 files",
        CRATES,
        tracked.len()
    );
    println!(
        "delivery zip: {} bytes",
        std::fs::metadata(&zip_path)?.len()
    );
    println!();
    println!(
        "{:<6} {:>12} {:>10} {:>12} {:>10}",
        "run", "gix(ms)", "spawns", "cli(ms)", "spawns"
    );

    let mut gix_ms: Vec<f64> = Vec::new();
    let mut cli_ms: Vec<f64> = Vec::new();
    let mut gix_sp: Vec<u64> = Vec::new();
    let mut cli_sp: Vec<u64> = Vec::new();

    for r in 0..runs {
        // ---- GIX engine path ----
        reset(&repo, &base);
        trace::reset_spawns();
        let shell = RealShell::new(repo.clone());
        let t = Instant::now();
        let entries = zipmem::read_entries(&zip_path, None)?;
        let prep = engine::prepare_tree(
            &shell,
            &repo,
            entries,
            Some(base.as_str()),
            "origin",
            "main",
        )?;
        if prep.dirty.len() != modified.len() {
            bail!(
                "dirty set mismatch: {} vs {}",
                prep.dirty.len(),
                modified.len()
            );
        }
        let (expanded, _drops) = manifest.expand_globs(&prep.dirty)?;
        let _tip = engine::commit_groups(&shell, &prep, &expanded, BRANCH)?;
        let gms = t.elapsed().as_secs_f64() * 1000.0;
        let gsp = trace::spawns();

        // ---- legacy CLI path (materialize + 4N loop) ----
        reset(&repo, &base);
        for rel in &modified {
            std::fs::copy(delivery.join(rel), repo.join(rel))?;
        }
        trace::reset_spawns();
        let shell = RealShell::new(repo.clone());
        let t = Instant::now();
        let _ = shell.run(&["git", "add", "-A"])?;
        for (msg, paths) in &group_data {
            let _ = shell.run(&["git", "log", "--format=%s", &format!("{base}..HEAD")])?;
            let mut add: Vec<&str> = vec!["git", "add", "-A", "--"];
            for p in paths {
                add.push(p);
            }
            let _ = shell.run(&add)?;
            let _ = shell.run(&["git", "status", "--porcelain"])?;
            let _ = shell.run(&["git", "commit", "--no-verify", "-m", msg])?;
        }
        let cms = t.elapsed().as_secs_f64() * 1000.0;
        let csp = trace::spawns();

        println!(
            "{:<6} {:>12.1} {:>10} {:>12.1} {:>10}",
            r, gms, gsp, cms, csp
        );
        gix_ms.push(gms);
        cli_ms.push(cms);
        gix_sp.push(gsp);
        cli_sp.push(csp);
    }

    println!();
    println!("================ BENCH SUMMARY (local work, no network) ================");
    println!(
        "gix engine : median {:>8.1} ms | spawns median {}",
        median(&gix_ms),
        median(&gix_sp.iter().map(|x| *x as f64).collect::<Vec<_>>())
    );
    println!(
        "legacy CLI : median {:>8.1} ms | spawns median {}",
        median(&cli_ms),
        median(&cli_sp.iter().map(|x| *x as f64).collect::<Vec<_>>())
    );
    let speedup = median(&cli_ms) / median(&gix_ms).max(0.001);
    println!("speedup    : {speedup:.1}x on the local delivery path");
    println!(
        "note       : push + PR + merge (network) measured separately via benches/bench.sh with BENCH_REMOTE"
    );
    println!("========================================================================");
    Ok(())
}
