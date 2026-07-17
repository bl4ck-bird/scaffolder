//! 루트 `Cli`(clap Parser)와 top-level 디스패치.

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::cli::commands::apply::{self, ApplyArgs};

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
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Apply(args) => apply::run(args),
    }
}
