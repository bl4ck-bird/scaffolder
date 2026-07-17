//! 바이너리 진입점.

fn main() -> std::process::ExitCode {
    if let Err(err) = scaffolder::cli::command::run() {
        eprintln!("error: {err:#}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
