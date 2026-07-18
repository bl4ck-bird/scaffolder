//! `Renderer`·`PartialSource`·`SyntaxChecker` 포트.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::domain::answer::AnswerContext;

/// 템플릿 문자열 + `AnswerContext` → 렌더 결과. infra가 MiniJinja로 구현한다.
pub trait Renderer {
    fn render_str(&self, template: &str, context: &AnswerContext) -> Result<String>;
}

/// `partials/` 조각 로딩 포트. `{% include %}`가 이름으로 pull하려면 렌더러가 업프런트에 전부
/// 등록해야 하므로 이름→소스 맵으로 열거한다. 이름은 `partials/` 상대경로(`/` 구분). `partials/`
/// 부재면 빈 맵.
pub trait PartialSource {
    fn load(&self, template_root: &Path) -> Result<BTreeMap<String, String>>;
}

/// 렌더/평가 없이 문법만 컴파일 검사하는 포트(`template validate` 정적 검사용). strict-undefined
/// 변수 참조는 파스 단계에서 걸리지 않으므로 검사 대상이 아니다(런타임 미정의 false positive 회피).
pub trait SyntaxChecker {
    fn check_template(&self, source: &str) -> Result<()>;
    fn check_expression(&self, source: &str) -> Result<()>;
}
