//! `template validate`.

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;

use crate::app::validate::{
    Finding, FindingKind, ValidatePorts, ValidationReport, validate_template,
};
use crate::domain::store::{SourceRootSource, TemplateCatalog, TemplateStore};
use crate::infra::load::manifest::TomlManifestSource;
use crate::infra::load::partials::FsPartialSource;
use crate::infra::load::source_root::FsSourceRootSource;
use crate::infra::load::store::FsTemplateStore;
use crate::infra::place::FsPayloadStore;
use crate::infra::render::render::MiniJinjaSyntaxChecker;

#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// 검사할 템플릿 이름(0개 이상). 미지정 시 스토어 전체를 검사한다.
    #[arg(value_name = "NAME")]
    pub names: Vec<String>,
    /// 스토어 조회 시 `$SCAFFOLDER_HOME`/`~/.scaffolder`보다 우선하는 디렉토리.
    #[arg(long = "template-dir", value_name = "PATH")]
    pub template_dir: Option<PathBuf>,
}

/// 해석된 검사 대상: 이름 + 템플릿 루트.
struct Target {
    name: String,
    template_root: PathBuf,
}

pub fn run(args: ValidateArgs) -> Result<()> {
    let store = FsTemplateStore::new(args.template_dir);
    let (targets, mut invalid_count) = resolve_targets(&store, &args.names)?;

    if targets.is_empty() && invalid_count == 0 {
        println!("No templates to validate.");
        return Ok(());
    }

    for target in &targets {
        if !validate_one(target) {
            invalid_count += 1;
        }
    }

    if invalid_count > 0 {
        bail!("{invalid_count} template(s) failed validation");
    }
    Ok(())
}

/// `names`가 비면 스토어 전체를 열거해 대상으로 삼는다. `names`가 있으면 각각 개별 `resolve`하되,
/// 실패한 이름은 즉시 출력하고 invalid 카운트에 반영한 뒤(대상 목록에는 넣지 않고) 다른 이름
/// 검사를 계속한다.
fn resolve_targets(store: &FsTemplateStore, names: &[String]) -> Result<(Vec<Target>, usize)> {
    if names.is_empty() {
        let targets = store
            .list()?
            .into_iter()
            .map(|listing| Target {
                name: listing.name,
                template_root: listing.path,
            })
            .collect();
        return Ok((targets, 0));
    }

    let mut targets = Vec::new();
    let mut invalid_count = 0usize;
    for name in names {
        match store.resolve(name) {
            Ok(template_root) => targets.push(Target {
                name: name.clone(),
                template_root,
            }),
            Err(err) => {
                println!("{name}: failed to resolve template: {err}");
                invalid_count += 1;
            }
        }
    }
    Ok((targets, invalid_count))
}

/// 대상 하나를 검사·출력한다. `true`면 유효, `false`면 무효(finding 있음 또는 검사 자체 실패).
fn validate_one(target: &Target) -> bool {
    // trust는 항상 false다 — validate는 `--trust` 플래그를 받지 않는다. 외부 심링크 제어파일은
    // 각 로더 읽기 지점에서 거부되어 finding/에러로 표면화된다(보수적 기본 동작).
    let effective_root = match FsSourceRootSource.resolve(&target.template_root) {
        Ok(root) => root,
        Err(err) => {
            println!("{}: failed to resolve template root: {err}", target.name);
            return false;
        }
    };
    let root_canon = match effective_root.canonicalize() {
        Ok(canon) => canon,
        Err(err) => {
            println!(
                "{}: failed to resolve template root {}: {err}",
                target.name,
                effective_root.display()
            );
            return false;
        }
    };

    let manifest_src = TomlManifestSource {
        root_canon: root_canon.clone(),
        trust: false,
    };
    let partial_source = FsPartialSource {
        root_canon: root_canon.clone(),
        trust: false,
    };
    let payload = FsPayloadStore;
    let syntax = MiniJinjaSyntaxChecker::new();

    let report = match validate_template(
        &effective_root,
        ValidatePorts {
            manifest_src: &manifest_src,
            partial_source: &partial_source,
            payload: &payload,
            syntax: &syntax,
        },
    ) {
        Ok(report) => report,
        Err(err) => {
            println!("{}: validation failed: {err}", target.name);
            return false;
        }
    };

    println!("{}", format_report(&target.name, &report));
    report.is_valid()
}

/// 템플릿별 그룹 리포트: 유효면 `"<name>: OK"`, 무효면 헤더 한 줄 + finding마다 오류류
/// 라벨을 붙인 한 줄.
fn format_report(name: &str, report: &ValidationReport) -> String {
    if report.is_valid() {
        return format!("{name}: OK");
    }

    let mut lines = vec![format!("{name}: {} issue(s)", report.findings.len())];
    lines.extend(report.findings.iter().map(finding_line));
    lines.join("\n")
}

fn finding_line(finding: &Finding) -> String {
    format!("  - [{}] {}", kind_label(finding.kind), finding.message)
}

fn kind_label(kind: FindingKind) -> &'static str {
    match kind {
        FindingKind::Manifest => "manifest",
        FindingKind::FileName => "filename",
        FindingKind::TemplateSyntax => "template-syntax",
        FindingKind::WhenSyntax => "when-syntax",
        FindingKind::SourceConflict => "source-conflict",
        FindingKind::PartialReference => "partial-reference",
    }
}
