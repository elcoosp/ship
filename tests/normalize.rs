use ship::mock_shell::MockShell;
use ship::normalize;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

#[test]
fn reverts_spurious_exec_bit() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.sh");
    fs::write(&file_path, "echo hi").unwrap();

    // Make it non-executable on disk
    let mut perms = fs::metadata(&file_path).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&file_path, perms).unwrap();

    let mock = MockShell::with_cwd(dir.path().to_str().unwrap());
    // Stub git ls-files -s to say it SHOULD be 100755
    mock.stub("git ls-files -s", 0, "100755 1234567890 0\ttest.sh\n", "");

    let reverted = normalize::exec_bit(&mock).unwrap();
    assert_eq!(reverted, 1);

    // Verify the disk mode was changed to 755
    let disk_mode = fs::metadata(&file_path).unwrap().permissions().mode();
    assert_eq!(disk_mode & 0o111, 0o111, "File should be executable");
}
