//! Assembles the apply lifecycle: parse manifest → resolve answers → merge data → plan
//! (side-effect-free) → hook confirm → before hooks → write → after hooks. Uses only domain
//! ports; hook orchestration lives in `app::hooks`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::app::hooks::{collect_active_inline, confirm_description, run_phase};
use crate::domain::answer::{
    AnswerContext, AnswerSource, AnswerValue, ConditionEvaluator, ScaffolderBuiltins,
    build_context, coerce, validate_choice,
};
use crate::domain::data::DataSource;
use crate::domain::hook::{
    Confirmer, Hook, HookPhase, HookRunner, HookScript, HookSource, hook_env,
};
use crate::domain::ignore::{IgnoreMatcher, IgnoreSource};
use crate::domain::manifest::ManifestSource;
use crate::domain::name::parse_file_name;
use crate::domain::place::{FileMode, PayloadStore, RelPath, TargetPreparation, safe_rel_path};
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
    /// Whether to clean up a target we created (`Created`) if a post-prepare step fails; false preserves it.
    pub cleanup_on_failure: bool,
}

#[derive(Debug)]
pub struct PlannedWrite {
    pub rel: RelPath,
    pub rendered: bool,
    pub content: Vec<u8>,
    pub mode: FileMode,
}

#[derive(Debug)]
pub struct ApplyReport {
    pub planned: Vec<PlannedWrite>,
}

/// The ports `apply` uses (a parameter object to keep the argument count down).
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

/// The ports `resolve_answers` uses (a parameter object).
struct AnswerPorts<'a> {
    answer_source: &'a dyn AnswerSource,
    condition_evaluator: &'a dyn ConditionEvaluator,
}

/// Hooks collected after dry-run. Inline hooks borrow `manifest`, hence the tied lifetime.
struct CollectedHooks<'a> {
    before_inline: Vec<&'a Hook>,
    before_scripts: Vec<HookScript>,
    after_inline: Vec<&'a Hook>,
    after_scripts: Vec<HookScript>,
}

impl CollectedHooks<'_> {
    fn is_empty(&self) -> bool {
        self.before_inline.is_empty()
            && self.before_scripts.is_empty()
            && self.after_inline.is_empty()
            && self.after_scripts.is_empty()
    }

    fn describe(&self) -> String {
        confirm_description(
            &self.before_inline,
            &self.before_scripts,
            &self.after_inline,
            &self.after_scripts,
        )
    }
}

pub fn apply(
    req: &ApplyRequest,
    builtins: ScaffolderBuiltins,
    ports: ApplyPorts,
) -> Result<ApplyReport> {
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

    // Snapshot answers into hook env before they are moved into build_context.
    let hook_env_map = hook_env(&answers);

    // Merge data after answers are resolved: fold data/*.toml (lexical order) onto the
    // manifest [data] base (a single left-fold).
    let data = ports.data_source.load(&req.template_root, manifest.data)?;
    let ctx = build_context(answers, Some(data), builtins);
    let matcher = ports.ignore_source.load(&req.template_root, &ctx)?;

    let planned = plan_writes(req, &ctx, matcher.as_ref(), &ports)?;

    if req.dry_run {
        return Ok(ApplyReport { planned });
    }

    // Collect hooks only after dry-run — dry-run does not touch hooks.
    let hooks = CollectedHooks {
        before_inline: collect_active_inline(
            &manifest.hooks.before,
            &ctx,
            ports.condition_evaluator,
        )?,
        after_inline: collect_active_inline(
            &manifest.hooks.after,
            &ctx,
            ports.condition_evaluator,
        )?,
        before_scripts: ports
            .hook_source
            .scripts(&req.template_root, HookPhase::Before)?,
        after_scripts: ports
            .hook_source
            .scripts(&req.template_root, HookPhase::After)?,
    };

    // Confirm all before+after hooks once, before any side effect (target creation, writes).
    if !hooks.is_empty() && !ports.confirmer.confirm_hook(&hooks.describe()) {
        bail!("hook execution was not confirmed; aborting before any writes");
    }

    // Create the target after the side-effect-free plan. Render and source-conflict errors
    // already failed in plan, so reaching here leaves no empty target.
    let prep = ports.payload.ensure_target(&req.target_root)?;

    let outcome = execute_side_effects(req, &planned, &ctx, &hooks, &hook_env_map, &ports);
    cleanup_created_target_on_failure(outcome, prep, req, ports.payload)?;

    Ok(ApplyReport { planned })
}

/// The side-effect-free plan step: enumerates payload entries, parses file-name grammar,
/// excludes `.scaffoldignore`-matched output paths, detects source conflicts (two entries
/// mapping to the same output path), and renders `.jinja` entries into `PlannedWrite`s.
fn plan_writes(
    req: &ApplyRequest,
    ctx: &AnswerContext,
    matcher: &dyn IgnoreMatcher,
    ports: &ApplyPorts,
) -> Result<Vec<PlannedWrite>> {
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
            Some(parent) => parent
                .join(&parsed.output_base)
                .to_string_lossy()
                .into_owned(),
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
            let text = String::from_utf8(raw).map_err(|_| {
                anyhow::anyhow!(
                    "entry {} is not valid UTF-8 but is marked for rendering",
                    entry.rel
                )
            })?;
            ports.renderer.render_str(&text, ctx)?.into_bytes()
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

    Ok(planned)
}

/// Runs every side effect after prepare, in order: before hooks → writes (per-entry
/// containment/overwrite/external-write gates) → after hooks. Returns the first `Err`; the
/// caller (`cleanup_created_target_on_failure`) decides whether to clean up.
fn execute_side_effects(
    req: &ApplyRequest,
    planned: &[PlannedWrite],
    ctx: &AnswerContext,
    hooks: &CollectedHooks,
    hook_env_map: &BTreeMap<String, String>,
    ports: &ApplyPorts,
) -> Result<()> {
    run_phase(
        ports.hook_runner,
        ports.renderer,
        ctx,
        &hooks.before_inline,
        &hooks.before_scripts,
        &req.target_root,
        hook_env_map,
    )?;

    for planned_write in planned {
        let status = ports
            .payload
            .dest_status(&req.target_root, &planned_write.rel)?;

        // A write escaping the target is confirmed; if declined, skip only that entry and
        // continue (unlike overwrite, not a hard fail).
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
        ctx,
        &hooks.after_inline,
        &hooks.after_scripts,
        &req.target_root,
        hook_env_map,
    )?;

    Ok(())
}

/// Cleanup guard for post-prepare side effects. Only when `outcome` is `Err`, the target was
/// `Created`, and cleanup is on does it best-effort delete, then propagate the **original**
/// error (a cleanup failure only warns). `Existing` targets and cleanup-off preserve partial
/// output; `Ok` passes through. A prepare failure occurs before this call, so it is not cleaned.
fn cleanup_created_target_on_failure(
    outcome: Result<()>,
    prep: TargetPreparation,
    req: &ApplyRequest,
    payload: &dyn PayloadStore,
) -> Result<()> {
    let Err(e) = outcome else {
        return Ok(());
    };
    if prep == TargetPreparation::Created && req.cleanup_on_failure {
        if let Err(cleanup_err) = payload.cleanup_target(&req.target_root) {
            eprintln!(
                "warning: failed to clean up target {}: {cleanup_err:#}",
                req.target_root.display()
            );
        }
    }
    Err(e)
}

/// Resolves answers by processing questions incrementally in declaration order. For each
/// question, `when` is evaluated against only the answers resolved so far + builtins (a later
/// question is not yet in context, so referencing it errors).
///
/// - No `when`, or active `when`: resolve by this precedence and insert.
///   1. `--answers` (raw string, type-converted with `coerce`)
///   2. `--answers-file` (already-typed value)
///   3. `--defaults`: the question default (error if none — no fallback to prompting)
///   4. interactive: `answer_source.ask`
///   5. otherwise (non-interactive, not `--defaults`): the default (error if none)
/// - Inactive `when`: given answers (`--answers`/`--answers-file`) are ignored. If a default
///   exists it is inserted, otherwise nothing is (absent from context).
///
/// Every resolved value (active values and inactive defaults) is checked with `validate_choice`.
/// Unmatched `--answers`/`--answers-file` keys only warn and continue.
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
                // Data merge (step 3) happens after answers are resolved (step 2), so `when`
                // sees only earlier answers + builtins; the data namespace is absent (None).
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
            eprintln!(
                "warning: '--answers-file' key '{key}' does not match any question; ignoring"
            );
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
    use crate::domain::question::QuestionType;
    use std::cell::RefCell;
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

    /// Test `Confirmer` that always declines the hook confirm; the other gates always approve
    /// (so this gate can be verified in isolation).
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

    /// Test `HookSource` with no folder scripts (empty).
    struct FakeHookSource;
    impl HookSource for FakeHookSource {
        fn scripts(&self, _template_root: &Path, _phase: HookPhase) -> Result<Vec<HookScript>> {
            Ok(Vec::new())
        }
    }

    /// Test `HookRunner` that records calls instead of running a real process.
    struct FakeHookRunner {
        calls: RefCell<Vec<String>>,
    }
    impl FakeHookRunner {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
            }
        }
    }
    impl HookRunner for FakeHookRunner {
        fn run_inline(
            &self,
            command: &str,
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<()> {
            self.calls.borrow_mut().push(format!("inline:{command}"));
            Ok(())
        }
        fn run_script_file(
            &self,
            path: &Path,
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("script:{}", path.display()));
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

    /// Test `HookRunner` that injects a before/after hook failure (for the cleanup guard).
    struct FailingHookRunner;
    impl HookRunner for FailingHookRunner {
        fn run_inline(
            &self,
            _command: &str,
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<()> {
            bail!("simulated hook failure")
        }
        fn run_script_file(
            &self,
            _path: &Path,
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<()> {
            bail!("simulated hook failure")
        }
        fn run_rendered(
            &self,
            _name: &str,
            _content: &[u8],
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<()> {
            bail!("simulated hook failure")
        }
    }

    /// Test `AnswerSource` returning a fixed value (`returning`) or asserting it must not be
    /// called (`unreachable`). Used to verify `ask` is skipped when precedence bypasses prompting.
    struct FakeAnswerSource {
        value: Option<AnswerValue>,
        called: RefCell<bool>,
    }
    impl FakeAnswerSource {
        fn returning(value: AnswerValue) -> Self {
            Self {
                value: Some(value),
                called: RefCell::new(false),
            }
        }

        fn unreachable() -> Self {
            Self {
                value: None,
                called: RefCell::new(false),
            }
        }
    }
    impl AnswerSource for FakeAnswerSource {
        fn ask(&self, question: &Question) -> Result<AnswerValue> {
            *self.called.borrow_mut() = true;
            self.value.clone().ok_or_else(|| {
                anyhow::anyhow!("ask should not have been called for '{}'", question.name)
            })
        }
    }

    /// Test `ConditionEvaluator` returning a fixed `default`, overridable per `when` string via `overrides`.
    struct FakeConditionEvaluator {
        default: bool,
        overrides: HashMap<String, bool>,
    }
    impl FakeConditionEvaluator {
        fn always(active: bool) -> Self {
            Self {
                default: active,
                overrides: HashMap::new(),
            }
        }
    }
    impl ConditionEvaluator for FakeConditionEvaluator {
        fn is_active(&self, when: &str, _ctx: &AnswerContext) -> Result<bool> {
            Ok(*self.overrides.get(when).unwrap_or(&self.default))
        }
    }

    /// Test `ConditionEvaluator` that references `when` as an earlier question name, to verify
    /// incremental context order. Returns the `Bool` answer if present, else errors (a
    /// not-yet-resolved later question).
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

    /// For source-conflict setups: returns two entries whose different basenames map to the
    /// same output rel (both verbatim, equal after `.jinja` strip).
    struct FakePayloadStore {
        entries: Vec<PayloadEntry>,
        contents: HashMap<String, Vec<u8>>,
        dest_statuses: RefCell<HashMap<String, DestStatus>>,
        written: RefCell<Vec<(RelPath, Vec<u8>)>>,
        /// The verdict `ensure_target` returns (Created/Existing per test).
        prep: TargetPreparation,
        /// Records whether `cleanup_target` was called.
        cleaned: RefCell<bool>,
        /// When true, `cleanup_target` records the call but returns an error (so the original error still wins).
        cleanup_fails: bool,
        /// The path `cleanup_target` received (to verify the exact prepared root is passed).
        cleaned_path: RefCell<Option<PathBuf>>,
        /// Records whether `ensure_target` was called (to verify it is not on a pre-prepare failure).
        prepared: RefCell<bool>,
        /// When true, `write_file` simulates a real I/O failure and returns an error.
        write_fails: bool,
        /// When true, `dest_status` returns an error (to exercise the post-prepare failure path).
        dest_status_fails: bool,
        /// When true, `ensure_target` returns an error (prepare itself fails → verify cleanup is not called).
        prepare_fails: bool,
    }

    impl PayloadStore for FakePayloadStore {
        fn list_entries(&self, _source_root: &Path) -> Result<Vec<PayloadEntry>> {
            Ok(self.entries.clone())
        }

        fn read_content(&self, _source_root: &Path, entry: &PayloadEntry) -> Result<Vec<u8>> {
            Ok(self
                .contents
                .get(&entry.rel.to_string())
                .cloned()
                .unwrap_or_default())
        }

        fn ensure_target(&self, _target_root: &Path) -> Result<TargetPreparation> {
            *self.prepared.borrow_mut() = true;
            if self.prepare_fails {
                bail!("simulated prepare failure");
            }
            Ok(self.prep)
        }

        fn cleanup_target(&self, target_root: &Path) -> Result<()> {
            *self.cleaned.borrow_mut() = true;
            *self.cleaned_path.borrow_mut() = Some(target_root.to_path_buf());
            if self.cleanup_fails {
                bail!("simulated cleanup failure");
            }
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
            if self.write_fails {
                bail!("simulated write failure");
            }
            self.written
                .borrow_mut()
                .push((rel.clone(), content.to_vec()));
            Ok(())
        }

        fn dest_status(&self, _target_root: &Path, rel: &RelPath) -> Result<DestStatus> {
            if self.dest_status_fails {
                bail!("simulated dest_status failure");
            }
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

    /// Test `IgnoreSource` that excludes only a fixed set of rels (or nothing when empty).
    struct FakeIgnoreSource {
        ignored: Vec<String>,
    }
    impl FakeIgnoreSource {
        fn none() -> Self {
            Self {
                ignored: Vec::new(),
            }
        }
    }
    impl IgnoreSource for FakeIgnoreSource {
        fn load(
            &self,
            _template_root: &Path,
            _ctx: &AnswerContext,
        ) -> Result<Box<dyn IgnoreMatcher>> {
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
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
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
            cleanup_on_failure: true,
        };

        let report = apply(
            &req,
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: true,
                },
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
                PayloadEntry {
                    rel: safe_rel_path("README.md").unwrap(),
                    is_dir: false,
                },
                PayloadEntry {
                    rel: safe_rel_path("README.md.jinja").unwrap(),
                    is_dir: false,
                },
            ],
            contents: HashMap::from([
                ("README.md".to_string(), b"verbatim".to_vec()),
                ("README.md.jinja".to_string(), b"rendered".to_vec()),
            ]),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
        };

        let req = ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers: BTreeMap::new(),
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: true,
            cleanup_on_failure: true,
        };

        let result = apply(
            &req,
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: true,
                },
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
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
        };

        let req = ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers: BTreeMap::new(),
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: true,
            cleanup_on_failure: true,
        };

        let result = apply(
            &req,
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: true,
                },
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
        // The rel string is always well-formed (safe_rel_path already rejects literal '..');
        // a containment escape only shows up at final-path resolution (e.g. an ancestor
        // symlink). An unconfirmed external write skips just that entry and apply still succeeds.
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
            entries: vec![PayloadEntry {
                rel: safe_rel_path("linked/outside.txt").unwrap(),
                is_dir: false,
            }],
            contents: HashMap::from([("linked/outside.txt".to_string(), b"content".to_vec())]),
            dest_statuses: RefCell::new(dest_statuses),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
        };

        let req = ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers: BTreeMap::new(),
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: false,
            cleanup_on_failure: true,
        };

        let result = apply(
            &req,
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: false,
                },
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
            entries: vec![PayloadEntry {
                rel: safe_rel_path("file.txt").unwrap(),
                is_dir: false,
            }],
            contents: HashMap::from([("file.txt".to_string(), b"content".to_vec())]),
            dest_statuses: RefCell::new(dest_statuses),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
        };

        let req = ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers: BTreeMap::new(),
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: false,
            cleanup_on_failure: true,
        };

        let result = apply(
            &req,
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: false,
                    external: true,
                },
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
        file.insert(
            "project".to_string(),
            AnswerValue::Text("from-file".to_string()),
        );

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

        assert_eq!(
            resolved.get("project"),
            Some(&AnswerValue::Text("from-cli".to_string()))
        );
    }

    #[test]
    fn answers_file_used_when_cli_answer_missing() {
        let question = string_question("project", Some("default-val"));
        let raw = BTreeMap::new();
        let mut file = BTreeMap::new();
        file.insert(
            "project".to_string(),
            AnswerValue::Text("from-file".to_string()),
        );

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

        assert_eq!(
            resolved.get("project"),
            Some(&AnswerValue::Text("from-file".to_string()))
        );
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

        assert_eq!(
            resolved.get("project"),
            Some(&AnswerValue::Text("default-val".to_string()))
        );
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

        assert_eq!(
            resolved.get("project"),
            Some(&AnswerValue::Text("asked".to_string()))
        );
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

        assert_eq!(
            resolved.get("project"),
            Some(&AnswerValue::Text("default-val".to_string()))
        );
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
            choices: vec![Choice {
                label: "MIT".to_string(),
                value: AnswerValue::Text("MIT".to_string()),
            }],
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

        assert_eq!(
            resolved.get("feature"),
            Some(&AnswerValue::Text("on".to_string()))
        );
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

        assert_eq!(
            resolved.get("feature"),
            Some(&AnswerValue::Text("fallback".to_string()))
        );
    }

    #[test]
    fn when_inactive_without_default_is_absent_not_error() {
        let question = string_question_with_when("feature", None, "gate");
        let mut raw = BTreeMap::new();
        raw.insert("feature".to_string(), "on".to_string());
        let mut file = BTreeMap::new();
        file.insert(
            "feature".to_string(),
            AnswerValue::Text("from-file".to_string()),
        );

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
        assert_eq!(
            resolved.get("dependent"),
            Some(&AnswerValue::Text("chosen".to_string()))
        );
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
        assert_eq!(
            resolved.get("dependent"),
            Some(&AnswerValue::Text("fallback".to_string()))
        );
    }

    #[test]
    fn when_referencing_a_later_unresolved_question_errors() {
        // Incremental context check: a `when` referencing a later question errors, since it is not yet resolved.
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
                before: vec![crate::domain::hook::Hook {
                    when: None,
                    run: "echo should-not-run".to_string(),
                }],
                after: vec![],
            },
            ..Default::default()
        };
        let store = FakePayloadStore {
            entries: vec![PayloadEntry {
                rel: safe_rel_path("file.txt").unwrap(),
                is_dir: false,
            }],
            contents: HashMap::from([("file.txt".to_string(), b"content".to_vec())]),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
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
            cleanup_on_failure: true,
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
        assert!(
            store.written.borrow().is_empty(),
            "no write must happen before hook confirm"
        );
        assert!(
            runner.calls.borrow().is_empty(),
            "hook must not run when confirm is declined"
        );
    }

    #[test]
    fn dry_run_skips_hook_confirm_and_execution_even_when_hooks_are_declared() {
        let manifest = Manifest {
            questions: vec![],
            hooks: crate::domain::hook::Hooks {
                before: vec![crate::domain::hook::Hook {
                    when: None,
                    run: "echo should-not-run".to_string(),
                }],
                after: vec![],
            },
            ..Default::default()
        };
        let store = FakePayloadStore {
            entries: vec![],
            contents: HashMap::new(),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
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
            cleanup_on_failure: true,
        };

        let report = apply(
            &req,
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                // If dry-run skips hook collection/confirm entirely, even a declining confirmer is harmless.
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
        assert!(
            runner.calls.borrow().is_empty(),
            "dry-run must not execute hooks"
        );
    }

    // --- target cleanup guard on failure ---

    /// Store that fails in the write step via overwrite refusal (for the cleanup guard). It
    /// reports the dest already exists, so with `confirm_overwrite=false` the write loop bails.
    fn overwrite_conflict_store(prep: TargetPreparation, cleanup_fails: bool) -> FakePayloadStore {
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
        FakePayloadStore {
            entries: vec![PayloadEntry {
                rel: safe_rel_path("file.txt").unwrap(),
                is_dir: false,
            }],
            contents: HashMap::from([("file.txt".to_string(), b"content".to_vec())]),
            dest_statuses: RefCell::new(dest_statuses),
            written: RefCell::new(Vec::new()),
            prep,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails,
            prepare_fails: false,
        }
    }

    fn cleanup_req(cleanup_on_failure: bool) -> ApplyRequest {
        ApplyRequest {
            template_root: PathBuf::from("/tpl"),
            target_root: PathBuf::from("/target"),
            answers: BTreeMap::new(),
            answers_file: BTreeMap::new(),
            defaults_only: false,
            interactive: false,
            dry_run: false,
            cleanup_on_failure,
        }
    }

    fn empty_manifest() -> Manifest {
        Manifest {
            questions: vec![],
            ..Default::default()
        }
    }

    /// Runs apply for the cleanup-guard tests: induces overwrite refusal (a post-prepare failure) and returns the result.
    fn run_with_overwrite_conflict(
        manifest: Manifest,
        store: &FakePayloadStore,
        req: &ApplyRequest,
    ) -> Result<ApplyReport> {
        apply(
            req,
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: store,
                confirmer: &FakeConfirmer {
                    overwrite: false,
                    external: true,
                },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
        )
    }

    // Verifies the cleanup guard directly, without going through `apply` (the key combos of its 6 branches).

    #[test]
    fn cleanup_guard_cleans_created_target_when_enabled_and_preserves_error() {
        let store = overwrite_conflict_store(TargetPreparation::Created, false);
        let req = cleanup_req(true);
        let out = cleanup_created_target_on_failure(
            Err(anyhow::anyhow!("original boom")),
            TargetPreparation::Created,
            &req,
            &store,
        );
        let err = out.expect_err("guard must propagate the original error");
        assert!(
            err.to_string().contains("original boom"),
            "original error must survive: {err}"
        );
        assert!(
            *store.cleaned.borrow(),
            "created target must be cleaned when enabled"
        );
        assert_eq!(
            store.cleaned_path.borrow().as_deref(),
            Some(req.target_root.as_path()),
            "cleanup must receive exactly the target root"
        );
    }

    #[test]
    fn cleanup_guard_preserves_existing_target() {
        let store = overwrite_conflict_store(TargetPreparation::Existing, false);
        let out = cleanup_created_target_on_failure(
            Err(anyhow::anyhow!("boom")),
            TargetPreparation::Existing,
            &cleanup_req(true),
            &store,
        );
        assert!(out.is_err());
        assert!(
            !*store.cleaned.borrow(),
            "pre-existing target must never be cleaned"
        );
    }

    #[test]
    fn cleanup_guard_respects_no_cleanup_flag() {
        let store = overwrite_conflict_store(TargetPreparation::Created, false);
        let out = cleanup_created_target_on_failure(
            Err(anyhow::anyhow!("boom")),
            TargetPreparation::Created,
            &cleanup_req(false),
            &store,
        );
        assert!(out.is_err());
        assert!(
            !*store.cleaned.borrow(),
            "--no-cleanup-on-failure must preserve the target"
        );
    }

    #[test]
    fn cleanup_guard_passes_through_ok_without_cleanup() {
        let store = overwrite_conflict_store(TargetPreparation::Created, false);
        let out = cleanup_created_target_on_failure(
            Ok(()),
            TargetPreparation::Created,
            &cleanup_req(true),
            &store,
        );
        assert!(out.is_ok(), "Ok outcome must pass through");
        assert!(!*store.cleaned.borrow(), "success must not clean up");
    }

    #[test]
    fn cleanup_guard_failure_does_not_mask_original_error() {
        let store = overwrite_conflict_store(TargetPreparation::Created, true);
        let out = cleanup_created_target_on_failure(
            Err(anyhow::anyhow!("original boom")),
            TargetPreparation::Created,
            &cleanup_req(true),
            &store,
        );
        let err = out.expect_err("guard must fail");
        assert!(*store.cleaned.borrow(), "cleanup must have been attempted");
        assert!(
            err.to_string().contains("original boom"),
            "cleanup failure must not mask the original error, got: {err}"
        );
    }

    #[test]
    fn created_target_is_cleaned_up_on_write_io_failure() {
        // Injects a real write_file I/O failure (not an overwrite-refusal proxy). Also verifies
        // cleanup is called with exactly the prepared target root path.
        let store = FakePayloadStore {
            entries: vec![PayloadEntry {
                rel: safe_rel_path("file.txt").unwrap(),
                is_dir: false,
            }],
            contents: HashMap::from([("file.txt".to_string(), b"x".to_vec())]),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: true,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
        };
        let req = cleanup_req(true);
        let result = apply(
            &req,
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(empty_manifest()),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: true,
                },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
        );
        assert!(result.is_err());
        assert!(
            *store.cleaned.borrow(),
            "created target must be cleaned up on write I/O failure"
        );
        assert_eq!(
            store.cleaned_path.borrow().as_deref(),
            Some(req.target_root.as_path()),
            "cleanup must be called with exactly the prepared target root"
        );
    }

    #[test]
    fn created_target_is_cleaned_up_on_dest_status_failure() {
        // A dest_status error is also a post-prepare failure path, so it gets the same policy (cleanup).
        let store = FakePayloadStore {
            entries: vec![PayloadEntry {
                rel: safe_rel_path("file.txt").unwrap(),
                is_dir: false,
            }],
            contents: HashMap::from([("file.txt".to_string(), b"x".to_vec())]),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: true,
            cleanup_fails: false,
            prepare_fails: false,
        };
        let result = apply(
            &cleanup_req(true),
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(empty_manifest()),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: true,
                },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
        );
        assert!(result.is_err());
        assert!(
            *store.cleaned.borrow(),
            "dest_status failure must also trigger cleanup"
        );
    }

    #[test]
    fn created_target_is_cleaned_up_on_before_hook_failure() {
        let manifest = Manifest {
            questions: vec![],
            hooks: crate::domain::hook::Hooks {
                before: vec![crate::domain::hook::Hook {
                    when: None,
                    run: "boom".to_string(),
                }],
                after: vec![],
            },
            ..Default::default()
        };
        let store = FakePayloadStore {
            entries: vec![],
            contents: HashMap::new(),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
        };
        let result = apply(
            &cleanup_req(true),
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: true,
                },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FailingHookRunner,
            },
        );
        assert!(result.is_err());
        assert!(
            *store.cleaned.borrow(),
            "created target must be cleaned up when before-hook fails"
        );
    }

    #[test]
    fn created_target_is_cleaned_up_on_after_hook_failure() {
        // before empty, only after fails → even after writes succeed, an after-hook failure still cleans up.
        let manifest = Manifest {
            questions: vec![],
            hooks: crate::domain::hook::Hooks {
                before: vec![],
                after: vec![crate::domain::hook::Hook {
                    when: None,
                    run: "boom".to_string(),
                }],
            },
            ..Default::default()
        };
        // Actually writes the payload (write succeeds), then fails in the after-hook, proving the
        // target is cleaned up "including the finished output".
        let store = FakePayloadStore {
            entries: vec![PayloadEntry {
                rel: safe_rel_path("file.txt").unwrap(),
                is_dir: false,
            }],
            contents: HashMap::from([("file.txt".to_string(), b"x".to_vec())]),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
        };
        let result = apply(
            &cleanup_req(true),
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: true,
                },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FailingHookRunner,
            },
        );
        assert!(result.is_err());
        assert!(
            !store.written.borrow().is_empty(),
            "write must have completed before after-hook"
        );
        assert!(
            *store.cleaned.borrow(),
            "created target must be cleaned up when after-hook fails"
        );
    }

    #[test]
    fn existing_target_is_preserved_on_failure() {
        let store = overwrite_conflict_store(TargetPreparation::Existing, false);
        let result = run_with_overwrite_conflict(empty_manifest(), &store, &cleanup_req(true));
        assert!(result.is_err());
        assert!(
            !*store.cleaned.borrow(),
            "pre-existing target must never be cleaned up"
        );
    }

    #[test]
    fn no_cleanup_flag_preserves_created_target_on_failure() {
        let store = overwrite_conflict_store(TargetPreparation::Created, false);
        let result = run_with_overwrite_conflict(empty_manifest(), &store, &cleanup_req(false));
        assert!(result.is_err());
        assert!(
            !*store.cleaned.borrow(),
            "--no-cleanup-on-failure must preserve the target"
        );
    }

    #[test]
    fn successful_apply_does_not_clean_up() {
        let mut dest_statuses = HashMap::new();
        dest_statuses.insert(
            "file.txt".to_string(),
            DestStatus {
                final_path: PathBuf::from("/target/file.txt"),
                inside_target: true,
                exists: false,
                is_symlink: false,
            },
        );
        let store = FakePayloadStore {
            entries: vec![PayloadEntry {
                rel: safe_rel_path("file.txt").unwrap(),
                is_dir: false,
            }],
            contents: HashMap::from([("file.txt".to_string(), b"content".to_vec())]),
            dest_statuses: RefCell::new(dest_statuses),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
        };
        let result = apply(
            &cleanup_req(true),
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(empty_manifest()),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: true,
                },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
        );
        assert!(result.is_ok());
        assert!(
            !*store.cleaned.borrow(),
            "successful apply must not clean up"
        );
    }

    #[test]
    fn cleanup_failure_does_not_mask_original_error() {
        let store = overwrite_conflict_store(TargetPreparation::Created, true);
        let result = run_with_overwrite_conflict(empty_manifest(), &store, &cleanup_req(true));
        let err = result.expect_err("apply must fail");
        assert!(*store.cleaned.borrow(), "cleanup must have been attempted");
        // The original (overwrite) error is not replaced by the cleanup-failure error.
        assert!(
            err.to_string().contains("exists"),
            "original error must survive, got: {err}"
        );
    }

    #[test]
    fn pre_prepare_failure_does_not_clean_up() {
        // A missing answer fails before ensure_target (in resolve_answers) → nothing to clean up.
        let manifest = Manifest {
            questions: vec![string_question("project", None)],
            ..Default::default()
        };
        let store = FakePayloadStore {
            entries: vec![],
            contents: HashMap::new(),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: false,
        };
        let result = apply(
            &cleanup_req(true),
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(manifest),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: true,
                },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
        );
        assert!(result.is_err());
        assert!(
            !*store.prepared.borrow(),
            "ensure_target must not be called on pre-prepare failure"
        );
        assert!(
            !*store.cleaned.borrow(),
            "pre-prepare failure must not trigger cleanup"
        );
    }

    #[test]
    fn prepare_failure_itself_does_not_clean_up() {
        // If ensure_target itself fails (permissions, etc.) there is no prepared target, so nothing is cleaned up.
        let store = FakePayloadStore {
            entries: vec![],
            contents: HashMap::new(),
            dest_statuses: RefCell::new(HashMap::new()),
            written: RefCell::new(Vec::new()),
            prep: TargetPreparation::Created,
            cleaned: RefCell::new(false),
            cleaned_path: RefCell::new(None),
            prepared: RefCell::new(false),
            write_fails: false,
            dest_status_fails: false,
            cleanup_fails: false,
            prepare_fails: true,
        };
        let result = apply(
            &cleanup_req(true),
            builtins(),
            ApplyPorts {
                manifest_src: &FakeManifestSource(empty_manifest()),
                data_source: &FakeDataSource,
                renderer: &FakeRenderer,
                payload: &store,
                confirmer: &FakeConfirmer {
                    overwrite: true,
                    external: true,
                },
                answer_source: &FakeAnswerSource::unreachable(),
                condition_evaluator: &FakeConditionEvaluator::always(true),
                ignore_source: &FakeIgnoreSource::none(),
                hook_source: &FakeHookSource,
                hook_runner: &FakeHookRunner::new(),
            },
        );
        assert!(result.is_err());
        assert!(*store.prepared.borrow(), "ensure_target was attempted");
        assert!(
            !*store.cleaned.borrow(),
            "prepare failure must not trigger cleanup"
        );
    }
}
