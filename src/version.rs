use crate::manifest::Manifest;
use crate::shell::{Shell, run_checked};
use anyhow::{Context, Result, bail};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BumpLevel {
    Patch,
    Minor,
    Major,
}

pub fn derive_bump(m: &Manifest) -> BumpLevel {
    let mut level = BumpLevel::Patch;
    for g in &m.commit {
        let subject = g.message.lines().next().unwrap_or("");
        if subject.contains("!:") || g.message.contains("BREAKING CHANGE:") {
            return BumpLevel::Major;
        }
        if subject.starts_with("feat") {
            level = BumpLevel::Minor;
        }
    }
    level
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn parse(s: &str) -> Result<Self> {
        let core = s.split('-').next().unwrap_or(s);
        let mut it = core.split('.');
        let major = it.next().context("version major")?.parse()?;
        let minor = it.next().context("version minor")?.parse()?;
        let patch = it.next().context("version patch")?.parse()?;
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    pub fn bump(&self, level: BumpLevel) -> Self {
        match level {
            BumpLevel::Major => Self {
                major: self.major + 1,
                minor: 0,
                patch: 0,
            },
            BumpLevel::Minor => Self {
                major: self.major,
                minor: self.minor + 1,
                patch: 0,
            },
            BumpLevel::Patch => Self {
                major: self.major,
                minor: self.minor,
                patch: self.patch + 1,
            },
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

pub fn read_workspace_version(cargo_toml: &Path) -> Result<Version> {
    let text = std::fs::read_to_string(cargo_toml)
        .with_context(|| format!("reading {}", cargo_toml.display()))?;
    let doc: toml::Value = toml::from_str(&text)?;
    let ver = doc
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .with_context(|| format!("no [workspace.package].version in {}", cargo_toml.display()))?;
    Version::parse(ver)
}

pub fn read_workspace_version_at(shell: &dyn Shell, rev: &str, path: &str) -> Result<Version> {
    let out = run_checked(shell, &["git", "show", &format!("{rev}:{path}")])?;
    let doc: toml::Value = toml::from_str(&out.stdout)?;
    let ver = doc
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .with_context(|| format!("no [workspace.package].version at {rev}:{path}"))?;
    Version::parse(ver)
}

/// Returns Ok(None) when the file doesn't exist at that rev (fresh repo).
pub fn read_workspace_version_at_opt(
    shell: &dyn Shell,
    rev: &str,
    path: &str,
) -> Result<Option<Version>> {
    let out = shell.run(&["git", "show", &format!("{rev}:{path}")])?;
    if out.status != 0 {
        return Ok(None);
    }
    let doc: toml::Value = toml::from_str(&out.stdout)?;
    let ver = doc
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str());
    match ver {
        Some(v) => Ok(Some(Version::parse(v)?)),
        None => Ok(None),
    }
}

pub fn assert_forward(old: Version, new: Version) -> Result<()> {
    if new <= old {
        bail!("new version {} is not > old version {}", new, old);
    }
    Ok(())
}
