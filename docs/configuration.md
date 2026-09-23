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
codex_bin = "/opt/codex/bin/codex"   # 可选：Codex exec-server 链用的 Codex 二进制，绝对路径
```

哪些必填取决于这个 node 承担什么角色：

- workspace 里除本机之外的每个 node 都要有 `ssh`；
- 只有 Agent 角色用得上 Claude 相关配置；
- 只有 Runtime 角色用得上 `runtime_user`。

`ccnm_bin` 可以是绝对路径，也可以是 `~/` 开头——`~` 由**对面**的登录 shell 展开，这是每种 shell 都认的写法。别人的家目录（`~someone/...`）不行，`..` 也不行，路径里只能有 `[A-Za-z0-9._/-]`，因为它要出现在一条 ssh 命令行上而 ccnm 不给它加引号。

`codex_bin` 只有 Runtime 自己读，见下面的 [`codex_exec_server`](#codex_exec_server)。必须是绝对路径，不从 `PATH` 找：执行模型命令的那个程序不该取决于 Runtime 账号的 shell 配置。它的 `--version` 必须正好是 ccnm 实测过的 Codex 版本（现在是 0.154.0），否则会话启动前就被拒，报 `CCNM_E_VERSION`。

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
agent_tools = ["web_search", "mcp_servers"] # 默认值，可省略：受管会话能用 Agent 那边的哪些功能
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

### `codex_exec_server`

```toml
codex_exec_server = true     # 默认 false
```

**当前状态：封存（2026-09-17）。**代码保留、默认关、只认 Codex 0.154.0，不再随 Codex 版本重测，也不为它发版；开了它就是按 P24–P30 实测的样子工作，没有新的证据和承诺，新项目别开。为什么封存、什么情况下解封，见[双执行入口方案](plan/runtime-surfaces.md)第 12.0 节。默认执行路径是 MCP 七工具，Claude 和 Codex 都走它。打开只影响 Codex 交互会话；封存前验证过的范围和已知限制以[支持矩阵](support-matrix.md)为准。

打开后，这个 workspace 的受管 **Codex 交互会话**改用 Codex **自带**的执行工具（它的 `exec_command`、`apply_patch` 等），由官方 `codex exec-server` 在这台 Runtime 上执行，不再注入 ccnm 的七个 MCP 工具（设计见[双执行入口方案](plan/runtime-surfaces.md)第 12 节）。需要三样都在：这一行、Runtime node 上的 `codex_bin`、会话的 Agent 是 Codex。缺哪样就在启动前拒绝，不会退回别的执行方式。

**它管到谁、管不到谁：**

- 同一 workspace 的 **Claude** 会话不受影响，照旧走 MCP 七工具。
- Codex 的 **print 模式**（`ccnm run --print`、Machine API）在创建会话前拒绝，报 `CCNM_E_INVALID_ARGS`：`codex exec` 会先在 Agent 本机检查 `-C` 的目录，而项目不在那台机器上（[P21 记录](research/p21-codex-native-surface-2026-09-16.md)第 1 条）。要跑 print 就把这一行关掉。
- 交互会话启动前，Agent 会先经 `exec-serve` 做一次空会话预检：Runtime 没 opt-in、没 `codex_bin`、Codex 版本不对，都在起 Codex 之前报出来，而不是等 Codex 里显示"environment unavailable"。不想起会话就先看，`ccnm doctor` 的 `Codex 原生链` 一行做的是同一次预检（[使用说明](usage.md#codex-原生链那一行)）。

**Agent 那一侧发生了什么**（[P23 记录](research/p23-stdio-transport-2026-09-16.md)）：Codex 0.154.0 从 `CODEX_HOME/environments.toml` 读它的 exec-server 传输，ccnm 给每个原生会话生成一份自己的 `CODEX_HOME`（session 目录下的 `codex-home/`），里面只有三样：`environments.toml`（让 Codex 自己 spawn `ccnm internal exec-transport`，那个进程再 exec 成到 Runtime 的 ssh）、指向 profile 里 `auth.json` 的 symlink（Codex 读写都穿过它，刷新的 token 落回 profile；ccnm 不读、不复制凭据）、只写了一条对 Runtime 根 `trust_level = "trusted"` 的 `config.toml`（否则每个会话都弹一次信任提示）。**代价**：profile 自己的 `config.toml` 在原生会话里不生效，模型要走实例注册表的 `model` 字段；Codex 的会话记录、历史和缓存也落在 `codex-home/`，随 session 目录一起 `purge`。没有监听端口，别的 OS 用户没有东西可连；Codex 对这种传输不重连、不 resume，断线后的命令哪里都不执行。

它不比 coding 会话多给任何权限，但也要满足 coding 会话的全部条件：

- 执行身份的审计和 `exec_command` 一样——没确认隔离又没写 `allow_unconfined_exec`，就不开。
- 和受管会话、外部 MCP 的 coding 会话抢**同一把**写入互斥锁。
- Codex 发给 exec-server 的每条请求先过 ccnm 的规则表：读写路径和 MCP 工具同一套规则（只许工作区内，不写 `.git`，不写穿 symlink）；命令和写文件必须带 Codex 实测过的那种沙箱，**人在 Codex 里批准提权后发出的请求一律拒绝**——Codex 里表现为工具失败，比如 `exec-server rejected request (-32600): ccnm refused process/start: a sandbox is required`；网络请求一律拒绝。规则表的依据见 [P21 记录](research/p21-codex-native-surface-2026-09-16.md)。
- Runtime 是 Linux 时要装 bubblewrap，并允许执行账号创建 user namespace，否则 Codex 发来的沙箱起不来，命令不执行。

会话结束时，ccnm 要先确认 exec-server 起过的进程都不在了才放锁。确认不了——比如有进程被杀后还在，或者列不出进程表——锁就保持 `held`，下一个会话按"状态未知"拒绝，恢复步骤和其他入口一样，见[运维手册](operations.md)。

**Agent 离开太久，会话会被 Runtime 结束**（P26）：Runtime 连续 30 秒收不到 Codex 的任何字节就发一个探活请求，Codex 回一个错误就算还在；连续 10 分钟一个字节都没有，就当 Agent 已经不在，按上面的正常收尾放锁。笔记本合盖、断网超过 10 分钟再回来，Codex 的下一条命令会报 `exec-server transport disconnected`，`/exit` 重开即可。这两个时间不能配置；为什么这样选、以及锁没释放时怎么办，见[运维手册](operations.md#agent-静默离网之后exec-server-链的锁一直-held)。

### `exec_sandbox`

```toml
exec_sandbox = "codex"       # 默认 off
```

**把这个 workspace 的每条 `exec_command` 包进 Codex 自带的 workspace-write 沙箱里跑**（P33）：命令只能写工作区根目录以内（`.git` 除外）、`$TMPDIR` 和 `/tmp`，不能连网；读不受限。Claude、Codex 的受管会话和外部 MCP 客户端（gld hub）的 coding 会话都生效，因为它们跑命令走的是同一个 `exec_command`。`--print`、`ccnm mcp bridge` 也一样。

**需要什么**：Runtime 节点写 [`codex_bin`](#node-的其他字段)（Codex 0.154.0，和 exec-server 链共用同一个版本 pin）；Linux 上装 bubblewrap 并允许执行账号创建 user namespace（[运维手册](operations.md#runtime-node-的前置条件与项目工具链)）。开了却给不了——没 `codex_bin`、版本不对、找不到状态目录——会话启动就失败（`CCNM_E_CONFIG` / `CCNM_E_VERSION`）；启动时 ccnm 还会用这个沙箱跑一条 `sh -c 'exit 0'` 探一下，起不来（Linux 没装 bubblewrap、建不了 user namespace，或者 ccnm 的状态目录在 `/tmp` 下——Codex 拒绝在临时目录里建它的辅助程序）也在启动时拒（`CCNM_E_DEPENDENCY`，带 Codex 自己的报错）。**任何一种都不会退回不带沙箱地跑。**

**代价**（本机 macOS 和 Linux 容器实测，[P33 记录](research/p33-exec-sandbox-2026-09-17.md)）：

- `git commit` 失败：`.git` 只读，和 `apply_patch` 一直以来的规则一样。`.git` 可写的话，命令能往 `.git/hooks` 放东西，人下次跑 git 时它在沙箱外执行。提交由人做，或者关掉开关。
- 依赖下不了：没有网络；就算放开网络，`~/.cargo`、`~/.npm` 这类放在 HOME 下的缓存也写不了。先在沙箱外 `cargo fetch` / `npm install` 一遍再开。
- 命令里跑不了 `ps`（macOS 的 Seatbelt 挡进程表，Linux 的 pid namespace 里没有 `/proc`）。
- 每条命令多约 40 ms；编译、测试的耗时没有差别。

**被挡住是什么样**：不是错误，是命令自己失败——退出码非 0，stderr 里是 `Operation not permitted`（macOS）或 `Read-only file system`（Linux），和命令本身写错了长得一样。ccnm 分不出来，Codex 自己也只能靠猜；所以每条结果末尾都带一行 `[sandboxed: …]`，模型看到 `Operation not permitted` 时知道那是沙箱，不会有"不带沙箱重试"的路——能让模型开的门不是门。

**权限对象是 Codex 0.154.0 发给它自己命令的那一份，一字不改**（P21 录下的 workspace-write 沙箱，`crates/ccnm-core/src/mcp/sandbox.rs` 有测试钉着）。不放宽也不收紧，别的形状都没量过。它和上面三个 `allow_*` 开关方向相反——那三个是放开，这个是收紧——所以不影响 doctor 的审计行，也不改变 `exec_gate` 的判断：执行账号本身仍然是上限，沙箱只是在它里面再画一圈。

### `agent_tools`

```toml
agent_tools = ["web_search", "mcp_servers"]              # 默认值，可省略
agent_tools = ["web_search", "mcp_servers", "web_fetch"] # 再允许抓网页
agent_tools = ["web_search"]                             # 不用 Agent 上装的 MCP server
agent_tools = []                                         # 全关，就是 P46 之前的样子
```

**远端受管会话能用 Agent 那边的哪些功能**（P46，`mcp_servers` 是 P50 加的）。读、改、搜项目和跑命令一律走 ccnm 的工具、在 Runtime 上执行；Claude Code / Codex 自带的文件、shell、notebook 和 skill 工具永远关着，这一行管不到它们。它管的是这些：

| 值 | 做什么 | Claude Code 里是 | Codex 里是 |
| --- | --- | --- | --- |
| `web_search`（默认开） | 搜网页 | `WebSearch` | `web_search = "cached"`：Codex 自己的默认值，用 OpenAI 的索引，不现抓网页 |
| `mcp_servers`（默认开） | 用 Agent 机器上装好的 MCP server | `mcp__ccnm_agent__call_mcp_tool`、`read_mcp_result` | 同名，在 `mcp__ccnm_agent` 下 |
| `web_fetch` | 抓任意 URL 的内容 | `WebFetch` | 没有对应工具，不起作用 |
| `subagents` | 派子代理分头干活 | `Agent`、`TaskStop` | 不起作用，子代理仍关 |
| `tasks` | 模型自己的待办清单 | `TaskCreate`、`TaskGet`、`TaskList`、`TaskUpdate` | 不起作用 |

**`mcp_servers` 默认开，和 `web_fetch` 默认关的理由是冲突的，这一点要知道**：Agent 上装的远端 server 收得到模型发给它的任何东西，exa 还带一个抓网页的工具——等于换了个名字的 `web_fetch`。默认开是用户 2026-09-22 的决定（"默认全开"）。项目不能接受的话，这一行去掉 `mcp_servers`。开了之后给哪些 server、本机进程类的给不给，是 Agent 那台机器自己的 [`[agent_mcp]`](#agent_mcp) 定的。

**`web_fetch` 为什么默认关**：它是往外的通道。模型读过的项目内容能拼进 URL 发给任意网站，而让模型这么做只需要一段提示注入——藏在项目某个文件里、或者某条搜索结果里。`web_search` 只把搜索词发给 Anthropic / OpenAI 自己的搜索服务，面窄得多。开之前想清楚这个项目能不能接受。

**子代理不是绕过去的路**：实测（Claude Code 2.1.278）子代理拿到的工具和主会话一模一样，同样没有 Read、Bash。它的代价是额度：每个子代理是一段独立的上下文。

**写在 Runtime 上，Agent 那边改不了**：和上面几个 `allow_*` 一样，担风险的是项目所在的机器，所以由它说了算。

**会是什么样**：

- 不写这一行等于开 `web_search` 和 `mcp_servers`；`[]` 全关；写了不认识的名字（比如 `todo`）整份配置读不进来，报错里列出能写的值；同一个名字写两遍算一个。
- P50 之前写了 `agent_tools = ["web_search"]` 的 workspace，意思不变：只有搜索，没有 Agent 上的 MCP server。
- 不认识这个字段的旧 Agent 连新 Runtime：默认配置照常起会话，只是没有搜索（旧版本本来就全关）；写了别的值，旧 Agent 会拒绝这个请求、报 `unknown field agent_tools`，升级 Agent 就好。认识这个字段、但不认识 `mcp_servers` 的 P46–P49 Agent：默认配置照常起会话（没有 Agent 上的 MCP server，它本来就没有）；明写了 `mcp_servers`，它会拒绝这个请求、报 `unknown variant mcp_servers`。
- print 模式（`ccnm run --print`、Machine API）没人能点"允许"，所以开了的工具会写进这个会话的权限允许表。实测不写的话，Claude Code 会自动拒绝 `WebSearch` 和 `WebFetch`。
- Codex 不写 `model`（用 CLI 默认模型）时，`web_search` 开了也看不到效果：实测 0.154.0 和 0.155.1 在默认模型下三种取值发出的请求一字不差；指定 `gpt-5.1-codex` 这类模型才会带上搜索工具。
- `WebFetch` 真正取网页之前，Claude Code 会先去 claude.ai 查这个域名安不安全。Agent 机器连不上 claude.ai 的话，每次都报 `Unable to verify if domain … is safe to fetch`。
- 只管远端受管会话。外部 MCP 客户端（比如 gld）自带自己的工具，和这一行无关。

实测依据和为什么这样映射：[P46 记录](research/p46-agent-tools-2026-09-22.md)。

### `external_instructions`

外部客户端在 MCP 握手里拿到什么项目说明。**只影响上下文，不影响权限**：

```toml
external_instructions = "generic"   # generic（默认）| project | none
```

- `generic`：只有 ccnm 自己那段（这是哪个 workspace、路径都是相对的、这次能不能写）。
- `project`：再加上项目自己的说明文件。Runtime 按固定顺序找 `AGENTS.md` → `CLAUDE.md`，取第一个存在的——外部客户端的 provider 无从得知，也不能靠它自称的名字去猜。整段握手按 Claude Code 的上限 2048 个 UTF-16 码元投影，ccnm 自己的说明和模式句约占 850，项目文件大约能放 1200 个字符（中英文都按字符数算），放不下的部分按行截掉，模型会被告知用 `read_file` 读全文。
- `none`：什么都不给。

## `[ui]`

这台机器怎么跟坐在它前面的人说话。纯本地、纯显示，底下一个字都不会跨到对面那台去。

```toml
[ui]
lang = "zh"   # zh（默认）| en
```

命令行 `--lang` 优先于环境变量 `CCNM_LANG`，`CCNM_LANG` 优先于这里。写了别的值（比如 `de`）会直接报错，不会悄悄退回英文——你要的东西不在，得说出来。

管的只有给人看的输出。错误码、协议字段、给模型的 MCP 文本都不跟着变，细节见[使用说明](usage.md#说什么语言)。

有一处它管不到：`--help` 不读这个文件。要读它就得先解析 `--config`、加载文件，而这一切发生在命令行还没校验之前——配置写坏了会连 `--help` 一起废掉，而 `--help` 恰恰是东西坏了的时候要跑的那条。所以 `--help` 只认 `--lang` 和 `CCNM_LANG`，其余情况用默认的中文。

## `[machine_skills]`

```toml
[machine_skills]
enabled = true            # 默认值，可省略
hidden = ["pdf", "pptx"]  # 这几个不交给会话，按会话里看到的名字
```

**这台机器的账号装好的 skills 交不交给会话**（P48）。"装好的"指 `~/.claude/skills`、`~/.agents/skills`、`~/.codex/skills`、`~/.claude/commands` 里的；项目里的（`.claude/skills` 那些）一直都给，这一节管不到。**默认全开**，是用户 2026-09-22 的决定。

两台机器各写各的、各管各的，和 `[ui]` 一样不跨机器：

| 写在哪 | 管的是 | 模型那边是什么样 |
| --- | --- | --- |
| Runtime Node | 执行账号（通常是 `ccrun`）HOME 里装的 | 并进 `load_skill`，排在项目的后面。附件用 `load_skill` 的 `file` 读；脚本就在项目这台机器上，`exec_command` 按路径直接跑 |
| Agent Node | 你自己账号 HOME 里装的 | 远端会话启动时，Claude Code / Codex 旁边多起一个小服务 `ccnm internal agent-skills`，工具叫 `mcp__ccnm_agent__load_skill`。附件同样用 `file` 读；脚本在 Agent 机器上，模型得先用 `apply_patch` 写进项目才能在 Runtime 上跑 |

Agent 这边为什么不直接用 Claude Code / Codex 自己的 skills：实测（toexec `evidence/v3-parity/machine-skills/`）Claude 读附件要放开 `Read`，而只对 skills 目录放开的 `Read` 照样能读到会话的工作目录（ccnm 给这个会话记的状态）；Codex 的清单要靠 shell 去读，ccnm 把 shell 关了，列出来也读不到。所以原生的一律关着——Codex 远端会话固定带 `-c skills.include_instructions=false`，不管这一节怎么写。

**会是什么样**：

- 同名时装好的赢，项目的被盖掉（和 Claude Code 原生一样）。被盖掉的出现在 `load_skill` 不带名字时返回的 `Not offered` 里，写明被哪个文件盖掉；把装好的那个名字写进 `hidden`，项目的就回来了。
- `hidden` 里的和没装一样：目录、全表、点名都没有，`Not offered` 里也不提。只管装好的，写项目 skill 的名字不起作用。
- 同一个 skill 经符号链接出现两次（`skills` CLI 就这么装：`~/.claude/skills/x` 链到 `~/.agents/skills/x`）只算一个。
- 读附件只在这个 skill 自己的目录里：点开头的文件（`.env` 这类）、链到目录外面的一律拒绝，报 `CCNM_E_POLICY`；不是 UTF-8 的报 `CCNM_E_INVALID_ARGS`（Runtime 上的会附上路径，让模型用 `exec_command` 就地用）。
- 一次最多回 64 KiB，长文件分段，每段末尾写明下一段从第几行开始。
- 目录挤不下时：Claude Code 只保留每个工具说明的前 2048 个字符，先保项目 skill 的描述，装好的依次退成只有名字、最后只剩个数；不带名字调 `load_skill` 总能拿到全表。一共最多 100 个，超了先丢装好的。
- 关掉（`enabled = false`）：Runtime 上关，`load_skill` 回到只有项目的；Agent 上关，只移除它的 skills 部分，P50 的 Agent MCP 仍开启且有可用 server 时，`ccnm_agent` 服务仍会启动。都从下一个会话开始算，开着的会话不变。
- 这台 Runtime 的执行账号是专门建的 `ccrun` 时，它的 HOME 里一般什么都没装，这一节在 Runtime 上等于没有效果；要给它装，就装到 `~ccrun/.claude/skills` 这类目录里。
- 旧版本的 ccnm 不认识这一节，读到它会整份配置报错（配置对未知字段是严格的），先升级再写。

## `[runtime_mcp]`

```toml
[runtime_mcp]
enabled = true         # 默认值，可省略
project = true         # 默认值：读项目根下的 .mcp.json
hidden = ["computer"]  # 这几个不转，按名字
```

**Runtime 上的 MCP server 转不转给会话**（P49）。只写在 Runtime Node 上、只管这台机器：会话里多一个工具 `call_mcp_tool`，经它用这台机器上的 server——项目 `.mcp.json` 里声明的，和执行账号给 Claude Code（`~/.claude.json`）或 Codex（`~/.codex/config.toml`）装的。**默认全开**，是用户 2026-09-22 的决定。

**会是什么样**：

- 只有能写的会话有这个工具（Managed 会话、`external_mcp = "coding"` 的外部连接），而且那台机器上至少有一个能转的 server；read 模式永远没有。
- 起一个 server 就是以执行账号跑一个程序，所以它过的门和 `exec_command` 一样：执行身份没隔离又没写 `allow_unconfined_exec` 时拒绝（报 `CCNM_E_POLICY`，写明理由）；配了 `exec_sandbox` 就套同一个沙箱（server 没有网络，context7 这类要联网的会失败）；有人值守的 Claude 会话每次调用都问人。只列清单（不带 `server`）不起任何东西，不过这些门。
- 项目的 `.mcp.json` 排最前、同名压过装好的（Claude Code 的规矩）。只转 stdio 的；HTTP 的列出来、写明"从 Agent 那边连"（Agent 上装的见 [`[agent_mcp]`](#agent_mcp)）。配置里自己关掉的（Codex 的 `enabled = false`、JSON 里的 `"disabled": true`）列出来、写明关着，不起。
- server 配置里自己的 `env` 照传，token 也传；Agent 的登录变量（`ANTHROPIC_API_KEY` 这些）不传。`${VAR}` 查不到像凭据的变量名——ccnm 的执行门本来就不许 Runtime 的环境里有它们——这样的 server 标成"缺什么"，不起。
- 结果文字超过 32 KiB 的，先交前 32 KiB，其余像命令输出一样用 `read_output` 接着读。
- 正常收尾先关闭 server、再放写锁；闲 5 分钟也会回收连接，下次调用重起。**当前存在已复现缺陷：server leader 正常退出、子进程仍写文件时，写锁仍可能被释放**，因此这不是“整棵进程树已停干净”的保证；详见[审计 C51-01](research/2026-09-23-lifecycle-and-docs-audit.md)。
- `project = false`：只转执行账号装的，不读项目的 `.mcp.json`——给托管别人项目、不想让项目文件点名要跑什么程序的机器。`hidden` 里的在哪声明都不转。
- 执行账号是专门建的 `ccrun` 时，它的 HOME 里一般什么都没装，转的就只有项目自己声明的。
- 改了从下一个会话开始算；开着的会话的工具说明（列了哪些 server）不变，但每次调用都重新读配置。
- 旧版本的 ccnm 不认识这一节，读到它会整份配置报错，先升级再写。

## `[agent_mcp]`

```toml
[agent_mcp]
enabled = true                   # 默认值，可省略
local = ["context7", "mcp-time"] # 本机进程类的 server，按名字给（默认一个不给）
hidden = ["exa-search"]          # 这几个不给，哪一类都一样
```

**Agent 机器上装好的 MCP server 给不给受管会话、给哪些**（P50）。只写在 Agent Node 上、只管这台机器。给了的话，会话里 `ccnm_agent` 那个小服务（P48 起它就交 Agent 上的 skills）多两个工具：`call_mcp_tool`（和项目那台机器上的同名、同样的用法：不带参数列 server，带 `server` 列它的工具，再带 `tool` 和 `arguments` 调用）和 `read_mcp_result`（读一次交不完的结果的后面部分）。读的是你给 Claude Code（`~/.claude.json` 顶层的 `mcpServers`）和 Codex（`~/.codex/config.toml`，或 `$CODEX_HOME` 下的）装的，同名时 Claude 的那份生效。

**分两类，默认不一样**：

| 哪类 | 例子（开发机上） | 默认 |
| --- | --- | --- |
| 别的机器上的地址（HTTP，不是 `127.0.0.1` / `localhost`） | exa、DeepWiki | **给**，除非写进 `hidden` |
| 这台机器上跑的：`command` 起的程序，或本机地址的 HTTP 服务 | context7（`npx` 起的）、Filesystem、desktop-commander、playwright、pencil | **不给**，写进 `local` 才给 |

为什么本机的要点名：它们以你的账号在 Agent 上跑，能读写这台机器的磁盘、跑命令、用你的 ssh 连到 Runtime——而受管会话特意关掉了 Claude / Codex 自带的文件和 shell 工具，就是为了"项目只能经 ccnm 碰到"。哪个 server 只联网、哪个碰磁盘，从配置上看不出来（context7 和 Filesystem 都是 `npx` 起的），所以不猜，由你点名。

**会是什么样**：

- 要 workspace 也同意：Runtime 上的 `agent_tools` 里有 `mcp_servers`（默认有）。它和这一节都开着，远端会话才有这两个工具；这台机器上一个能给的 server 都没有时，工具也不出现。
- 不带参数调 `call_mcp_tool`，每个 server 一行：能用的写"not started"或它的工具，不给的写明原因（本机的没点名、配置里关掉了、缺环境变量、老的 HTTP+SSE 传输）。
- 结果文字超过 32 KiB 的，先交前 32 KiB（尽量断在换行后面），末尾写明 `read_mcp_result ref=… offset=…`；留在内存里 30 分钟，单条最多 16 MiB。实测 52 000 字节在 Claude Code、Codex 里都一个字节不少地读回来了。
- HTTP 的经这台机器的 `curl` 连（没装 `curl` 的话报错说明）。地址和请求头（exa 的 key 就在地址里）写在只有你能读的临时文件里交给 `curl`，不出现在命令行上。要 OAuth 登录的 server 报"需要登录"：令牌在 Claude Code 那里，ccnm 拿不到。
- 本机程序类的 server 拿到的环境：起 `ccnm_agent` 的客户端给它什么，它就拿什么，去掉 Agent 的登录变量（`ANTHROPIC_API_KEY`、`CODEX_HOME` 这些）和 `SSH_AUTH_SOCK`，再加配置里 `env` 写的。`GITHUB_TOKEN` 这类不去掉——你直接用 Claude Code 时它们也拿得到。
- **配置里的 `${VAR}` 在受管会话里多半查不到你在 shell 里 export 的变量**：会话里的 Claude / Codex 是 Agent 上的 Controller（launchd 起的）经 tmux 带起来的，环境里只有 `HOME`、`PATH`、`SHELL`、`TMPDIR`、`USER` 这类，`~/.zshrc` 里 export 的不在（2026-09-23 在 fodelf 上看的）。Codex 更少：它交给 MCP server 的只有 `HOME`、`PATH`、`LC_CTYPE`、`__CF_USER_TEXT_ENCODING`（0.154.0、0.155.1 实测）。查不到的，清单里那个 server 写 `its config uses GITHUB_TOKEN, which this session's server does not have`，点名调用报 `CCNM_E_CONFIG`，不起。要在受管会话里用它，把值直接写进配置的 `env` 或地址；`${VAR:-默认值}` 查不到时用默认值，不会报缺。
- 正常路径在会话结束或闲置 5 分钟时回收 server，下次调用重起；Host 强杀服务及派生后代的清理不能据此保证。Agent 和 Runtime relay 复用了关闭回调，Runtime 已复现的后代残留问题在 Agent 侧仍须专项复验。
- `enabled = false`：这台机器的一个都不给。改了从下一个会话开始算。
- 同一个名字两台机器上都有（比如都装了 context7）：两个都能用，模型看到的说明是"项目那台机器上的那个在项目旁边"。
- 旧版本的 ccnm 不认识这一节，读到它会整份配置报错，先升级再写。

为什么经 ccnm 转、而不是把这些 server 直接写进 Claude / Codex 的配置：实测直接写进去的话，超过约 50 000 字符的结果 Claude Code 会存到本机磁盘、只给模型 2 KB 预览要它用 `Read` 去读（受管会话没有 `Read`），Codex 只留 12 KB；每个 server 的全部工具也会进每一次请求（playwright 一家 21 KB）。实测和取舍见 [P50 记录](research/p50-agent-mcp-2026-09-22.md)。

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
