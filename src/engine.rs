//! gitoxide-powered delivery engine (gix 0.90, inherent object API).
//!
//! zip entries -> blobs (parallel, dedup) -> AI snapshot tree/commit
//! -> 3-way merge via `git merge-tree --write-tree` (no worktree touch)
//! -> dirty set from in-memory tree diff (no `git status` scan)
//! -> N grouped commits as tree overlays (shared subtrees)
//! -> ONE final `git switch -C branch tip` checkout.
//!
//! Falls back to the legacy CLI path via UNSUPPORTED when the repo uses
//! filters/LFS/submodules/symlinks or git < 2.38.

use crate::manifest::Manifest;
use crate::shell::{Shell, run_checked};
use crate::zipmem::Entry;
use anyhow::{Context, Result, bail};
use gix::bstr::{BString, ByteSlice};
use gix::object::Kind;
use gix::objs::tree::{Entry as TreeEntry, EntryKind, EntryMode};
use gix::objs::{Commit, Tree};
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub const UNSUPPORTED: &str = "ENGINE_UNSUPPORTED";

type Flat = BTreeMap<String, (EntryMode, gix::ObjectId)>;

#[derive(Debug)]
pub struct Prepared {
    repo: gix::Repository,
    main_id: gix::ObjectId,
    main_flat: Flat,
    merged_flat: Flat,
    pub dirty: Vec<PathBuf>,
}

/// Batch-filter gitignored entries with a single `git check-ignore --stdin`.
pub fn filter_ignored(repo_path: &Path, entries: Vec<Entry>) -> Result<Vec<Entry>> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    if entries.is_empty() {
        return Ok(entries);
    }
    let mut child = Command::new("git")
        .args(["check-ignore", "--stdin"])
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawning git check-ignore")?;
    {
        let stdin = child.stdin.as_mut().context("check-ignore stdin")?;
        for e in &entries {
            let _ = writeln!(stdin, "{}", e.path);
        }
    }
    let out = child.wait_with_output()?;
    let ignored: BTreeSet<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Ok(entries
        .into_iter()
        .filter(|e| !ignored.contains(&e.path))
        .collect())
}

fn safety_checks(shell: &dyn Shell, repo_path: &Path, entries: &[Entry]) -> Result<()> {
    // core.autocrlf=true can rewrite blobs; 'input' only strips CRLF (safe for LF input).
    if let Ok(out) = shell.run(&["git", "config", "--get", "core.autocrlf"])
        && out.stdout.trim() == "true"
    {
        bail!("{UNSUPPORTED}: core.autocrlf=true — use --engine cli");
    }
    let mut has_filter = false;
    let mut has_text_norm = false;
    if let Ok(out) = shell.run(&["git", "ls-files", "--", "*.gitattributes"]) {
        for name in out.stdout.lines() {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(repo_path.join(name)) else {
                continue;
            };
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let mut toks = line.split_whitespace();
                let _pattern = toks.next();
                for attr in toks {
                    // clean/smudge filters (LFS...) genuinely transform blob content.
                    if attr.starts_with("filter=") {
                        has_filter = true;
                    }
                    // text normalization only changes blobs when input has CRLF.
                    if attr == "text" || attr == "text=auto" {
                        has_text_norm = true;
                    }
                }
            }
        }
    }
    if has_filter {
        bail!("{UNSUPPORTED}: .gitattributes clean/smudge filter (LFS?) — use --engine cli");
    }
    if has_text_norm
        && entries
            .iter()
            .any(|e| e.bytes.windows(2).any(|w| w == b"\r\n"))
    {
        bail!("{UNSUPPORTED}: CRLF content with text normalization — use --engine cli");
    }
    if entries.iter().any(|e| e.symlink) {
        bail!("{UNSUPPORTED}: zip contains symlinks — use --engine cli");
    }
    Ok(())
}

fn tree_of(repo: &gix::Repository, commit: gix::ObjectId) -> Result<gix::ObjectId> {
    Ok(repo.find_object(commit)?.peel_to_kind(Kind::Tree)?.id)
}

fn walk_tree(
    repo: &gix::Repository,
    id: gix::ObjectId,
    prefix: &str,
    out: &mut Flat,
) -> Result<()> {
    let tree = repo.find_tree(id)?;
    for entry in tree.iter() {
        let entry = entry?;
        let name = entry
            .filename()
            .to_str()
            .map_err(|_| anyhow::anyhow!("{UNSUPPORTED}: non-UTF8 path in tree"))?;
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let mode = entry.mode();
        if mode.is_tree() {
            walk_tree(repo, entry.oid().into(), &path, out)?;
        } else {
            out.insert(path, (mode, entry.oid().into()));
        }
    }
    Ok(())
}

fn flatten(repo: &gix::Repository, tree_id: gix::ObjectId) -> Result<Flat> {
    let mut out = Flat::new();
    walk_tree(repo, tree_id, "", &mut out)?;
    Ok(out)
}

#[derive(Default)]
struct DirNode {
    children: BTreeMap<String, Node>,
}

enum Node {
    File(EntryMode, gix::ObjectId),
    Dir(DirNode),
}

fn insert_path(node: &mut DirNode, parts: &[&str], mode: EntryMode, oid: gix::ObjectId) {
    if parts.len() == 1 {
        node.children
            .insert(parts[0].to_string(), Node::File(mode, oid));
        return;
    }
    let child = node
        .children
        .entry(parts[0].to_string())
        .or_insert_with(|| Node::Dir(DirNode::default()));
    if let Node::Dir(d) = child {
        insert_path(d, &parts[1..], mode, oid);
    }
}

fn build_trie(flat: &Flat) -> DirNode {
    let mut root = DirNode::default();
    for (path, (mode, oid)) in flat {
        let parts: Vec<&str> = path.split('/').collect();
        insert_path(&mut root, &parts, *mode, *oid);
    }
    root
}

/// Write trees bottom-up. `memo` reuses unchanged subtrees across groups.
/// `repo.write_object` pre-hashes and skips duplicates, so only genuinely new
/// trees hit the disk.
fn write_dir(
    repo: &gix::Repository,
    dir: &DirNode,
    path: &str,
    memo: &mut HashMap<String, (u64, gix::ObjectId)>,
) -> Result<gix::ObjectId> {
    use std::hash::{Hash, Hasher};
    let mut items: Vec<(Vec<u8>, TreeEntry)> = Vec::new();
    for (name, node) in &dir.children {
        match node {
            Node::File(mode, oid) => {
                items.push((
                    name.as_bytes().to_vec(),
                    TreeEntry {
                        mode: *mode,
                        filename: BString::from(name.as_str()),
                        oid: *oid,
                    },
                ));
            }
            Node::Dir(d) => {
                let sub = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                };
                let oid = write_dir(repo, d, &sub, memo)?;
                let mut key = name.as_bytes().to_vec();
                key.push(b'/');
                items.push((
                    key,
                    TreeEntry {
                        mode: EntryMode::from(EntryKind::Tree),
                        filename: BString::from(name.as_str()),
                        oid,
                    },
                ));
            }
        }
    }
    // Git tree ordering: directories compare as if they had a trailing '/'.
    items.sort_by(|a, b| a.0.cmp(&b.0));

    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (k, e) in &items {
        k.hash(&mut h);
        format!("{:?}", e.mode).hash(&mut h);
        e.oid.as_bytes().hash(&mut h);
    }
    let fp = h.finish();
    if let Some((old_fp, oid)) = memo.get(path)
        && *old_fp == fp
    {
        return Ok(*oid);
    }
    let tree = Tree {
        entries: items.into_iter().map(|(_, e)| e).collect(),
    };
    let oid = repo.write_object(&tree)?.detach();
    memo.insert(path.to_string(), (fp, oid));
    Ok(oid)
}

fn write_flat(
    repo: &gix::Repository,
    flat: &Flat,
    memo: &mut HashMap<String, (u64, gix::ObjectId)>,
) -> Result<gix::ObjectId> {
    let root = build_trie(flat);
    write_dir(repo, &root, "", memo)
}

fn make_commit(
    repo: &gix::Repository,
    tree: gix::ObjectId,
    parents: &[gix::ObjectId],
    message: &str,
) -> Result<gix::ObjectId> {
    let sr = match repo.committer() {
        Some(r) => r?,
        None => {
            bail!("{UNSUPPORTED}: git committer identity (user.name/user.email) not configured")
        }
    };
    let sig = sr.to_owned()?;
    let commit = Commit {
        tree,
        parents: parents.iter().copied().collect(),
        author: sig.clone(),
        committer: sig,
        encoding: None,
        message: BString::from(message),
        extra_headers: Vec::new(),
    };
    Ok(repo.write_object(&commit)?.detach())
}

/// Build the merged delivery tree entirely in the ODB. Returns the dirty set.
pub fn prepare_tree(
    shell: &dyn Shell,
    repo_path: &Path,
    entries: Vec<Entry>,
    base_commit: Option<&str>,
    remote: &str,
    default: &str,
) -> Result<Prepared> {
    let _p = crate::trace::phase("engine.prepare_tree");
    safety_checks(shell, repo_path, &entries)?;
    let entries = filter_ignored(repo_path, entries)?;
    if entries.is_empty() {
        bail!("zip has no deliverable files after ignore filtering");
    }

    let mut repo = gix::open(repo_path).context("gix::open")?;
    repo.object_cache_size(Some(64 * 1024 * 1024));

    let main_ref = format!("{remote}/{default}");
    let main_id = repo.rev_parse_single(main_ref.as_str())?.detach();
    let base_id = match base_commit {
        Some(b) => repo.rev_parse_single(b)?.detach(),
        None => main_id,
    };

    let main_flat = flatten(&repo, tree_of(&repo, main_id)?)?;
    let base_flat = flatten(&repo, tree_of(&repo, base_id)?)?;
    if base_flat
        .values()
        .any(|(m, _)| m.kind() == EntryKind::Commit)
    {
        bail!("{UNSUPPORTED}: repo has submodules (gitlinks) — use --engine cli");
    }

    // 1. Hash + write blobs in parallel (rayon + thread-local gix handles, dedup built-in).
    let sync = repo.clone().into_sync();
    let written: Vec<(String, gix::ObjectId, bool)> = entries
        .par_iter()
        .map(|e| -> Result<(String, gix::ObjectId, bool)> {
            let r = sync.to_thread_local();
            let id = r.write_blob(&e.bytes)?.detach();
            Ok((e.path.clone(), id, e.exec))
        })
        .collect::<Result<Vec<_>>>()?;

    // 2. AI snapshot = exact zip content on top of base (mirrors `git rm -r . + copy`).
    let zip_paths: BTreeSet<String> = entries.iter().map(|e| e.path.clone()).collect();
    let mut snapshot = base_flat.clone();
    snapshot.retain(|k, _| zip_paths.contains(k));
    for (path, id, exec) in written {
        let kind = if exec {
            EntryKind::BlobExecutable
        } else {
            EntryKind::Blob
        };
        snapshot.insert(path, (EntryMode::from(kind), id));
    }
    let mut memo = HashMap::new();
    let snapshot_tree = write_flat(&repo, &snapshot, &mut memo)?;
    let ai_id = make_commit(&repo, snapshot_tree, &[base_id], "AI delivery snapshot")?;

    // 3. Real 3-way merge, in memory, via git merge-tree (ORT). Conflicts: favor AI.
    let mb = format!("--merge-base={}", base_id.to_hex());
    let main_hex = main_id.to_hex().to_string();
    let ai_hex = ai_id.to_hex().to_string();
    let out = shell.run(&["git", "merge-tree", "--write-tree", &mb, &main_hex, &ai_hex])?;
    if out.status != 0 && out.status != 1 {
        let low = out.stderr.to_ascii_lowercase();
        if low.contains("unknown option") || low.contains("usage") || low.contains("unrecognized") {
            bail!("{UNSUPPORTED}: git merge-tree --write-tree unavailable (git >= 2.38 required)");
        }
        bail!("git merge-tree failed: {}", out.stderr.trim());
    }
    let tree_hex = out.stdout.lines().next().unwrap_or("").trim().to_string();
    if tree_hex.is_empty() {
        bail!("git merge-tree returned no tree oid");
    }
    let merged_tree_id = gix::ObjectId::from_hex(tree_hex.as_bytes())?;
    let mut merged_flat = flatten(&repo, merged_tree_id)?;
    if out.status == 1 {
        let mut conflicted: BTreeSet<String> = BTreeSet::new();
        for line in out.stdout.lines().skip(1) {
            if let Some((meta, path)) = line.split_once('\t')
                && meta.starts_with(|c: char| c.is_ascii_digit())
                && !path.is_empty()
            {
                conflicted.insert(path.to_string());
            }
        }
        if !conflicted.is_empty() {
            crate::ui::warn(&format!(
                "merge conflicts on {} path(s) — favoring AI changes",
                conflicted.len()
            ));
            for p in conflicted {
                match snapshot.get(&p) {
                    Some(v) => {
                        merged_flat.insert(p, *v);
                    }
                    None => {
                        merged_flat.remove(&p);
                    }
                }
            }
        }
    }

    // 4. Dirty set = in-memory tree diff. No `git status` scan.
    let mut dirty: Vec<PathBuf> = Vec::new();
    for (k, v) in &merged_flat {
        if main_flat.get(k) != Some(v) {
            dirty.push(PathBuf::from(k));
        }
    }
    for k in main_flat.keys() {
        if !merged_flat.contains_key(k) {
            dirty.push(PathBuf::from(k));
        }
    }
    dirty.sort();
    dirty.dedup();
    if dirty.is_empty() {
        bail!("zip produces no changes vs {main_ref} — nothing to ship");
    }

    Ok(Prepared {
        repo,
        main_id,
        main_flat,
        merged_flat,
        dirty,
    })
}

/// Chain the grouped commits in memory, then materialize with ONE checkout.
pub fn commit_groups(
    shell: &dyn Shell,
    prep: &Prepared,
    m: &Manifest,
    branch: &str,
) -> Result<String> {
    let _p = crate::trace::phase("engine.commit_groups");
    let mut cur = prep.main_flat.clone();
    let mut parent = prep.main_id;
    let mut memo = HashMap::new();
    let mut made = 0usize;
    let total = m.commit.iter().filter(|g| !g.files.is_empty()).count();
    for g in &m.commit {
        if g.files.is_empty() {
            continue;
        }
        for f in &g.files {
            let p = f.to_string_lossy().into_owned();
            match prep.merged_flat.get(&p) {
                Some(v) => {
                    cur.insert(p, *v);
                }
                None => {
                    cur.remove(&p);
                }
            }
        }
        let tree = write_flat(&prep.repo, &cur, &mut memo)?;
        parent = make_commit(&prep.repo, tree, &[parent], &g.message)?;
        made += 1;
        crate::ui::commit_step(made, total, &g.message);
    }
    if made == 0 {
        bail!("no commits created (all groups empty after expansion)");
    }
    let tip = parent.to_hex().to_string();
    run_checked(shell, &["git", "switch", "-C", branch, &tip])?;
    Ok(tip)
}
