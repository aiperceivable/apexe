# F3: Binding Output -- Replace Binding Generator with apcore-toolkit Writers

| Field | Value |
|---|---|
| **Feature ID** | F3 |
| **Tech Design Section** | 5.3 |
| **Priority** | P1 (Output) |
| **Dependencies** | F1 (Scanner Adapter) |
| **Depended On By** | F4 (MCP Server) |
| **New Files** | `src/output/mod.rs`, `src/output/yaml.rs`, `src/output/registry.rs`, `src/output/loader.rs` |
| **Deleted Files** | `src/binding/binding_gen.rs`, `src/binding/schema_gen.rs`, `src/binding/module_id.rs`, `src/binding/writer.rs`, `src/binding/mod.rs` |
| **Estimated LOC** | ~400 |
| **Estimated Tests** | ~20 |

---

## 1. Purpose

Replace apexe's custom binding generator (`BindingGenerator`, `SchemaGenerator`, `BindingYAMLWriter`) with apcore-toolkit's standardized output pipeline (`YAMLWriter`, `RegistryWriter`, `Verifier`). This gains output verification, display metadata resolution, and consistency with other apcore ecosystem tools.

---

## 2. Module Structure

### 2.1 `src/output/mod.rs`

```rust
pub mod loader;
pub mod skill;
pub mod yaml;

pub use loader::load_modules_from_dir;
pub use skill::SkillOutput;
pub use yaml::YamlOutput;
```

**`RegistryOutput` (originally planned for this module, §2.3 in earlier
drafts) was never built** — registration into an apcore `Registry` is
instead `crate::module::build_executor` (`src/module/registry.rs`), which
reads the on-disk `.binding.yaml` files written by `YamlOutput` rather than
registering directly from `Vec<ScannedModule>`, and has no `dry_run`
parameter. `SkillOutput` (writing a Claude Skill `SKILL.md` per module) was
added instead, later than this spec, and is not described here — see
`src/output/skill.rs`.

### 2.2 `src/output/yaml.rs` -- YamlOutput

**One file per module, not grouped by tool** — an earlier draft of this
section proposed grouping (`"cli.git.commit"` and `"cli.docker.container.ls"`
into `git.binding.yaml` / `docker.binding.yaml`); that was never built.
`YamlOutput` writes `{sanitized_module_id}.binding.yaml` per module, and
`verify` is a constructor choice (`new()` vs. `without_verification()`), not
a per-call parameter.

```rust
use std::path::Path;
use apcore::ModuleError;
use apcore_toolkit::{ScannedModule, WriteResult, YAMLWriter, YAMLVerifier, Verifier};

/// Wraps apcore-toolkit's YAMLWriter to write ScannedModules as `.binding.yaml` files.
pub struct YamlOutput {
    writer: YAMLWriter,
    verify: bool,
}

impl YamlOutput {
    /// Create a new YamlOutput with verification enabled.
    pub fn new() -> Self {
        Self { writer: YAMLWriter, verify: true }
    }

    /// Create a new YamlOutput with verification disabled.
    pub fn without_verification() -> Self {
        Self { writer: YAMLWriter, verify: false }
    }

    /// Write modules to YAML binding files in `output_dir`.
    ///
    /// Each module is written to its own file:
    /// `{sanitized_module_id}.binding.yaml`. Returns a `WriteResult` per
    /// module written.
    pub fn write(
        &self,
        modules: &[ScannedModule],
        output_dir: &Path,
        dry_run: bool,
    ) -> Result<Vec<WriteResult>, ModuleError>;
}
```

### 2.3 `src/output/loader.rs` -- Module Loader

```rust
use std::path::Path;
use apcore::ModuleError;
use apcore_toolkit::{DisplayResolver, ScannedModule};

/// Load ScannedModules from .binding.yaml files in a directory.
///
/// Uses DisplayResolver to merge display metadata from files.
pub fn load_modules_from_dir(dir: &Path) -> Result<Vec<ScannedModule>, ModuleError>;
```

**Load logic**:

```
1. Read all *.binding.yaml files from dir.
2. For each file, deserialize YAML into Vec<ScannedModule>.
3. Use DisplayResolver to resolve display metadata.
4. Flatten into single Vec<ScannedModule>.
5. Return modules or error.
```

---

## 3. File Format Compatibility

The output YAML format must be readable by apcore-toolkit and by the `load_modules_from_dir()` loader. The format is defined by apcore-toolkit's `YAMLWriter` and looks like:

```yaml
# git.binding.yaml
modules:
  - module_id: cli.git.commit
    description: "Record changes to the repository"
    input_schema:
      type: object
      properties:
        message:
          type: string
          description: "Commit message"
      required: [message]
    output_schema:
      type: object
      properties:
        stdout: { type: string }
        stderr: { type: string }
        exit_code: { type: integer }
    tags: [cli, git, gnu]
    target: "exec:///usr/bin/git commit"
    version: "2.43.0"
    annotations:
      readonly: false
      destructive: false
      idempotent: false
    examples:
      - "git commit -m 'initial commit'"
    warnings: []
```

This replaces the v0.1.x format which had a different structure (`bindings:` key with `metadata` subfields). The migration is clean because the loader reads the new format exclusively.

---

## 4. Integration with CLI

### 4.1 Updated ScanArgs::execute()

```rust
// In src/cli/mod.rs
impl ScanArgs {
    pub fn execute(self, config: &ApexeConfig) -> Result<(), ModuleError> {
        let orchestrator = ScanOrchestrator::new(config.clone());
        let scanned_tools = orchestrator.scan(&self.tools, self.no_cache, self.depth)?;

        let converter = CliToolConverter::new();
        let modules: Vec<ScannedModule> = scanned_tools
            .iter()
            .flat_map(|tool| converter.convert(tool))
            .collect();

        let output_dir = self.output_dir
            .unwrap_or_else(|| config.modules_dir.clone());

        let yaml_output = YamlOutput::new();
        let results = yaml_output.write(&modules, &output_dir, self.dry_run, self.verify)?;

        // Display results
        for result in &results {
            println!("Written: {} (verified: {})", result.path.display(), result.verified);
        }

        // Generate ACL (calls into F5)
        let acl = AclManager::generate_default(&modules);
        // ... write ACL file

        Ok(())
    }
}
```

---

## 5. Test Scenarios

### 5.1 YamlOutput Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_yaml_output_writes_file` | 3 modules from "git" tool | 3 files, one per module (§2.2 — grouping by tool was never implemented) |
| `test_yaml_output_file_is_valid_yaml` | Write and re-read | Deserialized modules match originals |
| `test_yaml_output_dry_run_no_files` | dry_run = true | No files created, WriteResults returned |
| `test_yaml_output_verify_catches_invalid` | Malformed module (empty module_id) | WriteResult.verification_error set |
| `test_yaml_output_creates_directory` | output_dir does not exist | Directory created, file written |
| `test_yaml_output_overwrites_existing` | File already exists | File overwritten with new content |
| `test_yaml_output_empty_modules` | Empty module list | No files created, empty results |
| `test_yaml_output_without_verification` | Use without_verification() | No verification errors even for edge cases |

### 5.2 Registration Tests

**There is no `RegistryOutput` type or `dry_run` at registration (§2.1) —
registration is `crate::module::build_executor` (`src/module/registry.rs`),
which reads bindings from disk rather than taking `Vec<ScannedModule>`
directly.**

| Test Name | Scenario | Expected |
|---|---|---|
| registration-count coverage | 3 bindings on disk | `Registry::count()` and descriptor fields match — see `registry.rs`'s own `build_executor` tests |
| `test_mcp_tools_call_executes` (`tests/mcp_integration.rs`) | Register and execute | Module executes CLI command |
| duplicate-id handling at registration | Two bindings resolving to the same `module_id` | Currently untested at the registration layer — `deduplicate_ids` runs upstream in `CliToolConverter::convert` (single-tool only, see F1 §5); `register_modules` warns and drops the loser but no test drives that path directly |

### 5.3 Loader Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_loader_reads_binding_files` | Directory with 2 .binding.yaml files | All modules loaded |
| `test_loader_empty_directory` | Empty dir | Empty Vec returned |
| `test_loader_nonexistent_directory` | Dir does not exist | Err(ModuleError) |
| `test_loader_ignores_non_yaml` | Dir with .txt files | Only .binding.yaml processed |
| `test_loader_handles_malformed_yaml` | Invalid YAML content | Err with descriptive message |

---

## 6. Migration Notes

### Deleted Types

| v0.1.x Type | Replacement |
|---|---|
| `GeneratedBinding` | `ScannedModule` from apcore-toolkit |
| `GeneratedBindingFile` | `Vec<ScannedModule>`, one `.binding.yaml` file per module |
| `BindingGenerator` | `CliToolConverter` (F1) + `YamlOutput` |
| `SchemaGenerator` | `adapter::schema` module (F1) |
| `BindingYAMLWriter` | `YamlOutput` wrapping `YAMLWriter` |

### Test Migration

63 binding tests are deleted. 20 new output tests replace them. The test count is lower because:
- Schema generation tests move to F1 (adapter).
- Module ID generation tests move to F1 (adapter).
- The remaining tests focus on write/load behavior, not generation logic.
