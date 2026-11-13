use super::sh::ENTRY_NAME;
use super::shell::Shell;
use super::shim::{Manifest, Route, MANIFEST_VERSION};
use crate::config::ApexeConfig;
use crate::module::ModuleFilter;
use crate::runtime_path::MANIFEST_NAME;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod check;

/// Enable, reload, disable or check governed shims, or print shell activation.
#[derive(Debug, clap::Args, Serialize, Deserialize)]
#[command(
    after_help = "Examples (default paths, bash/zsh):\n  apexe env --enable                         Install reviewed command shims\n  source ~/.apexe/bin/apexe-activate.sh        Activate in this terminal\n  apexe env --check                           Check installation and activation\n  source ~/.apexe/bin/apexe-deactivate.sh      Remove shims and restore this terminal\n\nEnabling cannot change the parent shell; source the activation helper.\nPATH shims cover eligible external commands; builtins and absolute paths can bypass them."
)]
pub struct EnvArgs {
    /// Install or update shims and configure automatic activation in new terminals.
    #[arg(long, group = "startup_config", conflicts_with_all = ["reload", "disable"])]
    pub enable: bool,
    /// Remove owned shims and managed shell startup blocks.
    #[arg(long, conflicts_with_all = ["enable", "reload"])]
    pub disable: bool,
    /// Replace the entry point and activation scripts when the record that
    /// proves this apexe owns them is gone.
    ///
    /// Files this apexe provably created — links to its own executable — are
    /// always recovered. This flag additionally allows replacing apexe's
    /// reserved names (`apexe-sh`, the activation scripts) when nothing proves
    /// they are apexe's. Command shims are never replaced by it.
    #[arg(long)]
    #[serde(default)]
    pub replace_unowned: bool,
    /// Refresh shims with the stored configuration, enabling them if not installed.
    #[arg(long, group = "startup_config", conflicts_with_all = ["enable", "disable"])]
    #[serde(default)]
    pub reload: bool,
    /// Check the installed shim environment without executing commands.
    #[arg(long, conflicts_with_all = ["enable", "reload", "disable", "bindings_dir", "acl", "tags", "prefix", "shell"])]
    #[serde(default)]
    pub check: bool,
    /// Install shims without touching shell startup files.
    ///
    /// The managed blocks put the shim directory first on `PATH` in every new
    /// terminal, where login-time tooling (pyenv, nvm, brew) then resolves its
    /// own `rm`/`ln`/`sort` through the shims and fails wherever the policy
    /// denies them. Use this for a host that reaches apexe through `apexe-sh`
    /// (`apexe agent <host> --write`), so only the agent's shell is governed.
    /// Blocks a previous installation left behind are removed.
    #[arg(long, requires = "startup_config", conflicts_with = "rc_file")]
    #[serde(default)]
    pub no_rc: bool,
    /// Shell startup file to configure instead of the detected default.
    #[arg(long, requires = "startup_config")]
    pub rc_file: Option<PathBuf>,
    /// Directory for command shims (default: <config_dir>/bin).
    #[arg(long)]
    pub shim_dir: Option<PathBuf>,
    /// Directory containing binding files (default: configured bindings directory).
    #[arg(long, value_name = "DIR")]
    pub bindings_dir: Option<PathBuf>,
    /// ACL policy file; defaults to <config_dir>/acl.yaml when present.
    #[arg(long, value_name = "PATH")]
    pub acl: Option<PathBuf>,
    /// Include only modules carrying every comma-separated tag.
    #[arg(long, value_name = "TAGS")]
    pub tags: Option<String>,
    /// Include only modules whose ID starts with this prefix.
    #[arg(long, value_name = "PREFIX")]
    pub prefix: Option<String>,
    /// Admit these shim commands even when the scanned policy excludes them.
    ///
    /// Comma-separated command names (`node,python3`), matched against the name
    /// the shim installs, not the module id. Use it to govern an interpreter
    /// under `--scanned readonly`, which admits only readonly commands.
    #[arg(long, value_name = "NAMES")]
    pub also: Option<String>,
    /// Refuse these shim commands even when the policy would admit them.
    #[arg(long, value_name = "NAMES")]
    pub exclude: Option<String>,
    /// Include bindings without a reviewed overlay: off, readonly, or all.
    ///
    /// Scanned routes execute the caller's original argv after strict schema
    /// validation; they never rebuild and reorder the command line. `readonly`
    /// admits only modules annotated readonly, non-destructive and closed-world.
    #[arg(long, value_enum)]
    pub scanned: Option<super::ScannedPolicy>,
    /// Shell syntax (activation: sh; enabling: detected from SHELL).
    #[arg(long, value_enum)]
    pub shell: Option<Shell>,
}

impl EnvArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        if self.check {
            let directory = self
                .shim_dir
                .or_else(|| std::env::var_os("APEXE_SHIM_DIR").map(PathBuf::from))
                .unwrap_or_else(|| config.config_dir.join("bin"));
            return check::execute(&std::path::absolute(directory)?, config);
        }
        let directory = std::path::absolute(
            self.shim_dir
                .as_deref()
                .unwrap_or(&config.config_dir.join("bin")),
        )?;
        if self.disable {
            let installed = super::shim::read_manifest(&directory).ok().map(|manifest| {
                (
                    manifest.commands.len(),
                    manifest.commands.values().map(Vec::len).sum::<usize>(),
                )
            });
            let result = super::shell_init::disable(&directory, || uninstall(&directory));
            if result.is_ok() {
                super::agent::warn_if_host_depends_on_removed_shims(&directory);
                report_disable(config, &directory, installed);
            }
            return result;
        }
        if self.reload && directory.join(MANIFEST_NAME).try_exists()? {
            return self.refresh(&directory, config);
        }
        if self.enable || self.reload {
            let (manifest, also_overrides) = self.plan(config)?;
            let heuristic = self
                .enable
                .then(|| heuristic_only_commands(&manifest.commands));
            super::shell_init::initialize(
                &directory,
                self.shell,
                self.rc_file.as_deref(),
                !self.no_rc,
                || install(&directory, manifest, self.replace_unowned),
            )?;
            // Only on the explicit installation. `--reload` also runs from the
            // activation script of every new terminal, where a host hint list
            // would repeat itself for no new information.
            if self.enable {
                super::agent::report_unconfigured_hosts(&directory);
                report_acl_coverage(&directory, config);
                if let Some(heuristic) = heuristic {
                    report_heuristic_coverage(&heuristic);
                }
                report_also_overrides(&also_overrides);
            }
            return Ok(());
        }
        super::shim::read_manifest(&directory)?;
        print_activation(&directory, self.shell.unwrap_or_default())
    }

    fn refresh(mut self, directory: &Path, config: &ApexeConfig) -> anyhow::Result<()> {
        let previous = super::shim::read_manifest(directory)?;
        self.bindings_dir = self.bindings_dir.or(Some(previous.bindings_dir));
        self.acl = self.acl.or(previous.acl);
        self.scanned = self.scanned.or(Some(previous.scanned));
        self.prefix = self.prefix.or(previous.filter.prefix);
        if self.also.is_none() && !previous.filter.also.is_empty() {
            self.also = Some(previous.filter.also.join(","));
        }
        if self.exclude.is_none() && !previous.filter.exclude.is_empty() {
            self.exclude = Some(previous.filter.exclude.join(","));
        }
        self.tags = self.tags.or_else(|| {
            previous
                .filter
                .tags
                .filter(|tags| !tags.is_empty())
                .map(|tags| tags.join(","))
        });
        let mut config = config.clone();
        config.bindings_pattern = previous.bindings_pattern;
        let (manifest, _) = self.plan(&config)?;
        if self.shell.is_some() || self.rc_file.is_some() {
            super::shell_init::initialize(
                directory,
                self.shell,
                self.rc_file.as_deref(),
                !self.no_rc,
                || install(directory, manifest, self.replace_unowned),
            )
        } else {
            install(directory, manifest, self.replace_unowned)
        }
    }

    fn plan(&self, config: &ApexeConfig) -> anyhow::Result<(Manifest, Vec<AlsoOverride>)> {
        let bindings =
            std::path::absolute(self.bindings_dir.as_deref().unwrap_or(&config.bindings_dir))?;
        let filter = ModuleFilter {
            prefix: self.prefix.clone(),
            tags: self
                .tags
                .as_deref()
                .map(super::parse_tag_list)
                .transpose()?,
            also: self
                .also
                .as_deref()
                .map(|raw| super::parse_command_list(raw, "--also"))
                .transpose()?
                .unwrap_or_default(),
            exclude: self
                .exclude
                .as_deref()
                .map(|raw| super::parse_command_list(raw, "--exclude"))
                .transpose()?
                .unwrap_or_default(),
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
        let CollectedRoutes {
            commands,
            overrides: also_overrides,
        } = collect_routes(
            &modules,
            |id| executor.registry().has(id),
            self.scanned.unwrap_or_default(),
            &filter,
        )?;
        Ok((
            Manifest {
                version: MANIFEST_VERSION,
                executable: std::env::current_exe()?.canonicalize()?,
                bindings_dir: bindings,
                bindings_pattern: config.bindings_pattern.clone(),
                acl: self.acl.as_deref().map(std::path::absolute).transpose()?,
                filter,
                commands,
                scanned: self.scanned.unwrap_or_default(),
                activation_script: None,
                deactivation_script: None,
                source_scripts_dir: None,
            },
            also_overrides,
        ))
    }
}

/// A command the installation admits only because `--also` named it.
///
/// `--scanned readonly` admits a module when its annotations say readonly,
/// non-destructive and closed-world. `--also` overrides that test by name, so
/// the installation can hold a command the policy would refuse while its
/// manifest still says `readonly`. Recording the reason lets `env --enable`
/// and `env --check` state the override instead of leaving the label to be
/// believed.
#[derive(Debug)]
pub(super) struct AlsoOverride {
    pub command: String,
    pub module_id: String,
    pub reason: &'static str,
}

/// The routes an installation exposes, plus the admissions `--also` forced.
#[derive(Debug)]
struct CollectedRoutes {
    commands: BTreeMap<String, Vec<Route>>,
    overrides: Vec<AlsoOverride>,
}

fn collect_routes(
    modules: &[apcore_toolkit::ScannedModule],
    registered: impl Fn(&str) -> bool,
    scanned: super::ScannedPolicy,
    filter: &ModuleFilter,
) -> anyhow::Result<CollectedRoutes> {
    let mut commands: BTreeMap<String, Vec<Route>> = BTreeMap::new();
    let mut binaries = BTreeMap::new();
    let mut overrides: Vec<AlsoOverride> = Vec::new();
    for module in modules {
        if !registered(&module.module_id) {
            continue;
        }
        // The policy skips a binding it cannot route (`scanned_admits` fails on
        // the same check), so aborting the whole installation over one malformed
        // file would block every other command it was meant to install.
        let path = match super::shim::command_path(module) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(
                    module_id = %module.module_id,
                    %error,
                    "Skipping binding without a usable command path"
                );
                continue;
            }
        };
        if filter.exclude.iter().any(|name| name == &path[0]) {
            tracing::info!(
                command = %path[0],
                module_id = %module.module_id,
                "Excluded from shims by --exclude"
            );
            continue;
        }
        // `--also` names the command the operator wants governed. It outranks
        // the annotation gate on purpose: `--scanned readonly` admits only
        // readonly modules, and an interpreter is neither readonly nor
        // destructive, so without this the only way to govern one is to admit
        // every heuristic route with `--scanned all`.
        let named = filter.also.iter().any(|name| name == &path[0]);
        let tier = match super::shim::eligible(module) {
            Ok(()) => super::shim::RouteTier::Verified,
            Err(error) => {
                // Structural eligibility is not negotiable. `--also` names a
                // command to govern; it does not vouch for a binding whose
                // binary is gone or whose scan format is stale. Letting the name
                // override this installed a shim that could only fail, and then
                // reported it as an admission the policy refused.
                if super::shim::scannable(module).is_err() {
                    tracing::warn!(
                        module_id = %module.module_id,
                        %error,
                        "Excluding module from shims: not structurally eligible"
                    );
                    continue;
                }
                let admitted_by_scan = super::shim::scanned_admits(module, scanned);
                if admitted_by_scan || named {
                    // `--also` outranks the annotation gate. Record when it did,
                    // so the installation can report a command the policy would
                    // have refused instead of letting `scanned: readonly` imply
                    // the surface is still readonly.
                    if named && !admitted_by_scan {
                        overrides.push(AlsoOverride {
                            command: path[0].clone(),
                            module_id: module.module_id.clone(),
                            reason: super::shim::scanned_refusal_reason(module, scanned),
                        });
                    }
                    // Admitted, but say which kind of admission this is: a
                    // binding older than the grammar stamp is inherited trust,
                    // not an earned one (issue #47).
                    if crate::models::grammar_tier(&module.metadata)
                        == Some(crate::models::GrammarTier::Legacy)
                    {
                        super::shim::RouteTier::Legacy
                    } else {
                        super::shim::RouteTier::Scanned
                    }
                } else {
                    tracing::warn!(
                        module_id = %module.module_id,
                        %error,
                        ?scanned,
                        "Excluding module from shims"
                    );
                    continue;
                }
            }
        };
        let binary = crate::scanner::resolver::extract_binary_path(&module.target)
            .ok_or_else(|| anyhow::anyhow!("Invalid target"))?;
        if let Some(previous) = binaries.insert(path[0].clone(), binary.clone()) {
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
            tier,
            tombstone: false,
        });
    }
    anyhow::ensure!(
        !commands.is_empty(),
        "No shim-eligible modules; rescan using reviewed overlays or enable --scanned"
    );
    Ok(CollectedRoutes {
        commands,
        overrides,
    })
}

fn entry_path(directory: &Path, name: &str) -> PathBuf {
    #[cfg(windows)]
    return directory.join(format!("{name}.exe"));
    #[cfg(not(windows))]
    directory.join(name)
}

fn owned(path: &Path, manifest: &Manifest) -> bool {
    links_to_executable(path, &manifest.executable)
}

/// Whether `path` is a link to this executable.
///
/// This is the shape of every entry apexe creates, so it proves ownership on its
/// own — without the installation record. The record is what made a lost
/// `.apexe-shims.json` unrecoverable: every shim apexe had installed became
/// "unowned" at once, including links that only apexe would have written.
fn links_to_executable(path: &Path, executable: &Path) -> bool {
    #[cfg(unix)]
    return std::fs::read_link(path).ok().as_deref() == Some(executable);
    #[cfg(not(unix))]
    std::fs::read(path)
        .ok()
        .zip(std::fs::read(executable).ok())
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

fn install(directory: &Path, mut manifest: Manifest, replace_unowned: bool) -> anyhow::Result<()> {
    let persistent =
        super::source_scripts::prepare(directory, &manifest.executable, replace_unowned)?;
    manifest.source_scripts_dir = Some(persistent.directory().to_path_buf());
    let script = super::activation::generate(directory, &manifest.executable)?;
    manifest.activation_script = Some(script.clone());
    let deactivation = super::activation::generate_deactivation(directory)?;
    manifest.deactivation_script = Some(deactivation.clone());
    let old = if directory.join(MANIFEST_NAME).try_exists()? {
        Some(super::shim::read_manifest(directory)?)
    } else {
        None
    };
    retain_removed_routes(&mut manifest, old.as_ref());
    verify_destinations(directory, &manifest, old.as_ref(), replace_unowned)?;
    std::fs::create_dir_all(directory)?;
    let stage = StagingDirectory(directory.join(format!(".install-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&stage.0)?;
    for name in manifest.commands.keys() {
        create_entry(&manifest.executable, &entry_path(&stage.0, name))?;
    }
    create_entry(&manifest.executable, &entry_path(&stage.0, ENTRY_NAME))?;
    std::fs::write(stage.0.join(super::activation::NAME), script)?;
    std::fs::write(
        stage.0.join(super::activation::DEACTIVATE_NAME),
        deactivation,
    )?;
    std::fs::write(
        stage.0.join(MANIFEST_NAME),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    persistent.publish()?;
    publish_entries(directory, &stage.0, &manifest)?;
    remove_stale_entries(directory, &manifest)?;
    report_coverage(&manifest);
    tracing::info!(directory = %directory.display(), count = manifest.commands.len(), "Installed governed command shims");
    persistent.log_paths();
    Ok(())
}

/// Say what disabling removed, and record it.
///
/// `env --disable` takes the whole governance layer down: command shims, the
/// `apexe-sh` entry point and the managed startup blocks. A host configured to
/// use the entry point does not fall back to its own shell, and every other
/// command silently runs ungoverned. Leaving that silent made it the cheapest
/// way for an agent that hit a refusal to unblock itself — the trail showed the
/// refusals and then nothing at all, with no record of who removed the layer.
///
/// The record is deliberately not a refusal: nothing was refused. It names the
/// action so a later reader can tell an uninstall from a broken install.
fn report_disable(config: &ApexeConfig, directory: &Path, installed: Option<(usize, usize)>) {
    use std::io::IsTerminal;
    let audit = crate::governance::AuditManager::new(&config.audit_log);
    audit.log_governance_event(
        "shim_disable",
        serde_json::json!({
            "shim_dir": directory.display().to_string(),
            "commands": installed.map(|(commands, _)| commands),
            "routes": installed.map(|(_, routes)| routes),
        }),
    );
    if !std::io::stderr().is_terminal() {
        return;
    }
    match installed {
        Some((commands, routes)) => eprintln!(
            "Removed {commands} command shim(s) ({routes} route(s)) and the apexe-sh entry point \
             from {}.\nThis shell and every host configured to use it now run commands \
             ungoverned; the action is recorded in {}.",
            directory.display(),
            config.audit_log.display()
        ),
        None => eprintln!(
            "Removed the apexe shim layer from {}; commands now run ungoverned until \
             'apexe env --enable' is run again.",
            directory.display()
        ),
    }
}

/// Name installed shims the loaded ACL refuses.
///
/// An installation and a policy are built from different inputs: `env --enable`
/// admits every route the bindings allow, while the ACL's `default_effect`
/// decides which of them a caller may run. Nothing compared the two, so `cat`
/// got a reviewed shim while the starter ACL listed no rule for it — the shim
/// existed only to be denied, and the refusal arrived later with no hint that
/// the installation itself was the mismatch.
fn report_acl_coverage(directory: &Path, config: &ApexeConfig) {
    use std::io::IsTerminal;
    let Ok(manifest) = super::shim::read_manifest(directory) else {
        return;
    };
    let Ok(Some(acl_path)) = super::local::acl_path(manifest.acl.as_deref(), config) else {
        return;
    };
    let Ok(acl) = crate::governance::AclManager::from_config(&acl_path) else {
        return;
    };
    let module_ids: std::collections::BTreeSet<&str> = manifest
        .commands
        .values()
        .flatten()
        .map(|route| route.module_id.as_str())
        .collect();
    // The shim surface calls as `local` (see `local::context`), so a policy
    // scoped to `callers: ['local']` is the one that decides these calls. Asking
    // `explain` — which evaluates as `@external` — reported every shim as denied
    // the moment an operator scoped their allow rule to the local shell.
    let shim_caller = super::local::LOCAL_CALLER;
    let denied: Vec<&str> = module_ids
        .into_iter()
        .filter(|module_id| acl.explain_for(shim_caller, module_id).effect != "allow")
        .collect();
    if denied.is_empty() || !std::io::stderr().is_terminal() {
        return;
    }
    eprintln!(
        "Installed shims the loaded ACL refuses for caller '{shim_caller}' ({}):",
        acl_path.display()
    );
    for module_id in denied {
        eprintln!(
            "  {module_id}  ->  add an allow rule to {}, or leave the shim uninstalled",
            acl_path.display()
        );
    }
}

#[cfg(test)]
mod acl_coverage_tests {

    /// A rule scoped to `callers: ['local']` decides the shim surface, so the
    /// report must not evaluate as `@external` and call the whole installation
    /// denied. Regression for exactly that: every shim was listed.
    #[test]
    fn test_acl_coverage_evaluates_as_the_local_caller() {
        let root = tempfile::tempdir().unwrap();
        let acl_path = root.path().join("acl.yaml");
        std::fs::write(
            &acl_path,
            "rules:\n- callers: ['*']\n  targets: ['cli.rm']\n  effect: deny\n\
             - callers: ['local']\n  targets: ['*']\n  effect: allow\ndefault_effect: deny\n",
        )
        .unwrap();
        let acl = crate::governance::AclManager::from_config(&acl_path).unwrap();
        assert_eq!(acl.explain_for("local", "cli.ls").effect, "allow");
        assert_eq!(acl.explain_for("local", "cli.rm").effect, "deny");
        // The generic caller keeps the default-deny it always had.
        assert_eq!(acl.explain_external("cli.ls").effect, "deny");
    }
}

/// Name commands the installation holds only because `--also` overrode the
/// scanned policy.
///
/// `--also rm,mv,cp` under `--scanned readonly` installs writers while the
/// manifest keeps saying `readonly`. The override is legitimate (an
/// interpreter is never readonly, and `--also` is the narrow way to govern
/// one), but it must not be silent: the operator set the policy by the flag
/// they typed, so the report has to name what that flag let through.
/// Name the commands whose entire installed surface is heuristic.
///
/// A scanned route executes the caller's original argv after schema validation,
/// which is right for a tool whose help text describes its whole grammar. It is
/// not enough for a tool the scan cannot fully describe: `git rev-parse
/// --show-toplevel` is refused as an undeclared flag, and the shim refuses it
/// rather than forwarding it. The operator meets that as a failed call, one
/// invocation at a time; naming the commands at install time is the only place
/// they can decide to leave one ungoverned instead.
fn report_heuristic_coverage(heuristic: &[(String, usize)]) {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return;
    }
    if heuristic.is_empty() {
        return;
    }
    eprintln!(
        "Commands whose subcommand surface is heuristic only ({}):",
        heuristic.len()
    );
    for (name, count) in heuristic {
        eprintln!(
            "  {name} ({count} route(s))  ->  a route the scan missed is refused, not \
             forwarded; leave it ungoverned with --exclude {name}"
        );
    }
}

/// Commands whose surface is a heuristic subcommand tree, most routes first.
///
/// The single-route case is deliberately left out. A tool with one help block —
/// `wc`, `grep`, `sort` — is described by that block, and the scan reads it in
/// full; the gap this report exists for is the tree, where the scan must find N
/// subcommands and misses the flags of whichever it read least well. Reporting
/// every scanned command produced nineteen lines on an ordinary install and
/// pointed at the ones with least to miss.
fn heuristic_only_commands(commands: &BTreeMap<String, Vec<Route>>) -> Vec<(String, usize)> {
    let mut heuristic: Vec<(String, usize)> = commands
        .iter()
        .filter(|(_, routes)| {
            routes.len() > 1
                && routes
                    .iter()
                    .all(|route| route.tier != super::shim::RouteTier::Verified)
        })
        .map(|(name, routes)| (name.clone(), routes.len()))
        .collect();
    heuristic.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    heuristic
}

fn report_also_overrides(overrides: &[AlsoOverride]) {
    use std::io::IsTerminal;
    if overrides.is_empty() || !std::io::stdout().is_terminal() {
        return;
    }
    println!(
        "{} command(s) installed only because --also named them; the scanned policy would \
         refuse them:",
        overrides.len()
    );
    for entry in overrides {
        println!(
            "  {}: {} ({})",
            entry.command, entry.reason, entry.module_id
        );
    }
    println!(
        "  These run as scanned routes: argv is validated against the scanned grammar and \
         refused when a token is undeclared. A reviewed overlay is the only way to earn \
         reviewed trust for them."
    );
}

/// Print what this installation admits, command by command.
///
/// Silence was the problem: an install that admitted one of `git`'s routes
/// looked identical to one that admitted all of them, and the difference only
/// surfaced later as a refusal. Printing the count next to each command makes
/// partial coverage visible at the moment it is created.
fn report_coverage(manifest: &Manifest) {
    use std::io::IsTerminal;
    // Only when a person is watching: `env --enable` is also how scripts and
    // tests install shims, and a summary line there would land in pipelines and
    // captured output that expect none.
    if manifest.commands.is_empty() || !std::io::stdout().is_terminal() {
        return;
    }
    println!(
        "Installed {} command shim(s) ({} route(s)):",
        manifest.commands.len(),
        manifest.commands.values().map(Vec::len).sum::<usize>()
    );
    for (name, routes) in &manifest.commands {
        let tiers: std::collections::BTreeMap<&str, usize> =
            routes.iter().fold(Default::default(), |mut counts, route| {
                let tier = match route.tier {
                    super::shim::RouteTier::Verified => "reviewed",
                    super::shim::RouteTier::Scanned => "scanned",
                    super::shim::RouteTier::Legacy => "legacy (unstamped, rescan to review)",
                };
                *counts.entry(tier).or_default() += 1;
                counts
            });
        let detail: Vec<String> = tiers
            .into_iter()
            .map(|(tier, count)| format!("{count} {tier}"))
            .collect();
        println!("  {name}: {}", detail.join(", "));
    }
    println!(
        "  Commands or subcommands that are not listed stay unmanaged: their calls are refused, \
         never forwarded to the original binary."
    );
}

fn retain_removed_routes(manifest: &mut Manifest, old: Option<&Manifest>) {
    if let Some(old) = old {
        // A module that is still registered but changed its `command_path`
        // must replace its old route, not keep both: the stale route makes
        // `route_candidates` refuse every invocation of the shim. Only routes
        // whose module disappeared are retained as tombstones.
        let current_ids: std::collections::BTreeSet<String> = manifest
            .commands
            .values()
            .flatten()
            .map(|route| route.module_id.clone())
            .collect();
        // Retain each missing command path as a tombstone, including removed
        // subcommands of a binary that still has other registered routes.
        for (name, routes) in &old.commands {
            // An operator who excluded this command did not lose a module: the
            // route must go, not become a tombstone that keeps the link on disk
            // and refuses every call. `--exclude` says so explicitly.
            if manifest
                .filter
                .exclude
                .iter()
                .any(|excluded| excluded == name)
            {
                continue;
            }
            let current = manifest.commands.entry(name.clone()).or_default();
            for route in routes {
                if current_ids.contains(&route.module_id) {
                    continue;
                }
                if !current
                    .iter()
                    .any(|entry| entry.command_path == route.command_path)
                {
                    let mut retained = route.clone();
                    retained.tombstone = true;
                    current.push(retained);
                }
            }
        }
    }
}

fn publish_entries(directory: &Path, stage: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    for name in manifest.commands.keys() {
        let destination = entry_path(directory, name);
        #[cfg(windows)]
        if destination.exists() {
            std::fs::remove_file(&destination)?;
        }
        std::fs::rename(entry_path(stage, name), destination)?;
    }
    std::fs::rename(
        stage.join(super::activation::NAME),
        directory.join(super::activation::NAME),
    )?;
    std::fs::rename(
        stage.join(super::activation::DEACTIVATE_NAME),
        directory.join(super::activation::DEACTIVATE_NAME),
    )?;
    std::fs::rename(
        entry_path(stage, ENTRY_NAME),
        entry_path(directory, ENTRY_NAME),
    )?;
    std::fs::rename(stage.join(MANIFEST_NAME), directory.join(MANIFEST_NAME))?;
    Ok(())
}

/// Remove command links this installation owns but the manifest no longer lists.
///
/// `env --reload --exclude head` narrows the set without the module disappearing
/// from the bindings, so nothing else removes the link: it stayed on disk and
/// kept resolving to a command the installation had just refused. A *removed
/// module* is a different case and is unaffected — `retain_removed_routes` keeps
/// its route in the manifest as a tombstone, so the link is listed and stays to
/// refuse the call.
fn remove_stale_entries(directory: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if reserved_shim_name(&name) || manifest.commands.contains_key(&name) {
            continue;
        }
        let path = entry.path();
        // Only links this apexe owns: a file an operator put there is theirs.
        if !links_to_executable(&path, &manifest.executable) {
            continue;
        }
        std::fs::remove_file(&path)?;
        tracing::info!(command = %name, "Removed shim the manifest no longer lists");
    }
    Ok(())
}

fn reserved_shim_name(name: &str) -> bool {
    name == ENTRY_NAME
        || name == super::activation::NAME
        || name == super::activation::DEACTIVATE_NAME
        || super::source_scripts::reserved_name(name)
}

fn verify_destinations(
    directory: &Path,
    manifest: &Manifest,
    old: Option<&Manifest>,
    replace_unowned: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !manifest
            .commands
            .keys()
            .any(|name| reserved_shim_name(name)),
        "Shim names used for activation scripts and their ownership record are reserved"
    );
    super::activation::verify_existing(
        directory,
        super::activation::NAME,
        old.and_then(|manifest| manifest.activation_script.as_deref()),
        replace_unowned,
    )?;
    super::activation::verify_existing(
        directory,
        super::activation::DEACTIVATE_NAME,
        old.and_then(|manifest| manifest.deactivation_script.as_deref()),
        replace_unowned,
    )?;
    verify_entry(directory, manifest, old, replace_unowned)?;
    for name in manifest.commands.keys() {
        anyhow::ensure!(
            super::shim::valid_name(name),
            "Invalid command name in manifest"
        );
        let path = entry_path(directory, name);
        if std::fs::symlink_metadata(&path).is_ok() {
            // Owned either because the record knows the name and the file still
            // matches it, or because the file is a link to this executable.
            let claimed = old.is_some_and(|manifest| {
                manifest.commands.contains_key(name) && owned(&path, manifest)
            });
            anyhow::ensure!(
                claimed || links_to_executable(&path, &manifest.executable),
                "Refusing to overwrite unowned shim path {}",
                path.display()
            );
        }
    }
    Ok(())
}

/// Decide whether the entry point may be replaced.
///
/// A link to this executable is always replaceable — that is the launcher, and
/// recreating it destroys nothing. Anything else at that path is the operator's
/// and is only replaced under `--replace-unowned`.
fn verify_entry(
    directory: &Path,
    manifest: &Manifest,
    old: Option<&Manifest>,
    replace_unowned: bool,
) -> anyhow::Result<()> {
    let entry = entry_path(directory, ENTRY_NAME);
    if std::fs::symlink_metadata(&entry).is_err() {
        return Ok(());
    }
    let ours = old.is_some_and(|manifest| owned(&entry, manifest))
        || links_to_executable(&entry, &manifest.executable);
    if ours {
        return Ok(());
    }
    anyhow::ensure!(
        replace_unowned,
        "Refusing to overwrite unowned entry point {}: it is {}, and no installation \
         record ({}) claims it. Run 'apexe env --enable --replace-unowned' to replace \
         it, or move it aside and retry.",
        entry.display(),
        describe_occupant(&entry),
        directory.join(MANIFEST_NAME).display()
    );
    eprintln!(
        "Replacing unowned entry point {} ({})",
        entry.display(),
        describe_occupant(&entry)
    );
    Ok(())
}

/// Name what occupies a path, so a refusal describes the obstacle rather than
/// only the location.
fn describe_occupant(path: &Path) -> String {
    match std::fs::read_link(path) {
        Ok(target) => format!("a symbolic link to {}", target.display()),
        Err(_) => match std::fs::metadata(path) {
            Ok(metadata) => format!("a regular file of {} bytes", metadata.len()),
            Err(error) => format!("unreadable: {error}"),
        },
    }
}

fn uninstall(directory: &Path) -> anyhow::Result<()> {
    let manifest = super::shim::read_manifest(directory)?;
    for (name, script) in manifest_scripts(&manifest) {
        if let Some(script) = script {
            super::activation::verify_existing(directory, name, Some(script), false)?;
        }
    }
    for name in manifest.commands.keys() {
        anyhow::ensure!(
            super::shim::valid_name(name) && !reserved_shim_name(name),
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
    for (name, script) in manifest_scripts(&manifest) {
        if script.is_some() && directory.join(name).try_exists()? {
            std::fs::remove_file(directory.join(name))?;
        }
    }
    let entry = entry_path(directory, ENTRY_NAME);
    if std::fs::symlink_metadata(&entry).is_ok() {
        anyhow::ensure!(
            owned(&entry, &manifest),
            "Refusing to remove unowned entry point {}",
            entry.display()
        );
        std::fs::remove_file(entry)?;
    }
    std::fs::remove_file(directory.join(MANIFEST_NAME))?;
    Ok(())
}

fn manifest_scripts(manifest: &Manifest) -> [(&str, Option<&str>); 2] {
    [
        (
            super::activation::NAME,
            manifest.activation_script.as_deref(),
        ),
        (
            super::activation::DEACTIVATE_NAME,
            manifest.deactivation_script.as_deref(),
        ),
    ]
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
            "{}export APEXE_SHIM_DIR={};\nexport PATH={};\n_APEXE_ACTIVE_SHIM_DIR=\"$APEXE_SHIM_DIR\";",
            super::activation::SAVE_ENVIRONMENT,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::shim::{Manifest, Route, RouteTier};

    fn manifest_with(routes: Vec<Route>) -> Manifest {
        let mut commands = std::collections::BTreeMap::new();
        commands.insert("git".to_string(), routes);
        Manifest {
            version: crate::cli::shim::MANIFEST_VERSION,
            executable: PathBuf::from("/usr/bin/apexe"),
            bindings_dir: PathBuf::from("/tmp/bindings"),
            bindings_pattern: None,
            acl: None,
            filter: crate::module::ModuleFilter::default(),
            commands,
            scanned: crate::cli::ScannedPolicy::Off,
            activation_script: None,
            deactivation_script: None,
            source_scripts_dir: None,
        }
    }

    fn route(module_id: &str, command_path: &[&str]) -> Route {
        Route {
            module_id: module_id.into(),
            command_path: command_path.iter().map(|part| (*part).into()).collect(),
            tier: RouteTier::Verified,
            tombstone: false,
        }
    }

    #[test]
    fn test_heuristic_only_commands_skips_commands_with_a_reviewed_route() {
        let mut commands = std::collections::BTreeMap::new();
        commands.insert(
            "git".to_string(),
            vec![route("cli.git.status", &["git", "status"])],
        );
        let mut guessed = route("cli.git.rev_parse", &["git", "rev_parse"]);
        guessed.tier = RouteTier::Scanned;
        let mut other = route("hg.add", &["hg", "add"]);
        other.tier = RouteTier::Scanned;
        commands.insert("hg".to_string(), vec![guessed, other]);

        let heuristic = heuristic_only_commands(&commands);

        assert_eq!(heuristic, vec![("hg".to_string(), 2)]);
    }

    #[test]
    fn test_heuristic_only_commands_ignores_a_single_route_command() {
        // `wc` is one help block the scan reads in full; reporting it pointed
        // the operator at the tools with the least to miss.
        let mut commands = std::collections::BTreeMap::new();
        let mut guessed = route("cli.wc", &["wc"]);
        guessed.tier = RouteTier::Scanned;
        commands.insert("wc".to_string(), vec![guessed]);
        let mut first = route("cli.git.status", &["git", "status"]);
        first.tier = RouteTier::Scanned;
        let mut second = route("cli.git.rev_parse", &["git", "rev_parse"]);
        second.tier = RouteTier::Scanned;
        commands.insert("git".to_string(), vec![first, second]);

        let heuristic = heuristic_only_commands(&commands);

        assert_eq!(heuristic, vec![("git".to_string(), 2)]);
    }

    #[test]
    fn test_retain_removed_routes_replaces_a_changed_command_path() {
        // The same module id surviving with a new command_path must replace
        // the old route; keeping both made every invocation of the shim fail
        // with "command_path changed".
        let mut current = manifest_with(vec![route("cli.git.status", &["git", "status"])]);
        let old = manifest_with(vec![route("cli.git.status", &["git", "st"])]);
        retain_removed_routes(&mut current, Some(&old));
        assert_eq!(current.commands["git"].len(), 1);
        assert_eq!(
            current.commands["git"][0].command_path,
            vec!["git".to_string(), "status".to_string()]
        );
    }

    #[test]
    fn test_retain_removed_routes_marks_a_removed_module_as_tombstone() {
        let mut current = manifest_with(Vec::new());
        let old = manifest_with(vec![route("cli.git.gone", &["git", "gone"])]);
        retain_removed_routes(&mut current, Some(&old));
        assert_eq!(current.commands["git"].len(), 1);
        assert!(current.commands["git"][0].tombstone);
    }

    /// A structurally valid binding with no reviewed overlay, so `eligible`
    /// fails on the contract and the scanned policy decides.
    fn binding(module_id: &str, command: &str, target: &str) -> apcore_toolkit::ScannedModule {
        let mut module = apcore_toolkit::ScannedModule::new(
            module_id.into(),
            "shim fixture".into(),
            serde_json::json!({"type": "object"}),
            serde_json::json!({"type": "object"}),
            vec![],
            target.into(),
        );
        module
            .metadata
            .insert("command_path".into(), serde_json::json!([command]));
        module
            .metadata
            .insert("apexe_scan_format".into(), serde_json::json!(3));
        module
    }

    fn also(names: &[&str]) -> crate::module::ModuleFilter {
        crate::module::ModuleFilter {
            also: names.iter().map(|name| (*name).to_string()).collect(),
            ..Default::default()
        }
    }

    /// `--also` names a command to govern; it does not vouch for a binding
    /// whose binary is gone. Admitting one installed a shim that could only
    /// fail and reported it as an admission the policy refused.
    #[test]
    fn test_also_cannot_admit_a_structurally_ineligible_binding() {
        let module = binding(
            "cli.ghost",
            "ghost",
            "exec:///nonexistent/apexe-test-binary",
        );
        let error = collect_routes(
            &[module],
            |_| true,
            crate::cli::ScannedPolicy::Readonly,
            &also(&["ghost"]),
        )
        .expect_err("--also must not vouch for a missing binary");
        assert!(
            format!("{error:#}").contains("No shim-eligible modules"),
            "unexpected error: {error:#}"
        );
    }

    /// The admission is legitimate, but it must be recorded: a manifest that
    /// says `readonly` while holding `rm` is the lie the report exists to
    /// prevent.
    #[test]
    fn test_also_records_the_annotation_it_overrode() {
        let mut module = binding("cli.rm", "rm", "exec:///bin/ls");
        module.annotations = Some(apcore::module::ModuleAnnotations {
            destructive: true,
            ..Default::default()
        });

        let collected = collect_routes(
            &[module],
            |_| true,
            crate::cli::ScannedPolicy::Readonly,
            &also(&["rm"]),
        )
        .unwrap();

        assert_eq!(collected.commands.len(), 1);
        assert_eq!(collected.overrides.len(), 1);
        assert_eq!(collected.overrides[0].command, "rm");
        assert_eq!(collected.overrides[0].reason, "it is destructive");
    }

    /// Naming a command the policy already admits is not an override, so the
    /// report stays silent. `--also node,python3` under `--scanned all` is the
    /// case this protects.
    #[test]
    fn test_also_is_silent_when_the_policy_already_admits_the_module() {
        let mut module = binding("cli.cat", "cat", "exec:///bin/ls");
        module.annotations = Some(apcore::module::ModuleAnnotations {
            readonly: true,
            open_world: false,
            ..Default::default()
        });

        let collected = collect_routes(
            &[module],
            |_| true,
            crate::cli::ScannedPolicy::Readonly,
            &also(&["cat"]),
        )
        .unwrap();

        assert_eq!(collected.commands.len(), 1);
        assert!(collected.overrides.is_empty());
    }
}
