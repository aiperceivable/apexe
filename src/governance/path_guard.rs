//! Compiled-in guard for filesystem locations a wrapped tool may not be
//! pointed at.
//!
//! # Why this does not live in an overlay
//!
//! An overlay describes what a command *accepts* — its flags, its operands,
//! their types — and every assertion in one is verifiable against a real
//! installation's man page (see [`docs/overlays.md`]). "`/etc` must not be
//! deleted" is neither: it is local policy, it has no provenance, and it would
//! have to be restated in every overlay for every command that writes. Worse,
//! `mode: authoritative` lets any overlay file replace the scan result
//! wholesale, so a boundary expressed there is a boundary a data file can
//! erase. The scanner also admits commands no overlay covers, which would
//! leave the majority of the registry unguarded — the unsafe failure
//! direction.
//!
//! So the baseline lives here, in code, and cannot be removed. Operators may
//! *add* to it through `config.yaml`; nothing subtracts from it.
//!
//! # Two lists, because reading and writing are different risks
//!
//! A single list forced one answer onto two unrelated questions and got both
//! wrong. `cat /etc/hosts` was refused, which buys nothing — the file is
//! world-readable and an agent's own file-reading tool reaches it anyway
//! (`docs/threat-model.md` §5.7) — while `cat ~/.ssh/id_rsa` and `rm -rf /etc`
//! were refused for the same stated reason despite being nothing alike.
//!
//! The split follows the risk rather than the directory:
//!
//! | | [`BASELINE_SYSTEM_PATHS`] | [`BASELINE_CREDENTIAL_HOME_SUBPATHS`] |
//! |---|---|---|
//! | [`AccessMode::ReadOnly`] | allowed — legible, and reachable regardless | **refused** — exfiltration is the primary risk |
//! | [`AccessMode::Write`] | **refused** — destruction is the primary risk | **refused** |
//!
//! Deleting a private key is noticed within the hour. Reading one into a model
//! context is not noticed at all, which is why the credential list is the
//! stricter of the two.
//!
//! # Why resolution happens before comparison
//!
//! The guard has to compare the path the *kernel* will act on, not the string
//! the caller sent. Three transformations stand between them, and skipping any
//! one turns the guard into decoration:
//!
//! 1. **Relative paths.** A bare `../../etc/passwd` means nothing until it is
//!    joined to the working directory the child will actually run in. That
//!    directory is [`PathGuard::root`], and [`crate::module::executor`] passes
//!    the same value to `Command::current_dir`, so the guard and the child can
//!    never disagree about where a relative path lands.
//! 2. **Symlinks.** `/tmp/x -> /etc` makes `rm -rf /tmp/x/` a request to
//!    delete `/etc`, and no amount of string comparison on `/tmp/x` sees it.
//! 3. **`..` components.** `std::path::absolute` deliberately preserves them
//!    (POSIX requires it), so joining alone leaves `../../etc` intact.
//!
//! [`resolve`] does 1 then 2 then 3, in that order. The order is not
//! interchangeable: `..` may only be collapsed lexically *after* the symlinks
//! ahead of it are resolved, because `/tmp/link/..` is the parent of link's
//! target, not `/tmp`.
//!
//! [`docs/overlays.md`]: https://github.com/tercel/apexe/blob/main/docs/overlays.md

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use apcore::{ErrorCode, ModuleError};

/// System locations a *writing* command may not be pointed at.
///
/// These are the directories whose contents belong to the operating system or
/// its package manager rather than to any workspace. A read-only command is
/// not held to this list — see the module documentation for why refusing
/// `cat /etc/hosts` protected nothing.
///
/// **`/` is deliberately not an entry.** Every absolute path starts with it,
/// so listing it would make [`PathGuard::denial_reason`]'s containment test
/// match unconditionally and refuse the entire filesystem. `rm /` is still
/// refused — by the *ancestor* half of that test, since `/` contains `/etc` —
/// which is the only direction that carries the intended meaning.
///
/// `/opt` is deliberately absent: it holds optional software (Homebrew lives
/// at `/opt/homebrew`) rather than the base system, and guarding it would
/// refuse ordinary package work. `/Volumes` is absent for the same reason in
/// the other direction — the mount point is system-owned but everything under
/// it is user data, and `starts_with` cannot separate the two.
const BASELINE_SYSTEM_PATHS: &[&str] = &[
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
    // macOS system roots. `/private/etc` and `/private/var` are where
    // `canonicalize` lands `/etc` and `/var`; they are listed as well so the
    // guard still holds if canonicalization fails.
    "/System",
    "/Library",
    "/Applications",
    "/private/etc",
    "/private/var",
];

/// Paths under the invoking user's home that no command may read *or* write,
/// whatever its annotations say.
///
/// Reading is the reason this list is separate. A private key copied into a
/// model context is a compromise that leaves no trace, whereas a deleted one
/// announces itself the next time the key is used — so the stricter treatment
/// goes to the risk that is harder to notice, not the one that sounds worse.
///
/// `~/.apexe` is here rather than in [`BASELINE_SYSTEM_PATHS`] for the same
/// reason: it holds the ACL and the audit trail that govern the very call
/// being made, and a wrapped tool that can read the policy can plan around it.
///
/// # Entries are credential *stores*, not directories that happen to hold one
///
/// Two of these are files and two sit under `.config`, which the first version
/// of this list could not express — it protected `~/.ssh` and `~/.aws` while
/// `gh`, `git` and `curl` kept their tokens somewhere else entirely. Each
/// addition names a store whose whole content is authentication material:
///
/// * `.config/gh` — the GitHub CLI's `hosts.yml`, which holds the OAuth token.
/// * `.config/gcloud` — the same role `~/.aws` and `~/.kube` already have here.
/// * `.git-credentials` — what git's `store` credential helper writes, in
///   plaintext, by default.
/// * `.netrc` — read by git and curl for HTTP auth, and by much else besides.
///
/// `.config` itself stays deliberately absent, and the two entries under it are
/// why the distinction matters. It otherwise holds ordinary application
/// settings — editor configuration, shell prompts — that a wrapped tool has
/// legitimate reason to read and write, and refusing the whole tree would cost
/// far more than it protects.
///
/// Mixed config-and-credential files (`.npmrc`, `.pypirc`) are **not** here for
/// that same reason: refusing them would block the settings a tool legitimately
/// needs in order to protect the token sharing the file. An operator who wants
/// them covered adds them through [`GuardConfig::denied`], which joins this
/// list at runtime.
const BASELINE_CREDENTIAL_HOME_SUBPATHS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".apexe",
    ".config/gh",
    ".config/gcloud",
    ".git-credentials",
    ".netrc",
];

/// Whether the call being rendered can modify what its paths name.
///
/// Taken from the module's `readonly` annotation, which the scanner infers and
/// an overlay can correct. [`AccessMode::Write`] is the conservative reading
/// and the one every unannotated module gets: a command nobody has classified
/// is treated as able to destroy, which is the safe direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMode {
    /// The command only reads. System directories stay legible; credential
    /// directories do not.
    ReadOnly,
    /// The command can modify what it names. Everything protected is refused.
    Write,
}

impl AccessMode {
    /// Read the mode off a module's `readonly` annotation.
    pub fn from_readonly(readonly: bool) -> Self {
        if readonly {
            Self::ReadOnly
        } else {
            Self::Write
        }
    }
}

/// What an operator's `config.yaml` contributes to the guard.
///
/// A struct rather than two slice parameters on purpose: `denied` and `allowed`
/// have the same type and opposite meanings, and a call that transposed them
/// would turn a deny list into an allow list without a compiler error.
#[derive(Debug, Clone, Copy, Default)]
pub struct GuardConfig<'a> {
    /// Appended to the credential list — refused to readers and writers alike.
    pub denied: &'a [PathBuf],
    /// Carve-outs *out of* the compiled-in baselines. Empty by default; see
    /// [`PathGuard::new`] for what an operator takes on by using it.
    pub allowed: &'a [PathBuf],
}

/// Locations that stay usable even though a denied entry contains them.
///
/// Exactly one case motivates this, and it is not a policy softening: on macOS
/// the per-user temporary directory *is* `/var/folders/<x>/<y>/T`, so guarding
/// `/var` — which holds real system state on both platforms — would also refuse
/// every use of the directory whose entire purpose is being written to. That
/// placement is a macOS implementation detail, not a statement that temporary
/// files are system state.
///
/// This is a compiled-in list of one, and `config.yaml` cannot extend it. The
/// configuration surface stays additive in the safe direction only: an operator
/// can lengthen the denied list and can never punch a hole in it.
///
/// [`accept_exemption`] additionally refuses any candidate that is, or
/// contains, a *system* location — so a `TMPDIR` pointing at `/etc` is
/// discarded rather than honoured.
fn baseline_exemptions() -> Vec<PathBuf> {
    vec![std::env::temp_dir()]
}

/// Whether `candidate` may be exempted without opening a system location.
///
/// An exemption is only ever a carve-out *inside* a guarded directory. A
/// candidate that equals a system entry, or sits above one, would expose it
/// instead — `entry.starts_with(candidate)` is true in both of those cases, and
/// false for the legitimate `/var/folders/…` under `/var`.
///
/// **Only [`BASELINE_SYSTEM_PATHS`] is consulted**, and the two lists that are
/// not are each excluded for a reason.
///
/// Weighing the *credential* list here is not merely unnecessary, it is
/// actively wrong: `$TMPDIR` legitimately contains a home directory in a
/// sandbox or CI container, which makes every credential path a descendant of
/// the carve-out, which discards the carve-out, which re-arms `/var` over the
/// whole temp directory. The result is a total refusal of temporary files that
/// looks like a guard bug and is very hard to trace back to `HOME`. Nothing is
/// lost by omitting it, because [`PathGuard::denial_reason`]'s specificity rule
/// already protects a credential path nested inside a carve-out — the deeper
/// entry wins, and an exact overlap is a tie, which refuses.
///
/// The operator's own additions are excluded for the same reason in a milder
/// form: one configured path under `$TMPDIR` would void the carve-out, and
/// specificity enforces them correctly without that side effect.
fn accept_exemption(candidate: &Path, system: &[PathBuf]) -> bool {
    !system.iter().any(|entry| entry.starts_with(candidate))
}

/// The most specific entry in `candidates` that contains `target`, if any.
///
/// "Most specific" is the deepest, so a carve-out nested inside a denied
/// directory outranks it and a configured path nested inside the carve-out
/// outranks that in turn.
fn deepest_containing<'a>(
    target: &Path,
    candidates: impl Iterator<Item = &'a PathBuf>,
) -> Option<&'a PathBuf> {
    candidates
        .filter(|entry| target.starts_with(entry))
        .max_by_key(|entry| specificity(entry))
}

/// How specific a rule is: the number of components it pins down.
///
/// `/var` scores 2 (root plus one name) and `/var/folders/x/y/T` scores 6, so
/// the carve-out outranks the baseline entry that contains it.
fn specificity(path: &Path) -> usize {
    path.components().count()
}

/// The guard installed for this process.
///
/// A `OnceLock` rather than a parameter threaded through [`build_argv`] on
/// purpose: a guard that can be passed is a guard that can be omitted, and
/// every call site that omitted it would execute unprotected. Reading it from
/// here means the protection is on by default and there is no signature to
/// opt out of. [`AccessMode`] *is* a parameter, because it varies per call in
/// a way no process-wide value could express.
///
/// [`build_argv`]: crate::module::executor::build_argv
static ACTIVE: OnceLock<PathGuard> = OnceLock::new();

/// The filesystem locations wrapped tools may not be pointed at, and the
/// working directory their relative paths resolve against.
#[derive(Debug, Clone)]
pub struct PathGuard {
    /// Working directory relative paths are joined to. The executor sets the
    /// child's `current_dir` to this same value.
    root: PathBuf,
    /// Resolved system locations, enforced against [`AccessMode::Write`] only.
    system: Vec<PathBuf>,
    /// Resolved credential locations plus whatever the operator configured,
    /// enforced against both access modes. This is the list [`applicable`]
    /// actually checks against; see [`credential_baseline`] and
    /// [`credential_configured`] for the same entries split by provenance,
    /// kept for introspection (`apexe policy`) rather than enforcement.
    ///
    /// [`applicable`]: PathGuard::applicable
    /// [`credential_baseline`]: PathGuard::credential_baseline
    /// [`credential_configured`]: PathGuard::credential_configured
    credential: Vec<PathBuf>,
    /// The compiled-in half of [`credential`](PathGuard::credential), before
    /// the operator's `additional_denied_paths` are merged in. Exists only so
    /// `apexe policy` can report which entries are built-in versus
    /// configured; enforcement always goes through the merged list.
    credential_baseline: Vec<PathBuf>,
    /// The operator's `additional_denied_paths`, resolved, before merging
    /// into [`credential`](PathGuard::credential). See
    /// [`credential_baseline`](PathGuard::credential_baseline) for why this
    /// is kept separately.
    credential_configured: Vec<PathBuf>,
    /// Resolved carve-outs that survived [`accept_exemption`]. See
    /// [`baseline_exemptions`] — this is the temp directory and nothing else.
    ///
    /// Kept apart from [`PathGuard::allowed`] because the two break ties in
    /// opposite directions, and the reason is whose decision each one is. This
    /// list is *derived* — nobody asked for it, it exists so `$TMPDIR` under
    /// `/var` keeps working — so an exact overlap with a protected path is a
    /// coincidence to be refused, not an instruction.
    exempt: Vec<PathBuf>,
    /// Resolved `allowed_paths` from the operator's configuration.
    ///
    /// An exact overlap with a protected path resolves in *this* list's favour,
    /// because someone wrote the path down on purpose. `allowed_paths: ["/etc"]`
    /// means `/etc`, and a guard that quietly declined would be overruling the
    /// person who deployed it — see [`PathGuard::new`].
    allowed: Vec<PathBuf>,
}

impl PathGuard {
    /// Build a guard from the compiled-in baselines plus an operator's config.
    ///
    /// Every entry is resolved the same way a caller's value will be, so a
    /// configured `/var` and a caller's `/private/var/log` compare equal on
    /// macOS.
    ///
    /// `config.denied` joins the **credential** list, so those paths are
    /// refused to readers as well as writers. An operator naming a path
    /// explicitly is asserting that it is sensitive, and the stronger of the
    /// two readings is the one that cannot be wrong in the dangerous direction.
    ///
    /// `config.allowed` is the one input that *relaxes* the guard, and it is
    /// **empty unless an operator writes it**. It exists because the real
    /// alternative was worse: an agent that legitimately needs to write
    /// `/etc/nginx/conf.d` had no option but to be handed the tool outside
    /// apexe entirely, which removes the audit trail and the ACL along with the
    /// path check. Nothing here validates that a carve-out is *wise* — an
    /// operator can name `/etc`, or a credential directory, and the guard will
    /// honour it. What it does instead is refuse to be quiet about it:
    /// [`warn_about_risky_carve_outs`] logs every entry that opens a baseline
    /// location, so the decision appears in the startup log rather than only in
    /// a config file nobody re-reads.
    pub fn new(root: PathBuf, config: GuardConfig<'_>) -> Self {
        let system: Vec<PathBuf> = BASELINE_SYSTEM_PATHS
            .iter()
            .flat_map(|entry| baseline_spellings(Path::new(entry), &root))
            .collect();

        let mut credential: Vec<PathBuf> = Vec::new();
        if let Some(home) = dirs::home_dir() {
            for entry in BASELINE_CREDENTIAL_HOME_SUBPATHS {
                credential.extend(baseline_spellings(&home.join(entry), &root));
            }
        } else {
            tracing::warn!(
                "No home directory: the credential directories in the path-guard \
                 baseline (~/.ssh, ~/.aws, …) are not protected in this process"
            );
        }

        let exempt: Vec<PathBuf> = baseline_exemptions()
            .iter()
            .map(|candidate| resolve(candidate, &root))
            .filter(|candidate| {
                let accepted = accept_exemption(candidate, &system);
                if !accepted {
                    tracing::warn!(
                        candidate = %candidate.display(),
                        "Discarding a temp-directory exemption that would expose a \
                         system location; check TMPDIR"
                    );
                }
                accepted
            })
            .collect();

        let credential_baseline = credential.clone();
        let credential_configured: Vec<PathBuf> = config
            .denied
            .iter()
            .flat_map(|entry| baseline_spellings(entry, &root))
            .collect();
        credential.extend(credential_configured.iter().cloned());
        credential.sort();
        credential.dedup();

        // Carve-outs deliberately do NOT get the two-spelling treatment the
        // denied lists get. A second spelling can only ever match more paths,
        // which is the right direction for a refusal and the wrong one for an
        // exemption: a carve-out that fails to apply costs a refused call an
        // operator can widen, while one that applies too broadly opens a
        // location nobody named.
        let configured: Vec<PathBuf> = config
            .allowed
            .iter()
            .map(|entry| resolve(entry, &root))
            .collect();
        warn_about_risky_carve_outs(&configured, &system, &credential);

        let mut allowed = configured;
        allowed.sort();
        allowed.dedup();

        Self {
            root,
            system,
            credential,
            credential_baseline,
            credential_configured,
            exempt,
            allowed,
        }
    }

    /// Build a guard rooted at the process's working directory.
    ///
    /// A working directory that cannot be read falls back to `/`, which every
    /// baseline entry sits under — so a relative path resolves somewhere the
    /// guard will judge rather than somewhere it silently trusts. That is the
    /// safe direction for a failure this unusual (the cwd was deleted out from
    /// under the process).
    pub fn from_env(config: GuardConfig<'_>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|error| {
            tracing::warn!(
                %error,
                "Cannot read the working directory; relative paths will resolve \
                 against /"
            );
            PathBuf::from("/")
        });
        Self::new(root, config)
    }

    /// The working directory relative paths resolve against.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Compiled-in system locations, refused to [`AccessMode::Write`] only.
    /// For introspection (`apexe policy`); enforcement reads [`applicable`](Self::applicable).
    pub fn system_baseline(&self) -> &[PathBuf] {
        &self.system
    }

    /// Compiled-in credential locations, refused to both access modes. Does
    /// **not** include the operator's `additional_denied_paths` — see
    /// [`credential_configured`](Self::credential_configured) for those.
    pub fn credential_baseline(&self) -> &[PathBuf] {
        &self.credential_baseline
    }

    /// The operator's `additional_denied_paths`, resolved. Refused to both
    /// access modes, same as the compiled-in credential list it is merged
    /// into for enforcement.
    pub fn credential_configured(&self) -> &[PathBuf] {
        &self.credential_configured
    }

    /// The operator's `allowed_paths` carve-outs, resolved. The only setting
    /// that relaxes the guard; see [`PathGuard::new`].
    pub fn allowed_paths(&self) -> &[PathBuf] {
        &self.allowed
    }

    /// Derived exemptions that are not an operator's decision — the
    /// temp-directory carve-out described at [`baseline_exemptions`].
    pub fn exempt_paths(&self) -> &[PathBuf] {
        &self.exempt
    }

    /// Refuse `value` if it resolves into a location `mode` may not touch.
    ///
    /// `subject` names the offending input the way the caller sees it, e.g.
    /// `"Element 0 of parameter 'file'"`.
    #[allow(clippy::result_large_err)] // ModuleError is 184 bytes; matches the crate boundary
    pub fn check(&self, subject: &str, value: &str, mode: AccessMode) -> Result<(), ModuleError> {
        let resolved = resolve(Path::new(value), &self.root);
        let Some(denied) = self.denial_reason(&resolved, mode) else {
            return Ok(());
        };
        tracing::warn!(
            subject = %subject,
            requested = %value,
            resolved = %resolved.display(),
            denied = %denied.display(),
            mode = ?mode,
            "Path guard refused a protected location"
        );
        Err(protected_path_error(subject, value, &resolved, denied))
    }

    /// The denied location `target` collides with under `mode`, if any.
    ///
    /// Two directions, and only one of them applies to a reader.
    ///
    /// **Containment** — `target` sits inside a protected location — applies to
    /// both modes. It is what refuses `cat ~/.ssh/id_rsa`.
    ///
    /// **Ancestry** — `target` sits *above* a protected location — applies to
    /// [`AccessMode::Write`] only, because it exists for the recursive
    /// operation that takes the protected directory with it: `rm -rf /` never
    /// names `/etc` and destroys it regardless. Holding a reader to it would
    /// refuse `ls ~` and `ls /` on the grounds that a home directory contains
    /// `.ssh`, which is most of what a reader legitimately does. The cost is
    /// stated in `docs/threat-model.md` §5.8: a recursive *read* rooted above a
    /// credential directory, `grep -r … /` being the sharp case, is not caught.
    ///
    /// [`Path::starts_with`] compares whole components, so `/etcetera` does not
    /// match `/etc`. A `str::starts_with` here would be a bug.
    ///
    /// Where a denied entry and a carve-out both contain `target`, the more
    /// specific one wins: `$TMPDIR` under `/var` is writable, a path the
    /// operator denied *inside* `$TMPDIR` is refused again, and each rule binds
    /// only the subtree it names.
    ///
    /// An exact tie is broken by *whose* carve-out it is. A configured
    /// `allowed_paths` entry wins, because `allowed_paths: ["/etc"]` is an
    /// instruction and honouring it is the whole point. The derived temp-
    /// directory exemption loses, because nobody asked for it and an exact
    /// overlap there is a coincidence — which is what stops a `TMPDIR` pointed
    /// at `~/.ssh` from reading a key out.
    fn denial_reason(&self, target: &Path, mode: AccessMode) -> Option<&Path> {
        if mode == AccessMode::Write {
            // A configured carve-out covering `target` also covers everything
            // under it, so the protected descendants it contains are the ones
            // the operator just said to permit. Without this, opening
            // `/etc/nginx/conf.d` would still refuse `rm -r` on that very
            // directory whenever a denial sat inside it.
            let carved_out = deepest_containing(target, self.allowed.iter()).is_some();
            if !carved_out {
                if let Some(descendant) = self
                    .applicable(mode)
                    .find(|denied| denied.starts_with(target))
                {
                    return Some(descendant.as_path());
                }
            }
        }

        let denied = deepest_containing(target, self.applicable(mode))?;
        let depth = specificity(denied);

        let configured_wins = deepest_containing(target, self.allowed.iter())
            .is_some_and(|allowed| specificity(allowed) >= depth);
        let derived_wins = deepest_containing(target, self.exempt.iter())
            .is_some_and(|exempt| specificity(exempt) > depth);

        if configured_wins || derived_wins {
            return None;
        }
        Some(denied.as_path())
    }

    /// The denied entries `mode` is held to.
    ///
    /// Credentials are in both modes; system locations only bind a writer.
    fn applicable(&self, mode: AccessMode) -> impl Iterator<Item = &PathBuf> {
        let system = match mode {
            AccessMode::Write => Some(self.system.iter()),
            AccessMode::ReadOnly => None,
        };
        self.credential.iter().chain(system.into_iter().flatten())
    }
}

/// Install the guard for this process. Later calls are ignored.
///
/// Returns whether this call is the one that installed it, so a caller that
/// cares can log the difference. Ignoring a second call rather than replacing
/// the first keeps the guard from being swapped out mid-flight.
pub fn install(guard: PathGuard) -> bool {
    let installed = ACTIVE.set(guard).is_ok();
    if !installed {
        tracing::warn!("Path guard already installed; ignoring the later configuration");
    }
    installed
}

/// The active guard, defaulting to the baseline rooted at the process cwd.
///
/// The default is what makes the protection unconditional: a binary that never
/// calls [`install`] — a test, an embedding, a code path added later — still
/// gets the compiled-in baseline rather than no guard at all.
pub fn active() -> &'static PathGuard {
    ACTIVE.get_or_init(|| PathGuard::from_env(GuardConfig::default()))
}

/// Log the configured carve-outs that open a compiled-in protection.
///
/// Deliberately a warning and not a refusal. The operator asked for this, and
/// second-guessing an explicit `allowed_paths` entry would put the guard in the
/// business of overruling the person who deployed it. What it must not do is
/// let the decision stay invisible: a carve-out over `/etc` or over a
/// credential directory is a material change to what the process will permit,
/// and it belongs in the startup log where an incident review will find it.
///
/// The two cases are separated because they are not equally alarming. Opening a
/// system path is the intended use — `/etc/nginx/conf.d` is exactly what this
/// feature is for. Opening a credential path is almost certainly a mistake, and
/// says so.
fn warn_about_risky_carve_outs(configured: &[PathBuf], system: &[PathBuf], credential: &[PathBuf]) {
    for entry in configured {
        if let Some(opened) = credential.iter().find(|c| c.starts_with(entry)) {
            tracing::warn!(
                allowed = %entry.display(),
                opens = %opened.display(),
                "Configured allowed_paths entry exposes a credential directory. \
                 Reading a private key leaves no trace; confirm this is intended."
            );
        } else if let Some(opened) = system.iter().find(|s| s.starts_with(entry)) {
            tracing::warn!(
                allowed = %entry.display(),
                opens = %opened.display(),
                "Configured allowed_paths entry opens an entire system location, \
                 not a subtree of one. Narrow it if a subdirectory would do."
            );
        } else {
            tracing::info!(
                allowed = %entry.display(),
                "Path guard carve-out configured"
            );
        }
    }
}

/// Both spellings of a baseline entry: the literal path and where it
/// canonicalizes to.
///
/// Passing the baseline through [`resolve`] alone was a hole on any platform
/// where a guarded location is a symlink, and macOS is one: `/etc` and `/var`
/// are links into `/private`, so every baseline entry canonicalized to
/// `/private/…` and *nothing in the list was spelled `/etc` any more*. The
/// comparison is then asymmetric, because a target does not always get
/// canonicalized the same way — [`resolve_existing_prefix`] only canonicalizes
/// the part of a path that exists and folds the rest lexically, so a `/etc`
/// component reached across a non-existent tail survives uncanonicalized.
///
/// `/usr/local/share/../../../etc/passwd` on a host without `/usr/local/share`
/// is exactly that shape: it resolves to the literal `/etc/passwd`, which
/// `starts_with("/private/etc")` does not match, and a writer aimed at
/// `/etc/passwd` was allowed through. The direct spelling was caught, because
/// that path exists and canonicalizes — which is what made the gap easy to
/// miss.
///
/// Keeping both spellings closes it in the only direction that matters. On a
/// platform where the entry is not a symlink the two are equal and dedupe to
/// one, so this costs nothing there; where they differ, the extra entry can
/// only ever refuse more.
fn baseline_spellings(entry: &Path, root: &Path) -> Vec<PathBuf> {
    let literal = if entry.is_absolute() {
        entry.to_path_buf()
    } else {
        root.join(entry)
    };
    let canonical = resolve(entry, root);
    if canonical == literal {
        vec![literal]
    } else {
        vec![literal, canonical]
    }
}

/// Resolve `path` to the absolute location the kernel would act on.
///
/// Three steps, in an order that is not interchangeable — see the module
/// documentation for why.
fn resolve(path: &Path, root: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    resolve_existing_prefix(&joined)
}

/// Canonicalize the longest prefix of `path` that exists, then fold the rest in
/// lexically.
///
/// Splitting the path this way is what lets the guard handle a target that does
/// not exist yet — `mkdir -p`, a `cp` destination — without giving up symlink
/// resolution on the part that does. Collapsing `..` lexically is safe only
/// across the non-existent tail, where no component can be a symlink.
///
/// The loop runs at most once per component and terminates: `/` canonicalizes
/// on any working system, and the empty-prefix case falls through to the
/// lexical answer.
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let components: Vec<Component<'_>> = path.components().collect();
    for split in (1..=components.len()).rev() {
        let prefix: PathBuf = components[..split].iter().collect();
        if let Ok(real) = prefix.canonicalize() {
            return fold_lexically(real, &components[split..]);
        }
    }
    fold_lexically(PathBuf::from("/"), &components)
}

/// Apply `rest` to `base` without touching the filesystem.
fn fold_lexically(base: PathBuf, rest: &[Component<'_>]) -> PathBuf {
    let mut out = base;
    for component in rest {
        match component {
            Component::Normal(name) => out.push(name),
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::RootDir => out = PathBuf::from("/"),
            Component::Prefix(prefix) => out = PathBuf::from(prefix.as_os_str()),
        }
    }
    out
}

/// Build the refusal, naming both what was asked for and where it landed.
///
/// The resolved path is in the message because the requested one frequently
/// does not look protected: the whole point of the symlink and `..` handling is
/// that `/tmp/x` and `/etc` can be the same directory, and a caller told only
/// "denied" cannot tell a mistake from a misconfiguration.
///
/// [`ErrorCode::ACLDenied`] rather than `GeneralInvalidInput`: this is a
/// governance decision, not a malformed value, and the classification matters
/// downstream — [`crate::module::breaker`] excludes governance refusals from
/// circuit-breaker health, so a caller repeatedly probing `/etc` cannot trip
/// the breaker and deny `cli.rm` to everyone else.
fn protected_path_error(
    subject: &str,
    requested: &str,
    resolved: &Path,
    denied: &Path,
) -> ModuleError {
    let mut details: HashMap<String, serde_json::Value> = HashMap::new();
    details.insert("requested_path".to_string(), serde_json::json!(requested));
    details.insert(
        "resolved_path".to_string(),
        serde_json::json!(resolved.display().to_string()),
    );
    details.insert(
        "protected_path".to_string(),
        serde_json::json!(denied.display().to_string()),
    );
    ModuleError::new(
        ErrorCode::ACLDenied,
        format!(
            "{subject} resolves to '{}', which is protected by '{}'",
            resolved.display(),
            denied.display()
        ),
    )
    .with_details(details)
    .with_ai_guidance(format!(
        "'{requested}' resolves to '{}', inside or above the protected location \
         '{}'. This is a compiled-in boundary that no configuration removes. \
         Choose a path outside the protected system and credential directories.",
        resolved.display(),
        denied.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use AccessMode::{ReadOnly, Write};

    /// A guard rooted at `root` with the baselines and nothing configured.
    pub(super) fn guard_at(root: &Path) -> PathGuard {
        PathGuard::new(root.to_path_buf(), GuardConfig::default())
    }

    /// A guard with only `denied` configured.
    fn guard_denying(root: &Path, denied: &[PathBuf]) -> PathGuard {
        PathGuard::new(
            root.to_path_buf(),
            GuardConfig {
                denied,
                ..Default::default()
            },
        )
    }

    /// A guard with only `allowed` configured.
    fn guard_allowing(root: &Path, allowed: &[PathBuf]) -> PathGuard {
        PathGuard::new(
            root.to_path_buf(),
            GuardConfig {
                allowed,
                ..Default::default()
            },
        )
    }

    /// The home-relative baseline entry, as a caller would spell it.
    fn home_path(sub: &str) -> String {
        dirs::home_dir()
            .expect("home directory")
            .join(sub)
            .to_string_lossy()
            .into_owned()
    }

    // ---- The read/write split ------------------------------------------

    #[test]
    fn test_a_reader_may_name_a_system_directory() {
        // `cat /etc/hosts` and `ls /usr/bin` are ordinary work. Refusing them
        // protected nothing: the files are world-readable and the agent's own
        // file tools reach them regardless.
        let guard = guard_at(Path::new("/"));

        assert!(guard
            .check("Parameter 'file'", "/etc/hosts", ReadOnly)
            .is_ok());
        assert!(guard
            .check("Parameter 'file'", "/usr/bin", ReadOnly)
            .is_ok());
        assert!(guard.check("Parameter 'file'", "/System", ReadOnly).is_ok());
    }

    #[test]
    fn test_a_writer_may_not_name_a_system_directory() {
        let guard = guard_at(Path::new("/"));

        for probe in ["/etc/hosts", "/usr/bin/env", "/System/Library"] {
            let error = guard.check("Parameter 'file'", probe, Write).unwrap_err();
            assert_eq!(error.code, ErrorCode::ACLDenied, "{probe} must be refused");
        }
    }

    #[test]
    fn test_a_reader_may_not_name_a_credential_directory() {
        // The exfiltration case, and the reason the two lists exist. A deleted
        // private key announces itself; one copied into a model context does
        // not.
        let guard = guard_at(Path::new("/"));

        for sub in BASELINE_CREDENTIAL_HOME_SUBPATHS {
            let probe = home_path(sub);
            let error = guard
                .check("Parameter 'file'", &probe, ReadOnly)
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::ACLDenied, "{probe} must be refused");
        }
    }

    /// `.config` holds ordinary settings and stays readable; the two credential
    /// stores inside it do not. The distinction is the whole reason those
    /// entries carry a path segment, and "simplifying" either one to `.config`
    /// would refuse every editor and shell config a wrapped tool legitimately
    /// reads — the kind of over-blocking an operator responds to by carving the
    /// tree back open, which would take the tokens with it.
    #[test]
    fn test_config_stays_readable_while_its_credential_stores_do_not() {
        let guard = guard_at(Path::new("/"));

        for readable in [".config", ".config/nvim", ".config/starship.toml"] {
            let probe = home_path(readable);
            assert!(
                guard.check("Parameter 'file'", &probe, ReadOnly).is_ok(),
                "{probe} is ordinary application settings and must stay legible"
            );
        }

        for refused in [".config/gh", ".config/gh/hosts.yml", ".config/gcloud"] {
            let probe = home_path(refused);
            let error = guard
                .check("Parameter 'file'", &probe, ReadOnly)
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::ACLDenied, "{probe} must be refused");
        }
    }

    /// The stores that motivated the file entries. `git` reads both for HTTP
    /// auth, so a `git` module able to name them leaks a token without ever
    /// touching `~/.ssh`.
    #[test]
    fn test_plaintext_credential_files_at_the_home_root_are_refused() {
        let guard = guard_at(Path::new("/"));

        for refused in [".git-credentials", ".netrc"] {
            let probe = home_path(refused);
            let error = guard
                .check("Parameter 'file'", &probe, ReadOnly)
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::ACLDenied, "{probe} must be refused");
        }
    }

    #[test]
    fn test_a_credential_file_is_refused_to_both_modes() {
        let guard = guard_at(Path::new("/"));
        let key = home_path(".ssh/id_rsa");

        assert!(guard.check("Parameter 'file'", &key, ReadOnly).is_err());
        assert!(guard.check("Parameter 'file'", &key, Write).is_err());
    }

    #[test]
    fn test_a_reader_may_list_a_directory_that_merely_contains_credentials() {
        // `ls ~` and `ls /` are most of what a reader legitimately does, and
        // the ancestor rule exists for recursive *destruction*, which a reader
        // cannot perform. The cost — a recursive read rooted above `~/.ssh` —
        // is stated in threat-model §5.8 rather than papered over.
        let guard = guard_at(Path::new("/"));
        let home = home_path("");

        assert!(guard.check("Parameter 'file'", &home, ReadOnly).is_ok());
        assert!(guard.check("Parameter 'file'", "/", ReadOnly).is_ok());
    }

    #[test]
    fn test_a_writer_may_not_target_a_directory_that_contains_credentials() {
        // The same two paths, the other mode: `rm -rf ~` and `rm -rf /` take
        // the credential directories with them without ever naming one.
        let guard = guard_at(Path::new("/"));

        assert!(guard
            .check("Parameter 'file'", &home_path(""), Write)
            .is_err());
        assert!(guard.check("Parameter 'file'", "/", Write).is_err());
    }

    // ---- Resolution ------------------------------------------------------

    #[test]
    fn test_check_resolves_a_relative_path_against_the_root_before_judging() {
        // The point of the root: the same string is allowed under one working
        // directory and refused under another, and only the join can tell.
        let system = guard_at(Path::new("/usr"));
        assert!(
            system.check("Parameter 'file'", "share", Write).is_err(),
            "'share' under /usr is /usr/share and must be refused to a writer"
        );

        let workspace = tempfile::tempdir().expect("temp dir");
        let scratch = guard_at(workspace.path());
        assert!(
            scratch.check("Parameter 'file'", "share", Write).is_ok(),
            "the same relative value is ordinary work under a scratch root"
        );
    }

    #[test]
    fn test_check_refuses_a_relative_path_that_climbs_out_with_parent_dirs() {
        // `std::path::absolute` keeps `..` (POSIX requires it), so a guard that
        // only joined would compare the literal string and let this through.
        let guard = guard_at(Path::new("/usr/local/share"));
        let error = guard
            .check("Parameter 'file'", "../../../etc/passwd", Write)
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::ACLDenied);
        let resolved = error.details.get("resolved_path").expect("resolved_path");
        assert!(
            resolved
                .as_str()
                .is_some_and(|p| p.ends_with("/etc/passwd")),
            "the climb must resolve to the real target: {resolved:?}"
        );
    }

    #[test]
    fn test_check_follows_a_symlink_that_points_into_a_system_directory() {
        let workspace = tempfile::tempdir().expect("temp dir");
        let link = workspace.path().join("innocent");
        std::os::unix::fs::symlink("/etc", &link).expect("symlink");

        let guard = guard_at(workspace.path());
        let error = guard
            .check("Parameter 'file'", &link.to_string_lossy(), Write)
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::ACLDenied);
        assert!(
            error.message.contains("etc"),
            "a symlink to /etc is a request to touch /etc: {}",
            error.message
        );
    }

    /// macOS firmlinks make this the sharpest evidence that resolution runs
    /// before comparison: `/home` is not a symlink and not in the baseline,
    /// but it canonicalizes to `/System/Volumes/Data/home`, which is under a
    /// baseline entry. A guard comparing the supplied string would allow it.
    ///
    /// `/Users` is deliberately asserted alongside — it is *not* remapped, so
    /// the firmlink handling cannot quietly swallow the user's own workspace.
    #[cfg(target_os = "macos")]
    #[test]
    fn test_check_sees_through_a_macos_firmlink_into_the_system_volume() {
        let guard = guard_at(Path::new("/"));

        assert!(
            guard
                .check("Parameter 'file'", "/home/someone/work.txt", Write)
                .is_err(),
            "/home resolves into the system volume on macOS and must be refused"
        );
        assert!(
            guard
                .check("Parameter 'file'", "/Users/someone/work.txt", Write)
                .is_ok(),
            "/Users is not remapped and must stay usable"
        );
    }

    #[test]
    fn test_check_compares_whole_components_not_string_prefixes() {
        // `/etcetera` shares a string prefix with `/etc` and nothing else.
        // `str::starts_with` here would be a bug; `Path::starts_with` is not.
        let guard = guard_at(Path::new("/"));
        assert!(guard
            .check("Parameter 'file'", "/etcetera/notes", Write)
            .is_ok());
    }

    #[test]
    fn test_check_judges_a_path_that_does_not_exist_yet() {
        // A `mkdir -p` or `cp` destination has no inode to canonicalize, and
        // must still be judged rather than waved through.
        let guard = guard_at(Path::new("/"));
        assert!(guard
            .check("Parameter 'target'", "/etc/nonexistent/deeper/file", Write)
            .is_err());
    }

    #[test]
    fn test_check_allows_an_ordinary_workspace_path() {
        let workspace = tempfile::tempdir().expect("temp dir");
        let guard = guard_at(workspace.path());

        assert!(guard
            .check("Parameter 'file'", "src/main.rs", Write)
            .is_ok());
        assert!(guard
            .check(
                "Parameter 'file'",
                &workspace.path().join("out.txt").to_string_lossy(),
                Write
            )
            .is_ok());
    }

    #[test]
    fn test_resolve_joins_normalizes_and_follows_in_that_order() {
        let workspace = tempfile::tempdir().expect("temp dir");
        let real = workspace.path().canonicalize().expect("canonical temp dir");
        std::fs::create_dir(real.join("deep")).expect("mkdir");

        assert_eq!(
            resolve(Path::new("deep/../deep/x"), &real),
            real.join("deep/x")
        );
        assert_eq!(resolve(Path::new("./deep/x"), &real), real.join("deep/x"));
        assert_eq!(
            resolve(&real.join("deep/x"), Path::new("/unused")),
            real.join("deep/x")
        );
    }

    // ---- Configuration is additive only ----------------------------------

    #[test]
    fn test_new_denies_the_additional_paths_an_operator_configured() {
        let workspace = tempfile::tempdir().expect("temp dir");
        let protected = workspace.path().join("golden");
        std::fs::create_dir(&protected).expect("mkdir");

        let guard = guard_denying(workspace.path(), std::slice::from_ref(&protected));

        assert!(
            guard
                .check("Parameter 'file'", "golden/data.db", Write)
                .is_err(),
            "a configured entry must be enforced like a baseline one"
        );
        assert!(guard
            .check("Parameter 'file'", "other/data.db", Write)
            .is_ok());
    }

    #[test]
    fn test_a_configured_path_binds_readers_too() {
        // Configured entries join the credential list, not the system one: an
        // operator naming a path explicitly is asserting it is sensitive, and
        // the stronger reading is the one that cannot be wrong dangerously.
        let workspace = tempfile::tempdir().expect("temp dir");
        let protected = workspace.path().join("golden");
        std::fs::create_dir(&protected).expect("mkdir");

        let guard = guard_denying(workspace.path(), std::slice::from_ref(&protected));

        assert!(guard
            .check("Parameter 'file'", "golden/data.db", ReadOnly)
            .is_err());
    }

    #[test]
    fn test_new_keeps_the_baseline_when_the_configuration_names_something_else() {
        // The configuration surface is additive. There is no input to `new`
        // that removes /etc, and this is the test that says so.
        let workspace = tempfile::tempdir().expect("temp dir");
        let guard = guard_denying(workspace.path(), &[workspace.path().join("golden")]);

        assert!(guard
            .check("Parameter 'file'", "/etc/passwd", Write)
            .is_err());
        assert!(guard
            .check("Parameter 'file'", &home_path(".ssh/id_rsa"), ReadOnly)
            .is_err());
    }

    // ---- Configured carve-outs (`allowed_paths`) -------------------------

    #[test]
    fn test_a_configured_carve_out_reopens_a_subtree_of_a_system_path() {
        // The motivating case: an agent that has to write /etc/nginx/conf.d.
        let guard = guard_allowing(Path::new("/"), &[PathBuf::from("/etc/nginx/conf.d")]);

        assert!(
            guard
                .check("Parameter 'file'", "/etc/nginx/conf.d/site.conf", Write)
                .is_ok(),
            "the carve-out must make its own subtree writable"
        );
        // And nothing outside it moves.
        assert!(guard
            .check("Parameter 'file'", "/etc/nginx/nginx.conf", Write)
            .is_err());
        assert!(guard
            .check("Parameter 'file'", "/etc/passwd", Write)
            .is_err());
    }

    #[test]
    fn test_a_carve_out_is_empty_unless_configured() {
        // No default. The feature is inert until an operator writes it down.
        let guard = guard_at(Path::new("/"));
        assert!(guard
            .check("Parameter 'file'", "/etc/nginx/conf.d/site.conf", Write)
            .is_err());
    }

    #[test]
    fn test_a_carve_out_is_honoured_even_when_it_is_unwise() {
        // The operator owns this decision — `allowed_paths` does not
        // second-guess an explicit entry, it logs it. Pinning the behaviour
        // here so a later "safety" check cannot be added without the test that
        // documents the trade-off failing first.
        let guard = guard_allowing(Path::new("/"), &[PathBuf::from("/etc")]);
        assert!(guard
            .check("Parameter 'file'", "/etc/passwd", Write)
            .is_ok());
    }

    #[test]
    fn test_a_carve_out_loses_to_a_more_specific_denial() {
        // Carve-outs and denials share one specificity ladder, so an operator
        // can open a subtree and still fence off part of it.
        let guard = PathGuard::new(
            PathBuf::from("/"),
            GuardConfig {
                denied: &[PathBuf::from("/etc/nginx/conf.d/secrets")],
                allowed: &[PathBuf::from("/etc/nginx/conf.d")],
            },
        );

        assert!(guard
            .check("Parameter 'file'", "/etc/nginx/conf.d/site.conf", Write)
            .is_ok());
        assert!(guard
            .check("Parameter 'file'", "/etc/nginx/conf.d/secrets/key", Write)
            .is_err());
    }

    #[test]
    fn test_a_configured_carve_out_may_reopen_a_credential_path() {
        // The operator owns this. It is almost certainly a mistake — hence the
        // warning `warn_about_risky_carve_outs` logs — but "almost certainly"
        // is not the guard's call to make, and silently declining would leave
        // an operator staring at a config file that appears to do nothing.
        let key = dirs::home_dir().expect("home").join(".ssh");
        let guard = guard_allowing(Path::new("/"), std::slice::from_ref(&key));

        assert!(guard
            .check(
                "Parameter 'file'",
                &key.join("id_rsa").to_string_lossy(),
                ReadOnly
            )
            .is_ok());
    }

    #[test]
    fn test_the_derived_exemption_still_loses_a_tie_to_a_credential_path() {
        // The other half of the split: a *derived* carve-out that happens to
        // land exactly on a protected path is a coincidence, not an
        // instruction, so it refuses. This is what keeps `TMPDIR=~/.ssh` from
        // reading a key out.
        let guard = PathGuard {
            root: PathBuf::from("/"),
            system: Vec::new(),
            credential: vec![PathBuf::from("/home/u/.ssh")],
            exempt: vec![PathBuf::from("/home/u/.ssh")],
            credential_baseline: Vec::new(),
            credential_configured: Vec::new(),
            allowed: Vec::new(),
        };
        assert!(guard
            .denial_reason(Path::new("/home/u/.ssh/id_rsa"), ReadOnly)
            .is_some());
    }

    #[test]
    fn test_a_writer_may_recurse_into_a_directory_the_operator_carved_out() {
        // `rm -r /etc/nginx/conf.d` after opening it. The ancestor rule would
        // otherwise refuse the carve-out's own root whenever a denial sat
        // inside it.
        let guard = PathGuard::new(
            PathBuf::from("/"),
            GuardConfig {
                denied: &[PathBuf::from("/etc/nginx/conf.d/secrets")],
                allowed: &[PathBuf::from("/etc/nginx/conf.d")],
            },
        );
        assert!(guard
            .check("Parameter 'file'", "/etc/nginx/conf.d", Write)
            .is_ok());
        // The fence inside it still holds for a path that names it.
        assert!(guard
            .check("Parameter 'file'", "/etc/nginx/conf.d/secrets/key", Write)
            .is_err());
    }

    #[test]
    fn test_a_carve_out_does_not_disturb_the_temp_directory_exemption() {
        let tmp = std::env::temp_dir();
        let guard = guard_allowing(Path::new("/"), &[PathBuf::from("/etc/nginx/conf.d")]);

        assert!(guard
            .check(
                "Parameter 'file'",
                &tmp.join("build.log").to_string_lossy(),
                Write
            )
            .is_ok());
    }

    // ---- The temp-directory carve-out ------------------------------------

    #[test]
    fn test_new_exempts_the_temp_directory_from_the_var_baseline() {
        // On macOS $TMPDIR lives under /var, which the baseline guards. Without
        // the carve-out every use of the temp directory would be refused.
        let workspace = tempfile::tempdir().expect("temp dir");
        let guard = guard_at(workspace.path());
        let scratch = std::env::temp_dir().join("apexe-guard-probe.txt");

        assert!(
            guard
                .check("Parameter 'file'", &scratch.to_string_lossy(), Write)
                .is_ok(),
            "the temp directory must stay writable: {}",
            scratch.display()
        );
    }

    #[test]
    fn test_accept_exemption_discards_a_candidate_that_would_expose_a_system_path() {
        let system = vec![PathBuf::from("/etc"), PathBuf::from("/var")];

        // A TMPDIR pointing at a protected directory, or above one.
        assert!(!accept_exemption(Path::new("/etc"), &system));
        assert!(!accept_exemption(Path::new("/"), &system));
        // The legitimate case: a carve-out strictly inside a denied directory.
        assert!(accept_exemption(Path::new("/var/folders/ab/cd/T"), &system));
    }

    /// Regression: a home directory under `$TMPDIR` — ordinary in a sandbox or
    /// a CI container — used to make every credential path a descendant of the
    /// carve-out, which discarded the carve-out, which re-armed `/var` across
    /// the whole temp directory. Every temporary file was refused, and nothing
    /// in the failure pointed at `HOME`.
    #[test]
    fn test_a_home_under_the_temp_directory_does_not_void_the_carve_out() {
        let tmp = std::env::temp_dir();
        let system = vec![PathBuf::from("/var"), PathBuf::from("/etc")];

        assert!(
            accept_exemption(&tmp, &system),
            "the carve-out must survive a credential path nested inside it"
        );

        // And the credential path nested inside it is still refused, which is
        // what makes dropping it from `accept_exemption` safe.
        let guard = PathGuard {
            root: PathBuf::from("/"),
            system: vec![PathBuf::from("/var")],
            credential: vec![tmp.join("fakehome/.ssh")],
            exempt: vec![tmp.clone()],
            credential_baseline: Vec::new(),
            credential_configured: Vec::new(),
            allowed: Vec::new(),
        };
        assert!(
            guard
                .denial_reason(&tmp.join("fakehome/.ssh/id_rsa"), ReadOnly)
                .is_some(),
            "specificity must still protect a credential path inside the carve-out"
        );
        assert!(
            guard
                .denial_reason(&tmp.join("build/artifact.tar"), Write)
                .is_none(),
            "an ordinary temp path must stay writable"
        );
    }

    /// An exact overlap between a carve-out and a credential path is a tie, and
    /// a tie refuses — so `TMPDIR=~/.ssh` cannot read a key out.
    #[test]
    fn test_a_carve_out_that_exactly_overlaps_a_credential_path_still_refuses() {
        let guard = PathGuard {
            root: PathBuf::from("/"),
            system: Vec::new(),
            credential: vec![PathBuf::from("/home/u/.ssh")],
            exempt: vec![PathBuf::from("/home/u/.ssh")],
            credential_baseline: Vec::new(),
            credential_configured: Vec::new(),
            allowed: Vec::new(),
        };
        assert!(guard
            .denial_reason(Path::new("/home/u/.ssh/id_rsa"), ReadOnly)
            .is_some());
    }

    #[test]
    fn test_denial_reason_lets_the_more_specific_rule_decide() {
        let guard = PathGuard {
            root: PathBuf::from("/"),
            system: vec![PathBuf::from("/var")],
            credential: vec![PathBuf::from("/var/scratch/golden")],
            exempt: vec![PathBuf::from("/var/scratch")],
            credential_baseline: Vec::new(),
            credential_configured: Vec::new(),
            allowed: Vec::new(),
        };

        // Baseline /var (2) refuses a writer.
        assert!(guard
            .denial_reason(Path::new("/var/log/system.log"), Write)
            .is_some());
        // The carve-out (3) outranks it.
        assert!(guard
            .denial_reason(Path::new("/var/scratch/work"), Write)
            .is_none());
        // A configured entry (4) outranks the carve-out again.
        assert!(guard
            .denial_reason(Path::new("/var/scratch/golden/data.db"), Write)
            .is_some());
        // And binds a reader, where the system entry above it does not.
        assert!(guard
            .denial_reason(Path::new("/var/scratch/golden/data.db"), ReadOnly)
            .is_some());
        assert!(guard
            .denial_reason(Path::new("/var/log/system.log"), ReadOnly)
            .is_none());
    }

    #[test]
    fn test_denial_reason_refuses_on_a_tie() {
        // Equal specificity is not a licence to proceed: the refusal wins.
        let guard = PathGuard {
            root: PathBuf::from("/"),
            system: vec![PathBuf::from("/data")],
            credential: Vec::new(),
            exempt: vec![PathBuf::from("/data")],
            credential_baseline: Vec::new(),
            credential_configured: Vec::new(),
            allowed: Vec::new(),
        };
        assert!(guard
            .denial_reason(Path::new("/data/file"), Write)
            .is_some());
    }

    // ---- Documentation and invariants ------------------------------------

    /// Read a doc from the repository root.
    fn read_doc(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The baselines are a security boundary operators read about before they
    /// rely on them. A silent divergence between these lists and the
    /// documented ones is the failure this test exists to make loud: an entry
    /// added here and not there reads as unprotected, and one removed here but
    /// still documented reads as protected when it is not.
    #[test]
    fn test_every_baseline_entry_is_documented() {
        let manual = read_doc("user-manual.md");
        let threat_model = read_doc("threat-model.md");

        for entry in BASELINE_SYSTEM_PATHS {
            assert!(
                manual.contains(entry),
                "user-manual.md does not list the system baseline entry {entry}"
            );
            assert!(
                threat_model.contains(entry),
                "threat-model.md does not list the system baseline entry {entry}"
            );
        }
        for entry in BASELINE_CREDENTIAL_HOME_SUBPATHS {
            let documented = format!("~/{entry}");
            assert!(
                manual.contains(&documented),
                "user-manual.md does not list the credential baseline entry {documented}"
            );
            assert!(
                threat_model.contains(&documented),
                "threat-model.md does not list the credential baseline entry {documented}"
            );
        }
    }

    /// `/` must stay out of the system list, and must stay out for a reason
    /// the next reader can see. The first build listed it and refused every
    /// absolute path on the planet.
    #[test]
    fn test_root_is_not_a_baseline_entry() {
        assert!(
            !BASELINE_SYSTEM_PATHS.contains(&"/"),
            "`/` as a baseline entry makes the containment test match everything"
        );
        let guard = guard_at(Path::new("/"));
        assert!(
            guard
                .check("Parameter 'file'", "/srv/data/work.txt", Write)
                .is_ok(),
            "an ordinary absolute path outside the baseline must pass"
        );
    }

    #[test]
    fn test_access_mode_defaults_to_write_for_an_unclassified_module() {
        // A command nobody annotated is assumed able to destroy.
        assert_eq!(AccessMode::from_readonly(false), Write);
        assert_eq!(AccessMode::from_readonly(true), ReadOnly);
    }

    #[test]
    fn test_error_names_the_requested_and_the_resolved_path() {
        // The two differ whenever a symlink or `..` is involved, and a caller
        // told only "denied" cannot tell a mistake from a misconfiguration.
        let guard = guard_at(Path::new("/usr/share"));
        let error = guard
            .check("Parameter 'file'", "../../etc/hosts", Write)
            .unwrap_err();

        assert_eq!(
            error.details.get("requested_path"),
            Some(&serde_json::json!("../../etc/hosts"))
        );
        assert!(error.details.contains_key("resolved_path"));
        assert!(error.details.contains_key("protected_path"));
    }

    // ---- Introspection accessors, for `apexe policy` --------------------

    #[test]
    fn test_credential_baseline_and_configured_are_reported_separately() {
        // `apexe policy` must be able to say which entries are compiled in
        // and which an operator added, even though enforcement merges both
        // into one list.
        let extra = PathBuf::from("/srv/production-data");
        let guard = guard_denying(Path::new("/"), std::slice::from_ref(&extra));

        assert_eq!(
            guard.credential_baseline().len(),
            BASELINE_CREDENTIAL_HOME_SUBPATHS.len(),
            "the baseline list must not include the operator's addition"
        );
        assert_eq!(
            guard.credential_configured(),
            &[resolve(&extra, Path::new("/"))]
        );
        assert!(guard
            .credential_baseline()
            .iter()
            .chain(guard.credential_configured())
            .all(|p| p != &PathBuf::new()),);
    }

    #[test]
    fn test_allowed_and_exempt_paths_are_exposed() {
        let carve_out = PathBuf::from("/etc/nginx/conf.d");
        let guard = guard_allowing(Path::new("/"), std::slice::from_ref(&carve_out));

        assert_eq!(
            guard.allowed_paths(),
            &[resolve(&carve_out, Path::new("/"))]
        );
        assert_eq!(
            guard.exempt_paths(),
            &[resolve(&std::env::temp_dir(), Path::new("/"))]
        );
    }

    /// Every compiled-in entry must reach the guard. The count is no longer
    /// 1:1: an entry that is a symlink contributes both spellings, which is
    /// what stops a lexically-folded `/etc/passwd` from slipping past a
    /// baseline canonicalized to `/private/etc`.
    #[test]
    fn test_every_compiled_in_system_path_reaches_the_guard() {
        let guard = guard_at(Path::new("/"));
        let baseline = guard.system_baseline();
        for entry in BASELINE_SYSTEM_PATHS {
            let literal = PathBuf::from(entry);
            assert!(
                baseline.contains(&literal)
                    || baseline.contains(&resolve(&literal, Path::new("/"))),
                "{entry} is compiled in but reached no spelling in the guard"
            );
        }
        assert!(baseline.len() >= BASELINE_SYSTEM_PATHS.len());
    }

    /// The bug this pair exists for: on macOS `/etc` is a symlink into
    /// `/private`, so the baseline canonicalized to `/private/etc` and nothing
    /// in it was spelled `/etc`. A target only gets canonicalized where it
    /// exists — `resolve_existing_prefix` folds the rest lexically — so a `..`
    /// climb through a directory that is not there produced the literal
    /// `/etc/passwd`, which matched no entry and was allowed to a writer. The
    /// direct spelling was refused the whole time, which is what hid it.
    #[test]
    fn test_a_lexically_folded_system_path_is_refused_in_both_spellings() {
        let guard = guard_at(Path::new("/"));
        for target in ["/etc/passwd", "/var/db/x"] {
            let folded = PathBuf::from(target);
            assert!(
                guard.denial_reason(&folded, Write).is_some(),
                "{target} must be refused as written, not only once canonicalized"
            );
            let canonical = resolve(&folded, Path::new("/"));
            assert!(
                guard.denial_reason(&canonical, Write).is_some(),
                "{} must be refused too",
                canonical.display()
            );
        }
    }
}
