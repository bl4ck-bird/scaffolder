//! The `template validate` command.

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
    #[arg(
        value_name = "NAME",
        help = "Templates to check (zero or more). When omitted, the whole store is checked."
    )]
    pub names: Vec<String>,
    #[arg(
        long = "template-dir",
        value_name = "PATH",
        help = "Directory searched before $SCAFFOLDER_HOME/~/.scaffolder when resolving a store name."
    )]
    pub template_dir: Option<PathBuf>,
}

/// A resolved check target: name plus template root.
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

/// When `names` is empty, enumerate the whole store as the target set. When `names`
/// is given, `resolve` each one individually; a name that fails to resolve is printed
/// immediately and counted as invalid (not added to the target list), and the remaining
/// names are still checked.
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

/// Checks and prints one target. `true` means valid, `false` means invalid (findings
/// present, or the check itself failed).
fn validate_one(target: &Target) -> bool {
    // trust is always false — validate takes no `--trust` flag. External-symlink control
    // files are refused at each loader's read point and surface as findings/errors
    // (the conservative default behavior).
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

/// Per-template grouped report: `"<name>: OK"` when valid, otherwise a header line plus
/// one line per finding, each tagged with its finding-kind label.
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
