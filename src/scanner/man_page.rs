use std::process::Command;

use crate::models::{ScannedFlag, ValueType};
use crate::scanner::protocol::ParsedHelp;

/// Tier 2 parser: extracts metadata from man pages.
///
/// Used to enrich Tier 1 results with additional descriptions and options.
pub struct ManPageParser;

impl ManPageParser {
    /// Parse man page for additional metadata.
    ///
    /// Runs `man -P cat <tool>`, extracts DESCRIPTION and OPTIONS sections.
    /// Returns None if man page is not available.
    pub fn parse_man_page(&self, tool_name: &str) -> Option<ParsedHelp> {
        let output = Command::new("man")
            .args(["-P", "cat", tool_name])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let description = extract_man_description(&text);

        if description.is_empty() {
            return None;
        }

        let option_pairs = extract_man_options(&text);
        let flags: Vec<ScannedFlag> = option_pairs
            .into_iter()
            .map(|(name, desc)| {
                let (long_name, short_name) = if name.starts_with("--") {
                    (Some(name), None)
                } else {
                    (None, Some(name))
                };
                ScannedFlag {
                    long_name,
                    short_name,
                    description: desc,
                    value_type: ValueType::Unknown,
                    required: false,
                    default: None,
                    enum_values: None,
                    repeatable: false,
                    value_name: None,
                }
            })
            .collect();

        Some(ParsedHelp {
            description,
            flags,
            ..Default::default()
        })
    }
}

/// Extract description from the DESCRIPTION section of a man page.
pub fn extract_man_description(text: &str) -> String {
    let mut in_desc = false;
    let mut lines = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "DESCRIPTION" || trimmed == "Description" {
            in_desc = true;
            continue;
        }
        if in_desc {
            // Check if we hit another section header (all caps, no leading space)
            if !trimmed.is_empty()
                && !line.starts_with(' ')
                && !line.starts_with('\t')
                && trimmed == trimmed.to_uppercase()
                && !lines.is_empty()
            {
                break;
            }
            if trimmed.is_empty() && !lines.is_empty() {
                break;
            }
            if !trimmed.is_empty() {
                lines.push(trimmed);
            }
        }
    }

    lines.join(" ").chars().take(200).collect()
}

/// Extract flags from the OPTIONS section of a man page.
///
/// Returns a list of `(flag_name, description)` pairs.
/// Flag names retain their leading dashes (e.g. `--verbose`, `-v`).
pub fn extract_man_options(text: &str) -> Vec<(String, String)> {
    let mut in_opts = false;
    let mut flags: Vec<(String, String)> = Vec::new();
    let mut current_flag: Option<String> = None;
    let mut current_desc: Vec<String> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();

        // Detect OPTIONS section
        if trimmed == "OPTIONS" || trimmed == "Options" {
            in_opts = true;
            continue;
        }
        if !in_opts {
            continue;
        }

        // End at next section header (all-caps, non-indented, non-empty)
        if !trimmed.is_empty()
            && !line.starts_with(' ')
            && !line.starts_with('\t')
            && trimmed == trimmed.to_uppercase()
        {
            break;
        }

        // Detect flag line: indented and starts with -
        let is_flag_line = (line.starts_with("       -")
            || line.starts_with("       --")
            || line.starts_with("\t-"))
            && trimmed.starts_with('-');

        if is_flag_line {
            // Save previous flag
            if let Some(ref flag) = current_flag {
                let desc = current_desc.join(" ").trim().to_string();
                if !desc.is_empty() {
                    flags.push((flag.clone(), desc));
                }
            }
            // Extract flag name (first token starting with -)
            current_flag = trimmed
                .split_whitespace()
                .next()
                .filter(|s| s.starts_with('-'))
                .map(|s| s.trim_end_matches(',').to_string());
            current_desc.clear();
            // Remainder of line after flag token may be description text
            let after_flag: String = trimmed
                .split_whitespace()
                .skip(1)
                .collect::<Vec<_>>()
                .join(" ");
            if !after_flag.is_empty() {
                current_desc.push(after_flag);
            }
        } else if current_flag.is_some() && !trimmed.is_empty() {
            current_desc.push(trimmed.to_string());
        } else if trimmed.is_empty() && current_flag.is_some() {
            // Blank line ends current flag description
            if let Some(ref flag) = current_flag {
                let desc = current_desc.join(" ").trim().to_string();
                if !desc.is_empty() {
                    flags.push((flag.clone(), desc));
                }
            }
            current_flag = None;
            current_desc.clear();
        }
    }

    // Flush last accumulated flag
    if let Some(ref flag) = current_flag {
        let desc = current_desc.join(" ").trim().to_string();
        if !desc.is_empty() {
            flags.push((flag.clone(), desc));
        }
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_man_description_basic() {
        let man_text = r#"NAME
       git - the stupid content tracker

SYNOPSIS
       git [--version] [--help] <command> [<args>]

DESCRIPTION
       Git is a fast, scalable, distributed revision control system with
       an unusually rich command set.

OPTIONS
       --version
              Prints the Git suite version.
"#;
        let desc = extract_man_description(man_text);
        assert!(desc.contains("Git is a fast"));
    }

    #[test]
    fn test_extract_man_description_missing() {
        let man_text = "NAME\n       tool - does things\n\nOPTIONS\n       --help\n";
        let desc = extract_man_description(man_text);
        assert!(desc.is_empty());
    }

    #[test]
    fn test_extract_man_description_truncated() {
        let long_desc = "A".repeat(300);
        let man_text = format!("DESCRIPTION\n       {long_desc}\n\nOPTIONS\n");
        let desc = extract_man_description(&man_text);
        assert_eq!(desc.len(), 200);
    }

    #[test]
    fn test_parse_man_page_nonexistent_tool() {
        let parser = ManPageParser;
        let result = parser.parse_man_page("zzz_no_such_tool_xyz_12345");
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_man_options_basic() {
        let man_text = r#"NAME
       mytool - does things

DESCRIPTION
       A useful tool.

OPTIONS
       --verbose
              Enable verbose output.

       --format
              Set output format.

ENVIRONMENT
       HOME   User home directory.
"#;
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[0].0, "--verbose");
        assert!(flags[0].1.contains("Enable verbose output"));
        assert_eq!(flags[1].0, "--format");
        assert!(flags[1].1.contains("Set output format"));
    }

    #[test]
    fn test_extract_man_options_empty() {
        let man_text = "NAME\n       tool - does things\n\nDESCRIPTION\n       A tool.\n";
        let flags = extract_man_options(man_text);
        assert!(flags.is_empty());
    }

    #[test]
    fn test_extract_man_options_multi_line_desc() {
        let man_text = r#"NAME
       mytool - does things

OPTIONS
       --output
              Specify the output file path. This flag accepts
              an absolute or relative filesystem path and will
              create intermediate directories as needed.

ENVIRONMENT
       HOME   User home.
"#;
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].0, "--output");
        assert!(flags[0].1.contains("Specify the output file path"));
        assert!(flags[0].1.contains("create intermediate directories"));
    }
}
