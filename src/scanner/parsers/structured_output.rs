use regex::Regex;

use crate::models::{ScannedFlag, StructuredOutputInfo};

/// Known patterns for JSON output flags.
const JSON_PATTERNS: &[(&str, &str)] = &[
    (r"--format\b", "--format json"),
    (r"--output-format\b", "--output-format json"),
    (r"-o\s+json\b|--output\s+json\b", "-o json"),
    (r"--json\b", "--json"),
    (r"-j\b", "-j"),
];

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

        // Fall back to regex patterns on raw help text
        for &(pattern, flag_str) in JSON_PATTERNS {
            if let Ok(re) = Regex::new(pattern) {
                if re.is_match(help_text) {
                    return StructuredOutputInfo {
                        supported: true,
                        flag: Some(flag_str.to_string()),
                        format: Some("json".to_string()),
                    };
                }
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
}
