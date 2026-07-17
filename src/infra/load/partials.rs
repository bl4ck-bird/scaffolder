//! `partials/` 로드 — `PartialSource`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::domain::render::PartialSource;

/// `<template_root>/partials/` 하위 조각을 이름→소스로 로드한다. `partials/`가 없으면 빈 맵.
pub struct FsPartialSource;

impl PartialSource for FsPartialSource {
    fn load(&self, template_root: &Path) -> Result<BTreeMap<String, String>> {
        let partials_dir = template_root.join("partials");
        let mut out = BTreeMap::new();
        if !partials_dir.exists() {
            return Ok(out);
        }
        collect(&partials_dir, &partials_dir, &mut out)?;
        Ok(out)
    }
}

fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
    let entries =
        fs::read_dir(dir).with_context(|| format!("failed to read partials dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to stat {}", path.display()))?;
        if file_type.is_dir() {
            collect(root, &path, out)?;
        } else {
            let name = path
                .strip_prefix(root)
                .with_context(|| format!("partial path {} escaped partials root", path.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read partial {}", path.display()))?;
            out.insert(name, source);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn loads_partials_including_subdirs() {
        let dir = TempDir::new().unwrap();
        let partials = dir.path().join("partials");
        fs::create_dir_all(partials.join("sub")).unwrap();
        fs::write(partials.join("greeting"), "hi {{ name }}").unwrap();
        fs::write(partials.join("sub/inner"), "nested").unwrap();

        let loaded = FsPartialSource.load(dir.path()).unwrap();

        assert_eq!(loaded.get("greeting").map(String::as_str), Some("hi {{ name }}"));
        assert_eq!(loaded.get("sub/inner").map(String::as_str), Some("nested"));
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn absent_partials_dir_returns_empty_map() {
        let dir = TempDir::new().unwrap();
        let loaded = FsPartialSource.load(dir.path()).unwrap();
        assert!(loaded.is_empty());
    }
}
