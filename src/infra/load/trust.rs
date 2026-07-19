//! Guards against control files that are symlinks pointing outside the template. Each loader
//! that might follow such a symlink calls this at the exact point where it reads the file,
//! rather than scanning everything up front. Because the check happens where the read happens —
//! the same code both verifies the path and uses it — there is no gap in between for the symlink
//! to be swapped out from under us (a TOCTOU race).

use anyhow::{Context, Result, bail};
use std::path::Path;

/// Resolves `path` to its real location and rejects it if that lands outside `root_canon` —
/// meaning the symlink escaped the source root — unless the caller passed `trust` to allow it.
/// A broken symlink (one that can't be resolved at all) is also rejected. This mirrors copier's
/// `ForbiddenPathError`.
pub fn ensure_within_root(path: &Path, root_canon: &Path, trust: bool) -> Result<()> {
    let canon = path
        .canonicalize()
        .with_context(|| format!("{} could not be resolved (broken symlink?)", path.display()))?;
    if !canon.starts_with(root_canon) && !trust {
        bail!(
            "{} (-> {}) escapes the template root; re-run with --trust to read it",
            path.display(),
            canon.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[test]
    fn internal_file_is_allowed() {
        let root = TempDir::new().unwrap();
        let root_canon = root.path().canonicalize().unwrap();
        let file = root.path().join("inner.txt");
        fs::write(&file, "hi").unwrap();

        assert!(ensure_within_root(&file, &root_canon, false).is_ok());
    }

    #[test]
    fn external_symlink_is_rejected_without_trust() {
        let root = TempDir::new().unwrap();
        let root_canon = root.path().canonicalize().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "shh").unwrap();
        let link = root.path().join("link.txt");
        symlink(&outside_file, &link).unwrap();

        assert!(ensure_within_root(&link, &root_canon, false).is_err());
    }

    #[test]
    fn external_symlink_is_allowed_with_trust() {
        let root = TempDir::new().unwrap();
        let root_canon = root.path().canonicalize().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "shh").unwrap();
        let link = root.path().join("link.txt");
        symlink(&outside_file, &link).unwrap();

        assert!(ensure_within_root(&link, &root_canon, true).is_ok());
    }

    #[test]
    fn broken_symlink_is_rejected_regardless_of_trust() {
        let root = TempDir::new().unwrap();
        let root_canon = root.path().canonicalize().unwrap();
        let link = root.path().join("broken.txt");
        symlink(root.path().join("nowhere"), &link).unwrap();

        assert!(ensure_within_root(&link, &root_canon, false).is_err());
        assert!(ensure_within_root(&link, &root_canon, true).is_err());
    }
}
