use crate::shell::Shell;
use anyhow::Result;
use std::os::unix::fs::PermissionsExt;

/// Revert spurious mode changes on tracked files.
///
/// Reads the exact mode from the Git index (`git ls-files -s`), checks the
/// actual file on disk, and forcibly applies `chmod` to match the index.
/// This bypasses `git diff` entirely, making it immune to format differences.
pub fn exec_bit(shell: &dyn Shell) -> Result<usize> {
    let out = shell.run(&["git", "ls-files", "-s"])?;
    if out.status != 0 {
        return Ok(0);
    }

    let cwd = shell.cwd();
    let mut reverted = 0usize;

    for line in out.stdout.lines() {
        // Format: 100644 <sha> 0\tpath/to/file
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 2 {
            continue;
        }
        let meta = parts[0];
        let path = parts[1];

        let meta_parts: Vec<&str> = meta.split(' ').collect();
        if meta_parts.len() < 3 {
            continue;
        }

        // Parse the mode from the index (e.g., 100755 -> 33261)
        let git_mode = u32::from_str_radix(meta_parts[0], 8).unwrap_or(0);

        let full_path = cwd.join(path);
        if let Ok(metadata) = std::fs::metadata(&full_path) {
            let disk_mode = metadata.permissions().mode();

            // Check if the executable bit matches
            let want_exec = git_mode & 0o111 != 0;
            let has_exec = disk_mode & 0o111 != 0;

            if want_exec != has_exec {
                let mut perms = metadata.permissions();
                if want_exec {
                    perms.set_mode(0o755);
                } else {
                    perms.set_mode(0o644);
                }
                let _ = std::fs::set_permissions(&full_path, perms);
                reverted += 1;
            }
        }
    }
    Ok(reverted)
}
