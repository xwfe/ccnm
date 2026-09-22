# 使用说明

`ccnm doctor <workspace>` 的基础链路确认正常后，同一个 workspace 可以从 Runtime Node 或 Agent Node 发起。

## 说什么语言

默认中文。要英文，三条路，从先到后谁先给算谁：

```bash
ccnm --lang en doctor my-project    # 只这一次
export CCNM_LANG=en                 # 这个 shell 里都用
```

```toml
# config.toml，这台机器长期用英文
[ui]
lang = "en"
```

**只管给人看的字。** 这些一律不跟着变，两种语言下一模一样：

- 错误码 `CCNM_E_*` 和退出码——脚本按它们判断，翻了就没法判断了
- 协议字段、`ccnm rpc` 的 JSON、MCP 工具名和参数名
- 给模型看的 MCP 文本（工具说明、报错正文）——那是模型据以改做法的指令，已经按英文验过
- 命令、配置键名、路径、SSH alias

`--lang` 也管 `--help`。但**不读系统的 `LANG`/`LC_ALL`**：ccnm 要去匹配 git、ssh、tmux、Codex 的英文输出（比如 git 的 `dubious ownership`、ssh 的 `permission denied`），拿 locale 当语言开关会让这些匹配悄悄失效，而且不报错。

两台机器各说各的：语言不跨 SSH 传，`doctor` 表里那些从 Runtime 传回来的 detail 仍是英文。

翻不动的一处：参数写错时 clap 报的 `Usage:` / `error:` 还是英文。

## 交互式会话

```bash
ccnm my-project
# 等价完整写法
ccnm run my-project
```

Agent session 本身运行在 Agent Node；它对项目的读取、搜索、修改和命令执行通过 MCP 落到 Runtime Node。

只启动、不 attach：

```bash
ccnm my-project --detached
```

之后重新接入：

```bash
ccnm attach my-project
```

SSH 断开、终端关闭或笔记本暂时离线，不等于结束 session。只要 Agent Node 上的 tmux/session 还活着，就可以重新 attach。

### 会话在 tmux 里，所以滚屏和复制跟你平时不一样

交互式会话跑在 Agent Node 的 tmux 里，你的终端只看到一帧画面。**滚上去的内容不在你本地终端的回滚里**，在对面 tmux 的缓冲里——这就是为什么有人只能截图。

ccnm 给自己的 tmux server（`tmux -L ccnm`，跟你自己开的 tmux 完全无关）设了这些，让它像个正常终端：

| 设置 | tmux 默认 | ccnm | 为什么 |
| --- | --- | --- | --- |
| `history-limit` | 2000 行 | 50000 | 一次 `cargo test` 就能冲掉 2000 行，冲掉就再也读不到了 |
| `mouse` | off | on | 滚轮能滚历史；拖选即复制 |
| `set-clipboard` | external | on | tmux 里选中的东西直接进**你坐的那台机器**的剪贴板（走 OSC 52，iTerm2 / Ghostty / WezTerm 支持，Terminal.app 不支持） |

- 想用终端自己的原生选择（跨折行那种），macOS 上**按住 Option 拖选**，绕过 tmux 的鼠标捕获。
- 这些值在 tmux server 启动时写入，**之后读一遍你的 `~/.tmux.conf`**——你自己写了什么就以你的为准。
- 只想临时改回去：`ssh <agent> 'tmux -L ccnm set -g mouse off'`，改到 server 重启为止。

**别在受管会话里用 Claude Code 的"后台"功能**：它会把会话 fork 成第二个进程，第二个进程拿不到 Runtime 工具（单写 guard 会拒），结果是一个什么都干不了的空会话。要离开就 detach，回来用 `ccnm attach`。详见[故障排查](troubleshooting.md#在受管会话里按了-claude-code-的后台工具全没了)。

输出要能随手复制、不想进 tmux 的话，用[非交互 `--print`](#非交互---print)：结果直接打在你本机终端里。

## 选择 Agent Instance

使用 `agent = { node = "worker", instance = "claude-main" }` 的 workspace 会默认选择该 instance。同一个 Agent Node 上可显式覆盖：

```bash
ccnm doctor my-project --agent codex-main
ccnm run my-project --agent codex-main
```

`--agent` 是受限 instance id，不是 Provider、Node、路径或官方 CLI 参数。legacy `agent_node` workspace 不接受它。Provider/profile 只由 Agent Node 本机配置解析；Codex 的专用 HOME 不会发给 Runtime。

一个正在运行的 session 固定绑定 workspace、root 和完整 Agent identity。换 Provider 或 instance 不会复用/替换旧 session；先精确停止旧 session。

## Prompt

单行开场白：

```bash
ccnm my-project "修复 parser 测试失败"
```

包含多行、引号或其他不适合放进远端 shell argv 的内容时，用 stdin：

```bash
ccnm my-project --prompt-stdin <<'EOF'
重构 parser。
保持 "strict" 行为不变。
先跑聚焦测试，再跑完整测试。
EOF
```

自由文本不会拼进远端 SSH 命令行，而是通过 stdin 传递，避免被远端 shell 重新解析。

## 查看状态和结束会话

### 一次看全部项目

```bash
ccnm ls          # 一项目一行
ccnm status      # 不带项目名：每个项目一块，细到进程
ccnm log         # 会话历史，默认最近 20 条；ccnm log gld -n 5 只看 gld 的 5 条
```

`ccnm ls` 长这样：

```text
项目   状态               已运行       工具
ccnm   没在跑
gld    运行中 · 1 个终端  59 分钟      断了
xdo    运行中 · 1 个终端  1 小时 2 分  通

! gld：工具断了。在 Claude 里 /mcp → ccnm → Reconnect
详情：ccnm status
```

在 **Runtime Node** 上跑时，`ccnm status` 除了 Agent Node 上的会话，还会列出本机的 `ccnm internal mcp-serve` 进程，并跟 Agent 那边对一遍：

| 它说 | 意思 |
| --- | --- |
| 服务着上面这个会话 | 正常 |
| --print 运行 | 一次 `--print` 正在跑，tmux 里看不到它，但它占着写锁 |
| 诊断用 / 外部 MCP 客户端 | `ccnm doctor`、`mcp probe`，或 `ccnm mcp bridge` 连进来的 |
| **孤儿** | Agent 那边这个会话已经结束，这个进程还占着写锁，新会话会被它挡住。后面会给出结束它的命令 |
| 跟 Agent 对不上 | 问不到 Agent，或者 Agent 没有这个会话的记录——说不清，不当孤儿处理 |

孤儿是怎么来的、为什么新版本基本不会再有，见[故障排查](troubleshooting.md#合上笔记本睡一觉第二天某个项目的工具连不上)。

在 **Agent Node** 上跑时，只看得到本机的 tmux 会话和会话记录：项目列表和 `mcp-serve` 都在 Runtime 那边，ccnm 不会为了看状态反过来连 Runtime。

`ccnm log` 的"开始"是**敲命令这台机器的本地时间**。它要求两台机器的 ccnm 都认识 `agent-history`；Agent Node 上还是旧版本时会报 `CCNM_E_VERSION`，让你把两台装成同一版本。

### 简写

| 完整 | 简写 |
| --- | --- |
| `ccnm attach` | `ccnm a` |
| `ccnm status` | `ccnm st` |
| `ccnm list` | `ccnm ls` |
| `ccnm log` | `ccnm logs` |
| `ccnm doctor` | `ccnm dr` |
| `ccnm result` | `ccnm res` |
| `ccnm workspace` / `list` / `remove` | `ccnm ws` / `ls` / `rm` |

`ccnm <名字>` 等于 `ccnm run <名字>`，但子命令和简写优先：workspace 如果恰好叫 `ls`、`st`、`a` 这类名字，就只能写全 `ccnm run ls`。

### 单个项目

```bash
ccnm status my-project
ccnm status my-project --all
ccnm stop my-project
```

精确寻址使用 ccnm session id：

```bash
ccnm status my-project --agent codex-main --session <ccnm-session-id>
ccnm attach my-project --agent codex-main --session <ccnm-session-id>
ccnm stop my-project --agent codex-main --session <ccnm-session-id>
```

ccnm session id 与 Claude/Codex 自己的 thread/resume id 是两类值，不能互换。精确操作会校验 session 的 workspace 和 Agent identity。状态区分 `starting`、`running`、`completed`、`failed`、`stopping`、`unknown`；不能证明进程已经结束时不会猜成 failed。

精确停止 print session 时，即使已有结果，也会检查已记录的 supervisor/Agent 进程组；组仍存在、PID 记录损坏或进程查询失败会返回 `NotReady`，不对历史 PID 发信号，也不改写结果。

session 建立后，Agent Node 上的 `attach/status/result/stop` 继续本机管理记录，不依赖重新解析 workspace root。Runtime Node 发起的命令仍由 Runtime 默认选择或 `--agent` 选择约束。

## 非交互 `--print`

当前应在定义 workspace 的一侧执行，通常就是 Runtime Node：

```bash
ccnm run my-project --print "找出问题，修复，然后运行测试"
```

如果执行时 SSH 断开，完成后的结果仍然保存在 Agent Node。读取最近一次结果：

```bash
ccnm result my-project
```

也可以指定 session id：

```bash
ccnm result my-project --session <id>
```

Agent Instance 建议同时带 `--agent <instance-id>`；不带 session 的“最近一次”只保留给人类兼容使用，不是稳定机器接口。

### 不想被打断：先想想 `--print`

交互式会话每次执行命令都会停下来问你一次，而且**任何权限模式都关不掉**（[为什么](troubleshooting.md#开了-bypasspermissionsexec_command-还是每次都问)）。被问烦了有两条路，先想想哪条更合适：

| | `--print` | `allow_unattended_exec = true` |
| --- | --- | --- |
| 形态 | 一问一答，跑完就结束 | 常驻会话，一直不问 |
| 输出在哪 | **直接打在你本机终端**，随手复制 | 在对面 tmux 里，要滚要选 |
| 中间有没有人 | 没有，但每次是你亲手发起的 | 没有，而且会话会自己连着做下去 |
| 适合 | 明确的一件事：跑测试、查状态、改一处 | 长时间结对，你在旁边看着 |

大部分"它老问我"的场景其实是第一种——你想让它做一件明确的事，不需要一个常驻会话：

```bash
ccnm my-project --print "跑 cargo test，把失败的贴给我"
ccnm my-project --print "把 README 里的版本号改成 0.5.0，然后 git diff 给我看"
```

**长任务不怕断线**：结果写在 Agent Node 的会话目录里，ssh 断了也还在，用 `ccnm result` 捞。默认 600 秒超时，长的用 `--timeout`：

```bash
ccnm my-project --print "跑完整测试套件" --timeout 1800
ccnm result my-project          # 断线之后回来捞
```

多行、带引号的 prompt 走 stdin，见上面的 [Prompt](#prompt) 一节。

真的需要常驻会话又不想被问，再去开 [`allow_unattended_exec`](configuration.md#allow_unattended_exec)——那是把最后一个有人在场的环节去掉，`ccnm doctor` 会一直提醒你它开着。

## MCP 诊断

本地 Runtime 诊断：

```bash
ccnm mcp probe my-project --local --calls 100
```

`--local` 仅适用于 legacy workspace；instance workspace 请使用不带 `--local` 的远程 probe，以便由 Agent 解析身份。probe 会参与 Runtime 写 guard，因此已有 writer 时会拒绝，不应为诊断清理活动锁。

它会启动一个真实 `ccnm internal mcp-serve` 子进程，证明多次 MCP 调用由同一个持久 runtime process 处理，而不是每个工具调用都重新启动一次进程。

真实跨 Node 链路由：

```bash
ccnm doctor my-project
```

进行验证。

### Codex 原生链那一行

workspace 写了 [`codex_exec_server = true`](configuration.md#codex_exec_server)、选中的 Agent 又是 Codex 时，doctor 表里 `远端 MCP 握手` 下面那行 `Codex 原生链`（英文 `Codex exec-server`）才会给结论：Agent 替你做一次 `ccnm run` 起 Codex 之前的**同一个**预检——经 ssh 在 Runtime 上开一个空的 `exec-serve` 会话，stdin 立刻关掉。Runtime 侧、Agent 侧跑 doctor 都一样。（这条链 2026-09-17 起封存，原因在[配置说明](configuration.md#codex_exec_server)；这一行的行为不变。）

| 状态 | 说明什么 |
| --- | --- |
| 正常 | Runtime 认这个 workspace 走原生链、审计放行命令执行、`codex_bin` 是 Codex 0.154.0、exec-server 起得来也停得掉，写锁取到又放回 |
| 失败 | 带 Runtime 自己报的码：`CCNM_E_CONFIG`（没配 `codex_bin`）、`CCNM_E_VERSION`（Codex 版本不对）、`CCNM_E_POLICY`（审计不放行，或写锁被占），排查见[出错了怎么办](troubleshooting.md#doctor-里-codex-原生链那一行失败) |
| 没查 | 没开 `codex_exec_server`、Agent 不是 Codex（Claude 照旧走 MCP 七工具），或前面的 SSH 已经失败——detail 写着是哪种 |

所以**没开这条链的 workspace 表里也有这一行**，是 `没查`：结论行的"N 项没查"比以前多 1，退出码不变（本来就有两行固定的"没查"，结论一直是还不能用）。

两件事要知道：

- **它和 `远端 MCP 握手` 一样要取一次写锁再放掉。**这个 workspace 正有会话在写（受管会话、外部 MCP 的 coding 会话、原生链会话都算），两行都会报 `workspace write guard is busy`。那说明有人在用，不是链路坏了；别为了让 doctor 变绿去清锁。
- **正常不代表 Linux 沙箱能用。**空会话一条命令都不跑，而 Codex 在 Linux 上靠 bubblewrap 和 user namespace 建沙箱，缺了要到第一条命令才报错（前提见[运维手册](operations.md#runtime-node-的前置条件与项目工具链)）。doctor 不去猜它：bwrap 的查找位置和 user namespace 的限制都读不准，读 sysctl 会在容器里报通过而沙箱实际起不来，理由记在 [ROADMAP P27.3](plan/ROADMAP.md)。

## 给程序用的接口

上面这些命令是给人敲的。要让别的程序驱动 ccnm，用 machine API：

```bash
ccnm rpc
```

它在 stdin/stdout 上说 JSON-RPC 2.0，一行一条消息，不开网络端口。谁能启动这个进程，谁就有这套 API 的全部权限。

现在能用的是 `print` 模式的完整一轮：握手、列 instance、启动、查状态、取结果、停止。Claude 与 Codex 各在真机上跑通过一次，协议 `ccnm.machine/1` **已于 2026-09-10 冻结**：往后加字段、加方法可以，删字段和改语义要升版本。方法、参数、错误码和 fixture 见[协议说明](protocol/README.md)。

## 把远端项目给已经在跑的 Agent 用

上面那条是 ccnm 帮你启动 Agent。反过来：你的 Claude Code 或 Codex 已经开着，只是项目在另一台机器上——那就把这个进程配进它的 MCP server 列表：

```bash
ccnm mcp bridge my-project --mode read
```

它不自己实现 MCP，而是 `exec` 成一条到 Runtime 的 ssh，真正回答工具调用的还是那台机器上的同一个 server。**默认什么都打不开**：Runtime 侧要先给那个 workspace 写 `external_mcp = "read"`（或 `coding`），见[配置说明](configuration.md)。`read` 给四个只读工具，`coding` 给七个并持有工作树的写入互斥锁；请求高于配置会直接拒绝启动，不降级。

契约 `ccnm.workspace-mcp/1` **已于 2026-09-11 冻结**。验收范围：一台 Debian 13 / x86_64 的 Runtime、一棵中型 Rust 项目、官方 Claude Code 2.1.268 的 `-p` 模式各一次真机（[dogfood 记录](research/p12-real-project-2026-09-11.md)）；Codex 当 Host、交互式 UI、别的发行版都没验，**egress 不作保证**。

三条上手就会遇到的：

- **给 Claude Code 配这个 server 时加一行 `"alwaysLoad": true`。**不加的话它会被延迟加载——模型每个任务得先花一个回合调 `ToolSearch`，才拿得到 ccnm 的工具。配置形状、实测数字和它的代价（首轮请求前会等 bridge 连上 Runtime）见[协议文档](protocol/remote-workspace-mcp-v1.md#alwaysload-是干什么的)。
- **Runtime 上要有 `ripgrep`**，`search_text` 调它；项目要编译测试，那套工具链也得在 Runtime 上，而且要装在**执行身份自己的 home** 里、写进非交互 ssh 看得见的 PATH——照默认装 rustup 会得到"cargo 没装"的错，原因和做法见[运维手册](operations.md#runtime-node-的前置条件与项目工具链)。
- **bridge 起不来时 Host 那边可能只显示 `Connection closed`**（Claude Code 就是这样）：`CCNM_E_*` 那行诊断留在 Host 丢掉的 stderr 上。在终端里手工跑一遍同一条 `ccnm mcp bridge …` 就能看到真正的原因。

完整契约见 [Remote Workspace MCP](protocol/remote-workspace-mcp-v1.md)。

## 当前模型能做什么

核心 MCP 工具：

```text
workspace_info
read_file
list_files
search_text
apply_patch
exec_command
read_output
load_skill
view_image
read_notebook
stop_command
```

主要行为：

- `read_file`、`list_files`、`search_text` 都受 workspace 路径边界约束；
- `search_text` 默认返回匹配行，也能只列文件（`output_mode: "files_with_matches"`）、按文件计数（`"count"`）、跨行匹配（`multiline`）、按文件类型过滤（`type: "rust"`）；dotfile 要写 `include_hidden: true` 才搜，`.git` 永远不搜；
- `apply_patch` 是结构化写入路径：`add` 新建，`update` 精确替换片段，`write` 整体替换一个已存在的文件，`edit_notebook` 替换、插入、删除 Jupyter cell，`delete`、`move`；改已有文件都要带 `read_file`（或 `read_notebook`）给的版本号，一次调用里的所有文件要么全改、要么都不改；
- `read_notebook` 按 cell 显示 Jupyter notebook：每个 cell 的 id、类型、源码，代码 cell 后面跟着输出，输出里的图作为图片。改 cell 用 `apply_patch` 的 `edit_notebook`，参数和 Claude Code 的 NotebookEdit 同名，见[协议第 5.4 节](protocol/remote-workspace-mcp-v1.md#54-read_notebook-与-edit_notebookjupyter-notebook-按-cell-读写p40-新增)。不执行 cell——要跑用 `exec_command` 调 `jupyter nbconvert --execute`；
- `exec_command` 二选一：`cmd` 给程序和参数（argv，不经过 shell），`shell` 给一行命令、用 `bash -c` 跑（Runtime 上要有 bash，没有会报 `CCNM_E_DEPENDENCY`）。两种写法的权限和确认完全一样，它本质上就是命令执行能力；
- 大输出由 `read_output` 分页读取，避免一次把全部输出塞进模型上下文；
- 要一直跑的命令（dev server、watch、很长的构建）用 `exec_command` 加 `run_in_background: true`：马上拿到 `output_ref`，命令在 Runtime 上接着跑。`read_output` 读它到目前为止的输出，加 `wait_ms` 等它结束；`stop_command` 停掉它。**后台命令活不过会话**：会话结束、断开、在 Claude Code 里 `/mcp` 重连，都会停掉这个会话起的所有命令。同时最多 8 个。中间隔了一层 hub 的时候，**它自己的调用预算通常先到**（gld 是一次调用 60 秒、coding 连接闲 2 分钟就收），连接一丢后台命令跟着停——症状和排查见[排错手册](troubleshooting.md#后台命令跑着跑着就没了)。细节见[协议第 5.5 节](protocol/remote-workspace-mcp-v1.md#55-后台命令run_in_backgroundwait_msstop_commandp41-新增)，四个时钟分别管什么见[第 6 节](protocol/remote-workspace-mcp-v1.md#6-连接生命周期)；
- 客户端取消一条还没跑完的 `exec_command`（MCP 的 `notifications/cancelled`，Claude Code 中止工具调用时发它），Runtime 上的命令会被停掉（先 TERM，2 秒后 KILL），不会接着跑完。**取消一次带 `wait_ms` 的 `read_output` 不一样**：停的只是这次等待，命令照跑，要停它只有 `stop_command`；
- `load_skill` 把项目自带的 skills 交给模型，见下一节；
- `view_image` 把 Runtime 上的 PNG、JPEG、GIF、WebP 图片交给模型看（单个文件最多 3932160 字节，太大时报错并给出缩小的命令）；图片原样发出，Claude Code 会自己缩放。受管 Codex 会话里模型要在脚本里调 `image()` 才看得到图，规则见[协议第 5.3 节](protocol/remote-workspace-mcp-v1.md#53-view_image看-workspace-里的图片p39-新增)；
- Claude 使用项目根 `CLAUDE.md` 上下文；Codex 使用根目录 `AGENTS.override.md`/`AGENTS.md` 的已测优先级；
- remote session 使用对应 Provider 的已测工具策略，让项目访问统一走 Runtime Node；Runtime 可以用执行账号的 `~/.agents/mcp.json` 再按名字关掉上面任意几个工具（[配置说明](configuration.md#agentsmcpjson再关掉一些工具和-skills)）；
- 受管会话里模型还能**搜网页**（Claude 的 `WebSearch`、Codex 的 `web_search`，默认开）；抓网页、子代理、待办清单要 workspace 自己开，关掉搜索写 `agent_tools = []`。这些都不碰 Agent 本机的磁盘，Agent 自带的文件和 shell 工具一直关着，见[配置说明](configuration.md#agent_tools)。

**传错参数会怎样**：`exec_command`、`apply_patch`、`stop_command` 不接受它们没声明的字段，连 `files[]` 里的每一项也一样——拒绝发生在命令跑起来、补丁落盘之前，结果里会列出它认识的字段名。只读那几个照常回答，只在末尾加一行说忽略了什么。`timeout_ms`、`preview_bytes` 超上限是拒不是钳（要跑更久用 `run_in_background`）。规则见[协议第 5.6 节](protocol/remote-workspace-mcp-v1.md#56-参数怎么验有副作用的拒绝只读的说一声p44-新增)。

**Codex 还有一条 opt-in 的路（已封存）**：workspace 写 `codex_exec_server = true` 后，Codex 交互会话不再拿这七个工具，而是用它自带的 `exec_command` / `apply_patch`，由 Runtime 上受 ccnm 监督和过滤的官方 `codex exec-server` 执行；只开交互模式，print 会被拒绝；Claude 不受影响。2026-09-17 起封存：只认 Codex 0.154.0、不再维护、新项目别开，原因见[双执行入口方案](plan/runtime-surfaces.md)第 12.0 节，开关和边界见[配置说明](configuration.md#codex_exec_server)。

## 项目自带的 skills

**skill 是项目写给 AI 的"这类任务该怎么做"**：`.claude/skills/<名字>/SKILL.md`，开头几行写名字和描述，后面是做法，旁边可以放脚本。直接在项目机器上跑官方 CLI 时，CLI 会在当前目录下发现它们；经 ccnm 时 CLI 的当前目录在 Agent Node 上，项目在 Runtime Node 上，它一个都发现不了——所以由 Runtime 这一侧来发现，经 `load_skill` 工具交给模型。Claude、Codex、外部 MCP 客户端三种入口都一样，不用配置。

会被找到的三个地方（相对项目根）：`.claude/skills/<名字>/SKILL.md`、`.agents/skills/<名字>/SKILL.md`、`.claude/commands/**/*.md`。

- **模型怎么知道有哪些**：每个 skill 的名字和描述就写在 `load_skill` 这个工具的说明里，模型整个会话都看得见；要用哪个，它带名字调一次，拿到正文照着做。
- **人怎么手动启动一个**（只有 Claude Code）：敲 `/mcp__ccnm__<名字> 参数`。Codex 不支持这种方式。
- **skill 里的脚本在哪跑**：Runtime Node 上，由模型用 `exec_command` 跑，和别的命令一样以执行账号的身份、受同样的限制。

三处和官方 CLI 不一样，都是故意的：

- SKILL.md 里的 `` !`命令` ``（官方 CLI 会在加载 skill 时先执行它、把输出填进正文）**不自动执行**。模型会看到一份清单，需要就自己用 `exec_command` 跑。一次"读 skill"不该变成一次"执行仓库指定的命令"。
- frontmatter 里的 `allowed-tools`、`hooks`、`model` 等**不起作用**，模型加载时会被告知。
- 只找项目里的。Runtime 执行账号 HOME 下的用户级 skills 不读。

**不想让模型看到某个 skill**：在 Runtime 执行账号的 `~/.agents/mcp.json` 里按名字给它一档——`name-only` 只列名字、`user-invocable-only` 只给人用 `/` 启动、`off` 哪里都没有。工具也能按名字关。写法和写错会怎样见[配置说明](configuration.md#agentsmcpjson再关掉一些工具和-skills)。被关掉的 skill 不会出现在下面说的 `Not offered` 里：那一段是给"写了却没生效"的，而这个是你有意藏的。

**写了 skill 但模型没用上，先这样查**：让模型（或你自己接一个 MCP 客户端）不带名字调一次 `load_skill`。返回的列表末尾有一段 `Not offered`，写着每个没被收进来的文件和原因——最常见的是 frontmatter 写错了（会说第几行）、没有 `description`、两个文件重名，以及 skills 目录是一个指到项目外面的 symlink（读路径出不了项目根，这条和 `read_file` 是同一个规矩）。另外，目录是会话开始时定下来的：会话中途新加的 skill 可以按名字加载，但要到下一个会话才出现在工具说明里。

完整规则见[协议文档第 5.1 节](protocol/remote-workspace-mcp-v1.md#51-load_skill-与-prompts项目自带的-skillsp36-新增)。**验到哪一步**：发现、加载、参数替换、目录长度、prompts 都有离线测试和一个不依赖 ccnm 代码的中立 MCP 客户端测试；"真实模型会不会主动去用 skill"**没有验**，见[支持矩阵](support-matrix.md)。

## 同一工作树的单写限制

Runtime MCP 在完整 session 生命周期持有独占写 guard。另一个 Agent Node、CLI 或后续 RPC 即使绕开上层协调，只要进入同一 Runtime workspace，也会在 MCP 初始化阶段得到 busy/unknown：

- canonical root、symlink alias 和嵌套 workspace 不会获得两份独立写权限；
- 同一 Git common dir 下的 worktree 保守互斥；
- 正常退出释放；异常退出留下 unknown，不会按超时自动接管。

unknown 的人工恢复步骤见[支持矩阵](support-matrix.md)。命令 parser 不是 sandbox；真正的边界仍是 `ccrun`/ACL/sudo/credential/network policy。

## 当前不做什么

以下能力暂时延后，不按功能清单机械实现：

- Git 专用 MCP 工具；
- 活得比会话久的后台进程（后台命令随会话结束而停，见上面"当前模型能做什么"）；
- Browser provider；
- image provider；
- Linux Controller；
- 多 Agent 自动编排。

优先让真实项目 dogfood 暴露真正高频、浪费 token 或需要人工介入的缺口，再定义这些工具的契约。
