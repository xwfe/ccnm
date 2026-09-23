# 架构说明

## 用角色描述能力，不用位置描述机器

ccnm 现在把机器统一抽象成 **Node**，再由角色说明这个 Node 负责什么。`home` 和 `work` 不再是架构概念。

Node 可以是：

- 笔记本；
- 台式机；
- Mac mini；
- NAS；
- VM；
- 云服务器。

同一个 Node 可以同时承担多个角色。

## Agent Node

运行 AI Coding Agent，并持有对应的登录/订阅/OAuth 凭证。

legacy 配置运行官方 Claude Code；Agent Instance 通过明确 enum 分发到 Claude 或实测版本的 Codex CLI。项目源码不要求存在于 Agent Node。第三 Provider 尚未实现，也不需要插件系统。

## Runtime Node

保存真实 workspace 和项目 toolchain。

Runtime 工具在这里执行，包括：

- 文件读取；
- 目录列举；
- 内容搜索；
- patch；
- 命令执行；
- 测试/构建；
- 后续可能加入的 Browser 等 runtime provider。

项目的事实来源在 Runtime Node，不通过 rsync、SMB 或云盘复制到 Agent Node。

P48/P50 后另有 Agent 侧 `ccnm_agent`：提供 Agent 安装的 skills，以及按配置允许的 MCP server。它不是 Runtime 工具的别名，使用 Agent Identity；本机 MCP opt-in 会扩大 Agent 的执行面。默认值与风险见[生产安全](production-safety.md#两侧-skills-与-mcp-的信任边界)。

## Controller

Controller 是 session 管理角色，不是项目数据角色。

当前 macOS 实现中，Controller 运行在 Agent Node 的 GUI 登录会话里，通过 LaunchAgent 常驻。这样官方 CLI Agent 可以使用自己的正常登录上下文，同时又能接受从 SSH 发起的 ccnm 请求。

Controller 负责：

- 启动 session supervisor；
- 创建和管理 tmux 交互会话；
- 按 Provider 获取官方 CLI 版本和登录状态；
- 管理 session 状态。

它不保存 workspace 真相，也不替 Runtime Node 执行项目工具。

## 四种操作系统身份

Node 说的是"哪台机器负责什么"，身份说的是"哪个账号在动手"。两者不是一回事，一台 Node 上可以有好几个身份：

- **Operator**：你本人，敲 public CLI / `ccnm rpc`。可以持有到 Agent Node 的 SSH 私钥。
- **Agent Identity**：Agent Node 上跑 Controller 与官方 CLI 的账号，持有登录/订阅，以及连到 Runtime Executor 的 SSH 私钥。
- **Runtime Executor**（建议叫 `ccrun`）：Runtime Node 上跑 `internal mcp-serve` 和全部项目工具的低权限账号。**入站专用**——Agent 连进来，它不为 ccnm 的控制链连出去。
- **Administrator**：建账号、配 ACL、改网络策略，不参与日常 session。

它不是新的 Node 角色，也不是 AI 账号。要分开是因为：**`exec_command` 到底继承哪个操作系统身份的权限**。Runtime Executor 执行项目命令；若启用 Agent 本机 MCP，Agent Identity 也可能执行模型请求，不能再声明它只有登录和控制能力。

完整表格、硬约束和当前实现与它的差距见 [生产安全](production-safety.md)；差距怎么收敛见[双执行入口方案](plan/runtime-surfaces.md)。

## 当前双 Node 拓扑

```text
Runtime Node                                  Agent Node
┌──────────────────────────────┐              ┌─────────────────────────────┐
│ workspace                    │              │ Claude Code / Codex CLI     │
│ Git / cargo / node / python  │              │ login / subscription        │
│ ccnm internal mcp-serve      │◀── SSH ────▶│ ccnm controller + tmux     │
│ Runtime Service Account      │   stdio MCP  │ session supervisor          │
└──────────────────────────────┘              └─────────────────────────────┘
```

网络上传输的是 MCP 请求和结果，不是整个仓库副本。

## 为什么选择 SSH stdio MCP

核心原因：

- 一条持久 SSH stdio transport 承载整个 MCP session，不需要每个工具调用重新起 SSH；
- workspace 的事实来源始终留在 Runtime Node；
- 没有 SMB/rsync 的缓存一致性和双份源码问题；
- AI 凭证始终留在 Agent Node；
- ccnm 只消费已有 OpenSSH alias，不接管 Tailscale/VPN/Tunnel；
- 项目工具自然运行在真正拥有 toolchain 的机器上。

## topology 模型与当前支持

拓扑由配置决定，不靠探测机器。判据只有两个：`this`（我是哪个 node）和 workspace 的 `agent_node` / `runtime_node`。

### 1. `runtime -> agent -> runtime`

项目在我这台，Agent 在那台，工具调用再回到我这台。这是当前主线。

```text
Runtime Node
    │
    │ 解析 workspace / 校验 root
    │
    ├──── SSH ────> Agent Node
    │               │
    │               ├─ Controller
    │               ├─ Claude/Codex / tmux
    │               │
    │               └──── SSH stdio MCP ────> Runtime Node
    │                                         ccnm internal mcp-serve
    │
    └─ 当前终端 attach 到 Agent session
```

### 2. `agent -> runtime`

坐在跑 Agent 的那台机器上发起。它不存 workspace 列表（顶层 `runtime_node` 就是这个意思），所以**先问 Runtime 这个 workspace 是什么**（`internal runtime-resolve`，只读，不启动任何东西），拿到 root、runtime node、instance 引用和 permission mode 之后，**在本机把会话起起来**，再本地 attach。已有 session 的 `attach/status/result/stop` 一直在 Agent 本机执行，避免 Runtime 短暂离线时连终端也无法管理。

诊断走同一条规矩：`ccnm doctor` 在这台机器上查 Controller、官方 CLI、登录和 tmux，向 Runtime 只发两个只读问题（`runtime-resolve` 与 `runtime-audit`），MCP transport 也由这台主动开；`ccnm mcp probe` 同理。**没有任何一条路径要求 Runtime 反过来连 Agent。** `--local` 在这台机器上直接拒绝——项目不在这儿，测本地 Runtime 无从谈起。

问一句而不是自己存一份，是为了避免出现第二份 workspace root 配置。两份列表就是两个"这个项目在哪"的答案，其中一份迟早过期，然后某个会话绑到一个已经搬走的目录上。

**以前不是这样的。** 以前这台机器把整条公共命令 `ccnm run <ws> --detached` ssh 过去，让 Runtime Node 去启动会话——work → home → work 绕一圈。代价是 P7.3 在真机上量出来的：ssh 落到的账号是 Runtime Executor，于是**它**在跑 launcher，而且必须持一把回连 Agent Node 的出站私钥。一个会执行模型产出内容的身份还能主动连出去，就没有任何它自己能证明的边界。现在过去的只有问题，答案回来，会话在 Agent 本机创建——Claude 本来就跑在这台。

### 3. `runtime -> agent`

Agent 和项目在同一台机器上，workspace 的 Agent Node 和 `runtime_node` 相同就是这种模型。

```text
这台（只负责发起）──── SSH ────> devbox
                                 ├─ Controller
                                 ├─ CLI Agent / tmux
                                 └─ 项目就在本地磁盘
```

当前 build **明确拒绝 colocated 启动**。Claude 的 native 候选命令已去掉 remote-only MCP/工具限制，但 installed CLI 尚未真机验证；Codex colocated 没有测量。不能因为配置能表达就声称支持，也不能静默降级。

### 为什么不是"单 Node"

安全边界是进程身份、文件权限和传输，不是物理机器的标签。同一机器可以承担多个角色，但默认形态下 Runtime 执行身份不能访问 Agent 凭据；这种部署必须单独验收，不能靠跳过诊断来证明隔离。一台机器一个账号、项目和登录在同一个家目录的情况根本没有东西可隔离，要跑就得在那个 workspace 上写 `allow_unisolated_credentials` **明确接受边界不存在**——它不是把诊断关掉：那几行照旧显示，只是从 FAIL 变成注明了接受者的 WARN。代价见[生产安全](production-safety.md#凭据隔离那一条怎么放开代价是什么)。

如果未来重新开放第 3 种拓扑，它只能作为显式受信任的 native 模式，不能冒充隔离 Runtime。当前准确结论见[支持矩阵](support-matrix.md)。

### 未来多 Agent

```text
Agent A ─┐
Agent B ─┼── coordination ──> Runtime Node(s)
Agent C ─┘
```

ccnm 不实现多 Agent 编排；图中的 coordination 属于独立 Orchestrator。节点概念保持通用，不代表 ccnm 承诺内建任务图、调度或业务验收；见[生命周期与职责](project-lifecycle.md)。

## 信任边界

MCP runtime 已经做了：

- workspace path policy；
- symlink/越界检查；
- 有界输出；
- patch 事务和版本检查；
- 所有已知 Agent 凭证环境变量剥离等保护；
- 工作树级 Runtime 单写 guard。

但 `exec_command` 仍然是在 Runtime OS 账号下执行真实程序。

因此真正的主机权限边界是：

```text
Runtime Executor 身份
+ filesystem ACL
+ sudo/admin 权限
+ credential exposure
+ 出站 SSH 凭据（含 SSH agent）
+ Docker/本地特权接口
+ network policy
```

而不是“命令名黑名单”。

出站 SSH 凭据算在边界里，是因为 Runtime Executor 有一把可用私钥，就等于 `exec_command` 能以它的名义连到别的机器。**现在没有任何一条 ccnm 路径要求它持有这样一把钥匙**：会话、诊断、MCP transport 全部是 Agent 连进来。这是 P7.4 的结果，并已在真机上复验：会话活着时执行身份的进程表里只有入站 sshd 与 `mcp-serve`，没有任何 ssh 客户端（[Batch E 记录](research/p7-batch-e-2026-09-10.md)）。细节见[生产安全](production-safety.md)与[双执行入口方案](plan/runtime-surfaces.md)。

## 历史术语

旧设计/研究文档中：

```text
home machine ≈ Runtime Node
work machine ≈ Agent Node
```

这些名称只代表当时的实验拓扑，不再是当前公开 API/config 模型。

## Agent Provider 内部边界

`crates/ccnm-core/src/provider/` 通过明确的 `AgentProvider::{Claude, Codex}` enum 分发；没有插件注册或动态加载。公开入口只接受已配置的 instance id，不能直接注入 Provider、路径或官方 CLI argv；Agent Node 本机 registry 决定实际 Provider/profile。

- `provider/claude/`：CLI 定位、version/auth 探测、配置目录环境变量、启动参数、交互/print 输入、MCP 配置和工具权限、结果解析，以及项目 instruction/context 规则。
- `provider/codex/`：已实测的官方 CLI `0.154.0` 适配（唯一接受的版本，见[支持矩阵](support-matrix.md)）、Agent-local HOME、固定工具策略、JSONL 结果和 Runtime 根目录 AGENTS 上下文。未测版本和 colocated 模式拒绝启动。
- `provider/types.rs`：Controller、work 和报告消费者使用的 Agent 观测/结果；保留 v1 字段形状。
- `provider` 的凭据元数据声明环境前缀、已知目录/容器、文件名和 egress 检查目标。P1 由 `safety/` 统一执行所有已知 Provider 的可访问性检查和分来源环境策略。凭据可访问或未知**不可由 `allow_unconfined_exec` 跳过**——那个开关只接受 confinement 风险，要接受凭据这一条得单独写 `allow_unisolated_credentials`；身份未知和继承来的认证环境两个开关都放不开。不读取或传递凭据内容。
- `session/transport.rs` 是两 Provider 共用的 Agent-side stdio wrapper；Claude MCP JSON 和 Codex 会话参数均指向它，再由它清理环境并执行 OpenSSH。SSH 与 Runtime child 的机制不放在 Codex 模块里；详情见 [安全契约](provider-safety.md)。
- session/Controller 负责进程、tmux 和生命周期；launcher/work 负责 topology、OpenSSH alias 与 Runtime Node 握手。Runtime 当前有 12 个工具定义，实际工具表按权限和配置生成；Agent 的 skills/MCP 是另一执行面，不能用早期七工具的边界概括它。

legacy 公开配置仍是 `claude_config_dir`、`claude_permission_mode`。Rust 内部使用通用字段名，通过 serde 显式保留旧 session/协议的 `claude_config_dir`、`claude_bin`、`claude-auth`、`claude`；没有顺便重命名旧字段。

### 行为等价的验证边界

`tests/fixtures/claude-provider-baseline.json` 是从抽取前的代码生成的合成行为快照，**不是新的真机测量**。它固定启动参数/环境/stdin、两种会话模式、remote/colocated 输入、策略文件、上下文、结果及旧协议形状。已有 `claude-print-2.1.260.json` 真机 fixture 原样保留。

P3 将 Claude colocated 候选命令中的 remote-only `--tools ""`、`--mcp-config`/strict 参数移除，并在单独断言中记录该差异，没有重录 golden。由于尚无 installed Claude 真机验收，公共启动在创建 session 前明确拒绝该 topology。

### 第二 provider 的兼容与隔离

历史 Codex internal 会话保留 `protocol=2` fixture；公开 Agent Instance 使用 v3 `InstanceRef` 请求，由 Agent 解析出完整 identity。旧 peer 因版本不匹配拒绝请求，不能忽略 identity 后误启动 Claude。Claude 的 v1 序列化不增加 provider 字段，旧配置字段和响应标签不改名。Codex 结果有独立 provider 标签；未报告的费用/API 耗时不填假零。

专用 HOME 由 Agent 自己按 ccnm 配置目录解析，不接受 Runtime 传来的路径，不读取认证文件内容。用户必须在 Agent 登录会话中独立使用官方 CLI 登录；目录与认证文件要求仅属主可访问、非符号链接。启动前通过官方 CLI 检查版本、登录和空 MCP inventory。所有 Codex SSH 连接在 Agent 侧清除敏感环境、禁止 agent forwarding，不复用个人 ControlMaster；Runtime payload 只有 workspace、root、session 和 provider 等执行上下文。

Codex 项目上下文仅投影 Runtime 根目录的 `AGENTS.override.md` 或 `AGENTS.md`，空 override 仍覆盖 base；不自动枚举嵌套 instructions。P48/P50 后，Agent 侧会按配置读取已安装 skills 和 MCP 定义，不能再笼统声明“不读 Agent 私人配置”。原生 CLI 文件/shell 工具策略、Runtime 账号权限与第三方 MCP 权限是不同层，固定 CLI tool policy 不等于 sandbox。

实测依据见 [Codex 内部接线](research/codex-internal-wiring-2026-09-07.md)；P3 公共入口的当前验收级别见[支持矩阵](support-matrix.md)。不扩展为多 Agent coordination 或并行 worktree 调度。

## Agent Instance 执行边界（P2/P3）

P2 增加 node-scoped instance 配置与公开身份 DTO：Runtime workspace 保存 node/instance 引用；Agent registry 保存 provider/profile_ref；私有目录在 Agent-local profiles.toml 中独立解析。不复制 root 或远端 profile 定义。`WorkspaceBinding` 分别由 Runtime 校验 root/引用、Agent 校验完整 registry identity；`ResolvedAgent`/`ResolvedProfile` 不实现 Serialize/Debug，私有目录不进入绑定消息。

P3 将 v3 identity 接入现有 launcher/work/Controller/supervisor/tmux/SSH MCP。Runtime 发出受限 `InstanceRef`；Agent 每一层重新解析并比较 identity，Controller 启动前重新加载权威配置，supervisor 再解析 profile。session 固定 workspace/root/runtime_node/identity；不同 Provider 或 instance 不复用也不自动替换。ccnm session id 与 Provider thread/resume id 分开。

Runtime MCP 初始化再用自己的配置重算 binding；legacy payload 不能打开 instance workspace。完整契约见[单 Agent 执行](agent-execution-p3.md)和[实例配置](agent-instance-config.md)。本阶段没有协调器、分布式 lease 或 worktree 调度。

## Runtime 权威解析（P7.4 Batch B）

"哪个项目、在哪、给谁开"由 **Runtime Executor 自己回答**，答案来自它本机的配置和文件系统。

旧的 serve payload 里有一个 `root`，是调用方传过来的。绑定过的 payload 会拿它跟 Runtime 的 workspace 定义核对，但**这个形状本身在问调用方项目在哪**——而信任它给的路径，就等于信任它对这台机器的处置：每一次工具调用、写入 guard、保留输出和安全结论都挂在那个目录上。

新的 open 请求（内部 wire protocol **4**）里**没有 root 字段**，也没有任何路径。它只说 workspace 名字和调用方解析出来的 Agent identity，其余由 Runtime 查自己的注册表。`deny_unknown_fields` 让这成为 wire 属性而不是约定：对端硬塞一个 root 进来是解码失败，不是被默默忽略。

身份核对分工明确：**Runtime 只认它拥有的那一半——workspace 的 Agent Node**，外加拓扑和 provider 能力。instance/provider/profile 是 Agent 本机的事实，而 `ccnm run --agent` 本来就允许在同一个 node 上换 instance，所以 Runtime 不重复校验 instance，由 Agent 自己的 registry 接受或拒绝。

**resolve 不是 capability token。** 从解析到真正 `mcp-serve` 打开之间有竞态，所以打开时 binding、安全审计、root canonicalize 和写入 guard 全部重做一遍。

两种 wire 靠 `protocol` 数字区分，没有第三条路也没有回退：本 build 不认识的数字直接 `CCNM_E_VERSION`。旧 build 拿到 protocol 4 也一样——它缺 `root` 又多 `agent`，解码就失败。

**公共 launcher 已在 P7.4 Batch C 接入无 root 的 open 请求**（这条授权边界在 protocol 4 引入）；外部 Workspace MCP 使用自己的 protocol 5 请求，也由 Runtime 解析 root。前文描述 Batch B 的动机，不表示当前仍待切换；实际版本以实现的握手为准，历史 payload 的兼容解析不应被当作新增调用方的默认入口。

## Runtime 单写者

有写权限的 MCP server 在 Runtime 初始化时按 canonical workspace resource 获取内核独占锁，并持有到收尾结束；外部 read 连接不因此占写权。Git workspace 使用 canonical `git-common-dir`，所以**共用同一 state 目录时**，不同 Agent Node、CLI/RPC 入口、路径 alias 及共享 common-dir 的 worktree 不能获得两份受管写权限。不同账号或不同 `XDG_STATE_HOME` 形成不同锁域，不提供全机或跨 state 的互斥。

正常退出写 `released` 并显式 unlock；异常退出保留 `held` marker。后者即使内核锁已经释放也保持 unknown，直到 Runtime 操作者证明旧进程与子进程结束后人工恢复，不按时钟自动转让。这个机制拒绝并发受管 writer，不承诺任意 shell 的 exactly-once、事务回滚或 sandbox。操作边界见[支持矩阵](support-matrix.md)。

P43 在普通命令收尾报告残留时保留 `held/abandoned`；但这不是任意后代进程的完整证明。**P51 已复现 Runtime MCP relay 的 leader 正常退出、同组子进程仍写文件、写锁却 `released` 的缺陷**；当前不得声明“所有 server 后代已结束才放锁”，细节和修复验收见[审计 C51-01](research/2026-09-23-lifecycle-and-docs-audit.md)。
