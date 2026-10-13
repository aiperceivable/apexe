use super::activation;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

const STATE_NAME: &str = ".apexe-source-scripts.json";
const ACTIVATE_NAME: &str = "apexe-activate.sh";
const DEACTIVATE_NAME: &str = "apexe-deactivate.sh";

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    shim_dir: PathBuf,
    executable: PathBuf,
    activate: String,
    deactivate: String,
}

struct ReadRecord {
    record: Record,
    text: String,
}

#[derive(Serialize, Deserialize)]
enum ScriptFile {
    Activate,
    Deactivate,
    State,
}

impl ScriptFile {
    fn name(&self) -> &'static str {
        match self {
            Self::Activate => ACTIVATE_NAME,
            Self::Deactivate => DEACTIVATE_NAME,
            Self::State => STATE_NAME,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct FileEdit {
    file: ScriptFile,
    original: Option<String>,
    replacement: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct PreparedScripts {
    directory: PathBuf,
    edits: Vec<FileEdit>,
}

impl PreparedScripts {
    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(super) fn publish(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.directory)?;
        for (index, edit) in self.edits.iter().enumerate() {
            if edit.original.as_deref() == Some(&edit.replacement) {
                continue;
            }
            if let Err(error) = write_atomic(
                &self.directory.join(edit.file.name()),
                Some(&edit.replacement),
            ) {
                for previous in self.edits[..index].iter().rev() {
                    if previous.original.as_deref() == Some(&previous.replacement) {
                        continue;
                    }
                    let path = self.directory.join(previous.file.name());
                    if let Err(rollback) = write_atomic(&path, previous.original.as_deref()) {
                        tracing::error!(path = %path.display(), %rollback, "Could not restore persistent source script after publication failure");
                    }
                }
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) fn log_paths(&self) {
        tracing::info!(activate = %self.directory.join(ACTIVATE_NAME).display(),
            deactivate = %self.directory.join(DEACTIVATE_NAME).display(),
            "Source these apexe scripts in sh/bash/zsh; they remain installed after disable");
    }
}

pub(super) fn prepare(shim_dir: &Path, executable: &Path) -> anyhow::Result<PreparedScripts> {
    let directory = script_directory(shim_dir)?;
    let previous = read_record(&directory)?;
    if let Some(previous) = &previous {
        anyhow::ensure!(
            same_directory(&previous.record.shim_dir, shim_dir),
            "Persistent source script record belongs to another shim directory"
        );
    }
    activation::verify_existing(
        &directory,
        ACTIVATE_NAME,
        previous
            .as_ref()
            .map(|value| value.record.activate.as_str()),
    )?;
    activation::verify_existing(
        &directory,
        DEACTIVATE_NAME,
        previous
            .as_ref()
            .map(|value| value.record.deactivate.as_str()),
    )?;
    let record = Record {
        version: 1,
        shim_dir: shim_dir.to_path_buf(),
        executable: executable.to_path_buf(),
        activate: activation::generate(shim_dir, executable)?,
        deactivate: activation::generate_deactivation(shim_dir, executable)?,
    };
    let edits = plan_edits(&directory, &record, previous)?;
    Ok(PreparedScripts { directory, edits })
}

fn plan_edits(
    directory: &Path,
    record: &Record,
    previous: Option<ReadRecord>,
) -> anyhow::Result<Vec<FileEdit>> {
    Ok(vec![
        FileEdit {
            file: ScriptFile::Activate,
            original: read_optional_script(&directory.join(ACTIVATE_NAME))?,
            replacement: record.activate.clone(),
        },
        FileEdit {
            file: ScriptFile::Deactivate,
            original: read_optional_script(&directory.join(DEACTIVATE_NAME))?,
            replacement: record.deactivate.clone(),
        },
        FileEdit {
            file: ScriptFile::State,
            original: previous.map(|value| value.text),
            replacement: serde_json::to_string_pretty(record)?,
        },
    ])
}

fn read_optional_script(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn reserved_name(name: &str) -> bool {
    [ACTIVATE_NAME, DEACTIVATE_NAME, STATE_NAME].contains(&name)
}

fn script_directory(shim_dir: &Path) -> anyhow::Result<PathBuf> {
    let directory = std::path::absolute(shim_dir)?;
    let text = directory
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Source script directories must be valid UTF-8"))?;
    anyhow::ensure!(
        !text.contains(['\n', '\r']),
        "Source script directories must not contain line breaks"
    );
    Ok(directory)
}

fn same_directory(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}

fn read_record(directory: &Path) -> anyhow::Result<Option<ReadRecord>> {
    let path = directory.join(STATE_NAME);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Cannot inspect persistent source script record"),
    };
    anyhow::ensure!(
        metadata.file_type().is_file() && metadata.len() <= 64 * 1024,
        "Persistent source script record must be a regular file of at most 64 KiB"
    );
    let text = std::fs::read_to_string(&path)?;
    let record: Record =
        serde_json::from_str(&text).context("Invalid persistent source script record")?;
    anyhow::ensure!(
        record.version == 1 && record.shim_dir.is_absolute() && record.executable.is_absolute(),
        "Unsupported or invalid persistent source script record"
    );
    Ok(Some(ReadRecord { record, text }))
}

pub(super) fn check(directory: &Path, shim_dir: &Path, executable: &Path) -> anyhow::Result<()> {
    let stored = read_record(directory)?.ok_or_else(|| {
        anyhow::anyhow!("Persistent source script record is missing; reload shims")
    })?;
    let record = stored.record;
    anyhow::ensure!(
        same_directory(&record.shim_dir, shim_dir) && record.executable == executable,
        "Persistent source scripts target another installation; reload shims"
    );
    anyhow::ensure!(
        record.activate == activation::generate(shim_dir, executable)?
            && record.deactivate == activation::generate_deactivation(shim_dir, executable)?,
        "Persistent source scripts need an update; reload shims"
    );
    activation::require_owned(directory, ACTIVATE_NAME, &record.activate)?;
    activation::require_owned(directory, DEACTIVATE_NAME, &record.deactivate)
}

fn write_atomic(path: &Path, content: Option<&str>) -> anyhow::Result<()> {
    let Some(content) = content else {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        };
    };
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid source script path"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(metadata) = std::fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary.write_all(content.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .with_context(|| format!("Cannot publish source script {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prepare_is_read_only_and_published_scripts_are_checkable() {
        let root = tempfile::tempdir().unwrap();
        let shim_dir = root.path().join("bin");
        let executable = root.path().join("apexe");
        let scripts = prepare(&shim_dir, &executable).unwrap();
        assert!(!scripts.directory().exists());
        scripts.publish().unwrap();
        check(scripts.directory(), &shim_dir, &executable).unwrap();
        assert!(check(scripts.directory(), &root.path().join("other"), &executable).is_err());
    }

    #[test]
    fn test_publication_failure_restores_earlier_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(ACTIVATE_NAME), "original").unwrap();
        std::fs::create_dir(root.path().join(STATE_NAME)).unwrap();
        let scripts = PreparedScripts {
            directory: root.path().to_path_buf(),
            edits: vec![
                FileEdit {
                    file: ScriptFile::Activate,
                    original: Some("original".into()),
                    replacement: "changed".into(),
                },
                FileEdit {
                    file: ScriptFile::Deactivate,
                    original: None,
                    replacement: "new".into(),
                },
                FileEdit {
                    file: ScriptFile::State,
                    original: None,
                    replacement: "cannot publish".into(),
                },
            ],
        };
        assert!(scripts.publish().is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join(ACTIVATE_NAME)).unwrap(),
            "original"
        );
        assert!(!root.path().join(DEACTIVATE_NAME).exists());
        assert!(root.path().join(STATE_NAME).is_dir());
    }

    #[test]
    fn test_record_rejects_oversized_corrupt_and_invalid_metadata() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(STATE_NAME);
        std::fs::write(&path, vec![b' '; 64 * 1024 + 1]).unwrap();
        assert!(read_record(root.path()).is_err());
        std::fs::write(&path, "corrupt").unwrap();
        assert!(read_record(root.path()).is_err());
        let record = Record {
            version: 999,
            shim_dir: root.path().join("bin"),
            executable: root.path().join("apexe"),
            activate: "a".into(),
            deactivate: "d".into(),
        };
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(read_record(root.path()).is_err());
    }

    #[test]
    fn test_prepare_repairs_missing_helper_and_rejects_foreign_record() {
        let root = tempfile::tempdir().unwrap();
        let shim_dir = root.path().join("bin");
        let executable = root.path().join("apexe");
        let scripts = prepare(&shim_dir, &executable).unwrap();
        scripts.publish().unwrap();
        std::fs::remove_file(scripts.directory().join(ACTIVATE_NAME)).unwrap();
        assert!(check(scripts.directory(), &shim_dir, &executable).is_err());
        prepare(&shim_dir, &executable).unwrap().publish().unwrap();
        check(scripts.directory(), &shim_dir, &executable).unwrap();
        let mut stored = read_record(scripts.directory()).unwrap().unwrap().record;
        stored.shim_dir = root.path().join("another");
        std::fs::write(
            scripts.directory().join(STATE_NAME),
            serde_json::to_vec(&stored).unwrap(),
        )
        .unwrap();
        assert!(prepare(&shim_dir, &executable).is_err());
    }
}
