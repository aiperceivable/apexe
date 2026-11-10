//! `builtin.copy_create`: staged, bounded, no-replace publication.
//!
//! The native `cp` cannot promise "create, never replace": it has no `O_EXCL`
//! semantics and follows a destination symlink. The specification therefore
//! makes this an apexe primitive with its own provenance, and the acceptance
//! gate is the atomic no-replace publication below (POL-05):
//!
//! 1. stage bytes in an exclusively created `0600` file on the destination
//!    filesystem;
//! 2. complete the copy within the budget and `fchmod` the staged file to
//!    `0640`;
//! 3. publish with `renameat2(RENAME_NOREPLACE)` where available, or the
//!    portable `link(2)` + `unlink(2)` pair, so a destination created after
//!    planning is never replaced.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::effect::validate_basename;
use super::error::{ManagedError, ManagedErrorCode};
use super::json::hex_lower;

/// One copy request, with every path already resolved by the policy layer.
#[derive(Debug, Clone, Copy)]
pub struct CopyRequest<'a> {
    pub source: &'a Path,
    pub destination_parent: &'a Path,
    pub basename: &'a str,
    /// Launcher-owned staging directory on the destination filesystem.
    pub staging: &'a Path,
    pub max_bytes: u64,
}

/// The result of a successful publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyOutcome {
    pub bytes: u64,
}

/// Copy and publish `request`, or refuse without touching the destination.
pub fn copy_create(request: CopyRequest<'_>) -> Result<CopyOutcome, ManagedError> {
    validate_basename(request.basename)?;
    if request.max_bytes == 0 {
        return Err(ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            "max_bytes must be positive",
        ));
    }
    let source_metadata = std::fs::symlink_metadata(request.source).map_err(|error| {
        ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            format!("source is not accessible: {error}"),
        )
    })?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_file() {
        return Err(ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            "source must be a regular file",
        )
        .with_field("source"));
    }
    require_directory(request.destination_parent, "destination")?;
    require_directory(request.staging, "staging")?;

    let staging_path = staging_name(request.staging)?;
    let mut staged = create_staging(&staging_path)?;
    let bytes = match copy_bytes(request.source, &mut staged, request.max_bytes) {
        Ok(bytes) => bytes,
        Err(error) => {
            drop(staged);
            let _ = std::fs::remove_file(&staging_path);
            return Err(error);
        }
    };
    staged
        .set_permissions(std::fs::Permissions::from_mode(0o640))
        .map_err(|error| io_failure("the staged file cannot be finalized", error))?;
    staged
        .sync_all()
        .map_err(|error| io_failure("the staged file cannot be flushed", error))?;
    drop(staged);

    let destination = request.destination_parent.join(request.basename);
    match publish(&staging_path, &destination) {
        Ok(true) => Ok(CopyOutcome { bytes }),
        Ok(false) => {
            let _ = std::fs::remove_file(&staging_path);
            Err(ManagedError::planning(
                ManagedErrorCode::DestinationExists,
                "the destination already exists",
            )
            .with_field("destination"))
        }
        Err(error) => {
            let _ = std::fs::remove_file(&staging_path);
            Err(error)
        }
    }
}

/// Whether `path` is an existing directory and not a symlink.
fn require_directory(path: &Path, field: &str) -> Result<(), ManagedError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            format!("{field} is not accessible: {error}"),
        )
        .with_field(field)
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            format!("{field} must be a directory"),
        )
        .with_field(field));
    }
    Ok(())
}

/// Build a fresh staging path inside the launcher-owned staging directory.
fn staging_name(staging: &Path) -> Result<PathBuf, ManagedError> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|error| {
        ManagedError::planning(
            ManagedErrorCode::IsolationFailed,
            format!("the OS CSPRNG is unavailable: {error}"),
        )
    })?;
    Ok(staging.join(format!(".apexe-stage-{}", hex_lower(&random))))
}

/// Create the staging file exclusively, mode `0600`.
fn create_staging(path: &Path) -> Result<File, ManagedError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| io_failure("the staging file cannot be created", error))
}

/// Copy up to `max_bytes`; refuse (and leave nothing behind) beyond it.
fn copy_bytes(source: &Path, staged: &mut File, max_bytes: u64) -> Result<u64, ManagedError> {
    let mut source = File::open(source).map_err(|error| {
        ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            format!("source cannot be opened: {error}"),
        )
    })?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|error| io_failure("source cannot be read", error))?;
        if read == 0 {
            return Ok(written);
        }
        written = written.saturating_add(read as u64);
        if written > max_bytes {
            return Err(ManagedError::execution(
                ManagedErrorCode::ResourceLimit,
                "the copy exceeds the call's output budget",
            ));
        }
        staged
            .write_all(&buffer[..read])
            .map_err(|error| io_failure("the staged file cannot be written", error))?;
    }
}

/// Publish `staging` at `destination` without replacing anything.
///
/// `Ok(true)` published, `Ok(false)` the destination already exists.
fn publish(staging: &Path, destination: &Path) -> Result<bool, ManagedError> {
    #[cfg(target_os = "linux")]
    {
        match rename_noreplace(staging, destination) {
            Ok(()) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP)
                ) => {}
            Err(error) => return Err(io_failure("publication failed", error)),
        }
    }
    match std::fs::hard_link(staging, destination) {
        Ok(()) => {
            let _ = std::fs::remove_file(staging);
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(io_failure("publication failed", error)),
    }
}

/// `renameat2(RENAME_NOREPLACE)`, the atomic Linux publication.
#[cfg(target_os = "linux")]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let from = std::ffi::CString::new(from.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: both pointers are NUL-terminated and the flag is a constant.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn io_failure(context: &str, error: std::io::Error) -> ManagedError {
    ManagedError::execution(
        ManagedErrorCode::IsolationFailed,
        format!("{context}: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: tempfile::TempDir,
        source: PathBuf,
        parent: PathBuf,
        staging: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let parent = dir.path().join("out");
            let staging = dir.path().join("staging");
            let source = dir.path().join("source.txt");
            std::fs::create_dir(&parent).unwrap();
            std::fs::create_dir(&staging).unwrap();
            std::fs::write(&source, b"hello").unwrap();
            Self {
                dir,
                source,
                parent,
                staging,
            }
        }

        fn request(&self) -> CopyRequest<'_> {
            CopyRequest {
                source: &self.source,
                destination_parent: &self.parent,
                basename: "copy.txt",
                staging: &self.staging,
                max_bytes: 1024,
            }
        }

        fn destination(&self) -> PathBuf {
            self.parent.join("copy.txt")
        }

        fn staging_entries(&self) -> usize {
            std::fs::read_dir(&self.staging).unwrap().count()
        }
    }

    #[test]
    fn test_copy_publishes_a_complete_file_and_cleans_staging() {
        let fixture = Fixture::new();
        let outcome = copy_create(fixture.request()).unwrap();
        assert_eq!(outcome.bytes, 5);
        assert_eq!(std::fs::read(fixture.destination()).unwrap(), b"hello");
        assert_eq!(fixture.staging_entries(), 0, "staging must be cleaned");
        let mode = std::fs::metadata(fixture.destination())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[test]
    fn test_an_existing_destination_is_never_replaced() {
        let fixture = Fixture::new();
        std::fs::write(fixture.destination(), b"original").unwrap();
        let error = copy_create(fixture.request()).expect_err("must not replace");
        assert_eq!(error.code, ManagedErrorCode::DestinationExists);
        assert_eq!(std::fs::read(fixture.destination()).unwrap(), b"original");
    }

    #[test]
    fn test_a_symlink_at_the_destination_is_not_followed() {
        let fixture = Fixture::new();
        let target = fixture.dir.path().join("target.txt");
        std::fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink(&target, fixture.destination()).unwrap();
        let error = copy_create(fixture.request()).expect_err("must not follow a link");
        assert_eq!(error.code, ManagedErrorCode::DestinationExists);
        assert_eq!(std::fs::read(&target).unwrap(), b"target");
    }

    #[test]
    fn test_a_directory_source_is_refused() {
        let fixture = Fixture::new();
        let mut request = fixture.request();
        request.source = &fixture.parent;
        let error = copy_create(request).expect_err("only a regular file may be copied");
        assert_eq!(error.code, ManagedErrorCode::InvalidArgument);
    }

    #[test]
    fn test_exceeding_the_budget_leaves_no_destination() {
        let fixture = Fixture::new();
        let mut request = fixture.request();
        request.max_bytes = 2;
        let error = copy_create(request).expect_err("the budget is finite");
        assert_eq!(error.code, ManagedErrorCode::ResourceLimit);
        assert!(!fixture.destination().exists());
        assert_eq!(
            fixture.staging_entries(),
            0,
            "a refused copy must not leave staging behind"
        );
    }

    #[test]
    fn test_an_invalid_basename_is_refused() {
        let fixture = Fixture::new();
        for bad in ["", ".", "..", "a/b", "a\u{1}b"] {
            let mut request = fixture.request();
            request.basename = bad;
            let error = copy_create(request).expect_err("an invalid basename must be refused");
            assert_eq!(error.code, ManagedErrorCode::InvalidArgument, "{bad:?}");
        }
    }
}
