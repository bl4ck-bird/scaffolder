//! `apply` 실행.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Args;

use crate::app::pipeline::{apply, ApplyRequest};
use crate::app::report::format_plan;
use crate::cli::confirm::StdConfirmer;
use crate::domain::answer::ScaffolderBuiltins;
use crate::infra::load::manifest::TomlManifestSource;
use crate::infra::place::FsPayloadStore;
use crate::infra::render::render::MiniJinjaRenderer;

#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// 템플릿 로컬 경로(스토어 조회는 이후 지원 예정).
    pub template: String,
    /// 새로 생성하거나 채울 대상 경로(`.` 허용).
    pub target: String,
    /// `scaffolder.name` 빌트인(기본: target basename).
    #[arg(long)]
    pub name: Option<String>,
    /// `k=v` 답변, 반복 가능.
    #[arg(long = "answers", value_name = "K=V")]
    pub answers: Vec<String>,
    /// 기존 dest를 자동으로 덮어쓴다.
    #[arg(long)]
    pub force: bool,
    /// plan만 출력하고 쓰지 않는다.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

pub fn run(args: ApplyArgs) -> Result<()> {
    let template_root = PathBuf::from(&args.template);
    let target_root = std::path::absolute(PathBuf::from(&args.target))
        .with_context(|| format!("failed to resolve target path {:?}", args.target))?;

    let answers = parse_answers(&args.answers)?;

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
        dry_run: args.dry_run,
    };

    if !args.dry_run && args.target != "." {
        fs::create_dir_all(&target_root)
            .with_context(|| format!("failed to create target directory {}", target_root.display()))?;
    }

    let manifest_src = TomlManifestSource;
    let renderer = MiniJinjaRenderer::new();
    let payload = FsPayloadStore;
    let confirmer = StdConfirmer::new(args.force);

    let report = apply(&req, builtins, &manifest_src, &renderer, &payload, &confirmer)?;

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
        let parsed = parse_answers(&["project=demo".to_string(), "license=MIT".to_string()]).unwrap();
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
