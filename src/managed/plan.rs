//! Plans and their digest.
//!
//! A plan is the immutable statement a `run` request executes: the form, the
//! typed operands, the derived effects, the fixed argv and the pinned
//! identities. The digest commits to all of it, so a `run` that carries a
//! different digest is refused as stale rather than executed against a plan the
//! caller never saw. See
//! `docs/managed-execution/session-protocol.md#canonical-representation`.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::backend::BackendId;
use super::effect::Effect;
use super::error::{ManagedError, ManagedErrorCode};
use super::form::{Approval, Args, FormId};
use super::json::hex_lower;

/// A 128-bit identifier rendered as 32 lower-case hex digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(String);

impl Id {
    /// Mint an identifier from the OS CSPRNG.
    pub fn generate() -> Result<Self, ManagedError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| {
            ManagedError::planning(
                ManagedErrorCode::IsolationFailed,
                format!("the OS CSPRNG is unavailable: {error}"),
            )
        })?;
        Ok(Self(hex_lower(&bytes)))
    }

    /// Parse a wire identifier: exactly 32 lower-case hex digits.
    pub fn parse(raw: &str) -> Result<Self, ManagedError> {
        let valid = raw.len() == 32
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if valid {
            Ok(Self(raw.to_string()))
        } else {
            Err(ManagedError::channel(
                ManagedErrorCode::ProtocolInvalid,
                "identifier must be 32 lower-case hex digits",
            ))
        }
    }

    /// The wire spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A session identifier.
pub type SessionId = Id;
/// A call identifier.
pub type CallId = Id;

/// The finite budget a plan carries (isolation-domain.md ISO-5 defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub wall_ms: u64,
    pub output_bytes: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            wall_ms: 10_000,
            output_bytes: 1024 * 1024,
        }
    }
}

impl Budget {
    /// Reject a non-finite or zero budget.
    pub fn validate(&self) -> Result<(), ManagedError> {
        if self.wall_ms == 0 || self.output_bytes == 0 {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                "budget values must be positive",
            )
            .with_field("budget"));
        }
        Ok(())
    }
}

/// An immutable, digestible execution plan.
#[derive(Debug, Clone)]
pub struct Plan {
    pub call_id: CallId,
    pub session_id: SessionId,
    pub form: FormId,
    pub args: Args,
    pub effects: Vec<Effect>,
    /// The fixed argv for a native form; `None` for a builtin.
    pub argv: Option<Vec<String>>,
    pub policy_digest: String,
    pub isolation_digest: String,
    pub backend: BackendId,
    pub runtime_image: String,
    /// The pinned executable identity, or `None` for a builtin.
    pub executable: Option<String>,
    pub budget: Budget,
    pub approval: Approval,
    pub prepared_at_ms: u64,
    pub expires_at_ms: u64,
}

impl Plan {
    /// The canonical plan digest: lower-case hex SHA-256 over RFC 8785 bytes.
    pub fn digest(&self) -> Result<String, ManagedError> {
        let canonical = serde_jcs::to_vec(&self.canonical_value()).map_err(|error| {
            ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                format!("plan cannot be canonicalized: {error}"),
            )
        })?;
        let mut hasher = Sha256::new();
        hasher.update(&canonical);
        Ok(hex_lower(&hasher.finalize()))
    }

    /// The effect array the protocol shows the agent.
    pub fn effects_json(&self) -> Vec<Value> {
        self.effects.iter().map(Effect::to_json).collect()
    }

    /// Whether the plan has expired at `now_ms`.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }

    /// The canonical JSON form committed to by [`Self::digest`].
    fn canonical_value(&self) -> Value {
        json!({
            "v": 1,
            "session_id": self.session_id.as_str(),
            "call_id": self.call_id.as_str(),
            "policy_digest": self.policy_digest,
            "isolation_digest": self.isolation_digest,
            "form": self.form.as_str(),
            "effects": self.effects_json(),
            "backend": self.backend.as_str(),
            "runtime_image": self.runtime_image,
            "executable": self.executable,
            "budget": {
                "wall_ms": self.budget.wall_ms,
                "output_bytes": self.budget.output_bytes,
            },
            "argv": self.argv,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed::effect::{Effect, Object, Op, Range, RootId};
    use crate::managed::form::ResourceRef;
    use crate::managed::path::RelPath;

    fn plan() -> Plan {
        let root = RootId::parse("workspace").unwrap();
        let args = Args::CatFile {
            path: ResourceRef {
                root: root.clone(),
                path: RelPath::parse("a.txt").unwrap(),
            },
        };
        let argv = args.fixed_argv(std::path::Path::new("/view/a.txt"));
        Plan {
            args,
            call_id: Id::parse("0123456789abcdef0123456789abcdef").unwrap(),
            session_id: Id::parse("fedcba9876543210fedcba9876543210").unwrap(),
            form: FormId::CatFile,
            effects: vec![Effect {
                op: Op::ReadFile,
                object: Object::file(root, RelPath::parse("a.txt").unwrap()),
                range: Range::Exact,
            }],
            argv,
            policy_digest: "p".repeat(64),
            isolation_digest: "i".repeat(64),
            backend: BackendId::Linux,
            runtime_image: "sha256:image".into(),
            executable: Some("cli.cat".into()),
            budget: Budget::default(),
            approval: Approval::None,
            prepared_at_ms: 0,
            expires_at_ms: 300_000,
        }
    }

    #[test]
    fn test_identifiers_are_32_lower_hex() {
        let id = Id::generate().unwrap();
        assert_eq!(id.as_str().len(), 32);
        assert!(Id::parse(id.as_str()).is_ok());
        let upper = "A".repeat(32);
        let short = "0".repeat(31);
        for bad in [upper.as_str(), short.as_str(), "zz"] {
            assert!(Id::parse(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn test_plan_digest_is_stable_and_field_sensitive() {
        let plan = plan();
        assert_eq!(plan.digest().unwrap(), plan.digest().unwrap());

        let mut changed = plan.clone();
        changed.budget.wall_ms += 1;
        assert_ne!(
            plan.digest().unwrap(),
            changed.digest().unwrap(),
            "the digest must commit to the budget"
        );

        let mut other_effect = plan.clone();
        other_effect.effects[0].op = Op::ListDir;
        assert_ne!(plan.digest().unwrap(), other_effect.digest().unwrap());
    }

    #[test]
    fn test_a_zero_budget_is_refused() {
        let budget = Budget {
            wall_ms: 0,
            output_bytes: 1,
        };
        assert!(budget.validate().is_err());
    }
}
