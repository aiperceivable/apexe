//! Installed rule corpora: what is on this machine, and how to take one away.
//!
//! `init` adds a corpus and a scan never changes the set, so without a removal
//! command a machine that installed the default rules could not be narrowed to
//! an operator's own corpus without deleting `~/.apexe/corpora` by hand. This
//! is the deliberate path out.

use crate::config::ApexeConfig;
use crate::corpus::CorpusStore;
use serde::Serialize;
use std::io::IsTerminal;

#[derive(Debug, clap::Args)]
#[command(
    after_help = "Examples:\n  apexe corpus list                 Show installed corpora, sources and identities\n  apexe corpus list --format json   Machine-readable inventory\n  apexe corpus remove cli-permissions\n\nRemoving a corpus deletes its snapshots and catalog entry. Bindings already\nwritten stay until a later `apexe init` or scan replaces them."
)]
pub struct CorpusArgs {
    #[command(subcommand)]
    pub command: CorpusCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum CorpusCommand {
    /// List installed corpora with their source and identity.
    List(ListArgs),
    /// Remove one installed corpus and its snapshots.
    Remove(RemoveArgs),
}

#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// Output format.
    #[arg(long, value_enum, default_value = "table")]
    pub format: ListFormat,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ListFormat {
    Table,
    Json,
}

#[derive(Debug, clap::Args)]
pub struct RemoveArgs {
    /// Corpus name as shown by `apexe corpus list`.
    pub name: String,
    /// Remove without asking for confirmation.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Serialize)]
struct CorpusRow {
    name: String,
    version: Option<String>,
    source: crate::corpus::SourceKind,
    identity: String,
    reference: String,
    repository: String,
}

impl CorpusArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        match self.command {
            CorpusCommand::List(args) => list(config, args.format),
            CorpusCommand::Remove(args) => remove(config, args),
        }
    }
}

fn list(config: &ApexeConfig, format: ListFormat) -> anyhow::Result<()> {
    let store = CorpusStore::new(config);
    let rows: Vec<CorpusRow> = store
        .list()?
        .into_iter()
        .map(|corpus| CorpusRow {
            version: corpus
                .manifest
                .as_ref()
                .map(|manifest| manifest.version.clone()),
            name: corpus.name,
            source: corpus.source,
            identity: corpus.commit,
            reference: corpus.reference,
            repository: corpus.repository,
        })
        .collect();
    match format {
        ListFormat::Json => println!("{}", serde_json::to_string_pretty(&rows)?),
        ListFormat::Table => {
            if rows.is_empty() {
                println!("No managed corpora are installed.");
                return Ok(());
            }
            for row in &rows {
                println!(
                    "{} {} from {} ({}: {})",
                    row.name,
                    row.version.as_deref().unwrap_or("unversioned"),
                    row.repository,
                    match row.source {
                        crate::corpus::SourceKind::Git => "commit",
                        crate::corpus::SourceKind::Archive => "sha256",
                    },
                    row.identity
                );
            }
        }
    }
    Ok(())
}

fn remove(config: &ApexeConfig, args: RemoveArgs) -> anyhow::Result<()> {
    let store = CorpusStore::new(config);
    // Read the source before deleting it: the most likely next action is
    // putting the corpus back, and the recorded repository is the answer.
    let repository = store
        .list()?
        .into_iter()
        .find(|corpus| corpus.name == args.name)
        .map(|corpus| corpus.repository);
    if !args.yes {
        confirm(&args.name)?;
    }
    store.remove(&args.name)?;
    println!("Removed corpus '{}'.", args.name);
    if let Some(repository) = repository {
        println!("Re-add it with: apexe init --overlays {repository}");
    }
    println!("Bindings it informed remain; run 'apexe init' or a scan to rebuild them.");
    Ok(())
}

/// Ask before deleting an installed rule set. A pipe is not consent, so a
/// non-interactive caller must pass `--yes` explicitly.
fn confirm(name: &str) -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("refusing to remove '{name}' without --yes in a non-interactive shell");
    }
    print!("Remove installed corpus '{name}' and its snapshots? [y/N] ");
    use std::io::Write;
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        anyhow::bail!("aborted; '{name}' is unchanged")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a_non_interactive_removal_requires_yes() {
        // Test runners are not terminals, which is the case the guard exists
        // for: a script must opt in instead of deleting rules it did not name.
        if !std::io::stdin().is_terminal() {
            let error = confirm("cli-permissions").unwrap_err();
            assert!(error.to_string().contains("--yes"), "{error}");
        }
    }
}
