//! 템플릿 스토어 조회 포트: `TemplateStore`, `SourceRootSource`.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// 스토어 이름 또는 로컬 경로를 템플릿 루트 경로로 해석하는 포트(§2).
pub trait TemplateStore {
    fn resolve(&self, name_or_path: &str) -> Result<PathBuf>;
}

/// `.scaffoldroot`를 해석해 실효 소스 루트를 얻는 포트(§1.7).
pub trait SourceRootSource {
    fn resolve(&self, template_root: &Path) -> Result<PathBuf>;
}
