use super::agent::{detected_host_status, host_name, AgentArgs, AgentHost, AgentScope};
use super::env::EnvArgs;
use super::init::InitArgs;
use super::sh::CheckFormat;
use crate::config::ApexeConfig;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// Prepare this machine in one step: rules, shims, and detected agent hosts.
///
/// This is orchestration, not a second implementation. Each step calls the
/// command that owns the decision (`init`, `env`, `agent`), so a change to any
/// of them reaches `setup` without a parallel code path to keep in sync.
#[derive(Debug, clap::Args)]
#[command(
    after_help = "Examples:\n  apexe setup                 Prepare rules, shims and detected agent hosts\n  apexe setup --dry-run       Print the plan and change nothing\n  apexe setup --yes           Configure detected hosts without asking\n  apexe setup --no-hosts      Prepare rules and shims only\n  apexe setup --overlays https://github.com/acme/team-corpus\n\nShims are installed without editing shell startup files. A host that accepts\na custom shell is configured through `apexe agent <host> --write`; Codex needs\nits login shell, so it is reported rather than changed."
)]
pub struct SetupArgs {
    /// Configure detected agent hosts without asking.
    #[arg(long)]
    pub yes: bool,
    /// Print the plan and change nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Install rules and shims but leave agent hosts alone.
    #[arg(long)]
    pub no_hosts: bool,
    /// Rules source for this run: a repository URL, a local checkout or a corpus archive.
    #[arg(long, value_name = "SOURCE")]
    pub overlays: Option<String>,
    /// Git branch, tag or commit within the selected source.
    #[arg(long = "ref")]
    pub reference: Option<String>,
    /// Use cached rules instead of downloading a release.
    #[arg(long, env = "APEXE_OFFLINE")]
    pub offline: bool,
    /// Replace an installed corpus that uses the same name from another source.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostAction {
    /// The host already selects this installation; nothing to do.
    Configured,
    /// A configuration file can be written with consent.
    Configure,
    /// The host reads its login shell; changing that is the operator's call.
    Manual,
}

#[derive(Debug)]
struct HostPlan {
    host: AgentHost,
    binary: PathBuf,
    action: HostAction,
}

impl SetupArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let shim_dir = config.config_dir.join("bin");
        let hosts = plan_hosts(&shim_dir, self.no_hosts);
        if self.dry_run {
            print_plan(self.overlays.as_deref(), self.reference.as_deref(), &hosts);
            return Ok(());
        }
        self.install_rules(config)?;
        self.install_shims(config)?;
        let failures = self.configure_hosts(&hosts, config)?;
        println!();
        println!(
            "Shell startup was not modified: only configured hosts and 'apexe sh' are governed."
        );
        println!("Run 'apexe env --enable' to govern every new terminal.");
        println!(
            "Review the ACL at {} before execution.",
            config.config_dir.join("acl.yaml").display()
        );
        println!("Next: apexe list --verbose, then apexe exec or apexe env --check.");
        anyhow::ensure!(
            failures == 0,
            "{failures} agent host(s) could not be configured; the steps above say what to run"
        );
        Ok(())
    }

    fn install_rules(&self, config: &ApexeConfig) -> anyhow::Result<()> {
        InitArgs {
            overlays: self.overlays.clone(),
            reference: self.reference.clone(),
            refresh: false,
            offline: self.offline,
            force: self.force,
            format: super::init::ReportFormat::Table,
        }
        .execute(config)
    }

    /// Install the launcher and shims without touching shell startup files.
    ///
    /// The managed PATH block reshapes every new terminal, where login-time
    /// tooling resolves its own `rm`/`ln`/`sort` through the shims. That is the
    /// right trade for a host that only reads its own shell, not the right
    /// default for a machine-wide install, so `setup` leaves startup alone and
    /// lets the host integration carry governance.
    fn install_shims(&self, config: &ApexeConfig) -> anyhow::Result<()> {
        EnvArgs {
            enable: true,
            no_rc: true,
            ..EnvArgs::default()
        }
        .execute(config)
    }

    fn configure_hosts(&self, hosts: &[HostPlan], config: &ApexeConfig) -> anyhow::Result<usize> {
        let mut failures = 0;
        println!();
        println!("Agent hosts:");
        for plan in hosts {
            let name = host_name(plan.host);
            match plan.action {
                HostAction::Configured => {
                    println!("  {name:<8} {}  already configured", plan.binary.display());
                }
                HostAction::Manual => {
                    println!(
                        "  {name:<8} {}  needs its login shell: apexe agent {name} --write",
                        plan.binary.display()
                    );
                }
                HostAction::Configure => {
                    let accepted = self.yes || confirm(plan)?;
                    if !accepted {
                        println!(
                            "  {name:<8} {}  skipped: apexe agent {name} --write",
                            plan.binary.display()
                        );
                        continue;
                    }
                    match (AgentArgs {
                        host: plan.host,
                        check: false,
                        format: CheckFormat::Text,
                        write: true,
                        mcp_only: false,
                        scope: Some(AgentScope::User),
                    })
                    .execute(config)
                    {
                        Ok(()) => {}
                        Err(error) => {
                            failures += 1;
                            println!("  {name:<8} {}  failed: {error}", plan.binary.display());
                        }
                    }
                }
            }
        }
        if hosts.is_empty() {
            println!("  none detected on PATH; use apexe env --enable for this shell, or configure a host later");
        }
        Ok(failures)
    }
}

fn plan_hosts(shim_dir: &Path, no_hosts: bool) -> Vec<HostPlan> {
    if no_hosts {
        return Vec::new();
    }
    detected_host_status(shim_dir)
        .into_iter()
        .map(|(host, binary, configured)| HostPlan {
            host,
            binary,
            action: action_for(host, configured),
        })
        .collect()
}

/// Which step a detected host needs. Codex is the exception: it has no
/// configuration file, so its remediation is its login shell, and rewriting
/// that is the operator's decision rather than a default.
fn action_for(host: AgentHost, configured: bool) -> HostAction {
    if configured {
        HostAction::Configured
    } else if host == AgentHost::Codex {
        HostAction::Manual
    } else {
        HostAction::Configure
    }
}

fn print_plan(overlays: Option<&str>, reference: Option<&str>, hosts: &[HostPlan]) {
    match (overlays, reference) {
        (Some(source), Some(reference)) => {
            println!("Would prepare rules from {source} at {reference}.")
        }
        (Some(source), None) => println!("Would prepare rules from {source}."),
        (None, Some(reference)) => {
            println!("Would prepare rules from the configured source at {reference}.")
        }
        (None, None) => println!("Would prepare rules from the configured source."),
    }
    println!("Would install shims under the config directory without editing shell startup.");
    if hosts.is_empty() {
        println!("Would leave agent hosts alone.");
        return;
    }
    println!("Agent hosts:");
    for plan in hosts {
        let name = host_name(plan.host);
        let action = match plan.action {
            HostAction::Configured => "already configured",
            HostAction::Configure => "would configure",
            HostAction::Manual => "would report the login-shell step",
        };
        println!("  {name:<8} {}  {action}", plan.binary.display());
    }
}

/// Ask before editing another tool's configuration.
///
/// A non-interactive caller gets no writes: the configuration files belong to
/// the operator, and a pipe is not consent.
fn confirm(plan: &HostPlan) -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    let name = host_name(plan.host);
    print!("  Configure {name} ({})? [Y/n] ", plan.binary.display());
    use std::io::Write;
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_configured_hosts_need_no_work() {
        for host in [
            AgentHost::Opencode,
            AgentHost::Pi,
            AgentHost::Codex,
            AgentHost::Claude,
        ] {
            assert_eq!(action_for(host, true), HostAction::Configured);
        }
    }

    #[test]
    fn test_unconfigured_shell_hosts_are_configured_but_codex_is_not() {
        assert_eq!(
            action_for(AgentHost::Opencode, false),
            HostAction::Configure
        );
        assert_eq!(action_for(AgentHost::Pi, false), HostAction::Configure);
        assert_eq!(action_for(AgentHost::Claude, false), HostAction::Configure);
        assert_eq!(action_for(AgentHost::Codex, false), HostAction::Manual);
    }

    #[test]
    fn test_no_hosts_skips_detection_entirely() {
        assert!(plan_hosts(Path::new("/nonexistent"), true).is_empty());
    }

    #[test]
    fn test_non_interactive_calls_get_no_writes() {
        let plan = HostPlan {
            host: AgentHost::Opencode,
            binary: PathBuf::from("/usr/bin/opencode"),
            action: HostAction::Configure,
        };
        // Test runners are not terminals, which is the case this guard exists
        // for: a piped installer must not edit another tool's configuration.
        if !std::io::stdin().is_terminal() {
            assert!(!confirm(&plan).unwrap());
        }
    }
}
