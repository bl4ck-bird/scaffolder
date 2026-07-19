//! Hook model and the `HookSource`, `HookRunner`, and `Confirmer` ports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::domain::answer::AnswerValue;

/// Confirmation gate for hooks, overwrites, and external writes; infra implements it interactively.
pub trait Confirmer {
    /// Confirm before running a hook; `description` is the inline command or `run <file>`.
    fn confirm_hook(&self, description: &str) -> bool;
    /// Confirm overwriting an existing destination (`--force` auto-approves in infra).
    fn confirm_overwrite(&self, path: &Path) -> bool;
    /// Confirm a write outside the target (including payload external symlinks).
    fn confirm_external_write(&self, path: &Path) -> bool;
}

/// Hook phase, mapped to the before/after stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    Before,
    After,
}

/// A single inline hook declared in the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub when: Option<String>,
    pub run: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hooks {
    pub before: Vec<Hook>,
    pub after: Vec<Hook>,
}

/// A script found under `hooks/<phase>/`: executables run as-is, templated files are rendered first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookScript {
    Executable { name: String, path: PathBuf },
    Template { name: String, raw: String },
}

/// Port enumerating `hooks/<phase>/` scripts in lexical order; implemented by infra over the filesystem.
pub trait HookSource {
    fn scripts(&self, template_root: &Path, phase: HookPhase) -> anyhow::Result<Vec<HookScript>>;
}

/// Port for running hooks; implemented by infra via process execution.
pub trait HookRunner {
    /// Run the manifest's inline `run` command via `/bin/sh -c`.
    fn run_inline(
        &self,
        command: &str,
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<()>;

    /// Run a `hooks/<phase>/` executable script in place.
    fn run_script_file(
        &self,
        path: &Path,
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<()>;

    /// Run a rendered template hook script from a secure temp file.
    fn run_rendered(
        &self,
        name: &str,
        content: &[u8],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<()>;
}

/// Converts the answer map into hook env vars, keyed `SCAFFOLDER_<UPPER(name)>`.
///
/// `List` (multiselect) is space-joined — deliberately not `answer::canonical_string`
/// (comma-joined for choice matching), a different format for a different purpose.
pub fn hook_env(answers: &BTreeMap<String, AnswerValue>) -> BTreeMap<String, String> {
    answers
        .iter()
        .map(|(name, value)| {
            let key = format!("SCAFFOLDER_{}", name.to_ascii_uppercase());
            let val = match value {
                AnswerValue::Text(s) => s.clone(),
                AnswerValue::Int(i) => i.to_string(),
                AnswerValue::Float(f) => f.to_string(),
                AnswerValue::Bool(b) => b.to_string(),
                AnswerValue::List(items) => items.join(" "),
            };
            (key, val)
        })
        .collect()
}

#[cfg(test)]
mod hook_env_tests {
    use super::*;

    #[test]
    fn hook_env_formats_each_answer_value_and_upper_snakes_keys() {
        let mut answers: BTreeMap<String, AnswerValue> = BTreeMap::new();
        answers.insert("feat".to_string(), AnswerValue::Bool(true));
        answers.insert("n".to_string(), AnswerValue::Int(3));
        answers.insert("r".to_string(), AnswerValue::Float(1.5));
        answers.insert("s".to_string(), AnswerValue::Text("hi".to_string()));
        answers.insert(
            "stacks".to_string(),
            AnswerValue::List(vec!["docker".to_string(), "ci".to_string()]),
        );

        let env = hook_env(&answers);

        let mut expected: BTreeMap<String, String> = BTreeMap::new();
        expected.insert("SCAFFOLDER_FEAT".to_string(), "true".to_string());
        expected.insert("SCAFFOLDER_N".to_string(), "3".to_string());
        expected.insert("SCAFFOLDER_R".to_string(), "1.5".to_string());
        expected.insert("SCAFFOLDER_S".to_string(), "hi".to_string());
        expected.insert("SCAFFOLDER_STACKS".to_string(), "docker ci".to_string());

        assert_eq!(env, expected);
    }
}
