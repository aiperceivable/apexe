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

/// Executables whose argument list *is* another command to run.
///
/// These are not tools that happen to be dangerous; they are tools whose entire
/// function is to execute something else, which makes every name-based
/// classification downstream of them meaningless. `env` was classified
/// `readonly` — its name is in [`READONLY_PATTERNS`] — and BSD `env` accepts
/// `-S "rm -rf /etc"`, which splits the string into argv and runs it. That
/// combination produced an explicit ACL *allow* on a general-purpose command
/// executor, and the path guard never saw the `/etc` because it lives inside a
/// `string`-typed flag rather than a path-typed one.
///
/// Marking them `destructive` fixes the two decisions that are actually
/// name-driven: the generated ACL denies rather than allows them, and the path
/// guard judges their arguments as a writer's. It does **not** make them safe.
/// Nothing can statically decide what argv a caller-supplied string will become
/// — see `docs/threat-model.md` §5.9, which recommends denying these outright.
///
/// Matched on the executable, like [`OPEN_WORLD_TOOLS`]: for these there is no
/// benign invocation to distinguish, because passing a command is the interface.
const EXEC_WRAPPER_TOOLS: &[&str] = &[
    "env", "xargs", "nice", "ionice", "nohup", "timeout", "chroot", "setsid", "stdbuf", "script",
    "watch", "time", "sudo", "doas", "su",
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

/// Flags whose *presence in a call* escalates it to `requires_approval`, even
/// when the command name is not in [`DESTRUCTIVE_PATTERNS`].
///
/// # These describe an invocation, not a command
///
/// A scan sees the flags a command *accepts*; a call carries the flags a caller
/// *sent*. Escalating on the former marked `git log` — which merely accepts
/// `--all` — as needing human approval on every invocation, while `git push
/// --upload-pack=<cmd>` earned exactly the same verdict. Of the 42 shipped
/// overlays, 32 declare a flag on this list, and the readers among them (`ls`,
/// `grep`, `diff`, `du`, `df`, `tail`, `cut`) stay quiet only because a human
/// wrote `requires_approval: false` into the overlay by hand. Nothing keeps an
/// un-overlaid tool quiet, and approval fatigue is not a cosmetic failure: the
/// documented response to a gate that prompts on every read is to switch the
/// gate off, which lands on the ungoverned default.
///
/// So the list is applied in two halves, neither of them here.
/// [`mark_escalating_params`] names the schema properties carrying these
/// literals and records them on the module's annotations, keeping
/// `requires_approval` true as a *ceiling* — apcore decides whether to run the
/// gate at all from that static flag, and neither `ApprovalHandler` nor its
/// `ExecutionPolicy` hook is given a call's arguments. [`ApprovalGate`] then
/// reads the recorded names against the arguments actually sent and prompts
/// only when one is present. `git log` stops prompting; `git push --force`
/// still does.
///
/// [`mark_escalating_params`]: crate::adapter::converter
/// [`ApprovalGate`]: crate::module::ApprovalGate
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
    let executable = executable_name(command);
    if OPEN_WORLD_TOOLS.iter().any(|tool| executable == *tool) {
        return true;
    }
    let name_lower = command.name.to_lowercase();
    OPEN_WORLD_SUBCOMMANDS.iter().any(|sub| name_lower == *sub)
}

/// The executable a scanned command belongs to, lowercased and unqualified.
fn executable_name(command: &ScannedCommand) -> String {
    command
        .full_command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_lowercase()
}

/// Whether this command's arguments are themselves a command to run.
///
/// See [`EXEC_WRAPPER_TOOLS`] for why this outranks the name patterns.
fn is_exec_wrapper(command: &ScannedCommand) -> bool {
    let executable = executable_name(command);
    EXEC_WRAPPER_TOOLS.iter().any(|tool| executable == *tool)
}

/// Infer behavioral annotations from command name and flags.
pub fn infer(command: &ScannedCommand) -> ModuleAnnotations {
    let name_lower = command.name.to_lowercase();

    // An exec wrapper outranks both name lists. `env` matches READONLY_PATTERNS
    // on its name and can run anything at all, and of the two facts only the
    // second one matters to a policy.
    let exec_wrapper = is_exec_wrapper(command);
    let destructive = exec_wrapper || DESTRUCTIVE_PATTERNS.iter().any(|p| name_lower == *p);
    let readonly = !destructive && READONLY_PATTERNS.iter().any(|p| name_lower == *p);
    let idempotent = IDEMPOTENT_PATTERNS.iter().any(|p| name_lower == *p);

    // No flag boosting here. `infer` sees the flags a command *accepts*, and
    // both annotations a declared flag used to move are statements about a
    // *call*: see [`APPROVAL_FLAGS`] for `requires_approval`, which is now
    // raised one layer up so the gate can weigh it against real arguments.
    //
    // `idempotent` had the same defect with no such remedy. A command that
    // accepts `--dry-run` does not run dry, so boosting on the declaration made
    // `cacheable` true for a mutating command whenever the caller left the flag
    // off — the reader would then be served a cached result for a call that
    // changed something. There is no per-call hook to re-apply it to (nothing
    // consults `idempotent` at invocation time), so the boost is simply gone;
    // an overlay's `annotation_overrides` remains the way to assert it.
    let requires_approval = destructive;
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

/// Annotation `extra` key naming *why* a module is marked `requires_approval`.
///
/// Absent means the mark is unconditional — the command is destructive by name,
/// or an overlay asserted it. [`APPROVAL_BASIS_FLAGS`] means it came from
/// [`APPROVAL_FLAGS`] alone, and the gate may stand down for a call that
/// carries none of the flags named under [`ESCALATING_PARAMS_KEY`].
pub const APPROVAL_BASIS_KEY: &str = "x-apexe-approval-basis";

/// The [`APPROVAL_BASIS_KEY`] value for a mark derived from accepted flags.
pub const APPROVAL_BASIS_FLAGS: &str = "flags";

/// Annotation `extra` key holding the escalating schema property names.
///
/// Written together with [`APPROVAL_BASIS_KEY`]; a `flags` basis without this
/// list is a malformed annotation, not an empty one, and the gate treats it as
/// a reason to prompt rather than to stand down.
pub const ESCALATING_PARAMS_KEY: &str = "x-apexe-escalating-params";

/// Whether a flag literal escalates the call that carries it.
///
/// Takes the literal (`--force`, `-f`) rather than a [`ScannedFlag`] so the
/// converter can ask about the `x-apexe-flag` value it already wrote into the
/// built schema — the only spelling guaranteed to sit on the property name a
/// caller will actually send.
///
/// [`ScannedFlag`]: crate::models::ScannedFlag
pub fn flag_literal_escalates(literal: &str) -> bool {
    APPROVAL_FLAGS.contains(&literal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{HelpFormat, StructuredOutputInfo};

    /// Build a root command for `executable`, the way a scan of that binary
    /// produces one (`full_command` is what `is_exec_wrapper` reads).
    fn make_root_command(executable: &str) -> ScannedCommand {
        let mut command = make_command_named(executable);
        command.full_command = executable.to_string();
        command
    }

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

    /// Accepting `--force` is not sending it. The escalation now lives one
    /// layer up (`converter::mark_escalating_params`) so the gate can weigh it
    /// against the arguments a call actually carries; raising it here made
    /// `git log`, which merely accepts `--all`, prompt on every invocation.
    #[test]
    fn test_infer_does_not_escalate_on_a_merely_accepted_approval_flag() {
        let cmd = make_command_with_flags("push", vec![("--force", "-f")]);
        let ann = infer(&cmd);
        assert!(
            !ann.requires_approval,
            "an accepted flag is a property of the command, not of a call"
        );
    }

    /// The reader that motivated the change: `log` accepts `--all`, and under
    /// declaration-level boosting that alone demanded human approval.
    #[test]
    fn test_infer_leaves_a_reader_unescalated_despite_accepting_all() {
        let cmd = make_command_with_flags("log", vec![("--all", "-a")]);
        let ann = infer(&cmd);
        assert!(!ann.requires_approval);
        assert!(ann.readonly);
    }

    /// A command that *accepts* `--dry-run` does not run dry. Boosting
    /// `idempotent` on the declaration made `cacheable` true for a mutating
    /// command whenever the caller left the flag off.
    #[test]
    fn test_infer_does_not_mark_idempotent_from_an_accepted_dry_run_flag() {
        let cmd = make_command_with_flags("apply", vec![("--dry-run", "")]);
        let ann = infer(&cmd);
        assert!(!ann.idempotent);
        assert!(!ann.cacheable);
    }

    /// Destructive-by-name still escalates: no absent flag makes `delete` safe.
    #[test]
    fn test_infer_still_escalates_a_destructive_name_without_any_flag() {
        let cmd = make_command_with_flags("delete", vec![]);
        let ann = infer(&cmd);
        assert!(ann.destructive);
        assert!(ann.requires_approval);
    }

    #[test]
    fn test_flag_literal_escalates_matches_long_and_short_forms() {
        assert!(flag_literal_escalates("--force"));
        assert!(flag_literal_escalates("-f"));
        assert!(flag_literal_escalates("--all"));
        assert!(!flag_literal_escalates("--verbose"));
        assert!(!flag_literal_escalates(""));
    }

    /// Regression: `env` matched `READONLY_PATTERNS` on its name, so the
    /// generated ACL gave a general-purpose command executor an explicit
    /// *allow*, and the path guard judged its arguments as a reader's. BSD
    /// `env -S "rm -rf /etc"` splits that string into argv and runs it.
    #[test]
    fn test_infer_classifies_env_as_destructive_not_readonly() {
        let annotations = infer(&make_root_command("env"));

        assert!(!annotations.readonly, "env executes arbitrary commands");
        assert!(annotations.destructive);
        assert!(annotations.requires_approval);
        assert!(
            !annotations.cacheable,
            "a command executor's output is not cacheable"
        );
    }

    #[test]
    fn test_infer_classifies_every_exec_wrapper_as_destructive() {
        for tool in EXEC_WRAPPER_TOOLS {
            let annotations = infer(&make_root_command(tool));
            assert!(
                annotations.destructive && !annotations.readonly,
                "{tool} runs whatever it is handed and must not read as safe"
            );
        }
    }

    /// The rule keys on the executable, not on a name appearing anywhere. A
    /// subcommand that happens to share a wrapper's name — `foo watch` — is not
    /// a command executor, and escalating it would be a false positive with no
    /// bound on how many tools it hits.
    #[test]
    fn test_infer_does_not_escalate_a_subcommand_named_like_a_wrapper() {
        let mut command = make_command_named("watch");
        command.full_command = "kubectl watch".to_string();

        assert!(!infer(&command).destructive);
    }

    /// The escalation must not disturb an ordinary readonly classification.
    #[test]
    fn test_infer_still_classifies_an_ordinary_reader_as_readonly() {
        assert!(infer(&make_root_command("cat")).readonly);
        assert!(infer(&make_root_command("ls")).readonly);
    }
}
