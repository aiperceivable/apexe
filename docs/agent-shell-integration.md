# Seamless agent integration: the `apexe sh` design evaluation (rev11)

- **Status**: Draft rev11 (stage 2 implemented: local environment inheritance, local approval, `scanned` original-argv execution and the Claude Code hook; verified with OpenCode 1.19.5, Codex CLI 0.145.0, Claude Code 2.1.318 and pi-coding-agent 1.1.0)
- **Date**: 2026-11-05
- **Scope**: how terminal AI agents (Claude Code, Codex, pi, OpenCode) reach apexe's local execution path
- **Out of scope**: MCP/A2A server behaviour, OpenAPI import, the overlay authoring workflow
- **Revision note**: rev1's "full governance" promise, three execution blockers, host-integration conclusions and acceptance cases were corrected after review (see Appendix D); rev3 records stage 1; rev4 records local environment inheritance, descendant routing and the restricted search path; rev5 records the local approval channel; rev6 records the shim coverage report; rev7 records `scanned` original-argv execution; rev8 records the Claude Code hook; rev9 records the host guide and `apexe agent`; rev10 records automatic config writes and MCP-only generation; rev11 records the experimental persistent shell.

---

## 0. Conclusions (after review)

1. **`apexe sh` is worth building, but its role is "a shell launcher that joins the existing governance path automatically"**, not a new enforcement boundary.
2. It reduces the ordinary mistake of an agent forgetting to call `apexe exec`; it **does not guarantee** that every execution goes through apexe. Prepending to PATH only changes command lookup order, and an agent can still modify PATH, use absolute paths, or route through an interpreter.
3. **The Skill has been demoted to a fallback.** Host shell/hook integration comes first; the Skill performs exec routing only when neither a shell nor a hook can be configured, and it continues to provide discovery, refusal explanations and usage guidance.
4. **Run a host-integration experiment before building the MVP.** Promote a host to the default route only after at least one target host passes end to end.
5. `scanned + approval` and `--locked` are **deferred** for separate review. Under the current implementation neither can support what the documentation originally promised.
6. Exactly one real blocker exists at the executor layer: **commands that pass through the executor have their environment cleared**. The current stdin and cwd behaviour is the correct governance contract; keep it, add tests, and do not open it up globally for `apexe sh`.

---

## 1. Problem statement

### 1.1 Where the experience stands

- **Integration friction**: every host needs the Skill installed, `CLAUDE.md`/`AGENTS.md` parsed, and one preflight per step in the order the Skill prescribes; behaviour drifts when the host or the model is upgraded.
- **Coverage friction**: `native_ready` currently covers only cat/head/ls; nearly every other command falls back to module JSON, so the agent must know apexe's module names. That breaks the "use your tools as they are" selling point.

### 1.2 Positioning

The claim "agents already carry their own terminal safety, so apexe is redundant" conflates two layers:

| Layer | Form | Can distinguish "an authorized rm" from "a catastrophic rm" | Evidence at execution time | Across CLI/HTTP |
|---|---|---|---|---|
| Agent system prompt / allowlist | String matching | No (the effect set is open-ended) | No | No |
| apexe execution boundary | Declarative contract + ACL + audit | Yes | Yes | Yes |

The two do not overlap; what overlaps is only the appearance of "both stop commands". apexe's irreplaceable value is **central policy + execution-time audit + one executor for CLI and HTTP**.

One rev1 wording needs correcting: **the Skill itself does not contradict the threat model.** What contradicts it is treating the Skill's routing convention as an enforcement guarantee. Skill routing is a prompt-layer constraint and should be described only as "reducing ordinary mistakes", never as a security boundary.

---

## 2. Integration surfaces and capability boundaries

| Surface | Agent cooperation | Coverage | Enforcement | Status |
|---|---|---|---|---|
| Agent Skill routing | Required | Theoretically everything | Weak (prompt layer) | Implemented (fallback) |
| PATH shim (`apexe env`) | None | Only `eligible()` commands | Medium | Implemented, 3 commands |
| **Governed shell (`apexe sh`)** | None | Only commands with a matching shim | **Medium (reduces the dependence on the agent cooperating with ordinary calls)** | **This proposal** |
| Host PreToolUse hook | None | The host's shell tool calls | Stronger (only on hosts that support it and are configured fail-closed) | Implemented (Claude Code `apexe hook claude`) |
| MCP/A2A tool surface (no shell) | None | Only declared operations | Strong | Implemented |

Stated explicitly:

- `apexe sh` **is not an enforcement boundary**. It does not change the number of `execve` entry points; it only changes which same-named command on PATH is resolved.
- Commands that no shim covers run ungoverned as before; builtins, functions, absolute paths, `sudo` and interpreter-internal execution are not governed.
- Covering descendant processes requires that "children still resolve commands from the governed PATH". That is a separate design problem (see §4.6) and cannot be obtained automatically by a launcher.

---

## 3. Proposal: `apexe sh`

### 3.1 Three hard constraints

1. **Zero stdout pollution.** Banners, warnings and logs all go to stderr. Claude Code builds a snapshot from the shell, so one extra byte on stdout corrupts the output the agent captures. Explicit diagnostics such as `--check` are exempt.
2. **`execve`, not a proxy.** After preparing the environment, `exec` the real shell directly and do not keep apexe in the process chain. TTY, signals, job control, exit codes and PID behaviour stay native.
3. **Do not parse the `-c` string.** Hand `apexe sh -c "a | b && c"` to the real shell unchanged; PATH shims intercept individual external commands. Any home-grown shell-string parser breaks pipes, redirects, subshells and quoting.

### 3.2 Process model

```
host --spawn--> apexe-sh -c "<cmd>"
                   │ execve(real_shell, argv, governed_env)
                   ▼
              /bin/bash -c "<cmd>"
                   ├─ simple external command → PATH shim → apexe exec
                   └─ builtin / pipe / redirect → real shell semantics
```

### 3.3 CLI interface (proposed)

```text
apexe sh [--shell PATH] [--check] [--] [shell-args...]

apexe-sh -c "<cmd>"      # stable entry name for hosts that accept only a shell path
```

- `--shell`: the real shell (recommended: provide it explicitly in the host configuration).
- `--check`: print diagnostics (real shell, shim directory, manifest version, whether an ACL is loaded, coverage tier, profile-block location).
- All unrecognized arguments are forwarded verbatim to the real shell.
- **rev1's `--locked` was dropped from this proposal's scope** (see §5.2).

### 3.4 Environment contract

Before `execve`, `apexe sh` sets:

| Variable | Purpose |
|---|---|
| `PATH` | Shim directory first, with other shim directories filtered out (reuses `runtime_path::clean_path`) |
| `APEXE_SHIM_DIR` | The current shim directory, for recursion detection and reuse by children |
| `APEXE_SHELL_DEPTH` | Recursion guard; when > 0, pass through without injecting again |
| `APEXE_REAL_SHELL` | Explicit source for the real shell, avoiding a `$SHELL` self-reference |
| `APEXE_ACL` / `APEXE_BINDINGS_DIR` | Explicit policy sources (optional) |

Unchanged: cwd, umask, file descriptors, locale and every other environment variable.

### 3.5 Recursion and traps

- **Self-identification**: `is_shim_invocation` (`src/cli/mod.rs`) treats any argv[0] whose basename is not `apexe` as a shim. An `apexe-sh` symlink therefore falls into `run_shim` by mistake, so it must be recognized explicitly before the shim branch.
- **Refuse itself**: if resolving the real shell points at apexe, `apexe-sh` or a shim directory, fail with a clear error.
- **`$SHELL` self-reference (a rev1 documentation defect)**: if a host is configured with `SHELL=/abs/apexe-sh` and the resolution order falls back to `$SHELL`, apexe selects itself and fails. Fix:
  - Resolution order: explicit `--shell` → `APEXE_REAL_SHELL` → `$SHELL` when it is not self → `/bin/sh`.
  - `apexe sh --check` must report a clear diagnosis when `$SHELL` points at itself instead of failing to run.
- **Nested shells**: running `bash` inside `apexe sh` inherits the environment, increments the recursion depth, and stays governed.
- **Login-shell PATH reset**: macOS `/etc/profile` runs `path_helper`, which rebuilds PATH, and `/etc/zprofile` does the same. `apexe env --enable` now writes both `.zshrc` and `.zprofile` for zsh, covering interactive shells and `zsh -lc`; when `ZDOTDIR` is set, both go into that directory. Older installations upgrade with `apexe env --reload --shell zsh`; a plain reload still preserves the original startup files.
  - Non-login `-c`: `apexe sh` sets PATH directly and **does not depend on any rc file** (this is the proposal's main value).
  - Login `-l`: a managed block must be appended to the end of `~/.bash_profile` / `~/.zprofile` so it runs after `path_helper`. Alternatively `--noprofile` buys determinism, but it loses nvm/pyenv/brew initialization and is not recommended as the default.
- **`env -i` / `sudo` / absolute paths**: cannot be governed; these are documented known boundaries.

---

## 4. Execution-semantics review (rev1 corrections)

### 4.1 The environment is cleared (the only confirmed blocker, and its scope is limited)

`src/module/executor.rs` calls `env_clear()` unconditionally and allows only `ENV_ALLOWLIST`:

```text
PATH HOME USER LOGNAME SHELL LANG LC_ALL TERM TZ TMPDIR (+ LC_*)
```

Scope: **only commands that pass through the executor**. Commands that do not hit a shim already inherit the full environment, so `aws s3 ls` cannot be used to observe executor behaviour.

**Implemented (rev4)**: the policy is selected by `ExecutionIo`; there is no new implicit global switch.

- `ExecutionIo::Passthrough` (local shim / native argv): inherits the caller's environment.
- `ExecutionIo::Captured` (MCP/A2A / module JSON): keeps `env_clear` + `ENV_ALLOWLIST`.
- Before local inheritance, `LOCAL_ENV_DENYLIST` entries are removed: `LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH`, `DYLD_FRAMEWORK_PATH`.
- `apexe sh --check` prints `local_env.policy: "inherit"` and the denylist.

**This is a trust grant, not leak mitigation.** Letting a tool see `GH_TOKEN`, `AWS_PROFILE` and the like grants that tool the corresponding authority. The denylist only addresses dynamic-loading injection; it does not solve "secrets handed to a child process".

### 4.2 stdin: keep the existing capability gate

rev1 was wrong. `stdin_mode()` in `src/cli/shim.rs` already selects by module capability:

- `x-apexe-streaming-stdin-safe == true` → `StdinMode::Inherit`;
- otherwise → `StdinMode::Null`.

The flag comes from an explicit overlay assertion (`src/adapter/overlay.rs`) and `src/module/cli_module.rs` refuses inheritance when the assertion is absent. Tests cover `tests/native_shims.rs::test_shim_streams_raw_stdin_without_json_or_buffering` (binary stdin passthrough) and `test_shim_inherits_stdin_without_explicit_capability`.

Conclusion: **do not change the default behaviour.** `apexe sh` should not switch every local call to inherited stdin; that would break the existing governance contract. A command that needs stdin should declare it in a reviewed contract.

### 4.3 cwd: current behaviour is correct; add a regression test

rev1 was wrong. `PathGuard::from_env()` (`src/governance/path_guard.rs`) establishes its root from the process's current directory; the executor uses the same root on purpose, so path checks agree with what actually runs.

Conclusion: **no evidence that a change is needed**. Add one regression test: `apexe sh` starts in its process cwd and a shimmed command sees the same cwd as the launch directory.

### 4.4 timeout: off by default locally, 30 s by default for captured calls

Local passthrough (shims, `apexe exec` native argv, `apexe sh`) has **no wall-clock bound** by default: the caller owns a terminal, and `node`, `vim`, `ssh`, `npm run dev`, pagers and an agent TUI all legitimately outlive any fixed value. Wrapping the whole command in `default_timeout` used to kill it at 30 seconds and tear the inherited output into broken escape sequences (that is how an agent TUI broke).

- The local bound is configured by `local_timeout` (seconds, `0` = unbounded, default `0`) / `APEXE_LOCAL_TIMEOUT`; set it only when a local call cannot be interrupted by hand.
- Captured calls (`serve`, MCP, A2A, module-mode `exec`) still use `default_timeout` (default 30 seconds) because no human is there to press Ctrl-C.
- The global `--timeout` bounds both.
- Implementation: `src/module/io.rs` reads `0` as "install no timer"; `config_for_timeouts(0)` also disables apcore's `executor.default_timeout` / `global_timeout`, otherwise the module layer would be unbounded while a pipeline still got cut off at 60 seconds.

### 4.5 Child-process routing and recursion are a separate problem

**Implemented (rev4)**:

- **When resolving the real target program**: `search_path()` (`src/runtime_path.rs`) strips shim directories, so there is no recursion.
- **After a local target program starts**: the `Passthrough` path keeps the caller's PATH (including the shim directory), so descendant commands keep hitting shims. Test: `test_descendant_commands_keep_shim_routing`.
- **MCP/A2A**: the `Captured` path still uses `clean_path`, so descendants do not inherit governed routing.

Still acknowledged: interpreter-internal execution, absolute paths, `sudo` and commands no shim covers do not become governed just because descendants inherit the environment.

### 4.6 Local approval channel (implemented)

Local `Passthrough` execution (native argv, shims, `apexe sh`) cannot use MCP elicitation, so it asks on the terminal instead:

- When stdin is a terminal, prompt on `/dev/tty` with the module ID, description and `[y/N]`; `y`/`yes` proceeds and anything else refuses.
- When stdin is not a terminal, refuse immediately with a remedy instead of blocking the agent.
- Waiting uses `libc::poll` with a timeout (default 60 seconds); a timeout returns a refusal and leaves no blocked thread.
- The `ApprovalGate` still writes refusals to the audit trail.
- `Captured` (MCP/A2A, module JSON) keeps the existing client elicitation behaviour.
- An `ApprovalStore` still takes precedence over local terminal approval.

### 4.7 Claude Code PreToolUse hook (implemented)

`apexe hook claude` reads Claude Code's PreToolUse JSON and rewrites simple external commands through `updatedInput`:

- Rewrites to `<abs apexe> sh -c '<original>'` with `permissionDecision: "allow"`; Claude Code still evaluates its own deny/ask rules.
- Commands already routed through `apexe` / `apexe-sh` pass through unchanged, preventing recursion.
- The following are not rewritten and continue to rely on PATH shims: state-changing builtins (`cd`, `export`, `set`, `alias`, `echo`, …), pipes, redirects, command substitution, compound commands and `sudo`.
- When the manifest is missing it returns `deny` and asks the operator to run `apexe env --enable`.
- Limitation: a one-shot `apexe sh -c` does not preserve a persistent shell's cwd, aliases or functions, so only simple external commands are rewritten.
- Unit tests cover rewriting, pass-through, refusal and recursion protection; verified on a real Claude Code 2.1.318 on 2026-11-05: debug logs showed the host parsing `updatedInput.command` and executing the rewritten command (the probe printed `HOOK_ROUTED:hello`); `permissionDecision: "allow"` was accepted under non-interactive `-p`; when the manifest was missing, the returned `deny` sent the call into `permission_denials` and the command did not run. Re-test after upgrading the host.

### 4.8 MCP-only deployment (a configuration mode, not a new code path)

Goal: the agent has no shell and can only call the MCP tools apexe exposes. The tool surface is the boundary; no hook or PATH injection is required.

- OpenCode: configure `mcp.apexe` in `opencode.json` (`type: "local"`, `command: ["apexe","serve","--acl",...]`) and set `"tools": {"bash": false}`.
- Claude Code: register `apexe serve` with `claude mcp add` and deny `Bash` in permissions.
- Codex: register `[mcp_servers.apexe]` in `config.toml`; Codex keeps its shell, so this is not strictly MCP-only and needs a sandbox alongside it.
- Advantage: no dependence on agent cooperation or command parsing; suitable for operations and read-only agents.
- Cost: the agent cannot write code freely and can call only registered modules; `--acl` is required for an explicit policy.

There is no configuration-generating command yet; an operator must merge the snippets above by hand. `apexe sh --check` prints shell-integration snippets, not MCP-only configuration.

### 4.9 Host usage guide and `apexe agent` (implemented)

The full user-facing host guide is [`agent-shell.md`](agent-shell.md):

- OpenCode: `opencode.json`'s `shell` points at `apexe-sh`.
- pi: `~/.pi/agent/settings.json`'s `shellPath` points at `apexe-sh`.
- Codex: the real login shell plus the `.zprofile` managed block written by `apexe env --reload --shell zsh`.
- Claude Code: a `PreToolUse` hook (`apexe hook claude`) or the launch environment's PATH.

`apexe agent <host>` prints the matching configuration; `--check` inspects the launcher and host configuration; `--write` merges into the host JSON atomically; `--mcp-only` generates an MCP-only deployment (writable for OpenCode/Claude, a printed TOML block for Codex). `apexe sh --check` continues to provide launcher-level diagnostics.

**Experimental persistent shell (rev11)**: `apexe hook claude --persistent` keeps one governed shell alive over a Unix socket, preserving cwd, variables, functions and aliases across calls and accepting compound commands. It lives in `src/cli/sh/session.rs`; the socket is `0600`, scoped by host session id + cwd, exits after 30 minutes idle by default, and stops with `APEXE_SESSION_STOP=1`. Limitations: commands that read stdin are unsupported, output is text, and one request is served at a time.

---

## 5. Coverage

A shim is generated only when `eligible()` (`src/cli/shim.rs`) passes, which
requires `metadata.grammar: reviewed` (per route, see `route_is_reviewed` in
`src/models/mod.rs`) plus a matching `shim_contract`; the check lives in
`check_route`, so it applies only to the route the call actually selected.
Bindings written before the `grammar` field existed fall back to
`shim_contract.authoritative`. That is the root cause of "only 3 commands", not
a configuration problem.

### 5.1 `scanned` routes (deferred item, then implemented)

rev1's `scanned + approval` was not usable at the time:

- **Missing approval channel**: `src/cli/local.rs` had `approval_store: None`. Even with `enable_approval: true`, the local executor only installed `ApprovalGate::with_audit`; with no interactive client, calls were refused and could not naturally reach "continue after the user confirms".
- **Approval cannot fix argv translation**: if the schema cannot express ordering, repeated parameters, positional arguments and unknown options accurately, adding approval does not make replay correct. A human approves the *intent*, not the *argument mapping*.

The two concerns are therefore separated:

| Dimension | Meaning | Current conclusion |
|---|---|---|
| Contract trust | Whether the schema/argv mapping can be replayed exactly | Only an authoritative overlay counts as verified |
| Execution permission | Whether execution is allowed and whether confirmation is required | Decided by the ACL; local `Passthrough` now has terminal approval, `Captured` keeps MCP elicitation |

**Implemented (rev7)**: `apexe env --enable --scanned <off|readonly|all>`.

- The scanned schema is used only for validation and the ACL preflight; **the user's original argv is executed**, without rebuilding or reordering.
- A parse failure → refuse; a success → run the original argv. There is no path that "rewrites parameters from a schema inference".
- `readonly` admits only readonly, non-destructive, closed-world modules; `all` admits any current binding.
- Audit records carry `tier: scanned`; `apexe sh --check` lists scanned routes separately.
- Known boundaries: parameters the scanner did not type are not path-checked; a schema inference is not a reviewed contract, so `all` needs manual review.
- Approval may still be layered on top, but its job is "may this run", not "fix the mapping".

### 5.2 `--locked`: restricted command search path (implemented, no full-governance claim)

rev1's claim that it guarantees "every external command in this shell stays inside the boundary" **does not hold**:

- A PATH entry is a directory, not an executable. Adding `/usr/bin` admits the whole directory; expressing a per-binary allowlist requires pre-building a directory containing only permitted commands (one shim/link per command) and keeping the language-runtime directories, which still expose every executable in them.
- `command_not_found_handle` is not portable across shells (bash has it, zsh uses a different name, dash lacks it), and it only handles lookup failures; it cannot stop a command that was already found from taking a detour.
- Even combined with a sandbox, ordinary filesystem/network isolation does not guarantee that "every command went through apexe".

**Implemented (rev4)**: `apexe sh --locked` restricts PATH to the shim directory plus `--allow-path` entries; `apexe-sh` uses the same setting through `APEXE_SHELL_LOCKED` / `APEXE_SHELL_ALLOW_PATH`. The name and documentation still say **restricted command search path**, claiming only fewer accidental invocations, never full governance.

---

## 6. Host integration: versioned experiment results

| Host | Candidate mechanism | Evidence strength | What must be re-verified |
|---|---|---|---|
| OpenCode | `"shell"` in `opencode.json` | **Verified on a real installation** (1.19.5, 2026-11-04): project-level configuration accepts a custom absolute path, the bash tool runs through `apexe-sh -c ...`, shimmed commands hit and audit records are written | The conclusion applies only to the tested version; re-test after upgrades |
| pi | settings `shellPath` → absolute path | **Verified on a real installation** (pi-coding-agent 1.1.0, macOS arm64, 2026-11-04): with `"shellPath": "<shim dir>/apexe-sh"` in `~/.pi/agent/settings.json`, the bash tool runs `apexe-sh -c <cmd>`; `command -v ls/cat/head` hits shims and `APEXE_SHELL_DEPTH=1`; shimmed commands write `audit.jsonl` (allow + execution), a syntax refusal writes `refusal` and fails closed (`head -3` produces no stdout, exits 1 and does not fall back to the real binary); `exit 7` passes through unchanged; nested shells stay governed | The real shell comes from `$SHELL` (locally `/bin/zsh`); to match pi's default bash, also set `APEXE_REAL_SHELL=/bin/bash` (verified); re-test after upgrades |
| Codex | Real login shell + managed startup blocks | **Automatic PATH-shim integration passes** (CLI 0.145.0, macOS arm64, 2026-11-04, deterministic host experiment): after the `.zprofile` fix, ordinary `/bin/zsh -lc` calls hit shims with the snapshot both on and off, with correct cwd, ACL and audit; `SHELL=apexe-sh` still cannot replace its shell | Run `apexe env --reload --shell zsh` and restart Codex; online-model verification, other versions and other platforms still need experiments; see §7 |
| Claude Code | Launch-environment PATH + `PreToolUse` hook | **PATH verified** (2.1.318, 2026-11-04, isolated HOME/config experiment): `$SHELL` and `CLAUDE_CODE_SHELL` pointing at `apexe-sh` are both rejected and fall back to `/bin/zsh`; the rc managed block does not enter the snapshot's PATH; when the launching process PATH already contains the shim directory, the Bash tool runs through shims and writes audit records. **Hook implemented and verified on a real installation** (`apexe hook claude`; 2.1.318, 2026-11-05): the rewritten `updatedInput` command was executed, and JSON `deny` blocked the call | The conclusion applies only to the tested version; re-test the hook output protocol after upgrades; a hook that fails to start is still fail-open |

Additional notes:

- **Two Claude Code hook modes**: both `exit 2` and JSON `permissionDecision: "deny"` block a call (both verified on 2.1.318); JSON `permissionDecision: "allow"` and `updatedInput` are adopted by the host, and the rewritten command was observed to execute. A single Bash call can still run several commands internally, so the hook rewrites only simple external commands and compound commands continue to rely on PATH shims. The hook command failing to start remains fail-open, so anyone depending on the hook must guarantee it is executable.
- Every host experiment must record the version and the configuration snippet; conclusions must not be extrapolated across versions.

---

## 7. Implementation order (revised)

### Stage 0: host-integration experiments (first, a gate)

Complete an end-to-end experiment on at least one target host:

1. is a custom shell accepted;
2. are arguments (`-c`, `-l`, script files) forwarded correctly;
3. does the environment/snapshot preserve PATH;
4. are refusals/failures observable.

Failing this, stage 1 must not become the default recommendation.

**Completed (OpenCode 1.19.5, 2026-11-04)**:

- Project-level `opencode.json` set `"shell": "<config_dir>/bin/apexe-sh"`; the custom absolute path was accepted.
- The agent's bash tool actually launched `apexe-sh -c "<command>"`.
- `apxepwd`, provided only by the shim, ran successfully and printed the project cwd.
- `audit.jsonl` recorded `module_id: cli.apxepwd`, `caller_id: local`, `status: success`.
- Conclusion: OpenCode can be the first validated shell host; other versions and hosts still need the same checklist.

**Codex CLI 0.145.0: deterministic host experiment (macOS arm64, 2026-11-04)**:

- A temporary HOME, CODEX_HOME, bindings, shim and ACL were used; no user shell configuration was modified. The test module `cli.apxepwd` targeted the installed `/bin/pwd`, its input schema had no parameters, and the command name was provided only by the shim. The ACL allowed only `local` to call it and denied everything else by default.
- An online `codex exec` first timed out on the connection and then reported that the current ChatGPT account does not support the configured model `gpt-6.1-terra`. So this run **did not complete an online-model end-to-end acceptance**.
- A local deterministic Responses test service was used instead, returning a preset tool call that the unmodified Codex CLI executed as `tools.exec_command`. This validates host execution behaviour, not whether a model would choose the route on its own.
- The environment was `SHELL=<temporary-home>/.apexe/bin/apexe-sh`, `APEXE_REAL_SHELL=/bin/zsh`, `shell_environment_policy.inherit="all"`; ordinary terminal calls did not specify a shell or modify PATH.

| Scenario | Observed behaviour | Conclusion |
|---|---|---|
| Keep the temporary `.zshrc` written by `apexe env --enable` | Codex records `/bin/zsh -lc`; the shim directory precedes system directories; `apxepwd` returns the correct cwd and exits 0; audit contains the ACL allow and execution success; `APEXE_SHELL_DEPTH` is empty | PATH-shim integration works; this does not prove the launcher was adopted |
| Move the temporary `.zshrc` away, keep only the environment above | Still `/bin/zsh -lc`; PATH has no shim directory; `apxepwd` exits 127 and no execution audit is added | Setting only `SHELL` cannot complete the integration |
| No temporary `.zshrc`; pass `shell=<entry-point>` explicitly to the terminal tool | Codex records `/bin/sh -lc`; `apxepwd` exits 127 | This version's tool `shell` parameter does not accept the custom entry point |
| No temporary `.zshrc`; the tool command explicitly calls `apexe sh --shell /bin/zsh -c '<probe>'` | The outer shell is still `/bin/zsh -lc`; the inner shell has the shim directory first, `APEXE_SHELL_DEPTH=1`, correct cwd, exit 0; audit contains the ACL allow and execution success | The launcher can be used explicitly inside Codex, but the agent must still wrap commands |

The above is the pre-fix comparison; **Codex is not marked as passing custom-shell replacement**. Full acceptance items such as refusal/cancel/TTY remain incomplete, and conclusions must not be extrapolated across versions.

**Automatic-integration fix and retest**: the root cause was that zsh startup configuration wrote only `.zshrc`, while Codex's default non-interactive login shell skips that file. The fix manages `.zshrc` and `.zprofile` together under the same `HOME`/`ZDOTDIR`, reusing the existing ownership checks and uninstall mechanism. `apexe sh --check` now also gives Codex-specific integration guidance.

- An older installation runs `apexe env --reload --shell zsh` (custom shim directories must pass `--shim-dir`), preserving the installed binding directory, ACL and filters; then restart Codex to refresh the shell snapshot.
- The real Codex CLI 0.145.0 used `SHELL=/bin/zsh`; test tools ran only ordinary probe commands, without specifying a shell, modifying PATH, or wrapping `apexe sh` / `apexe exec`. The local deterministic Responses service only supplied preset calls; execution and shell selection still belonged to Codex.
- The temporary `.zprofile` originally contained `export PATH=/usr/bin:/bin`, with the managed block appended after it; with `features.shell_snapshot` on and off, two calls both resolved to the temporary shim `apxepwd`, returned the correct cwd, exited 0, and audited both the `local` ACL allow and the `cli.apxepwd` execution success.
- Under the same automatic-integration setup, a real Codex tool running `exit 7` returned status 7 with no output; running `printf ok` returned 0 with exactly `ok` and no activation noise.
- New regression coverage: non-interactive login shells hitting real shims after a PATH reset, `ZDOTDIR`, repeated install and text restoration, preserving the ACL when upgrading an older installation, and refusing uninstall while a `.zprofile` managed block remains and keeping the shims.
- Conclusion: **automatic hits on the existing eligible shims now pass**, with no per-command wrapping by the agent; Codex still uses the real shell it accepts. This does not change the boundaries for builtins, absolute paths or uncovered commands.

Experiment material lives under `/private/tmp/apexe-codex-shell-probe/`: `host_probe.py`, `host-events.jsonl` (first round keeping startup files), `host-events-default.jsonl`, `host-events-explicit.jsonl`, `host-events-launcher.jsonl`; after the fix, `host_probe_auto.py` with `host-events-auto-snapshot.jsonl`, `host-events-auto-no-snapshot.jsonl`, `host-events-auto-exit.jsonl`, `host-events-auto-stdout.jsonl`, and the audit under `home/.apexe/audit.jsonl`. This is a temporary evidence directory, not a repository deliverable.

Pre-fix full regression `cargo test --all-features --locked`: **1496 passed, 0 failed, 3 ignored**. The first restricted-environment round failed the local HTTP Explorer case; after allowing a local service to listen, the full run passed.

Post-fix `make check` passed every gate: formatting, Clippy, character validation, build and all test gates succeeded; Rust tests (including example tests) totalled **1503 passed, 0 failed, 3 ignored**, Python regressions **77 passed**, the CLI runtime matrix **174 passed, 0 failed**, and the example runtime matrix **15 passed, 0 failed**.

**Claude Code 2.1.318: isolated host experiment (macOS arm64, 2026-11-04)**:

- A temporary `HOME`, `CLAUDE_CONFIG_DIR`, bindings, shim and ACL were used; the test module `cli.apxepwd` targeted `/bin/pwd` and its command name was provided only by the shim; the ACL allowed only `local`. The experiment covered headless (`claude -p`) and pty interactive sessions, without modifying the user's shell configuration.
- The conclusion applies only to CLI 2.1.318; re-test after upgrades.

| Scenario | Observed behaviour | Conclusion |
|---|---|---|
| `SHELL=<shim>/apexe-sh` + `APEXE_REAL_SHELL=/bin/zsh` | Inside the Bash tool `SHELL=/bin/zsh` and `APEXE_SHELL_DEPTH` is absent; `apxepwd` exits 127 with no audit | Custom-shell replacement is unsupported; `APEXE_REAL_SHELL` is not read |
| `CLAUDE_CODE_SHELL=<shim>/apexe-sh` | Debug log: `is not a valid bash/zsh path, falling back to detection`; `/bin/zsh` is used | The explicit override is rejected too |
| Install only the `.zshrc` / `.zprofile` managed blocks from `env --enable` | The snapshot's `export PATH` comes from the launch environment and has no shim directory; `apxepwd` exits 127 with no audit | The rc managed block is not an integration path for Claude Code |
| Prepend the shim directory to the launching process PATH (simulating an activated terminal) | Both headless and interactive: `apxepwd` prints the project cwd and exits 0; the snapshot PATH keeps the shim directory; audit contains the ACL allow and execution success | **Recommended integration: start Claude Code from a terminal with shims activated**, or inject PATH explicitly |
| PreToolUse hook `exit 2` | The Bash tool call is blocked and `permission_denials` contains Bash | Tool calls can be blocked |
| PreToolUse hook `exit 1` / hook command missing | Only a warning or silence; the command runs as usual | A non-2 exit code and a hook that fails to start are both fail-open; fail-closed requires the hook itself to return 2 reliably |
| PreToolUse hook JSON `permissionDecision: "deny"` | The call enters `permission_denials`, the command does not run, and the model receives the refusal reason | JSON `deny` is equivalent to `exit 2` |
| PreToolUse hook JSON `allow` + `updatedInput` | Debug logs parse the rewritten command and the probe prints `HOOK_ROUTED`; it runs under non-interactive `-p` without an extra allow rule | `updatedInput` works and can be used for routing |

- The snapshot mechanism was confirmed through debug logs: `Creating shell snapshot for zsh (/bin/zsh)`, `Looking for shell config file: <home>/.zshrc`, `Spawn-env probe captured 65 keys`; the final snapshot's `export PATH` is the launch environment's value, not the rc-modified one.
- Evidence material lives under `/private/tmp/apexe-claude-shell-probe/` (a temporary directory, not a repository deliverable): `env1.json`, `int4-tty.log`, `keep/snapshot.sh`, `dbg2.log`, `hook-{deny,warn,missing}.json`, with the audit in `home/.apexe/audit.jsonl`.
- The same conclusion as Codex: the host must inherit the governed PATH at launch; do not rely on the host rebuilding PATH before every command.

### Stage 1: minimal MVP

1. `src/cli/sh.rs`: `execve` the real shell, forward arguments, detect self, protect against recursion.
2. `src/cli/mod.rs` / `src/main.rs`: recognize `apexe-sh` in argv[0] before the `is_shim_invocation` branch.
3. Integrate only existing `verified` shims; add no new route tier.
4. `apexe sh --check`: effective-policy diagnostics (real shell, shim directory, manifest, ACL, coverage tier).
5. Keep the existing stdin and path-check contracts; add the §4.3 cwd regression test.
6. The first version only prints host configuration snippets; it does not modify user configuration.

### Stage 1 implementation status (rev3)

Implemented:

- `apexe sh`: `execve` the real shell, forward arguments, detect self, recursion environment variable.
- `apexe-sh` argv[0] dispatch, placed before `is_shim_invocation`.
- `apexe env --enable` installs the `apexe-sh` symlink; `env --disable` removes it; `env --check` validates it.
- `apexe sh --check --format text|json`: real-shell source, entry point, manifest, ACL, shim commands, host configuration snippets (OpenCode/pi/Codex/Claude Code) and a problem list.
- `--check` fails when the entry point or manifest is missing; a missing ACL is a warning.
- Keep the existing stdin and path-check contracts; add the cwd regression test.
- Runtime argument coverage: `--check`, `--format text/json`, `--shell`, forwarded argv.

Stage 2 implemented:

- Local environment inheritance: `Passthrough` inheritance + `LOCAL_ENV_DENYLIST`; `Captured` keeps the scrub.
- Descendant routing: local PATH keeps the shim directory; `Captured` still cleans it.
- Restricted command search path: `--locked` / `--allow-path`; `apexe-sh` supports `APEXE_SHELL_LOCKED` / `APEXE_SHELL_ALLOW_PATH`.
- Local approval: `/dev/tty` prompt, non-interactive refusal, poll timeout; MCP/A2A keep client elicitation.
- `scanned` original-argv execution: `--scanned readonly|all`, `tier: scanned` audit.
- Claude Code PreToolUse hook: `apexe hook claude`, rewriting simple external commands through `updatedInput`.
- Host guide `docs/agent-shell.md` and `apexe agent <host>` / `--check`.
- `apexe agent --write` merges host configuration automatically; `--mcp-only` generates an MCP-only deployment.

Still not implemented:

- Generating MCP-only configuration (only deployment instructions exist today).
- Cross-host support for the persistent shell and more complete I/O (stdin, binary output).

The stage-0 host experiments remain the gate for "promote to the default recommendation"; they do not affect this MVP's usability.

### Stage 2: only after separate review

1. ~~Local credential environment policy~~ **Implemented**: `Passthrough` inheritance + dynamic-loading denylist; `Captured` keeps the scrub.
2. ~~Approval channel (how a local call is confirmed)~~ **Implemented**: `/dev/tty` prompt + non-interactive refusal + poll timeout; `Captured` keeps MCP elicitation.
3. ~~Widen native coverage~~ **Implemented**: `--scanned readonly/all`, original-argv execution + `tier: scanned` audit; `--check` reports coverage.
4. ~~Child-process routing~~ **Implemented**: local PATH keeps the shim directory; `Captured` still cleans it.
5. ~~Restricted command search path (the original `--locked`)~~ **Implemented**: `--locked` + `--allow-path`, with no full-governance claim.
6. Host enforcement — **partially implemented**: the Claude Code hook is implemented; MCP-only is recorded as a deployment mode in §4.8, but configuration generation is not built.

---

## 8. Acceptance criteria (revised)

### 8.1 Command level

Use test commands that are **known to pass through a shim**, and assert output, exit status, refusal behaviour and audit records together:

```text
command -v ls                     # proves only the resolution location, not that governance took effect
ls -la                            # hits the shim; assert output and exit status; audit contains an execution record
apexe sh -c 'exit 7'              # the outer exit status must be 7
cat                               # assert stdin passthrough only for a reviewed module with streaming_stdin_safe
bash -c 'ls'                      # a nested shell still hits the shim and produces a second audit record
apexe sh -c 'echo ok' >out        # `out` contains only "ok", with no apexe bytes
apexe sh --check --format json    # diagnostics work; a $SHELL self-reference produces a clear error
```

Additional cases missing from rev1:

- **cwd**: print cwd with a shimmed command from the test fixture, not the `pwd` builtin.
- **Environment policy**: print the target environment variable from a shimmed fixture, not from an uncovered `aws`.
- **Exit status**: `apexe sh -c 'exit 7'`; do not use `false; echo $?` (which usually exits 0).
- **ACL**: assert a missing ACL, an ACL denial and an audit record separately.
- **Corrupt input**: a corrupt manifest, missing bindings and a removed shim directory must each give an actionable error.
- **Login shell**: under `-lc`, PATH has not been reset by `path_helper`.
- **Recursion**: the shim directory is not resolved through itself again, and nested calls do not recurse forever.
- **Cancel and TTY**: Ctrl-C in a TTY, interactive stdin, and the `git commit` editor.

### 8.2 Host regression (versions must be recorded)

- Stage-0 experiment records, covering the target host + version + configuration snippet.
- Only after verification may a host be written into the "recommended integration" documentation.

---

## 9. Risks and non-goals

### Risks

| Risk | Mitigation |
|---|---|
| Readers assume `apexe sh` means full governance | The documentation and `--check` state the coverage tier explicitly; claim only fewer ordinary mistakes |
| Environment inheritance grants credentials to child processes | **Implemented and accepted**: local `Passthrough` inheritance is a trust grant; the denylist handles only dynamic-loading injection and is not packaged as "leak mitigation" |
| Child processes bypass shims | **Local descendant inheritance is implemented**; interpreter-internal execution, absolute paths and uncovered commands remain known boundaries |
| A login-shell PATH reset silently disables governance | `--check` detects it; the managed block goes at the end of the profile |
| A host upgrade breaks the integration point | Versioned regression tests; fall back to PATH shims + profile injection on failure |
| No local approval in non-interactive environments | **Implemented**: refuse with a remedy instead of blocking the agent; interactive terminals prompt on `/dev/tty` |
| Windows semantics differ | v1 supports Unix only; Windows/PowerShell needs a separate proposal |

### Non-goals

- It is not a sandbox; it provides no filesystem/network isolation and does not treat a sandbox as a guarantee that "every command went through apexe".
- It does not replace the MCP/A2A tool surface or a host hook.
- It does not govern builtins, functions, absolute paths, `sudo`, or direct `execve` inside a child process.
- It does not promise transparent execution through `scanned + approval`.

---

## 10. Decisions for the team (revised)

1. Should `apexe sh` become a first-class command, with a Unix-first release? **Recommendation: yes.**
2. Default integration strategy: recommend the shell first only on verified hosts, with the Skill retained for discovery/explanation/guidance? **Recommendation: yes.**
3. ~~Local environment inheritance policy~~ **Decided and implemented**: `Passthrough` inheritance + dynamic-loading denylist; "credentials available means authority granted" is accepted.
4. `scanned` routes: show contract trust only at first, without execution? (**Recommendation: yes, defer execution**) — **superseded in rev7 by `--scanned readonly|all`.**
5. ~~Restricted command search path (the original `--locked`)~~ **Implemented**: `--locked` / `--allow-path`, with no full-governance claim.
6. Host configuration writes: print snippets in the first version and make automatic modification a separate installation feature? (**Recommendation: yes**) — **superseded in rev10 by `--write`.**
7. ~~Local approval channel~~ **Implemented**: `/dev/tty` prompt + non-interactive refusal; an `ApprovalStore` remains a library-level feature.

---

## Appendix A: key code locations

Line numbers drift; use the symbol names.

| Location | Purpose |
|---|---|
| `src/cli/mod.rs` → `is_shim_invocation` | argv[0] dispatch |
| `src/cli/mod.rs` → `run_shim` | Shim entry |
| `src/main.rs` | argv[0] dispatch: recognize `apexe-sh` before the `Cli` parser |
| `src/cli/shim.rs` → `try_execute` | Execute a shim invocation |
| `src/cli/shim.rs` → `resolve_route` | Route resolution |
| `src/cli/shim.rs` → `passthrough_stdin_mode` | stdin selected from `streaming_stdin_safe` |
| `src/cli/shim.rs` → `eligible` | Authoritative-overlay gate |
| `src/cli/shim.rs` → `check_route` | Validates the selected route; enforces `eligible` for a verified route |
| `src/cli/exec/native.rs` | Native argv resolution entry; reusable parsing and recursion protection |
| `src/module/executor.rs` → `ENV_ALLOWLIST` | Captured-execution environment allowlist |
| `src/module/executor.rs` → `current_dir` | Shared path-guard root |
| `src/module/executor.rs` → `env_clear` | Captured-execution environment scrub |
| `src/module/executor.rs` → `Stdio::null()` | Default stdin for captured execution |
| `src/module/executor.rs` | PATH passed to children with shim directories removed |
| `src/governance/path_guard.rs` → `PathGuard::from_env` | Root comes from the process cwd |
| `src/module/cli_module.rs` | Inherited stdin requires a `streaming_stdin_safe` assertion |
| `src/cli/local.rs` → `approval_store` | Local executor `approval_store: None` |
| `src/runtime_path.rs` → `is_shim_directory` | Recognize a shim directory |
| `src/runtime_path.rs` → `search_path` | Resolve the real program with shim directories removed |
| `tests/native_shims.rs` → `test_shim_streams_raw_stdin_without_json_or_buffering` | Binary stdin passthrough test |

## Appendix B: experiments still to verify

1. ~~OpenCode: whether an agent tool call works with `"shell"` pointing at `apexe-sh`, and whether a custom name is accepted.~~ **Passed (1.19.5)**: a custom absolute path is accepted, the bash tool runs through `apexe-sh -c`, and shimmed commands hit and write audit records.
2. ~~pi: whether `shellPath` pointing at `apexe-sh` works, and whether pi's own bin directory prepended to PATH shadows the shims.~~ **Passed (pi-coding-agent 1.1.0)**: pi prepends `~/.pi/agent/bin` through `getShellEnv()`, and `apexe-sh` then puts the shim directory first in PATH before `execve`, so shims still win; `apexe-sh -c` invocation, audit records, fail-closed refusal, exit-status passthrough and nested shells were all verified end to end with an online model. The real shell defaults to `$SHELL`; `APEXE_REAL_SHELL` was not set.
3. Codex: **automatic PATH-shim integration for CLI 0.145.0 is fixed and passed deterministic host experiments with snapshots on and off**, see §7. `SHELL=apexe-sh` and a custom tool shell path still do not replace the host shell; online-model end-to-end verification and other platforms/versions remain.
4. ~~Claude Code: whether PATH survives the `SHELL` + snapshot path; whether `APEXE_REAL_SHELL` correctly avoids a self-reference; the PreToolUse fail-closed configuration.~~ **Verified (2.1.318)**: both `SHELL` and `CLAUDE_CODE_SHELL` pointing at `apexe-sh` are rejected and fall back to `/bin/zsh`; the snapshot PATH comes from the launch environment and the rc managed block has no effect; integration passes when the launching process PATH contains the shim directory; PreToolUse `exit 2` and JSON `deny` both block, and the rewritten `updatedInput` command executes (2026-11-05); a hook that fails to start is fail-open.
5. Any of the above hosts: `apexe sh -c 'exit 7'`, shim audit records, ACL denial, corrupt-manifest diagnostics.
6. TTY behaviour: Ctrl-C, `cat`, the `git commit` editor.

## Appendix C: eight verification records (2026-11-05)

| Item | Result | Evidence |
|---|---|---|
| 1. Interactive `apexe sh` / `-- script.sh` | Pass | `tests/agent_shell_pty.rs`: PTY interaction, script arguments |
| 2. Login-shell `-l` profile PATH reset | Pass | The test resets `PATH` first, then the managed block restores the shim directory |
| 3. Ctrl-C / SIGINT / SIGTERM / SIGPIPE / job control | Pass | Rust signal and pipeline tests; Python PTY Ctrl-C, background job + `wait` |
| 4. Local approval yes/no/timeout | Pass | Rust PTY yes/no; Python PTY 1-second timeout (`APEXE_APPROVAL_TIMEOUT_SECONDS`) |
| 5. Denylist actually removed | Pass | End-to-end `LD_PRELOAD` / `DYLD_INSERT_LIBRARIES` probe prints `unset` |
| 6. `apexe sh` + streaming stdin | Pass | A `streaming_stdin_safe` shim reads piped input through `apexe sh -c` |
| 7. Claude Code `updatedInput` on a real installation | Pass | 2026-11-05, Claude Code 2.1.318 + `target/debug/apexe`: with the `PreToolUse` hook loaded via `--settings`, debug logs parsed `updatedInput.command` and the probe printed `HOOK_ROUTED:hello`; the same command without the hook exited 127. With the manifest missing, JSON `deny` entered `permission_denials` and the command did not run |
| 8. Real `~/.apexe` installation | Pass | Existing `~/.apexe/bin/apexe-sh`, manifest and ACL; `apexe sh --check` succeeds and `apexe-sh -c 'command -v cat'` hits the shim |

All eight are automated or verified on a real installation. Item 7's conclusion applies only to Claude Code 2.1.318; after a host upgrade, repeat the same scenario (set `APEXE_SHIM_DIR`, inject the hook with `--settings`, and observe whether the rewritten command executes and whether `deny` enters `permission_denials`).

## Appendix D: rev1 → rev2 revision record

| No. | rev1 statement | Review result | rev2 handling |
|---|---|---|---|
| 1 | Prepending PATH "removes the variable of whether the agent cooperates"; enforcement "medium to strong" | Overstated. It only changes command lookup order and is not an enforcement boundary | Reworded to "reduces the dependence on the agent cooperating with ordinary calls"; enforcement marked "medium" |
| 2 | The Skill contradicts the threat model | Inaccurate. The contradiction is in how it was advertised, not in the Skill itself | Reworded: the problem is treating the routing convention as an enforcement guarantee; the Skill keeps its discovery/explanation role |
| 3 | `--locked` can guarantee full governance | Does not hold. PATH is directory-granular; `command_not_found` is not portable; absolute paths/interpreters can bypass it | Renamed to "restricted command search path", deferred, and limited to reducing accidental invocations |
| 4 | Three blockers: environment clearing, stdin, cwd | Only environment clearing was real, and only for executor-path calls; stdin already follows a capability gate; the cwd root is the process cwd | Removed the stdin/cwd blocker claims; kept the capability gate and added a cwd regression test |
| 5 | `Inherit + denylist` mitigates secret leakage | Inaccurate. The denylist only reduces dynamic-loading injection; credentials available means authority granted | Reworded as a trust grant; the default set and the deny set are reviewed separately |
| 6 | `scanned + approval` works as a companion | The approval channel was missing (`approval_store: None`); approval does not fix argv mapping | Deferred; separated "contract trust" from "execution permission" |
| 7 | Host-integration conclusions (OpenCode/pi/Codex/Claude) | None supported a default recommendation; a `$SHELL` self-reference conflict existed | All demoted to pending experiments; added `APEXE_REAL_SHELL` and fixed the resolution order |
| 8 | Acceptance cases (`pwd`, `aws`, `false; echo $?`, `command -v`) | None could verify the behaviour it claimed | Replaced with fixture + shimmed command + exit-status/audit assertions; added ACL, corrupt input, login shell, recursion and TTY |
| 9 | Implementation order: MVP before host verification | Order was inverted | Changed to stage-0 host-experiment gate → stage-1 MVP → stage-2 separate review |
