use ship::manifest::Manifest;
use std::io::Write;

fn write_manifest(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

#[test]
fn valid_manifest_parses() {
    let content = r#"
[delivery]
branch = "feat/test"
pr_title = "feat: test"
pr_body = ".github/pr-body.md"
base_commit = "a1b2c3d4"

[version]
bump = "auto"

[[commit]]
message = "feat(core): add thing"
files = ["src/lib.rs"]
"#;
    let f = write_manifest(content);
    assert!(Manifest::load(f.path()).is_ok());
}

#[test]
fn invalid_branch_prefix_fails() {
    let content = r#"
[delivery]
branch = "bad/test"
pr_title = "feat: test"
pr_body = ".github/pr-body.md"
[[commit]]
message = "feat: test"
files = ["a"]
"#;
    let f = write_manifest(content);
    assert!(Manifest::load(f.path()).is_err());
}

#[test]
fn subject_too_long_fails() {
    let content = format!(
        r#"
[delivery]
branch = "feat/test"
pr_title = "feat: test"
pr_body = ".github/pr-body.md"
[[commit]]
message = "feat: {}"
files = ["a"]
"#,
        "a".repeat(80)
    );
    let f = write_manifest(&content);
    assert!(Manifest::load(f.path()).is_err());
}

#[test]
fn duplicate_file_fails() {
    let content = r#"
[delivery]
branch = "feat/test"
pr_title = "feat: test"
pr_body = ".github/pr-body.md"
[[commit]]
message = "feat: test 1"
files = ["a"]
[[commit]]
message = "feat: test 2"
files = ["a"]
"#;
    let f = write_manifest(content);
    assert!(Manifest::load(f.path()).is_err());
}

#[test]
fn empty_commit_groups_fails() {
    let content = r#"
[delivery]
branch = "feat/test"
pr_title = "feat: test"
pr_body = ".github/pr-body.md"
"#;
    let f = write_manifest(content);
    let res = Manifest::load(f.path());
    assert!(res.is_err());
    assert!(
        res.unwrap_err()
            .to_string()
            .contains("manifest has no [[commit]] groups")
    );
}
