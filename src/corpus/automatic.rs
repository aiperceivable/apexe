use super::{bootstrap, catalog, CorpusError, CorpusStore, InstalledCorpus};
use crate::config::ApexeConfig;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(super) const DEFAULT_NAME: &str = "cli-permissions";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize, Deserialize)]
enum DefaultCorpus {
    Cached(InstalledCorpus),
    Downloaded(InstalledCorpus),
    Configured,
    Offline,
}

pub(crate) fn prepare_default(config: &ApexeConfig, download: bool) {
    let result = resolve_default(config, download, |store| {
        store.install(
            &config.default_overlay_repository,
            Some(DEFAULT_NAME),
            None,
            None,
        )
    });
    match result {
        Ok(DefaultCorpus::Cached(corpus)) => tracing::info!(name = %corpus.name,
            commit = %corpus.commit, "Using cached default overlay corpus"),
        Ok(DefaultCorpus::Downloaded(corpus)) => tracing::info!(name = %corpus.name,
            commit = %corpus.commit, count = corpus.overlay_count,
            "Installed default overlay corpus; future scans use this cached snapshot"),
        Ok(DefaultCorpus::Configured) => tracing::info!(
            "Using explicitly configured overlay directories without automatic downloads"),
        Ok(DefaultCorpus::Offline) => tracing::warn!(
            "Default corpus is not cached and downloads are disabled; using local overlays or built-in classification"),
        Err(error) => tracing::warn!(%error,
            "Could not obtain a valid default overlay corpus; using local overlays or built-in classification"),
    }
}

fn resolve_default(
    config: &ApexeConfig,
    download: bool,
    fetch: impl FnOnce(&CorpusStore) -> Result<InstalledCorpus, CorpusError>,
) -> Result<DefaultCorpus, CorpusError> {
    let mut store = CorpusStore::new(config);
    store.timeout = store.timeout.min(DOWNLOAD_TIMEOUT);
    if let Some(corpus) = find_cached(&store, &config.default_overlay_repository)? {
        return Ok(DefaultCorpus::Cached(corpus));
    }
    if !config.overlay_dirs.is_empty() {
        return Ok(DefaultCorpus::Configured);
    }
    if !download {
        return Ok(DefaultCorpus::Offline);
    }
    fetch(&store).map(DefaultCorpus::Downloaded)
}

fn find_cached(
    store: &CorpusStore,
    repository: &str,
) -> Result<Option<InstalledCorpus>, CorpusError> {
    let location = bootstrap::parse_repository(repository)
        .ok()
        .map(|value| value.location);
    for corpus in store.list()? {
        if !location
            .as_deref()
            .is_some_and(|requested| bootstrap::same_repository(&corpus.repository, requested))
        {
            continue;
        }
        let directory = super::checked_directory(
            &catalog::package_path(&store.root, &corpus.name).join(corpus.snapshot.as_str()),
            &corpus.subdirectory,
        )?;
        super::validate_corpus(&directory)?;
        return Ok(Some(corpus));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::super::SnapshotId;
    use super::*;
    use crate::config::DEFAULT_OVERLAY_REPOSITORY as DEFAULT_REPOSITORY;
    use std::path::PathBuf;

    fn config(root: &std::path::Path) -> ApexeConfig {
        ApexeConfig {
            config_dir: root.to_path_buf(),
            ..ApexeConfig::default()
        }
    }

    fn corpus(name: &str) -> InstalledCorpus {
        InstalledCorpus {
            name: name.into(),
            repository: format!("{DEFAULT_REPOSITORY}.git"),
            reference: "HEAD".into(),
            commit: "a".repeat(40),
            subdirectory: PathBuf::from("overlays"),
            snapshot: SnapshotId::new(),
            overlay_count: 1,
            manifest: None,
            subdirectory_override: None,
        }
    }

    fn cache(config: &ApexeConfig, name: &str) -> (InstalledCorpus, PathBuf) {
        let store = CorpusStore::new(config);
        let corpus = corpus(name);
        let package = catalog::package_path(&store.root, name);
        let overlays = package.join(corpus.snapshot.as_str()).join("overlays");
        std::fs::create_dir_all(&overlays).unwrap();
        std::fs::write(overlays.join("fixture.json"), r#"{"schema_version":"1.1","command":"fixture","variant":"unknown","mode":"merge","confidence":"low","flags":[]}"#).unwrap();
        catalog::write_owner(&store.root, &corpus).unwrap();
        catalog::save(
            &store.root,
            &catalog::Catalog {
                version: 1,
                corpora: vec![corpus.clone()],
            },
        )
        .unwrap();
        (corpus, overlays)
    }

    #[test]
    fn test_offline_does_not_fetch_or_create_corpus_storage() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let result = resolve_default(&config, false, |_| panic!("offline must not fetch")).unwrap();
        assert!(matches!(result, DefaultCorpus::Offline));
        assert!(!root.path().join("corpora").exists());
    }

    #[test]
    fn test_configured_overlay_directories_skip_fetch() {
        let root = tempfile::tempdir().unwrap();
        let mut config = config(root.path());
        config.overlay_dirs = vec![root.path().join("operator-overlays")];
        let result = resolve_default(&config, true, |_| {
            panic!("configured source must not fetch")
        })
        .unwrap();
        assert!(matches!(result, DefaultCorpus::Configured));
    }

    #[test]
    fn test_download_is_bounded_by_config_timeout_and_cap() {
        let root = tempfile::tempdir().unwrap();
        for seconds in [1, 120] {
            let mut config = config(root.path());
            config.default_timeout = seconds;
            let result = resolve_default(&config, true, |store| {
                assert_eq!(store.timeout, Duration::from_secs(seconds.min(10)));
                Ok(corpus(DEFAULT_NAME))
            })
            .unwrap();
            assert!(matches!(result, DefaultCorpus::Downloaded(_)));
        }
    }

    #[test]
    fn test_fetch_error_is_reported_without_installing_state() {
        let root = tempfile::tempdir().unwrap();
        let result = resolve_default(&config(root.path()), true, |_| {
            Err(CorpusError::Git("unavailable".into()))
        });
        assert!(matches!(result, Err(CorpusError::Git(_))));
        assert!(!root.path().join("corpora").exists());
    }

    #[test]
    fn test_cached_corpus_under_an_alias_is_used_without_download() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let (cached, _) = cache(&config, "operator-alias");
        let result = resolve_default(&config, true, |_| panic!("cache must not fetch")).unwrap();
        let DefaultCorpus::Cached(found) = result else {
            panic!("expected cached corpus");
        };
        assert_eq!(found.commit, cached.commit);
        assert_eq!(found.name, "operator-alias");
    }

    #[test]
    fn test_changed_repository_does_not_reuse_a_named_default_cache() {
        let root = tempfile::tempdir().unwrap();
        let mut config = config(root.path());
        cache(&config, DEFAULT_NAME);
        config.default_overlay_repository = "https://example.invalid/new-rules".into();
        let result = resolve_default(&config, true, |_| {
            Err(CorpusError::Git("new source".into()))
        });
        assert!(matches!(result, Err(CorpusError::Git(message)) if message == "new source"));
    }

    #[test]
    fn test_corrupt_cached_overlay_is_not_used_or_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let (_, overlays) = cache(&config, DEFAULT_NAME);
        let path = overlays.join("fixture.json");
        std::fs::write(&path, "invalid").unwrap();
        assert!(resolve_default(&config, true, |_| panic!(
            "invalid cache must not be overwritten"
        ))
        .is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "invalid");
    }

    #[test]
    fn test_invalid_catalog_does_not_fetch_or_replace_operator_state() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let store = CorpusStore::new(&config);
        std::fs::create_dir_all(&store.root).unwrap();
        let path = store.root.join(catalog::INDEX_NAME);
        std::fs::write(&path, "{\"version\":99,\"corpora\":[]}").unwrap();
        assert!(
            resolve_default(&config, true, |_| panic!("invalid state must not fetch")).is_err()
        );
        assert!(std::fs::read_to_string(path).unwrap().contains("99"));
    }
}
