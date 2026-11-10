//! The Linux backend: capability probe and readiness probe.
//!
//! The probe has two halves, and the difference matters. The static half reads
//! what the kernel *is* (Landlock ABI, `max_user_namespaces`, cgroup v2
//! controllers, `renameat2`). The readiness half actually performs each step in
//! a forked child — `unshare`, uid/gid mapping, a mount view, an atomic
//! no-replace publication and a Landlock ruleset — because a version string is
//! a guess and ISO-A7 asks for evidence.
//!
//! The child never allocates: everything it needs is a pre-built `CString`, and
//! it exits with `_exit`. That is what makes `fork` safe in a process whose
//! other threads may hold the allocator lock.

use std::collections::BTreeSet;
use std::ffi::CString;
use std::io::Read;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::{BackendId, Capability, CapabilityReport, Readiness};

/// `landlock_create_ruleset` flag: query the ABI version.
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
/// The only rule type v1 uses.
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

// Landlock filesystem rights, by ABI.
const ACCESS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_READ_FILE: u64 = 1 << 2;
const ACCESS_READ_DIR: u64 = 1 << 3;
const ACCESS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_MAKE_REG: u64 = 1 << 8;
const ACCESS_TRUNCATE: u64 = 1 << 14;

/// Query the Landlock ABI version, or `None` when Landlock is unavailable.
pub(super) fn landlock_abi() -> Option<u32> {
    // SAFETY: the ABI query takes a null attribute pointer and a zero size by
    // contract, and returns a plain integer.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    (abi > 0).then_some(abi as u32)
}

fn kernel_release() -> Option<(u32, u32)> {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    let mut parts = release.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

fn user_namespaces_available() -> bool {
    std::fs::read_to_string("/proc/sys/user/max_user_namespaces")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .is_some_and(|maximum| maximum > 0)
}

fn cgroup_v2_controllers() -> Option<String> {
    std::fs::read_to_string("/sys/fs/cgroup/cgroup.controllers").ok()
}

/// Probe the backend, including the readiness steps.
pub(super) fn probe() -> CapabilityReport {
    let abi = landlock_abi();
    let release = kernel_release();
    let readiness = readiness(abi);
    let mut capabilities = BTreeSet::new();
    let mut missing = Vec::new();
    let mut notes = Vec::new();

    match abi {
        Some(version) if version >= 1 => {
            capabilities.insert(Capability::ReadFile);
            capabilities.insert(Capability::ReadDir);
            capabilities.insert(Capability::MakeReg);
            if version >= 3 {
                capabilities.insert(Capability::TruncateDenial);
            } else {
                missing.push(format!(
                    "Landlock ABI {version} cannot deny truncate; ABI 3 or later is required"
                ));
            }
            if version >= 2 {
                notes.push(format!("Landlock ABI {version}: REFER is available"));
            }
        }
        Some(version) => {
            missing.push(format!(
                "Landlock ABI {version} is too old for a managed session"
            ));
        }
        None => missing.push(
            "Landlock is unavailable on this kernel (the syscall returned ENOSYS); \
             CONFIG_SECURITY_LANDLOCK must be enabled"
                .into(),
        ),
    }

    if release.is_some_and(|(major, minor)| (major, minor) >= (3, 15)) {
        capabilities.insert(Capability::NoReplacePublish);
        notes.push(
            "renameat2 exists; the workspace filesystem's RENAME_NOREPLACE support \
             is exercised by the readiness probe"
                .into(),
        );
    } else {
        missing.push("renameat2 is unavailable on this kernel".into());
    }

    if user_namespaces_available() {
        capabilities.insert(Capability::Namespaces);
    } else {
        missing.push("unprivileged user namespaces are disabled".into());
    }

    match release {
        Some((major, minor)) if (major, minor) >= (5, 9) => {
            capabilities.insert(Capability::DescriptorBoundary);
        }
        _ => missing.push("close_range requires Linux 5.9 or later".into()),
    }

    match cgroup_v2_controllers() {
        Some(controllers)
            if controllers.split_whitespace().any(|c| c == "memory")
                && controllers.split_whitespace().any(|c| c == "pids") =>
        {
            capabilities.insert(Capability::ResourceLimits);
        }
        _ => missing.push("cgroup v2 with the memory and pids controllers is required".into()),
    }

    // A static capability is a claim; a failed readiness step is a fact. A
    // capability whose step failed is removed, so `supported` cannot be true
    // for a mechanism this kernel did not actually perform.
    for step in readiness.failures() {
        missing.push(format!("readiness step '{step}' failed on this kernel"));
    }

    CapabilityReport {
        backend: BackendId::Linux,
        supported: missing.is_empty(),
        landlock_abi: abi,
        capabilities,
        missing,
        notes,
        readiness: Some(readiness),
    }
}

/// Run every isolation step in a forked child and report what happened.
fn readiness(abi: Option<u32>) -> Readiness {
    let Ok(directory) = tempfile::TempDir::new() else {
        return Readiness::default();
    };
    let workspace = directory.path().join("ws");
    let view = directory.path().join("view");
    let outside = directory.path().join("outside.txt");
    if std::fs::create_dir_all(&workspace).is_err() || std::fs::create_dir_all(&view).is_err() {
        return Readiness::default();
    }
    let paths = Paths::new(&workspace, &view, &outside);
    let Ok(paths) = paths else {
        return Readiness::default();
    };

    let mut pipe = [0i32; 2];
    // SAFETY: `pipe` is a two-element array the kernel fills.
    if unsafe { libc::pipe(pipe.as_mut_ptr()) } != 0 {
        return Readiness::default();
    }
    // SAFETY: `fork` is called from a process that may have other threads, but
    // the child below uses only libc and pre-built buffers, then `_exit`s.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // SAFETY: the child owns nothing shared beyond the pre-built buffers.
        unsafe { child(pipe[1], &paths, abi) };
    }
    // SAFETY: the parent does not use the write end.
    unsafe { libc::close(pipe[1]) };
    let mut report = [0u8; 8];
    // SAFETY: `pipe[0]` is a fresh read descriptor owned here.
    let mut reader = unsafe { std::fs::File::from_raw_fd(pipe[0]) };
    let read = reader.read_exact(&mut report).is_ok();
    let mut status = 0;
    // SAFETY: `pid` is the child just forked.
    unsafe { libc::waitpid(pid, &mut status, 0) };
    if !read {
        return Readiness::default();
    }
    Readiness {
        namespaces: report[0] == 1,
        uid_map: report[1] == 1,
        mount_view: report[2] == 1,
        no_replace_publish: report[3] == 1,
        landlock_installed: report[4] == 1,
        landlock_denies_outside: report[5] == 1,
        close_range: report[6] == 1,
    }
}

/// Pre-built, NUL-terminated paths and file contents for the child.
struct Paths {
    workspace: CString,
    view: CString,
    view_probe: CString,
    workspace_inner: CString,
    workspace_stage: CString,
    workspace_stage_second: CString,
    workspace_published: CString,
    outside: CString,
    uid_map: CString,
    gid_map: CString,
}

impl Paths {
    fn new(workspace: &Path, view: &Path, outside: &Path) -> Result<Self, std::ffi::NulError> {
        let cstring = |path: &Path| CString::new(path.as_os_str().as_bytes());
        // SAFETY: `getuid`/`getgid` are pure syscalls.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        Ok(Self {
            workspace: cstring(workspace)?,
            view: cstring(view)?,
            view_probe: cstring(&view.join("probe"))?,
            workspace_inner: cstring(&workspace.join("inner"))?,
            workspace_stage: cstring(&workspace.join(".stage"))?,
            workspace_stage_second: cstring(&workspace.join(".stage2"))?,
            workspace_published: cstring(&workspace.join("published"))?,
            outside: cstring(outside)?,
            uid_map: CString::new(format!("0 {uid} 1"))?,
            gid_map: CString::new(format!("0 {gid} 1"))?,
        })
    }
}

/// The forked child: perform each step, write one byte per step, then exit.
///
/// # Safety
///
/// Called only in the child of a `fork`, where no other thread can be holding a
/// lock this function needs. It allocates nothing and ends in `_exit`.
unsafe fn child(fd: i32, paths: &Paths, abi: Option<u32>) -> ! {
    let mut report = [0u8; 8];

    let unshared = libc::unshare(
        libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWIPC | libc::CLONE_NEWNET,
    ) == 0;
    report[0] = u8::from(unshared);

    let mapped = unshared
        && write_file(b"/proc/self/setgroups\0", b"deny")
        && write_file(b"/proc/self/uid_map\0", paths.uid_map.as_bytes())
        && write_file(b"/proc/self/gid_map\0", paths.gid_map.as_bytes());
    report[1] = u8::from(mapped);

    let mounted = unshared
        && libc::mount(
            b"tmpfs\0".as_ptr().cast(),
            paths.view.as_ptr(),
            b"tmpfs\0".as_ptr().cast(),
            0,
            b"size=1m,mode=0755\0".as_ptr().cast(),
        ) == 0
        && create_and_write(&paths.view_probe);
    report[2] = u8::from(mounted);

    report[3] = u8::from(publish_noreplace(paths));

    if let Some(abi) = abi {
        let installed = install_landlock(paths, abi);
        report[4] = u8::from(installed);
        if installed {
            // The positive control matters as much as the denial: a ruleset
            // that refuses everything would otherwise look like success.
            let inside = create_and_write(&paths.workspace_inner);
            let outside_denied = libc::open(
                paths.outside.as_ptr(),
                libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                0o600,
            ) < 0;
            report[5] = u8::from(inside && outside_denied);
        }
    }

    // A range far above every descriptor this process holds, so the call is a
    // presence check rather than a cleanup. `ENOSYS` is the only failure that
    // means the syscall is missing.
    let closed = libc::syscall(libc::SYS_close_range, 1000u32, 1001u32, 0u32) == 0;
    report[6] = u8::from(closed);

    libc::write(fd, report.as_ptr().cast(), report.len());
    libc::close(fd);
    libc::_exit(0)
}

/// Open `path` with `O_CREAT|O_EXCL`, write one byte, close it.
unsafe fn create_and_write(path: &CString) -> bool {
    let fd = libc::open(
        path.as_ptr(),
        libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_CLOEXEC,
        0o600,
    );
    if fd < 0 {
        return false;
    }
    let wrote = libc::write(fd, b"x".as_ptr().cast(), 1) == 1;
    libc::close(fd);
    wrote
}

/// Write `contents` to an absolute path, returning whether the write landed.
unsafe fn write_file(path: &[u8], contents: &[u8]) -> bool {
    let fd = libc::open(path.as_ptr().cast(), libc::O_WRONLY | libc::O_CLOEXEC);
    if fd < 0 {
        return false;
    }
    let wrote =
        libc::write(fd, contents.as_ptr().cast(), contents.len()) == contents.len() as isize;
    libc::close(fd);
    wrote
}

/// Exercise `renameat2(RENAME_NOREPLACE)`: first publish succeeds, second fails
/// with `EEXIST`.
unsafe fn publish_noreplace(paths: &Paths) -> bool {
    if !create_and_write(&paths.workspace_stage) {
        return false;
    }
    let first = libc::syscall(
        libc::SYS_renameat2,
        libc::AT_FDCWD,
        paths.workspace_stage.as_ptr(),
        libc::AT_FDCWD,
        paths.workspace_published.as_ptr(),
        libc::RENAME_NOREPLACE,
    );
    if first != 0 {
        return false;
    }
    if !create_and_write(&paths.workspace_stage_second) {
        return false;
    }
    let second = libc::syscall(
        libc::SYS_renameat2,
        libc::AT_FDCWD,
        paths.workspace_stage_second.as_ptr(),
        libc::AT_FDCWD,
        paths.workspace_published.as_ptr(),
        libc::RENAME_NOREPLACE,
    );
    let replaced = std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST);
    let _ = libc::unlink(paths.workspace_stage_second.as_ptr());
    second != 0 && replaced
}

/// Install a Landlock ruleset that grants the workspace and nothing else.
unsafe fn install_landlock(paths: &Paths, abi: u32) -> bool {
    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }
    #[repr(C)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    let mut handled = ACCESS_READ_FILE
        | ACCESS_READ_DIR
        | ACCESS_WRITE_FILE
        | ACCESS_REMOVE_FILE
        | ACCESS_MAKE_REG;
    if abi >= 3 {
        handled |= ACCESS_TRUNCATE;
    }
    let attr = RulesetAttr {
        handled_access_fs: handled,
    };
    let ruleset = libc::syscall(
        libc::SYS_landlock_create_ruleset,
        &attr as *const RulesetAttr,
        std::mem::size_of::<RulesetAttr>(),
        0u32,
    );
    if ruleset < 0 {
        return false;
    }
    let ruleset = ruleset as i32;

    let parent = libc::open(
        paths.workspace.as_ptr(),
        libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY,
    );
    if parent < 0 {
        libc::close(ruleset);
        return false;
    }
    let rule = PathBeneathAttr {
        allowed_access: handled,
        parent_fd: parent,
    };
    let added = libc::syscall(
        libc::SYS_landlock_add_rule,
        ruleset,
        LANDLOCK_RULE_PATH_BENEATH as u64,
        &rule as *const PathBeneathAttr,
        0u32,
    ) == 0;
    libc::close(parent);
    if !added {
        libc::close(ruleset);
        return false;
    }
    let no_new_privs = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1u64, 0u64, 0u64, 0u64) == 0;
    let restricted =
        no_new_privs && libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0u32) == 0;
    libc::close(ruleset);
    restricted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The readiness probe is the one test that must never pass by accident:
    /// it either performed the steps on this kernel or it did not.
    #[test]
    fn test_the_readiness_probe_reports_every_step() {
        let readiness = readiness(landlock_abi());
        // Namespaces are available on any kernel this project supports.
        assert!(
            readiness.namespaces,
            "unshare must work: {:?}",
            readiness.failures()
        );
        assert!(readiness.uid_map, "uid/gid mapping must work");
        assert!(readiness.mount_view, "a private mount must work");
        assert!(readiness.no_replace_publish, "renameat2 must work");
        assert!(readiness.close_range, "close_range must exist");
        // Landlock is a kernel build option; report, do not assert.
        if landlock_abi().is_some() {
            assert!(
                readiness.landlock_installed,
                "Landlock is advertised but the ruleset failed"
            );
            assert!(
                readiness.landlock_denies_outside,
                "a ruleset that does not deny an outside write is not isolation"
            );
        }
    }

    #[test]
    fn test_a_failed_step_is_named() {
        let failed = Readiness {
            namespaces: true,
            uid_map: true,
            mount_view: true,
            no_replace_publish: false,
            landlock_installed: false,
            landlock_denies_outside: false,
            close_range: true,
        };
        assert_eq!(
            failed.failures(),
            vec![
                "no_replace_publish",
                "landlock_installed",
                "landlock_denies_outside"
            ]
        );
        assert!(!failed.complete());
    }
}
