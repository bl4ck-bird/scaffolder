//! 외부 심링크 제어파일 신뢰 가드. 심링크를 follow해 외부를 읽을 수 있는 각
//! 로더가 읽기 지점에서 호출한다 — pre-flight 스캔이 아니라 접근 지점 검사(파수꾼==실행자)라
//! 스캔↔읽기 TOCTOU 창이 없다.

use anyhow::{Context, Result, bail};
use std::path::Path;

/// `path`의 canonical 경로가 `root_canon` 밖이면(source root 이탈) `trust`가 아닌 한 거부한다.
/// broken 심링크(canonicalize 실패)도 에러. copier `ForbiddenPathError` 패턴.
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
