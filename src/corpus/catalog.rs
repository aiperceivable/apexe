use super::{source, CorpusError, InstalledCorpus};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) const INDEX_NAME: &str = "index.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct Catalog {
    pub version: u32,
    pub corpora: Vec<InstalledCorpus>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Owner {
    version: u32,
    name: String,
    repository: String,
}

pub(super) fn package_path(root: &Path, name: &str) -> PathBuf {
    root.join("packages").join(name)
}

pub(super) fn read(root: &Path) -> Result<Catalog, CorpusError> {
    let path = root.join(INDEX_NAME);
    if !path.try_exists()? {
        return Ok(Catalog {
            version: 1,
            corpora: Vec::new(),
        });
    }
    reject_symlink(&path)?;
    let catalog: Catalog = serde_json::from_slice(&std::fs::read(&path)?)?;
    if catalog.version != 1 {
        return Err(CorpusError::State(
            "Unsupported corpus catalog version".into(),
        ));
    }
    let mut names = std::collections::HashSet::new();
    for corpus in &catalog.corpora {
        source::validate_name(&corpus.name)?;
        source::validate_location(&corpus.repository)?;
        source::validate_reference(&corpus.reference)?;
        source::validate_subdirectory(&corpus.subdirectory)?;
        if let Some(manifest) = &corpus.manifest {
            super::manifest::validate(manifest)?;
        }
        if let Some(directory) = &corpus.subdirectory_override {
            source::validate_subdirectory(directory)?;
        }
        let snapshot = uuid::Uuid::parse_str(corpus.snapshot.as_str())
            .map_err(|_| CorpusError::State("Invalid corpus snapshot ID".into()))?;
        if snapshot.to_string() != corpus.snapshot.as_str() || !names.insert(&corpus.name) {
            return Err(CorpusError::State(
                "Duplicate corpus or noncanonical snapshot ID".into(),
            ));
        }
        verify_owner(root, corpus)?;
    }
    Ok(catalog)
}

pub(super) fn save(root: &Path, catalog: &Catalog) -> Result<(), CorpusError> {
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer_pretty(&mut temporary, catalog)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(root.join(INDEX_NAME))
        .map_err(|error| CorpusError::Io(error.error))?;
    Ok(())
}

pub(super) fn write_owner(root: &Path, corpus: &InstalledCorpus) -> Result<(), CorpusError> {
    let owner = Owner {
        version: 1,
        name: corpus.name.clone(),
        repository: corpus.repository.clone(),
    };
    let path = package_path(root, &corpus.name).join(".apexe-corpus.json");
    let file = std::fs::File::create_new(path)?;
    serde_json::to_writer_pretty(file, &owner)?;
    Ok(())
}

pub(super) fn verify_owner(root: &Path, corpus: &InstalledCorpus) -> Result<(), CorpusError> {
    reject_symlink(root)?;
    reject_symlink(&root.join("packages"))?;
    let directory = package_path(root, &corpus.name);
    reject_symlink(&directory)?;
    let marker = directory.join(".apexe-corpus.json");
    reject_symlink(&marker)?;
    let owner: Owner = serde_json::from_slice(&std::fs::read(marker)?)?;
    if owner.version != 1 || owner.name != corpus.name || owner.repository != corpus.repository {
        return Err(CorpusError::State(
            "Refusing to modify an unowned corpus directory".into(),
        ));
    }
    Ok(())
}

pub(super) fn reject_symlink(path: &Path) -> Result<(), CorpusError> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(CorpusError::State(format!(
            "Refusing managed symlink {}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) struct Lock {
    path: PathBuf,
    token: String,
}

impl Lock {
    pub fn acquire(root: &Path) -> Result<Self, CorpusError> {
        std::fs::create_dir_all(root)?;
        reject_symlink(root)?;
        let path = root.join(".lock");
        let mut file = std::fs::File::create_new(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                CorpusError::State("Another corpus operation holds the installation lock".into())
            } else {
                CorpusError::Io(error)
            }
        })?;
        let token = uuid::Uuid::new_v4().to_string();
        file.write_all(token.as_bytes())?;
        Ok(Self { path, token })
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).ok().as_deref() == Some(&self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
