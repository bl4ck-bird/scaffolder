//! 템플릿·스토어 로딩 어댑터.

pub mod manifest;
pub mod store;
pub mod source_root;
pub mod data;
pub mod ignore;
pub mod answers;
pub mod partials;

use anyhow::{bail, Result};

use crate::domain::answer::AnswerValue;

/// TOML 값을 `AnswerValue`로 변환한다. manifest의 `default`/`choices`와 answers-file의
/// `name = value` 양쪽에서 쓰는 공유 변환 로직.
pub(crate) fn toml_to_answer_value(value: &toml::Value) -> Result<AnswerValue> {
    match value {
        toml::Value::String(s) => Ok(AnswerValue::Text(s.clone())),
        toml::Value::Integer(i) => Ok(AnswerValue::Int(*i)),
        toml::Value::Float(f) => Ok(AnswerValue::Float(*f)),
        toml::Value::Boolean(b) => Ok(AnswerValue::Bool(*b)),
        toml::Value::Array(items) => {
            let items = items
                .iter()
                .map(|v| match v {
                    toml::Value::String(s) => Ok(s.clone()),
                    other => bail!("list value {other:?} must be a string"),
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(AnswerValue::List(items))
        }
        other => bail!("unsupported value {other:?}"),
    }
}
