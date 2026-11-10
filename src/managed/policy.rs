//! Managed policy: strict parsing, grant intersection and compilation.
//!
//! The policy is the operator's artifact, supplied to the launcher. A client
//! never submits one and cannot widen it. The contract is
//! `docs/managed-execution/policy.md`; this module implements the "Policy input
//! and compilation" section, including its two load-bearing rules:
//!
//! * a form grant must be covered by a session grant (the *intersection*, not
//!   the union);
//! * a policy the selected backend cannot enforce is refused, never degraded.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::backend::CapabilityReport;
use super::effect::{Effect, Grant, Op, RootId};
use super::error::{ManagedError, ManagedErrorCode};
use super::form::{Approval, FormId};
use super::json::{hex_lower, parse_strict};
use super::path::RelPath;

/// Maximum policy document size.
pub const MAX_POLICY_BYTES: usize = 64 * 1024;
/// Maximum session grants or per-form grants.
pub const MAX_GRANTS: usize = 64;
/// Maximum form rules.
pub const MAX_FORMS: usize = 5;
/// Maximum bytes in a rule ID.
const MAX_RULE_ID_BYTES: usize = 63;

/// What a resource root is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    /// The session's writable workspace projection.
    Workspace,
    /// An immutable reference materialized by the launcher.
    Reference,
}

/// The launcher's description of one resource root.
#[derive(Debug, Clone)]
pub struct RootSpec {
    pub kind: RootKind,
    /// Root-relative paths the launcher never exposes. V1 rejects a policy that
    /// asks to write anywhere that overlaps one.
    pub protected: Vec<RelPath>,
}

/// The launcher-owned resource map a policy is validated against.
#[derive(Debug, Clone, Default)]
pub struct ResourceMap {
    roots: BTreeMap<RootId, RootSpec>,
}

impl ResourceMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one root. A later entry for the same ID replaces the earlier one.
    pub fn add(&mut self, id: RootId, spec: RootSpec) {
        self.roots.insert(id, spec);
    }

    /// The spec for `id`, if the launcher declared it.
    pub fn get(&self, id: &RootId) -> Option<&RootSpec> {
        self.roots.get(id)
    }

    /// Whether `id` is an immutable reference root.
    pub fn is_reference(&self, id: &RootId) -> bool {
        self.roots
            .get(id)
            .is_some_and(|spec| spec.kind == RootKind::Reference)
    }

    /// A protected path that overlaps `prefix`, if any.
    ///
    /// Overlap is either direction: a grant covering a protected subtree, or a
    /// grant *inside* one. Both are refused, because v1 has no shadow mount and
    /// therefore no way to enforce a carve-out inside an exposed writable root.
    pub fn protected_overlap(&self, id: &RootId, prefix: &RelPath) -> Option<&RelPath> {
        let spec = self.roots.get(id)?;
        spec.protected
            .iter()
            .find(|protected| protected.starts_with(prefix) || prefix.starts_with(protected))
    }
}

/// One form rule: which form, what it may touch, and whether it needs approval.
#[derive(Debug, Clone)]
pub struct FormRule {
    pub id: String,
    pub form: FormId,
    pub grants: Vec<Grant>,
    pub approval: Approval,
}

/// A validated policy document.
#[derive(Debug, Clone)]
pub struct Policy {
    pub version: u8,
    pub session_grants: Vec<Grant>,
    pub forms: Vec<FormRule>,
}

/// A policy whose grants have been checked against a backend capability report.
#[derive(Debug, Clone)]
pub struct CompiledPolicy {
    pub digest: String,
    pub forms: BTreeMap<FormId, CompiledForm>,
}

/// One compiled form rule.
#[derive(Debug, Clone)]
pub struct CompiledForm {
    pub rule_id: String,
    pub form: FormId,
    pub grants: Vec<Grant>,
    pub approval: Approval,
}

impl Policy {
    /// Parse, validate and cross-check a policy document.
    pub fn parse(input: &[u8], resources: &ResourceMap) -> Result<Self, ManagedError> {
        if input.len() > MAX_POLICY_BYTES {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("policy exceeds {MAX_POLICY_BYTES} bytes"),
            ));
        }
        let text = std::str::from_utf8(input).map_err(|_| {
            ManagedError::planning(ManagedErrorCode::PolicyInvalid, "policy is not UTF-8")
        })?;
        let value = parse_strict(text)?;
        let object = exact_object(&value, &["version", "session_grants", "forms"], "policy")?;

        let version = object["version"].as_u64().ok_or_else(|| {
            ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                "version must be an integer",
            )
            .with_field("version")
        })?;
        if version != 1 {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("version {version} is not supported"),
            )
            .with_field("version"));
        }

        let session_grants = parse_grants(
            &object["session_grants"],
            "session_grants",
            resources,
            MAX_GRANTS,
        )?;
        let forms = parse_forms(&object["forms"], resources)?;
        check_form_grants_against_session(&forms, &session_grants)?;

        Ok(Self {
            version: 1,
            session_grants,
            forms,
        })
    }

    /// The canonical policy digest: lower-case hex SHA-256 over RFC 8785 bytes.
    pub fn digest(&self) -> Result<String, ManagedError> {
        let canonical = serde_jcs::to_vec(&self.canonical_value()).map_err(|error| {
            ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("policy cannot be canonicalized: {error}"),
            )
        })?;
        let mut hasher = Sha256::new();
        hasher.update(&canonical);
        Ok(hex_lower(&hasher.finalize()))
    }

    /// Check the backend can enforce every grant, then compile.
    pub fn compile(&self, report: &CapabilityReport) -> Result<CompiledPolicy, ManagedError> {
        if !report.supported {
            return Err(ManagedError::planning(
                ManagedErrorCode::BackendUnsupported,
                format!(
                    "backend '{}' cannot activate a managed session: {}",
                    report.backend.as_str(),
                    report.missing.join("; ")
                ),
            ));
        }
        let mut ops: BTreeSet<Op> = BTreeSet::new();
        for rule in &self.forms {
            for grant in &rule.grants {
                ops.extend(grant.ops.iter().copied());
            }
        }
        let unsatisfied = report.unsatisfied(&ops);
        if !unsatisfied.is_empty() {
            return Err(ManagedError::planning(
                ManagedErrorCode::BackendUnsupported,
                format!(
                    "backend '{}' cannot enforce: {}",
                    report.backend.as_str(),
                    unsatisfied
                        .iter()
                        .map(|capability| capability.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        let mut forms = BTreeMap::new();
        for rule in &self.forms {
            forms.insert(
                rule.form,
                CompiledForm {
                    rule_id: rule.id.clone(),
                    form: rule.form,
                    grants: rule.grants.clone(),
                    approval: rule.approval,
                },
            );
        }
        Ok(CompiledPolicy {
            digest: self.digest()?,
            forms,
        })
    }

    /// The policy as the `policy check` JSON body.
    pub fn to_json(&self) -> Value {
        self.canonical_value()
    }

    /// The canonical JSON form used for the digest and for reporting.
    fn canonical_value(&self) -> Value {
        let mut grants: Vec<Value> = self.session_grants.iter().map(grant_to_json).collect();
        grants.sort_by_key(|value| value.to_string());
        let mut forms: Vec<Value> = self.forms.iter().map(form_to_json).collect();
        forms.sort_by_key(|value| value["id"].as_str().unwrap_or("").to_string());
        json!({
            "version": self.version,
            "session_grants": grants,
            "forms": forms,
        })
    }
}

impl CompiledPolicy {
    /// Authorize every declared effect of one call.
    ///
    /// The check is against the form's own grants, which parsing has already
    /// constrained to the session grants, so this is the specification's
    /// "intersection of the session and form grants".
    pub fn authorize(
        &self,
        form: FormId,
        effects: &[Effect],
    ) -> Result<&CompiledForm, ManagedError> {
        let rule = self.forms.get(&form).ok_or_else(|| {
            ManagedError::planning(
                ManagedErrorCode::FormUnsupported,
                format!("form '{}' is not admitted by this policy", form.as_str()),
            )
            .with_field("form")
        })?;
        for effect in effects {
            let covered = rule.grants.iter().any(|grant| grant.covers(effect));
            if !covered {
                return Err(ManagedError::planning(
                    ManagedErrorCode::ResourceDenied,
                    format!(
                        "effect '{}' on {} is outside the form's grant",
                        effect.op.as_str(),
                        effect.object.locator.kind()
                    ),
                )
                .with_rule(rule.rule_id.clone()));
            }
        }
        Ok(rule)
    }
}

/// Parse a policy field that must be an object with exactly `fields`.
fn exact_object<'a>(
    value: &'a Value,
    fields: &[&str],
    what: &str,
) -> Result<&'a Map<String, Value>, ManagedError> {
    let object = value.as_object().ok_or_else(|| {
        ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            format!("{what} must be an object"),
        )
    })?;
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut wanted = fields.to_vec();
    wanted.sort_unstable();
    if keys != wanted {
        return Err(ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            format!("{what} must carry exactly {}", wanted.join(", ")),
        ));
    }
    Ok(object)
}

/// Parse a `grants` array.
fn parse_grants(
    value: &Value,
    field: &str,
    resources: &ResourceMap,
    maximum: usize,
) -> Result<Vec<Grant>, ManagedError> {
    let array = value.as_array().ok_or_else(|| {
        ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            format!("{field} must be an array"),
        )
        .with_field(field)
    })?;
    if array.is_empty() || array.len() > maximum {
        return Err(ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            format!("{field} must hold 1–{maximum} grants"),
        )
        .with_field(field));
    }
    let mut grants = Vec::with_capacity(array.len());
    let mut seen = BTreeSet::new();
    for entry in array {
        let grant = parse_grant(entry, resources)?;
        if !seen.insert(grant_key(&grant)) {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("{field} contains a duplicate grant"),
            )
            .with_field(field));
        }
        grants.push(grant);
    }
    Ok(grants)
}

/// Parse one grant object.
fn parse_grant(value: &Value, resources: &ResourceMap) -> Result<Grant, ManagedError> {
    let object = exact_object(value, &["root", "prefix", "ops"], "grant")?;
    let root = object["root"].as_str().ok_or_else(|| {
        ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            "grant root must be a string",
        )
        .with_field("root")
    })?;
    let root = RootId::parse(root)?;
    let spec = resources.get(&root).ok_or_else(|| {
        ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            format!("root '{root}' is not in the launcher's resource map"),
        )
        .with_field("root")
    })?;
    let prefix = object["prefix"].as_str().ok_or_else(|| {
        ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            "grant prefix must be a string",
        )
        .with_field("prefix")
    })?;
    let prefix = RelPath::parse(prefix)?;

    let ops_value = object["ops"].as_array().ok_or_else(|| {
        ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            "grant ops must be an array",
        )
        .with_field("ops")
    })?;
    if ops_value.is_empty() {
        return Err(ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            "grant ops must not be empty",
        )
        .with_field("ops"));
    }
    let mut ops = BTreeSet::new();
    for entry in ops_value {
        let raw = entry.as_str().ok_or_else(|| {
            ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                "grant ops entries must be strings",
            )
            .with_field("ops")
        })?;
        let op = Op::parse(raw)?;
        if !Op::V1.contains(&op) {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("operation '{raw}' is reserved and not grantable in v1"),
            )
            .with_field("ops"));
        }
        if !ops.insert(op) {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("operation '{raw}' is listed twice"),
            )
            .with_field("ops"));
        }
    }

    if ops.contains(&Op::CreateEntry) {
        if spec.kind == RootKind::Reference {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                "create_entry cannot be granted on an immutable reference",
            )
            .with_field("ops"));
        }
        if resources.protected_overlap(&root, &prefix).is_some() {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                "a writable grant may not overlap a protected subtree",
            )
            .with_field("prefix"));
        }
    }

    Ok(Grant { root, prefix, ops })
}

/// Parse the `forms` array.
fn parse_forms(value: &Value, resources: &ResourceMap) -> Result<Vec<FormRule>, ManagedError> {
    let array = value.as_array().ok_or_else(|| {
        ManagedError::planning(ManagedErrorCode::PolicyInvalid, "forms must be an array")
            .with_field("forms")
    })?;
    if array.is_empty() || array.len() > MAX_FORMS {
        return Err(ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            format!("forms must hold 1–{MAX_FORMS} rules"),
        )
        .with_field("forms"));
    }
    let mut rules = Vec::with_capacity(array.len());
    let mut ids = BTreeSet::new();
    let mut forms = BTreeSet::new();
    for entry in array {
        let object = exact_object(entry, &["id", "form", "grants", "approval"], "form rule")?;
        let id = object["id"].as_str().ok_or_else(|| {
            ManagedError::planning(ManagedErrorCode::PolicyInvalid, "form id must be a string")
                .with_field("id")
        })?;
        validate_rule_id(id)?;
        if !ids.insert(id.to_string()) {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("form rule id '{id}' is used twice"),
            )
            .with_field("id"));
        }
        let form_raw = object["form"].as_str().ok_or_else(|| {
            ManagedError::planning(ManagedErrorCode::PolicyInvalid, "form must be a string")
                .with_field("form")
        })?;
        let form = FormId::parse(form_raw)?;
        if !forms.insert(form) {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("form '{}' has more than one rule", form.as_str()),
            )
            .with_field("form"));
        }
        let grants = parse_grants(&object["grants"], "grants", resources, MAX_GRANTS)?;
        let approval_raw = object["approval"].as_str().ok_or_else(|| {
            ManagedError::planning(ManagedErrorCode::PolicyInvalid, "approval must be a string")
                .with_field("approval")
        })?;
        let approval = Approval::parse(approval_raw)?;
        rules.push(FormRule {
            id: id.to_string(),
            form,
            grants,
            approval,
        });
    }
    Ok(rules)
}

/// Validate a form rule ID: `[a-z][a-z0-9_-]{0,63}`.
fn validate_rule_id(id: &str) -> Result<(), ManagedError> {
    let mut chars = id.chars();
    let valid = id.len() <= MAX_RULE_ID_BYTES
        && matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(ManagedError::planning(
            ManagedErrorCode::PolicyInvalid,
            "rule id must match [a-z][a-z0-9_-]{0,63}",
        )
        .with_field("id"))
    }
}

/// Require every form grant to be covered by a session grant.
fn check_form_grants_against_session(
    forms: &[FormRule],
    session: &[Grant],
) -> Result<(), ManagedError> {
    for rule in forms {
        for grant in &rule.grants {
            for op in &grant.ops {
                let covered = session.iter().any(|candidate| {
                    candidate.root == grant.root
                        && candidate.ops.contains(op)
                        && grant.prefix.starts_with(&candidate.prefix)
                });
                if !covered {
                    return Err(ManagedError::planning(
                        ManagedErrorCode::PolicyInvalid,
                        format!(
                            "form rule '{}' grants '{}' outside the session grants",
                            rule.id,
                            op.as_str()
                        ),
                    )
                    .with_rule(rule.id.clone()));
                }
            }
        }
    }
    Ok(())
}

/// The identity of a grant, used for duplicate detection.
fn grant_key(grant: &Grant) -> (String, String, Vec<String>) {
    (
        grant.root.as_str().to_string(),
        grant.prefix.as_slash(),
        grant.ops.iter().map(|op| op.as_str().to_string()).collect(),
    )
}

/// One grant as canonical JSON, with `ops` sorted.
fn grant_to_json(grant: &Grant) -> Value {
    let ops: Vec<&str> = grant.ops.iter().map(|op| op.as_str()).collect();
    json!({
        "root": grant.root.as_str(),
        "prefix": grant.prefix.as_slash(),
        "ops": ops,
    })
}

/// One form rule as canonical JSON.
fn form_to_json(rule: &FormRule) -> Value {
    let mut grants: Vec<Value> = rule.grants.iter().map(grant_to_json).collect();
    grants.sort_by_key(|value| value.to_string());
    json!({
        "id": rule.id,
        "form": rule.form.as_str(),
        "grants": grants,
        "approval": match rule.approval {
            Approval::None => "none",
            Approval::Required => "required",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed::backend::{probe, BackendId, Capability};

    fn resources() -> ResourceMap {
        let mut map = ResourceMap::new();
        map.add(
            RootId::parse("workspace").unwrap(),
            RootSpec {
                kind: RootKind::Workspace,
                protected: vec![],
            },
        );
        map.add(
            RootId::parse("reference").unwrap(),
            RootSpec {
                kind: RootKind::Reference,
                protected: vec![],
            },
        );
        map
    }

    /// The fixture the tests mutate. Session grants:
    ///   workspace  ""     inspect_metadata, list_dir, read_file
    ///   workspace  "out"  inspect_metadata, create_entry
    ///   reference  ""     inspect_metadata, read_file
    fn policy_value() -> Value {
        json!({
            "version": 1,
            "session_grants": [
                {"root": "workspace", "prefix": "", "ops": ["inspect_metadata", "list_dir", "read_file"]},
                {"root": "workspace", "prefix": "out", "ops": ["inspect_metadata", "create_entry"]},
                {"root": "reference", "prefix": "", "ops": ["inspect_metadata", "read_file"]}
            ],
            "forms": [
                {
                    "id": "read-workspace",
                    "form": "cli.cat.file",
                    "grants": [{"root": "workspace", "prefix": "", "ops": ["inspect_metadata", "read_file"]}],
                    "approval": "none"
                },
                {
                    "id": "copy-out",
                    "form": "builtin.copy_create.to_path",
                    "grants": [{"root": "workspace", "prefix": "out", "ops": ["inspect_metadata", "create_entry"]}],
                    "approval": "none"
                }
            ]
        })
    }

    fn parse_value(value: &Value) -> Result<Policy, ManagedError> {
        Policy::parse(value.to_string().as_bytes(), &resources())
    }

    fn valid_policy() -> String {
        policy_value().to_string()
    }

    #[test]
    fn test_a_valid_policy_parses_and_digests() {
        let policy = parse_value(&policy_value()).expect("the fixture is valid");
        assert_eq!(policy.version, 1);
        let digest = policy.digest().unwrap();
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_duplicate_object_keys_are_refused() {
        let input = valid_policy().replacen("\"version\":1", "\"version\":1,\"version\":2", 1);
        let error = Policy::parse(input.as_bytes(), &resources())
            .expect_err("duplicate keys must be refused");
        assert_eq!(error.code, ManagedErrorCode::PolicyInvalid);
        assert!(error.message.contains("duplicate"), "{}", error.message);
    }

    #[test]
    fn test_unknown_root_is_refused() {
        let mut value = policy_value();
        value["session_grants"][0]["root"] = json!("elsewhere");
        let error = parse_value(&value).expect_err("an undeclared root must be refused");
        assert_eq!(error.code, ManagedErrorCode::PolicyInvalid);
    }

    #[test]
    fn test_a_form_grant_wider_than_the_session_is_refused() {
        let mut value = policy_value();
        value["forms"][1]["grants"][0]["prefix"] = json!("elsewhere");
        let error = parse_value(&value).expect_err("a form grant cannot widen the session");
        assert_eq!(error.code, ManagedErrorCode::PolicyInvalid);
        assert!(
            error.message.contains("outside the session grants"),
            "{}",
            error.message
        );
    }

    #[test]
    fn test_create_entry_on_a_reference_is_refused() {
        let mut value = policy_value();
        value["forms"][1]["grants"][0]["root"] = json!("reference");
        let error = parse_value(&value).expect_err("a reference is immutable");
        assert_eq!(error.code, ManagedErrorCode::PolicyInvalid);
    }

    #[test]
    fn test_a_writable_grant_overlapping_a_protected_subtree_is_refused() {
        // v1 has no shadow mount, so a workspace containing a protected
        // descendant cannot be exposed writable at all. The launcher must
        // materialize a root without one.
        let mut map = ResourceMap::new();
        map.add(
            RootId::parse("workspace").unwrap(),
            RootSpec {
                kind: RootKind::Workspace,
                protected: vec![RelPath::parse("out/secrets").unwrap()],
            },
        );
        map.add(
            RootId::parse("reference").unwrap(),
            RootSpec {
                kind: RootKind::Reference,
                protected: vec![],
            },
        );
        let error = Policy::parse(valid_policy().as_bytes(), &map)
            .expect_err("a writable grant over a protected subtree must be refused");
        assert_eq!(error.code, ManagedErrorCode::PolicyInvalid);
        assert!(error.message.contains("protected"), "{}", error.message);
    }

    #[test]
    fn test_reserved_operations_are_not_grantable_in_v1() {
        let mut value = policy_value();
        value["session_grants"][0]["ops"][1] = json!("remove_entry");
        let error = parse_value(&value).expect_err("reserved operations must be refused");
        assert_eq!(error.code, ManagedErrorCode::PolicyInvalid);
    }

    #[test]
    fn test_the_digest_ignores_grant_order_and_operation_order() {
        let policy = parse_value(&policy_value()).unwrap();
        let mut reordered = policy_value();
        let grants = reordered["session_grants"].as_array_mut().unwrap();
        grants.reverse();
        for grant in grants.iter_mut() {
            let ops = grant["ops"].as_array_mut().unwrap();
            ops.reverse();
        }
        let other = parse_value(&reordered).unwrap();
        assert_eq!(policy.digest().unwrap(), other.digest().unwrap());
    }

    fn sufficient_report() -> CapabilityReport {
        let mut report = probe(BackendId::Linux);
        report.supported = true;
        report.capabilities = [
            Capability::ReadFile,
            Capability::ReadDir,
            Capability::MakeReg,
            Capability::NoReplacePublish,
            Capability::TruncateDenial,
            Capability::Namespaces,
            Capability::DescriptorBoundary,
            Capability::ResourceLimits,
        ]
        .into_iter()
        .collect();
        report
    }

    #[test]
    fn test_compile_refuses_a_backend_missing_a_required_capability() {
        let policy = parse_value(&policy_value()).unwrap();
        let report = probe(BackendId::Sbpl);
        let error = policy.compile(&report).expect_err("sbpl cannot activate");
        assert_eq!(error.code, ManagedErrorCode::BackendUnsupported);
    }

    #[test]
    fn test_compile_refuses_when_one_capability_is_missing() {
        let policy = parse_value(&policy_value()).unwrap();
        let mut report = sufficient_report();
        report.capabilities.remove(&Capability::NoReplacePublish);
        let error = policy
            .compile(&report)
            .expect_err("the copy form needs publication");
        assert_eq!(error.code, ManagedErrorCode::BackendUnsupported);
        assert!(
            error.message.contains("no_replace_publish"),
            "{}",
            error.message
        );
    }

    #[test]
    fn test_authorize_denies_an_effect_outside_the_grant() {
        let policy = parse_value(&policy_value()).unwrap();
        let compiled = policy.compile(&sufficient_report()).unwrap();

        let allowed = crate::managed::form::Args::parse(
            FormId::CatFile,
            &json!({"path": {"root": "workspace", "path": "notes.txt"}}),
        )
        .unwrap()
        .effects()
        .unwrap();
        assert!(compiled.authorize(FormId::CatFile, &allowed).is_ok());

        let denied = crate::managed::form::Args::parse(
            FormId::CopyToPath,
            &json!({
                "source": {"root": "workspace", "path": "notes.txt"},
                "destination": {"root": "workspace", "path": "elsewhere/x.txt"}
            }),
        )
        .unwrap()
        .effects()
        .unwrap();
        let error = compiled
            .authorize(FormId::CopyToPath, &denied)
            .expect_err("outside the granted prefix");
        assert_eq!(error.code, ManagedErrorCode::ResourceDenied);
        assert_eq!(error.rule_id.as_deref(), Some("copy-out"));
    }

    #[test]
    fn test_an_unlisted_form_is_unsupported() {
        let policy = parse_value(&policy_value()).unwrap();
        let compiled = policy.compile(&sufficient_report()).unwrap();
        let error = compiled
            .authorize(FormId::LsList, &[])
            .expect_err("cli.ls.list has no rule");
        assert_eq!(error.code, ManagedErrorCode::FormUnsupported);
    }
}
