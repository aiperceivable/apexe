use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) const MANIFEST_NAME: &str = ".apexe-shims.json";

pub(crate) fn default_shim_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".apexe/bin")
}

fn same_directory(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}

pub(crate) fn is_shim_directory(path: &Path) -> bool {
    same_directory(path, &default_shim_dir())
        || std::env::var_os("APEXE_SHIM_DIR")
            .is_some_and(|dir| same_directory(path, Path::new(&dir)))
        || path.join(MANIFEST_NAME).is_file()
}

pub(crate) fn clean_path(path: &std::ffi::OsStr) -> OsString {
    let directories = std::env::split_paths(path).filter(|dir| !is_shim_directory(dir));
    std::env::join_paths(directories).unwrap_or_default()
}

pub(crate) fn search_path() -> OsString {
    clean_path(&std::env::var_os("PATH").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_path_excludes_manifest_directory_and_symlink_alias() {
        let temp = tempfile::tempdir().unwrap();
        let shims = temp.path().join("shims");
        let other = temp.path().join("other");
        std::fs::create_dir(&shims).unwrap();
        std::fs::write(shims.join(MANIFEST_NAME), "{}").unwrap();
        let directories = vec![shims.clone(), other.clone()];
        #[cfg(unix)]
        let directories = {
            let mut directories = directories;
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(&shims, &alias).unwrap();
            directories.push(alias);
            directories
        };
        let path = std::env::join_paths(directories).unwrap();
        assert_eq!(
            std::env::split_paths(&clean_path(&path)).collect::<Vec<_>>(),
            vec![other]
        );
    }
}
