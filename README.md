# ccnm

把 AI Coding Agent 和真实项目**放在两台机器上**。

Agent Node 跑官方 Claude Code / Codex CLI，AI 登录只在这台；Runtime Node 放源码、Git、构建和工具链，模型的读、搜、改、跑命令经一条持久的 SSH stdio MCP 通道在这里执行，而且可以用一个专用的低权限账号执行。

```text
Runtime Node                              Agent Node
┌────────────────────────┐                ┌─────────────────────────┐
│ workspace / Git        │                │ Claude Code / Codex CLI │
│ build / test / tools   │◀── SSH stdio ─▶│ login + controller/tmux │
│ ccnm MCP runtime       │      MCP       │ session supervisor      │
└────────────────────────┘                └─────────────────────────┘
```

*Keep the AI coding agent and the real project runtime on separate machines. The agent runs on an **Agent Node** (macOS only) and reaches the project on a **Runtime Node** (macOS or Debian 13 / x86\_64) over a persistent SSH stdio MCP transport. Docs are in Chinese.*

**它不做的事**：不整库同步；不把 AI 登录下放到 Runtime；不实现私有模型客户端；不做任务规划、审查、发布或多 Agent 调度（那是编排项目的事，见[执行接口交接](docs/orchestrator-handoff.md)）。模型需要的源码片段仍会经工具结果进入 Agent 和模型服务——分开机器不等于"源码永不离开 Runtime"。

## 现在是什么状态

| | |
| --- | --- |
| 最新发布 | [v0.11.0](https://github.com/xwfe/ccnm/releases)（2026-10-07） |
| v0.11.0 比 v0.10.1 多了什么 | **受管 Codex 交互会话执行命令前问你**，和 Claude 一样每条 `exec_command` 都问；不同的是会话里的人能用 `/permissions` 切到 Full Access 关掉它（P71）。Agent 上管运行的监督进程丢了，几秒内就是 `unknown`，不再等满超时；原始输出在第一次读之前丢了，如实标 `agent_refused`，不再说"空且完整"（P68）。doctor 对旧 Agent 先报版本不一致，转述丢的字段不再算到 Runtime 头上，版本行写实际节点名；`Command approval` 一行按 Provider 说实话（P69、P70）。`ccnm workspace add` 按配置里的节点名写，好几个候选时用新参数 `--agent-node` 指定（P69）。内部协议号没变（仍是 10），但两端版本号不同照样互相拒绝：**两台机器要一起升** |
| 真机验收 | P62（macOS Agent → Debian 13 Runtime）**2026-10-04 完成**：两个 Provider 的受管闭环、外部 MCP、Machine API、候选包安装升级回退都在真机上过了（[第一轮](docs/research/2026-09-30-p62-real-machine.md)、[续跑](docs/research/2026-10-04-p62-resume-release.md)），续跑查出的"监督进程丢了""原始输出丢了"两项由 P68 修好后真机复验通过（[P62.4 复验](docs/research/2026-10-04-p62-4-recheck.md)）。P69、P70 只有离线测试与零额度实测；P71（Codex 会话的命令审批）2026-10-07 在 macOS Agent → Debian 13 Runtime（`ccrun`）上用真实模型复验：问、放行、取消都对，另查出 F27（[P71 真机复验](docs/research/2026-10-07-p71-real-machine-recheck.md)） |

每一项能力验到了哪一步、明确**没**验过什么，逐条在[支持矩阵](docs/support-matrix.md)里。

## 什么时候用

- **项目在一台机器上，AI 订阅登录在另一台上，两边都不想挪。** 比如源码和工具链在服务器上，不想在那里登录 Claude；登录着 Claude 的 Mac 上又不该出现源码。
- **不想让模型用你自己的账号跑命令。** Runtime 那边用专用低权限账号执行，模型碰不到你的 SSH 私钥和 AI 登录；还可以给每条命令再套一层 OS 沙箱。
- **Claude Code / Codex 已经在你本机开着，项目在远端。** 不用 ccnm 启动 Agent，只把远端项目作为一组 MCP 工具交给它。
- **要让别的程序驱动这一切**（编排器、批处理）：stdio 上的 JSON-RPC，不开网络端口。

## 能在哪跑

| 角色 | macOS | Linux | Windows |
| --- | --- | --- | --- |
| Agent Node（跑 Claude/Codex） | 支持 | 没实现（Controller 是 launchd LaunchAgent） | 没实现 |
| Runtime Node（放项目） | 支持 | 只验过 Debian 13 / x86_64 | 没实现 |

Codex 受管会话只认实测过的 Codex **0.154.0**；Claude Code 用 Agent 上装的那个。

## 安装

两台机器装**同一个构建**。去 [Releases](https://github.com/xwfe/ccnm/releases) 拿包：macOS 取 `macos-universal`，Linux 取 `linux-x86_64`（只有 Runtime 那一半）。

```bash
tar -xzf ccnm-<版本>-macos-universal.tar.gz
mkdir -p ~/.local/bin
mv ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
```

三个会咬人的地方：

- **别用 `cp` 覆盖跑过的 ccnm。** Apple Silicon 上写进已执行过的 Mach-O 会让代码签名失效，之后每次执行都 `Killed: 9`，而老进程还在用老代码跑。上面"新文件 + 改名"就是为了避开它。
- **浏览器下载的包带隔离属性**，macOS 拒绝执行：`xattr -d com.apple.quarantine ccnm`。用 `curl` 下载不会带。
- **版本号一样不代表是同一个构建。** 两次发版之间从 main 编的构建都叫上一个发布的号（比如 v0.11.0 之后自己编的也叫 0.11.0）。v0.10.0 起 doctor 在版本号相同时再比内部协议最高号，对不上就报 `not the same build`——但只有新的那一端会说，旧构建的 doctor 照样全绿，起会话时才报 `message is not valid for protocol 1`。所以两台都跑一遍 doctor，以新的那台为准。

## 快速开始

前提：非交互 SSH 两个方向都通（Runtime 上敲命令的人 → Agent；Agent → Runtime 的执行账号，执行账号不需要任何出站私钥）；Agent 上官方 CLI 已登录；Runtime 上装好 `git`、`ripgrep` 和项目工具链。

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

`init` 给哪个 flag 就说明这台机器是谁：放项目的给 `--agent`，跑 Claude 的给 `--runtime`，两个一起给会被拒。SSH alias 只在定义它的机器上有意义，所以两台各写各的。

日常命令（两台机器上都能敲）：

```bash
ccnm my-project                          # 起会话并接上
ccnm attach my-project                   # 接回已有会话（简写 ccnm a）
ccnm ls                                  # 所有项目：在不在跑、跑了多久、工具通不通
ccnm status                              # 同上，更细（简写 ccnm st）
ccnm log                                 # 跑过的会话，最新的在前
ccnm stop my-project
ccnm my-project --print "修复 parser 测试"   # 一问一答，不进 tmux
ccnm cleanup my-project                  # 在 Runtime 上：预览会话残留，再按提示加 --apply
```

每一步的完整说明和 doctor 红了怎么办见[快速开始](docs/getting-started.md)；Codex、Agent Instance、多行 prompt 见[使用说明](docs/usage.md)。默认说中文，要英文加 `--lang en`。

## 真实项目之前：给模型一个专用账号

默认配置没有替你建隔离账号。模型命令如果以**你自己的账号**跑，ccnm 只分开了机器，没分开权限。真实项目先在 Runtime Node 建一个低权限账号，并写进配置：

```toml
[nodes.runtime]
runtime_user = "ccrun"
```

意思是"Agent 连进来之后，项目命令以 `ccrun` 的身份跑"，不是"你要用 `ccrun` 敲 ccnm"。做法见[生产安全](docs/production-safety.md)。

- **Linux 上项目可以放在执行账号的家目录里**，哪怕你（Operator）进不去（Debian 12 起家目录默认 0700）：登记时写绝对路径，`ccnm doctor` 的 `Runtime 上的项目` 一行会是"没查"，执行账号的回答在 `workspace 根目录` 那一行。v0.9.0 还会把它误报成"不是这台机器上的目录"，那一版上把项目放到 `/srv/...` 这类你能进入父目录的地方，见[排错手册](docs/troubleshooting.md#linux-上-ccnm-run-报-workspace-root--is-not-a-directory-on-this-machine目录明明在)。
- **项目和 Claude 登录本来就在同一个账号下**时没有东西可隔离，ccnm 默认在 MCP 握手之前就拒绝，doctor 红在 `Claude 凭据` 那一行。两条出路见[快速开始](docs/getting-started.md#如果项目和-claude-登录在同一个账号下)。

## 会话里模型能用什么

Runtime 有 12 个工具，实际给哪些取决于读写模式和配置，以 `tools/list` 为准：外部 `read` 模式 7 个只读工具，coding 会话 11 个，Runtime 上有可转接的 MCP server 时加 `call_mcp_tool`。

```text
workspace_info  read_file      list_files     search_text
apply_patch     exec_command   read_output    load_skill
view_image      read_notebook  stop_command   call_mcp_tool
```

- **skills**：项目自带的（`.claude/skills/` 等）和两台机器上装好的，经 `load_skill` 交给模型——官方 CLI 的当前目录不在项目机器上，自己发现不了。见[使用说明](docs/usage.md#项目自带的-skills)。
- **MCP server**：项目那台机器上的经 `call_mcp_tool`，和 `exec_command` 过同一道门；Agent 机器上装的经 `ccnm_agent` 下的同名工具，默认只给远端地址，本机跑的要在 Agent 配置里点名——那是额外信任，不是只读白名单。见[使用说明](docs/usage.md#项目那台机器上的-mcp-server)。
- **Agent 那一侧不是一无所有。** 官方 CLI 的原生 Read/Bash 被关掉，但 `ccnm_agent` 的工具以 Agent 账号运行；受管会话默认还开着 `web_search` 和 `mcp_servers`，抓网页、子代理、待办清单要另开（`agent_tools`，见[配置说明](docs/configuration.md)）。
- **后台命令**能启动、分页读、停止，但**活不过 MCP 连接**。关掉终端只是离开 tmux；MCP 断线才会收掉命令。
- **两个按 workspace 打开的开关**（默认关）：`exec_sandbox = "codex"` 把 Runtime 命令包进 OS 沙箱（限制工作区外写入、`.git` 写入和网络，不覆盖 Agent 上的 MCP，见[配置说明](docs/configuration.md#exec_sandbox)）；`codex_exec_server = true` 已于 2026-09-17 **封存**，新项目别用。

## 另外两个入口

- **本机已经开着 Claude Code / Codex**：`ccnm mcp bridge <workspace>` 把远端项目作为一组 MCP 工具交给它，权限由 Runtime 侧的 `external_mcp` 决定（默认关）。契约 `ccnm.workspace-mcp/1` 已冻结，见[使用说明](docs/usage.md#把远端项目给已经在跑的-agent-用)。
- **让程序驱动 ccnm**：`ccnm rpc`，stdio 上的 JSON-RPC 2.0。契约 `ccnm.machine/1` 已冻结；当前只有非交互 `print`，结果可倒序分页读回（每个流保留最后 32 MiB），启动前问一次写锁、被占回 busy（只是观察，不是预留）。schema、fixture 和可以直接抄走的 Python 客户端见[协议说明](docs/protocol/README.md)。

## 已知限制

- **不声明任何网络出口边界。** 模型的命令能连到哪里没有逐项验证；关掉 `web_fetch` 也不等于禁止 MCP 或命令外发数据。
- **写互斥要求各入口用同一个 state 目录。** 同一棵树配两个 `XDG_STATE_HOME` 就是两把互不知晓的锁。
- **离开进程组的后代够不着。** Runtime MCP server 派生的 `setsid` / 守护进程、以及 `mcp-serve` 被 `kill -9` 后留下的后台命令，ccnm 停不掉；写锁会因此保持 unknown，按[运维手册](docs/operations.md#写入-guard-残留)人工收。
- **项目和 Agent 同机（colocated）没有真实验收**，明确拒绝，不静默降级。
- **受管 Codex 会话的命令审批，会话里的人能自己关掉。** P71 起 Codex 会话和 Claude 一样，每条 `exec_command` 前都问你；但在 Codex 里用 `/permissions` 切到 Full Access 就不再问，Claude 那边任何权限模式都关不掉。P71 之前的构建对 Codex 一律不问（[配置说明](docs/configuration.md#allow_unattended_exec)）。v0.11.0 及之前的 Agent 上，选过一次 Approve for me 会被 Codex 记进 profile，之后的受管会话都不再问（F27，P72 已修、未发版；[怎么去掉](docs/troubleshooting.md#受管-codex-会话exec_command-每次都弹或者一次都不弹)）。
- **doctor 验不了 Codex 的令牌还有没有效**，只能看到"登录过"（P65 起那一行自己会说）；令牌被吊销要到会话的第一条消息才知道。P62 续跑查出的 F20–F24 列在[续跑记录](docs/research/2026-10-04-p62-resume-release.md)第 7 节，现象和绕法在[排错手册](docs/troubleshooting.md)。

阶段完成、Agent 退出成功和项目验收通过是三个不同的结论。

## 文档

从[文档导航](docs/README.md)进；接手开发先读 [AGENTS.md](AGENTS.md)。

| 用途 | 文档 |
| --- | --- |
| 上手与使用 | [快速开始](docs/getting-started.md) · [使用](docs/usage.md) · [配置](docs/configuration.md) |
| 安全部署与恢复 | [生产安全](docs/production-safety.md) · [运维](docs/operations.md) · [排错](docs/troubleshooting.md) |
| 能力与集成 | [支持矩阵](docs/support-matrix.md) · [架构](docs/architecture.md) · [公开协议](docs/protocol/README.md) |
| 维护与演进 | [生命周期与职责](docs/project-lifecycle.md) · [执行接口交接](docs/orchestrator-handoff.md) · [开发与发布](docs/development.md) · [计划与进度](docs/plan/README.md) · [研究记录](docs/research/) |

手机或浏览器接入直接用 PocketShell 这类第三方终端连到机器上敲同样的命令，ccnm 不另做移动端（[使用说明](docs/usage.md#通过第三方终端使用)）。旧设计文档里的 `home`/`work` 是历史叫法，对应关系见[架构说明](docs/architecture.md#历史术语)。

## 许可证

MIT，见 [LICENSE](LICENSE)。
