//! End-to-end argv rendering against the real binaries.
//!
//! Both defects these tests cover render perfectly well as strings — the
//! rendered command *looks* right in either spelling, and only the wrapped
//! binary can tell the two apart. A unit test asserting `--max-time=1` would
//! have passed for the whole time curl was refusing it. So these execute.
//!
//! Every test states its environment precondition and skips rather than fails
//! when it is not met: a Linux CI box has GNU `find` with no `-f`, and a
//! container image may ship no `curl` at all.

use apexe::module::executor::{build_arguments, execute_subprocess, DEFAULT_MAX_OUTPUT_BYTES};
use serde_json::{json, Value};

/// A local port nothing listens on, so `curl` exercises option parsing and
/// then fails to connect instead of reaching the network.
const CLOSED_PORT_URL: &str = "http://127.0.0.1:9/";

/// `curl`'s exit code for an unknown option, as distinct from 7 (could not
/// connect), which means the option parsed.
const CURL_EXIT_UNKNOWN_OPTION: i32 = 2;

fn kwargs(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

fn binary_exists(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

/// Whether this host's `/usr/bin/find` is the BSD one. GNU find answers
/// `--version`; BSD find rejects it, which is the same probe the shipped
/// overlays match on.
fn find_is_bsd() -> bool {
    std::process::Command::new("/usr/bin/find")
        .arg("--version")
        .output()
        .is_ok_and(|out| !out.status.success())
}

/// A schema shaped the way the scanner emits `cli.curl`: long options with a
/// required value, plus the `url` operand.
fn curl_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "url": { "type": "string", "x-apexe-positional": 0 },
            "max_time": { "type": "number", "x-apexe-flag": "--max-time" },
            "user_agent": { "type": "string", "x-apexe-flag": "--user-agent" },
        }
    })
}

/// A schema shaped the way the `find` overlays describe the tool: paths ahead
/// of the flags, true options ahead of the paths, primaries and the expression
/// trailing, and `--` declared as an accepted end-of-options separator.
fn find_schema(end_of_options: bool) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "array",
                "x-apexe-positional": 0,
                "x-apexe-operand-position": "before-flags"
            },
            "expression": { "type": "array", "x-apexe-positional": 1 },
            "f": {
                "type": "array",
                "x-apexe-flag": "-f",
                "x-apexe-flag-position": "before-operands"
            },
            "name": { "type": "string", "x-apexe-flag": "-name" },
        }
    });
    if end_of_options {
        schema["x-apexe-end-of-options"] = json!(true);
    }
    schema
}

/// Regression (#24): a long option's value used to be attached with `=`, on the
/// premise that an option whose value is *required* accepts both spellings.
/// `curl` implements its own option parser and accepts neither `=` form, which
/// made 134 of `cli.curl`'s 258 properties unusable.
#[tokio::test]
async fn test_curl_accepts_a_separated_long_option_value() {
    if !binary_exists("/usr/bin/curl") {
        eprintln!("skipping: no /usr/bin/curl on this host");
        return;
    }

    let args = build_arguments(
        &kwargs(&[
            ("url", json!(CLOSED_PORT_URL)),
            ("max_time", json!(1)),
            ("user_agent", json!("apexe")),
        ]),
        Some(&curl_schema()),
    )
    .expect("a curl invocation with two valued long options must render");

    assert!(
        !args.iter().any(|arg| arg.starts_with("--max-time=")),
        "the attached spelling is the defect: {args:?}"
    );

    let out = execute_subprocess(
        "/usr/bin/curl",
        &args,
        None,
        10_000,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
    .await
    .expect("curl should run");

    assert_ne!(
        out.exit_code, CURL_EXIT_UNKNOWN_OPTION,
        "curl rejected an option apexe rendered: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("is unknown"),
        "curl rejected an option apexe rendered: {}",
        out.stderr
    );
}

/// The premise of the test above, asserted rather than assumed: the `=` form
/// really is refused, so the separated form is not merely a stylistic choice.
#[tokio::test]
async fn test_curl_refuses_an_attached_long_option_value() {
    if !binary_exists("/usr/bin/curl") {
        eprintln!("skipping: no /usr/bin/curl on this host");
        return;
    }

    let args = vec!["--max-time=1".to_string(), CLOSED_PORT_URL.to_string()];
    let out = execute_subprocess(
        "/usr/bin/curl",
        &args,
        None,
        10_000,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
    .await
    .expect("curl should run");

    assert_eq!(
        out.exit_code, CURL_EXIT_UNKNOWN_OPTION,
        "curl was expected to refuse `--max-time=1`: {out:?}"
    );
    assert!(
        out.stderr.contains("--max-time=1"),
        "curl should name the option it refused: {}",
        out.stderr
    );
}

/// Regression (#25): every one of `find`'s expression primaries begins with
/// `-`, so the `expression` operand could not carry any legal value at all.
#[tokio::test]
async fn test_find_expression_operand_reaches_the_binary() {
    if !binary_exists("/usr/bin/find") {
        eprintln!("skipping: no /usr/bin/find on this host");
        return;
    }
    let sandbox = tempfile::tempdir().expect("sandbox");
    std::fs::write(sandbox.path().join("wanted.txt"), b"x").unwrap();
    std::fs::write(sandbox.path().join("ignored.log"), b"x").unwrap();
    let root = sandbox.path().to_str().unwrap().to_string();

    let args = build_arguments(
        &kwargs(&[
            ("path", json!([root])),
            ("expression", json!(["-name", "*.txt"])),
        ]),
        Some(&find_schema(true)),
    )
    .expect("an expression of primaries must render once `--` is available");

    assert_eq!(
        args.first().map(String::as_str),
        Some("--"),
        "the separator belongs ahead of the paths: {args:?}"
    );

    let out = execute_subprocess(
        "/usr/bin/find",
        &args,
        None,
        10_000,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
    .await
    .expect("find should run");

    assert_eq!(
        out.exit_code,
        0,
        "find rejected {args:?}: {}",
        out.stderr.trim()
    );
    assert!(
        out.stdout.contains("wanted.txt") && !out.stdout.contains("ignored.log"),
        "the expression did not take effect: {out:?}"
    );
}

/// The other half of #25: `-f` exists to introduce a path that begins with a
/// character the expression parser would claim, and option parsing continues
/// past its value — `find -f dir -name x` is "illegal option -- n". Both facts
/// are BSD-specific, so this skips on a GNU host.
#[tokio::test]
async fn test_find_f_option_carries_a_dash_leading_path() {
    if !binary_exists("/usr/bin/find") || !find_is_bsd() {
        eprintln!("skipping: `-f` is a BSD find option and this host has neither");
        return;
    }
    let sandbox = tempfile::tempdir().expect("sandbox");
    let weird = sandbox.path().join("-weird-dir");
    std::fs::create_dir(&weird).unwrap();
    std::fs::write(weird.join("wanted.txt"), b"x").unwrap();

    // The value is relative to the sandbox, which is how the caller would have
    // to write a name whose first character is `-`.
    let args = build_arguments(
        &kwargs(&[("f", json!(["-weird-dir"])), ("name", json!("*.txt"))]),
        Some(&find_schema(true)),
    )
    .expect("`-f` exists precisely for a path that begins with '-'");

    assert_eq!(
        args,
        vec!["-f", "-weird-dir", "--", "-name", "*.txt"],
        "the separator belongs after the pre-operand flags"
    );

    let mut command = std::process::Command::new("/usr/bin/find");
    let out = command
        .current_dir(sandbox.path())
        .args(&args)
        .output()
        .expect("find should run");

    assert!(
        out.status.success(),
        "find rejected {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("wanted.txt"),
        "the expression did not take effect: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The default must not move: a tool whose overlay has not stated that it
/// honours `--` keeps refusing a value that begins with `-`. Guessing where the
/// separator goes would turn a clean refusal into a failing invocation —
/// `find . -- -name '*.txt'` is rejected by BSD and GNU alike.
#[test]
fn test_option_like_values_stay_refused_without_the_marker() {
    let err = build_arguments(
        &kwargs(&[
            ("path", json!(["/tmp"])),
            ("expression", json!(["-name", "*.txt"])),
        ]),
        Some(&find_schema(false)),
    )
    .expect_err("without the marker the guard must still refuse");

    assert_eq!(err.code, apcore::ErrorCode::GeneralInvalidInput);
    assert!(
        err.message.contains("Element 0 of parameter 'expression'"),
        "the message must name the offending element: {}",
        err.message
    );
}
