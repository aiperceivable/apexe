use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub(crate) const SOCKET_ENV: &str = "APEXE_SESSION_SOCKET";
pub(crate) const DAEMON_ENV: &str = "APEXE_SESSION_DAEMON";
pub(crate) const CWD_ENV: &str = "APEXE_SESSION_CWD";
pub(crate) const STOP_ENV: &str = "APEXE_SESSION_STOP";
pub(crate) const IDLE_ENV: &str = "APEXE_SESSION_IDLE_SECONDS";

pub(crate) fn run_client(socket: &Path, command: &str) -> anyhow::Result<i32> {
    let cwd = std::env::var_os(CWD_ENV)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let mut stream = connect_or_start(socket, &cwd)?;
    if std::env::var_os(STOP_ENV).is_some() {
        writeln!(stream, "{}", json!({"stop": true}))?;
        return Ok(0);
    }
    writeln!(stream, "{}", json!({"command": command}))?;
    stream.flush()?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    anyhow::ensure!(!response.is_empty(), "session daemon closed the connection");
    let value: Value = serde_json::from_str(&response)?;
    print!("{}", value["stdout"].as_str().unwrap_or_default());
    Ok(value["exit_code"].as_i64().unwrap_or(1) as i32)
}

pub(crate) fn run_daemon(socket: &Path) -> anyhow::Result<i32> {
    let cwd = std::env::var_os(CWD_ENV)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let current = std::env::current_exe()?;
    let (shell, _) = super::resolve_real_shell(None, &current)?;
    let config = crate::config::load_config(None)?;
    let shim_dir = super::shim_directory(&config);
    let (locked, allow_path) = super::restricted_options(false, &[])?;
    // A direct `apexe sh` session can run without installed shims (the PTY
    // tests and a plain launcher use it that way). The hook's persistent mode
    // requires the manifest before it allows a call, so this is where the
    // governance boundary is enforced, not here.
    let environment = if crate::cli::shim::read_manifest(&shim_dir).is_ok() {
        super::governed_environment(&shim_dir, &shell, locked, &allow_path)?
    } else {
        Vec::new()
    };
    let mut session = ShellSession::spawn(&shell, &cwd, &environment)?;
    let listener = bind_socket(socket)?;
    listener.set_nonblocking(true)?;
    let idle = std::env::var(IDLE_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(1800));
    let mut last_activity = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // macOS/BSD accept() inherits O_NONBLOCK from the listener.
                // The handler uses blocking-style read_line/write; without this
                // reset a response larger than the socket buffer fails with
                // WouldBlock and the daemon dies.
                stream.set_nonblocking(false)?;
                last_activity = Instant::now();
                if handle_connection(&mut session, stream)? {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if last_activity.elapsed() > idle {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
    session.stop();
    let _ = std::fs::remove_file(socket);
    Ok(0)
}

fn handle_connection(session: &mut ShellSession, stream: UnixStream) -> anyhow::Result<bool> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    anyhow::ensure!(!request.is_empty(), "empty session request");
    let value: Value = serde_json::from_str(&request)?;
    if value["stop"].as_bool() == Some(true) {
        return Ok(true);
    }
    let command = value["command"].as_str().unwrap_or_default();
    let result = session.execute(command)?;
    let mut writer = stream;
    serde_json::to_writer(
        &mut writer,
        &json!({"stdout": result.stdout, "exit_code": result.exit_code}),
    )?;
    writeln!(writer)?;
    Ok(false)
}

fn connect_or_start(socket: &Path, cwd: &Path) -> anyhow::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket) {
        return Ok(stream);
    }
    let executable = std::env::current_exe()?;
    let mut command = Command::new(executable);
    command
        .args(["sh"])
        .env(SOCKET_ENV, socket)
        .env(DAEMON_ENV, "1")
        .env(CWD_ENV, cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the child hook only calls `setsid`, which is async-signal-safe
    // and does not touch memory owned by the parent.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command.spawn()?;
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(stream) = UnixStream::connect(socket) {
            return Ok(stream);
        }
    }
    anyhow::bail!(
        "session daemon did not start at {}; set {} to a writable path",
        socket.display(),
        SOCKET_ENV
    )
}

fn bind_socket(socket: &Path) -> anyhow::Result<UnixListener> {
    if socket.exists() {
        let _ = std::fs::remove_file(socket);
    }
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

struct ShellSession {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<File>,
    next_token: u64,
}

struct SessionResult {
    stdout: String,
    exit_code: i32,
}

impl ShellSession {
    /// Spawn the session shell with the same governed environment a
    /// non-persistent `apexe sh` uses. Without this the daemon inherited the
    /// host PATH and skipped the shim directory entirely, so the hook reported
    /// calls as governed while they ran ungoverned.
    fn spawn(
        shell: &Path,
        cwd: &Path,
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> anyhow::Result<Self> {
        let mut descriptors = [0; 2];
        // SAFETY: `descriptors` is valid storage for two file descriptors.
        anyhow::ensure!(
            unsafe { libc::pipe(descriptors.as_mut_ptr()) } == 0,
            "cannot create session pipe"
        );
        let read = descriptors[0];
        let write = descriptors[1];
        // SAFETY: `write` is an open descriptor; `dup` returns a second one.
        let write_for_stderr = unsafe { libc::dup(write) };
        anyhow::ensure!(write_for_stderr >= 0, "cannot duplicate session pipe");
        let mut command = Command::new(shell);
        command
            .arg("-s")
            .env("PS1", "")
            .env("PS2", "")
            .env("TERM", "dumb");
        for (key, value) in environment {
            command.env(key, value);
        }
        command
            .current_dir(cwd)
            .stdin(Stdio::piped())
            // SAFETY: both descriptors are owned by the child process after
            // spawn and are not used by the parent again.
            .stdout(unsafe { Stdio::from_raw_fd(write) })
            .stderr(unsafe { Stdio::from_raw_fd(write_for_stderr) });
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no shell stdin"))?;
        // SAFETY: `read` is the parent's end of the pipe created above.
        let reader = BufReader::new(unsafe { File::from_raw_fd(read) });
        let mut session = Self {
            child,
            stdin,
            reader,
            next_token: 0,
        };
        session.send("shopt -s expand_aliases 2>/dev/null || true")?;
        Ok(session)
    }

    fn send(&mut self, command: &str) -> anyhow::Result<()> {
        writeln!(self.stdin, "{command}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn execute(&mut self, command: &str) -> anyhow::Result<SessionResult> {
        let token = format!("__APEXE_SESSION_{}__", self.next_token);
        self.next_token += 1;
        self.send(&format!(
            "{{\n{command}\n}} 2>&1\n__apexe_status=$?\nprintf '\\n{token}%s\\n' \"$__apexe_status\""
        ))?;
        let mut output = String::new();
        loop {
            let mut line = Vec::new();
            let read = self.reader.read_until(b'\n', &mut line)?;
            anyhow::ensure!(read > 0, "session shell exited before returning a status");
            let text = String::from_utf8_lossy(&line);
            if let Some(code) = text.trim_end().strip_prefix(&token) {
                let exit_code = code.parse::<i32>().unwrap_or(1);
                if output.ends_with('\n') {
                    output.pop();
                }
                return Ok(SessionResult {
                    stdout: output,
                    exit_code,
                });
            }
            output.push_str(&text);
        }
    }

    fn stop(&mut self) {
        let _ = self.send("exit");
        let _ = self.child.wait();
    }
}
