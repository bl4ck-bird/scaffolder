//! `partials/` loading — `PartialSource`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::domain::render::PartialSource;
use crate::infra::load::trust::ensure_within_root;

/// Loads the fragments under `<template_root>/partials/` as name→source. Absent `partials/` →
/// empty map. The `partials/` root, or any leaf reached recursively (including via a dir), is
/// rejected without `trust` if it is an external symlink.
pub struct FsPartialSource {
    pub root_canon: PathBuf,
    pub trust: bool,
}

impl PartialSource for FsPartialSource {
    fn load(&self, template_root: &Path) -> Result<BTreeMap<String, String>> {
        let partials_dir = template_root.join("partials");
        let mut out = BTreeMap::new();
        if !partials_dir.exists() {
            return Ok(out);
        }
        ensure_within_root(&partials_dir, &self.root_canon, self.trust)?;
        // Seed the ancestor chain with the recursion's start dir so a true cycle — a symlinked
        // subdir pointing back at the partials root (or an ancestor) — is cut at first re-entry.
        // This set holds only "ancestors of the current recursion path" (not all visited dirs), so
        // a diamond (two distinct symlinks to the same inner dir, not a cycle) is still traversed on both.
        let root_dir_canon = partials_dir.canonicalize().with_context(|| {
            format!("failed to resolve partials dir {}", partials_dir.display())
        })?;
        let mut ancestors = HashSet::new();
        ancestors.insert(root_dir_canon);
        collect(
            &partials_dir,
            &partials_dir,
            &mut out,
            &self.root_canon,
            self.trust,
            &ancestors,
        )?;
        Ok(out)
    }
}

fn collect(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, String>,
    root_canon: &Path,
    trust: bool,
    ancestors: &HashSet<PathBuf>,
) -> Result<()> {
    let entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read partials dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        // Decide by following: no-follow (`DirEntry::file_type()`) sees a symlink-to-directory as a
        // symlink, misjudges it as a leaf, and read_to_string then fails with "Is a directory"
        // (allowing dir symlinks via --trust but not descending breaks completeness). Broken symlink = fail-loud.
        let meta = fs::metadata(&path)
            .with_context(|| format!("failed to stat {} (broken symlink?)", path.display()))?;
        if meta.is_dir() {
            ensure_within_root(&path, root_canon, trust)?;
            let canon = path
                .canonicalize()
                .with_context(|| format!("failed to resolve {}", path.display()))?;
            // Cut only a true cycle where a symlinked dir points at its own ancestor (including
            // itself) — a dir already visited on a sibling branch (diamond) is not an ancestor.
            if ancestors.contains(&canon) {
                continue;
            }
            let mut child_ancestors = ancestors.clone();
            child_ancestors.insert(canon);
            collect(root, &path, out, root_canon, trust, &child_ancestors)?;
        } else {
            let rel = path.strip_prefix(root).with_context(|| {
                format!("partial path {} escaped partials root", path.display())
            })?;
            // Names are referenced as UTF-8 strings in `{% include %}`, so a non-UTF-8 path could
            // collapse under lossy conversion to another file's name and silently overwrite it — fail loud.
            let name = rel
                .to_str()
                .ok_or_else(|| anyhow!("partial name {} is not valid UTF-8", rel.display()))?
                .replace('\\', "/");
            ensure_within_root(&path, root_canon, trust)?;
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

    fn source(dir: &Path) -> FsPartialSource {
        FsPartialSource {
            root_canon: dir.canonicalize().unwrap(),
            trust: false,
        }
    }

    #[test]
    fn loads_partials_including_subdirs() {
        let dir = TempDir::new().unwrap();
        let partials = dir.path().join("partials");
        fs::create_dir_all(partials.join("sub")).unwrap();
        fs::write(partials.join("greeting"), "hi {{ name }}").unwrap();
        fs::write(partials.join("sub/inner"), "nested").unwrap();

        let loaded = source(dir.path()).load(dir.path()).unwrap();

        assert_eq!(
            loaded.get("greeting").map(String::as_str),
            Some("hi {{ name }}")
        );
        assert_eq!(loaded.get("sub/inner").map(String::as_str), Some("nested"));
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn absent_partials_dir_returns_empty_map() {
        let dir = TempDir::new().unwrap();
        let loaded = source(dir.path()).load(dir.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn internal_symlinked_partial_is_allowed() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let partials = dir.path().join("partials");
        fs::create_dir_all(&partials).unwrap();
        let real = dir.path().join("real-partial");
        fs::write(&real, "hi {{ name }}").unwrap();
        symlink(&real, partials.join("greeting")).unwrap();

        let loaded = source(dir.path()).load(dir.path()).unwrap();
        assert_eq!(
            loaded.get("greeting").map(String::as_str),
            Some("hi {{ name }}")
        );
    }

    #[test]
    fn external_symlinked_partial_is_rejected_without_trust() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let partials = dir.path().join("partials");
        fs::create_dir_all(&partials).unwrap();
        let outside = TempDir::new().unwrap();
        let external = outside.path().join("secret-partial");
        fs::write(&external, "hi").unwrap();
        symlink(&external, partials.join("greeting")).unwrap();

        let result = source(dir.path()).load(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn external_symlinked_partial_is_allowed_with_trust() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let partials = dir.path().join("partials");
        fs::create_dir_all(&partials).unwrap();
        let outside = TempDir::new().unwrap();
        let external = outside.path().join("secret-partial");
        fs::write(&external, "hi").unwrap();
        symlink(&external, partials.join("greeting")).unwrap();

        let trusted = FsPartialSource {
            root_canon: dir.path().canonicalize().unwrap(),
            trust: true,
        };
        let loaded = trusted.load(dir.path()).unwrap();
        assert_eq!(loaded.get("greeting").map(String::as_str), Some("hi"));
    }

    #[test]
    fn external_symlinked_partials_root_is_rejected_without_trust() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let external_partials = outside.path().join("partials");
        fs::create_dir_all(&external_partials).unwrap();
        fs::write(external_partials.join("greeting"), "hi").unwrap();
        symlink(&external_partials, dir.path().join("partials")).unwrap();

        let result = source(dir.path()).load(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn internal_symlinked_subdir_partials_load() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let partials = dir.path().join("partials");
        fs::create_dir_all(&partials).unwrap();
        let real_sub = dir.path().join("real-sub");
        fs::create_dir_all(&real_sub).unwrap();
        fs::write(real_sub.join("inner"), "nested via symlink dir").unwrap();
        symlink(&real_sub, partials.join("sub")).unwrap();

        let loaded = source(dir.path()).load(dir.path()).unwrap();
        assert_eq!(
            loaded.get("sub/inner").map(String::as_str),
            Some("nested via symlink dir")
        );
    }

    #[test]
    fn external_symlinked_subdir_is_rejected_without_trust() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let partials = dir.path().join("partials");
        fs::create_dir_all(&partials).unwrap();
        let outside = TempDir::new().unwrap();
        let external_sub = outside.path().join("external-sub");
        fs::create_dir_all(&external_sub).unwrap();
        fs::write(external_sub.join("inner"), "secret nested").unwrap();
        symlink(&external_sub, partials.join("sub")).unwrap();

        let result = source(dir.path()).load(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn external_symlinked_subdir_is_allowed_with_trust() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let partials = dir.path().join("partials");
        fs::create_dir_all(&partials).unwrap();
        let outside = TempDir::new().unwrap();
        let external_sub = outside.path().join("external-sub");
        fs::create_dir_all(&external_sub).unwrap();
        fs::write(external_sub.join("inner"), "secret nested").unwrap();
        symlink(&external_sub, partials.join("sub")).unwrap();

        let trusted = FsPartialSource {
            root_canon: dir.path().canonicalize().unwrap(),
            trust: true,
        };
        let loaded = trusted.load(dir.path()).unwrap();
        assert_eq!(
            loaded.get("sub/inner").map(String::as_str),
            Some("secret nested")
        );
    }

    /// diamond: two distinct symlinks (`a`, `b`) to the same inner dir (`real`) is not a cycle —
    /// partial names are partials-root-relative, so `a/x`≠`b/x`≠`real/x` and all three must load. A
    /// global visited set (all visited dirs) would keep only the first-processed alias and silently
    /// drop the rest; an ancestor chain (only the current recursion path) preserves the diamond.
    #[test]
    fn diamond_symlinked_dirs_both_load_independently() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let partials = dir.path().join("partials");
        fs::create_dir_all(partials.join("real")).unwrap();
        fs::write(partials.join("real/x"), "hi").unwrap();
        symlink(partials.join("real"), partials.join("a")).unwrap();
        symlink(partials.join("real"), partials.join("b")).unwrap();

        let loaded = source(dir.path()).load(dir.path()).unwrap();

        assert_eq!(loaded.get("real/x").map(String::as_str), Some("hi"));
        assert_eq!(loaded.get("a/x").map(String::as_str), Some("hi"));
        assert_eq!(loaded.get("b/x").map(String::as_str), Some("hi"));
    }

    #[test]
    fn symlinked_subdir_cycle_terminates_without_infinite_recursion() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let partials = dir.path().join("partials");
        fs::create_dir_all(&partials).unwrap();
        fs::write(partials.join("greeting"), "hi").unwrap();
        // `partials/loop` -> `partials`: a cycle pointing back at itself.
        symlink(&partials, partials.join("loop")).unwrap();

        let loaded = source(dir.path()).load(dir.path()).unwrap();
        assert_eq!(loaded.get("greeting").map(String::as_str), Some("hi"));
    }
}
