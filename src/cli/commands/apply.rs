//! The `apply` command.

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
    #[arg(help = "Template store name or local path.")]
    pub template: String,
    #[arg(help = "Target path to create or fill (\".\" is allowed).")]
    pub target: String,
    #[arg(
        long = "template-dir",
        value_name = "PATH",
        help = "Directory searched before $SCAFFOLDER_HOME/~/.scaffolder when resolving a store name."
    )]
    pub template_dir: Option<PathBuf>,
    #[arg(
        long,
        help = "Value for the scaffolder.name builtin (default: target basename)."
    )]
    pub name: Option<String>,
    #[arg(
        long = "answers",
        value_name = "K=V",
        help = "Answer as k=v; repeatable. A matching key overrides --answers-file."
    )]
    pub answers: Vec<String>,
    #[arg(
        long = "answers-file",
        value_name = "PATH",
        help = "TOML file of answers."
    )]
    pub answers_file: Option<PathBuf>,
    #[arg(
        long,
        help = "Use each question's default without prompting (error if a question has no default)."
    )]
    pub defaults: bool,
    #[arg(long, help = "Overwrite an existing destination without prompting.")]
    pub force: bool,
    #[arg(long, help = "Skip the hook confirmation prompt.")]
    pub yes: bool,
    #[arg(
        long,
        help = "Allow reading control files reached by a symlink that points outside the source root (refused by default)."
    )]
    pub trust: bool,
    #[arg(long = "dry-run", help = "Print the plan without writing anything.")]
    pub dry_run: bool,
    #[arg(
        long = "no-cleanup-on-failure",
        help = "Keep a newly created target when apply fails partway (default: it is removed); a pre-existing target is always preserved. Cleanup is best-effort and does not cover signals, forced termination, or power loss."
    )]
    pub no_cleanup_on_failure: bool,
}

pub fn run(args: ApplyArgs) -> Result<()> {
    let store = FsTemplateStore::new(args.template_dir.clone());
    let template_root = store.resolve(&args.template)?;
    // `.scaffoldroot` can itself be a symlink. If it points outside the template,
    // `FsSourceRootSource::resolve` below would read its contents — which choose the effective
    // source root — before the `--trust` gate is even wired up. So check it here first, against
    // the original template_root, before anything reads it.
    let scaffoldroot_marker = template_root.join(".scaffoldroot");
    if scaffoldroot_marker.symlink_metadata().is_ok() {
        let template_root_canon = template_root
            .canonicalize()
            .with_context(|| format!("template root {} does not exist", template_root.display()))?;
        ensure_within_root(&scaffoldroot_marker, &template_root_canon, args.trust)?;
    }
    // Resolve the effective source root via `.scaffoldroot`. All later loading
    // (manifest, files, partials, data, ignore) is relative to the effective root.
    let template_root = FsSourceRootSource.resolve(&template_root)?;
    // Every later loader's external-symlink guard is relative to this effective root.
    let root_canon = template_root.canonicalize().with_context(|| {
        format!(
            "failed to resolve template root {}",
            template_root.display()
        )
    })?;
    let trust = args.trust;
    // Settle the effective target path once, here at the composition root. `std::path::absolute`
    // keeps any `..` in the path (it only makes the path absolute, without resolving it), so
    // normalize it right away and hand the same settled path to every later step — the hook cwd,
    // dest_status, write_file, ensure_target, and cleanup_target. If we normalized in some places
    // but not others, a `..` target would be seen as one path while preparing and a different one
    // while writing or running hooks, and apply would fail on a target that is actually fine.
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
    // Loading partials and building the renderer can fail, so do it before creating
    // the target — a failure must not leave an empty target behind.
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
