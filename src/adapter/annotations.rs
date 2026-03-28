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
        open_world: true,
        streaming: false,
        cacheable,
        cache_ttl: 0,
        cache_key_fields: None,
        paginated: false,
        pagination_style: "cursor".to_string(),
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
            raw_help: String::new(),
        }
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
