#![cfg(unix)]

use apcore::ModuleAnnotations;
use apcore_toolkit::ScannedModule;
use assert_cmd::Command;
use serde_json::json;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            home: TempDir::new().unwrap(),
        };
        fixture.add_binding(
            "cli.apxepwd",
            "apxepwd",
            "/bin/pwd",
            ModuleAnnotations {
                readonly: true,
                ..Default::default()
            },
            false,
        );
        fixture
    }

    fn bindings(&self) -> PathBuf {
        self.home.path().join(".apexe/bindings")
    }

    fn shims(&self) -> PathBuf {
        self.home.path().join(".apexe/bin")
    }

    fn std_command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_apexe"));
        command
            .env("APEXE_OFFLINE", "true")
            .env("HOME", self.home.path())
            .env("SHELL", "/bin/sh")
            .env_remove("APEXE_REAL_SHELL")
            .env_remove("APEXE_SHIM_DIR")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env("RUST_LOG", "error")
            .current_dir(self.home.path());
        command
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("apexe").unwrap();
        command
            .env("APEXE_OFFLINE", "true")
            .env("HOME", self.home.path())
            .env("SHELL", "/bin/sh")
            .env_remove("APEXE_REAL_SHELL")
            .env_remove("APEXE_SHIM_DIR")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env("RUST_LOG", "error")
            .current_dir(self.home.path());
        command
    }

    fn add_binding(
        &self,
        module_id: &str,
        name: &str,
        target: &str,
        annotations: ModuleAnnotations,
        streaming: bool,
    ) {
        let mut module = ScannedModule::new(
            module_id.into(),
            "Agent shell PTY module".into(),
            json!({
                "type": "object",
                "x-apexe-end-of-options": true,
                "properties": {},
                "additionalProperties": false
            }),
            json!({"type": "object"}),
            vec!["pilot".into()],
            format!("exec://{target}"),
        );
        module.annotations = Some(annotations);
        module.metadata.insert("command_path".into(), json!([name]));
        module.metadata.insert("variant".into(), json!("unknown"));
        module.metadata.insert(
            "shim_contract".into(),
            json!({
                "authoritative": true,
                "variant": "unknown",
                "short_flag_bundling": true,
                "streaming_stdin_safe": streaming,
                "cross_flag_ordering": false
            }),
        );
        module.input_schema["x-apexe-streaming-stdin-safe"] = json!(streaming);
        module.input_schema["x-apexe-short-flag-bundling"] = json!(true);
        module.input_schema["x-apexe-cross-flag-ordering"] = json!(false);
        apexe::output::YamlOutput::without_verification()
            .write(&[module], &self.bindings(), false)
            .unwrap();
    }

    fn add_script(
        &self,
        module_id: &str,
        name: &str,
        body: &str,
        annotations: ModuleAnnotations,
        streaming: bool,
    ) {
        use std::os::unix::fs::PermissionsExt;

        let path = self.home.path().join(format!("{name}.sh"));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        self.add_binding(
            module_id,
            name,
            path.to_str().unwrap(),
            annotations,
            streaming,
        );
    }

    fn enable(&self) {
        self.command().args(["env", "--enable"]).assert().success();
    }

    fn enable_with_rc(&self, rc: &Path) {
        self.command()
            .args(["env", "--enable", "--shell", "sh", "--rc-file"])
            .arg(rc)
            .assert()
            .success();
    }

    fn pty_command(&self, args: &[&str]) -> std::process::Command {
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_apexe"));
        #[cfg(target_os = "macos")]
        let mut command = {
            let mut command = std::process::Command::new("script");
            command
                .args(["-q", "/dev/null"])
                .arg(&executable)
                .args(args);
            command
        };
        #[cfg(not(target_os = "macos"))]
        let mut command = {
            let mut words = vec![shell_words::quote(executable.to_str().unwrap()).into_owned()];
            words.extend(args.iter().map(|arg| shell_words::quote(arg).into_owned()));
            let mut command = std::process::Command::new("script");
            command.args(["-q", "-e", "-c", &words.join(" "), "/dev/null"]);
            command
        };
        command
            .env("HOME", self.home.path())
            .env("SHELL", "/bin/sh")
            .env("APEXE_REAL_SHELL", "/bin/sh")
            .env_remove("APEXE_SHIM_DIR")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env("RUST_LOG", "error")
            .current_dir(self.home.path());
        command
    }

    fn pty(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .pty_command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn pty_answer(&self, args: &[&str], answer: &str) -> Output {
        let mut child = self
            .pty_command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        std::thread::sleep(Duration::from_millis(400));
        stdin.write_all(answer.as_bytes()).unwrap();
        stdin.flush().unwrap();
        let output = child.wait_with_output().unwrap();
        drop(stdin);
        output
    }
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn test_pty_interactive_shell_runs_commands() {
    let fixture = Fixture::new();
    fixture.enable();

    let output = fixture.pty(&["sh"], "echo PTY_OK\nexit\n");
    assert!(output_text(&output).contains("PTY_OK"), "{output:?}");
}

#[test]
fn test_pty_script_file_receives_arguments() {
    let fixture = Fixture::new();
    fixture.enable();
    let script = fixture.home.path().join("probe.sh");
    std::fs::write(&script, "#!/bin/sh\necho SCRIPT_OK \"$@\"\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = fixture.pty(&["sh", "--", script.to_str().unwrap(), "one", "two"], "");
    assert!(
        output_text(&output).contains("SCRIPT_OK one two"),
        "{output:?}"
    );
}

#[test]
fn test_pty_approval_yes_executes_and_no_refuses() {
    let fixture = Fixture::new();
    fixture.add_script(
        "cli.approval",
        "approval",
        "echo APPROVED",
        ModuleAnnotations {
            readonly: true,
            requires_approval: true,
            ..Default::default()
        },
        false,
    );
    fixture.enable();

    let approved = fixture.pty_answer(&["sh", "-c", "approval"], "y\n");
    assert!(output_text(&approved).contains("APPROVED"), "{approved:?}");

    let refused = fixture.pty_answer(&["sh", "-c", "approval"], "n\n");
    assert!(
        output_text(&refused).contains("Rejected at the local approval prompt"),
        "{refused:?}"
    );
}

#[test]
fn test_sh_sigint_terminates_the_foreground_command() {
    let fixture = Fixture::new();
    fixture.enable();

    let child = fixture
        .std_command()
        .args(["sh", "-c", "sleep 30; echo DONE"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    // SAFETY: the pid is the process group we created above.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGINT);
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(!output_text(&output).contains("DONE"), "{output:?}");
}

#[test]
fn test_sh_sigterm_terminates_the_shell() {
    let fixture = Fixture::new();
    fixture.enable();

    let child = fixture
        .std_command()
        .args(["sh", "-c", "sleep 30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    // SAFETY: the pid is the process group we created above.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGTERM);
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success(), "{output:?}");
}

#[test]
fn test_sh_pipeline_closes_sigpipe_without_hanging() {
    let fixture = Fixture::new();
    fixture.add_script(
        "cli.big",
        "big",
        "while :; do echo line; done",
        ModuleAnnotations {
            readonly: true,
            ..Default::default()
        },
        false,
    );
    fixture.enable();

    fixture
        .command()
        .args(["sh", "-c", "big | head -n 1"])
        .timeout(Duration::from_secs(5))
        .assert()
        .success()
        .stdout(predicates::str::contains("line"));
}

#[test]
fn test_sh_removes_dynamic_loader_variables() {
    let fixture = Fixture::new();
    fixture.add_script(
        "cli.envprobe",
        "envprobe",
        "printf '%s|%s' \"${LD_PRELOAD-unset}\" \"${DYLD_INSERT_LIBRARIES-unset}\"",
        ModuleAnnotations {
            readonly: true,
            ..Default::default()
        },
        false,
    );
    fixture.enable();

    fixture
        .command()
        .env("LD_PRELOAD", "")
        .env("DYLD_INSERT_LIBRARIES", "")
        .args(["sh", "-c", "envprobe"])
        .assert()
        .success()
        .stdout("unset|unset");
}

#[test]
fn test_sh_inherits_stdin_for_streaming_safe_shim() {
    let fixture = Fixture::new();
    fixture.add_binding(
        "cli.streamcat",
        "streamcat",
        "/bin/cat",
        ModuleAnnotations {
            readonly: true,
            ..Default::default()
        },
        true,
    );
    fixture.enable();

    fixture
        .command()
        .args(["sh", "-c", "streamcat"])
        .write_stdin("hello")
        .assert()
        .success()
        .stdout("hello");
}

#[test]
fn test_login_shell_loads_managed_block_after_path_reset() {
    let fixture = Fixture::new();
    fixture.enable();
    let profile = fixture.home.path().join(".profile");
    std::fs::write(
        &profile,
        "PATH=/usr/bin:/bin\nexport PATH\necho PROFILE_LOADED\n",
    )
    .unwrap();
    fixture.enable_with_rc(&profile);

    fixture
        .command()
        .args(["sh", "-l", "-c", "command -v apxepwd"])
        .assert()
        .success()
        .stdout(predicates::str::contains("PROFILE_LOADED"))
        .stdout(predicates::str::contains(fixture.shims().to_str().unwrap()));
}

#[test]
fn test_persistent_session_preserves_state_and_exit_codes() {
    let home = TempDir::new().unwrap();
    let socket = home.path().join("session.sock");
    let run = |command: &str| {
        let mut process = std::process::Command::new(env!("CARGO_BIN_EXE_apexe"));
        process
            .args(["sh", "-c", command])
            .env("HOME", home.path())
            .env("SHELL", "/bin/sh")
            .env("APEXE_SESSION_SOCKET", &socket)
            .env("APEXE_SESSION_CWD", home.path())
            .env("APEXE_SESSION_IDLE_SECONDS", "5")
            .env_remove("APEXE_SESSION_DAEMON")
            .env_remove("APEXE_REAL_SHELL")
            .env_remove("APEXE_SHIM_DIR")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env("RUST_LOG", "error")
            .current_dir(home.path());
        process.output().unwrap()
    };

    let first = run("pwd");
    assert!(first.status.success());
    assert_eq!(
        String::from_utf8_lossy(&first.stdout).trim(),
        std::fs::canonicalize(home.path())
            .unwrap()
            .display()
            .to_string()
    );

    let changed = run("cd /tmp; pwd");
    assert_eq!(String::from_utf8_lossy(&changed.stdout).trim(), "/tmp");
    let persisted = run("pwd");
    assert_eq!(String::from_utf8_lossy(&persisted.stdout).trim(), "/tmp");

    let function = run("f() { echo FN; }; f");
    assert_eq!(String::from_utf8_lossy(&function.stdout).trim(), "FN");

    let pipeline = run("printf 'a\\nb\\n' | tail -n 1");
    assert_eq!(String::from_utf8_lossy(&pipeline.stdout).trim(), "b");

    // Regression: the daemon used blocking-style I/O on a socket that
    // inherited O_NONBLOCK from the listener. Responses over ~8 KiB failed
    // with WouldBlock and killed the daemon.
    let large = run("seq 1 20000");
    assert!(
        large.status.success(),
        "large session response must succeed"
    );
    assert_eq!(
        String::from_utf8_lossy(&large.stdout).lines().count(),
        20_000
    );

    let failed = run("false");
    assert_eq!(failed.status.code(), Some(1));

    let stop = std::process::Command::new(env!("CARGO_BIN_EXE_apexe"))
        .args(["sh", "-c", "stop"])
        .env("HOME", home.path())
        .env("APEXE_SESSION_SOCKET", &socket)
        .env("APEXE_SESSION_STOP", "1")
        .env_remove("APEXE_SESSION_DAEMON")
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(stop.status.success());
    for _ in 0..50 {
        if !socket.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!socket.exists());
}
