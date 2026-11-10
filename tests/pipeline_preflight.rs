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
