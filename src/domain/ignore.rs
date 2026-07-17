//! 제외 glob 매칭(출력 경로)과 `IgnoreSource` 포트.

use std::path::Path;

use anyhow::Result;

use crate::domain::answer::AnswerContext;

/// 출력 경로 하나가 제외 대상인지 판정한다(계산된 출력 경로 문자열에 대해 — `safe_rel_path`
/// 검증 이전이므로 `RelPath`가 아니라 `Path`를 받는다). infra가 gitignore 시맨틱으로 구현한다.
pub trait IgnoreMatcher {
    fn is_ignored(&self, rel: &Path) -> bool;
}

/// `.scaffoldignore`(`.jinja`) 로드 포트. infra가 구현한다.
pub trait IgnoreSource {
    fn load(&self, template_root: &Path, ctx: &AnswerContext) -> Result<Box<dyn IgnoreMatcher>>;
}
