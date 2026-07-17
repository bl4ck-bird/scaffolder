//! `when` 조건식 평가(strict undefined) — `ConditionEvaluator`.

use anyhow::{bail, Context as _, Result};
use minijinja::value::Value as JinjaValue;
use minijinja::Environment;

use crate::domain::answer::{AnswerContext, ConditionEvaluator};
use crate::infra::render::render::{base_environment, RenderContext};

/// MiniJinja 기반 `ConditionEvaluator`. render.rs와 동일한 strict undefined + `env()` 설정을
/// 공유한다.
pub struct MiniJinjaConditionEvaluator {
    env: Environment<'static>,
}

impl MiniJinjaConditionEvaluator {
    pub fn new() -> Self {
        Self {
            env: base_environment(),
        }
    }
}

impl Default for MiniJinjaConditionEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl ConditionEvaluator for MiniJinjaConditionEvaluator {
    fn is_active(&self, when: &str, ctx: &AnswerContext) -> Result<bool> {
        let expr = self
            .env
            .compile_expression(when)
            .with_context(|| format!("failed to compile `when` expression {when:?}"))?;
        let ctx_value = JinjaValue::from_object(RenderContext(ctx.clone()));
        let result = expr
            .eval(ctx_value)
            .with_context(|| format!("failed to evaluate `when` expression {when:?}"))?;
        // minijinja's strict undefined only fires when an undefined value is used in an
        // operation; a bare undefined reference (e.g. `when = "nope"`) evaluates to Undefined
        // without erroring, so it must be rejected explicitly here.
        if result.is_undefined() {
            bail!("`when` expression {when:?} references an undefined value");
        }
        Ok(result.is_true())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::answer::{build_context, AnswerValue, ScaffolderBuiltins};
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

    fn ctx_with(name: &str, value: AnswerValue) -> AnswerContext {
        let mut answers = BTreeMap::new();
        answers.insert(name.to_string(), value);
        build_context(answers, crate::domain::data::DataValue::empty_table(), builtins())
    }

    #[test]
    fn membership_check_is_true_when_value_present_in_list() {
        let ctx = ctx_with("stacks", AnswerValue::List(vec!["ci".to_string()]));
        let evaluator = MiniJinjaConditionEvaluator::new();

        assert!(evaluator.is_active("'ci' in stacks", &ctx).unwrap());
    }

    #[test]
    fn membership_check_is_false_when_list_empty() {
        let ctx = ctx_with("stacks", AnswerValue::List(vec![]));
        let evaluator = MiniJinjaConditionEvaluator::new();

        assert!(!evaluator.is_active("'ci' in stacks", &ctx).unwrap());
    }

    #[test]
    fn membership_check_is_false_when_value_not_in_list() {
        let ctx = ctx_with("stacks", AnswerValue::List(vec!["docker".to_string()]));
        let evaluator = MiniJinjaConditionEvaluator::new();

        assert!(!evaluator.is_active("'ci' in stacks", &ctx).unwrap());
    }

    #[test]
    fn numeric_comparison_is_true_when_edition_at_least_2021() {
        let ctx = ctx_with("edition", AnswerValue::Int(2021));
        let evaluator = MiniJinjaConditionEvaluator::new();

        assert!(evaluator.is_active("edition >= 2021", &ctx).unwrap());
    }

    #[test]
    fn numeric_comparison_is_false_when_edition_below_2021() {
        let ctx = ctx_with("edition", AnswerValue::Int(2018));
        let evaluator = MiniJinjaConditionEvaluator::new();

        assert!(!evaluator.is_active("edition >= 2021", &ctx).unwrap());
    }

    #[test]
    fn undefined_variable_reference_errors() {
        let ctx = ctx_with("edition", AnswerValue::Int(2021));
        let evaluator = MiniJinjaConditionEvaluator::new();

        assert!(evaluator.is_active("nope", &ctx).is_err());
    }

    #[test]
    fn undefined_variable_used_in_operation_errors() {
        let ctx = ctx_with("edition", AnswerValue::Int(2021));
        let evaluator = MiniJinjaConditionEvaluator::new();

        assert!(evaluator.is_active("'ci' in nope", &ctx).is_err());
    }

    #[test]
    fn builtins_and_env_are_available_in_conditions() {
        let ctx = ctx_with("edition", AnswerValue::Int(2021));
        let evaluator = MiniJinjaConditionEvaluator::new();

        assert!(evaluator.is_active("scaffolder.os == 'macos'", &ctx).unwrap());
        assert!(evaluator.is_active("env('SC_DEFINITELY_ABSENT') == ''", &ctx).unwrap());
    }
}
