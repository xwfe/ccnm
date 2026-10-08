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

## 通过第三方终端使用

PocketShell 等第三方工具只作为终端入口；只要能在目标机器提供可输入自定义命令的交互终端（PTY），就按普通终端使用 ccnm，不需要安装手机端 ccnm、开发适配插件或另外部署 ccnm 网页服务。这里描述接入条件，不代表某个 PocketShell/浏览器版本已经实测兼容。

默认进入 **Agent Node 的 Operator shell**：该机器上既有 ccnm 配置、Controller 和官方 Agent 登录；workspace 对应的项目和工具仍在 hpsrv Runtime。外部工具若有自己的后端，以最终 shell 的机器、UID、PATH 为准，不能把浏览器所在地当成执行位置。若从 Runtime Node 发起，也须使用其已配置的 Operator，而不是让 `ccrun` 持有回连 Agent 的凭据。

在进入的远端终端中使用现有命令，`my-project` 替换成已注册 workspace：

```bash
ccnm ls
ccnm run my-project                 # 需要启动或进入会话时使用
ccnm status my-project --all
ccnm attach my-project              # 离开后接回已有会话
ccnm stop my-project                # 仅在明确要结束会话时使用
```

多 instance 或需要精确目标时，用现有 `--agent` 和 `--session`，见[单个项目](#单个项目)。不另做“只允许 attach”的网页绑定；`--print` 仍遵循[非交互模式](#非交互---print)的发起侧限制，不能为了手机方便把回连身份改成 `ccrun`。

离开时使用 tmux detach，按键以 ccnm 状态栏/现有配置为准；不要用 Claude 的“后台会话”替代，也不要让外部工具另起裸 Claude/Codex 来接管 ccnm 会话。终端断开与 Agent→Runtime MCP 断开是两件事：前者在受管会话仍存活时可重新 attach，后者会触发 Runtime 命令收尾。外部客户端主动退出 Agent、停止后端、机器睡眠或重启不能套用“只是 detach”的保证；重连后先核对状态，不盲目重发未确认的输入。

SSH、认证、手机 VPN/代理、隧道和网页访问方式由外部工具/部署环境负责，ccnm 不要求手机必须连接 Tailscale，也不自动开放任何公网端口。入口按 Operator 权限保护，不能将 AI 登录或转发的凭据下放给 Runtime。键盘、渲染和连接问题先在客户端侧定位；能在普通终端复现的 ccnm 执行/会话问题再按[排错手册](troubleshooting.md)处理，不新增客户端专属产品阶段。

## 选择 Agent Instance

使用 `agent = { node = "worker", instance = "claude-main" }` 的 workspace 会默认选择该 instance。同一个 Agent Node 上可显式覆盖：

```bash
ccnm doctor my-project --agent codex-main
ccnm run my-project --agent codex-main
```

`--agent` 是受限 instance id，不是 Provider、Node、路径或官方 CLI 参数。legacy `agent_node` workspace 不接受它。Provider/profile 只由 Agent Node 本机配置解析；Codex 的专用 HOME 不会发给 Runtime。

一个正在运行的 session 固定绑定 workspace、root 和完整 Agent identity。换 Provider 或 instance 不会复用/替换旧 session；先精确停止旧 session。

**交互会话第一次起来，官方 CLI 会先问几句**（P62 实测，Codex 0.154.0 与 Claude Code 2.1.285）：

- **"是否信任这个目录"**：两家都会问。问的是 Agent Node 上 ccnm 给这个 workspace 建的空占位目录（`~/.local/state/ccnm/workspaces/<名字>`），不是你的项目，选"信任"即可；Claude 默认选中的是 "No, exit"，要先按一次下箭头。答过之后同一个 workspace 下次不再问。
- **Claude Code："要不要把 auto mode 设成默认权限模式"**：选 "No"。选 "Yes" 改的是 Agent 账号上 Claude Code 的全局默认，你在那台机器上直接用 Claude 时也会跟着变；ccnm 起会话时自己用 `--permission-mode` 指定模式，用不着这个默认。

`--detached` 起的会话要先 `ccnm attach` 答完这几句，工具才会连上——在那之前 `ccnm status` 显示 `TOOLS DOWN`。

ccnm 不替你提前答：官方 CLI 把"信任过这个目录"记在 Agent 账号上它自己的配置里，ccnm 不改官方 CLI 的配置文件；Codex 能用命令行参数临时覆盖这一项，但在 ccnm 适配的 Codex 版本上没实测过，不按猜的参数去传（P66 定的，原因见 [P66 记录](research/2026-10-01-p66-low-impact-findings.md)）。

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

**不带 `--agent` 时只列这个 workspace 默认实例的会话。** 项目的终端要是被别的实例占着（比如用 `--agent codex-main` 起的），会多一行指给你看（P66 起；更早的构建只说"没有在跑的会话"，P62 真机上就这么误导过）：

```text
实例 claude-main 没有在跑的会话（--print 的运行不算在内）
这个项目的终端现在是实例 codex-main 的会话：加 --agent codex-main 看它
```

照它说的加上 `--agent codex-main`，或者用不带项目名的 `ccnm status`，就能看到那个会话的详情。

精确寻址使用 ccnm session id：

```bash
ccnm status my-project --agent codex-main --session <ccnm-session-id>
ccnm attach my-project --agent codex-main --session <ccnm-session-id>
ccnm stop my-project --agent codex-main --session <ccnm-session-id>
```

在 Runtime Node 上跑 `ccnm status my-project`，最后一行是这个项目的写锁：空闲、被谁占着、故意留着，还是说不清（P60 起）。它经 Agent 问 Runtime 执行账号，所以 `--print` 运行和外部 MCP 客户端占着锁也看得见；它只看不拿锁，"空闲"也不代表替你占住了。各种说法怎么处理见[运维手册](operations.md#写入-guard-残留)。

ccnm session id 与 Claude/Codex 自己的 thread/resume id 是两类值，不能互换。精确操作会校验 session 的 workspace 和 Agent identity。状态区分 `starting`、`running`、`completed`、`failed`、`stopping`、`unknown`；不能证明进程已经结束时不会猜成 failed。

精确停止 print session 时，即使已有结果，也会检查已记录的 supervisor/Agent 进程组；组仍存在、PID 记录损坏或进程查询失败会返回 `NotReady`，不对历史 PID 发信号，也不改写结果。

session 建立后，Agent Node 上的 `attach/status/result/stop` 继续本机管理记录，不依赖重新解析 workspace root。Runtime Node 发起的命令仍由 Runtime 默认选择或 `--agent` 选择约束。

### 清掉结束了的会话留下的东西

在 Runtime Node 上：

```bash
ccnm cleanup my-project                 # 先看：列出会删什么、留什么、为什么，什么都不删
ccnm cleanup my-project --apply <令牌>   # 照预览删，令牌在预览最后一行
```

三台机器、三个账号各删各的，项目、写锁和凭据都不碰，还在跑或说不清的会话不碰；Machine API 的会话删的是输出，记录和 `start_key` 留着，之后 `session.result` 回 `expired`。细节和会留下哪些东西见[运维手册](operations.md#想立刻腾地方ccnm-cleanup)。

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

### 不想一条条确认：开 `allow_unattended_exec`

交互式会话默认每次执行命令都停下来问你一次。Claude 那边**任何权限模式都关不掉**（[为什么](troubleshooting.md#开了-bypasspermissionsexec_command-还是每次都问)）；Codex 那边你可以在会话里用 `/permissions` 切到 Full Access 或 Approve for me，但只管那一个会话（v0.11.0 及之前的 Agent 上 Approve for me 会延续到之后的会话，[去掉的办法](troubleshooting.md#受管-codex-会话exec_command-每次都弹或者一次都不弹)）。

平时用交互会话干活的项目，建议在**项目那台机器**的 `config.toml` 里给它打开：

```toml
[workspaces.my-project]
allow_unattended_exec = true
```

新起的会话就不再问了。开了之后少了什么、还剩什么挡着、怎么收回，见[配置说明](configuration.md#allow_unattended_exec)。

另一条路是 `--print`，两者的区别：

| | `allow_unattended_exec = true` | `--print` |
| --- | --- | --- |
| 形态 | 常驻会话，一直不问 | 一问一答，跑完就结束 |
| 输出在哪 | 在对面 tmux 里，要滚要选 | **直接打在你本机终端**，随手复制 |
| 适合 | 长时间结对、一来一回地改 | 明确的一件事：跑测试、查状态、改一处 |

一件明确的事，不需要常驻会话：

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

四条上手就会遇到的：

- **给 Claude Code 配这个 server 时加一行 `"alwaysLoad": true`。**不加的话它会被延迟加载——模型每个任务得先花一个回合调 `ToolSearch`，才拿得到 ccnm 的工具。配置形状、实测数字和它的代价（首轮请求前会等 bridge 连上 Runtime）见[协议文档](protocol/remote-workspace-mcp-v1.md#alwaysload-是干什么的)。
- **Runtime 上要有 `ripgrep`**，`search_text` 调它；项目要编译测试，那套工具链也得在 Runtime 上，而且要装在**执行身份自己的 home** 里、写进非交互 ssh 看得见的 PATH——照默认装 rustup 会得到"cargo 没装"的错，原因和做法见[运维手册](operations.md#runtime-node-的前置条件与项目工具链)。
- **Codex 当 Host 要给这个 server 写 `tool_timeout_sec = 1870`。**不写的话 Codex 等一次调用最多 300 秒，超过就跟模型说超时了，命令却还在 Runtime 上跑，模型一重试就是两份。配置形状和这个数怎么来的见[协议文档](protocol/remote-workspace-mcp-v1.md#codex-当-host写上-tool_timeout_sec)；受管 Codex 会话由 ccnm 自己传，不用管。
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
call_mcp_tool
```

这里列的是 Runtime 的全部工具定义，不是每条连接固定提供十二个。外部 read 只有七个只读工具；coding 没有可转接 server 时不提供 `call_mcp_tool`。Agent 的 `ccnm_agent` 另有自己的工具表，不能与 Runtime 的同名工具混用位置或身份。

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
- remote session 使用对应 Provider 的已测工具策略，让项目访问统一走 Runtime Node；
- 受管会话里模型还能**搜网页**（Claude 的 `WebSearch`、Codex 的 `web_search`）、用 **Agent 机器上装好的 MCP server**（默认只给远端地址的，见下面[那一节](#agent-机器上的-mcp-server)），Claude 还能**抓网页、派子代理、记待办清单**——P77 起这五项默认全开，不想要哪项就在 Runtime 的 workspace 上写 `agent_tools` 去掉它，全关写 `agent_tools = []`。Agent 自带的文件和 shell 工具一直关着，见[配置说明](configuration.md#agent_tools)。

**传错参数会怎样**：`exec_command`、`apply_patch`、`stop_command` 不接受它们没声明的字段，连 `files[]` 里的每一项也一样——拒绝发生在命令跑起来、补丁落盘之前。P49 的 `call_mcp_tool` 外层参数也拒绝未知字段，嵌套 `arguments` 则是目标 server 的参数对象，不能套用 ccnm 文件工具的 schema。只读文件工具通常接受额外字段并在末尾说明；Agent 的 `read_mcp_result` 有自己的严格 schema，不应据此推定所有只读工具都相同。`timeout_ms`、`preview_bytes` 超上限是拒不是钳，规则见[协议](protocol/remote-workspace-mcp-v1.md)。

## 项目自带的 skills

**skill 是项目写给 AI 的"这类任务该怎么做"**：`.claude/skills/<名字>/SKILL.md`，开头几行写名字和描述，后面是做法，旁边可以放脚本。直接在项目机器上跑官方 CLI 时，CLI 会在当前目录下发现它们；经 ccnm 时 CLI 的当前目录在 Agent Node 上，项目在 Runtime Node 上，它一个都发现不了——所以由 Runtime 这一侧来发现，经 `load_skill` 工具交给模型。Claude、Codex、外部 MCP 客户端三种入口都一样，不用配置。

会被找到的三个地方（相对项目根）：`.claude/skills/<名字>/SKILL.md`、`.agents/skills/<名字>/SKILL.md`、`.claude/commands/**/*.md`。

- **模型怎么知道有哪些**：每个 skill 的名字和描述就写在 `load_skill` 这个工具的说明里，模型整个会话都看得见；要用哪个，它带名字调一次，拿到正文照着做。
- **人怎么手动启动一个**（只有 Claude Code）：敲 `/mcp__ccnm__<名字> 参数`。Codex 不支持这种方式。
- **skill 里的脚本在哪跑**：Runtime Node 上，由模型用 `exec_command` 跑，和别的命令一样以执行账号的身份、受同样的限制。

**skill 要求替它跑的命令，看会话里跑命令问不问人**（P79）。SKILL.md 里的 `` !`命令` ``（官方 CLI 会在加载 skill 时先执行它、把输出填进正文）和 frontmatter 里的 `hooks`，都是没人一条条批准就要跑的命令，所以只在**命令本来就不问人**的会话里跑：开了 [`allow_unattended_exec`](configuration.md#allow_unattended_exec) 的交互会话、`--print`、`ccnm mcp bridge` 的 coding 模式。其余会话（默认的交互会话、只读的外部会话）照旧不跑，模型加载时会看到一份清单和原因，需要就自己用 `exec_command` 跑。

| frontmatter | 在 ccnm 里 |
| --- | --- |
| `` !`命令` `` | 命令不问人的会话：加载时在项目那台机器上跑，输出填进正文，跑失败这次加载就报错（和原生一样）。其余会话：不跑，列出来 |
| `hooks` | 命令不问人的会话：加载后登记，本会话剩下的时间里，在项目那台机器上围着 ccnm 自己的工具跑。`PreToolUse` 退出码 2 或 `permissionDecision: "deny"` 会拦下这次调用，`PostToolUse` 的 stderr 或 `additionalContext` 附在结果后面。`matcher` 写原生工具名也行：`Bash` 对应 `exec_command`，`Read` 对应 `read_file`，`Edit`/`Write` 对应 `apply_patch`（每个文件一次），`Grep`、`Glob` 对应 `search_text`、`list_files`；钩子从 stdin 读到的是原生的格式（`tool_input.command`、`tool_input.file_path`），给本地 Claude Code 写的脚本照样能用。`Stop` 这类发生在客户端里的事件、`http` 等非 command 类型不跑，会写明。其余会话：不登记 |
| `allowed-tools` | 不逐条放行：命令不问人的会话里本来就不用放行；会问的会话里一个 skill 关不掉那一问（要关去开 `allow_unattended_exec`）。列了 `WebFetch` 这类 Agent 自带工具时，会告诉模型它们在这个 workspace 开没开（[`agent_tools`](configuration.md#agent_tools) 定） |
| `context: fork`（连同 `agent`、`model`） | Claude 会话、开着子代理（默认开）时，告诉模型用 `Agent` 工具派一个子代理去跑，带上 `agent` 和 `model`。这是给模型的指示，ccnm 看不到它照没照做。没有子代理（Codex、关了 `subagents`）时写明没生效 |
| 只写了 `model` | 不起作用：经 MCP 交出去的 skill 换不了会话的模型 |
| `effort`、`shell`、`disallowed-tools` | 不起作用，加载时写明 |

**钩子什么时候开始管、管到什么时候**：模型（或你用 `/mcp__ccnm__<名字>`）加载这个 skill 之后才登记，在那之前一条都不跑；登记后管到这个会话结束，改了 SKILL.md 也不撤，要撤就重开会话。钩子在项目机器上、workspace 根下、以执行账号跑，`CLAUDE_PROJECT_DIR` 是 workspace 根，最长 600 秒（可用 `timeout` 改短）。所以想让钩子从一开始就管着，就在项目的 `CLAUDE.md` / `AGENTS.md` 里写一句"改代码前先加载 guard 这个 skill"。

一个例子：拦下 force push、加载时把当前分支填进正文。放在项目的 `.claude/skills/guard/SKILL.md`；命令不问人的会话里两样都生效，要问人的会话里都不跑（`jq` 要装在项目机器上）：

```markdown
---
description: 推送和发版前的规矩。改代码、推送之前先加载它。
hooks:
  PreToolUse:
    - matcher: Bash
      hooks:
        - type: command
          command: "jq -r .tool_input.command | grep -q 'push --force' && { echo '别 force push，用 scripts/release.sh' >&2; exit 2; }; exit 0"
---
当前分支：!`git branch --show-current`。推送一律走 scripts/release.sh。
```

模型之后要是跑 `git push --force`，拿到的是 `CCNM_E_POLICY: exec_command was not run: a PreToolUse hook of skill guard stopped it: 别 force push，用 scripts/release.sh`，命令没有执行。

还有两点要知道：

- Agent 机器上装的 skill（下一条），`` !`命令` `` 和 `hooks` 一律不跑：那台机器上有 AI 的登录，而且项目的工具调用不经过它。
- 两台机器上**装好的** skills（`~/.claude/skills`、`~/.agents/skills` 这些）也会交给模型（P48，默认全开）：Runtime 执行账号装的并进 `load_skill`，排在项目的后面；Agent 上你自己装的由一个叫 `mcp__ccnm_agent__load_skill` 的工具交出去。附件用 `load_skill` 的 `file` 读。怎么关、怎么按名字藏、同名谁赢，见[配置说明](configuration.md#machine_skills)。

**写了 skill 但模型没用上，先这样查**：让模型（或你自己接一个 MCP 客户端）不带名字调一次 `load_skill`。返回的列表末尾有一段 `Not offered`，写着每个没被收进来的文件和原因——最常见的是 frontmatter 写错了（会说第几行）、没有 `description`、两个文件重名（包括被机器上装好的同名 skill 盖掉，会写明被谁盖掉），以及 skills 目录是一个指到项目外面的 symlink（读路径出不了项目根，这条和 `read_file` 是同一个规矩）。另外，目录是会话开始时定下来的：会话中途新加的 skill 可以按名字加载，但要到下一个会话才出现在工具说明里。

完整规则见[协议文档第 5.1 节](protocol/remote-workspace-mcp-v1.md#51-load_skill-与-prompts项目自带的-skillsp36-新增)。**验到哪一步**：发现、加载、参数替换、目录长度、prompts，以及 P79 的加载时命令和 hooks，都有离线测试和一个不依赖 ccnm 代码的中立 MCP 客户端测试；"真实模型会不会主动去用 skill"、"会不会照提示派子代理"**没有验**，见[支持矩阵](support-matrix.md)。

## 项目那台机器上的 MCP server

**进程收尾（P52）**：server 关闭时，它留在自己进程组里的子进程一起被杀掉并确认；清不掉就不交出写权。离开进程组的后代（`setsid`、守护进程）：Linux 上会话结束时一并收掉（P84，见[协议](protocol/remote-workspace-mcp-v1.md)开头 2026-10-09 那条），macOS 上够不着，会这样做的 server 怎么处理见[支持矩阵](support-matrix.md)里 C51-01 那段。详见 [P52 记录](research/2026-09-25-p52-relay-group-cleanup.md)。

项目的 `.mcp.json` 里声明了 server（比如一个连本地数据库的），或者 Runtime 的执行账号给 Claude Code / Codex 装了 server，模型会多一个工具 `call_mcp_tool`（P49，默认全开）：

```text
call_mcp_tool                                        有哪些 server、各自什么状态（什么都不起）
call_mcp_tool  server=db                             db 的工具和参数表（这一步才把它起起来）
call_mcp_tool  server=db  tool=query  arguments={…}  调用
```

- **只在能写的会话里有**（Managed 会话、`coding` 模式的外部连接），因为起 server 就是以执行账号跑程序：和 `exec_command` 过同一道执行门、同一个沙箱，有人值守时每次都问你。
- **只转在这台机器上起的程序**（stdio）；HTTP 的 server 不需要跑在项目旁边，会列出来并说明——Agent 机器上装的由下一节那个同名工具转。
- 结果太长时先给 32 KiB，其余用 `read_output` 接着读，和命令输出一样。
- 会话结束时先停掉这些 server，再把写锁交出去。

怎么关、怎么不读项目的 `.mcp.json`、按名字藏，见[配置说明](configuration.md#runtime_mcp)；完整规则见[协议第 5.7 节](protocol/remote-workspace-mcp-v1.md#57-call_mcp_toolruntime-上的-mcp-serverp49-新增)。

**起不来，先这样查**：不带参数调一次 `call_mcp_tool`，每个 server 后面写着状态；"not relayed" 的写着原因（HTTP 的、配置里用了执行账号环境里没有的变量）。带 `server` 调失败时报 `CCNM_E_DEPENDENCY`，后面是它在 stderr 上说的最后一段话——最常见的是程序不在执行账号的 `PATH` 上（`npx`、`uvx` 装在你自己账号的 mise / nvm 目录里，`ccrun` 看不到）。

## Agent 机器上的 MCP server

这组工具使用 Agent Identity，不受 Runtime 的 `exec_sandbox` 保护。本机服务显式 opt-in 后可能读写 Agent 文件或调用其本地服务；关闭原生 Read/Bash 不封锁第三方 server 的能力。信任范围见[生产安全](production-safety.md#两侧-skills-与-mcp-的信任边界)。

你自己给 Claude Code / Codex 装的 MCP server（`~/.claude.json`、`~/.codex/config.toml` 里的），远端会话也能用（P50）：模型看到 `ccnm_agent` 下的 `call_mcp_tool`，用法和上一节一样，另有 `read_mcp_result` 读长结果的后面部分。

```text
mcp__ccnm_agent__call_mcp_tool                                     这台机器上有哪些、各自什么状态
mcp__ccnm_agent__call_mcp_tool   server=exa-search                 它的工具和参数表（这一步才连上它）
mcp__ccnm_agent__call_mcp_tool   server=exa-search tool=… arguments={…}  调用
mcp__ccnm_agent__read_mcp_result ref=… offset=…                    结果太长时照上一次末尾的说明接着读
```

- **默认只给别的机器上的地址**（exa、DeepWiki 这类 HTTP server）。在这台机器上跑的——`npx` / `uvx` 起的程序、`127.0.0.1` 上的服务——能碰这台机器的磁盘、用你的 ssh，默认不给；要哪个，在 Agent 机器的配置里点名：

  ```toml
  [agent_mcp]
  local = ["context7", "mcp-time"]
  ```

- workspace 那边也要同意：Runtime 配置里 `agent_tools` 默认含 `mcp_servers`，去掉就不给（见[配置说明](configuration.md#agent_tools)）。
- 结果太长时先给 32 KiB，其余留 30 分钟，用 `read_mcp_result` 接着读。
- HTTP 的经这台机器的 `curl` 连；要 OAuth 登录的连不上（令牌在 Claude Code 那里）。

**不给、连不上，先这样查**：不带参数调一次 `mcp__ccnm_agent__call_mcp_tool`，每个 server 后面写着状态或原因——"runs as a program on this machine" 就是没点名，"turned off in ~/.codex/config.toml" 是你在 Codex 里关了它。开关全在 [`[agent_mcp]`](configuration.md#agent_mcp)。

## 同一工作树的单写限制

Runtime MCP 在完整 session 生命周期持有独占写 guard。另一个 Agent Node、CLI 或后续 RPC 即使绕开上层协调，只要进入同一 Runtime workspace，也会在 MCP 初始化阶段得到 busy/unknown：

- canonical root、symlink alias 和嵌套 workspace 不会获得两份独立写权限；
- 同一 Git common dir 下的 worktree 保守互斥；
- 正常退出释放；异常退出留下 unknown，不会按超时自动接管。

unknown 的人工恢复步骤见[支持矩阵](support-matrix.md)。命令 parser 不是 sandbox；真正的边界仍是执行账号本身（默认是你自己的，要隔离就建专用账号，见[生产安全](production-safety.md#要不要建专用账号)）与 ACL/sudo/credential/network policy。

## 当前不做什么

以下能力暂时延后，不按功能清单机械实现：

- Git 专用 MCP 工具；
- 活得比会话久的后台进程（后台命令随会话结束而停，见上面"当前模型能做什么"）；
- Browser provider；
- image provider；
- Linux Controller；
- 多 Agent 自动编排。

优先让真实项目 dogfood 暴露真正高频、浪费 token 或需要人工介入的缺口，再定义这些工具的契约。
