//! 템플릿 스토어 조회·생성·열거 포트: `TemplateStore`, `TemplateInitializer`, `SourceRootSource`,
//! `TemplateCatalog`.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::domain::skeleton::SkeletonEntry;

/// 스토어 이름 또는 로컬 경로를 템플릿 루트 경로로 해석하는 포트.
pub trait TemplateStore {
    fn resolve(&self, name_or_path: &str) -> Result<PathBuf>;
}

/// 스토어에 신규 템플릿 뼈대를 생성하는 포트. 대상 이름이 이미 존재하면 부작용 없이 에러여야
/// 한다(`template new`의 재실행 안전성).
pub trait TemplateInitializer {
    fn create(&self, name: &str, entries: &[SkeletonEntry]) -> Result<PathBuf>;
}

/// `.scaffoldroot`를 해석해 실효 소스 루트를 얻는 포트.
pub trait SourceRootSource {
    fn resolve(&self, template_root: &Path) -> Result<PathBuf>;
}

/// 스토어 base들에 걸쳐 템플릿 디렉토리를 열거하는 포트.
pub trait TemplateCatalog {
    fn list(&self) -> Result<Vec<TemplateListing>>;
}

/// 열거된 템플릿 하나 — 이름, 루트 경로, 소속 base.
///
/// 여러 base에 동명 템플릿이 있어도 열거는 전부 반환한다(base로 소속을 구분); 중복 판정과
/// 우선순위 표시는 표시 계층 소관이다.
pub struct TemplateListing {
    pub name: String,
    pub path: PathBuf,
    pub base: PathBuf,
}

/// `name`이 스토어 내 단일 경로 컴포넌트인지 검증한다: 빈 문자열·경로 구분자·`.`/`..`는 거부.
/// `FsTemplateStore::resolve`가 강제하는 규칙과 정합(그쪽은 이미 있는 스토어 항목 조회용, 이
/// 함수는 `template new`의 신규 이름 검증용).
pub fn validate_template_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        bail!("template name {name:?} must be a single path component");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_name() {
        assert!(validate_template_name("").is_err());
    }

    #[test]
    fn rejects_path_separator() {
        assert!(validate_template_name("a/b").is_err());
    }

    #[test]
    fn rejects_current_dir_component() {
        assert!(validate_template_name(".").is_err());
    }

    #[test]
    fn rejects_parent_dir_component() {
        assert!(validate_template_name("..").is_err());
    }

    #[test]
    fn accepts_single_component_name() {
        assert!(validate_template_name("my-template").is_ok());
        assert!(validate_template_name("rust_starter").is_ok());
    }
}
