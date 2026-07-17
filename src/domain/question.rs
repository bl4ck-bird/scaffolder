//! `Question`, `QuestionType`(select/multiselect/string/int/float/boolean),
//! `Choice { label, value }`와 `QuestionSource` 포트.

use std::collections::HashSet;

use anyhow::{bail, Result};

use crate::domain::answer::AnswerValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionType {
    Select,
    Multiselect,
    String,
    Int,
    Float,
    Boolean,
}

/// select/multiselect choice. 값은 리터럴 타입을 유지한다(§1.2, 라벨≠값은 `{label, value}`).
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub label: String,
    pub value: AnswerValue,
}

#[derive(Debug, Clone)]
pub struct Question {
    pub name: String,
    pub qtype: QuestionType,
    pub prompt: Option<String>,
    pub choices: Vec<Choice>,
    pub default: Option<AnswerValue>,
    pub when: Option<String>,
    pub help: Option<String>,
}

const RESERVED_NAMES: [&str; 4] = ["name", "scaffolder", "data", "env"];

/// 질문명이 identifier `[A-Za-z_][A-Za-z0-9_]*`이고 예약어가 아닌지 검증한다.
pub fn validate_question_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let starts_ok = chars
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false);
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '_');

    if !starts_ok || !rest_ok {
        bail!("question name {name:?} is not a valid identifier [A-Za-z_][A-Za-z0-9_]*");
    }

    if RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        bail!("question name {name:?} is reserved");
    }

    Ok(())
}

/// 대소문자 무시 유일성 검증(§1.2).
pub fn validate_unique_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name.to_ascii_lowercase()) {
            bail!("question name {name:?} collides case-insensitively with an earlier question");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_identifier_name() {
        assert!(validate_question_name("stacks").is_ok());
        assert!(validate_question_name("_private2").is_ok());
    }

    #[test]
    fn rejects_non_identifier_name() {
        assert!(validate_question_name("2fast").is_err());
        assert!(validate_question_name("has-dash").is_err());
    }

    #[test]
    fn rejects_reserved_names_case_insensitively() {
        assert!(validate_question_name("name").is_err());
        assert!(validate_question_name("Scaffolder").is_err());
        assert!(validate_question_name("DATA").is_err());
        assert!(validate_question_name("env").is_err());
    }

    #[test]
    fn rejects_case_insensitive_duplicate_names() {
        let names = ["foo", "FOO"];
        assert!(validate_unique_names(names).is_err());
    }

    #[test]
    fn accepts_distinct_names() {
        let names = ["foo", "bar"];
        assert!(validate_unique_names(names).is_ok());
    }
}
