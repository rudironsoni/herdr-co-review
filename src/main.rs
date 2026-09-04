use clap::Parser;

use co_review::cli::Cli;

/// Internal entry point, typed by `start` into the agent pane:
/// `co-review __launch-agent`. Deliberately NOT a clap subcommand, so it can
/// never leak into `--help`, shell completions, or the man page, and it takes
/// exactly zero arguments (session identity comes from `$CO_REVIEW_SESSION`
/// only — extra arguments are an error, never silently ignored).
const LAUNCH_AGENT: &str = "__launch-agent";

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some(LAUNCH_AGENT) {
        if let Some(extra) = argv.get(1) {
            eprintln!("co-review: error: {LAUNCH_AGENT} takes no arguments (got '{extra}')");
            return std::process::ExitCode::FAILURE;
        }
        return exit(co_review::commands::launch_agent());
    }
    let cli = Cli::parse();
    exit(co_review::run(cli))
}

fn exit(result: anyhow::Result<()>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // anyhow's Display chains the causes with `: `.
            eprintln!("co-review: error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
