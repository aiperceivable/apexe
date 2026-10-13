use super::behavior::{BehaviorOptions, Case, CaseMode, PreparedCase};
use super::{isolation, VerificationStatus};
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};

pub(super) struct CaseOutcome {
    pub status: VerificationStatus,
    pub reason: Option<String>,
    pub isolation: Option<String>,
}

impl CaseOutcome {
    pub fn unverifiable(reason: String) -> Self {
        Self {
            status: VerificationStatus::Unverifiable,
            reason: Some(reason),
            isolation: None,
        }
    }
}

pub(super) async fn run_case(
    case: &Case,
    module: &apcore_toolkit::ScannedModule,
    _bindings: &Path,
    timeout: u64,
    options: BehaviorOptions,
) -> CaseOutcome {
    let owned_case = case.clone();
    let owned_module = module.clone();
    let prepared = match tokio::task::spawn_blocking(move || {
        super::behavior::prepare_case(&owned_case, &owned_module)
    })
    .await
    {
        Ok(Ok(prepared)) => prepared,
        error => {
            return CaseOutcome {
                status: VerificationStatus::Failed,
                reason: Some(format!("Invalid case preparation: {error:?}")),
                isolation: None,
            }
        }
    };
    let plan = match isolation_plan(case, module, &prepared, options).await {
        Ok(plan) => plan,
        Err(error) => return CaseOutcome::unverifiable(error.to_string()),
    };
    let backend = plan
        .as_ref()
        .map_or("none", |plan| plan.backend.as_str())
        .to_string();
    let result = execute_case(case, module, &prepared, plan.as_ref(), timeout).await;
    CaseOutcome {
        status: if result.is_ok() {
            VerificationStatus::Passed
        } else {
            VerificationStatus::Failed
        },
        reason: result.err().map(|error| error.to_string()),
        isolation: Some(backend),
    }
}

async fn isolation_plan(
    case: &Case,
    module: &apcore_toolkit::ScannedModule,
    prepared: &PreparedCase,
    options: BehaviorOptions,
) -> anyhow::Result<Option<isolation::IsolationPlan>> {
    let required = case.mode != CaseMode::Readonly
        || matches!(options.isolation, super::IsolationMode::Required);
    if matches!(options.isolation, super::IsolationMode::None) {
        anyhow::ensure!(!required, "This case requires operating-system isolation");
        return Ok(None);
    }
    let root = prepared.temporary.path().to_path_buf();
    let executable = std::env::current_exe()?;
    let binary = super::behavior::validate_target(module, case.mode)?;
    let readonly = case.mode != CaseMode::Isolated
        || module
            .annotations
            .as_ref()
            .is_some_and(|annotations| annotations.readonly);
    let plan = tokio::task::spawn_blocking(move || {
        isolation::prepare(&root, &executable, &binary, readonly)
    })
    .await?;
    let available = match plan {
        Ok(plan) => match plan.probe().await {
            Ok(()) => Ok(plan),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    match available {
        Ok(plan) => Ok(Some(plan)),
        Err(error) if required => Err(anyhow::anyhow!("Required isolation unavailable: {error}")),
        Err(error) => {
            tracing::warn!(%error, "Isolation unavailable; using independently reviewed readonly admission");
            Ok(None)
        }
    }
}

async fn execute_case(
    case: &Case,
    module: &apcore_toolkit::ScannedModule,
    prepared: &PreparedCase,
    plan: Option<&isolation::IsolationPlan>,
    timeout: u64,
) -> anyhow::Result<()> {
    let command = case_command(case, prepared, plan, timeout)?;
    let output = capture(command, timeout.saturating_add(2)).await?;
    let output: Value = serde_json::from_slice(&output)?;
    anyhow::ensure!(
        output["exit_code"] == 0
            && output["stdout"] == case.expected_stdout
            && output["stderr"] == case.expected_stderr,
        "Behavioral output did not match the explicit expectation"
    );
    let owned_module = module.clone();
    let owned_case = case.clone();
    let work = prepared.work.clone();
    let before = prepared.binary_digest.clone();
    tokio::task::spawn_blocking(move || {
        anyhow::ensure!(
            before == super::binary_digest(&owned_module)?,
            "Binary changed during verification"
        );
        check_expected_files(&owned_case, &work)
    })
    .await??;
    Ok(())
}

fn case_command(
    case: &Case,
    prepared: &PreparedCase,
    plan: Option<&isolation::IsolationPlan>,
    timeout: u64,
) -> anyhow::Result<tokio::process::Command> {
    let executable = std::env::current_exe()?;
    let mut command = plan.map_or_else(
        || tokio::process::Command::new(&executable),
        |plan| plan.command(&executable),
    );
    command
        .args(["exec", &case.module_id, "--bindings-dir"])
        .arg(&prepared.bindings)
        .arg("--input")
        .arg(serde_json::to_string(&prepared.input)?)
        .arg("--acl")
        .arg(&prepared.acl)
        .current_dir(&prepared.work)
        .env_clear()
        .env("HOME", &prepared.home)
        .env("PATH", crate::runtime_path::search_path())
        .env("LC_ALL", "C")
        .env("RUST_LOG", "error")
        .env("APEXE_TIMEOUT", timeout.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    Ok(command)
}

struct ProcessGroup {
    #[cfg(unix)]
    pid: i32,
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: this process group was created exclusively for the verification child.
        unsafe {
            libc::kill(-self.pid, libc::SIGKILL);
        }
    }
}

async fn capture(mut command: tokio::process::Command, timeout: u64) -> anyhow::Result<Vec<u8>> {
    let mut child = command.spawn()?;
    let _group = ProcessGroup {
        #[cfg(unix)]
        pid: child
            .id()
            .ok_or_else(|| anyhow::anyhow!("Missing verification child PID"))? as i32,
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Missing stdout pipe"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("Missing stderr pipe"))?;
    let run = async {
        tokio::try_join!(read_pipe(stdout), read_pipe(stderr), async {
            Ok::<_, anyhow::Error>(child.wait().await?)
        })
    };
    let (stdout, stderr, _) =
        tokio::time::timeout(std::time::Duration::from_secs(timeout), run).await??;
    anyhow::ensure!(
        !stdout.is_empty(),
        "Governed behavioral call produced no result: {}",
        String::from_utf8_lossy(&stderr)
    );
    Ok(stdout)
}

async fn read_pipe(pipe: impl AsyncRead + Unpin) -> anyhow::Result<Vec<u8>> {
    const LIMIT: u64 = 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    pipe.take(LIMIT + 1).read_to_end(&mut bytes).await?;
    anyhow::ensure!(
        bytes.len() as u64 <= LIMIT,
        "Verification output exceeded 16 MiB"
    );
    Ok(bytes)
}

fn check_expected_files(case: &Case, work: &Path) -> anyhow::Result<()> {
    for (name, contents) in &case.expected_files {
        anyhow::ensure!(
            Path::new(name).components().count() == 1 && !matches!(name.as_str(), "." | ".."),
            "Expected file name must be a relative component"
        );
        let path = work.join(name);
        let metadata = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            metadata.is_file() && metadata.len() <= 1024 * 1024,
            "Invalid expected file"
        );
        anyhow::ensure!(
            std::fs::read_to_string(path)? == *contents,
            "File output did not match expectation"
        );
    }
    Ok(())
}
