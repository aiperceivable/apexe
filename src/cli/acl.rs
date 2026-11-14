//! Policy authorship: the commands that write an ACL, as distinct from the
//! commands that report one.
//!
//! `apexe scan` produces an inventory — which routes exist, what each one does.
//! It does not decide who may run them, and it used to write that decision
//! anyway, so every scan edited the policy as a side effect of learning about a
//! tool. The split here is by authority:
//!
//! * [`AclCommand::Init`] writes a starter policy **once**, from a named model
//!   (see [`PolicyProfile`]). It refuses to overwrite, because a file that was
//!   edited by hand is the operator's, not the generator's.
//! * [`AclCommand::Sync`] refreshes only the denials that the scan's annotations
//!   maintain, and never grants anything. It is safe to run in a loop and in CI.
//! * [`AclCommand::Explain`] answers "why was this refused" against a policy
//!   without executing anything.
//!
//! Nothing here writes an allow rule except `init`.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use apcore_toolkit::ScannedModule;

use crate::config::ApexeConfig;
use crate::governance::{AclManager, PolicyProfile};

#[derive(Debug, clap::Args)]
pub struct AclArgs {
    #[command(subcommand)]
    pub command: AclCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum AclCommand {
    /// Write a starter policy. Refuses to overwrite an existing file.
    #[command(display_order = 10)]
    Init(AclInitArgs),
    /// Refresh the destructive and open-world denials from current annotations.
    ///
    /// Only denials are recalculated; allow rules are never added, removed or
    /// reordered. This is the one policy write that is safe to automate.
    #[command(display_order = 20)]
    Sync(AclSyncArgs),
    /// Show how the policy decides one module, and what to change to change it.
    #[command(display_order = 30)]
    Explain(AclExplainArgs),
}

#[derive(Debug, clap::Args)]
pub struct AclInitArgs {
    /// Grant model for the local shell.
    ///
    /// `developer` (default) lets the local shell — the shims and `apexe exec`
    /// — run whatever this machine installed, except the destructive and
    /// network-reaching routes the policy denies to every caller. `strict`
    /// grants nothing implicitly: name the modules the local shell may run in
    /// rules of your own, above the generated ones.
    #[arg(long, value_enum, default_value = "developer")]
    pub profile: AclProfile,

    /// Write the policy to this path instead of `<config_dir>/acl.yaml`.
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Overwrite an existing policy. The previous file is not kept.
    #[arg(long)]
    pub force: bool,

    /// Print the policy instead of writing it.
    #[arg(long)]
    pub stdout: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AclProfile {
    Developer,
    Strict,
}

impl From<AclProfile> for PolicyProfile {
    fn from(profile: AclProfile) -> Self {
        match profile {
            AclProfile::Developer => PolicyProfile::Developer,
            AclProfile::Strict => PolicyProfile::Strict,
        }
    }
}

#[derive(Debug, clap::Args)]
pub struct AclSyncArgs {
    /// Policy to refresh (default: `<config_dir>/acl.yaml`).
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Bindings directory to derive the denials from (default: configured).
    #[arg(long, value_name = "DIR")]
    pub bindings_dir: Option<PathBuf>,

    /// Report the policy a sync would produce without writing it.
    ///
    /// Exits non-zero when the file on disk differs from it, which is the form
    /// a pipeline needs: policy drift against the scanned surface fails the
    /// build instead of being discovered by a refused call.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, clap::Args)]
pub struct AclExplainArgs {
    /// Module id to explain, e.g. `cli.git.status`.
    pub module_id: String,

    /// Caller identity to evaluate as: `local` (this machine's shims and
    /// `apexe exec`) or `@external` (an unauthenticated serve/a2a client).
    #[arg(long, default_value = "local")]
    pub caller: String,

    /// Policy to evaluate (default: `<config_dir>/acl.yaml`).
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
}

impl AclArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        match self.command {
            AclCommand::Init(args) => args.execute(config),
            AclCommand::Sync(args) => args.execute(config),
            AclCommand::Explain(args) => args.execute(config),
        }
    }
}

impl AclInitArgs {
    fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let path = self
            .path
            .unwrap_or_else(|| config.config_dir.join("acl.yaml"));
        let modules = super::load_all_modules(config)?;
        let starter = AclManager::generate_starter(&modules, self.profile.into());
        if self.stdout {
            println!("{}", starter.render()?);
            return Ok(());
        }
        if path.exists() && !self.force {
            anyhow::bail!(
                "{} already exists. Review it, or pass --force to replace it \
                 (there is no backup), or --stdout to print the starter instead.",
                path.display()
            );
        }
        write_policy(&starter, &path)?;
        println!(
            "Wrote {} ({} profile). Review it before relying on it; `apexe acl sync` \
             maintains the denials from now on.",
            path.display(),
            match self.profile {
                AclProfile::Developer => "developer",
                AclProfile::Strict => "strict",
            }
        );
        Ok(())
    }
}

impl AclSyncArgs {
    fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let path = self
            .path
            .unwrap_or_else(|| config.config_dir.join("acl.yaml"));
        anyhow::ensure!(
            path.exists(),
            "{} does not exist. Run `apexe acl init` first: sync refreshes the \
             denials of an existing policy and never creates one.",
            path.display()
        );
        let modules = super::load_all_modules(config)?;
        let synced = AclManager::sync_deny_floor(&path, &modules)
            .with_context(|| format!("Failed to refresh {}", path.display()))?;
        let existing = AclManager::from_config(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if existing.deny_floor_targets() == synced.deny_floor_targets() {
            println!("{} is already in sync.", path.display());
            return Ok(());
        }
        if self.dry_run {
            println!("{} would change:", path.display());
            println!("{}", render_diff(&path, &synced)?);
            anyhow::bail!("The policy is out of sync with the scanned surface");
        }
        write_policy(&synced, &path)?;
        println!(
            "Refreshed the destructive and open-world denials in {}. Allow rules \
             were left untouched.",
            path.display()
        );
        Ok(())
    }
}

impl AclExplainArgs {
    fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let path = self
            .path
            .unwrap_or_else(|| config.config_dir.join("acl.yaml"));
        let manager = AclManager::from_config(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let decision = manager.explain_for(&self.caller, &self.module_id);
        println!("{} as {}: {}", self.module_id, self.caller, decision.effect);
        match (
            &decision.matched_rule_index,
            &decision.matched_rule_description,
        ) {
            (Some(index), description) => println!(
                "  decided by rule {index}{}",
                description
                    .as_deref()
                    .map(|description| format!(": {description}"))
                    .unwrap_or_default()
            ),
            _ => println!(
                "  decided by default_effect: {} (no rule matched)",
                decision.default_effect
            ),
        }
        if decision.matched_rule_has_conditions {
            println!(
                "  this rule carries conditions; a report cannot evaluate them, so the \
                 verdict above is the rule's effect, not a matching decision"
            );
        }
        Ok(())
    }
}

/// Write the starter policy when none exists, then refresh the denials.
///
/// The behaviour `scan --write-acl` keeps for compatibility: it may create a
/// policy (an explicit request) and may tighten one, and it never grants.
pub(super) fn write_starter_or_sync(
    acl_path: &Path,
    modules: &[ScannedModule],
) -> anyhow::Result<()> {
    let policy = if acl_path.exists() {
        let synced = AclManager::sync_deny_floor(acl_path, modules)?;
        // Rewriting an unchanged document costs the operator their formatting
        // and rule order for nothing, so the write is skipped when the floor it
        // expresses is the one already on disk.
        let existing = AclManager::from_config(acl_path)?;
        if existing.same_document(&synced)? {
            return Ok(());
        }
        synced
    } else {
        AclManager::generate_starter(modules, PolicyProfile::Developer)
    };
    write_policy(&policy, acl_path)
}

fn write_policy(policy: &AclManager, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    policy
        .write_config(path)
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// A line-oriented diff of the denials a sync would change.
///
/// Not a general YAML differ: `acl sync` only ever writes deny-rule target
/// lists, so the useful question is "which module ids join or leave which deny
/// rule", and answering exactly that is more legible than a document diff.
fn render_diff(path: &Path, synced: &AclManager) -> anyhow::Result<String> {
    let existing = AclManager::from_config(path)?;
    let before = existing.deny_floor_targets();
    let after = synced.deny_floor_targets();
    let mut out = String::new();
    for description in before.keys().chain(after.keys()) {
        let empty = Vec::new();
        let before_targets = before.get(description).unwrap_or(&empty);
        let after_targets = after.get(description).unwrap_or(&empty);
        if before_targets.is_empty() && after_targets.is_empty() {
            continue;
        }
        for target in after_targets.iter().filter(|t| !before_targets.contains(t)) {
            out.push_str(&format!("  + {description}: {target}\n"));
        }
        for target in before_targets.iter().filter(|t| !after_targets.contains(t)) {
            out.push_str(&format!("  - {description}: {target}\n"));
        }
    }
    Ok(out)
}
