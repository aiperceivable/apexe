use apcore::ModuleError;
use clap::{CommandFactory, Parser};
use tracing_subscriber::EnvFilter;

use apexe::cli::Cli;
use apexe::errors::ApexeError;

fn main() {
    // Set documentation URL for help and man page output
    apcore_cli::set_docs_url(Some("https://github.com/aiperceivable/apexe".to_string()));

    // Handle --man before clap parsing (raw argv inspection)
    let raw_args: Vec<String> = std::env::args().collect();
    if apcore_cli::has_man_flag(&raw_args) {
        let cmd = Cli::command();
        let man = apcore_cli::build_program_man_page(
            &cmd,
            "apexe",
            apexe::VERSION,
            Some("Outside-In CLI-to-Agent Bridge"),
            apcore_cli::get_docs_url().as_deref(),
        );
        println!("{man}");
        return;
    }

    let cli = Cli::parse();

    // Resolve config up front so APEXE_LOG_LEVEL / config.yaml can drive logging.
    // Precedence: RUST_LOG > --log-level (explicit) > config.log_level > "info".
    let config_level = apexe::config::load_config(None, None)
        .map(|c| c.log_level)
        .unwrap_or_else(|_| "info".to_string());
    let fallback_level = cli.effective_log_level(&config_level);

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&fallback_level)),
        )
        .init();

    if let Err(e) = cli.run() {
        // Try to downcast to ApexeError and convert to ModuleError for rich display
        match e.downcast::<ApexeError>() {
            Ok(apexe_err) => {
                let module_err: ModuleError = apexe_err.into();
                eprintln!("Error: {}", module_err.message);
                if let Some(ref guidance) = module_err.ai_guidance {
                    eprintln!("Suggestion: {guidance}");
                }
            }
            Err(e) => {
                eprintln!("Error: {e}");
            }
        }
        std::process::exit(1);
    }
}
