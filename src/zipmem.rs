//! In-memory zip reading (zip 9.x): no mktemp, no unzip spawn, no disk copy.
use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: String,
    pub bytes: Vec<u8>,
    pub exec: bool,
    pub symlink: bool,
}

fn normalize(raw: &str) -> String {
    raw.strip_prefix("./").unwrap_or(raw).to_string()
}

fn is_git_path(p: &str) -> bool {
    p == ".git" || p.starts_with(".git/") || p.contains("/.git/") || p.ends_with("/.git")
}

/// File entry names (directories excluded), normalized. Replaces `unzip -Z1`.
pub fn list_names(zip: &Path) -> Result<Vec<String>> {
    let f = std::fs::File::open(zip).with_context(|| format!("opening {}", zip.display()))?;
    let mut ar = zip::ZipArchive::new(f)
        .with_context(|| format!("reading zip {} (corrupt?)", zip.display()))?;
    let mut out = Vec::new();
    for i in 0..ar.len() {
        let zf = ar.by_index(i).context("zip entry")?;
        if zf.is_dir() {
            continue;
        }
        let name = normalize(&zf.name().context("zip entry name")?);
        if name.is_empty() {
            continue;
        }
        out.push(name);
    }
    Ok(out)
}

/// Read all file entries into memory, stripping the wrapper prefix if any.
pub fn read_entries(zip: &Path, wrapper: Option<&str>) -> Result<Vec<Entry>> {
    let f = std::fs::File::open(zip).with_context(|| format!("opening {}", zip.display()))?;
    let mut ar = zip::ZipArchive::new(f)
        .with_context(|| format!("reading zip {} (corrupt?)", zip.display()))?;
    let prefix = wrapper.map(|w| format!("{w}/"));
    let mut out = Vec::new();
    for i in 0..ar.len() {
        let mut zf = match ar.by_index(i) {
            Ok(z) => z,
            Err(e) => bail!("{}: zip entry {i}: {e}", crate::engine::UNSUPPORTED),
        };
        if zf.is_dir() {
            continue;
        }
        let raw = match zf.name() {
            Ok(n) => normalize(&n),
            Err(e) => bail!("{}: zip entry name {i}: {e}", crate::engine::UNSUPPORTED),
        };
        if is_git_path(&raw) {
            bail!("zip contains .git — refusing to extract");
        }
        let path = match &prefix {
            Some(p) => match raw.strip_prefix(p.as_str()) {
                Some(rest) => rest.to_string(),
                None => continue,
            },
            None => raw,
        };
        if path.is_empty() {
            continue;
        }
        if is_git_path(&path) {
            bail!("zip contains .git — refusing to extract");
        }
        if path.starts_with("__MACOSX/") || path.ends_with(".DS_Store") {
            continue;
        }
        let mode = zf.unix_mode();
        let symlink = mode.map(|m| m & 0o170000 == 0o120000).unwrap_or(false);
        let exec = mode.map(|m| m & 0o111 != 0).unwrap_or(false);
        let mut bytes = Vec::new();
        if let Err(e) = zf.read_to_end(&mut bytes) {
            bail!(
                "{}: cannot decompress {path}: {e}",
                crate::engine::UNSUPPORTED
            );
        }
        out.push(Entry {
            path,
            bytes,
            exec,
            symlink,
        });
    }
    Ok(out)
}

pub fn find<'a>(entries: &'a [Entry], path: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.path == path)
}
