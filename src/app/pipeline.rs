//! apply 라이프사이클 조립(BLUEPRINT §1.9 최소): 매니페스트 파싱 → answer 확정 → plan(부작용
//! 없음) → dry-run이면 종료 → write(overwrite/외부쓰기 confirm 반영). `.scaffoldroot`·ignore·
//! partials·data·hook은 이후 슬라이스에서 확장한다. 도메인 포트만 사용한다.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::domain::answer::{build_context, coerce_string, AnswerValue, ScaffolderBuiltins};
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
) -> Result<ApplyReport> {
    let manifest_path = req.template_root.join("scaffold.toml");
    let manifest = manifest_src.load(&manifest_path)?;

    let answers = resolve_answers(&manifest.questions, &req.answers)?;
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

        // §1.10: target 밖으로 이탈하는 쓰기는 confirm하고, 미승인이면 그 엔트리만 건너뛰고
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

/// `--answers` > default 순으로 확정한다(§1.9-2 축소판; 프롬프트는 M2+). S1은
/// `QuestionType::String`만 coerce하고, 다른 타입은 default를 그대로 통과시킨다.
/// `req.answers`의 미매칭 키는 경고만 하고 계속 진행한다(§1.2 unknown=경고).
fn resolve_answers(
    questions: &[Question],
    raw_answers: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, AnswerValue>> {
    let mut resolved = BTreeMap::new();

    for question in questions {
        let value = match raw_answers.get(&question.name) {
            Some(raw) => coerce_string(question.qtype, raw)?,
            None => match &question.default {
                Some(default) => default.clone(),
                None => bail!("missing answer for '{}'", question.name),
            },
        };
        resolved.insert(question.name.clone(), value);
    }

    let known: std::collections::HashSet<&str> =
        questions.iter().map(|q| q.name.as_str()).collect();
    for key in raw_answers.keys() {
        if !known.contains(key.as_str()) {
            eprintln!("warning: '--answers {key}=...' does not match any question; ignoring");
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
            dry_run: true,
        };

        let report = apply(
            &req,
            builtins(),
            &FakeManifestSource(manifest),
            &FakeRenderer,
            &store,
            &FakeConfirmer { overwrite: true, external: true },
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
            dry_run: true,
        };

        let result = apply(
            &req,
            builtins(),
            &FakeManifestSource(manifest),
            &FakeRenderer,
            &store,
            &FakeConfirmer { overwrite: true, external: true },
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
            dry_run: true,
        };

        let result = apply(
            &req,
            builtins(),
            &FakeManifestSource(manifest),
            &FakeRenderer,
            &store,
            &FakeConfirmer { overwrite: true, external: true },
        );

        assert!(result.is_err());
    }

    #[test]
    fn external_write_without_confirmation_is_skipped_not_written() {
        // rel 문자열은 `safe_rel_path`가 literal '..'을 이미 거부하므로 항상 정상 형태다;
        // containment 이탈은 상위 심링크 등 최종 경로 해석 단계에서만 드러난다(§1.10).
        // 미승인 외부쓰기는 그 엔트리만 스킵하고 apply는 성공한다(§1.10 "아니면 스킵").
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
            dry_run: false,
        };

        let result = apply(
            &req,
            builtins(),
            &FakeManifestSource(manifest),
            &FakeRenderer,
            &store,
            &FakeConfirmer { overwrite: true, external: false },
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
            dry_run: false,
        };

        let result = apply(
            &req,
            builtins(),
            &FakeManifestSource(manifest),
            &FakeRenderer,
            &store,
            &FakeConfirmer { overwrite: false, external: true },
        );

        assert!(result.is_err());
        assert!(store.written.borrow().is_empty());
    }
}
