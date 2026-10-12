use apcore::{ErrorCode, ModuleError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdinMode {
    #[default]
    Null,
    Inherit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionIo {
    #[default]
    Captured,
    Passthrough {
        stdin: StdinMode,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassthroughOutput {
    pub exit_code: i32,
}

#[derive(Debug)]
pub(crate) enum ExecutionOutput {
    Captured(super::executor::SubprocessOutput),
    Passthrough(PassthroughOutput),
}

pub(crate) fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map_or(1, |signal| 128 + signal)
    }
    #[cfg(not(unix))]
    1
}

#[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
pub(crate) async fn execute(
    binary: &str,
    args: &[String],
    json_flag: Option<&str>,
    timeout_ms: u64,
    max_output_bytes: usize,
    io: ExecutionIo,
) -> Result<ExecutionOutput, ModuleError> {
    if io == ExecutionIo::Captured {
        return super::executor::execute_subprocess(
            binary,
            args,
            json_flag,
            timeout_ms,
            max_output_bytes,
        )
        .await
        .map(ExecutionOutput::Captured);
    }
    let run = async {
        let mut child = super::executor::spawn_with_io(binary, args, io)?;
        let status = child.wait().await.map_err(|error| {
            ModuleError::new(
                ErrorCode::ModuleExecuteError,
                format!("Failed to wait on '{binary}': {error}"),
            )
        })?;
        Ok(ExecutionOutput::Passthrough(PassthroughOutput {
            exit_code: exit_code(status),
        }))
    };
    tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), run)
        .await
        .map_err(|_| {
            ModuleError::new(
                ErrorCode::ModuleTimeout,
                format!("Command '{binary}' timed out after {timeout_ms}ms"),
            )
        })?
}
