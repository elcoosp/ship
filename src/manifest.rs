use anyhow::{Context, Result, bail};
use globset::GlobBuilder;
use regex::Regex;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Deserialize, Clone)]
pub struct Manifest {
    pub delivery: Delivery,
    #[serde(default)]
    pub version: VersionSection,
    #[serde(default)]
    pub commit: Vec<CommitGroup>,
}

#[derive(Deserialize, Clone)]
pub struct Delivery {
    pub branch: String,
    pub pr_title: String,
    pub pr_body: String,
    pub base_commit: Option<String>,
    #[serde(default = "default_remote")]
    pub remote: String,
    pub base: Option<String>,
}

#[derive(Deserialize, Clone, Default)]
pub struct VersionSection {
    #[serde(default = "default_bump")]
    pub bump: String,
}

#[derive(Deserialize, Clone)]
pub struct CommitGroup {
    pub message: String,
    pub files: Vec<PathBuf>,
}

/// Per-group record of paths that were declared but not dirty
/// (dropped from the expanded manifest). Parallel to `commit`.
#[derive(Default, Clone, Debug)]
pub struct DropReport {
    pub per_group: Vec<Vec<PathBuf>>,
}

fn default_remote() -> String {
    "origin".into()
}
fn default_bump() -> String {
    "auto".into()
}

impl Manifest {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let m: Manifest = toml::from_str(&text).context("parsing ship.toml")?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<()> {
        let prefixes = [
            "feat", "fix", "docs", "refactor", "perf", "chore", "build", "test", "ci", "style",
            "revert",
        ];
        let (prefix, _) = self
            .delivery
            .branch
            .split_once('/')
            .with_context(|| format!("branch '{}' has no prefix/", self.delivery.branch))?;
        if !prefixes.contains(&prefix) {
            bail!("branch prefix '{}' not in {:?}", prefix, prefixes);
        }
        let re = Regex::new(
            r"^(feat|fix|docs|refactor|perf|chore|build|test|ci|style|revert)(\([^)]+\))?!?: .+$",
        )?;
        let mut seen: HashSet<&PathBuf> = HashSet::new();
        for (i, g) in self.commit.iter().enumerate() {
            let subject = g.message.lines().next().unwrap_or("");
            if subject.len() > 72 {
                bail!(
                    "[[commit]][{}] subject is {} chars (>72): {:?}",
                    i,
                    subject.len(),
                    subject
                );
            }
            if !re.is_match(subject) {
                bail!(
                    "[[commit]][{}] not a conventional subject: {:?}",
                    i,
                    subject
                );
            }
            for f in &g.files {
                if !seen.insert(f) {
                    bail!(
                        "path {} appears in more than one [[commit]] group",
                        f.display()
                    );
                }
            }
        }
        Ok(())
    }

    /// Expand every `files` entry against the actual dirty set.
    ///
    /// Rules:
    ///   • Literal path that IS dirty → included.
    ///   • Literal path that is NOT dirty → dropped (recorded in DropReport).
    ///     Resume-friendly: files already committed or missing from the zip
    ///     are skipped, not fatal.
    ///   • Glob that matches ≥1 dirty path → included.
    ///   • Glob that matches 0 dirty paths → dropped.
    ///   • Invalid glob syntax → fatal (typo in the pattern).
    ///   • Overlap across groups after expansion → fatal.
    ///
    /// A group with zero remaining files is skipped by the caller.
    pub fn expand_globs(&self, dirty: &[PathBuf]) -> Result<(Self, DropReport)> {
        let dirty_set: HashSet<&PathBuf> = dirty.iter().collect();
        let mut out = self.clone();
        let mut report = DropReport {
            per_group: vec![Vec::new(); self.commit.len()],
        };

        for (i, group) in out.commit.iter_mut().enumerate() {
            let mut expanded: Vec<PathBuf> = Vec::new();
            for pattern in &group.files {
                let s = pattern.to_string_lossy();
                if !has_glob_chars(&s) {
                    if dirty_set.contains(pattern) {
                        expanded.push(pattern.clone());
                    } else {
                        report.per_group[i].push(pattern.clone());
                    }
                    continue;
                }
                let matcher = GlobBuilder::new(&s)
                    .literal_separator(true)
                    .build()
                    .with_context(|| {
                        format!("invalid glob in [[commit]] '{}': {}", group.message, s)
                    })?
                    .compile_matcher();
                let before = expanded.len();
                for d in dirty {
                    if matcher.is_match(d) {
                        expanded.push(d.clone());
                    }
                }
                if expanded.len() == before {
                    report.per_group[i].push(pattern.clone());
                }
            }
            expanded.sort();
            expanded.dedup();
            group.files = expanded;
        }

        // Overlap after expansion (two globs hitting the same dirty file).
        let mut seen: HashSet<PathBuf> = HashSet::new();
        for (i, g) in out.commit.iter().enumerate() {
            for f in &g.files {
                if !seen.insert(f.clone()) {
                    bail!(
                        "path {} appears in multiple [[commit]] groups after glob expansion (check [[commit]][{i}] = {:?})",
                        f.display(),
                        g.message
                    );
                }
            }
        }
        Ok((out, report))
    }
}

fn has_glob_chars(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}
