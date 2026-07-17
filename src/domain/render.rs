//! `Renderer`·`PartialSource` 포트.

use anyhow::Result;

use crate::domain::answer::AnswerContext;

/// 템플릿 문자열 + `AnswerContext` → 렌더 결과. infra가 MiniJinja로 구현한다.
pub trait Renderer {
    fn render_str(&self, template: &str, context: &AnswerContext) -> Result<String>;
}

/// `partials/` 이름 조회 포트(§1.4). 이후 슬라이스에서 배선한다.
pub trait PartialSource {
    fn partial(&self, name: &str) -> Result<Option<String>>;
}
