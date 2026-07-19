//! `apply` 실행.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::app::pipeline::{ApplyPorts, ApplyRequest, apply};
use crate::app::report::format_plan;
use crate::cli::confirm::StdConfirmer;
use crate::cli::prompt::InquireAnswerSource;
use crate::domain::answer::ScaffolderBuiltins;
use crate::domain::place::normalize_target;
use crate::domain::render::PartialSource;
use crate::domain::store::{SourceRootSource, TemplateStore};
use crate::infra::hook::{FsHookSource, StdHookRunner};
use crate::infra::load::answers::load_answers_file;
use crate::infra::load::data::FsDataSource;
use crate::infra::load::ignore::FsIgnoreSource;
use crate::infra::load::manifest::TomlManifestSource;
use crate::infra::load::partials::FsPartialSource;
use crate::infra::load::source_root::FsSourceRootSource;
use crate::infra::load::store::FsTemplateStore;
use crate::infra::load::trust::ensure_within_root;
use crate::infra::place::FsPayloadStore;
use crate::infra::render::expr::MiniJinjaConditionEvaluator;
use crate::infra::render::render::MiniJinjaRenderer;

#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// 템플릿 스토어명 또는 로컬 경로.
    pub template: String,
    /// 새로 생성하거나 채울 대상 경로(`.` 허용).
    pub target: String,
    /// 스토어 조회 시 `$SCAFFOLDER_HOME`/`~/.scaffolder`보다 우선하는 디렉토리.
    #[arg(long = "template-dir", value_name = "PATH")]
    pub template_dir: Option<PathBuf>,
    /// `scaffolder.name` 빌트인(기본: target basename).
    #[arg(long)]
    pub name: Option<String>,
    /// `k=v` 답변, 반복 가능. 동일 키는 `--answers-file`보다 우선한다.
    #[arg(long = "answers", value_name = "K=V")]
    pub answers: Vec<String>,
    /// 답변을 담은 TOML 파일 경로.
    #[arg(long = "answers-file", value_name = "PATH")]
    pub answers_file: Option<PathBuf>,
    /// 미답변 질문에 프롬프트하지 않고 default만 쓴다(default 없으면 에러).
    #[arg(long)]
    pub defaults: bool,
    /// 기존 dest를 자동으로 덮어쓴다.
    #[arg(long)]
    pub force: bool,
    /// 훅 confirm을 생략한다.
    #[arg(long)]
    pub yes: bool,
    /// 외부(source root 밖) 심링크 제어파일 읽기를 허용한다(기본 거부).
    #[arg(long)]
    pub trust: bool,
    /// plan만 출력하고 쓰지 않는다.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// 실패 시 새로 만든 target 정리를 끈다(기본: 정리함). 켜져 있으면 apply가 새로 만든 target을
    /// 만든 뒤 실패했을 때 그 target을 지운다(사전 존재 target은 항상 보존). 정리는 best-effort로,
    /// 정상 종료 실패에만 적용되며 signal·강제 종료·전원 장애에는 보장되지 않는다.
    #[arg(long = "no-cleanup-on-failure")]
    pub no_cleanup_on_failure: bool,
}

pub fn run(args: ApplyArgs) -> Result<()> {
    let store = FsTemplateStore::new(args.template_dir.clone());
    let template_root = store.resolve(&args.template)?;
    // `.scaffoldroot` 자체가 외부 심링크면 `FsSourceRootSource::resolve`가 `--trust` 배선 전에
    // 그 내용(실효 소스 루트 선택)을 읽어버린다 — 원본 template_root 기준으로 여기서 먼저 가드한다.
    let scaffoldroot_marker = template_root.join(".scaffoldroot");
    if scaffoldroot_marker.symlink_metadata().is_ok() {
        let template_root_canon = template_root
            .canonicalize()
            .with_context(|| format!("template root {} does not exist", template_root.display()))?;
        ensure_within_root(&scaffoldroot_marker, &template_root_canon, args.trust)?;
    }
    // `.scaffoldroot`으로 실효 소스 루트를 해석한다. 이후 모든 로딩(manifest·files·
    // partials·data·ignore)은 실효 루트를 기준으로 한다.
    let template_root = FsSourceRootSource.resolve(&template_root)?;
    // 이후 모든 로더 읽기 지점의 외부 심링크 가드는 이 실효 루트 기준이다.
    let root_canon = template_root.canonicalize().with_context(|| {
        format!(
            "failed to resolve template root {}",
            template_root.display()
        )
    })?;
    let trust = args.trust;
    // 실효 target을 합성 루트에서 한 번 확정한다: `std::path::absolute`는 `..`를 lexical로 보존하므로,
    // 곧바로 정규화해 이후 모든 소비자(훅 cwd·dest_status·write_file·ensure_target·cleanup_target)가
    // 동일한 경로를 쓰게 한다. 부분 정규화는 `..` target에서 prepare(정규화)와 write/hook(raw)이 갈려
    // 유효한 target에 apply가 실패하는 원인이 된다.
    let target_root = std::path::absolute(PathBuf::from(&args.target))
        .with_context(|| format!("failed to resolve target path {:?}", args.target))?;
    let target_root = normalize_target(&target_root);

    let answers = parse_answers(&args.answers)?;
    let answers_file = match &args.answers_file {
        Some(path) => load_answers_file(path)?,
        None => BTreeMap::new(),
    };
    let interactive = std::io::stdin().is_terminal();

    let name = args.name.clone().unwrap_or_else(|| {
        target_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    let builtins = ScaffolderBuiltins {
        name,
        target: target_root.clone(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        username: std::env::var("USER").unwrap_or_default(),
    };

    let req = ApplyRequest {
        template_root,
        target_root: target_root.clone(),
        answers,
        answers_file,
        defaults_only: args.defaults,
        interactive,
        dry_run: args.dry_run,
        cleanup_on_failure: !args.no_cleanup_on_failure,
    };

    let manifest_src = TomlManifestSource {
        root_canon: root_canon.clone(),
        trust,
    };
    let data_source = FsDataSource {
        root_canon: root_canon.clone(),
        trust,
    };
    // partial 로드·렌더러 구성은 실패할 수 있으므로 target 생성 전에 수행한다 — 실패 시 빈
    // target을 남기지 않는다.
    let partials = FsPartialSource {
        root_canon: root_canon.clone(),
        trust,
    }
    .load(&req.template_root)?;
    let renderer = MiniJinjaRenderer::with_partials(partials)?;
    let payload = FsPayloadStore;
    let confirmer = StdConfirmer::new(args.force, args.yes);
    let answer_source = InquireAnswerSource;
    let condition_evaluator = MiniJinjaConditionEvaluator::new();
    let ignore_source = FsIgnoreSource::new(&renderer, root_canon.clone(), trust);
    let hook_source = FsHookSource {
        root_canon: root_canon.clone(),
        trust,
    };
    let hook_runner = StdHookRunner;

    let report = apply(
        &req,
        builtins,
        ApplyPorts {
            manifest_src: &manifest_src,
            data_source: &data_source,
            renderer: &renderer,
            payload: &payload,
            confirmer: &confirmer,
            answer_source: &answer_source,
            condition_evaluator: &condition_evaluator,
            ignore_source: &ignore_source,
            hook_source: &hook_source,
            hook_runner: &hook_runner,
        },
    )?;

    if args.dry_run {
        println!("{}", format_plan(&report));
    }

    Ok(())
}

fn parse_answers(raw: &[String]) -> Result<BTreeMap<String, String>> {
    let mut answers = BTreeMap::new();
    for entry in raw {
        let Some((key, value)) = entry.split_once('=') else {
            bail!("invalid --answers entry {entry:?}: expected 'key=value'");
        };
        answers.insert(key.to_string(), value.to_string());
    }
    Ok(answers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_answers_splits_key_value_pairs() {
        let parsed =
            parse_answers(&["project=demo".to_string(), "license=MIT".to_string()]).unwrap();
        assert_eq!(parsed.get("project"), Some(&"demo".to_string()));
        assert_eq!(parsed.get("license"), Some(&"MIT".to_string()));
    }

    #[test]
    fn parse_answers_rejects_missing_equals() {
        assert!(parse_answers(&["noequals".to_string()]).is_err());
    }

    #[test]
    fn parse_answers_last_duplicate_wins() {
        let parsed = parse_answers(&["a=1".to_string(), "a=2".to_string()]).unwrap();
        assert_eq!(parsed.get("a"), Some(&"2".to_string()));
    }
}
