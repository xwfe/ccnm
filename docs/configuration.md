# 配置说明

ccnm 默认读取：

```text
~/.config/ccnm/config.toml
```

也可以通过全局 `--config` 或环境变量 `CCNM_CONFIG` 指定其他文件。

配置描述的是 **Node** 和 **workspace**。Node 名只是用户自己选择的标识符；`ccnm init` 默认使用 `agent` 和 `runtime`。

## Runtime Node 配置

Runtime Node 通常保存 workspace 列表，并知道两个 SSH 方向：

```toml
## AI agent 所在机
[nodes.agent]
ssh_from_runtime = "agent-ssh-alias"

## 项目所在地
[nodes.runtime]
ssh_from_agent = "runtime-ssh-alias"
runtime_user = "ccrun"

[workspaces.my-project]
agent_node = "agent"
runtime_node = "runtime" # 可省略，默认就是 runtime
root = "/Users/me/code/my-project"
claude_permission_mode = "acceptEdits"
```

### `ssh_from_runtime`

表示：**从 Runtime Node 出发**访问这个 Node 时使用的 OpenSSH alias。

例如：

```toml
[nodes.agent]
ssh_from_runtime = "agent-ssh-alias"
```

就是 Runtime Node 执行：

```bash
ssh agent-ssh-alias ...
```

### `ssh_from_agent`

表示反方向：**从 Agent Node 出发**访问这个 Node 时使用的 alias。

```toml
[nodes.runtime]
ssh_from_agent = "runtime-ssh-alias"
```

就是 Agent Node 执行：

```bash
ssh runtime-ssh-alias ...
```

这些 alias 都来自已有 `~/.ssh/config`。ccnm 不保存 IP、SSH 私钥、Tailscale Node ID 或 Tunnel URL。

## Agent-only Node 配置

如果一个 Node 只承担 Agent/Controller 角色，不保存项目，就不应该复制 workspace 列表：

```toml
[nodes.runtime]
ssh_from_agent = "runtime-ssh-alias"
```

当你在这里执行：

```bash
ccnm my-project
```

它会让 Runtime Node 解析 workspace 并执行完整启动路径，然后在 Agent Node 本地 attach 到 session。

这样每个 workspace root 永远只有一个定义，不会出现双份配置漂移。

## Node 字段

```toml
[nodes.some-node]
ssh_from_runtime = "alias"       # Runtime -> 此 Node 时使用
ssh_from_agent = "alias"         # Agent -> 此 Node 时使用
ccnm_bin = "/absolute/path/ccnm" # 可选：此 Node 上 ccnm 的远端执行路径
claude_config_dir = "/path"      # 可选：Agent 角色使用的 CLAUDE_CONFIG_DIR
runtime_user = "ccrun"            # Runtime 角色期望的系统账号
```

字段是否必需取决于 Node 承担的角色：

- workspace 的 `agent_node` 必须有 `ssh_from_runtime`；
- workspace 的 `runtime_node` 必须有 `ssh_from_agent`；
- Agent Node 才需要 Claude 相关配置；
- Runtime Node 才需要 `runtime_user`。

一个物理 Node 可以同时具备这些字段，也就是同时承担多个角色。

## Workspace 字段

当前主路径 backend 是 `mcp-ssh`：

```toml
[workspaces.my-project]
backend = "mcp-ssh"
agent_node = "agent"
runtime_node = "runtime"
root = "/absolute/project/root"
claude_permission_mode = "acceptEdits"
allow_unconfined_exec = false
```

### `agent_node`

运行 AI Coding Agent 的 Node。当前实现是运行 Claude Code 的 Node。

### `runtime_node`

保存真实项目并执行 MCP tools 的 Node。省略时默认是：

```toml
runtime_node = "runtime"
```

### `root`

Runtime Node 上真实项目的绝对路径。

### `claude_permission_mode`

直接映射官方 Claude Code 的 `--permission-mode`。默认值为 `acceptEdits`。

### `allow_unconfined_exec`

发布前 dogfood 逃生开关：

```toml
allow_unconfined_exec = true
```

它允许 Runtime OS 账号没有通过 confinement 检查时仍执行 `exec_command`，但每次结果都会明确标记 runtime **未隔离**。

这不是生产安全配置。真实项目应创建 `ccrun` 或其他专用 Runtime Service Account，并把它改回 `false`。

## CLI 修改配置

Runtime Node：

```bash
ccnm init --agent agent-ssh-alias --runtime runtime-ssh-alias
ccnm workspace add my-project /absolute/project/root
```

Agent-only Node：

```bash
ccnm init --runtime runtime-ssh-alias
```

workspace 管理：

```bash
ccnm workspace list
ccnm workspace add <name> [path]
ccnm workspace remove <name>
```

`ws` 是 `workspace` 的别名。

## 为什么不保留旧配置兼容层

项目尚未发布，因此这次直接移除了旧的：

```text
[hosts.*]
work_host
runtime_host
ssh
ssh_from_work
```

当前统一使用：

```text
[nodes.*]
agent_node
runtime_node
ssh_from_runtime
ssh_from_agent
```

现在 dogfood 阶段一次性完成破坏性迁移，比发布之后长期背兼容层成本更低。
