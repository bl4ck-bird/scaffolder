//! `Question`, `QuestionType`(select/multiselect/string/int/float/boolean),
//! `Choice { label, value }`.

use std::collections::HashSet;

use anyhow::{Result, bail};

use crate::domain::answer::{AnswerValue, canonical_string};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionType {
    Select,
    Multiselect,
    String,
    Int,
    Float,
    Boolean,
}

/// select/multiselect choice. 값은 리터럴 타입을 유지한다(라벨≠값은 `{label, value}`).
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

/// 대소문자 무시 유일성 검증.
pub fn validate_unique_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name.to_ascii_lowercase()) {
            bail!("question name {name:?} collides case-insensitively with an earlier question");
        }
    }
    Ok(())
}

/// select/multiselect choice 값이 콤마·공백을 포함하지 않는지, 그리고 multiselect choice가
/// 문자열 값만 쓰는지(현재 `AnswerValue::List`가 원소 타입을 보존하지 못하므로) 검증한다.
/// choices가 없는 타입은 항상 Ok.
pub fn validate_choices(question: &Question) -> Result<()> {
    match question.qtype {
        QuestionType::Select | QuestionType::Multiselect => {
            for choice in &question.choices {
                let s = canonical_string(&choice.value);
                if s.contains(',') || s.chars().any(char::is_whitespace) {
                    bail!(
                        "question {:?} has a choice value {s:?} containing a comma or whitespace, which is not allowed",
                        question.name
                    );
                }
                if question.qtype == QuestionType::Multiselect
                    && !matches!(choice.value, AnswerValue::Text(_))
                {
                    bail!(
                        "question {:?} is multiselect but has a non-string choice value {s:?}: multiselect choices must be strings in this version",
                        question.name
                    );
                }
            }
            Ok(())
        }
        QuestionType::String | QuestionType::Int | QuestionType::Float | QuestionType::Boolean => {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::answer::AnswerValue;

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

    #[test]
    fn validate_choices_rejects_comma_in_choice_value() {
        let choices = vec![Choice {
            label: "a,b".to_string(),
            value: AnswerValue::Text("a,b".to_string()),
        }];
        let q = question(QuestionType::Select, choices);
        assert!(validate_choices(&q).is_err());
    }

    #[test]
    fn validate_choices_rejects_whitespace_in_choice_value() {
        let choices = vec![Choice {
            label: "a b".to_string(),
            value: AnswerValue::Text("a b".to_string()),
        }];
        let q = question(QuestionType::Multiselect, choices);
        assert!(validate_choices(&q).is_err());
    }

    #[test]
    fn validate_choices_accepts_clean_select_choices() {
        let choices = vec![
            Choice {
                label: "MIT".to_string(),
                value: AnswerValue::Text("MIT".to_string()),
            },
            Choice {
                label: "2021".to_string(),
                value: AnswerValue::Int(2021),
            },
        ];
        let q = question(QuestionType::Select, choices);
        assert!(validate_choices(&q).is_ok());
    }

    #[test]
    fn validate_choices_rejects_non_string_multiselect_choice() {
        let choices = vec![Choice {
            label: "1".to_string(),
            value: AnswerValue::Int(1),
        }];
        let q = question(QuestionType::Multiselect, choices);
        assert!(validate_choices(&q).is_err());
    }

    #[test]
    fn validate_choices_accepts_string_multiselect_choices() {
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
        assert!(validate_choices(&q).is_ok());
    }

    #[test]
    fn validate_choices_is_noop_for_types_without_choices() {
        let q = question(QuestionType::String, vec![]);
        assert!(validate_choices(&q).is_ok());
    }

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
