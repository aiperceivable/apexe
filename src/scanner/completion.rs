use std::path::PathBuf;

use regex::Regex;

use crate::scanner::protocol::ParsedHelp;

/// Tier 3 parser: extracts metadata from shell completion scripts.
///
/// Handles zsh and bash completion files.
pub struct CompletionParser;

impl CompletionParser {
    /// Parse shell completion scripts for subcommand/flag discovery.
    ///
    /// Checks:
    /// 1. `/usr/share/zsh/functions/Completion/_<tool>`
    /// 2. `/usr/local/share/zsh/site-functions/_<tool>`
    /// 3. `/etc/bash_completion.d/<tool>`
    ///
    /// Returns ParsedHelp or None.
    pub fn parse_completions(&self, tool_name: &str) -> Option<ParsedHelp> {
        let paths = [
            PathBuf::from(format!("/usr/share/zsh/functions/Completion/_{tool_name}")),
            PathBuf::from(format!("/usr/local/share/zsh/site-functions/_{tool_name}")),
            PathBuf::from(format!("/etc/bash_completion.d/{tool_name}")),
        ];

        let content = paths
            .iter()
            .find(|p| p.exists())
            .and_then(|p| std::fs::read_to_string(p).ok())?;

        let subcommands = extract_completion_subcommands(&content);

        if subcommands.is_empty() {
            return None;
        }

        Some(ParsedHelp {
            subcommand_names: subcommands,
            ..Default::default()
        })
    }
}

/// Extract subcommand names from shell completion scripts.
pub fn extract_completion_subcommands(content: &str) -> Vec<String> {
    let mut names = Vec::new();

    // Pattern for zsh completion: 'command-name:description'
    let zsh_re = Regex::new(r#"'([a-z][\w-]*):.*'"#).unwrap();
    for cap in zsh_re.captures_iter(content) {
        let name = cap[1].to_string();
        if !names.contains(&name) {
            names.push(name);
        }
    }

    // Pattern for bash completion: case statement entries
    if names.is_empty() {
        let bash_re = Regex::new(r#"(?m)^\s+([a-z][\w-]*)\)"#).unwrap();
        for cap in bash_re.captures_iter(content) {
            let name = cap[1].to_string();
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }

    names
}

/// Extract flag names from completion scripts (less reliable than help parsing).
pub fn extract_completion_flags(content: &str) -> Vec<String> {
    let flag_re = Regex::new(r"(--[a-z][\w-]*)").unwrap();
    let mut flags = Vec::new();

    for cap in flag_re.captures_iter(content) {
        let flag = cap[1].to_string();
        if !flags.contains(&flag) {
            flags.push(flag);
        }
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_zsh_completion_subcommands() {
        let content = r#"
_git_commands() {
    local commands
    commands=(
        'add:Add file contents to the index'
        'commit:Record changes to the repository'
        'push:Update remote refs'
    )
}
"#;
        let subs = extract_completion_subcommands(content);
        assert!(subs.contains(&"add".to_string()));
        assert!(subs.contains(&"commit".to_string()));
        assert!(subs.contains(&"push".to_string()));
    }

    #[test]
    fn test_extract_bash_completion_subcommands() {
        let content = r#"
_tool_completions() {
    case "$prev" in
        sub1)
            COMPREPLY=( --flag1 --flag2 )
            ;;
        sub2)
            COMPREPLY=( --flag3 )
            ;;
    esac
}
"#;
        let subs = extract_completion_subcommands(content);
        assert!(subs.contains(&"sub1".to_string()));
        assert!(subs.contains(&"sub2".to_string()));
    }

    #[test]
    fn test_extract_completion_subcommands_empty() {
        let subs = extract_completion_subcommands("no subcommands here");
        assert!(subs.is_empty());
    }

    #[test]
    fn test_extract_completion_flags() {
        let content = "--verbose --output --format --help";
        let flags = extract_completion_flags(content);
        assert!(flags.contains(&"--verbose".to_string()));
        assert!(flags.contains(&"--output".to_string()));
    }

    #[test]
    fn test_parse_completions_nonexistent_tool() {
        let parser = CompletionParser;
        let result = parser.parse_completions("zzz_no_such_tool_xyz_12345");
        assert!(result.is_none());
    }
}
