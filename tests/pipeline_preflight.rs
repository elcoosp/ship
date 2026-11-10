use ship::manifest::{CommitGroup, Delivery, Manifest, VersionSection};
use ship::mock_shell::MockShell;
use ship::pipeline;
use std::path::PathBuf;

fn mock_manifest() -> Manifest {
    Manifest {
        delivery: Delivery {
            branch: "feat/test".into(),
            pr_title: "feat: test".into(),
            pr_body: ".github/pr-body.md".into(),
            remote: "origin".into(),
            base: None,
            base_commit: None,
        },
        version: VersionSection {
            bump: "auto".into(),
        },
        commit: vec![CommitGroup {
            message: "feat: test".into(),
            files: vec![PathBuf::from("a")],
        }],
    }
}

#[test]
fn fails_if_not_authenticated() {
    let mock = MockShell::new();
    mock.stub("gh auth status", 1, "", "not logged in");

    let res = pipeline::prepare(false, false, false, &mock_manifest(), &mock);
    assert!(res.is_err());
    assert!(
        res.unwrap_err()
            .to_string()
            .contains("command failed: gh auth status")
    );
}

#[test]
fn fails_if_tag_exists() {
    let mock = MockShell::new();
    mock.stub("git symbolic-ref", 0, "origin/main\n", "");
    mock.stub("gh auth status", 0, "", "");
    mock.stub("git rev-list", 0, "0", ""); // not ahead
    mock.stub("git config user.email", 0, "test@example.com", "");
    mock.stub("git config user.name", 0, "Test", "");
    mock.stub("git check-ignore", 0, "", ""); // gitignored
    mock.stub("git ls-files", 0, "", ""); // not tracked

    // Stub version reads
    mock.stub("git rev-parse HEAD", 0, "a1b2c3d4", "");
    mock.stub("git fetch", 0, "", "");
    mock.stub("git describe", 1, "", ""); // no tags

    // Stub the tag check to say it EXISTS
    mock.stub("git rev-parse -q --verify refs/tags", 0, "exists", ""); // local tag exists

    // We need a fake Cargo.toml on disk for the version read to not crash before the tag check
    // Actually, `read_workspace_version_at_opt` uses `git show origin/main:Cargo.toml`
    mock.stub(
        "git show",
        0,
        "[workspace.package]\nversion = \"0.1.0\"\n",
        "",
    );

    let res = pipeline::prepare(false, false, false, &mock_manifest(), &mock);

    // It should fail at tag_absent
    let err = res.unwrap_err().to_string();
    assert!(
        err.contains("tag v0.2.0 already exists"),
        "Unexpected error: {err}"
    );
}
