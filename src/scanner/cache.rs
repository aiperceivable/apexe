use std::path::PathBuf;

use crate::models::ScannedCLITool;

/// Filesystem cache for scan results.
pub struct ScanCache {
    cache_dir: PathBuf,
}

impl ScanCache {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Retrieve cached scan result. Returns None on cache miss or corruption.
    pub fn get(&self, tool_name: &str, tool_version: Option<&str>) -> Option<ScannedCLITool> {
        let key = format!(
            "{}_{}.scan.json",
            tool_name,
            tool_version.unwrap_or("unknown")
        );
        let path = self.cache_dir.join(&key);

        let contents = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&contents).ok()
    }

    /// Store scan result in cache.
    pub fn put(&self, tool: &ScannedCLITool) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let key = format!(
            "{}_{}.scan.json",
            tool.name,
            tool.version.as_deref().unwrap_or("unknown")
        );
        let path = self.cache_dir.join(&key);
        let json = serde_json::to_string_pretty(tool)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Remove all cached results for a given tool name.
    pub fn invalidate(&self, tool_name: &str) {
        if let Ok(entries) = std::fs::read_dir(&self.cache_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&format!("{tool_name}_"))
                    && name_str.ends_with(".scan.json")
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::StructuredOutputInfo;
    use tempfile::TempDir;

    fn make_tool(name: &str, version: Option<&str>) -> ScannedCLITool {
        ScannedCLITool {
            name: name.into(),
            binary_path: format!("/usr/bin/{name}"),
            version: version.map(|v| v.to_string()),
            subcommands: vec![],
            global_flags: vec![],
            structured_output: StructuredOutputInfo::default(),
            scan_tier: 1,
            warnings: vec![],
        }
    }

    // T33: store and retrieve
    #[test]
    fn test_cache_put_and_get() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let tool = make_tool("git", Some("2.43.0"));
        cache.put(&tool).unwrap();

        let cached = cache.get("git", Some("2.43.0"));
        assert!(cached.is_some());
        let cached = cached.unwrap();
        assert_eq!(cached.name, "git");
        assert_eq!(cached.version, Some("2.43.0".into()));
    }

    #[test]
    fn test_cache_miss_wrong_version() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let tool = make_tool("git", Some("2.43.0"));
        cache.put(&tool).unwrap();

        let cached = cache.get("git", Some("2.44.0"));
        assert!(cached.is_none());
    }

    #[test]
    fn test_cache_miss_empty() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let cached = cache.get("git", Some("2.43.0"));
        assert!(cached.is_none());
    }

    #[test]
    fn test_cache_unknown_version() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let tool = make_tool("mytool", None);
        cache.put(&tool).unwrap();

        let cached = cache.get("mytool", None);
        assert!(cached.is_some());
    }

    // T34: invalidation
    #[test]
    fn test_cache_invalidate() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let tool1 = make_tool("git", Some("2.43.0"));
        let tool2 = make_tool("git", Some("2.44.0"));
        cache.put(&tool1).unwrap();
        cache.put(&tool2).unwrap();

        // Both should be cached
        assert!(cache.get("git", Some("2.43.0")).is_some());
        assert!(cache.get("git", Some("2.44.0")).is_some());

        // Invalidate
        cache.invalidate("git");

        // Both should be gone
        assert!(cache.get("git", Some("2.43.0")).is_none());
        assert!(cache.get("git", Some("2.44.0")).is_none());
    }

    #[test]
    fn test_cache_invalidate_empty_no_error() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());
        // Should not panic or error
        cache.invalidate("nonexistent");
    }

    #[test]
    fn test_cache_invalidate_preserves_other_tools() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let git = make_tool("git", Some("2.43.0"));
        let docker = make_tool("docker", Some("24.0.0"));
        cache.put(&git).unwrap();
        cache.put(&docker).unwrap();

        cache.invalidate("git");

        assert!(cache.get("git", Some("2.43.0")).is_none());
        assert!(cache.get("docker", Some("24.0.0")).is_some());
    }
}
