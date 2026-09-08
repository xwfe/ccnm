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

当前实现运行的是官方 Claude Code。项目源码不要求存在于 Agent Node。

以后接入 Codex、Gemini CLI 或其他 Agent 时，也应该继续使用 Agent 角色，而不是再为每种物理部署方式发明新的机器名。

## Runtime Node

保存真实 workspace 和项目 toolchain。

当前 MCP 工具都在这里执行，包括：

- 文件读取；
- 目录列举；
- 内容搜索；
- patch；
- 命令执行；
- 测试/构建；
- 后续可能加入的 Browser 等 runtime provider。

项目的事实来源在 Runtime Node，不通过 rsync、SMB 或云盘复制到 Agent Node。

## Controller

Controller 是 session 管理角色，不是项目数据角色。

当前 macOS 实现中，Controller 运行在 Agent Node 的 GUI 登录会话里，通过 LaunchAgent 常驻。这样官方 Claude Code 可以正常访问自己登录会话中的 Keychain/OAuth 上下文，同时又能接受从 SSH 发起的 ccnm 请求。

Controller 负责：

- 启动 session supervisor；
- 创建和管理 tmux 交互会话；
- 获取 Claude 版本和登录状态；
- 管理 session 状态。

它不保存 workspace 真相，也不替 Runtime Node 执行项目工具。

## Runtime Service Account

`ccrun` 是 Runtime Node 上建议使用的低权限 Unix 账号。

它不是新的 Node 角色，也不是 AI 账号。它解决的是：**`exec_command` 到底继承哪个操作系统身份的权限。**

详细边界见 [生产安全](production-safety.md)。

## 当前双 Node 拓扑

```text
Runtime Node                                  Agent Node
┌──────────────────────────────┐              ┌─────────────────────────────┐
│ workspace                    │              │ Claude Code                 │
│ Git / cargo / node / python  │              │ OAuth / subscription        │
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

## 支持的三种拓扑

拓扑由配置决定，不靠探测机器。判据只有两个：`this`（我是哪个 node）和 workspace 的 `agent_node` / `runtime_node`。

### 1. `runtime -> agent -> runtime`

项目在我这台，Claude 在那台，工具调用再回到我这台。这是主线。

```text
Runtime Node
    │
    │ 解析 workspace / 校验 root
    │
    ├──── SSH ────> Agent Node
    │               │
    │               ├─ Controller
    │               ├─ Claude Code / tmux
    │               │
    │               └──── SSH stdio MCP ────> Runtime Node
    │                                         ccnm internal mcp-serve
    │
    └─ 当前终端 attach 到 Agent session
```

### 2. `agent -> runtime`

坐在跑 Claude 的那台机器上发起。它不存 workspace 列表（顶层 `runtime_node` 就是这个意思），所以先把启动命令委托给 Runtime Node，再走上面那条完整路径，最后在本地 attach。

多这一跳是为了避免出现第二份 workspace root 配置。两份列表就是两个"这个项目在哪"的答案，其中一份迟早过期，然后某个会话绑到一个已经搬走的目录上。

### 3. `runtime -> agent`

**Claude 和项目在同一台机器上**，我只是从别处把会话拉起来、attach 上去。workspace 里 `agent_node` 和 `runtime_node` 是同一个 node 就是这种。

```text
这台（只负责发起）──── SSH ────> devbox
                                 ├─ Controller
                                 ├─ Claude Code / tmux
                                 └─ 项目就在本地磁盘
```

这条路径**不建 MCP 通道**，Claude 直接用自己的原生工具（Read/Edit/Write/Grep/Glob/Bash）读写眼前的项目。

具体差别就两个文件：不写 `mcp.json`，`settings.json` 里也不 deny 原生工具。**这两条都不能搞错**——deny 列表存在的意义是"项目在另一台机器上时，别让模型碰到本机磁盘"；项目就在本机时它只会碍事，结果是一个能启动但读不了任何文件的会话。

### 为什么不是"单 Node"

早先的文档写过一个"Agent + Runtime + Controller 全在一台机器"的单 Node 形态。**那个形态跑不起来**，而且不是实现没跟上，是自相矛盾：

- Agent Node 必须持有 Claude 凭证，否则没法登录；
- Runtime Node 绝不能持有 Claude 凭证（第 [生产安全](production-safety.md) 节），否则它就成了 Anthropic 出口。

同一台机器同时被当成两个 node 来审计，doctor 会因为"它是它自己"给出 6 条无法修复的 FAIL。

第 3 种拓扑解决的正是这个需求，做法是**不把那台机器当成 Runtime Node 来审计**：它只承担 agent 角色，项目恰好也在那儿，不启用 MCP runtime，也就没有"runtime 必须隔离"这套要求。想要"就在一台装了 Claude 的电脑上干活"，用这个。

### 未来多 Agent

```text
Agent A ─┐
Agent B ─┼── coordination ──> Runtime Node(s)
Agent C ─┘
```

多 Agent 编排还没有实现。现在只是保证底层概念不会再次被 `home/work` 这种物理位置命名限制住。

## 信任边界

MCP runtime 已经做了：

- workspace path policy；
- symlink/越界检查；
- 有界输出；
- patch 事务和版本检查；
- Claude 凭证环境变量剥离等保护。

但 `exec_command` 仍然是在 Runtime OS 账号下执行真实程序。

因此真正的主机权限边界是：

```text
Runtime Service Account
+ filesystem ACL
+ sudo/admin 权限
+ credential exposure
+ Docker/本地特权接口
+ network policy
```

而不是“命令名黑名单”。

## 历史术语

旧设计/研究文档中：

```text
home machine ≈ Runtime Node
work machine ≈ Agent Node
```

这些名称只代表当时的实验拓扑，不再是当前公开 API/config 模型。

## Agent Provider 内部边界

`crates/ccnm-core/src/provider/` 通过明确的 `AgentProvider::{Claude, Codex}` enum 分发；没有插件注册、动态加载或公开 provider 配置选择器。公开入口仍只选 Claude，Codex 限于版本化 internal 请求的验证。

- `provider/claude/`：CLI 定位、version/auth 探测、配置目录环境变量、启动参数、交互/print 输入、MCP 配置和工具权限、结果解析，以及项目 instruction/context 规则。
- `provider/codex/`：已实测的官方 CLI `0.153.4` 适配、Agent-local HOME、固定工具策略、JSONL 结果和 Runtime 根目录 AGENTS 上下文。未测版本和 colocated 模式拒绝启动。
- `provider/types.rs`：Controller、work 和报告消费者使用的 Agent 观测/结果；保留 v1 字段形状。
- `provider` 的凭据元数据声明环境前缀、已知目录/容器、文件名和 egress 检查目标。P1 由 `safety/` 统一执行所有已知 Provider 的可访问性检查和分来源环境策略，未知/认证失败不可由 unconfined 开关跳过；不读取或传递凭据内容。
- `session/transport.rs` 是两 Provider 共用的 Agent-side stdio wrapper；Claude MCP JSON 和 Codex 会话参数均指向它，再由它清理环境并执行 OpenSSH。SSH 与 Runtime child 的机制不放在 Codex 模块里；详情见 [安全契约](provider-safety.md)。
- session/Controller 仍负责进程、tmux 和生命周期；launcher/work 仍负责 topology、OpenSSH alias 与 Runtime Node 握手。Runtime MCP 的 7 个工具和执行边界未变。

公开配置仍是 `claude_config_dir`、`claude_permission_mode`。Rust 内部使用通用字段名，通过 serde 显式保留旧 session/协议的 `claude_config_dir`、`claude_bin`、`claude-auth`、`claude`。旧 `claude` 和 `mcp::context` 模块只是兼容出口，不再承载实现。

### 行为等价的验证边界

`tests/fixtures/claude-provider-baseline.json` 是从抽取前的代码生成的合成行为快照，**不是新的真机测量**。它固定启动参数/环境/stdin、两种会话模式、remote/colocated 输入、策略文件、上下文、结果及旧协议形状。已有 `claude-print-2.1.260.json` 真机 fixture 原样保留。

有一个已存在的差异刻意未修复：colocated session 不写 `mcp.json`、settings 不 deny 原生工具，但启动函数仍无条件传 `--tools ""`、`--mcp-config` 等 remote 参数。现有 colocated 测试使用假 supervisor，不能证明真实 Claude 接受这组输入；本轮快照保留该现状，避免把功能修复夹进内部解耦。

### 第二 provider 的兼容与隔离

Codex 的启动、探测、session 和 MCP 请求必须显式携带 `provider="codex"`、`protocol=2`。旧 peer 因版本不匹配拒绝请求，不能忽略 provider 后误启动 Claude。Claude 的 v1 序列化不增加 provider 字段，旧配置字段和响应标签不改名。只读响应保留 v1 外层格式，Codex 结果有独立 provider 标签；未报告的费用/API 耗时不填假零。

专用 HOME 由 Agent 自己按 ccnm 配置目录解析，不接受 Runtime 传来的路径，不读取认证文件内容。用户必须在 Agent 登录会话中独立使用官方 CLI 登录；目录与认证文件要求仅属主可访问、非符号链接。启动前通过官方 CLI 检查版本、登录和空 MCP inventory。所有 Codex SSH 连接在 Agent 侧清除敏感环境、禁止 agent forwarding，不复用个人 ControlMaster；Runtime payload 只有 workspace、root、session 和 provider 等执行上下文。

Codex 项目上下文仅投影 Runtime 根目录的 `AGENTS.override.md` 或 `AGENTS.md`，空 override 仍覆盖 base。它不是完整的 Codex 本机文件遍历：不读 Agent 私人配置，不自动枚举嵌套 instructions。Runtime/MCP 七工具及 ccrun/ACL/sudo/network policy 仍是原边界，固定 CLI tool policy 不等于 sandbox。

实测依据与尚未开放的边界见 [Codex 内部接线](research/codex-internal-wiring-2026-09-07.md)。不扩展为 Agent Instance、多 Agent coordination 或并行 worktree 模型。
