# 配置说明

ccnm 默认读取：

```text
~/.config/ccnm/config.toml
```

也可以通过全局 `--config` 或环境变量 `CCNM_CONFIG` 指定其他文件。

**每台机器有自己的一份，内容不一样。** 不要把同一份文件复制到两台机器上——里面的 `ssh` alias 是"从本机出发"的，复制过去就指错地方了。

配置描述的是 **Node**、**workspace** 和 Agent Node 本机的 **Agent Instance**。Node 名是你自己起的标识符；`ccnm init` 默认用 `agent` 和 `runtime`。旧 `agent_node`/Claude 字段不改名，继续兼容。

## Agent Instance 模型

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

workspace 中的 `agent` 是默认选择。公共命令可用同一 Node 上的 instance id 覆盖：

```bash
ccnm doctor demo --agent codex-main
ccnm run demo --agent codex-main
ccnm status demo --agent codex-main
ccnm result demo --agent codex-main --session <ccnm-session-id>
ccnm attach demo --agent codex-main --session <ccnm-session-id>
ccnm stop demo --agent codex-main --session <ccnm-session-id>
```

`--agent` 只替换 instance id，不接受 `worker/codex-main`、`provider=codex`、路径、root 或原始官方 CLI 参数。Runtime 用自己的 workspace 配置固定 node/root；Agent 再从本机 registry 解析 provider/profile。legacy `agent_node` workspace 使用 `--agent` 会明确报错，不会静默改成 Claude 或 Codex。

在 Agent-only 配置所在的机器上，`run`、`doctor` 和 MCP probe 会先去 Runtime 获取 workspace 权威信息；已存在 session 的 `attach/status/result/stop` 仍在 Agent 本机执行，这样 Runtime 链路暂时断开时终端管理行为不变。要强制校验 instance，请显式带 `--agent`；稳定自动化应再带 `--session`。

迁移预览目前仅有库 API `configedit::Edit::preview_instance(workspace, &InstanceRef)`，返回候选 TOML，不修改 editor 或磁盘，没有自动迁移命令。已有自定义 `claude_config_dir`、非默认权限或跨 Node 迁移会拒绝机械转换，需先确定语义；其他 workspace 与注释保留。更多约束见 [实例契约](agent-instance-config.md)。

Agent Instance 公共执行已接入现有 Controller/session/SSH MCP，Claude 和 Codex 两个方向都在授权双机上真机跑通，专用低权限执行身份的凭据隔离也已实测。仍未验证的是 egress/网络策略——因此本项目不声明任何出口边界。不要把“代码可执行”写成“已生产支持”，准确范围见[支持矩阵](support-matrix.md)。

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
ccnm_bin = "~/.local/bin/ccnm"   # 可选：它上面 ccnm 的路径，这一行写的就是默认值
claude_config_dir = "/path"      # 可选：Agent 角色用的 CLAUDE_CONFIG_DIR
runtime_user = "ccrun"           # Runtime Executor 期望的系统账号
```

哪些必填取决于这个 node 承担什么角色：

- workspace 里除本机之外的每个 node 都要有 `ssh`；
- 只有 Agent 角色用得上 Claude 相关配置；
- 只有 Runtime 角色用得上 `runtime_user`。

`ccnm_bin` 可以是绝对路径，也可以是 `~/` 开头——`~` 由**对面**的登录 shell 展开，这是每种 shell 都认的写法。别人的家目录（`~someone/...`）不行，`..` 也不行，路径里只能有 `[A-Za-z0-9._/-]`，因为它要出现在一条 ssh 命令行上而 ccnm 不给它加引号。

`runtime_user` 说的是 **Agent 的 MCP transport 落到哪个账号上**，项目工具就以谁的身份执行。它不规定谁可以敲 `ccnm`——那是 Operator，通常就是你自己的账号。四种身份怎么分见[生产安全](production-safety.md)。

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
allow_unisolated_credentials = false   # 默认值；开它之前先读下面那一节
allow_unattended_exec = false          # 默认值：每条命令执行前问你一次
external_mcp = "disabled"    # 默认值，可省略
```

### `agent_node`

legacy Claude workspace 中运行 Agent 的 node。新 workspace 可改用：

```toml
agent = { node = "worker", instance = "claude-main" }
```

两种 selector 不能同时出现。`agent` 的 provider/profile 不在 Runtime 定义。

### `runtime_node`

存真实项目、执行 MCP tools 的 node。**注意这是 workspace 里的字段，跟顶层那个同名字段不是一回事**：这里说的是"这个项目在哪台机器上"，顶层说的是"我不存列表，去问谁"。

把它写成和 Agent Node 相同会形成 colocated 配置模型，但当前执行入口明确拒绝：Claude 的 native 候选启动尚未真机验收，Codex colocated 未测。不要据此配置生产 workspace；见[支持矩阵](support-matrix.md)。

### `root`

Runtime Node 上真实项目的绝对路径。

### `claude_permission_mode`

直接映射官方 Claude Code 的 `--permission-mode`。默认 `acceptEdits`。可写的值和官方一样：`acceptEdits`、`bypassPermissions`、`plan`、`manual`、`auto`、`dontAsk`。

**它盖过你自己 `~/.claude/settings.json` 里的 `permissions.defaultMode`**，因为 ccnm 是把它当命令行参数传给官方 CLI 的，而命令行赢设置文件。所以你在 Agent Node 上写了 `"defaultMode": "bypassPermissions"`，ccnm 会话里照样一个个问你——要改得改这里。

这个字段写在 **Runtime 侧**的配置里（workspace 定义在哪它就在哪），**只对之后新起的会话生效**，正在跑的会话不会变。

```toml
[workspaces.my-project]
claude_permission_mode = "bypassPermissions"
```

**它管不到 `exec_command`。** 交互式会话里那个工具带着 `anthropic/requiresUserInteraction`，Claude Code 在任何权限模式下都认，所以每次执行命令还是会问你一次——[故意的，理由在这里](troubleshooting.md#开了-bypasspermissionsexec_command-还是每次都问)。要关掉它得单独写 [`allow_unattended_exec`](#allow_unattended_exec)，或者走 `--print`（那条路上本来就不带这个键）。

**代价说清楚**：`bypassPermissions` 是"什么都不问直接跑"。如果这个 workspace 同时开了 `allow_unisolated_credentials`，那就是**模型改文件、读你的 Agent 登录都不经你确认**——`exec_command` 那一问会是唯一还有人在场的环节。只在你自己的机器、你自己的项目上这么配。

instance workspace（用 `agent` 而不是 `agent_node` 的）不接受这个字段，配了会被拒；instance 的策略在 Agent 端。

### `allow_unconfined_exec`

逃生开关，不是生产配置：

```toml
allow_unconfined_exec = true
```

它允许 Runtime OS 账号没通过 confinement 检查时仍然执行 `exec_command`，但每条命令结果都会标记 runtime **未隔离**。

真实项目应该在 Runtime Node 建 `ccrun` 之类的专用低权限账号，然后把它改回 `false`。

**它waive不了凭据那一条**，那是下面那个开关的事。

### `allow_unisolated_credentials`

跑这个 workspace 命令的账号能读到本机已知的 Agent 登录（`~/.claude`、`~/.codex` 之类）时，仍然允许打开：

```toml
allow_unconfined_exec = true                 # 两个都要写
allow_unisolated_credentials = true
```

**先读一遍你接受了什么**：模型跑的每一条命令都能读到那份登录，而让它跑一条命令只需要一句 prompt——包括从它被要求读的文件里冒出来的那一句。这是这个项目唯一那条硬边界，放开之后没有别的东西在挡着。完整说明和代价见[生产安全](production-safety.md#凭据隔离那一条怎么放开代价是什么)。

**两个开关互不蕴含。** `allow_unconfined_exec` 说的是"这个账号 OS 权限比它该有的大"，这一个说的是"它能读我的 Agent 登录"，是两件事，所以要分别写。

**名字为什么是这个。** 它和 `allow_unconfined_exec` 同一个形状：两句都在说"那个性质不成立，也放行"，而且说的都是**性质**（confinement / isolation），不是机器。最早写成 `allow_agent_credentials_on_runtime`，读起来像还有个 `on_agent` 与之配对——并没有：**所有 workspace 字段都只从 Runtime 自己的配置里读**，承担风险的那台机器自己决定，调用方说了不算。

ccnm 的反应：第一次用它启动会话时在终端上把风险讲一遍（**只讲一次**；关掉再打开算新决定，会再讲），`ccnm doctor` 里那几行永远显示为 **WARN 并注明是接受的**（不会变成 OK），每条命令结果里的 unconfined 说明也会写明这一条。

**任何开关都放不开的两条**：执行身份未知，以及认证环境是继承来的（`ANTHROPIC_*` / `CLAUDE_*` 在 Runtime 服务环境里）。前者没人能说清是谁接受了什么，后者是把凭证直接塞给每一个子进程。

### `allow_unattended_exec`

交互式会话执行命令前不再问你：

```toml
allow_unattended_exec = true
```

**默认是会问的，而且任何权限模式都关不掉。** ccnm 给 `exec_command` 挂了 `anthropic/requiresUserInteraction`，Claude Code 在每一种权限模式下都认它——`bypassPermissions` 也一样。理由很直接：一个调用方能关掉的闸门不叫闸门。这个开关是**承担风险的那台机器**把它关掉的唯一入口。

只有 `exec_command` 会问。另外六个工具被路径策略框在 workspace 根目录里，这一个是别人机器上的一个 shell。

**它跟前两个开关不是一类东西：它不授权任何事。** 命令能做什么由 `exec_gate` 和 Runtime 执行身份决定，这个开关一点都动不了；它只决定中间还有没有人。所以：

- `ccnm doctor` 里那行 `Command approval` 会变成 WARN，**永远不会是 OK**；
- 第一次用它起会话时终端上讲一次风险（只讲一次，关掉再打开算新决定）；
- `--print` 和 `ccnm mcp bridge` **完全不受影响**——那两条路上本来就不问，因为两边都没人在等。

**想要"不被打断"，先考虑 `--print`。** 一问一答、不常驻、输出直接落在你本机终端，边界还是执行身份本身。见[使用说明](usage.md#不想被打断先想想---print)。

### `external_mcp`

外部 MCP Host（你本机已经在跑的 Claude Code / Codex / 别的客户端）能不能把这个 workspace 当成远程项目工具用，以及最多能做什么：

```toml
external_mcp = "read"        # disabled（默认）| read | coding
```

| 值 | 给出去的东西 |
| --- | --- |
| `disabled` | 什么都没有。不写这一行就是它 |
| `read` | 四个只读工具：`workspace_info` / `read_file` / `list_files` / `search_text` |
| `coding` | 七工具，并且**持有这个工作树的写入互斥锁**，和受管会话抢同一把 |

**不写就是关着的**：别人能 SSH 到 Runtime 账号，不等于能打开这台机器上每一个项目。客户端可以要求比这更少（`--mode read`），要求更多会被拒绝启动，不会静默降级。

只给外部 MCP 用的 workspace **可以没有 Agent**：没有 `agent` 也没有 `agent_node` 时，只要 `external_mcp` 不是 `disabled` 就合法——那种项目从来不由 ccnm 启动 Agent。它必须定义在自己的 Runtime Node 上。

用法和限制见 [Remote Workspace MCP 契约](protocol/remote-workspace-mcp-v1.md)，验收范围见[支持矩阵](support-matrix.md)。契约于 2026-09-11 冻结。

### `external_instructions`

外部客户端在 MCP 握手里拿到什么项目说明。**只影响上下文，不影响权限**：

```toml
external_instructions = "generic"   # generic（默认）| project | none
```

- `generic`：只有 ccnm 自己那段（这是哪个 workspace、路径都是相对的、这次能不能写）。
- `project`：再加上项目自己的说明文件。Runtime 按固定顺序找 `AGENTS.md` → `CLAUDE.md`，取第一个存在的——外部客户端的 provider 无从得知，也不能靠它自称的名字去猜。
- `none`：什么都不给。

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

这些字段是在项目第一次发布之前移除的，没有兼容层：

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
