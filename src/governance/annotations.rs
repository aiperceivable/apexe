use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::binding::binding_gen::GeneratedBinding;

/// Command name patterns that indicate destructive operations.
const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "delete",
    "remove",
    "rm",
    "drop",
    "kill",
    "destroy",
    "purge",
    "wipe",
    "clean",
    "reset",
    "uninstall",
    "truncate",
    "erase",
    "revoke",
];

/// Command name patterns that indicate read-only operations.
const READONLY_PATTERNS: &[&str] = &[
    "list", "show", "status", "info", "get", "cat", "ls", "describe", "inspect", "view", "print",
    "help", "version", "check", "diff", "log", "search", "find", "which", "whoami", "count",
    "stat", "top", "ps", "env", "config",
];

/// Flag names that indicate the command should require approval.
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

/// Flag names that indicate the command is idempotent.
const IDEMPOTENT_FLAGS: &[&str] = &[
    "--dry-run",
    "--check",
    "--diff",
    "--noop",
    "--simulate",
    "--whatif",
    "--plan",
];

/// Behavioral annotations for a module.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModuleAnnotations {
    pub readonly: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub requires_approval: bool,
    pub open_world: bool,
}

/// Annotation inference result with confidence metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferredAnnotations {
    pub annotations: ModuleAnnotations,
    pub confidence: f64,
    pub reasoning: String,
}

/// Infer behavioral annotations from command semantics.
///
/// Checks destructive patterns first, then readonly (if not destructive),
/// then flag-based boosting, then computes confidence.
pub fn infer_annotations(
    command_name: &str,
    _full_command: &str,
    flags: &[String],
    description: &str,
) -> InferredAnnotations {
    let mut reasons: Vec<String> = Vec::new();
    let mut readonly = false;
    let mut destructive = false;
    let mut idempotent = false;
    let mut requires_approval = false;

    let cmd_lower = command_name.to_lowercase();
    let desc_lower = description.to_lowercase();

    // Check destructive patterns
    for &pattern in DESTRUCTIVE_PATTERNS {
        if cmd_lower.contains(pattern) || desc_lower.contains(pattern) {
            destructive = true;
            reasons.push(format!(
                "Command/description contains destructive keyword '{pattern}'"
            ));
            break;
        }
    }

    // Check readonly patterns (only if not destructive)
    if !destructive {
        for &pattern in READONLY_PATTERNS {
            if cmd_lower.contains(pattern) || desc_lower.contains(pattern) {
                readonly = true;
                reasons.push(format!(
                    "Command/description contains readonly keyword '{pattern}'"
                ));
                break;
            }
        }
    }

    // Check for approval-requiring flags
    for flag in flags {
        if APPROVAL_FLAGS.contains(&flag.as_str()) {
            requires_approval = true;
            reasons.push(format!("Command has dangerous flag '{flag}'"));
        }
    }

    // Force approval for destructive commands
    if destructive {
        requires_approval = true;
        reasons.push("Destructive commands require approval by default".to_string());
    }

    // Check for idempotent indicators
    for flag in flags {
        if IDEMPOTENT_FLAGS.contains(&flag.as_str()) {
            idempotent = true;
            reasons.push(format!("Command has idempotent indicator flag '{flag}'"));
        }
    }

    // Calculate confidence
    let confidence = if reasons.is_empty() {
        reasons.push("No strong signals detected, using defaults".to_string());
        0.3
    } else {
        (0.5 + 0.1 * reasons.len() as f64).min(0.95)
    };

    InferredAnnotations {
        annotations: ModuleAnnotations {
            readonly,
            destructive,
            idempotent,
            requires_approval,
            open_world: false,
        },
        confidence,
        reasoning: reasons.join("; "),
    }
}

/// Apply inferred annotations to a list of generated bindings.
///
/// For each binding:
/// 1. Extract command_name from module_id (last segment after "cli.")
/// 2. Extract full_command from metadata
/// 3. Extract flag names from input_schema properties
/// 4. Call infer_annotations()
/// 5. Populate binding.annotations (skip keys already set by user)
/// 6. Store confidence and reasoning in binding.metadata
pub fn annotate_bindings(bindings: &mut [GeneratedBinding]) {
    for binding in bindings.iter_mut() {
        let command_name = binding
            .module_id
            .rsplit('.')
            .next()
            .unwrap_or("unknown")
            .to_string();

        let full_command = binding
            .metadata
            .get("apexe_command")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();

        let flags: Vec<String> = binding
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|props| {
                props
                    .keys()
                    .map(|k| format!("--{}", k.replace('_', "-")))
                    .collect()
            })
            .unwrap_or_default();

        let inferred =
            infer_annotations(&command_name, &full_command, &flags, &binding.description);

        // Only set annotations that are not already user-provided
        if !binding.annotations.contains_key("readonly") {
            binding
                .annotations
                .insert("readonly".to_string(), json!(inferred.annotations.readonly));
        }
        if !binding.annotations.contains_key("destructive") {
            binding.annotations.insert(
                "destructive".to_string(),
                json!(inferred.annotations.destructive),
            );
        }
        if !binding.annotations.contains_key("idempotent") {
            binding.annotations.insert(
                "idempotent".to_string(),
                json!(inferred.annotations.idempotent),
            );
        }
        if !binding.annotations.contains_key("requires_approval") {
            binding.annotations.insert(
                "requires_approval".to_string(),
                json!(inferred.annotations.requires_approval),
            );
        }
        if !binding.annotations.contains_key("open_world") {
            binding
                .annotations
                .insert("open_world".to_string(), json!(false));
        }

        binding.metadata.insert(
            "apexe_annotation_confidence".to_string(),
            json!(inferred.confidence),
        );
        binding.metadata.insert(
            "apexe_annotation_reasoning".to_string(),
            json!(inferred.reasoning),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use serde_json::json;
    use std::collections::HashMap;

    // T1: Destructive pattern detection
    #[rstest]
    #[case("rm", true, true)]
    #[case("delete", true, true)]
    #[case("kill", true, true)]
    #[case("destroy", true, true)]
    #[case("purge", true, true)]
    #[case("wipe", true, true)]
    #[case("erase", true, true)]
    fn test_destructive_detection(
        #[case] command: &str,
        #[case] expect_destructive: bool,
        #[case] expect_approval: bool,
    ) {
        let result = infer_annotations(command, command, &[], "");
        assert_eq!(result.annotations.destructive, expect_destructive);
        assert_eq!(result.annotations.requires_approval, expect_approval);
    }

    #[test]
    fn test_destructive_description_match() {
        let result = infer_annotations("foo", "foo", &[], "permanently deletes files");
        assert!(result.annotations.destructive);
        assert!(result.annotations.requires_approval);
        assert!(result.confidence > 0.5);
    }

    // T2: Readonly pattern detection
    #[rstest]
    #[case("status", true)]
    #[case("ls", true)]
    #[case("get", true)]
    #[case("ps", true)]
    #[case("show", true)]
    #[case("inspect", true)]
    #[case("help", true)]
    fn test_readonly_detection(#[case] command: &str, #[case] expect_readonly: bool) {
        let result = infer_annotations(command, command, &[], "");
        assert_eq!(result.annotations.readonly, expect_readonly);
        assert!(!result.annotations.destructive);
    }

    #[test]
    fn test_readonly_description_match() {
        let result = infer_annotations("foo", "foo", &[], "show current status");
        assert!(result.annotations.readonly);
        assert!(!result.annotations.destructive);
    }

    #[test]
    fn test_destructive_wins_over_readonly() {
        // "remove-status" contains both "remove" (destructive) and "status" (readonly)
        let result = infer_annotations("remove-status", "remove-status", &[], "");
        assert!(result.annotations.destructive);
        assert!(!result.annotations.readonly);
    }

    // T3: Flag-based annotation boosting
    #[test]
    fn test_force_flag_requires_approval() {
        let result = infer_annotations("push", "git push", &["--force".to_string()], "");
        assert!(result.annotations.requires_approval);
    }

    #[test]
    fn test_hard_flag_requires_approval() {
        let result = infer_annotations("reset", "git reset", &["--hard".to_string()], "");
        assert!(result.annotations.requires_approval);
    }

    #[test]
    fn test_dry_run_sets_idempotent() {
        let result = infer_annotations("apply", "kubectl apply", &["--dry-run".to_string()], "");
        assert!(result.annotations.idempotent);
    }

    #[test]
    fn test_combined_flags() {
        let result = infer_annotations(
            "push",
            "git push",
            &["--force".to_string(), "--dry-run".to_string()],
            "",
        );
        assert!(result.annotations.requires_approval);
        assert!(result.annotations.idempotent);
    }

    // T4: Confidence scoring
    #[test]
    fn test_no_signal_confidence() {
        let result = infer_annotations("foo", "foo", &[], "");
        assert!((result.confidence - 0.3).abs() < f64::EPSILON);
        assert!(!result.annotations.readonly);
        assert!(!result.annotations.destructive);
        assert!(result.reasoning.contains("No strong signals"));
    }

    #[test]
    fn test_single_signal_confidence() {
        let result = infer_annotations("status", "git status", &[], "");
        assert!((result.confidence - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn test_multi_signal_confidence() {
        // "delete" + --force -> destructive keyword + approval flag + forced approval = 3 reasons
        let result = infer_annotations("delete", "delete", &["--force".to_string()], "");
        assert!(result.confidence >= 0.7);
    }

    #[test]
    fn test_confidence_never_exceeds_095() {
        // Many flags to push confidence high
        let result = infer_annotations(
            "delete",
            "delete stuff",
            &[
                "--force".to_string(),
                "--recursive".to_string(),
                "--all".to_string(),
                "--yes".to_string(),
                "--dry-run".to_string(),
                "--check".to_string(),
            ],
            "removes and deletes everything",
        );
        assert!(result.confidence <= 0.95);
    }

    #[test]
    fn test_reasoning_always_nonempty() {
        let result = infer_annotations("foo", "foo", &[], "");
        assert!(!result.reasoning.is_empty());
    }

    // T5: annotate_bindings
    #[test]
    fn test_annotate_bindings_readonly() {
        let mut bindings = vec![GeneratedBinding {
            module_id: "cli.git.status".to_string(),
            description: "Show working tree status".to_string(),
            target: String::new(),
            input_schema: json!({}),
            output_schema: json!({}),
            tags: vec![],
            version: "1.0.0".to_string(),
            annotations: HashMap::new(),
            metadata: HashMap::new(),
        }];

        annotate_bindings(&mut bindings);

        assert_eq!(bindings[0].annotations["readonly"], json!(true));
        assert!(bindings[0]
            .metadata
            .contains_key("apexe_annotation_confidence"));
        assert!(bindings[0]
            .metadata
            .contains_key("apexe_annotation_reasoning"));
    }

    #[test]
    fn test_annotate_bindings_destructive() {
        let mut bindings = vec![GeneratedBinding {
            module_id: "cli.docker.rm".to_string(),
            description: "Remove containers".to_string(),
            target: String::new(),
            input_schema: json!({}),
            output_schema: json!({}),
            tags: vec![],
            version: "1.0.0".to_string(),
            annotations: HashMap::new(),
            metadata: HashMap::new(),
        }];

        annotate_bindings(&mut bindings);

        assert_eq!(bindings[0].annotations["destructive"], json!(true));
        assert_eq!(bindings[0].annotations["requires_approval"], json!(true));
    }

    #[test]
    fn test_annotate_bindings_with_force_flag_in_schema() {
        let mut bindings = vec![GeneratedBinding {
            module_id: "cli.git.push".to_string(),
            description: "Update remote refs".to_string(),
            target: String::new(),
            input_schema: json!({
                "properties": {
                    "force": { "type": "boolean" }
                }
            }),
            output_schema: json!({}),
            tags: vec![],
            version: "1.0.0".to_string(),
            annotations: HashMap::new(),
            metadata: HashMap::new(),
        }];

        annotate_bindings(&mut bindings);

        assert_eq!(bindings[0].annotations["requires_approval"], json!(true));
    }

    #[test]
    fn test_annotate_bindings_user_override_preserved() {
        let mut bindings = vec![GeneratedBinding {
            module_id: "cli.docker.rm".to_string(),
            description: "Remove containers".to_string(),
            target: String::new(),
            input_schema: json!({}),
            output_schema: json!({}),
            tags: vec![],
            version: "1.0.0".to_string(),
            annotations: {
                let mut m = HashMap::new();
                m.insert("readonly".to_string(), json!(true)); // user override
                m
            },
            metadata: HashMap::new(),
        }];

        annotate_bindings(&mut bindings);

        // User override should survive
        assert_eq!(bindings[0].annotations["readonly"], json!(true));
    }

    #[test]
    fn test_annotate_bindings_metadata_populated() {
        let mut bindings = vec![GeneratedBinding {
            module_id: "cli.git.commit".to_string(),
            description: "Record changes".to_string(),
            target: String::new(),
            input_schema: json!({}),
            output_schema: json!({}),
            tags: vec![],
            version: "1.0.0".to_string(),
            annotations: HashMap::new(),
            metadata: {
                let mut m = HashMap::new();
                m.insert("apexe_command".to_string(), json!(["git", "commit"]));
                m
            },
        }];

        annotate_bindings(&mut bindings);

        let confidence = bindings[0].metadata["apexe_annotation_confidence"]
            .as_f64()
            .unwrap();
        assert!(confidence >= 0.3);
        assert!(confidence <= 0.95);
        assert!(bindings[0].metadata["apexe_annotation_reasoning"]
            .as_str()
            .is_some());
    }
}
