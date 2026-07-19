//! Trust guard for external-symlink control files. Called at the read point by each loader
//! that could follow a symlink to read outside — a check-at-access (guard == executor), not a
//! pre-flight scan, so there is no scan↔read TOCTOU window.

use anyhow::{Context, Result, bail};
use std::path::Path;

/// Rejects when `path`'s canonical path is outside `root_canon` (escapes the source root)
/// unless `trust`. A broken symlink (canonicalize failure) also errors. copier's
/// `ForbiddenPathError` pattern.
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
