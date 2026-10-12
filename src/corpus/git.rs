use super::CorpusError;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

const OUTPUT_LIMIT: u64 = 1024 * 1024;

struct GitRunner {
    executable: PathBuf,
    search_path: std::ffi::OsString,
    empty_config: tempfile::NamedTempFile,
    empty_hooks: tempfile::TempDir,
}

impl GitRunner {
    fn new() -> Result<Self, CorpusError> {
        let search_path = crate::runtime_path::search_path();
        let executable = which::which_in("git", Some(&search_path), std::env::current_dir()?)
            .map_err(|_| CorpusError::Git("Git is not installed on PATH".into()))?;
        Ok(Self {
            executable,
            search_path,
            empty_config: tempfile::NamedTempFile::new()?,
            empty_hooks: tempfile::tempdir()?,
        })
    }

    fn command(&self, arguments: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.executable);
        for (name, _) in
            std::env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("GIT_"))
        {
            command.env_remove(name);
        }
        command
            .args([
                "-c",
                "protocol.file.allow=always",
                "-c",
                "core.fsmonitor=false",
                "-c",
            ])
            .arg(format!(
                "core.hooksPath={}",
                self.empty_hooks.path().display()
            ))
            .args(arguments)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.empty_config.path())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("PATH", &self.search_path)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
}

pub(super) fn checkout(
    repository: &str,
    reference: &str,
    directory: &Path,
    timeout: Duration,
) -> Result<String, CorpusError> {
    let runner = GitRunner::new()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(checkout_async(
        &runner, repository, reference, directory, timeout,
    ))
}

async fn checkout_async(
    runner: &GitRunner,
    repository: &str,
    reference: &str,
    directory: &Path,
    timeout: Duration,
) -> Result<String, CorpusError> {
    initialize_and_fetch(runner, repository, reference, directory, timeout).await?;
    let prefix = ["-C", path_text(directory)?];
    run(
        runner,
        &[
            &prefix[..],
            &["checkout", "--quiet", "--detach", "FETCH_HEAD", "--"],
        ]
        .concat(),
        timeout,
    )
    .await?;
    let commit = run(
        runner,
        &[&prefix[..], &["rev-parse", "--verify", "HEAD"]].concat(),
        timeout,
    )
    .await?;
    let commit = commit.trim().to_string();
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CorpusError::Git("Git returned an invalid commit ID".into()));
    }
    Ok(commit)
}

async fn initialize_and_fetch(
    runner: &GitRunner,
    repository: &str,
    reference: &str,
    directory: &Path,
    timeout: Duration,
) -> Result<(), CorpusError> {
    run(
        runner,
        &[
            "init",
            "--quiet",
            "--template=",
            "--",
            path_text(directory)?,
        ],
        timeout,
    )
    .await?;
    let prefix = ["-C", path_text(directory)?];
    run(
        runner,
        &[
            &prefix[..],
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--depth=1",
                "--",
                repository,
                reference,
            ],
        ]
        .concat(),
        timeout,
    )
    .await?;
    Ok(())
}

fn path_text(path: &Path) -> Result<&str, CorpusError> {
    path.to_str()
        .ok_or_else(|| CorpusError::InvalidSource("Git checkout paths must be valid UTF-8".into()))
}

async fn read_output(pipe: impl AsyncRead + Unpin) -> Result<Vec<u8>, CorpusError> {
    let mut bytes = Vec::new();
    pipe.take(OUTPUT_LIMIT + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err(CorpusError::Git(
            "Git output exceeded the capture limit".into(),
        ));
    }
    Ok(bytes)
}

async fn run(
    runner: &GitRunner,
    arguments: &[&str],
    timeout: Duration,
) -> Result<String, CorpusError> {
    let mut child = runner.command(arguments).spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CorpusError::Git("Missing Git stdout pipe".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CorpusError::Git("Missing Git stderr pipe".into()))?;
    let output = async {
        tokio::try_join!(read_output(stdout), read_output(stderr), async {
            Ok::<_, CorpusError>(child.wait().await?)
        })
    };
    let (stdout, stderr, status) = tokio::time::timeout(timeout, output)
        .await
        .map_err(|_| CorpusError::Git("Git operation timed out".into()))??;
    if !status.success() {
        return Err(CorpusError::Git(
            String::from_utf8_lossy(&stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}
