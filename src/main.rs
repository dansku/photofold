use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use clap::Parser;
use photofold::cli::Cli;
use photofold::progress::Progress;
use photofold::{pipeline, report};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let config = match cli.into_config() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("photofold: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Bars would corrupt the output of a pipe or a log file.
    let progress = Progress::new(std::io::stderr().is_terminal());

    match pipeline::run(&config, &progress) {
        Ok(outcome) => {
            let mut stdout = std::io::stdout().lock();
            let _ = report::print_summary(&mut stdout, &outcome.report, &config);
            let _ = stdout.flush();
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("photofold: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `-v` shows what the program decided; `-vv` shows why.
fn init_logging(verbose: u8) {
    // Third-party crates are noisy at warn level. `nom-exif` logs a warning for
    // every photo without GPS, which is most of them, so it is turned down here
    // rather than being allowed to bury the program's own messages.
    let default = match verbose {
        0 => "warn,nom_exif=error",
        1 => "photofold=info,warn,nom_exif=error",
        _ => "photofold=debug,nom_exif=warn",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}
