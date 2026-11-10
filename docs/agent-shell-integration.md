# Agent 无缝对接方案：`apexe sh` 设计评估稿（rev11）

- **状态**：Draft rev8（阶段 2 已实现：本地环境继承、本地审批、`scanned` 原 argv 执行、Claude Code hook；OpenCode 1.19.5、Codex CLI 0.145.0、Claude Code 2.1.318、pi-coding-agent 1.1.0 已验证）
- **日期**：2026-11-05
- **范围**：终端型 AI agent（Claude Code、Codex、pi、OpenCode）与 apexe 的本地执行接入
- **不涉及**：MCP/A2A server 行为、OpenAPI 导入、overlay 编写流程
- **修订说明**：rev1 中的"全量治理"承诺、三处执行阻断项、宿主接入结论与验收用例已被复核修正，详见附录 D；rev3 记录阶段 1；rev4 记录本地环境继承、后代路由与受限搜索路径；rev5 记录本地审批通道；rev6 记录 shim 覆盖度报告；rev7 记录 `scanned` 原 argv 执行；rev8 记录 Claude Code hook；rev9 记录宿主指南与 `apexe agent`；rev10 记录自动写配置与 MCP-only 生成；rev11 记录实验性持久化 shell。

---

## 0. 结论（评审后）

1. **值得做 `apexe sh`，但定位是"自动接入现有治理链路的 shell 启动器"**，不是新的强制边界。
2. 它降低的是"agent 忘记调用 `apexe exec`"的常规失误，**不保证**所有执行行为都经过 apexe。PATH 前置只改变命令查找顺序，agent 仍可修改 PATH、使用绝对路径、经解释器绕过。
3. **Skill 已降级为 fallback。** 宿主 shell/hook 集成优先；Skill 只在无法配置 shell/hook 时承担 exec 路由，并继续提供发现、拒绝原因解释和使用指导。
4. **先做宿主接入实验，再做 MVP**。至少一个目标宿主端到端验证通过后，才决定是否提升为默认接入方式。
5. `scanned + approval` 与 `--locked` **暂缓**，单独评审。当前实现下二者都无法支撑文档原先承诺的能力。
6. 执行器层面只确认一项真实阻断：**经过执行器的命令其环境被清空**。stdin 与 cwd 的现有行为是正确的治理契约，应保留并补测试，不应为 `apexe sh` 全局放开。

---

## 1. 问题陈述

### 1.1 体验现状

- **接入摩擦**：每个宿主都要安装 Skill、解析 `CLAUDE.md`/`AGENTS.md`、按 Skill 的编排要求逐条 preflight；宿主升级或换模型后行为漂移。
- **覆盖摩擦**：`native_ready` 目前只有 cat/head/ls；绝大多数命令退回 module JSON，agent 必须知道 apexe 的模块名，破坏了"工具照旧使用"的卖点。

### 1.2 定位判断

"agent 自带终端安全机制，所以 apexe 多余"这一判断混淆了两层：

| 层 | 形式 | 能否区分"被授权的 rm"与"灾难性的 rm" | 执行时刻证据 | 跨 CLI/HTTP |
|---|---|---|---|---|
| agent 系统提示 / allowlist | 字符串匹配 | 否（效果集合是开放的） | 否 | 否 |
| apexe 执行边界 | 声明式契约 + ACL + 审计 | 是 | 是 | 是 |

两者不重叠；重叠的只是"都会拦命令"的表象。apexe 的不可替代价值是 **集中策略 + 执行时刻审计 + CLI 与 HTTP 同一执行器**。

需要修正 rev1 的措辞：**Skill 本身不与 threat model 矛盾。** 矛盾的是把 Skill 的路由约定当作强制执行保证。Skill 的路由是 prompt 层约束，只应被描述为"降低常规失误"，不能作为安全边界。

---

## 2. 接入位与能力边界

| 接入位 | agent 需配合 | 覆盖 | 强制力 | 现状 |
|---|---|---|---|---|
| Agent Skill 路由 | 需要 | 理论全部 | 弱（prompt 层） | 已实现（fallback） |
| PATH shim（`apexe env`） | 无 | 仅 `eligible()` 命令 | 中 | 已实现，3 个命令 |
| **governed shell（`apexe sh`）** | 无 | 仅命中 shim 的命令 | **中（减少常规调用对 agent 主动配合的依赖）** | **本方案** |
| host PreToolUse hook | 无 | 宿主暴露的 shell 工具调用 | 较强（仅支持且配置为 fail-closed 的宿主） | 已实现（Claude Code `apexe hook claude`） |
| MCP/A2A 工具面（无 shell） | 无 | 仅声明的操作 | 强 | 已实现 |

必须明确：

- `apexe sh` **不是强制边界**。它不改变 `execve` 的入口数量，只改变 PATH 上同名命令的解析结果。
- 未被 shim 覆盖的命令照常裸跑；builtin、函数、绝对路径、`sudo`、解释器内部执行均不受治理。
- 能覆盖 descendant 进程的前提是"子进程仍从受治理的 PATH 解析命令"，这是独立设计问题（见 §4.6），不能由启动器自动获得。

---

## 3. 方案：`apexe sh`

### 3.1 三条硬约束

1. **零 stdout 污染**。banner、警告、日志一律走 stderr。Claude Code 会基于 shell 生成 snapshot，stdout 多一个字节就会污染 agent 捕获的输出。`--check` 等显式诊断命令除外。
2. **`execve` 而非 proxy**。设置完环境后直接 `exec` 真实 shell，不在进程链中保留 apexe。保证 TTY、信号、job control、退出码、PID 原生。
3. **不解析 `-c` 字符串**。`apexe sh -c "a | b && c"` 原样交给真实 shell；单条外部命令由 PATH shim 拦截。任何自研 shell 字符串解析都会破坏 pipe/redirect/subshell/quoting。

### 3.2 进程模型

```
host --spawn--> apexe-sh -c "<cmd>"
                   │ execve(real_shell, argv, governed_env)
                   ▼
              /bin/bash -c "<cmd>"
                   ├─ 简单外部命令 → PATH shim → apexe exec
                   └─ builtin/管道/重定向 → 真实 shell 语义
```

### 3.3 CLI 接口（建议）

```text
apexe sh [--shell PATH] [--check] [--] [shell-args...]

apexe-sh -c "<cmd>"      # 稳定入口名，供只接受 shell 路径的宿主使用
```

- `--shell`：真实 shell（推荐由接入配置显式提供）。
- `--check`：输出诊断（真实 shell、shim 目录、manifest 版本、ACL 是否加载、覆盖档位、profile block 位置）。
- 所有未识别参数原样转发给真实 shell。
- **rev1 的 `--locked` 已移出本方案范围**（见 §5.2）。

### 3.4 环境契约

`apexe sh` 在 `execve` 前设置：

| 变量 | 作用 |
|---|---|
| `PATH` | shim 目录前置，并过滤掉其他 shim 目录（复用 `runtime_path::clean_path`） |
| `APEXE_SHIM_DIR` | 当前 shim 目录，供递归检测与子进程复用 |
| `APEXE_SHELL_DEPTH` | 递归保护；>0 时直接透传，不再注入 |
| `APEXE_REAL_SHELL` | 真实 shell 的显式来源，避免 `$SHELL` 自引用 |
| `APEXE_ACL` / `APEXE_BINDINGS_DIR` | 显式策略来源（可选） |

不修改：cwd、umask、文件描述符、locale、其他环境变量。

### 3.5 递归与陷阱

- **自身识别**：`is_shim_invocation`（`src/cli/mod.rs:27`）把任何 basename 不为 `apexe` 的 argv[0] 当作 shim。因此 `apexe-sh` 符号链接会误入 `run_shim`（`src/main.rs:95`），必须在 shim 分支之前显式识别。
- **拒绝自身**：真实 shell 解析结果如果指向 apexe、`apexe-sh` 或 shim 目录，直接报错。
- **`$SHELL` 自引用（rev1 的文档缺陷）**：若宿主被配置为 `SHELL=/abs/apexe-sh`，而解析顺序又回退到 `$SHELL`，会选中自身并报错。修正：
  - 解析顺序改为：显式 `--shell` → `APEXE_REAL_SHELL` → 若 `$SHELL` 不等于自身则用 `$SHELL` → `/bin/sh`。
  - `apexe sh --check` 必须在 `$SHELL` 指向自身时给出明确诊断，而不是运行失败。
- **嵌套 shell**：`apexe sh` 内再执行 `bash`，环境变量继承，递归深度递增，仍受治理。
- **登录 shell 的 PATH 重置**：macOS `/etc/profile` 会执行 `path_helper` 重建 PATH，`/etc/zprofile` 同理。`apexe env --enable` 现为 zsh 同时配置 `.zshrc` 与 `.zprofile`，支持交互 shell 与 `zsh -lc`；设置 `ZDOTDIR` 时两者均写入该目录。旧安装使用 `apexe env --reload --shell zsh` 升级，普通无参数 reload 仍保留原有启动文件。
  - 非登录 `-c`：由 `apexe sh` 直接设置 PATH，**不依赖任何 rc 文件**（这是本方案最大的价值）。
  - 登录 `-l`：必须在 `~/.bash_profile` / `~/.zprofile` 末尾写入 managed block，确保在 `path_helper` 之后。或使用 `--noprofile` 换取确定性，但会丢失 nvm/pyenv/brew 等初始化，不推荐作为默认。
- **`env -i` / `sudo` / 绝对路径**：无法治理，写入文档的已知边界。

---

## 4. 执行语义核对（rev1 修正）

### 4.1 环境被清空（唯一确认的阻断项，但范围有限）

`src/module/executor.rs:967` 无条件 `env_clear()`，仅放行 `ENV_ALLOWLIST`（`src/module/executor.rs:911`）：

```text
PATH HOME USER LOGNAME SHELL LANG LC_ALL TERM TZ TMPDIR (+ LC_*)
```

影响范围：**只有经过执行器的命令**。未命中 shim 的命令本来就继承完整环境，因此不能用 `aws s3 ls` 来验证执行器行为。

**已实现（rev4）**：策略按 `ExecutionIo` 区分，不新增隐式全局开关。

- `ExecutionIo::Passthrough`（本地 shim / native argv）：继承调用方环境。
- `ExecutionIo::Captured`（MCP/A2A / module JSON）：维持 `env_clear` + `ENV_ALLOWLIST`。
- 本地继承前移除 `LOCAL_ENV_DENYLIST`：`LD_PRELOAD`、`LD_LIBRARY_PATH`、`DYLD_INSERT_LIBRARIES`、`DYLD_LIBRARY_PATH`、`DYLD_FRAMEWORK_PATH`。
- `apexe sh --check` 输出 `local_env.policy: "inherit"` 与 denylist。

**这是信任授权，不是泄露缓解。** 允许工具拿到 `GH_TOKEN`、`AWS_PROFILE` 等，本身就是把对应权限授予该工具。denylist 只处理动态加载注入，不能解决"秘密传给子进程"的问题。

### 4.2 stdin：保留现有能力门槛

rev1 判断错误。`src/cli/shim.rs:308` 的 `stdin_mode()` 已按模块能力选择：

- `x-apexe-streaming-stdin-safe == true` → `StdinMode::Inherit`；
- 否则 → `StdinMode::Null`。

该标志来自显式 overlay 断言（`src/adapter/overlay.rs:334`），且 `src/module/cli_module.rs:98` 会在缺少断言时拒绝继承。已有测试覆盖：`tests/native_shims.rs:534`（二进制 stdin 透传）与 `test_shim_null_stdin_without_explicit_capability`。

结论：**不改默认行为**。`apexe sh` 不应把本地调用全部改成继承 stdin，那会破坏现有治理契约。若某个命令需要 stdin，应通过 reviewed contract 显式声明。

### 4.3 cwd：现有行为正确，补回归测试

rev1 判断错误。`PathGuard::from_env()`（`src/governance/path_guard.rs:439`）以进程当前目录建立 root；执行器使用同一 root（`src/module/executor.rs:966`）是为了让路径检查与实际执行一致。

结论：**没有证据需要修正**。补一条回归测试即可：`apexe sh` 在其进程 cwd 下启动，shim 命令看到的 cwd 与启动目录一致。

### 4.4 timeout：本地默认关闭，captured 默认 30 秒

本地 passthrough（shim、`apexe exec` 原生 argv、`apexe sh`）默认**没有墙钟上限**：调用方自己拥有终端，`node`、`vim`、`ssh`、`npm run dev`、分页器和 agent 的 TUI 都天然长于任何固定值。此前把整条命令包在 `default_timeout` 里，30 秒就杀掉，继承的输出被打断成破碎的转义序列（agent TUI 就是这样坏的）。

- 本地边界由 `local_timeout`（秒，`0` = 不限，默认 `0`）/ `APEXE_LOCAL_TIMEOUT` 配置；需要给无法手动中断的本地调用设上限时才填。
- captured（`serve`、MCP、A2A、module-mode `exec`）仍用 `default_timeout`（默认 30 秒），因为那里没有人类按 Ctrl-C。
- 全局 `--timeout` 同时约束两者。
- 实现：`src/module/io.rs` 把 `0` 读作“不装定时器”；`config_for_timeouts(0)` 同时关闭 apcore 的 `executor.default_timeout` / `global_timeout`，否则模块层不限时、pipeline 仍会在 60 秒处掐断。

### 4.5 子进程路由与递归是独立问题

**已实现（rev4）**：

- **查找真实目标程序时**：`search_path()`（`src/runtime_path.rs:33`）清理 shim 目录，因此不会递归。
- **本地目标程序启动后**：`Passthrough` 路径保留调用方 PATH（包含 shim 目录），后代命令继续命中 shim。测试：`test_descendant_commands_keep_shim_routing`。
- **MCP/A2A**：`Captured` 路径仍使用 `clean_path`，后代不继承治理路由。

仍需承认：解释器内部执行、绝对路径、`sudo` 和未被 shim 覆盖的命令不因后代继承而自动受治理。

### 4.6 本地审批通道（已实现）

本地 `Passthrough` 执行（native argv、shim、`apexe sh`）无法使用 MCP elicitation，改为终端审批：

- stdin 是终端时，在 `/dev/tty` 提示模块 ID、描述和 `[y/N]`；`y`/`yes` 通过，其余拒绝。
- stdin 不是终端时立即拒绝，给出补救建议，不阻塞 agent。
- 等待使用 `libc::poll` 超时（默认 60 秒），超时返回拒绝，不留下阻塞线程。
- 审批拒绝仍由 `ApprovalGate` 写入审计。
- `Captured`（MCP/A2A、module JSON）保持原有客户端 elicitation 行为。
- `ApprovalStore` 仍优先于本地终端审批。

### 4.7 Claude Code PreToolUse hook（已实现）

`apexe hook claude` 读取 Claude Code 的 PreToolUse JSON，并用 `updatedInput` 改写简单外部命令：

- 改写为 `<abs apexe> sh -c '<original>'`，`permissionDecision: "allow"`；Claude Code 仍会评估自身的 deny/ask 规则。
- 已路由命令（`apexe` / `apexe-sh` 开头）原样放行，避免递归。
- 以下命令不改写，继续依赖 PATH shim：会改变会话状态的 builtin（`cd`、`export`、`set`、`alias`、`echo` 等）、管道、重定向、命令替换、复合命令、`sudo`。
- manifest 缺失时返回 `deny`，提示运行 `apexe env --enable`。
- 限制：一次性 `apexe sh -c` 不保留持久 shell 的 cwd、alias、函数；因此只改写简单外部命令。
- 已用单测覆盖改写、放行、拒绝和递归保护；2026-11-05 在 Claude Code 2.1.318 上实机验证：debug 日志显示宿主解析出 `updatedInput.command`，改写后的命令被执行（探针输出 `HOOK_ROUTED:hello`）；`permissionDecision: "allow"` 在非交互 `-p` 下放行；manifest 缺失时返回的 `deny` 使调用进入 `permission_denials` 且命令未执行。升级宿主后需复测。

### 4.8 MCP-only 部署（配置模式，非新代码路径）

目标：agent 没有 shell，只能调用 apexe 暴露的 MCP 工具。工具面即边界，不需要 hook 或 PATH 注入。

- OpenCode：`opencode.json` 配置 `mcp.apexe`（`type: "local"`，`command: ["apexe","serve","--acl",...]`），并设 `"tools": {"bash": false}`。
- Claude Code：用 `claude mcp add` 注册 `apexe serve`，并在 permissions 中 deny `Bash`。
- Codex：在 `config.toml` 的 `[mcp_servers.apexe]` 注册；Codex 仍保留 shell，因此这不是严格 MCP-only，需配合 sandbox。
- 优点：不依赖 agent 配合，也不依赖命令解析；适合运维/只读 agent。
- 代价：agent 不能自由编码，只能调用已注册模块；需要 `--acl` 明确策略。

当前未提供配置生成命令；上述片段需由 operator 手动合并。`apexe sh --check` 输出的是 shell 接入片段，不是 MCP-only 配置。

### 4.9 宿主用法指南与 `apexe agent`（已实现）

面向用户的完整宿主用法在 [`agent-shell.md`](agent-shell.md)：

- OpenCode：`opencode.json` 的 `shell` 指向 `apexe-sh`。
- pi：`~/.pi/agent/settings.json` 的 `shellPath` 指向 `apexe-sh`。
- Codex：真实 login shell + `apexe env --reload --shell zsh` 写入的 `.zprofile` 管理块。
- Claude Code：`PreToolUse` hook（`apexe hook claude`）或启动环境 PATH。

`apexe agent <host>` 输出对应配置，`--check` 检查 launcher 与宿主配置；`--write` 原子合并宿主 JSON，`--mcp-only` 生成 MCP-only 部署（OpenCode/Claude 可写，Codex 打印 TOML）。`apexe sh --check` 继续提供 launcher 级诊断。

**实验性持久化 shell（rev11）**：`apexe hook claude --persistent` 通过 Unix socket 维护一个受治理 shell，跨调用保留 cwd、变量、函数和 alias，可改写复合命令。实现为 `src/cli/sh/session.rs`；socket 0600，按 host session id + cwd 隔离，默认 30 分钟空闲退出，`APEXE_SESSION_STOP=1` 停止。限制：不支持读取 stdin 的命令，文本输出，单请求串行。

---

## 5. 覆盖度

shim 仅在 `eligible()`（`src/cli/shim.rs:104`）通过时生成，要求 `metadata.grammar: reviewed`（每条路由一个值，见 `src/models/mod.rs:57` 的 `route_is_reviewed`）加上匹配的 `shim_contract`；筛选发生在 `check_route`（`src/cli/shim.rs:377`），只对调用实际选中的那条路由生效。旧绑定没有 `grammar` 字段时回退到 `shim_contract.authoritative`。这就是"只有 3 个命令"的根因，不是配置问题。

### 5.1 `scanned` 路由暂缓

rev1 提出的 `scanned + approval` 目前不具备可用性：

- **审批通道缺失**：`src/cli/local.rs:56` 的 `approval_store` 为 `None`。本地执行器即使 `enable_approval: true`，也只会安装 `ApprovalGate::with_audit`；没有可交互的客户端时调用被拒绝，无法自然进入"用户确认后继续"。
- **审批不能修复 argv 转换**：schema 若不能准确表达顺序、重复参数、位置参数和未知选项，增加 approval 不会让重放变得正确。人工批准的是"意图"，不是"参数映射"。

建议把两件事拆开：

| 维度 | 含义 | 当前结论 |
|---|---|---|
| 契约可信度 | schema/argv 映射是否可精确重放 | 只有 authoritative overlay 可视为 verified |
| 执行权限 | 是否允许执行、是否需要确认 | 依赖 ACL；本地 `Passthrough` 已接入终端审批，`Captured` 保持 MCP elicitation |

**已实现（rev7）**：`apexe env --enable --scanned <off|readonly|all>`。

- 扫描 schema 只用于校验和 ACL 预检，**执行的是用户原始 argv**，不重建、不重排。
- 解析失败 → 拒绝；解析成功 → 原 argv 执行；不存在"schema 推断改写参数"的路径。
- `readonly` 只接受 readonly、非 destructive、closed-world 的模块；`all` 接受任意 current binding。
- 审计记录 `tier: scanned`；`apexe sh --check` 单独列出 scanned 路由。
- 已知边界：扫描器未类型化的参数不做路径检查；schema 推断不是 reviewed contract，`all` 需人工审核。
- 审批仍可叠加，但作用是“是否允许执行”，不是“修正映射”。

### 5.2 `--locked`：受限命令搜索路径（已实现，不承诺全量治理）

rev1 声称可保证"该 shell 内所有外部命令都在边界内"，**不成立**：

- PATH 的条目是目录，不是单个可执行文件。加入 `/usr/bin` 等于放行整个目录；要表达逐二进制 allowlist，需要预先构建只含允许命令的目录（每个命令一个 shim/链接），并保留语言运行时目录，后者仍会暴露该目录下的全部可执行文件。
- `command_not_found_handle` 不是跨 shell 统一机制（bash 有，zsh 是另一个名字，dash 没有），且只处理查找失败，无法阻止已成功找到的绕行命令。
- 即使结合 sandbox，普通的文件/网络隔离也不保证"每个命令都经过 apexe"。

**已实现（rev4）**：`apexe sh --locked` 把 PATH 限制为 shim 目录 + `--allow-path` 条目；`apexe-sh` 通过 `APEXE_SHELL_LOCKED` / `APEXE_SHELL_ALLOW_PATH` 使用同一设置。名称和文档仍称 **受限命令搜索路径**，只宣称减少误调用，不宣称全量治理。

---

## 6. 宿主接入：版本化实验结果

| Host | 候选机制 | 证据强度 | 必须验证 |
|---|---|---|---|
| OpenCode | `opencode.json` 的 `"shell"` | **已实测通过**（1.19.5，2026-11-04）：项目级配置接受自定义绝对路径，bash 工具经 `apexe-sh -c ...` 执行，shim 命令命中并写入审计 | 结论仅适用于已测版本；升级后复测 |
| pi | settings `shellPath` → 绝对路径 | **已实测通过**（pi-coding-agent 1.1.0，macOS arm64，2026-11-04）：`~/.pi/agent/settings.json` 设 `"shellPath": "<shim 目录>/apexe-sh"` 后，bash 工具以 `apexe-sh -c <cmd>` 执行；`command -v ls/cat/head` 命中 shim，`APEXE_SHELL_DEPTH=1`；shim 命令写入 `audit.jsonl`（allow + execution），语法拒绝写入 `refusal` 且 fail-closed（`head -3` 无 stdout、退出码 1、不回落真实二进制）；`exit 7` 原样透传；嵌套 shell 仍受治理 | 真实 shell 取自 `$SHELL`（本机 `/bin/zsh`），如需与 pi 默认 bash 一致可额外设 `APEXE_REAL_SHELL=/bin/bash`（已实测）；升级后复测 |
| Codex | 真实 login shell + managed startup blocks | **自动 PATH shim 接入通过**（CLI 0.145.0，macOS arm64，2026-11-04，确定性宿主实验）：`.zprofile` 修复后普通 `/bin/zsh -lc` 调用在 snapshot 开/关时均命中 shim，cwd、ACL 与审计正确；`SHELL=apexe-sh` 仍不能替换其 shell | 采用 `apexe env --reload --shell zsh` 后重启 Codex；在线模型验证、其他版本与平台仍待实验，详见 §7 |
| Claude Code | 启动环境 PATH + `PreToolUse` hook | **PATH 已验证**（2.1.318，2026-11-04，隔离 HOME/config 实验）：`$SHELL` 与 `CLAUDE_CODE_SHELL` 指向 `apexe-sh` 均被拒绝并回退 `/bin/zsh`；rc 管理块不进入 snapshot 的 PATH；启动进程 PATH 自带 shim 时，Bash 工具经 shim 执行并写入审计。**Hook 已实现并实机验证**（`apexe hook claude`；2.1.318，2026-11-05）：`updatedInput` 的改写命令被执行，JSON `deny` 阻断调用 | 结论仅适用于已测版本；升级后复测 hook 输出协议，hook 启动失败仍为 fail-open |

补充：

- **Claude Code hook 的两种模式**：`exit 2` 与 JSON `permissionDecision: "deny"` 都会阻断调用（均在 2.1.318 实测）；JSON `permissionDecision: "allow"` 与 `updatedInput` 会被宿主采用，实测改写后的命令确实执行。一次 Bash 调用内部仍可执行多条命令，因此 hook 只改写简单外部命令，复合命令继续依赖 PATH shim。hook 命令无法启动时仍为 fail-open，依赖 hook 必须保证其可执行。
- 所有宿主实验需记录版本号与配置片段；结论不得跨版本外推。

---

## 7. 实施顺序（修订）

### 阶段 0：宿主接入实验（先行，门禁）

至少完成一个目标宿主的端到端实验：

1. 自定义 shell 是否被接受；
2. 参数（`-c`、`-l`、脚本文件）是否正确透传；
3. 环境与 snapshot 是否保留 PATH；
4. 拒绝/失败是否可观测。

未通过不得进入阶段 1 的默认推荐。

**已完成（OpenCode 1.19.5，2026-11-04）**：

- 项目级 `opencode.json` 设置 `"shell": "<config_dir>/bin/apexe-sh"`，自定义绝对路径被接受。
- agent 的 bash 工具实际以 `apexe-sh -c "<command>"` 启动。
- 仅由 shim 提供的 `apxepwd` 成功执行，输出为项目 cwd。
- `audit.jsonl` 记录 `module_id: cli.apxepwd`、`caller_id: local`、`status: success`。
- 结论：OpenCode 可作为 shell 接入的首个验证宿主；其他版本与其他宿主仍需按本清单实验。

**Codex CLI 0.145.0：确定性宿主实验（macOS arm64，2026-11-04）**：

- 使用临时 HOME、CODEX_HOME、绑定、shim 和 ACL；未修改用户 shell 配置。测试模块 `cli.apxepwd` 的目标是实际安装的 `/bin/pwd`，输入 schema 无参数，命令名仅由 shim 提供。ACL 仅允许 `local` 调用该模块，其余默认拒绝。
- 在线 `codex exec` 先发生连接超时，随后返回当前 ChatGPT 账户不支持已配置模型 `gpt-6.1-terra`。因此本次**没有完成在线模型端到端验收**。
- 改用本机确定性 Responses 测试服务返回预设工具调用，实际由未修改的 Codex CLI 执行 `tools.exec_command`。这验证宿主执行行为，不验证模型是否会自主选择接入方式。
- 环境配置为 `SHELL=<temporary-home>/.apexe/bin/apexe-sh`、`APEXE_REAL_SHELL=/bin/zsh`、`shell_environment_policy.inherit="all"`；普通终端调用不指定 shell，也不修改 PATH。

| 对照场景 | 实际行为 | 结论 |
|---|---|---|
| 保留 `apexe env --enable` 生成的临时 `.zshrc` | Codex 记录 `/bin/zsh -lc`；shim 路径在系统目录之前；`apxepwd` 返回正确 cwd，退出 0；审计含 ACL allow 与 execution success；`APEXE_SHELL_DEPTH` 为空 | PATH shim 接入生效，不能据此认定启动器被采用 |
| 移开临时 `.zshrc`，仅保留上述环境配置 | 仍为 `/bin/zsh -lc`；PATH 不含 shim；`apxepwd` 退出 127，无新增执行审计 | 仅设置 `SHELL` 不能完成自动接入 |
| 无临时 `.zshrc`，终端工具参数显式设置 `shell=<entry-point>` | Codex 记录 `/bin/sh -lc`；`apxepwd` 退出 127 | 该版本的工具 shell 参数未接受此自定义入口 |
| 无临时 `.zshrc`，工具命令显式调用 `apexe sh --shell /bin/zsh -c '<probe>'` | 外层仍为 `/bin/zsh -lc`；内层 shim 路径居首；`APEXE_SHELL_DEPTH=1`；cwd 正确、退出 0；审计含 ACL allow 与 execution success | 启动器在 Codex 内可显式使用，但仍需 agent 主动包装命令 |

以上为修复前对照；**不把 Codex 标记为自定义 shell 替换通过**。拒绝/取消/TTY 等完整验收项仍未完成，结论不得跨版本外推。

**自动接入修复与复测**：根因是 zsh 启动配置只写 `.zshrc`，而 Codex 默认的非交互登录 shell 跳过该文件。修复在同一 `HOME` / `ZDOTDIR` 下同时管理 `.zshrc` 和 `.zprofile`，复用已有所有权校验与卸载机制。`apexe sh --check` 现在单独给出 Codex 接入指导。

- 旧安装执行 `apexe env --reload --shell zsh`（自定义 shim 目录需携带 `--shim-dir`），保留已安装的绑定目录、ACL 与过滤条件；然后重启 Codex，刷新 shell snapshot。
- 真实 Codex CLI 0.145.0 使用 `SHELL=/bin/zsh`；测试工具只执行普通探针命令，不指定 shell、不修改 PATH、不包装 `apexe sh` / `apexe exec`。本机确定性 Responses 服务仅提供预设调用，执行与 shell 选择仍由 Codex 完成。
- 临时 `.zprofile` 原先含 `export PATH=/usr/bin:/bin`，managed block 追加在后；开启和关闭 `features.shell_snapshot` 两次调用均解析到临时 shim `apxepwd`，返回正确 cwd、退出 0，审计均记录 `local` 的 ACL allow 与 `cli.apxepwd` execution success。
- 同一自动接入配置下，真实 Codex 工具执行 `exit 7` 返回退出码 7、无输出；执行 `printf ok` 返回退出码 0，输出恰为 `ok`，无激活噪声。
- 新增回归覆盖非交互登录 shell 在 PATH 重置后命中真实 shim、ZDOTDIR、重复安装与原文恢复、旧安装升级时保留 ACL，以及修改 `.zprofile` 管理块时拒绝卸载且保留 shim。
- 结论：**自动命中现有 eligible shim 已通过**，无需 agent 逐条包装命令；Codex 仍使用其认可的真实 shell。这不改变 builtin、绝对路径和未覆盖命令的边界。

实验材料位于本机 `/private/tmp/apexe-codex-shell-probe/`：`host_probe.py`、`host-events.jsonl`（保留启动文件的首轮）、`host-events-default.jsonl`、`host-events-explicit.jsonl`、`host-events-launcher.jsonl`；修复后使用 `host_probe_auto.py`，证据为 `host-events-auto-snapshot.jsonl`、`host-events-auto-no-snapshot.jsonl`、`host-events-auto-exit.jsonl`、`host-events-auto-stdout.jsonl`，审计在 `home/.apexe/audit.jsonl`。这是临时证据目录，不属于仓库交付物。

修复前全量回归 `cargo test --all-features --locked`：**1496 passed、0 failed、3 ignored**。首轮受限环境在本地 HTTP Explorer 用例失败；允许本地服务监听后，全量复测通过。

修复后 `make check` 全部通过：格式、Clippy、字符检查、构建与所有测试门禁成功；Rust 测试（含 example tests）合计 **1503 passed、0 failed、3 ignored**，Python 回归 **77 passed**，CLI 运行矩阵 **174 passed、0 failed**，示例运行矩阵 **15 passed、0 failed**。

**Claude Code 2.1.318：隔离宿主实验（macOS arm64，2026-11-04）**：

- 使用临时 `HOME`、`CLAUDE_CONFIG_DIR`、绑定、shim 与 ACL；测试模块 `cli.apxepwd` 的目标是 `/bin/pwd`，命令名仅由 shim 提供；ACL 仅允许 `local` 调用。实验覆盖 headless（`claude -p`）与 pty 交互会话，未修改用户 shell 配置。
- 结论仅适用于 CLI 2.1.318；升级后需复测。

| 对照场景 | 实际行为 | 结论 |
|---|---|---|
| `SHELL=<shim>/apexe-sh` + `APEXE_REAL_SHELL=/bin/zsh` | Bash 工具内 `SHELL=/bin/zsh`，`APEXE_SHELL_DEPTH` 不存在；`apxepwd` 退出 127，无审计 | 自定义 shell 替换不受支持；`APEXE_REAL_SHELL` 不会被读取 |
| `CLAUDE_CODE_SHELL=<shim>/apexe-sh` | debug 日志：`is not a valid bash/zsh path, falling back to detection`；最终使用 `/bin/zsh` | 显式覆盖同样被拒绝 |
| 仅安装 `env --enable` 的 `.zshrc` / `.zprofile` 管理块 | snapshot 的 `export PATH` 取自启动环境，不含 shim；`apxepwd` 退出 127，无审计 | rc 管理块不构成 Claude Code 的接入路径 |
| 启动进程 PATH 前置 shim（模拟已激活终端） | headless 与交互均：`apxepwd` 输出项目 cwd、退出 0；snapshot PATH 保留 shim；审计含 ACL allow 与 execution success | **推荐接入：从已激活 shim 的终端启动 Claude Code**，或显式注入 PATH |
| PreToolUse hook `exit 2` | Bash 工具调用被阻断，`permission_denials` 含 Bash | 可阻断工具调用 |
| PreToolUse hook `exit 1` / hook 命令不存在 | 仅告警或静默，命令照常执行 | 非 2 退出码与 hook 启动失败均为 fail-open；fail-closed 需要 hook 自身可靠返回 2 |
| PreToolUse hook JSON `permissionDecision: "deny"` | 调用进入 `permission_denials`，命令未执行，模型收到拒绝理由 | JSON `deny` 与 `exit 2` 等价 |
| PreToolUse hook JSON `allow` + `updatedInput` | debug 日志解析出改写后的命令，探针输出 `HOOK_ROUTED`；非交互 `-p` 下未加额外允许规则仍执行 | `updatedInput` 生效，可用于路由 |

- snapshot 机制由 debug 日志确认：`Creating shell snapshot for zsh (/bin/zsh)`、`Looking for shell config file: <home>/.zshrc`、`Spawn-env probe captured 65 keys`，最终 snapshot 的 `export PATH` 是启动环境的值而非 rc 修改后的值。
- 证据材料位于 `/private/tmp/apexe-claude-shell-probe/`（临时目录，不属于仓库交付物）：`env1.json`、`int4-tty.log`、`keep/snapshot.sh`、`dbg2.log`、`hook-{deny,warn,missing}.json`，审计在 `home/.apexe/audit.jsonl`。
- 与 Codex 一致的结论：宿主必须在启动时继承受治理 PATH；不要依赖宿主在每次命令前重新构造 PATH。

### 阶段 1：最小 MVP

1. `src/cli/sh.rs`：`execve` 真实 shell、参数透传、自身检测、递归保护。
2. `src/cli/mod.rs` / `src/main.rs`：识别 `apexe-sh` argv[0]，置于 `is_shim_invocation` 分支之前。
3. 仅接入现有 `verified` shim；不新增路由档位。
4. `apexe sh --check`：有效策略诊断（真实 shell、shim 目录、manifest、ACL、覆盖档位）。
5. 保持现有 stdin 与路径检查契约；新增 §4.3 的 cwd 回归测试。
6. 首版只输出宿主配置片段，不自动修改用户配置。

### 阶段 1 实施状态（rev3）

已实现：

- `apexe sh`：`execve` 真实 shell、参数透传、自身检测、递归环境变量。
- `apexe-sh` argv[0] 分派，位于 `is_shim_invocation` 之前。
- `apexe env --enable` 安装 `apexe-sh` 符号链接；`env --disable` 移除；`env --check` 校验。
- `apexe sh --check --format text|json`：真实 shell 来源、entry point、manifest、ACL、shim 命令、宿主配置片段（OpenCode/pi/Codex/Claude Code）和问题列表。
- `--check` 在 entry point 或 manifest 缺失时失败；ACL 缺失为警告。
- 保持现有 stdin 与路径检查契约；新增 cwd 回归测试。
- 运行时参数覆盖：`--check`、`--format text/json`、`--shell`、转发 argv。

阶段 2 已实现：

- 本地环境继承：`Passthrough` 继承 + `LOCAL_ENV_DENYLIST`；`Captured` 维持 scrub。
- 后代路由：本地 PATH 保留 shim 目录；`Captured` 仍清理。
- 受限命令搜索路径：`--locked` / `--allow-path`；`apexe-sh` 支持 `APEXE_SHELL_LOCKED` / `APEXE_SHELL_ALLOW_PATH`。
- 本地审批：`/dev/tty` 提示、非交互拒绝、poll 超时；MCP/A2A 保持客户端 elicitation。
- `scanned` 原 argv 执行：`--scanned readonly|all`，`tier: scanned` 审计。
- Claude Code PreToolUse hook：`apexe hook claude`，`updatedInput` 改写简单外部命令。
- 宿主指南 `docs/agent-shell.md` 与 `apexe agent <host>` / `--check`。
- `apexe agent --write` 自动合并宿主配置；`--mcp-only` 生成 MCP-only 部署。

仍未实现：

- MCP-only 配置生成（当前只有部署说明）。
- 持久化 shell 的跨宿主适配与更完整的 I/O（stdin、二进制输出）。

阶段 0 的宿主实验仍是"提升为默认推荐"的门禁，不影响本 MVP 的可用性。

### 阶段 2：独立评审后再说

1. ~~本地凭据环境策略~~ **已实现**：`Passthrough` 继承 + 动态加载 denylist；`Captured` 维持 scrub。
2. ~~审批通道（本地如何确认被审批的调用）~~ **已实现**：`/dev/tty` 提示 + 非交互拒绝 + poll 超时；`Captured` 保持 MCP elicitation。
3. ~~扩大 native 覆盖~~ **已实现**：`--scanned readonly/all`，原 argv 执行 + `tier: scanned` 审计；`--check` 报告覆盖度。
4. ~~子进程路由~~ **已实现**：本地 PATH 保留 shim 目录；`Captured` 仍清理。
5. ~~受限命令搜索路径（原 `--locked`）~~ **已实现**：`--locked` + `--allow-path`，不宣称全量治理。
6. 宿主强制执行——**部分实现**：Claude Code hook 已实现；MCP-only 已在 §4.8 记录部署模式，未做配置生成。

---

## 8. 验收标准（修订）

### 8.1 命令级

只使用**确定经过 shim** 的测试命令，并同时断言输出、退出码、拒绝行为与审计记录：

```text
command -v ls                     # 仅证明解析位置，不证明治理生效
ls -la                            # 命中 shim；断言输出与退出码；审计含 execution 记录
apexe sh -c 'exit 7'              # 外层退出码必须为 7
cat                               # 仅在 streaming_stdin_safe 的 reviewed 模块下断言 stdin 透传
bash -c 'ls'                      # 嵌套 shell 仍命中 shim，并产生第二条审计
apexe sh -c 'echo ok' >out        # out 中只有 "ok"，无 apexe 任何字节
apexe sh --check --format json    # 诊断正常；$SHELL 自引用时给出明确错误
```

补充用例（rev1 缺失）：

- **cwd**：用测试 fixture 中经过 shim 的命令打印 cwd，而不是 builtin `pwd`。
- **环境策略**：用经过 shim 的 fixture 打印目标环境变量，而不是未覆盖的 `aws`。
- **退出码**：`apexe sh -c 'exit 7'`；不要用 `false; echo $?`（后者通常以 0 退出）。
- **ACL**：缺失 ACL、ACL 拒绝、审计记录三种情况分别断言。
- **损坏输入**：manifest 损坏、绑定缺失、shim 目录被移走时给出可操作错误。
- **登录 shell**：`-lc` 下 PATH 未被 `path_helper` 重置。
- **递归**：shim 目录不被自身再次解析；嵌套调用不无限递归。
- **取消与 TTY**：TTY 下 `Ctrl-C`、交互式 stdin、`git commit` 编辑器。

### 8.2 宿主回归（需记录版本）

- 阶段 0 的实验记录，覆盖目标宿主 + 版本 + 配置片段。
- 验证通过后才写入"推荐接入"文档。

---

## 9. 风险与非目标

### 风险

| 风险 | 缓解 |
|---|---|
| 误以为 `apexe sh` 等于全量治理 | 文档与 `--check` 明确覆盖档位；仅宣称减少常规误调用 |
| 环境继承导致凭据被授予子进程 | **已实现并接受**：本地 `Passthrough` 继承是信任授权；denylist 仅处理加载注入，不包装成"泄露缓解" |
| 子进程绕过 shim | **已实现本地后代继承**；解释器内部、绝对路径与未覆盖命令仍是已知边界 |
| 登录 shell PATH 重置导致静默失效 | `--check` 检测；managed block 写入 profile 末尾 |
| 宿主升级后接入点失效 | 版本化回归测试；失败时回退到 PATH shim + profile 注入 |
| 非交互环境无法完成本地审批 | **已实现**：拒绝并给出补救建议，不阻塞 agent；交互终端在 `/dev/tty` 提示 |
| Windows 语义差异 | v1 仅支持 Unix；Windows/PowerShell 另立方案 |

### 非目标

- 不是 sandbox；不提供文件系统/网络隔离，也不把 sandbox 当成"每个命令都经过 apexe"的保证。
- 不替代 MCP/A2A 工具面或 host hook。
- 不治理 builtin、函数、绝对路径、`sudo`、以及子进程内部直接 `execve`。
- 不通过 `scanned + approval` 承诺透明执行能力。

---

## 10. 待团队决策（修订）

1. 是否将 `apexe sh` 提升为一等命令，并接受 Unix 首发？**建议：是。**
2. 默认接入策略：是否只在验证通过的宿主上优先推荐 shell，Skill 保留发现/解释/指导职责？**建议：是。**
3. ~~本地环境继承策略~~ **已决定并实现**：`Passthrough` 继承 + 动态加载 denylist；接受"凭据可用即授权"。
4. `scanned` 路由：是否先只做契约可信度展示，不接入执行？（**建议：是，暂缓执行**）
5. ~~受限命令搜索路径（原 `--locked`）~~ **已实现**：`--locked` / `--allow-path`，不宣称全量治理。
6. 宿主配置写入：首版只输出片段，自动修改配置作为独立安装功能？（**建议：是**）
7. ~~本地审批通道~~ **已实现**：`/dev/tty` 提示 + 非交互拒绝；`ApprovalStore` 仍为库级特性。

---

## 附录 A：关键代码位置

| 位置 | 说明 |
|---|---|
| `src/cli/mod.rs:27` | `is_shim_invocation`，argv[0] 分派 |
| `src/cli/mod.rs:33` | `run_shim` |
| `src/main.rs:95` | shim 分派入口，需在此之前识别 `apexe-sh` |
| `src/cli/shim.rs:175` | `try_execute` |
| `src/cli/shim.rs:200` | `resolve_route` |
| `src/cli/shim.rs:308` | `stdin_mode`，按 `streaming_stdin_safe` 选择 |
| `src/cli/shim.rs:76` | `eligible`，authoritative overlay 门槛 |
| `src/cli/shim.rs:257` | `route_candidates` 强制 `eligible` |
| `src/cli/exec/native.rs:20` | native argv 解析入口，可复用的解析/递归保护逻辑 |
| `src/module/executor.rs:911` | `ENV_ALLOWLIST` |
| `src/module/executor.rs:966` | `current_dir` 设置（统一 path guard root） |
| `src/module/executor.rs:967` | `env_clear` |
| `src/module/executor.rs:968` | `stdin` 默认 `/dev/null` |
| `src/module/executor.rs:981` | 传递 PATH 时剔除 shim 目录 |
| `src/governance/path_guard.rs:439` | `PathGuard::from_env`，root 来自进程 cwd |
| `src/module/cli_module.rs:98` | 缺少 `streaming_stdin_safe` 时拒绝继承 stdin |
| `src/cli/local.rs:56` | 本地执行器 `approval_store: None` |
| `src/runtime_path.rs:21` | `is_shim_directory` |
| `src/runtime_path.rs:33` | `search_path`（清理 shim 目录） |
| `tests/native_shims.rs:534` | 二进制 stdin 透传测试 |

## 附录 B：待验证实验清单

1. ~~OpenCode：`"shell"` 指向 `apexe-sh` 后，agent 工具调用是否生效；自定义名是否被接受。~~ **已通过（1.19.5）**：自定义绝对路径被接受，bash 工具经 `apexe-sh -c` 执行，shim 命中并写入审计。
2. ~~pi：`shellPath` 指向 `apexe-sh` 后生效，且 pi 自身 bin 目录前置不覆盖 shim。~~ **已通过（pi-coding-agent 1.1.0）**：pi 通过 `getShellEnv()` 前置 `~/.pi/agent/bin`，`apexe-sh` 在 `execve` 前再把 shim 目录置于 PATH 首位，因此 shim 仍优先命中；`apexe-sh -c` 调用、审计写入、拒绝 fail-closed、退出码透传与嵌套 shell 均以在线模型端到端验证。真实 shell 默认继承 `$SHELL`，未设 `APEXE_REAL_SHELL`。
3. Codex：**CLI 0.145.0 的自动 PATH shim 接入已修复，并通过 snapshot 开/关的确定性宿主实验**，详见 §7。`SHELL=apexe-sh` 与工具自定义 shell 路径仍不支持直接替换宿主 shell；在线模型端到端验证及其他平台/版本仍待完成。
4. ~~Claude Code：`SHELL` + snapshot 后 PATH 是否保留；`APEXE_REAL_SHELL` 是否正确避免自引用；PreToolUse 的 fail-closed 配置。~~ **已验证（2.1.318）**：`SHELL` / `CLAUDE_CODE_SHELL` 指向 `apexe-sh` 均被拒绝并回退 `/bin/zsh`；snapshot PATH 取自启动环境，rc 管理块无效；启动进程 PATH 含 shim 时接入通过；PreToolUse 的 `exit 2` 与 JSON `deny` 均阻断，`updatedInput` 的改写命令被执行（2026-11-05），hook 启动失败为 fail-open。
5. 上述任一宿主：`apexe sh -c 'exit 7'`、shim 审计记录、ACL 拒绝、manifest 损坏诊断。
6. TTY 下的 `Ctrl-C`、`cat`、`git commit` 编辑器行为。

## 附录 C：八项验证记录（2026-11-05）

| 项 | 结果 | 证据 |
|---|---|---|
| 1. 交互式 `apexe sh` / `-- script.sh` | 通过 | `tests/agent_shell_pty.rs`：PTY 交互、脚本参数 |
| 2. 登录 shell `-l` 的 profile PATH 重置 | 通过 | 测试先重置 `PATH`，再由 managed block 恢复 shim |
| 3. Ctrl-C / SIGINT / SIGTERM / SIGPIPE / job control | 通过 | Rust 信号与管道测试；Python PTY 的 Ctrl-C、后台 job + `wait` |
| 4. 本地审批 yes/no/超时 | 通过 | Rust PTY 的 yes/no；Python PTY 的 1 秒超时（`APEXE_APPROVAL_TIMEOUT_SECONDS`） |
| 5. denylist 实际移除 | 通过 | `LD_PRELOAD` / `DYLD_INSERT_LIBRARIES` 端到端探针输出 `unset` |
| 6. `apexe sh` + streaming stdin | 通过 | `streaming_stdin_safe` shim 经 `apexe sh -c` 读取管道输入 |
| 7. Claude Code `updatedInput` 实机 | 通过 | 2026-11-05，Claude Code 2.1.318 + `target/debug/apexe`：`--settings` 加载 `PreToolUse` hook 后，debug 日志解析出 `updatedInput.command`，探针输出 `HOOK_ROUTED:hello`；对照组无 hook 时同一命令退出 127。manifest 缺失时 JSON `deny` 进入 `permission_denials`，命令未执行 |
| 8. 真实 `~/.apexe` 安装 | 通过 | 现有 `~/.apexe/bin/apexe-sh`、manifest、ACL；`apexe sh --check` 成功，`apexe-sh -c 'command -v cat'` 命中 shim |

八项均已自动化或实机验证。第 7 项的结论只适用于 Claude Code 2.1.318；宿主升级后需按同一场景复测（设置 `APEXE_SHIM_DIR`，用 `--settings` 注入 hook，观察改写命令是否执行以及 `deny` 是否进入 `permission_denials`）。

## 附录 D：rev1 → rev2 修订记录

| 编号 | rev1 表述 | 复核结果 | rev2 处理 |
|---|---|---|---|
| 1 | PATH 前置"消掉 agent 是否配合这个变量"、强制力"中～强" | 高估。只改变命令查找顺序，不构成强制边界 | 改为"减少常规调用对 agent 主动配合的依赖"，强制力标"中" |
| 2 | Skill 与 threat model 自相矛盾 | 不准确。矛盾在"宣传方式"，不在 Skill 本身 | 改为"把路由约定当强制执行保证"才是问题；保留 Skill 的发现/解释职责 |
| 3 | `--locked` 可保证全量治理 | 不成立。PATH 是目录粒度；`command_not_found` 不可移植；绝对路径/解释器可绕过 | 改称"受限命令搜索路径"，暂缓，明确只减少误调用 |
| 4 | 环境清空、stdin、cwd 三处阻断 | 仅环境清空属实且范围限于执行器；stdin 已按能力门槛继承；cwd root 即进程 cwd | 删除 stdin/cwd 阻断判断；保留能力门槛并补 cwd 回归测试 |
| 5 | `Inherit + denylist` 可缓解密钥泄露 | 不准确。denylist 只降低动态加载注入，凭据可用即授权 | 改写为信任授权；默认集合与拒绝集合独立评审 |
| 6 | `scanned + approval` 可作为配套 | 审批通道缺失（`approval_store: None`）；审批不修复 argv 映射 | 暂缓；分离"契约可信度"与"执行权限" |
| 7 | 宿主接入结论（OpenCode/pi/Codex/Claude） | 均不足以支撑默认推荐；存在 `$SHELL` 自引用冲突 | 全部降级为待实验；加入 `APEXE_REAL_SHELL` 并修正解析顺序 |
| 8 | 验收用例（`pwd`、`aws`、`false; echo $?`、`command -v`） | 四项均不能验证其声称的行为 | 改为 fixture + shim 命令 + 退出码/审计断言；补充 ACL、损坏输入、登录 shell、递归、TTY |
| 9 | 实施顺序：先 MVP 再宿主验证 | 顺序倒置 | 改为阶段 0 宿主实验门禁 → 阶段 1 MVP → 阶段 2 独立评审 |
