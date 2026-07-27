use std::path::PathBuf;
use std::process::Command;
use std::sync::{LazyLock, OnceLock};

use regex::Regex;

use crate::scanner::protocol::ParsedHelp;

/// Bash completion directories, covering both distro and Homebrew layouts.
const BASH_COMPLETION_DIRS: &[&str] = &[
    "/usr/share/bash-completion/completions",
    "/usr/local/share/bash-completion/completions",
    "/opt/homebrew/share/bash-completion/completions",
    "/etc/bash_completion.d",
    "/usr/local/etc/bash_completion.d",
    "/opt/homebrew/etc/bash_completion.d",
];

/// zsh function directories used when `zsh` itself cannot be run to report
/// `$fpath` (e.g. zsh is not installed).
const FALLBACK_ZSH_DIRS: &[&str] = &[
    "/usr/share/zsh/site-functions",
    "/usr/local/share/zsh/site-functions",
    "/opt/homebrew/share/zsh/site-functions",
    "/usr/share/zsh/functions/Completion",
];

/// A `commands=(` / `subcommands=(` array opening, which in zsh completions
/// precedes a list of `'name:description'` entries.
static COMMAND_ARRAY_START_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(commands|subcommands|cmds)\s*=\s*\(").expect("valid static regex")
});

/// A `'name:description'` entry. The description must not contain a colon,
/// which excludes `_arguments` specs like `'formats:format: _date_formats'`.
static COMMAND_ENTRY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"'([a-z][\w-]*):([^:']*)'").expect("valid static regex"));

/// A bash `case` branch label, e.g. `    commit)`.
static BASH_CASE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s+([a-z][\w-]*)\)").expect("valid static regex"));

/// Which shell's convention a completion script follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionKind {
    Zsh,
    Bash,
}

/// Tier 3 parser: extracts subcommands from shell completion scripts.
pub struct CompletionParser;

impl CompletionParser {
    /// Parse a shell completion script for subcommand discovery.
    ///
    /// zsh completion directories are discovered by asking zsh for its
    /// `$fpath` rather than guessing, because the version-stamped path
    /// (`/usr/share/zsh/5.9/functions`) that actually holds the system
    /// completions cannot be hardcoded.
    pub fn parse_completions(&self, tool_name: &str) -> Option<ParsedHelp> {
        let (content, kind) = read_completion_script(tool_name)?;
        let subcommands = match kind {
            CompletionKind::Zsh => extract_zsh_subcommands(&content, tool_name),
            CompletionKind::Bash => extract_bash_subcommands(&content),
        };

        if subcommands.is_empty() {
            return None;
        }

        Some(ParsedHelp {
            subcommand_names: subcommands,
            ..Default::default()
        })
    }
}

/// Locate and read the first completion script found for `tool_name`.
fn read_completion_script(tool_name: &str) -> Option<(String, CompletionKind)> {
    for (path, kind) in completion_candidates(tool_name) {
        if !path.is_file() {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some((content, kind));
        }
    }
    None
}

/// Candidate completion script paths, most authoritative first.
fn completion_candidates(tool_name: &str) -> Vec<(PathBuf, CompletionKind)> {
    let mut candidates: Vec<(PathBuf, CompletionKind)> = zsh_function_dirs()
        .iter()
        .map(|dir| (dir.join(format!("_{tool_name}")), CompletionKind::Zsh))
        .collect();

    for dir in BASH_COMPLETION_DIRS {
        candidates.push((PathBuf::from(dir).join(tool_name), CompletionKind::Bash));
    }

    candidates
}

/// zsh's `$fpath`, queried once per process.
///
/// Falls back to a fixed list when zsh is unavailable or reports nothing.
fn zsh_function_dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut dirs = query_zsh_fpath().unwrap_or_default();
        for dir in FALLBACK_ZSH_DIRS {
            let path = PathBuf::from(dir);
            if !dirs.contains(&path) {
                dirs.push(path);
            }
        }
        dirs
    })
}

/// Ask zsh to print its `$fpath`, one directory per line.
fn query_zsh_fpath() -> Option<Vec<PathBuf>> {
    let output = Command::new("zsh")
        .args(["-c", r#"printf "%s\n" $fpath"#])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let dirs: Vec<PathBuf> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();

    (!dirs.is_empty()).then_some(dirs)
}

/// Extract subcommand names from a zsh completion script.
///
/// Prefers the `_<tool>-<subcommand> ()` function convention, which system
/// completions follow and which yields no false positives. Only when that finds
/// nothing does it fall back to scanning `commands=(...)` arrays.
pub fn extract_zsh_subcommands(content: &str, tool_name: &str) -> Vec<String> {
    let by_function = extract_zsh_function_subcommands(content, tool_name);
    if !by_function.is_empty() {
        return by_function;
    }
    extract_zsh_command_arrays(content)
}

/// Extract subcommands from `_<tool>-<subcommand> ()` function definitions.
fn extract_zsh_function_subcommands(content: &str, tool_name: &str) -> Vec<String> {
    let pattern = format!(r"(?m)^_{}-([a-z][\w-]*)\s*\(\)", regex::escape(tool_name));
    let Ok(function_re) = Regex::new(&pattern) else {
        return Vec::new();
    };

    let mut names: Vec<String> = Vec::new();
    for cap in function_re.captures_iter(content) {
        let name = cap[1].to_string();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Extract subcommands from `commands=( 'name:description' ... )` arrays.
///
/// Scanning is confined to the array body. A whole-file scan for
/// `'name:description'` also matches `_arguments` option specs — `_ls` has no
/// subcommands at all, yet advertises a spec named `formats`.
fn extract_zsh_command_arrays(content: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut in_array = false;

    for line in content.lines() {
        if !in_array {
            in_array = COMMAND_ARRAY_START_RE.is_match(line);
            continue;
        }
        for cap in COMMAND_ENTRY_RE.captures_iter(line) {
            let name = cap[1].to_string();
            if !names.contains(&name) {
                names.push(name);
            }
        }
        if line.trim_start().starts_with(')') || line.trim_end().ends_with(')') {
            in_array = false;
        }
    }

    names
}

/// Extract subcommand names from a bash completion script's `case` branches.
pub fn extract_bash_subcommands(content: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for cap in BASH_CASE_RE.captures_iter(content) {
        let name = cap[1].to_string();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Extract flag names from completion scripts (less reliable than help parsing).
pub fn extract_completion_flags(content: &str) -> Vec<String> {
    let flag_re = Regex::new(r"(--[a-z][\w-]*)").expect("valid static regex");
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
    fn test_extract_zsh_function_convention() {
        // System zsh completions define one function per subcommand.
        let content = "\
(( $+functions[_git-commit] )) ||
_git-commit () {
  _arguments
}
(( $+functions[_git-commit-graph] )) ||
_git-commit-graph() {
  _arguments
}
";
        let subs = extract_zsh_subcommands(content, "git");
        assert_eq!(subs, vec!["commit", "commit-graph"]);
    }

    #[test]
    fn test_extract_zsh_ignores_argument_specs() {
        // Regression: a whole-file scan for `'name:desc'` turned `_ls`'s date
        // format spec into a phantom `formats` subcommand.
        let content = "\
_ls () {
  _arguments \\
    '(-1 -C -m -x)-D+[specify format for date]:format: _date_formats' \\
    '-l[long format]'
}
";
        assert!(extract_zsh_subcommands(content, "ls").is_empty());
    }

    #[test]
    fn test_extract_zsh_command_array_fallback() {
        // Tools shipping their own completion often use a commands array
        // instead of the per-subcommand function convention.
        let content = "\
_mytool() {
    local commands
    commands=(
        'add:Add file contents to the index'
        'commit:Record changes to the repository'
        'push:Update remote refs'
    )
    _describe commands commands
}
";
        let subs = extract_zsh_subcommands(content, "mytool");
        assert_eq!(subs, vec!["add", "commit", "push"]);
    }

    #[test]
    fn test_extract_zsh_command_array_stops_at_close_paren() {
        let content = "\
    commands=(
        'add:Add something'
    )
    other=(
        'notacommand:should not appear'
    )
";
        // The second array is not a commands array, so its entries are ignored.
        let subs = extract_zsh_subcommands(content, "mytool");
        assert_eq!(subs, vec!["add"]);
    }

    #[test]
    fn test_extract_zsh_function_convention_wins_over_arrays() {
        let content = "\
_tool-real () { }
    commands=(
        'phantom:from an unrelated array'
    )
";
        let subs = extract_zsh_subcommands(content, "tool");
        assert_eq!(subs, vec!["real"]);
    }

    #[test]
    fn test_extract_bash_case_subcommands() {
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
        let subs = extract_bash_subcommands(content);
        assert!(subs.contains(&"sub1".to_string()));
        assert!(subs.contains(&"sub2".to_string()));
    }

    #[test]
    fn test_extract_zsh_subcommands_empty() {
        assert!(extract_zsh_subcommands("no subcommands here", "tool").is_empty());
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
        assert!(parser
            .parse_completions("zzz_no_such_tool_xyz_12345")
            .is_none());
    }

    #[test]
    fn test_completion_candidates_cover_zsh_and_bash() {
        let candidates = completion_candidates("git");
        assert!(candidates
            .iter()
            .any(|(path, kind)| *kind == CompletionKind::Zsh
                && path.file_name().is_some_and(|name| name == "_git")));
        assert!(candidates
            .iter()
            .any(|(path, kind)| *kind == CompletionKind::Bash
                && path.file_name().is_some_and(|name| name == "git")));
    }

    #[test]
    fn test_zsh_function_dirs_includes_fallbacks() {
        let dirs = zsh_function_dirs();
        assert!(dirs
            .iter()
            .any(|dir| dir.ends_with("site-functions") || dir.ends_with("functions")));
    }
}
