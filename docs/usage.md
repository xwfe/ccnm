# 使用说明

`ccnm doctor <workspace>` 的基础链路确认正常后，同一个 workspace 可以从 Runtime Node 或 Agent Node 发起。

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

两条上手就会遇到的：

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
```

主要行为：

- `read_file`、`list_files`、`search_text` 都受 workspace 路径边界约束；
- `apply_patch` 是结构化写入路径，带版本检查，并提供事务/恢复保护；
- `exec_command` 使用 argv，不主动通过 shell 执行，但调用者仍然可以显式运行 `sh -c` 等程序，所以它本质上仍然是命令执行能力；
- 大输出由 `read_output` 分页读取，避免一次把全部输出塞进模型上下文；
- Claude 使用项目根 `CLAUDE.md` 上下文；Codex 使用根目录 `AGENTS.override.md`/`AGENTS.md` 的已测优先级；
- remote session 使用对应 Provider 的已测工具策略，让项目访问统一走 Runtime Node。

## 同一工作树的单写限制

Runtime MCP 在完整 session 生命周期持有独占写 guard。另一个 Agent Node、CLI 或后续 RPC 即使绕开上层协调，只要进入同一 Runtime workspace，也会在 MCP 初始化阶段得到 busy/unknown：

- canonical root、symlink alias 和嵌套 workspace 不会获得两份独立写权限；
- 同一 Git common dir 下的 worktree 保守互斥；
- 正常退出释放；异常退出留下 unknown，不会按超时自动接管。

unknown 的人工恢复步骤见[支持矩阵](support-matrix.md)。命令 parser 不是 sandbox；真正的边界仍是 `ccrun`/ACL/sudo/credential/network policy。

## 当前不做什么

以下能力暂时延后，不按功能清单机械实现：

- Git 专用 MCP 工具；
- 托管后台长进程；
- Browser provider；
- image provider；
- Linux Controller；
- 多 Agent 自动编排。

优先让真实项目 dogfood 暴露真正高频、浪费 token 或需要人工介入的缺口，再定义这些工具的契约。
