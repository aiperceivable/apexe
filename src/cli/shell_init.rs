use super::shell::Shell;
use crate::runtime_path::MANIFEST_NAME;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

const STATE_NAME: &str = ".apexe-shell-init.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Initialization {
    version: u32,
    files: Vec<StartupFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StartupFile {
    path: PathBuf,
    block: String,
}

struct FileEdit {
    path: PathBuf,
    original: Option<String>,
    replacement: Option<String>,
    permissions: Option<std::fs::Permissions>,
}

pub(super) fn initialize(
    directory: &Path,
    shell: Option<Shell>,
    rc_file: Option<&Path>,
    install: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    std::env::join_paths([directory])?;
    let shell = shell.map(Ok).unwrap_or_else(Shell::detect)?;
    let paths = startup_paths(shell, rc_file)?;
    let mut state = read_state(directory)?.unwrap_or(Initialization {
        version: 1,
        files: Vec::new(),
    });
    let block = activation_block(directory, shell)?;
    let mut edits = Vec::new();
    for path in paths {
        let path = resolve_path(&path)?;
        anyhow::ensure!(
            !path.starts_with(resolve_path(directory)?),
            "Shell startup files must be outside the shim directory"
        );
        let previous = state.files.iter().position(|file| file.path == path);
        let old_block = previous.map(|index| state.files[index].block.as_str());
        let (edit, installed_block) = plan_block(&path, old_block, Some(&block))?;
        edits.push(edit);
        let file = StartupFile {
            path,
            block: installed_block,
        };
        if let Some(index) = previous {
            state.files[index] = file;
        } else {
            state.files.push(file);
        }
    }
    edits.push(plan_state(directory, Some(&state))?);
    // Check startup ownership and syntax before installing any command links.
    install()?;
    apply_edits(&edits)?;
    for file in &state.files {
        tracing::info!(path = %file.path.display(), "Configured shim activation for new terminals");
    }
    tracing::info!(directory = %directory.display(), "Open a new terminal after installing shims; the current shell is unchanged");
    Ok(())
}

pub(super) fn disable(
    directory: &Path,
    remove_links: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let state = read_state(directory)?;
    let has_manifest = directory.join(MANIFEST_NAME).try_exists()?;
    if state.is_none() && !has_manifest {
        tracing::info!(directory = %directory.display(), "Governed shims are already disabled");
        return Ok(());
    }
    let mut edits = Vec::new();
    if let Some(state) = state {
        for file in state.files {
            let (edit, _) = plan_block(&file.path, Some(&file.block), None)?;
            edits.push(edit);
        }
        edits.push(plan_state(directory, None)?);
    }
    // Validate every startup block before removing any command links.
    if has_manifest {
        remove_links()?;
    }
    apply_edits(&edits)
}

fn startup_paths(shell: Shell, rc_file: Option<&Path>) -> anyhow::Result<Vec<PathBuf>> {
    if let Some(path) = rc_file {
        return Ok(vec![std::path::absolute(path)?]);
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot locate home directory"))?;
    match shell {
        Shell::Sh => Ok(vec![home.join(".profile")]),
        Shell::Zsh => Ok(vec![environment_directory("ZDOTDIR", &home)?.join(".zshrc")]),
        Shell::Bash => {
            let mut login = home.join(".bash_profile");
            for name in [".bash_profile", ".bash_login", ".profile"] {
                let candidate = home.join(name);
                if candidate.try_exists()? {
                    login = candidate;
                    break;
                }
            }
            Ok(vec![home.join(".bashrc"), login])
        }
        Shell::Fish => Ok(vec![environment_directory(
            "XDG_CONFIG_HOME",
            &home.join(".config"),
        )?
        .join("fish/config.fish")]),
        Shell::Powershell => anyhow::bail!(
            "PowerShell initialization requires --rc-file pointing to the current $PROFILE"
        ),
    }
}

fn environment_directory(name: &str, fallback: &Path) -> anyhow::Result<PathBuf> {
    let path = std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback.to_path_buf());
    anyhow::ensure!(path.is_absolute(), "{name} must be an absolute directory");
    Ok(path)
}

fn markers(directory: &Path) -> anyhow::Result<(String, String)> {
    let directory = resolve_path(directory)?;
    let directory = path_text(&directory)?;
    let key = format!("{:x}", Sha256::digest(directory.as_bytes()));
    Ok((
        format!("# >>> apexe shims {key} >>>"),
        format!("# <<< apexe shims {key} <<<"),
    ))
}

fn path_text(path: &Path) -> anyhow::Result<&str> {
    let text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Shell initialization paths must be valid UTF-8"))?;
    anyhow::ensure!(
        !text.contains(['\n', '\r']),
        "Shell initialization paths must not contain line breaks"
    );
    Ok(text)
}

fn activation_block(directory: &Path, shell: Shell) -> anyhow::Result<String> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let manifest = directory.join(MANIFEST_NAME);
    let executable = path_text(&executable)?;
    let manifest = path_text(&manifest)?;
    let directory_text = path_text(directory)?;
    let activation = match shell {
        Shell::Sh | Shell::Bash | Shell::Zsh => {
            let executable = shell_words::quote(executable);
            let directory = shell_words::quote(directory_text);
            format!(
                "if [ -x {executable} ] && [ -f {} ]; then\n  eval \"$({executable} env --shim-dir {directory} --shell sh)\"\nfi\n",
                shell_words::quote(manifest)
            )
        }
        Shell::Fish => format!(
            "if test -x {}; and test -f {}\n  {} env --shim-dir {} --shell fish | source\nend\n",
            shell_words::quote(executable), shell_words::quote(manifest),
            shell_words::quote(executable), shell_words::quote(directory_text)
        ),
        Shell::Powershell => format!(
            "if ((Test-Path -LiteralPath '{}') -and (Test-Path -LiteralPath '{}')) {{\n  (& '{}' env --shim-dir '{}' --shell powershell) | Out-String | Invoke-Expression\n}}\n",
            executable.replace('\'', "''"), manifest.replace('\'', "''"),
            executable.replace('\'', "''"), directory_text.replace('\'', "''")
        ),
    };
    let (start, end) = markers(directory)?;
    Ok(format!("{start}\n{activation}{end}\n"))
}

fn resolve_path(path: &Path) -> anyhow::Result<PathBuf> {
    path_text(path)?;
    let path = std::path::absolute(path)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => path
            .canonicalize()
            .with_context(|| format!("Cannot resolve startup file {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Invalid startup path"))?;
            let name = path
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Invalid startup filename"))?;
            Ok(resolve_path(parent)?.join(name))
        }
        Err(error) => Err(error).with_context(|| format!("Cannot inspect {}", path.display())),
    }
}

fn read_state(directory: &Path) -> anyhow::Result<Option<Initialization>> {
    let path = directory.join(STATE_NAME);
    if !path.try_exists()? {
        return Ok(None);
    }
    let state: Initialization = serde_json::from_slice(&std::fs::read(&path)?)?;
    anyhow::ensure!(
        state.version == 1,
        "Unsupported shell initialization version"
    );
    let (start, end) = markers(directory)?;
    for file in &state.files {
        anyhow::ensure!(
            file.path.is_absolute()
                && file
                    .block
                    .trim_start_matches('\n')
                    .starts_with(&format!("{start}\n"))
                && file.block.ends_with(&format!("{end}\n")),
            "Invalid owned shell initialization record"
        );
    }
    Ok(Some(state))
}

fn plan_state(directory: &Path, state: Option<&Initialization>) -> anyhow::Result<FileEdit> {
    let path = directory.join(STATE_NAME);
    let mut edit = read_edit(&path)?;
    edit.replacement = state.map(serde_json::to_string_pretty).transpose()?;
    Ok(edit)
}

fn read_edit(path: &Path) -> anyhow::Result<FileEdit> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "Refusing to replace changed symlink {}",
            path.display()
        );
    }
    let original = match std::fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("Cannot read {}", path.display())),
    };
    let permissions = if original.is_some() {
        Some(std::fs::metadata(path)?.permissions())
    } else {
        None
    };
    Ok(FileEdit {
        path: path.to_path_buf(),
        original,
        replacement: None,
        permissions,
    })
}

fn plan_block(
    path: &Path,
    old_block: Option<&str>,
    new_block: Option<&str>,
) -> anyhow::Result<(FileEdit, String)> {
    let mut edit = read_edit(path)?;
    let original = edit.original.as_deref().unwrap_or_default();
    let body = new_block
        .or(old_block)
        .unwrap_or_default()
        .trim_start_matches('\n');
    let start = body.lines().next().unwrap_or_default();
    let end = body.lines().last().unwrap_or_default();
    let has_marker = original.contains(start) || original.contains(end);
    let installed_block = if has_marker {
        let owned = old_block.ok_or_else(|| {
            anyhow::anyhow!(
                "Refusing to replace unowned shell initialization in {}",
                path.display()
            )
        })?;
        anyhow::ensure!(
            original.matches(owned).count() == 1
                && original.matches(start).count() == 1 && original.matches(end).count() == 1,
            "Managed shell initialization was modified in {}; restore or remove its block before retrying",
            path.display()
        );
        let prefix = if owned.starts_with('\n') { "\n" } else { "" };
        let replacement = new_block
            .map(|block| format!("{prefix}{block}"))
            .unwrap_or_default();
        edit.replacement = Some(original.replacen(owned, &replacement, 1));
        replacement
    } else if let Some(block) = new_block {
        let prefix = if original.is_empty() || original.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        let installed = format!("{prefix}{block}");
        edit.replacement = Some(format!("{original}{installed}"));
        installed
    } else {
        edit.replacement = edit.original.clone();
        String::new()
    };
    Ok((edit, installed_block))
}

fn apply_edits(edits: &[FileEdit]) -> anyhow::Result<()> {
    for (index, edit) in edits.iter().enumerate() {
        if edit.original == edit.replacement {
            continue;
        }
        if let Err(error) = write_edit(edit, edit.replacement.as_deref()) {
            for previous in edits[..index].iter().rev() {
                if previous.original != previous.replacement {
                    if let Err(rollback_error) = write_edit(previous, previous.original.as_deref())
                    {
                        tracing::error!(path = %previous.path.display(), %rollback_error, "Could not restore startup file after initialization failure");
                    }
                }
            }
            return Err(error);
        }
    }
    Ok(())
}

fn write_edit(edit: &FileEdit, content: Option<&str>) -> anyhow::Result<()> {
    let Some(content) = content else {
        if edit.path.try_exists()? {
            std::fs::remove_file(&edit.path)?;
        }
        return Ok(());
    };
    let parent = edit
        .path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid startup path"))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if let Some(permissions) = &edit.permissions {
        temporary.as_file().set_permissions(permissions.clone())?;
    }
    temporary.write_all(content.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&edit.path)
        .with_context(|| format!("Cannot update {}", edit.path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialization_rolls_back_earlier_writes_after_publication_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let existing = temporary.path().join("existing.rc");
        let created = temporary.path().join("new.rc");
        let invalid_parent = temporary.path().join("not-a-directory");
        std::fs::write(&existing, "original configuration").unwrap();
        std::fs::write(&invalid_parent, "keep parent file").unwrap();
        let mut first = read_edit(&existing).unwrap();
        first.replacement = Some("changed configuration".into());
        let mut second = read_edit(&created).unwrap();
        second.replacement = Some("new configuration".into());
        let last = FileEdit {
            path: invalid_parent.join("startup.rc"),
            original: None,
            replacement: Some("cannot publish".into()),
            permissions: None,
        };
        assert!(apply_edits(&[first, second, last]).is_err());
        assert_eq!(
            std::fs::read_to_string(existing).unwrap(),
            "original configuration"
        );
        assert!(!created.exists());
        assert_eq!(
            std::fs::read_to_string(invalid_parent).unwrap(),
            "keep parent file"
        );
    }

    #[test]
    fn test_initialization_preserves_crlf_and_restores_original_text() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("startup.rc");
        let original = "# Existing configuration\r\nexport VALUE=1\r\n";
        std::fs::write(&path, original).unwrap();
        let block = "# >>> apexe test >>>\nactivate\n# <<< apexe test <<<\n";
        let (installation, owned) = plan_block(&path, None, Some(block)).unwrap();
        apply_edits(&[installation]).unwrap();
        let (cleanup, _) = plan_block(&path, Some(&owned), None).unwrap();
        apply_edits(&[cleanup]).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}
