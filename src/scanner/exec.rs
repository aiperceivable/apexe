//! Subprocess execution with a wall-clock timeout.
//!
//! Every CLI probe the scanner runs (`--help`, `--version`, expanded help,
//! per-subcommand help) must be bounded: a scanned tool that blocks on stdin,
//! spawns a pager, or simply hangs would otherwise stall the entire scan
//! indefinitely. All spawn sites route through [`run_with_timeout`].

use std::io;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Run `program` with `args`, capturing stdout/stderr, killing the child if it
/// exceeds `timeout`.
///
/// stdin is connected to `/dev/null` so a tool that waits for input fails fast
/// instead of blocking. On timeout the child is killed and reaped (no orphan)
/// and an [`io::ErrorKind::TimedOut`] error is returned.
pub fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> io::Result<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("`{program}` timed out after {timeout:?}"),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    child.wait_with_output()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_with_timeout_captures_stdout() {
        let out = run_with_timeout("echo", &["hello"], Duration::from_secs(5)).unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }

    #[test]
    fn test_run_with_timeout_kills_hung_process() {
        // `sleep 30` far exceeds the 200ms budget; must return TimedOut quickly.
        let start = Instant::now();
        let result = run_with_timeout("sleep", &["30"], Duration::from_millis(200));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout was not enforced promptly"
        );
    }

    #[test]
    fn test_run_with_timeout_nonexistent_program() {
        let result = run_with_timeout("zzz_no_such_binary_xyz", &[], Duration::from_secs(5));
        assert!(result.is_err());
    }
}
