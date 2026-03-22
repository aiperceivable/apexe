use thiserror::Error;

/// Top-level error type for all apexe operations.
#[derive(Debug, Error)]
pub enum ApexeError {
    /// CLI tool binary not found on PATH.
    #[error("Tool '{tool_name}' not found on PATH")]
    ToolNotFound { tool_name: String },

    /// Scanning error (general).
    #[error("Scan error: {0}")]
    ScanError(String),

    /// Subprocess timed out during scanning.
    #[error("Command '{command}' timed out after {timeout}s")]
    ScanTimeout { command: String, timeout: u64 },

    /// Subprocess execution denied.
    #[error("Permission denied executing '{command}'")]
    ScanPermission { command: String },

    /// Input contains shell injection characters.
    #[error("Parameter '{param_name}' contains prohibited characters: {chars:?}")]
    CommandInjection {
        param_name: String,
        chars: Vec<char>,
    },

    /// Help text parsing failed.
    #[error("Parse error: {0}")]
    ParseError(String),

    /// IO error wrapper.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// YAML serialization error.
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),

    /// JSON serialization error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_not_found_display() {
        let err = ApexeError::ToolNotFound {
            tool_name: "git".into(),
        };
        assert_eq!(err.to_string(), "Tool 'git' not found on PATH");
    }

    #[test]
    fn test_scan_error_display() {
        let err = ApexeError::ScanError("something broke".into());
        assert_eq!(err.to_string(), "Scan error: something broke");
    }

    #[test]
    fn test_scan_timeout_display() {
        let err = ApexeError::ScanTimeout {
            command: "git status".into(),
            timeout: 30,
        };
        assert_eq!(
            err.to_string(),
            "Command 'git status' timed out after 30s"
        );
    }

    #[test]
    fn test_scan_permission_display() {
        let err = ApexeError::ScanPermission {
            command: "secret-tool".into(),
        };
        assert_eq!(
            err.to_string(),
            "Permission denied executing 'secret-tool'"
        );
    }

    #[test]
    fn test_command_injection_display() {
        let err = ApexeError::CommandInjection {
            param_name: "file".into(),
            chars: vec![';', '|'],
        };
        assert_eq!(
            err.to_string(),
            "Parameter 'file' contains prohibited characters: [';', '|']"
        );
    }

    #[test]
    fn test_parse_error_display() {
        let err = ApexeError::ParseError("unexpected token".into());
        assert_eq!(err.to_string(), "Parse error: unexpected token");
    }

    #[test]
    fn test_io_error_transparent() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = ApexeError::Io(io_err);
        assert_eq!(err.to_string(), "file missing");
    }

    #[test]
    fn test_json_error_transparent() {
        let json_err = serde_json::from_str::<String>("not json").unwrap_err();
        let err = ApexeError::Json(json_err);
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn test_yaml_error_transparent() {
        let yaml_err = serde_yaml::from_str::<String>(":\n  :\n  bad").unwrap_err();
        let err = ApexeError::Yaml(yaml_err);
        assert!(!err.to_string().is_empty());
    }
}
