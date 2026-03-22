use regex::Regex;

use crate::errors::ApexeError;

/// Generate a canonical apcore module ID from a CLI command path.
///
/// # Errors
///
/// Returns an error if the generated ID exceeds 128 characters or
/// does not match the required pattern.
pub fn generate_module_id(tool_name: &str, command_path: &[String]) -> Result<String, ApexeError> {
    let prefix = "cli";
    let sanitized_tool = sanitize_segment(tool_name);
    let sanitized_path: Vec<String> = command_path.iter().map(|s| sanitize_segment(s)).collect();

    let mut segments = vec![prefix.to_string(), sanitized_tool];
    segments.extend(sanitized_path);

    let module_id = segments.join(".");

    let re = Regex::new(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$").unwrap();
    if !re.is_match(&module_id) {
        return Err(ApexeError::ParseError(format!(
            "Generated module ID '{module_id}' does not match required pattern"
        )));
    }

    if module_id.len() > 128 {
        return Err(ApexeError::ParseError(format!(
            "Module ID '{module_id}' exceeds 128 characters"
        )));
    }

    Ok(module_id)
}

/// Sanitize a string for use as a module ID segment.
fn sanitize_segment(segment: &str) -> String {
    let mut s = segment.to_lowercase();
    s = s.replace('-', "_");
    s.retain(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');

    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s = format!("x{s}");
    }

    if s.is_empty() {
        "unknown".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("git", &["commit"], "cli.git.commit")]
    #[case("docker", &["container", "ls"], "cli.docker.container.ls")]
    #[case("my-tool", &["sub-cmd"], "cli.my_tool.sub_cmd")]
    #[case("3ds", &[], "cli.x3ds")]
    #[case("ffmpeg", &[], "cli.ffmpeg")]
    #[case("kubectl", &["get", "pods"], "cli.kubectl.get.pods")]
    #[case("aws", &["s3", "cp"], "cli.aws.s3.cp")]
    #[case("3ds-tool", &[], "cli.x3ds_tool")]
    fn test_module_id_generation(
        #[case] tool: &str,
        #[case] path: &[&str],
        #[case] expected: &str,
    ) {
        let path_strings: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        let result = generate_module_id(tool, &path_strings).unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_empty_tool_name() {
        let result = generate_module_id("", &[]).unwrap();
        assert_eq!(result, "cli.unknown");
    }

    #[test]
    fn test_too_long_module_id() {
        // Create a path that will exceed 128 chars
        let long_segments: Vec<String> = (0..30)
            .map(|i| format!("segment{i}"))
            .collect();
        let result = generate_module_id("toolname", &long_segments);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("exceeds 128 characters"));
    }

    #[test]
    fn test_generated_ids_match_regex() {
        let re = Regex::new(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$").unwrap();
        let test_cases = vec![
            ("git", vec!["commit"]),
            ("docker", vec!["container", "ls"]),
            ("my-tool", vec!["sub-cmd"]),
            ("3ds", vec![]),
        ];
        for (tool, path) in test_cases {
            let path_strings: Vec<String> = path.iter().map(|s| s.to_string()).collect();
            let id = generate_module_id(tool, &path_strings).unwrap();
            assert!(re.is_match(&id), "ID '{id}' does not match regex");
        }
    }

    #[test]
    fn test_special_chars_stripped() {
        let path_strings: Vec<String> = vec!["sub@cmd!".to_string()];
        let result = generate_module_id("my.tool", &path_strings).unwrap();
        assert_eq!(result, "cli.mytool.subcmd");
    }
}
