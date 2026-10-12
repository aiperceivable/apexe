use super::{catalog, source, CorpusError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CorpusManifest {
    pub schema_version: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub overlay_directory: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

pub(super) fn read(checkout: &Path) -> Result<Option<CorpusManifest>, CorpusError> {
    let path = checkout.join("manifest.json");
    if !path.try_exists()? {
        return Ok(None);
    }
    catalog::reject_symlink(&path)?;
    if std::fs::metadata(&path)?.len() > 64 * 1024 {
        return Err(CorpusError::InvalidCorpus(
            "manifest.json exceeds 64 KiB".into(),
        ));
    }
    let manifest: CorpusManifest = serde_json::from_slice(&std::fs::read(path)?)?;
    validate(&manifest)?;
    Ok(Some(manifest))
}

pub(super) fn validate(manifest: &CorpusManifest) -> Result<(), CorpusError> {
    source::validate_name(&manifest.name)?;
    source::validate_subdirectory(&manifest.overlay_directory)?;
    if manifest.schema_version != "1.0"
        || manifest.description.trim().is_empty()
        || manifest.description.len() > 4096
        || manifest.version.len() > 256
        || semver::Version::parse(&manifest.version).is_err()
    {
        return Err(CorpusError::InvalidCorpus(
            "Invalid corpus manifest schema, description or SemVer version".into(),
        ));
    }
    if let Some(homepage) = &manifest.homepage {
        let url = url::Url::parse(homepage)
            .map_err(|_| CorpusError::InvalidCorpus("Invalid corpus homepage URL".into()))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(CorpusError::InvalidCorpus(
                "Corpus homepage must use HTTPS without embedded credentials".into(),
            ));
        }
    }
    if manifest
        .license
        .as_ref()
        .is_some_and(|license| license.trim().is_empty() || license.len() > 256)
    {
        return Err(CorpusError::InvalidCorpus("Invalid corpus license".into()));
    }
    Ok(())
}
