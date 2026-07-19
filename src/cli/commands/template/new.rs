//! The `template new` command (simple / full).

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::domain::skeleton::skeleton;
use crate::domain::store::{TemplateInitializer, validate_template_name};
use crate::infra::load::store::FsTemplateStore;

#[derive(Debug, Args)]
pub struct NewArgs {
    #[arg(help = "Name of the new template (a single path component in the store).")]
    pub name: String,
    #[arg(
        long,
        help = "Create the full skeleton, including partials/data/hooks samples."
    )]
    pub full: bool,
    #[arg(
        long = "template-dir",
        value_name = "PATH",
        help = "Store to create the template in; takes priority over $SCAFFOLDER_HOME/~/.scaffolder."
    )]
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
