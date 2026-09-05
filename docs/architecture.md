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

## 当前一次启动的真实路径

从 Runtime Node 发起：

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

从 Agent Node 发起时，Agent-only config 故意不保存 workspace 列表，所以会先把用户级启动命令委托给 Runtime Node，再走同一条完整路径。

多这一跳的目的是避免出现第二份 workspace root 配置。

## 一个 Node 可以承担多个角色

角色模型不要求永远是“两台机器”。

### 单 Node

```text
[Agent + Runtime + Controller]
```

适合全部能力都在同一台机器的情况。

### 当前双 Node

```text
[Runtime] <────> [Agent + Controller]
```

适合项目和 AI 登录环境分离。

### 家庭服务器作为 Runtime

```text
[NAS / Mac mini: Runtime] <────> [云端或远端 Agent + Controller]
```

### 未来多 Agent

```text
Agent A ─┐
Agent B ─┼── coordination ──> Runtime Node(s)
Agent C ─┘
```

多 Agent 编排目前还没有实现。现在只是保证底层概念不会再次被 `home/work` 这种物理位置命名限制住。

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
