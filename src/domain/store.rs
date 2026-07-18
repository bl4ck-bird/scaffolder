//! 템플릿 스토어 조회 포트: `TemplateStore`, `SourceRootSource`.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// 스토어 이름 또는 로컬 경로를 템플릿 루트 경로로 해석하는 포트.
pub trait TemplateStore {
    fn resolve(&self, name_or_path: &str) -> Result<PathBuf>;
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
