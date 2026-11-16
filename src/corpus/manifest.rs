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

/// Corpus releases this apexe was built to read. The corpus release version is
/// independent of the manifest and overlay format versions; this range is the
/// one statement of which corpus content this binary understands. Schema
/// changes are rejected separately by the `schema_version` check above, because
/// they break parsing rather than compatibility.
const SUPPORTED_CORPUS_VERSIONS: &str = ">=0.1.0, <1.0.0";

pub(super) fn validate(manifest: &CorpusManifest) -> Result<(), CorpusError> {
    source::validate_name(&manifest.name)?;
    source::validate_subdirectory(&manifest.overlay_directory)?;
    if manifest.schema_version != "1.0"
        || manifest.description.trim().is_empty()
        || manifest.description.len() > 4096
        || manifest.version.len() > 256
    {
        return Err(CorpusError::InvalidCorpus(
            "Invalid corpus manifest schema, description or SemVer version".into(),
        ));
    }
    let version = semver::Version::parse(&manifest.version).map_err(|_| {
        CorpusError::InvalidCorpus(
            "Invalid corpus manifest schema, description or SemVer version".into(),
        )
    })?;
    let supported = semver::VersionReq::parse(SUPPORTED_CORPUS_VERSIONS).map_err(|error| {
        CorpusError::InvalidCorpus(format!("Invalid supported corpus range: {error}"))
    })?;
    if !supported.matches(&version) {
        return Err(CorpusError::UnsupportedCorpus(format!(
            "Corpus version {} is outside the supported range {}; upgrade apexe or install an older corpus release",
            manifest.version, SUPPORTED_CORPUS_VERSIONS
        )));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str) -> CorpusManifest {
        CorpusManifest {
            schema_version: "1.0".into(),
            name: "cli-permissions".into(),
            description: "Reviewed CLI command facts.".into(),
            version: version.into(),
            overlay_directory: PathBuf::from("overlays"),
            homepage: None,
            license: None,
        }
    }

    #[test]
    fn test_supported_corpus_versions_are_accepted() {
        for version in ["0.1.0", "0.2.0", "0.99.99"] {
            assert!(validate(&manifest(version)).is_ok(), "version {version}");
        }
    }

    #[test]
    fn test_corpus_versions_outside_the_range_are_unsupported() {
        for version in ["0.0.9", "1.0.0", "2.0.0"] {
            assert!(
                matches!(
                    validate(&manifest(version)),
                    Err(CorpusError::UnsupportedCorpus(_))
                ),
                "version {version}"
            );
        }
    }

    #[test]
    fn test_malformed_versions_are_invalid_rather_than_unsupported() {
        assert!(matches!(
            validate(&manifest("latest")),
            Err(CorpusError::InvalidCorpus(_))
        ));
    }
}
