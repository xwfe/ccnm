# ccnm Remote Workspace MCP v1（契约）

> **状态：`ccnm.workspace-mcp/1` 于 2026-09-11 冻结。**
> 依据是两轮真机：允许矩阵在真实 Claude Code 2.1.268 上跑过（[P11 记录](../research/p11-real-host-2026-09-11.md)、[证据](../research/p11-matrix-20260911.json)），远端真实项目 dogfood 在 Debian 13 / x86_64 的 Runtime 上跑过（[P12 记录](../research/p12-real-project-2026-09-11.md)、[证据](../research/p12-dogfood-20260911.json)）。
> **冻结的意思是**：往后加工具、加字段、加错误原因属于加法，可以；删工具、改 `disabled`/`read`/`coding` 三个值的含义、改权限判定或错误码语义要升到 `ccnm.workspace-mcp/2`。
> 验收范围、已知代价和**不作保证的 egress** 见[支持矩阵](../support-matrix.md)；这一版明确不做的东西见第 12 节。

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
      "args": ["mcp", "bridge", "myproject", "--mode", "read"]
    }
  }
}
```

**实测过的只有 Claude Code**（2.1.268，上面这个形状，`-p` 模式）。其他 Host 的配置格式各不相同（Codex 用 TOML），本文不声称验证过它们。契约只保证：一个进程、stdin/stdout 说 MCP、参数如上。

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
| `read_output` | read | `true` | — | — | `false` |
| `apply_patch` | write | `false` | `true` | `false` | `false` |
| `exec_command` | exec | `false` | `true` | `false` | `true` |

（按 MCP 规范，`destructiveHint` / `idempotentHint` 只在 `readOnlyHint` 为 `false` 时才有意义，所以只读那几行留空。）

三条规矩：

1. **annotations 只改善 Host 的审批 UX，不是门禁。** 一个完全忽略它们的 Host，得到的授权结果必须和尊重它们的 Host 一模一样——真正的门禁是 OS 身份、workspace 绑定、access mode 和写入互斥。
2. **`exec_command` 永远按 destructive + open-world 处理。** 不因为这次的命令"看起来只是 `ls`"就动态改注解。注解是工具的属性，不是某次调用的属性。
3. `apply_patch` 不是 open-world：它只能改这个 workspace 里的文件。但它是 destructive——update 会替换内容，delete 会删文件。

另外，Managed 路径上 `exec_command` 会带一个 `_meta` 键 `anthropic/requiresUserInteraction`（只在有人坐在终端前的交互式 session 里带，而且该 workspace 没有写 `allow_unattended_exec`）。**外部 MCP 永远不发这个键**：bridge 不知道 Host 那头有没有人，冒充知道比不说更糟，所以那个开关对 bridge 没有任何影响。

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
| `search_text` | 200 条结果、上下文 10 行、整体 32 KiB、单行 512 字节 |
| `exec_command` 超时 | 最大 600000 ms（10 分钟） |
| `exec_command` 回传 | 预览总共默认 4 KiB，`preview_bytes` 最大 16 KiB；stderr 最多占一半，其余给 stdout，某个流超出时只留它的开头和结尾。完整输出用 `output_ref` 读 |
| `read_output` 一次最多 | 32 KiB（默认 16 KiB） |
| `apply_patch` | 一次最多 50 个文件；一次请求里所有文件的新内容**合计** 1 MiB；被编辑的文件超过 16 MiB 直接拒绝 |
| 保留输出 | 每个 session 最多 100 次运行 / 64 MiB，超了删最旧的 |
| `instructions` | 16 KiB（含项目说明文件），超了按行切断 |

保留的输出**留在远端**，只在这个 session 的目录里。session 结束后由 ccnm 原有的维护动作清理；契约不承诺任何保留时长。

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
