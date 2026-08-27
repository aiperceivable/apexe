mod config_gen;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing::info;

use crate::config::{load_config, ApexeConfig};

/// apexe -- Outside-In CLI-to-Agent Bridge.
///
/// Wraps CLI tools into governed apcore modules served via MCP/A2A.
#[derive(Debug, Parser)]
#[command(name = "apexe", version, about, long_about = None)]
pub struct Cli {
    /// Log level (trace, debug, info, warn, error). When omitted, the level
    /// resolves from RUST_LOG, then APEXE_LOG_LEVEL / config.yaml, then "info".
    #[arg(long, global = true)]
    pub log_level: Option<String>,

    /// Per-call timeout in seconds, overriding `default_timeout` from
    /// `config.yaml`. Applies to every subcommand that runs a wrapped binary
    /// or probes one during a scan.
    #[arg(long, global = true, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: Option<u64>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Scan CLI tools and generate apcore binding files.
    Scan(ScanArgs),
    /// Start MCP server for scanned CLI tools.
    Serve(ServeArgs),
    /// Start A2A agent server for scanned CLI tools.
    A2a(A2aArgs),
    /// List previously scanned CLI tools and their modules.
    List(ListArgs),
    /// Show or initialize apexe configuration.
    Config(ConfigArgs),
    /// Show the filesystem access boundary wrapped tools are checked
    /// against, or check one path against it.
    Policy(PolicyArgs),
}

impl Cli {
    /// Resolve the log-level fallback used when `RUST_LOG` is unset: an explicit
    /// `--log-level` wins, otherwise the resolved `config_level` (which already
    /// folds in `APEXE_LOG_LEVEL` and `config.yaml`).
    pub fn effective_log_level(&self, config_level: &str) -> String {
        self.log_level
            .clone()
            .unwrap_or_else(|| config_level.to_string())
    }

    pub fn run(self) -> anyhow::Result<()> {
        let config = load_config(None)?.with_timeout_override(self.timeout);
        config.ensure_dirs()?;
        install_path_guard(&config)?;

        match self.command {
            Commands::Scan(args) => args.execute(&config),
            Commands::Serve(args) => args.execute(&config),
            Commands::A2a(args) => args.execute(&config),
            Commands::List(args) => args.execute(&config),
            Commands::Config(args) => args.execute(&config),
            Commands::Policy(args) => args.execute(&config),
        }
    }
}

/// Install the process-wide path guard from configuration.
///
/// Called before any subcommand runs, so every path a wrapped tool receives is
/// checked whichever surface delivered it. The guard is not optional and there
/// is no flag to disable it: `config.yaml` can only lengthen the denied list
/// (see [`ApexeConfig::additional_denied_paths`]), and skipping this call would
/// leave the compiled-in baseline in force rather than nothing at all.
///
/// A refused installation — something already read the guard and pinned the
/// default — is only an error when the operator configured extra paths. Their
/// protection is the part that would go missing, and running on under a policy
/// the operator wrote and the process never loaded is exactly the false sense
/// of safety §5.3 of the threat model warns about. With no extra paths
/// configured the compiled-in baseline is identical either way, so there is
/// nothing to refuse over.
fn install_path_guard(config: &crate::config::ApexeConfig) -> anyhow::Result<()> {
    let guard_config = crate::governance::GuardConfig {
        denied: &config.additional_denied_paths,
        allowed: &config.allowed_paths,
    };
    let configured = guard_config.denied.len() + guard_config.allowed.len();
    let guard = crate::governance::PathGuard::from_env(guard_config);
    tracing::debug!(
        root = %guard.root().display(),
        denied = guard_config.denied.len(),
        allowed = guard_config.allowed.len(),
        "Installing path guard"
    );
    if !crate::governance::path_guard::install(guard) && configured > 0 {
        anyhow::bail!(
            "path guard was already installed, so the {configured} configured \
             `additional_denied_paths` / `allowed_paths` entries are not in \
             force; refusing to start rather than run under a policy that was \
             not loaded"
        );
    }
    Ok(())
}

/// Scan CLI tools and generate apcore binding files.
///
/// TOOLS: One or more CLI tool names to scan (e.g., git docker ffmpeg).
#[derive(Debug, clap::Args)]
pub struct ScanArgs {
    /// CLI tool names to scan
    #[arg(required = true)]
    pub tools: Vec<String>,

    /// Output directory for binding files (default: ~/.apexe/modules/)
    #[arg(long)]
    pub output_dir: Option<PathBuf>,

    /// Maximum subcommand recursion depth (1-5)
    #[arg(long, default_value = "2", value_parser = clap::value_parser!(u32).range(1..=5))]
    pub depth: u32,

    /// Force re-scan, bypassing cache
    #[arg(long)]
    pub no_cache: bool,

    /// Output format for scan results
    #[arg(long, default_value = "table", value_parser = ["json", "yaml", "table"])]
    pub format: String,

    /// Also write a Claude Skill (`SKILL.md`) per module under
    /// `<DIR>/.claude/skills/<module_id>/SKILL.md`
    #[arg(long)]
    pub skills_dir: Option<PathBuf>,

    /// Curated tool overlay to apply, overriding the heuristic scan result.
    ///
    /// Outranks the built-in and `~/.apexe/overlays/` overlays, and skips the
    /// variant, platform and probe conditions the file declares: naming the
    /// file is the operator's own assertion that it applies. The command name
    /// must still agree.
    #[arg(long)]
    pub overlay: Option<PathBuf>,

    /// Fail the command when a written binding does not verify.
    ///
    /// The YAML verifier already runs on every write; without this flag a
    /// failure is a warning and the command still exits 0, which is right for
    /// a scan whose other tools succeeded. With it, an unverifiable binding is
    /// an error — the form a pipeline needs, since a binding that does not
    /// parse is not a deliverable.
    #[arg(long)]
    pub verify: bool,

    /// Report what would be written without creating or overwriting anything.
    ///
    /// Covers all three deliverables — bindings, the ACL policy and any
    /// skills — so a dry run never touches the filesystem.
    #[arg(long)]
    pub dry_run: bool,
}

impl ScanArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let mut orchestrator = crate::scanner::ScanOrchestrator::new(config.clone());
        if let Some(ref overlay_path) = self.overlay {
            orchestrator
                .load_overlay(overlay_path)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        let outcome = orchestrator.scan(&self.tools, self.no_cache, self.depth);

        // Nothing scanned means there is nothing to write, and writing an empty
        // ACL over a good one would be worse than doing nothing.
        if outcome.is_total_failure() {
            return Err(anyhow::anyhow!(
                "No tool could be scanned:\n{}",
                Self::render_failures(&outcome.failures)
            ));
        }

        let output_dir = self
            .output_dir
            .clone()
            .unwrap_or_else(|| config.modules_dir.clone());

        let converter = crate::adapter::CliToolConverter::new();
        let modules = converter.convert_all(&outcome.tools);

        // These are the command's deliverables. A failed write must surface as
        // a non-zero exit, not a swallowed warning — otherwise `apexe scan`
        // reports success with no bindings (or, worse, no ACL policy) written.
        self.write_bindings(&modules, &output_dir)?;
        self.write_acl(&modules, config)?;
        self.write_skills(&modules)?;
        let scanned_count = outcome.tools.len();
        self.print_results(outcome.tools, &modules)?;

        Self::report_partial_run(
            &outcome.failures,
            scanned_count,
            self.tools.len(),
            &output_dir,
        )
    }

    /// Fail the command when some requested tools could not be scanned.
    ///
    /// A partial run wrote real bindings, so the successes are reported and
    /// kept on disk — but it also silently produced fewer modules than asked
    /// for, which is the failure mode this project has already been bitten by.
    /// Exiting non-zero is what stops a pipeline from treating a short surface
    /// as the whole surface; the message names both halves so the exit code is
    /// never the only information available.
    fn report_partial_run(
        failures: &[crate::scanner::ScanFailure],
        scanned_count: usize,
        requested_count: usize,
        output_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        if failures.is_empty() {
            return Ok(());
        }
        Err(anyhow::anyhow!(
            "Scanned {} of {} tools; bindings for the {} that succeeded were written to {}.\n\
             Failed:\n{}",
            scanned_count,
            requested_count,
            scanned_count,
            output_dir.display(),
            Self::render_failures(failures)
        ))
    }

    /// One `  tool: reason` line per failure, in the order they were requested.
    fn render_failures(failures: &[crate::scanner::ScanFailure]) -> String {
        failures
            .iter()
            .map(|failure| format!("  {}: {}", failure.tool, failure.error))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn write_bindings(
        &self,
        modules: &[apcore_toolkit::ScannedModule],
        output_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        let yaml_output = crate::output::YamlOutput::new();
        let write_results = yaml_output
            .write(modules, output_dir, self.dry_run)
            .map_err(|e| anyhow::anyhow!("Failed to write binding files: {e}"))?;

        let mut unverified: Vec<String> = Vec::new();
        for wr in &write_results {
            // A dry run reports no path — nothing was created — so the
            // module id is what identifies the deliverable.
            match (&wr.path, self.dry_run) {
                (_, true) => info!(
                    module_id = %wr.module_id,
                    dir = %output_dir.display(),
                    "Would write binding"
                ),
                (Some(path), false) => info!(path, "Generated binding"),
                (None, false) => {
                    tracing::warn!(module_id = %wr.module_id, "Binding written with no path reported")
                }
            }
            // The YAML verifier runs on every write, but its result was
            // discarded: a binding that does not parse was reported as
            // "Generated binding" and the command exited 0. It is a warning by
            // default — one bad tool should not throw away the scan of the
            // others — and an error under `--verify`, which is the form a
            // pipeline needs.
            if !wr.verified {
                let detail = wr
                    .verification_error
                    .as_deref()
                    .unwrap_or("no reason reported");
                tracing::warn!(
                    module_id = %wr.module_id,
                    error = detail,
                    "Binding failed verification"
                );
                unverified.push(format!("  {}: {detail}", wr.module_id));
            }
        }

        self.report_unverified(&unverified)
    }

    /// Turn the unverified-binding list into the command's verdict.
    ///
    /// A warning by default — a scan whose other tools succeeded still produced
    /// real deliverables, and throwing them away over one bad binding is the
    /// wrong trade. `--verify` escalates, because a pipeline that treats a
    /// short surface as the whole surface is the failure this project has
    /// already been bitten by.
    ///
    /// Split out so the escalation is reachable from a test: driving it through
    /// a real scan would need a tool whose help text yields an unparseable
    /// binding, which is not something to manufacture.
    fn report_unverified(&self, unverified: &[String]) -> anyhow::Result<()> {
        if self.verify && !unverified.is_empty() {
            anyhow::bail!(
                "{} binding(s) failed verification:\n{}",
                unverified.len(),
                unverified.join("\n")
            );
        }
        Ok(())
    }

    /// Write the ACL policy, merging into an existing file rather than
    /// replacing it wholesale — `apexe scan` covers only the tools named on
    /// this command line, so overwriting would discard every earlier scan's
    /// rules (and any the operator hand-authored). See
    /// [`AclManager::merge_default`](crate::governance::AclManager::merge_default).
    fn write_acl(
        &self,
        modules: &[apcore_toolkit::ScannedModule],
        config: &ApexeConfig,
    ) -> anyhow::Result<()> {
        let acl_path = config.config_dir.join("acl.yaml");
        if self.dry_run {
            info!(path = %acl_path.display(), "Would write ACL policy");
            return Ok(());
        }
        let acl_manager = if acl_path.exists() {
            crate::governance::AclManager::merge_default(&acl_path, modules)
                .map_err(|e| anyhow::anyhow!("Failed to load existing ACL for merge: {e}"))?
        } else {
            crate::governance::AclManager::generate_default(modules)
        };
        acl_manager
            .write_config(&acl_path)
            .map_err(|e| anyhow::anyhow!("Failed to write ACL: {e}"))?;
        Ok(())
    }

    fn write_skills(&self, modules: &[apcore_toolkit::ScannedModule]) -> anyhow::Result<()> {
        let Some(ref skills_dir) = self.skills_dir else {
            return Ok(());
        };
        if self.dry_run {
            info!(
                count = modules.len(),
                dir = %skills_dir.join(".claude").join("skills").display(),
                "Would write skill file(s)"
            );
            return Ok(());
        }
        let paths = crate::output::SkillOutput::new()
            .write(modules, skills_dir)
            .map_err(|e| anyhow::anyhow!("Failed to write skill files: {e}"))?;
        for path in &paths {
            info!(path = %path.display(), "Generated skill");
        }
        Ok(())
    }

    /// Print the scan outcome in the requested format.
    ///
    /// Structured formats emit a single [`ScanReport`] document covering every
    /// scanned tool. Previously each tool was printed as its own document,
    /// which made a multi-tool scan's stdout invalid JSON and gave consumers no
    /// flattened command list to bind against.
    fn print_results(
        &self,
        results: Vec<crate::models::ScannedCLITool>,
        modules: &[apcore_toolkit::ScannedModule],
    ) -> anyhow::Result<()> {
        match self.format.as_str() {
            "json" => {
                let report = crate::adapter::ScanReport::new(results, modules);
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
            "yaml" => {
                let report = crate::adapter::ScanReport::new(results, modules);
                println!("{}", serde_yaml::to_string(&report)?);
            }
            _ => {
                for tool in &results {
                    Self::print_tool_table(tool);
                }
            }
        }
        Ok(())
    }

    fn print_tool_table(tool: &crate::models::ScannedCLITool) {
        println!(
            "Tool: {} ({})",
            tool.name,
            tool.version.as_deref().unwrap_or("unknown")
        );
        println!("  Binary: {}", tool.binary_path);
        println!("  Variant: {}", tool.variant.as_str());
        if let Some(ref overlay) = tool.overlay {
            println!("  Overlay: {overlay}");
        }
        println!("  Scan tier: {}", tool.scan_tier);
        println!("  Subcommands: {}", tool.subcommands.len());
        println!("  Global flags: {}", tool.global_flags.len());
        if tool.structured_output.supported {
            println!(
                "  Structured output: {} ({})",
                tool.structured_output.flag.as_deref().unwrap_or(""),
                tool.structured_output.format.as_deref().unwrap_or("")
            );
        }
        if !tool.warnings.is_empty() {
            println!("  Warnings: {}", tool.warnings.join(", "));
        }
        println!();
    }
}

/// Start MCP server for scanned CLI tools.
#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// MCP transport type. `sse` still works but is deprecated upstream --
    /// prefer `http` (streamable HTTP) for anything new.
    #[arg(long, default_value = "stdio", value_parser = ["stdio", "http", "sse"])]
    pub transport: String,

    /// Host for HTTP transports
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Port for HTTP transports (1-65535)
    #[arg(long, default_value = "8000", value_parser = clap::value_parser!(u16).range(1..))]
    pub port: u16,

    /// Enable browser-based Tool Explorer UI (HTTP only)
    #[arg(long)]
    pub explorer: bool,

    /// Directory containing binding files
    #[arg(long)]
    pub modules_dir: Option<PathBuf>,

    /// MCP server name
    #[arg(long, default_value = "apexe")]
    pub name: String,

    /// Print integration config snippet and exit. The snippet reproduces the
    /// rest of this invocation (`--modules-dir`, `--tags`, `--prefix`,
    /// `--acl`, the governance toggles) so the configured client serves the
    /// same surface. Credentials are never included.
    #[arg(long, value_parser = config_gen::ConfigFormat::VALUES)]
    pub show_config: Option<String>,

    /// Restrict the served tools to those carrying every listed tag
    /// (comma-separated). Excluded modules are neither listed nor callable.
    #[arg(long)]
    pub tags: Option<String>,

    /// Restrict the served tools to those whose module ID starts with this
    /// prefix. Excluded modules are neither listed nor callable.
    #[arg(long)]
    pub prefix: Option<String>,

    /// Path to ACL policy YAML file
    #[arg(long)]
    pub acl: Option<PathBuf>,

    /// Gate every call to a module marked `requires_approval` on a human
    /// decision, delivered to the connected MCP client as an elicitation
    /// prompt. A client that declared no elicitation support cannot be
    /// prompted and is refused -- use `--acl` for a per-caller boundary that
    /// needs no human.
    #[arg(long)]
    pub enable_approval: bool,

    /// Credential required on the HTTP transports: `token` (default), `jwt`,
    /// or `none`. Ignored for stdio.
    #[arg(long, value_parser = ["token", "jwt", "none"])]
    pub auth: Option<String>,

    /// Bearer token for `--auth token`. Falls back to APEXE_AUTH_TOKEN; one is
    /// generated and written to stderr at startup if neither is set.
    #[arg(long, env = "APEXE_AUTH_TOKEN", hide_env_values = true)]
    pub auth_token: Option<String>,

    /// Signing secret for `--auth jwt`. Falls back to APEXE_JWT_SECRET.
    #[arg(long, env = "APEXE_JWT_SECRET", hide_env_values = true)]
    pub jwt_secret: Option<String>,

    /// Acknowledge serving with `--auth none` on a non-loopback bind, which
    /// exposes every wrapped binary to the network with no credential.
    #[arg(long)]
    pub allow_unauthenticated_bind: bool,

    /// Accepted and ignored. It used to be the acknowledgement `--transport
    /// sse` required; apcore-mcp 0.18 fixed the defect, so SSE needs none.
    /// Kept so an existing invocation still parses; will be removed later.
    #[arg(long, hide = true)]
    pub allow_deprecated_sse: bool,

    /// Disable structured logging middleware
    #[arg(long)]
    pub no_logging: bool,

    /// Drop call arguments and output from every log event, error records
    /// included. Credentials passed as tool arguments are otherwise logged at
    /// INFO unless the scanner recognized the option as sensitive, and the
    /// schema-driven redaction cannot cover a `--data` body or a key in a URL.
    /// A failed call still produces one ERROR record naming the module, caller,
    /// error code and duration -- it just carries nothing the caller sent.
    #[arg(long)]
    pub no_log_arguments: bool,

    /// Disable circuit breaker (short-circuits a hanging/broken tool)
    #[arg(long)]
    pub no_circuit_breaker: bool,

    /// Disable retry (only ever retries idempotent commands after a timeout)
    #[arg(long)]
    pub no_retry: bool,

    /// Enable /metrics (Prometheus) and /usage observability endpoints
    /// (HTTP/SSE transports only)
    #[arg(long)]
    pub metrics: bool,
    // Note: no `--skip-validation` flag. It advertised "skip input validation
    // against tool schemas" and skipped nothing: every schema check runs in
    // apcore's `input_validation` pipeline step, which apexe never removes,
    // while the flag toggled apcore-mcp's separate pre-dispatch validation --
    // a hook that only fires if the executor adapter implements
    // `McpExecutor::validate`, and `ApcoreExecutorAdapter` does not. Removing
    // it is the honest end state: apcore does expose
    // `ExecutionStrategy::remove("input_validation")`, but a CLI flag whose
    // only effect is to unvalidate the input of a process spawner is a
    // liability, not a feature.
}

impl ServeArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        if let Some(ref format) = self.show_config {
            let snippet = config_gen::generate_config(format, &self.invocation())?;
            println!("{snippet}");
            return Ok(());
        }

        let server = self.build_server(config)?;
        let opts = self.serve_options();
        server
            .serve_with_options(opts)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Describe this invocation for `--show-config`.
    ///
    /// `--auth-token` / `--jwt-secret` are intentionally absent: a snippet is
    /// pasted into a config file that is shared and often committed.
    fn invocation(&self) -> config_gen::ServeInvocation {
        config_gen::ServeInvocation {
            name: self.name.clone(),
            transport: self.transport.clone(),
            host: self.host.clone(),
            port: self.port,
            modules_dir: self.modules_dir.clone(),
            tags: self.tags.clone(),
            prefix: self.prefix.clone(),
            acl: self.acl.clone(),
            enable_approval: self.enable_approval,
            no_logging: self.no_logging,
            no_log_arguments: self.no_log_arguments,
            no_circuit_breaker: self.no_circuit_breaker,
            no_retry: self.no_retry,
        }
    }

    fn build_server(&self, config: &ApexeConfig) -> anyhow::Result<apcore_mcp::APCoreMCP> {
        let modules_dir = self
            .modules_dir
            .clone()
            .unwrap_or_else(|| config.modules_dir.clone());
        let mut builder = crate::mcp::McpServerBuilder::new()
            .name(&self.name)
            .transport(&self.transport)
            .host(&self.host)
            .port(self.port)
            .explorer(self.explorer)
            .modules_dir(modules_dir)
            .timeout_ms(config.default_timeout * 1000)
            .enable_logging(!self.no_logging)
            .log_arguments(!self.no_log_arguments)
            .enable_approval(self.enable_approval)
            .enable_circuit_breaker(!self.no_circuit_breaker)
            .enable_retry(!self.no_retry)
            .enable_metrics(self.metrics)
            .auth(self.auth_options()?)
            .allow_deprecated_sse(self.allow_deprecated_sse)
            .audit_path(config.audit_log.clone());

        // Only load ACL when explicitly specified via --acl flag.
        // Without --acl, the server runs without access control (all tools allowed).
        if let Some(ref acl_path) = self.acl {
            builder = builder.acl_path(acl_path);
        }

        if let Some(ref tags_str) = self.tags {
            builder = builder.tags(parse_tag_list(tags_str)?);
        }
        if let Some(ref prefix) = self.prefix {
            builder = builder.prefix(prefix);
        }

        builder.build().map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Translate the `--auth*` flags into [`AuthOptions`].
    ///
    /// The per-transport default lives in [`crate::auth::resolve_auth`], not
    /// here: what the operator typed and what the bind address implies are two
    /// separate decisions, and only the latter can refuse to start.
    fn auth_options(&self) -> anyhow::Result<crate::auth::AuthOptions> {
        let mode = match self.auth {
            Some(ref value) => Some(crate::auth::AuthMode::parse(value).ok_or_else(|| {
                anyhow::anyhow!("Unknown --auth mode '{value}' (expected token, jwt or none)")
            })?),
            None => None,
        };
        Ok(crate::auth::AuthOptions {
            mode,
            token: self.auth_token.clone(),
            jwt_secret: self.jwt_secret.clone(),
            allow_unauthenticated_bind: self.allow_unauthenticated_bind,
        })
    }

    fn serve_options(&self) -> apcore_mcp::ServeOptions {
        apcore_mcp::ServeOptions {
            explorer: apcore_mcp::ExplorerOptions {
                explorer: self.explorer,
                allow_execute: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// Split a comma-separated `--tags` value, refusing an empty token.
///
/// `--tags readonly,` splits to `["readonly", ""]`, and
/// [`ModuleFilter::admits`](crate::module::ModuleFilter::admits) requires
/// *every* token. No module carries an empty tag and none can be made to, so
/// the filter is unsatisfiable against any registry, present or future: the
/// registry comes up empty and the entire tool surface is uncallable. Before
/// the filter moved to registration time this typo was harmless.
///
/// This is refused rather than warned about for the same reason the ACL
/// validator refuses a structurally inert rule (see
/// [`validate_acl_rules`](crate::governance::validate_acl_rules)): the list can
/// never match anything, so no registry makes it correct. A tag that is merely
/// unknown to *this* host stays a warning — one invocation is meant to be
/// portable across differently scanned machines.
fn parse_tag_list(raw: &str) -> anyhow::Result<Vec<String>> {
    raw.split(',')
        .map(|token| {
            let tag = token.trim();
            if tag.is_empty() {
                anyhow::bail!(
                    "--tags '{raw}' contains an empty tag. No module carries an empty tag, so \
                     the filter would admit nothing and the server would start with no callable \
                     tools. Remove the stray comma."
                );
            }
            Ok(tag.to_string())
        })
        .collect()
}

/// Start A2A agent server for scanned CLI tools.
#[derive(Debug, clap::Args)]
pub struct A2aArgs {
    /// Base URL to bind the A2A server to
    #[arg(long, default_value = "http://127.0.0.1:8000")]
    pub url: String,

    /// Directory containing binding files
    #[arg(long)]
    pub modules_dir: Option<PathBuf>,

    /// A2A agent name
    #[arg(long, default_value = "apexe")]
    pub name: String,

    /// Enable browser-based Explorer UI
    #[arg(long)]
    pub explorer: bool,

    /// Path to ACL policy YAML file
    #[arg(long)]
    pub acl: Option<PathBuf>,

    // Note: no `--enable-approval` flag on `a2a`. Unlike MCP, A2A has no
    // interactive elicitation transport, and there is no CLI way to supply an
    // approval store, so `A2aServerBuilder::serve` fails fast on it. Approval on
    // A2A is a library-only feature (construct the builder with an approval
    // store directly). See ExecutorOptions::approval_store.
    /// Disable structured logging middleware
    #[arg(long)]
    pub no_logging: bool,

    /// Drop call arguments and output from every log event, error records
    /// included. Credentials passed as tool arguments are otherwise logged at
    /// INFO unless the scanner recognized the option as sensitive, and the
    /// schema-driven redaction cannot cover a `--data` body or a key in a URL.
    /// A failed call still produces one ERROR record naming the module, caller,
    /// error code and duration -- it just carries nothing the caller sent.
    #[arg(long)]
    pub no_log_arguments: bool,

    /// Disable circuit breaker (short-circuits a hanging/broken tool)
    #[arg(long)]
    pub no_circuit_breaker: bool,

    /// Disable retry (only ever retries idempotent commands after a timeout)
    #[arg(long)]
    pub no_retry: bool,

    /// Per-task execution timeout in seconds
    #[arg(long, default_value = "300")]
    pub execution_timeout: u64,

    /// Restrict the served skills to those carrying every listed tag
    /// (comma-separated). Excluded modules are neither advertised on the agent
    /// card nor callable.
    #[arg(long)]
    pub tags: Option<String>,

    /// Restrict the served skills to those whose module ID starts with this
    /// prefix. Excluded modules are neither advertised on the agent card nor
    /// callable.
    #[arg(long)]
    pub prefix: Option<String>,

    /// Allowed CORS origin (repeatable)
    #[arg(long)]
    pub cors_origin: Vec<String>,

    /// Acknowledge binding A2A to a non-loopback address. A2A has no
    /// authenticator at all, so every wrapped binary is reachable from the
    /// network with no credential -- there is no `--auth token` to opt back
    /// into, which is why the acknowledgement is mandatory rather than a
    /// warning.
    #[arg(long)]
    pub allow_unauthenticated_bind: bool,
}

impl A2aArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let server = self.build_server(config)?;
        let runtime = tokio::runtime::Runtime::new()?;
        runtime
            .block_on(server.serve())
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    fn build_server(&self, config: &ApexeConfig) -> anyhow::Result<crate::a2a::A2aServerBuilder> {
        let modules_dir = self
            .modules_dir
            .clone()
            .unwrap_or_else(|| config.modules_dir.clone());
        let mut builder = crate::a2a::A2aServerBuilder::new()
            .name(&self.name)
            .url(&self.url)
            .explorer(self.explorer)
            .modules_dir(modules_dir)
            .timeout_ms(config.default_timeout * 1000)
            .enable_logging(!self.no_logging)
            .log_arguments(!self.no_log_arguments)
            .enable_circuit_breaker(!self.no_circuit_breaker)
            .enable_retry(!self.no_retry)
            .execution_timeout(self.execution_timeout)
            .cors_origins(self.cors_origin.clone())
            .allow_unauthenticated_bind(self.allow_unauthenticated_bind)
            .audit_path(config.audit_log.clone());

        if let Some(ref tags_str) = self.tags {
            builder = builder.tags(parse_tag_list(tags_str)?);
        }
        if let Some(ref prefix) = self.prefix {
            builder = builder.prefix(prefix);
        }

        // Only load ACL when explicitly specified via --acl flag.
        // Without --acl, the server runs without access control (all tools allowed).
        if let Some(ref acl_path) = self.acl {
            builder = builder.acl_path(acl_path);
        }

        Ok(builder)
    }
}

/// List previously scanned CLI tools and their modules.
#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// Output format
    #[arg(long, default_value = "table", value_parser = ["json", "table"])]
    pub format: String,

    /// Directory containing binding files
    #[arg(long)]
    pub modules_dir: Option<PathBuf>,

    /// Print each module's behavioral annotations (readonly, destructive,
    /// idempotent, requires_approval, open_world) and, when an ACL policy is
    /// loaded, the allow/deny decision an unauthenticated caller would get
    /// from it.
    ///
    /// Rendered with apcore-cli's own `format_module_detail` -- the renderer
    /// `apcore-cli describe` uses -- rather than a bespoke one, so a module's
    /// annotations appear exactly as they would there; the ACL decision rides
    /// along through the same `x-`-prefixed extension-field convention that
    /// renderer already documents for caller-supplied metadata.
    #[arg(long)]
    pub verbose: bool,

    /// ACL policy file to evaluate against in `--verbose` output.
    ///
    /// Defaults to `<config_dir>/acl.yaml` -- where `apexe scan` writes one --
    /// when that file exists. This is a read-only report: unlike `--acl` on
    /// `serve`/`a2a`, naming a file here does not enable enforcement anywhere.
    #[arg(long)]
    pub acl: Option<PathBuf>,

    /// Only list modules whose underlying binary is still reachable on this
    /// machine.
    ///
    /// A binding file's `target` is a snapshot taken at `apexe scan` time --
    /// the tool it names can be uninstalled, moved, or the `modules_dir`
    /// copied to a different host since. Off by default so `list` still shows
    /// everything ever scanned (e.g. to review before moving to a new
    /// machine); `apexe serve`/`apexe a2a` apply this check unconditionally,
    /// since a server should never advertise a tool it cannot run.
    #[arg(long)]
    pub available_only: bool,
}

impl ListArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let modules_dir = self.modules_dir.as_ref().unwrap_or(&config.modules_dir);

        let mut modules = self.load_modules(modules_dir)?;
        if modules.is_empty() {
            println!("No modules found. Run 'apexe scan <tool>' first.");
            return Ok(());
        }

        if self.available_only {
            let loaded = modules.len();
            modules.retain(|m| crate::scanner::resolver::target_is_available(&m.target));
            if modules.is_empty() {
                println!(
                    "No modules found. {loaded} scanned module(s) exist but none of their \
                     binaries are reachable on this machine."
                );
                return Ok(());
            }
        }

        if self.verbose {
            self.print_verbose(&modules, config)?;
        } else {
            self.print_modules(&modules)?;
        }
        Ok(())
    }

    fn load_modules(
        &self,
        dir: &std::path::Path,
    ) -> anyhow::Result<Vec<apcore_toolkit::ScannedModule>> {
        // An absent directory legitimately means "no modules yet". A load
        // FAILURE (e.g. a corrupt .binding.yaml) must surface, not be masked as
        // an empty list that misleadingly prints "No modules found".
        if !dir.exists() {
            return Ok(vec![]);
        }
        crate::output::load_modules_from_dir(dir).map_err(|e| anyhow::anyhow!(e))
    }

    /// `--acl`, or `<config_dir>/acl.yaml` when `--acl` is absent and that
    /// file exists. Neither is required: without one, `--verbose` still
    /// prints annotations, just no ACL decision.
    fn resolve_acl_path(&self, config: &ApexeConfig) -> Option<PathBuf> {
        if let Some(path) = &self.acl {
            return Some(path.clone());
        }
        let default_path = config.config_dir.join("acl.yaml");
        default_path.exists().then_some(default_path)
    }

    fn print_verbose(
        &self,
        modules: &[apcore_toolkit::ScannedModule],
        config: &ApexeConfig,
    ) -> anyhow::Result<()> {
        let acl_path = self.resolve_acl_path(config);
        let acl_manager = match &acl_path {
            Some(path) => Some(
                crate::governance::AclManager::from_config(path)
                    .map_err(|e| anyhow::anyhow!("Failed to load ACL '{}': {e}", path.display()))?,
            ),
            None => None,
        };

        let acl_header = match &acl_path {
            Some(path) => format!("ACL policy: {}", path.display()),
            None => {
                "ACL policy: none (pass --acl, or run `apexe scan` to generate one)".to_string()
            }
        };

        let mut sorted: Vec<&apcore_toolkit::ScannedModule> = modules.iter().collect();
        sorted.sort_by(|a, b| a.module_id.cmp(&b.module_id));

        if self.format == "json" {
            let mut items: Vec<serde_json::Value> = Vec::with_capacity(sorted.len());
            for module in &sorted {
                let descriptor = Self::verbose_descriptor(module, acl_manager.as_ref())?;
                // Round-tripped through apcore-cli's own JSON renderer so the
                // shape matches `apcore-cli describe` exactly (including which
                // fields it keeps and how it surfaces `x-` extensions),
                // instead of dumping this crate's internal `ScannedModule`
                // layout.
                let rendered = apcore_cli::format_module_detail(&descriptor, "json");
                items.push(serde_json::from_str(&rendered)?);
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "acl_policy": acl_path.map(|p| p.display().to_string()),
                    "modules": items,
                }))?
            );
            return Ok(());
        }

        println!("{acl_header}\n");
        for module in sorted {
            let descriptor = Self::verbose_descriptor(module, acl_manager.as_ref())?;
            println!("{}", apcore_cli::format_module_detail(&descriptor, "table"));
        }
        Ok(())
    }

    /// A module's descriptor JSON, with the ACL decision (if any) layered on
    /// via `x-acl-effect` / `x-acl-rule` -- apcore-cli's own extension-field
    /// convention for caller-supplied metadata it does not otherwise know
    /// about (see `format_module_detail`'s "Extension Metadata" section).
    fn verbose_descriptor(
        module: &apcore_toolkit::ScannedModule,
        acl_manager: Option<&crate::governance::AclManager>,
    ) -> anyhow::Result<serde_json::Value> {
        let mut descriptor = serde_json::to_value(module)?;
        if let Some(manager) = acl_manager {
            let decision = manager.explain(&module.module_id);
            if let Some(obj) = descriptor.as_object_mut() {
                obj.insert(
                    "x-acl-effect".to_string(),
                    serde_json::Value::String(decision.effect.clone()),
                );
                let rule = match decision.matched_rule_index {
                    Some(idx) => {
                        let desc = decision
                            .matched_rule_description
                            .as_deref()
                            .unwrap_or("(no description)");
                        let caveat = if decision.matched_rule_has_conditions {
                            "; also carries `conditions`, evaluated against the real \
                             caller's identity/role at call time -- the effect above may \
                             not hold for every caller"
                        } else {
                            ""
                        };
                        format!("rule {idx}: {desc}{caveat}")
                    }
                    None => format!(
                        "no rule matched; default_effect: {}",
                        decision.default_effect
                    ),
                };
                obj.insert("x-acl-rule".to_string(), serde_json::Value::String(rule));
            }
        }
        Ok(descriptor)
    }

    fn print_modules(&self, modules: &[apcore_toolkit::ScannedModule]) -> anyhow::Result<()> {
        let mut sorted: Vec<_> = modules
            .iter()
            .map(|m| (m.module_id.as_str(), m.description.as_str()))
            .collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));

        match self.format.as_str() {
            "json" => {
                let json: Vec<serde_json::Value> = sorted
                    .iter()
                    .map(|(id, desc)| serde_json::json!({"module_id": id, "description": desc}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
            _ => {
                println!("{:<40} DESCRIPTION", "MODULE ID");
                println!("{:<40} {}", "\u{2500}".repeat(40), "\u{2500}".repeat(40));
                for (id, desc) in &sorted {
                    let truncated = if desc.chars().count() > 60 {
                        format!("{}...", desc.chars().take(57).collect::<String>())
                    } else {
                        desc.to_string()
                    };
                    println!("{:<40} {}", id, truncated);
                }
                println!("\n{} module(s) found.", sorted.len());
            }
        }
        Ok(())
    }
}

/// Show or initialize apexe configuration.
#[derive(Debug, clap::Args)]
pub struct ConfigArgs {
    /// Show current configuration
    #[arg(long)]
    pub show: bool,

    /// Initialize default config file
    #[arg(long)]
    pub init: bool,
}

impl ConfigArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        if self.show {
            let yaml = serde_yaml::to_string(config)?;
            println!("{yaml}");
        }
        if self.init {
            let config_path = config.config_dir.join("config.yaml");
            if !config_path.exists() {
                let default = ApexeConfig::default();
                let yaml = serde_yaml::to_string(&default)?;
                std::fs::write(&config_path, yaml)?;
                println!("Config written to {}", config_path.display());
            } else {
                println!("Config already exists at {}", config_path.display());
            }
        }
        Ok(())
    }
}

/// Report the filesystem boundary wrapped tools are checked against, or
/// check one path against it directly.
///
/// This is the same [`crate::governance::PathGuard`] `Cli::run` installs
/// before any subcommand executes anything (see `install_path_guard`), read
/// through [`crate::governance::path_guard::active`] rather than rebuilt --
/// so the summary can never describe a policy other than the one actually in
/// force. `--path` goes one step further and calls the guard's own `check`,
/// the exact call a wrapped tool's argument goes through, so its verdict
/// cannot drift from what a real invocation would get.
#[derive(Debug, clap::Args)]
pub struct PolicyArgs {
    /// Check this path against the guard instead of printing the whole
    /// policy. Resolved the same way a wrapped tool's argument would be:
    /// joined to the working directory, symlinks followed, `..` collapsed.
    #[arg(long)]
    pub path: Option<String>,

    /// Access mode `--path` is checked under. `write` is the conservative
    /// default every unannotated module gets; `read` is the narrower
    /// boundary a `readonly`-annotated module is held to.
    #[arg(long, default_value = "write", value_parser = ["read", "write"])]
    pub mode: String,

    /// Output format
    #[arg(long, default_value = "table", value_parser = ["json", "table"])]
    pub format: String,
}

impl PolicyArgs {
    pub fn execute(self, _config: &ApexeConfig) -> anyhow::Result<()> {
        let guard = crate::governance::path_guard::active();
        match &self.path {
            Some(path) => self.check_path(guard, path),
            None => self.print_summary(guard),
        }
    }

    fn access_mode(&self) -> crate::governance::AccessMode {
        if self.mode == "read" {
            crate::governance::AccessMode::ReadOnly
        } else {
            crate::governance::AccessMode::Write
        }
    }

    fn check_path(&self, guard: &crate::governance::PathGuard, path: &str) -> anyhow::Result<()> {
        let result = guard.check("the given --path", path, self.access_mode());

        if self.format == "json" {
            let json = match &result {
                Ok(()) => serde_json::json!({
                    "path": path,
                    "mode": self.mode,
                    "decision": "allow",
                }),
                Err(e) => serde_json::json!({
                    "path": path,
                    "mode": self.mode,
                    "decision": "deny",
                    "reason": e.message,
                    "details": e.details,
                    "ai_guidance": e.ai_guidance,
                }),
            };
            println!("{}", serde_json::to_string_pretty(&json)?);
            return Ok(());
        }

        match &result {
            Ok(()) => println!("ALLOW  {path}  (mode: {})", self.mode),
            Err(e) => {
                println!("DENY   {path}  (mode: {})", self.mode);
                println!("  {}", e.message);
                if let Some(guidance) = &e.ai_guidance {
                    println!("  hint: {guidance}");
                }
            }
        }
        Ok(())
    }

    fn print_summary(&self, guard: &crate::governance::PathGuard) -> anyhow::Result<()> {
        if self.format == "json" {
            let json = serde_json::json!({
                "root": guard.root().display().to_string(),
                "system_baseline": Self::path_list(guard.system_baseline()),
                "credential_baseline": Self::path_list(guard.credential_baseline()),
                "credential_configured": Self::path_list(guard.credential_configured()),
                "allowed_paths": Self::path_list(guard.allowed_paths()),
                "exempt_paths": Self::path_list(guard.exempt_paths()),
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
            return Ok(());
        }

        println!(
            "Working directory (relative paths resolve here): {}\n",
            guard.root().display()
        );
        Self::print_section(
            "System paths (builtin; write refused, read allowed)",
            guard.system_baseline(),
        );
        Self::print_section(
            "Credential paths (builtin; refused to read and write)",
            guard.credential_baseline(),
        );
        Self::print_section(
            "Credential paths (config.yaml additional_denied_paths; refused to read and write)",
            guard.credential_configured(),
        );
        Self::print_section(
            "Carve-outs (config.yaml allowed_paths -- the only setting that relaxes the guard)",
            guard.allowed_paths(),
        );
        Self::print_section(
            "Derived exemptions (temp directory; not an operator decision)",
            guard.exempt_paths(),
        );
        Ok(())
    }

    fn print_section(title: &str, paths: &[PathBuf]) {
        println!("{title}:");
        if paths.is_empty() {
            println!("  (none)");
        } else {
            for path in paths {
                println!("  {}", path.display());
            }
        }
        println!();
    }

    fn path_list(paths: &[PathBuf]) -> Vec<String> {
        paths.iter().map(|p| p.display().to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_scan_subcommand() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git"]).unwrap();
        assert!(matches!(cli.command, Commands::Scan(_)));
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.tools, vec!["git".to_string()]);
        }
    }

    #[test]
    fn test_parse_no_subcommand_fails() {
        let result = Cli::try_parse_from(["apexe"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_log_level_flag() {
        let cli = Cli::try_parse_from(["apexe", "--log-level", "debug", "scan", "git"]).unwrap();
        assert_eq!(cli.log_level.as_deref(), Some("debug"));
        // Explicit flag wins over the config-resolved level.
        assert_eq!(cli.effective_log_level("warn"), "debug");
    }

    #[test]
    fn test_parse_default_log_level() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git"]).unwrap();
        assert_eq!(cli.log_level, None);
        // With no flag, the resolved config level is used (not a hardcoded "info").
        assert_eq!(cli.effective_log_level("debug"), "debug");
    }

    // ScanArgs validation tests
    #[test]
    fn test_scan_no_tools_fails() {
        let result = Cli::try_parse_from(["apexe", "scan"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_depth_zero_fails() {
        let result = Cli::try_parse_from(["apexe", "scan", "git", "--depth", "0"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_depth_six_fails() {
        let result = Cli::try_parse_from(["apexe", "scan", "git", "--depth", "6"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_depth_three_succeeds() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git", "--depth", "3"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.depth, 3);
        }
    }

    #[test]
    fn test_scan_format_xml_fails() {
        let result = Cli::try_parse_from(["apexe", "scan", "git", "--format", "xml"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_format_json_succeeds() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git", "--format", "json"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.format, "json");
        }
    }

    #[test]
    fn test_scan_multiple_tools() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git", "docker"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.tools, vec!["git".to_string(), "docker".to_string()]);
        }
    }

    #[test]
    fn test_scan_default_depth() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.depth, 2);
        }
    }

    #[test]
    fn test_scan_default_format() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.format, "table");
        }
    }

    #[test]
    fn test_scan_skills_dir_default_none() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert!(args.skills_dir.is_none());
        }
    }

    #[test]
    fn test_scan_overlay_default_none() {
        let cli = Cli::try_parse_from(["apexe", "scan", "ls"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert!(args.overlay.is_none());
        } else {
            panic!("expected Commands::Scan");
        }
    }

    #[test]
    fn test_scan_overlay_flag() {
        let cli =
            Cli::try_parse_from(["apexe", "scan", "ls", "--overlay", "/tmp/ls.json"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.overlay, Some(PathBuf::from("/tmp/ls.json")));
        } else {
            panic!("expected Commands::Scan");
        }
    }

    #[test]
    fn test_scan_execute_reports_unreadable_overlay() {
        // A named overlay that cannot be read must fail the scan, not silently
        // fall back to heuristics under an authoritative-looking banner.
        let config = ApexeConfig::default();
        let args = ScanArgs {
            verify: false,
            dry_run: false,
            tools: vec!["echo".to_string()],
            output_dir: None,
            depth: 1,
            no_cache: true,
            format: "table".to_string(),
            skills_dir: None,
            overlay: Some(PathBuf::from("/nonexistent/overlay_xyz.json")),
        };
        let err = args.execute(&config).unwrap_err().to_string();
        assert!(
            err.contains("Failed to read overlay"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_scan_skills_dir_flag() {
        let cli =
            Cli::try_parse_from(["apexe", "scan", "git", "--skills-dir", "/tmp/skills"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.skills_dir, Some(PathBuf::from("/tmp/skills")));
        }
    }

    // ServeArgs validation tests
    #[test]
    fn test_serve_defaults() {
        let cli = Cli::try_parse_from(["apexe", "serve"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert_eq!(args.transport, "stdio");
            assert_eq!(args.host, "127.0.0.1");
            assert_eq!(args.port, 8000);
            assert!(!args.explorer);
        }
    }

    #[test]
    fn test_serve_invalid_transport_fails() {
        let result = Cli::try_parse_from(["apexe", "serve", "--transport", "invalid"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serve_port_zero_fails() {
        let result = Cli::try_parse_from(["apexe", "serve", "--port", "0"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serve_with_all_flags() {
        let cli = Cli::try_parse_from([
            "apexe",
            "serve",
            "--transport",
            "http",
            "--host",
            "0.0.0.0",
            "--port",
            "9000",
            "--explorer",
        ])
        .unwrap();
        if let Commands::Serve(args) = cli.command {
            assert_eq!(args.transport, "http");
            assert_eq!(args.host, "0.0.0.0");
            assert_eq!(args.port, 9000);
            assert!(args.explorer);
        }
    }

    #[test]
    fn test_serve_resilience_flags_default_enabled() {
        let cli = Cli::try_parse_from(["apexe", "serve"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert!(!args.no_circuit_breaker);
            assert!(!args.no_retry);
        } else {
            panic!("expected Commands::Serve");
        }
    }

    #[test]
    fn test_serve_metrics_default_disabled() {
        let cli = Cli::try_parse_from(["apexe", "serve"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert!(!args.metrics);
        } else {
            panic!("expected Commands::Serve");
        }
    }

    #[test]
    fn test_serve_metrics_flag() {
        let cli = Cli::try_parse_from(["apexe", "serve", "--metrics"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert!(args.metrics);
        } else {
            panic!("expected Commands::Serve");
        }
    }

    #[test]
    fn test_serve_resilience_flags_can_be_disabled() {
        let cli =
            Cli::try_parse_from(["apexe", "serve", "--no-circuit-breaker", "--no-retry"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert!(args.no_circuit_breaker);
            assert!(args.no_retry);
        } else {
            panic!("expected Commands::Serve");
        }
    }

    #[test]
    fn test_serve_auth_defaults_to_unset() {
        // Unset means "per-transport default", resolved in crate::auth —
        // not "no auth".
        let cli = Cli::try_parse_from(["apexe", "serve"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert!(args.auth.is_none());
            assert!(!args.allow_unauthenticated_bind);
            assert!(!args.allow_deprecated_sse);
            assert!(!args.no_log_arguments);
        } else {
            panic!("expected Commands::Serve");
        }
    }

    #[test]
    fn test_serve_auth_rejects_unknown_mode() {
        let result = Cli::try_parse_from(["apexe", "serve", "--auth", "basic"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serve_auth_options_maps_flags() {
        let cli = Cli::try_parse_from([
            "apexe",
            "serve",
            "--auth",
            "token",
            "--auth-token",
            "s3cret",
            "--allow-unauthenticated-bind",
        ])
        .unwrap();
        let Commands::Serve(args) = cli.command else {
            panic!("expected Commands::Serve");
        };
        let opts = args.auth_options().unwrap();
        assert_eq!(opts.mode, Some(crate::auth::AuthMode::Token));
        assert_eq!(opts.token.as_deref(), Some("s3cret"));
        assert!(opts.allow_unauthenticated_bind);
    }

    #[test]
    fn test_serve_auth_options_defaults_to_no_explicit_mode() {
        let cli = Cli::try_parse_from(["apexe", "serve"]).unwrap();
        let Commands::Serve(args) = cli.command else {
            panic!("expected Commands::Serve");
        };
        let opts = args.auth_options().unwrap();
        assert!(opts.mode.is_none());
        assert!(!opts.allow_unauthenticated_bind);
    }

    #[test]
    fn test_serve_rejects_removed_skip_validation_flag() {
        // `--skip-validation` skipped nothing: schema validation lives in
        // apcore's `input_validation` pipeline step, which apexe never
        // removes. Failing at parse is louder than accepting a no-op that
        // reads as "validation is off".
        let result = Cli::try_parse_from(["apexe", "serve", "--skip-validation"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serve_show_config_rejects_unknown_format() {
        // `--show-config vscode` used to print a sentence to stdout and exit
        // 0, so `> mcp.json` wrote that sentence as the file body.
        let result = Cli::try_parse_from(["apexe", "serve", "--show-config", "vscode"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_tag_list_splits_and_trims() {
        assert_eq!(
            parse_tag_list("readonly, git ,cli").unwrap(),
            vec!["readonly", "git", "cli"]
        );
        assert_eq!(parse_tag_list("readonly").unwrap(), vec!["readonly"]);
    }

    #[test]
    fn test_parse_tag_list_rejects_an_empty_tag() {
        // `--tags readonly,` splits to ["readonly", ""], and `admits` requires
        // every token. No module carries an empty tag, so the registry comes
        // up empty and the whole tool surface is uncallable — previously
        // signalled only by `admitted=0` at info level.
        for raw in ["readonly,", ",readonly", "readonly,,git", "readonly, ,git"] {
            let err = parse_tag_list(raw).unwrap_err().to_string();
            assert!(err.contains("empty tag"), "{raw}: {err}");
            assert!(err.contains("no callable"), "{raw}: {err}");
        }
        assert!(parse_tag_list("").is_err());
    }

    #[test]
    fn test_serve_build_server_rejects_a_trailing_comma_in_tags() {
        let cli = Cli::try_parse_from(["apexe", "serve", "--tags", "readonly,"]).unwrap();
        let Commands::Serve(args) = cli.command else {
            panic!("expected Commands::Serve");
        };
        let err = match args.build_server(&ApexeConfig::default()) {
            Err(err) => err,
            Ok(_) => panic!("a trailing comma must not produce a toolless server"),
        };
        assert!(err.to_string().contains("empty tag"), "{err}");
    }

    #[test]
    fn test_serve_invocation_carries_every_surface_flag() {
        let cli = Cli::try_parse_from([
            "apexe",
            "serve",
            "--modules-dir",
            "/srv/modules",
            "--tags",
            "readonly",
            "--prefix",
            "cli.git",
            "--acl",
            "/etc/apexe/acl.yaml",
            "--enable-approval",
            "--no-retry",
            "--name",
            "mytools",
        ])
        .unwrap();
        let Commands::Serve(args) = cli.command else {
            panic!("expected Commands::Serve");
        };
        let invocation = args.invocation();
        assert_eq!(invocation.name, "mytools");
        assert_eq!(invocation.modules_dir, Some(PathBuf::from("/srv/modules")));
        assert_eq!(invocation.tags.as_deref(), Some("readonly"));
        assert_eq!(invocation.prefix.as_deref(), Some("cli.git"));
        assert_eq!(invocation.acl, Some(PathBuf::from("/etc/apexe/acl.yaml")));
        assert!(invocation.enable_approval);
        assert!(invocation.no_retry);
        assert!(!invocation.no_logging);
    }

    #[test]
    fn test_serve_invocation_omits_credentials() {
        // ServeInvocation has no credential field at all; this pins that the
        // rendered snippet cannot carry one.
        let cli = Cli::try_parse_from([
            "apexe",
            "serve",
            "--transport",
            "http",
            "--auth",
            "token",
            "--auth-token",
            "super-secret-value",
        ])
        .unwrap();
        let Commands::Serve(args) = cli.command else {
            panic!("expected Commands::Serve");
        };
        let snippet = config_gen::generate_config("claude-desktop", &args.invocation()).unwrap();
        assert!(
            !snippet.contains("super-secret-value"),
            "snippet leaked the bearer token: {snippet}"
        );
    }

    // A2aArgs validation tests
    #[test]
    fn test_a2a_rejects_enable_approval_flag() {
        // A2A has no elicitation and no CLI approval-store path, so
        // --enable-approval is not a valid a2a flag (it would only ever error
        // at serve time). It must be rejected at parse.
        let result = Cli::try_parse_from(["apexe", "a2a", "--enable-approval"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_a2a_build_server_wires_the_surface_filters() {
        // The three a2a filter tests all drive `A2aServerBuilder` directly, so
        // the CLI-to-builder wiring was unguarded: dropping either `if let` arm
        // left every test green while `apexe a2a --prefix` served the whole
        // scanned surface. That matters more here than on `apexe serve` —
        // A2A has no authenticator, so narrowing the registered surface is the
        // only limiting mechanism available.
        let cli =
            Cli::try_parse_from(["apexe", "a2a", "--prefix", "cli.git.", "--tags", "readonly"])
                .unwrap();
        let Commands::A2a(args) = cli.command else {
            panic!("expected Commands::A2a");
        };
        let builder = args
            .build_server(&ApexeConfig::default())
            .expect("well-formed flags build a server");
        let filter = builder.module_filter();

        assert_eq!(filter.prefix.as_deref(), Some("cli.git."));
        assert_eq!(
            filter.tags.as_deref(),
            Some(["readonly".to_string()].as_slice())
        );
    }

    #[test]
    fn test_a2a_build_server_rejects_a_trailing_comma_in_tags() {
        let cli = Cli::try_parse_from(["apexe", "a2a", "--tags", "readonly,"]).unwrap();
        let Commands::A2a(args) = cli.command else {
            panic!("expected Commands::A2a");
        };
        let err = match args.build_server(&ApexeConfig::default()) {
            Ok(_) => panic!("an empty tag makes the filter unsatisfiable"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("empty tag"),
            "the refusal must name the stray comma: {err}"
        );
    }

    fn scan_args(verify: bool) -> ScanArgs {
        let cli = Cli::try_parse_from(
            ["apexe", "scan", "ls"]
                .iter()
                .copied()
                .chain(verify.then_some("--verify")),
        )
        .unwrap();
        match cli.command {
            Commands::Scan(args) => args,
            _ => panic!("expected Commands::Scan"),
        }
    }

    #[test]
    fn test_verify_turns_an_unverified_binding_into_a_failure() {
        // The YAML verifier already ran on every write; its verdict was
        // discarded, so a binding that does not parse was reported as
        // "Generated binding" and the command exited 0. `--verify` is what
        // makes that a failure, which is the form a pipeline needs.
        let unverified = |verify: bool| -> anyhow::Result<()> {
            scan_args(verify)
                .report_unverified(&["cli.broken: could not parse as YAML".to_string()])
        };

        assert!(
            unverified(false).is_ok(),
            "without the flag an unverified binding is a warning: one bad tool \
             must not throw away the scan of the others"
        );
        let err = match unverified(true) {
            Ok(()) => panic!("--verify must fail on an unverified binding"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("cli.broken"),
            "the failure names the module: {err}"
        );
        assert!(
            err.contains("could not parse as YAML"),
            "and the reason: {err}"
        );
    }

    #[test]
    fn test_scan_args_verify_flag() {
        let cli = Cli::try_parse_from(["apexe", "scan", "ls", "--verify"]).unwrap();
        let Commands::Scan(args) = cli.command else {
            panic!("expected Commands::Scan");
        };
        assert!(args.verify);
        assert!(!args.dry_run, "the two flags are independent");
    }

    #[test]
    fn test_scan_args_dry_run_flag() {
        let cli = Cli::try_parse_from(["apexe", "scan", "ls", "--dry-run"]).unwrap();
        let Commands::Scan(args) = cli.command else {
            panic!("expected Commands::Scan");
        };
        assert!(args.dry_run);
        assert!(!args.verify);
    }

    #[test]
    fn test_scan_args_default_to_neither_flag() {
        let cli = Cli::try_parse_from(["apexe", "scan", "ls"]).unwrap();
        let Commands::Scan(args) = cli.command else {
            panic!("expected Commands::Scan");
        };
        assert!(!args.verify, "a scan must not fail on a warning by default");
        assert!(!args.dry_run, "a scan writes by default");
    }

    #[test]
    fn test_global_timeout_flag() {
        // Global, so it parses after the subcommand too — which is how anyone
        // actually types it.
        for argv in [
            ["apexe", "--timeout", "120", "scan", "ls"],
            ["apexe", "scan", "ls", "--timeout", "120"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert_eq!(cli.timeout, Some(120), "{argv:?}");
        }
    }

    #[test]
    fn test_global_timeout_rejects_zero() {
        // A zero timeout would kill every call before it started.
        assert!(Cli::try_parse_from(["apexe", "--timeout", "0", "scan", "ls"]).is_err());
    }

    #[test]
    fn test_timeout_override_beats_the_config_file() {
        let config = ApexeConfig {
            default_timeout: 30,
            ..ApexeConfig::default()
        };
        assert_eq!(
            config.clone().with_timeout_override(None).default_timeout,
            30
        );
        assert_eq!(
            config.with_timeout_override(Some(120)).default_timeout,
            120,
            "a CLI flag outranks config.yaml, matching --log-level"
        );
    }

    #[test]
    fn test_a2a_defaults() {
        let cli = Cli::try_parse_from(["apexe", "a2a"]).unwrap();
        if let Commands::A2a(args) = cli.command {
            assert_eq!(args.url, "http://127.0.0.1:8000");
            assert_eq!(args.execution_timeout, 300);
            assert!(!args.explorer);
            assert!(args.cors_origin.is_empty());
        } else {
            panic!("expected Commands::A2a");
        }
    }

    #[test]
    fn test_a2a_with_flags() {
        let cli = Cli::try_parse_from([
            "apexe",
            "a2a",
            "--url",
            "http://0.0.0.0:9090",
            "--explorer",
            "--execution-timeout",
            "600",
            "--cors-origin",
            "https://example.com",
            "--cors-origin",
            "https://foo.example.com",
        ])
        .unwrap();
        if let Commands::A2a(args) = cli.command {
            assert_eq!(args.url, "http://0.0.0.0:9090");
            assert!(args.explorer);
            assert_eq!(args.execution_timeout, 600);
            assert_eq!(
                args.cors_origin,
                vec!["https://example.com", "https://foo.example.com"]
            );
        } else {
            panic!("expected Commands::A2a");
        }
    }

    #[test]
    fn test_a2a_resilience_flags_can_be_disabled() {
        let cli =
            Cli::try_parse_from(["apexe", "a2a", "--no-circuit-breaker", "--no-retry"]).unwrap();
        if let Commands::A2a(args) = cli.command {
            assert!(args.no_circuit_breaker);
            assert!(args.no_retry);
        } else {
            panic!("expected Commands::A2a");
        }
    }

    // ListArgs validation tests
    #[test]
    fn test_list_default_format() {
        let cli = Cli::try_parse_from(["apexe", "list"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert_eq!(args.format, "table");
        }
    }

    #[test]
    fn test_list_format_json() {
        let cli = Cli::try_parse_from(["apexe", "list", "--format", "json"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert_eq!(args.format, "json");
        }
    }

    #[test]
    fn test_list_format_xml_fails() {
        let result = Cli::try_parse_from(["apexe", "list", "--format", "xml"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_verbose_flag() {
        let cli = Cli::try_parse_from(["apexe", "list", "--verbose"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert!(args.verbose);
            assert_eq!(args.acl, None);
        } else {
            panic!("expected Commands::List");
        }
    }

    #[test]
    fn test_list_acl_flag() {
        let cli =
            Cli::try_parse_from(["apexe", "list", "--verbose", "--acl", "/tmp/acl.yaml"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert_eq!(args.acl, Some(PathBuf::from("/tmp/acl.yaml")));
        } else {
            panic!("expected Commands::List");
        }
    }

    #[test]
    fn test_list_available_only_flag_defaults_false() {
        let cli = Cli::try_parse_from(["apexe", "list"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert!(!args.available_only);
        } else {
            panic!("expected Commands::List");
        }
    }

    #[test]
    fn test_list_available_only_flag_parses() {
        let cli = Cli::try_parse_from(["apexe", "list", "--available-only"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert!(args.available_only);
        } else {
            panic!("expected Commands::List");
        }
    }

    #[test]
    fn test_list_available_only_excludes_modules_with_missing_binaries() {
        let tmp = tempfile::TempDir::new().unwrap();
        let modules = vec![
            apcore_toolkit::ScannedModule::new(
                "cli.real".to_string(),
                "Exists".to_string(),
                serde_json::json!({"type": "object"}),
                serde_json::json!({"type": "object"}),
                vec!["cli".to_string()],
                "exec:///bin/echo hello".to_string(),
            ),
            apcore_toolkit::ScannedModule::new(
                "cli.ghost".to_string(),
                "Does not exist".to_string(),
                serde_json::json!({"type": "object"}),
                serde_json::json!({"type": "object"}),
                vec!["cli".to_string()],
                "exec:///nonexistent/zzz_no_such_binary_xyz".to_string(),
            ),
        ];
        crate::output::YamlOutput::without_verification()
            .write(&modules, tmp.path(), false)
            .unwrap();

        let args = ListArgs {
            format: "table".to_string(),
            modules_dir: Some(tmp.path().to_path_buf()),
            verbose: false,
            acl: None,
            available_only: true,
        };
        let loaded = args.load_modules(tmp.path()).unwrap();
        let available: Vec<_> = loaded
            .iter()
            .filter(|m| crate::scanner::resolver::target_is_available(&m.target))
            .collect();
        assert_eq!(loaded.len(), 2);
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].module_id, "cli.real");
    }

    #[test]
    fn test_list_resolve_acl_path_prefers_explicit_flag() {
        let tmp = tempfile::TempDir::new().unwrap();
        let default_acl = tmp.path().join("acl.yaml");
        std::fs::write(&default_acl, "rules: []\ndefault_effect: deny\n").unwrap();
        let explicit_acl = tmp.path().join("explicit.yaml");
        std::fs::write(&explicit_acl, "rules: []\ndefault_effect: allow\n").unwrap();

        let config = ApexeConfig {
            config_dir: tmp.path().to_path_buf(),
            ..ApexeConfig::default()
        };
        let args = ListArgs {
            format: "table".to_string(),
            modules_dir: None,
            verbose: true,
            acl: Some(explicit_acl.clone()),
            available_only: false,
        };
        assert_eq!(args.resolve_acl_path(&config), Some(explicit_acl));
    }

    #[test]
    fn test_list_resolve_acl_path_defaults_to_config_dir_acl_yaml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let default_acl = tmp.path().join("acl.yaml");
        std::fs::write(&default_acl, "rules: []\ndefault_effect: deny\n").unwrap();

        let config = ApexeConfig {
            config_dir: tmp.path().to_path_buf(),
            ..ApexeConfig::default()
        };
        let args = ListArgs {
            format: "table".to_string(),
            modules_dir: None,
            verbose: true,
            acl: None,
            available_only: false,
        };
        assert_eq!(args.resolve_acl_path(&config), Some(default_acl));
    }

    #[test]
    fn test_list_resolve_acl_path_none_when_nothing_configured_or_generated() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = ApexeConfig {
            config_dir: tmp.path().to_path_buf(),
            ..ApexeConfig::default()
        };
        let args = ListArgs {
            format: "table".to_string(),
            modules_dir: None,
            verbose: true,
            acl: None,
            available_only: false,
        };
        assert_eq!(args.resolve_acl_path(&config), None);
    }

    #[test]
    fn test_list_verbose_json_reports_annotations_and_acl_decision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let acl_path = tmp.path().join("acl.yaml");
        std::fs::write(
            &acl_path,
            "rules:\n  - callers: [\"*\"]\n    targets: [\"cli.ls\"]\n    effect: allow\n    description: \"Auto-allow readonly CLI commands\"\ndefault_effect: deny\n",
        )
        .unwrap();

        let mut module = apcore_toolkit::ScannedModule::new(
            "cli.ls".to_string(),
            "List files".to_string(),
            serde_json::json!({"type": "object"}),
            serde_json::json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/ls".to_string(),
        );
        module.annotations = Some(apcore::module::ModuleAnnotations {
            readonly: true,
            ..Default::default()
        });

        let acl_manager = crate::governance::AclManager::from_config(&acl_path).unwrap();
        let descriptor = ListArgs::verbose_descriptor(&module, Some(&acl_manager)).unwrap();

        assert_eq!(descriptor["x-acl-effect"], "allow");
        assert!(descriptor["x-acl-rule"]
            .as_str()
            .unwrap()
            .contains("Auto-allow readonly CLI commands"));
        assert_eq!(descriptor["annotations"]["readonly"], true);
    }

    // ConfigArgs tests
    #[test]
    fn test_config_show_flag() {
        let cli = Cli::try_parse_from(["apexe", "config", "--show"]).unwrap();
        if let Commands::Config(args) = cli.command {
            assert!(args.show);
            assert!(!args.init);
        }
    }

    #[test]
    fn test_config_init_flag() {
        let cli = Cli::try_parse_from(["apexe", "config", "--init"]).unwrap();
        if let Commands::Config(args) = cli.command {
            assert!(!args.show);
            assert!(args.init);
        }
    }

    #[test]
    fn test_config_no_flags_parses() {
        let cli = Cli::try_parse_from(["apexe", "config"]).unwrap();
        if let Commands::Config(args) = cli.command {
            assert!(!args.show);
            assert!(!args.init);
        }
    }

    // ConfigArgs execute tests
    #[test]
    fn test_config_no_flags_is_noop() {
        let config = ApexeConfig::default();
        let args = ConfigArgs {
            show: false,
            init: false,
        };
        let result = args.execute(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_config_show_outputs_valid_yaml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = ApexeConfig {
            modules_dir: tmp.path().join("modules"),
            cache_dir: tmp.path().join("cache"),
            config_dir: tmp.path().to_path_buf(),
            audit_log: tmp.path().join("audit.jsonl"),
            log_level: "info".to_string(),
            default_timeout: 30,
            scan_depth: 2,
            json_output_preference: true,
            ..ApexeConfig::default()
        };

        // --show should serialize to valid YAML
        let yaml = serde_yaml::to_string(&config).unwrap();
        let deserialized: ApexeConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.log_level, "info");
        assert_eq!(deserialized.default_timeout, 30);
    }

    #[test]
    fn test_config_init_creates_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = ApexeConfig {
            modules_dir: tmp.path().join("modules"),
            cache_dir: tmp.path().join("cache"),
            config_dir: tmp.path().to_path_buf(),
            audit_log: tmp.path().join("audit.jsonl"),
            log_level: "info".to_string(),
            default_timeout: 30,
            scan_depth: 2,
            json_output_preference: true,
            ..ApexeConfig::default()
        };

        let args = ConfigArgs {
            show: false,
            init: true,
        };
        args.execute(&config).unwrap();

        let config_path = tmp.path().join("config.yaml");
        assert!(config_path.exists());

        // Verify the written file is valid YAML
        let contents = std::fs::read_to_string(&config_path).unwrap();
        let parsed: ApexeConfig = serde_yaml::from_str(&contents).unwrap();
        assert_eq!(parsed.log_level, "info");
    }

    #[test]
    fn test_config_init_does_not_overwrite() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(&config_path, "existing content").unwrap();

        let config = ApexeConfig {
            modules_dir: tmp.path().join("modules"),
            cache_dir: tmp.path().join("cache"),
            config_dir: tmp.path().to_path_buf(),
            audit_log: tmp.path().join("audit.jsonl"),
            log_level: "info".to_string(),
            default_timeout: 30,
            scan_depth: 2,
            json_output_preference: true,
            ..ApexeConfig::default()
        };

        let args = ConfigArgs {
            show: false,
            init: true,
        };
        args.execute(&config).unwrap();

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(contents, "existing content");
    }

    // ScanArgs execute error case test
    #[test]
    fn test_scan_execute_nonexistent_tool_errors() {
        let config = ApexeConfig::default();
        let args = ScanArgs {
            verify: false,
            dry_run: false,
            tools: vec!["nonexistent_tool_xyz_12345".to_string()],
            output_dir: None,
            depth: 2,
            no_cache: false,
            format: "table".to_string(),
            skills_dir: None,
            overlay: None,
        };
        let result = args.execute(&config);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not found on PATH"),
            "Expected 'not found on PATH' in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_scan_write_bindings_surfaces_write_failure() {
        // A binding-write failure must return Err (non-zero exit), not warn-and-continue.
        let tmp = tempfile::TempDir::new().unwrap();
        // Use an output path whose parent component is a file, so create_dir_all fails.
        let file_path = tmp.path().join("iamafile");
        std::fs::write(&file_path, "x").unwrap();
        let bad_output = file_path.join("nested");

        let args = ScanArgs {
            verify: false,
            dry_run: false,
            tools: vec!["echo".to_string()],
            output_dir: None,
            depth: 2,
            no_cache: false,
            format: "table".to_string(),
            skills_dir: None,
            overlay: None,
        };
        let modules = vec![apcore_toolkit::ScannedModule::new(
            "cli.echo".to_string(),
            "Echo".to_string(),
            serde_json::json!({"type": "object"}),
            serde_json::json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/echo".to_string(),
        )];

        let result = args.write_bindings(&modules, &bad_output);
        assert!(
            result.is_err(),
            "write_bindings must surface a write failure, not swallow it"
        );
    }

    #[test]
    fn test_write_acl_merges_with_an_existing_policy_instead_of_overwriting_it() {
        // Regression: a second `apexe scan` used to truncate-overwrite
        // acl.yaml from the current batch alone, discarding every rule an
        // earlier scan (or the operator by hand) had put there. `apexe scan
        // ls` then `apexe scan echo` must leave cli.ls's allow rule intact.
        let tmp = tempfile::TempDir::new().unwrap();
        let config = ApexeConfig {
            config_dir: tmp.path().to_path_buf(),
            ..ApexeConfig::default()
        };
        let args = ScanArgs {
            verify: false,
            dry_run: false,
            tools: vec!["ls".to_string()],
            output_dir: None,
            depth: 1,
            no_cache: true,
            format: "table".to_string(),
            skills_dir: None,
            overlay: None,
        };

        let mut readonly_module = apcore_toolkit::ScannedModule::new(
            "cli.ls".to_string(),
            "List".to_string(),
            serde_json::json!({"type": "object"}),
            serde_json::json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/ls".to_string(),
        );
        readonly_module.annotations = Some(apcore::module::ModuleAnnotations {
            readonly: true,
            ..Default::default()
        });
        args.write_acl(&[readonly_module], &config).unwrap();

        let echo_module = apcore_toolkit::ScannedModule::new(
            "cli.echo".to_string(),
            "Echo".to_string(),
            serde_json::json!({"type": "object"}),
            serde_json::json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/echo".to_string(),
        );
        args.write_acl(&[echo_module], &config).unwrap();

        let acl_yaml = std::fs::read_to_string(config.config_dir.join("acl.yaml")).unwrap();
        assert!(
            acl_yaml.contains("cli.ls"),
            "the earlier scan's allow rule must survive a later scan: {acl_yaml}"
        );
    }

    #[test]
    fn test_list_load_modules_surfaces_corrupt_binding() {
        // A corrupt binding file must surface as an error, not a misleading empty list.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("bad.binding.yaml"),
            "not: valid: binding: [[[",
        )
        .unwrap();

        let args = ListArgs {
            format: "table".to_string(),
            modules_dir: Some(tmp.path().to_path_buf()),
            verbose: false,
            acl: None,
            available_only: false,
        };
        let result = args.load_modules(tmp.path());
        assert!(
            result.is_err(),
            "corrupt binding must surface, not collapse to an empty module list"
        );
    }

    // PolicyArgs tests
    #[test]
    fn test_policy_parses_with_no_args() {
        let cli = Cli::try_parse_from(["apexe", "policy"]).unwrap();
        if let Commands::Policy(args) = cli.command {
            assert_eq!(args.path, None);
            assert_eq!(args.mode, "write");
            assert_eq!(args.format, "table");
        } else {
            panic!("expected Commands::Policy");
        }
    }

    #[test]
    fn test_policy_parses_path_and_mode() {
        let cli = Cli::try_parse_from([
            "apexe",
            "policy",
            "--path",
            "/etc/hosts",
            "--mode",
            "read",
            "--format",
            "json",
        ])
        .unwrap();
        if let Commands::Policy(args) = cli.command {
            assert_eq!(args.path.as_deref(), Some("/etc/hosts"));
            assert_eq!(args.mode, "read");
            assert_eq!(args.format, "json");
        } else {
            panic!("expected Commands::Policy");
        }
    }

    #[test]
    fn test_policy_invalid_mode_fails() {
        let result = Cli::try_parse_from(["apexe", "policy", "--path", "/x", "--mode", "execute"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_policy_check_path_denies_a_credential_directory() {
        let guard = crate::governance::PathGuard::new(
            PathBuf::from("/"),
            crate::governance::GuardConfig::default(),
        );
        let home = dirs::home_dir().expect("home directory");
        let ssh_key = home.join(".ssh/id_rsa").to_string_lossy().into_owned();

        let args = PolicyArgs {
            path: Some(ssh_key.clone()),
            mode: "read".to_string(),
            format: "json".to_string(),
        };
        args.check_path(&guard, &ssh_key).unwrap();
        // check_path only prints; assert on the underlying guard call directly
        // for the decision itself.
        assert!(guard
            .check(
                "the given --path",
                &ssh_key,
                crate::governance::AccessMode::ReadOnly
            )
            .is_err());
    }

    #[test]
    fn test_policy_check_path_allows_an_ordinary_path() {
        let guard = crate::governance::PathGuard::new(
            PathBuf::from("/"),
            crate::governance::GuardConfig::default(),
        );
        let args = PolicyArgs {
            path: Some("/srv/data/work.txt".to_string()),
            mode: "write".to_string(),
            format: "json".to_string(),
        };
        assert!(args.check_path(&guard, "/srv/data/work.txt").is_ok());
        assert!(guard
            .check(
                "the given --path",
                "/srv/data/work.txt",
                crate::governance::AccessMode::Write
            )
            .is_ok());
    }

    #[test]
    fn test_policy_summary_lists_baseline_and_configured_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let extra = tmp.path().join("secret-data");
        let guard = crate::governance::PathGuard::new(
            PathBuf::from("/"),
            crate::governance::GuardConfig {
                denied: std::slice::from_ref(&extra),
                ..Default::default()
            },
        );
        let args = PolicyArgs {
            path: None,
            mode: "write".to_string(),
            format: "json".to_string(),
        };
        // print_summary only prints to stdout; exercise it for panics and
        // verify the guard's own accessors carry the configured entry, since
        // that is what the printed JSON is built from. The stored entry is
        // resolved (symlinks followed, e.g. macOS's `/tmp` -> `/private/tmp`),
        // so compare by suffix rather than exact equality with `extra`.
        args.print_summary(&guard).unwrap();
        assert_eq!(guard.credential_configured().len(), 1);
        assert!(guard.credential_configured()[0].ends_with("secret-data"));
    }
}
