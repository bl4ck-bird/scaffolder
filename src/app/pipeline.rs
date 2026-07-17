//! apply 라이프사이클 조립: 매니페스트 파싱 → answer 확정 → plan(부작용
//! 없음) → dry-run이면 종료 → write(overwrite/외부쓰기 confirm 반영). `.scaffoldroot`·ignore·
//! partials·data·hook은 이후 슬라이스에서 확장한다. 도메인 포트만 사용한다.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::domain::answer::{build_context, coerce, validate_choice, AnswerSource, AnswerValue, ScaffolderBuiltins};
use crate::domain::hook::Confirmer;
use crate::domain::manifest::ManifestSource;
use crate::domain::name::parse_file_name;
use crate::domain::place::{safe_rel_path, FileMode, PayloadStore, RelPath};
use crate::domain::question::Question;
use crate::domain::render::Renderer;

pub struct ApplyRequest {
    pub template_root: PathBuf,
    pub target_root: PathBuf,
    pub answers: BTreeMap<String, String>,
    pub answers_file: BTreeMap<String, AnswerValue>,
    pub defaults_only: bool,
    pub interactive: bool,
    pub dry_run: bool,
}

pub struct PlannedWrite {
    pub rel: RelPath,
    pub rendered: bool,
    pub content: Vec<u8>,
}

pub struct ApplyReport {
    pub planned: Vec<PlannedWrite>,
}

pub fn apply(
    req: &ApplyRequest,
    builtins: ScaffolderBuiltins,
    manifest_src: &dyn ManifestSource,
    renderer: &dyn Renderer,
    payload: &dyn PayloadStore,
    confirmer: &dyn Confirmer,
    answer_source: &dyn AnswerSource,
) -> Result<ApplyReport> {
    let manifest_path = req.template_root.join("scaffold.toml");
    let manifest = manifest_src.load(&manifest_path)?;

    let answers = resolve_answers(
        &manifest.questions,
        &req.answers,
        &req.answers_file,
        req.defaults_only,
        req.interactive,
        answer_source,
    )?;
    let ctx = build_context(answers, builtins);

    let files_root = req.template_root.join("files");
    let entries = payload.list_entries(&files_root)?;

    let mut planned: Vec<PlannedWrite> = Vec::new();
    for entry in entries.iter().filter(|e| !e.is_dir) {
        let basename = entry
            .rel
            .as_path()
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("entry {} has no valid basename", entry.rel))?;
        let parsed = parse_file_name(basename)?;

        let out_rel_str = match entry.rel.as_path().parent() {
            Some(parent) if parent.as_os_str().is_empty() => parsed.output_base.clone(),
            Some(parent) => parent.join(&parsed.output_base).to_string_lossy().into_owned(),
            None => parsed.output_base.clone(),
        };
        let out_rel = safe_rel_path(&out_rel_str)?;

        if planned.iter().any(|p| p.rel == out_rel) {
            bail!("source conflict: multiple entries map to output path {out_rel}");
        }

        let raw = payload.read_content(&files_root, entry)?;
        let content = if parsed.render {
            let text = String::from_utf8(raw)
                .map_err(|_| anyhow::anyhow!("entry {} is not valid UTF-8 but is marked for rendering", entry.rel))?;
            renderer.render_str(&text, &ctx)?.into_bytes()
        } else {
            raw
        };

        planned.push(PlannedWrite {
            rel: out_rel,
            rendered: parsed.render,
            content,
        });
    }

    if req.dry_run {
        return Ok(ApplyReport { planned });
    }

    for planned_write in &planned {
        let status = payload.dest_status(&req.target_root, &planned_write.rel)?;

        // target 밖으로 이탈하는 쓰기는 confirm하고, 미승인이면 그 엔트리만 건너뛰고
        // 계속한다(overwrite와 달리 hard-fail이 아니다).
        if !status.inside_target && !confirmer.confirm_external_write(&status.final_path) {
            eprintln!(
                "warning: skipping {} — write escapes target and was not confirmed",
                status.final_path.display()
            );
            continue;
        }

        if status.exists && !confirmer.confirm_overwrite(&status.final_path) {
            bail!(
                "destination {} exists; pass --force to overwrite",
                status.final_path.display()
            );
        }

        payload.write_file(
            &req.target_root,
            &planned_write.rel,
            &planned_write.content,
            FileMode::base(),
        )?;
    }

    Ok(ApplyReport { planned })
}

/// answer 확정 precedence(순서대로 첫 매치를 채택):
/// 1. `--answers`(raw 문자열, `coerce`로 타입 변환)
/// 2. `--answers-file`(이미 타입이 정해진 값)
/// 3. `--defaults`면 question default(없으면 에러 — 프롬프트로 폴백하지 않는다)
/// 4. 대화형이면 `answer_source.ask`
/// 5. 그 외(비대화형·`--defaults` 아님)는 default(없으면 에러)
///
/// 확정된 값마다 `validate_choice`로 검증한다. `--answers`/`--answers-file`의 미매칭 키는
/// 경고만 하고 계속 진행한다.
fn resolve_answers(
    questions: &[Question],
    raw_answers: &BTreeMap<String, String>,
    answers_file: &BTreeMap<String, AnswerValue>,
    defaults_only: bool,
    interactive: bool,
    answer_source: &dyn AnswerSource,
) -> Result<BTreeMap<String, AnswerValue>> {
    let mut resolved = BTreeMap::new();

    for question in questions {
        let value = if let Some(raw) = raw_answers.get(&question.name) {
            coerce(question, raw)?
        } else if let Some(value) = answers_file.get(&question.name) {
            value.clone()
        } else if defaults_only {
            question
                .default
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing answer for '{}'", question.name))?
        } else if interactive {
            answer_source.ask(question)?
        } else {
            question
                .default
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing answer for '{}'", question.name))?
        };

        validate_choice(question, &value)?;
        resolved.insert(question.name.clone(), value);
    }

    let known: std::collections::HashSet<&str> =
        questions.iter().map(|q| q.name.as_str()).collect();
    let mut warned: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for key in raw_answers.keys() {
        if !known.contains(key.as_str()) && warned.insert(key.as_str()) {
            eprintln!("warning: '--answers {key}=...' does not match any question; ignoring");
        }
    }
    for key in answers_file.keys() {
        if !known.contains(key.as_str()) && warned.insert(key.as_str()) {
            eprintln!("warning: '--answers-file' key '{key}' does not match any question; ignoring");
        }
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::answer::AnswerContext;
    use crate::domain::hook::Confirmer;
    use crate::domain::manifest::Manifest;
    use crate::domain::place::{DestStatus, PayloadEntry};
    use std::cell::RefCell;
    use crate::domain::question::QuestionType;
    use std::collections::HashMap;
    use std::path::Path;

    fn builtins() -> ScaffolderBuiltins {
        ScaffolderBuiltins {
            name: "demo".to_string(),
            target: PathBuf::from("/tmp/demo"),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            username: "tester".to_string(),
        }
    }

    struct FakeManifestSource(Manifest);
    impl ManifestSource for FakeManifestSource {
        fn load(&self, _path: &Path) -> Result<Manifest> {
            Ok(self.0.clone())
        }
    }

    struct FakeRenderer;
    impl Renderer for FakeRenderer {
        fn render_str(&self, template: &str, _context: &AnswerContext) -> Result<String> {
            Ok(format!("rendered:{template}"))
        }
    }

    struct FakeConfirmer {
        overwrite: bool,
        external: bool,
    }
    impl Confirmer for FakeConfirmer {
        fn confirm_hook(&self, _description: &str) -> bool {
            true
        }
        fn confirm_overwrite(&self, _path: &Path) -> bool {
            self.overwrite
        }
        fn confirm_external_write(&self, _path: &Path) -> bool {
            self.external
        }
    }

    /// 고정값을 반환하거나(`returning`), 호출되면 실패해야 함을 표시하는(`unreachable`)
    /// 테스트용 `AnswerSource`. precedence가 프롬프트를 건너뛰어야 하는 경로에서
    /// `ask`가 호출되지 않았음을 검증하는 데 쓴다.
    struct FakeAnswerSource {
        value: Option<AnswerValue>,
        called: RefCell<bool>,
    }
    impl FakeAnswerSource {
        fn returning(value: AnswerValue) -> Self {
            Self { value: Some(value), called: RefCell::new(false) }
        }

        fn unreachable() -> Self {
            Self { value: None, called: RefCell::new(false) }
        }
    }
    impl AnswerSource for FakeAnswerSource {
        fn ask(&self, question: &Question) -> Result<AnswerValue> {
            *self.called.borrow_mut() = true;
            self.value
                .clone()
                .ok_or_else(|| anyhow::anyhow!("ask should not have been called for '{}'", question.name))
        }
    }

    /// 소스 충돌 유발용: 서로 다른 basename이 같은 출력 rel로 매핑되도록(둘 다 verbatim,
    /// `.jinja` strip 후 동일) 두 엔트리를 반환한다.
    struct FakePayloadStore {
        entries: Vec<PayloadEntry>,
        contents: HashMap<String, Vec<u8>>,
        dest_statuses: RefCell<HashMap<String, DestStatus>>,
        written: RefCell<Vec<(RelPath, Vec<u8>)>>,
    }

    impl PayloadStore for FakePayloadStore {
        fn list_entries(&self, _source_root: &Path) -> Result<Vec<PayloadEntry>> {
            Ok(self.entries.clone())
        }

        fn read_content(&self, _source_root: &Path, entry: &PayloadEntry) -> Result<Vec<u8>> {
            Ok(self.contents.get(&entry.rel.to_string()).cloned().unwrap_or_default())
        }

        fn write_file(
            &self,
            _target_root: &Path,
            rel: &RelPath,
            content: &[u8],
            _mode: crate::domain::place::FileMode,
        ) -> Result<()> {
            self.written.borrow_mut().push((rel.clone(), content.to_vec()));
            Ok(())
        }

        fn dest_status(&self, _target_root: &Path, rel: &RelPath) -> Result<DestStatus> {
            Ok(self
                .dest_statuses
                .borrow()
                .get(&rel.to_string())
                .cloned()
                .unwrap_or(DestStatus {
                    final_path: PathBuf::from(rel.to_string()),
                    inside_target: true,
                    exists: false,
                    is_symlink: false,
                }))
        }
    }

    fn string_question(name: &str, default: Option<&str>) -> Question {
        Question {
            name: name.to_string(),
            qtype: QuestionType::String,
            prompt: None,
            choices: Vec::new(),
            default: default.map(|d| AnswerValue::Text(d.to_string())),
            when: None,
            help: None,
        }
    }

    #[test]
    fn dry_run_produces_plan_without_writing() {
        let manifest = Manifest {
            questions: vec![string_question("project", None)],
        };
        let store = FakePayloadStore {
            entries: vec![PayloadEntry {
                rel: safe_rel_path("README.md.jinja").unwrap(),
                is_dir: false,
            }],
            contents: HashMap::from([("README.md.jinja".to_string(), b"# {{ project }}".to_vec())]),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
        };

        let mut answers = BTreeMap::new();
        answers.insert("project".to_string(), "demo".to_string());
        let req = ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers,
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: true,
        };

        let report = apply(
            &req,
            builtins(),
            &FakeManifestSource(manifest),
            &FakeRenderer,
            &store,
            &FakeConfirmer { overwrite: true, external: true },
            &FakeAnswerSource::unreachable(),
        )
        .expect("apply should succeed");

        assert_eq!(report.planned.len(), 1);
        assert_eq!(report.planned[0].rel.to_string(), "README.md");
        assert!(report.planned[0].rendered);
        assert!(store.written.borrow().is_empty(), "dry-run must not write");
    }

    #[test]
    fn source_conflict_on_same_output_path_is_error() {
        let manifest = Manifest { questions: vec![] };
        let store = FakePayloadStore {
            entries: vec![
                PayloadEntry { rel: safe_rel_path("README.md").unwrap(), is_dir: false },
                PayloadEntry { rel: safe_rel_path("README.md.jinja").unwrap(), is_dir: false },
            ],
            contents: HashMap::from([
                ("README.md".to_string(), b"verbatim".to_vec()),
                ("README.md.jinja".to_string(), b"rendered".to_vec()),
            ]),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
        };

        let req = ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers: BTreeMap::new(),
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: true,
        };

        let result = apply(
            &req,
            builtins(),
            &FakeManifestSource(manifest),
            &FakeRenderer,
            &store,
            &FakeConfirmer { overwrite: true, external: true },
            &FakeAnswerSource::unreachable(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn missing_required_answer_without_default_is_error() {
        let manifest = Manifest {
            questions: vec![string_question("project", None)],
        };
        let store = FakePayloadStore {
            entries: vec![],
            contents: HashMap::new(),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
        };

        let req = ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers: BTreeMap::new(),
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: true,
        };

        let result = apply(
            &req,
            builtins(),
            &FakeManifestSource(manifest),
            &FakeRenderer,
            &store,
            &FakeConfirmer { overwrite: true, external: true },
            &FakeAnswerSource::unreachable(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn external_write_without_confirmation_is_skipped_not_written() {
        // rel 문자열은 `safe_rel_path`가 literal '..'을 이미 거부하므로 항상 정상 형태다;
        // containment 이탈은 상위 심링크 등 최종 경로 해석 단계에서만 드러난다.
        // 미승인 외부쓰기는 그 엔트리만 스킵하고 apply는 성공한다.
        let manifest = Manifest { questions: vec![] };
        let mut dest_statuses = HashMap::new();
        dest_statuses.insert(
            "linked/outside.txt".to_string(),
            DestStatus {
                final_path: PathBuf::from("/outside/outside.txt"),
                inside_target: false,
                exists: false,
                is_symlink: false,
            },
        );
        let store = FakePayloadStore {
            entries: vec![PayloadEntry { rel: safe_rel_path("linked/outside.txt").unwrap(), is_dir: false }],
            contents: HashMap::from([("linked/outside.txt".to_string(), b"content".to_vec())]),
            dest_statuses: RefCell::new(dest_statuses),
            written: RefCell::new(Vec::new()),
        };

        let req = ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers: BTreeMap::new(),
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: false,
        };

        let result = apply(
            &req,
            builtins(),
            &FakeManifestSource(manifest),
            &FakeRenderer,
            &store,
            &FakeConfirmer { overwrite: true, external: false },
            &FakeAnswerSource::unreachable(),
        );

        assert!(result.is_ok());
        assert!(store.written.borrow().is_empty());
    }

    #[test]
    fn existing_destination_without_overwrite_confirmation_is_error() {
        let manifest = Manifest { questions: vec![] };
        let mut dest_statuses = HashMap::new();
        dest_statuses.insert(
            "file.txt".to_string(),
            DestStatus {
                final_path: PathBuf::from("/target/file.txt"),
                inside_target: true,
                exists: true,
                is_symlink: false,
            },
        );
        let store = FakePayloadStore {
            entries: vec![PayloadEntry { rel: safe_rel_path("file.txt").unwrap(), is_dir: false }],
            contents: HashMap::from([("file.txt".to_string(), b"content".to_vec())]),
            dest_statuses: RefCell::new(dest_statuses),
            written: RefCell::new(Vec::new()),
        };

        let req = ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers: BTreeMap::new(),
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: false,
        };

        let result = apply(
            &req,
            builtins(),
            &FakeManifestSource(manifest),
            &FakeRenderer,
            &store,
            &FakeConfirmer { overwrite: false, external: true },
            &FakeAnswerSource::unreachable(),
        );

        assert!(result.is_err());
        assert!(store.written.borrow().is_empty());
    }

    #[test]
    fn cli_answers_override_answers_file() {
        let question = string_question("project", Some("default-val"));
        let mut raw = BTreeMap::new();
        raw.insert("project".to_string(), "from-cli".to_string());
        let mut file = BTreeMap::new();
        file.insert("project".to_string(), AnswerValue::Text("from-file".to_string()));

        let resolved = resolve_answers(&[question], &raw, &file, false, false, &FakeAnswerSource::unreachable())
            .expect("resolve should succeed");

        assert_eq!(resolved.get("project"), Some(&AnswerValue::Text("from-cli".to_string())));
    }

    #[test]
    fn answers_file_used_when_cli_answer_missing() {
        let question = string_question("project", Some("default-val"));
        let raw = BTreeMap::new();
        let mut file = BTreeMap::new();
        file.insert("project".to_string(), AnswerValue::Text("from-file".to_string()));

        let resolved = resolve_answers(&[question], &raw, &file, false, false, &FakeAnswerSource::unreachable())
            .expect("resolve should succeed");

        assert_eq!(resolved.get("project"), Some(&AnswerValue::Text("from-file".to_string())));
    }

    #[test]
    fn defaults_only_uses_question_default_when_unanswered() {
        let question = string_question("project", Some("default-val"));

        let resolved = resolve_answers(
            &[question],
            &BTreeMap::new(),
            &BTreeMap::new(),
            true,
            false,
            &FakeAnswerSource::unreachable(),
        )
        .expect("resolve should succeed");

        assert_eq!(resolved.get("project"), Some(&AnswerValue::Text("default-val".to_string())));
    }

    #[test]
    fn defaults_only_without_default_is_error() {
        let question = string_question("project", None);

        let result = resolve_answers(
            &[question],
            &BTreeMap::new(),
            &BTreeMap::new(),
            true,
            false,
            &FakeAnswerSource::unreachable(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn interactive_asks_when_unanswered_and_not_defaults_only() {
        let question = string_question("project", None);
        let source = FakeAnswerSource::returning(AnswerValue::Text("asked".to_string()));

        let resolved = resolve_answers(&[question], &BTreeMap::new(), &BTreeMap::new(), false, true, &source)
            .expect("resolve should succeed");

        assert_eq!(resolved.get("project"), Some(&AnswerValue::Text("asked".to_string())));
        assert!(*source.called.borrow(), "ask should have been called");
    }

    #[test]
    fn noninteractive_unanswered_falls_back_to_default() {
        let question = string_question("project", Some("default-val"));

        let resolved = resolve_answers(
            &[question],
            &BTreeMap::new(),
            &BTreeMap::new(),
            false,
            false,
            &FakeAnswerSource::unreachable(),
        )
        .expect("resolve should succeed");

        assert_eq!(resolved.get("project"), Some(&AnswerValue::Text("default-val".to_string())));
    }

    #[test]
    fn noninteractive_unanswered_without_default_is_error() {
        let question = string_question("project", None);

        let result = resolve_answers(
            &[question],
            &BTreeMap::new(),
            &BTreeMap::new(),
            false,
            false,
            &FakeAnswerSource::unreachable(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn answers_file_value_failing_choice_validation_is_error() {
        use crate::domain::question::Choice;

        let question = Question {
            name: "license".to_string(),
            qtype: QuestionType::Select,
            prompt: None,
            choices: vec![Choice { label: "MIT".to_string(), value: AnswerValue::Text("MIT".to_string()) }],
            default: None,
            when: None,
            help: None,
        };
        let mut file = BTreeMap::new();
        file.insert("license".to_string(), AnswerValue::Text("BSD".to_string()));

        let result = resolve_answers(
            &[question],
            &BTreeMap::new(),
            &file,
            false,
            false,
            &FakeAnswerSource::unreachable(),
        );

        assert!(result.is_err());
    }
}
