use apcore::ModuleError;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use apexe::cli::Cli;
use apexe::errors::ApexeError;

fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
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
