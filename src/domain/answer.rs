//! Answer values, the immutable answer context, and the `AnswerSource` /
//! `ConditionEvaluator` ports.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

use crate::domain::data::DataValue;
use crate::domain::question::{Question, QuestionType};

#[derive(Debug, Clone, PartialEq)]
pub enum AnswerValue {
    Text(String),
    List(Vec<String>),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// The `scaffolder.*` render builtins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffolderBuiltins {
    pub name: String,
    pub target: PathBuf,
    pub os: String,
    pub arch: String,
    pub username: String,
}

/// Immutable context built only by `build_context` (no public setters). When `data` is
/// `None` the `data` namespace is absent from the render context, so referencing `data`
/// in a `when` expression (evaluated before merge) fails as strict-undefined.
#[derive(Debug, Clone)]
pub struct AnswerContext {
    answers: BTreeMap<String, AnswerValue>,
    data: Option<DataValue>,
    builtins: ScaffolderBuiltins,
}

impl AnswerContext {
    pub fn answer(&self, name: &str) -> Option<&AnswerValue> {
        self.answers.get(name)
    }

    pub fn builtins(&self) -> &ScaffolderBuiltins {
        &self.builtins
    }

    /// The static data tree exposed as `data.*` (`[data]` + `data/*.toml`); `None` before merge.
    pub fn data(&self) -> Option<&DataValue> {
        self.data.as_ref()
    }
}

pub fn build_context(
    answers: BTreeMap<String, AnswerValue>,
    data: Option<DataValue>,
    builtins: ScaffolderBuiltins,
) -> AnswerContext {
    AnswerContext {
        answers,
        data,
        builtins,
    }
}

/// Canonical string for choice matching. `List` is never a choice value per spec, so its
/// comma-join is only a deterministic fallback.
pub(crate) fn canonical_string(value: &AnswerValue) -> String {
    match value {
        AnswerValue::Text(s) => s.clone(),
        AnswerValue::Int(i) => i.to_string(),
        AnswerValue::Float(f) => f.to_string(),
        AnswerValue::Bool(b) => b.to_string(),
        AnswerValue::List(items) => items.join(","),
    }
}

/// Compares a raw `--answers` string against a choice value by parsing it to the choice's
/// type — `canonical_string` only normalizes display (e.g. `f64` `2.0` → `"2"`), so choice
/// matching needs a typed comparison.
fn raw_matches_choice(raw: &str, value: &AnswerValue) -> bool {
    match value {
        AnswerValue::Text(s) => raw == s,
        AnswerValue::Int(i) => raw.parse::<i64>().is_ok_and(|v| v == *i),
        AnswerValue::Float(f) => raw.parse::<f64>().is_ok_and(|v| v.is_finite() && v == *f),
        AnswerValue::Bool(b) => match raw {
            "true" => *b,
            "false" => !*b,
            _ => false,
        },
        AnswerValue::List(_) => false,
    }
}

/// Coerces a raw `--answers` string into the question's type.
pub fn coerce(question: &Question, raw: &str) -> Result<AnswerValue> {
    let name = &question.name;
    match question.qtype {
        QuestionType::String => Ok(AnswerValue::Text(raw.to_string())),
        QuestionType::Int => raw
            .parse::<i64>()
            .map(AnswerValue::Int)
            .map_err(|e| anyhow!("invalid int value {raw:?} for question {name:?}: {e}")),
        QuestionType::Float => {
            let value: f64 = raw
                .parse()
                .map_err(|e| anyhow!("invalid float value {raw:?} for question {name:?}: {e}"))?;
            if !value.is_finite() {
                bail!(
                    "invalid float value {raw:?} for question {name:?}: must be finite (not NaN/inf)"
                );
            }
            Ok(AnswerValue::Float(value))
        }
        QuestionType::Boolean => match raw {
            "true" => Ok(AnswerValue::Bool(true)),
            "false" => Ok(AnswerValue::Bool(false)),
            other => {
                bail!(
                    "invalid boolean value {other:?} for question {name:?}: expected \"true\" or \"false\""
                )
            }
        },
        QuestionType::Select => {
            if question.choices.is_empty() {
                bail!("question {name:?} has type select but no choices are configured");
            }
            question
                .choices
                .iter()
                .find(|choice| raw_matches_choice(raw, &choice.value))
                .map(|choice| choice.value.clone())
                .ok_or_else(|| anyhow!("value {raw:?} is not a valid choice for question {name:?}"))
        }
        QuestionType::Multiselect => {
            if raw.is_empty() {
                return Ok(AnswerValue::List(vec![]));
            }
            let mut selected = Vec::new();
            for item in raw.split(',') {
                let item = item.trim();
                if item.is_empty() {
                    continue;
                }
                let choice = question
                    .choices
                    .iter()
                    .find(|choice| raw_matches_choice(item, &choice.value))
                    .ok_or_else(|| {
                        anyhow!("value {item:?} is not a valid choice for question {name:?}")
                    })?;
                selected.push(canonical_string(&choice.value));
            }
            Ok(AnswerValue::List(selected))
        }
    }
}

/// Validates an already-typed value (e.g. from `--answers-file`) against the choices;
/// a no-op for types without choices.
pub fn validate_choice(question: &Question, value: &AnswerValue) -> Result<()> {
    let name = &question.name;
    match question.qtype {
        QuestionType::Select => {
            let is_member = question
                .choices
                .iter()
                .any(|choice| canonical_string(&choice.value) == canonical_string(value));
            if is_member {
                Ok(())
            } else {
                bail!("value is not a valid choice for question {name:?}")
            }
        }
        QuestionType::Multiselect => {
            let AnswerValue::List(items) = value else {
                bail!("expected a list value for multiselect question {name:?}");
            };
            for item in items {
                let is_member = question
                    .choices
                    .iter()
                    .any(|choice| canonical_string(&choice.value) == *item);
                if !is_member {
                    bail!("value {item:?} is not a valid choice for question {name:?}");
                }
            }
            Ok(())
        }
        QuestionType::String | QuestionType::Int | QuestionType::Float | QuestionType::Boolean => {
            Ok(())
        }
    }
}

/// Port for interactive answer prompting; implemented by infra/cli.
pub trait AnswerSource {
    fn ask(&self, question: &Question) -> Result<AnswerValue>;
}

/// Port evaluating a `Question.when` expression; implemented by infra via MiniJinja.
pub trait ConditionEvaluator {
    fn is_active(&self, when: &str, ctx: &AnswerContext) -> Result<bool>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::question::{Choice, QuestionType};
    use std::path::PathBuf;

    fn question(qtype: QuestionType, choices: Vec<Choice>) -> Question {
        Question {
            name: "q".to_string(),
            qtype,
            prompt: None,
            choices,
            default: None,
            when: None,
            help: None,
        }
    }

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
    fn build_context_exposes_answers_and_builtins() {
        let mut answers = std::collections::BTreeMap::new();
        answers.insert("license".to_string(), AnswerValue::Text("MIT".to_string()));

        let ctx = build_context(answers, Some(DataValue::empty_table()), builtins());

        assert_eq!(
            ctx.answer("license"),
            Some(&AnswerValue::Text("MIT".to_string()))
        );
        assert_eq!(ctx.answer("missing"), None);
        assert_eq!(ctx.builtins().name, "demo");
        assert_eq!(ctx.builtins().os, "macos");
    }

    #[test]
    fn coerce_string_type_succeeds() {
        let q = question(QuestionType::String, vec![]);
        assert_eq!(
            coerce(&q, "hello world").unwrap(),
            AnswerValue::Text("hello world".to_string())
        );
    }

    #[test]
    fn coerce_int_succeeds_and_rejects_overflow() {
        let q = question(QuestionType::Int, vec![]);
        assert_eq!(coerce(&q, "42").unwrap(), AnswerValue::Int(42));
        assert_eq!(coerce(&q, "-7").unwrap(), AnswerValue::Int(-7));

        // i64::MAX = 9223372036854775807; one past it overflows.
        assert!(coerce(&q, "9223372036854775808").is_err());
        assert!(coerce(&q, "not-a-number").is_err());
    }

    #[test]
    fn coerce_float_succeeds_and_rejects_nan_inf_and_garbage() {
        let q = question(QuestionType::Float, vec![]);
        assert_eq!(coerce(&q, "2.75").unwrap(), AnswerValue::Float(2.75));
        assert_eq!(coerce(&q, "-0.5").unwrap(), AnswerValue::Float(-0.5));

        assert!(coerce(&q, "NaN").is_err());
        assert!(coerce(&q, "inf").is_err());
        assert!(coerce(&q, "-infinity").is_err());
        assert!(coerce(&q, "abc").is_err());
    }

    #[test]
    fn coerce_boolean_succeeds_and_rejects_other_words() {
        let q = question(QuestionType::Boolean, vec![]);
        assert_eq!(coerce(&q, "true").unwrap(), AnswerValue::Bool(true));
        assert_eq!(coerce(&q, "false").unwrap(), AnswerValue::Bool(false));

        assert!(coerce(&q, "yes").is_err());
        assert!(coerce(&q, "True").is_err());
        assert!(coerce(&q, "1").is_err());
    }

    #[test]
    fn coerce_select_matches_integer_choice_and_keeps_literal_type() {
        let choices = vec![
            Choice {
                label: "2018".to_string(),
                value: AnswerValue::Int(2018),
            },
            Choice {
                label: "2021".to_string(),
                value: AnswerValue::Int(2021),
            },
        ];
        let q = question(QuestionType::Select, choices);

        assert_eq!(coerce(&q, "2021").unwrap(), AnswerValue::Int(2021));
        assert!(coerce(&q, "2099").is_err());
    }

    #[test]
    fn coerce_select_matches_float_choice_across_formatting() {
        let choices = vec![
            Choice {
                label: "1.5".to_string(),
                value: AnswerValue::Float(1.5),
            },
            Choice {
                label: "2.0".to_string(),
                value: AnswerValue::Float(2.0),
            },
        ];
        let q = question(QuestionType::Select, choices);

        assert_eq!(coerce(&q, "2.0").unwrap(), AnswerValue::Float(2.0));
        assert_eq!(coerce(&q, "2").unwrap(), AnswerValue::Float(2.0));
        assert!(coerce(&q, "3.0").is_err());
    }

    #[test]
    fn coerce_select_rejects_when_no_choices_configured() {
        let q = question(QuestionType::Select, vec![]);
        assert!(coerce(&q, "anything").is_err());
    }

    #[test]
    fn coerce_multiselect_splits_and_matches_choices() {
        let choices = vec![
            Choice {
                label: "docker".to_string(),
                value: AnswerValue::Text("docker".to_string()),
            },
            Choice {
                label: "ci".to_string(),
                value: AnswerValue::Text("ci".to_string()),
            },
        ];
        let q = question(QuestionType::Multiselect, choices);

        assert_eq!(
            coerce(&q, "docker,ci").unwrap(),
            AnswerValue::List(vec!["docker".to_string(), "ci".to_string()])
        );
        assert_eq!(
            coerce(&q, "docker, ci").unwrap(),
            AnswerValue::List(vec!["docker".to_string(), "ci".to_string()])
        );
        assert_eq!(coerce(&q, "").unwrap(), AnswerValue::List(vec![]));
        assert!(coerce(&q, "docker,unknown").is_err());
    }

    #[test]
    fn coerce_multiselect_skips_empty_trailing_segments() {
        let choices = vec![
            Choice {
                label: "docker".to_string(),
                value: AnswerValue::Text("docker".to_string()),
            },
            Choice {
                label: "ci".to_string(),
                value: AnswerValue::Text("ci".to_string()),
            },
        ];
        let q = question(QuestionType::Multiselect, choices);

        assert_eq!(
            coerce(&q, "docker,").unwrap(),
            AnswerValue::List(vec!["docker".to_string()])
        );
        assert_eq!(coerce(&q, "").unwrap(), AnswerValue::List(vec![]));
    }

    #[test]
    fn validate_choice_select_accepts_member_and_rejects_non_member() {
        let choices = vec![
            Choice {
                label: "MIT".to_string(),
                value: AnswerValue::Text("MIT".to_string()),
            },
            Choice {
                label: "Apache-2.0".to_string(),
                value: AnswerValue::Text("Apache-2.0".to_string()),
            },
        ];
        let q = question(QuestionType::Select, choices);

        assert!(validate_choice(&q, &AnswerValue::Text("MIT".to_string())).is_ok());
        assert!(validate_choice(&q, &AnswerValue::Text("BSD".to_string())).is_err());
    }

    #[test]
    fn validate_choice_multiselect_checks_every_element() {
        let choices = vec![
            Choice {
                label: "docker".to_string(),
                value: AnswerValue::Text("docker".to_string()),
            },
            Choice {
                label: "ci".to_string(),
                value: AnswerValue::Text("ci".to_string()),
            },
        ];
        let q = question(QuestionType::Multiselect, choices);

        assert!(
            validate_choice(
                &q,
                &AnswerValue::List(vec!["docker".to_string(), "ci".to_string()])
            )
            .is_ok()
        );
        assert!(
            validate_choice(
                &q,
                &AnswerValue::List(vec!["docker".to_string(), "unknown".to_string()])
            )
            .is_err()
        );
    }

    #[test]
    fn validate_choice_is_noop_for_types_without_choices() {
        let q = question(QuestionType::String, vec![]);
        assert!(validate_choice(&q, &AnswerValue::Text("anything".to_string())).is_ok());
    }
}
