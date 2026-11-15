use crate::config::ApexeConfig;
use crate::governance::sandbox::SandboxRuntime;
use crate::governance::AuditManager;

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum SandboxFormat {
    #[default]
    Table,
    Json,
}

/// Show or inspect the sandbox delegation configuration.
#[derive(Debug, clap::Args)]
pub struct SandboxArgs {
    #[command(subcommand)]
    pub command: SandboxCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum SandboxCommand {
    /// Report the selected backend, enforcement and capabilities.
    Check(SandboxCheckArgs),
}

#[derive(Debug, clap::Args)]
pub struct SandboxCheckArgs {
    /// Output format.
    #[arg(long, value_enum, default_value = "table")]
    pub format: SandboxFormat,
}

impl SandboxArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        match self.command {
            SandboxCommand::Check(args) => args.execute(config),
        }
    }
}

impl SandboxCheckArgs {
    fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let runtime = SandboxRuntime::from_config(config);
        let check = runtime.check();

        let audit = AuditManager::new(&config.audit_log);
        audit.log_governance_event(
            "sandbox_probe",
            serde_json::to_value(&check).unwrap_or_else(|_| serde_json::json!({})),
        );

        match self.format {
            SandboxFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&check)?);
            }
            SandboxFormat::Table => {
                println!("backend: {}", check.backend);
                println!("enforcement: {}", check.enforcement);
                println!("available: {}", check.available);
                if let Some(reason) = &check.reason {
                    println!("reason: {reason}");
                }
                println!(
                    "capabilities: {}",
                    if check.capabilities.is_empty() {
                        "(none)".to_string()
                    } else {
                        check.capabilities.join(", ")
                    }
                );
                println!("mode: {}", check.mode);
                println!(
                    "modules: {}",
                    if check.modules.is_empty() {
                        "(none)".to_string()
                    } else {
                        check.modules.join(", ")
                    }
                );
                println!("external: {}", check.external);
                if let Some(image) = &check.docker_image {
                    println!("docker_image: {image}");
                }
                println!("probe: {}", check.probe);
            }
        }
        Ok(())
    }
}
