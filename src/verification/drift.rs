use super::{result, Change, ChangeKind, ModuleVerification, VerificationStatus};
use apcore_toolkit::ScannedModule;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn compare(
    old: &[ScannedModule],
    fresh: &[ScannedModule],
) -> anyhow::Result<Vec<ModuleVerification>> {
    let current: BTreeMap<&str, &ScannedModule> = fresh
        .iter()
        .map(|module| (module.module_id.as_str(), module))
        .collect();
    let previous: BTreeSet<&str> = old.iter().map(|module| module.module_id.as_str()).collect();
    let mut report = Vec::new();
    for module in old {
        let mut entry = result(module, VerificationStatus::Unchanged, None)?;
        if let Some(new) = current.get(module.module_id.as_str()) {
            compare_module(module, new, &mut entry);
        } else {
            entry.changes.push(change(
                "module",
                ChangeKind::Breaking,
                serde_json::json!(module.module_id),
                Value::Null,
            ));
        }
        if !entry.changes.is_empty() {
            entry.status = VerificationStatus::Changed;
        }
        report.push(entry);
    }
    for module in fresh
        .iter()
        .filter(|module| !previous.contains(module.module_id.as_str()))
    {
        let mut entry = result(module, VerificationStatus::Changed, None)?;
        entry.changes.push(change(
            "module",
            ChangeKind::Additive,
            Value::Null,
            serde_json::json!(module.module_id),
        ));
        report.push(entry);
    }
    report.sort_by(|left, right| left.module_id.cmp(&right.module_id));
    Ok(report)
}

fn compare_module(old: &ScannedModule, new: &ScannedModule, entry: &mut ModuleVerification) {
    let Some(source) = old.metadata.get("source_contract") else {
        entry.status = VerificationStatus::Unverifiable;
        entry.reason = Some("Missing source baseline; re-run apexe scan before verifying".into());
        return;
    };
    let Some(current) = new.metadata.get("source_contract") else {
        entry.status = VerificationStatus::Unverifiable;
        entry.reason = Some("Fresh scan has no source baseline".into());
        return;
    };
    diff("source", source, current, &mut entry.changes);
    diff(
        "published.input_schema",
        &old.input_schema,
        &new.input_schema,
        &mut entry.changes,
    );
    diff(
        "published.output_schema",
        &old.output_schema,
        &new.output_schema,
        &mut entry.changes,
    );
    diff(
        "published.annotations",
        &serde_json::json!(old.annotations),
        &serde_json::json!(new.annotations),
        &mut entry.changes,
    );
}

fn diff(path: &str, before: &Value, after: &Value, changes: &mut Vec<Change>) {
    if before == after {
        return;
    }
    if let (Some(left), Some(right)) = (before.as_object(), after.as_object()) {
        let keys: BTreeSet<&str> = left
            .keys()
            .chain(right.keys())
            .map(String::as_str)
            .collect();
        for key in keys {
            diff(
                &format!("{path}.{key}"),
                left.get(key).unwrap_or(&Value::Null),
                right.get(key).unwrap_or(&Value::Null),
                changes,
            );
        }
    } else {
        changes.push(change(
            path,
            classify(path, before, after),
            before.clone(),
            after.clone(),
        ));
    }
}

fn classify(path: &str, before: &Value, after: &Value) -> ChangeKind {
    if path.ends_with(".description") || path.ends_with(".title") || path.ends_with(".tool_version")
    {
        return ChangeKind::Cosmetic;
    }
    if path.ends_with(".documentation") {
        return ChangeKind::Review;
    }
    if path.contains("output_schema") || path.contains("annotations") || path.ends_with(".variant")
    {
        return ChangeKind::Breaking;
    }
    if path.ends_with(".required") {
        let left: BTreeSet<&str> = before
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let right: BTreeSet<&str> = after
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        return if right.is_subset(&left) {
            ChangeKind::Additive
        } else {
            ChangeKind::Breaking
        };
    }
    if before.is_null() && path.contains(".properties.") {
        return ChangeKind::Additive;
    }
    ChangeKind::Breaking
}

fn change(path: &str, kind: ChangeKind, before: Value, after: Value) -> Change {
    Change {
        path: path.into(),
        kind,
        before,
        after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn test_diff_classifies_removed_added_required_and_type_changes() {
        let mut changes = vec![];
        diff(
            "source.input_schema",
            &json!({"properties":{"old":{"type":"string"},"typed":{"type":"string"}},"required":[]}),
            &json!({"properties":{"new":{"type":"string"},"typed":{"type":"integer"}},"required":["new"]}),
            &mut changes,
        );
        assert!(changes
            .iter()
            .any(|change| change.path.ends_with(".old") && change.kind == ChangeKind::Breaking));
        assert!(changes
            .iter()
            .any(|change| change.path.ends_with(".new") && change.kind == ChangeKind::Additive));
        assert!(changes.iter().any(
            |change| change.path.ends_with(".required") && change.kind == ChangeKind::Breaking
        ));
        assert!(changes
            .iter()
            .any(|change| change.path.ends_with(".typed.type")
                && change.kind == ChangeKind::Breaking));
    }
    #[test]
    fn test_diff_does_not_call_unknown_source_text_changes_cosmetic() {
        assert_eq!(
            classify("source.documentation", &json!("before"), &json!("after")),
            ChangeKind::Review
        );
        assert_eq!(
            classify(
                "source.input_schema.description",
                &json!("before"),
                &json!("after")
            ),
            ChangeKind::Cosmetic
        );
    }
}
