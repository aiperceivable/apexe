use apcore_cli::security::sandbox::{ModuleExecutionError, Sandbox};
use serde_json::Value;

/// Manages subprocess isolation using apcore-cli's Sandbox.
pub struct SandboxManager {
    sandbox: Sandbox,
}

impl SandboxManager {
    /// Create a new `SandboxManager`.
    ///
    /// # Arguments
    /// * `enabled`    - whether subprocess isolation is active
    /// * `timeout_ms` - subprocess timeout in milliseconds
    pub fn new(enabled: bool, timeout_ms: u64) -> Self {
        Self {
            sandbox: Sandbox::new(enabled, timeout_ms),
        }
    }

    /// Return `true` when subprocess isolation is enabled.
    pub fn is_enabled(&self) -> bool {
        self.sandbox.is_enabled()
    }

    /// Execute a module, optionally in an isolated subprocess.
    pub async fn execute(
        &self,
        module_id: &str,
        input_data: Value,
    ) -> Result<Value, ModuleExecutionError> {
        self.sandbox.execute(module_id, input_data).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_new_enabled() {
        let mgr = SandboxManager::new(true, 5000);
        assert!(mgr.is_enabled());
    }

    #[test]
    fn test_sandbox_new_disabled() {
        let mgr = SandboxManager::new(false, 5000);
        assert!(!mgr.is_enabled());
    }
}
