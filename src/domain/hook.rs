//! `Hook`, `HookPhase`(before/after)와 `HookSource`·`HookRunner`·`Confirmer` 포트
//! (훅·overwrite·외부쓰기 confirm 겸용).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::domain::answer::AnswerValue;

/// 훅 실행·overwrite·외부쓰기 confirm 게이트. infra가 대화형으로 구현한다.
pub trait Confirmer {
    /// 훅 실행 전 confirm. `description`은 인라인 명령 또는 `run <file>` 표시.
    fn confirm_hook(&self, description: &str) -> bool;
    /// 기존 dest overwrite confirm(`--force`는 infra가 자동 승인으로 처리).
    fn confirm_overwrite(&self, path: &Path) -> bool;
    /// target 밖 쓰기 confirm(payload 외부 심링크 포함).
    fn confirm_external_write(&self, path: &Path) -> bool;
}

/// 훅 실행 시점: manifest `when` 조건 없이 before/after 단계에 매핑된다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    Before,
    After,
}

/// manifest에 선언된 단일 훅(인라인 `run` 명령, 선택적 `when` 조건).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub when: Option<String>,
    pub run: String,
}

/// before/after 단계별 훅 목록.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hooks {
    pub before: Vec<Hook>,
    pub after: Vec<Hook>,
}

/// `hooks/<phase>/` 폴더에서 발견된 스크립트. 실행 가능 파일은 그대로 실행하고,
/// 템플릿 확장자가 붙은 파일은 렌더 후 실행한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookScript {
    Executable { name: String, path: PathBuf },
    Template { name: String, raw: String },
}

/// `hooks/<phase>/` 폴더 스크립트를 lexical 순서로 열거하는 포트. infra가 파일시스템으로
/// 구현한다.
pub trait HookSource {
    fn scripts(&self, template_root: &Path, phase: HookPhase) -> anyhow::Result<Vec<HookScript>>;
}

/// 훅 실행 포트. infra가 프로세스 실행으로 구현한다.
pub trait HookRunner {
    /// manifest의 인라인 `run` 명령을 `/bin/sh -c`로 실행한다.
    fn run_inline(
        &self,
        command: &str,
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<()>;

    /// `hooks/<phase>/` 폴더의 실행 가능 스크립트를 원위치에서 실행한다.
    fn run_script_file(
        &self,
        path: &Path,
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<()>;

    /// 렌더된 템플릿 훅 스크립트를 secure temp 파일로 써서 실행한다.
    fn run_rendered(
        &self,
        name: &str,
        content: &[u8],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<()>;
}

/// answer 맵을 훅 실행용 env로 변환한다. 키는 `SCAFFOLDER_<UPPER(name)>`.
///
/// `List`(multiselect)는 공백으로 join한다 — `answer::canonical_string`(choice 매칭용, 콤마
/// join)과는 목적이 다른 별도 포맷이라 재사용하지 않는다.
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
