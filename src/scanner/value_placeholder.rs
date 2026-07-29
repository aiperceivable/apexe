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

/// Placeholder words this pass will accept when they appear bare and in caps.
///
/// A closed list, because the bare-caps form is the one spelling that is
/// genuinely ambiguous, and guessing wrong is the expensive direction: a flag
/// wrongly re-typed to `string` can no longer be sent as `true`, so a *working*
/// switch becomes unusable, whereas a placeholder this list misses merely
/// leaves the flag exactly as the parser produced it.
///
/// The words below are the conventional GNU/BSD operand names. Real
/// descriptions that open with an unlisted caps token are common and are
/// sentence-initial identifiers, not placeholders — `ld -noprebind` begins
/// "LD_PREBIND is no longer supported", `ex --echo-wid` begins "GTK GUI only:".
const PLACEHOLDER_WORDS: &[&str] = &[
    "ARG",
    "CHAR",
    "CMD",
    "COMMAND",
    "COUNT",
    "DATE",
    "DIR",
    "DIRECTORY",
    "EXPR",
    "FILE",
    "FILENAME",
    "FORMAT",
    "HOST",
    "ID",
    "INT",
    "KEY",
    "LEVEL",
    "LIST",
    "MODE",
    "N",
    "NAME",
    "NUM",
    "NUMBER",
    "PATH",
    "PATTERN",
    "PORT",
    "PREFIX",
    "RANGE",
    "SECONDS",
    "SIZE",
    "SPEC",
    "STRING",
    "SUFFIX",
    "TEXT",
    "TYPE",
    "URI",
    "URL",
    "USER",
    "VALUE",
    "WHEN",
    "WIDTH",
    "WORD",
];

/// Split a leading value placeholder off a description, returning the
/// placeholder's bare name and the remaining prose.
///
/// Three spellings are accepted, in decreasing order of certainty:
///
/// * `<seconds>` — angle brackets are only ever a placeholder. The contents may
///   contain spaces (`<fractional seconds>`), so the closing `>` terminates it
///   rather than the next space.
/// * `[protocol://]host[:port]` — a first token that *mixes* bracketed and
///   unbracketed text. The mixing is load-bearing: a token that is bracketed
///   end to end is a documentation tag, not a value. Info-ZIP prefixes 16
///   boolean switches with `[WIN32]` / `[VMS]` / `[MacOS]`, and `[deprecated]`
///   is a widespread convention; accepting those re-typed working switches into
///   value-taking options that the tool then reads as a filename.
/// * `ARG` — a bare caps token, accepted only from [`PLACEHOLDER_WORDS`] and
///   only when followed by more prose. See that list for why it is closed.
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

    if is_mixed_bracket_placeholder(first) {
        return Some((strip_brackets(first), tail.to_string()));
    }

    if !tail.is_empty() && PLACEHOLDER_WORDS.contains(&first) {
        return Some((first.to_string(), tail.to_string()));
    }

    None
}

/// Whether `token` is a value form such as `[protocol://]host[:port]` rather
/// than a wholly-bracketed documentation tag such as `[WIN32]`.
///
/// Requires at least one bracketed section *and* at least one character outside
/// every bracket. That is exactly what separates an optional-value spelling from
/// a platform or status tag.
fn is_mixed_bracket_placeholder(token: &str) -> bool {
    if !token.contains('[') || !token.contains(']') {
        return false;
    }
    let mut depth = 0i32;
    let mut outside = 0usize;
    for ch in token.chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => outside += 1,
            _ => {}
        }
    }
    outside > 0
}

/// Remove the optional-value brackets from a placeholder, keeping its text.
fn strip_brackets(token: &str) -> String {
    token.replace(['[', ']'], "")
}

/// Map a placeholder's wording onto the value type it implies.
///
/// The single table for the whole scanner. It used to be three — one in
/// `parsers::gnu`, one in `parsers::clap_parser`, and this one — and they had
/// drifted: `SIZE` was String in one and Integer in another, `FLOAT`/`DECIMAL`
/// existed only in the GNU copy, and durations only here. Both paths feed the
/// same merged `global_flags`, so the emitted JSON type for an option depended
/// on whether that host's `--help` happened to be rich enough for the Tier 1
/// parser to win — `{"size": 4096}` validated against one host's contract and
/// was rejected by another's.
///
/// Matching is case-insensitive so the GNU/BSD `FILE` convention and the
/// man-page `<file>` convention resolve identically.
///
/// Deliberately narrow: an unrecognized placeholder means "takes a value",
/// which is already the whole point of this pass, and a wrong numeric guess
/// would reject a value the tool accepts.
pub fn infer_value_type(placeholder: &str) -> ValueType {
    let lowered = placeholder.to_ascii_lowercase();
    match lowered.as_str() {
        // Durations are matched as whole words, never as substrings. A
        // `contains("time")` rule typed curl's `-z, --time-cond <time>` as a
        // number, and that option takes a date expression or a filename — so
        // the contract admitted only values curl cannot use and rejected every
        // value it can, which is the precise failure this function's own
        // narrowness is supposed to prevent.
        "seconds" | "fractional seconds" | "secs" | "milliseconds" | "ms" => ValueType::Float,
        "float" | "decimal" => ValueType::Float,
        "num" | "number" | "count" | "n" | "port" | "int" | "integer" | "size" => {
            ValueType::Integer
        }
        "file" | "filename" | "file name" | "path" | "dir" | "directory" => ValueType::Path,
        "url" | "uri" => ValueType::Url,
        _ => ValueType::String,
    }
}

/// The value type a flag carries, given the placeholder its parser found.
///
/// `None` means the parser saw no placeholder at all, which is what makes a
/// flag boolean. Parsers call this rather than writing their own table.
pub fn flag_value_type(value_name: Option<&str>) -> ValueType {
    match value_name {
        None => ValueType::Boolean,
        Some(placeholder) => infer_value_type(placeholder),
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
    fn test_recover_values_leaves_platform_tagged_boolean_switches_alone() {
        // Regression: Info-ZIP prefixes 16 boolean switches with a bracketed
        // platform tag, and `ex` / `ld` open descriptions with a caps
        // identifier. Each was re-typed to a value-taking string option, so a
        // working switch could no longer be sent as `true` at all — the exact
        // inverse of the bug this module exists to fix.
        let mut flags = vec![
            boolean_flag(
                "--archive-clear",
                "[WIN32]  Once archive is created, clear the archive bits of files.",
            ),
            boolean_flag(
                "--datafork",
                "[MacOS] Include only data-fork of files zipped.",
            ),
            boolean_flag("--echo-wid", "GTK GUI only: Echo the Window ID on stdout."),
            boolean_flag(
                "-noprebind",
                "LD_PREBIND is no longer supported as a way to enable prebinding.",
            ),
            boolean_flag("--experimental", "[experimental] Enable the new resolver."),
        ];
        recover_values_from_descriptions(&mut flags);

        for flag in &flags {
            assert_eq!(
                flag.value_type,
                ValueType::Boolean,
                "a documentation tag is not a value placeholder: {flag:?}"
            );
            assert!(flag.value_name.is_none(), "{flag:?}");
        }
    }

    #[test]
    fn test_recover_values_still_takes_a_mixed_bracket_placeholder() {
        // The bracket branch must keep working for the form it was written for:
        // brackets mixed with unbracketed text is an optional-value spelling.
        let mut flags = vec![
            boolean_flag("--proxy", "[protocol://]host[:port] Use this proxy."),
            boolean_flag("-D", "[bind_address:]port Dynamic port forwarding."),
        ];
        recover_values_from_descriptions(&mut flags);

        assert_eq!(flags[0].value_name.as_deref(), Some("protocol://host:port"));
        assert_eq!(flags[1].value_name.as_deref(), Some("bind_address:port"));
    }

    #[test]
    fn test_flag_value_type_is_the_scanner_wide_table() {
        // Regression: four parsers each carried their own placeholder table and
        // they had drifted — SIZE was String in one and Integer in another,
        // FLOAT/DECIMAL existed only in the GNU copy. Both the help path and the
        // man path feed the same merged `global_flags`, so an option's emitted
        // JSON type depended on which parser happened to win on that host.
        assert_eq!(flag_value_type(None), ValueType::Boolean);
        // Case-insensitive, so the GNU `FILE` and man-page `<file>` conventions
        // resolve identically.
        assert_eq!(flag_value_type(Some("FILE")), ValueType::Path);
        assert_eq!(flag_value_type(Some("file")), ValueType::Path);
        // The entries that used to exist in only one of the copies.
        assert_eq!(flag_value_type(Some("SIZE")), ValueType::Integer);
        assert_eq!(flag_value_type(Some("FLOAT")), ValueType::Float);
        assert_eq!(flag_value_type(Some("DECIMAL")), ValueType::Float);
        assert_eq!(flag_value_type(Some("INTEGER")), ValueType::Integer);
        assert_eq!(flag_value_type(Some("SECONDS")), ValueType::Float);
        assert_eq!(flag_value_type(Some("URL")), ValueType::Url);
        // Unrecognized still means "takes a value", not a guess.
        assert_eq!(flag_value_type(Some("WIDGET")), ValueType::String);
    }

    #[test]
    fn test_infer_value_type_matches_durations_as_whole_words() {
        // Regression: `contains("time")` typed curl's `-z, --time-cond <time>`
        // as a number, but it takes a date expression or a filename.
        assert_eq!(infer_value_type("time"), ValueType::String);
        assert_eq!(infer_value_type("datetime"), ValueType::String);
        assert_eq!(infer_value_type("timestamp"), ValueType::String);
        // Real durations still type as numbers.
        assert_eq!(infer_value_type("seconds"), ValueType::Float);
        assert_eq!(infer_value_type("fractional seconds"), ValueType::Float);
        assert_eq!(infer_value_type("milliseconds"), ValueType::Float);
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
