//! Backend capability reports.
//!
//! A policy is only admissible when the selected backend can enforce every
//! promise it makes. The compiler therefore asks the backend first and refuses
//! to activate when a required capability is missing — it never degrades a
//! promised restriction into an advisory one. See
//! `docs/managed-execution/policy.md#policy-input-and-compilation`.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::effect::Op;

#[cfg(target_os = "linux")]
mod linux;

/// The backend a managed session is compiled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendId {
    /// Linux namespaces plus Landlock. The v1 managed backend.
    Linux,
    /// macOS Seatbelt/SBPL. A prototype that cannot activate a managed session.
    Sbpl,
}

impl BackendId {
    /// Parse the `--backend` value.
    pub fn parse(raw: &str) -> Result<Self, super::error::ManagedError> {
        match raw {
            "linux" => Ok(Self::Linux),
            "sbpl" => Ok(Self::Sbpl),
            other => Err(super::error::ManagedError::planning(
                super::error::ManagedErrorCode::BackendUnsupported,
                format!("unknown backend '{other}'"),
            )
            .with_field("backend")),
        }
    }

    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Sbpl => "sbpl",
        }
    }
}

/// One enforcement capability a policy can require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ReadFile,
    ReadDir,
    MakeReg,
    NoReplacePublish,
    TruncateDenial,
    Namespaces,
    DescriptorBoundary,
    ResourceLimits,
}

impl Capability {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::ReadDir => "read_dir",
            Self::MakeReg => "make_reg",
            Self::NoReplacePublish => "no_replace_publish",
            Self::TruncateDenial => "truncate_denial",
            Self::Namespaces => "namespaces",
            Self::DescriptorBoundary => "descriptor_boundary",
            Self::ResourceLimits => "resource_limits",
        }
    }
}

/// What a backend reports it can enforce right now.
#[derive(Debug, Clone)]
pub struct CapabilityReport {
    pub backend: BackendId,
    /// Whether the backend may activate a managed session at all.
    pub supported: bool,
    /// Landlock ABI, when the backend is Linux.
    pub landlock_abi: Option<u32>,
    pub capabilities: BTreeSet<Capability>,
    /// Human-readable reasons a capability or the backend is unavailable.
    pub missing: Vec<String>,
    pub notes: Vec<String>,
    /// What the readiness probe actually executed. `None` on a host where the
    /// probe cannot run at all (a non-Linux host, or the SBPL prototype).
    pub readiness: Option<Readiness>,
}

/// The result of actually performing each isolation step, not inferring it.
///
/// This is the probe ISO-A7 asks for: the launcher must prove the mechanism
/// works on this kernel, because a capability list derived from a version
/// string is a guess. Each field is the *observed* outcome of the real call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Readiness {
    pub namespaces: bool,
    pub uid_map: bool,
    pub mount_view: bool,
    pub no_replace_publish: bool,
    pub landlock_installed: bool,
    pub landlock_denies_outside: bool,
    pub close_range: bool,
}

impl Readiness {
    /// Whether every step the v1 profile needs succeeded.
    pub fn complete(self) -> bool {
        self.namespaces
            && self.uid_map
            && self.mount_view
            && self.no_replace_publish
            && self.landlock_installed
            && self.landlock_denies_outside
            && self.close_range
    }

    /// The failed steps, in the order the specification lists them.
    pub fn failures(self) -> Vec<&'static str> {
        let mut failed = Vec::new();
        for (ok, name) in [
            (self.namespaces, "namespaces"),
            (self.uid_map, "uid_map"),
            (self.mount_view, "mount_view"),
            (self.no_replace_publish, "no_replace_publish"),
            (self.landlock_installed, "landlock_installed"),
            (self.landlock_denies_outside, "landlock_denies_outside"),
            (self.close_range, "close_range"),
        ] {
            if !ok {
                failed.push(name);
            }
        }
        failed
    }
}

impl CapabilityReport {
    /// The capabilities required to enforce `op`, always including the
    /// session-wide ones.
    pub fn required_for(op: Op) -> &'static [Capability] {
        match op {
            Op::InspectMetadata => &[],
            Op::ListDir => &[Capability::ReadDir],
            Op::ReadFile => &[Capability::ReadFile],
            Op::CreateEntry => &[
                Capability::MakeReg,
                Capability::NoReplacePublish,
                Capability::TruncateDenial,
            ],
            Op::WriteFile => &[Capability::ReadFile, Capability::MakeReg],
            Op::TruncateFile => &[Capability::TruncateDenial],
            Op::RemoveEntry => &[Capability::MakeReg],
            Op::RenameEntry => &[Capability::MakeReg],
            Op::ModifyMetadata => &[Capability::TruncateDenial],
        }
    }

    /// The session-wide capabilities every managed session requires.
    pub const SESSION_WIDE: [Capability; 3] = [
        Capability::Namespaces,
        Capability::DescriptorBoundary,
        Capability::ResourceLimits,
    ];

    /// Every capability required by `ops`, deduplicated and ordered.
    pub fn required_for_ops(ops: &BTreeSet<Op>) -> BTreeSet<Capability> {
        let mut required: BTreeSet<Capability> = Self::SESSION_WIDE.into_iter().collect();
        for op in ops {
            required.extend(Self::required_for(*op));
        }
        required
    }

    /// The required capabilities this report cannot enforce.
    pub fn unsatisfied(&self, ops: &BTreeSet<Op>) -> Vec<Capability> {
        Self::required_for_ops(ops)
            .into_iter()
            .filter(|capability| !self.capabilities.contains(capability))
            .collect()
    }

    /// The report as the `policy check` JSON body.
    pub fn to_json(&self) -> Value {
        json!({
            "backend": self.backend.as_str(),
            "supported": self.supported,
            "landlock_abi": self.landlock_abi,
            "capabilities": self.capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            "missing": self.missing,
            "notes": self.notes,
            "readiness": self.readiness,
        })
    }
}

/// Probe the selected backend.
///
/// The report is evidence, not a promise: a successful probe is required for
/// launch even after a passing `policy check`, because the filesystem and the
/// kernel can differ between the two.
pub fn probe(backend: BackendId) -> CapabilityReport {
    match backend {
        BackendId::Sbpl => CapabilityReport {
            backend,
            supported: false,
            landlock_abi: None,
            capabilities: BTreeSet::new(),
            missing: vec!["SBPL is a prototype backend".into()],
            readiness: None,
            notes: vec![
                "macOS Seatbelt can express deny-inside-allow, but v1 does not \
                 activate a managed session on it; see the isolation-domain \
                 release boundary"
                    .into(),
            ],
        },
        BackendId::Linux => {
            #[cfg(target_os = "linux")]
            {
                linux::probe()
            }
            #[cfg(not(target_os = "linux"))]
            {
                CapabilityReport {
                    backend,
                    supported: false,
                    landlock_abi: None,
                    capabilities: BTreeSet::new(),
                    missing: vec![format!(
                        "the linux backend requires a Linux host; this is {}",
                        std::env::consts::OS
                    )],
                    readiness: None,
                    notes: vec!["run the v1 managed profile in a Linux VM; the macOS SBPL \
                         probe does not validate it"
                        .into()],
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_ids_round_trip() {
        for backend in [BackendId::Linux, BackendId::Sbpl] {
            assert_eq!(BackendId::parse(backend.as_str()).unwrap(), backend);
        }
        assert!(BackendId::parse("docker").is_err());
    }

    #[test]
    fn test_sbpl_never_activates_a_managed_session() {
        let report = probe(BackendId::Sbpl);
        assert!(!report.supported);
        assert!(report.capabilities.is_empty());
        assert!(!report.missing.is_empty());
    }

    #[test]
    fn test_required_capabilities_follow_the_effect_table() {
        let mut ops = BTreeSet::new();
        ops.insert(Op::CreateEntry);
        let required = CapabilityReport::required_for_ops(&ops);
        assert!(required.contains(&Capability::MakeReg));
        assert!(required.contains(&Capability::NoReplacePublish));
        assert!(required.contains(&Capability::Namespaces));
        assert!(!required.contains(&Capability::ReadDir));
    }

    #[test]
    fn test_an_empty_report_unsatisfies_every_session_wide_capability() {
        let report = CapabilityReport {
            backend: BackendId::Linux,
            supported: false,
            landlock_abi: None,
            capabilities: BTreeSet::new(),
            missing: vec![],
            notes: vec![],
            readiness: None,
        };
        let ops = BTreeSet::from([Op::ReadFile]);
        let unsatisfied = report.unsatisfied(&ops);
        assert!(unsatisfied.contains(&Capability::ReadFile));
        assert!(unsatisfied.contains(&Capability::Namespaces));
    }
}
