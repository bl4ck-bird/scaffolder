//! MiniJinja `Environment` 구성(partials 등록·`scaffolder.*` 빌트인·`env()` 함수) — `Renderer`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use minijinja::value::{Object, Value as JinjaValue};
use minijinja::{Environment, UndefinedBehavior};

use crate::domain::answer::{AnswerContext, AnswerValue, ScaffolderBuiltins};
use crate::domain::render::Renderer;

/// MiniJinja 기반 `Renderer`. strict undefined와 `scaffolder.*`/`env()` 빌트인을 배선한다.
pub struct MiniJinjaRenderer {
    env: Environment<'static>,
}

impl MiniJinjaRenderer {
    pub fn new() -> Self {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.add_function("env", env_fn);
        Self { env }
    }
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

/// `AnswerContext`를 이름 기반 동적 조회로 노출한다. 포트가 전체 열거 API를 제공하지 않으므로
/// top-level(`{{ name }}`)과 `scaffolder.*` 조회는 값 단위로 위임한다.
#[derive(Debug)]
struct RenderContext(AnswerContext);

impl Object for RenderContext {
    fn get_value(self: &Arc<Self>, key: &JinjaValue) -> Option<JinjaValue> {
        let key = key.as_str()?;
        if key == "scaffolder" {
            return Some(builtins_value(self.0.builtins()));
        }
        self.0.answer(key).map(answer_value)
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
        let ctx = build_context(answers, builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer.render_str("hi {{ name }}", &ctx).unwrap();

        assert_eq!(out, "hi proj");
    }

    #[test]
    fn renders_scaffolder_builtin() {
        let ctx = build_context(BTreeMap::new(), builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer.render_str("{{ scaffolder.os }}", &ctx).unwrap();

        assert_eq!(out, "macos");
    }

    #[test]
    fn env_missing_var_renders_empty() {
        let ctx = build_context(BTreeMap::new(), builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer
            .render_str("{{ env(\"SC_TEST_ABSENT\") }}", &ctx)
            .unwrap();

        assert_eq!(out, "");
    }

    #[test]
    fn env_missing_var_uses_default() {
        let ctx = build_context(BTreeMap::new(), builtins());

        let renderer = MiniJinjaRenderer::new();
        let out = renderer
            .render_str("{{ env(\"SC_TEST_ABSENT\", \"d\") }}", &ctx)
            .unwrap();

        assert_eq!(out, "d");
    }

    #[test]
    fn strict_undefined_errors_on_unknown_variable() {
        let ctx = build_context(BTreeMap::new(), builtins());

        let renderer = MiniJinjaRenderer::new();
        let result = renderer.render_str("{{ nope }}", &ctx);

        assert!(result.is_err());
    }
}
