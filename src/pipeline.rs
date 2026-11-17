use crate::guards;
use crate::manifest::Manifest;
use crate::shell::{Shell, run_checked};
use crate::{engine, trace, zipmem};
use anyhow::{Result, anyhow, bail};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Default, Debug)]
pub struct Outcome {
    pub pr_url: Option<String>,
    pub merged: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MergeMode {
    /// Try direct push to default branch (PR becomes a receipt), fall back to via-pr.
    #[default]
    Auto,
    /// Force direct push; fail if rejected.
    Direct,
    /// Legacy gh pr merge --admin --rebase with fast backoff polling.
    ViaPr,
}

pub struct EngineInput {
    pub repo_path: PathBuf,
    pub entries: Vec<zipmem::Entry>,
    pub base_commit: Option<String>,
}

#[derive(Default)]
pub struct PrepareOpts {
    pub merge_mode: MergeMode,
    pub engine: Option<EngineInput>,
    pub default: Option<String>,
}

/// Legacy entry point (CLI engine, Auto merge). Kept for tests and `ship prepare`.
pub fn prepare(
    dry_run: bool,
    reset: bool,
    clean_locks: bool,
    m_orig: &Manifest,
    shell: &dyn Shell,
) -> Result<Outcome> {
    prepare_with(
        dry_run,
        reset,
        clean_locks,
        m_orig,
        shell,
        PrepareOpts::default(),
    )
}

pub fn prepare_with(
    dry_run: bool,
    reset: bool,
    clean_locks: bool,
    m_orig: &Manifest,
    shell: &dyn Shell,
    opts: PrepareOpts,
) -> Result<Outcome> {
    let total = std::time::Instant::now();
    trace::reset_spawns();
    let remote = &m_orig.delivery.remote;
    let default = match opts.default {
        Some(d) => d,
        None => guards::default_branch(shell, remote)?,
    };

    guards::gh_authenticated(shell)?;
    guards::ensure_no_git_locks(shell, clean_locks)?;
    guards::no_git_operation_in_progress(shell)?;
    guards::local_default_not_ahead(shell, remote, &default)?;
    let _email = guards::config_email(shell)?;
    let _name = guards::config_name(shell)?;
    // Artifacts guard runs FIRST so .gitignore is updated before any ignore filtering.
    guards::ship_artifacts_gitignored(
        shell,
        &PathBuf::from("ship.toml"),
        &PathBuf::from(&m_orig.delivery.pr_body),
    )?;

    let engine_mode = opts.engine.is_some();
    if !engine_mode {
        // Revert spurious chmod +x from a zip produced with `chmod -R +x` (once).
        let reverted = crate::normalize::exec_bit(shell)?;
        if reverted > 0 {
            crate::ui::warn(&format!(
                "reverted spurious mode changes on {reverted} tracked file(s)"
            ));
        }
    }

    if reset {
        if engine_mode {
            bail!("--reset is only supported with the CLI engine (--engine cli)");
        }
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

    // ONE fetch for the whole run (was 3-4).
    crate::ui::step("fetching origin...");
    {
        let _p = trace::phase("fetch");
        run_checked(shell, &["git", "fetch", "--no-tags", remote, &default])?;
    }

    let base_ref = format!("{remote}/{default}");
    let (m, engine_prep, resume_mode) = if let Some(input) = opts.engine {
        let prep = engine::prepare_tree(
            shell,
            &input.repo_path,
            input.entries,
            input.base_commit.as_deref(),
            remote,
            &default,
        )?;
        let (expanded, drops) = m_orig.expand_globs(&prep.dirty)?;
        report_drops(m_orig, &expanded, &drops);
        let mut manifest_paths = Vec::new();
        for g in &expanded.commit {
            manifest_paths.extend(g.files.iter().cloned());
        }
        guards::union_check(&prep.dirty, &manifest_paths)?;
        (expanded, Some(prep), false)
    } else {
        let (expanded, _dirty, resume) = validate_inner(m_orig, shell, &base_ref)?;
        (expanded, None, resume)
    };

    if dry_run {
        if resume_mode {
            println!(
                "# RESUME MODE — tree clean, all {} groups already committed",
                m.commit.len()
            );
            println!("# would push: {remote}/{}", m.delivery.branch);
            println!("# would open PR: {:?}", m.delivery.pr_title);
            println!("# would merge: mode {:?}", opts.merge_mode);
        } else {
            print_plan(&m, &default);
        }
        return Ok(Outcome::default());
    }

    if resume_mode {
        crate::ui::resume();
    }

    if let Some(prep) = engine_prep {
        let tip = engine::commit_groups(shell, &prep, &m, &m.delivery.branch)?;
        crate::ui::info(&format!("engine tip: {tip}"));
        ensure_no_leftover_dirty(shell)?;
    } else {
        ensure_on_branch(shell, &m, remote, &default)?;
        if !resume_mode {
            ensure_committed(shell, &m, remote, &default)?;
            ensure_no_leftover_dirty(shell)?;
        }
    }

    // OPTIMISATION ULTIME : Single Push.
    // On pousse la branche ET main en une seule négociation git.
    // Cela élimine le push initial de la branche (~2.3s) et réduit les handshakes TLS.
    // Une fois main mis à jour, on crée le PR, que GitHub marque instantanément comme "Merged".
    let tip = head_sha(shell)?;
    let refspec_branch = format!("{}:refs/heads/{}", tip, m.delivery.branch);
    let refspec_main = format!("{}:refs/heads/{}", tip, default);

    let pr_url: Option<String>;

    let merged = match opts.merge_mode {
        MergeMode::ViaPr => {
            // Fallback legacy : push branche puis gh pr merge
            let _p = trace::phase("push");
            ensure_pushed(shell, remote, &m.delivery.branch)?;
            pr_url = Some(ensure_pr(shell, &m, &default)?);
            merge_via_pr(
                shell,
                pr_url.as_ref().unwrap(),
                &m.delivery.branch,
                remote,
                &default,
            )?
        }
        MergeMode::Direct | MergeMode::Auto => {
            // PR-as-Receipt Pattern:
            // 1. Push branch first (so it's ahead of main)
            // 2. Create PR (GitHub accepts it because branch > main)
            // 3. Push main (fast-forward). GitHub sees PR head is reachable from main and auto-merges it.

            let _p_push1 = trace::phase("push_branch");
            crate::ui::step("pushing delivery branch...");
            let out_branch = shell.run(&[
                "git",
                "push",
                "--no-verify",
                "--force-with-lease",
                remote,
                &refspec_branch,
            ])?;
            if out_branch.status != 0 {
                bail!("failed to push delivery branch:\n{}", out_branch.stderr);
            }

            let _p_pr = trace::phase("pr");
            pr_url = Some(ensure_pr(shell, &m, &default)?);

            let _p_push2 = trace::phase("push_main");
            crate::ui::step("fast-forwarding main (PR will auto-merge)...");

            // Protect main from concurrent pushes
            let expected_main_sha =
                run_checked(shell, &["git", "rev-parse", &format!("{remote}/{default}")])?
                    .stdout
                    .trim()
                    .to_string();

            let out_main = shell.run(&[
                "git",
                "push",
                "--no-verify",
                &format!("--force-with-lease={}:{}", default, expected_main_sha),
                remote,
                &refspec_main,
            ])?;

            if out_main.status == 0 {
                crate::ui::info("main fast-forwarded successfully; PR auto-merged by GitHub");
                spawn_branch_delete(shell.cwd(), remote, &m.delivery.branch);
                tip
            } else {
                crate::ui::warn(&format!(
                    "push to main rejected:\n{}\n{}\n\n                     Hint: This repo may have branch protection rules on '{default}'.\n                     Falling back to standard PR merge...",
                    out_main.stdout.trim(),
                    out_main.stderr.trim()
                ));
                if opts.merge_mode == MergeMode::Direct {
                    bail!("direct push to {default} rejected and --merge direct was forced");
                }
                let _p_fallback = trace::phase("push_fallback");
                merge_via_pr(
                    shell,
                    pr_url.as_ref().unwrap(),
                    &m.delivery.branch,
                    remote,
                    &default,
                )?
            }
        }
    };

    // ─── POST-DELIVERY LOCAL CLEANUP (ZERO NETWORK) ───────────────
    // Instead of `ensure_synced` (which fetches), we locally reset `default`
    // to the known `merged` SHA. This is instantaneous and avoids network.
    let current_branch = run_checked(shell, &["git", "rev-parse", "--abbrev-ref", "HEAD"])?
        .stdout
        .trim()
        .to_string();

    if current_branch == m.delivery.branch {
        crate::ui::step("cleaning up local workspace (zero network)...");
        // 1. Force-reset local `default` (e.g., main) to the merged SHA and check it out
        run_checked(shell, &["git", "checkout", "-B", &default, &merged])?;
        // 2. Delete the local delivery branch (ignore error if already gone)
        let _ = shell.run(&["git", "branch", "-D", &m.delivery.branch]);
        crate::ui::info("workspace cleaned: local main updated and delivery branch deleted");
    } else if current_branch != default {
        // Fallback: if we are on some other unexpected branch, sync it normally
        ensure_synced(shell, &default, remote, &merged)?;
    }
    // ──────────────────────────────────────────────────────────────

    guards::verify_author(shell)?;

    trace::event("total", "prepare", total.elapsed().as_secs_f64() * 1000.0);
    trace::summary("prepare");

    Ok(Outcome {
        pr_url,
        merged: Some(merged),
    })
}

fn head_sha(shell: &dyn Shell) -> Result<String> {
    Ok(run_checked(shell, &["git", "rev-parse", "HEAD"])?
        .stdout
        .trim()
        .to_string())
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
    )
}

/// Combined state+mergeCommit view: ONE gh call per poll instead of two.
fn pr_view(shell: &dyn Shell, pr_url: &str) -> Result<(String, Option<String>)> {
    let out = run_checked(
        shell,
        &["gh", "pr", "view", pr_url, "--json", "state,mergeCommit"],
    )?;
    let v: serde_json::Value = serde_json::from_str(&out.stdout)
        .map_err(|e| anyhow!("cannot parse gh pr view output: {e}"))?;
    let state = v["state"].as_str().unwrap_or("").to_string();
    let oid = v["mergeCommit"]["oid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Ok((state, oid))
}

fn spawn_branch_delete(cwd: &std::path::Path, remote: &str, branch: &str) {
    // Fire-and-forget: branch deletion is cosmetic; rate limits must not fail the run.
    let cwd = cwd.to_path_buf();
    let remote = remote.to_string();
    let branch = branch.to_string();
    std::thread::spawn(move || {
        let _ = std::process::Command::new("git")
            .args(["push", &remote, "--delete", &branch])
            .current_dir(&cwd)
            .output();
    });
}

/// P2: the branch is already rebased on origin/default by the 3-way merge, so
/// fast-forwarding the default branch ourselves makes GitHub mark the PR as
/// MERGED (head commits reachable from base). The merged SHA is known locally:
/// no server-side rebase, no mergeCommit polling, no extra fetch.
/// Legacy admin rebase merge, with 250ms exponential backoff instead of 2x5s sleeps.
fn merge_via_pr(
    shell: &dyn Shell,
    pr_url: &str,
    branch: &str,
    remote: &str,
    default: &str,
) -> Result<String> {
    let _p = trace::phase("merge.via_pr");
    let (state, oid) = pr_view(shell, pr_url)?;
    match state.as_str() {
        "MERGED" => {
            if let Some(sha) = oid {
                return Ok(sha);
            }
        }
        "CLOSED" => bail!("PR {pr_url} is CLOSED — not merging"),
        _ => {
            crate::ui::step("merging PR (--admin --rebase)...");
            let out = shell.run(&["gh", "pr", "merge", pr_url, "--admin", "--rebase"])?;
            if out.status != 0 {
                let (state2, oid2) = pr_view(shell, pr_url)?;
                if state2 != "MERGED" {
                    bail!(
                        "gh pr merge --admin failed (are you a repo admin?):\n{}{}",
                        out.stdout.trim(),
                        out.stderr.trim()
                    );
                }
                if let Some(sha) = oid2 {
                    spawn_branch_delete(shell.cwd(), remote, branch);
                    return Ok(sha);
                }
                crate::ui::warn("gh pr merge reported an error but the PR is MERGED.");
            }
        }
    }
    // gh pr merge waits for the server-side merge to complete. Read the merged SHA
    // over the git protocol (fetch) instead of polling the REST API: fewer GitHub
    // API calls, no sleeps, immune to API rate-limits.
    run_checked(shell, &["git", "fetch", remote, default])?;
    let sha = run_checked(shell, &["git", "rev-parse", &format!("{remote}/{default}")])?
        .stdout
        .trim()
        .to_string();
    if sha.is_empty() {
        bail!("could not read {remote}/{default} after merge");
    }
    spawn_branch_delete(shell.cwd(), remote, branch);
    Ok(sha)
}

fn ensure_on_branch(shell: &dyn Shell, m: &Manifest, remote: &str, default: &str) -> Result<()> {
    let branch = &m.delivery.branch;
    // No second fetch here: prepare_with already fetched.
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
            std::thread::sleep(Duration::from_millis(250));
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

fn committed_subjects(shell: &dyn Shell, base: &str) -> HashSet<String> {
    let out = shell.run(&["git", "log", "--format=%s", &format!("{base}..HEAD")]);
    match out {
        Ok(o) if o.status == 0 => o.stdout.lines().map(|l| l.trim().to_string()).collect(),
        _ => HashSet::new(),
    }
}

/// P0: N+1 spawns instead of 4N, ONE status scan instead of N.
/// `git add -A` once, then per-group pathspec commits (worktree content of the
/// named paths; other groups stay staged). Resume detection via one log read.
fn ensure_committed(shell: &dyn Shell, m: &Manifest, remote: &str, default: &str) -> Result<()> {
    let _p = trace::phase("cli.ensure_committed");
    let base = format!("{remote}/{default}");
    let subjects = committed_subjects(shell, &base);
    let total = m.commit.iter().filter(|g| !g.files.is_empty()).count();
    let pending: Vec<&crate::manifest::CommitGroup> = m
        .commit
        .iter()
        .filter(|g| {
            !g.files.is_empty() && !subjects.contains(g.message.lines().next().unwrap_or("").trim())
        })
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    run_checked(shell, &["git", "add", "-A"])?;
    let mut made = 0usize;
    for g in pending {
        let paths: Vec<String> = g
            .files
            .iter()
            .map(|f| f.to_string_lossy().into_owned())
            .collect();
        let mut argv: Vec<&str> = vec!["git", "commit", "--no-verify", "-m", &g.message, "--"];
        for p in &paths {
            argv.push(p);
        }
        let out = shell.run(&argv)?;
        if out.status != 0 {
            let combined = format!("{}{}", out.stdout, out.stderr);
            if combined.contains("nothing to commit") || combined.contains("no changes") {
                crate::ui::warn(&format!("nothing to commit for '{}', skipping", g.message));
                continue;
            }
            bail!(
                "git commit failed for '{}':\n{}{}",
                g.message,
                out.stdout.trim(),
                out.stderr.trim()
            );
        }
        made += 1;
        crate::ui::commit_step(made, total, &g.message);
    }
    Ok(())
}

fn ensure_pushed(shell: &dyn Shell, remote: &str, branch: &str) -> Result<()> {
    let _p = trace::phase("push");
    crate::ui::step("pushing branch...");
    run_checked(
        shell,
        &["git", "push", "--force-with-lease", remote, branch],
    )?;
    Ok(())
}

fn ensure_pr(shell: &dyn Shell, m: &Manifest, default: &str) -> Result<String> {
    let _p = trace::phase("pr");
    let branch = &m.delivery.branch;
    // ONE list call for all states (was two: open + closed).
    let listed = run_checked(
        shell,
        &[
            "gh",
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--limit",
            "5",
            "--json",
            "url,state",
        ],
    )?;
    let arr: serde_json::Value = serde_json::from_str(listed.stdout.trim())
        .map_err(|e| anyhow!("cannot parse gh pr list output: {e}"))?;
    if let Some(items) = arr.as_array() {
        for pr in items {
            let state = pr["state"].as_str().unwrap_or("");
            let url = pr["url"].as_str().unwrap_or("");
            if state == "OPEN" && !url.is_empty() {
                crate::ui::pr_reused(url);
                return Ok(url.to_string());
            }
            if !url.is_empty() {
                bail!("PR {url} exists for {branch} but is {state} — delete the branch and retry");
            }
        }
    }
    // Try GraphQL (persistent connection, no gh spawn); fall back to gh CLI.
    let body = std::fs::read_to_string(shell.cwd().join(&m.delivery.pr_body)).unwrap_or_default();
    match crate::github::Gh::new(shell) {
        Ok(gh) => match gh.create_pr(branch, default, &m.delivery.pr_title, &body) {
            Ok(url) => {
                crate::ui::pr_opened(&url);
                return Ok(url);
            }
            Err(e) => {
                crate::ui::warn(&format!(
                    "GraphQL PR create failed ({e:#}) — falling back to gh"
                ));
            }
        },
        Err(e) => {
            crate::ui::warn(&format!("GraphQL unavailable ({e:#}) — using gh"));
        }
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
    let _p = trace::phase("sync");
    run_checked(shell, &["git", "fetch", "--no-tags", remote, default])?;
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
    let head = head_sha(shell)?;
    if head != merged {
        bail!("after sync, HEAD is {head} but the merged SHA was {merged} — origin moved mid-run");
    }
    Ok(())
}

/// Public validate (used by `ship validate`): artifacts guard + tree check.
pub fn validate(
    m_orig: &Manifest,
    shell: &dyn Shell,
    base: &str,
) -> Result<(Manifest, Vec<PathBuf>, bool)> {
    guards::ship_artifacts_gitignored(
        shell,
        &PathBuf::from("ship.toml"),
        &PathBuf::from(&m_orig.delivery.pr_body),
    )?;
    validate_inner(m_orig, shell, base)
}

/// Single status scan (was is_dirty + dirty_paths back-to-back).
fn validate_inner(
    m_orig: &Manifest,
    shell: &dyn Shell,
    base: &str,
) -> Result<(Manifest, Vec<PathBuf>, bool)> {
    let dirty_now = guards::dirty_paths(shell)?;
    let resume_mode = dirty_now.is_empty();
    if resume_mode {
        let subjects = committed_subjects(shell, base);
        let all_committed = m_orig
            .commit
            .iter()
            .all(|g| subjects.contains(g.message.lines().next().unwrap_or("").trim()));
        if !all_committed {
            bail!(
                "working tree is clean and not all [[commit]] groups are on the branch — \
                 extract the zip into the repo, or check ship.toml"
            );
        }
    }

    let (expanded, drops) = m_orig.expand_globs(&dirty_now)?;
    if !resume_mode {
        report_drops(m_orig, &expanded, &drops);
        let mut manifest_paths = Vec::new();
        for g in &expanded.commit {
            manifest_paths.extend(g.files.iter().cloned());
        }
        guards::union_check(&dirty_now, &manifest_paths)?;
    }
    Ok((expanded, dirty_now, resume_mode))
}
