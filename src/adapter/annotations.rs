use std::collections::HashMap;

use apcore::module::ModuleAnnotations;

use crate::models::ScannedCommand;

const READONLY_PATTERNS: &[&str] = &[
    "list", "ls", "show", "get", "status", "info", "version", "help", "describe", "view", "cat",
    "log", "diff", "search", "find", "check", "inspect", "display", "print", "whoami", "env",
    "top", "ps",
];

const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "delete", "rm", "remove", "destroy", "purge", "drop", "kill", "prune", "clean", "reset",
    "format", "wipe", "erase",
];

const IDEMPOTENT_PATTERNS: &[&str] = &[
    "get", "list", "show", "status", "info", "describe", "version", "help", "check",
];

/// Executables whose whole purpose is to reach another host.
///
/// Matched on the executable rather than the subcommand, because for these there
/// is no local-only invocation to distinguish.
const OPEN_WORLD_TOOLS: &[&str] = &[
    "curl", "wget", "ssh", "scp", "sftp", "rsync", "nc", "netcat", "telnet", "ftp", "http",
    "httpie", "wscat",
];

/// Subcommands that reach the network on an otherwise local tool.
///
/// `git` is local until it is `git push`, and the same split holds for package
/// managers and container tools, so this is matched on the subcommand name.
const OPEN_WORLD_SUBCOMMANDS: &[&str] = &[
    "push",
    "pull",
    "fetch",
    "clone",
    "publish",
    "upload",
    "download",
    "install",
    "uninstall",
    "deploy",
    "sync",
    "login",
    "logout",
];

/// Flags that escalate a command to requires_approval even if the command name
/// is not in DESTRUCTIVE_PATTERNS.
const APPROVAL_FLAGS: &[&str] = &[
    "--force",
    "-f",
    "--hard",
    "--recursive",
    "-r",
    "--all",
    "--prune",
    "--no-preserve-root",
    "--cascade",
    "--purge",
    "--yes",
    "-y",
];

/// Flags that indicate idempotent behavior.
const IDEMPOTENT_FLAGS: &[&str] = &[
    "--dry-run",
    "--check",
    "--diff",
    "--noop",
    "--simulate",
    "--whatif",
    "--plan",
];

/// Whether this command reaches outside the machine.
///
/// Previously hardcoded to `true`, which made the annotation carry no signal at
/// all: `ls` and `curl` were indistinguishable, `risk_from_annotations` had to
/// ignore the field, and `Risk::OpenWorld` was therefore never emitted. A
/// consumer that gates on the boundary — demanding a declared credential, or
/// refusing to execute a networked command in a local sandbox — was left with
/// nothing to gate on.
///
/// Two signals, because networking sits at two different levels: the executable
/// itself (`curl`), or one subcommand of an otherwise local tool (`git push`).
/// Name-based, so it is a floor rather than a guarantee — a tool that opens a
/// socket under an unremarkable name is not caught, and an overlay's
/// `annotation_overrides` remains the way to state the truth for a specific one.
fn infer_open_world(command: &ScannedCommand) -> bool {
    let executable = command
        .full_command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if OPEN_WORLD_TOOLS.iter().any(|tool| executable == *tool) {
        return true;
    }
    let name_lower = command.name.to_lowercase();
    OPEN_WORLD_SUBCOMMANDS.iter().any(|sub| name_lower == *sub)
}

/// Infer behavioral annotations from command name and flags.
pub fn infer(command: &ScannedCommand) -> ModuleAnnotations {
    let name_lower = command.name.to_lowercase();

    let destructive = DESTRUCTIVE_PATTERNS.iter().any(|p| name_lower == *p);
    let readonly = !destructive && READONLY_PATTERNS.iter().any(|p| name_lower == *p);
    let mut idempotent = IDEMPOTENT_PATTERNS.iter().any(|p| name_lower == *p);
    let mut requires_approval = destructive;

    // Flag boosting: check command flags for escalation/idempotent signals
    for flag in &command.flags {
        let flag_name = flag.long_name.as_deref().unwrap_or("");
        let short_name = flag.short_name.as_deref().unwrap_or("");

        if APPROVAL_FLAGS
            .iter()
            .any(|p| flag_name == *p || short_name == *p)
        {
            requires_approval = true;
        }
        if IDEMPOTENT_FLAGS
            .iter()
            .any(|p| flag_name == *p || short_name == *p)
        {
            idempotent = true;
        }
    }

    let cacheable = readonly && idempotent;

    ModuleAnnotations {
        readonly,
        destructive,
        idempotent,
        requires_approval,
        open_world: infer_open_world(command),
        streaming: false,
        cacheable,
        cache_ttl: 0,
        cache_key_fields: None,
        paginated: false,
        pagination_style: "cursor".to_string(),
        discoverable: true,
        extra: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{HelpFormat, StructuredOutputInfo};

    fn make_command_named(name: &str) -> ScannedCommand {
        ScannedCommand {
            name: name.to_string(),
            full_command: format!("tool {name}"),
            description: String::new(),
            flags: vec![],
            positional_args: vec![],
            subcommands: vec![],
            examples: vec![],
            help_format: HelpFormat::Gnu,
            structured_output: StructuredOutputInfo::default(),
            end_of_options: false,
            raw_help: String::new(),
        }
    }

    fn make_command_for(full_command: &str) -> ScannedCommand {
        let name = full_command
            .split_whitespace()
            .last()
            .unwrap_or(full_command);
        ScannedCommand {
            full_command: full_command.to_string(),
            ..make_command_named(name)
        }
    }

    #[test]
    fn test_open_world_is_inferred_not_asserted() {
        // Hardcoding this to `true` made `ls` and `curl` indistinguishable, so
        // nothing downstream could gate on the boundary.
        assert!(!infer(&make_command_for("ls")).open_world);
        assert!(!infer(&make_command_for("cat")).open_world);
        assert!(!infer(&make_command_for("git add")).open_world);
    }

    #[test]
    fn test_open_world_networked_executable() {
        for executable in ["curl", "wget", "ssh", "scp", "rsync"] {
            assert!(
                infer(&make_command_for(executable)).open_world,
                "{executable} reaches another host"
            );
        }
    }

    #[test]
    fn test_open_world_executable_is_matched_by_basename() {
        assert!(infer(&make_command_for("/usr/bin/curl")).open_world);
    }

    #[test]
    fn test_open_world_networked_subcommand_of_a_local_tool() {
        // `git` is local until it is `git push`.
        assert!(infer(&make_command_for("git push")).open_world);
        assert!(infer(&make_command_for("git clone")).open_world);
        assert!(!infer(&make_command_for("git commit")).open_world);
    }

    #[test]
    fn test_annotations_list_is_readonly() {
        let cmd = make_command_named("list");
        let ann = infer(&cmd);
        assert!(ann.readonly);
        assert!(!ann.destructive);
    }

    #[test]
    fn test_annotations_delete_is_destructive() {
        let cmd = make_command_named("delete");
        let ann = infer(&cmd);
        assert!(ann.destructive);
        assert!(ann.requires_approval);
        assert!(!ann.readonly);
    }

    #[test]
    fn test_annotations_create_is_write() {
        let cmd = make_command_named("create");
        let ann = infer(&cmd);
        assert!(!ann.readonly);
        assert!(!ann.destructive);
    }

    #[test]
    fn test_annotations_get_is_idempotent() {
        let cmd = make_command_named("get");
        let ann = infer(&cmd);
        assert!(ann.idempotent);
    }

    #[test]
    fn test_annotations_readonly_is_cacheable() {
        // "status" is both readonly and idempotent.
        let cmd = make_command_named("status");
        let ann = infer(&cmd);
        assert!(ann.readonly);
        assert!(ann.idempotent);
        assert!(ann.cacheable);
    }

    #[test]
    fn test_annotations_unknown_defaults() {
        let cmd = make_command_named("xyzzy");
        let ann = infer(&cmd);
        assert!(!ann.readonly);
        assert!(!ann.destructive);
        assert!(!ann.idempotent);
        assert!(!ann.cacheable);
        assert!(!ann.requires_approval);
    }

    fn make_command_with_flags(name: &str, flags: Vec<(&str, &str)>) -> ScannedCommand {
        use crate::models::{ScannedFlag, ValueType};
        let scanned_flags = flags
            .into_iter()
            .map(|(long, short)| ScannedFlag {
                long_name: if long.is_empty() {
                    None
                } else {
                    Some(long.to_string())
                },
                short_name: if short.is_empty() {
                    None
                } else {
                    Some(short.to_string())
                },
                description: String::new(),
                value_type: ValueType::Boolean,
                required: false,
                default: None,
                enum_values: None,
                repeatable: false,
                value_name: None,
                ..Default::default()
            })
            .collect();
        ScannedCommand {
            name: name.to_string(),
            flags: scanned_flags,
            ..make_command_named(name)
        }
    }

    #[test]
    fn test_annotations_force_flag_requires_approval() {
        let cmd = make_command_with_flags("push", vec![("--force", "-f")]);
        let ann = infer(&cmd);
        assert!(ann.requires_approval);
    }

    #[test]
    fn test_annotations_dry_run_flag_is_idempotent() {
        let cmd = make_command_with_flags("apply", vec![("--dry-run", "")]);
        let ann = infer(&cmd);
        assert!(ann.idempotent);
    }

    #[test]
    fn test_annotations_combined_flags() {
        let cmd = make_command_with_flags("deploy", vec![("--force", ""), ("--dry-run", "")]);
        let ann = infer(&cmd);
        assert!(ann.requires_approval);
        assert!(ann.idempotent);
    }
}
