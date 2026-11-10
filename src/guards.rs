use crate::shell::{Shell, run_checked, run_ok};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn default_branch(shell: &dyn Shell, remote: &str) -> Result<String> {
    let refname = format!("refs/remotes/{remote}/HEAD");
    if let Ok(out) = shell.run(&["git", "symbolic-ref", "--short", &refname])
        && out.status == 0
    {
        let raw = out.stdout.trim();
        let stripped = raw.strip_prefix(&format!("{remote}/")).unwrap_or(raw);
        if !stripped.is_empty() {
            return Ok(stripped.to_string());
        }
    }
    if let Ok(out) = shell.run(&["git", "ls-remote", "--symref", remote, "HEAD"])
        && out.status == 0
    {
        for line in out.stdout.lines() {
            if let Some(rest) = line.strip_prefix("ref: ")
                && let Some((target, _)) = rest.split_once('\t')
                && let Some(branch) = target.strip_prefix("refs/heads/")
            {
                return Ok(branch.to_string());
            }
        }
    }
    for candidate in ["main", "master"] {
        let ok = shell
            .run(&[
                "git",
                "rev-parse",
                "--verify",
                &format!("refs/heads/{candidate}"),
            ])
            .map(|o| o.status == 0)
            .unwrap_or(false);
        if ok {
            return Ok(candidate.to_string());
        }
    }
    bail!("could not resolve default branch for '{remote}' — try: git remote set-head {remote} -a")
}

pub fn is_git_repo_root(shell: &dyn Shell, expected: &Path) -> Result<bool> {
    let out = match shell.run(&["git", "rev-parse", "--show-toplevel"]) {
        Ok(o) if o.status == 0 => o,
        _ => return Ok(false),
    };
    let top = PathBuf::from(out.stdout.trim());
    let a = top.canonicalize().unwrap_or(top);
    let b = expected
        .canonicalize()
        .unwrap_or_else(|_| expected.to_path_buf());
    Ok(a == b)
}

pub fn is_dirty(shell: &dyn Shell) -> Result<bool> {
    Ok(
        !run_checked(shell, &["git", "status", "--porcelain", "-uall"])?
            .stdout
            .trim()
            .is_empty(),
    )
}

pub fn dirty_paths(shell: &dyn Shell) -> Result<Vec<PathBuf>> {
    let out = run_checked(shell, &["git", "status", "--porcelain", "-uall"])?;
    let mut paths = Vec::new();
    for line in out.stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let rest = &line[3..];
        let p = rest.split(" -> ").last().unwrap_or(rest);
        paths.push(PathBuf::from(p));
    }
    Ok(paths)
}

pub fn union_check(dirty: &[PathBuf], manifest: &[PathBuf]) -> Result<()> {
    let m: HashSet<&PathBuf> = manifest.iter().collect();
    let missing: Vec<_> = dirty.iter().filter(|d| !m.contains(d)).collect();
    if !missing.is_empty() {
        let shown: Vec<_> = missing
            .iter()
            .take(10)
            .map(|p| format!("    {}", p.display()))
            .collect();
        let extra = if missing.len() > 10 {
            format!("\n    … and {} more", missing.len() - 10)
        } else {
            String::new()
        };
        bail!(
            "ship.toml ↔ working tree mismatch\n  dirty but not in ship.toml ({}):\n{}{}\nAdd these paths to ship.toml.",
            missing.len(),
            shown.join("\n"),
            extra
        );
    }
    Ok(())
}

pub fn local_default_not_ahead(shell: &dyn Shell, remote: &str, default: &str) -> Result<()> {
    let range = format!("{remote}/{default}..{default}");
    let out = run_checked(shell, &["git", "rev-list", "--count", &range])?;
    let n: usize = out.stdout.trim().parse().context("rev-list --count")?;
    if n > 0 {
        bail!(
            "local {default} is {n} commit(s) ahead of {remote}/{default} — push or reset them manually; refusing to discard"
        );
    }
    Ok(())
}

pub fn tag_absent(shell: &dyn Shell, remote: &str, tag: &str) -> Result<()> {
    if run_ok(
        shell,
        &[
            "git",
            "rev-parse",
            "-q",
            "--verify",
            &format!("refs/tags/{tag}"),
        ],
    )? {
        bail!("tag {tag} already exists locally — refusing to re-tag");
    }
    let out = run_checked(
        shell,
        &[
            "git",
            "ls-remote",
            "--refs",
            remote,
            &format!("refs/tags/{tag}"),
        ],
    )?;
    if !out.stdout.trim().is_empty() {
        bail!("tag {tag} already exists on {remote} — release already shipped");
    }
    Ok(())
}

pub fn no_git_operation_in_progress(shell: &dyn Shell) -> Result<()> {
    let check = |name: &str| -> Result<bool> {
        let out = run_checked(shell, &["git", "rev-parse", "--git-path", name])?;
        Ok(Path::new(out.stdout.trim()).exists())
    };
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "BISECT_LOG",
    ] {
        if check(marker)? {
            bail!("a git {marker} operation is in progress — finish or abort it first");
        }
    }
    Ok(())
}

pub fn find_git_locks(shell: &dyn Shell) -> Result<Vec<PathBuf>> {
    let out = shell.run(&["git", "rev-parse", "--absolute-git-dir"])?;
    if out.status != 0 {
        return Ok(Vec::new());
    }
    let git_dir = PathBuf::from(out.stdout.trim());
    let mut locks = Vec::new();

    if let Ok(rd) = std::fs::read_dir(&git_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("lock") {
                locks.push(p);
            }
        }
    }

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().and_then(|s| s.to_str()) == Some("lock") {
                    out.push(p);
                }
            }
        }
    }
    walk(&git_dir.join("refs"), &mut locks);
    Ok(locks)
}

pub fn ensure_no_git_locks(shell: &dyn Shell, clean_stale: bool) -> Result<()> {
    for attempt in 0..20 {
        let locks = find_git_locks(shell)?;
        if locks.is_empty() {
            return Ok(());
        }

        let now = std::time::SystemTime::now();
        let mut stale: Vec<(PathBuf, u64)> = Vec::new();
        let mut fresh: Vec<PathBuf> = Vec::new();
        for p in locks {
            let age = std::fs::metadata(&p)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok());
            match age {
                Some(d) if d.as_secs() >= 30 => stale.push((p, d.as_secs())),
                _ => fresh.push(p),
            }
        }

        if !stale.is_empty() {
            if clean_stale {
                for (p, age) in &stale {
                    let _ = std::fs::remove_file(p);
                    eprintln!("removed stale lock (age {age}s): {}", p.display());
                }
            } else {
                return Err(format_locks_err(&stale, &fresh));
            }
        }

        if attempt == 19 {
            return Err(format_locks_err(&[], &fresh));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Ok(())
}

fn format_locks_err(stale: &[(PathBuf, u64)], fresh: &[PathBuf]) -> anyhow::Error {
    let mut msg = String::from("git lock files present:\n");
    for (p, age) in stale {
        msg.push_str(&format!("  STALE ({}s old): {}\n", age, p.display()));
    }
    for p in fresh {
        msg.push_str(&format!(
            "  FRESH (<30s, retried 10s without clearing): {}\n",
            p.display()
        ));
    }
    msg.push_str("\nRecovery:\n");
    if !stale.is_empty() {
        msg.push_str("  • Re-run with --clean-locks to remove stale locks automatically\n");
    }
    msg.push_str(
        "  • Or manually remove each lock file above (safe if no git process is running)\n",
    );
    msg.push_str(
        "  • If locks persist: an editor's git integration may be holding them; close it and retry\n",
    );
    anyhow::anyhow!("{}", msg)
}

pub fn gh_authenticated(shell: &dyn Shell) -> Result<()> {
    run_checked(shell, &["gh", "auth", "status"])?;
    Ok(())
}

pub fn config_email(shell: &dyn Shell) -> Result<String> {
    let out = run_checked(shell, &["git", "config", "user.email"])?;
    let email = out.stdout.trim().to_string();
    if email.is_empty() {
        bail!("git config user.email is empty — set it before running ship");
    }
    Ok(email)
}

pub fn config_name(shell: &dyn Shell) -> Result<String> {
    let out = run_checked(shell, &["git", "config", "user.name"])?;
    let name = out.stdout.trim().to_string();
    if name.is_empty() {
        bail!("git config user.name is empty — set it before running ship");
    }
    Ok(name)
}

pub fn ship_artifacts_gitignored(shell: &dyn Shell, manifest: &Path, pr_body: &Path) -> Result<()> {
    let mut hard_fail: Vec<String> = Vec::new();
    let mut soft_warn: Vec<String> = Vec::new();
    let mut to_append: Vec<String> = Vec::new();

    for path in [manifest, pr_body] {
        let s = path.to_string_lossy().into_owned();
        let tracked = run_ok(shell, &["git", "ls-files", "--error-unmatch", &s])?;
        let ignored = run_ok(shell, &["git", "check-ignore", "-q", &s])?;

        if ignored {
            if tracked {
                soft_warn.push(format!(
                    concat!(
                        "{} is both tracked AND matched by .gitignore.\n",
                        "This usually means .gitignore was added after the file was already committed.\n",
                        "Consider: git rm --cached {}"
                    ),
                    path.display(),
                    s
                ));
            }
            continue;
        }

        if tracked {
            hard_fail.push(format!(
                concat!(
                    "{} is TRACKED (in git's index).\n",
                    ".gitignore only applies to untracked files, so adding a pattern does NOT help here.\n",
                    "\n",
                    "Fix — pick one:\n",
                    "  A. Untrack it (recommended): git rm --cached {s}\n",
                    "     Then .gitignore applies. The file stays on disk and stays in .gitignore.\n",
                    "  B. Keep tracking it — remove its entry from .gitignore, accept it in the tree,\n",
                    "     and either add it to ship.toml or remove it from the repo before shipping.\n",
                    "\n",
                    "Verify current state:\n",
                    "  git ls-files {s}      # shows it if tracked\n",
                    "  git check-ignore {s}  # silent + exit 0 if ignored, exit 1 if not"
                ),
                path.display(),
                s = s
            ));
        } else {
            to_append.push(s);
        }
    }

    if !to_append.is_empty() {
        let gi = Path::new(".gitignore");
        let mut content = if gi.exists() {
            std::fs::read_to_string(gi).unwrap_or_default()
        } else {
            String::new()
        };
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        for line in &to_append {
            content.push_str(line);
            content.push('\n');
            eprintln!("WARN: added '{line}' to .gitignore (was missing)");
        }
        std::fs::write(gi, content)?;

        for line in &to_append {
            let ok = run_ok(shell, &["git", "check-ignore", "-q", line])?;
            if !ok {
                hard_fail.push(format!(
                    "{} is still not gitignored after appending — check .gitignore manually",
                    line
                ));
            }
        }
    }

    for w in &soft_warn {
        eprintln!("WARN: {w}");
    }
    if !hard_fail.is_empty() {
        bail!("{}", hard_fail.join("\n\n"));
    }
    Ok(())
}

pub fn verify_author(shell: &dyn Shell) -> Result<()> {
    let want = config_email(shell)?;
    let got = run_checked(shell, &["git", "log", "-1", "--format=%ae", "HEAD"])?
        .stdout
        .trim()
        .to_string();
    if got != want {
        bail!("merged HEAD author ({got}) is not the local git identity ({want})");
    }
    Ok(())
}

pub fn run_status_paths(shell: &dyn Shell, paths: &[String]) -> Result<Vec<String>> {
    let mut argv: Vec<&str> = vec!["git", "status", "--porcelain", "-uall", "--"];
    for p in paths {
        argv.push(p);
    }
    let out = run_checked(shell, &argv)?;
    let mut result = Vec::new();
    for line in out.stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let rest = &line[3..];
        let p = rest.split(" -> ").last().unwrap_or(rest);
        result.push(p.to_string());
    }
    Ok(result)
}

pub fn staged_is_empty(shell: &dyn Shell) -> Result<bool> {
    Ok(shell.run(&["git", "diff", "--cached", "--quiet"])?.status == 0)
}

pub fn require_command(shell: &dyn Shell, argv: &[&str], hint: &str) -> Result<()> {
    let out = shell.run(argv)?;
    if out.status != 0 {
        bail!(
            "required command not found: {}\n  install: {hint}",
            argv.join(" ")
        );
    }
    Ok(())
}

pub fn assert_on_main_and_in_sync(shell: &dyn Shell, remote: &str, default: &str) -> Result<()> {
    let branch = run_checked(shell, &["git", "symbolic-ref", "--short", "-q", "HEAD"])?
        .stdout
        .trim()
        .to_string();
    if branch != default {
        bail!("not on {default} (on '{branch}') — switch to {default}, then re-drop the zip");
    }
    let ahead: usize = run_checked(
        shell,
        &[
            "git",
            "rev-list",
            "--count",
            &format!("{remote}/{default}..{default}"),
        ],
    )?
    .stdout
    .trim()
    .parse()
    .unwrap_or(0);
    if ahead > 0 {
        bail!("{default} is {ahead} commit(s) ahead of {remote}/{default} — push or reset first");
    }
    Ok(())
}
