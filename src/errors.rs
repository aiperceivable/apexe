use std::collections::HashMap;

use apcore::{ErrorCode, ModuleError};
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

impl ApexeError {
    /// Convert to `ModuleError` with an attached trace_id.
    pub fn into_module_error_with_trace(self, trace_id: String) -> ModuleError {
        let module_err: ModuleError = self.into();
        module_err.with_trace_id(trace_id)
    }
}

impl From<ApexeError> for ModuleError {
    fn from(err: ApexeError) -> ModuleError {
        match err {
            ApexeError::ToolNotFound { ref tool_name } => {
                let mut details = HashMap::new();
                details.insert("tool_name".to_string(), serde_json::json!(tool_name));
                ModuleError::new(ErrorCode::ModuleNotFound, err.to_string())
                    .with_details(details)
                    .with_retryable(false)
                    .with_ai_guidance(format!(
                        "The tool '{}' is not installed. Install it and try again.",
                        tool_name
                    ))
            }

            ApexeError::ScanError(ref msg) => {
                let mut details = HashMap::new();
                details.insert("scan_error".to_string(), serde_json::json!(msg));
                ModuleError::new(ErrorCode::GeneralInternalError, err.to_string())
                    .with_details(details)
                    .with_ai_guidance(format!("An internal scanning error occurred: {}", msg))
            }

            ApexeError::ScanTimeout {
                ref command,
                timeout,
            } => {
                let mut details = HashMap::new();
                details.insert("command".to_string(), serde_json::json!(command));
                details.insert("timeout_seconds".to_string(), serde_json::json!(timeout));
                ModuleError::new(ErrorCode::ModuleTimeout, err.to_string())
                    .with_details(details)
                    .with_retryable(true)
                    .with_ai_guidance(
                        "The command took too long. Try with simpler arguments or increase timeout.",
                    )
            }

            ApexeError::ScanPermission { ref command } => {
                let mut details = HashMap::new();
                details.insert("command".to_string(), serde_json::json!(command));
                ModuleError::new(ErrorCode::AclDenied, err.to_string())
                    .with_details(details)
                    .with_retryable(false)
                    .with_ai_guidance(
                        "Permission denied. Check file permissions or run with appropriate privileges.",
                    )
            }

            ApexeError::CommandInjection {
                ref param_name,
                ref chars,
            } => {
                let mut details = HashMap::new();
                details.insert("param_name".to_string(), serde_json::json!(param_name));
                details.insert(
                    "prohibited_chars".to_string(),
                    serde_json::json!(chars.iter().map(|c| c.to_string()).collect::<Vec<_>>()),
                );
                ModuleError::new(ErrorCode::GeneralInvalidInput, err.to_string())
                    .with_details(details)
                    .with_retryable(false)
                    .with_ai_guidance(format!(
                        "Remove shell metacharacters ({:?}) from parameter '{}'.",
                        chars, param_name
                    ))
            }

            ApexeError::ParseError(ref msg) => {
                let mut details = HashMap::new();
                details.insert("parse_error".to_string(), serde_json::json!(msg));
                ModuleError::new(ErrorCode::GeneralInternalError, err.to_string())
                    .with_details(details)
                    .with_ai_guidance(format!(
                        "Help text parsing failed: {}. The tool may use a non-standard help format.",
                        msg
                    ))
            }

            ApexeError::Io(ref e) => {
                let mut details = HashMap::new();
                details.insert(
                    "io_error_kind".to_string(),
                    serde_json::json!(format!("{:?}", e.kind())),
                );
                ModuleError::new(ErrorCode::GeneralInternalError, err.to_string())
                    .with_details(details)
                    .with_ai_guidance(format!("I/O error: {}", e))
            }

            ApexeError::Yaml(_) => {
                ModuleError::new(ErrorCode::GeneralInternalError, err.to_string())
                    .with_ai_guidance(format!("YAML processing error: {}", err))
            }

            ApexeError::Json(_) => {
                ModuleError::new(ErrorCode::GeneralInternalError, err.to_string())
                    .with_ai_guidance(format!("JSON processing error: {}", err))
            }
        }
    }
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
        assert_eq!(err.to_string(), "Command 'git status' timed out after 30s");
    }

    #[test]
    fn test_scan_permission_display() {
        let err = ApexeError::ScanPermission {
            command: "secret-tool".into(),
        };
        assert_eq!(err.to_string(), "Permission denied executing 'secret-tool'");
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

    #[test]
    fn test_apcore_types_accessible() {
        use apcore::{ErrorCode, ModuleError};
        let err = ModuleError::new(ErrorCode::GeneralInternalError, "test");
        assert_eq!(err.code, ErrorCode::GeneralInternalError);
        assert_eq!(err.message, "test");
    }

    #[test]
    fn test_tool_not_found_to_module_error() {
        let apexe_err = ApexeError::ToolNotFound {
            tool_name: "git".into(),
        };
        let module_err: ModuleError = apexe_err.into();
        assert_eq!(module_err.code, ErrorCode::ModuleNotFound);
        assert_eq!(module_err.retryable, Some(false));
        assert!(module_err.ai_guidance.as_ref().unwrap().contains("git"));
        assert!(module_err
            .ai_guidance
            .as_ref()
            .unwrap()
            .contains("not installed"));
        assert_eq!(
            module_err.details.get("tool_name").unwrap(),
            &serde_json::json!("git")
        );
    }

    // F6-T3: ScanError and ParseError
    #[test]
    fn test_scan_error_to_module_error() {
        let err: ModuleError = ApexeError::ScanError("broken".into()).into();
        assert_eq!(err.code, ErrorCode::GeneralInternalError);
        assert!(err.ai_guidance.as_ref().unwrap().contains("scanning error"));
        assert_eq!(
            err.details.get("scan_error").unwrap(),
            &serde_json::json!("broken")
        );
    }

    #[test]
    fn test_parse_error_to_module_error() {
        let err: ModuleError = ApexeError::ParseError("unexpected".into()).into();
        assert_eq!(err.code, ErrorCode::GeneralInternalError);
        assert!(err.ai_guidance.as_ref().unwrap().contains("parsing failed"));
    }

    // F6-T4: ScanTimeout
    #[test]
    fn test_scan_timeout_to_module_error() {
        let err: ModuleError = ApexeError::ScanTimeout {
            command: "git status".into(),
            timeout: 30,
        }
        .into();
        assert_eq!(err.code, ErrorCode::ModuleTimeout);
        assert_eq!(err.retryable, Some(true));
        assert_eq!(
            err.details.get("command").unwrap(),
            &serde_json::json!("git status")
        );
        assert_eq!(
            err.details.get("timeout_seconds").unwrap(),
            &serde_json::json!(30)
        );
        assert!(err.ai_guidance.as_ref().unwrap().contains("too long"));
    }

    // F6-T5: ScanPermission and CommandInjection
    #[test]
    fn test_scan_permission_to_module_error() {
        let err: ModuleError = ApexeError::ScanPermission {
            command: "secret".into(),
        }
        .into();
        assert_eq!(err.code, ErrorCode::AclDenied);
        assert!(err.ai_guidance.as_ref().unwrap().contains("ermission"));
    }

    #[test]
    fn test_command_injection_to_module_error() {
        let err: ModuleError = ApexeError::CommandInjection {
            param_name: "file".into(),
            chars: vec![';', '|'],
        }
        .into();
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert_eq!(
            err.details.get("param_name").unwrap(),
            &serde_json::json!("file")
        );
        assert!(err.details.contains_key("prohibited_chars"));
        assert!(err.ai_guidance.as_ref().unwrap().contains("metacharacters"));
    }

    // F6-T6: Io, Yaml, Json
    #[test]
    fn test_io_error_to_module_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err: ModuleError = ApexeError::Io(io_err).into();
        assert_eq!(err.code, ErrorCode::GeneralInternalError);
        assert!(err.details.contains_key("io_error_kind"));
        assert!(err.ai_guidance.as_ref().unwrap().contains("I/O"));
    }

    #[test]
    fn test_yaml_error_to_module_error() {
        let yaml_err = serde_yaml::from_str::<String>(":\n  :\n  bad").unwrap_err();
        let err: ModuleError = ApexeError::Yaml(yaml_err).into();
        assert_eq!(err.code, ErrorCode::GeneralInternalError);
        assert!(err.ai_guidance.as_ref().unwrap().contains("YAML"));
    }

    #[test]
    fn test_json_error_to_module_error() {
        let json_err = serde_json::from_str::<String>("not json").unwrap_err();
        let err: ModuleError = ApexeError::Json(json_err).into();
        assert_eq!(err.code, ErrorCode::GeneralInternalError);
        assert!(err.ai_guidance.as_ref().unwrap().contains("JSON"));
    }

    // F6-T7: Convenience constructor with trace_id
    #[test]
    fn test_into_module_error_with_trace() {
        let apexe_err = ApexeError::ScanError("broken".into());
        let err = apexe_err.into_module_error_with_trace("abc-123".into());
        assert_eq!(err.trace_id, Some("abc-123".to_string()));
        assert_eq!(err.code, ErrorCode::GeneralInternalError);
        assert!(err.ai_guidance.is_some());
    }

    // F6-T8: Cross-cutting tests
    #[test]
    fn test_all_variants_have_ai_guidance() {
        let variants: Vec<ApexeError> = vec![
            ApexeError::ToolNotFound {
                tool_name: "x".into(),
            },
            ApexeError::ScanError("x".into()),
            ApexeError::ScanTimeout {
                command: "x".into(),
                timeout: 1,
            },
            ApexeError::ScanPermission {
                command: "x".into(),
            },
            ApexeError::CommandInjection {
                param_name: "x".into(),
                chars: vec![';'],
            },
            ApexeError::ParseError("x".into()),
            ApexeError::Io(std::io::Error::other("x")),
            ApexeError::Yaml(serde_yaml::from_str::<String>(":\n  :\n  bad").unwrap_err()),
            ApexeError::Json(serde_json::from_str::<String>("bad").unwrap_err()),
        ];
        for apexe_err in variants {
            let module_err: ModuleError = apexe_err.into();
            assert!(
                module_err.ai_guidance.is_some(),
                "ai_guidance missing for {:?}",
                module_err.code
            );
        }
    }

    #[test]
    fn test_question_mark_operator_converts() {
        fn produces_apexe_error() -> Result<(), ApexeError> {
            Err(ApexeError::ScanError("test".into()))
        }
        #[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable in test code
        fn consumes_module_error() -> Result<(), ModuleError> {
            produces_apexe_error()?;
            Ok(())
        }
        let result = consumes_module_error();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInternalError);
    }
}
