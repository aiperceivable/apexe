//! Subprocess execution with a wall-clock timeout.
//!
//! Every CLI probe the scanner runs (`--help`, `--version`, expanded help,
//! per-subcommand help) must be bounded: a scanned tool that blocks on stdin,
//! spawns a pager, or simply hangs would otherwise stall the entire scan
//! indefinitely. All spawn sites route through [`run_with_timeout`].

use std::io;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Drain a pipe on its own thread.
///
/// Both pipes are read concurrently with the wait loop: reading one to
/// completion while the other stays unread deadlocks the child once its pipe
/// buffer fills. A read error yields whatever arrived before it — the exit
/// status is the outcome that matters here.
/// Highest number of bytes [`drain_pipe`] will buffer from a single stream.
///
/// Any realistic probe output — even `curl --help all`, the largest case
/// this scanner is known to hit — is well under 100 KiB. This cap is
/// deliberately generous headroom while still bounding worst-case memory
/// when a scanned binary streams indefinitely (e.g. `yes --help`).
const MAX_PROBE_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// Drain a pipe on its own thread, up to [`MAX_PROBE_OUTPUT_BYTES`].
///
/// Both pipes are read concurrently with the wait loop: reading one to
/// completion while the other stays unread deadlocks the child once its pipe
/// buffer fills. A read error yields whatever arrived before it — the exit
/// status is the outcome that matters here. Once the cap is reached, this
/// thread stops reading; the child then blocks on its next `write()` (pipe
/// backpressure), which the wall-clock deadline in `run_with_timeout`'s poll
/// loop will still catch and kill.
fn drain_pipe(mut pipe: impl io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            if buf.len() >= MAX_PROBE_OUTPUT_BYTES {
                break;
            }
            let to_read = chunk.len().min(MAX_PROBE_OUTPUT_BYTES - buf.len());
            match pipe.read(&mut chunk[..to_read]) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        buf
    })
}

/// Run `program` with `args`, capturing stdout/stderr, killing the child if it
/// exceeds `timeout`.
///
/// stdin is connected to `/dev/null` so a tool that waits for input fails fast
/// instead of blocking. On timeout the child is killed and reaped (no orphan)
/// and an [`io::ErrorKind::TimedOut`] error is returned.
///
/// stdout and stderr are drained on dedicated threads so a child that fills its
/// pipe buffer (>~64 KiB) never blocks in `write()` — reading only after the
/// child exits would deadlock on large output (the same reason
/// `std::process::Command::output()` drains both pipes concurrently). This
/// matters for big-help tools (`kubectl`, `docker`, `--help all`).
pub fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> io::Result<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // INVARIANT: stdout/stderr were configured Stdio::piped() above, so take() is Some.
    let out_reader = drain_pipe(child.stdout.take().expect("stdout was piped"));
    let err_reader = drain_pipe(child.stderr.take().expect("stderr was piped"));

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            // Readers unblock once the killed child's pipe ends close.
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("`{program}` timed out after {timeout:?}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    Ok(Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
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

    #[test]
    fn test_drain_pipe_caps_output_instead_of_buffering_unbounded_data() {
        // Regression: a source producing far more than the cap must be
        // truncated by drain_pipe rather than buffered in full. Unbounded
        // buffering is how a subprocess that streams forever (e.g.
        // `yes --help`) exhausts memory before the wall-clock deadline in
        // run_with_timeout's poll loop is ever consulted — the deadline
        // governs try_wait, not the reader thread.
        struct FiniteButLargeReader {
            remaining: usize,
        }
        impl io::Read for FiniteButLargeReader {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.remaining == 0 {
                    return Ok(0);
                }
                let n = buf.len().min(self.remaining);
                for b in &mut buf[..n] {
                    *b = b'x';
                }
                self.remaining -= n;
                Ok(n)
            }
        }
        let source = FiniteButLargeReader {
            remaining: MAX_PROBE_OUTPUT_BYTES * 2,
        };
        let bytes = drain_pipe(source).join().unwrap();
        assert!(
            bytes.len() <= MAX_PROBE_OUTPUT_BYTES,
            "drain_pipe buffered {} bytes, more than the {}-byte cap",
            bytes.len(),
            MAX_PROBE_OUTPUT_BYTES
        );
    }

    #[test]
    fn test_run_with_timeout_large_output_no_deadlock() {
        // Regression: a child that fills the pipe buffer (>64 KiB) before
        // exiting must NOT deadlock. Reading pipes only after wait would hang
        // here until the deadline; concurrent draining captures it all.
        let out = run_with_timeout(
            "sh",
            &["-c", "head -c 200000 /dev/zero"],
            Duration::from_secs(10),
        )
        .expect("large-output command should complete, not time out");
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 200_000);
    }
}
