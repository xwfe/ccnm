# ccnm

让 Claude Code / Codex 在你登录着 AI 的那台 Mac 上运行，却在**另一台机器上的真实项目**里读代码、改代码、跑测试。源码和构建留在项目机器上，AI 登录留在 Mac 上，模型的命令由项目机器上一个专用的低权限账号执行。

*ccnm lets Claude Code or Codex run on the Mac where you are logged in, while every file read, edit and command happens on another machine that holds the real project, executed there by a dedicated low-privilege account over a persistent SSH stdio MCP channel. The agent machine must be macOS; the project machine can be macOS or Debian 13 (x86\_64). Docs are in Chinese.*

## 它解决什么问题

项目在一台机器上（家里的 Linux 服务器、公司的台式机），AI 订阅登录在另一台上（你的 MacBook）。常见的两种凑合办法都有代价：

| 做法 | 代价 |
| --- | --- |
| 在项目机器上也登录 Claude / Codex | AI 登录多放一处；模型的命令用你自己的账号跑，你的 SSH 私钥、sudo、别的仓库它都碰得到 |
| 把代码同步到 Mac 上改 | 两份代码要对齐；Mac 上没有服务器的构建环境，测试跑不了或跑得不一样 |

ccnm 的做法是两边都不挪：Claude / Codex 照常在 Mac 上跑，但它自带的读文件、Bash 被关掉，换成 ccnm 提供的一组工具；每次调用经 SSH 送到项目机器执行，结果再送回来。

```text
你的 Mac（Agent Node）                              项目机器（Runtime Node）
Claude Code / Codex  ── SSH 上的 MCP 工具调用 ──▶  ccnm，以执行账号 ccrun 的身份
AI 登录只在这里      ◀─────────── 结果 ──────────  读 / 搜 / 改 / 跑命令
                                                    源码、Git、工具链只在这里
```

## 先认识几个词

| 词 | 说白了 |
| --- | --- |
| **Agent Node** | 跑 Claude Code / Codex 的机器，AI 登录只在这里。目前只能是 macOS |
| **Runtime Node** | 放项目源码、Git 和构建工具链的机器，模型的命令在这里执行。macOS 或 Debian 13 x86_64 |
| **workspace** | 在 Runtime Node 上登记过的一个项目目录，起个名字，比如 `my-project`。命令里写的都是这个名字 |
| **执行账号**（Runtime Executor，建议叫 `ccrun`） | Runtime Node 上专门替模型跑命令的低权限系统账号，要你自己建。建好了，模型就碰不到你的 SSH 私钥、AI 登录和 sudo |
| **Operator** | 你自己的账号，也就是敲 `ccnm` 命令的人。不是执行账号 |
| **Controller** | Agent Node 上的常驻后台（macOS 的 LaunchAgent），负责在 tmux 里拉起 Claude / Codex |
| **会话** | 一次 Claude / Codex 运行，住在 Agent Node 的 tmux 里；关掉终端它还在，随时接回。由 ccnm 起的叫**受管会话**，区别于你自己直接开的 Claude Code |

两台机器是什么都行——笔记本、Mac mini、NAS、云服务器——只要能互相 SSH。

## 一次会话怎么跑

1. 你在 Runtime Node 上敲 `ccnm my-project`。
2. ccnm 经 SSH 让 Agent Node 的 Controller 在 tmux 里起 Claude Code（或 Codex），并把你的终端接进去。
3. 你像平常一样跟它对话。它读文件、搜代码、打补丁、跑 `cargo test`，都经 SSH 回到 Runtime Node，由 `ccrun` 执行。读、搜、改文件被限定在项目目录里。
4. **每条命令执行前都会停下来问你。** Claude 会话在任何权限模式下都问；Codex 会话也问，但终端前的人能在 Codex 的 `/permissions` 里把当前会话切成不问。
5. 想走开就关终端，会话照跑；回来 `ccnm attach my-project`。做完 `ccnm stop my-project`。

## 适合的场景

- **服务器上的项目，Mac 上的 AI**：最主要的用法，就是上面那一套。
- **本机已经开着 Claude Code / Codex，想让它操作远端项目**：`ccnm mcp bridge <workspace>` 把远端项目变成一组 MCP 工具交给它，不用 ccnm 起 Agent。默认关，要在 Runtime 那边的 workspace 配置里打开（[说明](docs/usage.md#把远端项目给已经在跑的-agent-用)）。
- **一句话的活，不想进 tmux**：`ccnm my-project --print "修复 parser 测试"`，结果直接打在你的终端上，中间不问。
- **让脚本或编排器派活**：`ccnm rpc` 是 stdio 上的 JSON-RPC，不开网络端口，有冻结的契约和能直接抄走的 Python 客户端（[协议说明](docs/protocol/README.md)）。
- **在手机上看进度、批命令**：用 PocketShell 这类 SSH 终端 App 连上机器，敲同样的 `ccnm attach`；ccnm 不另做手机端（[说明](docs/usage.md#通过第三方终端使用)）。

## 不适合的场景

- **项目和 AI 在同一台机器上**：直接用 Claude Code / Codex 就好。ccnm 的同机部署没验收过，会明确拒绝。
- **想要多 Agent 协作、任务拆解、自动 review 或重试**：ccnm 只管"让一个 Agent 在远端项目上干活"，编排是另一个项目的事（[交接说明](docs/orchestrator-handoff.md)）。
- **想让源码一点都不离开项目机器**：模型读到的代码片段会经 Agent 发给模型服务。ccnm 分开的是机器和权限，不是数据。
- **需要网络隔离、防止数据外传**：ccnm 不声明任何网络出口边界，要在 Runtime 上自己配防火墙。
- **Agent 想用 Linux 或 Windows**：没实现（Controller 依赖 macOS 的 LaunchAgent）。Windows 当 Runtime 也没实现。

## 上手

前提（每一项怎么准备见[快速开始](docs/getting-started.md)）：

- 两台机器装**同一个版本**的 ccnm；
- 两个方向的非交互 SSH 都通：Runtime 上你的账号 → Agent；Agent → Runtime 上的执行账号；
- Agent Node 上 Claude Code（或 Codex 0.154.0）已登录，装了 `tmux`；
- Runtime Node 上装好 `git`、`ripgrep` 和项目自己的工具链。

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
ccnm doctor my-project      # 只读检查，逐项说哪里没配好；红的先处理
ccnm my-project             # 开始
```

`init` 给哪个参数就说明这台机器是谁：放项目的给 `--agent`（指向对面的 Agent），跑 Claude 的给 `--runtime`。SSH alias 只在定义它的机器上有意义，所以两台各写各的。用 Codex、在一台 Agent 上挂多个实例，见[使用说明](docs/usage.md)。输出默认中文，加 `--lang en` 换英文。

日常命令（前五条两台机器上都能敲，后两条只在 Runtime 上）：

```bash
ccnm my-project                          # 起会话并接上
ccnm attach my-project                   # 接回已有会话（简写 ccnm a）
ccnm ls                                  # 所有项目：在不在跑、跑了多久、工具通不通
ccnm log                                 # 跑过的会话，最新的在前
ccnm stop my-project                     # 结束会话
ccnm my-project --print "修复 parser 测试"   # 一问一答，不进 tmux
ccnm cleanup my-project                  # 预览会话残留，再按提示加 --apply
```

**安装与升级**：去 [Releases](https://github.com/xwfe/ccnm/releases) 下载，macOS 取 `macos-universal`，Linux 取 `linux-x86_64`（只有 Runtime 那一半）。用"新文件 + 改名"放进去：

```bash
tar -xzf ccnm-<版本>-macos-universal.tar.gz
mkdir -p ~/.local/bin
mv ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
```

浏览器下载的包 macOS 会拒绝执行，先 `xattr -d com.apple.quarantine ccnm`；用 `curl` 下载不会这样。

## 该做的和别做的

**该做**

- **真实项目先建执行账号 `ccrun`**，写进 Runtime 配置的 `runtime_user`。默认配置不替你建，命令会以你自己的账号跑——那样只分开了机器，没分开权限。做法见[生产安全](docs/production-safety.md)。
- **改完配置或升级后，先跑 `ccnm doctor <项目>`**，最后一行是"可以用了"再开会话。标"不查"的行是 doctor 本来就不查的（比如网络隔离，ccnm 管不着），读一下说明就行；标"没查"或"失败"的行要处理。v0.11.1 及之前没有"不查"，0 项失败也会写"还不能用"。
- **两台机器一起升级**，装同一个版本。版本对不上时 doctor 和起会话都会报 `CCNM_E_VERSION`。
- **无人值守的活用 `--print`**：一问一答，每次都是你亲手发起的。
- **认真看每次执行命令前的提问**：那是会话里唯一还有人把关的环节。

**别做**

- **别给执行账号任何 SSH 私钥、AI 登录或 sudo。** 它只接受别人连进来，不该能连出去。
- **别把 Agent 上的 AI 登录拷到 Runtime。** ccnm 的前提就是登录只在 Agent 上；项目和 AI 登录在同一个账号下时，ccnm 默认拒绝开会话（[两条出路](docs/getting-started.md#如果项目和-claude-登录在同一个账号下)）。
- **别用 `cp` 覆盖跑过的 ccnm。** Apple Silicon 上会让签名失效，之后每次执行都 `Killed: 9`。用上面的"新文件 + 改名"。
- **别为了省事给常驻会话开 `allow_unattended_exec`。** 那等于会话一路自己跑下去没人看着；要不问就用 `--print`。
- **受管 Codex 会话里别随手切 Full Access / Approve for me。** 切了，这个会话就不再问你。
- **别让同一个项目有两个 state 目录**（比如配了两个不同的 `XDG_STATE_HOME`）。"同一时间只有一个会话能写"靠的是同一把锁，两个目录就是两把互不知道的锁。
- **别开 `codex_exec_server`。** 这条路 2026-09-17 起封存，不再维护。

## 会话里模型能用什么

Runtime 一共 12 个工具，实际给哪些看会话类型和配置（以 `tools/list` 为准）：

```text
workspace_info  read_file      list_files     search_text
apply_patch     exec_command   read_output    load_skill
view_image      read_notebook  stop_command   call_mcp_tool
```

- **skills**：项目自带的（`.claude/skills/` 等）和两台机器上装好的，都经 `load_skill` 交给模型。
- **MCP server**：项目机器上的经 `call_mcp_tool`，和执行命令一样要问；Agent 机器上装的经 `ccnm_agent`，默认只给远端地址的，本机跑的要在 Agent 配置里点名。
- **Agent 那一侧**：受管会话默认开着网页搜索；抓网页、子代理、待办清单要另开（`agent_tools`）。
- **后台命令**能启动、分页读输出、停止，但**活不过那条 SSH 连接**：关终端没事，SSH 断了命令就会被收掉。
- 可选：`exec_sandbox = "codex"` 给 Runtime 上的每条命令再套一层 OS 沙箱。

细节见[使用说明](docs/usage.md)和[配置说明](docs/configuration.md)。

## 现在的状态

| | |
| --- | --- |
| 最新发布 | [v0.11.2](https://github.com/xwfe/ccnm/releases)（2026-10-07），每个版本改了什么写在 Releases 页 |
| 真机上验到哪 | macOS Agent → Debian 13 Runtime（执行账号 `ccrun`）上：Claude 与 Codex 的受管会话、`--print`、外部 MCP、程序接口、安装升级与回退。逐项范围和**没验过的**见[支持矩阵](docs/support-matrix.md) |

已知限制：

- **不声明任何网络出口边界。** 模型的命令能连到哪里没有逐项验证。
- **还在用 v0.11.0 的 Agent**：受管 Codex 会话里有人选过一次 Approve for me，之后所有会话都不再问。升到 v0.11.1 就好；不升的话，会话里 `/status` 写着 `(Approve for me)` 就是它，去掉的办法见[排错手册](docs/troubleshooting.md#受管-codex-会话exec_command-每次都弹或者一次都不弹)。
- **doctor 只看得出 Codex "登录过"**，令牌失效要到会话的第一条消息才知道。
- **脱离进程组的守护进程 ccnm 停不掉**（比如命令里 `setsid` 出去的），写锁会停在"说不清"（unknown），按[运维手册](docs/operations.md#写入-guard-残留)人工收。
- Codex 只认实测过的 **0.154.0**；Claude Code 用 Agent 上装的那个。

## 文档

从[文档导航](docs/README.md)进；参与开发先读 [AGENTS.md](AGENTS.md)。

| 想做什么 | 看哪份 |
| --- | --- |
| 第一次搭起来 | [快速开始](docs/getting-started.md) → [配置](docs/configuration.md) → [使用](docs/usage.md) |
| 接真实项目、建执行账号 | [生产安全](docs/production-safety.md) · [支持矩阵](docs/support-matrix.md) |
| 升级、断线、报错 | [运维](docs/operations.md) · [排错](docs/troubleshooting.md) |
| 写客户端或编排器 | [公开协议](docs/protocol/README.md) · [执行接口交接](docs/orchestrator-handoff.md) |
| 了解内部设计 | [架构](docs/architecture.md) · [开发与发布](docs/development.md) · [计划与进度](docs/plan/README.md) · [研究记录](docs/research/) |

## 许可证

MIT，见 [LICENSE](LICENSE)。
