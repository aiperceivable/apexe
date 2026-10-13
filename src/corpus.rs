mod automatic;
pub(crate) mod bootstrap;
mod catalog;
mod git;
mod manifest;
mod source;

pub(crate) use automatic::prepare_default;

use crate::adapter::overlay::{validate_overlay, ToolOverlay};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum CorpusError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Invalid repository source: {0}")]
    InvalidSource(String),
    #[error("Invalid corpus installation state: {0}")]
    State(String),
    #[error("Git operation failed: {0}")]
    Git(String),
    #[error("Invalid overlay corpus: {0}")]
    InvalidCorpus(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct SnapshotId(String);

impl SnapshotId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstalledCorpus {
    pub name: String,
    pub repository: String,
    pub reference: String,
    pub commit: String,
    pub subdirectory: PathBuf,
    pub snapshot: SnapshotId,
    pub overlay_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<manifest::CorpusManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdirectory_override: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CorpusStore {
    root: PathBuf,
    timeout: Duration,
}

struct PendingDirectory {
    path: PathBuf,
    published: bool,
}

impl PendingDirectory {
    fn create(path: PathBuf) -> Result<Self, CorpusError> {
        std::fs::create_dir(&path)?;
        Ok(Self {
            path,
            published: false,
        })
    }
}

impl Drop for PendingDirectory {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

impl CorpusStore {
    pub(crate) fn new(config: &crate::config::ApexeConfig) -> Self {
        Self {
            root: config.config_dir.join("corpora"),
            timeout: Duration::from_secs(config.default_timeout),
        }
    }

    pub(crate) fn list(&self) -> Result<Vec<InstalledCorpus>, CorpusError> {
        Ok(catalog::read(&self.root)?.corpora)
    }

    pub(crate) fn install(
        &self,
        repository: &str,
        name: Option<&str>,
        reference: Option<&str>,
        subdirectory: Option<&Path>,
    ) -> Result<InstalledCorpus, CorpusError> {
        let repository = source::parse_repository(repository)?;
        let use_manifest_name = name.is_none();
        if let Some(name) = name {
            source::validate_name(name)?;
        }
        let name = name.unwrap_or(&repository.name);
        source::validate_reference(reference.unwrap_or("HEAD"))?;
        if let Some(subdirectory) = subdirectory {
            source::validate_subdirectory(subdirectory)?;
        }
        let _lock = catalog::Lock::acquire(&self.root)?;
        let catalog = catalog::read(&self.root)?;
        let mut corpus = InstalledCorpus {
            name: name.into(),
            repository: repository.location,
            reference: reference.unwrap_or("HEAD").into(),
            commit: String::new(),
            subdirectory: subdirectory.unwrap_or(Path::new("overlays")).into(),
            snapshot: SnapshotId::new(),
            overlay_count: 0,
            manifest: None,
            subdirectory_override: subdirectory.map(Path::to_path_buf),
        };
        let staging = self.fetch(&mut corpus)?;
        resolve_installation_name(&mut corpus, use_manifest_name)?;
        if catalog
            .corpora
            .iter()
            .any(|installed| installed.name == corpus.name)
        {
            return Err(CorpusError::State(format!(
                "Corpus '{}' is installed; use apexe init --overlays {} --refresh",
                corpus.name, corpus.repository
            )));
        }
        self.register_installation(catalog, corpus, staging)
    }

    fn register_installation(
        &self,
        mut catalog: catalog::Catalog,
        corpus: InstalledCorpus,
        staging: tempfile::TempDir,
    ) -> Result<InstalledCorpus, CorpusError> {
        let packages = self.root.join("packages");
        std::fs::create_dir_all(&packages)?;
        catalog::reject_symlink(&packages)?;
        let mut package =
            PendingDirectory::create(catalog::package_path(&self.root, &corpus.name))?;
        catalog::write_owner(&self.root, &corpus)?;
        let mut snapshot = self.publish_snapshot(&corpus, staging)?;
        catalog.corpora.push(corpus.clone());
        catalog::save(&self.root, &catalog)?;
        package.published = true;
        snapshot.published = true;
        Ok(corpus)
    }

    pub(crate) fn update(
        &self,
        name: &str,
        reference: Option<&str>,
    ) -> Result<InstalledCorpus, CorpusError> {
        source::validate_name(name)?;
        let _lock = catalog::Lock::acquire(&self.root)?;
        let mut catalog = catalog::read(&self.root)?;
        let index = catalog
            .corpora
            .iter()
            .position(|corpus| corpus.name == name)
            .ok_or_else(|| CorpusError::State(format!("Corpus '{name}' is not installed")))?;
        let mut corpus = catalog.corpora[index].clone();
        if let Some(reference) = reference {
            source::validate_reference(reference)?;
            corpus.reference = reference.into();
        }
        corpus.snapshot = SnapshotId::new();
        let staging = self.fetch(&mut corpus)?;
        let mut snapshot = self.publish_snapshot(&corpus, staging)?;
        catalog.corpora[index] = corpus.clone();
        catalog::save(&self.root, &catalog)?;
        snapshot.published = true;
        // Older snapshots remain until removal so concurrent scans can finish.
        Ok(corpus)
    }

    #[cfg(test)]
    pub(crate) fn remove(&self, name: &str) -> Result<(), CorpusError> {
        source::validate_name(name)?;
        let _lock = catalog::Lock::acquire(&self.root)?;
        let mut catalog = catalog::read(&self.root)?;
        let index = catalog
            .corpora
            .iter()
            .position(|corpus| corpus.name == name)
            .ok_or_else(|| CorpusError::State(format!("Corpus '{name}' is not installed")))?;
        let corpus = catalog.corpora.remove(index);
        let package = catalog::package_path(&self.root, &corpus.name);
        let trash = self.root.join(format!(".remove-{}", uuid::Uuid::new_v4()));
        std::fs::rename(&package, &trash)?;
        if let Err(error) = catalog::save(&self.root, &catalog) {
            std::fs::rename(&trash, &package)?;
            return Err(error);
        }
        std::fs::remove_dir_all(trash)?;
        Ok(())
    }

    fn fetch(&self, corpus: &mut InstalledCorpus) -> Result<tempfile::TempDir, CorpusError> {
        let staging = tempfile::tempdir_in(&self.root)?;
        let checkout = staging.path().join("checkout");
        corpus.commit = git::checkout(
            &corpus.repository,
            &corpus.reference,
            &checkout,
            self.timeout,
        )?;
        corpus.manifest = manifest::read(&checkout)?;
        if corpus.subdirectory_override.is_none() {
            if let Some(manifest) = &corpus.manifest {
                corpus.subdirectory = manifest.overlay_directory.clone();
            }
        }
        let overlays = checked_directory(&checkout, &corpus.subdirectory)?;
        corpus.overlay_count = validate_corpus(&overlays)?;
        Ok(staging)
    }

    fn publish_snapshot(
        &self,
        corpus: &InstalledCorpus,
        staging: tempfile::TempDir,
    ) -> Result<PendingDirectory, CorpusError> {
        let path = catalog::package_path(&self.root, &corpus.name).join(corpus.snapshot.as_str());
        std::fs::rename(staging.path().join("checkout"), &path)?;
        Ok(PendingDirectory {
            path,
            published: false,
        })
    }
}

fn resolve_installation_name(
    corpus: &mut InstalledCorpus,
    use_manifest_name: bool,
) -> Result<(), CorpusError> {
    if let Some(manifest) = &corpus.manifest {
        if use_manifest_name {
            corpus.name = manifest.name.clone();
        }
    }
    source::validate_name(&corpus.name)
}

fn checked_directory(checkout: &Path, subdirectory: &Path) -> Result<PathBuf, CorpusError> {
    source::validate_subdirectory(subdirectory)?;
    let mut directory = checkout.to_path_buf();
    catalog::reject_symlink(&directory)?;
    for component in subdirectory.components() {
        directory.push(component);
        catalog::reject_symlink(&directory)?;
    }
    if !directory.is_dir() {
        return Err(CorpusError::InvalidCorpus(format!(
            "Missing overlay directory {}",
            directory.display()
        )));
    }
    Ok(directory)
}

fn validate_corpus(directory: &Path) -> Result<usize, CorpusError> {
    let mut count = 0;
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("json" | "yaml" | "yml")
        ) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            return Err(CorpusError::InvalidCorpus(format!(
                "Overlay must be a regular file of at most 1 MiB: {}",
                path.display()
            )));
        }
        validate_document(&path)?;
        count += 1;
    }
    if count == 0 {
        return Err(CorpusError::InvalidCorpus(
            "No JSON or YAML overlays found".into(),
        ));
    }
    Ok(count)
}

fn validate_document(path: &Path) -> Result<(), CorpusError> {
    let document = std::fs::read_to_string(path)?;
    let overlay: ToolOverlay = if document.trim_start().starts_with('{') {
        serde_json::from_str(&document).map_err(|error| error.to_string())
    } else {
        serde_yaml::from_str(&document).map_err(|error| error.to_string())
    }
    .map_err(|error| CorpusError::InvalidCorpus(format!("{}: {error}", path.display())))?;
    if !overlay.is_supported_version() {
        return Err(CorpusError::InvalidCorpus(format!(
            "{}: unsupported schema version",
            path.display()
        )));
    }
    let defects = validate_overlay(&overlay);
    if !defects.is_empty() {
        return Err(CorpusError::InvalidCorpus(format!(
            "{}: {defects:?}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn installed_overlay_dirs(config_dir: &Path) -> Result<Vec<PathBuf>, CorpusError> {
    let root = config_dir.join("corpora");
    let mut corpora = catalog::read(&root)?.corpora;
    // Load the automatic default before corpora explicitly installed by the operator.
    corpora.sort_by_key(|corpus| corpus.name != automatic::DEFAULT_NAME);
    corpora
        .iter()
        .map(|corpus| {
            checked_directory(
                &catalog::package_path(&root, &corpus.name).join(corpus.snapshot.as_str()),
                &corpus.subdirectory,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;
