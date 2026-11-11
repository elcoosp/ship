use crate::guards;
use crate::manifest::Manifest;
use crate::pipeline;
use crate::shell::{RealShell, Shell, run_checked};
use crate::ui;
use anyhow::{Context, Result, bail};
use notify::{EventKind, RecursiveMode, Watcher as _};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct WatchConfig {
    pub watch_dir: PathBuf,
    pub repos_root: PathBuf,
    pub processed_dir: PathBuf,
    pub failed_dir: PathBuf,
    pub log_file: Option<PathBuf>,
    pub settle: Duration,
    pub allow_repo_manifest: bool,
    pub clean_locks: bool,
    pub poll_only: bool,
    pub poll_interval: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        let watch_dir = default_watch_dir();
        let state = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
            .join(".local/state/ship");
        Self {
            processed_dir: watch_dir.join(".processed"),
            failed_dir: watch_dir.join(".failed"),
            watch_dir,
            repos_root: default_repos_root(),
            log_file: Some(state.join("watcher.log")),
            settle: Duration::from_secs(2),
            allow_repo_manifest: false,
            clean_locks: false,
            poll_only: false,
            poll_interval: Duration::from_secs(2),
        }
    }
}

pub fn default_watch_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SHIP_WATCH_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join("Downloads")
}

pub fn default_repos_root() -> PathBuf {
    if let Ok(p) = std::env::var("SHIP_REPOS_ROOT") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let candidates = [
        PathBuf::from(&home).join("Documents/Repos"),
        PathBuf::from(&home).join("Documents/repos"),
        PathBuf::from(&home).join("Repos"),
        PathBuf::from(&home).join("repos"),
    ];
    for c in &candidates {
        if c.is_dir() {
            return c.clone();
        }
    }
    candidates[0].clone()
}

pub fn list_projects(repos_root: &Path) -> Result<Vec<String>> {
    let read = std::fs::read_dir(repos_root)
        .with_context(|| format!("cannot read repos_root {}", repos_root.display()))?;
    let mut projects = Vec::new();
    for entry in read.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            projects.push(name.to_string());
        }
    }
    projects.sort();
    Ok(projects)
}

pub fn clean_processed(watch_dir: &Path) -> Result<()> {
    let processed_dir = watch_dir.join(".processed");
    if !processed_dir.exists() {
        crate::ui::info("Nothing to clean: .processed folder does not exist.");
        return Ok(());
    }

    let mut count = 0;
    let mut bytes = 0u64;

    for entry in std::fs::read_dir(&processed_dir)?.flatten() {
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            if let Ok(meta) = entry.metadata() {
                bytes += meta.len();
            }
            if std::fs::remove_file(entry.path()).is_ok() {
                count += 1;
            }
        }
    }

    crate::ui::print_clean_summary(count, bytes, &processed_dir);
    Ok(())
}

fn clean_old_processed(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
                && let Ok(age) = now.duration_since(modified)
                && age.as_secs() > 7 * 24 * 3600
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

pub fn run(cfg: WatchConfig, once: bool) -> Result<()> {
    std::fs::create_dir_all(&cfg.watch_dir).context("create watch_dir")?;
    std::fs::create_dir_all(&cfg.processed_dir)?;
    clean_old_processed(&cfg.processed_dir);
    std::fs::create_dir_all(&cfg.failed_dir)?;
    if let Some(p) = &cfg.log_file
        && let Some(parent) = p.parent()
    {
        std::fs::create_dir_all(parent)?;
    }
    ui::init_log(cfg.log_file.clone());
    let _lock = acquire_lock(&cfg.watch_dir.join(".ship-watcher.pid"))?;

    ui::banner_startup();

    let projects = list_projects(&cfg.repos_root).unwrap_or_default();
    let mut flags: Vec<&str> = Vec::new();
    if cfg.clean_locks {
        flags.push("clean_locks");
    }
    if cfg.allow_repo_manifest {
        flags.push("allow_repo_manifest");
    }
    if cfg.poll_only {
        flags.push("poll");
    }

    ui::context(&cfg.watch_dir, &cfg.repos_root, &projects, &flags);
    if projects.is_empty() {
        ui::warn("no projects found — pass --repos-root DIR or set SHIP_REPOS_ROOT");
    }
    ui::ready();

    let start = Instant::now();
    let shipped = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let mut seen: HashSet<PathBuf> = HashSet::new();

    let zips = list_zips(&cfg.watch_dir)?;
    if !zips.is_empty() {
        for zip in zips {
            seen.insert(zip.clone());
            process_zip(&cfg, &zip, Arc::clone(&shipped), Arc::clone(&failed));
        }
        if once {
            let mut waited = 0;
            while ACTIVE_COUNT.load(Ordering::Relaxed) > 0 && waited < 300 {
                std::thread::sleep(Duration::from_millis(500));
                waited += 1;
            }
            let s = shipped.load(Ordering::Relaxed);
            let f = failed.load(Ordering::Relaxed);
            if s > 0 || f > 0 {
                ui::final_summary(s, f, start.elapsed());
            }
        }
        if once {
            return Ok(());
        }
    }

    if cfg.poll_only {
        ui::info(&format!(
            "poll mode active (interval {:?}) — drop a zip to ship",
            cfg.poll_interval
        ));
        return poll_loop(&cfg, seen, shipped, failed);
    }

    let (tx, rx) = channel();
    match notify::recommended_watcher(move |ev| {
        let _ = tx.send(ev);
    }) {
        Ok(mut watcher) => match watcher.watch(&cfg.watch_dir, RecursiveMode::NonRecursive) {
            Ok(()) => return debounce_loop(&cfg, rx, seen, shipped, failed),
            Err(e) => ui::warn(&format!(
                "notify watch failed ({e}) — falling back to polling"
            )),
        },
        Err(e) => ui::warn(&format!(
            "notify init failed ({e}) — falling back to polling"
        )),
    }
    poll_loop(&cfg, seen, shipped, failed)
}

fn poll_loop(
    cfg: &WatchConfig,
    mut seen: HashSet<PathBuf>,
    shipped: Arc<AtomicUsize>,
    failed: Arc<AtomicUsize>,
) -> Result<()> {
    loop {
        match list_zips(&cfg.watch_dir) {
            Ok(zips) => {
                for zip in zips {
                    if seen.contains(&zip) {
                        continue;
                    }
                    if !zip.exists() {
                        seen.insert(zip);
                        continue;
                    }
                    seen.insert(zip.clone());
                    process_zip(cfg, &zip, Arc::clone(&shipped), Arc::clone(&failed));
                }
            }
            Err(e) => ui::warn(&format!("list_zips failed: {e:#}")),
        }
        std::thread::sleep(cfg.poll_interval);
    }
}

fn debounce_loop(
    cfg: &WatchConfig,
    rx: Receiver<notify::Result<notify::Event>>,
    mut seen: HashSet<PathBuf>,
    shipped: Arc<AtomicUsize>,
    failed: Arc<AtomicUsize>,
) -> Result<()> {
    let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
    loop {
        if let Ok(Ok(event)) = rx.recv_timeout(Duration::from_millis(200)) {
            let relevant = matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_));
            if !relevant {
                continue;
            }
            for p in event.paths {
                if !is_zip(&p) {
                    continue;
                }
                if p.parent() != Some(cfg.watch_dir.as_path()) {
                    continue;
                }
                if p.file_name()
                    .map(|n| n.to_string_lossy().starts_with('.'))
                    .unwrap_or(false)
                {
                    continue;
                }
                pending.insert(p, Instant::now());
            }
        }
        let now = Instant::now();
        let settled: Vec<PathBuf> = pending
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= cfg.settle)
            .map(|(p, _)| p.clone())
            .collect();
        for zip in settled {
            pending.remove(&zip);
            if seen.contains(&zip) {
                continue;
            }
            if !zip.exists() {
                seen.insert(zip);
                continue;
            }
            seen.insert(zip.clone());
            process_zip(cfg, &zip, Arc::clone(&shipped), Arc::clone(&failed));
        }
    }
}

static ACTIVE_REPOS: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);
static ACTIVE_COUNT: AtomicUsize = AtomicUsize::new(0);

fn process_zip(
    cfg: &WatchConfig,
    zip: &Path,
    shipped: Arc<AtomicUsize>,
    failed: Arc<AtomicUsize>,
) -> bool {
    if !zip.exists() {
        return false;
    }
    let name = zip
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| zip.display().to_string());
    let stem = match zip.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => {
            ui::skip(&format!("{name}: bad filename"));
            return false;
        }
    };

    let project = match resolve_project(&cfg.repos_root, &stem) {
        Ok(Some((p, _slug))) => p,
        Ok(None) => {
            ui::skip(&format!("{name}: no project matches '{stem}'"));
            return false;
        }
        Err(e) => {
            ui::error_panel(&name, &format!("{e:#}"), &[]);
            move_to(&cfg.failed_dir, zip, &name);
            return false;
        }
    };

    let repo = cfg.repos_root.join(&project);

    {
        let mut guard = ACTIVE_REPOS.lock().unwrap();
        let active = guard.get_or_insert_with(HashSet::new);
        if active.contains(&repo) {
            ui::skip(&format!(
                "{name}: skipping, {} is already being processed",
                project
            ));
            return false;
        }
        active.insert(repo.clone());
    }

    ACTIVE_COUNT.fetch_add(1, Ordering::Relaxed);

    let zip = zip.to_path_buf();
    let cfg_processed = cfg.processed_dir.clone();
    let cfg_failed = cfg.failed_dir.clone();
    let name_clone = name.clone();
    let project_clone = project.clone();
    let repo_clone = repo.clone();

    std::thread::spawn(move || {
        crate::ui::start_task(&project_clone);

        let wrapper = match detect_wrapper_prefix(&zip) {
            Ok(w) => w,
            Err(e) => {
                let recovery = recovery_for(&format!("{e:#}"), &project_clone, &zip);
                ui::error_panel(&name_clone, &format!("{e:#}"), &recovery);
                move_to(&cfg_failed, &zip, &name_clone);
                notify(
                    &format!("ship failed: {}", project_clone),
                    "Check the terminal for details",
                    None,
                    true,
                );
                failed.fetch_add(1, Ordering::Relaxed);
                ACTIVE_REPOS
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .remove(&repo_clone);
                ACTIVE_COUNT.fetch_sub(1, Ordering::Relaxed);
                return;
            }
        };

        let started = std::time::Instant::now();
        match process_zip_inner(&repo_clone, &zip, &name_clone, wrapper.as_deref()) {
            Ok(outcome) => {
                let pr_url = outcome.pr_url.as_deref();
                ui::section_ok(&project_clone, pr_url, started.elapsed());
                move_to(&cfg_processed, &zip, &name_clone);

                let title = format!("ship succeeded: {}", project_clone);
                let body = match pr_url {
                    Some(url) => format!("Merged: {}", url),
                    None => "Merged".to_string(),
                };
                notify(&title, &body, outcome.pr_url.as_deref(), false);

                shipped.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                let err = format!("{e:#}");
                let recovery = recovery_for(&err, &project_clone, &zip);
                ui::error_panel(&name_clone, &err, &recovery);
                move_to(&cfg_failed, &zip, &name_clone);
                notify(
                    &format!("ship failed: {}", project_clone),
                    "Check the terminal for details",
                    None,
                    true,
                );
                failed.fetch_add(1, Ordering::Relaxed);
            }
        }

        ACTIVE_REPOS
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .remove(&repo_clone);
        ACTIVE_COUNT.fetch_sub(1, Ordering::Relaxed);
    });

    true
}

fn recovery_for(err: &str, project: &str, zip: &Path) -> Vec<String> {
    let zip_name = zip
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<zip>")
        .to_string();
    let repo = format!("~/Documents/Repos/{project}");
    let failed_dir = "~/Downloads/.failed";

    if err.contains("ship.toml ↔ working tree mismatch")
        || err.contains("invalid [version].bump")
        || err.contains("not a conventional subject")
    {
        return vec![
            format!("cd {repo}"),
            format!("unzip -o {failed_dir}/{zip_name} -d ."),
            "edit ship.toml (fix the file paths or commit messages)".into(),
            "ship prepare".into(),
        ];
    }
    if err.contains("is TRACKED") || err.contains("not gitignored") {
        return vec![
            format!("cd {repo}"),
            "git rm --cached .github/pr-body.md  # and/or: ship.toml".into(),
            format!("mv {failed_dir}/{zip_name} ~/Downloads/"),
            "ship watch --once".into(),
        ];
    }
    if err.contains("git lock files present") {
        return vec![
            format!("cd {repo}"),
            "ls -la .git/*.lock   # confirm no git process is running".into(),
            "rm .git/*.lock".into(),
            format!("mv {failed_dir}/{zip_name} ~/Downloads/"),
            "ship watch --once --clean-locks".into(),
        ];
    }
    if err.contains("no project matches") {
        return vec!["check the zip name: it must be <project>.zip or <project>-<slug>.zip".into()];
    }
    if err.contains("is not a git repository") || err.contains("not a git repository") {
        return vec![
            format!("cd {repo}"),
            "git init -b main".into(),
            "git remote add origin <url>".into(),
            "git fetch origin && git reset --hard origin/main".into(),
            format!("mv {failed_dir}/{zip_name} ~/Downloads/"),
            "ship watch --once".into(),
        ];
    }
    if err.contains("not on main") || err.contains("switch to main") {
        return vec![
            format!("cd {repo}"),
            "git switch main".into(),
            format!("mv {failed_dir}/{zip_name} ~/Downloads/"),
            "ship watch --once".into(),
        ];
    }
    if err.contains("ahead of") && err.contains("commit") {
        return vec![
            format!("cd {repo}"),
            "git push origin main   # or: git reset --hard origin/main".into(),
            format!("mv {failed_dir}/{zip_name} ~/Downloads/"),
            "ship watch --once".into(),
        ];
    }
    if err.contains("tag v") && err.contains("already exists") {
        return vec![
            format!("cd {repo}"),
            "git tag -l   # see what's already there".into(),
            "manually choose the next version, or delete the stray tag".into(),
        ];
    }
    if err.contains("Operation not permitted") || err.contains("FSEvents") {
        return vec!["ship watch --poll".into()];
    }
    if err.contains("ship.toml") && err.contains("not found") {
        return vec![
            "the zip is missing ship.toml — regenerate it with the manifest included".into(),
        ];
    }
    Vec::new()
}

fn process_zip_inner(
    repo: &Path,
    zip: &Path,
    name: &str,
    wrapper: Option<&str>,
) -> Result<crate::pipeline::Outcome> {
    let detail = wrapper.map(|w| format!("strip wrapper '{w}/'"));
    ui::section_begin(name, detail.as_deref());

    let shell = RealShell::new(repo.to_path_buf());
    if !guards::is_git_repo_root(&shell, repo)? {
        bail!("{} is not a git repository root", repo.display());
    }

    (|| -> Result<crate::pipeline::Outcome> {
        let default = guards::default_branch(&shell, "origin")?;
        guards::assert_on_main_and_in_sync(&shell, "origin", &default)?;
        guards::ensure_no_git_locks(&shell, false)?;

        let stashed = guards::is_dirty(&shell)?;
        if stashed {
            run_checked(
                &shell,
                &[
                    "git",
                    "stash",
                    "push",
                    "-u",
                    "-m",
                    &format!("ship: wip before {}", zip.display()),
                ],
            )?;
            ui::info("WIP stashed");
        }

        if let Err(e) = run_checked(
            &shell,
            &["git", "pull", "--ff-only", "--quiet", "origin", &default],
        ) {
            if stashed {
                let _ = run_checked(&shell, &["git", "stash", "pop"]);
            }
            return Err(e.context("git pull --ff-only failed — WIP restored"));
        }

        // 1. Extract to temp dir to read the manifest
        let tmp = mktemp_dir()?;
        run_checked(
            &shell,
            &[
                "unzip",
                "-q",
                "-o",
                &zip.to_string_lossy(),
                "-d",
                &tmp.to_string_lossy(),
            ],
        )?;

        let source_dir = if let Some(prefix) = wrapper {
            let p = tmp.join(prefix);
            if !p.is_dir() {
                let _ = std::fs::remove_dir_all(&tmp);
                bail!("expected {}/ inside zip", prefix);
            }
            p
        } else {
            tmp.clone()
        };

        let manifest_path = source_dir.join("ship.toml");
        if !manifest_path.exists() {
            let _ = std::fs::remove_dir_all(&tmp);
            bail!("ship.toml is missing from the root of the extracted repo.");
        }
        let manifest = Manifest::load(&manifest_path)?;

        // 2. Apply the tree (3-way merge or blind overwrite)
        if let Some(base) = &manifest.delivery.base_commit {
            let short_base = if base.len() > 7 { &base[..7] } else { base };
            ui::info(&format!("3-way merge enabled (base: {})", short_base));
            let ai_branch = "ship-temp-ai";

            // Create AI snapshot branch
            run_checked(&shell, &["git", "switch", "-C", ai_branch, base])?;
            run_checked(&shell, &["git", "rm", "-r", "-f", "."])?;
            copy_dir_contents(&source_dir, repo)?;
            run_checked(&shell, &["git", "add", "-A"])?;
            let _ = shell.run(&["git", "commit", "-m", "AI delivery snapshot"]);

            // Merge into feature branch
            let branch = &manifest.delivery.branch;
            run_checked(
                &shell,
                &["git", "switch", "-C", branch, &format!("origin/{default}")],
            )?;
            let merge = shell.run(&["git", "merge", ai_branch, "--no-commit", "--no-ff"])?;
            // Unstage the merge so ensure_committed can group the files correctly.
            let _ = shell.run(&["git", "reset"]);
            if merge.status != 0 {
                let _ = shell.run(&["git", "merge", "--abort"]);
                let _ = shell.run(&["git", "branch", "-D", ai_branch]);
                let _ = std::fs::remove_dir_all(&tmp);
                bail!(
                    "Merge conflict! The AI's zip touches the same lines as your recent pushes.\nPlease resolve manually or regenerate the zip from the latest main."
                );
            }
            let _ = shell.run(&["git", "branch", "-D", ai_branch]);

            // Re-copy untracked config files (ship.toml, pr-body) that were removed by `git rm`
            let _ = std::fs::copy(source_dir.join("ship.toml"), repo.join("ship.toml"));
            let _ = std::fs::create_dir_all(repo.join(".github"));
            let _ = std::fs::copy(
                source_dir.join(".github/pr-body.md"),
                repo.join(".github/pr-body.md"),
            );
        } else {
            ui::warn("No base_commit in ship.toml. Blind overwrite. Pushed commits may be lost.");
            let branch = &manifest.delivery.branch;
            run_checked(
                &shell,
                &["git", "switch", "-C", branch, &format!("origin/{default}")],
            )?;
            copy_dir_contents(&source_dir, repo)?;
        }

        let _ = std::fs::remove_dir_all(&tmp);

        // 3. Revert mode changes
        let reverted = crate::normalize::exec_bit(&shell)?;
        if reverted > 0 {
            ui::warn(&format!(
                "reverted spurious mode changes on {reverted} tracked file(s)"
            ));
        }

        // 4. Prepare
        let outcome = pipeline::prepare(false, false, false, &manifest, &shell)?;

        if stashed {
            if run_checked(&shell, &["git", "stash", "pop"]).is_ok() {
                ui::info("WIP restored");
            } else {
                ui::warn("WIP conflict — stash preserved; resolve in tree, then: git stash drop");
            }
        }
        Ok(outcome)
    })()
}

fn mktemp_dir() -> Result<PathBuf> {
    let out = std::process::Command::new("mktemp").arg("-d").output()?;
    if !out.status.success() {
        bail!("mktemp -d failed");
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        bail!("mktemp -d returned empty path");
    }
    Ok(PathBuf::from(p))
}

fn copy_dir_contents(src: &Path, dest: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let sp = entry.path();
        let dp = dest.join(entry.file_name());
        if ft.is_dir() {
            std::fs::create_dir_all(&dp)?;
            copy_dir_contents(&sp, &dp)?;
        } else if ft.is_file() {
            if let Some(parent) = dp.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&sp, &dp)
                .with_context(|| format!("copying {} → {}", sp.display(), dp.display()))?;
        }
    }
    Ok(())
}

fn detect_wrapper_prefix(zip: &Path) -> Result<Option<String>> {
    let entries = zip_entries(zip)?;
    let normalized: Vec<&str> = entries
        .iter()
        .map(|e| e.strip_prefix("./").unwrap_or(e.as_str()))
        .collect();
    if normalized
        .iter()
        .any(|e| *e == ".git/" || e.contains("/.git/") || e.starts_with(".git/"))
    {
        bail!("zip contains .git — refusing to extract");
    }
    if normalized.contains(&"ship.toml") {
        return Ok(None);
    }
    let candidates: Vec<&str> = normalized
        .iter()
        .filter_map(|e| {
            e.strip_suffix("/ship.toml")
                .filter(|p| !p.is_empty() && !p.contains('/'))
        })
        .collect();
    match candidates.len() {
        0 => {
            let preview: Vec<String> = normalized
                .iter()
                .take(20)
                .map(|e| format!("    {e}"))
                .collect();
            let more = if normalized.len() > 20 {
                format!("\n    … ({} more)", normalized.len() - 20)
            } else {
                String::new()
            };
            bail!(
                "zip has no ship.toml at the archive root or one level deep.\nZip contains {} entries; first 20:\n{}\n{}",
                normalized.len(),
                preview.join("\n"),
                more
            );
        }
        1 => Ok(Some(candidates[0].to_string())),
        _ => bail!(
            "zip has multiple ship.toml under different dirs: {:?}",
            candidates
        ),
    }
}

fn zip_entries(zip: &Path) -> Result<Vec<String>> {
    let out = std::process::Command::new("unzip")
        .args(["-Z1", &zip.to_string_lossy()])
        .output()
        .context("running unzip -Z1")?;
    if !out.status.success() {
        bail!(
            "unzip -Z1 failed (corrupt or not a zip?): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.ends_with('/'))
        .collect())
}

fn resolve_project(repos_root: &Path, stem: &str) -> Result<Option<(String, String)>> {
    let projects = list_projects(repos_root)?;
    let mut best: Option<(String, String)> = None;
    for project in &projects {
        if let Some(slug) = match_project(stem, project) {
            let replace = match &best {
                None => true,
                Some((b, _)) => project.len() > b.len(),
            };
            if replace {
                best = Some((project.clone(), slug));
            }
        }
    }
    Ok(best)
}

fn match_project(stem: &str, project: &str) -> Option<String> {
    if stem == project {
        return Some(String::new());
    }
    let prefix = format!("{project}-");
    if let Some(slug) = stem.strip_prefix(&prefix) {
        return Some(slug.to_string());
    }
    let stem_lower = stem.to_ascii_lowercase();
    let project_lower = project.to_ascii_lowercase();
    if stem_lower == project_lower {
        return Some(String::new());
    }
    let prefix_lower = format!("{project_lower}-");
    if stem_lower.starts_with(&prefix_lower) {
        return Some(stem[prefix_lower.len()..].to_string());
    }
    None
}

fn move_to(dir: &Path, zip: &Path, name: &str) {
    if !zip.exists() {
        return;
    }
    let _ = std::fs::rename(zip, dir.join(name));
}

fn list_zips(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        if e.file_type().map(|t| t.is_file()).unwrap_or(false) && is_zip(&e.path()) {
            out.push(e.path());
        }
    }
    Ok(out)
}

fn is_zip(p: &Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
}

fn notify(title: &str, body: &str, pr_url: Option<&str>, is_error: bool) {
    let sound = if is_error { "Basso" } else { "Glass" };

    #[cfg(target_os = "macos")]
    {
        // Check if terminal-notifier is installed for clickable notifications
        let has_tn = std::process::Command::new("which")
            .arg("terminal-notifier")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if has_tn {
            let mut cmd = std::process::Command::new("terminal-notifier");
            cmd.args([
                "-title",
                title,
                "-message",
                body,
                "-sound",
                sound,
                "-subtitle",
                "ship pipeline",
            ]);
            if let Some(url) = pr_url {
                cmd.args(["-open", url]);
            }
            let _ = cmd.output();
        } else {
            // Fallback to AppleScript
            let script = format!(
                r#"display notification {:?} with title {:?} subtitle "ship pipeline" sound name {:?}"#,
                body, title, sound
            );
            let _ = std::process::Command::new("osascript")
                .args(["-e", &script])
                .output();
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut cmd = std::process::Command::new("notify-send");
        if is_error {
            cmd.args(["-u", "critical"]);
        }
        cmd.args([title, body]);
        let _ = cmd.output();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, body, pr_url, is_error);
    }
}

struct PidLock {
    path: PathBuf,
}
impl Drop for PidLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_lock(path: &Path) -> Result<PidLock> {
    if let Ok(s) = std::fs::read_to_string(path)
        && let Ok(pid) = s.trim().parse::<u32>()
        && process_alive(pid)
    {
        bail!("another ship watch is running (pid {pid})");
    }
    std::fs::write(path, std::process::id().to_string())?;
    Ok(PidLock {
        path: path.to_path_buf(),
    })
}

fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
