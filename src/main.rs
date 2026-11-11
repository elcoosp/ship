use anyhow::Result;
use clap::{Parser, Subcommand};
use ship::manifest;
use ship::pipeline;
use ship::shell::RealShell;
use ship::watch::{self, WatchConfig};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about = "git-only milestone delivery pipeline")]
struct Cli {
    #[arg(long, default_value = "ship.toml")]
    manifest: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Commit, bump version, push, open PR, admin-merge, sync default.
    Prepare {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        reset: bool,
        /// Remove stale (>30s old) git lock files before running.
        #[arg(long)]
        clean_locks: bool,
    },
    /// Watch a directory for {project}-{slug}.zip and ship each one.
    Watch {
        #[arg(long)]
        watch_dir: Option<PathBuf>,
        #[arg(long)]
        repos_root: Option<PathBuf>,
        #[arg(long)]
        log_file: Option<PathBuf>,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        allow_repo_manifest: bool,
        #[arg(long)]
        clean_locks: bool,
        /// Force polling mode; skip notify/FSEvents entirely.
        #[arg(long)]
        poll: bool,
        /// Poll interval in milliseconds (default 2000).
        #[arg(long, default_value_t = 2000)]
        poll_interval_ms: u64,
    },
    /// List detected projects under repos_root.
    Projects {
        #[arg(long)]
        repos_root: Option<PathBuf>,
    },
    /// Remove zips from .processed to free disk space (keeps .failed).
    Clean {
        #[arg(long)]
        watch_dir: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Cmd::Watch {
        watch_dir,
        repos_root,
        log_file,
        once,
        allow_repo_manifest,
        clean_locks,
        poll,
        poll_interval_ms,
    } = &cli.cmd
    {
        let mut cfg = WatchConfig::default();
        if let Some(p) = watch_dir {
            cfg.watch_dir = p.clone();
        }
        if let Some(p) = repos_root {
            cfg.repos_root = p.clone();
        }
        if let Some(p) = log_file {
            cfg.log_file = Some(p.clone());
        }
        cfg.allow_repo_manifest = *allow_repo_manifest;
        cfg.clean_locks = *clean_locks;
        cfg.poll_only = *poll;
        cfg.poll_interval = Duration::from_millis(*poll_interval_ms);
        return watch::run(cfg, *once);
    }

    if let Cmd::Projects { repos_root } = &cli.cmd {
        let root = repos_root.clone().unwrap_or_else(watch::default_repos_root);
        let projects = watch::list_projects(&root)?;
        ship::ui::projects_table(&root, &projects);
        return Ok(());
    }

    if let Cmd::Clean { watch_dir } = &cli.cmd {
        let dir = watch_dir.clone().unwrap_or_else(watch::default_watch_dir);
        watch::clean_processed(&dir)?;
        return Ok(());
    }

    let manifest = manifest::Manifest::load(&cli.manifest)?;
    let shell = RealShell::new(std::env::current_dir()?);

    let outcome = match cli.cmd {
        Cmd::Prepare {
            dry_run,
            reset,
            clean_locks,
        } => pipeline::prepare(dry_run, reset, clean_locks, &manifest, &shell)?,
        Cmd::Watch { .. } | Cmd::Projects { .. } | Cmd::Clean { .. } => unreachable!(),
    };

    if let Some(u) = outcome.pr_url.as_deref() {
        ship::ui::machine("PR_URL", u);
    }
    if let Some(s) = outcome.merged.as_deref() {
        ship::ui::machine("MERGED", s);
    }

    ship::ui::final_success(outcome.pr_url.as_deref(), outcome.merged.as_deref());

    Ok(())
}
