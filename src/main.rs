use apcore::ModuleError;
use clap::{ArgMatches, CommandFactory, FromArgMatches};

mod logging;

use apexe::cli::Cli;
use apexe::errors::{ApexeError, CliErrorEnvelope, ErrorFormat};

/// Print apexe's man page, when `--man` is present in raw argv.
///
/// Handled before clap parses, because `--man` is not one of its arguments.
fn print_man_page_if_requested(raw_args: &[String]) -> bool {
    let options: Vec<String> = raw_args
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .cloned()
        .collect();
    if !apcore_cli::has_man_flag(&options) {
        return false;
    }
    let cmd = Cli::command();
    let man = apcore_cli::build_program_man_page(
        &cmd,
        "apexe",
        apexe::VERSION,
        Some(env!("CARGO_PKG_DESCRIPTION")),
        apcore_cli::get_docs_url().as_deref(),
    );
    println!("{man}");
    true
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
fn report_error(error: anyhow::Error, format: ErrorFormat) {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    match format {
        ErrorFormat::Text => {
            let _ = writeln!(stderr, "{}", render_error(error));
        }
        ErrorFormat::Json => {
            let envelope = CliErrorEnvelope::from_error(error);
            let _ = serde_json::to_writer(&mut stderr, &envelope);
            let _ = writeln!(stderr);
        }
    }
}

fn main() {
    let raw: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let format = if apexe::cli::is_shim_invocation(&raw) {
        ErrorFormat::Text
    } else {
        error_format_from_args(&raw)
    };
    let code = match run(format) {
        Ok(code) => code,
        Err(error) => {
            report_error(error, format);
            1
        }
    };
    // The logging worker guard has dropped inside run, flushing queued records.
    std::process::exit(code);
}

fn run(format: ErrorFormat) -> anyhow::Result<i32> {
    apcore_cli::set_docs_url(Some("https://github.com/aiperceivable/apexe".to_string()));

    let raw_os_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if apexe::cli::is_shim_invocation(&raw_os_args) {
        let _logging_guard = logging::init(None, false, false)?;
        return run_with_logging(ErrorFormat::Text, || apexe::cli::run_shim(&raw_os_args));
    }
    let raw_args: Vec<String> = raw_os_args
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    if print_man_page_if_requested(&raw_args) {
        return Ok(0);
    }

    let matches = match Cli::command().try_get_matches() {
        Ok(matches) => matches,
        Err(error) => return report_argument_error(error, format),
    };
    let mut cli = Cli::from_arg_matches(&matches)?;
    let suppress_sdk_calls = match &cli.command {
        apexe::cli::Commands::Serve(args) => args.no_logging || args.no_log_arguments,
        apexe::cli::Commands::A2a(args) => args.no_logging || args.no_log_arguments,
        _ => false,
    };
    let _logging_guard =
        logging::init(cli.log_level.as_deref(), cli.log_stderr, suppress_sdk_calls)?;
    run_with_logging(cli.error_format, || {
        let config = apexe::config::load_config(None)?;
        if matches!(
            cli.command,
            apexe::cli::Commands::Scan(_) | apexe::cli::Commands::Verify(_)
        ) {
            apply_depth_default(&mut cli, &matches, &config)?;
        }
        cli.run_with_config(config)
    })
}

fn error_format_from_args(raw: &[std::ffi::OsString]) -> ErrorFormat {
    let mut arguments = raw.iter().skip(1).take_while(|value| *value != "--");
    let mut format = ErrorFormat::Text;
    while let Some(argument) = arguments.next() {
        let value = if argument == "--error-format" {
            arguments.next().and_then(|value| value.to_str())
        } else {
            argument
                .to_str()
                .and_then(|value| value.strip_prefix("--error-format="))
        };
        match value {
            Some("json") => format = ErrorFormat::Json,
            Some("text") => format = ErrorFormat::Text,
            _ => {}
        }
    }
    format
}

fn report_argument_error(error: clap::Error, format: ErrorFormat) -> anyhow::Result<i32> {
    let code = error.exit_code();
    if !error.use_stderr() || format == ErrorFormat::Text {
        error.print()?;
    } else {
        report_error(
            ModuleError::new(apcore::ErrorCode::GeneralInvalidInput, error.to_string())
                .with_ai_guidance("Run 'apexe <command> --help' to check arguments.")
                .into(),
            format,
        );
    }
    Ok(code)
}

fn apply_depth_default(
    cli: &mut Cli,
    matches: &ArgMatches,
    config: &apexe::config::ApexeConfig,
) -> anyhow::Result<()> {
    let Some((_, arguments)) = matches.subcommand() else {
        return Ok(());
    };
    if arguments.value_source("depth") != Some(clap::parser::ValueSource::DefaultValue) {
        return Ok(());
    }
    anyhow::ensure!(
        (1..=5).contains(&config.scan_depth),
        "Configured scan_depth must be between 1 and 5"
    );
    match &mut cli.command {
        apexe::cli::Commands::Scan(args) => args.depth = config.scan_depth,
        apexe::cli::Commands::Verify(args) => args.depth = config.scan_depth,
        _ => {}
    }
    Ok(())
}

fn run_with_logging(
    format: ErrorFormat,
    command: impl FnOnce() -> anyhow::Result<i32>,
) -> anyhow::Result<i32> {
    match command() {
        Ok(code) => Ok(code),
        Err(error) => {
            tracing::error!(%error, "Command failed");
            report_error(error, format);
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_format_detection_supports_global_positions_and_stops_at_native_args() {
        for (arguments, expected) in [
            (
                vec!["apexe", "--error-format=json", "exec"],
                ErrorFormat::Json,
            ),
            (
                vec!["apexe", "exec", "--error-format", "json"],
                ErrorFormat::Json,
            ),
            (
                vec!["apexe", "exec", "--", "toy", "--error-format", "json"],
                ErrorFormat::Text,
            ),
            (
                vec!["apexe", "--error-format", "invalid"],
                ErrorFormat::Text,
            ),
        ] {
            let raw: Vec<_> = arguments
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect();
            assert_eq!(error_format_from_args(&raw), expected);
        }
    }

    #[test]
    fn test_scan_and_verify_depth_defaults_use_config_and_preserve_explicit_values() {
        let config = apexe::config::ApexeConfig {
            scan_depth: 4,
            ..Default::default()
        };
        for arguments in [vec!["apexe", "scan", "git"], vec!["apexe", "verify"]] {
            for explicit in [false, true] {
                let mut arguments = arguments.clone();
                if explicit {
                    arguments.extend(["--depth", "2"]);
                }
                let matches = Cli::command().try_get_matches_from(arguments).unwrap();
                let mut cli = Cli::from_arg_matches(&matches).unwrap();
                apply_depth_default(&mut cli, &matches, &config).unwrap();
                let depth = match cli.command {
                    apexe::cli::Commands::Scan(args) => args.depth,
                    apexe::cli::Commands::Verify(args) => args.depth,
                    _ => unreachable!(),
                };
                assert_eq!(depth, if explicit { 2 } else { 4 });
            }
        }
    }

    #[test]
    fn test_invalid_depth_config_does_not_override_an_explicit_valid_flag() {
        let config = apexe::config::ApexeConfig {
            scan_depth: 0,
            ..Default::default()
        };
        for arguments in [vec!["apexe", "scan", "git"], vec!["apexe", "verify"]] {
            let matches = Cli::command()
                .try_get_matches_from(arguments.clone())
                .unwrap();
            let mut cli = Cli::from_arg_matches(&matches).unwrap();
            assert!(apply_depth_default(&mut cli, &matches, &config).is_err());
            let matches = Cli::command()
                .try_get_matches_from(arguments.into_iter().chain(["--depth", "2"]))
                .unwrap();
            let mut cli = Cli::from_arg_matches(&matches).unwrap();
            assert!(apply_depth_default(&mut cli, &matches, &config).is_ok());
        }
    }

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
