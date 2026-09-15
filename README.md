# ccnm

把 AI Coding Agent 和真实项目**放在两台机器上**。

Agent Node 跑官方 Claude Code / Codex CLI，只有它持有 AI 登录凭证；Runtime Node 放源码、Git、构建和工具链，模型的每一次读写和命令执行都落在这里。两者之间是一条持久的 SSH stdio MCP 通道。

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

Codex、Agent Instance、多行 prompt 这些见[快速开始](docs/getting-started.md)和[使用说明](docs/usage.md)。

**ccnm 跟你说话默认用中文**。要英文就加 `--lang en`，或者设 `CCNM_LANG=en`，或者在 config.toml 里写：

```toml
[ui]
lang = "en"
```

只管给人看的那些字。错误码（`CCNM_E_*`）、协议字段、给模型的 MCP 文本，还有 ccnm 自己要去匹配的 git/ssh/tmux 英文输出，都不跟着变——所以照着错误码搜文档、写脚本判断退出码，两种语言下都一样。文档里贴的 `ccnm doctor` 样本是英文那版（`--lang en`）。

一处翻不动：命令行参数写错时，clap 报的 `Usage:` / `error:` 还是英文，它没给任何接口改。

## 跑通之后马上要做的一件事

默认配置里，模型的命令是用**你自己的账号**跑的——那样 ccnm 只帮你分开了机器，没帮你分开权限。真实项目应该在 Runtime Node 建一个专用低权限账号：

```toml
[nodes.runtime]
runtime_user = "ccrun"
```

意思是"Agent 连进来之后，项目命令以 `ccrun` 的身份跑"，不是"你要用 `ccrun` 敲 ccnm"。做法见[生产安全](docs/production-safety.md)。

**如果项目和 Claude 登录本来就在同一台机器、同一个账号下**，那就没有东西可隔离，ccnm 默认会在 MCP 握手之前直接拒绝——那条边界正是它存在的理由。要么建专用账号，要么在 Runtime 侧那个 workspace 上把 `allow_unconfined_exec` 和 `allow_unisolated_credentials` 都写上，**明确接受**模型跑的每条命令都能读到那份登录。开关会在第一次起会话时把风险讲一次，`ccnm doctor` 里那几行永远是 WARN 而不是 OK。代价见[生产安全](docs/production-safety.md#凭据隔离那一条怎么放开代价是什么)。

## 会话里模型能用什么

七个工具，全部落在 Runtime Node 上；模型自己机器上的文件工具是关掉的。

```text
workspace_info   read_file   list_files   search_text
apply_patch      exec_command            read_output
```

## 另外两个入口

**项目在远端，而 Claude Code 已经在你本机跑着**——用 `ccnm mcp bridge <workspace>`，把远端项目作为一组 MCP 工具交给它。权限由 Runtime 侧的 `external_mcp` 决定（默认关）。契约 `ccnm.workspace-mcp/1` 已于 2026-09-11 冻结。

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

已经在真机上跑通的、以及明确**没有**验过的，逐条列在[支持矩阵](docs/support-matrix.md)里。几条最该先知道的：

- **不声明任何网络出口边界**。egress 没有逐项验证，需要这种保证的场景由 OS 和网络层自己落实。
- **Linux 只验过 Runtime 那一半**，而且只验过 Debian 13 / x86_64。Linux 上的 Agent/Controller 没有实现。
- **machine API 的 `interactive` 模式没有实现**。协议冻结的是契约，不是说这些已经补上。
- colocated（项目和 Agent 同机）没有真实验收，因此明确拒绝，不静默降级。

仓库里的大型设计文档属于研发历史，部分旧章节仍会出现 `home`/`work`，那是历史术语；当前公开模型统一以 **Node + Agent / Runtime / Controller** 为准。

## 许可证

MIT，见 [LICENSE](LICENSE)。
