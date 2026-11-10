use clap::{Parser, Subcommand};
use ship::manifest;
use ship::pipeline::{self, Outcome};
use ship::shell::RealShell;
use ship::watch::{self, WatchConfig};
use std::path::PathBuf;

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
    Prepare {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        reset: bool,
        /// Remove stale (>30s old) git lock files before running.
        #[arg(long)]
        clean_locks: bool,
    },
    Tag {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        clean_locks: bool,
    },
    All {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        reset: bool,
        #[arg(long)]
        clean_locks: bool,
    },
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
    },
    Projects {
        #[arg(long)]
        repos_root: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Init the log sink for every subcommand. Watch overrides with its own
    // --log-file when present (OnceLock::set is idempotent — first wins, so
    // watch's init happens inside watch::run BEFORE any other path).
    let default_log = std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".local/state/ship/watcher.log"));
    ship::ui::init_log(default_log);

    if let Cmd::Watch {
        watch_dir,
        repos_root,
        log_file,
        once,
        allow_repo_manifest,
        clean_locks,
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
        return watch::run(cfg, *once);
    }

    if let Cmd::Projects { repos_root } = &cli.cmd {
        let root = repos_root.clone().unwrap_or_else(watch::default_repos_root);
        let projects = watch::list_projects(&root)?;
        ship::ui::projects_table(&root, &projects);
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
        Cmd::Tag {
            dry_run,
            clean_locks,
        } => pipeline::tag(dry_run, clean_locks, &manifest, &shell)?,
        Cmd::All {
            dry_run,
            reset,
            clean_locks,
        } => {
            let p = pipeline::prepare(dry_run, reset, clean_locks, &manifest, &shell)?;
            if dry_run {
                p
            } else {
                let t = pipeline::tag(false, clean_locks, &manifest, &shell)?;
                Outcome {
                    pr_url: p.pr_url.or(t.pr_url),
                    merged: p.merged.or(t.merged),
                    tag: t.tag,
                }
            }
        }
        Cmd::Watch { .. } | Cmd::Projects { .. } => unreachable!(),
    };

    if cli.cmd_is_prepare_like() {
        ship::ui::final_success(
            outcome.pr_url.as_deref(),
            outcome.merged.as_deref(),
            outcome.tag.as_deref(),
        );
    } else {
        // tag command: no PR panel, just the tag
        if let Some(t) = outcome.tag.as_deref() {
            ship::ui::tagged(t, true);
        }
    }
    Ok(())
}

// Helper so main() can branch on the command without re-matching.
impl Cli {
    fn cmd_is_prepare_like(&self) -> bool {
        matches!(self.cmd, Cmd::Prepare { .. } | Cmd::All { .. })
    }
}
