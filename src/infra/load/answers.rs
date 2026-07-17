//! `--answers-file` TOML 로딩 — `name = value`를 타입 그대로 `AnswerValue`로 매핑한다.
//! choices/타입 검증은 pipeline이 questions와 대조해 수행한다(여기선 순수 변환만).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::domain::answer::AnswerValue;
use crate::infra::load::toml_to_answer_value;

pub fn load_answers_file(path: &Path) -> Result<BTreeMap<String, AnswerValue>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read answers file at {}", path.display()))?;
    parse_answers(&text)
        .with_context(|| format!("failed to parse answers file at {}", path.display()))
}

fn parse_answers(text: &str) -> Result<BTreeMap<String, AnswerValue>> {
    let raw: toml::Table = toml::from_str(text).context("invalid answers TOML")?;

    let mut answers = BTreeMap::new();
    for (name, value) in raw {
        let value = toml_to_answer_value(&value)
            .with_context(|| format!("answer {name:?} has an unsupported value"))?;
        answers.insert(name, value);
    }
    Ok(answers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(contents: &str) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("create temp file");
        fs::write(file.path(), contents).expect("write temp file");
        file
    }

    #[test]
    fn loads_string_int_float_bool_and_array_values() {
        let file = write_temp(
            r#"
                license = "MIT"
                port = 3000
                ratio = 2.5
                private = true
                stacks = ["docker", "ci"]
            "#,
        );

        let answers = load_answers_file(file.path()).expect("answers should load");

        assert_eq!(
            answers.get("license"),
            Some(&AnswerValue::Text("MIT".to_string()))
        );
        assert_eq!(answers.get("port"), Some(&AnswerValue::Int(3000)));
        assert_eq!(answers.get("ratio"), Some(&AnswerValue::Float(2.5)));
        assert_eq!(answers.get("private"), Some(&AnswerValue::Bool(true)));
        assert_eq!(
            answers.get("stacks"),
            Some(&AnswerValue::List(vec![
                "docker".to_string(),
                "ci".to_string()
            ]))
        );
    }

    #[test]
    fn rejects_array_with_non_string_elements() {
        let file = write_temp("edition = [2018, 2021]\n");
        assert!(load_answers_file(file.path()).is_err());
    }

    #[test]
    fn rejects_nested_table_values() {
        let file = write_temp(
            r#"
                [nested]
                a = 1
            "#,
        );
        assert!(load_answers_file(file.path()).is_err());
    }

    #[test]
    fn rejects_invalid_toml_syntax() {
        let file = write_temp("this is not valid toml ===");
        assert!(load_answers_file(file.path()).is_err());
    }

    #[test]
    fn reports_missing_file() {
        let path = Path::new("/nonexistent/answers.toml");
        assert!(load_answers_file(path).is_err());
    }
}
