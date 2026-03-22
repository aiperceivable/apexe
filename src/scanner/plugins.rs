use std::path::Path;

use tracing::warn;

use crate::scanner::protocol::CliParser;

/// Discover parser plugins from a plugins directory.
///
/// Scans the given directory for .so/.dylib files that export parser plugins.
/// Currently returns an empty vec -- dynamic loading is deferred to a future release.
///
/// Returns an empty vec when:
/// - The directory does not exist
/// - The directory is empty
/// - No valid plugin files are found
pub fn discover_parser_plugins(plugins_dir: &Path) -> Vec<Box<dyn CliParser>> {
    if !plugins_dir.exists() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(plugins_dir) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(path = %plugins_dir.display(), "Failed to read plugins directory: {e}");
            return Vec::new();
        }
    };

    let mut _plugin_files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext == "so" || ext == "dylib" {
            _plugin_files.push(path);
        }
    }

    // Dynamic loading deferred -- would use libloading here
    // For each plugin file:
    //   let lib = unsafe { libloading::Library::new(&path) }?;
    //   let create: Symbol<extern "C" fn() -> Box<dyn CliParser>> =
    //       unsafe { lib.get(b"create_parser") }?;
    //   plugins.push(create());

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // T35: discover_parser_plugins
    #[test]
    fn test_discover_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let plugins = discover_parser_plugins(tmp.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn test_discover_nonexistent_dir() {
        let plugins = discover_parser_plugins(Path::new("/tmp/nonexistent_apexe_plugins_dir"));
        assert!(plugins.is_empty());
    }

    #[test]
    fn test_discover_dir_with_non_plugin_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("readme.txt"), "not a plugin").unwrap();
        std::fs::write(tmp.path().join("config.yaml"), "not a plugin").unwrap();
        let plugins = discover_parser_plugins(tmp.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn test_discover_returns_vec_of_cli_parser() {
        let tmp = TempDir::new().unwrap();
        let plugins: Vec<Box<dyn CliParser>> = discover_parser_plugins(tmp.path());
        // Type check: the return type is correct
        assert!(plugins.is_empty());
    }
}
