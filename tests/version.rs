use ship::manifest::{CommitGroup, Delivery, Manifest, VersionSection};
use ship::version::{BumpLevel, Version, derive_bump};
use std::path::PathBuf;

fn mk(msgs: &[&str]) -> Manifest {
    Manifest {
        delivery: Delivery {
            branch: "feat/x".into(),
            pr_title: "feat: x".into(),
            pr_body: ".github/pr-body.md".into(),
            base_commit: None,
            remote: "origin".into(),
            base: None,
        },
        version: VersionSection::default(),
        commit: msgs
            .iter()
            .enumerate()
            .map(|(i, m)| CommitGroup {
                message: (*m).into(),
                files: vec![PathBuf::from(format!("f{i}"))],
            })
            .collect(),
    }
}

#[test]
fn breaking_overrides_feat() {
    assert_eq!(
        derive_bump(&mk(&["feat(a): x", "refactor!: y"])),
        BumpLevel::Major
    );
}

#[test]
fn feat_is_minor() {
    assert_eq!(
        derive_bump(&mk(&["fix(a): x", "feat(a): y", "chore: z"])),
        BumpLevel::Minor
    );
}

#[test]
fn fix_only_is_patch() {
    assert_eq!(derive_bump(&mk(&["fix: x", "docs: y"])), BumpLevel::Patch);
}

#[test]
fn arithmetic() {
    let v = Version::parse("0.6.3").unwrap();
    assert_eq!(v.bump(BumpLevel::Patch).to_string(), "0.6.4");
    assert_eq!(v.bump(BumpLevel::Minor).to_string(), "0.7.0");
    assert_eq!(v.bump(BumpLevel::Major).to_string(), "1.0.0");
}

#[test]
fn pr_suffix_ignored() {
    let v = Version::parse("1.2.3-beta.4").unwrap();
    assert_eq!(v.to_string(), "1.2.3");
}
