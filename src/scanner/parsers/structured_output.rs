use std::sync::LazyLock;

use regex::Regex;

use crate::models::{ScannedFlag, StructuredOutputInfo};

/// Known JSON-output flag patterns, compiled once (the scan hot path calls
/// `detect` per subcommand; recompiling these every call is wasteful).
///
/// Every pattern requires the word `json` on the same line as the flag, because
/// the flag alone does not mean the tool emits JSON: `tar --format` selects an
/// archive format (`ustar`, `pax`, `cpio`) and `tar -j` selects bzip2
/// compression, yet a bare `--format` or `-j` pattern matched both and tagged
/// `tar` as structured-output — telling an agent it could request JSON from a
/// command that has never emitted any.
///
/// The line is the unit rather than the token immediately after the flag,
/// because a help entry names the value separately from the choices:
/// `--format string   Output format (json, text)`. `[^\n]*` keeps that a match
/// while `tar`'s bare `--format <format>` stays out.
///
/// `-j` is dropped entirely: across CLIs it means "jobs" or "bzip2" far more
/// often than JSON, so it cannot carry the claim by itself.
///
/// INVARIANT: every pattern is a compile-time constant known to be a valid
/// regex, so `Regex::new` never fails here.
static JSON_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"--format[^\n]*\bjson\b", "--format json"),
        (r"--output-format[^\n]*\bjson\b", "--output-format json"),
        (r"-o[^\n]*\bjson\b|--output[^\n]*\bjson\b", "-o json"),
        (r"--json\b", "--json"),
    ]
    .into_iter()
    .map(|(pattern, flag)| (Regex::new(pattern).expect("valid static regex"), flag))
    .collect()
});

/// Detects if a CLI tool supports structured (JSON) output.
pub struct StructuredOutputDetector;

impl StructuredOutputDetector {
    /// Detect structured output support from flags and help text.
    pub fn detect(&self, flags: &[ScannedFlag], help_text: &str) -> StructuredOutputInfo {
        // Check parsed flags first
        for flag in flags {
            let long = flag.long_name.as_deref().unwrap_or("");
            if matches!(long, "--format" | "--output-format" | "--output") {
                // Check enum values for json
                if let Some(ref enums) = flag.enum_values {
                    if enums.iter().any(|v| v == "json") {
                        return StructuredOutputInfo {
                            supported: true,
                            flag: Some(format!("{long} json")),
                            format: Some("json".to_string()),
                        };
                    }
                }
                // Check description for json mention (common in Cobra-style help)
                if flag.description.to_lowercase().contains("json") {
                    return StructuredOutputInfo {
                        supported: true,
                        flag: Some(format!("{long} json")),
                        format: Some("json".to_string()),
                    };
                }
            }
            if long == "--json" {
                return StructuredOutputInfo {
                    supported: true,
                    flag: Some("--json".to_string()),
                    format: Some("json".to_string()),
                };
            }
        }

        // Fall back to precompiled regex patterns on raw help text
        for (re, flag_str) in JSON_PATTERNS.iter() {
            if re.is_match(help_text) {
                return StructuredOutputInfo {
                    supported: true,
                    flag: Some((*flag_str).to_string()),
                    format: Some("json".to_string()),
                };
            }
        }

        StructuredOutputInfo::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ValueType;

    #[test]
    fn test_detect_format_enum_with_json() {
        let detector = StructuredOutputDetector;
        let flags = vec![ScannedFlag {
            long_name: Some("--format".into()),
            short_name: None,
            description: "Output format".into(),
            value_type: ValueType::Enum,
            required: false,
            default: None,
            enum_values: Some(vec!["json".into(), "text".into()]),
            repeatable: false,
            value_name: None,
            ..Default::default()
        }];
        let info = detector.detect(&flags, "");
        assert!(info.supported);
        assert_eq!(info.flag.as_deref(), Some("--format json"));
        assert_eq!(info.format.as_deref(), Some("json"));
    }

    #[test]
    fn test_detect_json_flag() {
        let detector = StructuredOutputDetector;
        let flags = vec![ScannedFlag {
            long_name: Some("--json".into()),
            short_name: None,
            description: "JSON output".into(),
            value_type: ValueType::Boolean,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
            ..Default::default()
        }];
        let info = detector.detect(&flags, "");
        assert!(info.supported);
        assert_eq!(info.flag.as_deref(), Some("--json"));
    }

    #[test]
    fn test_detect_no_json_flags() {
        let detector = StructuredOutputDetector;
        let flags = vec![ScannedFlag {
            long_name: Some("--verbose".into()),
            short_name: None,
            description: "Verbose".into(),
            value_type: ValueType::Boolean,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
            ..Default::default()
        }];
        let info = detector.detect(&flags, "Some help text without json mentions");
        assert!(!info.supported);
    }

    #[test]
    fn test_detect_regex_fallback_json_flag() {
        let detector = StructuredOutputDetector;
        let info = detector.detect(&[], "Use --json to get JSON output");
        assert!(info.supported);
        assert_eq!(info.flag.as_deref(), Some("--json"));
    }

    #[test]
    fn test_detect_regex_fallback_format_flag() {
        let detector = StructuredOutputDetector;
        let info = detector.detect(&[], "  --format string  Output format (json, text)");
        assert!(info.supported);
        assert_eq!(info.flag.as_deref(), Some("--format json"));
    }

    #[test]
    fn test_format_flag_without_json_is_not_structured_output() {
        // `tar --format` selects an archive format. Claiming structured output
        // here tells an agent it can ask for JSON from a command that emits none.
        let detector = StructuredOutputDetector;
        let info = detector.detect(
            &[],
            "  --format <format>  Archive format (ustar, pax, cpio)",
        );
        assert!(!info.supported);
    }

    #[test]
    fn test_short_j_flag_is_not_structured_output() {
        // `-j` is bzip2 for tar and "jobs" for make; it never implies JSON.
        let detector = StructuredOutputDetector;
        let info = detector.detect(&[], "  -j, --bzip2   Compress with bzip2");
        assert!(!info.supported);
    }

    #[test]
    fn test_json_on_another_line_does_not_bind_to_the_flag() {
        // A tool that merely mentions JSON somewhere in its help does not thereby
        // gain a `--format json`.
        let detector = StructuredOutputDetector;
        let info = detector.detect(
            &[],
            "  --format <format>  Archive format\n\n  Reads JSON manifests.",
        );
        assert!(!info.supported);
    }
}
