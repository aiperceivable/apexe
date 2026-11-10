//! The five reviewed v1 forms and their typed operands.
//!
//! A form is the unit of authorization: it fixes the executable, the argument
//! shape and the effect set. Nothing from the caller's argv reaches the worker;
//! a native form is rendered from typed data into a fixed argv, and a builtin
//! form has no argv at all. See
//! `docs/managed-execution/policy.md#managed-forms`.

use std::path::Path;

use serde_json::Value;

use super::effect::{validate_basename, Effect, Object, Op, Range, RootId};
use super::error::{ManagedError, ManagedErrorCode};
use super::path::RelPath;

/// Whether a form requires a human decision before it may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Approval {
    None,
    Required,
}

impl Approval {
    /// Parse the policy's `approval` field.
    pub fn parse(raw: &str) -> Result<Self, ManagedError> {
        match raw {
            "none" => Ok(Self::None),
            "required" => Ok(Self::Required),
            other => Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("unknown approval mode '{other}'"),
            )
            .with_field("approval")),
        }
    }
}

/// The reviewed form IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub enum FormId {
    LsList,
    CatFile,
    HeadLines,
    CopyToPath,
    CopyIntoDir,
}

impl FormId {
    /// The stable wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LsList => "cli.ls.list",
            Self::CatFile => "cli.cat.file",
            Self::HeadLines => "cli.head.lines",
            Self::CopyToPath => "builtin.copy_create.to_path",
            Self::CopyIntoDir => "builtin.copy_create.into_dir",
        }
    }

    /// Parse one form ID.
    pub fn parse(raw: &str) -> Result<Self, ManagedError> {
        match raw {
            "cli.ls.list" => Ok(Self::LsList),
            "cli.cat.file" => Ok(Self::CatFile),
            "cli.head.lines" => Ok(Self::HeadLines),
            "builtin.copy_create.to_path" => Ok(Self::CopyToPath),
            "builtin.copy_create.into_dir" => Ok(Self::CopyIntoDir),
            other => Err(ManagedError::planning(
                ManagedErrorCode::FormUnsupported,
                format!("form '{other}' is not a reviewed v1 form"),
            )
            .with_field("form")),
        }
    }

    /// Every v1 form, in the order the policy specification lists them.
    pub const V1: [FormId; 5] = [
        Self::LsList,
        Self::CatFile,
        Self::HeadLines,
        Self::CopyToPath,
        Self::CopyIntoDir,
    ];

    /// Whether the form is an apexe builtin rather than a native binary.
    pub fn is_builtin(self) -> bool {
        matches!(self, Self::CopyToPath | Self::CopyIntoDir)
    }

    /// The builtin identity recorded in the plan digest, if any.
    pub fn builtin_identity(self) -> Option<&'static str> {
        self.is_builtin().then_some("builtin.copy_create")
    }
}

/// A resource reference in a request: a root ID plus a relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRef {
    pub root: RootId,
    pub path: RelPath,
}

impl ResourceRef {
    /// Parse `{"root":"workspace","path":"a/b"}` with exactly those two fields.
    fn parse(value: &Value, field: &str) -> Result<Self, ManagedError> {
        let object = value.as_object().ok_or_else(|| {
            ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                format!("{field} must be an object"),
            )
            .with_field(field)
        })?;
        if object.len() != 2 || !object.contains_key("root") || !object.contains_key("path") {
            return Err(ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                format!("{field} must carry exactly 'root' and 'path'"),
            )
            .with_field(field));
        }
        let root = object["root"].as_str().ok_or_else(|| {
            ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                format!("{field}.root must be a string"),
            )
            .with_field(field)
        })?;
        let path = object["path"].as_str().ok_or_else(|| {
            ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                format!("{field}.path must be a string"),
            )
            .with_field(field)
        })?;
        Ok(Self {
            root: RootId::parse(root)?,
            path: RelPath::parse(path)?,
        })
    }

    /// The object this reference names.
    fn file_object(&self) -> Object {
        Object::file(self.root.clone(), self.path.clone())
    }

    /// The directory object this reference names.
    fn directory_object(&self) -> Object {
        Object::directory(self.root.clone(), self.path.clone())
    }
}

/// Typed operands for one form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Args {
    LsList {
        path: ResourceRef,
    },
    CatFile {
        path: ResourceRef,
    },
    HeadLines {
        path: ResourceRef,
        lines: u32,
    },
    CopyToPath {
        source: ResourceRef,
        destination: ResourceRef,
    },
    CopyIntoDir {
        source: ResourceRef,
        destination: ResourceRef,
    },
}

/// Maximum `lines` operand accepted by `cli.head.lines`.
pub const MAX_HEAD_LINES: u32 = 1000;

impl Args {
    /// The form these operands belong to.
    pub fn form(&self) -> FormId {
        match self {
            Self::LsList { .. } => FormId::LsList,
            Self::CatFile { .. } => FormId::CatFile,
            Self::HeadLines { .. } => FormId::HeadLines,
            Self::CopyToPath { .. } => FormId::CopyToPath,
            Self::CopyIntoDir { .. } => FormId::CopyIntoDir,
        }
    }

    /// Parse the request's `args` object for `form`.
    ///
    /// The field set is exact: a form accepts the fields it declares and
    /// nothing else, so a caller cannot smuggle a flag through an extra key.
    pub fn parse(form: FormId, value: &Value) -> Result<Self, ManagedError> {
        let object = value.as_object().ok_or_else(|| {
            ManagedError::planning(ManagedErrorCode::InvalidArgument, "args must be an object")
                .with_field("args")
        })?;
        let keys: Vec<&str> = object.keys().map(String::as_str).collect();
        let expect = |wanted: &[&str]| -> Result<(), ManagedError> {
            let mut sorted = keys.clone();
            sorted.sort_unstable();
            let mut wanted = wanted.to_vec();
            wanted.sort_unstable();
            if sorted == wanted {
                Ok(())
            } else {
                Err(ManagedError::planning(
                    ManagedErrorCode::InvalidArgument,
                    format!("{} takes exactly {:?}", form.as_str(), wanted.join(", ")),
                )
                .with_field("args"))
            }
        };
        match form {
            FormId::LsList => {
                expect(&["path"])?;
                Ok(Self::LsList {
                    path: ResourceRef::parse(&object["path"], "path")?,
                })
            }
            FormId::CatFile => {
                expect(&["path"])?;
                Ok(Self::CatFile {
                    path: ResourceRef::parse(&object["path"], "path")?,
                })
            }
            FormId::HeadLines => {
                expect(&["path", "lines"])?;
                let lines = object["lines"].as_u64().ok_or_else(|| {
                    ManagedError::planning(
                        ManagedErrorCode::InvalidArgument,
                        "lines must be an integer",
                    )
                    .with_field("lines")
                })?;
                let lines = u32::try_from(lines).map_err(|_| {
                    ManagedError::planning(
                        ManagedErrorCode::InvalidArgument,
                        "lines is out of range",
                    )
                    .with_field("lines")
                })?;
                if !(1..=MAX_HEAD_LINES).contains(&lines) {
                    return Err(ManagedError::planning(
                        ManagedErrorCode::InvalidArgument,
                        format!("lines must be between 1 and {MAX_HEAD_LINES}"),
                    )
                    .with_field("lines"));
                }
                Ok(Self::HeadLines {
                    path: ResourceRef::parse(&object["path"], "path")?,
                    lines,
                })
            }
            FormId::CopyToPath | FormId::CopyIntoDir => {
                expect(&["source", "destination"])?;
                let source = ResourceRef::parse(&object["source"], "source")?;
                let destination = ResourceRef::parse(&object["destination"], "destination")?;
                if form == FormId::CopyToPath {
                    Ok(Self::CopyToPath {
                        source,
                        destination,
                    })
                } else {
                    Ok(Self::CopyIntoDir {
                        source,
                        destination,
                    })
                }
            }
        }
    }

    /// The effect set this call declares, in a stable order.
    ///
    /// Copy forms always declare `inspect_metadata` + `read_file` on the source
    /// and `inspect_metadata` + `create_entry` on the destination entry. The
    /// destination entry of `into_dir` is derived from the source basename, as
    /// the specification requires.
    pub fn effects(&self) -> Result<Vec<Effect>, ManagedError> {
        let exact = |op, object| Effect {
            op,
            object,
            range: Range::Exact,
        };
        match self {
            Self::LsList { path } => Ok(vec![
                exact(Op::InspectMetadata, path.directory_object()),
                Effect {
                    op: Op::ListDir,
                    object: path.directory_object(),
                    range: Range::ImmediateEntries,
                },
            ]),
            Self::CatFile { path } | Self::HeadLines { path, .. } => Ok(vec![
                exact(Op::InspectMetadata, path.file_object()),
                exact(Op::ReadFile, path.file_object()),
            ]),
            Self::CopyToPath {
                source,
                destination,
            } => {
                let (parent, basename) = destination_entry(destination)?;
                Ok(copy_effects(source, destination, parent, basename))
            }
            Self::CopyIntoDir {
                source,
                destination,
            } => {
                let basename = source.path.basename().ok_or_else(|| {
                    ManagedError::planning(
                        ManagedErrorCode::InvalidArgument,
                        "source must name a file",
                    )
                    .with_field("source")
                })?;
                validate_basename(basename)?;
                let parent = destination.path.clone();
                Ok(copy_effects(
                    source,
                    destination,
                    parent,
                    basename.to_string(),
                ))
            }
        }
    }

    /// The fixed argv for a native form, rendered against the worker view.
    ///
    /// `view` is the launcher-assigned absolute path of the object inside the
    /// worker's view. Builtin forms have no argv; they are identified by
    /// [`FormId::builtin_identity`] in the plan digest instead.
    pub fn fixed_argv(&self, view: &Path) -> Option<Vec<String>> {
        let path = view.to_string_lossy().to_string();
        match self {
            Self::LsList { .. } => Some(vec!["-1".into(), "--".into(), path]),
            Self::CatFile { .. } => Some(vec!["--".into(), path]),
            Self::HeadLines { lines, .. } => {
                Some(vec!["-n".into(), lines.to_string(), "--".into(), path])
            }
            Self::CopyToPath { .. } | Self::CopyIntoDir { .. } => None,
        }
    }
}

/// Split a `to_path` destination into its parent and validated basename.
fn destination_entry(destination: &ResourceRef) -> Result<(RelPath, String), ManagedError> {
    let basename = destination.path.basename().ok_or_else(|| {
        ManagedError::planning(
            ManagedErrorCode::InvalidArgument,
            "destination must name a new entry",
        )
        .with_field("destination")
    })?;
    validate_basename(basename)?;
    Ok((
        destination.path.parent().unwrap_or_else(RelPath::root),
        basename.to_string(),
    ))
}

/// Effects shared by both copy forms.
fn copy_effects(
    source: &ResourceRef,
    destination: &ResourceRef,
    parent: RelPath,
    basename: String,
) -> Vec<Effect> {
    let exact = |op, object| Effect {
        op,
        object,
        range: Range::Exact,
    };
    vec![
        exact(Op::InspectMetadata, source.file_object()),
        exact(Op::ReadFile, source.file_object()),
        exact(Op::InspectMetadata, destination.directory_object()),
        exact(
            Op::CreateEntry,
            Object::entry(destination.root.clone(), parent, basename),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reference(root: &str, path: &str) -> Value {
        json!({"root": root, "path": path})
    }

    #[test]
    fn test_form_ids_round_trip() {
        for form in FormId::V1 {
            assert_eq!(FormId::parse(form.as_str()).unwrap(), form);
        }
        assert!(FormId::parse("cli.rm.recursive").is_err());
    }

    #[test]
    fn test_args_reject_an_extra_field() {
        let error = Args::parse(
            FormId::CatFile,
            &json!({"path": reference("workspace", "a.txt"), "force": true}),
        )
        .expect_err("an extra field must be refused");
        assert_eq!(error.code, ManagedErrorCode::InvalidArgument);
    }

    #[test]
    fn test_head_lines_are_bounded() {
        for bad in [0u64, 1001] {
            let error = Args::parse(
                FormId::HeadLines,
                &json!({"path": reference("workspace", "a.txt"), "lines": bad}),
            )
            .expect_err("out-of-range lines must be refused");
            assert_eq!(error.code, ManagedErrorCode::InvalidArgument);
        }
        assert!(Args::parse(
            FormId::HeadLines,
            &json!({"path": reference("workspace", "a.txt"), "lines": 10})
        )
        .is_ok());
    }

    #[test]
    fn test_into_dir_derives_the_destination_from_the_source_basename() {
        let args = Args::parse(
            FormId::CopyIntoDir,
            &json!({
                "source": reference("workspace", "src/notes.txt"),
                "destination": reference("workspace", "out")
            }),
        )
        .unwrap();
        let effects = args.effects().unwrap();
        assert_eq!(effects.len(), 4);
        assert_eq!(effects[3].op, Op::CreateEntry);
        assert_eq!(
            effects[3].object.locator,
            super::super::effect::Locator::Entry {
                parent: RelPath::parse("out").unwrap(),
                basename: "notes.txt".into()
            }
        );
    }

    #[test]
    fn test_to_path_requires_a_new_entry_name() {
        let error = Args::parse(
            FormId::CopyToPath,
            &json!({
                "source": reference("workspace", "a.txt"),
                "destination": reference("workspace", "")
            }),
        )
        .and_then(|args| args.effects())
        .expect_err("a root destination is not a new entry");
        assert_eq!(error.code, ManagedErrorCode::InvalidArgument);
    }

    #[test]
    fn test_native_forms_render_a_fixed_argv() {
        let args = Args::parse(
            FormId::HeadLines,
            &json!({"path": reference("workspace", "a.txt"), "lines": 5}),
        )
        .unwrap();
        assert_eq!(
            args.fixed_argv(Path::new("/view/a.txt")).unwrap(),
            vec!["-n", "5", "--", "/view/a.txt"]
        );
        let copy = Args::parse(
            FormId::CopyToPath,
            &json!({
                "source": reference("workspace", "a.txt"),
                "destination": reference("workspace", "b.txt")
            }),
        )
        .unwrap();
        assert!(copy.fixed_argv(Path::new("/view/a.txt")).is_none());
        assert_eq!(copy.form().builtin_identity(), Some("builtin.copy_create"));
    }
}
