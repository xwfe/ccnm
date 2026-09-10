# ccnm

Keep the AI coding agent and the real project runtime on separate nodes.

The current pre-release build runs official CLI agents on an **Agent Node** and executes project tools on a **Runtime Node** through a persistent SSH stdio MCP transport. Legacy Claude Code remains the default; configured Agent Instances can select Claude or the measured Codex CLI version. Source code, Git, builds, tests, and toolchains stay on the Runtime Node; AI credentials stay on the Agent Node.

**No source sync. No remote AI credentials. No custom model API client.**

> Status: pre-release dogfood, macOS only. Both provider chains have been exercised on two real macOS nodes; the machine API for external programs is implemented but has not yet run against a real agent. Not published yet, and configuration may still change.

---

把 AI Coding Agent 和真实项目运行环境放在不同的节点（node）上。

当前预发布实现是在 **Agent Node** 上运行官方 CLI Agent，通过持久 **SSH stdio MCP** 在 **Runtime Node** 上执行项目工具。旧配置仍默认 Claude Code；Agent Instance 配置可选择 Claude 或已测版本的 Codex CLI。源码、Git、构建、测试和工具链都留在 Runtime Node；AI 登录凭证只留在 Agent Node。

**不复制源码，不把 AI 凭证下放到 Runtime Node，也不实现私有模型 API Client。**

> 当前处于发布前 dogfood 阶段，**只支持 macOS**。Claude 和 Codex 两个方向的公共入口都已在授权双机上真机跑通；给外部程序用的 machine API 已经实现，但**还没跟真实 Agent 跑过一次**。准确范围和未验证项见[支持矩阵](docs/support-matrix.md)。

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
- [架构说明](docs/architecture.md)：Node / Agent / Runtime / Controller 与 SSH stdio MCP
- [生产安全](docs/production-safety.md)：`ccrun`、ACL、凭证、sudo、网络出口边界
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

## 还没验证的

- **egress / 网络策略没有逐项验证。** 因此这个项目**不声明任何出口边界**，需要这种保证的场景由 OS 和网络层自己落实。
- **Ctrl-D 没有证据**：官方 CLI 对该键无响应，只用 `/exit` 覆盖了同一条自然退出路径，两者不等价。
- **machine API 没跟真实 Agent 跑过一次。** 协议是 v1 候选，不是稳定 v1。
- colocated 模式没有真实验收，因此明确拒绝，不静默降级。

## 给程序用的接口

要让别的程序驱动 ccnm，用 `ccnm rpc`：stdio 上的 JSON-RPC 2.0，不开网络端口。契约、schema、fixture 和一个可以直接抄走的 Python 客户端见[协议说明](docs/protocol/README.md)。

## 接下来做什么

暂时不继续堆功能，优先用真实项目 dogfood 决定后续契约。Git 专用 MCP 工具、后台长进程、Browser provider、Linux Controller 和多 Agent 编排都放到真实需求出现之后再做。

## 许可证

MIT，见 [LICENSE](LICENSE)。
