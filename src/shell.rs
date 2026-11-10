use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait Shell {
    fn run(&self, argv: &[&str]) -> Result<Output>;
    fn cwd(&self) -> &std::path::Path;
}

/// Runs commands in `cwd`. Never mutates process state beyond spawning.
pub struct RealShell {
    cwd: PathBuf,
}

impl RealShell {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

impl Shell for RealShell {
    fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }
    fn run(&self, argv: &[&str]) -> Result<Output> {
        if argv.is_empty() {
            bail!("empty argv");
        }
        let out = Command::new(argv[0])
            .args(&argv[1..])
            .current_dir(&self.cwd)
            .output()
            .with_context(|| format!("executing {:?}", argv))?;
        Ok(Output {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Fail on non-zero exit. `argv` is shown in the error message.
pub fn run_checked(shell: &dyn Shell, argv: &[&str]) -> Result<Output> {
    let out = shell.run(argv)?;
    if out.status != 0 {
        bail!("command failed: {}\n{}", argv.join(" "), out.stderr.trim());
    }
    Ok(out)
}

/// Success/failure without an error on failure (for `git rev-parse --verify`).
pub fn run_ok(shell: &dyn Shell, argv: &[&str]) -> Result<bool> {
    Ok(shell.run(argv)?.status == 0)
}
