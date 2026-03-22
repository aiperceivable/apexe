use std::process::Command;

use crate::scanner::protocol::ParsedHelp;

/// Tier 2 parser: extracts metadata from man pages.
///
/// Used to enrich Tier 1 results with additional descriptions and options.
pub struct ManPageParser;

impl ManPageParser {
    /// Parse man page for additional metadata.
    ///
    /// Runs `man -P cat <tool>`, extracts DESCRIPTION section.
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

        Some(ParsedHelp {
            description,
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
}
