use super::{result, IsolationMode, ModuleVerification};
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
    #[serde(skip)]
    pub source: Option<super::CaseSource>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Case {
    pub(super) module_id: String,
    pub(super) input: Value,
    pub(super) fixtures: BTreeMap<String, String>,
    pub(super) expected_stdout: String,
    #[serde(default)]
    pub(super) expected_stderr: String,
    #[serde(default)]
    pub(super) mode: CaseMode,
    #[serde(default)]
    pub(super) binary_digest: Option<String>,
    #[serde(default)]
    pub(super) dry_run_parameter: Option<String>,
    #[serde(default)]
    pub(super) expected_files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CaseMode {
    #[default]
    Readonly,
    DryRun,
    Isolated,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct BehaviorOptions {
    pub allow_dry_run: bool,
    pub isolation: IsolationMode,
}

pub(crate) fn read_cases(path: &Path) -> anyhow::Result<Cases> {
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= 1024 * 1024,
        "Behavioral cases exceed 1 MiB"
    );
    let bytes = std::fs::read(path)?;
    let mut cases: Cases = serde_json::from_slice(&bytes)?;
    use sha2::{Digest, Sha256};
    cases.source = Some(super::CaseSource {
        path: std::path::absolute(path)?,
        digest: format!("{:x}", Sha256::digest(&bytes)),
    });
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
    options: BehaviorOptions,
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
        let admitted = tokio::task::spawn_blocking(move || {
            admit(&owned_case, &allowlist, &owned_module, options)
        })
        .await?;
        let outcome = match admitted {
            Err(error) => super::runner::CaseOutcome::unverifiable(error.to_string()),
            Ok(()) => super::runner::run_case(&case, module, bindings, timeout, options).await,
        };
        let module = module.clone();
        entries.push(
            tokio::task::spawn_blocking(move || {
                let mut entry = result(&module, outcome.status, outcome.reason)?;
                entry.case_digest = Some(super::digest(&case)?);
                entry.mode = Some(
                    match case.mode {
                        CaseMode::Readonly => "readonly",
                        CaseMode::DryRun => "dry_run",
                        CaseMode::Isolated => "isolated",
                    }
                    .into(),
                );
                entry.isolation = outcome.isolation;
                Ok::<_, anyhow::Error>(entry)
            })
            .await??,
        );
    }
    Ok(entries)
}

fn admit(
    case: &Case,
    allowlist: &BTreeSet<String>,
    module: &ScannedModule,
    options: BehaviorOptions,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        allowlist.contains(&case.module_id),
        "Module is not explicitly allowlisted"
    );
    let annotations = module
        .annotations
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No behavioral annotations"))?;
    anyhow::ensure!(
        !annotations.open_world,
        "Networked modules are never behaviorally verified"
    );
    anyhow::ensure!(
        !annotations.readonly
            || case
                .expected_files
                .iter()
                .all(|(name, contents)| case.fixtures.get(name) == Some(contents)),
        "Readonly annotations conflict with expected fixture writes"
    );
    if case.mode == CaseMode::DryRun {
        admit_dry_run(case, module, options)?;
    } else {
        anyhow::ensure!(
            (annotations.readonly || annotations.idempotent)
                && !annotations.destructive
                && !annotations.requires_approval,
            "Behavioral verification refuses destructive or approval-required modules"
        );
    }
    let target = validate_target(module, case.mode)?;
    if case.mode == CaseMode::Readonly {
        admit_readonly_binary(&target)?;
    } else {
        anyhow::ensure!(
            case.binary_digest.as_ref() == Some(&super::binary_digest(module)?),
            "An explicit matching binary_digest is required for this case mode"
        );
    }
    validate_case_shape(case, module)
}

pub(super) fn validate_target(
    module: &ScannedModule,
    mode: CaseMode,
) -> anyhow::Result<std::path::PathBuf> {
    let target = module
        .target
        .strip_prefix("exec://")
        .ok_or_else(|| anyhow::anyhow!("Not a CLI target"))?;
    let words = shell_words::split(target)?;
    let binary = words
        .first()
        .ok_or_else(|| anyhow::anyhow!("Missing CLI binary"))?;
    anyhow::ensure!(
        Path::new(binary).is_absolute(),
        "Verification requires an absolute binary"
    );
    if words.len() > 1 {
        anyhow::ensure!(
            mode != CaseMode::Readonly,
            "Readonly admission covers root binaries only"
        );
        let declared = module
            .metadata
            .get("command_path")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("Subcommand targets require command_path metadata"))?;
        let declared: Vec<_> = declared.iter().skip(1).filter_map(Value::as_str).collect();
        anyhow::ensure!(
            words[1..].iter().map(String::as_str).eq(declared),
            "Fixed subcommands must match command_path"
        );
        anyhow::ensure!(
            words[1..].iter().all(|word| !word.starts_with('-')
                && word.chars().all(
                    |character| character.is_ascii_alphanumeric() || "-_ .".contains(character)
                )
                && !word.contains(' ')),
            "Only literal subcommand names are admitted; execution wrappers are refused"
        );
    }
    Ok(binary.into())
}

fn validate_case_shape(case: &Case, module: &ScannedModule) -> anyhow::Result<()> {
    anyhow::ensure!(case.input.is_object(), "Case input must be an object");
    anyhow::ensure!(
        !module.metadata.contains_key("json_flag"),
        "Verification refuses implicit JSON-output flags"
    );
    anyhow::ensure!(
        case.expected_stdout.len() <= 1024 * 1024,
        "Expected output exceeds 1 MiB"
    );
    anyhow::ensure!(
        case.expected_stderr.len() <= 1024 * 1024,
        "Expected stderr exceeds 1 MiB"
    );
    anyhow::ensure!(case.expected_files.len() <= 32, "Too many expected files");
    Ok(())
}

fn admit_dry_run(
    case: &Case,
    module: &ScannedModule,
    options: BehaviorOptions,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        options.allow_dry_run,
        "Dry-run cases require --allow-dry-run"
    );
    anyhow::ensure!(
        !matches!(options.isolation, IsolationMode::None),
        "Dry-run cases require operating-system isolation"
    );
    let parameter = case
        .dry_run_parameter
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Declare the reviewed dry_run_parameter"))?;
    anyhow::ensure!(
        case.input[parameter] == true
            && module.input_schema["properties"][parameter]["type"] == "boolean",
        "The declared dry-run boolean parameter must be true"
    );
    let flag = module.input_schema["properties"][parameter]["x-apexe-flag"]
        .as_str()
        .unwrap_or_default();
    anyhow::ensure!(
        ["--dry-run", "--check", "--noop", "--plan", "--whatif"].contains(&flag),
        "The parameter must declare a supported literal dry-run switch"
    );
    Ok(())
}

fn admit_readonly_binary(target: &Path) -> anyhow::Result<()> {
    let binary = target.canonicalize()?;
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
    Ok(())
}

#[derive(Debug)]
pub(super) struct PreparedCase {
    pub(super) temporary: tempfile::TempDir,
    pub(super) bindings: std::path::PathBuf,
    pub(super) work: std::path::PathBuf,
    pub(super) home: std::path::PathBuf,
    pub(super) input: Value,
    pub(super) acl: std::path::PathBuf,
    pub(super) binary_digest: String,
}

pub(super) fn prepare_case(case: &Case, module: &ScannedModule) -> anyhow::Result<PreparedCase> {
    let binary_digest = super::binary_digest(module)?;
    anyhow::ensure!(
        case.binary_digest
            .as_ref()
            .is_none_or(|digest| digest == &binary_digest),
        "Reviewed binary changed after admission"
    );
    let temporary = tempfile::tempdir()?;
    let work = temporary.path().join("work");
    let home = temporary.path().join("home");
    std::fs::create_dir(&work)?;
    std::fs::create_dir(&home)?;
    write_fixtures(case, &work)?;
    let input = resolve_input(
        &case.input,
        &module.input_schema,
        &work,
        case.mode == CaseMode::Readonly,
    )?;
    let acl = temporary.path().join("verification-acl.yaml");
    std::fs::write(
        &acl,
        serde_yaml::to_string(&serde_json::json!({
            "version":"1.0", "default_effect":"deny", "rules":[{
                "callers":["*"], "targets":[module.module_id], "effect":"allow"
            }]
        }))?,
    )?;
    let bindings = write_scoped_binding(case, module, temporary.path())?;
    Ok(PreparedCase {
        acl,
        bindings,
        temporary,
        work,
        home,
        input,
        binary_digest,
    })
}

fn write_scoped_binding(
    case: &Case,
    module: &ScannedModule,
    directory: &Path,
) -> anyhow::Result<std::path::PathBuf> {
    let bindings = directory.join("bindings");
    let mut scoped = module.clone();
    if case.mode == CaseMode::DryRun {
        let annotations = scoped
            .annotations
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing annotations"))?;
        // This invocation is explicitly opted in, digest-pinned and sandboxed read-only.
        annotations.requires_approval = false;
        annotations.destructive = false;
        annotations.readonly = true;
    }
    crate::output::YamlOutput::without_verification().write(&[scoped], &bindings, false)?;
    Ok(bindings)
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

fn resolve_input(
    input: &Value,
    schema: &Value,
    work: &Path,
    require_paths: bool,
) -> anyhow::Result<Value> {
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
        paths > 0 || !require_paths,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn module() -> ScannedModule {
        ScannedModule::new(
            "cli.fixture".into(),
            "Fixture".into(),
            json!({"type":"object","properties":{"file":{"type":"string","x-apexe-path":true,"x-apexe-positional":0}}}),
            json!({"type":"object"}),
            vec![],
            "exec:///bin/cat".into(),
        )
    }

    #[test]
    fn test_subcommand_targets_require_exact_literal_command_paths() {
        let mut module = module();
        module.target = "exec:///bin/cat inspect-file".into();
        module
            .metadata
            .insert("command_path".into(), json!(["cat", "inspect-file"]));
        assert!(validate_target(&module, CaseMode::Isolated).is_ok());
        assert!(validate_target(&module, CaseMode::Readonly).is_err());
        module
            .metadata
            .insert("command_path".into(), json!(["cat", "different"]));
        assert!(validate_target(&module, CaseMode::DryRun).is_err());
        module.target = "exec:///bin/sh -c".into();
        module
            .metadata
            .insert("command_path".into(), json!(["sh", "-c"]));
        assert!(validate_target(&module, CaseMode::Isolated).is_err());
    }

    #[test]
    fn test_preparation_rechecks_the_admitted_binary_digest() {
        let module = module();
        let case: Case =
            serde_json::from_value(json!({"module_id":"cli.fixture","mode":"isolated",
            "binary_digest":"0".repeat(64),"input":{"file":"$FIXTURE/a"},"fixtures":{"a":"data"},
            "expected_stdout":"data"}))
            .unwrap();
        assert!(prepare_case(&case, &module)
            .unwrap_err()
            .to_string()
            .contains("changed after admission"));
    }
}
