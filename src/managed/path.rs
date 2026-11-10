//! Root-relative paths for managed execution.
//!
//! A managed operand never names a host path. It names a launcher-defined root
//! ID plus a relative path, and the launcher resolves that path beneath a
//! pinned root. The rules here are the ones in
//! `docs/managed-execution/policy.md#resource-and-effect-model`:
//!
//! * components are UTF-8, 1–255 bytes, with no NUL, C0 controls, DEL, empty
//!   component, `.` or `..`;
//! * at most 64 components and 4095 bytes in the slash-joined form;
//! * resolution never follows a symlink and never crosses a mount.

use std::path::{Path, PathBuf};

use super::error::{ManagedError, ManagedErrorCode};

/// Maximum components in one relative path.
pub const MAX_COMPONENTS: usize = 64;
/// Maximum bytes in the slash-joined representation.
pub const MAX_PATH_BYTES: usize = 4095;
/// Maximum bytes in one component.
pub const MAX_COMPONENT_BYTES: usize = 255;

/// A validated path relative to a managed resource root.
///
/// The empty path names the root itself. Components are stored separately so
/// every comparison is component-wise: `/etcetera` can never match `/etc`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelPath(Vec<String>);

impl RelPath {
    /// The root itself.
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// Parse and validate a slash-joined relative path.
    pub fn parse(raw: &str) -> Result<Self, ManagedError> {
        if raw.is_empty() {
            return Ok(Self::root());
        }
        let components: Vec<String> = raw.split('/').map(str::to_string).collect();
        let path = Self(components);
        path.validate()?;
        Ok(path)
    }

    /// Append one already-validated component.
    pub fn join(&self, name: &str) -> Result<Self, ManagedError> {
        validate_component(name, "path component")?;
        let mut components = self.0.clone();
        components.push(name.to_string());
        let path = Self(components);
        path.validate()?;
        Ok(path)
    }

    /// The parent path, or `None` at the root.
    pub fn parent(&self) -> Option<Self> {
        if self.0.is_empty() {
            return None;
        }
        let mut components = self.0.clone();
        components.pop();
        Some(Self(components))
    }

    /// The final component, or `None` at the root.
    pub fn basename(&self) -> Option<&str> {
        self.0.last().map(String::as_str)
    }

    /// The components in order.
    pub fn components(&self) -> &[String] {
        &self.0
    }

    /// Whether this path names the root.
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The slash-joined representation.
    pub fn as_slash(&self) -> String {
        self.0.join("/")
    }

    /// Whether `self` is `prefix` or sits beneath it, component-wise.
    pub fn starts_with(&self, prefix: &Self) -> bool {
        prefix.0.len() <= self.0.len() && prefix.0[..] == self.0[..prefix.0.len()]
    }

    /// Join this relative path onto an absolute root.
    pub fn to_path_under(&self, root: &Path) -> PathBuf {
        let mut path = root.to_path_buf();
        for component in &self.0 {
            path.push(component);
        }
        path
    }

    /// Enforce the component count, component bytes and total byte limits.
    fn validate(&self) -> Result<(), ManagedError> {
        if self.0.len() > MAX_COMPONENTS {
            return Err(ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                format!("path has more than {MAX_COMPONENTS} components"),
            ));
        }
        for component in &self.0 {
            validate_component(component, "path component")?;
        }
        if self.as_slash().len() > MAX_PATH_BYTES {
            return Err(ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                format!("path exceeds {MAX_PATH_BYTES} bytes"),
            ));
        }
        Ok(())
    }
}

/// Validate one path component or entry basename.
///
/// `field` names the caller-supplied field in the refusal; it is one of the
/// fixed protocol names, never a caller value.
pub fn validate_component(name: &str, field: &str) -> Result<(), ManagedError> {
    let invalid = |reason: &str| {
        ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            format!("{field} {reason}"),
        )
        .with_field(field)
    };
    if name.is_empty() {
        return Err(invalid("is empty"));
    }
    if name == "." || name == ".." {
        return Err(invalid("is a dot component"));
    }
    if name.len() > MAX_COMPONENT_BYTES {
        return Err(invalid("exceeds 255 bytes"));
    }
    if name.contains('/') {
        return Err(invalid("contains a separator"));
    }
    if name
        .chars()
        .any(|c| c == '\0' || ('\u{0}'..='\u{1f}').contains(&c) || c == '\u{7f}')
    {
        return Err(invalid("contains a control character"));
    }
    Ok(())
}

/// The object type a resolution expects at the final component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    File,
    Directory,
    /// The path need not exist yet; only its parent is resolved and checked.
    FutureEntry,
}

/// Resolve `relative` beneath `root` without following a symlink or crossing a
/// mount, and check the final object type.
///
/// This is the portable resolver used by the policy compiler, the macOS
/// prototype and the test suite. The Linux backend additionally opens the
/// result with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS |
/// RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV)` so the check and the use share one
/// descriptor; see [`open_beneath`].
pub fn resolve_beneath(
    root: &Path,
    relative: &RelPath,
    expect: Expect,
) -> Result<PathBuf, ManagedError> {
    let root_meta = std::fs::symlink_metadata(root).map_err(|error| {
        ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            format!("resource root is not readable: {error}"),
        )
    })?;
    if !root_meta.is_dir() {
        return Err(ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            "resource root is not a directory",
        ));
    }
    let root_dev = device_of(&root_meta);

    let mut current = root.to_path_buf();
    let components = relative.components();
    let last = components.len().saturating_sub(1);
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let is_last = index == last;
        if is_last && expect == Expect::FutureEntry {
            // A future entry cannot be stat'ed; its parent has already been
            // validated by the loop. The basename is validated separately.
            break;
        }
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                format!("path component is not accessible: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                "path traverses a symbolic link",
            )
            .with_field("path"));
        }
        if device_of(&metadata) != root_dev {
            return Err(ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                "path crosses a mount",
            )
            .with_field("path"));
        }
        if is_last {
            match expect {
                Expect::File if !metadata.is_file() => {
                    return Err(ManagedError::planning(
                        ManagedErrorCode::InvalidArgument,
                        "path is not a regular file",
                    )
                    .with_field("path"));
                }
                Expect::Directory if !metadata.is_dir() => {
                    return Err(ManagedError::planning(
                        ManagedErrorCode::InvalidArgument,
                        "path is not a directory",
                    )
                    .with_field("path"));
                }
                _ => {}
            }
        } else if !metadata.is_dir() {
            return Err(ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                "path component is not a directory",
            )
            .with_field("path"));
        }
    }
    Ok(current)
}

#[cfg(unix)]
fn device_of(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.dev()
}

#[cfg(not(unix))]
fn device_of(_metadata: &std::fs::Metadata) -> u64 {
    0
}

/// Open a resolved path beneath `root` with the kernel's no-escape guarantees.
///
/// Linux-only: `openat2` is the "equivalent verified mechanism" the policy
/// specification requires in place of the portable lstat walk, because it makes
/// resolution and use one operation. `flags` are the `open(2)` access flags
/// without `O_CREAT`; creation goes through the typed publication primitive in
/// [`super::copy`], never through this helper.
#[cfg(target_os = "linux")]
pub fn open_beneath(
    root: &Path,
    relative: &RelPath,
    flags: i32,
) -> Result<std::os::fd::OwnedFd, ManagedError> {
    use std::os::fd::{FromRawFd, OwnedFd};

    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    const RESOLVE_NO_XDEV: u64 = 0x01;
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    const RESOLVE_BENEATH: u64 = 0x08;

    let root_file = std::fs::File::open(root).map_err(|error| {
        ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            format!("resource root is not readable: {error}"),
        )
    })?;
    use std::os::fd::AsRawFd;
    let raw = std::ffi::CString::new(relative.as_slash()).map_err(|_| {
        ManagedError::planning(ManagedErrorCode::InvalidArgument, "path contains NUL")
    })?;
    let how = OpenHow {
        flags: (flags | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV,
    };
    // SAFETY: `how` is a plain C struct, `raw` is NUL-terminated, and the
    // returned fd is owned by `OwnedFd` immediately.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root_file.as_raw_fd(),
            raw.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        return Err(ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            format!("path cannot be opened beneath its root: {error}"),
        ));
    }
    // SAFETY: the syscall returned a fresh descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relative_path_rejects_dot_and_separator_and_control() {
        for raw in ["a/../b", "./a", "a//b", "a/\u{1}b", "a/b\u{7f}"] {
            assert!(
                RelPath::parse(raw).is_err(),
                "{raw:?} must be refused as a relative path"
            );
        }
        assert!(RelPath::parse("a/b/c").is_ok());
        assert!(RelPath::parse("").unwrap().is_root());
    }

    #[test]
    fn test_relative_path_limits_are_enforced() {
        let deep = vec!["d"; MAX_COMPONENTS + 1].join("/");
        assert!(RelPath::parse(&deep).is_err(), "too many components");
        let long = "x".repeat(MAX_COMPONENT_BYTES + 1);
        assert!(RelPath::parse(&long).is_err(), "component too long");
    }

    #[test]
    fn test_prefix_matching_is_component_wise() {
        let etc = RelPath::parse("etc").unwrap();
        let etcetera = RelPath::parse("etcetera/x").unwrap();
        assert!(RelPath::parse("etc/hosts").unwrap().starts_with(&etc));
        assert!(!etcetera.starts_with(&etc));
        assert!(etc.starts_with(&RelPath::root()));
    }

    #[test]
    fn test_resolution_refuses_a_symlink_component() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        let error = resolve_beneath(
            dir.path(),
            &RelPath::parse("link").unwrap(),
            Expect::Directory,
        )
        .expect_err("a symlink must be refused");
        assert_eq!(error.code, ManagedErrorCode::InvalidArgument);
    }

    #[test]
    fn test_resolution_checks_the_final_object_type() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("file"), b"x").unwrap();
        assert!(
            resolve_beneath(dir.path(), &RelPath::parse("file").unwrap(), Expect::File).is_ok()
        );
        assert!(resolve_beneath(
            dir.path(),
            &RelPath::parse("file").unwrap(),
            Expect::Directory
        )
        .is_err());
    }

    #[test]
    fn test_a_future_entry_only_resolves_its_parent() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("out")).unwrap();
        let resolved = resolve_beneath(
            dir.path(),
            &RelPath::parse("out/new.txt").unwrap(),
            Expect::FutureEntry,
        )
        .expect("the parent exists");
        assert_eq!(resolved, dir.path().join("out/new.txt"));
    }
}
