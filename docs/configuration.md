# 配置说明

ccnm 默认读取：

```text
~/.config/ccnm/config.toml
```

也可以通过全局 `--config` 或环境变量 `CCNM_CONFIG` 指定其他文件。

**每台机器有自己的一份，内容不一样。** 不要把同一份文件复制到两台机器上——里面的 `ssh` alias 是"从本机出发"的，复制过去就指错地方了。

配置描述的是 **Node** 和 **workspace**。Node 名是你自己起的标识符；`ccnm init` 默认用 `agent` 和 `runtime`。P2 另支持下文的 Agent Instance 配置模型，但实例执行入口尚未开放。

## Agent Instance 模型（可解析，暂不可执行）

Runtime 只持引用，不复制 Agent 的 provider/profile 定义：

```toml
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "agent-ssh-alias"
[workspaces.demo]
root = "/runtime/project"
agent = { node = "worker", instance = "codex-main" }
```

Agent 在自己的配置中定义 instance；node 由 this 给出，id 是表名：

```toml
this = "worker"
runtime_node = "runtime"
[nodes.worker]
[nodes.runtime]
ssh = "runtime-ssh-alias"
[agents.claude-main]
provider = "claude"
profile_ref = "default"
[agents.codex-main]
provider = "codex"
profile_ref = "default"
```

default 按 provider 区分：Claude 为 Agent 的 `~/.claude`；Codex 保持 `~/.config/ccnm/agents/codex/`（尊重 Agent 的 XDG_CONFIG_HOME），不复用个人 `~/.codex`，不继承 CODEX_HOME 来改变身份。两个 default 不需要创建 profiles.toml。

named profile 的路径只定义在 **Agent-local** `~/.config/ccnm/profiles.toml`，不放到 Runtime 配置或 instance binding 中：

```toml
[profiles.claude-extra]
provider = "claude"
directory = "/absolute/agent/private/claude-extra"
```

这是路径 schema 示例，不代表目录已存在或已登录。文件必须归当前 Agent UID、不向组/其他用户授权（建议 0600）且非 symlink；通过 `profile_ref = "claude-extra"` 引用。profile 未定义、provider 不匹配、目录是相对路径/含 `..`、重复目录或覆盖 default 均不接受。新目录需要用户独立官方登录，ccnm 不创建、复制或链接 auth。

迁移预览目前仅有库 API `configedit::Edit::preview_instance(workspace, &InstanceRef)`，返回候选 TOML，不修改 editor 或磁盘，没有自动迁移命令。已有自定义 `claude_config_dir`、非默认权限或跨 Node 迁移会拒绝机械转换，需先确定语义；其他 workspace 与注释保留。更多约束见 [实例契约](agent-instance-config.md)。

**当前用新 agent 字段调用 run、旧内部 session/MCP 执行入口会被拒绝，不会退回 Claude。** 要继续现有工作，请保留下面的 legacy 配置；P3 才接入公共选择、真实 profile preflight 和执行闭环。

## 最小的两份配置

Runtime Node（项目所在的机器，存 workspace 列表）：

```toml
this = "runtime"

[nodes.runtime]

[nodes.agent]
ssh = "agent-ssh-alias"

[workspaces.my-project]
agent_node = "agent"
root = "/Users/me/code/my-project"
```

Agent Node（跑 Claude 的机器，不存 workspace 列表）：

```toml
this = "agent"
runtime_node = "runtime"

[nodes.agent]

[nodes.runtime]
ssh = "runtime-ssh-alias"
```

两份都由 `ccnm init` 生成，不用手写：

```bash
# 在项目所在的机器上
ccnm init --agent agent-ssh-alias

# 在跑 Claude 的机器上
ccnm init --runtime runtime-ssh-alias
```

## `this`

**这台机器是哪个 node。** 必填。

文件里每个 `ssh` 都是从这个 node 出发写的，不说清楚是谁，那些 alias 就没法解释。它同时决定了本机在一个 workspace 里扮演什么角色。

漏写会报：

```text
CCNM_E_CONFIG: `this` is not set: every `ssh` alias in this file is
written from one node's point of view, and without `this` there is no
way to know whose
```

`this` 指向的那个 node **不能有 `ssh`**——自己不 ssh 自己。写了会报 `does not ssh to itself`，因为那说明这份文件是照着别人的视角写的。

## `nodes.<name>.ssh`

**从本机出发，连这个 node 用的 OpenSSH alias。**

```toml
[nodes.agent]
ssh = "agent-ssh-alias"
```

意思就是本机执行 `ssh agent-ssh-alias ...` 能连上。

一个方向一个字段是刻意的：alias 只在定义它的那台机器的 `~/.ssh/config` 里有意义。对面那台机器有它自己的配置文件、自己的 alias，**不需要、也不应该由这份文件描述**。所以跨机器传的请求里只有 node 的**名字**，对面拿名字查自己的配置。

漏写会报：

```text
CCNM_E_CONFIG: workspaces.x.agent_node = "agent" is another machine,
所以 [nodes.agent] 需要一个 ssh alias
```

## `runtime_node`（顶层）

**这台机器不存 workspace 列表，任何 workspace 都去问这个 node。** 只有 Agent Node 写。

```toml
this = "agent"
runtime_node = "runtime"
```

不能省，也不能指向自己。它回答的不是"我是谁"（那是 `this`），而是"workspace 列表在谁那儿"：

**一台刚 `init` 完、还没 `workspace add` 过的 Runtime Node，和一台 Agent Node 的配置文件除了这一行完全一样。** 靠猜的话，猜错的那边会把请求 ssh 给对方，对方发现自己也不认识这个 workspace，再 ssh 回来。

顶层写了 `runtime_node`，就不能再有 `[workspaces.*]`——一个项目的 root 只在一台机器上定义，两份迟早对不上。

## Node 的其他字段

```toml
[nodes.some-node]
ssh = "alias"                    # 从本机连它用的 alias
ccnm_bin = "/absolute/path/ccnm" # 可选：它上面 ccnm 的路径，默认 ~/.local/bin/ccnm
claude_config_dir = "/path"      # 可选：Agent 角色用的 CLAUDE_CONFIG_DIR
runtime_user = "ccrun"           # Runtime 角色期望的系统账号
```

哪些必填取决于这个 node 承担什么角色：

- workspace 里除本机之外的每个 node 都要有 `ssh`；
- 只有 Agent 角色用得上 Claude 相关配置；
- 只有 Runtime 角色用得上 `runtime_user`。

一个 node 可以同时具备这些字段，也就是同时承担多个角色。

## Workspace 字段

```toml
[workspaces.my-project]
backend = "mcp-ssh"          # 默认值，可省略
agent_node = "agent"
runtime_node = "runtime"     # 可省略，默认就是 "runtime"
root = "/absolute/project/root"
claude_permission_mode = "acceptEdits"
allow_unconfined_exec = false
```

### `agent_node`

跑 AI coding agent 的 node。当前实现是跑官方 Claude Code 的那台。

### `runtime_node`

存真实项目、执行 MCP tools 的 node。**注意这是 workspace 里的字段，跟顶层那个同名字段不是一回事**：这里说的是"这个项目在哪台机器上"，顶层说的是"我不存列表，去问谁"。

把它写成和 `agent_node` 相同的值，就是第三种拓扑：Claude 和项目在同一台机器上，不建 MCP 通道，Claude 用自己的原生工具。见[架构说明](architecture.md)。

### `root`

Runtime Node 上真实项目的绝对路径。

### `claude_permission_mode`

直接映射官方 Claude Code 的 `--permission-mode`。默认 `acceptEdits`。

### `allow_unconfined_exec`

发布前 dogfood 逃生开关：

```toml
allow_unconfined_exec = true
```

它允许 Runtime OS 账号没通过 confinement 检查时仍然执行 `exec_command`，但每条命令结果都会标记 runtime **未隔离**。

这不是生产安全配置。真实项目应该在 Runtime Node 建 `ccrun` 之类的专用低权限账号，然后把它改回 `false`。

## CLI 改配置

```bash
ccnm init --agent <alias>       # 在项目所在的机器上
ccnm init --runtime <alias>     # 在跑 Claude 的机器上

ccnm workspace list
ccnm workspace add <name> [path]
ccnm workspace remove <name>
```

`ws` 是 `workspace` 的别名。

ccnm 用 `toml_edit` 增量修改这个文件，你写的注释不会被吃掉。写之前会整份 parse 一遍，不合法就一个字节都不写。

## 校验是严格的

未知字段直接报错，不静默忽略。把 `runtime_node` 打成 `runtime_hots`，如果被忽略就会悄悄用默认值，那正是 doctor 存在的意义所在的那种漂移。

config 里不存任何 secret：

```text
Claude OAuth        由 Claude Code 自己管
SSH private key     由 OpenSSH 管
```

## 为什么不保留旧配置兼容层

项目尚未发布，所以直接移除了旧的：

```text
[hosts.*]        work_host        runtime_host
ssh              ssh_from_work    ssh_from_runtime / ssh_from_agent
```

现在统一是：

```text
[nodes.*]        this             runtime_node（顶层）
ssh              agent_node       runtime_node（workspace 内）
```

dogfood 阶段一次性做完破坏性迁移，比发布之后长期背兼容层便宜。
