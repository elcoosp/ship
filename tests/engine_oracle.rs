//! Oracle tests: the gix engine must produce EXACTLY the same trees as git CLI
//! plumbing on the same inputs (tree SHAs are timestamp-independent).
use ship::engine;
use ship::manifest::{CommitGroup, Delivery, Manifest};
use ship::shell::RealShell;
use ship::zipmem::Entry;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn merge_tree_supported() -> bool {
    let out = Command::new("git").arg("--version").output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let ver = s.split_whitespace().nth(2).unwrap_or("0");
    let mut it = ver.split('.');
    let major: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let minor: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    major > 2 || (major == 2 && minor >= 38)
}

fn setup(repo: &Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    for d in ["a", "b", "c"] {
        std::fs::create_dir_all(repo.join(d)).unwrap();
        for i in 0..4 {
            std::fs::write(
                repo.join(d).join(format!("f{i}.txt")),
                format!("base {d} {i}\n"),
            )
            .unwrap();
        }
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
}

fn snapshot_entries(repo: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    for d in ["a", "b", "c"] {
        for i in 0..4 {
            let rel = format!("{d}/f{i}.txt");
            let bytes = std::fs::read(repo.join(&rel)).unwrap();
            out.push(Entry {
                path: rel,
                bytes,
                exec: false,
                symlink: false,
            });
        }
    }
    out
}

/// Expected merged tree built with pure git plumbing on a scratch index.
fn expected_tree(repo: &Path, base_ref: &str, edits: &[(&str, Option<&[u8]>)]) -> String {
    let idx = repo.join(".git/scratch-index");
    let idx_s = idx.to_string_lossy().into_owned();
    let g = |args: &[&str]| -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_INDEX_FILE", &idx_s)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    g(&["read-tree", base_ref]);
    for (path, content) in edits {
        match content {
            Some(bytes) => {
                let f = repo.join(".git/scratch-blob");
                std::fs::write(&f, bytes).unwrap();
                let sha = g(&["hash-object", "-w", &f.to_string_lossy()]);
                g(&["update-index", "--add", "--cacheinfo", "100644", &sha, path]);
            }
            None => {
                g(&["update-index", "--force-remove", path]);
            }
        }
    }
    g(&["write-tree"])
}

fn manifest(base: &str, groups: Vec<(&str, Vec<&str>)>) -> Manifest {
    Manifest {
        delivery: Delivery {
            branch: "feat/engine-test".into(),
            pr_title: "feat: engine test".into(),
            pr_body: ".github/pr-body.md".into(),
            base_commit: Some(base.to_string()),
            remote: "origin".into(),
            base: None,
        },
        commit: groups
            .into_iter()
            .map(|(msg, files)| CommitGroup {
                message: msg.to_string(),
                files: files.into_iter().map(PathBuf::from).collect(),
            })
            .collect(),
    }
}

#[test]
fn engine_matches_cli_tree_and_groups() {
    if !merge_tree_supported() {
        eprintln!("SKIP engine_matches_cli_tree_and_groups: git < 2.38");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup(repo);
    let base = git(repo, &["rev-parse", "HEAD"]);

    let mut entries = snapshot_entries(repo);
    for e in entries.iter_mut() {
        if e.path == "a/f0.txt" {
            e.bytes = b"ai change a0\n".to_vec();
        }
        if e.path == "b/f1.txt" {
            e.bytes = b"ai change b1\n".to_vec();
        }
    }
    entries.retain(|e| e.path != "c/f3.txt");
    entries.push(Entry {
        path: "d/new.txt".into(),
        bytes: b"new file\n".to_vec(),
        exec: false,
        symlink: false,
    });

    let shell = RealShell::new(repo.to_path_buf());
    let prep =
        engine::prepare_tree(&shell, repo, entries, Some(base.as_str()), "origin", "main").unwrap();
    let mut dirty: Vec<String> = prep
        .dirty
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    dirty.sort();
    assert_eq!(dirty, vec!["a/f0.txt", "b/f1.txt", "c/f3.txt", "d/new.txt"]);

    let m = manifest(
        &base,
        vec![
            ("feat: group one", vec!["a/f0.txt", "d/new.txt"]),
            ("fix: group two", vec!["b/f1.txt", "c/f3.txt"]),
        ],
    );
    let tip = engine::commit_groups(&shell, &prep, &m, "feat/engine-test").unwrap();

    let expected = expected_tree(
        repo,
        "HEAD",
        &[
            ("a/f0.txt", Some(b"ai change a0\n")),
            ("b/f1.txt", Some(b"ai change b1\n")),
            ("d/new.txt", Some(b"new file\n")),
            ("c/f3.txt", None),
        ],
    );
    let got = git(repo, &["rev-parse", &format!("{tip}^{{tree}}")]);
    assert_eq!(got, expected, "engine tree must equal CLI plumbing tree");

    let subjects = git(repo, &["log", "--format=%s", &format!("{base}..{tip}")]);
    assert_eq!(
        subjects.lines().collect::<Vec<_>>(),
        vec!["fix: group two", "feat: group one"]
    );
    assert_eq!(
        git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feat/engine-test"
    );
    assert!(
        git(repo, &["status", "--porcelain"]).is_empty(),
        "worktree must be clean after engine checkout"
    );
}

#[test]
fn engine_conflict_favors_ai_and_keeps_concurrent_push() {
    if !merge_tree_supported() {
        eprintln!("SKIP engine_conflict_favors_ai_and_keeps_concurrent_push: git < 2.38");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup(repo);
    let base = git(repo, &["rev-parse", "HEAD"]);
    let mut entries = snapshot_entries(repo);

    // Concurrent user push on main: edits a/f0 differently + adds user.txt.
    std::fs::write(repo.join("a/f0.txt"), "user change\n").unwrap();
    std::fs::write(repo.join("user.txt"), "user file\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "user push"]);
    git(repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);

    // AI edits a/f0 from base.
    for e in entries.iter_mut() {
        if e.path == "a/f0.txt" {
            e.bytes = b"ai change a0\n".to_vec();
        }
    }

    let shell = RealShell::new(repo.to_path_buf());
    let prep =
        engine::prepare_tree(&shell, repo, entries, Some(base.as_str()), "origin", "main").unwrap();
    let dirty: Vec<String> = prep
        .dirty
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        dirty,
        vec!["a/f0.txt"],
        "user.txt is already on main; only the conflict is dirty"
    );

    let m = manifest(&base, vec![("feat: ai wins", vec!["a/f0.txt"])]);
    let tip = engine::commit_groups(&shell, &prep, &m, "feat/engine-test").unwrap();

    let expected = expected_tree(
        repo,
        "origin/main",
        &[("a/f0.txt", Some(b"ai change a0\n"))],
    );
    let got = git(repo, &["rev-parse", &format!("{tip}^{{tree}}")]);
    assert_eq!(
        got, expected,
        "conflict must favor AI while keeping the concurrent push"
    );
}

#[test]
fn engine_rejects_empty_delivery() {
    if !merge_tree_supported() {
        eprintln!("SKIP engine_rejects_empty_delivery: git < 2.38");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup(repo);
    let base = git(repo, &["rev-parse", "HEAD"]);
    let entries = snapshot_entries(repo);
    let shell = RealShell::new(repo.to_path_buf());
    let err = engine::prepare_tree(&shell, repo, entries, Some(base.as_str()), "origin", "main")
        .unwrap_err()
        .to_string();
    assert!(err.contains("nothing to ship"), "got: {err}");
}
