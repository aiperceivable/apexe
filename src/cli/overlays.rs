use crate::config::ApexeConfig;
use crate::corpus::CorpusStore;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

/// Install and manage Git-hosted overlay corpora.
#[derive(Debug, clap::Args, Serialize, Deserialize)]
pub struct OverlaysArgs {
    #[command(subcommand)]
    pub command: OverlayCommand,
}

#[derive(Debug, clap::Subcommand, Serialize, Deserialize)]
pub enum OverlayCommand {
    /// Download and register an overlay corpus from GitHub, HTTPS or local Git.
    Install {
        repository: String,
        /// Installation name (default: manifest name, then repository basename).
        #[arg(long)]
        name: Option<String>,
        /// Branch, tag or commit to fetch (default: remote HEAD).
        #[arg(long = "ref")]
        reference: Option<String>,
        /// Relative directory containing overlay files.
        #[arg(long)]
        subdir: Option<PathBuf>,
    },
    /// Fetch a new validated snapshot using the stored repository and reference.
    Update {
        name: String,
        /// Override the stored branch, tag or commit.
        #[arg(long = "ref")]
        reference: Option<String>,
    },
    /// List installed corpora, their resolved commits and overlay counts.
    List,
    /// Unregister and remove an owned corpus and its retained snapshots.
    Remove { name: String },
}

impl OverlaysArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let store = CorpusStore::new(config);
        let result = match self.command {
            OverlayCommand::Install {
                repository,
                name,
                reference,
                subdir,
            } => serde_json::to_value(store.install(
                &repository,
                name.as_deref(),
                reference.as_deref(),
                subdir.as_deref(),
            )?)?,
            OverlayCommand::Update { name, reference } => {
                serde_json::to_value(store.update(&name, reference.as_deref())?)?
            }
            OverlayCommand::List => serde_json::to_value(store.list()?)?,
            OverlayCommand::Remove { name } => {
                store.remove(&name)?;
                serde_json::json!({"removed": name})
            }
        };
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &result)?;
        writeln!(stdout)?;
        Ok(())
    }
}
