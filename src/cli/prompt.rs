//! `inquire` 타입별 위젯 — `AnswerSource`.

use anyhow::{Context, Result, anyhow, bail};
use inquire::{Confirm, MultiSelect, Select, Text};

use crate::domain::answer::{AnswerSource, AnswerValue, coerce, validate_choice};
use crate::domain::question::{Question, QuestionType};

/// tty 대화형 `AnswerSource`. 취소·입력 에러는 anyhow context와 함께 전파한다.
pub struct InquireAnswerSource;

impl AnswerSource for InquireAnswerSource {
    fn ask(&self, question: &Question) -> Result<AnswerValue> {
        match question.qtype {
            QuestionType::String => ask_string(question),
            QuestionType::Int | QuestionType::Float => ask_numeric(question),
            QuestionType::Boolean => ask_boolean(question),
            QuestionType::Select => ask_select(question),
            QuestionType::Multiselect => ask_multiselect(question),
        }
    }
}

fn ask_string(question: &Question) -> Result<AnswerValue> {
    let message = prompt_message(question);
    let default = match &question.default {
        Some(AnswerValue::Text(s)) => Some(s.clone()),
        _ => None,
    };

    let mut text = Text::new(&message);
    if let Some(default) = &default {
        text = text.with_default(default);
    }
    if let Some(help) = &question.help {
        text = text.with_help_message(help);
    }

    let raw = text
        .prompt()
        .with_context(|| format!("failed to prompt question {:?}", question.name))?;
    Ok(AnswerValue::Text(raw))
}

/// int/float는 텍스트로 입력받아 `coerce`로 재파싱한다 — `--answers` 경로와 파싱 로직을
/// 하나로 유지하기 위함.
fn ask_numeric(question: &Question) -> Result<AnswerValue> {
    let message = prompt_message(question);
    let mut text = Text::new(&message);
    if let Some(help) = &question.help {
        text = text.with_help_message(help);
    }

    let raw = text
        .prompt()
        .with_context(|| format!("failed to prompt question {:?}", question.name))?;
    coerce(question, &raw)
}

fn ask_boolean(question: &Question) -> Result<AnswerValue> {
    let message = prompt_message(question);
    let mut confirm = Confirm::new(&message);
    if let Some(AnswerValue::Bool(default)) = &question.default {
        confirm = confirm.with_default(*default);
    }
    if let Some(help) = &question.help {
        confirm = confirm.with_help_message(help);
    }

    let value = confirm
        .prompt()
        .with_context(|| format!("failed to prompt question {:?}", question.name))?;
    Ok(AnswerValue::Bool(value))
}

fn ask_select(question: &Question) -> Result<AnswerValue> {
    let message = prompt_message(question);
    let mut select = Select::new(&message, choice_labels(question));
    if let Some(help) = &question.help {
        select = select.with_help_message(help);
    }

    let picked = select
        .raw_prompt()
        .with_context(|| format!("failed to prompt question {:?}", question.name))?;
    let value = resolve_choice_value(question, picked.index)?;
    validate_choice(question, &value)?;
    Ok(value)
}

fn ask_multiselect(question: &Question) -> Result<AnswerValue> {
    let message = prompt_message(question);
    let mut multiselect = MultiSelect::new(&message, choice_labels(question));
    if let Some(help) = &question.help {
        multiselect = multiselect.with_help_message(help);
    }

    let picked = multiselect
        .raw_prompt()
        .with_context(|| format!("failed to prompt question {:?}", question.name))?;
    let items = picked
        .iter()
        .map(|option| resolve_choice_string(question, option.index))
        .collect::<Result<Vec<_>>>()?;
    let value = AnswerValue::List(items);
    validate_choice(question, &value)?;
    Ok(value)
}

fn prompt_message(question: &Question) -> String {
    question
        .prompt
        .clone()
        .unwrap_or_else(|| question.name.clone())
}

fn choice_labels(question: &Question) -> Vec<String> {
    question.choices.iter().map(|c| c.label.clone()).collect()
}

/// 선택된 인덱스(inquire label 목록 기준) → 원래 choice의 리터럴 값.
fn resolve_choice_value(question: &Question, index: usize) -> Result<AnswerValue> {
    question
        .choices
        .get(index)
        .map(|choice| choice.value.clone())
        .ok_or_else(|| {
            anyhow!(
                "selected index {index} is out of range for question {:?}",
                question.name
            )
        })
}

fn resolve_choice_string(question: &Question, index: usize) -> Result<String> {
    let value = resolve_choice_value(question, index)?;
    choice_value_to_string(&value)
}

/// multiselect의 `List` 항목은 문자열이다 — choice 값을 정규 문자열로 낮춘다.
fn choice_value_to_string(value: &AnswerValue) -> Result<String> {
    match value {
        AnswerValue::Text(s) => Ok(s.clone()),
        AnswerValue::Int(i) => Ok(i.to_string()),
        AnswerValue::Float(f) => Ok(f.to_string()),
        AnswerValue::Bool(b) => Ok(b.to_string()),
        AnswerValue::List(_) => bail!("multiselect choice value cannot itself be a list"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::question::Choice;

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
    fn prompt_message_falls_back_to_name() {
        let q = question(QuestionType::String, vec![]);
        assert_eq!(prompt_message(&q), "q");
    }

    #[test]
    fn prompt_message_prefers_explicit_prompt() {
        let mut q = question(QuestionType::String, vec![]);
        q.prompt = Some("Pick a license".to_string());
        assert_eq!(prompt_message(&q), "Pick a license");
    }

    #[test]
    fn choice_labels_preserves_order() {
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
        assert_eq!(choice_labels(&q), vec!["MIT", "Apache-2.0"]);
    }

    #[test]
    fn resolve_choice_value_keeps_literal_type_by_index() {
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

        assert_eq!(resolve_choice_value(&q, 1).unwrap(), AnswerValue::Int(2021));
        assert!(resolve_choice_value(&q, 5).is_err());
    }

    #[test]
    fn resolve_choice_string_converts_non_text_values() {
        let choices = vec![Choice {
            label: "Enable".to_string(),
            value: AnswerValue::Bool(true),
        }];
        let q = question(QuestionType::Multiselect, choices);

        assert_eq!(resolve_choice_string(&q, 0).unwrap(), "true");
    }

    #[test]
    fn choice_value_to_string_rejects_list_values() {
        assert!(choice_value_to_string(&AnswerValue::List(vec!["a".to_string()])).is_err());
    }
}
