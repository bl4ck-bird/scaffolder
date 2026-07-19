//! `template new`(심플/full).

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::domain::skeleton::skeleton;
use crate::domain::store::{TemplateInitializer, validate_template_name};
use crate::infra::load::store::FsTemplateStore;

#[derive(Debug, Args)]
pub struct NewArgs {
    /// 새 템플릿 이름(스토어 내 단일 경로 컴포넌트).
    pub name: String,
    /// partials/data/hooks 샘플까지 포함한 전체 뼈대를 생성한다.
    #[arg(long)]
    pub full: bool,
    /// 생성 대상 스토어. `$SCAFFOLDER_HOME`/`~/.scaffolder`보다 우선한다.
    #[arg(long = "template-dir", value_name = "PATH")]
    pub template_dir: Option<PathBuf>,
}

pub fn run(args: NewArgs) -> Result<()> {
    validate_template_name(&args.name)?;
    let entries = skeleton(args.full);
    let initializer = FsTemplateStore::new(args.template_dir);
    let created = initializer.create(&args.name, &entries)?;
    println!("Created template at {}", created.display());
    Ok(())
}
