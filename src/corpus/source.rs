use super::CorpusError;
use std::path::{Component, Path};

pub(super) struct Repository {
    pub location: String,
    pub name: String,
}

pub(super) fn parse_repository(input: &str) -> Result<Repository, CorpusError> {
    let location = if Path::new(input).is_dir() {
        url::Url::from_directory_path(Path::new(input).canonicalize()?)
            .map_err(|_| CorpusError::InvalidSource("Invalid local repository path".into()))?
    } else if !input.contains("://") && input.split('/').count() == 2 {
        url::Url::parse(&format!("https://github.com/{input}"))
            .map_err(|_| CorpusError::InvalidSource("Invalid GitHub repository".into()))?
    } else {
        url::Url::parse(input).map_err(|_| {
            CorpusError::InvalidSource(
                "Expected an HTTPS repository, OWNER/REPO or local Git directory".into(),
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
