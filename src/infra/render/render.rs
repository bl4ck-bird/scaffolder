//! MiniJinja `Environment` setup (partial registration, `scaffolder.*` builtins, `env()`) — `Renderer`.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use minijinja::value::{Object, Value as JinjaValue};
use minijinja::{Environment, UndefinedBehavior};

use crate::domain::answer::{AnswerContext, AnswerValue, ScaffolderBuiltins};
use crate::domain::data::DataValue;
use crate::domain::render::{Renderer, SyntaxChecker};

/// MiniJinja-based `Renderer`. Wires strict undefined and the `scaffolder.*`/`env()` builtins.
pub struct MiniJinjaRenderer {
    env: Environment<'static>,
}

impl MiniJinjaRenderer {
    pub fn new() -> Self {
        let mut env = base_environment();
        // minijinja trims the trailing newline by default; preserve it to honor generated files'
        // `insert_final_newline` convention.
        env.set_keep_trailing_newline(true);
        Self { env }
    }

    /// Registers the partials as named templates so `{% include "name" %}` can pull them in.
    /// Because `include` only resolves names that were registered, there is no way to include
    /// anything from outside `partials/`: an unregistered name is simply an error.
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

/// Base `Environment` with strict undefined + the `env()` builtin. Shared by rendering and `when` evaluation.
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

/// MiniJinja-based `SyntaxChecker`. It only compiles (parses), never renders/evaluates, so
/// strict-undefined variable references are not caught — deliberate, so `template validate` does
/// not report runtime-undefined as a false positive.
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
        // Use a fresh scratch environment on every call so we only surface parse errors and do not
        // accumulate registered templates, which would let state bleed across repeated validate
        // calls. Pass the minijinja error straight through into anyhow: its Display already includes
        // "syntax error" and a source location, so wrapping it in extra context would only hide that.
        let mut env = base_environment();
        env.add_template_owned("__scaffolder_validate__".to_string(), source.to_string())?;
        Ok(())
    }

    fn check_expression(&self, source: &str) -> Result<()> {
        self.env.compile_expression_owned(source.to_string())?;
        Ok(())
    }
}

/// Exposes an `AnswerContext` to the templates through name-by-name dynamic lookup. The port has
/// no API for enumerating everything at once, so top-level references like `{{ name }}` and
/// `scaffolder.*` are resolved one value at a time. It is visible within the crate so the same
/// context mapping can be reused when evaluating `when` expressions.
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
    fn env_present_var_renders_value() {
        // SAFETY: the test process does not manage env single-threaded, but a unique var name
        // avoids name collisions under parallel test execution and it is cleaned up at the end.
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
        // The parse stage does not apply strict-undefined — runtime-undefined is out of scope.
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
