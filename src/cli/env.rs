use super::shell::Shell;
use super::shim::{Manifest, Route, MANIFEST_VERSION};
use crate::config::ApexeConfig;
use crate::module::ModuleFilter;
use crate::runtime_path::MANIFEST_NAME;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Enable, reload or disable governed command shims, or print shell activation.
#[derive(Debug, clap::Args, Serialize, Deserialize)]
pub struct EnvArgs {
    /// Install or update shims and configure automatic activation in new terminals.
    #[arg(long, group = "startup_config", conflicts_with_all = ["reload", "disable"])]
    pub enable: bool,
    /// Remove owned shims and managed shell startup blocks.
    #[arg(long, conflicts_with_all = ["enable", "reload"])]
    pub disable: bool,
    /// Refresh shims with the stored configuration, enabling them if not installed.
    #[arg(long, group = "startup_config", conflicts_with_all = ["enable", "disable"])]
    #[serde(default)]
    pub reload: bool,
    /// Shell startup file to configure instead of the detected default.
    #[arg(long, requires = "startup_config")]
    pub rc_file: Option<PathBuf>,
    /// Directory for command shims (default: <config_dir>/bin).
    #[arg(long)]
    pub shim_dir: Option<PathBuf>,
    #[arg(long)]
    pub bindings_dir: Option<PathBuf>,
    #[arg(long)]
    pub acl: Option<PathBuf>,
    #[arg(long)]
    pub tags: Option<String>,
    #[arg(long)]
    pub prefix: Option<String>,
    /// Shell syntax (activation: sh; enabling: detected from SHELL).
    #[arg(long, value_enum)]
    pub shell: Option<Shell>,
}

impl EnvArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let directory = std::path::absolute(
            self.shim_dir
                .as_deref()
                .unwrap_or(&config.config_dir.join("bin")),
        )?;
        if self.disable {
            return super::shell_init::disable(&directory, || uninstall(&directory));
        }
        if self.reload && directory.join(MANIFEST_NAME).try_exists()? {
            return self.refresh(&directory, config);
        }
        if self.enable || self.reload {
            let manifest = self.plan(config)?;
            return super::shell_init::initialize(
                &directory,
                self.shell,
                self.rc_file.as_deref(),
                || install(&directory, manifest),
            );
        }
        super::shim::read_manifest(&directory)?;
        print_activation(&directory, self.shell.unwrap_or_default())
    }

    fn refresh(mut self, directory: &Path, config: &ApexeConfig) -> anyhow::Result<()> {
        let previous = super::shim::read_manifest(directory)?;
        self.bindings_dir = self.bindings_dir.or(Some(previous.bindings_dir));
        self.acl = self.acl.or(previous.acl);
        self.prefix = self.prefix.or(previous.filter.prefix);
        self.tags = self.tags.or_else(|| {
            previous
                .filter
                .tags
                .filter(|tags| !tags.is_empty())
                .map(|tags| tags.join(","))
        });
        let mut config = config.clone();
        config.bindings_pattern = previous.bindings_pattern;
        let manifest = self.plan(&config)?;
        if self.shell.is_some() || self.rc_file.is_some() {
            super::shell_init::initialize(directory, self.shell, self.rc_file.as_deref(), || {
                install(directory, manifest)
            })
        } else {
            install(directory, manifest)
        }
    }

    fn plan(&self, config: &ApexeConfig) -> anyhow::Result<Manifest> {
        let bindings =
            std::path::absolute(self.bindings_dir.as_deref().unwrap_or(&config.bindings_dir))?;
        let filter = ModuleFilter {
            prefix: self.prefix.clone(),
            tags: self
                .tags
                .as_deref()
                .map(super::parse_tag_list)
                .transpose()?,
        };
        let acl = super::local::acl_path(self.acl.as_deref(), config)?;
        let options = super::local::options(
            config,
            &bindings,
            config.bindings_pattern.as_deref(),
            acl.as_deref(),
            filter.clone(),
        );
        let modules =
            crate::output::load_modules_from_dir(&bindings, config.bindings_pattern.as_deref())?;
        let executor =
            crate::module::registry::build_executor_from_modules(&options, modules.clone())?;
        let commands = collect_routes(&modules, |id| executor.registry().has(id))?;
        Ok(Manifest {
            version: MANIFEST_VERSION,
            executable: std::env::current_exe()?.canonicalize()?,
            bindings_dir: bindings,
            bindings_pattern: config.bindings_pattern.clone(),
            acl: self.acl.as_deref().map(std::path::absolute).transpose()?,
            filter,
            commands,
        })
    }
}

fn collect_routes(
    modules: &[apcore_toolkit::ScannedModule],
    registered: impl Fn(&str) -> bool,
) -> anyhow::Result<BTreeMap<String, Vec<Route>>> {
    let mut commands: BTreeMap<String, Vec<Route>> = BTreeMap::new();
    let mut binaries = BTreeMap::new();
    for module in modules {
        if !registered(&module.module_id) {
            continue;
        }
        if let Err(error) = super::shim::eligible(module) {
            tracing::warn!(module_id = %module.module_id, %error, "Excluding module from shims");
            continue;
        }
        let path = super::shim::command_path(module)?;
        let binary = crate::scanner::resolver::extract_binary_path(&module.target)
            .ok_or_else(|| anyhow::anyhow!("Invalid target"))?;
        if let Some(previous) = binaries.insert(path[0].clone(), binary.to_owned()) {
            anyhow::ensure!(
                previous == binary,
                "Shim name '{}' is claimed by different binaries",
                path[0]
            );
        }
        let routes = commands.entry(path[0].clone()).or_default();
        anyhow::ensure!(
            !routes.iter().any(|route| route.command_path == path),
            "Shim command '{}' is claimed by multiple modules",
            path.join(" ")
        );
        routes.push(Route {
            module_id: module.module_id.clone(),
            command_path: path,
        });
    }
    anyhow::ensure!(
        !commands.is_empty(),
        "No eligible shim modules; rescan using matching authoritative overlays"
    );
    Ok(commands)
}

fn entry_path(directory: &Path, name: &str) -> PathBuf {
    #[cfg(windows)]
    return directory.join(format!("{name}.exe"));
    #[cfg(not(windows))]
    directory.join(name)
}

fn owned(path: &Path, manifest: &Manifest) -> bool {
    #[cfg(unix)]
    return std::fs::read_link(path).ok().as_ref() == Some(&manifest.executable);
    #[cfg(not(unix))]
    std::fs::read(path)
        .ok()
        .zip(std::fs::read(&manifest.executable).ok())
        .is_some_and(|(left, right)| left == right)
}

fn create_entry(executable: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(executable, path);
    #[cfg(not(unix))]
    std::fs::hard_link(executable, path)
}

struct StagingDirectory(PathBuf);
impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn install(directory: &Path, mut manifest: Manifest) -> anyhow::Result<()> {
    let old = if directory.join(MANIFEST_NAME).try_exists()? {
        Some(super::shim::read_manifest(directory)?)
    } else {
        None
    };
    if let Some(old) = &old {
        // Retain missing commands as tombstones: their shims refuse rather
        // than falling through to ungoverned binaries during an upgrade.
        for (name, routes) in &old.commands {
            manifest
                .commands
                .entry(name.clone())
                .or_insert_with(|| routes.clone());
        }
    }
    verify_destinations(directory, &manifest, old.as_ref())?;
    std::fs::create_dir_all(directory)?;
    let stage = StagingDirectory(directory.join(format!(".install-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&stage.0)?;
    for name in manifest.commands.keys() {
        create_entry(&manifest.executable, &entry_path(&stage.0, name))?;
    }
    std::fs::write(
        stage.0.join(MANIFEST_NAME),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    for name in manifest.commands.keys() {
        let destination = entry_path(directory, name);
        #[cfg(windows)]
        if destination.exists() {
            std::fs::remove_file(&destination)?;
        }
        std::fs::rename(entry_path(&stage.0, name), destination)?;
    }
    std::fs::rename(stage.0.join(MANIFEST_NAME), directory.join(MANIFEST_NAME))?;
    tracing::info!(directory = %directory.display(), count = manifest.commands.len(), "Installed governed command shims");
    Ok(())
}

fn verify_destinations(
    directory: &Path,
    manifest: &Manifest,
    old: Option<&Manifest>,
) -> anyhow::Result<()> {
    for name in manifest.commands.keys() {
        anyhow::ensure!(
            super::shim::valid_name(name),
            "Invalid command name in manifest"
        );
        let path = entry_path(directory, name);
        if std::fs::symlink_metadata(&path).is_ok() {
            anyhow::ensure!(
                old.is_some_and(
                    |manifest| manifest.commands.contains_key(name) && owned(&path, manifest)
                ),
                "Refusing to overwrite unowned shim path {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn uninstall(directory: &Path) -> anyhow::Result<()> {
    let manifest = super::shim::read_manifest(directory)?;
    for name in manifest.commands.keys() {
        anyhow::ensure!(
            super::shim::valid_name(name),
            "Invalid command name in manifest"
        );
        let path = entry_path(directory, name);
        if std::fs::symlink_metadata(&path).is_ok() {
            anyhow::ensure!(
                owned(&path, &manifest),
                "Refusing to remove unowned path {}",
                path.display()
            );
        }
    }
    for name in manifest.commands.keys() {
        let path = entry_path(directory, name);
        if std::fs::symlink_metadata(&path).is_ok() {
            std::fs::remove_file(path)?;
        }
    }
    std::fs::remove_file(directory.join(MANIFEST_NAME))?;
    Ok(())
}

fn print_activation(directory: &Path, shell: Shell) -> anyhow::Result<()> {
    use std::io::Write;
    let mut paths: Vec<PathBuf> =
        std::env::split_paths(&crate::runtime_path::search_path()).collect();
    paths.retain(|path| path != directory);
    paths.insert(0, directory.into());
    let path = std::env::join_paths(paths)?;
    let path = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("PATH must be valid UTF-8 to print shell activation"))?;
    let directory = directory
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Shim directory must be valid UTF-8"))?;
    let output = match shell {
        Shell::Sh | Shell::Bash | Shell::Zsh => format!(
            "export APEXE_SHIM_DIR={};\nexport PATH={};",
            shell_words::quote(directory),
            shell_words::quote(path)
        ),
        Shell::Fish => format!(
            "set -gx APEXE_SHIM_DIR {};\nset -gx PATH {};",
            shell_words::quote(directory),
            paths_for_fish(path)
        ),
        Shell::Powershell => format!(
            "$env:APEXE_SHIM_DIR = '{}';\n$env:PATH = '{}';",
            directory.replace('\'', "''"),
            path.replace('\'', "''")
        ),
    };
    writeln!(std::io::stdout().lock(), "{output}")?;
    Ok(())
}

fn paths_for_fish(path: &str) -> String {
    std::env::split_paths(path)
        .map(|part| shell_words::quote(&part.to_string_lossy()).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}
