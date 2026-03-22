use std::collections::HashMap;

use super::loader::LoadedBinding;

/// Registry of loaded MCP tools, indexed by module_id.
#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, LoadedBinding>,
}

impl ToolRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Build a registry from a list of loaded bindings.
    pub fn from_bindings(bindings: Vec<LoadedBinding>) -> Self {
        let mut registry = Self::new();
        for binding in bindings {
            registry.register(binding);
        }
        registry
    }

    /// Register a single binding. Overwrites if the module_id already exists.
    pub fn register(&mut self, binding: LoadedBinding) {
        self.tools.insert(binding.module_id.clone(), binding);
    }

    /// Look up a tool by module_id.
    pub fn get(&self, module_id: &str) -> Option<&LoadedBinding> {
        self.tools.get(module_id)
    }

    /// List all registered tools.
    pub fn list(&self) -> Vec<&LoadedBinding> {
        self.tools.values().collect()
    }

    /// Return the number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap as StdHashMap;

    fn make_binding(module_id: &str) -> LoadedBinding {
        LoadedBinding {
            module_id: module_id.to_string(),
            description: format!("Description for {module_id}"),
            input_schema: json!({"type": "object", "properties": {}}),
            output_schema: json!({"type": "object"}),
            annotations: StdHashMap::new(),
            tool_command: vec!["echo".to_string()],
            tool_binary: "echo".to_string(),
            timeout: 30,
            json_flag: None,
        }
    }

    #[test]
    fn test_new_registry_is_empty() {
        let reg = ToolRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.list().is_empty());
    }

    #[test]
    fn test_register_and_get() {
        let mut reg = ToolRegistry::new();
        reg.register(make_binding("cli.echo"));

        assert_eq!(reg.len(), 1);
        let tool = reg.get("cli.echo").unwrap();
        assert_eq!(tool.module_id, "cli.echo");
    }

    #[test]
    fn test_from_bindings() {
        let bindings = vec![
            make_binding("cli.git.status"),
            make_binding("cli.git.commit"),
        ];
        let reg = ToolRegistry::from_bindings(bindings);

        assert_eq!(reg.len(), 2);
        assert!(reg.get("cli.git.status").is_some());
        assert!(reg.get("cli.git.commit").is_some());
    }

    #[test]
    fn test_get_nonexistent() {
        let reg = ToolRegistry::new();
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn test_list_returns_all() {
        let bindings = vec![
            make_binding("cli.a"),
            make_binding("cli.b"),
            make_binding("cli.c"),
        ];
        let reg = ToolRegistry::from_bindings(bindings);

        let list = reg.list();
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn test_duplicate_id_overwrites() {
        let mut reg = ToolRegistry::new();
        let mut b1 = make_binding("cli.echo");
        b1.description = "First".to_string();
        let mut b2 = make_binding("cli.echo");
        b2.description = "Second".to_string();

        reg.register(b1);
        reg.register(b2);

        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("cli.echo").unwrap().description, "Second");
    }

    #[test]
    fn test_from_empty_bindings() {
        let reg = ToolRegistry::from_bindings(vec![]);
        assert!(reg.is_empty());
    }
}
