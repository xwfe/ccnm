# ccnm

把 AI Coding Agent 和真实项目**放在两台机器上**。

Agent Node 跑官方 Claude Code / Codex CLI，只有它持有 AI 登录凭证；Runtime Node 放源码、Git、构建和工具链，模型的每一次读写和命令执行都落在这里。两者之间是一条持久的 SSH stdio MCP 通道（MCP 是 AI 客户端调用外部工具的通用协议）。

**不复制源码，不把 AI 凭证下放到 Runtime Node，也不实现私有模型 API Client。**

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

- **项目在一台机器上，AI 订阅登录在另一台上，两边都不想挪。** 比如源码和工具链在工作机或服务器上，不能（或不想）在那里登录 Claude；而登录着 Claude 的那台 Mac 上又不该出现源码。
- **不想让模型用你自己的账号跑命令。** Runtime 那边可以用一个专用的低权限账号执行，模型读不到你的 SSH 私钥和 AI 登录；还可以再给每条命令套一层 OS 沙箱。
- **项目在远端，而 Claude Code 已经在你本机开着。** 不用 ccnm 启动 Agent，只把远端项目作为一组 MCP 工具交给它（见[另外两个入口](#另外两个入口)）。
- **要让别的程序驱动这一切**（编排器、批处理）：stdio 上的 JSON-RPC，不开网络端口。

**能不能在你的机器上跑**：Agent 那一侧只支持 **macOS**（Controller 是 launchd LaunchAgent，Linux/Windows 没实现）；Runtime 那一侧 **macOS** 和 **Debian 13 / x86_64** 都有真机证据。准确范围见[支持矩阵](docs/support-matrix.md)。

## 装

**两台机器都要装，而且必须是同一个版本**——版本对不上，起会话时会直接拒绝。去 [Releases](https://github.com/xwfe/ccnm/releases) 拿对应的包（macOS 取 `macos-universal`，Linux 取 `linux-x86_64`，后者只有 Runtime 那一半）：

```bash
tar -xzf ccnm-<版本>-macos-universal.tar.gz
mv ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
```

两个会咬人的地方：

- **别用 `cp` 覆盖一个已经跑过的 ccnm。** Apple Silicon 上写进已执行过的 Mach-O 会让代码签名失效，之后每次执行都被 SIGKILL（`Killed: 9`），而老进程还在用老代码跑——现象极其迷惑。上面那句"新文件 + 改名"就是为了避开它。
- **浏览器下载的包带隔离属性**，macOS 直接拒绝执行：`xattr -d com.apple.quarantine ccnm`。用 `curl` 下载不会带。

## 快速开始

前提：两台机器能互相非交互 SSH（不要密码），Agent Node 上官方 CLI 已登录，Runtime Node 上装好项目要用的 toolchain 和 `ripgrep`。

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

## 跑通之后马上要做的一件事

默认配置里，模型的命令是用**你自己的账号**跑的——那样 ccnm 只帮你分开了机器，没帮你分开权限。真实项目应该在 Runtime Node 建一个专用低权限账号：

```toml
[nodes.runtime]
runtime_user = "ccrun"
```

意思是"Agent 连进来之后，项目命令以 `ccrun` 的身份跑"，不是"你要用 `ccrun` 敲 ccnm"。做法见[生产安全](docs/production-safety.md)。

**如果项目和 Claude 登录本来就在同一台机器、同一个账号下**，那就没有东西可隔离，ccnm 默认在 MCP 握手之前直接拒绝，`doctor` 红在 `Claude 凭据` 那一行。这不是配错了；两条出路（建专用账号，或明确接受风险）见[快速开始](docs/getting-started.md#如果项目和-claude-登录在同一个账号下)。

## 会话里模型能用什么

八个工具，全部落在 Runtime Node 上；模型自己机器上的文件工具是关掉的。

```text
workspace_info   read_file   list_files   search_text
apply_patch      exec_command            read_output
load_skill
```

`load_skill` 把项目自带的 skills（`.claude/skills/`、`.claude/commands/`、`.agents/skills/`）交给模型：官方 CLI 靠当前目录发现它们，而 CLI 的当前目录不在项目机器上，所以由 Runtime 这边来找。skill 里的脚本照样在 Runtime 上跑。两台机器上装好的 skills（`~/.claude/skills` 这些）默认也交出去，见[配置说明](docs/configuration.md#machine_skills)。项目那台机器上的 MCP server（项目的 `.mcp.json`、执行账号装的）经 `call_mcp_tool` 交给能写的会话，和 `exec_command` 过同一道门，见[使用说明](docs/usage.md#项目那台机器上的-mcp-server)；Agent 机器上装的经 `ccnm_agent` 下的同名工具交出去，默认只给远端地址的，本机跑的要点名，见[使用说明](docs/usage.md#agent-机器上的-mcp-server)。细节和三处与官方不同的地方见[使用说明](docs/usage.md#项目自带的-skills)。

两个按 workspace 打开的开关，默认都关：

- `exec_sandbox = "codex"`：每条 `exec_command` 包进一层 OS 沙箱——只能写工作区（`.git` 除外）和临时目录，不能连网。代价是 `git commit` 和依赖下载得在沙箱外做，见[配置说明](docs/configuration.md#exec_sandbox)。
- `codex_exec_server = true`：让 Codex 用它自带的执行工具。**2026-09-17 已封存**，代码保留但不再维护，别在新项目上用；原因见[双执行入口方案](docs/plan/runtime-surfaces.md)第 12.0 节。

## 另外两个入口

**项目在远端，而 Claude Code 已经在你本机跑着**——用 `ccnm mcp bridge <workspace>`，把远端项目作为一组 MCP 工具交给它。权限由 Runtime 侧的 `external_mcp` 决定（默认关）。契约 `ccnm.workspace-mcp/1` 已于 2026-09-11 冻结，上手步骤见[使用说明](docs/usage.md#把远端项目给已经在跑的-agent-用)。

**要让别的程序驱动 ccnm**——用 `ccnm rpc`：stdio 上的 JSON-RPC 2.0，不开网络端口。契约 `ccnm.machine/1` 已于 2026-09-10 冻结，schema、fixture 和一个可以直接抄走的 Python 客户端见[协议说明](docs/protocol/README.md)。

要写的是一个**编排项目**（决定谁做什么、验收和重试），先看[执行接口交接](docs/orchestrator-handoff.md)：哪份状态归你、哪份归 ccnm。ccnm 自己不做编排。

## 文档

- [快速开始](docs/getting-started.md)：安装、SSH 前提、首次配置、Controller
- [使用说明](docs/usage.md)：run、attach、status、stop、result、prompt
- [配置说明](docs/configuration.md)：`nodes`、workspace、双向 SSH 字段
- [故障排查](docs/troubleshooting.md)：真撞过的问题，按现象找
- [生产安全](docs/production-safety.md)：`ccrun`、ACL、凭证、sudo、网络出口边界
- [运维](docs/operations.md)：安装、回退、配置迁移、状态清理、故障恢复
- [支持矩阵](docs/support-matrix.md)：Provider、版本、topology、验收级别与明确拒绝项
- [架构说明](docs/architecture.md)：Node / Agent / Runtime / Controller 与 SSH stdio MCP
- [公开协议](docs/protocol/README.md)：给外部程序的 machine API 契约与客户端示例
- [执行接口交接](docs/orchestrator-handoff.md)：独立 Orchestrator 的状态归属边界
- [开发与发布](docs/development.md)：测试、CI、release、mutation test
- [研究记录](docs/research/)：实现调研和历史测量数据

## 这个项目对"没验过"这件事很较真

已经在真机上跑通的、以及明确**没有**验过的，逐条列在[支持矩阵](docs/support-matrix.md)里。最该先知道的三条：

- **不声明任何网络出口边界**。模型的命令能连到哪里没有逐项验证，需要这种保证就由 OS 和网络层自己落实。
- **Linux 只验过 Runtime 那一半**，而且只验过 Debian 13 / x86_64。
- 项目和 Agent 同机（colocated）没有真实验收，因此明确拒绝，不静默降级。

旧设计文档里偶尔出现的 `home`/`work` 是历史叫法，对应关系见[架构说明](docs/architecture.md#历史术语)。

## 许可证

MIT，见 [LICENSE](LICENSE)。
