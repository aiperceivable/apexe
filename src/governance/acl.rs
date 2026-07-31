use std::path::Path;

use apcore::{match_pattern, ACLRule, ErrorCode, ModuleError, ACL};
use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};

/// Serializable representation of an ACL config file (rules + default_effect).
#[derive(Debug, Serialize, Deserialize)]
struct AclConfig {
    rules: Vec<ACLRule>,
    default_effect: String,
}

/// Manages access control for CLI modules using apcore's ACL system.
///
/// # Rule ordering is first-match-wins, not most-specific-wins
///
/// apcore's `ACL::check` walks the rule list in order and returns the effect of
/// the *first* rule whose `callers` and `targets` both match; it never scores
/// rules by specificity. So
///
/// ```yaml
/// rules:
///   - { callers: ["*"], targets: ["cli.*"], effect: allow }
///   - { callers: ["*"], targets: ["cli.cp"], effect: deny }   # never reached
/// ```
///
/// lets `cli.cp` through — the broad `allow` wins because it comes first. This
/// is the opposite of the "most specific rule wins" convention that firewall-
/// and IAM-style rule lists usually follow, and it is deliberate on apcore's
/// side (documented as "first-match-wins evaluation"), so apexe does not
/// reorder or re-rank the rules it loads. Write exceptions *above* the broad
/// rule they carve out of.
pub struct AclManager {
    acl: ACL,
    /// Cached default_effect so we can serialize without needing an accessor on ACL.
    default_effect: String,
}

impl AclManager {
    /// Load ACL from a YAML config file.
    #[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
    pub fn from_config(config_path: &Path) -> Result<Self, ModuleError> {
        let acl = ACL::load(&config_path.to_string_lossy()).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to load ACL: {e}"),
            )
        })?;
        // Re-read the file to extract default_effect since ACL has no public accessor.
        let default_effect = Self::read_default_effect(config_path);
        Ok(Self {
            acl,
            default_effect,
        })
    }

    /// Generate default ACL from scanned modules based on annotations.
    pub fn generate_default(modules: &[ScannedModule]) -> Self {
        let mut rules = Vec::new();

        // Readonly modules -> allow
        let readonly_ids: Vec<String> = modules
            .iter()
            .filter(|m| m.annotations.as_ref().is_some_and(|a| a.readonly))
            .map(|m| m.module_id.clone())
            .collect();
        if !readonly_ids.is_empty() {
            rules.push(ACLRule {
                callers: vec!["*".to_string()],
                targets: readonly_ids,
                effect: "allow".to_string(),
                description: Some("Auto-allow readonly CLI commands".to_string()),
                conditions: None,
            });
        }

        // Destructive modules -> unconditional deny. `require_approval` is not
        // a registered apcore ACL condition key (only `identity_types`,
        // `roles`, `max_call_depth`, `$or`, `$not` are); a rule with an
        // unregistered condition key can never match (apcore treats an
        // unknown condition as unsatisfied), so it must not be used here —
        // it would silently fall through to whatever rule/default_effect
        // follows instead of denying. Actual approval-gating for destructive
        // commands happens via the Executor's ApprovalHandler
        // (see `crate::module::build_executor`), not the ACL layer.
        let destructive_ids: Vec<String> = modules
            .iter()
            .filter(|m| m.annotations.as_ref().is_some_and(|a| a.destructive))
            .map(|m| m.module_id.clone())
            .collect();
        if !destructive_ids.is_empty() {
            rules.push(ACLRule {
                callers: vec!["*".to_string()],
                targets: destructive_ids,
                effect: "deny".to_string(),
                description: Some("Block destructive CLI commands by default".to_string()),
                conditions: None,
            });
        }

        let acl = ACL::new(rules, "deny", None);
        Self {
            acl,
            default_effect: "deny".to_string(),
        }
    }

    /// Write ACL to a YAML file.
    #[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
    pub fn write_config(&self, path: &Path) -> Result<(), ModuleError> {
        let config = AclConfig {
            rules: self.acl.rules().to_vec(),
            default_effect: self.default_effect.clone(),
        };
        let yaml = serde_yaml::to_string(&config).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to serialize ACL: {e}"),
            )
        })?;
        std::fs::write(path, yaml).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to write ACL file: {e}"),
            )
        })?;
        Ok(())
    }

    /// Consume the manager and return the inner ACL.
    pub fn into_inner(self) -> ACL {
        self.acl
    }

    /// Read `default_effect` from a YAML file (best-effort, falls back to "deny").
    fn read_default_effect(path: &Path) -> String {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_yaml::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("default_effect")?.as_str().map(String::from))
            .unwrap_or_else(|| "deny".to_string())
    }
}

/// Compound-operator sentinels apcore recognises as the FIRST element of a
/// `callers`/`targets` list. `$or` consumes every following pattern; `$not`
/// consumes exactly one.
const OR_SENTINEL: &str = "$or";
const NOT_SENTINEL: &str = "$not";

/// Maximum number of near-miss module ids named in one diagnostic.
const MAX_SUGGESTIONS: usize = 3;

/// A `callers` or `targets` list apcore's matcher can never satisfy, so the
/// rule carrying it is dead weight no matter which modules are registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InertRule {
    /// Index of the rule in the ACL file's `rules` list.
    pub rule_index: usize,
    /// Which list is inert: `"callers"` or `"targets"`.
    pub field: String,
    /// The rule's declared effect, so the diagnostic can say what was lost.
    pub effect: String,
    /// Why apcore's matcher can never satisfy this list.
    pub reason: String,
}

/// A target pattern that matches none of the module ids actually registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmatchedTarget {
    /// Index of the rule in the ACL file's `rules` list.
    pub rule_index: usize,
    /// The rule's declared effect.
    pub effect: String,
    /// The literal pattern as written in the ACL file.
    pub pattern: String,
    /// `true` when `pattern` is the operand of a `$not`, which inverts what
    /// "matches nothing" means for the rule — see [`TargetPattern`].
    pub negated: bool,
    /// Registered module ids that differ from `pattern` only in separator,
    /// case, or leading segments — i.e. the spelling the operator probably
    /// meant. Empty when nothing close is registered.
    pub suggestions: Vec<String>,
}

impl UnmatchedTarget {
    /// Whether a registered module is a near-identical spelling of this
    /// pattern. A near miss proves the module *is* present under a different
    /// spelling, which makes the pattern a typo rather than a forward-looking
    /// rule for a module that does not exist here yet.
    pub fn is_near_miss(&self) -> bool {
        !self.suggestions.is_empty()
    }
}

/// Outcome of checking a loaded ACL against the registry it will guard.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclValidationReport {
    /// Rules whose caller or target list can never match anything.
    pub inert_rules: Vec<InertRule>,
    /// Target patterns that match no registered module id.
    pub unmatched_targets: Vec<UnmatchedTarget>,
}

impl AclValidationReport {
    /// Whether the findings warrant refusing to start. See
    /// [`validate_acl_rules`] for the refuse-vs-warn split.
    pub fn is_fatal(&self) -> bool {
        !self.inert_rules.is_empty()
            || self
                .unmatched_targets
                .iter()
                .any(UnmatchedTarget::is_near_miss)
    }

    /// Log the non-fatal findings — target patterns that match nothing today
    /// but are not typos of anything registered.
    pub fn emit_warnings(&self) {
        for target in self.unmatched_targets.iter().filter(|t| !t.is_near_miss()) {
            if target.negated {
                tracing::warn!(
                    rule_index = target.rule_index,
                    effect = %target.effect,
                    pattern = %target.pattern,
                    "ACL `$not` operand matches no registered module, so apcore matches \
                     this rule against EVERY module rather than none. Under \
                     first-match-wins an `allow` here nullifies every rule below it. \
                     Harmless if the operand anticipates a module this server does not \
                     serve (a glob, or an ACL shared with a differently filtered \
                     server); a typo otherwise."
                );
            } else {
                tracing::warn!(
                    rule_index = target.rule_index,
                    effect = %target.effect,
                    pattern = %target.pattern,
                    "ACL target matches no registered module — this rule currently \
                     protects nothing. Harmless if the pattern anticipates a module \
                     this server does not serve (a glob, or an ACL shared with a \
                     differently filtered server); a typo otherwise."
                );
            }
        }
    }

    /// Build the refusal error for the fatal findings, or `None` when there
    /// are none.
    pub fn fatal_error(&self, acl_path: &Path) -> Option<ModuleError> {
        if !self.is_fatal() {
            return None;
        }
        let mut lines: Vec<String> = self.inert_rules.iter().map(describe_inert).collect();
        lines.extend(
            self.unmatched_targets
                .iter()
                .filter(|t| t.is_near_miss())
                .map(describe_near_miss),
        );
        Some(
            ModuleError::new(
                ErrorCode::GeneralInvalidInput,
                format!(
                    "ACL '{}' contains {} rule(s) that protect nothing:\n{}\n\
                     Refusing to start: an operator who asked for access control must not \
                     silently get less of it than they wrote.",
                    acl_path.display(),
                    lines.len(),
                    lines.join("\n")
                ),
            )
            .with_retryable(false),
        )
    }
}

/// One human-readable line describing an inert rule.
fn describe_inert(rule: &InertRule) -> String {
    format!(
        "  - rule {} ({}): `{}` is {}",
        rule.rule_index, rule.effect, rule.field, rule.reason
    )
}

/// One human-readable line describing a target that is a typo of a registered
/// module id.
fn describe_near_miss(target: &UnmatchedTarget) -> String {
    if target.negated {
        return format!(
            "  - rule {} ({}): `$not` operand '{}' matches no registered module, so this rule \
             applies to every module instead of excluding one; did you mean {}?",
            target.rule_index,
            target.effect,
            target.pattern,
            target.suggestions.join(" or ")
        );
    }
    format!(
        "  - rule {} ({}): target '{}' matches no registered module; did you mean {}?",
        target.rule_index,
        target.effect,
        target.pattern,
        target.suggestions.join(" or ")
    )
}

/// Check a loaded ACL's rules against the module ids that are actually
/// registered on this server.
///
/// # Why this cannot live in [`AclManager::from_config`]
///
/// The ACL is loaded before anything knows which modules exist.
/// `crate::module::build_executor` populates the `Registry` first and
/// constructs the ACL second, so it is the only place where both sets are in
/// hand — and the ids must come from the registry rather than from the
/// modules directory, because `ModuleFilter` deliberately drops modules at
/// registration time.
///
/// # Refuse vs. warn
///
/// **Refuse** on a structurally inert rule (`targets: []`, `callers: []`, a
/// bare `$not`/`$or`). apcore's `match_patterns` returns `false` for an empty
/// pattern list, so such a rule can never fire against *any* registry, present
/// or future. apcore already rejects an *omitted* `callers`/`targets` key for
/// exactly this reason; accepting the empty list leaves the same hole one
/// character away. This is the posture `--acl` already takes on a missing or
/// malformed file.
///
/// **Refuse** on a target that matches nothing while a registered module id
/// differs from it only in separator, case, or leading segments
/// (`cli.git.cat-file` vs. the registered `cli.git.cat_file`; `cp` vs.
/// `cli.cp`). The near miss is proof the module is present under another
/// spelling, so the rule is a typo whose subject is live and unguarded.
///
/// **Warn** on any other target that matches nothing. A glob is forward-
/// looking by design (`cli.*.status` may match a module tomorrow's scan adds),
/// and one ACL file is meant to be shared across servers whose registries
/// differ (`--prefix`, `--tags`, different scans). A target naming a module
/// that is not registered also cannot itself let anything through: the
/// registry has nothing to serve under that name. Refusing here would turn a
/// portable policy file into a per-host one, which is a worse failure than the
/// warning.
///
/// The target check is skipped entirely when the registry is empty — there is
/// nothing to validate against, and every pattern would be reported.
pub fn validate_acl_rules(rules: &[ACLRule], registered_ids: &[String]) -> AclValidationReport {
    let mut report = AclValidationReport::default();
    for (rule_index, rule) in rules.iter().enumerate() {
        collect_inert(rule_index, rule, &mut report);
        if registered_ids.is_empty() {
            continue;
        }
        collect_unmatched_targets(rule_index, rule, registered_ids, &mut report);
    }
    report
}

/// Record the rule's `callers`/`targets` lists that apcore can never satisfy.
fn collect_inert(rule_index: usize, rule: &ACLRule, report: &mut AclValidationReport) {
    for (field, patterns) in [("callers", &rule.callers), ("targets", &rule.targets)] {
        if let Some(reason) = never_matches(patterns) {
            report.inert_rules.push(InertRule {
                rule_index,
                field: field.to_string(),
                effect: rule.effect.clone(),
                reason,
            });
        }
    }
}

/// Record the rule's target patterns that match no registered module id.
fn collect_unmatched_targets(
    rule_index: usize,
    rule: &ACLRule,
    registered_ids: &[String],
    report: &mut AclValidationReport,
) {
    for target in target_patterns(&rule.targets) {
        if registered_ids
            .iter()
            .any(|id| match_pattern(target.pattern, id))
        {
            continue;
        }
        report.unmatched_targets.push(UnmatchedTarget {
            rule_index,
            effect: rule.effect.clone(),
            pattern: target.pattern.to_string(),
            negated: target.negated,
            suggestions: suggest_similar(target.pattern, registered_ids),
        });
    }
}

/// Why apcore's `match_patterns` can never return `true` for this list, or
/// `None` when it can.
fn never_matches(patterns: &[String]) -> Option<String> {
    if patterns.is_empty() {
        return Some(
            "an empty list — apcore's matcher returns `false` for an empty pattern list, so \
             this rule can never fire"
                .to_string(),
        );
    }
    match patterns[0].as_str() {
        NOT_SENTINEL if patterns.len() < 2 => {
            Some("`$not` with no operand, which apcore's matcher rejects outright".to_string())
        }
        OR_SENTINEL if patterns.len() < 2 => {
            Some("`$or` with no operands, so there is nothing to match".to_string())
        }
        _ => None,
    }
}

/// One entry of a `targets` list, with apcore's `$not` sentinel resolved into
/// the polarity it gives the pattern.
///
/// The polarity is not cosmetic: it inverts what "this pattern matches nothing
/// registered" *means*. Positively, the rule fires for no module and protects
/// nothing. Under `$not`, apcore evaluates `!match(operand, module_id)`, so an
/// operand matching nothing makes the rule fire for **every** module — and
/// under first-match-wins a blanket `allow` there nullifies every deny below
/// it. Reporting the two the same way told the operator the opposite of what
/// was happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TargetPattern<'a> {
    /// The module-id pattern, sentinel stripped.
    pattern: &'a str,
    /// `true` when `pattern` is the operand of a leading `$not`.
    negated: bool,
}

/// The concrete module-id patterns in a `targets` list, with apcore's compound
/// sentinels stripped.
///
/// Mirrors `ACL::match_patterns`: a leading `$or` makes every following entry a
/// pattern, and a leading `$not` consumes exactly *one*. Anything after a
/// `$not` operand is never consulted by the matcher, so it is not reported —
/// flagging it would be a false alarm about a pattern that has no effect either
/// way.
fn target_patterns(targets: &[String]) -> Vec<TargetPattern<'_>> {
    fn positive(pattern: &str) -> TargetPattern<'_> {
        TargetPattern {
            pattern,
            negated: false,
        }
    }
    match targets.first().map(String::as_str) {
        Some(OR_SENTINEL) => targets[1..]
            .iter()
            .map(String::as_str)
            .map(positive)
            .collect(),
        Some(NOT_SENTINEL) => targets
            .get(1)
            .map(|pattern| TargetPattern {
                pattern: pattern.as_str(),
                negated: true,
            })
            .into_iter()
            .collect(),
        _ => targets.iter().map(String::as_str).map(positive).collect(),
    }
}

/// Registered ids that differ from `pattern` only in separator, case, or
/// leading segments.
///
/// Deliberately not a general edit distance: the spellings operators actually
/// reach for are the pre-#19 hyphenated id (`cli.git.cat-file`), the bare
/// command (`cp`), and the shell invocation (`git log`). Normalising every
/// separator to `.` and testing for equality or a trailing-segment match names
/// exactly those without inventing matches between unrelated modules.
fn suggest_similar(pattern: &str, registered_ids: &[String]) -> Vec<String> {
    let needle = normalize_id(pattern);
    if needle.is_empty() || needle.contains('*') {
        return vec![];
    }
    let suffix = format!(".{needle}");
    let mut hits: Vec<String> = registered_ids
        .iter()
        .filter(|id| {
            let normalized = normalize_id(id);
            normalized == needle || normalized.ends_with(&suffix)
        })
        .cloned()
        .collect();
    hits.truncate(MAX_SUGGESTIONS);
    hits
}

/// Fold the spellings that differ only in separator or case into one form.
fn normalize_id(id: &str) -> String {
    id.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c == '-' || c == '_' || c == '/' || c.is_whitespace() {
                '.'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_module_with_annotations(id: &str, readonly: bool, destructive: bool) -> ScannedModule {
        let mut module = ScannedModule::new(
            id.to_string(),
            format!("Test {id}"),
            json!({"type": "object"}),
            json!({"type": "object"}),
            vec!["cli".to_string()],
            format!("exec:///usr/bin/test {id}"),
        );
        module.annotations = Some(apcore::module::ModuleAnnotations {
            readonly,
            destructive,
            requires_approval: destructive,
            ..Default::default()
        });
        module
    }

    #[test]
    fn test_acl_generate_default_readonly() {
        let modules = vec![
            make_module_with_annotations("cli.git.status", true, false),
            make_module_with_annotations("cli.git.log", true, false),
        ];
        let mgr = AclManager::generate_default(&modules);
        let rules = mgr.acl.rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].effect, "allow");
        assert_eq!(rules[0].targets.len(), 2);
    }

    #[test]
    fn test_acl_generate_default_destructive() {
        let modules = vec![make_module_with_annotations("cli.git.clean", false, true)];
        let mgr = AclManager::generate_default(&modules);
        let rules = mgr.acl.rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].effect, "deny");
        // Must be an unconditional deny: apcore only registers `identity_types`,
        // `roles`, `max_call_depth`, `$or`, `$not` as condition keys, so any
        // other key (e.g. `require_approval`) can never match and would let
        // the rule silently fall through instead of denying.
        assert!(rules[0].conditions.is_none());
    }

    #[test]
    fn test_acl_generate_default_destructive_rule_denies_on_its_own() {
        // Regression for the CRITICAL finding: the destructive-deny rule
        // previously carried `conditions: Some({"require_approval": true})`,
        // a condition key apcore never registers. An unregistered condition
        // key can never be satisfied, so the rule could never match — it
        // relied entirely on `default_effect: "deny"` for protection. Prove
        // the rule denies on its own merits by appending a more permissive
        // rule after it (lower priority) and confirming the deny still wins,
        // which only happens if the deny rule actually matches.
        let modules = vec![make_module_with_annotations("cli.git.clean", false, true)];
        let mgr = AclManager::generate_default(&modules);
        let mut rules = mgr.acl.rules().to_vec();
        rules.push(ACLRule {
            callers: vec!["*".to_string()],
            targets: vec!["cli.git.clean".to_string()],
            effect: "allow".to_string(),
            description: None,
            conditions: None,
        });
        let acl = ACL::new(rules, "allow", None);
        assert!(
            !acl.check(None, "cli.git.clean", None),
            "destructive command must be denied by its own ACL rule, not merely \
             by a coincidental default_effect"
        );
    }

    #[test]
    fn test_acl_generate_default_mixed() {
        let modules = vec![
            make_module_with_annotations("cli.git.status", true, false),
            make_module_with_annotations("cli.git.clean", false, true),
        ];
        let mgr = AclManager::generate_default(&modules);
        let rules = mgr.acl.rules();
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn test_acl_generate_default_empty() {
        let mgr = AclManager::generate_default(&[]);
        let rules = mgr.acl.rules();
        assert!(rules.is_empty());
        assert_eq!(mgr.default_effect, "deny");
    }

    #[test]
    fn test_acl_write_and_load() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("acl_manager.yaml");

        let modules = vec![
            make_module_with_annotations("cli.git.status", true, false),
            make_module_with_annotations("cli.git.clean", false, true),
        ];
        let mgr = AclManager::generate_default(&modules);
        mgr.write_config(&path).unwrap();

        let loaded = AclManager::from_config(&path).unwrap();
        assert_eq!(loaded.acl.rules().len(), 2);
        assert_eq!(loaded.default_effect, "deny");
    }

    fn rule(targets: &[&str], effect: &str) -> ACLRule {
        ACLRule {
            callers: vec!["*".to_string()],
            targets: targets.iter().map(|t| (*t).to_string()).collect(),
            effect: effect.to_string(),
            description: None,
            conditions: None,
        }
    }

    fn registered(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn test_validate_acl_rules_accepts_exact_registered_target() {
        let report = validate_acl_rules(
            &[rule(&["cli.cp"], "deny")],
            &registered(&["cli.cp", "cli.ls"]),
        );
        assert_eq!(report, AclValidationReport::default());
        assert!(!report.is_fatal());
    }

    #[test]
    fn test_validate_acl_rules_flags_empty_target_list_as_inert() {
        // #39 item 5(b): apcore rejects an OMITTED `targets` key but accepts
        // `targets: []`, and its matcher returns false for an empty pattern
        // list — so the deny rule never fires and the module runs.
        let report = validate_acl_rules(&[rule(&[], "deny")], &registered(&["cli.cp"]));
        assert_eq!(report.inert_rules.len(), 1);
        assert_eq!(report.inert_rules[0].field, "targets");
        assert_eq!(report.inert_rules[0].effect, "deny");
        assert!(report.is_fatal());
    }

    #[test]
    fn test_validate_acl_rules_flags_empty_caller_list_as_inert() {
        // `callers` goes through the same `match_patterns`, so an empty caller
        // list is inert for exactly the same reason.
        let mut inert = rule(&["cli.cp"], "deny");
        inert.callers = vec![];
        let report = validate_acl_rules(&[inert], &registered(&["cli.cp"]));
        assert_eq!(report.inert_rules.len(), 1);
        assert_eq!(report.inert_rules[0].field, "callers");
    }

    #[test]
    fn test_validate_acl_rules_flags_bare_compound_sentinels_as_inert() {
        let report = validate_acl_rules(
            &[rule(&["$not"], "deny"), rule(&["$or"], "allow")],
            &registered(&["cli.cp"]),
        );
        assert_eq!(report.inert_rules.len(), 2);
        assert!(report.inert_rules[0].reason.contains("$not"));
        assert!(report.inert_rules[1].reason.contains("$or"));
    }

    #[test]
    fn test_validate_acl_rules_flags_hyphenated_id_as_near_miss() {
        // #39 item 5(a): the pre-#19 hyphenated id. The module is registered
        // under `cli.git.cat_file`, so this deny protects nothing.
        let report = validate_acl_rules(
            &[rule(&["cli.git.cat-file"], "deny")],
            &registered(&["cli.git.cat_file"]),
        );
        assert_eq!(report.unmatched_targets.len(), 1);
        assert_eq!(
            report.unmatched_targets[0].suggestions,
            vec!["cli.git.cat_file".to_string()]
        );
        assert!(report.unmatched_targets[0].is_near_miss());
        assert!(report.is_fatal());
    }

    #[test]
    fn test_validate_acl_rules_flags_bare_command_names_as_near_miss() {
        // The other three spellings from #39 item 5(a): `cp`, `ls`, `git log`.
        for (pattern, expected) in [
            ("cp", "cli.cp"),
            ("ls", "cli.ls"),
            ("git log", "cli.git.log"),
        ] {
            let report = validate_acl_rules(
                &[rule(&[pattern], "deny")],
                &registered(&["cli.cp", "cli.ls", "cli.git.log"]),
            );
            assert_eq!(
                report.unmatched_targets[0].suggestions,
                vec![expected.to_string()],
                "'{pattern}' should point at '{expected}'"
            );
            assert!(report.is_fatal(), "'{pattern}' should refuse to start");
        }
    }

    #[test]
    fn test_validate_acl_rules_warns_but_does_not_refuse_on_unknown_target() {
        // A target naming nothing close to a registered module is the shared-
        // ACL / not-yet-scanned case: report it, but do not refuse — the
        // registry has nothing to serve under that name anyway.
        let report = validate_acl_rules(
            &[rule(&["cli.kubectl.apply"], "deny")],
            &registered(&["cli.cp", "cli.ls"]),
        );
        assert_eq!(report.unmatched_targets.len(), 1);
        assert!(report.unmatched_targets[0].suggestions.is_empty());
        assert!(!report.is_fatal());
    }

    #[test]
    fn test_validate_acl_rules_accepts_working_globs() {
        // `cli.c*`, `*.log` and `cli.*.status` are verified working in #39 and
        // must not produce a finding — a validator that cries wolf on a
        // functioning rule is worse than no validator.
        let report = validate_acl_rules(
            &[
                rule(&["cli.c*"], "deny"),
                rule(&["*.log"], "allow"),
                rule(&["cli.*.status"], "allow"),
                rule(&["*"], "deny"),
            ],
            &registered(&["cli.cp", "cli.git.log", "cli.git.status"]),
        );
        assert_eq!(report, AclValidationReport::default());
    }

    #[test]
    fn test_validate_acl_rules_warns_on_glob_matching_nothing() {
        // A glob that matches nothing today may match a module added tomorrow,
        // so it is reported without refusing to start.
        let report = validate_acl_rules(&[rule(&["cli.z*"], "deny")], &registered(&["cli.cp"]));
        assert_eq!(report.unmatched_targets.len(), 1);
        assert!(!report.is_fatal());
    }

    #[test]
    fn test_validate_acl_rules_understands_compound_targets() {
        // `$or` makes every following entry a target; `$not` consumes exactly
        // one, so entries past the first are never consulted by apcore's
        // matcher and must not be reported.
        let report = validate_acl_rules(
            &[
                rule(&["$or", "cli.cp", "cli.ls"], "deny"),
                rule(&["$not", "cli.cp", "never.consulted"], "allow"),
            ],
            &registered(&["cli.cp", "cli.ls"]),
        );
        assert_eq!(report, AclValidationReport::default());
    }

    #[test]
    fn test_validate_acl_rules_flags_unmatched_operand_inside_compound() {
        let report = validate_acl_rules(
            &[rule(&["$or", "cli.cp", "cli.git.cat-file"], "deny")],
            &registered(&["cli.cp", "cli.git.cat_file"]),
        );
        assert_eq!(report.unmatched_targets.len(), 1);
        assert_eq!(report.unmatched_targets[0].pattern, "cli.git.cat-file");
        assert!(report.is_fatal());
    }

    #[test]
    fn test_validate_acl_rules_reports_a_not_operand_as_covering_everything() {
        // apcore evaluates `$not` as `!match(operand, module_id)`, so an
        // operand matching nothing makes the rule match EVERY module. The old
        // diagnostic stripped the sentinel and said "this rule currently
        // protects nothing" — the exact opposite, about a blanket allow that
        // under first-match-wins nullifies every deny below it.
        let report = validate_acl_rules(
            &[rule(&["$not", "cli.kubectl.apply"], "allow")],
            &registered(&["cli.cp", "cli.ls"]),
        );
        assert_eq!(report.unmatched_targets.len(), 1);
        let finding = &report.unmatched_targets[0];
        assert!(finding.negated, "the $not polarity must be carried through");
        assert_eq!(finding.pattern, "cli.kubectl.apply");
        // Not a typo of anything registered, so it warns rather than refuses —
        // the same shared-ACL allowance the positive case gets.
        assert!(!finding.is_near_miss());
        assert!(!report.is_fatal());
    }

    #[test]
    fn test_validate_acl_rules_refuses_a_near_miss_not_operand() {
        // The operator meant "every module except cli.git.cat_file" and wrote
        // the pre-#19 hyphenated spelling, so the one module the rule was
        // written to carve out is the one it now covers.
        let report = validate_acl_rules(
            &[rule(&["$not", "cli.git.cat-file"], "allow")],
            &registered(&["cli.git.cat_file", "cli.cp"]),
        );
        assert_eq!(report.unmatched_targets.len(), 1);
        assert!(report.unmatched_targets[0].negated);
        assert!(report.is_fatal());

        let message = report
            .fatal_error(Path::new("/etc/apexe/acl.yaml"))
            .expect("a near-miss $not operand must refuse to start")
            .message;
        assert!(message.contains("$not"), "{message}");
        assert!(
            message.contains("applies to every module"),
            "the diagnostic must not claim the rule protects nothing: {message}"
        );
        assert!(message.contains("cli.git.cat_file"), "{message}");
    }

    #[test]
    fn test_validate_acl_rules_skips_target_check_on_empty_registry() {
        // Nothing to validate against — reporting every pattern would be noise.
        // Structural findings still stand.
        let report = validate_acl_rules(&[rule(&["cli.cp"], "deny"), rule(&[], "deny")], &[]);
        assert!(report.unmatched_targets.is_empty());
        assert_eq!(report.inert_rules.len(), 1);
    }

    #[test]
    fn test_acl_validation_report_fatal_error_names_the_findings() {
        let report = validate_acl_rules(
            &[rule(&["cli.git.cat-file"], "deny"), rule(&[], "deny")],
            &registered(&["cli.git.cat_file"]),
        );
        let err = report
            .fatal_error(Path::new("/etc/apexe/acl.yaml"))
            .expect("report should be fatal");
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert!(err.message.contains("/etc/apexe/acl.yaml"));
        assert!(err.message.contains("cli.git.cat_file"));
        assert!(err.message.contains("empty list"));
    }

    #[test]
    fn test_acl_validation_report_fatal_error_none_when_clean() {
        let report = validate_acl_rules(&[rule(&["cli.cp"], "deny")], &registered(&["cli.cp"]));
        assert!(report.fatal_error(Path::new("/tmp/acl.yaml")).is_none());
    }

    #[test]
    fn test_normalize_id_folds_separators_and_case() {
        assert_eq!(normalize_id("cli.git.cat-file"), "cli.git.cat.file");
        assert_eq!(normalize_id("CLI_GIT_LOG"), "cli.git.log");
        assert_eq!(normalize_id("  git log  "), "git.log");
    }

    #[test]
    fn test_suggest_similar_does_not_guess_across_unrelated_modules() {
        // A trailing-segment match is the whole heuristic; unrelated ids that
        // merely share letters must not be offered as "did you mean".
        assert!(suggest_similar("clip", &registered(&["cli.cp"])).is_empty());
        assert!(suggest_similar("cli.c*", &registered(&["cli.cp"])).is_empty());
    }

    #[test]
    fn test_acl_decision_recorded_via_audit_logger() {
        // F5: with an audit logger attached, ACL allow/deny decisions are
        // persisted (so an operator can answer "who was denied which module").
        use std::sync::Arc;
        let tmp = tempfile::TempDir::new().unwrap();
        let audit_path = tmp.path().join("audit.jsonl");
        let audit = Arc::new(crate::governance::AuditManager::new(&audit_path));

        let modules = vec![make_module_with_annotations("cli.rm", false, true)];
        let mut acl = AclManager::generate_default(&modules).into_inner();
        {
            let audit = audit.clone();
            acl.set_audit_logger(move |entry| audit.log_acl_decision(entry));
        }

        // A destructive module is denied for an external caller; audited.
        let allowed = acl.check(Some("@external"), "cli.rm", None);
        assert!(!allowed);

        let content = std::fs::read_to_string(&audit_path).unwrap();
        assert!(
            content.contains("\"decision\""),
            "ACL decision not recorded: {content}"
        );
        assert!(content.contains("cli.rm"));

        // The audit log must be owner-only (0o600) even when an ACL denial is
        // the first write — it carries caller identities and denied targets.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&audit_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "audit log should be owner-only");
        }
    }
}
