//! `.scaffoldroot` resolution — `SourceRootSource`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::domain::store::SourceRootSource;

/// Reads `<template_root>/.scaffoldroot` to resolve the effective source root. Its content is
/// a template_root-relative subpath. Absent file → template_root unchanged. `..`, absolute
/// paths, and external symlinks that escape template_root are errors.
pub struct FsSourceRootSource;

impl SourceRootSource for FsSourceRootSource {
    fn resolve(&self, template_root: &Path) -> Result<PathBuf> {
        let marker = template_root.join(".scaffoldroot");
        let content = match fs::read_to_string(&marker) {
            Ok(content) => content,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                // Distinguish "no file" from "existing (dangling) symlink whose target is
                // missing" — both give NotFound from read_to_string. If the marker itself
                // exists (including a symlink), fail loud.
                match marker.symlink_metadata() {
                    Err(meta_err) if meta_err.kind() == ErrorKind::NotFound => {
                        return Ok(template_root.to_path_buf());
                    }
                    _ => {
                        return Err(e).with_context(|| {
                            format!(
                                ".scaffoldroot at {} exists but could not be read (dangling symlink?)",
                                marker.display()
                            )
                        });
                    }
                }
            }
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", marker.display()));
            }
        };

        let subpath = content.trim();
        if subpath.is_empty() {
            return Ok(template_root.to_path_buf());
        }

        let sub = Path::new(subpath);
        if sub.is_absolute() {
            bail!(".scaffoldroot {subpath:?} must be a relative subpath, not absolute");
        }
        if sub.components().any(|c| matches!(c, Component::ParentDir)) {
            bail!(".scaffoldroot {subpath:?} must not contain `..`");
        }

        // Confirm the canonical path is inside template_root, to also block escapes via external symlink.
        let canonical_root = template_root
            .canonicalize()
            .with_context(|| format!("template root {} does not exist", template_root.display()))?;
        let effective = template_root.join(sub);
        let canonical_effective = effective.canonicalize().with_context(|| {
            format!(
                ".scaffoldroot {subpath:?} points to {} which does not exist",
                effective.display()
            )
        })?;
        if !canonical_effective.starts_with(&canonical_root) {
            bail!(".scaffoldroot {subpath:?} escapes the template root");
        }

        Ok(canonical_effective)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[test]
    fn absent_scaffoldroot_returns_template_root() {
        let dir = TempDir::new().unwrap();
        let resolved = FsSourceRootSource.resolve(dir.path()).unwrap();
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn resolves_subpath_to_effective_root() {
        let dir = TempDir::new().unwrap();
        let inner = dir.path().join("template");
        fs::create_dir_all(&inner).unwrap();
        fs::write(dir.path().join(".scaffoldroot"), "template\n").unwrap();

        let resolved = FsSourceRootSource.resolve(dir.path()).unwrap();
        assert_eq!(resolved, inner.canonicalize().unwrap());
    }

    #[test]
    fn empty_or_whitespace_scaffoldroot_falls_back_to_template_root() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".scaffoldroot"), "   \n").unwrap();
        let resolved = FsSourceRootSource.resolve(dir.path()).unwrap();
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn rejects_parent_dir_escape() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".scaffoldroot"), "../outside").unwrap();
        assert!(FsSourceRootSource.resolve(dir.path()).is_err());
    }

    #[test]
    fn rejects_absolute_path() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".scaffoldroot"), "/etc").unwrap();
        assert!(FsSourceRootSource.resolve(dir.path()).is_err());
    }

    #[test]
    fn rejects_dangling_scaffoldroot_symlink() {
        let dir = TempDir::new().unwrap();
        // A .scaffoldroot symlink pointing to a missing target: must error, not be treated as absent.
        symlink(dir.path().join("nowhere"), dir.path().join(".scaffoldroot")).unwrap();
        assert!(FsSourceRootSource.resolve(dir.path()).is_err());
    }

    #[test]
    fn rejects_symlink_escaping_template_root() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        fs::write(dir.path().join(".scaffoldroot"), "link").unwrap();
        assert!(FsSourceRootSource.resolve(dir.path()).is_err());
    }
}
