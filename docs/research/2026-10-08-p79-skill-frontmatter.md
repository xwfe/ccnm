# P79：skill 的 `hooks`、`` !`命令` ``、`allowed-tools`、`model` 在远端会话里生效（2026-10-08）

设计与依据见[设计记录](2026-10-07-skill-frontmatter-design.md)；用户 2026-10-08 定第 7 节四件事全按建议。本文记做了什么、和设计的出入、怎么验的。

## 结论

| 字段 | 命令不问人的会话 | 要问人的会话、`read` 模式、Agent 机器上的 skill |
| --- | --- | --- |
| `` !`命令` `` | 加载时在 Runtime 上跑，输出填进正文；一条失败，整次加载 `CCNM_E_INVALID_ARGS` | 不跑，列出行号、命令和原因（和 P78 之前一样，只是多了原因） |
| `hooks` | 登记到这个 server 进程结束；`PreToolUse` 能拦下调用（`CCNM_E_POLICY`），`PostToolUse` 的话附在结果末尾 | 不登记，写明原因 |
| `allowed-tools` | 写"没什么可放行" | 写"不起作用"和原因；列了 Agent 自带工具时写明开没开 |
| `context: fork` + `model` / `agent` | 受管 Claude 会话、开着 `subagents`：告诉模型用 `Agent` 派子代理；Codex 或关了子代理：写明没生效；外部客户端：写"如果你的客户端能派子代理" | 同左（和命令问不问无关） |
| 只写 `model` | 写明没生效 | 同左 |
| `effort`、`shell`、`disallowed-tools` | 列为不起作用 | 同左 |

"命令不问人的会话"由一个函数回答，`tools/list` 要不要给 `exec_command` 挂 `anthropic/requiresUserInteraction` 用的也是它（`asks_a_person`），两边不会说法不一：受管会话是 `--print`，或交互会话开了 `allow_unattended_exec`；外部 bridge 的 coding 模式；而且执行门放行 `exec_command`。

## 怎么做的

- 新模块 `mcp::hooks`：读 frontmatter 里的 `hooks`（只认 `PreToolUse`、`PostToolUse`、`type: command`，其余进"不跑"清单）；`matcher` 只认名字、`|`、`.*` / `*`，整名匹配，别的正则语法整条记为不跑，不猜；把 ccnm 的调用转成原生工具的样子（`exec_command`→`Bash` 的 `command`，`apply_patch` 每个文件一次 `Edit`/`Write`/`NotebookEdit`，`read_file` 等→`Read` 的绝对 `file_path`……）；按原生规则读结果（退出 2、`permissionDecision`、`additionalContext`、`decision: block`、`continue: false`）；`Registry` 按 skill 去重、`once` 退出 0 后撤；`Runner` 用 `bash -c` 在 workspace 根下跑，先做和 `exec_command` 同样的凭据复查，环境照 `runtime_child` 清理，开了 `exec_sandbox` 就套上。
- `skills::load_skill` / `prompt_text` 多一个 `Effects` 参数（能不能跑命令及原因、有没有子代理、`agent_tools`），返回 `Loaded`（正文和要登记的钩子）。`` !`命令` `` 在参数填好之后才跑，和原生一样；替换用的 `fill` 与共享库 `inject::find` 走同一套规则，普通代码块里的、空的、没闭合的 `!` 块都不动。
- `Server` 手写 `call_tool`：有钩子时先跑 `PreToolUse`，被拦就直接回 `isError`；调用没出错再跑 `PostToolUse`，附言放在结果最后一个文本块里（`view_image` 的图片仍在 `content[1]`）。没有钩子时就是宏生成的那一行。钩子在阻塞线程池里跑、持着 `Inner`，所以会话结束时写锁要等它跑完或超时才放。
- `prompts/get`（人敲 `/mcp__ccnm__<名字>`）和 `load_skill` 一样登记钩子、跑加载时命令。
- Agent 上的 `ccnm_agent` 一律不跑；它的启动参数多一个可选的 `subagents`（P79 之前的参数里没有，读成"不知道"），用来回答 `context: fork`。

## 和设计记录的出入

- 设计写"超时用 `exec_command` 同一套"，实际：`` !`命令` `` 单条 120 秒（`exec_command` 的默认值），钩子默认 600 秒、最多 600 秒（原生默认 600 秒）。`` !`命令` `` 的输出每条最多 16 KiB，正文整体仍是 64 KiB 截断。
- 钩子用的是一次性跑完的 `SystemRunner`（进程组、超时杀整组），不是 `exec_command` 的后台作业表：钩子是短命令，也不该出现在 `read_output` 里。代价：`stop_all` 管不到它，会话结束时等它跑完或超时。
- `permissionDecision: "ask"` 当成拦下：这些会话按定义没人可问。
- 同一个 skill 加载两次不重复登记（设计没写）。

## 用例

- `mcp::hooks` 单元测试 10 条：原生写法读得出、读不了的点名；`^Bash$` 这种正则不猜；整名匹配和通配；原生形状的 `tool_input`（`cmd` 数组按 shell 规则拼成一行、`apply_patch` 每文件一次、删除不算 `Edit`）；ccnm 自己的名字拿原始参数；退出 2 前拦后说；JSON 决定（deny、ask 拦，allow 带 `updatedInput` 写明不照做，`additionalContext` 附上，纯 stdout 不给模型）；失败与超时不挡调用；去重与 `once`；stdin 的字段。
- `mcp::skills` 5 条：命令不问人时输出填进正文、普通代码块与空占位不动、钩子返回给调用方、`allowed-tools` 与 `agent_tools` 的说明；加载时命令失败整次加载报错；要问人时什么都不跑；`context: fork` 在三种 `subagents` 下的说法、只写 `model` 的说法；`fill` 与 `find` 一致。另改 1 条旧用例的断言（`allowed-tools` 不再进"no effect"清单，改成自己一行）。
- `mcp::server` 1 条：四种会话（print、交互、交互 + `allow_unattended_exec`、外部 read）里能不能跑 skill 的命令，并钉住"能跑"和"客户端被要求问人"不会同时成立。
- 真实二进制 2 条（`ccnm-cli/tests/mcp_read_file.rs`）：共用账号、非交互会话里加载带钩子的 skill，`!`cat marker.txt`` 的输出进了正文；`exec_command rm` 被 `PreToolUse` 钩子拦下、文件还在；`apply_patch` 新建文件后结果末尾是 `PostToolUse` 的 `additionalContext`，钩子收到的 stdin 是 `tool_name: Write` 和绝对 `file_path`。交互会话里同一个 skill 什么都不跑、`rm` 照常执行。把"能不能跑"临时恒为否时这两条都红。
- 中立 MCP 客户端 1 条（`test_remote_workspace_mcp.py`）：经 bridge coding 加载，`!`touch`` 跑了，`rm` 被钩子拦下、`true` 照常。

协议 fixture：改 `call-load-skill-ok.json`（要问人的会话的新说法），新增 `call-load-skill-ran.json`、`call-blocked-by-hook.json`。

## 门禁

本机 macOS 26.6.2 arm64、rustc 1.98.0：`cargo fmt --check`、`cargo clippy --workspace --all-targets -D warnings`、`cargo +1.89 check --locked` 通过；`cargo test --workspace --no-fail-fast` 1120 passed / 0 failed（P78 时 1101）；`python3 -B scripts/ci_gates.py` 的结果见状态里的 P79 证据。

## 没做的

- 没跑模型：模型会不会照 `context: fork` 的提示派子代理、会不会在被钩子拦下后改做法，都没验。
- 没在真实 Claude Code / Codex 上接过：钩子附言作为第二个文本块在 Host 里是否完整给到模型，没量（`view_image`、`read_notebook` 已经是多块结果，按同样方式送到）。
- 没在 Linux、真机上跑；没发版。
