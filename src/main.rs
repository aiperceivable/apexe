use apcore::ModuleError;
use clap::{CommandFactory, Parser};
use tracing_subscriber::EnvFilter;

use apexe::cli::Cli;
use apexe::errors::ApexeError;

/// Print apexe's man page, when `--man` is present in raw argv.
///
/// Handled before clap parses, because `--man` is not one of its arguments.
fn print_man_page_if_requested(raw_args: &[String]) -> bool {
    if !apcore_cli::has_man_flag(raw_args) {
        return false;
    }
    let cmd = Cli::command();
    let man = apcore_cli::build_program_man_page(
        &cmd,
        "apexe",
        apexe::VERSION,
        Some("Outside-In CLI-to-Agent Bridge"),
        apcore_cli::get_docs_url().as_deref(),
    );
    println!("{man}");
    true
}

/// Start the tracing subscriber.
///
/// Precedence: `RUST_LOG` > `--log-level` (explicit) > `config.log_level` >
/// "info". Config is resolved here rather than in `Cli::run` so a level set in
/// `config.yaml` governs the startup path too.
///
/// Logs go to stderr so stdout stays a clean machine-readable channel:
/// `scan --format json` is consumed by other processes (e.g. AP Studio's
/// importer does `JSON.parse(stdout)`), and interleaved log lines would make
/// every such parse fail.
fn init_logging(cli: &Cli) {
    let config_level = apexe::config::load_config(None, None)
        .map(|c| c.log_level)
        .unwrap_or_else(|_| "info".to_string());
    let fallback_level = cli.effective_log_level(&config_level);
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&fallback_level)),
        )
        .init();
}

/// Report a failed run, using the richer `ModuleError` rendering when the cause
/// is an [`ApexeError`] — that is where the actionable suggestion lives.
fn report_error(error: anyhow::Error) {
    match error.downcast::<ApexeError>() {
        Ok(apexe_err) => {
            let module_err: ModuleError = apexe_err.into();
            eprintln!("Error: {}", module_err.message);
            if let Some(ref guidance) = module_err.ai_guidance {
                eprintln!("Suggestion: {guidance}");
            }
        }
        Err(error) => eprintln!("Error: {error}"),
    }
}

fn main() {
    apcore_cli::set_docs_url(Some("https://github.com/aiperceivable/apexe".to_string()));

    let raw_args: Vec<String> = std::env::args().collect();
    if print_man_page_if_requested(&raw_args) {
        return;
    }

    let cli = Cli::parse();
    init_logging(&cli);

    if let Err(e) = cli.run() {
        report_error(e);
        std::process::exit(1);
    }
}
