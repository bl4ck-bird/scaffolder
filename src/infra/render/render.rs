//! MiniJinja `Environment` 구성(partials 등록·`scaffolder.*` 빌트인·`env()` 함수) — `Renderer`.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use minijinja::value::{Object, Value as JinjaValue};
use minijinja::{Environment, UndefinedBehavior};

use crate::domain::answer::{AnswerContext, AnswerValue, ScaffolderBuiltins};
use crate::domain::data::DataValue;
use crate::domain::render::{Renderer, SyntaxChecker};

/// MiniJinja 기반 `Renderer`. strict undefined와 `scaffolder.*`/`env()` 빌트인을 배선한다.
pub struct MiniJinjaRenderer {
    env: Environment<'static>,
}

impl MiniJinjaRenderer {
    pub fn new() -> Self {
        let mut env = base_environment();
        // minijinja 기본은 trailing newline을 잘라낸다; 생성 파일의 `insert_final_newline` 관례를
        // 지키려면 보존해야 한다.
        env.set_keep_trailing_newline(true);
        Self { env }
    }

    /// partials를 명명 템플릿으로 등록해 `{% include "name" %}`가 pull할 수 있게 한다. include는
    /// 등록된 이름만 해석하므로 `partials/` 밖 include는 불가능(미등록 이름=에러).
    pub fn with_partials(partials: BTreeMap<String, String>) -> Result<Self> {
        let mut env = base_environment();
        env.set_keep_trailing_newline(true);
        for (name, source) in partials {
            env.add_template_owned(name, source)
                .context("failed to register partial template")?;
        }
        Ok(Self { env })
    }
}

/// strict undefined + `env()` 빌트인을 갖춘 기본 `Environment`. 렌더와 `when` 표현식 평가가
/// 공유한다.
pub(crate) fn base_environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.add_function("env", env_fn);
    crate::infra::render::filters::register(&mut env);
    env
}

impl Default for MiniJinjaRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer for MiniJinjaRenderer {
    fn render_str(&self, template: &str, context: &AnswerContext) -> Result<String> {
        let ctx = JinjaValue::from_object(RenderContext(context.clone()));
        self.env
            .render_str(template, ctx)
            .context("template render failed")
    }
}

fn env_fn(name: String, default: Option<String>) -> String {
    std::env::var(&name).unwrap_or_else(|_| default.unwrap_or_default())
}

/// MiniJinja 기반 `SyntaxChecker`. 컴파일(파스)만 하고 렌더/평가는 하지 않으므로 strict-undefined
/// 변수 참조는 걸리지 않는다 — `template validate`가 런타임 미정의를 false positive로 보고하지
/// 않기 위한 의도적 설계다.
pub struct MiniJinjaSyntaxChecker {
    env: Environment<'static>,
}

impl MiniJinjaSyntaxChecker {
    pub fn new() -> Self {
        Self {
            env: base_environment(),
        }
    }
}

impl Default for MiniJinjaSyntaxChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntaxChecker for MiniJinjaSyntaxChecker {
    fn check_template(&self, source: &str) -> Result<()> {
        // 매 호출 스크래치 environment에 등록해 파스 에러만 잡는다 — 등록된 템플릿을 누적하지
        // 않는다(반복 validate 호출 간 상태 오염 방지). minijinja 에러를 그대로 anyhow로
        // 올린다 — 자체 Display가 이미 "syntax error"와 위치를 담으므로 추가 context로
        // 가리지 않는다.
        let mut env = base_environment();
        env.add_template_owned("__scaffolder_validate__".to_string(), source.to_string())?;
        Ok(())
    }

    fn check_expression(&self, source: &str) -> Result<()> {
        self.env.compile_expression_owned(source.to_string())?;
        Ok(())
    }
}

/// `AnswerContext`를 이름 기반 동적 조회로 노출한다. 포트가 전체 열거 API를 제공하지 않으므로
/// top-level(`{{ name }}`)과 `scaffolder.*` 조회는 값 단위로 위임한다. `when` 표현식 평가와
/// 컨텍스트 매핑을 공유하기 위해 crate 내부에 노출한다.
#[derive(Debug)]
pub(crate) struct RenderContext(pub(crate) AnswerContext);

impl Object for RenderContext {
    fn get_value(self: &Arc<Self>, key: &JinjaValue) -> Option<JinjaValue> {
        let key = key.as_str()?;
        if key == "scaffolder" {
            return Some(builtins_value(self.0.builtins()));
        }
        if key == "data" {
            return self.0.data().map(data_value);
        }
        self.0.answer(key).map(answer_value)
    }
}

fn data_value(value: &DataValue) -> JinjaValue {
    match value {
        DataValue::Table(map) => map
            .iter()
            .map(|(k, v)| (k.as_str(), data_value(v)))
            .collect(),
        DataValue::Array(items) => items.iter().map(data_value).collect(),
        DataValue::Str(s) => JinjaValue::from(s.as_str()),
        DataValue::Int(i) => JinjaValue::from(*i),
        DataValue::Float(f) => JinjaValue::from(*f),
        DataValue::Bool(b) => JinjaValue::from(*b),
    }
}

fn builtins_value(builtins: &ScaffolderBuiltins) -> JinjaValue {
    [
        ("name", JinjaValue::from(builtins.name.as_str())),
        (
            "target",
            JinjaValue::from(builtins.target.to_string_lossy().into_owned()),
        ),
        ("os", JinjaValue::from(builtins.os.as_str())),
        ("arch", JinjaValue::from(builtins.arch.as_str())),
        ("username", JinjaValue::from(builtins.username.as_str())),
    ]
    .into_iter()
    .collect()
}

fn answer_value(value: &AnswerValue) -> JinjaValue {
    match value {
        AnswerValue::Text(s) => JinjaValue::from(s.as_str()),
        AnswerValue::List(items) => items.iter().map(String::as_str).collect(),
        AnswerValue::Int(i) => JinjaValue::from(*i),
        AnswerValue::Float(f) => JinjaValue::from(*f),
        AnswerValue::Bool(b) => JinjaValue::from(*b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::answer::build_context;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn builtins() -> ScaffolderBuiltins {
        ScaffolderBuiltins {
            name: "demo".to_string(),
            target: PathBuf::from("/tmp/demo"),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            username: "bl4ckbird".to_string(),
        }
    }

    #[test]
    fn renders_top_level_answer() {
        let mut answers = BTreeMap::new();
        answers.insert("name".to_string(), AnswerValue::Text("proj".to_string()));
        let ctx = build_context(answers, Some(DataValue::empty_table()), builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer.render_str("hi {{ name }}", &ctx).unwrap();

        assert_eq!(out, "hi proj");
    }

    #[test]
    fn renders_scaffolder_builtin() {
        let ctx = build_context(BTreeMap::new(), Some(DataValue::empty_table()), builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer.render_str("{{ scaffolder.os }}", &ctx).unwrap();

        assert_eq!(out, "macos");
    }

    #[test]
    fn env_missing_var_renders_empty() {
        let ctx = build_context(BTreeMap::new(), Some(DataValue::empty_table()), builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer
            .render_str("{{ env(\"SC_TEST_ABSENT\") }}", &ctx)
            .unwrap();

        assert_eq!(out, "");
    }

    #[test]
    fn env_missing_var_uses_default() {
        let ctx = build_context(BTreeMap::new(), Some(DataValue::empty_table()), builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer
            .render_str("{{ env(\"SC_TEST_ABSENT\", \"d\") }}", &ctx)
            .unwrap();

        assert_eq!(out, "d");
    }

    #[test]
    fn strict_undefined_errors_on_unknown_variable() {
        let ctx = build_context(BTreeMap::new(), Some(DataValue::empty_table()), builtins());

        let renderer = MiniJinjaRenderer::new();
        let result = renderer.render_str("{{ nope }}", &ctx);

        assert!(result.is_err());
    }

    #[test]
    fn trailing_newline_is_preserved() {
        let ctx = build_context(BTreeMap::new(), Some(DataValue::empty_table()), builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer.render_str("line\n", &ctx).unwrap();

        assert_eq!(out, "line\n");
    }

    #[test]
    fn int_answer_preserves_numeric_comparison() {
        let mut answers = BTreeMap::new();
        answers.insert("edition".to_string(), AnswerValue::Int(2021));
        let ctx = build_context(answers, Some(DataValue::empty_table()), builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer
            .render_str("{% if edition >= 2021 %}yes{% else %}no{% endif %}", &ctx)
            .unwrap();

        assert_eq!(out, "yes");
    }

    #[test]
    fn env_present_var_renders_value() {
        // SAFETY: 테스트 프로세스는 단일 스레드로 env를 다루지 않지만, 테스트 병렬 실행 시
        // 이름 충돌을 피하기 위해 고유한 var명을 쓰고 끝에 정리한다.
        unsafe {
            std::env::set_var("SC_TEST_PRESENT", "v");
        }
        let ctx = build_context(BTreeMap::new(), Some(DataValue::empty_table()), builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer
            .render_str("{{ env(\"SC_TEST_PRESENT\") }}", &ctx)
            .unwrap();

        unsafe {
            std::env::remove_var("SC_TEST_PRESENT");
        }

        assert_eq!(out, "v");
    }

    #[test]
    fn syntax_checker_accepts_valid_template() {
        let checker = MiniJinjaSyntaxChecker::new();
        assert!(checker.check_template("hi {{ name }}").is_ok());
    }

    #[test]
    fn syntax_checker_rejects_malformed_template() {
        let checker = MiniJinjaSyntaxChecker::new();
        assert!(checker.check_template("{% if unterminated %}").is_err());
    }

    #[test]
    fn syntax_checker_does_not_error_on_undefined_variable_reference() {
        // 파스 단계는 strict-undefined를 적용하지 않는다 — 런타임 미정의는 검사 대상이 아니다.
        let checker = MiniJinjaSyntaxChecker::new();
        assert!(
            checker
                .check_template("{{ totally_undefined_var }}")
                .is_ok()
        );
    }

    #[test]
    fn syntax_checker_accepts_valid_expression() {
        let checker = MiniJinjaSyntaxChecker::new();
        assert!(checker.check_expression("edition >= 2021").is_ok());
    }

    #[test]
    fn syntax_checker_rejects_malformed_expression() {
        let checker = MiniJinjaSyntaxChecker::new();
        assert!(checker.check_expression("edition >=").is_err());
    }
}
