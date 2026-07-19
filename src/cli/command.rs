//! Root `Cli` (clap `Parser`) and top-level dispatch.

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::cli::commands::apply::{self, ApplyArgs};
use crate::cli::commands::template::list::{self, ListArgs};
use crate::cli::commands::template::new::{self, NewArgs};
use crate::cli::commands::template::validate::{self, ValidateArgs};

#[derive(Debug, Parser)]
#[command(
    name = "scaffolder",
    version,
    about = "Scaffold new projects from declarative templates."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Apply a template to a target directory.")]
    Apply(ApplyArgs),
    #[command(about = "Manage the template store.")]
    Template(TemplateArgs),
}

#[derive(Debug, Args)]
struct TemplateArgs {
    #[command(subcommand)]
    command: TemplateCommand,
}

#[derive(Debug, Subcommand)]
enum TemplateCommand {
    #[command(about = "List the templates in the store.")]
    List(ListArgs),
    #[command(about = "Create a new template skeleton in the store.")]
    New(NewArgs),
    #[command(about = "Statically check templates.")]
    Validate(ValidateArgs),
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Apply(args) => apply::run(args),
        Command::Template(args) => match args.command {
            TemplateCommand::List(args) => list::run(args),
            TemplateCommand::New(args) => new::run(args),
            TemplateCommand::Validate(args) => validate::run(args),
        },
    }
}
