//! 루트 `Cli`(clap Parser)와 top-level 디스패치.

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::cli::commands::apply::{self, ApplyArgs};
use crate::cli::commands::template::list::{self, ListArgs};
use crate::cli::commands::template::new::{self, NewArgs};
use crate::cli::commands::template::validate::{self, ValidateArgs};

#[derive(Debug, Parser)]
#[command(name = "scaffolder", version, about = "선언형 프로젝트 스캐폴딩 CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 템플릿을 target에 적용한다.
    Apply(ApplyArgs),
    /// 템플릿 스토어를 다룬다.
    Template(TemplateArgs),
}

#[derive(Debug, Args)]
struct TemplateArgs {
    #[command(subcommand)]
    command: TemplateCommand,
}

#[derive(Debug, Subcommand)]
enum TemplateCommand {
    /// 스토어의 템플릿 목록을 출력한다.
    List(ListArgs),
    /// 스토어에 신규 템플릿 뼈대를 생성한다.
    New(NewArgs),
    /// 템플릿을 정적 검사한다.
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
