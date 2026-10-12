use super::{result, ModuleVerification, VerificationStatus};
use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cases {
    allowlist: BTreeSet<String>,
    cases: Vec<Case>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Case {
    module_id: String,
    input: Value,
    fixtures: BTreeMap<String, String>,
    expected_stdout: String,
}

pub(crate) fn read_cases(path: &Path) -> anyhow::Result<Cases> {
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= 1024 * 1024,
        "Behavioral cases exceed 1 MiB"
    );
    let cases: Cases = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(
        !cases.cases.is_empty() && cases.cases.len() <= 100,
        "Provide 1 to 100 cases"
    );
    Ok(cases)
}

pub(crate) async fn run_cases(
    cases: Cases,
    modules: &[ScannedModule],
    bindings: &Path,
    timeout: u64,
) -> anyhow::Result<Vec<ModuleVerification>> {
    let mut entries = Vec::new();
    for case in cases.cases {
        let module = modules
            .iter()
            .find(|module| module.module_id == case.module_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown behavioral module '{}'", case.module_id))?;
        let owned_case = case.clone();
        let owned_module = module.clone();
        let allowlist = cases.allowlist.clone();
        let admitted =
            tokio::task::spawn_blocking(move || admit(&owned_case, &allowlist, &owned_module))
                .await?;
        let (status, reason) = match admitted {
            Err(error) => (VerificationStatus::Unverifiable, Some(error.to_string())),
            Ok(()) => match run_case(&case, module, bindings, timeout).await {
                Ok(()) => (VerificationStatus::Passed, None),
                Err(error) => (VerificationStatus::Failed, Some(error.to_string())),
            },
        };
        let module = module.clone();
        entries.push(
            tokio::task::spawn_blocking(move || {
                let mut entry = result(&module, status, reason)?;
                entry.case_digest = Some(super::digest(&case)?);
                Ok::<_, anyhow::Error>(entry)
            })
            .await??,
        );
    }
    Ok(entries)
}

fn admit(case: &Case, allowlist: &BTreeSet<String>, module: &ScannedModule) -> anyhow::Result<()> {
    anyhow::ensure!(
        allowlist.contains(&case.module_id),
        "Module is not explicitly allowlisted"
    );
    let annotations = module
        .annotations
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No behavioral annotations"))?;
    anyhow::ensure!(
        (annotations.readonly || annotations.idempotent)
            && !annotations.destructive
            && !annotations.requires_approval
            && !annotations.open_world,
        "Behavioral verification refuses destructive, approval-required or networked modules"
    );
    let target = module
        .target
        .strip_prefix("exec://")
        .ok_or_else(|| anyhow::anyhow!("Not a CLI target"))?;
    anyhow::ensure!(
        target.split_whitespace().count() == 1,
        "Only reviewed root binaries are admitted"
    );
    let binary = Path::new(target).canonicalize()?;
    let allowed = [
        "/bin/cat",
        "/usr/bin/cat",
        "/bin/ls",
        "/usr/bin/ls",
        "/bin/head",
        "/usr/bin/head",
    ];
    anyhow::ensure!(
        allowed
            .iter()
            .filter_map(|path| Path::new(path).canonicalize().ok())
            .any(|path| path == binary),
        "Binary is outside the independently reviewed readonly allowlist"
    );
    anyhow::ensure!(case.input.is_object(), "Case input must be an object");
    anyhow::ensure!(
        !module.metadata.contains_key("json_flag"),
        "Reviewed readonly tools have no JSON-output flag"
    );
    anyhow::ensure!(
        case.expected_stdout.len() <= 1024 * 1024,
        "Expected output exceeds 1 MiB"
    );
    Ok(())
}

struct PreparedCase {
    _temporary: tempfile::TempDir,
    work: std::path::PathBuf,
    home: std::path::PathBuf,
    input: Value,
    acl: std::path::PathBuf,
    binary_digest: String,
}

fn prepare_case(case: &Case, module: &ScannedModule) -> anyhow::Result<PreparedCase> {
    let temporary = tempfile::tempdir()?;
    let work = temporary.path().join("work");
    let home = temporary.path().join("home");
    std::fs::create_dir(&work)?;
    std::fs::create_dir(&home)?;
    write_fixtures(case, &work)?;
    let input = resolve_input(&case.input, &module.input_schema, &work)?;
    let acl = temporary.path().join("verification-acl.yaml");
    std::fs::write(
        &acl,
        serde_yaml::to_string(&serde_json::json!({
            "version":"1.0", "default_effect":"deny", "rules":[{
                "callers":["*"], "targets":[module.module_id], "effect":"allow"
            }]
        }))?,
    )?;
    Ok(PreparedCase {
        acl,
        _temporary: temporary,
        work,
        home,
        input,
        binary_digest: super::binary_digest(module)?,
    })
}

async fn run_case(
    case: &Case,
    module: &ScannedModule,
    bindings: &Path,
    timeout: u64,
) -> anyhow::Result<()> {
    let owned_case = case.clone();
    let owned_module = module.clone();
    let prepared =
        tokio::task::spawn_blocking(move || prepare_case(&owned_case, &owned_module)).await??;
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .args(["run", &case.module_id, "--bindings-dir"])
        .arg(std::path::absolute(bindings)?)
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
        .kill_on_drop(true);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout.saturating_add(2)),
        command.output(),
    )
    .await??;
    anyhow::ensure!(output.status.success(), "Governed behavioral call failed");
    let output: Value = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(
        output["exit_code"] == 0 && output["stdout"] == case.expected_stdout,
        "Behavioral output did not match the explicit expectation"
    );
    let owned_module = module.clone();
    let after = tokio::task::spawn_blocking(move || super::binary_digest(&owned_module)).await??;
    anyhow::ensure!(
        prepared.binary_digest == after,
        "Binary changed during verification"
    );
    Ok(())
}

fn write_fixtures(case: &Case, work: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(case.fixtures.len() <= 32, "Too many fixtures");
    for (name, contents) in &case.fixtures {
        anyhow::ensure!(
            Path::new(name).components().count() == 1
                && !matches!(name.as_str(), "." | "..")
                && !Path::new(name).is_absolute(),
            "Fixture name must be a single relative component"
        );
        anyhow::ensure!(contents.len() <= 1024 * 1024, "Fixture exceeds 1 MiB");
        std::fs::write(work.join(name), contents)?;
    }
    Ok(())
}

fn resolve_input(input: &Value, schema: &Value, work: &Path) -> anyhow::Result<Value> {
    let mut input = input.clone();
    let mut paths = 0;
    for (name, value) in input
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Expected object"))?
    {
        anyhow::ensure!(
            schema["properties"][name]["x-apexe-long-running"] != true,
            "Long-running options are not verifiable"
        );
        if !crate::module::executor::value_is_path(Some(schema), name) {
            anyhow::ensure!(
                value.is_boolean() || value.is_number(),
                "Non-path inputs must be boolean or numeric in the reviewed verifier"
            );
            let flag = schema["properties"][name]["x-apexe-flag"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing literal option spelling"))?;
            anyhow::ensure!(
                flag.starts_with('-')
                    && flag.len() > 1
                    && flag
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '-'),
                "Invalid literal option spelling"
            );
            continue;
        }
        if let Some(values) = value.as_array_mut() {
            for value in values {
                resolve_path(value, work)?;
                paths += 1;
            }
        } else {
            resolve_path(value, work)?;
            paths += 1;
        }
    }
    anyhow::ensure!(
        paths > 0,
        "Cases must supply fixture path operands; ambient paths are never sampled"
    );
    Ok(input)
}

fn resolve_path(value: &mut Value, work: &Path) -> anyhow::Result<()> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Fixture path must be text"))?;
    let path = if text == "$WORKDIR" {
        work.to_path_buf()
    } else {
        let name = text
            .strip_prefix("$FIXTURE/")
            .ok_or_else(|| anyhow::anyhow!("Path operands must use $WORKDIR or $FIXTURE/name"))?;
        anyhow::ensure!(
            Path::new(name).components().count() == 1
                && !matches!(name, "." | "..")
                && !Path::new(name).is_absolute(),
            "Invalid fixture path"
        );
        work.join(name)
    };
    anyhow::ensure!(path.exists(), "Fixture does not exist");
    *value = serde_json::json!(path);
    Ok(())
}
