//! Run the portable real CLI matrix against the compiled Clap interface.
#![cfg(unix)]
use clap::CommandFactory;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

fn argument_manifest(argument: &clap::Arg) -> Value {
    let values: Vec<String> = argument
        .get_value_parser()
        .possible_values()
        .into_iter()
        .flatten()
        .map(|value| value.get_name().to_owned())
        .collect();
    json!({
        "id": argument.get_id().as_str(),
        "long": argument.get_long().map(|value| format!("--{value}")),
        "short": argument.get_short().map(|value| format!("-{value}")),
        "global": argument.is_global_set(),
        "takes_value": argument.get_action().takes_values(),
        "index": argument.get_index(),
        "last": argument.is_last_set(),
        "values": values,
        "aliases": argument.get_all_aliases().unwrap_or_default().iter().map(|value| format!("--{value}")).collect::<Vec<_>>(),
        "short_aliases": argument.get_all_short_aliases().unwrap_or_default().iter().map(|value| format!("-{value}")).collect::<Vec<_>>(),
    })
}

fn interface_commands(command: &clap::Command, prefix: &str, result: &mut Vec<Value>) {
    let arguments: Vec<Value> = command.get_arguments().map(argument_manifest).collect();
    result.push(
        json!({"name": if prefix.is_empty() { "root" } else { prefix }, "arguments": arguments}),
    );
    for child in command
        .get_subcommands()
        .filter(|child| child.get_name() != "help")
    {
        let name = if prefix.is_empty() {
            child.get_name().to_owned()
        } else {
            format!("{prefix} {}", child.get_name())
        };
        interface_commands(child, &name, result);
        for alias in child.get_all_aliases() {
            let name = if prefix.is_empty() {
                alias.to_owned()
            } else {
                format!("{prefix} {alias}")
            };
            interface_commands(child, &name, result);
        }
    }
}

#[test]
fn test_all_cli_parameters_have_passing_runtime_scenarios() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = std::env::var_os("APEXE_RUNTIME_REPORT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("target/runtime-validation"));
    std::fs::create_dir_all(&output).unwrap();
    let mut command = apexe::cli::Cli::command();
    command.build();
    let mut commands = Vec::new();
    interface_commands(&command, "", &mut commands);
    let manifest = output.join("cli-interface.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec_pretty(&json!({"schema_version":1,"commands":commands})).unwrap(),
    )
    .unwrap();
    let python = which::which("python3").expect("runtime checks require Python 3");
    let mut check = assert_cmd::Command::new(python);
    check
        .arg(repository.join("scripts/validate_cli.py"))
        .args(["--apexe", env!("CARGO_BIN_EXE_apexe"), "--manifest"])
        .arg(manifest)
        .arg("--report")
        .arg(output.join("cli.json"))
        .current_dir(&repository)
        .timeout(Duration::from_secs(300));
    if let Ok(shells) = std::env::var("APEXE_RUNTIME_REQUIRED_SHELLS") {
        for shell in shells.split(',').filter(|shell| !shell.is_empty()) {
            check.args(["--require-shell", shell]);
        }
    }
    check.assert().success();
}
