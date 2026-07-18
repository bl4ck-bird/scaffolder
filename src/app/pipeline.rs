//! apply 라이프사이클 조립: 매니페스트 파싱 → answer 확정 → data 병합(`[data]`+`data/*.toml`,
//! §1.9 step 3) → plan(부작용 없음, `.scaffoldignore` 매칭 출력 경로는 제외) → dry-run이면 종료
//! → 훅 confirm(부작용 전 단일 게이트, §1.9-5) → before 훅 → write(overwrite/외부쓰기 confirm
//! 반영) → after 훅. partials는 `Renderer` 포트에 주입된다. 훅 오케스트레이션은 `app::hooks`가
//! 맡고 여기는 포트 배선만 한다. 도메인 포트만 사용한다.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::app::hooks::{collect_active_inline, confirm_description, run_phase};
use crate::domain::answer::{
    build_context, coerce, validate_choice, AnswerSource, AnswerValue, ConditionEvaluator,
    ScaffolderBuiltins,
};
use crate::domain::data::DataSource;
use crate::domain::hook::{hook_env, Confirmer, HookPhase, HookRunner, HookSource};
use crate::domain::ignore::IgnoreSource;
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
    pub mode: FileMode,
}

pub struct ApplyReport {
    pub planned: Vec<PlannedWrite>,
}

/// `apply`가 쓰는 포트 묶음(인자 개수 축소용 parameter object).
pub struct ApplyPorts<'a> {
    pub manifest_src: &'a dyn ManifestSource,
    pub data_source: &'a dyn DataSource,
    pub renderer: &'a dyn Renderer,
    pub payload: &'a dyn PayloadStore,
    pub confirmer: &'a dyn Confirmer,
    pub answer_source: &'a dyn AnswerSource,
    pub condition_evaluator: &'a dyn ConditionEvaluator,
    pub ignore_source: &'a dyn IgnoreSource,
    pub hook_source: &'a dyn HookSource,
    pub hook_runner: &'a dyn HookRunner,
}

/// `resolve_answers`가 쓰는 포트 묶음(인자 개수 축소용 parameter object).
struct AnswerPorts<'a> {
    answer_source: &'a dyn AnswerSource,
    condition_evaluator: &'a dyn ConditionEvaluator,
}

pub fn apply(req: &ApplyRequest, builtins: ScaffolderBuiltins, ports: ApplyPorts) -> Result<ApplyReport> {
    let manifest_path = req.template_root.join("scaffold.toml");
    let manifest = ports.manifest_src.load(&manifest_path)?;

    let answers = resolve_answers(
        &manifest.questions,
        &req.answers,
        &req.answers_file,
        req.defaults_only,
        req.interactive,
        AnswerPorts {
            answer_source: ports.answer_source,
            condition_evaluator: ports.condition_evaluator,
        },
        &builtins,
    )?;

    // answers는 build_context에 이동되기 전에 훅 env로 스냅샷해 둔다(§67).
    let hook_env_map = hook_env(&answers);

    // §1.9 step 3: answer 확정 이후 data를 병합한다. `[data]`(manifest)를 base로 `data/*.toml`을
    // lexical 순서로 fold한다(단일 left-fold — §1.5).
    let data = ports.data_source.load(&req.template_root, manifest.data)?;
    let ctx = build_context(answers, Some(data), builtins);
    let matcher = ports.ignore_source.load(&req.template_root, &ctx)?;

    let files_root = req.template_root.join("files");
    let entries = ports.payload.list_entries(&files_root)?;

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
        if matcher.is_ignored(std::path::Path::new(&out_rel_str)) {
            continue;
        }
        let out_rel = safe_rel_path(&out_rel_str)?;

        if planned.iter().any(|p| p.rel == out_rel) {
            bail!("source conflict: multiple entries map to output path {out_rel}");
        }

        let raw = ports.payload.read_content(&files_root, entry)?;
        let content = if parsed.render {
            let text = String::from_utf8(raw)
                .map_err(|_| anyhow::anyhow!("entry {} is not valid UTF-8 but is marked for rendering", entry.rel))?;
            ports.renderer.render_str(&text, &ctx)?.into_bytes()
        } else {
            raw
        };

        planned.push(PlannedWrite {
            rel: out_rel,
            rendered: parsed.render,
            content,
            mode: FileMode::from_modes(&parsed.modes),
        });
    }

    if req.dry_run {
        return Ok(ApplyReport { planned });
    }

    // dry-run 이후에만 훅을 수집한다 — dry-run은 훅과 무관하다.
    let before_inline = collect_active_inline(&manifest.hooks.before, &ctx, ports.condition_evaluator)?;
    let after_inline = collect_active_inline(&manifest.hooks.after, &ctx, ports.condition_evaluator)?;
    let before_scripts = ports.hook_source.scripts(&req.template_root, HookPhase::Before)?;
    let after_scripts = ports.hook_source.scripts(&req.template_root, HookPhase::After)?;

    let has_hooks = !before_inline.is_empty()
        || !before_scripts.is_empty()
        || !after_inline.is_empty()
        || !after_scripts.is_empty();

    // §1.9-5: before+after에서 실행될 훅 전부를 부작용(target 생성·쓰기) 전에 한 번만 confirm한다.
    if has_hooks {
        let description = confirm_description(&before_inline, &before_scripts, &after_inline, &after_scripts);
        if !ports.confirmer.confirm_hook(&description) {
            bail!("hook execution was not confirmed; aborting before any writes");
        }
    }

    // §1.9 step 6: target은 부작용 없는 plan 이후에 생성한다. render·소스 충돌 에러는 이미 plan에서
    // 실패했으므로, 여기 도달 시 빈 target을 남기지 않는다.
    ports.payload.ensure_target(&req.target_root)?;

    run_phase(
        ports.hook_runner,
        ports.renderer,
        &ctx,
        &before_inline,
        &before_scripts,
        &req.target_root,
        &hook_env_map,
    )?;

    for planned_write in &planned {
        let status = ports.payload.dest_status(&req.target_root, &planned_write.rel)?;

        // target 밖으로 이탈하는 쓰기는 confirm하고, 미승인이면 그 엔트리만 건너뛰고
        // 계속한다(overwrite와 달리 hard-fail이 아니다).
        if !status.inside_target && !ports.confirmer.confirm_external_write(&status.final_path) {
            eprintln!(
                "warning: skipping {} — write escapes target and was not confirmed",
                status.final_path.display()
            );
            continue;
        }

        if status.exists && !ports.confirmer.confirm_overwrite(&status.final_path) {
            bail!(
                "destination {} exists; pass --force to overwrite",
                status.final_path.display()
            );
        }

        ports.payload.write_file(
            &req.target_root,
            &planned_write.rel,
            &planned_write.content,
            planned_write.mode,
            status.exists,
        )?;
    }

    run_phase(
        ports.hook_runner,
        ports.renderer,
        &ctx,
        &after_inline,
        &after_scripts,
        &req.target_root,
        &hook_env_map,
    )?;

    Ok(ApplyReport { planned })
}

/// 질문을 선언 순서대로 증분 처리해 answer를 확정한다. 매 질문마다 `when`은 지금까지
/// 확정된 answers + builtins로만 평가한다(뒤 질문은 아직 컨텍스트에 없어 참조 시 에러).
///
/// - `when` 없음, 또는 `when` active: 아래 precedence로 확정해 넣는다.
///   1. `--answers`(raw 문자열, `coerce`로 타입 변환)
///   2. `--answers-file`(이미 타입이 정해진 값)
///   3. `--defaults`면 question default(없으면 에러 — 프롬프트로 폴백하지 않는다)
///   4. 대화형이면 `answer_source.ask`
///   5. 그 외(비대화형·`--defaults` 아님)는 default(없으면 에러)
/// - `when` inactive: 준 답변(`--answers`/`--answers-file`)은 무시한다. default가 있으면
///   그 값을 넣고, 없으면 넣지 않는다(컨텍스트에서 부재).
///
/// 확정된 값(active 값·inactive default 값)마다 `validate_choice`로 검증한다.
/// `--answers`/`--answers-file`의 미매칭 키는 경고만 하고 계속 진행한다.
fn resolve_answers(
    questions: &[Question],
    raw_answers: &BTreeMap<String, String>,
    answers_file: &BTreeMap<String, AnswerValue>,
    defaults_only: bool,
    interactive: bool,
    ports: AnswerPorts,
    builtins: &ScaffolderBuiltins,
) -> Result<BTreeMap<String, AnswerValue>> {
    let mut resolved = BTreeMap::new();

    for question in questions {
        let active = match &question.when {
            Some(when) => {
                // §1.9: data 병합(step 3)은 answer 확정(step 2) 이후다. 따라서 `when`은 앞선
                // 답변 + builtins만 참조하며 data 네임스페이스는 컨텍스트에서 부재다(None).
                let ctx = build_context(resolved.clone(), None, builtins.clone());
                ports.condition_evaluator.is_active(when, &ctx)?
            }
            None => true,
        };

        if !active {
            if let Some(default) = question.default.clone() {
                validate_choice(question, &default)?;
                resolved.insert(question.name.clone(), default);
            }
            continue;
        }

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
            ports.answer_source.ask(question)?
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
    use crate::domain::hook::{Confirmer, HookPhase, HookRunner, HookScript, HookSource};
    use crate::domain::ignore::IgnoreMatcher;
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

    struct FakeDataSource;
    impl DataSource for FakeDataSource {
        fn load(
            &self,
            _template_root: &Path,
            base: crate::domain::data::DataValue,
        ) -> Result<crate::domain::data::DataValue> {
            Ok(base)
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

    /// 훅 confirm을 항상 거절하는 테스트용 `Confirmer` — 나머지 게이트는 `FakeConfirmer`와 동일하게
    /// 항상 승인한다(이 게이트만 격리 검증하기 위함).
    struct DecliningHookConfirmer;
    impl Confirmer for DecliningHookConfirmer {
        fn confirm_hook(&self, _description: &str) -> bool {
            false
        }
        fn confirm_overwrite(&self, _path: &Path) -> bool {
            true
        }
        fn confirm_external_write(&self, _path: &Path) -> bool {
            true
        }
    }

    /// 폴더 스크립트가 없는(빈) 테스트용 `HookSource`.
    struct FakeHookSource;
    impl HookSource for FakeHookSource {
        fn scripts(&self, _template_root: &Path, _phase: HookPhase) -> Result<Vec<HookScript>> {
            Ok(Vec::new())
        }
    }

    /// 실제 프로세스를 실행하지 않고 호출만 기록하는 테스트용 `HookRunner`.
    struct FakeHookRunner {
        calls: RefCell<Vec<String>>,
    }
    impl FakeHookRunner {
        fn new() -> Self {
            Self { calls: RefCell::new(Vec::new()) }
        }
    }
    impl HookRunner for FakeHookRunner {
        fn run_inline(&self, command: &str, _cwd: &Path, _env: &BTreeMap<String, String>) -> Result<()> {
            self.calls.borrow_mut().push(format!("inline:{command}"));
            Ok(())
        }
        fn run_script_file(&self, path: &Path, _cwd: &Path, _env: &BTreeMap<String, String>) -> Result<()> {
            self.calls.borrow_mut().push(format!("script:{}", path.display()));
            Ok(())
        }
        fn run_rendered(
            &self,
            name: &str,
            _content: &[u8],
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<()> {
            self.calls.borrow_mut().push(format!("rendered:{name}"));
            Ok(())
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

    /// 고정 `default`를 반환하되 특정 `when` 문자열에는 `overrides`로 결과를 지정할 수 있는
    /// 테스트용 `ConditionEvaluator`.
    struct FakeConditionEvaluator {
        default: bool,
        overrides: HashMap<String, bool>,
    }
    impl FakeConditionEvaluator {
        fn always(active: bool) -> Self {
            Self { default: active, overrides: HashMap::new() }
        }
    }
    impl ConditionEvaluator for FakeConditionEvaluator {
        fn is_active(&self, when: &str, _ctx: &AnswerContext) -> Result<bool> {
            Ok(*self.overrides.get(when).unwrap_or(&self.default))
        }
    }

    /// `when`을 앞선 질문명 그대로 참조하게 해 증분 컨텍스트 순서를 검증하는 테스트용
    /// `ConditionEvaluator`. `ctx.answer(when)`이 `Bool`이면 그 값을, 없으면(아직 확정되지
    /// 않은 뒤 질문을 참조하면) 에러를 낸다.
    struct AnswerRefConditionEvaluator;
    impl ConditionEvaluator for AnswerRefConditionEvaluator {
        fn is_active(&self, when: &str, ctx: &AnswerContext) -> Result<bool> {
            match ctx.answer(when) {
                Some(AnswerValue::Bool(b)) => Ok(*b),
                Some(_) => Ok(true),
                None => bail!("`when` refers to an unresolved question '{when}'"),
            }
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

        fn ensure_target(&self, _target_root: &Path) -> Result<()> {
            Ok(())
        }

        fn write_file(
            &self,
            _target_root: &Path,
            rel: &RelPath,
            content: &[u8],
            _mode: crate::domain::place::FileMode,
            _overwrite: bool,
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

    /// 고정된 rel 집합만 제외하는(또는 빈 집합이면 아무것도 제외하지 않는) 테스트용
    /// `IgnoreSource`.
    struct FakeIgnoreSource {
        ignored: Vec<String>,
    }
    impl FakeIgnoreSource {
        fn none() -> Self {
            Self { ignored: Vec::new() }
        }
    }
    impl IgnoreSource for FakeIgnoreSource {
        fn load(&self, _template_root: &Path, _ctx: &AnswerContext) -> Result<Box<dyn IgnoreMatcher>> {
            Ok(Box::new(FakeIgnoreMatcher(self.ignored.clone())))
        }
    }

    struct FakeIgnoreMatcher(Vec<String>);
    impl IgnoreMatcher for FakeIgnoreMatcher {
        fn is_ignored(&self, rel: &Path) -> bool {
            self.0.iter().any(|ignored| Path::new(ignored) == rel)
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
            ..Default::default()
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
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer { overwrite: true, external: true },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
        )
        .expect("apply should succeed");

        assert_eq!(report.planned.len(), 1);
        assert_eq!(report.planned[0].rel.to_string(), "README.md");
        assert!(report.planned[0].rendered);
        assert!(store.written.borrow().is_empty(), "dry-run must not write");
    }

    #[test]
    fn source_conflict_on_same_output_path_is_error() {
        let manifest = Manifest {
            questions: vec![],
            ..Default::default()
        };
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
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer { overwrite: true, external: true },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn missing_required_answer_without_default_is_error() {
        let manifest = Manifest {
            questions: vec![string_question("project", None)],
            ..Default::default()
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
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer { overwrite: true, external: true },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn external_write_without_confirmation_is_skipped_not_written() {
        // rel 문자열은 `safe_rel_path`가 literal '..'을 이미 거부하므로 항상 정상 형태다;
        // containment 이탈은 상위 심링크 등 최종 경로 해석 단계에서만 드러난다.
        // 미승인 외부쓰기는 그 엔트리만 스킵하고 apply는 성공한다.
        let manifest = Manifest {
            questions: vec![],
            ..Default::default()
        };
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
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer { overwrite: true, external: false },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
        );

        assert!(result.is_ok());
        assert!(store.written.borrow().is_empty());
    }

    #[test]
    fn existing_destination_without_overwrite_confirmation_is_error() {
        let manifest = Manifest {
            questions: vec![],
            ..Default::default()
        };
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
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer { overwrite: false, external: true },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
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

        let resolved = resolve_answers(
            &[question],
            &raw,
            &file,
            false,
            false,
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
            },
            &builtins(),
        )
        .expect("resolve should succeed");

        assert_eq!(resolved.get("project"), Some(&AnswerValue::Text("from-cli".to_string())));
    }

    #[test]
    fn answers_file_used_when_cli_answer_missing() {
        let question = string_question("project", Some("default-val"));
        let raw = BTreeMap::new();
        let mut file = BTreeMap::new();
        file.insert("project".to_string(), AnswerValue::Text("from-file".to_string()));

        let resolved = resolve_answers(
            &[question],
            &raw,
            &file,
            false,
            false,
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
            },
            &builtins(),
        )
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
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
            },
            &builtins(),
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
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
            },
            &builtins(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn interactive_asks_when_unanswered_and_not_defaults_only() {
        let question = string_question("project", None);
        let source = FakeAnswerSource::returning(AnswerValue::Text("asked".to_string()));

        let resolved = resolve_answers(
            &[question],
            &BTreeMap::new(),
            &BTreeMap::new(),
            false,
            true,
            AnswerPorts {
                answer_source: &source,
                condition_evaluator: &FakeConditionEvaluator::always(true),
            },
            &builtins(),
        )
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
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
            },
            &builtins(),
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
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
            },
            &builtins(),
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
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
            },
            &builtins(),
        );

        assert!(result.is_err());
    }

    fn bool_question(name: &str, default: Option<bool>, when: Option<&str>) -> Question {
        Question {
            name: name.to_string(),
            qtype: QuestionType::Boolean,
            prompt: None,
            choices: Vec::new(),
            default: default.map(AnswerValue::Bool),
            when: when.map(|w| w.to_string()),
            help: None,
        }
    }

    fn string_question_with_when(name: &str, default: Option<&str>, when: &str) -> Question {
        Question {
            name: name.to_string(),
            qtype: QuestionType::String,
            prompt: None,
            choices: Vec::new(),
            default: default.map(|d| AnswerValue::Text(d.to_string())),
            when: Some(when.to_string()),
            help: None,
        }
    }

    #[test]
    fn when_active_resolves_value_via_precedence() {
        let question = string_question_with_when("feature", None, "gate");
        let mut raw = BTreeMap::new();
        raw.insert("feature".to_string(), "on".to_string());

        let resolved = resolve_answers(
            &[question],
            &raw,
            &BTreeMap::new(),
            false,
            false,
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
            },
            &builtins(),
        )
        .expect("resolve should succeed");

        assert_eq!(resolved.get("feature"), Some(&AnswerValue::Text("on".to_string())));
    }

    #[test]
    fn when_inactive_uses_default_and_ignores_given_answer() {
        let question = string_question_with_when("feature", Some("fallback"), "gate");
        let mut raw = BTreeMap::new();
        raw.insert("feature".to_string(), "on".to_string());

        let resolved = resolve_answers(
            &[question],
            &raw,
            &BTreeMap::new(),
            false,
            false,
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(false),
            },
            &builtins(),
        )
        .expect("resolve should succeed");

        assert_eq!(resolved.get("feature"), Some(&AnswerValue::Text("fallback".to_string())));
    }

    #[test]
    fn when_inactive_without_default_is_absent_not_error() {
        let question = string_question_with_when("feature", None, "gate");
        let mut raw = BTreeMap::new();
        raw.insert("feature".to_string(), "on".to_string());
        let mut file = BTreeMap::new();
        file.insert("feature".to_string(), AnswerValue::Text("from-file".to_string()));

        let resolved = resolve_answers(
            &[question],
            &raw,
            &file,
            false,
            false,
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(false),
            },
            &builtins(),
        )
        .expect("resolve should succeed");

        assert_eq!(resolved.get("feature"), None);
    }

    #[test]
    fn when_evaluates_against_incrementally_confirmed_earlier_answer() {
        let gate = bool_question("gate", None, None);
        let dependent = string_question_with_when("dependent", None, "gate");
        let mut raw = BTreeMap::new();
        raw.insert("gate".to_string(), "true".to_string());
        raw.insert("dependent".to_string(), "chosen".to_string());

        let resolved = resolve_answers(
            &[gate, dependent],
            &raw,
            &BTreeMap::new(),
            false,
            false,
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &AnswerRefConditionEvaluator,
            },
            &builtins(),
        )
        .expect("resolve should succeed");

        assert_eq!(resolved.get("gate"), Some(&AnswerValue::Bool(true)));
        assert_eq!(resolved.get("dependent"), Some(&AnswerValue::Text("chosen".to_string())));
    }

    #[test]
    fn when_inactive_based_on_earlier_answer_drops_default() {
        let gate = bool_question("gate", None, None);
        let dependent = string_question_with_when("dependent", Some("fallback"), "gate");
        let mut raw = BTreeMap::new();
        raw.insert("gate".to_string(), "false".to_string());
        raw.insert("dependent".to_string(), "chosen".to_string());

        let resolved = resolve_answers(
            &[gate, dependent],
            &raw,
            &BTreeMap::new(),
            false,
            false,
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &AnswerRefConditionEvaluator,
            },
            &builtins(),
        )
        .expect("resolve should succeed");

        assert_eq!(resolved.get("gate"), Some(&AnswerValue::Bool(false)));
        assert_eq!(resolved.get("dependent"), Some(&AnswerValue::Text("fallback".to_string())));
    }

    #[test]
    fn when_referencing_a_later_unresolved_question_errors() {
        // 증분 컨텍스트 확인: `when`이 뒤 질문을 참조하면 아직 확정되지 않았으므로 에러다.
        let dependent = string_question_with_when("dependent", None, "gate");
        let gate = bool_question("gate", None, None);
        let mut raw = BTreeMap::new();
        raw.insert("gate".to_string(), "true".to_string());
        raw.insert("dependent".to_string(), "chosen".to_string());

        let result = resolve_answers(
            &[dependent, gate],
            &raw,
            &BTreeMap::new(),
            false,
            false,
            AnswerPorts {
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &AnswerRefConditionEvaluator,
            },
            &builtins(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn hook_present_and_confirm_declined_aborts_before_any_write_or_target_creation() {
        let manifest = Manifest {
            questions: vec![],
            hooks: crate::domain::hook::Hooks {
                before: vec![crate::domain::hook::Hook { when: None, run: "echo should-not-run".to_string() }],
                after: vec![],
            },
            ..Default::default()
        };
        let store = FakePayloadStore {
            entries: vec![PayloadEntry { rel: safe_rel_path("file.txt").unwrap(), is_dir: false }],
            contents: HashMap::from([("file.txt".to_string(), b"content".to_vec())]),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
        };
        let runner = FakeHookRunner::new();

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
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &DecliningHookConfirmer,
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &runner,
            },
        );

        assert!(result.is_err(), "declined hook confirm must abort");
        assert!(store.written.borrow().is_empty(), "no write must happen before hook confirm");
        assert!(runner.calls.borrow().is_empty(), "hook must not run when confirm is declined");
    }

    #[test]
    fn dry_run_skips_hook_confirm_and_execution_even_when_hooks_are_declared() {
        let manifest = Manifest {
            questions: vec![],
            hooks: crate::domain::hook::Hooks {
                before: vec![crate::domain::hook::Hook { when: None, run: "echo should-not-run".to_string() }],
                after: vec![],
            },
            ..Default::default()
        };
        let store = FakePayloadStore {
            entries: vec![],
            contents: HashMap::new(),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
        };
        let runner = FakeHookRunner::new();

        let req = ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers: BTreeMap::new(),
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: true,
        };

        let report = apply(
            &req,
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                // dry-run이 훅 collection·confirm 자체를 건너뛴다면 거절 confirmer라도 무해해야 한다.
                confirmer: &DecliningHookConfirmer,
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &runner,
            },
        )
        .expect("dry-run must succeed even though hook confirm would be declined");

        assert!(report.planned.is_empty());
        assert!(runner.calls.borrow().is_empty(), "dry-run must not execute hooks");
    }
}
