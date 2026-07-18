//! `.scaffoldroot` 해석 — `SourceRootSource`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::domain::store::SourceRootSource;

/// `<template_root>/.scaffoldroot`를 읽어 실효 소스 루트를 해석한다(§1.7). 내용은 template_root
/// 상대 subpath다. 파일이 없으면 template_root 그대로. `..`·절대경로·외부 심링크로 template_root를
/// 벗어나면 에러.
pub struct FsSourceRootSource;

impl SourceRootSource for FsSourceRootSource {
    fn resolve(&self, template_root: &Path) -> Result<PathBuf> {
        let marker = template_root.join(".scaffoldroot");
        let content = match fs::read_to_string(&marker) {
            Ok(content) => content,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(template_root.to_path_buf()),
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
        if sub
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            bail!(".scaffoldroot {subpath:?} must not contain `..`");
        }

        // 외부 심링크로 벗어나는 경우까지 막기 위해 canonical 경로가 template_root 안인지 확인한다.
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
    fn rejects_symlink_escaping_template_root() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        fs::write(dir.path().join(".scaffoldroot"), "link").unwrap();
        assert!(FsSourceRootSource.resolve(dir.path()).is_err());
    }
}
