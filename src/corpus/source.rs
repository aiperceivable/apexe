use super::CorpusError;
use std::path::{Component, Path};

pub(super) struct Repository {
    pub location: String,
    pub name: String,
}

/// How a corpus is fetched. A Git source resolves a commit; an archive source
/// downloads one release asset and identifies it by the digest of its bytes.
pub(super) enum InstallSource {
    Git {
        repository: String,
        reference: String,
    },
    Archive {
        url: String,
    },
}

/// A caller-supplied source resolved into what the store needs to record: the
/// location and reference that make a later refresh reproduce the same choice.
///
/// The reference is the whole decision. `latest` means "the newest release of
/// this repository"; a `v` tag names one release; anything else is a Git ref.
/// `from_installed` turns that pair back into a fetchable source, so the same
/// rule decides installation and refresh instead of two divergent paths.
pub(super) struct Resolved {
    pub repository: Repository,
    pub reference: String,
}

const LATEST: &str = "latest";

pub(super) fn parse_repository(input: &str) -> Result<Repository, CorpusError> {
    let location = if Path::new(input).is_dir() {
        url::Url::from_directory_path(Path::new(input).canonicalize()?)
            .map_err(|_| CorpusError::InvalidSource("Invalid local repository path".into()))?
    } else if Path::new(input).is_file() {
        url::Url::from_file_path(Path::new(input).canonicalize()?)
            .map_err(|_| CorpusError::InvalidSource("Invalid local archive path".into()))?
    } else if !input.contains("://") && input.split('/').count() == 2 {
        url::Url::parse(&format!("https://github.com/{input}"))
            .map_err(|_| CorpusError::InvalidSource("Invalid GitHub repository".into()))?
    } else {
        url::Url::parse(input).map_err(|_| {
            CorpusError::InvalidSource(
                "Expected an HTTPS repository, OWNER/REPO, local Git directory or corpus archive"
                    .into(),
            )
        })?
    };
    validate_location(location.as_str())?;
    let name = location
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
        .to_string();
    Ok(Repository {
        location: location.to_string(),
        name,
    })
}

/// Resolve `--overlays` (or the configured default) into the pair the store
/// records.
///
/// The default is a GitHub release archive, because `init` should not require
/// a Git installation to read rules that are published as a release. Git stays
/// the path for an exact commit, a branch, a non-GitHub host and local
/// checkouts, which an archive cannot express.
pub(super) fn resolve(input: &str, reference: Option<&str>) -> Result<Resolved, CorpusError> {
    let mut repository = parse_repository(input)?;
    if is_archive_location(&repository.location) {
        if reference.is_some_and(|value| !is_release_reference(value)) {
            return Err(CorpusError::InvalidSource(
                "An archive URL already names one release; use the URL of the release you want, or a Git source with --ref".into(),
            ));
        }
        repository.name = archive_name(&repository.location);
        return Ok(Resolved {
            repository,
            reference: LATEST.into(),
        });
    }
    if let Some(reference) = reference {
        validate_reference(reference)?;
        return Ok(Resolved {
            repository,
            reference: reference.into(),
        });
    }
    let reference = if release_base(&repository.location).is_some() {
        LATEST
    } else {
        "HEAD"
    };
    Ok(Resolved {
        repository,
        reference: reference.into(),
    })
}

/// Resolve an installed corpus back into the source a refresh must use.
///
/// Only `latest` selects a release archive. Every other reference is a Git
/// branch, tag or commit: a repository that tags without attaching a release
/// asset keeps working, and the same reference never means two different
/// things depending on what some repository happens to publish.
pub(super) fn from_installed(
    location: &str,
    reference: &str,
) -> Result<InstallSource, CorpusError> {
    if is_archive_location(location) {
        return Ok(InstallSource::Archive {
            url: location.to_string(),
        });
    }
    validate_reference(reference)?;
    if is_release_reference(reference) {
        if let Some(release) = release_base(location) {
            return Ok(InstallSource::Archive {
                url: latest_asset_url(&release),
            });
        }
    }
    Ok(InstallSource::Git {
        repository: location.to_string(),
        reference: reference.to_string(),
    })
}

pub(super) fn is_archive_location(location: &str) -> bool {
    let lower = location.to_ascii_lowercase();
    lower.ends_with(".tar.gz") || lower.ends_with(".tgz")
}

fn archive_name(location: &str) -> String {
    let name = location
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let name = name
        .strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix(".tgz"))
        .unwrap_or(name);
    if name.is_empty() {
        "corpus".into()
    } else {
        name.to_string()
    }
}

/// A reference that is not `latest` is always a Git reference. Kept as a
/// named rule so `resolve` and `from_installed` cannot drift apart.
fn is_release_reference(reference: &str) -> bool {
    reference == LATEST
}

/// Whether this location has a release to try before Git.
///
/// True only for a GitHub repository URL: an explicit archive URL names one
/// file, and a non-GitHub host has no release root to derive.
pub(super) fn derives_release(location: &str) -> bool {
    !is_archive_location(location) && release_base(location).is_some()
}

/// Whether a failed archive download may fall back to the repository's Git
/// history. Only an installation that asked for "the newest release" qualifies:
/// an explicit archive URL has no repository to fall back to.
pub(super) fn may_fall_back_to_git(location: &str, reference: &str) -> bool {
    is_release_reference(reference) && derives_release(location)
}

pub(super) fn git_head_reference() -> &'static str {
    "HEAD"
}

struct GithubRelease {
    owner: String,
    repo: String,
}

fn release_base(location: &str) -> Option<GithubRelease> {
    let url = url::Url::parse(location).ok()?;
    if url.scheme() != "https" || url.host_str()? != "github.com" {
        return None;
    }
    let segments: Vec<&str> = url.path().trim_matches('/').split('/').collect();
    if segments.len() != 2 {
        return None;
    }
    let repo = segments[1].trim_end_matches(".git");
    if repo.is_empty() {
        return None;
    }
    Some(GithubRelease {
        owner: segments[0].to_string(),
        repo: repo.to_string(),
    })
}

fn asset_name(release: &GithubRelease) -> String {
    format!("{}.tar.gz", release.repo)
}

fn latest_asset_url(release: &GithubRelease) -> String {
    format!(
        "https://github.com/{}/{}/releases/latest/download/{}",
        release.owner,
        release.repo,
        asset_name(release)
    )
}

pub(super) fn validate_location(location: &str) -> Result<(), CorpusError> {
    let url = url::Url::parse(location)
        .map_err(|_| CorpusError::InvalidSource("Invalid repository URL".into()))?;
    if !matches!(url.scheme(), "https" | "file")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() == "https" && url.host_str().is_none())
        || (url.scheme() == "file" && url.to_file_path().is_err())
    {
        return Err(CorpusError::InvalidSource(
            "Use HTTPS or a local file repository; embedded credentials, queries and fragments are unsupported".into()
        ));
    }
    Ok(())
}

pub(super) fn validate_name(name: &str) -> Result<(), CorpusError> {
    if name.len() > 100
        || !name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return Err(CorpusError::InvalidSource("Corpus names must start with an ASCII letter or digit and contain only letters, digits, '.', '_' or '-'".into()));
    }
    Ok(())
}

pub(super) fn validate_reference(reference: &str) -> Result<(), CorpusError> {
    if reference.is_empty()
        || reference.len() > 1024
        || reference.starts_with('-')
        || reference
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(CorpusError::InvalidSource(
            "Invalid Git branch, tag or commit reference".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_subdirectory(path: &Path) -> Result<(), CorpusError> {
    if path.as_os_str().is_empty()
        || path.components().any(|part| match part {
            Component::Normal(name) => name == ".git",
            Component::CurDir => false,
            _ => true,
        })
    {
        return Err(CorpusError::InvalidSource(
            "Overlay subdirectory must remain inside the checkout and outside .git".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(input: &str, reference: Option<&str>) -> InstallSource {
        let resolved = resolve(input, reference).unwrap();
        from_installed(&resolved.repository.location, &resolved.reference).unwrap()
    }

    #[test]
    fn test_archive_locations_are_detected_by_extension() {
        assert!(is_archive_location("https://example.com/corpus.tar.gz"));
        assert!(is_archive_location("https://example.com/corpus.TAR.GZ"));
        assert!(is_archive_location("file:///tmp/corpus.tgz"));
        assert!(!is_archive_location("https://github.com/o/r"));
    }

    #[test]
    fn test_default_source_is_the_release_archive() {
        match source("https://github.com/o/cli-permissions", None) {
            InstallSource::Archive { url } => assert_eq!(
                url,
                "https://github.com/o/cli-permissions/releases/latest/download/cli-permissions.tar.gz"
            ),
            InstallSource::Git { .. } => panic!("expected an archive source"),
        }
    }

    #[test]
    fn test_explicit_references_always_use_git() {
        // A repository that tags without attaching a release asset must keep
        // working, and `--ref v0.2.0` must not mean two different things
        // depending on what some repository happens to publish.
        for reference in [
            "v0.2.0",
            "main",
            "3f2a9c1d4e5b6a7c8d9e0f1a2b3c4d5e6f7a8b9c",
            "HEAD",
        ] {
            let resolved =
                resolve("https://github.com/o/cli-permissions", Some(reference)).unwrap();
            assert_eq!(resolved.reference, reference);
            match source("https://github.com/o/cli-permissions", Some(reference)) {
                InstallSource::Git {
                    reference: recorded,
                    ..
                } => assert_eq!(recorded, reference),
                InstallSource::Archive { .. } => {
                    panic!("{reference} must stay on Git")
                }
            }
        }
    }

    #[test]
    fn test_commit_and_branch_references_stay_on_git() {
        for reference in ["main", "3f2a9c1d4e5b6a7c8d9e0f1a2b3c4d5e6f7a8b9c", "HEAD"] {
            assert!(
                matches!(
                    source("https://github.com/o/cli-permissions", Some(reference)),
                    InstallSource::Git { .. }
                ),
                "{reference} must stay on Git"
            );
        }
    }

    #[test]
    fn test_non_github_hosts_stay_on_git() {
        let resolved = resolve("https://git.example.com/o/r", None).unwrap();
        assert_eq!(resolved.reference, "HEAD");
        assert!(matches!(
            source("https://git.example.com/o/r", None),
            InstallSource::Git { .. }
        ));
    }

    #[test]
    fn test_refresh_reproduces_the_installed_source() {
        assert!(matches!(
            from_installed("https://github.com/o/r", "latest").unwrap(),
            InstallSource::Archive { .. }
        ));
        for reference in ["v0.2.0", "3f2a9c1d4e5b6a7c8d9e0f1a2b3c4d5e6f7a8b9c"] {
            assert!(
                matches!(
                    from_installed("https://github.com/o/r", reference).unwrap(),
                    InstallSource::Git { .. }
                ),
                "{reference} must refresh through Git"
            );
        }
    }

    #[test]
    fn test_archive_url_ignores_a_reference_in_refresh_but_rejects_it_on_install() {
        let url = "https://example.com/corpus.tar.gz";
        assert!(matches!(
            from_installed(url, "latest").unwrap(),
            InstallSource::Archive { .. }
        ));
        assert!(resolve(url, Some("v0.2.0")).is_err());
    }

    #[test]
    fn test_fallback_only_applies_to_a_newest_release_request() {
        let github = "https://github.com/o/r";
        assert!(derives_release(github));
        assert!(may_fall_back_to_git(github, "latest"));
        // An explicit reference already uses Git, so there is nothing to fall
        // back to; an archive URL has no repository behind it.
        assert!(!may_fall_back_to_git(github, "v0.2.0"));
        assert!(!may_fall_back_to_git(
            "https://example.com/r.tar.gz",
            "latest"
        ));
        assert!(!may_fall_back_to_git(
            "https://git.example.com/o/r",
            "latest"
        ));
    }

    #[test]
    fn test_archive_name_strips_the_archive_suffix() {
        assert_eq!(
            archive_name("https://example.com/cli-permissions.tar.gz"),
            "cli-permissions"
        );
        assert_eq!(archive_name("https://example.com/x.tgz"), "x");
    }
}
