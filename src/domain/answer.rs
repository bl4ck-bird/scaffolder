//! `AnswerValue`(Text/List/Int/Float/Bool), 불변 `AnswerContext`, `build_context`와
//! `AnswerSource`·`ConditionEvaluator` 포트.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::domain::question::QuestionType;

/// 확정된 answer 값. 타입 그대로 유지한다.
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerValue {
    Text(String),
    List(Vec<String>),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// `scaffolder.*` 렌더 빌트인.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffolderBuiltins {
    pub name: String,
    pub target: PathBuf,
    pub os: String,
    pub arch: String,
    pub username: String,
}

/// 답변 확정 후 불변 컨텍스트. 공개 setter가 없다 — `build_context`로만 생성한다.
#[derive(Debug, Clone)]
pub struct AnswerContext {
    answers: BTreeMap<String, AnswerValue>,
    builtins: ScaffolderBuiltins,
}

impl AnswerContext {
    pub fn answer(&self, name: &str) -> Option<&AnswerValue> {
        self.answers.get(name)
    }

    pub fn builtins(&self) -> &ScaffolderBuiltins {
        &self.builtins
    }
}

pub fn build_context(
    answers: BTreeMap<String, AnswerValue>,
    builtins: ScaffolderBuiltins,
) -> AnswerContext {
    AnswerContext { answers, builtins }
}

/// `--answers` 문자열 coerce. 현재는 `QuestionType::String`만 지원하고,
/// 다른 타입은 아직 미구현임을 명확히 알린다.
pub fn coerce_string(qtype: QuestionType, raw: &str) -> Result<AnswerValue> {
    match qtype {
        QuestionType::String => Ok(AnswerValue::Text(raw.to_string())),
        other => bail!("coerce for question type {other:?} is not implemented yet"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::question::QuestionType;
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
    fn build_context_exposes_answers_and_builtins() {
        let mut answers = std::collections::BTreeMap::new();
        answers.insert("license".to_string(), AnswerValue::Text("MIT".to_string()));

        let ctx = build_context(answers, builtins());

        assert_eq!(
            ctx.answer("license"),
            Some(&AnswerValue::Text("MIT".to_string()))
        );
        assert_eq!(ctx.answer("missing"), None);
        assert_eq!(ctx.builtins().name, "demo");
        assert_eq!(ctx.builtins().os, "macos");
    }

    #[test]
    fn coerce_string_wraps_text() {
        let value = coerce_string(QuestionType::String, "hello").unwrap();
        assert_eq!(value, AnswerValue::Text("hello".to_string()));
    }

    #[test]
    fn coerce_string_rejects_unsupported_types() {
        assert!(coerce_string(QuestionType::Int, "3").is_err());
    }
}
