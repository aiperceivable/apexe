use std::path::Path;

use tracing_appender::{non_blocking::WorkerGuard, rolling::Rotation};
use tracing_subscriber::{fmt::writer::MakeWriterExt, EnvFilter};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Configure the same diagnostic pipeline for commands and governed shims.
pub(super) fn init(
    explicit_level: Option<&str>,
    stderr: bool,
    suppress_sdk_calls: bool,
) -> anyhow::Result<WorkerGuard> {
    let config = apexe::config::load_config(None).unwrap_or_default();
    let filter = resolve_filter(
        explicit_level,
        std::env::var("RUST_LOG").ok().as_deref(),
        &config.log_level,
    )?;
    let directory = apexe::config::ApexeConfig::default().config_dir.join("log");
    let (writer, guard) = file_writer(&directory)?;
    let writer = writer.and(std::io::stderr.with_filter(move |_| stderr));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .finish()
        .with(tracing_subscriber::filter::filter_fn(move |metadata| {
            allow_diagnostic(metadata, suppress_sdk_calls)
        }))
        .try_init()
        .map_err(|error| anyhow::anyhow!("Failed to initialize logging: {error}"))?;
    Ok(guard)
}

fn allow_diagnostic(metadata: &tracing::Metadata<'_>, suppress_sdk_calls: bool) -> bool {
    // The SDK router logs inputs at DEBUG and executor errors at ERROR. Those
    // diagnostics are independent of the host's execution logging middleware.
    // Filter the source, not message text, so explicit RUST_LOG directives
    // cannot re-enable call payloads while the user requests suppression.
    !suppress_sdk_calls
        || !(metadata.target() == "apcore_mcp::server::router"
            || metadata
                .target()
                .starts_with("apcore_mcp::server::router::"))
}

fn resolve_filter(
    explicit_level: Option<&str>,
    environment: Option<&str>,
    config_level: &str,
) -> anyhow::Result<EnvFilter> {
    let level = explicit_level.or(environment).unwrap_or(config_level);
    EnvFilter::try_new(level)
        .map_err(|error| anyhow::anyhow!("Invalid log filter '{level}': {error}"))
}

fn file_writer(
    directory: &Path,
) -> anyhow::Result<(tracing_appender::non_blocking::NonBlocking, WorkerGuard)> {
    // Retention scans the directory before tracing-appender creates its first file.
    std::fs::create_dir_all(directory).map_err(|error| {
        anyhow::anyhow!(
            "Failed to create log directory {}: {error}",
            directory.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Diagnostics may contain tool inputs when server argument logging is
        // enabled. Protect existing and future rotated files via their directory.
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("apexe.log")
        .max_log_files(14)
        .build(directory)
        .map_err(|error| {
            anyhow::anyhow!(
                "Failed to open log directory {}: {error}",
                directory.display()
            )
        })?;
    Ok(tracing_appender::non_blocking(appender))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(directory: &Path, filter: EnvFilter) {
        let (writer, guard) = file_writer(directory).unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(writer)
            .with_env_filter(filter)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("info diagnostic");
            tracing::warn!("warning diagnostic");
            tracing::error!("error diagnostic");
        });
        drop(guard);
    }

    fn read_log(directory: &Path) -> String {
        let entry = std::fs::read_dir(directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        std::fs::read_to_string(entry.path()).unwrap()
    }

    #[test]
    fn test_file_logging_filters_and_flushes_on_exit() {
        let directory = tempfile::tempdir().unwrap();
        let directory = directory.path().join("new-log-directory");
        capture(&directory, resolve_filter(None, None, "warn").unwrap());
        let log = read_log(&directory);
        assert!(!log.contains("info diagnostic"));
        assert!(log.contains("warning diagnostic"));
        assert!(log.contains("error diagnostic"));
        assert!(!log.contains('\u{1b}'));
    }

    #[test]
    fn test_file_logging_appends_across_invocations() {
        let directory = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            capture(
                directory.path(),
                resolve_filter(None, None, "info").unwrap(),
            );
        }
        assert_eq!(
            read_log(directory.path())
                .matches("info diagnostic")
                .count(),
            2
        );
    }

    #[test]
    fn test_logging_cli_overrides_environment_and_config() {
        let filter = resolve_filter(Some("error"), Some("trace"), "debug").unwrap();
        assert_eq!(filter.to_string(), "error");
        let filter = resolve_filter(None, Some("apexe=debug"), "warn").unwrap();
        assert_eq!(filter.to_string(), "apexe=debug");
        assert!(resolve_filter(None, Some("apexe[invalid"), "warn").is_err());
    }

    #[test]
    fn test_file_logging_reports_unwritable_path() {
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(file_writer(&file.path().join("log")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_file_logging_makes_existing_directory_private() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        capture(
            directory.path(),
            resolve_filter(None, None, "warn").unwrap(),
        );
        assert_eq!(
            std::fs::metadata(directory.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(read_log(directory.path()).contains("warning diagnostic"));
    }
}
