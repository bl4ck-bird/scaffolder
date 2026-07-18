//! `scaffold.toml` 파싱(TOML 격리) — `ManifestSource`.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::domain::answer::AnswerValue;
use crate::domain::hook::{Hook, Hooks};
use crate::domain::manifest::{Manifest, ManifestSource};
use crate::domain::question::{
    validate_choices, validate_question_name, validate_unique_names, Choice, Question,
    QuestionType,
};
use crate::infra::load::{toml_to_answer_value, toml_to_data_value};

/// TOML로 `scaffold.toml`을 읽는 `ManifestSource`.
pub struct TomlManifestSource;

impl ManifestSource for TomlManifestSource {
    fn load(&self, path: &Path) -> Result<Manifest> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read manifest at {}", path.display()))?;
        parse_manifest(&text)
            .with_context(|| format!("failed to parse manifest at {}", path.display()))
    }
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(default)]
    questions: Vec<RawQuestion>,
    #[serde(default)]
    data: toml::value::Table,
    #[serde(default)]
    hooks: RawHooks,
}

#[derive(Debug, Deserialize, Default)]
struct RawHooks {
    #[serde(default)]
    before: Vec<RawHook>,
    #[serde(default)]
    after: Vec<RawHook>,
}

#[derive(Debug, Deserialize)]
struct RawHook {
    run: String,
    when: Option<String>,
}

impl From<RawHook> for Hook {
    fn from(raw: RawHook) -> Self {
        Hook {
            when: raw.when,
            run: raw.run,
        }
    }
}

impl From<RawHooks> for Hooks {
    fn from(raw: RawHooks) -> Self {
        Hooks {
            before: raw.before.into_iter().map(Hook::from).collect(),
            after: raw.after.into_iter().map(Hook::from).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawQuestion {
    name: String,
    r#type: String,
    prompt: Option<String>,
    #[serde(default)]
    choices: Vec<toml::Value>,
    default: Option<toml::Value>,
    when: Option<String>,
    help: Option<String>,
}

fn parse_manifest(text: &str) -> Result<Manifest> {
    let raw: RawManifest = toml::from_str(text).context("invalid scaffold.toml")?;

    let mut questions = Vec::with_capacity(raw.questions.len());
    for rq in raw.questions {
        let question = rq.into_domain()?;
        validate_choices(&question)?;
        questions.push(question);
    }

    validate_unique_names(questions.iter().map(|q| q.name.as_str()))?;

    let data = toml_to_data_value(&toml::Value::Table(raw.data));
    let hooks = Hooks::from(raw.hooks);

    Ok(Manifest {
        questions,
        data,
        hooks,
    })
}

impl RawQuestion {
    fn into_domain(self) -> Result<Question> {
        validate_question_name(&self.name)?;
        let qtype = parse_question_type(&self.name, &self.r#type)?;

        let choices = self
            .choices
            .iter()
            .map(parse_choice)
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("question {:?} has an invalid choice", self.name))?;

        let default = self
            .default
            .as_ref()
            .map(toml_to_answer_value)
            .transpose()
            .with_context(|| format!("question {:?} has an invalid default", self.name))?;

        Ok(Question {
            name: self.name,
            qtype,
            prompt: self.prompt,
            choices,
            default,
            when: self.when,
            help: self.help,
        })
    }
}

fn parse_question_type(question_name: &str, raw: &str) -> Result<QuestionType> {
    match raw {
        "select" => Ok(QuestionType::Select),
        "multiselect" => Ok(QuestionType::Multiselect),
        "string" => Ok(QuestionType::String),
        "int" => Ok(QuestionType::Int),
        "float" => Ok(QuestionType::Float),
        "boolean" => Ok(QuestionType::Boolean),
        other => bail!("question {question_name:?} has unknown type {other:?}"),
    }
}

fn parse_choice(value: &toml::Value) -> Result<Choice> {
    if let toml::Value::Table(table) = value {
        let label = table
            .get("label")
            .and_then(toml::Value::as_str)
            .context("choice table is missing a string 'label'")?
            .to_string();
        let raw_value = table
            .get("value")
            .context("choice table is missing 'value'")?;
        let value = toml_to_answer_value(raw_value)?;
        Ok(Choice { label, value })
    } else {
        let value = toml_to_answer_value(value)?;
        let label = match &value {
            AnswerValue::Text(s) => s.clone(),
            AnswerValue::Int(i) => i.to_string(),
            AnswerValue::Float(f) => f.to_string(),
            AnswerValue::Bool(b) => b.to_string(),
            AnswerValue::List(_) => bail!("choice value cannot be a list"),
        };
        Ok(Choice { label, value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_question_with_default() {
        let toml = r#"
            [[questions]]
            name = "license"
            type = "string"
            default = "MIT"
        "#;

        let manifest = parse_manifest(toml).expect("manifest should parse");

        assert_eq!(manifest.questions.len(), 1);
        let question = &manifest.questions[0];
        assert_eq!(question.name, "license");
        assert_eq!(question.qtype, QuestionType::String);
        assert_eq!(question.default, Some(AnswerValue::Text("MIT".to_string())));
        assert_eq!(question.prompt, None);
        assert!(question.choices.is_empty());
    }

    #[test]
    fn rejects_unknown_question_type() {
        let toml = r#"
            [[questions]]
            name = "license"
            type = "not-a-real-type"
        "#;

        assert!(parse_manifest(toml).is_err());
    }

    #[test]
    fn rejects_invalid_question_name() {
        let toml = r#"
            [[questions]]
            name = "2fast"
            type = "string"
        "#;

        assert!(parse_manifest(toml).is_err());
    }

    #[test]
    fn rejects_case_insensitive_duplicate_names() {
        let toml = r#"
            [[questions]]
            name = "foo"
            type = "string"

            [[questions]]
            name = "FOO"
            type = "string"
        "#;

        assert!(parse_manifest(toml).is_err());
    }

    #[test]
    fn rejects_select_choice_value_containing_comma() {
        let toml = r#"
            [[questions]]
            name = "tags"
            type = "select"
            choices = ["a,b", "c"]
        "#;

        assert!(parse_manifest(toml).is_err());
    }

    #[test]
    fn rejects_multiselect_choice_value_containing_whitespace() {
        let toml = r#"
            [[questions]]
            name = "stacks"
            type = "multiselect"
            choices = ["has space", "clean"]
        "#;

        assert!(parse_manifest(toml).is_err());
    }

    #[test]
    fn rejects_multiselect_choice_with_non_string_value() {
        let toml = r#"
            [[questions]]
            name = "years"
            type = "multiselect"
            choices = [2018, 2021]
        "#;

        assert!(parse_manifest(toml).is_err());
    }

    #[test]
    fn parses_select_question_with_object_choices() {
        let toml = r#"
            [[questions]]
            name = "telemetry"
            type = "select"
            choices = [{ label = "Enable", value = true }, { label = "Disable", value = false }]
            default = false
        "#;

        let manifest = parse_manifest(toml).expect("manifest should parse");
        let question = &manifest.questions[0];
        assert_eq!(question.qtype, QuestionType::Select);
        assert_eq!(question.choices.len(), 2);
        assert_eq!(question.choices[0].label, "Enable");
        assert_eq!(question.choices[0].value, AnswerValue::Bool(true));
        assert_eq!(question.default, Some(AnswerValue::Bool(false)));
    }

    #[test]
    fn load_reads_manifest_from_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "scaffolder-manifest-test-{}.toml",
            std::process::id()
        ));
        fs::write(
            &path,
            r#"
                [[questions]]
                name = "license"
                type = "string"
                default = "MIT"
            "#,
        )
        .expect("write temp manifest");

        let result = TomlManifestSource.load(&path);
        fs::remove_file(&path).ok();

        let manifest = result.expect("manifest should load");
        assert_eq!(manifest.questions[0].name, "license");
    }

    #[test]
    fn load_reports_missing_file() {
        let path = Path::new("/nonexistent/scaffold.toml");
        assert!(TomlManifestSource.load(path).is_err());
    }

    #[test]
    fn parses_before_and_after_hooks() {
        let toml = r#"
            [[hooks.before]]
            run = "cargo init"

            [[hooks.after]]
            when = "'docker' in stacks"
            run = "git init"
        "#;

        let manifest = parse_manifest(toml).expect("manifest should parse");

        assert_eq!(manifest.hooks.before.len(), 1);
        assert_eq!(manifest.hooks.before[0].run, "cargo init");
        assert_eq!(manifest.hooks.before[0].when, None);

        assert_eq!(manifest.hooks.after.len(), 1);
        assert_eq!(manifest.hooks.after[0].run, "git init");
        assert_eq!(
            manifest.hooks.after[0].when,
            Some("'docker' in stacks".to_string())
        );
    }

    #[test]
    fn rejects_hook_missing_run() {
        let toml = r#"
            [[hooks.before]]
            when = "true"
        "#;

        assert!(parse_manifest(toml).is_err());
    }

    #[test]
    fn defaults_to_empty_hooks_when_not_declared() {
        let toml = r#"
            [[questions]]
            name = "license"
            type = "string"
        "#;

        let manifest = parse_manifest(toml).expect("manifest should parse");

        assert!(manifest.hooks.before.is_empty());
        assert!(manifest.hooks.after.is_empty());
    }
}
