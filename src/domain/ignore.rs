//! 제외 glob 매칭(출력 경로)과 `IgnoreSource` 포트.

use std::path::Path;

use anyhow::Result;

use crate::domain::answer::AnswerContext;
use crate::domain::place::RelPath;

/// 출력 경로 하나가 제외 대상인지 판정한다. infra가 gitignore 시맨틱으로 구현한다.
pub trait IgnoreMatcher {
    fn is_ignored(&self, rel: &RelPath) -> bool;
}

/// `.scaffoldignore`(`.jinja`) 로드 포트. infra가 구현한다.
pub trait IgnoreSource {
    fn load(&self, template_root: &Path, ctx: &AnswerContext) -> Result<Box<dyn IgnoreMatcher>>;
}
