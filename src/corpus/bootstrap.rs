use super::{source, CorpusError, CorpusStore, InstalledCorpus};
use crate::config::ApexeConfig;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct BootstrapSource {
    pub corpus: Option<InstalledCorpus>,
    pub warnings: Vec<String>,
}

pub(crate) fn prepare(
    config: &ApexeConfig,
    explicit: Option<&str>,
    reference: Option<&str>,
    refresh: bool,
    offline: bool,
    replace: bool,
) -> Result<BootstrapSource, CorpusError> {
    if let Some(reference) = reference {
        source::validate_reference(reference)?;
    }
    let repository = explicit.unwrap_or(&config.default_overlay_repository);
    let parsed = parse_repository(repository)?;
    let store = CorpusStore::new(config);
    let cached = find_cached(&store, &parsed.location)?;
    if let Some(corpus) = cached {
        return use_cached(&store, corpus, reference, refresh, offline);
    }
    if explicit.is_none() && !config.overlay_dirs.is_empty() {
        return Ok(BootstrapSource::default());
    }
    let result = ensure_online(offline, &parsed.location)
        .and_then(|()| store.install(repository, None, reference, None, replace));
    match result {
        Ok(corpus) => {
            let warnings = fallback_warnings(&corpus, reference);
            Ok(BootstrapSource {
                corpus: Some(corpus),
                warnings,
            })
        }
        Err(error) if explicit.is_none() && reference.is_none() && !refresh => {
            tracing::warn!(%error, "Default rules unavailable; initialization will report fallback coverage");
            Ok(fallback(error))
        }
        Err(error) => Err(error),
    }
}

fn find_cached(
    store: &CorpusStore,
    repository: &str,
) -> Result<Option<InstalledCorpus>, CorpusError> {
    Ok(store
        .list()?
        .into_iter()
        .find(|corpus| same_repository(&corpus.repository, repository)))
}

pub(super) fn same_repository(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        let local = url::Url::parse(value)
            .ok()
            .and_then(|url| url.to_file_path().ok());
        if let Some(path) = local {
            return normalize_local_path(&path)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
        }
        value
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .to_string()
    };
    normalize(left) == normalize(right)
}

fn normalize_local_path(path: &std::path::Path) -> Result<std::path::PathBuf, CorpusError> {
    let path = std::path::absolute(path)?;
    for ancestor in path.ancestors() {
        if let Ok(root) = ancestor.canonicalize() {
            let suffix = path
                .strip_prefix(ancestor)
                .map_err(|error| CorpusError::State(error.to_string()))?;
            return Ok(root.join(suffix));
        }
    }
    Ok(path)
}

/// Explain a source that was discovered rather than requested.
///
/// A GitHub repository with no release archive is fetched with Git, and the
/// recorded reference becomes `HEAD`. That is a real change of source, so the
/// report says it instead of letting the identity column change unexplained.
fn fallback_warnings(corpus: &InstalledCorpus, reference: Option<&str>) -> Vec<String> {
    if reference.is_none()
        && source::derives_release(&corpus.repository)
        && corpus.reference == source::git_head_reference()
    {
        return vec![format!(
            "{} publishes no release archive; installed its Git history ({}). \
             Publish a release, or pass --ref to state the source explicitly.",
            corpus.name, corpus.commit
        )];
    }
    Vec::new()
}

fn ensure_online(offline: bool, repository: &str) -> Result<(), CorpusError> {
    if offline && repository.starts_with("https://") {
        return Err(CorpusError::State("Network downloads are disabled by --offline; use a cached snapshot or local Git repository".into()));
    }
    Ok(())
}

fn fallback(error: CorpusError) -> BootstrapSource {
    BootstrapSource {
        corpus: None,
        warnings: vec![format!(
            "Default rules unavailable; fallback classification: {error}"
        )],
    }
}

pub(super) fn parse_repository(repository: &str) -> Result<source::Repository, CorpusError> {
    if std::path::Path::new(repository).is_absolute()
        || repository.starts_with("./")
        || repository.starts_with("../")
    {
        let url =
            url::Url::from_directory_path(normalize_local_path(std::path::Path::new(repository))?)
                .map_err(|_| CorpusError::InvalidSource("Invalid local path".into()))?;
        source::parse_repository(url.as_str())
    } else {
        source::parse_repository(repository)
    }
}

fn use_cached(
    store: &CorpusStore,
    corpus: InstalledCorpus,
    reference: Option<&str>,
    refresh: bool,
    offline: bool,
) -> Result<BootstrapSource, CorpusError> {
    if refresh || reference.is_some_and(|value| value != corpus.reference) {
        ensure_online(offline, &corpus.repository)?;
        return store
            .update(&corpus.name, reference)
            .map(|corpus| BootstrapSource {
                corpus: Some(corpus),
                warnings: vec![],
            });
    }
    super::validate_corpus(&super::checked_directory(
        &store
            .root
            .join("packages")
            .join(&corpus.name)
            .join(corpus.snapshot.as_str()),
        &corpus.subdirectory,
    )?)?;
    Ok(BootstrapSource {
        corpus: Some(corpus),
        warnings: vec![],
    })
}
