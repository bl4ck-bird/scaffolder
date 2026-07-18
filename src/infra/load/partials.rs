//! `partials/` 로드 — `PartialSource`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::domain::render::PartialSource;
use crate::infra::load::trust::ensure_within_root;

/// `<template_root>/partials/` 하위 조각을 이름→소스로 로드한다. `partials/`가 없으면 빈 맵.
/// §1.8: `partials/` 루트나 재귀 하위의 leaf가 외부 심링크(dir 경유 포함)면 `trust` 없이 거부한다.
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
        // 재귀 시작 dir을 조상 체인에 미리 넣어, 심링크 하위 dir이 partials 루트(또는 조상)를
        // 되가리키는 진짜 cycle을 첫 재진입에서 차단한다. 이 집합은 "현재 재귀 경로의 조상"만
        // 담는다(모든 방문 dir 누적이 아니다) — 그래야 diamond(서로 다른 두 심링크가 같은 내부
        // dir을 가리키는 것, cycle 아님)가 둘 다 순회된다.
        let root_dir_canon = partials_dir
            .canonicalize()
            .with_context(|| format!("failed to resolve partials dir {}", partials_dir.display()))?;
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
    let entries =
        fs::read_dir(dir).with_context(|| format!("failed to read partials dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        // follow해 판정한다: no-follow(`DirEntry::file_type()`)는 심링크-to-디렉토리를 symlink로
        // 봐 leaf로 오판하고 `read_to_string`이 "Is a directory"로 실패한다(--trust로 dir 심링크를
        // 허용해도 안 내려가면 완결성이 깨진다). broken 심링크는 fail-loud.
        let meta = fs::metadata(&path)
            .with_context(|| format!("failed to stat {} (broken symlink?)", path.display()))?;
        if meta.is_dir() {
            ensure_within_root(&path, root_canon, trust)?;
            let canon = path
                .canonicalize()
                .with_context(|| format!("failed to resolve {}", path.display()))?;
            // 심링크 dir이 자신의 조상(자기 자신 포함)을 가리키는 진짜 cycle만 끊는다 — 형제
            // 가지에서 이미 방문한 dir은(diamond) 조상이 아니므로 걸리지 않는다.
            if ancestors.contains(&canon) {
                continue;
            }
            let mut child_ancestors = ancestors.clone();
            child_ancestors.insert(canon);
            collect(root, &path, out, root_canon, trust, &child_ancestors)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .with_context(|| format!("partial path {} escaped partials root", path.display()))?;
            // 이름은 `{% include %}`에서 UTF-8 문자열로 참조되므로 비-UTF8 경로는 lossy 변환 시
            // 다른 파일과 같은 이름으로 축약돼 조용히 덮어쓸 수 있다 — fail-loud로 거부한다.
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

        assert_eq!(loaded.get("greeting").map(String::as_str), Some("hi {{ name }}"));
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
        assert_eq!(loaded.get("greeting").map(String::as_str), Some("hi {{ name }}"));
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

    /// diamond: 서로 다른 두 심링크(`a`, `b`)가 같은 내부 dir(`real`)을 가리키는 것은 cycle이
    /// 아니다 — partial 이름은 partials-root-상대경로라 `a/x`≠`b/x`≠`real/x`이므로 셋 다 로드돼야
    /// 한다. 전역 visited(모든 방문 dir 누적)는 먼저 처리된 alias만 남기고 나머지를 조용히
    /// 누락시킨다; ancestor-chain(현재 재귀 경로의 조상만 추적)이어야 diamond가 보존된다.
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
        // `partials/loop` -> `partials` 자기 자신을 가리키는 순환.
        symlink(&partials, partials.join("loop")).unwrap();

        let loaded = source(dir.path()).load(dir.path()).unwrap();
        assert_eq!(loaded.get("greeting").map(String::as_str), Some("hi"));
    }
}
