//! Effects, resource objects and the grant vocabulary.
//!
//! An effect is `(operation, object, range)` — the specification's
//! "normalized effect". The split between a file right and a directory-entry
//! right is deliberate: `mv a b` removes the entry `a` from its parent and
//! creates the entry `b` in another, and it does not modify either file's
//! contents. Modelling that as "write `a`" is how a `0444` source gets refused
//! for a move the kernel would allow, and how a `0666` source in an unwritable
//! directory gets allowed for a move it would refuse.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::error::{ManagedError, ManagedErrorCode};
use super::path::{validate_component, RelPath};

/// Maximum bytes in a resource root ID.
const MAX_ROOT_ID_BYTES: usize = 63;

/// A launcher-defined resource root (`workspace`, `reference`, ...).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootId(String);

impl RootId {
    /// Validate a root ID: `[a-z][a-z0-9_-]{0,63}`.
    pub fn parse(raw: &str) -> Result<Self, ManagedError> {
        let mut chars = raw.chars();
        let valid = raw.len() <= MAX_ROOT_ID_BYTES
            && matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if !valid {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                "resource root ID must match [a-z][a-z0-9_-]{0,63}",
            )
            .with_field("root"));
        }
        Ok(Self(raw.to_string()))
    }

    /// The wire spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RootId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A managed operation.
///
/// V1 grants only the first four; the rest are named because the effect table
/// in the policy specification reserves them and a future form must not invent
/// a second spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    InspectMetadata,
    ListDir,
    ReadFile,
    CreateEntry,
    WriteFile,
    TruncateFile,
    RemoveEntry,
    RenameEntry,
    ModifyMetadata,
}

impl Op {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InspectMetadata => "inspect_metadata",
            Self::ListDir => "list_dir",
            Self::ReadFile => "read_file",
            Self::CreateEntry => "create_entry",
            Self::WriteFile => "write_file",
            Self::TruncateFile => "truncate_file",
            Self::RemoveEntry => "remove_entry",
            Self::RenameEntry => "rename_entry",
            Self::ModifyMetadata => "modify_metadata",
        }
    }

    /// Parse one operation from the policy's `ops` array.
    pub fn parse(raw: &str) -> Result<Self, ManagedError> {
        match raw {
            "inspect_metadata" => Ok(Self::InspectMetadata),
            "list_dir" => Ok(Self::ListDir),
            "read_file" => Ok(Self::ReadFile),
            "create_entry" => Ok(Self::CreateEntry),
            "write_file" => Ok(Self::WriteFile),
            "truncate_file" => Ok(Self::TruncateFile),
            "remove_entry" => Ok(Self::RemoveEntry),
            "rename_entry" => Ok(Self::RenameEntry),
            "modify_metadata" => Ok(Self::ModifyMetadata),
            other => Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("unknown operation '{other}'"),
            )
            .with_field("ops")),
        }
    }

    /// The operations a v1 grant may name.
    pub const V1: [Op; 4] = [
        Self::InspectMetadata,
        Self::ListDir,
        Self::ReadFile,
        Self::CreateEntry,
    ];
}

/// What an effect names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Locator {
    /// A regular file at a root-relative path.
    File(RelPath),
    /// A directory at a root-relative path.
    Directory(RelPath),
    /// A directory entry: a parent directory plus one basename.
    Entry { parent: RelPath, basename: String },
}

impl Locator {
    /// The path a grant prefix is matched against.
    ///
    /// For an entry this is the child path, because the policy grants
    /// `create_entry` on the final name and derives the backend right from its
    /// parent.
    pub fn granted_path(&self) -> Result<RelPath, ManagedError> {
        match self {
            Self::File(path) | Self::Directory(path) => Ok(path.clone()),
            Self::Entry { parent, basename } => parent.join(basename),
        }
    }

    /// The kind name used on the wire.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::File(_) => "file",
            Self::Directory(_) => "directory",
            Self::Entry { .. } => "dir_entry",
        }
    }

    /// The locator as the protocol's `effects[].object` object.
    pub fn to_json(&self, root: &RootId) -> Value {
        match self {
            Self::File(path) => {
                json!({"kind": "file", "root": root.as_str(), "path": path.as_slash()})
            }
            Self::Directory(path) => {
                json!({"kind": "directory", "root": root.as_str(), "path": path.as_slash()})
            }
            Self::Entry { parent, basename } => json!({
                "kind": "dir_entry",
                "root": root.as_str(),
                "parent": parent.as_slash(),
                "basename": basename,
            }),
        }
    }
}

/// One resource object: a root plus a locator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub root: RootId,
    pub locator: Locator,
}

impl Object {
    /// Build a file object.
    pub fn file(root: RootId, path: RelPath) -> Self {
        Self {
            root,
            locator: Locator::File(path),
        }
    }

    /// Build a directory object.
    pub fn directory(root: RootId, path: RelPath) -> Self {
        Self {
            root,
            locator: Locator::Directory(path),
        }
    }

    /// Build a directory-entry object.
    pub fn entry(root: RootId, parent: RelPath, basename: impl Into<String>) -> Self {
        Self {
            root,
            locator: Locator::Entry {
                parent,
                basename: basename.into(),
            },
        }
    }
}

/// How much of a tree an effect may touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    Exact,
    ImmediateEntries,
    Subtree,
}

impl Range {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::ImmediateEntries => "immediate_entries",
            Self::Subtree => "subtree",
        }
    }
}

/// One normalized effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effect {
    pub op: Op,
    pub object: Object,
    pub range: Range,
}

impl Effect {
    /// The protocol's `effects[]` element.
    pub fn to_json(&self) -> Value {
        json!({
            "op": self.op.as_str(),
            "object": self.object.locator.to_json(&self.object.root),
            "range": self.range.as_str(),
        })
    }
}

/// A resource grant: a root, a path prefix and the operations it allows there.
///
/// Prefix matching is component-wise, so `etc` never covers `etcetera`. An
/// empty prefix grants on the root itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub root: RootId,
    pub prefix: RelPath,
    pub ops: BTreeSet<Op>,
}

impl Grant {
    /// Whether this grant allows `effect`.
    pub fn covers(&self, effect: &Effect) -> bool {
        if self.root != effect.object.root || !self.ops.contains(&effect.op) {
            return false;
        }
        effect
            .object
            .locator
            .granted_path()
            .is_ok_and(|path| path.starts_with(&self.prefix))
    }

    /// Whether this grant's prefix is an ancestor of (or equal to) `other`.
    pub fn prefix_covers(&self, other: &RelPath) -> bool {
        other.starts_with(&self.prefix)
    }
}

/// Validate an entry basename with the path-component rules.
pub fn validate_basename(name: &str) -> Result<(), ManagedError> {
    validate_component(name, "basename")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> RootId {
        RootId::parse("workspace").unwrap()
    }

    fn grant(prefix: &str, ops: &[Op]) -> Grant {
        Grant {
            root: root(),
            prefix: RelPath::parse(prefix).unwrap(),
            ops: ops.iter().copied().collect(),
        }
    }

    #[test]
    fn test_root_id_validation() {
        assert!(RootId::parse("workspace").is_ok());
        assert!(RootId::parse("work-space_2").is_ok());
        for bad in ["Workspace", "2work", "-work", "", &"x".repeat(64)] {
            assert!(RootId::parse(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn test_grant_matches_whole_components() {
        let etc = grant("etc", &[Op::ReadFile]);
        let file = Effect {
            op: Op::ReadFile,
            object: Object::file(root(), RelPath::parse("etc/hosts").unwrap()),
            range: Range::Exact,
        };
        assert!(etc.covers(&file));
        let other = Effect {
            op: Op::ReadFile,
            object: Object::file(root(), RelPath::parse("etcetera/hosts").unwrap()),
            range: Range::Exact,
        };
        assert!(!etc.covers(&other), "prefix must not match a sibling name");
    }

    #[test]
    fn test_create_entry_matches_the_child_path_not_the_parent() {
        let out = grant("out", &[Op::CreateEntry]);
        let effect = Effect {
            op: Op::CreateEntry,
            object: Object::entry(root(), RelPath::parse("out").unwrap(), "notes.txt"),
            range: Range::Exact,
        };
        assert!(out.covers(&effect));
        let sibling = Effect {
            op: Op::CreateEntry,
            object: Object::entry(root(), RelPath::parse("other").unwrap(), "notes.txt"),
            range: Range::Exact,
        };
        assert!(!out.covers(&sibling));
    }

    #[test]
    fn test_an_operation_not_in_the_grant_is_not_covered() {
        let read_only = grant("", &[Op::ReadFile]);
        let create = Effect {
            op: Op::CreateEntry,
            object: Object::entry(root(), RelPath::root(), "x"),
            range: Range::Exact,
        };
        assert!(!read_only.covers(&create));
    }

    #[test]
    fn test_effect_json_uses_the_specified_shape() {
        let effect = Effect {
            op: Op::CreateEntry,
            object: Object::entry(root(), RelPath::parse("out").unwrap(), "notes.txt"),
            range: Range::Exact,
        };
        assert_eq!(
            effect.to_json(),
            json!({
                "op": "create_entry",
                "object": {
                    "kind": "dir_entry",
                    "root": "workspace",
                    "parent": "out",
                    "basename": "notes.txt"
                },
                "range": "exact"
            })
        );
    }
}
