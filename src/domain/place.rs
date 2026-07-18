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

    /// base(`0o666`)에 mode prefix를 적용한다. 파일명의 접두사 순서는 무관하며, 계산은 §1.3의
    /// 고정 순서(executable → private → readonly)로 적용한다 — `|0o111`과 `&^0o77`은 비가환이라
    /// 적용 순서가 결과를 바꾸기 때문이다. stackable.
    pub fn from_modes(modes: &[crate::domain::name::Mode]) -> Self {
        use crate::domain::name::Mode;
        let mut mode = FileMode::base();
        if modes.contains(&Mode::Executable) {
            mode = mode.with_executable();
        }
        if modes.contains(&Mode::Private) {
            mode = mode.with_private();
        }
        if modes.contains(&Mode::Readonly) {
            mode = mode.with_readonly();
        }
        mode
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
    /// target 디렉토리를 보장한다(부재 시 생성). §1.9 step 6 — plan 이후 write 직전에 호출한다.
    fn ensure_target(&self, target_root: &Path) -> Result<()>;
    /// payload 파일 하나를 원자적으로 쓴다. `overwrite`가 false면 dest가 새로 생겨야 하며, 경쟁으로
    /// dest가 먼저 생기면 조용히 덮지 않고 실패한다(no-clobber). true면 기존 dest를 원자 교체한다.
    fn write_file(
        &self,
        target_root: &Path,
        rel: &RelPath,
        content: &[u8],
        mode: FileMode,
        overwrite: bool,
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

    #[test]
    fn from_modes_is_prefix_order_independent() {
        use crate::domain::name::Mode;
        // 파일명 접두사 순서가 달라도 §1.3 고정 계산 순서로 같은 결과.
        let a = FileMode::from_modes(&[Mode::Executable, Mode::Private]);
        let b = FileMode::from_modes(&[Mode::Private, Mode::Executable]);
        assert_eq!(a.bits(), b.bits());
        assert_eq!(a.bits(), 0o700);
    }

    #[test]
    fn from_modes_empty_is_base() {
        assert_eq!(FileMode::from_modes(&[]).bits(), 0o666);
    }

    #[test]
    fn from_modes_all_three_stacked() {
        use crate::domain::name::Mode;
        // exec(|0o111) → private(&^0o77) → readonly(&^0o222): 0o777→0o700→0o500(execute 유지, write 제거).
        let m = FileMode::from_modes(&[Mode::Readonly, Mode::Executable, Mode::Private]);
        assert_eq!(m.bits(), 0o500);
    }

    #[test]
    fn from_modes_two_way_combos() {
        use crate::domain::name::Mode;
        // §1.3 전체 계약을 잠근다(umask 전 8개 결과 중 2-way 조합).
        // exec+readonly: 0o777 &^0o222 = 0o555.
        assert_eq!(
            FileMode::from_modes(&[Mode::Executable, Mode::Readonly]).bits(),
            0o555
        );
        // private+readonly: 0o600 &^0o222 = 0o400.
        assert_eq!(
            FileMode::from_modes(&[Mode::Private, Mode::Readonly]).bits(),
            0o400
        );
        // exec+private: 0o777 &^0o77 = 0o700.
        assert_eq!(
            FileMode::from_modes(&[Mode::Executable, Mode::Private]).bits(),
            0o700
        );
    }
}
