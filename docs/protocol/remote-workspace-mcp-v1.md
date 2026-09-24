# ccnm Remote Workspace MCP v1（契约）

> **实现缺陷披露（P51 发现，P52 修复）**：Runtime MCP relay 的 server leader 正常退出后，其同组子进程曾仍可写入，写锁却释放并允许第二个 writer（[审计 C51-01](../research/2026-09-23-lifecycle-and-docs-audit.md)）。P52 起关闭时清掉整组并确认，清不掉不交出写锁；macOS 已验证，Linux 未跑，离开进程组的后代仍在范围外（[P52 记录](../research/2026-09-25-p52-relay-group-cleanup.md)）。这是实现修复，不是新的许可语义，不修改冻结契约。

> **状态：`ccnm.workspace-mcp/1` 于 2026-09-11 冻结。**
> 依据是两轮真机：允许矩阵在真实 Claude Code 2.1.268 上跑过（[P11 记录](../research/p11-real-host-2026-09-11.md)、[证据](../research/p11-matrix-20260911.json)），远端真实项目 dogfood 在 Debian 13 / x86_64 的 Runtime 上跑过（[P12 记录](../research/p12-real-project-2026-09-11.md)、[证据](../research/p12-dogfood-20260911.json)）。
> **冻结的意思是**：往后加工具、加字段、加错误原因属于加法，可以；删工具、改 `disabled`/`read`/`coding` 三个值的含义、改权限判定或错误码语义要升到 `ccnm.workspace-mcp/2`。
> 验收范围、已知代价和**不作保证的 egress** 见[支持矩阵](../support-matrix.md)；这一版明确不做的东西见第 12 节。
> **冻结之后的加法**：2026-09-17（P36）加了第八个工具 `load_skill` 和 `prompts` 能力，用来把项目自带的 skills 交给模型和人，见第 5.1 节。原来七个工具的名字、参数和语义没有动。
> 2026-09-17（P37）给三个老工具加了可选参数：`search_text` 的输出模式、跨行、文件类型和 dotfile，`apply_patch` 的 op `write`，`exec_command` 的 `shell`，见第 5.2 节。不带新参数的调用和以前完全一样；`exec_command` 的 `required` 因此从 `["cmd"]` 变成空。同日修了两个行为缺陷：调用方的 `glob` 能把 dotfile 和 `.gitignore` 排除的文件带回搜索（同一节末尾，P37、P38）。
> 2026-09-17（P39）加了第九个工具 `view_image`，只读，把 workspace 里的图片作为 MCP 图片块交给模型，见第 5.3 节。
> 2026-09-17（P40）加了第十个工具 `read_notebook`（只读），`apply_patch` 多了 op `edit_notebook`，按 cell 读写 Jupyter notebook，见第 5.4 节。`read_file` 读 `.ipynb` 的结果不变，只多一条提示。
> 2026-09-18（P41）加了后台命令：`exec_command` 的 `run_in_background`、`read_output` 的 `wait_ms`，和只在 coding 模式有的第十一个工具 `stop_command`，见第 5.5 节。前台调用的结果不变。同时修了两个行为缺陷：`notifications/cancelled` 之后命令照跑、连接结束时远端 server 要等命令自己跑完才退出（第 6.2 节）。
> 2026-09-20（P42）第 6 节开头多了一段：四个时钟各管什么、调用方自己的调用预算为什么不是 Runtime 的运行时限，以及三句本来就成立却没写下来的话（session-bound 是权威语义、取消等待不等于取消命令、终态只有 Runtime 说了算）。**没有加工具、参数或行为**，只是把散在第 5.5、6.2 和第 8 节的规则收到一处，好让别的产品照着实现。
> 2026-09-20（P43）**修了一处写权会被错误交出的缺口**：会话结束时如果有命令停不掉（离开了进程组、又攥着管道，信号够不着），写入互斥不再被标成可用——下一个会话被拒，并看到还剩哪些 `output_ref`（第 7 节）。错误码没变，仍是 `CCNM_E_POLICY`；变的是**什么时候放锁**，而之前那种情况下放锁等于让两个写者同时改一棵树，本就违反第 4.4 节。锁标记里同时开始记 pid，只为让诊断说得准，不改变任何判定。同一节还写明了一条一直存在、此前一个字都没写过的边界：这把锁只在一个 state 目录内有效。
> 2026-09-20（P44）**服务端自己验参数，有副作用的三个工具收紧了**：`exec_command`、`apply_patch`、`stop_command`（连同 `files[]` 里的嵌套结构）不再接受它们没声明的字段，超上限的 `timeout_ms` / `preview_bytes` 也从"悄悄钳到上限"改成拒绝；只读那七个照旧接受，但结果里写明忽略了什么（第 5.6 节）。**这是收紧，不是加法。**它同时修好一处声明与实现不一致：`tools/list` 里以前一个 `additionalProperties` 都没有，等于声明"随便加字段"，现在每个工具都说实话。**不升 `/2` 的理由**：`/1` 从没承诺过"未知字段会被忽略"，而按 schema 生成参数的客户端一个都不受影响——schema 现在就是服务端执行的那套；拒绝发生在执行之前，是 `isError` 工具结果，不作废句柄也不改错误码。
> 2026-09-22（P49）**加了第十二个工具 `call_mcp_tool`**：把 Runtime 上的 MCP server 转给会话——项目 `.mcp.json` 里声明的，和执行账号给 Claude Code / Codex 装的。只在 coding 模式、而且那台机器上确实有能转的 server 时才出现在 `tools/list` 里；起 server 就是以执行账号跑程序，所以它过的门和 `exec_command` 一样。原来十一个工具不变。Runtime 配置 `[runtime_mcp]` 可以关。见第 5.7 节。
> 2026-09-22 `read_file`、`load_skill`、`call_mcp_tool` 的工具定义多了 `_meta` 键 `anthropic/maxResultSizeChars`：Claude Code 会把超过约 50 000 字符的结果存盘、只给模型预览，而受管会话读不回来（第 5 节末）。工具、参数、结果都没变。
> 2026-09-22（P48）**`load_skill` 也交出 Runtime 执行账号装好的 skills**（`~/.claude/skills`、`~/.agents/skills`、`~/.codex/skills`、`~/.claude/commands`），排在项目的后面；同名时装好的赢（照原生）。新增两个可选参数 `file`、`line`：读 skill 目录里的其他文件，长文件分段。不带新参数、执行账号 HOME 里又没装 skill 的调用和以前一样，只有工具的固定说明文字换了。Runtime 配置 `[machine_skills]` 可以整段关掉或按名字藏。见第 5.1 节。
> 2026-09-22（P45）**skill 的 frontmatter 改成照 Claude Code 2.1.278 的读法读**（共享库 `toexec-skill` 0.2.0），工具、参数、错误码都没变，变的是同一个 SKILL.md 读出来的结果：以 `` ` `` `@` `*` 开头的描述不再让 skill 被跳过，`argument-hint: [filename] [format]` 和 `[issue-number]` 的提示不再丢；`disable-model-invocation: yes` / `on` / `1` 现在生效；写了 `user-invocable` 却不是 true（空值、认不出的字）现在不登记成 prompt；同一个键写两遍后写的赢。宿主会整段丢弃的 frontmatter、重复的键，`load_skill` 的返回开头各多一行说明。见第 5.1 节。

面向的读者是**已经在本机跑着 Claude Code / Codex / 别的 MCP Host，但项目在另一台机器上的人**。它给你的不是一条裸 SSH 通道，而是一个绑定了 workspace 的远程项目工具集。

## 1. 它是什么，和 Managed Agent Runtime 有什么区别

ccnm 有两个入口，共用同一个 Runtime 执行核心：

```text
入口 A：Managed Agent Runtime（已实现，v1）
  你 → ccnm → 启动官方 Agent → Agent 通过 SSH stdio MCP 用远端项目

入口 B：Remote Workspace MCP（本文，v1.x 契约）
  你的 Agent 已经在跑 → 它通过 MCP 连上 ccnm bridge → bridge 通过 SSH 用远端项目
```

一句话区别：**A 里 ccnm 负责启动和管理 Agent；B 里 ccnm 根本不知道调用方是谁**，也不想知道。B 不读你的 Claude/Codex 登录，不管理它的生命周期，只回答工具调用。

```text
External MCP Host（Claude Code / Codex / 其他）
      │ stdio MCP：initialize / tools/list / tools/call
      ▼
ccnm mcp bridge <workspace>          ← 跑在 Host 这台机器上
      │ 一条持久 SSH（OpenSSH alias）
      ▼
ccnm internal mcp-serve @ Runtime Executor（ccrun）
      │
      └─ 权威 workspace：Git / 构建 / 测试 / 工具链
```

**bridge 不是 MCP server 的实现**，真正的 server 在远端 Runtime 上——就是 Managed 路径用的同一个进程、同一套七工具、同一份路径策略和同一把写入互斥锁。bridge 只做两件事：解析要连哪台机器，然后 `exec` 成那条 SSH——它不转发字节，它就是那条连接。

## 2. 一次连接从头到尾

```text
Host                     bridge                    Runtime Executor
 │  启动子进程              │                            │
 │ ────────────────────────>│                            │
 │                          │ 读本机配置：node alias      │
 │                          │ SSH ─────────────────────> │ 按自己的权威配置解析
 │                          │                            │ workspace / root / 模式
 │                          │                            │ coding 才抢写入互斥
 │  initialize              │                            │
 │ ────────────────────────>│ ─────────────────────────> │
 │  <── result（协议版本、工具能力、instructions）──────── │
 │  tools/list              │                            │
 │ ────────────────────────>│ ─────────────────────────> │
 │  <── 7 个或 10 个工具 ────────────────────────────────│
 │  tools/call              │                            │
 │ ────────────────────────>│ ─────────────────────────> │ 在项目目录里真的执行
 │  <── content / isError ───────────────────────────────│
 │  关闭 stdin（EOF）        │                            │
 │ ────────────────────────>│ 关 SSH ──────────────────> │ server 退出，guard 释放
```

**远端失败在 initialize 之前就发生了。** workspace 没开放、模式越权、写入互斥被占、远端 ccnm 太旧、Runtime 执行身份没通过安全门禁——这些都让连接**不回答 initialize 就结束**，退出码非 0，stderr 上是一条以 `CCNM_E_*` 开头的诊断。Host 那边看到的是"这个 MCP server 起不来"，而不是一个能连上却什么都做不了的 server。

## 3. 入口形状

### 3.1 命令定稿：`ccnm mcp bridge <workspace>`

```bash
ccnm mcp bridge myproject --node runtime --mode read
```

规划文档里出现过的 `ccnm mcp connect` 只是占位，**以本节为准**。为什么叫 bridge：

- 不叫 `serve`：真正 serve MCP 的是远端 `ccnm internal mcp-serve`。两个都叫 serve，出问题时没人说得清死的是哪一个进程。
- 不叫 `connect`：`connect` 听着像一次性动作，而这个进程要活到 Host 关掉它为止。
- `bridge` 说的就是它干的事：一头 stdio，一头 SSH，中间不加工。

### 3.2 参数

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `<workspace>` | 是 | 远端 Runtime **权威配置里**的 workspace 名 |
| `--node <name>` | 否 | 本机配置里的 Runtime node（决定用哪个 SSH alias）。只有一个时可省 |
| `--mode read\|coding` | 否 | 请求的权限，默认 `read`。见第 4 节 |
| `--config <FILE>` | 否 | 换一份本机配置，和其他子命令一致 |

**这些参数之外的东西一律不接受**，尤其是：

- 任意 `host` / `user` / 端口：只能用本机配置里已有的 node 和它的 OpenSSH alias。
- 任意 `root` 或绝对路径：远端 root 由 Runtime 自己解析，调用方给不了，也覆盖不了。
- 任何私钥路径或凭据：transport 认证是 OpenSSH 自己的事，不经过 MCP 参数。

**更不接受把这些做成 MCP tool 的输入。** 工具的输入只有 workspace 内的相对路径和命令，没有一个字段能指向另一台机器或另一个目录——否则"绑定一个 workspace"这句话就是假的。

### 3.3 Host 怎么配

Claude Code 的 `mcpServers` 形状：

```json
{
  "mcpServers": {
    "ccnm-myproject": {
      "command": "ccnm",
      "args": ["mcp", "bridge", "myproject", "--mode", "read"],
      "alwaysLoad": true
    }
  }
}
```

**实测过的只有 Claude Code**：`command`/`args` 这两行是 2.1.268 的 `-p` 模式验过的，`alwaysLoad` 是 2.1.269 上验的（见下一小节）。其他 Host 的配置格式各不相同（Codex 用 TOML），本文不声称验证过它们。契约只保证：一个进程、stdin/stdout 说 MCP、参数如上。

#### `alwaysLoad` 是干什么的

`alwaysLoad` 不是 MCP 标准字段，是 **Claude Code 自己的配置键**，写在 server 这一层，跟 ccnm 的协议无关——别的 Host 不认它。

**不写它会怎样**：Claude Code 默认把 MCP 工具放进「延迟加载池」——工具表里只留名字，模型想用得先调一次内置的 `ToolSearch` 把 schema 取回来。看着是好的（省上下文），但 ccnm 这种「不用它就干不了活」的 server，结果是**每个任务白多一个回合**。写上 `"alwaysLoad": true`，七个（或只读模式下四个）工具首轮就在工具表里。

**实测**（Claude Code 2.1.269 + ccnm 0.7.0，本机，未登录所以没发模型请求）：延迟池里的工具数 `coding` 模式 21 → 14、`read` 模式 18 → 14，少掉的正好是 ccnm 这一组。模型侧的收益是跨仓计划 workspace-kernel 的 V2-Q2 量的：3 个任务 × 2 组 × 3 次共 18 格，加了这个键的一组 `ToolSearch` 调用为 0、每个任务少一个回合，成功率、墙钟、token 都不更差。记录见 [P15 的实测](../research/p15-alwaysload-2026-09-16.md)。

**代价**：Claude Code 会把带 `alwaysLoad` 的 server 排进「首轮请求前必须连上」的那一组。bridge 要 ssh 到 Runtime，正常是几百毫秒（doctor 记录 555–581 ms）；**Runtime 睡着或网络不通时，你的 Claude Code 启动会卡在这里**，而不是先跑起来再说工具不可用。想避开就删掉这一行，功能不受影响，只是回到延迟加载。

Managed 路径（`ccnm` 自己启动的 Claude Code 会话）不需要也没有这个设置：那条路的 `--tools` 只列 workspace 开了的几个 Agent 功能（P46 起默认是 `WebSearch`，之前是空），`ToolSearch` 永远不在里面，所以本身就不可用，ccnm 的工具一直是全量加载的。P46 实测过：把 `ToolSearch` 加进 `--tools`，ccnm 的工具就全进了延迟加载池。

### 3.4 stdio 的硬规矩

- **stdout 只有 MCP 消息**，一个字节的杂音都不行。日志全部走 stderr，和 Managed 路径同一条规矩。
- **一个进程一个 workspace、一个 Runtime**。连接中途不能换机器、换目录、换模式；要换就让 Host 起另一个进程。
- bridge 不读 Host 的任何配置文件，也不猜调用方是 Claude 还是 Codex（见第 10 节）。

## 4. 授权模型

### 4.1 opt-in 在 Runtime 一侧，默认关

远端 Runtime 的权威配置里，每个 workspace 自己声明最大权限：

```toml
[workspaces.myproject]
root = "/Users/ccrun/projects/myproject"
agent = { node = "work", instance = "claude-main" }

# 外部 MCP 的最大权限。不写这一行 = disabled。
external_mcp = "read"      # disabled | read | coding
```

| 值 | 含义 |
| --- | --- |
| `disabled`（默认） | 外部 MCP 不能打开这个 workspace。连都连不上 |
| `read` | 只暴露确定只读的工具 |
| `coding` | 暴露七工具，并且**持有 workspace 写入互斥** |

**没写 `external_mcp` 就是关着的。** 这是故意的：能 SSH 到 `ccrun` 不等于能写这台机器上每一个项目。Managed 路径能用的 workspace，外部 MCP 不会因此自动能用。

### 4.2 请求不能高于配置，也不静默降级

```text
配置 coding + 请求 coding  → coding
配置 coding + 请求 read    → read（自愿降级，允许）
配置 read   + 请求 read    → read
配置 read   + 请求 coding  → 拒绝启动，CCNM_E_POLICY
配置 disabled + 任何请求   → 拒绝启动，CCNM_E_POLICY
```

**为什么越权时不降级成 read 让它先跑起来？** 因为 Host 配置里写着 `--mode coding` 的人以为自己能改文件。降级之后，模型会一路调 `apply_patch`、一路收到"没有这个工具"，最后大概率编一个"我已经改好了"。宁可在启动时就失败——那条错误人能看见。

### 4.3 两种模式的工具矩阵

| 工具 | `read` | `coding` |
| --- | --- | --- |
| `workspace_info` | ✅ | ✅ |
| `read_file` | ✅ | ✅ |
| `list_files` | ✅ | ✅ |
| `search_text` | ✅ | ✅ |
| `load_skill` | ✅ | ✅ |
| `view_image` | ✅ | ✅ |
| `read_notebook` | ✅ | ✅ |
| `read_output` | ❌ | ✅ |
| `apply_patch` | ❌ | ✅ |
| `exec_command` | ❌ | ✅ |
| `stop_command` | ❌ | ✅ |
| `call_mcp_tool` | ❌ | ✅（那台机器上有能转的 MCP server 时，P49） |

`read` 模式**永远没有 `exec_command`**。哪怕调用方保证"只跑 `cat`"也不行：任意 exec 能写磁盘、能联网、能起后台进程，靠解析命令字符串判断只读是假安全。将来真要"只读 shell"，那得靠独立的 OS sandbox 或者白名单可执行文件契约，不是靠猜。

`read` 模式也没有 `read_output`，理由不同：`output_ref` 只在**产生它的那个 session 的保留目录里**有意义（实现上 `read_output` 就是拿这个 ref 去 join 本 session 的目录）。read 模式没有 `exec_command`，永远产不出 ref，留着它就是一个必然失败的工具；而让它去解析**别的 session** 的 ref，就是跨会话泄漏。所以直接不发。

`stop_command`（P41）同理：read 模式起不了命令，也就没有可停的。`call_mcp_tool`（P49）和 `exec_command` 同一个理由：起一个 MCP server 就是以执行账号跑一个程序。

> 这比 ROADMAP P9.2 的下限（"read 模式没有 `apply_patch` 和 `exec_command`"）更窄。窄的那一格是 `read_output`，理由如上。

### 4.4 写入互斥：只有 coding 抢

```text
Managed Claude/Codex session ─┐
                              ├─ 同一个 Runtime workspace write guard
Remote MCP（coding 模式）  ────┘
```

`coding` 模式在**远端 server 打开的时候**就去拿这把锁——和 Managed session 走同一个函数、同一把锁文件，锁的键是工作树的规范化根路径，所以别名路径、嵌套路径和共享 `.git` 的 worktree 算同一棵树。拿不到就启动失败，不会出现"连上了但是写不了"或者更糟的"两个 Agent 同时改一棵树"。

`read` 模式不碰这把锁，因此多个 read 连接可以并存，也可以和一个 Managed coding session 并存。

### 4.5 首版不做多租户

transport 的认证边界是 **OpenSSH identity + 独立的 Runtime OS 账号**。一个共享的 `ccrun` key 不是多租户授权：拿到那把 key 的人得到的是那个账号的全部权限。细粒度 token、按用户区分的审计、第三方共享，全部不在首版。

## 5. 工具语义与 annotations

七工具的实现和 Managed 路径**完全共用**，不复制第二份。下面这张表是 Runtime 内部真正用于授权的语义，以及发给 Host 的标准 MCP annotations：

| 工具 | access | `readOnlyHint` | `destructiveHint` | `idempotentHint` | `openWorldHint` |
| --- | --- | --- | --- | --- | --- |
| `workspace_info` | read | `true` | — | — | `false` |
| `read_file` | read | `true` | — | — | `false` |
| `list_files` | read | `true` | — | — | `false` |
| `search_text` | read | `true` | — | — | `false` |
| `load_skill` | read | `true` | — | — | `false` |
| `view_image` | read | `true` | — | — | `false` |
| `read_notebook` | read | `true` | — | — | `false` |
| `read_output` | read | `true` | — | — | `false` |
| `apply_patch` | write | `false` | `true` | `false` | `false` |
| `exec_command` | exec | `false` | `true` | `false` | `true` |
| `stop_command` | exec | `false` | `true` | `false` | `false` |
| `call_mcp_tool` | exec | `false` | `true` | `false` | `true` |

（按 MCP 规范，`destructiveHint` / `idempotentHint` 只在 `readOnlyHint` 为 `false` 时才有意义，所以只读那几行留空。）

三条规矩：

1. **annotations 只改善 Host 的审批 UX，不是门禁。** 一个完全忽略它们的 Host，得到的授权结果必须和尊重它们的 Host 一模一样——真正的门禁是 OS 身份、workspace 绑定、access mode 和写入互斥。
2. **`exec_command` 永远按 destructive + open-world 处理。** 不因为这次的命令"看起来只是 `ls`"就动态改注解。注解是工具的属性，不是某次调用的属性。
3. `apply_patch` 不是 open-world：它只能改这个 workspace 里的文件。但它是 destructive——update、write 会替换内容，delete 会删文件。

另外，Managed 路径上 `exec_command` 和 `call_mcp_tool`（P49）会带一个 `_meta` 键 `anthropic/requiresUserInteraction`（只在有人坐在终端前的交互式 session 里带，而且该 workspace 没有写 `allow_unattended_exec`）。**外部 MCP 永远不发这个键**：bridge 不知道 Host 那头有没有人，冒充知道比不说更糟，所以那个开关对 bridge 没有任何影响。

`read_file`、`load_skill`、`call_mcp_tool` 三个工具在两个入口上都带 `_meta` 键 `anthropic/maxResultSizeChars: 200000`（2026-09-22 起）。不带的话，Claude Code 2.1.278 收到超过约 50 000 字符的结果，不交给模型，而是存到本机磁盘、只给模型 2 KB 预览和一个路径，要它用 `Read` 去读；受管会话没有 `Read`，后面的就丢了（零额度实测：50 000 字节原样到达，52 000 字节只剩预览；声明之后 150 000 字节原样到达）。这三个工具一次最多回 64 KiB，其余工具最多 32 KiB，到不了那条线。**这个键不改变 ccnm 回多少**，第 8 节的上限照旧，只决定已经切好的一页能不能完整到模型面前。

### 5.1 `load_skill` 与 prompts：项目自带的 skills（P36 新增）

**skill 是什么**：项目在 `.claude/skills/<名字>/SKILL.md` 里写下的"这类任务该怎么做"——开头一段 YAML（名字、描述、参数），后面是给模型的正文，旁边可以带脚本和参考文件。`.claude/commands/*.md` 是同一种格式的单文件版本。官方 CLI 靠"当前目录"发现它们；项目在远端时 CLI 的当前目录不在项目里，一个都发现不了，所以由 Runtime 这一侧来发现。

**在哪找**：项目里的都相对 workspace 根，走和 `read_file` 同一套路径策略；装好的在 Runtime 执行账号的 HOME 下（P48 起，Runtime 配置 `[machine_skills] enabled = false` 时不找）。

| 位置 | 形状 |
| --- | --- |
| `~/.claude/skills/<名字>/SKILL.md`、`~/.agents/skills/<名字>/SKILL.md`、`~/.codex/skills/<名字>/SKILL.md` | 装好的 skill（P48）。可以是符号链接，跟着走 |
| `.claude/skills/<名字>/SKILL.md` | skill |
| `.agents/skills/<名字>/SKILL.md` | skill（跨 Agent 的通用写法，Codex 找的是这里） |
| `~/.claude/commands/**/*.md`（最深 3 层） | 装好的命令（P48） |
| `.claude/commands/**/*.md`（最深 3 层） | 命令；名字是文件名 |

重名时按上表从上到下谁先谁赢——装好的赢过项目的，这是 Claude Code 2.1.278 的原生规则（实测，toexec `evidence/v3-parity/machine-skills/`）。输的那个不会悄悄消失：不带名字调 `load_skill` 返回的完整列表末尾会写出它的路径和原因。读不了的 frontmatter、没有描述的文件、经 symlink 指到 workspace 外面的**项目** skill 目录，同样列在那里。两个例外不列：`[machine_skills] hidden` 里的名字（和没装一样，项目里同名的那个就回来了）；同一个装好的 skill 经符号链接出现第二次（skills CLI 就是这么装的）。目录是 HOME 本身或文件系统根的"skill"不收。最多 100 个，超了先丢装好的；单个文件超过 1 MiB 不读。

**`load_skill` 怎么用：**

| 调用 | 返回 |
| --- | --- |
| 不带 `name` | 完整列表：项目的在前、装好的在后，每个 skill 的名字、参数提示、描述（最多 1536 字符）、文件路径；只能由人启动的、没被收进来的也列出并说明原因 |
| `name`（可选 `arguments`，一个字符串） | 这个 skill 的正文，见下 |
| `name` + `file`（可选 `line`，从 1 数） | skill 目录里的一个文件（P48）：一次最多 64 KiB，在行边界截断，末尾写明下一段用 `line` 从第几行接着读。`file=SKILL.md` 读 skill 本身 |

返回的正文前面有几行方括号，是 server 加的：skill 在哪个文件、`${CLAUDE_SKILL_DIR}` 是哪个目录、哪些命令**没有被执行**、哪些 frontmatter 在这里不起作用。样例见 [`call-load-skill-ok.json`](fixtures-mcp/call-load-skill-ok.json)。正文本身：

- frontmatter 去掉；`$ARGUMENTS`、`$ARGUMENTS[N]`、`$N`、声明过的 `$name` 按 Claude Code 2.1.273 的实际规则替换（没给到的 `$N` 原样留着——正文里的 `awk '{print $1}'` 因此不会被抹掉）；
- `${CLAUDE_SKILL_DIR}` 换成 skill 目录的 **workspace 相对路径**（装好的换成它在 Runtime 上的**绝对路径**），`${CLAUDE_PROJECT_DIR}` 换成 `.`。项目 skill 的脚本和参考文件就是 workspace 里的普通文件：模型用 `read_file` 读、用 `exec_command` 跑，所以它们在 Runtime 上、以执行身份、受同一套写入互斥和 `exec_sandbox` 约束执行。装好的在 workspace 外面，`read_file` 读不到，用 `file` 读；脚本同样由 `exec_command` 按那个绝对路径跑；
- skill 目录里还有别的文件时，开头多一行列出它们（不含点文件，最多 100 个）；
- 超过 64 KiB 在行边界截断，并写明从哪一行接着读：项目的用 `read_file`，装好的用 `load_skill` 的 `file=SKILL.md` 加 `line`。

**`file` 只在这个 skill 自己的目录里读**（规则在共享库 `toexec-skill` 0.3.0 的 `dir` 模块，gld 的 `get_skill` 用同一份）：`..`、绝对路径、解析后跑到目录外的符号链接、路径上任何一段以 `.` 开头的——`CCNM_E_POLICY`；不存在、是目录、超过 1 MiB、不是 UTF-8——`CCNM_E_INVALID_ARGS`，不是文本的会给出它在 Runtime 上的绝对路径，好让模型用 `exec_command` 就地使用。`read` 模式下没有 `exec_command`、`read_file` 又出不了 workspace，这是读到 workspace 外面的**唯一**一条路，所以边界卡得这么死；不想要就在 Runtime 上关掉 `[machine_skills]`。命令是单个文件，对它用 `file` 报 `CCNM_E_INVALID_ARGS`。

**目录放在哪**：`load_skill` 自己的 `description` 里。它的前半段是固定文本，后半段是这个 workspace 的 skill 目录（名字、参数提示、折成一行并截到 200 字符的描述），整段不超过 2048 个 UTF-16 码元——Claude Code 2.1.273 对每个工具的 description 只留这么多（实测；Codex 0.154.0 不截）。放不下的 skill 只列名字。**这是七个老工具没有的性质：`description` 随 workspace 变。** 没有 skill 时它是固定文本，[`tools-list-*.json`](fixtures-mcp/tools-list-read.json) 逐字节比对的就是那一版。目录在会话开始时定下来（Host 整个连接期间都留着 `tools/list` 的结果）；调用时重新扫描，所以会话中途新写的 skill 能加载，只是要到下一个会话才出现在目录里。

**三条和官方 CLI 不一样的地方，都是故意的：**

1. **`` !`命令` `` 注入不执行。** 官方 CLI 在加载 skill 时先跑这些命令、把输出填进正文。这里原样保留，并在开头列出行号和命令，模型需要就自己用 `exec_command` 跑。理由：一次"读"调用不该触发仓库指定的命令——那会绕过 `exec_command` 上的人工确认（`allow_unattended_exec` 管的那一层），`read` 模式下更是直接变成了执行。
2. **`allowed-tools`、`disallowed-tools`、`hooks`、`model`、`effort`、`context`、`agent`、`shell` 不起作用**，出现时在返回文本里点名。ccnm 改不了 Host 的权限和模型，也不在 Agent 那台机器上执行任何来自仓库的东西。Claude Code 自己对经 MCP 来的 skill 也不认 `hooks` 和 `allowed-tools`。
3. **装好的 skill 的文件用 `file` 读**，而不是像原生那样给一个路径让模型自己去读：`read_file` 只读 workspace（P48 之前这里写的是"执行账号 HOME 下的用户级 skills 不读"）。

`disable-model-invocation: true` 的 skill 不进目录，`load_skill` 拒绝它（`CCNM_E_POLICY`）；`user-invocable: false` 的不登记成 prompt。

**frontmatter 照 Claude Code 2.1.278 的读法读**（P45 起，差分证据在 toexec 仓库 `evidence/x08-skill-frontmatter/`）：

- 先按 YAML 严格读；读不了，照宿主的规则给顶层带特殊字符的值加引号再读——所以 ``description: `git` helper``、`argument-hint: [filename] [format]` 都读得出来，结果和宿主一样。宿主两步都读不了时会把整段 frontmatter 当空的（名字、描述、开关全丢）；这里宽松读出来，并在 `load_skill` 返回的开头写一行 `this frontmatter is not valid YAML`，好让作者知道原生客户端里它不生效。
- 两个开关不对称，照宿主：`disable-model-invocation` 只有真值才生效；`user-invocable` 没写才默认可用，写了就只有真值才算——空值、认不出的字都会让它不登记成 prompt。真值认 `true` / `yes` / `on` / `1`，不分大小写。
- 同一个键写了两遍，后写的赢（宿主如此），返回开头点名是哪几行。键名只差大小写或 `-` / `_` 时这里也认（宿主只认原样）——对两个开关，这是照作者本意、更保守的一侧。
- `argument-hint` 写成 YAML 列表（官方例子 `[issue-number]`）时，提示是各项用逗号接起来，和宿主显示的一样。

**prompts**：每个可由人启动的 skill / 命令同时登记成一个 MCP prompt（[`prompts-list-ok.json`](fixtures-mcp/prompts-list-ok.json)、[`prompts-get-ok.json`](fixtures-mcp/prompts-get-ok.json)），`prompts/get` 返回的就是 `load_skill` 会返回的那段文本。Claude Code 把它变成斜杠命令 `/mcp__ccnm__<名字>`（server 在 Host 配置里叫别的名字，中间那段就跟着变）。prompt 的参数是 skill 在 frontmatter 的 `arguments` 里声明的名字；一个都没声明时是单个 `arguments`。**Claude Code 把人敲的参数按空白切开、依次对应声明的参数，多出来的词被它丢掉**（2.1.273 实测）——要传多个词，skill 得声明多个参数。Codex 0.154.0 连上之后只调 `tools/list`，看不到 prompts，所以 prompts 是锦上添花，`load_skill` 才是主通道。

**没做的**：MCP 官方的 skills 扩展（SEP-2640，`skills/list` / `skill://` 资源）。Claude Code 里它的客户端已经写好，但挂在一个默认关闭的开关后面，现在对哪个 Host 都不生效；它是另一个阶段。依据见 [P36 记录](../research/p36-skills-surface-2026-09-17.md)。

### 5.2 搜索模式、整文件覆盖、一行 shell（P37 新增）

对齐的是 Claude Code 2.1.273 的 Grep、Write、Bash（读的是它打包代码里的工具定义，依据见 [P37 记录](../research/p37-execution-surface-batch1-2026-09-17.md)）。三处都是可选参数或新操作，不带它们的调用和以前一模一样。

**`search_text` 多了四个参数：**

| 参数 | 取值 | 效果 |
| --- | --- | --- |
| `output_mode` | `content`（默认）、`files_with_matches`、`count` | 后两种只返回路径，或 `路径:匹配行数`。这时 `max_results` 数的是文件，`context_lines` 不起作用 |
| `multiline` | 布尔，默认 `false` | 匹配可以跨行，正则里的 `.` 也匹配换行（`rg -U --multiline-dotall`）。跨行的结果按行展开，每行照样受单行 512 字节、总量 32 KiB 限制。query 里带换行却没开它，报 `CCNM_E_INVALID_ARGS` |
| `type` | rg 的文件类型名，如 `rust`、`py`、`ts` | 只搜这类文件；rg 不认识的名字报 `CCNM_E_INVALID_ARGS`。和 `glob` 同时给时两个条件都要满足（P37 曾经拒绝这种组合，P38 起不再拒绝） |
| `include_hidden` | 布尔，默认 `false` | 也搜 dotfile 和点开头的目录。`.git` 不管怎么设都不搜 |

计数按"匹配的行"算，和 `rg --count` 一样：一行里出现两次算一行，一个跨行匹配算一次。

和原生 Grep 不一样的地方：默认仍是 `content`（原生默认只列文件，但冻结时这里就是内容，改默认值要升版本）；dotfile 默认不搜（原生默认搜，只排除版本库目录）；没有分开的 `-A` / `-B`、`head_limit` / `offset` 分页和只输出匹配部分的 `-o`。

**`apply_patch` 多了 op `write`**：`{"op": "write", "path": …, "version": …, "content": …}` 用 `content` 整体替换一个**已经存在**的文件。和 `update` 一样必须带 `read_file` 给的 `version`，过期报 `CCNM_E_STALE_EPOCH`；文件不存在时报 `CCNM_E_INVALID_ARGS` 并指向 `add`——一个带着版本号来的 `write` 碰上文件没了，说明有人删了它，不该当成"那就新建"。原子替换、中断日志、失败回滚、保留文件权限，和 `update` 走同一条路。原生 Write 不存在的文件也能写；这里新建仍然是 `add`。

**`exec_command` 多了 `shell`**：一行命令，Runtime 用 `bash -c <这一行>` 执行，管道、重定向、`&&`、`cd sub && …` 都能用。和 `cmd` 二选一：都给或都不给报 `CCNM_E_INVALID_ARGS`；JSON Schema 的 `required` 表达不了"恰好一个"，所以 `required` 是空的，由 server 在调用时检查。

- **不放宽任何东西。** 模型本来就能发 `{"cmd": ["bash", "-c", "…"]}`，`shell` 就是这条 argv：同一道执行门、同一个人工确认、开了 `exec_sandbox` 时同样被包起来。
- **是 bash，不是 sh，也不退回 sh。** Runtime 上找不到 bash 报 `CCNM_E_DEPENDENCY`，消息里说改用 `cmd`。Debian 的 `sh` 是 dash，`[[ ]]`、`set -o pipefail` 在那里意思不同，悄悄换解释器比报错更糟。
- 结果第一行 `$ …` 是原样的那一行。工作目录不跨调用保持（原生 Bash 会保持），每次用 `cwd` 指定。

**同日修的两个行为缺陷。** 都是 `glob` 让结果多出了工具说明里写着"永远不搜"的东西，修完模型看到的结果只会变少，属于修正而不是加法，不升版本。

1. **dotfile 和 `.git`（P37）**：`glob` 写成 `*`、`**`、`**/*` 这类能匹配目录的形状时，dotfile 会被搜出来发给模型，`.git/` 也会被 rg 扫一遍（那里的命中由事后检查丢掉，没有发出去）。原因是 rg 15.2.0 里 glob 优先于"不搜隐藏文件"，多个 glob 同时命中时最后一个说了算，而排除规则排在调用方 glob 前面。现在排除规则放在最后。
2. **`.gitignore`（P38）**：给了 `glob` 就会搜 `.gitignore` 排除的东西——`**` 搜进 `target/`、`node_modules/`，只能匹配文件的 `**/*.yml` 也会搜到被忽略的 `secret.yml`。rg 里一个 glob 命中了路径，就不再看 `.gitignore`。现在调用方的 `glob` 不再作为 rg 的 `--glob`：文件名部分交给 rg 缩小范围（`--type-add`，这种过滤排在 `.gitignore` 之后），整条 glob 由 ccnm 按 rg 原来的规则过滤结果。

**`glob` 的含义没有变**，仍是 rg（也就是 gitignore）的规则，和 `list_files` 的 `glob` 不一样：

| 写法 | 匹配 |
| --- | --- |
| 不含 `/`，如 `*.rs` | 任意深度的文件名 |
| 含 `/`，如 `src/*.rs` | 从 workspace 根算起的整条路径，`*` 不跨目录；`search_text` 的 `path` 不改变这个起点 |
| 以 `/` 结尾，如 `src/` | 只匹配目录，所以什么文件都匹配不到 |
| 以 `!` 开头，如 `!*.md` | 排除规则，其余文件照搜 |

"含不含 `/`"看的是写出来的整条 glob：`{*.rs,src/*.py}` 里的 `*.rs` 也只匹配根目录下的文件。唯一有意变了的写法是 `./src/*.rs`：rg 当年匹配不到任何文件，现在和 `src/*.rs` 一样。

代价：文件名部分是 `**` 的 glob（如 `src/**`）没法交给 rg 缩小范围，rg 会读所有没被忽略的文件、由 ccnm 丢掉不匹配的，慢一些，仍受 60 秒超时约束。依据见 [P38 记录](../research/p38-glob-gitignore-2026-09-17.md)。

### 5.3 `view_image`：看 workspace 里的图片（P39 新增）

**怎么用**：`{"path": "shots/login.png"}`。路径规则和 `read_file` 一样。成功时结果里有两个内容块：

```json
{"content": [
  {"type": "text",  "text": "shots/login.png: PNG, 48213 bytes"},
  {"type": "image", "data": "<文件原样的 base64>", "mimeType": "image/png"}
]}
```

完整样例见 [`call-view-image-ok.json`](fixtures-mcp/call-view-image-ok.json)。

| 情况 | 结果 |
| --- | --- |
| PNG、JPEG、GIF、WebP，不超过 3932160 字节 | 上面那两个块。格式看文件头，不看扩展名 |
| 超过 3932160 字节 | `CCNM_E_INVALID_ARGS`，消息里给出在 workspace 里缩一份小图的命令（`sips -Z 2000` / ImageMagick `convert -resize`） |
| SVG | `CCNM_E_INVALID_ARGS`，指向 `read_file`（SVG 是文本） |
| 其他格式（BMP、TIFF、HEIC……）、普通文件 | `CCNM_E_INVALID_ARGS`，说明要先用 `exec_command` 转换 |
| 目录、fifo、socket、设备，workspace 外的路径 | 和 `read_file` 相同的错误 |

`read_file` 读到这四种图片时，报 `CCNM_E_INVALID_ARGS` 并指向 `view_image`（P39 起；原来按"二进制文件"拒绝，前 8 KiB 没有 NUL 的 JPEG 还会被当成乱码文本读出来）。

**为什么是 `image` 块、不缩放、只认这四种**——依据是零额度实测（toexec 仓库 `evidence/v3-parity/media-surface/`）：

| | Claude Code 2.1.273（读打包代码） | Codex 0.154.0（本机假模型真的调工具） |
| --- | --- | --- |
| `image` 块 | 当图片交给模型；超过 2000×2000 或字节预算就自己缩放、压缩 | 变成 `input_image`（`detail: high`）交给模型，原样转发。真实 ccnm 二进制实测过，图片逐字节一致 |
| `resource` 块里的 blob | **写到跑 CLI 的那台机器的磁盘上**，模型只拿到那台机器上的路径 | 整个块被序列化成文本，base64 原样塞进上下文 |
| 这四种以外的图片类型 | 同 `resource` blob，落盘 | 没测 |

所以只发 `image` 块；Claude Code 会自己缩放，ccnm 不在 Runtime 上缩放、不加图像处理依赖；上限取 Claude Code 能收的最大值；别的类型会被 Claude Code 落到持有凭据的那台机器上，所以不发。

**Codex Code Mode 的差别**：ccnm 受管的 Codex 会话默认开着 Code Mode，模型不直接调工具，而是写 JS 调；工具结果是个对象，模型要自己写 `image(result.content[1])` 才会看到图（实测过这一步确实生效）。工具说明里写了这一句；模型会不会照做没有验。

### 5.4 `read_notebook` 与 `edit_notebook`：Jupyter notebook 按 cell 读写（P40 新增）

**读**：`{"path": "analysis.ipynb", "start_cell": 0}`（`start_cell` 可省）。结果里文本块和图片块按 notebook 里的顺序交替出现，样例见 [`call-read-notebook-ok.json`](fixtures-mcp/call-read-notebook-ok.json)：

```text
[notebook analysis.ipynb: 5 cells, python]
<cell id="b7d3a901" index="1" type="code" execution_count="1">
print("rows:", len(df))
</cell>
<output cell="b7d3a901" type="stream" name="stdout">
rows: 3
</output>
…
[cells 0-4 of 5 shown, end of notebook; version 2396-…]
```

- 代码 cell 的输出跟在 cell 后面：`stream` 原文、`execute_result` / `display_data` 的 `text/plain`、`error` 的名字、消息和去掉终端颜色码的 traceback。输出里的 PNG / JPEG 作为 MCP `image` 块插在那个位置（形状同第 5.3 节）；HTML、LaTeX、SVG 输出不渲染。
- 放不下时停在 cell 边界，页脚写 `continue with start_cell=N`。单个 cell 比整个预算还大时照样给出，但截断并说明；单个输出超过 4 KiB 截断，说明完整内容在 `read_file` 能看到的 JSON 里。
- 页脚的 `version` 和 `read_file` 的是同一种，`edit_notebook` 要它。
- 老 notebook（nbformat 4.5 之前）的 cell 没有 id，显示成 `cell-N`（N 是序号），编辑时照样能用。nbformat 3 及更早报 `CCNM_E_INVALID_ARGS`，消息里给转换命令。

**改**：`apply_patch` 的一项文件变更：

```json
{"op": "edit_notebook", "path": "analysis.ipynb", "version": "<read_notebook 给的>",
 "cells": [
   {"cell_id": "c4e8f7aa", "new_source": "df.describe()"},
   {"cell_id": "b7d3a901", "edit_mode": "insert", "cell_type": "markdown", "new_source": "## 数据概览"},
   {"cell_id": "d0f19b3c", "edit_mode": "delete"}
 ]}
```

| 字段 | 含义 |
| --- | --- |
| `edit_mode` | `replace`（默认）、`insert`、`delete` |
| `cell_id` | `read_notebook` 显示的 id。`replace`、`delete` 必填；`insert` 时新 cell 放在它后面，不给就放在最前面 |
| `new_source` | `replace`、`insert` 必填 |
| `cell_type` | `code` 或 `markdown`。`insert` 必填；`replace` 时给了就改类型 |

`cells` 按顺序应用，后一项看到的是前一项改完的 notebook（先删一个 cell，后面的 `cell-N` 序号跟着变）。任何一项出错，整个 `apply_patch` 什么都不写，错误消息指出是 `cells[i]` 哪一项、缺什么或者有哪些 id。和其他操作一样原子提交、带中断日志和回滚。

语义照 Claude Code 2.1.273 的 NotebookEdit（读的是它的打包代码）：替换代码 cell 时清空 `outputs`、`execution_count` 置空；nbformat ≥ 4.5 时新 cell 得到一个 8 位十六进制 id。**两处不同**：改类型时去掉新类型不允许的键（nbformat 的 schema 不许 markdown cell 有 `outputs`，Claude Code 会留着）；`source` 按 nbformat 的习惯写成行数组，而不是一个长字符串。

**写回的文件长什么样**：键按名字排序、非 ASCII 不转义，缩进宽度和结尾换行照原文件——和 nbformat（Jupyter 用它写文件）的写法一致。用 nbformat 5.11.1 核对过（[`check_with_nbformat.py`](../../tests/fixtures/notebook/check_with_nbformat.py)）：样例是 nbformat 自己的写法；经 `edit_notebook` 做五项编辑后的文件通过 nbformat 的 schema 校验，而且 nbformat 再写一遍和 ccnm 写的逐字节一致。已知差别：元数据里的浮点数，Python 写 `1e-05`，这里写 `1e-5`。

**为什么不直接让 `read_file` 按 cell 显示**：`read_file` 返回 notebook 的 JSON 文本，已经有人照着这份文本用 `update` 改 notebook；换成 cell 视图，这些改动就对不上了——冻结契约下这算改语义。所以 `read_file` 的结果不变，只在末尾多一条提示，指向 `read_notebook` 和 `edit_notebook`。

### 5.5 后台命令：`run_in_background`、`wait_ms`、`stop_command`（P41 新增）

**怎么用**，三步：

1. `exec_command` 加 `"run_in_background": true`。调用马上返回 `output_ref`，命令在远端接着跑（[样例](fixtures-mcp/call-exec-background-ok.json)）。
2. `read_output` 拿这个 ref 读。命令还在跑时，读到末尾不算结束：脚注写"到目前为止"和下次从哪个 offset 读，最后一行是命令的状态（[样例](fixtures-mcp/call-read-output-running.json)）。给 `wait_ms` 就先等它结束——结束立刻返回，等满了也返回，最多 600000 毫秒。
3. `stop_command` 停掉它：命令和它起的所有进程（同一个进程组）先收到 TERM，2 秒后还在就 KILL；返回它怎么结束的（[样例](fixtures-mcp/call-stop-command-ok.json)）。已经结束的命令再停不算错，照实报状态。

**它活多久**：

- 没给 `timeout_ms` 就没有期限；给了就到点杀，上限和前台一样是 600000。
- **活不过连接。** 连接结束（Host 关掉、SSH 断、Managed 会话 `/mcp Reconnect`）时，远端 server 先停掉这条连接起的所有命令——前台后台都算，停法和 `stop_command` 一样——再放写入互斥、退出。
- 同一条连接最多 **8 个**后台命令同时在跑，第 9 个报 `CCNM_E_INVALID_ARGS`，消息里列出在跑的 `output_ref`。原因见第 8 节"保留输出"：在跑的命令的输出不参与回收，8 个最多多占 1 GiB。
- 其余和前台完全一样：执行门、人工确认、`exec_sandbox`、环境变量剥离、每个流 64 MiB。

**状态行**说的是这些之一：

| 状态行 | 意思 |
| --- | --- |
| `running for 12.3 s` | 还在跑 |
| `exited 0 after 12.3 s` | 自己结束了，带退出码 |
| `killed on its timeout after 600.0 s` | 到了 `timeout_ms` |
| `stopped by stop_command after 12.3 s` | 被 `stop_command` 停掉 |
| `stopped when its session ended, after 12.3 s` | 连接结束时被停掉（只有重连后的 Managed 会话读得到这一行） |
| `no longer running, and its exit status is unknown: …` | 跑它的 server 没来得及记下就没了（被 `SIGKILL` 之类）；这种情况下它起的进程组没人收，可能还在 |

**为什么命令结束时不通知模型**：MCP 里没有现成的办法让 server 叫醒模型。Claude Code 2.1.273 能让 MCP server 往会话里推消息（channels），但要组织管理员打开；MCP 标准的长任务扩展（SEP-2663）客户端代码在，入口是关着的。所以只能由模型来问，`wait_ms` 让它不必空转轮询。

**两个 Host 等一次调用多久**（决定 `wait_ms` 能给多大）：Claude Code 2.1.273 对 stdio MCP server 的空闲超时是 30 分钟；交互会话里一次调用超过 120 秒，它自己把这次调用转到后台，结果照样回来。Codex 0.154.0 默认配置等满一次 75 秒的调用没有超时，更长的没测。依据见 [P41 记录](../research/p41-background-commands-2026-09-18.md)。

**不做**：给命令喂 stdin、分配终端（Codex 不开 tty 时 stdin 也是关的）；逐行推送输出（Claude Code 的 Monitor）；前台超时转后台（前台照旧到点杀）；命令活过连接。

### 5.6 参数怎么验：有副作用的拒绝，只读的说一声（P44 新增）

**服务端自己验，不指望别人先拦。**中间层（比如 hub）通常会按工具表拦一道，但外部 CLI 可以绕过它直连这个 server——那条路上没有第二个人检查。

两条规矩，按工具分：

| | `exec_command`、`apply_patch`、`stop_command`、`call_mcp_tool`（P49） | 其余七个（只读） |
| --- | --- | --- |
| 收到它没声明的字段 | **拒绝**，并列出它认识的字段名 | 照常回答，结果末尾多一行 `[ignored, this tool has no such argument: …]` |
| `tools/list` 里的 `additionalProperties` | `false` | `true`（`workspace_info` 没有参数结构，不发这个键） |
| 超过上限的数值 | **拒绝**，并说该改用什么 | 钳到上限，并写明钳了（比如 `read_output` 的 `wait_ms`） |

**为什么分开**：一个没人读的字段，在写和执行这边意味着**命令按调用方没同意的条件跑了**——它以为自己传了 `sandbox: false`，而那个字段根本没人看。嵌套里的也一样（`files[]` 的每一项），所以 `files[0]` 多一个 `mode` 同样被拒，而且是在任何东西落盘之前。读这边不会这样：一次读最多是结果不如预期，为一个多余字段让整次读失败反而更糟，所以它回答，但不装作看见了那个字段。

**声明和解析是同一件事。**上表第二行是 `tools/list` 里真发出去的，`published_tool_tables_match_the_running_server` 对着真实 server 比它。schema 说收，服务端就收；说不收，服务端就拒。

不认识的枚举值（`op`、`output_mode`、`edit_mode`）、类型不对、必填缺失，一直都是拒绝，并且把合法取值列出来——这三条不分工具。

**所有这些拒绝都是工具结果（`isError`），不是 JSON-RPC 错误。**所以一次参数写错既不会作废 coding 会话的句柄，也不该被调用方当成传输故障。

### 5.7 `call_mcp_tool`：Runtime 上的 MCP server（P49 新增）

把项目那台机器上的 MCP server 转给会话：数据库这类只能跑在项目旁边的 server 是它存在的理由。

**哪些 server**，按这个顺序，同名时先列的赢：

| 从哪读 | 读什么 |
| --- | --- |
| workspace 根下的 `.mcp.json` | `mcpServers`（Claude Code 的 project 级写法） |
| 执行账号的 `~/.claude.json` | 顶层 `mcpServers`（Claude Code 的 user 级） |
| 执行账号的 `$CODEX_HOME/config.toml`（默认 `~/.codex/config.toml`） | `[mcp_servers.*]` |

项目的压过装好的，这是 Claude Code 自己的规矩（project 级压过 user 级）；Claude 的压过 Codex 的只是得定一个。`${VAR}` / `${VAR:-默认值}` 照 Claude Code 展开，但**像凭据的变量名一律当没设**（Agent 的登录变量和 `*_TOKEN`、`*_API_KEY` 这类），这样的 server 列出来、标明缺什么、不起。**只转 stdio 的**：HTTP server 不需要跑在项目旁边，列出来并写明原因。

**参数**（全都可选，`additionalProperties: false`）：

| 参数 | 是什么 |
| --- | --- |
| `server` | 不给：列出所有 server 和它们的状态（没起 / 在跑、有哪些工具 / 不转的原因），**什么都不起** |
| `tool` | 给了 `server` 不给它：列出这个 server 的工具、各自的参数表和 server 自己的说明（这一步才起它） |
| `arguments` | 给了 `server` 和 `tool`：调用那个工具，这是它自己的参数对象，原样转过去 |

工具说明的固定部分是 `Use an MCP server on the runtime machine: …`，末尾是会话开始时能转的 server 名字（`Servers here: a, b.`），名字放不下 2048 个 UTF-16 码元时写"还有几个"。**有能转的 server 时才出现在 `tools/list`**，所以它不在两份 `tools-list-*.json` fixture 里（那个测试 server 没有可转的）；名字、参数、说明由中立客户端测试（`tests/test_remote_workspace_mcp.py`）和 `mcp::relay` 的单元测试核对。

**过的门和 `exec_command` 一样**（起 server 之前；只列清单不起东西，不过门）：

- 执行门（第 4.1 节的执行身份检查）、Runtime 凭据检查——拒绝时报 `CCNM_E_POLICY`，措辞写明"和 exec_command 同一个理由"；
- 工作区配了 `exec_sandbox` 就套同一个 OS 沙箱：server 只能写 workspace（不含 `.git`）、`$TMPDIR`、`/tmp`，**没有网络**；
- 环境变量按命令的规矩清理；server 配置里自己的 `env` 照传（token 也传，那是写配置的人给它的），但 Agent 的登录变量不传；
- 工作目录是 workspace 根（Codex 的 `cwd` 按它算）；
- Managed 路径上有人值守时每次调用都问人（`requiresUserInteraction`，见上面第 5 节末）。

**结果**：server 的内容块原样交回（文字、图片），`isError` 原样保留。有文字时**去掉内容相同的 `structuredContent`**（按 P11 实测，Claude Code 两者都有时只给模型看结构化那份）；只有它时转成文字。文字超过 32 KiB（`read_output` 一页的上限）时先交前 32 KiB，全文放进这个会话的留存目录，末尾一行写 `read_output output_ref=… offset=…`——和命令输出同一套分页、上限和过期。单张图片超过 3,932,160 字节（base64，`view_image` 的上限）换成一句说明。

**连接**：用到才起，同一个会话复用；闲 5 分钟收掉（每分钟看一次）；**会话结束时先停 server 再放写锁**——它能写工作树。起不来、超时、断开报 `CCNM_E_DEPENDENCY`；调用发出去之后断了或超时，报的话里写明"做没做成不知道"，下一次调用会重起它。server 回了 JSON-RPC 错误（参数不对）报 `CCNM_E_INVALID_ARGS`。握手接 2024-11-05 到 2025-11-25 之间的版本。

**开关**：Runtime 自己的配置 `[runtime_mcp]`：`enabled = false` 全关，`project = false` 不读项目的 `.mcp.json`，`hidden = [...]` 按名字藏。默认全开。见[配置说明](../configuration.md#runtime_mcp)。

## 6. 连接生命周期

**先分清四个时钟。**它们常被当成一件事，然后有人以为"把超时调大点"就能让任务活下去：

| 时钟 | 谁定 | 多长 | 到点会怎样 |
| --- | --- | --- | --- |
| 单次等待 | 调用方给 `read_output.wait_ms` | 最多 600000 ms | 这一次读返回，**命令继续跑** |
| 命令运行期限 | 调用方给 `exec_command.timeout_ms` | 前台默认 120000 ms、上限 600000；后台不给就没有期限 | 杀掉整个进程组（第 5.5 节） |
| 会话 | 连接本身，没有别的东西 | 连接开着它就活着 | 连接一断，这条连接起的命令全停，然后才放写入互斥 |
| 输出保留 | Runtime | 本入口连接结束即删；跨入口是最后一次运行过去 7 天 | 旧 `output_ref` 报 `CCNM_E_INVALID_ARGS`（第 8 节） |

**还有第五个，它不是 Runtime 的**：调用方自己肯等一次调用多久。Claude Code 2.1.273 在交互会话里超过 120 秒就把这次调用转到后台，结果照样回来；换成 hub 一类的中间层，常见做法是到点**丢掉这条连接**——连接一丢，这个会话里所有后台命令跟着停。调用方的调用预算不是 Runtime 的运行时限，但在整条链上它往往先到。所以命令要跑得比调用方的预算长，就得 `run_in_background`，再用 `read_output` 分次去看，而不是把 `wait_ms` 调大。

下面三句是契约，不是实现细节：

**一、session-bound 就是权威语义。**没有租约，没有续租，没有跨连接恢复。一条命令只有三个终点：它自己结束、`stop_command`、连接结束。想让任务活过连接，那是**另一种**生命周期（durable job），要升 `ccnm.workspace-mcp/2` 或者一次显式协商才能有——不能在 `/1` 下把"断开即停"悄悄改成"断开继续"，因为现有调用方正是按前者在算账（比如断开后就不必再去收尾）。

**二、取消一次等待不等于取消命令。**取消 `exec_command` 的调用会停掉它正在跑的那条命令（第 6.2 节）；取消 `read_output` 的等待只停这次等待，命令照跑。**两件事只差一个工具名**，而调用方往往把"超时了就发取消"写成同一段代码。要停一条后台命令，只有 `stop_command`。

**三、命令怎么结束的，只有 Runtime 说了算。**`read_output` 和 `stop_command` 结果里的状态行（第 5.5 节那张表）是唯一权威。调用方自己的调用超时、传输错误、SSH 断开都只说明**它不知道**，不能据此给远端进程编一个终态——尤其不能把"我这边超时了"记成"命令失败了"，然后重发一个已经执行过的写操作。真不知道的时候，状态行会明说不知道。

### 6.1 正常路径

| 阶段 | 谁做什么 |
| --- | --- |
| 启动 | Host 起 bridge 进程；bridge 立刻建 SSH，在 initialize 之前就完成远端打开 |
| `initialize` | 由远端 server 回答：协议版本、`serverInfo`（name `ccnm`，version 是远端 ccnm 的版本）、tools 能力、`instructions` |
| `tools/list` | 按模式返回 7 个或 11 个工具（冻结时是 4 个或 7 个，P36、P39、P40 各加了一个只读工具，P41 加了 coding 模式的 `stop_command`） |
| `tools/call` | 在远端项目目录里真的执行 |
| EOF | Host 关 stdin → bridge 关 SSH → 远端 server 停掉这条连接起的所有命令 → 退出 → 写入互斥释放 |

### 6.2 断线、中断、崩溃

- **Host 关掉 bridge（EOF 或 SIGTERM）**：没有子进程要回收——bridge 做完本机检查就 `exec` 成那条 ssh，所以这个进程**就是** transport。EOF 和信号直接落在 ssh 上，远端 server 随之结束，锁随进程释放。**留不下孤儿 transport**，因为没有第二个进程可留。
- **SSH 断了**：bridge 把这条连接当作结束，退出；**不自动重连**。重连意味着换一个远端 session，而调用方手里的 `output_ref` 属于旧 session——静默重连会让它们指向不存在的东西。
- **bridge 自己崩了**：同一件事——崩的就是那条 ssh，远端 server 读到 EOF 后结束。
- **连接半开（对面没了，Runtime 这边不知道）**：远端 server 空闲时**每 30 秒主动发一次 MCP `ping`**（MCP 规范允许任一方发）。Host 在就回一个空结果；Host 那头的连接已经不存在时，这一写会被对方内核 RST，sshd 退出，server 读到 EOF，照正常路径结束、锁变 `released`。**ping 没回应不会断开**——对面只是睡着的话 TCP 还活着，断了反而害人重连；只有写失败才结束。Host 必须按 MCP 规范回应 `ping`，至少不能因为收到它就关连接：实测 Claude Code 2.1.269 / 2.1.272 都回 `{"result":{}}`，工具调用进行中收到也一样；Codex 用的 rmcp 客户端在 SDK 源码里自动回应。
- **MCP 的 `notifications/cancelled`**：转发给远端。被取消的是一次还没回答的 `exec_command` 时，远端停掉这条命令（TERM，2 秒后 KILL 整个进程组）；取消在命令开始之前到达时，命令不会启动。取消是通知、没有回应，所以契约不承诺"发出取消 = 命令已停"。**被取消的是一次带 `wait_ms` 的 `read_output` 时，停的只是这次等待**，命令照跑。已经返回了的后台命令不受取消影响，用 `stop_command` 停。（P41 之前取消不停命令，命令照跑到结束或超时，最长 10 分钟。）
- **连接结束时还有命令在跑**：远端 server 先停掉它们（同上），再放写入互斥、退出。P41 之前是等它们自己跑完——一个 8 秒的命令让 server 在断开后又占了 8.0 秒写入互斥，最长可到 10 分钟。
- **bridge 绝不影响 Managed session。** 它只管自己这一条 SSH 和这一个远端进程；不去枚举、不去清理别人的 session，哪怕它们属于同一个 workspace。

### 6.3 没有 resume

一次 bridge 进程 = 一个远端 session。断了就是断了，重开是**新** session：新的保留输出目录、新的 `output_ref` 空间。契约里没有"接着上次那条"这种操作。

## 7. busy 和 unknown 怎么表达

两种情况都发生在 initialize 之前，所以都是**启动失败**，不是工具结果：

| 情况 | 诊断 | 怎么办 |
| --- | --- | --- |
| 工作树被别的 coding session 占着 | `CCNM_E_POLICY`，一句话说明 guard busy | 等，或者改用 `--mode read` |
| 锁的状态无法确定（锁文件坏了、持有者存活性证明不了） | `CCNM_E_POLICY`，说明拒绝转移写权限 | **人去看现场**，不要重试到它"好了" |
| 上一个会话结束时有命令**停不掉**（离开了进程组又攥着管道，信号够不着） | `CCNM_E_POLICY`，说明写权是**故意**没交出来的，并点名还剩哪些 `output_ref`（P43 新增） | 先按那些 ref 找到命令、把它们收掉，**再**谈清锁；顺序反了就是两个写者进同一棵树 |

**unknown 绝不自动降级成"没人占，那就给你"。** 把写权限交给第二个人的代价是两个 Agent 同时改一棵树，宁可停在这里等人。上一个会话的 pid 记在锁标记里，只为让诊断说得准（那个 pid 还在跑 / 已经不在 / 变成了别的程序）；**pid 不在从来不是交权的理由**，它起的命令可能还活着。

**这把锁的作用范围是一个 state 目录。**它存在启动时传进来的 `${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/` 下，文件名按工作树的规范化路径算。所以同一棵工作树，两个 ccnm 进程各用一个 state 目录时，就是两把互不相干的锁——两个 coding 会话能同时开、同时写，**这一版不做跨 state 目录的协调**。服务同一棵树的所有 ccnm 进程必须共用一个 `XDG_STATE_HOME`；两个系统用户各跑各的 ccnm 服务同一棵树也踩这条（各自的 home 就是各自的 state）。运维上怎么避见[运维手册](../operations.md#一棵树配两个-state-目录--两个互不知晓的写域)。

## 8. 输出预算与保留

这些上限由远端 Runtime 强制，和 Managed 路径共用同一批常量（当前实现的实测值）：

| 位置 | 上限 |
| --- | --- |
| `read_file` 一次最多 | 2000 行（`max_lines`）、64 KiB（`max_bytes`，默认 32 KiB），超了给你续读的行号 |
| `list_files` 一次最多 | 1000 条（`max_entries`） |
| `search_text` | 200 条结果（只列文件、计数两种模式下是 200 个文件）、上下文 10 行、整体 32 KiB、单行 512 字节 |
| `exec_command` 超时 | 最大 600000 ms（10 分钟）；后台命令不给就没有期限 |
| 后台命令 | 一条连接最多 8 个同时在跑；`read_output` 的 `wait_ms` 最大 600000 ms；停的时候 TERM 之后 2 秒 KILL，一个信号都够不着的（离开了进程组又占着管道）等 10 秒后放弃 |
| `exec_command` 回传 | 预览总共默认 4 KiB，`preview_bytes` 最大 16 KiB；stderr 最多占一半，其余给 stdout，某个流超出时只留它的开头和结尾。完整输出用 `output_ref` 读 |
| `read_output` 一次最多 | 32 KiB（默认 16 KiB） |
| `apply_patch` | 一次最多 50 个文件；一次请求里所有文件的新内容**合计** 1 MiB；被编辑的文件超过 16 MiB 直接拒绝 |
| 保留输出 | 每次运行的 stdout、stderr **各自**最多落盘 64 MiB，超出的不再写，命令照常跑完、结果里带一条说明；每个 session 只留最新的 100 次运行，开始第 101 次前删最旧的；一个 session 已结束运行的输出**合计**最多 256 MiB，每次运行结束后从最旧的删。还在跑的运行不删，所以同一个 session 并发跑命令时可以暂时超过 256 MiB，超出部分不超过"进行中的运行数 × 128 MiB"；后台命令跑多久就算多久"进行中"，8 个最多 1 GiB |
| `read_notebook` | 文件最多 16 MiB；一次最多 32 KiB 文本、单个输出 4 KiB、8 张图（合计不超过 `view_image` 的上限），放不下时停在 cell 边界 |
| `view_image` | 文件最多 3932160 字节（base64 后 5 MiB，Claude Code 2.1.273 的上限）；只发 PNG、JPEG、GIF、WebP |
| `instructions` | 2048 个 UTF-16 码元（含项目说明文件），超了由 ccnm 按行截断，见第 10 节 |
| `call_mcp_tool`（P49） | 结果文字一次最多 32 KiB，其余进留存目录用 `read_output` 读（算在上面"保留输出"里）；单张图片同 `view_image`；一个 server 的工具表最多 64 KiB，放不下先去参数表再去描述；server 自己的说明最多 4 KiB；一条消息超过 32 MiB 这次调用报错；server 起 30 秒、一次调用 60 秒（Codex 配置里的 `startup_timeout_sec` / `tool_timeout_sec` 会改它） |

保留的输出**留在远端**，只在这个 session 的目录里。本入口的 session 在连接结束时删掉自己的输出：第 6 节说过，断了就是断了，重开是新 session，旧的 `output_ref` 本来就没人能再用。Managed 会话的输出不随连接删，它重连后沿用同一个 session，旧 ref 还要能读。不管哪个入口，最后一次运行过去 7 天、Runtime 上又没有进程在服务它的 session，输出会被删掉；运维上怎么看、怎么提前清见[运维手册](../operations.md#状态文件在哪多大怎么清)。

契约不承诺任何保留时长。被删掉的 `output_ref` 再拿去读，报 `CCNM_E_INVALID_ARGS`（`no output kept for r-…`），和从没有过这个 ref 一样。

「保留输出」这一行和上两段 2026-09-17 改过两次。先是按实现更正措辞：原文「每个 session 最多 100 次运行 / 64 MiB」和「session 结束后清理」都与当时的实现不符（实际是每流每次 64 MiB、没有 session 总量上限、从不清理）。同日 P31 加了 session 总量上限、本入口结束即删和 7 天过期，才是现在写的样子。两次都不升 `ccnm.workspace-mcp/2`：契约从没承诺保留时长，被删 ref 的错误码和消息不变，只是删得更早。

## 9. 版本与不匹配

三个版本号，别混：

| 版本 | 谁和谁之间 | 现在是什么 |
| --- | --- | --- |
| MCP 协议版本 | Host ↔ 远端 server | 服务端默认 `2025-11-25`（SDK 的 LATEST） |
| ccnm 内部 open 协议 | bridge ↔ 远端 ccnm | 整数，当前是 4；不匹配 fail-closed |
| ccnm 版本 | 两端的二进制 | 两端不必逐位相同，但内部协议必须谈得拢 |

**MCP 版本协商的实际行为**（rmcp 3.2.0 实测代码路径，不是猜）：客户端要一个服务端认识的旧版本（早于 `2026-07-28` 的那几个：`2024-11-05` / `2025-03-26` / `2025-06-18` / `2025-11-25`），服务端就照它回；否则回服务端自己最新的旧版本。只有当服务端一个带 `initialize` 握手的版本都不支持时才会回 `-32022`——ccnm 不会走到那一步。**所以不要写"版本不匹配会报错"这种话**，实际是安静地协商到一个共同版本。

内部 open 协议不一样：**不匹配就停**，不静默回退到不检查 root 的老路径。远端 ccnm 太旧的表现是启动失败 + `CCNM_E_VERSION`，不是"连上了但行为不同"。

## 10. 项目说明（instructions）

Managed 路径知道自己启动的是 Claude 还是 Codex，所以能按 provider 投影 `CLAUDE.md` / `AGENTS.md`。**外部 MCP 不知道调用方是谁，也不许猜**——MCP 握手里的 `clientInfo` 是对方自己填的字符串，不是身份证明。

契约选定的方案是**在 Runtime 的权威配置里声明**（runtime-surfaces §7.5 的方案一）：

```toml
[workspaces.myproject]
external_mcp = "read"
external_instructions = "generic"   # generic（默认）| project | none
```

| 值 | `initialize.result.instructions` 里有什么 |
| --- | --- |
| `generic`（默认） | 只有 ccnm 自己那段：这是哪个 workspace、路径都是相对的、有哪些工具 |
| `project` | 上面那段 + 项目说明文件，Runtime 按固定顺序找：`AGENTS.md` → `CLAUDE.md`，取第一个存在的 |
| `none` | 什么都不给 |

**长度和顺序（P13，2026-09-16 起）。**整段按 2048 个 UTF-16 码元算——bridge 不知道对面是哪个 Host，只能按已知最严的 Claude Code 算（它按 JS 字符串长度截到 2048，多出的换成 `… [truncated]`）。顺序是：ccnm 自己那段 → 模式句 → `[project instructions: …]` 标记行 → 项目说明文件正文；正文放不下时 ccnm 按行截断，标记行写明文件多大、给了多少、用 `read_file` 读全文。改之前上限写的是 16 KiB 字节、标记行在最后，对 Claude Code 来说超过 2048 的部分连同标记行一起被 Host 截掉。这只改上下文文本的长度和位置，不涉及工具、权限和错误码，不升版本。

固定顺序是因为外部 MCP 没有 provider 可依据；**不读调用方机器上的任何 Agent 配置**，也不把 Agent profile 当成 Runtime 的上下文来源。这个选项只影响上下文文本，**不影响权限**——给了 `project` 不等于多一分授权。

## 11. 错误与泄漏边界

### 11.1 两种失败，走两条路

| 失败的是 | 怎么回 | 例子 |
| --- | --- | --- |
| 工具**干的活** | `tools/call` 的结果，`isError: true`，正文第一行是 `CCNM_E_*` | 路径在 workspace 外、命令返回非零、补丁基于旧内容 |
| **调用本身**不合法 | JSON-RPC 错误 | 工具名不认识（`-32602`，message `tool not found`）、参数结构不对 |

这个区分不是洁癖：`isError` 的结果模型能读到、能据此改做法；JSON-RPC 错误很多 Host 根本不给模型看。"这个路径不在 workspace 里"必须让模型看见，所以它是结果不是错误。

### 11.2 启动失败：不是 MCP 消息

initialize 之前失败时，bridge **不回答 initialize**，退出码非 0，stderr 上是 ccnm 一贯的错误形状——第一行是 `CCNM_E_*` 名字，后面是给人看的解释：

```text
CCNM_E_POLICY:
workspace myproject allows external MCP in read mode; coding was requested
```

**远端拒绝和本机拒绝长得一样**，因为打印它们的是同一段代码：远端那份由 Runtime 打在自己的 stderr 上，ssh 原样带回来。要机器判断就看退出码和第一行的名字，不要解析后面的措辞。

**但退出码能不能到你手上，是 SSH 服务端的事，不是 ccnm 的。** bridge 不是 ssh 的父进程，它 `exec` 成那条 ssh；远端的退出码要靠 SSH 的 exit-status 消息带回来，服务端不发，客户端就只能退 0。2026-09-16 在 Tailscale SSH 上实测到的就是这一种：远端 `mcp-serve` 自己退 33，`ccnm mcp bridge` 退 0，`ssh -T <host> "exit 33"` 同样退 0。**这条链路上退出码不可用**，判断只能靠 stderr 第一行的 `CCNM_E_*`；而有的 Host（实测 Claude Code 2.1.268）又会把子进程 stderr 丢掉，两样凑齐就什么都没有了——那时只能按[排错手册](../troubleshooting.md)手工跑一遍同一条命令。ccnm 这边没有可改的地方，记在这里是为了别把「退了 0」读成「起来了」。

### 11.3 可能出现的 `CCNM_E_*`

| 名字 | 什么时候 | 到达方式 |
| --- | --- | --- |
| `CCNM_E_CONFIG` | 本机或远端配置缺失/解析不了/校验不过 | 启动失败 |
| `CCNM_E_VERSION` | 两端 ccnm 的内部协议谈不拢 | 启动失败 |
| `CCNM_E_RUNTIME_UNREACHABLE` | SSH 到 Runtime 不通 | 启动失败 |
| `CCNM_E_POLICY` | workspace 没 opt-in、模式越权、写入互斥 busy/unknown、安全 audit 拒绝 | 启动失败 |
| `CCNM_E_WRONG_WORKSPACE` | 远端 root 不是目录，或不是同一个项目 | 启动失败 |
| `CCNM_E_NOT_READY` | 没有已知失败，但也验证不过（功能没实现、检查 SKIP） | 启动失败 |
| `CCNM_E_INVALID_ARGS` | 工具参数 ccnm 用不了：行号为 0、区间反了、路径指向读不了的东西 | `isError` 结果 |
| `CCNM_E_DEPENDENCY` | 远端缺少这次操作需要的程序 | `isError` 结果 |
| `CCNM_E_INTERNAL` | bug 或没预料到的 OS 失败 | 两者都可能 |

### 11.4 不泄漏什么

- **不区分"workspace 不存在"和"没对外开放"**：两种都是同一条 `CCNM_E_POLICY`，同一段文本。否则错误消息本身就成了探测远端机器上有什么项目的工具。
- 错误、结果、`instructions` 里**不出现** workspace 根之外的绝对路径、Agent 的 profile 路径、凭据内容或环境变量值。工具看到的路径一律相对于 workspace 根（唯一的例外是中断补丁的恢复说明，那是人必须能找到的文件，Managed 路径同样处理）。
- `workspace_info` 只说 workspace 名、git 状态、平台、server pid 和调用计数；不报 Runtime 的用户名、home 或网络信息。

## 12. 这一版明确不做

- 任意 `ssh_exec(host, command)`、端口转发、SFTP 浏览器、数据库/容器专用工具——那会把一个有边界的 workspace runtime 退化成万能远控面板。
- HTTP / 公网 MCP 网关。只有本地 stdio；远程 transport 和它的认证是另一个问题，要真实客户端提出需求后另立阶段。
- 自动把官方 Agent 部署到 Runtime，或者代理任何 AI 凭据。
- 多租户、共享 token、按调用方区分的授权。
- 猜调用方是什么 Agent，或者读它的配置。
- 按 shell 文本判断"这条命令是只读的"。

## 13. 机器可检查的部分

| 文件 | 是什么 |
| --- | --- |
| [schema/remote-workspace-mcp-v1.schema.json](schema/remote-workspace-mcp-v1.schema.json) | 本文涉及的消息形状 |
| [fixtures-mcp/](fixtures-mcp/) | initialize、两种模式的工具表、成功/失败的调用、启动失败诊断 |

```bash
python3 scripts/check_protocol.py
```

它检查 fixture 符合 schema、`CCNM_E_*` 名字和本文的表一致、每个写进表里的名字都有样例。

**它证明的是这几份文件互相自洽，不证明实现的行为和它们一致。** 那一半由别的东西证：Rust 集成测试（`cargo test -p ccnm-cli --test external_mcp`）、一个不 import ccnm 代码的中立 MCP 客户端（`python3 -m unittest tests.test_remote_workspace_mcp`），以及两轮真机（见页首）。本文里标"实测"的地方，依据是仓库代码或那两轮记录；标"契约"的地方是设计决定。

### 工具表以哪一份为准

两份 `tools-list-*.json` 是**工具名、说明文字和参数名在文档侧的唯一记载**——本文第 5 节只有 annotations，第 8 节只顺带提了几个上限参数。所以下面这些是精确的，由 `external_mcp` 的 `published_tool_tables_match_the_running_server` 起一个真实 `internal mcp-serve`、两种模式各取一次 `tools/list` 逐字节比对，对不上就失败：

| 比 | 为什么 |
| --- | --- |
| 工具名集合 | 模式的边界，Host 照着它决定有哪些工具 |
| 每个工具的 `description` | 它是人手写进 `#[tool(description = ...)]` 的，**也是模型实际读到的那段文本** |
| 参数名与 `required` | Host 照着它拼 `tools/call` 的 arguments，名字错一个字就每次都被拒 |

**`call_mcp_tool`（P49）不在这两份里**：它只在那台机器上有能转的 MCP server 时才出现，而这个测试起的 server 没有。它的名字、参数和说明的固定部分记在第 5.7 节，由中立客户端测试和 `mcp::relay` 的单元测试对着真实 server 核对。

**`check_protocol.py` 证明不了这些，别指望它。** 它把 fixture 对着 `schema/` 里手写的 JSON Schema 校验，两份都是手写的，一起漂走也照样通过——2026-09-16 发现的 `apply_patch` 就是这样：fixture 写着 `changes`，wire 上一直叫 `files`，照 fixture 实现的 Host 每次调用都被拒，而协议检查一直是绿的。

fixture 里**不精确的只剩每个参数内部**的类型、上下界和说明：server 发的是 `schemars` 从 Rust 类型生成的完整 schema（带 `default`、`format`、`minimum` 这些），fixture 写的是简写。要这一份的准确内容，连上去读 `tools/list`，或者看 `crates/ccnm-core/src/mcp/` 里对应的 `*Args`。

**改了工具说明就手动同步 fixture。** 没有"跑一次把 server 输出写回 fixture"的开关，这是故意的：那种开关会让一次没想清楚的措辞改动被一键洗绿，而冻结契约的意义就是改它要费一点劲。失败信息里两段文本都会打出来，照着贴即可。
