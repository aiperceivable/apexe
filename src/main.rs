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
    let config_level = apexe::config::load_config(None)
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

/// Render the `Error: ...` / `Suggestion: ...` lines for a failed run.
///
/// The crate's two domain error types reach here at different points in
/// their journey. `ApexeError` is what the scanner/resolver layer raises,
/// but every call site downstream today converts or stringifies it before
/// `main` ever sees it (`ScanOrchestrator::scan` renders it into a `String`
/// inside `ScanFailure`) — so a bare `ApexeError` is not currently
/// reachable here. `ModuleError` (e.g. from `ApexeConfig::ensure_dirs`)
/// reaches here as itself. Checking for both, and converting `ApexeError`
/// into `ModuleError` before rendering, means this renders correctly
/// regardless of which one a call site propagates, today or in the future,
/// rather than depending on one specific error's current path staying
/// exactly as it is.
///
/// Split out from `report_error` so the rendering logic is testable without
/// capturing the process's actual stderr.
fn render_error(error: anyhow::Error) -> String {
    let module_err = match error.downcast::<ModuleError>() {
        Ok(module_err) => module_err,
        Err(error) => match error.downcast::<ApexeError>() {
            Ok(apexe_err) => apexe_err.into(),
            Err(error) => return format!("Error: {error}"),
        },
    };
    let mut rendered = format!("Error: {}", module_err.message);
    if let Some(ref guidance) = module_err.ai_guidance {
        rendered.push_str(&format!("\nSuggestion: {guidance}"));
    }
    rendered
}

/// Report a failed run to stderr.
fn report_error(error: anyhow::Error) {
    eprintln!("{}", render_error(error));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_report_error_renders_guidance_for_a_module_error() {
        // Regression: report_error's rich-rendering branch only matched
        // downcast::<ApexeError>(), but no production path ever propagates a
        // bare ApexeError to main() -- ApexeConfig::ensure_dirs, for one,
        // already converts to ModuleError before returning. The guidance
        // every ModuleError carries was never shown, and the fallback
        // branch's Display impl leaked the internal error-code tag instead.
        let module_err = ModuleError::new(
            apcore::ErrorCode::GeneralInternalError,
            "failed to create /no/such/dir: Not a directory (os error 20)",
        )
        .with_ai_guidance("Check that the parent path is a directory you can write to.");
        let error: anyhow::Error = module_err.into();

        let rendered = render_error(error);

        assert!(
            rendered.contains("failed to create /no/such/dir"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Suggestion: Check that the parent path"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("GeneralInternalError"),
            "the internal error-code tag must not leak to the user: {rendered}"
        );
    }

    #[test]
    fn test_report_error_converts_an_apexe_error_to_module_error_for_rendering() {
        let error: anyhow::Error = ApexeError::ToolNotFound {
            tool_name: "zzz".to_string(),
        }
        .into();

        let rendered = render_error(error);

        assert!(rendered.starts_with("Error:"), "{rendered}");
        assert!(
            rendered.contains("Suggestion:"),
            "ToolNotFound carries ai_guidance: {rendered}"
        );
    }

    #[test]
    fn test_report_error_falls_back_to_display_for_an_untyped_anyhow_error() {
        let error = anyhow::anyhow!("plain io failure");
        let rendered = render_error(error);
        assert_eq!(rendered, "Error: plain io failure");
    }
}
