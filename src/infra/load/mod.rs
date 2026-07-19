//! Template and store loading adapters.

pub mod answers;
pub mod data;
pub mod ignore;
pub mod manifest;
pub mod partials;
pub mod source_root;
pub mod store;
pub mod trust;

use anyhow::{Result, bail};

use crate::domain::answer::AnswerValue;
use crate::domain::data::DataValue;

/// Converts a TOML value into an `AnswerValue`. Shared by the manifest `default`/`choices`
/// and the answers-file `name = value`.
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

/// Converts a TOML value into a `DataValue`. Used by both `[data]` (manifest) and `data/*.toml`
/// (`DataSource`). Being static data, it accepts every type (unlike answers); datetimes are
/// demoted to strings.
pub(crate) fn toml_to_data_value(value: &toml::Value) -> DataValue {
    match value {
        toml::Value::String(s) => DataValue::Str(s.clone()),
        toml::Value::Integer(i) => DataValue::Int(*i),
        toml::Value::Float(f) => DataValue::Float(*f),
        toml::Value::Boolean(b) => DataValue::Bool(*b),
        toml::Value::Datetime(dt) => DataValue::Str(dt.to_string()),
        toml::Value::Array(items) => {
            DataValue::Array(items.iter().map(toml_to_data_value).collect())
        }
        toml::Value::Table(map) => DataValue::Table(
            map.iter()
                .map(|(k, v)| (k.clone(), toml_to_data_value(v)))
                .collect(),
        ),
    }
}
