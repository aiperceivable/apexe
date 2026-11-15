//! Sandbox delegation: translate the path policy into an OS/container backend.
//!
//! This module does not implement a sandbox. It selects an available backend,
//! normalizes the effective [`PathPolicy`] once, translates it into a
//! [`SandboxPlan`], and lets the executor spawn that plan through its one spawn
//! point. The backend remains responsible for enforcement.
//!
//! 0.15.0 ships `systemd-run`, `bwrap`, `sandbox-exec` and `docker`. `nono`,
//! microVMs, remote sandboxes and third-party adapters are later.

use crate::config::{
    ApexeConfig, DockerSandboxConfig, PathPolicy, SandboxBackend, SandboxConfig, SandboxMode,
};
use apcore::{ErrorCode, ModuleError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;
use thiserror::Error;

/// The concrete backend after `auto` has been resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedBackend {
    SystemdRun,
    Bwrap,
    SandboxExec,
    Docker,
}

impl ResolvedBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemdRun => "systemd-run",
            Self::Bwrap => "bwrap",
            Self::SandboxExec => "sandbox-exec",
            Self::Docker => "docker",
        }
    }

    pub fn enforcement(self) -> &'static str {
        "enforcing"
    }

    fn supports_write_only(self) -> bool {
        // Seatbelt can deny reads while leaving writes allowed. The other three
        // cannot express write-only without approximating it as read-write.
        matches!(self, Self::SandboxExec)
    }
}

/// One normalized path and the spellings a backend may need to emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSpelling {
    pub original: PathBuf,
    pub resolved: PathBuf,
    pub is_file: bool,
    pub exists: bool,
}

/// The effective path policy after normalization.
#[derive(Debug, Clone, Default)]
pub struct MountPlan {
    pub read_only: Vec<PathSpelling>,
    pub read_write: Vec<PathSpelling>,
    pub no_access: Vec<PathSpelling>,
    pub write_only: Vec<PathSpelling>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MountAccess {
    ReadOnly,
    ReadWrite,
    WriteOnly,
    NoAccess,
}

impl MountAccess {
    /// Order used for equal-specificity entries. A no-access entry is last so
    /// an exact duplicate cannot be re-opened by a read-write entry.
    fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::ReadWrite => 1,
            Self::WriteOnly => 2,
            Self::NoAccess => 3,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MountEntry<'a> {
    spelling: &'a PathSpelling,
    access: MountAccess,
}

impl MountPlan {
    pub fn from_policy(policy: &PathPolicy, cwd: &Path) -> Self {
        // The working directory is a path too: resolve a symlinked cwd once so
        // `--chdir`/`--workdir` point at the same inode the mounts use.
        let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let mut plan = Self {
            cwd: cwd.clone(),
            ..Self::default()
        };
        plan.read_only = dedup(normalize_all(&policy.read_only, &cwd));
        plan.read_write = dedup(normalize_all(&policy.read_write, &cwd));
        plan.no_access = dedup(normalize_all(&policy.no_access, &cwd));
        plan.write_only = dedup(normalize_all(&policy.write_only, &cwd));
        plan
    }

    /// Entries in increasing specificity. A later mount/rule therefore wins
    /// over a broader one, matching the path guard's deepest-match rule.
    fn ordered_entries(&self) -> Vec<MountEntry<'_>> {
        let mut entries = Vec::new();
        for spelling in &self.read_only {
            entries.push(MountEntry {
                spelling,
                access: MountAccess::ReadOnly,
            });
        }
        for spelling in &self.read_write {
            entries.push(MountEntry {
                spelling,
                access: MountAccess::ReadWrite,
            });
        }
        for spelling in &self.write_only {
            entries.push(MountEntry {
                spelling,
                access: MountAccess::WriteOnly,
            });
        }
        for spelling in &self.no_access {
            entries.push(MountEntry {
                spelling,
                access: MountAccess::NoAccess,
            });
        }
        entries.sort_by(|left, right| {
            path_depth(&left.spelling.resolved)
                .cmp(&path_depth(&right.spelling.resolved))
                .then_with(|| left.access.rank().cmp(&right.access.rank()))
        });
        entries
    }
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn dedup(mut entries: Vec<PathSpelling>) -> Vec<PathSpelling> {
    let mut seen = std::collections::HashSet::new();
    entries.retain(|entry| seen.insert(entry.resolved.clone()));
    entries
}

fn normalize_all(paths: &[PathBuf], cwd: &Path) -> Vec<PathSpelling> {
    paths
        .iter()
        .map(|path| normalize_path(path, cwd))
        .filter(|entry| entry.exists)
        .collect()
}

fn normalize_path(path: &Path, cwd: &Path) -> PathSpelling {
    let expanded = expand_tilde(path);
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    let original = lexical_normalize(&absolute);
    // Canonicalize the un-folded absolute path: the kernel resolves a symlink
    // before a following `..`, while `lexical_normalize` does the opposite.
    let resolved = std::fs::canonicalize(&absolute).unwrap_or_else(|_| original.clone());
    let metadata = std::fs::metadata(&resolved).ok();
    let is_file = metadata.as_ref().is_some_and(std::fs::Metadata::is_file);
    PathSpelling {
        original,
        resolved,
        is_file,
        exists: metadata.is_some(),
    }
}

fn expand_tilde(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if text == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    let Some(rest) = text.strip_prefix("~/") else {
        return path.to_path_buf();
    };
    dirs::home_dir()
        .map(|home| home.join(rest))
        .unwrap_or_else(|| path.to_path_buf())
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                out.push(component.as_os_str())
            }
        }
    }
    out
}

/// The result of the dry-run plan, and the audit record of an applied plan.
#[derive(Debug, Clone, Serialize)]
pub struct SandboxOutcome {
    pub backend: String,
    pub enforcement: String,
    pub outcome: String,
    pub policy_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A translated launch, held by the executor until the child has started.
#[derive(Debug)]
pub struct SandboxPlan {
    pub backend: ResolvedBackend,
    pub enforcement: String,
    pub program: String,
    pub args: Vec<String>,
    pub policy_digest: String,
    /// Kept alive until the child has opened the profile. `sandbox-exec` reads
    /// it by path; the temporary file is removed when this plan drops.
    _profile: Option<NamedTempFile>,
}

impl SandboxPlan {
    pub fn outcome(&self) -> SandboxOutcome {
        SandboxOutcome {
            backend: self.backend.as_str().to_string(),
            enforcement: self.enforcement.clone(),
            outcome: "applied".to_string(),
            policy_digest: self.policy_digest.clone(),
            reason: None,
        }
    }

    pub fn planned_outcome(&self) -> SandboxOutcome {
        SandboxOutcome {
            backend: self.backend.as_str().to_string(),
            enforcement: self.enforcement.clone(),
            outcome: "planned".to_string(),
            policy_digest: self.policy_digest.clone(),
            reason: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SandboxCheck {
    pub backend: String,
    pub enforcement: String,
    pub available: bool,
    pub reason: Option<String>,
    pub capabilities: Vec<String>,
    pub mode: String,
    pub modules: Vec<String>,
    pub external: String,
    pub docker_image: Option<String>,
    pub probe: String,
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("sandbox backend '{backend}' is unavailable: {reason}")]
    Unavailable { backend: String, reason: String },
    #[error("sandbox backend '{backend}' cannot express the policy: {reason}")]
    Unsupported { backend: String, reason: String },
    #[error("sandbox translation failed: {0}")]
    Translation(String),
}

impl SandboxError {
    pub fn into_module_error(self) -> ModuleError {
        let outcome = self.outcome();
        let mut details = std::collections::HashMap::new();
        details.insert(
            "sandbox_outcome".to_string(),
            serde_json::json!(outcome.outcome),
        );
        details.insert(
            "sandbox_backend".to_string(),
            serde_json::json!(outcome.backend),
        );
        details.insert(
            "sandbox_reason".to_string(),
            serde_json::json!(outcome.reason),
        );
        ModuleError::new(ErrorCode::ACLDenied, self.to_string())
            .with_details(details)
            .with_ai_guidance(
                "The sandbox is a governance boundary. Run `apexe sandbox check` to inspect the \
                 selected backend, its capabilities and the refusal reason. `sandbox.required` \
                 and `sandbox.auto` refuse rather than fall back to unconfined execution.",
            )
    }

    /// The audit record for a refusal that happened before a child was spawned.
    pub fn outcome(&self) -> SandboxOutcome {
        let (backend, outcome) = match self {
            Self::Unavailable { backend, .. } => (backend.clone(), "unavailable"),
            Self::Unsupported { backend, .. } => (backend.clone(), "refused"),
            Self::Translation(_) => ("unknown".to_string(), "refused"),
        };
        SandboxOutcome {
            backend,
            enforcement: "advisory".to_string(),
            outcome: outcome.to_string(),
            policy_digest: String::new(),
            reason: Some(self.to_string()),
        }
    }
}

/// The runtime attached to the shared executor when `sandbox` is enabled.
#[derive(Debug)]
pub struct SandboxRuntime {
    config: SandboxConfig,
    policy: PathPolicy,
    cwd: PathBuf,
}

impl SandboxRuntime {
    pub fn new(config: SandboxConfig, policy: PathPolicy, cwd: PathBuf) -> Self {
        Self {
            config,
            policy,
            cwd,
        }
    }

    /// Build from the effective configuration and the installed path guard.
    pub fn from_config(config: &ApexeConfig) -> Self {
        let guard = crate::governance::path_guard::active();
        Self::new(
            config.sandbox.clone(),
            guard.sandbox_policy(),
            guard.root().to_path_buf(),
        )
    }

    pub fn is_enabled(&self) -> bool {
        self.config.default != SandboxMode::Off && !self.config.modules.is_empty()
    }

    /// Only selected module ids are confined; the module id is the same
    /// vocabulary ACL uses.
    pub fn applies(&self, module_id: &str) -> bool {
        self.is_enabled()
            && self
                .config
                .modules
                .iter()
                .any(|selected| selected == module_id)
    }

    /// The check is static: it reports whether the backend exists and whether
    /// the Docker image is configured. It does not run the A/B/C probes.
    pub fn check(&self) -> SandboxCheck {
        let backend = match self.resolve_backend() {
            Ok(backend) => backend,
            Err(error) => {
                return SandboxCheck {
                    backend: self.config.backend.as_str().to_string(),
                    enforcement: "advisory".to_string(),
                    available: false,
                    reason: Some(error.to_string()),
                    capabilities: Vec::new(),
                    mode: self.config.default.as_str().to_string(),
                    modules: self.config.modules.clone(),
                    external: self.config.external.as_str().to_string(),
                    docker_image: self.config.backends.docker.image.clone(),
                    probe: "static".to_string(),
                };
            }
        };
        let plan = MountPlan::from_policy(&self.policy, &self.cwd);
        if let Some(reason) = capability_refusal(backend, &plan) {
            return SandboxCheck {
                backend: backend.as_str().to_string(),
                enforcement: "advisory".to_string(),
                available: false,
                reason: Some(reason),
                capabilities: capabilities(backend),
                mode: self.config.default.as_str().to_string(),
                modules: self.config.modules.clone(),
                external: self.config.external.as_str().to_string(),
                docker_image: self.config.backends.docker.image.clone(),
                probe: "static".to_string(),
            };
        }
        SandboxCheck {
            backend: backend.as_str().to_string(),
            enforcement: backend.enforcement().to_string(),
            available: true,
            reason: None,
            capabilities: capabilities(backend),
            mode: self.config.default.as_str().to_string(),
            modules: self.config.modules.clone(),
            external: self.config.external.as_str().to_string(),
            docker_image: self.config.backends.docker.image.clone(),
            probe: "static".to_string(),
        }
    }

    /// Translate the effective policy for one call. `None` means the module is
    /// not selected; `Err` means the selected mode cannot run it.
    pub fn plan(
        &self,
        module_id: &str,
        binary: &str,
        argv: &[String],
    ) -> Result<Option<SandboxPlan>, SandboxError> {
        if !self.applies(module_id) {
            return Ok(None);
        }
        let backend = self.resolve_backend()?;
        let plan = MountPlan::from_policy(&self.policy, &self.cwd);
        if let Some(reason) = capability_refusal(backend, &plan) {
            return Err(SandboxError::Unsupported {
                backend: backend.as_str().to_string(),
                reason,
            });
        }
        self.translate(backend, &plan, binary, argv).map(Some)
    }

    fn resolve_backend(&self) -> Result<ResolvedBackend, SandboxError> {
        let candidates: Vec<SandboxBackend> = match self.config.backend {
            SandboxBackend::Auto => platform_candidates(),
            explicit => vec![explicit],
        };
        let mut reasons = Vec::new();
        for candidate in candidates {
            match candidate {
                SandboxBackend::Auto => continue,
                SandboxBackend::SystemdRun => {
                    if which("systemd-run").is_none() {
                        reasons.push("systemd-run not found in PATH".to_string());
                    } else if effective_root() {
                        return Ok(ResolvedBackend::SystemdRun);
                    } else {
                        // The user manager accepts ReadOnlyPaths/InaccessiblePaths
                        // but silently ignores them: a `systemd-run --user` service
                        // wrote into a ReadOnlyPaths directory in the 0.15.0 probe
                        // (Debian 12, systemd 252). Reporting that as enforcing
                        // would be a false security claim.
                        reasons.push(
                            "systemd-run --user does not enforce ReadOnlyPaths/InaccessiblePaths; \
                             use bwrap, or run apexe as a system service (root)"
                                .to_string(),
                        );
                    }
                }
                SandboxBackend::Bwrap => {
                    if which("bwrap").is_some() {
                        return Ok(ResolvedBackend::Bwrap);
                    }
                    reasons.push(
                        "bwrap not found in PATH (install the bubblewrap package)".to_string(),
                    );
                }
                SandboxBackend::SandboxExec => {
                    if which("sandbox-exec").is_some() {
                        return Ok(ResolvedBackend::SandboxExec);
                    }
                    reasons.push("sandbox-exec not found in PATH".to_string());
                }
                SandboxBackend::Docker => match self.docker_image() {
                    Some(_) if which("docker").is_some() => return Ok(ResolvedBackend::Docker),
                    Some(_) => reasons.push("docker not found in PATH".to_string()),
                    None => reasons.push(
                        "docker image is empty; set sandbox.backends.docker.image".to_string(),
                    ),
                },
            }
        }
        Err(SandboxError::Unavailable {
            backend: self.config.backend.as_str().to_string(),
            reason: reasons.join("; "),
        })
    }

    fn docker_image(&self) -> Option<String> {
        self.config
            .backends
            .docker
            .image
            .as_ref()
            .map(|image| image.trim())
            .filter(|image| !image.is_empty())
            .map(ToString::to_string)
    }

    fn translate(
        &self,
        backend: ResolvedBackend,
        plan: &MountPlan,
        binary: &str,
        argv: &[String],
    ) -> Result<SandboxPlan, SandboxError> {
        let mut profile_text = None;
        let (program, args, profile) = match backend {
            ResolvedBackend::SystemdRun => (
                "systemd-run".to_string(),
                systemd_run_args(plan, binary, argv, effective_root()),
                None,
            ),
            ResolvedBackend::Bwrap => ("bwrap".to_string(), bwrap_args(plan, binary, argv), None),
            ResolvedBackend::SandboxExec => {
                let profile = sandbox_exec_profile(plan);
                let file = write_profile(&profile)?;
                let path = file.path().to_path_buf();
                let mut args = vec!["-f".to_string(), path.display().to_string()];
                args.push("--".to_string());
                args.push(binary.to_string());
                args.extend(argv.iter().cloned());
                profile_text = Some(profile);
                ("sandbox-exec".to_string(), args, Some(file))
            }
            ResolvedBackend::Docker => (
                "docker".to_string(),
                docker_args(&self.config.backends.docker, plan, binary, argv)?,
                None,
            ),
        };
        let profile_digest = profile
            .as_ref()
            .map(|file| (file.path(), profile_text.as_deref().unwrap_or_default()));
        let digest = policy_digest(backend, &program, &args, profile_digest);
        Ok(SandboxPlan {
            backend,
            enforcement: backend.enforcement().to_string(),
            program,
            args,
            policy_digest: digest,
            _profile: profile,
        })
    }
}

fn platform_candidates() -> Vec<SandboxBackend> {
    #[cfg(target_os = "macos")]
    {
        vec![SandboxBackend::SandboxExec, SandboxBackend::Docker]
    }
    #[cfg(target_os = "linux")]
    {
        vec![SandboxBackend::Bwrap, SandboxBackend::Docker]
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        vec![SandboxBackend::Docker]
    }
}

fn capability_refusal(backend: ResolvedBackend, plan: &MountPlan) -> Option<String> {
    if !plan.write_only.is_empty() && !backend.supports_write_only() {
        return Some(format!(
            "write_only is not expressible by {}",
            backend.as_str()
        ));
    }
    None
}

fn capabilities(backend: ResolvedBackend) -> Vec<String> {
    let mut capabilities = vec![
        "read_only".to_string(),
        "read_write".to_string(),
        "no_access".to_string(),
        "deny_within_grant".to_string(),
    ];
    if backend.supports_write_only() {
        capabilities.push("write_only".to_string());
    }
    if matches!(backend, ResolvedBackend::SandboxExec) {
        // Seatbelt is the only built-in whose file rule denies metadata too.
        capabilities.push("no_access_metadata".to_string());
    }
    capabilities
}

fn which(name: &str) -> Option<PathBuf> {
    which::which_in(
        name,
        Some(crate::runtime_path::search_path()),
        std::env::current_dir().ok()?,
    )
    .ok()
}

fn policy_digest(
    backend: ResolvedBackend,
    program: &str,
    args: &[String],
    profile: Option<(&Path, &str)>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(backend.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(program.as_bytes());
    for arg in args {
        hasher.update([0]);
        if profile.is_some_and(|(path, _)| Path::new(arg) == path) {
            hasher.update(b"<profile>");
        } else {
            hasher.update(arg.as_bytes());
        }
    }
    if let Some((_, contents)) = profile {
        hasher.update([0]);
        hasher.update(b"profile:");
        hasher.update(contents.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn effective_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and no memory effects.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn systemd_run_args(plan: &MountPlan, binary: &str, argv: &[String], system: bool) -> Vec<String> {
    let mut args = Vec::new();
    if !system {
        args.push("--user".to_string());
    }
    args.extend([
        "--quiet".to_string(),
        "--pipe".to_string(),
        "--wait".to_string(),
        "--collect".to_string(),
        "-p".to_string(),
        "NoNewPrivileges=yes".to_string(),
        "-p".to_string(),
        "PrivateNetwork=yes".to_string(),
        "-p".to_string(),
        // Deliberately no `PrivateTmp=yes`: it replaces `/tmp` before the path
        // properties are applied, so a read_only/no_access entry below `/tmp`
        // (including `~/.apexe` when HOME lives under a temporary directory)
        // would reference a path that no longer exists and the unit would fail
        // with NAMESPACE. The explicit path policy is the contract here.
        format!("WorkingDirectory={}", plan.cwd.display()),
    ]);
    for spelling in &plan.read_only {
        args.push("-p".to_string());
        args.push(format!("ReadOnlyPaths={}", spelling.resolved.display()));
    }
    for spelling in &plan.no_access {
        args.push("-p".to_string());
        args.push(format!("InaccessiblePaths={}", spelling.resolved.display()));
    }
    for spelling in &plan.read_write {
        args.push("-p".to_string());
        args.push(format!("ReadWritePaths={}", spelling.resolved.display()));
    }
    args.push("--".to_string());
    args.push(binary.to_string());
    args.extend(argv.iter().cloned());
    args
}

fn bind_cwd_args(args: &mut Vec<String>, plan: &MountPlan) {
    if plan.cwd == Path::new("/") {
        return;
    }
    args.push("--bind".to_string());
    args.push(plan.cwd.display().to_string());
    args.push(plan.cwd.display().to_string());
}

fn bwrap_args(plan: &MountPlan, binary: &str, argv: &[String]) -> Vec<String> {
    let mut args = vec![
        "--unshare-all".to_string(),
        "--new-session".to_string(),
        "--die-with-parent".to_string(),
        "--cap-drop".to_string(),
        "ALL".to_string(),
        // The path policy's default is read-write. Start from the host tree and
        // apply the stricter mounts after it; later binds win.
        "--bind".to_string(),
        "/".to_string(),
        "/".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
    ];
    // `--tmpfs /tmp` is applied above; bind the cwd after it so a workspace
    // under `/tmp` is visible instead of being replaced by the private tmpfs.
    bind_cwd_args(&mut args, plan);
    for entry in plan.ordered_entries() {
        match entry.access {
            MountAccess::ReadOnly => {
                args.push("--ro-bind".to_string());
                args.push(entry.spelling.resolved.display().to_string());
                args.push(entry.spelling.resolved.display().to_string());
            }
            MountAccess::ReadWrite => {
                args.push("--bind".to_string());
                args.push(entry.spelling.resolved.display().to_string());
                args.push(entry.spelling.resolved.display().to_string());
            }
            MountAccess::NoAccess => {
                if entry.spelling.is_file {
                    args.push("--ro-bind".to_string());
                    args.push("/dev/null".to_string());
                    args.push(entry.spelling.resolved.display().to_string());
                } else {
                    args.push("--tmpfs".to_string());
                    args.push(entry.spelling.resolved.display().to_string());
                }
            }
            MountAccess::WriteOnly => {
                // `plan()` refuses this before translation; keep the branch
                // exhaustive without pretending bwrap can express it.
            }
        }
    }
    args.push("--chdir".to_string());
    args.push(plan.cwd.display().to_string());
    args.push("--".to_string());
    args.push(binary.to_string());
    args.extend(argv.iter().cloned());
    args
}

fn sandbox_exec_profile(plan: &MountPlan) -> String {
    let mut lines = vec![
        "(version 1)".to_string(),
        // The path policy's default is read-write. The default allow keeps that
        // semantics; the explicit rules below narrow it. Rules are emitted in
        // increasing path specificity, so the deepest matching entry wins.
        "(allow default)".to_string(),
        "(deny network*)".to_string(),
    ];
    for entry in plan.ordered_entries() {
        for filter in spellings(entry.spelling) {
            match entry.access {
                MountAccess::ReadOnly => lines.push(format!("(deny file-write* {filter})")),
                MountAccess::ReadWrite => {
                    // Re-open both rights: a deeper read_write carve-out may
                    // sit under a broader no_access entry.
                    lines.push(format!("(allow file-read* file-write* {filter})"));
                }
                MountAccess::NoAccess => {
                    lines.push(format!("(deny file-read* file-write* {filter})"));
                }
                MountAccess::WriteOnly => lines.push(format!("(deny file-read* {filter})")),
            }
        }
    }
    lines.join("\n") + "\n"
}

fn spellings(spelling: &PathSpelling) -> Vec<String> {
    let mut out = Vec::new();
    let literal = if spelling.is_file {
        "literal"
    } else {
        "subpath"
    };
    out.push(format!(
        "({literal} \"{}\")",
        profile_escape(&spelling.resolved.display().to_string())
    ));
    if spelling.original != spelling.resolved {
        out.push(format!(
            "({literal} \"{}\")",
            profile_escape(&spelling.original.display().to_string())
        ));
    }
    out
}

fn profile_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn write_profile(profile: &str) -> Result<NamedTempFile, SandboxError> {
    let mut file = NamedTempFile::new()
        .map_err(|error| SandboxError::Translation(format!("profile file: {error}")))?;
    use std::io::Write;
    file.write_all(profile.as_bytes())
        .map_err(|error| SandboxError::Translation(format!("profile write: {error}")))?;
    file.flush()
        .map_err(|error| SandboxError::Translation(format!("profile flush: {error}")))?;
    Ok(file)
}

fn is_system_mount(path: &Path) -> bool {
    // Exact roots, not prefixes: a user path under `/private/tmp` or
    // `/private/var` is still host data and must be mounted.
    const SYSTEM_ROOTS: &[&str] = &[
        "/bin",
        "/boot",
        "/dev",
        "/etc",
        "/lib",
        "/lib32",
        "/lib64",
        "/proc",
        "/run",
        "/sbin",
        "/sys",
        "/usr",
        "/var",
        "/tmp",
        "/System",
        "/Library",
        "/Applications",
        "/private/etc",
        "/private/var",
        "/private/tmp",
    ];
    SYSTEM_ROOTS.iter().any(|root| path == Path::new(root))
}

fn docker_args(
    config: &DockerSandboxConfig,
    plan: &MountPlan,
    binary: &str,
    argv: &[String],
) -> Result<Vec<String>, SandboxError> {
    let image = config
        .image
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SandboxError::Unavailable {
            backend: "docker".to_string(),
            reason: "sandbox.backends.docker.image is empty".to_string(),
        })?;

    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--network".to_string(),
        config.network.as_str().to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
        "--cap-drop".to_string(),
        "ALL".to_string(),
        "--security-opt".to_string(),
        "no-new-privileges".to_string(),
    ];
    if config.read_only {
        args.push("--read-only".to_string());
    }
    if let Some(user) = config
        .user
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        args.push("--user".to_string());
        args.push(user.clone());
    }
    if let Some(memory) = config
        .memory
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        args.push("--memory".to_string());
        args.push(memory.clone());
    }
    if config.pids > 0 {
        args.push("--pids-limit".to_string());
        args.push(config.pids.to_string());
    }
    // The working directory is the default read-write path. Bind it explicitly
    // because the container's root filesystem is read-only. Policy entries are
    // mounted afterwards in increasing specificity, so a deeper deny wins.
    if plan.cwd != Path::new("/") {
        args.push("--mount".to_string());
        args.push(format!(
            "type=bind,src={},dst={}",
            plan.cwd.display(),
            plan.cwd.display()
        ));
    }
    docker_mount_args(&mut args, plan);
    args.push("--workdir".to_string());
    args.push(plan.cwd.display().to_string());
    args.push("--entrypoint".to_string());
    args.push(binary.to_string());
    args.push(image.to_string());
    args.extend(argv.iter().cloned());
    Ok(args)
}

fn docker_mount_args(args: &mut Vec<String>, plan: &MountPlan) {
    for entry in plan.ordered_entries() {
        let path = entry.spelling.resolved.display();
        match entry.access {
            MountAccess::ReadOnly => {
                // The container has its own read-only system tree from the
                // image. Mounting host `/usr`, `/etc`, `/lib` or `/bin` over it
                // would replace the image's tools with host binaries and fail
                // for paths the image lacks. Only host data paths are mounted.
                if is_system_mount(&entry.spelling.resolved) {
                    continue;
                }
                args.push("--mount".to_string());
                args.push(format!("type=bind,src={path},dst={path},readonly"));
            }
            MountAccess::ReadWrite => {
                args.push("--mount".to_string());
                args.push(format!("type=bind,src={path},dst={path}"));
            }
            MountAccess::NoAccess if entry.spelling.is_file => {
                args.push("--mount".to_string());
                args.push(format!("type=bind,src=/dev/null,dst={path},readonly"));
            }
            MountAccess::NoAccess => {
                // A read-only empty tmpfs hides the protected directory's
                // entries and refuses writes. The mount point itself remains
                // stat-able; see docs/sandbox.md §5.2.
                args.push("--mount".to_string());
                args.push(format!("type=tmpfs,dst={path},readonly"));
            }
            MountAccess::WriteOnly => {
                // `plan()` refuses this before translation; Docker has no
                // write-only mount.
            }
        }
    }
}

/// The command for a plan, without spawning it. Used by the dry-run and tests.
pub fn planned_command(plan: &SandboxPlan) -> Command {
    let mut command = Command::new(&plan.program);
    command.args(&plan.args);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DockerNetwork, SandboxBackend, SandboxExternal, SandboxMode};

    fn policy() -> PathPolicy {
        PathPolicy {
            read_only: vec![PathBuf::from("/etc")],
            read_write: vec![PathBuf::from("/tmp")],
            // `/etc/hosts` exists on both supported platforms and is a file,
            // so the file-vs-directory branch is covered without relying on a
            // workspace path that CI may not have.
            no_access: vec![PathBuf::from("/etc/hosts")],
            write_only: Vec::new(),
        }
    }

    fn runtime(backend: SandboxBackend, modules: Vec<&str>) -> SandboxRuntime {
        SandboxRuntime::new(
            SandboxConfig {
                backend,
                default: SandboxMode::Required,
                modules: modules.into_iter().map(ToString::to_string).collect(),
                external: SandboxExternal::None,
                backends: crate::config::SandboxBackendsConfig {
                    docker: DockerSandboxConfig {
                        image: Some("example/tools:latest".to_string()),
                        network: DockerNetwork::None,
                        ..DockerSandboxConfig::default()
                    },
                },
            },
            policy(),
            PathBuf::from("/tmp"),
        )
    }

    #[test]
    fn test_path_spelling_emits_original_and_resolved_for_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let spelling = normalize_path(&link, temp.path());
        assert_ne!(spelling.original, spelling.resolved);
        assert_eq!(spelling.resolved, target.canonicalize().unwrap());
    }

    #[test]
    fn test_no_access_denies_content_and_metadata_in_profile() {
        let plan = MountPlan::from_policy(&policy(), Path::new("/tmp"));
        let profile = sandbox_exec_profile(&plan);
        assert!(
            profile.contains("(deny file-read* file-write*"),
            "{profile}"
        );
        assert!(profile.contains("/etc/hosts"), "{profile}");
    }

    #[test]
    fn test_write_only_capability_is_backend_specific() {
        assert!(!ResolvedBackend::Bwrap.supports_write_only());
        assert!(!ResolvedBackend::SystemdRun.supports_write_only());
        assert!(!ResolvedBackend::Docker.supports_write_only());
        assert!(ResolvedBackend::SandboxExec.supports_write_only());
    }

    #[test]
    fn test_unselected_module_has_no_plan() {
        let runtime = runtime(SandboxBackend::SandboxExec, vec!["cli.sed"]);
        assert!(runtime.plan("cli.ls", "/bin/ls", &[]).unwrap().is_none());
    }

    #[test]
    fn test_docker_without_image_is_refused() {
        let mut runtime = runtime(SandboxBackend::Docker, vec!["cli.sed"]);
        runtime.config.backends.docker.image = None;
        let error = runtime
            .plan("cli.sed", "/bin/true", &[])
            .expect_err("docker needs an image");
        assert!(matches!(error, SandboxError::Unavailable { .. }));
    }

    #[test]
    fn test_systemd_args_use_path_properties_and_user_pipe() {
        let plan = MountPlan::from_policy(&policy(), Path::new("/tmp"));
        let args = systemd_run_args(&plan, "/bin/true", &["--flag".to_string()], false);
        assert!(args.contains(&"--user".to_string()));
        assert!(args.contains(&"--pipe".to_string()));
        assert!(args.contains(&"--wait".to_string()));
        assert!(args
            .iter()
            .any(|arg| arg.contains("ReadOnlyPaths=") && arg.ends_with("/etc")));
        assert!(args
            .iter()
            .any(|arg| arg.contains("ReadWritePaths=") && arg.ends_with("/tmp")));
        assert!(args
            .iter()
            .any(|arg| arg.contains("InaccessiblePaths=") && arg.ends_with("/etc/hosts")));
        assert!(!args.iter().any(|arg| arg.starts_with("PrivateTmp=")));
        assert_eq!(args.last().map(String::as_str), Some("--flag"));
        let system = systemd_run_args(&plan, "/bin/true", &[], true);
        assert!(!system.contains(&"--user".to_string()));
    }

    #[test]
    fn test_bwrap_starts_rw_then_applies_stricter_mounts() {
        let plan = MountPlan::from_policy(&policy(), Path::new("/tmp"));
        let args = bwrap_args(&plan, "/bin/true", &[]);
        assert!(args.windows(3).any(|w| w == ["--bind", "/", "/"]));
        assert!(args.contains(&"--unshare-all".to_string()));
        assert!(args.contains(&"--die-with-parent".to_string()));
        assert!(args.contains(&"--cap-drop".to_string()));
        assert!(args
            .windows(3)
            .any(|w| w[0] == "--ro-bind" && w[1].ends_with("/etc")));
        assert!(args
            .windows(3)
            .any(|w| w[0] == "--bind" && w[1].ends_with("/tmp")));
        assert!(args
            .windows(3)
            .any(|w| w[0] == "--ro-bind" && w[1] == "/dev/null" && w[2].ends_with("/etc/hosts")));
    }

    #[test]
    fn test_deny_within_grant_is_emitted_after_the_allow() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let log = root.join("log");
        let secret = log.join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        let policy = PathPolicy {
            read_only: vec![root.clone()],
            read_write: vec![log.clone()],
            no_access: vec![secret.clone()],
            write_only: Vec::new(),
        };
        let profile = sandbox_exec_profile(&MountPlan::from_policy(&policy, temp.path()));
        let allow = format!(
            "(allow file-read* file-write* (subpath \"{}\"))",
            log.display()
        );
        let deny = format!(
            "(deny file-read* file-write* (subpath \"{}\"))",
            secret.display()
        );
        let allow_index = profile.find(&allow).expect("read_write allow");
        let deny_index = profile.rfind(&deny).expect("no_access deny");
        assert!(
            deny_index > allow_index,
            "no_access must be emitted after the allow: {profile}"
        );
    }

    #[test]
    fn test_deeper_read_only_wins_over_broader_read_write() {
        let temp = tempfile::tempdir().unwrap();
        let broad = temp.path().join("broad");
        let narrow = broad.join("narrow");
        std::fs::create_dir_all(&narrow).unwrap();
        let policy = PathPolicy {
            read_only: vec![narrow],
            read_write: vec![broad],
            ..PathPolicy::default()
        };
        let profile = sandbox_exec_profile(&MountPlan::from_policy(&policy, temp.path()));
        let allow = profile
            .find("(allow file-read* file-write* (subpath")
            .expect("read_write allow");
        let deny = profile
            .rfind("(deny file-write* (subpath")
            .expect("read_only deny");
        assert!(deny > allow, "deeper read_only must win: {profile}");
    }

    #[test]
    fn test_missing_paths_are_dropped_before_translation() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("does-not-exist");
        let policy = PathPolicy {
            read_only: vec![missing],
            ..PathPolicy::default()
        };
        let plan = MountPlan::from_policy(&policy, temp.path());
        assert!(plan.read_only.is_empty());
    }

    #[test]
    fn test_check_refuses_an_inexpressible_write_only_policy() {
        let temp = tempfile::tempdir().unwrap();
        let writable = temp.path().join("writable");
        std::fs::create_dir(&writable).unwrap();
        let plan = MountPlan::from_policy(
            &PathPolicy {
                write_only: vec![writable],
                ..PathPolicy::default()
            },
            temp.path(),
        );
        assert!(capability_refusal(ResolvedBackend::Bwrap, &plan).is_some());
        assert!(capability_refusal(ResolvedBackend::SandboxExec, &plan).is_none());
    }

    #[test]
    fn test_sandbox_exec_digest_is_deterministic() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = SandboxRuntime::new(
            SandboxConfig {
                backend: SandboxBackend::SandboxExec,
                default: SandboxMode::Required,
                modules: vec!["cli.sh".to_string()],
                ..SandboxConfig::default()
            },
            PathPolicy::default(),
            temp.path().to_path_buf(),
        );
        let first = runtime
            .plan("cli.sh", "/bin/sh", &["-c".to_string(), "true".to_string()])
            .unwrap()
            .unwrap();
        let second = runtime
            .plan("cli.sh", "/bin/sh", &["-c".to_string(), "true".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(first.policy_digest, second.policy_digest);
    }

    #[test]
    fn test_docker_args_mask_no_access_and_honor_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret");
        std::fs::create_dir(&secret).unwrap();
        let policy = PathPolicy {
            no_access: vec![secret.clone()],
            ..PathPolicy::default()
        };
        let plan = MountPlan::from_policy(&policy, temp.path());
        let args = docker_args(
            &DockerSandboxConfig {
                image: Some("example/tools:latest".to_string()),
                read_only: false,
                ..DockerSandboxConfig::default()
            },
            &plan,
            "/bin/true",
            &[],
        )
        .unwrap();
        assert!(!args.contains(&"--read-only".to_string()));
        assert!(args.windows(2).any(|window| {
            window[0] == "--mount"
                && window[1].contains("type=tmpfs")
                && window[1].contains(secret.to_str().unwrap())
        }));
    }

    #[test]
    fn test_docker_args_are_deterministic() {
        let plan = MountPlan::from_policy(&policy(), Path::new("/tmp"));
        let config = DockerSandboxConfig {
            image: Some("example/tools:latest".to_string()),
            ..DockerSandboxConfig::default()
        };
        let first = docker_args(&config, &plan, "/bin/true", &[]).unwrap();
        let second = docker_args(&config, &plan, "/bin/true", &[]).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_docker_args_are_isolated() {
        let plan = MountPlan::from_policy(&policy(), Path::new("/tmp"));
        let args = docker_args(
            &DockerSandboxConfig {
                image: Some("example/tools:latest".to_string()),
                ..DockerSandboxConfig::default()
            },
            &plan,
            "/bin/true",
            &[],
        )
        .unwrap();
        assert!(args.contains(&"--network".to_string()));
        assert!(args.contains(&"none".to_string()));
        assert!(args.contains(&"--read-only".to_string()));
        assert!(args.contains(&"--cap-drop".to_string()));
        assert!(args.contains(&"--security-opt".to_string()));
    }
}
