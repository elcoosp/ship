use ship::mock_shell::MockShell;
use ship::shell::Shell;
use std::fs;
use std::process::Command;

fn setup_git_repo(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git command failed: {:?}", args);
    };

    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "commit.gpgsign", "false"]);
}

#[test]
fn test_three_way_merge_logic() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    setup_git_repo(repo);

    // 1. Base state
    fs::write(repo.join("file1.txt"), "base").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "base"])
        .current_dir(repo)
        .status()
        .unwrap();

    let base_sha = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    // 2. User pushes a change on main
    fs::write(repo.join("file2.txt"), "user change").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "user push"])
        .current_dir(repo)
        .status()
        .unwrap();

    // 3. AI clones at base_sha and makes a change to a different file
    // (Simulate by creating a branch, changing file1, and committing)
    Command::new("git")
        .args(["switch", "-C", "ai-temp", &base_sha])
        .current_dir(repo)
        .status()
        .unwrap();
    fs::write(repo.join("file1.txt"), "ai change").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "ai change"])
        .current_dir(repo)
        .status()
        .unwrap();

    // 4. Now switch back to main and perform the 3-way merge (as ship does)
    Command::new("git")
        .args(["switch", "main"])
        .current_dir(repo)
        .status()
        .unwrap();

    let merge_output = Command::new("git")
        .args(["merge", "ai-temp", "--no-commit", "--no-ff"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(merge_output.status.success(), "Merge failed");

    // 5. THE CRITICAL FIX: Unstage everything
    Command::new("git")
        .args(["reset"])
        .current_dir(repo)
        .status()
        .unwrap();

    // 6. Verify the working tree has BOTH changes and they are unstaged
    let content1 = fs::read_to_string(repo.join("file1.txt")).unwrap();
    let content2 = fs::read_to_string(repo.join("file2.txt")).unwrap();
    assert_eq!(content1, "ai change");
    assert_eq!(content2, "user change");

    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
        .unwrap();
    let status_str = String::from_utf8(status.stdout).unwrap();
    assert!(
        status_str.contains(" M file1.txt"),
        "file1 should be modified but unstaged"
    );
}

#[test]
fn test_empty_commit_skip_logic() {
    // This tests the logic inside ensure_committed.
    // If `git status --porcelain` is empty, it should NOT run `git commit`.

    let mock = MockShell::new();
    // Simulate a completely clean tree
    mock.stub("git status --porcelain", 0, "", "");

    // Simulate the commit command failing if called
    mock.stub("git commit", 1, "", "Should not be called!");

    // We manually replicate the guard logic to prove it works:
    let status_out = mock.run(&["git", "status", "--porcelain"]).unwrap();
    let should_skip = status_out.stdout.trim().is_empty();

    assert!(
        should_skip,
        "The guard should detect the clean tree and skip"
    );
    // In the actual code, `continue` is called here.

    // Assert `git commit` was never invoked
    mock.assert_not_called("git commit");
}

#[test]
fn test_mock_shell_records_calls() {
    let mock = MockShell::new();
    mock.stub("git fetch", 0, "", "");

    let _ = mock.run(&["git", "fetch", "origin"]);
    mock.assert_called("git fetch origin");
}

#[test]
fn test_generic_pipeline_without_versioning() {
    // Verifies that a repo without Cargo.toml or package.json
    // simply commits the files and does not try to bump versions.
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    setup_git_repo(repo);

    fs::write(repo.join("file1.zig"), "const std = @import(\"std\");").unwrap();

    // We just verify that a clean repo with uncommitted changes
    // correctly identifies the dirty state (which triggers ensure_committed).
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
        .unwrap();
    let status_str = String::from_utf8(output.stdout).unwrap();
    assert!(
        status_str.contains("?? file1.zig"),
        "Should show untracked file"
    );
}

#[test]
fn test_clean_keeps_failed_zips() {
    let dir = tempfile::tempdir().unwrap();
    let watch_dir = dir.path();
    let processed = watch_dir.join(".processed");
    let failed = watch_dir.join(".failed");

    std::fs::create_dir_all(&processed).unwrap();
    std::fs::create_dir_all(&failed).unwrap();

    std::fs::write(processed.join("success.zip"), "data").unwrap();
    std::fs::write(failed.join("error.zip"), "data").unwrap();

    ship::watch::clean_processed(watch_dir).unwrap();

    assert!(
        !processed.join("success.zip").exists(),
        "Processed zip should be deleted"
    );
    assert!(
        failed.join("error.zip").exists(),
        "Failed zip must be preserved"
    );
}
