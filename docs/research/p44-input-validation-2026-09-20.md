# P44 服务端自己验输入（2026-09-20）

环境：macOS 26.6.2 arm64，rustc 1.98.0。零模型额度，没上真机。探针是真实 `ccnm internal mcp-serve` 加一个不 import 任何 ccnm 代码的中立 MCP 客户端。

来源是主评审 `toexec/docs/plan/2026-09-19-cross-project-refactor-review.md` 的 X06/X10，和本仓[落地清单](2026-09-19-cross-project-refactor-actions.md)的 C-C。

## 1. 结论

- **未知字段以前被静默吃掉**，顶层和嵌套都是。`exec_command` 带一个编出来的 `sandbox: false` 照样把命令跑了；`files[0]` 多一个 `mode` 照样落盘。
- 而已发布的 `inputSchema` 里**一个 `additionalProperties` 都没有**，等于声明"随便加字段"——所以这不只是解析宽松，是**声明和实现一起在说同一句错话**。X06 验收的"声明的 schema 与真实解析一致"当时不成立。
- 不认识的枚举、类型不对、必填缺失这三条一直都拒得对，而且是 `isError` 工具结果而不是 JSON-RPC 错误，**句柄不受伤**——正合 X06 要的"拒绝发生在执行之前，且不误伤仍有效的 coding 会话"。
- 收紧的力度由用户 2026-09-20 定：**有副作用的三个工具拒绝，只读的照答但说清楚忽略了什么。**
- 能力代次那半（X10）不用另造：`initialize` 的 `serverInfo.version` 就是远端 ccnm 的版本，`tools/list` 就是真实工具表。

## 2. 探针：改之前是什么样

| 喂什么 | 改之前 | 改之后 |
| --- | --- | --- |
| `exec_command` 带 `sandbox: false` | **ok，命令照跑** | `unknown field \`sandbox\`, expected one of \`cmd\`, \`shell\`, …` |
| `apply_patch` 顶层带 `force: true` | **ok，补丁照落盘** | `unknown field \`force\`, expected \`files\` or \`dry_run\`` |
| `files[0]` 带 `mode: "0777"` | **ok，文件照建** | `unknown field \`mode\`, expected one of \`op\`, \`path\`, …`（落盘之前） |
| `read_file` 带 `follow_symlinks: true` | ok，什么都不说 | ok，末尾多一行 `[ignored, this tool has no such argument: follow_symlinks …]` |
| `op: "chmod"` | 拒绝，列出合法值 | 不变 |
| `timeout_ms: "soon"` | 拒绝，`invalid type` | 不变 |
| 不给 `files` | 拒绝，`missing field` | 不变 |
| `timeout_ms: 99999999` | **静默钳到 600000** | 拒绝，并指路 `run_in_background` |
| `read_output` 的 `wait_ms: 99999999` | 静默钳到 600000 | 仍然钳，但写明 `[waited up to 600000 ms, not the 99999999 ms asked for…]` |

## 3. 为什么写和读分开

一个没人读的字段，在**写和执行**这边意味着命令按调用方没同意的条件跑了。它以为自己传了 `sandbox: false`，而那个字段根本没人看——等发现时命令已经执行完了。所以那三个工具（`exec_command`、`apply_patch`、`stop_command`）连同 `files[]` 里的嵌套结构（`FilePatch`、`Edit`、`CellEdit`）一律 `deny_unknown_fields`，拒绝发生在任何副作用之前。

**读**不会这样：一次读最多是结果不如预期。为一个多余字段让整次读失败反而更糟——调用方可能只是多带了一个自己那边的字段。所以只读工具照常回答，但结果里写明忽略了什么：静默丢掉 `follow_symlinks`，正是"我让它跟随符号链接了"变成"它跟随了符号链接"的那一步。

超界值按同一条线分：写和执行这边**拒**（`timeout_ms` 决定命令什么时候被杀，钳了等于让调用方拿着一个十分钟的命令以为自己有 27 小时），读这边**钳但说一声**（等得比要求的短不是错答案，但不说的话调用方会把"还在跑"读成"卡死了"）。

## 4. schemars 帮了一半

给结构体加 `#[serde(deny_unknown_fields)]`，schemars **自动**在它生成的 schema 里发 `additionalProperties: false`；只读工具那边加的 `#[serde(flatten)] ignored: Ignored`（一个 `BTreeMap`，用来把多余字段收下来好报出去）则让它发 `additionalProperties: true`。实测：

```text
apply_patch      additionalProperties=False
exec_command     additionalProperties=False
stop_command     additionalProperties=False
list_files       additionalProperties=True
load_skill       additionalProperties=True
read_file        additionalProperties=True
read_notebook    additionalProperties=True
read_output      additionalProperties=True
search_text      additionalProperties=True
view_image       additionalProperties=True
workspace_info   （缺）   ← 它没有参数结构，server 也不发这个键
```

所以声明和解析是一次改对的，不用手写 schema。两份 `tools-list-*.json` 跟着补了这个键，`published_tool_tables_match_the_running_server` 现在也比它——schema 说收，服务端就收；说不收，服务端就拒。

## 5. 为什么不升 `ccnm.workspace-mcp/2`

这是收紧，不是加法，所以要说清楚为什么留在 `/1`：

- `/1` **从没承诺过**"未知字段会被忽略"。冻结说明列的破坏性变更是删工具、改 `disabled`/`read`/`coding` 三个值的含义、改权限判定或错误码语义——这一条都不占。
- 按 schema 生成参数的客户端**一个都不受影响**，而 schema 现在就是服务端执行的那套。之前受影响的只有"传了 ccnm 没声明的字段还指望它生效"的调用方，而那种调用方本来就在自欺。
- 错误码没变，拒绝是 `isError` 工具结果，发生在执行之前，不作废 coding 句柄。
- gld 那边不受影响：它的 `Offered` 早就只转发远端 schema 里有的参数。

主评审专门警告过"不能一边宣称完全兼容、一边全局开 `deny_unknown_fields`"。这里没有全局开：只读那七个仍然容错，而且收紧的那三个把 `additionalProperties: false` 发在了 `tools/list` 上——客户端读得到，这就是显式的那一半。

## 6. 这一轮没有证明的事

- **真实 Host 会不会给工具塞多余字段**：没有额度、没登录，所以这条收紧对真实 Claude Code / Codex 会话的影响**没有实测**。理论上它们按 `inputSchema` 生成参数，而 schema 现在明说不收；但这是推理，不是证据。
- Linux 上没跑。
- 没有 MCP 层的能力协商扩展（SEP 那条另立范围）。客户端要知道服务端收什么，只能读 `tools/list`。
