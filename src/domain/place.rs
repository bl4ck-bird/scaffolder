//! `RelPath`, `safe_rel_path`, 소스 충돌 탐지, `FileMode`와 `PayloadStore` 포트
//! (payload 읽기 + target 쓰기).

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Result};

/// 정규화된 상대 경로. 리터럴 `..`와 절대 경로를 배제한 뒤에만 생성 가능하다.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(PathBuf);

impl RelPath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

/// 컴포넌트 단위로 정규화하고 리터럴 `..` 이탈·절대경로를 거부한다.
pub fn safe_rel_path(input: &str) -> Result<RelPath> {
    let mut normalized = PathBuf::new();

    for component in Path::new(input).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                bail!("path {input:?} escapes root via literal '..'")
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("absolute path {input:?} is not allowed")
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        bail!("path {input:?} is empty after normalization");
    }

    Ok(RelPath(normalized))
}

/// Unix permission bits: `0o666` base → executable `|0o111` → private `&^0o77` →
/// readonly `&^0o222`. 특수비트 없음.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMode(u32);

impl FileMode {
    pub fn base() -> Self {
        FileMode(0o666)
    }

    pub fn with_executable(self) -> Self {
        FileMode(self.0 | 0o111)
    }

    pub fn with_private(self) -> Self {
        FileMode(self.0 & !0o77)
    }

    pub fn with_readonly(self) -> Self {
        FileMode(self.0 & !0o222)
    }

    pub fn bits(self) -> u32 {
        self.0
    }
}

/// payload 엔트리 하나(write 단계 입력).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadEntry {
    pub rel: RelPath,
    pub is_dir: bool,
}

/// dest 상태 판정 결과(write 단계 confirm 판단용). confirm 자체는 하지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestStatus {
    /// 심링크 해석 후 실제 기록 위치.
    pub final_path: PathBuf,
    /// per-component containment 통과 여부(false=외부쓰기 confirm 대상).
    pub inside_target: bool,
    /// dest가 이미 존재하는지(심링크 자체 포함).
    pub exists: bool,
    /// dest가 심링크인지(심링크 자체, 대상이 아님).
    pub is_symlink: bool,
}

/// payload 열거/읽기 + target 쓰기 포트. infra가 구현한다.
pub trait PayloadStore {
    fn list_entries(&self, source_root: &Path) -> Result<Vec<PayloadEntry>>;
    fn read_content(&self, source_root: &Path, entry: &PayloadEntry) -> Result<Vec<u8>>;
    fn write_file(
        &self,
        target_root: &Path,
        rel: &RelPath,
        content: &[u8],
        mode: FileMode,
    ) -> Result<()>;
    fn dest_status(&self, target_root: &Path, rel: &RelPath) -> Result<DestStatus>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_relative_path() {
        let rel = safe_rel_path("a/b.txt").unwrap();
        assert_eq!(rel.as_path(), std::path::Path::new("a/b.txt"));
    }

    #[test]
    fn rejects_literal_parent_dir_escape() {
        assert!(safe_rel_path("../x").is_err());
    }

    #[test]
    fn rejects_absolute_path() {
        assert!(safe_rel_path("/etc/x").is_err());
    }

    #[test]
    fn rejects_literal_parent_dir_even_when_it_resolves_inside() {
        assert!(safe_rel_path("a/../b").is_err());
    }

    #[test]
    fn file_mode_computes_executable_private_readonly() {
        let base = FileMode::base();
        assert_eq!(base.bits(), 0o666);
        assert_eq!(base.with_executable().bits(), 0o777);
        assert_eq!(base.with_private().bits(), 0o600);
        assert_eq!(base.with_readonly().bits(), 0o444);
        assert_eq!(
            base.with_executable().with_private().bits(),
            0o700
        );
    }
}
