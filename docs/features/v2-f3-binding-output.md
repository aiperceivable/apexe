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
pub mod registry;
pub mod yaml;

pub use loader::load_modules_from_dir;
pub use registry::RegistryOutput;
pub use yaml::YamlOutput;
```

### 2.2 `src/output/yaml.rs` -- YamlOutput

```rust
use std::path::Path;
use apcore::ModuleError;
use apcore_toolkit::{ScannedModule, WriteResult, YAMLWriter, YAMLVerifier, SyntaxVerifier, Verifier};

/// Writes ScannedModules to .binding.yaml files using apcore-toolkit's YAMLWriter.
pub struct YamlOutput {
    /// Underlying toolkit writer.
    writer: YAMLWriter,
    /// Verifiers to run before writing.
    verifiers: Vec<Box<dyn Verifier>>,
}

impl YamlOutput {
    /// Create a new YamlOutput with default verifiers (YAML syntax + structure).
    pub fn new() -> Self {
        Self {
            writer: YAMLWriter,
            verifiers: vec![
                Box::new(YAMLVerifier),
                Box::new(SyntaxVerifier),
            ],
        }
    }

    /// Create a YamlOutput with no verifiers (for testing or speed).
    pub fn without_verification() -> Self {
        Self {
            writer: YAMLWriter,
            verifiers: vec![],
        }
    }

    /// Write modules to YAML binding files in the given directory.
    ///
    /// Steps:
    /// 1. Group modules by tool name (extracted from module_id prefix).
    /// 2. For each group, call YAMLWriter::write() with the module list.
    /// 3. If verify is true, run all verifiers on the output.
    /// 4. Return Vec<WriteResult> with paths and verification status.
    pub fn write(
        &self,
        modules: &[ScannedModule],
        output_dir: &Path,
        dry_run: bool,
        verify: bool,
    ) -> Result<Vec<WriteResult>, ModuleError>;
}
```

**Write logic**:

```
1. Create output_dir if it does not exist.
2. Group modules by extracting tool name from module_id:
   "cli.git.commit" -> tool_name = "git"
   "cli.docker.container.ls" -> tool_name = "docker"
3. For each tool group:
   a. filename = "{tool_name}.binding.yaml"
   b. Call self.writer.write(
        &group_modules,
        output_dir,
        dry_run,
        verify,
        &self.verifiers,
      )
   c. Collect WriteResults
4. Return all WriteResults.
```

### 2.3 `src/output/registry.rs` -- RegistryOutput

```rust
use std::sync::Arc;
use apcore::{ModuleError, Registry};
use apcore_toolkit::{RegistryWriter, ScannedModule, HandlerFactory};

use crate::module::CliModule;
use crate::governance::{AuditManager, SandboxManager};

/// Registers ScannedModules directly into an apcore Registry as CliModules.
pub struct RegistryOutput {
    writer: RegistryWriter,
}

impl RegistryOutput {
    /// Create a new RegistryOutput with a handler factory that produces CliModules.
    pub fn new(
        timeout_ms: u64,
        sandbox: Option<Arc<SandboxManager>>,
        audit: Option<Arc<AuditManager>>,
    ) -> Self {
        let timeout = timeout_ms;
        let sb = sandbox.clone();
        let au = audit.clone();

        let factory: HandlerFactory = Box::new(move |module: &ScannedModule| {
            Box::new(CliModule::from_scanned(module, timeout, sb.clone(), au.clone())?)
        });

        Self {
            writer: RegistryWriter::with_handler_factory(factory),
        }
    }

    /// Register all modules into the given registry.
    ///
    /// Steps:
    /// 1. For each ScannedModule, create a CliModule via the handler factory.
    /// 2. Register the CliModule in the registry.
    /// 3. Return count of registered modules or error.
    pub fn register(
        &self,
        modules: &[ScannedModule],
        registry: &Registry,
        dry_run: bool,
        verify: bool,
    ) -> Result<usize, ModuleError>;
}
```

### 2.4 `src/output/loader.rs` -- Module Loader

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
| `test_yaml_output_writes_file` | 3 modules from "git" tool | `git.binding.yaml` created |
| `test_yaml_output_groups_by_tool` | Modules from "git" and "docker" | Two files: `git.binding.yaml`, `docker.binding.yaml` |
| `test_yaml_output_file_is_valid_yaml` | Write and re-read | Deserialized modules match originals |
| `test_yaml_output_dry_run_no_files` | dry_run = true | No files created, WriteResults returned |
| `test_yaml_output_verify_catches_invalid` | Malformed module (empty module_id) | WriteResult.verification_error set |
| `test_yaml_output_creates_directory` | output_dir does not exist | Directory created, file written |
| `test_yaml_output_overwrites_existing` | File already exists | File overwritten with new content |
| `test_yaml_output_empty_modules` | Empty module list | No files created, empty results |
| `test_yaml_output_without_verification` | Use without_verification() | No verification errors even for edge cases |

### 5.2 RegistryOutput Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_registry_output_registers_modules` | 3 modules | Registry contains 3 entries |
| `test_registry_output_dry_run` | dry_run = true | Registry unchanged, returns count |
| `test_registry_output_creates_cli_modules` | Register and execute | Module executes CLI command |
| `test_registry_output_duplicate_ids_handled` | Two modules with same ID | Deduplication applied |

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
| `GeneratedBindingFile` | Vec<ScannedModule> grouped by tool |
| `BindingGenerator` | `CliToolConverter` (F1) + `YamlOutput` |
| `SchemaGenerator` | `adapter::schema` module (F1) |
| `BindingYAMLWriter` | `YamlOutput` wrapping `YAMLWriter` |

### Test Migration

63 binding tests are deleted. 20 new output tests replace them. The test count is lower because:
- Schema generation tests move to F1 (adapter).
- Module ID generation tests move to F1 (adapter).
- The remaining tests focus on write/load behavior, not generation logic.
