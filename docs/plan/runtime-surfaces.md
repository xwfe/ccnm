# ccnm 双执行入口与后续实施方案

本文固定 ccnm 接下来两个彼此正交、共用同一 Runtime 的产品入口，并把 P7 真机暴露的身份矛盾收敛成可实施的架构决定。它是后续模型实施时的设计约束；实时完成状态仍以 [status.json](status.json) 为准，验收编号仍在 [ROADMAP.md](ROADMAP.md)。

参考材料：

- [Claude Code SSH Remote 功能解读](https://ccb.agent-aura.top/docs/features/ssh-remote)
- [Claude Code Tool 系统解读](https://ccb.agent-aura.top/docs/tools/what-are-tools)

这两份材料是第三方源码解读，用来帮助判断产品边界，不作为 Claude Code 官方稳定接口。Provider 的具体参数、登录、输出和版本兼容仍必须按仓库既有原则真机测量。

## 1. 产品结论

ccnm 不是“远程 Claude Code 的替代实现”，也不是“通用 SSH 命令 MCP”。它的核心是：

> **让 AI Coding Agent 在不占有项目机器完整权限、不复制项目和 Agent 凭据的前提下，使用一个有明确 workspace 边界、执行身份和生命周期的远程 Runtime。**

围绕这个 Runtime，保留两个入口：

```text
入口 A：Managed Agent Runtime（主线，v1）

Operator @ Runtime Node
        │ control
        ▼
Agent Node
  Controller + Claude/Codex
        │ persistent SSH stdio MCP
        ▼
Runtime Executor (ccrun) @ Runtime Node
        │
        └─ authoritative workspace / Git / build / test


入口 B：Remote Workspace MCP（扩展，v1.x）

Claude Code / Codex / ChatGPT / other MCP client
        │ stdio MCP
        ▼
ccnm bridge @ client/Agent machine
        │ persistent SSH
        ▼
Runtime Executor (ccrun) @ remote Runtime Node
        │
        └─ authoritative remote workspace
```

两者共享 Runtime 数据面，不共享 Agent 生命周期：

- A 由 ccnm 选择、启动、观察和停止官方 Agent；需要 Agent Provider、Controller/session 和 Machine API。
- B 的 Agent 已经由外部客户端启动；ccnm 不管理它，不读取它的登录，不需要知道调用方是 Claude 还是 Codex，只提供标准 MCP workspace 能力。

**A 是 ccnm v1 的核心差异化能力；B 是 v1 稳定后的第一个优先扩展。B 不反向阻塞 v1。**

## 2. 从 Claude SSH Remote 得出的边界

参考实现的 SSH Remote 走的是另一条路线：把 Claude/兼容 CLI 放到远端运行，通过双向消息、自动部署和认证反向隧道让远端 Agent 使用本地认证，然后直接调用远端文件/Bash 工具。

ccnm 不复制这条路线。原因不是技术做不到，而是它会破坏当前产品最有价值的三个边界：

1. **Agent 与 Runtime 分离。** Agent 登录、订阅和官方 CLI 留在 Agent Node；项目仍在 Runtime Node。
2. **凭据不隧道化。** 不实现 AuthProxy，不通过反向端口转发注入 Anthropic/OpenAI OAuth/API 凭据，不把“远端无凭据也能调用模型”作为功能。
3. **SSH 只是 transport。** 不自动把 Agent 本体部署进 Runtime；Runtime 只需要兼容的 ccnm 执行端和项目工具链。

所以两者的核心区别是：

```text
SSH Remote 类方案：Agent 移到项目旁边
ccnm Managed Runtime：Agent 留在 Agent Node，工具移动到项目旁边
```

入口 B 也不退化成“把任意 Bash 通过 SSH 暴露给 Claude”。外部 MCP client 得到的是绑定 workspace 的七工具和 Runtime 策略，而不是裸 `host + command`。

## 3. 从 Claude Tool 抽象借鉴什么

参考 Tool 解读把输入校验、权限、只读/破坏性、并发、中断和输出预算作为工具语义，而不是散落在各个调用点。MCP 自身也有 read-only/destructive/idempotent/open-world 等 annotations，但它们只是客户端提示，不是安全边界。

ccnm 不复制 Claude 的完整 Tool 类型；在 Runtime 内部只维护真正影响执行的最小语义：

```text
ToolSemantics（内部概念，不要求按此名字实现）
├─ access: read | write | exec
├─ read_only
├─ destructive
├─ idempotent_hint
├─ open_world_hint
├─ interrupt_behavior
└─ output_budget
```

当前七工具的保守分类：

| 工具 | Runtime 权限语义 | 说明 |
| --- | --- | --- |
| `workspace_info` | read | 只读 Runtime/session 信息 |
| `read_file` | read | 受 workspace path policy 约束 |
| `list_files` | read | 只读目录/Git 视图 |
| `search_text` | read | 只读搜索 |
| `read_output` | read | 读取当前 Runtime 保留输出 |
| `apply_patch` | write | 修改工作树，必须受写互斥保护 |
| `exec_command` | **exec/write-capable** | 任意程序都可能写文件或联网，永远不能靠解析命令猜成只读 |

实现 Remote Workspace MCP 时可以给标准 MCP tool annotations 填准确的提示，但真正授权仍由 Runtime 的 OS identity、workspace binding、access policy 和 write guard 决定。annotations 不能代替这些门禁。

## 4. v1 必须先修正的身份模型

P7.3 已经真机证明：当前代码把“运行 public ccnm 的人”和“真正执行 workspace 工具的人”当成了同一身份。以 `ccrun` 跑 doctor 为绿，以个人账号跑同一 workspace 为红；同时当前回跳链路又迫使 `ccrun` 持有一把到 Agent 的主动 SSH 私钥。把私钥从 `~/.ssh` 挪到其他目录只是躲开启发式检查，不是隔离。

v1 固定四种角色，其中前三种是正常运行身份：

| 角色 | 运行内容 | 可以持有什么 | 不应持有什么 |
| --- | --- | --- | --- |
| Operator / Control Identity | public CLI、`ccnm rpc`、本地控制逻辑 | 到 Agent 的 control SSH 凭据、用户自己的管理配置 | 不自动继承给 Runtime 工具 |
| Agent Identity | Controller、Claude/Codex、Agent-local profile | Agent 官方登录；到 Runtime Executor 的 SSH 凭据 | Runtime 项目私密凭据除非项目明确需要 |
| Runtime Executor (`ccrun`) | `internal mcp-serve`、workspace/Git/build/test、Runtime 权威检查 | `authorized_keys` 等入站公开状态、项目最小必要凭据 | Agent 登录、个人凭据、**任何 ccnm 所需的主动 SSH 私钥/SSH agent**、sudo/admin/特权 socket |
| Administrator | 安装账号/ACL/网络策略 | 主机管理能力 | 不参与日常 Agent session |

硬约束：

> **任何以 Runtime Executor 身份运行、会处理 Agent 输入或项目数据的 ccnm 进程，都不能为了完成 ccnm 正常控制链主动 SSH 到别处。**

因此 `No SSH keys` 的长期语义也要收窄成“Runtime Executor 没有可用于 ccnm 出站的 SSH credentials”，而不是声称扫描了整台机器所有秘密。至少检查标准私钥候选、已知 ccnm transport 位置和 `SSH_AUTH_SOCK`；真正不可访问性仍靠独立账号/ACL。

## 5. Managed Agent Runtime：v1 目标链路

### 5.1 Runtime 侧发起（主场景）

```text
Operator@Runtime
  │
  │ public ccnm / ccnm rpc
  │ 只负责控制和路由
  ▼
Agent Node
  │ Controller -> official Agent
  │
  └──────── SSH stdio MCP ────────> ccrun@Runtime
                                      │
                                      └─ resolve + verify + execute workspace
```

Operator 可以持到 Agent 的 SSH key；它不是 Runtime Executor，因此 doctor 不应该用 Operator 当前 UID 代替 `ccrun` 的安全结论。

### 5.2 Agent 侧发起

当前 `Agent → Runtime(public run as ccrun) → Agent` 的回跳必须退出 v1 正式路径。目标流程：

```text
Agent Node
  │
  ├─ 1. 向 Runtime Executor 查询 workspace authority / binding
  │      （只返回非秘密的 workspace/instance/runtime identity 信息）
  │
  ├─ 2. 在 Agent 本机 Controller 启动 Claude/Codex
  │
  └─ 3. Agent 的 MCP transport SSH 到 Runtime Executor
            │
            └─ Runtime 再次按自己的权威配置验证并打开 workspace
```

内部原语的名字由实现阶段确定，可以是 `runtime-resolve` / `workspace-resolve`；关键不是名称，而是下面四条：

1. Runtime root 的最终决定在 Runtime Executor，不由 Agent/Operator 任意传绝对路径覆盖。
2. Agent Instance/binding 双端核对；旧 peer 不能忽略新 identity 后静默运行。
3. `ccrun` 只接受入站 SSH，不持出站 control key。
4. 查询与真正 `mcp-serve` 启动之间仍有竞态，所以 MCP 打开时必须重新验证 binding、安全和 write guard，不能把 resolve 当 capability token。

### 5.3 Doctor

`doctor` 分开显示 Control 与 Runtime 两类事实：

```text
Control path / Agent SSH          当前 Operator 能否控制 Agent
Runtime executor identity         真正 mcp-serve 报告的 UID/user
Runtime confinement               真正 Runtime Executor 的 safety audit 摘要
Remote MCP handshake              真实 ccrun 进程启动后的工具/版本信息
```

Runtime safety verdict 必须来自即将执行项目的 Runtime 进程，或来自与它相同 OS identity 的只读 probe；不能用 public CLI 当前进程做替身。安全报告只传结构化结论，不返回私有路径、环境值或凭据内容。

`doctor` 和 `mcp probe` 都必须能从 Runtime 侧或 Agent 侧发起；**不要在 Agent Node 直接拒绝这两个诊断命令**。Agent Node 本身是合法控制面，而且后续 Remote Workspace MCP 也需要从 Agent/client 一侧诊断远端 Runtime。

Agent 侧诊断不能再复用“把整条 public 命令委托给 Runtime”的 `public_cmd_from_agent` 路径。正确形状是 topology-aware 的双端采集：

```text
Agent Node 上执行 ccnm doctor <workspace>
  ├─ 本机：Agent instance / provider / auth / Controller / session
  ├─ SSH → Runtime Executor：runtime-resolve / runtime-audit
  └─ SSH → Runtime Executor：真实 MCP handshake / tools probe

Agent Node 上执行 ccnm mcp probe <workspace>
  └─ 本机直接建立 Agent → Runtime Executor 的实际 MCP transport 并测量
```

其中 Runtime Executor 只回答 Runtime 自己的事实或承载 MCP；它不需要、也不允许再 SSH 回 Agent。Runtime 侧发起的 `doctor` 则继续允许 Operator → Agent，由 Agent 再直接进入 Runtime Executor；无论从哪边发起，都不出现 `ccrun → Agent`。

`mcp probe --local` 在 Agent-only 拓扑上没有“本地项目”语义，应明确拒绝或只在真正 colocated/local Runtime 时允许，不能为了兼容把它偷换成 remote probe。

### 5.4 Runtime authority 与配置

逻辑事实来源保持一个：Runtime Node 的 workspace registry。不同 OS 用户为了 transport/UX 保存的本地配置不能形成第二个可覆盖 root 的权威来源。

v1 改造优先采用最小方案：

- Runtime Executor 加载自己的权威 workspace 配置并验证 root、runtime_user、Agent binding；
- Operator 侧配置只用于本机能验证的控制路由/展示，不能让调用方下发不同 root 绕过 Runtime；
- Agent 侧只保存 Agent-local profile、Node alias 以及运行所需公开引用，不复制 Runtime root 列表作为事实来源；
- 如果现有公开命令为了本地 UX 仍保存 root，Runtime 打开时必须重新以 Executor 配置为准，二者不一致 fail-closed。

不要为 v1 身份修正临时引入常驻 root daemon。以后若多用户/系统服务需求证明需要共享 Runtime registry，再单独设计 system service/socket。

## 6. P7 / v1 修复的实施批次

在 Codex 额度等待期间，Claude Code 应先完成 P7.4 的架构修复，不必等待 P7.3 Codex 付费验证。每一批单独提交、更新状态并停下来核对；不要一次重写整条链。

### Batch A — 身份契约先行

- 更新 `production-safety.md`、架构和运维文档：明确 Operator / Agent / Runtime Executor / Admin。
- 将 `runtime_user` 定义成 **Runtime Executor expected identity**，不再定义 public CLI 应由谁运行。
- 明确 `ccrun` inbound-only；废弃“把 ccnm transport 私钥藏到 `~/.ssh` 之外就算安全”的正式方案。
- 加离线契约测试：Operator 与 Runtime audit 分离、Runtime 无出站凭据仍可被 Agent SSH 进入。

停止点：只完成身份语义和测试骨架，不改启动链。

### Batch B — Runtime authority / resolve

- 抽出一个 Runtime-only resolve/open 边界，让 workspace name 在 Runtime 权威配置中解析。
- 新 internal wire 版本不能信任调用方 root；Managed binding 必须包含并核对 Agent identity。
- 旧协议兼容只保留到明确的 migration boundary；新链不能在对端不理解时静默回退。
- `mcp-serve` 打开时仍重新做 config binding、安全、root canonicalization、write guard。

停止点：resolve 可以独立离线/假 SSH 测试，还没有切 public launcher。

### Batch C — 切 Managed control path

- Runtime 发起：Operator 直接控制 Agent；Agent 反向进入 ccrun。
- Agent 发起：先 resolve Runtime authority，再本机 Controller start；删除正式路径上的 `ccrun → Agent` 回跳。
- CLI 与 `ccnm rpc` 共用同一应用逻辑，不恢复“RPC shell out 到 CLI”的实现。
- session identity、start_key、result、stop 等现有 Machine API 语义不因换链路漂移。

停止点：全部离线/golden/black-box 通过后再上真机。

### Batch D — Doctor 与安全结论

- Runtime safety rows 改成权威 Runtime probe 结果；Operator 本地只报告 control-path 自己能证明的事实。
- workspace root 检查补“执行身份实际可用于项目”的语义：至少覆盖实际 owner/Git safe-directory 问题，不把“目录可写”说成“项目可用”。
- SSH credential finding 改成精确措辞并覆盖已知 ccnm transport 位置与 agent socket。
- 不把 inaccessible Docker socket 说成不存在；结论和观测分开。

停止点：doctor 同一 workspace 从不同 Operator 身份运行时，Runtime Executor 部分必须一致。

### Batch D2 — Agent 侧诊断去回跳

Batch D 完成后、Batch E 真机复验前必须补这一批。它解决的不是新的产品功能，而是把两个现有公共诊断入口纳入已经确定的 `ccrun inbound-only` 硬约束。

- Agent Node 上的 `ccnm doctor <workspace>` 不再把整条 public doctor 命令 SSH 到 Runtime；在 Agent 本机组合 Agent/Controller/session 检查，并直接向 Runtime Executor 请求权威 resolve/audit 与实际 MCP probe。
- Agent Node 上的 `ccnm mcp probe <workspace>` 直接从 Agent Identity 建立到 Runtime Executor 的 MCP transport；不允许 `Runtime Executor → Agent` 回拨。
- Runtime Node 上原有 doctor/probe 语义保持：Operator 可以控制 Agent，真正 Runtime verdict 仍由 Executor 自己回答。
- 两个来源的诊断对同一 Runtime Executor 应给出同一 Runtime safety/workspace 结论；Agent-only 路径不得需要 Runtime Executor 的私钥或 `SSH_AUTH_SOCK`。
- 给 `delegate_public_from_agent` 增加边界测试：doctor/mcp probe 不再走它；如果它仍服务其他非诊断命令，不能顺手扩大范围。
- Agent-only 拓扑的 `mcp probe --local` 明确 fail-closed，不静默解释成 remote。

停止点：离线测试证明，从 Agent 发起 doctor/probe 时命令轨迹中不存在“先到 Runtime 再由 Runtime SSH 回 Agent”的公共命令委托。完成并提交后才进入 Batch E。

### Batch E — v1 回归与真机再确认

身份/控制链改变会使旧 P7.3 的 Claude 真机证据不再完整覆盖新路径。无需重复整个昂贵 dogfood，但至少重新完成一次：

1. Operator public CLI → Agent → ccrun 的 Claude smoke/parity；
2. `ccnm rpc` 同路径一次真实 session；
3. 产物属主确认为 Runtime Executor；
4. `ccrun` 的 `~/.ssh`、已知 ccnm transport 私钥位置无私钥，`SSH_AUTH_SOCK` 不可用；
5. Runtime Executor 进程轨迹中没有 ccnm 所需 outbound SSH；
6. 从 Agent Node 各跑一次 `ccnm doctor` 与 remote `ccnm mcp probe`，两者不要求 ccrun 持私钥/SSH agent，Runtime verdict 与 Runtime 侧发起一致；
7. result/status/stop、write guard 和资源归零；
8. 完整离线门禁重新跑。

Batch E 只做**最小真机复验**，不重复 P7.3 的完整昂贵 dogfood。部署范围只覆盖当前 P7 测试拓扑所需的两端 ccnm 二进制/Controller 兼容更新；替换前记录版本/路径/哈希并保留可回退副本。不要趁本批创建新账号、改 ACL/防火墙、跑 Codex、发布/tag/push，除非另有明确授权。

Codex 额度恢复后再按新身份模型补 P7.3 缺失的一次 real-machine Machine API parity。**只有 Claude 新路径复验 + Codex 缺口补齐 + P7 其余 blocker 处理完，才做最终 v1 freeze。**

## 7. Remote Workspace MCP：v1.x 产品入口

该入口解决的是：Claude Code/Codex 已经在本机或 Agent 机器运行，但项目在 NAS、服务器、Mac mini、Linux 开发机等远端，Agent 需要比裸 SSH execute 更完整的项目能力。

它不是 Agent Provider：

```text
External MCP Host
      │
      │ initialize / tools/list / tools/call
      ▼
ccnm local bridge
      │
      │ one persistent SSH stdio connection
      ▼
remote ccnm Runtime
      │
      └─ workspace-scoped seven tools
```

### 7.1 首版范围

- 标准 **stdio MCP server** 入口，命令名在 P9 定稿；文档暂以 `ccnm mcp connect <workspace>` 表示设计目标，不代表当前已经存在。
- 一次 MCP process 固定一个 workspace 和一个 Runtime alias；连接中不能动态换 host/root。
- remote root 只由 Runtime 权威 workspace 配置解析，客户端不能给任意绝对目录。
- 使用现有 OpenSSH alias；不内置 Tailscale/FRP/Cloudflare，不做 SCP 项目同步，不自动部署远端 Agent。
- 复用现有七工具、路径/符号链接策略、输出保留、安全 audit 和工作树 write guard。
- 首版不做任意 `ssh_exec(host, command)`、端口转发、SFTP 浏览器、数据库/容器专用工具；这些会把 workspace Runtime 退化成万能远控面板。

### 7.2 授权模型

Remote MCP 默认关闭写能力，不能因为客户端能 SSH 到 ccrun 就自动获得所有项目的写权限。P9 要把下面的 Runtime-side policy 定稿：

```text
disabled       不允许外部 MCP 打开该 workspace
read           只暴露确定只读工具
coding         暴露七工具，持有 workspace writer guard
```

策略必须来自 Runtime 权威配置；client 只能请求不高于该策略的模式，不能自行升级。

`read` 模式绝不能包含 `exec_command`：即使名字叫 `cat`，任意 exec 也能写磁盘、访问网络、启动后台进程。若未来需要“只读 shell”，必须依赖独立 OS sandbox/allowlisted executable contract，不能解析 shell 字符串假装安全。

外部 MCP 的 SSH key 仍是 transport credential。对单用户 v1.x，OpenSSH identity + 独立 Runtime OS account 是认证边界；多租户、第三方共享、细粒度 token 不在首版，不能用一个共享 `ccrun` key 冒充多租户授权。

### 7.3 与 Managed session 共用写互斥

这是两个入口能安全共存的硬条件：

```text
Managed Claude/Codex session ─┐
                              ├─ same Runtime workspace write guard
Remote Workspace MCP client ──┘
```

任何 coding MCP process 都必须和 Managed session 竞争同一个 Runtime guard。不能出现“Managed path 有锁、MCP bridge 直接调用工具绕过锁”。busy/unknown 的外部 MCP 表达方式在 P9 契约阶段定稿。

### 7.4 Tool annotations 与权限 UX

外部 MCP 会被不同 Host 消费，因此在 SDK/协议支持时发布准确的标准 annotations：

- 确定只读的 file/list/search/info/output 标 `readOnlyHint`；
- patch/exec 不标只读；
- `exec_command` 保守地视为 destructive/open-world-capable；
- 不把“某个命令看起来安全”动态改注解；
- annotations 只改善 Host 的审批 UX，Runtime 仍自行 enforce access mode 和 guard。

Claude Code 自己还有更丰富的 Tool 权限/并发/中断语义；ccnm 不依赖这些私有字段。不同 Host 完全忽略 annotations 时，Runtime 的安全结果也必须相同。

### 7.5 项目 instructions

Managed Provider 已知道 Claude/Codex，因此可以按已测 Provider 规则投影 `CLAUDE.md` / `AGENTS.md`。External MCP client 则不能可靠从 MCP 握手推断自己是什么 Agent。

首版不要偷偷猜客户端。P9 在以下两种窄方案中择一并用真客户端验证：

1. Runtime workspace 明确配置一个 external-MCP instruction policy；或
2. bridge 注册时显式选择公开的 context policy（它只影响上下文格式，不是权限）。

无论选哪种，默认都提供通用 workspace/tool 说明；不读取 Client 机器的私人 Agent 配置，也不把 Agent profile 当 Runtime context 配置。

## 8. Machine API、Remote MCP、Orchestrator 三者不要混

```text
Machine API (`ccnm rpc`)
    = control plane：启动/观察/停止 Managed Agent session

Remote Workspace MCP
    = data/tool plane：外部已运行 Agent 直接操作一个 Runtime workspace

独立 Orchestrator
    = policy plane：决定谁做什么、顺序、review/retry/merge
```

Orchestrator 可以把 ccnm Machine API 作为 `ExecutionBackend`；需要直接给某个外部 Agent workspace 工具时，也可以让那个 Agent 使用 Remote Workspace MCP。但 ccnm 不因此加入 planner/router/task DAG。

不要在 v1.x 为 ChatGPT 单独实现私有协议。ChatGPT/Claude/Codex 是否能接入，取决于它们当时支持的标准 MCP transport；本地 stdio 与云端 remote MCP 的网络/auth 是不同问题。只有真实客户端要求后，才另立安全的 remote transport 阶段。

## 9. Remote Workspace MCP 实施阶段

ROADMAP 的 P9–P12 对应以下顺序；P8 仍先完成独立 Orchestrator 的接口交接，因为它只写边界，不要求先创建新项目。

### P9 — 契约与权限模型

先写配置/CLI/MCP 契约，不写桥接实现。定稿：workspace opt-in、read/coding、context policy、busy/unknown、tool annotations、连接生命周期、错误与输出边界。禁止 raw host/root/credential 参数成为 MCP tool input。

### P10 — stdio bridge 与复用 Runtime

实现本地 MCP server → persistent SSH → remote `mcp-serve`。优先复用现有 Runtime server；若需要新 internal open payload，只做 transport/binding 差异，不复制七工具实现。EOF/中断必须回收 SSH 子进程，不杀死无关 Managed session。

### P11 — 跨入口安全/并发与真实 Host

验证 Managed 与 Remote MCP 竞争同一 write guard；read mode 无 exec/write；Host 忽略 annotations 也无法越权。至少用一个真实 Claude Code MCP 配置和一个 provider-neutral MCP 测试客户端跑七工具/只读矩阵，不依赖模型自然语言自述验收。

### P12 — 远端真实项目 dogfood 与 v1.x 冻结

至少一个真正远端项目（优先 Linux server，以补当前 macOS-only 证据）完成 read/search/patch/exec/output、断线/重连、版本错配、权限拒绝、资源归零。更新支持矩阵后才把 Remote Workspace MCP 从 experimental 提升为支持能力。

## 10. 明确不做

在上述 P7/P9–P12 结束前，不自动扩张到：

- AuthProxy / OAuth/API token 隧道；
- 把官方 Agent 自动部署进 Runtime；
- generic SSH/SFTP/port-forward 管理器；
- 第三 Agent Provider；
- Browser/Git/数据库/Kubernetes 等专用工具目录；
- 多 Agent planner/router/reviewer；
- 多租户共享 Runtime；
- HTTP daemon / 公网 MCP 网关；
- 根据 shell 文本猜“这条 exec 只读”。

这些以后都可以讨论，但不能因为相邻产品有功能就稀释 ccnm 的 Runtime 边界。

## 11. 后续模型的执行顺序

看到当前 `status.json.current_task == "P7"` 时：

1. 先读 P7 真机记录，**不要重做已经通过的 P7.1/P7.2 和 Claude 完整 dogfood**。
2. 优先实施 P7.4 的身份/控制链 Batch A→E；这是当前已经定下的产品决策，不再等待“是否要修”的确认。
3. 每个 Batch 有独立测试和提交；改 wire/protocol 时同步 fixture，但禁止为绿灯重录与行为无关的 golden。
4. 新路径至少做 Claude 最小真机 parity；Codex 额度恢复后再补 P7.3 Codex 缺口。
5. P7.3 与 P7.4 都有证据后，完成 P7.5 v1 freeze。
6. 再做 P8 交接文档。**P8 完成不等于创建/实现 Orchestrator。**
7. P9–P12 是 ccnm 自己的 v1.x Remote Workspace MCP；逐阶段实施，不和 Orchestrator 并行改同一执行契约。

如果真实代码证明某一批需要调整顺序，可以改计划，但必须先在 status blocker/evidence 写清楚“哪条已验证假设被推翻”，不能静默换架构。

## 12. Codex 原生执行链（P21–P24；P21–P23 已完成，离线可验，P24 真机验收待授权）

来自跨仓计划 toexec v2 的 V2-C。**现在的 Managed Codex 关掉自己的执行工具，改用 ccnm 的七个 MCP 工具**；原生链让 Codex 用它自带的执行工具，由官方 `codex exec-server` 在 Runtime 上执行。它是入口 A 在 Codex 上的一个 opt-in 变体，默认仍走 MCP，不影响 Claude 和入口 B。

```text
Agent Node（Agent Identity）                                        Runtime Node（ccrun）
Codex ── stdio（它自己 spawn 的子进程）──> ccnm exec-transport ── SSH stdio ──> ccnm exec-serve ── stdio ──> codex exec-server
         按 CODEX_HOME/environments.toml 起      exec 成一条 ssh                 解析 workspace / 审计
         没有端口、没有秘密                      每会话一条，Codex 保证            写锁 / 按方法过滤
```

### 12.1 谁负责什么

| 位置 | 负责 | 不负责 |
| --- | --- | --- |
| Agent 侧传输程序 | 被 Codex 按每会话的 `environments.toml` 启动，exec 成一条到 Runtime 的 ssh；每会话一份 CODEX_HOME（`auth.json` symlink 到 profile，`environments.toml`，`config.toml` 只有信任条目） | 不解析方法、不做授权——授权只在 Runtime 做一次，避免两份规则漂开 |
| Runtime 受管入口 | 按自己的配置解析 workspace、安全审计、取写锁、监督 exec-server、逐条过滤 JSON-RPC | 不信任客户端给的 root、sandbox 或版本声明 |
| exec-server | 执行已放行的请求 | **不能当权限边界**：它完全信客户端传来的 sandbox |

授权放在 Runtime，是因为路径要在项目所在的文件系统上解析 symlink 才判得准，root 也只有 Runtime 知道。

**为什么不是 WebSocket 网桥。**立项时按 `CODEX_EXEC_SERVER_URL` 设计：ccnm 在 Agent 本机监听回环端口、按对端 uid 放行、每会话只放一条连接（toexec G05-peer 实测通过）。P23 开工前核对 0.154.0 源码：TUI 启动时先读 `CODEX_HOME/environments.toml`，里面每个环境要么写 `url`（WebSocket），要么写 `program`/`args`/`env`/`cwd`——后者由 Codex 自己 spawn 成子进程、拿它的 stdin/stdout 走同一套 JSON-RPC，客户端里这条传输没有 reconnect 策略。实测（[P23 记录](../research/p23-stdio-transport-2026-09-16.md)）：TUI 真的这样连；传输程序死掉后 Codex 不再起第二个，第二条命令报 `exec-server transport disconnected`；`--ignore-user-config` 会关掉 environments.toml，但交互模式本来就不传它。于是网桥要解决的三件事——连接身份、只放一条、不 resume——都由"Codex 自己 spawn、自己持有管道"直接给出，不需要监听端口，也不需要新依赖。

依据（toexec 仓库，均为 Codex 0.154.0 实测、零模型额度）：[连接身份](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/g05-peer/README.md)（URL 令牌方案否决；按 uid 放行通过；放行重连时 Codex 会自己 resume——这两条现在只作对照）、[协议](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/g01/README.md)（没有版本协商；未知通知和超过 64 MiB 的帧直接断连）、[权限](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/g06/README.md)（`sandbox: null` 就不受限；`http/request` 无限制；`environmentConfig/read` 返回服务端配置里的凭据）、[stdio 传输](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/p23-stdio/README.md)（environments.toml 的 program 传输、断线不重连、symlink 的 auth.json 读写穿透）。

### 12.2 原生读和 MCP 读同一个契约（用户 2026-09-16 决定）

**exec-server 的文件读方法**（`fs/readFile`、`fs/open`/`readBlock`、`fs/readDirectory`、`fs/walk`、`fs/getMetadata`、`fs/canonicalize`）**按 ccnm `read_file` 的路径契约校验**：先查原始输入、拒绝 `..`，再解析 symlink，结果必须仍在 workspace 根内。不看请求里的 sandbox 是什么——Codex 自己发的这些请求本来就是 `sandbox: null`。

Codex 启动时会从工作区一路往上查 `.git`（实测直到 `/`）。根以上的这类查询**不转给 exec-server**，由受管入口照 exec-server 自己的"不存在"原样回答（`-32004`）；P21 实测这样回答后 Codex 的请求和"上面确实没有仓库"时一致。代价：workspace 是某个 Git 仓库的子目录时，Codex 看不到上层仓库——不拦的话，它找到上层 `.git` 后还会去那个仓库根读 `AGENTS.md`，那已经在根外了。

没选的方案：让"能读的范围 = ccrun 账号能读的范围"。实现简单，但原生入口会比 MCP 入口宽，同一个 workspace 换个入口就能读到根外的文件。

**命令执行不受这条约束**，与 MCP 的 `exec_command` 一样：命令能读 ccrun 能读的一切，写入受 Codex 发来的 sandbox 限制（受管入口要求 sandbox 存在且根钉在 workspace 上）。真正的上限仍是 ccrun 身份，见第 4 节。

### 12.3 首版范围

- **只开 coding 会话。**Codex 靠跑命令读文件，而只读会话不开任意命令（第 7.2 节同一理由），原生链开了也没法用；只读会话继续走 MCP。
- **只开交互模式，启动时传 `-C <Runtime 根>`。**`codex exec`（print）会先在 Agent 本机检查这个目录，要求 Agent Node 上有同一绝对路径，而 Runtime 根常在 Agent 账号建不了的地方；交互模式不检查。代价是 Machine API（只有 print）起的 Codex 会话继续走 MCP。依据见 [P21 记录](../research/p21-codex-native-surface-2026-09-16.md)第 1 条。
- **规则表只核对 sandbox 在不在是不够的。**人在 Codex 里批准提权后，命令会带 `sandbox: null`，越界 patch 会带一条多出来的路径写条目；逐方法的规则见 P21 记录的规则表。
- **Linux Runtime 要装 bubblewrap，并允许执行账号创建 user namespace**，否则 Codex 发来的沙箱起不来（失败即拒，命令不执行）。
- **不 resume。**断线就结束会话，与 ccnm v1 一致；Codex 对 stdio 传输没有重连策略（实测传输死掉后不再起第二个），受管入口另外拒绝带 `resumeSessionId` 的握手。
- **每个原生会话一份 CODEX_HOME。**Codex 只从 `CODEX_HOME/environments.toml` 读传输配置，profile 目录是凭据所在、多个会话共用，不能写会话文件进去。session 目录下的 `codex-home/` 放 `environments.toml`、指向 profile `auth.json` 的 symlink、只含信任条目的 `config.toml`；Codex 的会话记录、历史和缓存也落在这里，随 session 目录一起 purge。代价：profile 自己的 `config.toml` 在原生会话里不生效（print 模式本来就带 `--ignore-user-config`，MCP 交互会话仍读它），要改模型走实例注册表的 `model` 字段。
- **只对 Codex Agent 生效。**同一 workspace 的 Claude 会话仍走 MCP 七工具；opt-in 的 workspace 起 Codex print 会话在创建 session 前拒绝，不退回 MCP。
- `http/request` 一律拒绝；exec-server 的环境和 MCP `exec_command` 的子进程用同一套清理，`CODEX_HOME` 由 ccnm 生成、不含凭据。
- **会话结束先证明进程都没了才放锁。**exec-server 给每条命令单独开进程组，`setsid` 脱离的进程它关 stdin 时也不清；ccnm 按每个会话独有的环境变量标记扫进程表，扫不干净锁就留在 `held`（P22）。
- Claude 经 exec-server 是另一件事（toexec v2 的 V2-P 实验线），不在这里。
