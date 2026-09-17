# ccnm Remote Workspace MCP v1（契约）

> **状态：`ccnm.workspace-mcp/1` 于 2026-09-11 冻结。**
> 依据是两轮真机：允许矩阵在真实 Claude Code 2.1.268 上跑过（[P11 记录](../research/p11-real-host-2026-09-11.md)、[证据](../research/p11-matrix-20260911.json)），远端真实项目 dogfood 在 Debian 13 / x86_64 的 Runtime 上跑过（[P12 记录](../research/p12-real-project-2026-09-11.md)、[证据](../research/p12-dogfood-20260911.json)）。
> **冻结的意思是**：往后加工具、加字段、加错误原因属于加法，可以；删工具、改 `disabled`/`read`/`coding` 三个值的含义、改权限判定或错误码语义要升到 `ccnm.workspace-mcp/2`。
> 验收范围、已知代价和**不作保证的 egress** 见[支持矩阵](../support-matrix.md)；这一版明确不做的东西见第 12 节。
> **冻结之后的加法**：2026-09-17（P36）加了第八个工具 `load_skill` 和 `prompts` 能力，用来把项目自带的 skills 交给模型和人，见第 5.1 节。原来七个工具的名字、参数和语义没有动。
> 2026-09-17（P37）给三个老工具加了可选参数：`search_text` 的输出模式、跨行、文件类型和 dotfile，`apply_patch` 的 op `write`，`exec_command` 的 `shell`，见第 5.2 节。不带新参数的调用和以前完全一样；`exec_command` 的 `required` 因此从 `["cmd"]` 变成空。同日修了一个行为缺陷：调用方的 `glob` 能把 dotfile 带回搜索（同一节末尾）。

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
 │  <── 4 个或 7 个工具 ─────────────────────────────────│
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

Managed 路径（`ccnm` 自己启动的 Claude Code 会话）不需要也没有这个设置：那条路传 `--tools ""`，`ToolSearch` 本身就不可用，七个工具一直是全量加载的。

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
| `read_output` | ❌ | ✅ |
| `apply_patch` | ❌ | ✅ |
| `exec_command` | ❌ | ✅ |

`read` 模式**永远没有 `exec_command`**。哪怕调用方保证"只跑 `cat`"也不行：任意 exec 能写磁盘、能联网、能起后台进程，靠解析命令字符串判断只读是假安全。将来真要"只读 shell"，那得靠独立的 OS sandbox 或者白名单可执行文件契约，不是靠猜。

`read` 模式也没有 `read_output`，理由不同：`output_ref` 只在**产生它的那个 session 的保留目录里**有意义（实现上 `read_output` 就是拿这个 ref 去 join 本 session 的目录）。read 模式没有 `exec_command`，永远产不出 ref，留着它就是一个必然失败的工具；而让它去解析**别的 session** 的 ref，就是跨会话泄漏。所以直接不发。

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
| `read_output` | read | `true` | — | — | `false` |
| `apply_patch` | write | `false` | `true` | `false` | `false` |
| `exec_command` | exec | `false` | `true` | `false` | `true` |

（按 MCP 规范，`destructiveHint` / `idempotentHint` 只在 `readOnlyHint` 为 `false` 时才有意义，所以只读那几行留空。）

三条规矩：

1. **annotations 只改善 Host 的审批 UX，不是门禁。** 一个完全忽略它们的 Host，得到的授权结果必须和尊重它们的 Host 一模一样——真正的门禁是 OS 身份、workspace 绑定、access mode 和写入互斥。
2. **`exec_command` 永远按 destructive + open-world 处理。** 不因为这次的命令"看起来只是 `ls`"就动态改注解。注解是工具的属性，不是某次调用的属性。
3. `apply_patch` 不是 open-world：它只能改这个 workspace 里的文件。但它是 destructive——update、write 会替换内容，delete 会删文件。

另外，Managed 路径上 `exec_command` 会带一个 `_meta` 键 `anthropic/requiresUserInteraction`（只在有人坐在终端前的交互式 session 里带，而且该 workspace 没有写 `allow_unattended_exec`）。**外部 MCP 永远不发这个键**：bridge 不知道 Host 那头有没有人，冒充知道比不说更糟，所以那个开关对 bridge 没有任何影响。

### 5.1 `load_skill` 与 prompts：项目自带的 skills（P36 新增）

**skill 是什么**：项目在 `.claude/skills/<名字>/SKILL.md` 里写下的"这类任务该怎么做"——开头一段 YAML（名字、描述、参数），后面是给模型的正文，旁边可以带脚本和参考文件。`.claude/commands/*.md` 是同一种格式的单文件版本。官方 CLI 靠"当前目录"发现它们；项目在远端时 CLI 的当前目录不在项目里，一个都发现不了，所以由 Runtime 这一侧来发现。

**在哪找**（都相对 workspace 根，走和 `read_file` 同一套路径策略）：

| 位置 | 形状 |
| --- | --- |
| `.claude/skills/<名字>/SKILL.md` | skill |
| `.agents/skills/<名字>/SKILL.md` | skill（跨 Agent 的通用写法，Codex 找的是这里） |
| `.claude/commands/**/*.md`（最深 3 层） | 命令；名字是文件名 |

重名时按上表从上到下谁先谁赢，输的那个不会悄悄消失——不带名字调 `load_skill` 返回的完整列表末尾会写出它的路径和原因。读不了的 frontmatter、没有描述的文件、经 symlink 指到 workspace 外面的 skill 目录，同样列在那里。最多 100 个；单个文件超过 1 MiB 不读。

**`load_skill` 怎么用：**

| 调用 | 返回 |
| --- | --- |
| 不带 `name` | 完整列表：每个 skill 的名字、参数提示、描述（最多 1536 字符）、文件路径；只能由人启动的、没被收进来的也列出并说明原因 |
| `name`（可选 `arguments`，一个字符串） | 这个 skill 的正文，见下 |

返回的正文前面有几行方括号，是 server 加的：skill 在哪个文件、`${CLAUDE_SKILL_DIR}` 是哪个目录、哪些命令**没有被执行**、哪些 frontmatter 在这里不起作用。样例见 [`call-load-skill-ok.json`](fixtures-mcp/call-load-skill-ok.json)。正文本身：

- frontmatter 去掉；`$ARGUMENTS`、`$ARGUMENTS[N]`、`$N`、声明过的 `$name` 按 Claude Code 2.1.273 的实际规则替换（没给到的 `$N` 原样留着——正文里的 `awk '{print $1}'` 因此不会被抹掉）；
- `${CLAUDE_SKILL_DIR}` 换成 skill 目录的 **workspace 相对路径**，`${CLAUDE_PROJECT_DIR}` 换成 `.`。skill 的脚本和参考文件就是 workspace 里的普通文件：模型用 `read_file` 读、用 `exec_command` 跑，所以它们在 Runtime 上、以执行身份、受同一套写入互斥和 `exec_sandbox` 约束执行；
- 超过 64 KiB 在行边界截断，并写明用 `read_file` 从哪一行接着读。

**目录放在哪**：`load_skill` 自己的 `description` 里。它的前半段是固定文本，后半段是这个 workspace 的 skill 目录（名字、参数提示、折成一行并截到 200 字符的描述），整段不超过 2048 个 UTF-16 码元——Claude Code 2.1.273 对每个工具的 description 只留这么多（实测；Codex 0.154.0 不截）。放不下的 skill 只列名字。**这是七个老工具没有的性质：`description` 随 workspace 变。** 没有 skill 时它是固定文本，[`tools-list-*.json`](fixtures-mcp/tools-list-read.json) 逐字节比对的就是那一版。目录在会话开始时定下来（Host 整个连接期间都留着 `tools/list` 的结果）；调用时重新扫描，所以会话中途新写的 skill 能加载，只是要到下一个会话才出现在目录里。

**三条和官方 CLI 不一样的地方，都是故意的：**

1. **`` !`命令` `` 注入不执行。** 官方 CLI 在加载 skill 时先跑这些命令、把输出填进正文。这里原样保留，并在开头列出行号和命令，模型需要就自己用 `exec_command` 跑。理由：一次"读"调用不该触发仓库指定的命令——那会绕过 `exec_command` 上的人工确认（`allow_unattended_exec` 管的那一层），`read` 模式下更是直接变成了执行。
2. **`allowed-tools`、`disallowed-tools`、`hooks`、`model`、`effort`、`context`、`agent`、`shell` 不起作用**，出现时在返回文本里点名。ccnm 改不了 Host 的权限和模型，也不在 Agent 那台机器上执行任何来自仓库的东西。Claude Code 自己对经 MCP 来的 skill 也不认 `hooks` 和 `allowed-tools`。
3. **只找 workspace 里的。** 执行账号 HOME 下的用户级 skills 不读。

`disable-model-invocation: true` 的 skill 不进目录，`load_skill` 拒绝它（`CCNM_E_POLICY`）；`user-invocable: false` 的不登记成 prompt。

**prompts**：每个可由人启动的 skill / 命令同时登记成一个 MCP prompt（[`prompts-list-ok.json`](fixtures-mcp/prompts-list-ok.json)、[`prompts-get-ok.json`](fixtures-mcp/prompts-get-ok.json)），`prompts/get` 返回的就是 `load_skill` 会返回的那段文本。Claude Code 把它变成斜杠命令 `/mcp__ccnm__<名字>`（server 在 Host 配置里叫别的名字，中间那段就跟着变）。prompt 的参数是 skill 在 frontmatter 的 `arguments` 里声明的名字；一个都没声明时是单个 `arguments`。**Claude Code 把人敲的参数按空白切开、依次对应声明的参数，多出来的词被它丢掉**（2.1.273 实测）——要传多个词，skill 得声明多个参数。Codex 0.154.0 连上之后只调 `tools/list`，看不到 prompts，所以 prompts 是锦上添花，`load_skill` 才是主通道。

**没做的**：MCP 官方的 skills 扩展（SEP-2640，`skills/list` / `skill://` 资源）。Claude Code 里它的客户端已经写好，但挂在一个默认关闭的开关后面，现在对哪个 Host 都不生效；它是另一个阶段。依据见 [P36 记录](../research/p36-skills-surface-2026-09-17.md)。

### 5.2 搜索模式、整文件覆盖、一行 shell（P37 新增）

对齐的是 Claude Code 2.1.273 的 Grep、Write、Bash（读的是它打包代码里的工具定义，依据见 [P37 记录](../research/p37-execution-surface-batch1-2026-09-17.md)）。三处都是可选参数或新操作，不带它们的调用和以前一模一样。

**`search_text` 多了四个参数：**

| 参数 | 取值 | 效果 |
| --- | --- | --- |
| `output_mode` | `content`（默认）、`files_with_matches`、`count` | 后两种只返回路径，或 `路径:匹配行数`。这时 `max_results` 数的是文件，`context_lines` 不起作用 |
| `multiline` | 布尔，默认 `false` | 匹配可以跨行，正则里的 `.` 也匹配换行（`rg -U --multiline-dotall`）。跨行的结果按行展开，每行照样受单行 512 字节、总量 32 KiB 限制。query 里带换行却没开它，报 `CCNM_E_INVALID_ARGS` |
| `type` | rg 的文件类型名，如 `rust`、`py`、`ts` | 只搜这类文件；rg 不认识的名字报 `CCNM_E_INVALID_ARGS`。**不能和 `glob` 同时给**，同时给报 `CCNM_E_INVALID_ARGS`：rg 里命中 glob 的文件根本不看类型，一起传会悄悄把别的类型也搜出来 |
| `include_hidden` | 布尔，默认 `false` | 也搜 dotfile 和点开头的目录。`.git` 不管怎么设都不搜 |

计数按"匹配的行"算，和 `rg --count` 一样：一行里出现两次算一行，一个跨行匹配算一次。

和原生 Grep 不一样的地方：默认仍是 `content`（原生默认只列文件，但冻结时这里就是内容，改默认值要升版本）；dotfile 默认不搜（原生默认搜，只排除版本库目录）；没有分开的 `-A` / `-B`、`head_limit` / `offset` 分页和只输出匹配部分的 `-o`。

**`apply_patch` 多了 op `write`**：`{"op": "write", "path": …, "version": …, "content": …}` 用 `content` 整体替换一个**已经存在**的文件。和 `update` 一样必须带 `read_file` 给的 `version`，过期报 `CCNM_E_STALE_EPOCH`；文件不存在时报 `CCNM_E_INVALID_ARGS` 并指向 `add`——一个带着版本号来的 `write` 碰上文件没了，说明有人删了它，不该当成"那就新建"。原子替换、中断日志、失败回滚、保留文件权限，和 `update` 走同一条路。原生 Write 不存在的文件也能写；这里新建仍然是 `add`。

**`exec_command` 多了 `shell`**：一行命令，Runtime 用 `bash -c <这一行>` 执行，管道、重定向、`&&`、`cd sub && …` 都能用。和 `cmd` 二选一：都给或都不给报 `CCNM_E_INVALID_ARGS`；JSON Schema 的 `required` 表达不了"恰好一个"，所以 `required` 是空的，由 server 在调用时检查。

- **不放宽任何东西。** 模型本来就能发 `{"cmd": ["bash", "-c", "…"]}`，`shell` 就是这条 argv：同一道执行门、同一个人工确认、开了 `exec_sandbox` 时同样被包起来。
- **是 bash，不是 sh，也不退回 sh。** Runtime 上找不到 bash 报 `CCNM_E_DEPENDENCY`，消息里说改用 `cmd`。Debian 的 `sh` 是 dash，`[[ ]]`、`set -o pipefail` 在那里意思不同，悄悄换解释器比报错更糟。
- 结果第一行 `$ …` 是原样的那一行。工作目录不跨调用保持（原生 Bash 会保持），每次用 `cwd` 指定。

**同日修的行为缺陷（P37 实现时发现）。** 以前 `search_text` 的 `glob` 写成 `*`、`**`、`**/*` 这类能匹配目录的形状时，dotfile 会被搜出来发给模型，`.git/` 也会被 rg 扫一遍（那里的命中由事后检查丢掉，没有发出去）——和工具说明里"dotfile 和 .git 永远不搜"矛盾。原因是 rg 15.2.0 里 glob 优先于"不搜隐藏文件"，多个 glob 同时命中时最后一个说了算，而排除规则排在调用方 glob 前面。现在排除规则放在最后，模型看到的结果变少了，属于修正而不是加法，不升版本。

**还没修的同类问题**：同样形状的 `glob` 也会让 rg 搜进 `.gitignore` 排除掉的目录（`glob: "**"` 会搜到 `target/`、`node_modules/`）。它和上一条是同一个优先级规则，但没法靠调整顺序修：rg 没有"只作用于文件的 glob"，要么由 ccnm 自己按 glob 过滤结果（`*.rs` 的含义会跟着变），要么拒绝能匹配目录的 glob，需要单独立项。原先就是这样，P37 没有让它变坏。

## 6. 连接生命周期

### 6.1 正常路径

| 阶段 | 谁做什么 |
| --- | --- |
| 启动 | Host 起 bridge 进程；bridge 立刻建 SSH，在 initialize 之前就完成远端打开 |
| `initialize` | 由远端 server 回答：协议版本、`serverInfo`（name `ccnm`，version 是远端 ccnm 的版本）、tools 能力、`instructions` |
| `tools/list` | 按模式返回 4 个或 7 个工具 |
| `tools/call` | 在远端项目目录里真的执行 |
| EOF | Host 关 stdin → bridge 关 SSH → 远端 server 退出 → 写入互斥释放 |

### 6.2 断线、中断、崩溃

- **Host 关掉 bridge（EOF 或 SIGTERM）**：没有子进程要回收——bridge 做完本机检查就 `exec` 成那条 ssh，所以这个进程**就是** transport。EOF 和信号直接落在 ssh 上，远端 server 随之结束，锁随进程释放。**留不下孤儿 transport**，因为没有第二个进程可留。
- **SSH 断了**：bridge 把这条连接当作结束，退出；**不自动重连**。重连意味着换一个远端 session，而调用方手里的 `output_ref` 属于旧 session——静默重连会让它们指向不存在的东西。
- **bridge 自己崩了**：同一件事——崩的就是那条 ssh，远端 server 读到 EOF 后结束。
- **连接半开（对面没了，Runtime 这边不知道）**：远端 server 空闲时**每 30 秒主动发一次 MCP `ping`**（MCP 规范允许任一方发）。Host 在就回一个空结果；Host 那头的连接已经不存在时，这一写会被对方内核 RST，sshd 退出，server 读到 EOF，照正常路径结束、锁变 `released`。**ping 没回应不会断开**——对面只是睡着的话 TCP 还活着，断了反而害人重连；只有写失败才结束。Host 必须按 MCP 规范回应 `ping`，至少不能因为收到它就关连接：实测 Claude Code 2.1.269 / 2.1.272 都回 `{"result":{}}`，工具调用进行中收到也一样；Codex 用的 rmcp 客户端在 SDK 源码里自动回应。
- **MCP 的 `notifications/cancelled`**：转发给远端；但一次已经在跑的 `exec_command` 是否能立刻停下取决于那个进程，契约不承诺"取消返回 = 命令已停"。
- **bridge 绝不影响 Managed session。** 它只管自己这一条 SSH 和这一个远端进程；不去枚举、不去清理别人的 session，哪怕它们属于同一个 workspace。

### 6.3 没有 resume

一次 bridge 进程 = 一个远端 session。断了就是断了，重开是**新** session：新的保留输出目录、新的 `output_ref` 空间。契约里没有"接着上次那条"这种操作。

## 7. busy 和 unknown 怎么表达

两种情况都发生在 initialize 之前，所以都是**启动失败**，不是工具结果：

| 情况 | 诊断 | 怎么办 |
| --- | --- | --- |
| 工作树被别的 coding session 占着 | `CCNM_E_POLICY`，一句话说明 guard busy | 等，或者改用 `--mode read` |
| 锁的状态无法确定（锁文件坏了、持有者存活性证明不了） | `CCNM_E_POLICY`，说明拒绝转移写权限 | **人去看现场**，不要重试到它"好了" |

**unknown 绝不自动降级成"没人占，那就给你"。** 把写权限交给第二个人的代价是两个 Agent 同时改一棵树，宁可停在这里等人。

## 8. 输出预算与保留

这些上限由远端 Runtime 强制，和 Managed 路径共用同一批常量（当前实现的实测值）：

| 位置 | 上限 |
| --- | --- |
| `read_file` 一次最多 | 2000 行（`max_lines`）、64 KiB（`max_bytes`，默认 32 KiB），超了给你续读的行号 |
| `list_files` 一次最多 | 1000 条（`max_entries`） |
| `search_text` | 200 条结果（只列文件、计数两种模式下是 200 个文件）、上下文 10 行、整体 32 KiB、单行 512 字节 |
| `exec_command` 超时 | 最大 600000 ms（10 分钟） |
| `exec_command` 回传 | 预览总共默认 4 KiB，`preview_bytes` 最大 16 KiB；stderr 最多占一半，其余给 stdout，某个流超出时只留它的开头和结尾。完整输出用 `output_ref` 读 |
| `read_output` 一次最多 | 32 KiB（默认 16 KiB） |
| `apply_patch` | 一次最多 50 个文件；一次请求里所有文件的新内容**合计** 1 MiB；被编辑的文件超过 16 MiB 直接拒绝 |
| 保留输出 | 每次运行的 stdout、stderr **各自**最多落盘 64 MiB，超出的不再写，命令照常跑完、结果里带一条说明；每个 session 只留最新的 100 次运行，开始第 101 次前删最旧的；一个 session 已结束运行的输出**合计**最多 256 MiB，每次运行结束后从最旧的删。还在跑的运行不删，所以同一个 session 并发跑命令时可以暂时超过 256 MiB，超出部分不超过"进行中的运行数 × 128 MiB" |
| `instructions` | 2048 个 UTF-16 码元（含项目说明文件），超了由 ccnm 按行截断，见第 10 节 |

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

**`check_protocol.py` 证明不了这些，别指望它。** 它把 fixture 对着 `schema/` 里手写的 JSON Schema 校验，两份都是手写的，一起漂走也照样通过——2026-09-16 发现的 `apply_patch` 就是这样：fixture 写着 `changes`，wire 上一直叫 `files`，照 fixture 实现的 Host 每次调用都被拒，而协议检查一直是绿的。

fixture 里**不精确的只剩每个参数内部**的类型、上下界和说明：server 发的是 `schemars` 从 Rust 类型生成的完整 schema（带 `default`、`format`、`minimum` 这些），fixture 写的是简写。要这一份的准确内容，连上去读 `tools/list`，或者看 `crates/ccnm-core/src/mcp/` 里对应的 `*Args`。

**改了工具说明就手动同步 fixture。** 没有"跑一次把 server 输出写回 fixture"的开关，这是故意的：那种开关会让一次没想清楚的措辞改动被一键洗绿，而冻结契约的意义就是改它要费一点劲。失败信息里两段文本都会打出来，照着贴即可。
