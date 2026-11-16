use super::CorpusError;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use url::Url;

/// A release archive is a delivery format, not a trust boundary: every byte is
/// treated as hostile input. These limits bound what a wrong or malicious
/// archive can make `init` write before any overlay is validated.
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;

/// Download, unpack and identify one corpus release archive.
///
/// Returns the sha256 of the downloaded bytes. For an archive source that
/// digest is the identity the installation records, the way a Git source
/// records a commit: it names the exact bytes that were admitted, which a
/// version number alone cannot do.
pub(super) fn fetch(
    url: &str,
    destination: &Path,
    timeout: Duration,
) -> Result<String, CorpusError> {
    let parsed = Url::parse(url)
        .map_err(|_| CorpusError::InvalidSource("Invalid corpus archive URL".into()))?;
    let bytes = match parsed.scheme() {
        "file" => read_local(&parsed)?,
        "https" => download(url, timeout)?,
        other => {
            return Err(CorpusError::InvalidSource(format!(
                "Unsupported archive scheme '{other}'; use HTTPS or a local file"
            )))
        }
    };
    let digest = hex_digest(&bytes);
    extract(&bytes, destination).map_err(|error| match error {
        // A decoder error is an I/O error with no idea which source produced
        // it. Name the URL: several archives may be tried across a lifetime,
        // and "invalid gzip header" alone does not say which one to look at.
        CorpusError::Io(inner) => CorpusError::Archive(format!("Cannot unpack {url}: {inner}")),
        other => other,
    })?;
    Ok(digest)
}

fn read_local(url: &Url) -> Result<Vec<u8>, CorpusError> {
    let path = url
        .to_file_path()
        .map_err(|_| CorpusError::InvalidSource("Invalid local archive path".into()))?;
    let metadata = std::fs::metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CorpusError::ArchiveMissing(format!("{} does not exist", path.display()))
        } else {
            CorpusError::Io(error)
        }
    })?;
    if metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(CorpusError::Archive(format!(
            "Archive {} exceeds {} bytes",
            path.display(),
            MAX_ARCHIVE_BYTES
        )));
    }
    Ok(std::fs::read(path)?)
}

fn download(url: &str, timeout: Duration) -> Result<Vec<u8>, CorpusError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| CorpusError::Archive(format!("HTTP client: {error}")))?;
    let response = client
        .get(url)
        .send()
        .map_err(|error| CorpusError::Archive(format!("Download failed: {error}")))?;
    if !response.status().is_success() {
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(CorpusError::ArchiveMissing(format!(
                "{url} returned 404; the repository publishes no release asset for this reference"
            )));
        }
        return Err(CorpusError::Archive(format!(
            "Download failed: {url} returned {status}"
        )));
    }
    if let Some(length) = response.content_length() {
        if length > MAX_ARCHIVE_BYTES {
            return Err(CorpusError::Archive(format!(
                "Archive advertises {length} bytes, above the {MAX_ARCHIVE_BYTES} byte limit"
            )));
        }
    }
    let mut body = Vec::new();
    response
        .take(MAX_ARCHIVE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|error| CorpusError::Archive(format!("Download failed: {error}")))?;
    if body.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(CorpusError::Archive(format!(
            "Archive exceeds {MAX_ARCHIVE_BYTES} bytes"
        )));
    }
    Ok(body)
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Unpack a gzip-compressed tar archive into `destination`.
///
/// Files are written directly rather than through `tar`'s unpack helper so the
/// accepted entry types stay explicit: regular files and directories, all
/// under one top-level directory, with no link, device or traversal escape.
fn extract(bytes: &[u8], destination: &Path) -> Result<(), CorpusError> {
    let parent = destination.parent().ok_or_else(|| {
        CorpusError::Archive("Archive destination has no parent directory".into())
    })?;
    std::fs::create_dir_all(parent)?;
    let staging = tempfile::tempdir_in(parent)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut root: Option<PathBuf> = None;
    let mut files = HashSet::new();
    let mut entries = 0usize;
    let mut total = 0u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(CorpusError::Archive(format!(
                "Archive contains more than {MAX_ENTRIES} entries"
            )));
        }
        let path = entry.path()?.into_owned();
        let relative = strip_top_level(&path, &mut root)?;
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                if !relative.as_os_str().is_empty() {
                    std::fs::create_dir_all(staging.path().join(&relative))?;
                }
            }
            tar::EntryType::Regular => {
                if relative.as_os_str().is_empty() {
                    return Err(CorpusError::Archive(format!(
                        "Archive entry '{}' is a file outside the top-level directory",
                        path.display()
                    )));
                }
                let size = entry.header().size()?;
                if size > MAX_ENTRY_BYTES {
                    return Err(CorpusError::Archive(format!(
                        "Archive entry '{}' exceeds {MAX_ENTRY_BYTES} bytes",
                        path.display()
                    )));
                }
                total = total.saturating_add(size);
                if total > MAX_TOTAL_BYTES {
                    return Err(CorpusError::Archive(format!(
                        "Archive expands past {MAX_TOTAL_BYTES} bytes"
                    )));
                }
                if !files.insert(relative.clone()) {
                    return Err(CorpusError::Archive(format!(
                        "Archive contains '{}' more than once",
                        relative.display()
                    )));
                }
                write_entry(&mut entry, &staging.path().join(&relative), size)?;
            }
            other => {
                return Err(CorpusError::Archive(format!(
                    "Archive entry '{}' has unsupported type {:?}",
                    path.display(),
                    other
                )))
            }
        }
    }
    if root.is_none() {
        return Err(CorpusError::Archive("Archive is empty".into()));
    }
    if !staging.path().join("manifest.json").is_file() {
        return Err(CorpusError::Archive(
            "Archive has no manifest.json at its top level; a release without a version cannot be identified".into(),
        ));
    }
    std::fs::rename(staging.path(), destination)?;
    Ok(())
}

/// Write one archive entry, creating parent directories as needed.
fn write_entry<R: Read>(entry: &mut R, target: &Path, size: u64) -> Result<(), CorpusError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(target)?;
    let written = std::io::copy(entry, &mut file)?;
    if written != size {
        return Err(CorpusError::Archive(format!(
            "Archive entry '{}' declared {size} bytes but delivered {written}",
            target.display()
        )));
    }
    Ok(())
}

/// Reject entries that escape the archive's single top-level directory.
///
/// The top-level directory is what makes an archive self-contained: it is the
/// only way to tell `cli-permissions-0.2.0/overlays/x.json` from a payload
/// aimed at `overlays/x.json` in the caller's working directory.
fn strip_top_level(path: &Path, root: &mut Option<PathBuf>) -> Result<PathBuf, CorpusError> {
    let mut components = path.components();
    let first = match components.next() {
        Some(Component::Normal(name)) => name.to_os_string(),
        _ => {
            return Err(CorpusError::Archive(format!(
                "Archive entry '{}' must be relative and start with a directory",
                path.display()
            )))
        }
    };
    match root {
        Some(existing) if existing.as_os_str() != first => {
            return Err(CorpusError::Archive(
                "Archive entries do not share one top-level directory".into(),
            ))
        }
        Some(_) => {}
        None => *root = Some(PathBuf::from(&first)),
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(name) => {
                if name == ".git" {
                    return Err(CorpusError::Archive(format!(
                        "Archive entry '{}' contains a .git component",
                        path.display()
                    )));
                }
                relative.push(name);
            }
            _ => {
                return Err(CorpusError::Archive(format!(
                    "Archive entry '{}' escapes its top-level directory",
                    path.display()
                )))
            }
        }
    }
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    fn archive(entries: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            match contents {
                Some(bytes) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(bytes.len() as u64);
                    header.set_cksum();
                    builder.append(&header, *bytes).unwrap();
                }
                None => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    header.set_cksum();
                    builder.append(&header, std::io::empty()).unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn extract_into(bytes: &[u8]) -> (tempfile::TempDir, Result<(), CorpusError>) {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("checkout");
        let result = extract(bytes, &destination);
        (directory, result)
    }

    #[test]
    fn test_extract_strips_the_single_top_level_directory() {
        let bytes = archive(&[
            ("corpus-0.1.0/", None),
            ("corpus-0.1.0/manifest.json", Some(b"{}")),
            ("corpus-0.1.0/overlays/ls.json", Some(b"{}")),
        ]);
        let (directory, result) = extract_into(&bytes);
        result.unwrap();
        let checkout = directory.path().join("checkout");
        assert!(checkout.join("manifest.json").is_file());
        assert!(checkout.join("overlays/ls.json").is_file());
    }

    #[test]
    fn test_extract_rejects_parent_traversal() {
        // `tar` refuses to build the entry, so the guard is exercised where it
        // lives: the path validator every extracted entry passes through.
        let mut root = None;
        let result = strip_top_level(Path::new("corpus-0.1.0/../escape.json"), &mut root);
        assert!(matches!(result, Err(CorpusError::Archive(_))));
    }

    #[test]
    fn test_extract_rejects_absolute_paths() {
        let mut root = None;
        let result = strip_top_level(Path::new("/etc/passwd"), &mut root);
        assert!(matches!(result, Err(CorpusError::Archive(_))));
    }

    #[test]
    fn test_extract_rejects_a_second_top_level_directory() {
        let bytes = archive(&[
            ("corpus-0.1.0/manifest.json", Some(b"{}")),
            ("other/manifest.json", Some(b"{}")),
        ]);
        let (_directory, result) = extract_into(&bytes);
        assert!(matches!(result, Err(CorpusError::Archive(_))));
    }

    #[test]
    fn test_extract_requires_a_manifest() {
        let bytes = archive(&[("corpus-0.1.0/overlays/ls.json", Some(b"{}"))]);
        let (_directory, result) = extract_into(&bytes);
        assert!(matches!(result, Err(CorpusError::Archive(_))));
    }

    #[test]
    fn test_extract_rejects_symlink_entries() {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_path("corpus-0.1.0/manifest.json").unwrap();
        header.set_link_name("/etc/passwd").unwrap();
        header.set_size(0);
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        let (_directory, result) = extract_into(&bytes);
        assert!(matches!(result, Err(CorpusError::Archive(_))));
    }

    #[test]
    fn test_digest_matches_the_sha256_of_the_downloaded_bytes() {
        // Known answer: an empty input is the canonical SHA-256 of nothing.
        assert_eq!(
            hex_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let bytes = archive(&[("corpus-0.1.0/manifest.json", Some(b"{}"))]);
        let digest = hex_digest(&bytes);
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn test_local_archive_round_trips_through_a_file_url() {
        let bytes = archive(&[(
            "corpus-0.1.0/manifest.json",
            Some(b"{\"version\":\"0.1.0\"}"),
        )]);
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("corpus.tar.gz");
        std::fs::write(&path, &bytes).unwrap();
        let url = Url::from_file_path(&path).unwrap();
        let destination = source.path().join("checkout");
        let digest = fetch(url.as_str(), &destination, Duration::from_secs(5)).unwrap();
        assert_eq!(digest, hex_digest(&bytes));
        assert!(destination.join("manifest.json").is_file());
    }

    #[test]
    fn test_missing_local_archive_names_the_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("absent.tar.gz");
        let url = Url::from_file_path(&path).unwrap();
        let result = fetch(
            url.as_str(),
            &directory.path().join("checkout"),
            Duration::from_secs(5),
        );
        match result {
            Err(CorpusError::ArchiveMissing(detail)) => {
                assert!(detail.contains("absent.tar.gz"), "{detail}")
            }
            other => panic!("expected ArchiveMissing, got {other:?}"),
        }
    }

    #[test]
    fn test_corrupt_archive_names_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("corrupt.tar.gz");
        std::fs::write(&path, b"not a gzip stream").unwrap();
        let url = Url::from_file_path(&path).unwrap();
        let error = fetch(
            url.as_str(),
            &directory.path().join("checkout"),
            Duration::from_secs(5),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("corrupt.tar.gz"), "{message}");
    }

    #[test]
    fn test_write_entry_reports_a_truncated_entry() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("file");
        let mut payload: &[u8] = b"short";
        let result = write_entry(&mut payload, &target, 10);
        assert!(matches!(result, Err(CorpusError::Archive(_))));
    }
}
