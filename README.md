# ccnm

把 AI Coding Agent 和真实项目**放在两台机器上**。

Agent Node 跑官方 Claude Code / Codex CLI 并持有 AI 登录；Runtime Node 放源码、Git、构建和工具链，项目工具经持久的 SSH stdio MCP 通道在这里执行。Agent 侧另可提供已安装的 skills 和经配置允许的 MCP，不能把“禁用原生 Read/Bash”理解为 Agent 上没有任何文件访问或执行。

**不整库同步，不把 AI 登录凭据下放到 Runtime Node，也不实现私有模型 API Client。** 模型需要的源码片段仍会经工具结果进入 Agent 与模型服务；节点分离不是“源码数据永不离开 Runtime”的保证。

```text
Runtime Node                              Agent Node
┌────────────────────────┐                ┌─────────────────────────┐
│ workspace / Git        │                │ Claude Code / Codex CLI │
│ build / test / tools   │◀── SSH stdio ─▶│ login + controller/tmux │
│ ccnm MCP runtime       │      MCP       │ session supervisor      │
└────────────────────────┘                └─────────────────────────┘
```

*Keep the AI coding agent and the real project runtime on separate machines. The agent runs on an **Agent Node** (macOS only) and reaches the project on a **Runtime Node** (macOS or Debian 13 / x86\_64) over a persistent SSH stdio MCP transport. Docs are in Chinese.*

## 什么时候用

当前版本 **v0.9.0**。ccnm 是执行层，不是任务规划、审查、发布或多 Agent 调度平台。能否覆盖一个项目的完整交付，见[生命周期与职责](docs/project-lifecycle.md)。

**已知缺陷 C51-01 的现状（2026-09-25）**：Runtime MCP 转接服务正常退出后子进程仍写工作区、写锁却已释放——P51 用真实二进制复现，P52 已在代码上修复并在 macOS 验证，**Linux 还没验**。Linux Runtime 上需要可靠单写交接的项目，在验完之前仍先停用 `[runtime_mcp]`、核实并清理旧进程后再换会话；server 派生的、离开进程组的后代任何平台都够不着；停用转接也不等于普通命令已具备完整进程隔离。见 [P52 记录](docs/research/2026-09-25-p52-relay-group-cleanup.md)。

- **项目在一台机器上，AI 订阅登录在另一台上，两边都不想挪。** 比如源码和工具链在工作机或服务器上，不能（或不想）在那里登录 Claude；而登录着 Claude 的那台 Mac 上又不该出现源码。
- **不想让模型用你自己的账号跑命令。** Runtime 那边可以用一个专用的低权限账号执行，模型读不到你的 SSH 私钥和 AI 登录；还可以再给每条命令套一层 OS 沙箱。
- **项目在远端，而 Claude Code 已经在你本机开着。** 不用 ccnm 启动 Agent，只把远端项目作为一组 MCP 工具交给它（见[另外两个入口](#另外两个入口)）。
- **要让别的程序驱动这一切**（编排器、批处理）：stdio 上的 JSON-RPC，不开网络端口。

**能不能在你的机器上跑**：Agent 那一侧只支持 **macOS**（Controller 是 launchd LaunchAgent，Linux/Windows 没实现）；Runtime 那一侧 **macOS** 和 **Debian 13 / x86_64** 都有真机证据。准确范围见[支持矩阵](docs/support-matrix.md)。

## 装

**两台机器都要装，而且必须是同一个版本**——版本对不上，起会话时会直接拒绝。去 [Releases](https://github.com/xwfe/ccnm/releases) 拿对应的包（macOS 取 `macos-universal`，Linux 取 `linux-x86_64`，后者只有 Runtime 那一半）：

```bash
tar -xzf ccnm-<版本>-macos-universal.tar.gz
mkdir -p ~/.local/bin
mv ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
```

两个会咬人的地方：

- **别用 `cp` 覆盖一个已经跑过的 ccnm。** Apple Silicon 上写进已执行过的 Mach-O 会让代码签名失效，之后每次执行都被 SIGKILL（`Killed: 9`），而老进程还在用老代码跑——现象极其迷惑。上面那句"新文件 + 改名"就是为了避开它。
- **浏览器下载的包带隔离属性**，macOS 直接拒绝执行：`xattr -d com.apple.quarantine ccnm`。用 `curl` 下载不会带。

## 快速开始

前提：按入口配置好非交互 SSH（Operator → Agent、Agent → Runtime Executor，不要求执行身份持有出站私钥），Agent Node 上官方 CLI 已登录，Runtime Node 上装好 `git`、`ripgrep` 和项目工具链。真实项目先准备低权限执行身份；配置向导不会代建账号或授予 ACL。

在 **Runtime Node**（放项目那台）：

```bash
ccnm init --agent <agent 的 ssh alias>
cd /path/to/project
ccnm workspace add my-project
```

在 **Agent Node**（跑 Claude 那台）：

```bash
ccnm init --runtime <runtime 的 ssh alias>
ccnm controller install
```

回到 **Runtime Node**：

```bash
ccnm doctor my-project      # 只读检查，先把红的处理掉
ccnm my-project             # 开始
```

**`ccnm init` 给哪个 flag，就说明这台机器是谁**——在放项目的机器上给 `--agent`，在跑 Claude 的机器上给 `--runtime`；两个一起给会被拒绝。SSH alias 只在定义它的那台机器上有意义，所以两台各写各的。

日常就这几条，两台机器上都能敲：

```bash
ccnm my-project                          # 起会话并接上
ccnm attach my-project                   # 接回已有会话（简写 ccnm a）
ccnm ls                                  # 所有项目：在不在跑、跑了多久、工具通不通
ccnm status                              # 同上，更细；不带项目名（简写 ccnm st）
ccnm log                                 # 跑过的会话，最新的在前
ccnm stop my-project
ccnm my-project --print "修复 parser 测试"   # 一问一答，不进 tmux，输出直接打在本地终端
```

每一步的完整说明和 `doctor` 红了怎么办见[快速开始](docs/getting-started.md)；Codex、Agent Instance、多行 prompt 见[使用说明](docs/usage.md)。ccnm 默认说中文，要英文加 `--lang en`（细节见[使用说明](docs/usage.md#说什么语言)）。

## 真实项目接入前：配置执行身份

默认配置没有替你创建隔离账号。模型命令若以**你自己的账号**跑，ccnm 只帮你分开机器，没有分开权限；诊断可能因此拒绝执行。真实项目应该先在 Runtime Node 建一个专用低权限账号：

```toml
[nodes.runtime]
runtime_user = "ccrun"
```

意思是"Agent 连进来之后，项目命令以 `ccrun` 的身份跑"，不是"你要用 `ccrun` 敲 ccnm"。做法见[生产安全](docs/production-safety.md)。

**如果项目和 Claude 登录本来就在同一台机器、同一个账号下**，那就没有东西可隔离，ccnm 默认在 MCP 握手之前直接拒绝，`doctor` 红在 `Claude 凭据` 那一行。这不是配错了；两条出路（建专用账号，或明确接受风险）见[快速开始](docs/getting-started.md#如果项目和-claude-登录在同一个账号下)。

## 会话里模型能用什么

Runtime 有 **12 个工具定义**；实际提供哪些取决于读写模式、配置和可用服务，以 `tools/list` 为准。外部 `read` 模式只有七个只读工具；coding 会话没有可转接 MCP 时是十一个工具。

```text
workspace_info  read_file      list_files     search_text
apply_patch     exec_command   read_output    load_skill
view_image      read_notebook  stop_command   call_mcp_tool
```

`load_skill` 把项目自带的 skills（`.claude/skills/`、`.claude/commands/`、`.agents/skills/`）交给模型：官方 CLI 靠当前目录发现它们，而 CLI 的当前目录不在项目机器上，所以由 Runtime 这边来找。skill 里的脚本照样在 Runtime 上跑。两台机器上装好的 skills（`~/.claude/skills` 这些）默认也交出去，见[配置说明](docs/configuration.md#machine_skills)。项目那台机器上的 MCP server（项目的 `.mcp.json`、执行账号装的）经 `call_mcp_tool` 交给能写的会话，和 `exec_command` 过同一道门，见[使用说明](docs/usage.md#项目那台机器上的-mcp-server)；Agent 机器上装的经 `ccnm_agent` 下的同名工具交出去，默认只给远端地址的，本机跑的要点名，见[使用说明](docs/usage.md#agent-机器上的-mcp-server)。细节和三处与官方不同的地方见[使用说明](docs/usage.md#项目自带的-skills)。

Agent 侧的 `ccnm_agent` 是另一组工具，不计入上面的 Runtime 工具数。`agent_tools` 默认开启 `web_search`、`mcp_servers`；抓取网页、子代理和待办清单须另行开启。两侧同名 MCP 工具使用不同节点的身份；Agent 的 `[agent_mcp] local` 点名允许本机服务，是额外信任，不是只读白名单。

后台命令支持启动、分页读输出与停止，但**活不过 MCP 连接**。断开 Operator 终端可能只是离开 tmux；MCP 断线会触发 Runtime 命令收尾，Runtime 笔记本睡眠后不能承诺继续构建。

两个按 workspace 打开的开关，默认都关：

- `exec_sandbox = "codex"`：Runtime 命令与 Runtime MCP server 包进 OS 沙箱——限制工作区外写入、`.git` 写入与网络。`git commit` 和依赖下载不能据此声称自动跑通；**它不覆盖 Agent MCP**，见[配置说明](docs/configuration.md#exec_sandbox)。
- `codex_exec_server = true`：让 Codex 用它自带的执行工具。**2026-09-17 已封存**，代码保留但不再维护，别在新项目上用；原因见[双执行入口方案](docs/plan/runtime-surfaces.md)第 12.0 节。

## 另外两个入口

**项目在远端，而 Claude Code 已经在你本机跑着**——用 `ccnm mcp bridge <workspace>`，把远端项目作为一组 MCP 工具交给它。权限由 Runtime 侧的 `external_mcp` 决定（默认关）。契约 `ccnm.workspace-mcp/1` 已于 2026-09-11 冻结，上手步骤见[使用说明](docs/usage.md#把远端项目给已经在跑的-agent-用)。

**要让别的程序驱动 ccnm**——用 `ccnm rpc`：stdio 上的 JSON-RPC 2.0，不开网络端口。契约 `ccnm.machine/1` 已于 2026-09-10 冻结；当前只实现非交互 `print`，结果仅最后 8 KiB、无分页，启动不返回契约中的 busy 码。实现差距、schema、fixture 和 Python 客户端见[协议说明](docs/protocol/README.md)。

要写的是一个**编排项目**（决定谁做什么、验收和重试），先看[执行接口交接](docs/orchestrator-handoff.md)：哪份状态归你、哪份归 ccnm。ccnm 自己不做编排。

## 文档

总入口：[文档导航](docs/README.md)；评估与下一步：[生命周期与职责](docs/project-lifecycle.md)、[2026-09-23 审计](docs/research/2026-09-23-lifecycle-and-docs-audit.md)。

手机远程操作的[实施计划](docs/plan/mobile-access.md)：方案一为 Tailscale + 手机 SSH，方案二为 ttyd + Tailscale Serve 浏览器终端。**尚未部署或做手机验收**，任务与前置门禁见计划，不代表新增已支持入口。

| 用途 | 文档 |
| --- | --- |
| 上手与使用 | [快速开始](docs/getting-started.md) · [使用](docs/usage.md) · [配置](docs/configuration.md) |
| 安全部署与恢复 | [生产安全](docs/production-safety.md) · [运维](docs/operations.md) · [排错](docs/troubleshooting.md) |
| 能力与集成 | [支持矩阵](docs/support-matrix.md) · [架构](docs/architecture.md) · [公开协议](docs/protocol/README.md) |
| 维护与演进 | [执行接口交接](docs/orchestrator-handoff.md) · [开发与发布](docs/development.md) · [研究记录](docs/research/) |

## 这个项目对"没验过"这件事很较真

已经在真机上跑通的、以及明确**没有**验过的，逐条列在[支持矩阵](docs/support-matrix.md)里。最该先知道的三条：

- **不声明任何网络出口边界**。模型的命令能连到哪里没有逐项验证，需要这种保证就由 OS 和网络层自己落实。
- **Linux 只验过 Runtime 那一半**，而且只验过 Debian 13 / x86_64。
- 项目和 Agent 同机（colocated）没有真实验收，因此明确拒绝，不静默降级。

关闭 `web_fetch` 不等于禁止其他 MCP 或命令外发数据；同一工作树的写互斥还要求各入口共用同一个 state 目录。阶段完成、Agent 退出成功与项目验收通过是不同结论。

旧设计文档里偶尔出现的 `home`/`work` 是历史叫法，对应关系见[架构说明](docs/architecture.md#历史术语)。

## 许可证

MIT，见 [LICENSE](LICENSE)。
