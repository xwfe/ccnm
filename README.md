# ccnm

Keep the AI coding agent and the real project runtime on separate nodes.

The current pre-release build runs official CLI agents on an **Agent Node** and executes project tools on a **Runtime Node** through a persistent SSH stdio MCP transport. Legacy Claude Code remains the default; configured Agent Instances can select Claude or the measured Codex CLI version. Source code, Git, builds, tests, and toolchains stay on the Runtime Node; AI credentials stay on the Agent Node.

**No source sync. No remote AI credentials. No custom model API client.**

> Status: release candidate, not published yet. Platform support is two different answers: the **Agent side** (Controller, sessions) is **macOS only** — the Controller is a launchd LaunchAgent; the **Runtime side** (`internal mcp-serve` and the seven tools) has real evidence on **macOS and on Debian 13 / x86_64**. Both provider chains have been exercised on real macOS nodes, and the machine API has closed the loop against a real agent once per provider, compared against the human CLI by side effect. Both contracts are frozen: `ccnm.machine/1` on 2026-09-10, `ccnm.workspace-mcp/1` on 2026-09-11.

---

把 AI Coding Agent 和真实项目运行环境放在不同的节点（node）上。

当前预发布实现是在 **Agent Node** 上运行官方 CLI Agent，通过持久 **SSH stdio MCP** 在 **Runtime Node** 上执行项目工具。旧配置仍默认 Claude Code；Agent Instance 配置可选择 Claude 或已测版本的 Codex CLI。源码、Git、构建、测试和工具链都留在 Runtime Node；AI 登录凭证只留在 Agent Node。

**不复制源码，不把 AI 凭证下放到 Runtime Node，也不实现私有模型 API Client。**

> 当前是**发布候选**，尚未发布。平台支持要分两件事说，因为它们不是同一个答案：
>
> - **Agent 那一侧（Controller、会话管理）只支持 macOS**——Controller 是 launchd LaunchAgent，会话上下文检查直接问 `launchctl` 和 `security`；Linux/Windows Controller 均未验收。
> - **Runtime 那一侧（`internal mcp-serve` 与七个工具）在 macOS 和 Debian 13 / x86_64 上都有真机证据**；其他发行版和 arm64 Linux 没验过。
>
> Claude 和 Codex 两个方向的公共入口都已在授权真机上跑通；给外部程序用的 machine API 也已用两个 provider 各跑通一次真机闭环并与人类 CLI 做过副作用对照。两个协议都已冻结：`ccnm.machine/1` 于 2026-09-10，`ccnm.workspace-mcp/1` 于 2026-09-11。准确范围和未验证项见[支持矩阵](docs/support-matrix.md)。

## 角色模型

ccnm 把机器抽象成 **Node**，再由角色描述它负责什么：

- **Agent Node**：运行官方 Claude Code 或已测版本的 Codex CLI，并持有各自登录/订阅凭证。
- **Runtime Node**：保存真实 workspace 和项目 toolchain，执行 MCP 工具。
- **Controller**：运行在 Agent Node 的登录会话中，负责启动和管理 Agent session。
- **Runtime Service Account (`ccrun`)**：Runtime Node 上建议使用的低权限系统账号，用来限制 AI 命令继承的操作系统权限。

一个 Node 可以同时承担多个角色。当前双机结构只是其中一种部署方式。

```text
Runtime Node                              Agent Node
┌────────────────────────┐                ┌─────────────────────────┐
│ workspace / Git        │                │ Claude Code / Codex CLI │
│ build / test / tools   │◀── SSH stdio ─▶│ login + controller/tmux │
│ ccnm MCP runtime       │      MCP       │ session supervisor      │
└────────────────────────┘                └─────────────────────────┘
```

当前开放的是 remote SSH MCP 拓扑。未完成真机验收的 colocated 模式会明确拒绝，不静默降级；见[支持矩阵](docs/support-matrix.md)和[架构说明](docs/architecture.md)。

## 快速开始

当前前提：两台 macOS Node 可以双向 SSH；两边安装同一个 ccnm build；Agent Node 已登录 Claude Code；Runtime Node 已安装项目所需 toolchain。

在 **Runtime Node**：

```bash
ccnm init --agent agent-ssh-alias
cd /path/to/project
ccnm workspace add my-project
```

在 **Agent Node**：

```bash
ccnm init --runtime runtime-ssh-alias
ccnm controller install
```

回到 Runtime Node：

```bash
ccnm doctor my-project
ccnm my-project
```

两边配置好之后，也可以直接在 Agent Node 发起和管理会话：

```bash
ccnm my-project
ccnm status my-project
ccnm attach my-project
ccnm stop my-project
```

要配置同一 Agent Node 上的 Claude/Codex instance，并用 `--agent <instance-id>` 覆盖 workspace 默认值，见[配置说明](docs/configuration.md)。Codex 使用 ccnm 管理的专用 `~/.config/ccnm/agents/codex/`；必须由用户在 Agent Node 登录会话中用官方 CLI 独立登录，不能复制个人 `~/.codex` 或把 `CODEX_HOME` 发给 Runtime Node。

如果是有价值的真实项目，建议先完成 `ccrun` Runtime Service Account 隔离，再关闭 `allow_unconfined_exec`。见 [生产安全](docs/production-safety.md)。

**项目和 Claude 的登录在同一个账号下怎么办。** 那种情况没有东西可隔离，ccnm 默认会在 MCP 握手之前直接拒绝——那条边界正是它存在的理由。要么建专用账号，要么在 Runtime 侧那个 workspace 上把 `allow_unconfined_exec` 和 `allow_unisolated_credentials` 都写上，**明确接受**模型跑的每条命令都能读到那份登录。开关会在第一次起会话时把风险讲一次，`ccnm doctor` 里那几行永远是 WARN 而不是 OK。代价见[生产安全](docs/production-safety.md#凭据隔离那一条怎么放开代价是什么)。

## 核心能力

当前 MCP runtime 已经跑通完整编码闭环：

```text
workspace_info
read_file
list_files
search_text
apply_patch
exec_command
read_output
```

受 ccnm 管理的 remote Agent 会话使用各 Provider 已验证的工具策略，让项目访问统一经过 Runtime Node。

## 文档

- [快速开始](docs/getting-started.md)：安装、SSH 前提、首次配置、Controller
- [使用说明](docs/usage.md)：run、attach、status、stop、result、prompt
- [配置说明](docs/configuration.md)：`nodes`、workspace、双向 SSH 字段
- [支持矩阵](docs/support-matrix.md)：Provider、版本、topology、验收级别与明确拒绝项
- [公开协议](docs/protocol/README.md)：给外部程序的 machine API 契约与客户端示例
- [执行接口交接](docs/orchestrator-handoff.md)：独立 Orchestrator 的状态归属边界与最小 ExecutionBackend 示例
- [架构说明](docs/architecture.md)：Node / Agent / Runtime / Controller 与 SSH stdio MCP
- [生产安全](docs/production-safety.md)：`ccrun`、ACL、凭证、sudo、网络出口边界
- [运维](docs/operations.md)：安装、回退、配置迁移、状态清理、故障恢复
- [故障排查](docs/troubleshooting.md)：实际遇到过的运行问题
- [开发与发布](docs/development.md)：测试、CI、release、mutation test
- [研究记录](docs/research/)：实现调研和历史测量数据

仓库中的大型设计文档属于研发历史，部分旧章节仍会出现 `home/work`，那是历史术语；当前公开模型统一以 **Node + Agent / Runtime / Controller** 为准。

## 已经真机验证过什么

在两台真实 macOS 机器上跑通的：

- 持久 SSH stdio MCP session，search → read → patch → test → output 闭环
- Claude 和 Codex 两个方向的公共入口：print、interactive、stop、detach/reattach、Controller 重启、链路失败
- tmux 内交互式会话；detached 之后重新 attach；Controller 重启不杀正在运行的 session
- 精确 session 寻址、伪造 session 被拒、Runtime 单写 guard 的持有与释放
- 专用低权限执行身份：能正常用项目，读不到任何已知 Agent 凭据和 SSH 私有状态，没有 sudo/admin，特权 socket 不可写
- 项目 `CLAUDE.md` 投影；任一 Node 发起 prompt，包括多行 stdin

在一台真实 **Debian 13 / x86_64** 机器上跑通的（Runtime 那一侧，[记录](docs/research/p12-real-project-2026-09-11.md)）：

- 本机的 Claude Code 经 `ccnm mcp bridge` 在那棵远端树上完成了一次真改动：改文案、在远端跑测试、自己做了一个 commit，产物属主是 Runtime 执行身份
- 专用执行身份（uid 1002、只有自己的组、无 sudo、docker socket 不可写、`~/.ssh` 无私钥、读不到别人的 home）；`cargo`/`rustc`/`node`/`npm`/`git` 由 `exec_command` 在那台机器上答出版本
- 整套 Rust 测试在那台机器上绿，与 macOS 同数——**那一次是从两个红开始的**，两个都是真问题，都修了
- 六条失败路径各有具名拒绝：writer busy、没 opt-in、read 请求 coding 不降级、协议号不认识、远端进程被杀、Host 被杀

## 还没验证的

- **egress / 网络策略没有逐项验证。** 因此这个项目**不声明任何出口边界**，需要这种保证的场景由 OS 和网络层自己落实。
- **Ctrl-D 没有证据**：官方 CLI 对该键无响应，只用 `/exit` 覆盖了同一条自然退出路径，两者不等价。
- **machine API 的 `interactive` 模式没有实现**，输出不分页、结果不过期、`-32008` 从不返回。协议 `ccnm.machine/1` 已冻结，但冻结的是契约，不是说这些已经补上——补它们属于加法。
- colocated 模式没有真实验收，因此明确拒绝，不静默降级。
- **Linux 只验过 Runtime 那一侧**，而且只验过 Debian 13 / x86_64 一种。Linux 上的 Agent/Controller 没有实现也没有验收；arm64 Linux、别的发行版都没验过。
- Remote Workspace MCP 只验过**一个 Host**（Claude Code 2.1.268 的 `-p` 模式）和**一棵中型 Rust 项目**：Codex 当 Host、交互式 UI 里 bridge 启动失败怎么显示、monorepo 规模、真实 node 项目的 `npm ci && npm test` 都没验过（Node 目前只验到"叫得动"）。

## 给程序用的接口

要让别的程序驱动 ccnm，用 `ccnm rpc`：stdio 上的 JSON-RPC 2.0，不开网络端口。契约、schema、fixture 和一个可以直接抄走的 Python 客户端见[协议说明](docs/protocol/README.md)。

要写的是一个**编排项目**（决定谁做什么、验收和重试），先看[执行接口交接](docs/orchestrator-handoff.md)：哪份状态归你、哪份归 ccnm，以及一个最小的 `ExecutionBackend` 示例。ccnm 自己不做编排。

如果你的 Claude Code / Codex 已经在本机跑着，只是项目在另一台机器上——那是另一个入口 **Remote Workspace MCP**：`ccnm mcp bridge <workspace>` 把远端项目作为一组绑定 workspace 的 MCP 工具交给它用，权限由 Runtime 侧的 `external_mcp` 决定（默认关）。契约 `ccnm.workspace-mcp/1` **已于 2026-09-11 冻结**，验收范围是一台 Debian 13 的 Linux Runtime 加一个真实 Claude Code（[dogfood 记录](docs/research/p12-real-project-2026-09-11.md)）；能用和不保证什么见[支持矩阵](docs/support-matrix.md)，契约见[协议说明](docs/protocol/README.md)。

## 接下来做什么

两个协议都已经冻结（`ccnm.machine/1` 2026-09-10、`ccnm.workspace-mcp/1` 2026-09-11），**真实项目 dogfood 那一轮已经结束**，契约不再等它来决定：往后加字段、加方法属于加法，删字段、改语义、加终态要升大版本号。

现在是发布前收口，不继续堆功能。Git 专用 MCP 工具、后台长进程、Browser provider、Linux Controller 和多 Agent 编排仍然放到真实需求出现之后再做；编排（谁做什么、怎么验收、什么时候重试）是[独立项目](docs/orchestrator-handoff.md)的事，不进 ccnm。

真正要补的是上一节"还没验证的"那几条——它们需要的是真机、额度和一次产品决定，不需要改设计。

## 许可证

MIT，见 [LICENSE](LICENSE)。
