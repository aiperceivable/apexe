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
    // `0` means the local surface configured no bound. A passthrough call is
    // the caller's own command, and the caller's terminal is a better stop
    // signal than a wall clock: an editor, a pager, `ssh`, `npm run dev` or an
    // agent's own TUI outlives any fixed number, and killing them mid-flight
    // truncates inherited output into torn escape sequences.
    if timeout_ms == 0 {
        return run.await;
    }
    tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), run)
        .await
        .map_err(|_| {
            ModuleError::new(
                ErrorCode::ModuleTimeout,
                format!("Command '{binary}' timed out after {timeout_ms}ms"),
            )
        })?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passthrough() -> ExecutionIo {
        ExecutionIo::Passthrough {
            stdin: StdinMode::Null,
        }
    }

    /// The regression behind a broken agent session: a local shim wrapped the
    /// caller's whole command in `default_timeout`, so an editor, a server or
    /// a TUI was killed at 30 s and its inherited output arrived torn.
    #[tokio::test]
    async fn test_passthrough_with_zero_timeout_is_unbounded() {
        let output = execute("sleep", &["1".to_string()], None, 0, 1024, passthrough())
            .await
            .expect("a zero local timeout must not arm a timer");
        match output {
            ExecutionOutput::Passthrough(output) => assert_eq!(output.exit_code, 0),
            ExecutionOutput::Captured(_) => panic!("expected passthrough output"),
        }
    }

    /// The knob still works: an operator who sets `local_timeout` gets a bound.
    #[tokio::test]
    async fn test_passthrough_with_a_timeout_still_bounds_the_run() {
        let error = execute("sleep", &["10".to_string()], None, 50, 1024, passthrough())
            .await
            .expect_err("a bounded passthrough call must time out");
        assert_eq!(error.code, ErrorCode::ModuleTimeout);
    }
}
