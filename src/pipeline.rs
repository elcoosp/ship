use crate::guards;
use crate::manifest::Manifest;
use crate::shell::{Shell, run_checked};
use anyhow::{Result, anyhow, bail};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Default, Debug)]
pub struct Outcome {
    pub pr_url: Option<String>,
    pub merged: Option<String>,
}

pub fn prepare(
    dry_run: bool,
    reset: bool,
    clean_locks: bool,
    m_orig: &Manifest,
    shell: &dyn Shell,
) -> Result<Outcome> {
    let remote = &m_orig.delivery.remote;
    let default = guards::default_branch(shell, remote)?;

    guards::gh_authenticated(shell)?;
    guards::ensure_no_git_locks(shell, clean_locks)?;
    guards::no_git_operation_in_progress(shell)?;
    guards::local_default_not_ahead(shell, remote, &default)?;
    let _email = guards::config_email(shell)?;
    let _name = guards::config_name(shell)?;
    guards::ship_artifacts_gitignored(
        shell,
        &PathBuf::from("ship.toml"),
        &PathBuf::from(&m_orig.delivery.pr_body),
    )?;

    // Revert spurious chmod +x from a zip produced with `chmod -R +x`.
    let reverted = crate::normalize::exec_bit(shell)?;
    if reverted > 0 {
        crate::ui::warn(&format!(
            "reverted spurious mode changes on {reverted} tracked file(s)"
        ));
    }

    if reset {
        let base = run_checked(
            shell,
            &["git", "merge-base", &format!("{remote}/{default}"), "HEAD"],
        )?
        .stdout
        .trim()
        .to_string();
        if base.is_empty() {
            bail!("cannot compute merge-base of {remote}/{default} and HEAD");
        }
        if dry_run {
            println!("# would reset --soft to {base}");
        } else {
            run_checked(shell, &["git", "reset", "--soft", &base])?;
            crate::ui::info(&format!(
                "reset --soft {base} (undoing prior ship commits; working tree preserved)"
            ));
        }
    }

    crate::ui::step("fetching origin...");
    run_checked(shell, &["git", "fetch", remote, "--prune"])?;

    let dirty_now = if guards::is_dirty(shell)? {
        guards::dirty_paths(shell)?
    } else {
        let all_committed = m_orig.commit.iter().all(|g| {
            let subject = g.message.lines().next().unwrap_or("");
            already_committed(shell, subject, &format!("{remote}/{default}")).unwrap_or(false)
        });
        if !all_committed {
            bail!(
                "working tree is clean and not all [[commit]] groups are on the branch — \
                 extract the zip into the repo, or check ship.toml"
            );
        }
        Vec::new()
    };
    let resume_mode = dirty_now.is_empty();

    let (expanded, drops) = m_orig.expand_globs(&dirty_now)?;
    if !resume_mode {
        report_drops(m_orig, &expanded, &drops);
    }
    let m = &expanded;

    if !resume_mode {
        let mut manifest_paths = Vec::new();
        for g in &m.commit {
            manifest_paths.extend(g.files.iter().cloned());
        }
        guards::union_check(&dirty_now, &manifest_paths)?;
    }

    if dry_run {
        if resume_mode {
            println!(
                "# RESUME MODE — tree clean, all {} groups already committed",
                m.commit.len()
            );
            println!("# would push: {remote}/{}", m.delivery.branch);
            println!("# would open PR: {:?}", m.delivery.pr_title);
            println!("# would merge: --admin --rebase --delete-branch");
        } else {
            print_plan(m, &default);
        }
        return Ok(Outcome::default());
    }

    if resume_mode {
        crate::ui::resume();
    }

    ensure_on_branch(shell, m, remote, &default)?;
    if !resume_mode {
        ensure_committed(shell, m, remote, &default)?;
        ensure_no_leftover_dirty(shell)?;
    }

    ensure_pushed(shell, &m.delivery.branch)?;
    let pr_url = ensure_pr(shell, m, &default)?;
    let merged = merge_immediately(shell, &pr_url)?;
    crate::ui::merged(&merged);

    ensure_synced(shell, &default, remote, &merged)?;
    guards::verify_author(shell)?;

    Ok(Outcome {
        pr_url: Some(pr_url),
        merged: Some(merged),
    })
}

fn report_drops(original: &Manifest, expanded: &Manifest, drops: &crate::manifest::DropReport) {
    for (i, dropped) in drops.per_group.iter().enumerate() {
        if dropped.is_empty() {
            continue;
        }
        let message = &original.commit[i].message;
        if expanded.commit[i].files.is_empty() {
            eprintln!(
                "note: [[commit]][{i}] '{}' — empty (all {} declared path(s) not dirty):",
                message,
                dropped.len()
            );
        } else {
            eprintln!(
                "note: [[commit]][{i}] '{}': dropping {} not-dirty path(s):",
                message,
                dropped.len()
            );
        }
        for p in dropped.iter().take(20) {
            eprintln!("    {}", p.display());
        }
        if dropped.len() > 20 {
            eprintln!("    … and {} more", dropped.len() - 20);
        }
    }
}

fn print_plan(m: &Manifest, default: &str) {
    let _ = default;
    crate::ui::plan_header(&m.delivery.branch);
    let non_empty = m.commit.iter().filter(|g| !g.files.is_empty()).count();
    for (i, g) in m.commit.iter().enumerate() {
        if g.files.is_empty() {
            continue;
        }
        let label = format!("{}  ({} files)", g.message, g.files.len());
        crate::ui::plan_commit(i, non_empty, &label);
    }
    crate::ui::plan_footer();
}

fn ensure_no_leftover_dirty(shell: &dyn Shell) -> Result<()> {
    let remaining = guards::dirty_paths(shell)?;
    if remaining.is_empty() {
        return Ok(());
    }
    let shown: Vec<_> = remaining
        .iter()
        .take(20)
        .map(|p| format!("    {}", p.display()))
        .collect();
    let extra = if remaining.len() > 20 {
        format!("\n    … and {} more", remaining.len() - 20)
    } else {
        String::new()
    };
    bail!(
        "working tree still dirty after all commits:\n{}{}\nAdd these paths to ship.toml (or gitignore them).",
        shown.join("\n"),
        extra
    );
}

fn merge_immediately(shell: &dyn Shell, pr_url: &str) -> Result<String> {
    let state = pr_state(shell, pr_url)?;
    match state.as_str() {
        "MERGED" => {
            return merged_sha(shell, pr_url)?
                .ok_or_else(|| anyhow!("PR is MERGED but mergeCommit.oid is empty"));
        }
        "CLOSED" => bail!("PR {pr_url} is CLOSED — not merging"),
        _ => {}
    }
    crate::ui::step("merging PR (--admin)...");
    let out = shell.run(&[
        "gh",
        "pr",
        "merge",
        pr_url,
        "--admin",
        "--rebase",
        "--delete-branch",
    ])?;
    if out.status != 0 {
        bail!(
            "gh pr merge --admin failed (are you a repo admin?): {}",
            out.stderr.trim()
        );
    }
    for attempt in 0..3 {
        if let Some(sha) = merged_sha(shell, pr_url)? {
            return Ok(sha);
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_secs(5));
        }
    }
    bail!("gh pr merge --admin returned 0 but mergeCommit.oid is still empty after 10s")
}

fn pr_state(shell: &dyn Shell, pr_url: &str) -> Result<String> {
    Ok(run_checked(
        shell,
        &[
            "gh", "pr", "view", pr_url, "--json", "state", "--jq", ".state",
        ],
    )?
    .stdout
    .trim()
    .to_string())
}

fn merged_sha(shell: &dyn Shell, pr_url: &str) -> Result<Option<String>> {
    let out = run_checked(
        shell,
        &[
            "gh",
            "pr",
            "view",
            pr_url,
            "--json",
            "mergeCommit",
            "--jq",
            r#".mergeCommit.oid // empty"#,
        ],
    )?
    .stdout;
    let s = out.trim();
    if s.is_empty() {
        Ok(None)
    } else {
        Ok(Some(s.to_string()))
    }
}

fn ensure_on_branch(shell: &dyn Shell, m: &Manifest, remote: &str, default: &str) -> Result<()> {
    let branch = &m.delivery.branch;
    run_checked(shell, &["git", "fetch", remote, "--prune"])?;
    let current = run_checked(shell, &["git", "rev-parse", "--abbrev-ref", "HEAD"])?
        .stdout
        .trim()
        .to_string();
    if &current == branch {
        return Ok(());
    }
    let stashed = guards::is_dirty(shell)?;
    if stashed {
        run_checked(
            shell,
            &["git", "stash", "push", "-u", "-m", "ship: pre-branch"],
        )?;
    }
    let switch_res = run_checked(
        shell,
        &[
            "git",
            "switch",
            "-C",
            branch,
            &format!("{remote}/{default}"),
        ],
    );
    if let Err(e) = switch_res {
        if e.to_string().contains("index.lock") {
            crate::ui::warn("git index is locked, retrying switch...");
            std::thread::sleep(std::time::Duration::from_millis(1000));
            run_checked(
                shell,
                &[
                    "git",
                    "switch",
                    "-C",
                    branch,
                    &format!("{remote}/{default}"),
                ],
            )?;
        } else {
            return Err(e);
        }
    }
    if stashed {
        run_checked(shell, &["git", "stash", "pop"])?;
    }
    let now = run_checked(shell, &["git", "rev-parse", "--abbrev-ref", "HEAD"])?
        .stdout
        .trim()
        .to_string();
    if &now != branch {
        bail!("expected to be on {branch}, got {now}");
    }
    Ok(())
}

fn ensure_committed(shell: &dyn Shell, m: &Manifest, remote: &str, default: &str) -> Result<()> {
    let base = format!("{remote}/{default}");
    let total = m.commit.len();
    for (idx, g) in m.commit.iter().enumerate() {
        if g.files.is_empty() {
            continue;
        }
        if already_committed(shell, &g.message, &base)? {
            continue;
        }
        let paths: Vec<String> = g
            .files
            .iter()
            .map(|f| f.to_string_lossy().into_owned())
            .collect();
        let mut add: Vec<&str> = vec!["git", "add", "-A", "--"];
        for p in &paths {
            add.push(p);
        }
        run_checked(shell, &add)?;

        // git status --porcelain is the absolute truth. If it's empty, skip.
        let status_out = shell.run(&["git", "status", "--porcelain"])?;
        if status_out.stdout.trim().is_empty() {
            crate::ui::warn(&format!(
                "git status is clean, skipping commit for '{}'",
                g.message
            ));
            continue;
        }

        let commit_res = shell.run(&["git", "commit", "--no-verify", "-m", &g.message]);
        if let Ok(out) = &commit_res
            && out.status != 0
        {
            crate::ui::warn(&format!("git commit failed for '{}'", g.message));
            crate::ui::info(&format!("stdout: {}", out.stdout.trim()));
            crate::ui::info(&format!("stderr: {}", out.stderr.trim()));
        }
        commit_res?;
        crate::ui::commit_step(idx + 1, total, &g.message);
    }
    Ok(())
}

fn already_committed(shell: &dyn Shell, message: &str, base: &str) -> Result<bool> {
    let subject = message.lines().next().unwrap_or("");
    let out = shell.run(&["git", "log", "--format=%s", &format!("{base}..HEAD")])?;
    if out.status != 0 {
        return Ok(false);
    }
    Ok(out.stdout.lines().any(|l| l.trim() == subject.trim()))
}

fn ensure_pushed(shell: &dyn Shell, branch: &str) -> Result<()> {
    crate::ui::step("pushing branch...");
    run_checked(
        shell,
        &["git", "push", "--force-with-lease", "origin", branch],
    )?;
    Ok(())
}

fn ensure_pr(shell: &dyn Shell, m: &Manifest, default: &str) -> Result<String> {
    let branch = &m.delivery.branch;
    let listed = run_checked(
        shell,
        &[
            "gh",
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "open",
            "--json",
            "url",
            "--jq",
            ".[0].url // empty",
        ],
    )?
    .stdout
    .trim()
    .to_string();
    if !listed.is_empty() {
        crate::ui::pr_reused(&listed);
        return Ok(listed);
    }
    let closed = run_checked(
        shell,
        &[
            "gh",
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "closed",
            "--json",
            "url",
            "--jq",
            ".[0].url // empty",
        ],
    )?
    .stdout
    .trim()
    .to_string();
    if !closed.is_empty() {
        bail!("PR {closed} exists for {branch} but is closed — delete the branch and retry");
    }
    let url = run_checked(
        shell,
        &[
            "gh",
            "pr",
            "create",
            "--base",
            default,
            "--head",
            branch,
            "--title",
            &m.delivery.pr_title,
            "--body-file",
            &m.delivery.pr_body,
        ],
    )?
    .stdout
    .trim()
    .to_string();
    if url.is_empty() {
        bail!("gh pr create returned no URL");
    }
    crate::ui::pr_opened(&url);
    Ok(url)
}

fn ensure_synced(shell: &dyn Shell, default: &str, remote: &str, merged: &str) -> Result<()> {
    run_checked(shell, &["git", "fetch", remote, "--prune"])?;
    guards::local_default_not_ahead(shell, remote, default)?;
    let stashed = guards::is_dirty(shell)?;
    if stashed {
        run_checked(
            shell,
            &["git", "stash", "push", "-u", "-m", "ship: pre-sync"],
        )?;
    }
    run_checked(
        shell,
        &[
            "git",
            "switch",
            "-C",
            default,
            &format!("{remote}/{default}"),
        ],
    )?;
    if stashed {
        run_checked(shell, &["git", "stash", "pop"])?;
    }
    let head = run_checked(shell, &["git", "rev-parse", "HEAD"])?
        .stdout
        .trim()
        .to_string();
    if head != merged {
        bail!("after sync, HEAD is {head} but the merged SHA was {merged} — origin moved mid-run");
    }
    Ok(())
}
