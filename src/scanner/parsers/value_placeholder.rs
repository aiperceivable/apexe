//! Recover an option's value placeholder when it was left in the description.
//!
//! Every help-format parser splits an option line into "the option" and "the
//! prose after it", and each does so with its own regex. When the placeholder
//! is spelled in a form that regex does not anticipate — `--proxy
//! [protocol://]host[:port]`, or a value separated from the description by a
//! single space rather than two — the placeholder falls into the prose half.
//! The flag is then typed `boolean`, and the resulting contract is wrong in
//! both directions at once:
//!
//! * the schema only permits `true`, so the option cannot be given a value; and
//! * `true` renders as a bare flag, which a value-taking option answers by
//!   eating the next token — `--proxy --connect-timeout` makes curl read
//!   `--connect-timeout` as the proxy address, and the tool honours something
//!   the caller never asked for.
//!
//! The information needed to fix it is already in hand: the placeholder is
//! sitting at the head of `description`. This pass moves it back where it
//! belongs, so the repair applies to every parser rather than being fixed one
//! regex at a time.

use crate::models::{ScannedFlag, ValueType};

/// Repair every flag whose value placeholder was captured as description text.
///
/// Flags that already carry a `value_name` are left alone: a parser that
/// identified the value is more trustworthy than this heuristic.
pub fn recover_values_from_descriptions(flags: &mut [ScannedFlag]) {
    for flag in flags.iter_mut() {
        if flag.value_name.is_some() || flag.value_type != ValueType::Boolean {
            continue;
        }
        let Some((placeholder, rest)) = split_leading_placeholder(&flag.description) else {
            continue;
        };
        flag.value_name = Some(placeholder.clone());
        flag.description = rest;
        flag.value_type = infer_value_type(&placeholder);
    }
}

/// Split a leading value placeholder off a description, returning the
/// placeholder's bare name and the remaining prose.
///
/// Three spellings are accepted, in decreasing order of certainty:
///
/// * `<seconds>` — angle brackets are only ever a placeholder. The contents may
///   contain spaces (`<fractional seconds>`), so the closing `>` terminates it
///   rather than the next space.
/// * `[protocol://]host[:port]` — a first token containing a bracketed part.
///   Optional-value notation is written this way and nothing else is.
/// * `ARG` — a first token in caps. This one is ambiguous, so it is accepted
///   only when at least two characters long *and* followed by more prose: a
///   description reading exactly `SSL` is a description, not a placeholder.
fn split_leading_placeholder(description: &str) -> Option<(String, String)> {
    let trimmed = description.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix('<') {
        let (inner, tail) = rest.split_once('>')?;
        if inner.is_empty() {
            return None;
        }
        return Some((inner.to_string(), tail.trim_start().to_string()));
    }

    let (first, tail) = match trimmed.split_once(char::is_whitespace) {
        Some((first, tail)) => (first, tail.trim_start()),
        None => (trimmed, ""),
    };

    if first.contains('[') && first.contains(']') {
        return Some((strip_brackets(first), tail.to_string()));
    }

    let is_caps = first.len() >= 2
        && first
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if is_caps && !tail.is_empty() {
        return Some((first.to_string(), tail.to_string()));
    }

    None
}

/// Remove the optional-value brackets from a placeholder, keeping its text.
fn strip_brackets(token: &str) -> String {
    token.replace(['[', ']'], "")
}

/// Map a placeholder's wording onto the value type it implies.
///
/// Deliberately narrow: an unrecognized placeholder means "takes a value",
/// which is already the whole point of this pass, and a wrong numeric guess
/// would reject a value the tool accepts.
pub fn infer_value_type(placeholder: &str) -> ValueType {
    let lowered = placeholder.to_ascii_lowercase();
    if lowered.contains("seconds") || lowered.contains("time") {
        return ValueType::Float;
    }
    match lowered.as_str() {
        "num" | "number" | "count" | "n" | "port" | "int" | "size" => ValueType::Integer,
        "file" | "filename" | "file name" | "path" | "dir" | "directory" => ValueType::Path,
        "url" | "uri" => ValueType::Url,
        _ => ValueType::String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boolean_flag(long: &str, description: &str) -> ScannedFlag {
        ScannedFlag {
            long_name: Some(long.to_string()),
            description: description.to_string(),
            value_type: ValueType::Boolean,
            ..Default::default()
        }
    }

    #[test]
    fn test_recover_values_types_angle_bracket_placeholder() {
        // curl's `--max-time <fractional seconds>` was typed boolean, so the
        // option could neither be given a value nor used as a bare flag.
        let mut flags = vec![boolean_flag(
            "--max-time",
            "<fractional seconds> Maximum time in seconds that you allow each transfer to take.",
        )];
        recover_values_from_descriptions(&mut flags);

        assert_eq!(flags[0].value_name.as_deref(), Some("fractional seconds"));
        assert_eq!(flags[0].value_type, ValueType::Float);
        assert!(
            flags[0].description.starts_with("Maximum time"),
            "placeholder must leave the description: {:?}",
            flags[0].description
        );
    }

    #[test]
    fn test_recover_values_types_bracketed_placeholder() {
        // `--proxy [protocol://]host[:port]` is the form that made curl read
        // the *next flag* as the proxy address.
        let mut flags = vec![boolean_flag(
            "--proxy",
            "[protocol://]host[:port] Use this proxy.",
        )];
        recover_values_from_descriptions(&mut flags);

        assert_eq!(flags[0].value_name.as_deref(), Some("protocol://host:port"));
        assert_eq!(flags[0].value_type, ValueType::String);
        assert_eq!(flags[0].description, "Use this proxy.");
    }

    #[test]
    fn test_recover_values_types_caps_placeholder() {
        let mut flags = vec![boolean_flag("--output", "FILE Write output here.")];
        recover_values_from_descriptions(&mut flags);

        assert_eq!(flags[0].value_name.as_deref(), Some("FILE"));
        assert_eq!(flags[0].value_type, ValueType::Path);
    }

    #[test]
    fn test_recover_values_leaves_prose_descriptions_alone() {
        let mut flags = vec![
            boolean_flag("--verbose", "Make the operation more talkative."),
            boolean_flag("--ssl", "SSL"),
            boolean_flag("--http2", "HTTP/2 Use HTTP/2."),
        ];
        recover_values_from_descriptions(&mut flags);

        for flag in &flags {
            assert_eq!(
                flag.value_type,
                ValueType::Boolean,
                "boolean flag retyped from prose: {flag:?}"
            );
            assert!(flag.value_name.is_none(), "{flag:?}");
        }
    }

    #[test]
    fn test_recover_values_does_not_override_a_parsed_value_name() {
        let mut flag = boolean_flag("--output", "FILE Write output here.");
        flag.value_name = Some("TARGET".to_string());
        flag.value_type = ValueType::Path;
        let mut flags = vec![flag];
        recover_values_from_descriptions(&mut flags);

        assert_eq!(flags[0].value_name.as_deref(), Some("TARGET"));
        assert_eq!(flags[0].description, "FILE Write output here.");
    }
}
