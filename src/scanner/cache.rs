use std::path::PathBuf;

use crate::models::{ScannedCLITool, ToolVariant};

/// Filesystem cache for scan results.
pub struct ScanCache {
    cache_dir: PathBuf,
}

impl ScanCache {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Retrieve cached scan result. Returns None on cache miss or corruption.
    pub fn get(
        &self,
        tool_name: &str,
        variant: ToolVariant,
        tool_version: Option<&str>,
    ) -> Option<ScannedCLITool> {
        let path = self
            .cache_dir
            .join(cache_key(tool_name, variant, tool_version));

        let contents = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&contents).ok()
    }

    /// Store scan result in cache.
    pub fn put(&self, tool: &ScannedCLITool) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let path =
            self.cache_dir
                .join(cache_key(&tool.name, tool.variant, tool.version.as_deref()));
        let json = serde_json::to_string_pretty(tool)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Remove all cached results for a given tool name, across every variant.
    pub fn invalidate(&self, tool_name: &str) {
        if let Ok(entries) = std::fs::read_dir(&self.cache_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                // Keys are "{name}@{variant}_{version}.scan.json". Compare the
                // exact name (stem minus the trailing "_{version}" and the
                // "@{variant}" suffix), not a bare prefix — otherwise
                // invalidate("foo") would also delete a sibling tool literally
                // named "foo_bar" (foo_bar@unknown_2.0.scan.json).
                let Some(stem) = name_str.strip_suffix(".scan.json") else {
                    continue;
                };
                if let Some((keyed_name, _version)) = stem.rsplit_once('_') {
                    let cached_name = keyed_name.split('@').next().unwrap_or(keyed_name);
                    if cached_name == tool_name {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

/// How this build parses a tool. Bump whenever a scanner change alters the
/// result for an unchanged binary.
///
/// A cache entry records what apexe *understood*, not just what the tool is, so
/// the key has to name both. Without this component an upgrade was invisible to
/// the cache: every field added to the scanned model is `#[serde(default)]`, so
/// a document written by an older build still deserializes and is returned
/// before any parsing runs. A user who upgraded and re-ran `apexe scan git`
/// got the pre-fix result — overstriked documentation, a flag named
/// `--exec-path[`, a property keyed `unknown`, subcommands carrying the tool's
/// global flags — at exit 0, with nothing saying the new scanner never ran.
/// `--no-cache` was the only escape and nothing pointed at it.
///
/// History:
/// - 1: 0.6.0 and earlier.
/// - 2: man-page parsing at Tier 1, overstrike stripping at capture, option
///   name/placeholder repairs, and operand/flag placement.
const SCAN_FORMAT_VERSION: u32 = 2;

/// Build the on-disk cache file name for one scan result.
///
/// The variant is part of the key because a command name does not identify a
/// program: a machine can hold BSD `/bin/ls` and Homebrew's GNU `ls` at the
/// same version-less identity, and serving one's flags for the other would be
/// worse than a cache miss. [`SCAN_FORMAT_VERSION`] is part of it for the
/// mirror-image reason: the same binary parsed by a different scanner is a
/// different result.
fn cache_key(tool_name: &str, variant: ToolVariant, tool_version: Option<&str>) -> String {
    format!(
        "{}@{}_{}_v{}.scan.json",
        tool_name,
        variant.as_str(),
        tool_version.unwrap_or("unknown"),
        SCAN_FORMAT_VERSION
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::StructuredOutputInfo;
    use tempfile::TempDir;

    fn make_tool(name: &str, version: Option<&str>) -> ScannedCLITool {
        ScannedCLITool {
            name: name.into(),
            description: String::new(),
            binary_path: format!("/usr/bin/{name}"),
            version: version.map(|v| v.to_string()),
            subcommands: vec![],
            global_flags: vec![],
            structured_output: StructuredOutputInfo::default(),
            scan_tier: 1,
            warnings: vec![],
            ..Default::default()
        }
    }

    // T33: store and retrieve
    #[test]
    fn test_cache_put_and_get() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let tool = make_tool("git", Some("2.43.0"));
        cache.put(&tool).unwrap();

        let cached = cache.get("git", ToolVariant::Unknown, Some("2.43.0"));
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

        let cached = cache.get("git", ToolVariant::Unknown, Some("2.44.0"));
        assert!(cached.is_none());
    }

    #[test]
    fn test_cache_miss_empty() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let cached = cache.get("git", ToolVariant::Unknown, Some("2.43.0"));
        assert!(cached.is_none());
    }

    #[test]
    fn test_cache_unknown_version() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let tool = make_tool("mytool", None);
        cache.put(&tool).unwrap();

        let cached = cache.get("mytool", ToolVariant::Unknown, None);
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
        assert!(cache
            .get("git", ToolVariant::Unknown, Some("2.43.0"))
            .is_some());
        assert!(cache
            .get("git", ToolVariant::Unknown, Some("2.44.0"))
            .is_some());

        // Invalidate
        cache.invalidate("git");

        // Both should be gone
        assert!(cache
            .get("git", ToolVariant::Unknown, Some("2.43.0"))
            .is_none());
        assert!(cache
            .get("git", ToolVariant::Unknown, Some("2.44.0"))
            .is_none());
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

        assert!(cache
            .get("git", ToolVariant::Unknown, Some("2.43.0"))
            .is_none());
        assert!(cache
            .get("docker", ToolVariant::Unknown, Some("24.0.0"))
            .is_some());
    }

    #[test]
    fn test_cache_invalidate_does_not_delete_prefix_sibling() {
        // invalidate("foo") must not delete a distinct tool named "foo_bar",
        // whose key "foo_bar_2.0.scan.json" shares the "foo_" prefix.
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        cache.put(&make_tool("foo", Some("1.0"))).unwrap();
        cache.put(&make_tool("foo_bar", Some("2.0"))).unwrap();

        cache.invalidate("foo");

        assert!(
            cache
                .get("foo", ToolVariant::Unknown, Some("1.0"))
                .is_none(),
            "foo not invalidated"
        );
        assert!(
            cache
                .get("foo_bar", ToolVariant::Unknown, Some("2.0"))
                .is_some(),
            "sibling foo_bar wrongly deleted by invalidate(foo)"
        );
    }

    #[test]
    fn test_cache_separates_variants_of_the_same_command() {
        // BSD /bin/ls and Homebrew GNU ls can coexist under the same command
        // name and both report no version. Serving one's flags for the other
        // would be worse than a cache miss, so the variant is part of the key.
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let mut bsd = make_tool("ls", None);
        bsd.variant = ToolVariant::Bsd;
        bsd.binary_path = "/bin/ls".to_string();
        let mut gnu = make_tool("ls", None);
        gnu.variant = ToolVariant::Gnu;
        gnu.binary_path = "/opt/homebrew/bin/ls".to_string();
        cache.put(&bsd).unwrap();
        cache.put(&gnu).unwrap();

        assert_eq!(
            cache.get("ls", ToolVariant::Bsd, None).unwrap().binary_path,
            "/bin/ls"
        );
        assert_eq!(
            cache.get("ls", ToolVariant::Gnu, None).unwrap().binary_path,
            "/opt/homebrew/bin/ls"
        );
    }

    #[test]
    fn test_cache_invalidate_clears_every_variant() {
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        let mut bsd = make_tool("ls", None);
        bsd.variant = ToolVariant::Bsd;
        let mut gnu = make_tool("ls", None);
        gnu.variant = ToolVariant::Gnu;
        cache.put(&bsd).unwrap();
        cache.put(&gnu).unwrap();

        cache.invalidate("ls");

        assert!(cache.get("ls", ToolVariant::Bsd, None).is_none());
        assert!(cache.get("ls", ToolVariant::Gnu, None).is_none());
    }

    #[test]
    fn test_cache_key_includes_command_variant_version_and_scan_format() {
        assert_eq!(
            cache_key("ls", ToolVariant::Bsd, Some("9.4")),
            format!("ls@bsd_9.4_v{SCAN_FORMAT_VERSION}.scan.json")
        );
        assert_eq!(
            cache_key("ls", ToolVariant::Unknown, None),
            format!("ls@unknown_unknown_v{SCAN_FORMAT_VERSION}.scan.json")
        );
    }

    #[test]
    fn test_a_cache_entry_from_an_older_scanner_is_not_reused() {
        // Regression: the key named only the tool, so an entry written by a
        // build with different parsing was served unchanged. Every new model
        // field is `#[serde(default)]`, so the stale document deserializes
        // cleanly and the fixed scanner never runs — the user sees the old
        // result at exit 0 with no warning.
        let tmp = TempDir::new().unwrap();
        let cache = ScanCache::new(tmp.path().to_path_buf());

        // Write a document under the previous key shape, by hand.
        let stale = tmp
            .path()
            .join(format!("ls@bsd_9.4_v{}.scan.json", SCAN_FORMAT_VERSION - 1));
        std::fs::write(
            &stale,
            serde_json::to_string(&ScannedCLITool {
                name: "ls".to_string(),
                description: "stale".to_string(),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();

        assert!(
            cache.get("ls", ToolVariant::Bsd, Some("9.4")).is_none(),
            "an entry produced by a different scanner version must miss"
        );
    }
}
