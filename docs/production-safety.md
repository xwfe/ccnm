# Runtime 安全与 `ccrun`

`exec_command` 本质上是在 **Runtime Node** 上，以某个真实操作系统账号执行命令。这个账号才是 ccnm 当前最重要的权限边界。

`ccrun` 是建议使用的 **Runtime Service Account**：一个只用于 ccnm Runtime 执行的低权限 Unix 账号。

它不绑定“家庭机”这种物理位置，也不只是为了隐藏某几个目录。它真正解决的是：**AI 的一次工具调用，不应该自动继承你个人登录账号能做的一切。**

## 四种身份，别混成一个

一套 ccnm 里有四个操作系统身份。它们可能分布在两台机器上，也可能有几个在同一台，但**不能是同一个账号**：

| 身份 | 干什么 | 可以持有 | 不该持有 |
| --- | --- | --- | --- |
| **Operator**（控制身份） | 你本人。敲 `ccnm run/status/stop`、跑 `ccnm rpc` | 连到 Agent Node 的 SSH 私钥、你自己的配置 | 这些不会自动传给 Runtime 工具 |
| **Agent Identity** | Agent Node 上跑 Controller 和 Claude/Codex 的账号 | 官方 CLI 的登录/订阅、连到 Runtime Executor 的 SSH 私钥 | 项目的私密凭据（除非项目确实需要） |
| **Runtime Executor**（`ccrun`） | Runtime Node 上跑 `internal mcp-serve` 和全部项目工具 | `authorized_keys` 这类**入站**公开状态、项目最小必要凭据 | Agent 登录、你的个人凭据、**任何 ccnm 正常运行所需的出站 SSH 私钥或 SSH agent**、sudo/admin/特权 socket |
| **Administrator** | 建账号、配 ACL、改网络策略 | 主机管理权限 | 不参与日常 Agent session |

一句话硬约束：

> **以 Runtime Executor 身份运行、会碰到 Agent 输入或项目数据的 ccnm 进程，不能为了完成 ccnm 自己的控制链而主动 SSH 到别处。** `ccrun` 只接受入站连接。

**为什么要分。** Runtime Executor 是唯一会执行模型产出内容的身份。它多一把出站私钥，就等于把"Agent 让我跑一条命令"变成"Agent 可以以我的名义连到别的机器"。而 Operator 手上有出站钥匙是正常的——因为 Operator 不执行模型的命令，它只发号施令。

**代码里怎么落实的**（P7.3 真机量出问题，P7.4 四批改完）：

- 控制链不再要求 Runtime Executor 出站（Batch C）：Runtime 侧发起时是 **Operator** 的进程拨号去 Agent Node，Agent 侧发起时只把一句只读的 `internal runtime-resolve` 问过去、会话在 Agent 本机创建。
- `ccnm doctor` 关于 Runtime 的那几行不再判"敲命令的人"（Batch D）：由 Runtime Executor 自己回答，经 Agent 那条 ssh 取回。**换个人跑同一个 workspace，这几行一字不差**，有测试钉着。
- 诊断入口也一样（Batch D2）：在 Agent Node 上跑 `ccnm doctor` / `ccnm mcp probe`，过去是把整条公共命令 ssh 给 Runtime 执行、再由它连回 Agent 探测——执行身份为了一个诊断出站了一次。现在两端各查各能证明的：Agent 本机查 Controller/官方 CLI/登录/tmux，Runtime 的结论由 `ccrun` 自己回答（`runtime-resolve` / `runtime-audit`），MCP transport 由 Agent 主动开。**两个方向跑出来的 Runtime 结论一字不差**，同样有测试钉着。

所以现在的做法就是直白的那个：**用你自己的账号（Operator）敲 ccnm，让 `ccrun` 名下一把私钥都没有。** 诊断命令两台机器上都能跑。

**这些在真机上复验过了**（[Batch E 记录](research/p7-batch-e-2026-09-10.md)）：以普通管理员账号跑 doctor，Runtime 那几行报的是 `ccrun` 且全绿——同一条命令在 P7.4 之前会报 7 个 FAIL；两个方向跑 doctor 结论逐字相同；会话期间执行身份的进程表里只有入站 sshd 与 `mcp-serve`。仍未验的是 Codex 那条链。

## `ccrun` 能解决什么

如果直接让 Runtime 以个人账号运行，`exec_command` 理论上可以继承这个账号能访问的所有资源，例如：

- 个人 SSH 私钥和 SSH agent；
- Git / 云服务凭证；
- 其他仓库和 home 目录数据；
- admin 或 passwordless sudo 能力；
- 浏览器 profile 和本地应用数据；
- Docker socket；
- 该账号可读写的其他本地资源。

换成专用 `ccrun` 后，可以把影响范围缩小到你明确授予它的 workspace 和 toolchain。

所以它解决的是：

```text
host/account blast radius
credential inheritance
filesystem authority
privilege inheritance
```

而不只是“防止 AI 读到别的文件”。

## `ccrun` 不能解决什么

**Unix 账号不是完整 sandbox。**

如果 `ccrun` 同时：

```text
能读项目源码
+
可以自由访问公网
```

那么某条命令仍然可以把源码上传出去。

如果它可以写 Docker socket、调用特权本地服务、使用错误配置的 sudoers，也可能突破预期边界。

所以更强的隔离还需要根据威胁模型叠加：

- OS 网络策略；
- 独立 VM；
- 容器隔离；
- 更严格的文件系统挂载；
- egress policy。

ccnm 不会假装“禁止 `curl` / `wget` / 某几个程序名”就等于 sandbox，因为 shell、解释器、绝对路径、wrapper 和自定义二进制都可以绕过这种黑名单。

## ccnm 当前会检查什么

在 Runtime Node 执行：

```bash
ccnm doctor <workspace>
```

**先说清楚这几行在审谁**：审的是 Runtime Executor——`ccnm doctor` 把这个问题经 Agent 的那条 ssh 交给它自己回答（`internal runtime-audit`），拿回来的是结构化结论，不含路径、环境值或凭据内容。你用哪个账号敲 doctor 不改变答案。

Agent 不通的时候这几行是 SKIP，不是 OK：问不到的 Runtime 必须读作"没查"，不能读作"没问题"。

confinement gate 会检查 Runtime Executor 能在它本机可靠判断的性质：

```text
Runs as root            Runtime 不能是 root
Runtime user            被审计的账号必须匹配 nodes.<runtime>.runtime_user
No sudo                 不能 passwordless sudo
Not an admin            不应属于 admin / wheel / sudo 等管理组
No SSH keys             ~/.ssh 和 ~/.config/ccnm 里都不该有该账号可读的私钥
Workspace root          项目目录对该账号是可用的（存在、是目录、git 不因属主拒绝）
No Claude credential    Runtime identity 不应持有 Claude 凭证
No Codex credential     不论当前选谁，都检查 Codex 默认、专用及本地引用目录
No authentication environment  不接受未授权的认证环境（只检查名称，不打印值）
No Docker socket        当前账号不应能写 Docker socket
exec_command            confinement 通过后才正常允许
```

`allow_unconfined_exec = true` 是逃生开关，不是生产配置。

它的含义只是：

> 我知道当前 Runtime 没有隔离，但这个测试 workspace 暂时允许执行命令。

它不会让 Runtime 变安全，而且命令结果会明确标记为 unconfined。

**这个开关不跳过身份未知、认证环境继承和 Agent 凭据隔离**——最后那一条要另一个开关，见下一节。同一物理机器可以有 Agent 和 Runtime 两种角色，但隔离 Runtime 的执行身份不能读取已知 Agent 认证文件/容器。失败会在 MCP 初始化、Git 探测前拒绝；exec 前再次检查。目录/ACL/symlink 不明不能报成“没有凭据”。SSH 检查不读取私钥内容，保留 `authorized_keys` 等已知公开文件，其余可疑候选保守拒绝。具体范围、环境来源及未证明的 OS credential service/其他目录见 [Provider 安全契约](provider-safety.md)。

## 凭据隔离那一条，怎么放开，代价是什么

有一类人确实卡在这里：项目和 Claude 的登录在同一个家目录里——一台机器、一个账号、想先试试这东西。对他们来说没有东西可隔离，而 ccnm 直接拒绝启动，等于没法用。

所以有第二个开关，写在 **Runtime 那一侧**那个 workspace 上：

```toml
[workspaces.demo]
root = "/Users/me/code/demo"
allow_unconfined_exec = true            # 这个账号没被约束
allow_unisolated_credentials = true   # 这个账号能读到 Agent 的登录
```

**两个都要写，而且互不蕴含。** 它们是两件不同的事：前一句说"跑命令的账号 OS 权限比它该有的大"，后一句说"一句 prompt 就能把我的登录读出去"。后面这一件正是这个项目存在的理由，所以它绝不会被前一句顺带打开。

**你接受的到底是什么，说清楚：** 跑这个 workspace 命令的那个账号能读到那台机器上已知的 Agent 登录（`~/.claude`、`~/.codex` 之类）。模型跑的每一条命令也能读到——**而让它跑一条命令只需要一句 prompt**，包括从它被要求读的文件里冒出来的那一句。装个依赖、看个 issue、读份 README，都算。

这不是"风险提高了一点"，是这个程序唯一那条硬边界没了。**其他任何东西都没在挡着。**

放开之后 ccnm 做三件事，一件不少：

1. **开着的时候说一次。** 第一次用它启动会话时，终端上打一段话，讲清上面这些。只说一次——每条命令都喊的警告没人看。关掉再打开，算一次新的决定，会再说一次。
2. **`ccnm doctor` 一直显示。** 那几行从 FAIL 变成 **WARN**，并注明是这个 workspace 自己接受的。**不会变成 OK**：那个性质并没有成立，只是有人说他能接受。
3. **每条命令的结果都带着。** 会话产物里那行 unconfined 说明会同时写明凭据这一条。

仍然不给放开的两条，任何开关都不行：

- **执行身份未知**（identity 探针答不出来）——没人能说清是谁接受了什么；
- **认证环境是继承来的**（`ANTHROPIC_*`、`CLAUDE_*` 之类在 Runtime 服务环境里）——那是把凭证直接塞进每一个子进程，比放在磁盘上等人去找严重一个量级，而且它的修法只是别 export。

还有一条要知道：`No <Agent> credential` 里那种"**目录是 symlink / 列不出来，可达性未知**"的结论，也在这个开关的覆盖范围内。也就是说你接受的包括"说不清"。doctor 行里原话照旧，不会被改写成"没有凭据"。

**真要长期用，还是去建一个专用账号。** 下面两节就是。这个开关是给"我知道我在做什么，我现在就想跑起来"的场景用的。

## macOS 创建 Runtime Service Account

下面这些命令会修改主机安全模型，所以 ccnm **不会自动执行**。应由你自己在 Runtime Node 上完成。

先找一个未使用 UID：

```bash
dscl . -list /Users UniqueID | awk '{print $2}' | sort -n | tail -1
```

然后用空闲 UID 创建 `ccrun`。下面仅以 `502` 为例，实际必须确认没有占用：

```bash
sudo dscl . -create /Users/ccrun
sudo dscl . -create /Users/ccrun UserShell /bin/zsh
sudo dscl . -create /Users/ccrun RealName "ccnm runtime"
sudo dscl . -create /Users/ccrun UniqueID 502
sudo dscl . -create /Users/ccrun PrimaryGroupID 20
sudo dscl . -create /Users/ccrun NFSHomeDirectory /Users/ccrun
sudo mkdir -p /Users/ccrun
sudo chown -R ccrun:staff /Users/ccrun
sudo chmod 700 /Users/ccrun
```

不要把它加入 `admin`：

```bash
dscl . -read /Groups/admin GroupMembership
```

## Linux 创建 Runtime Service Account

用发行版自己的命令，原则和上面一样：**独立 UID、独立主组、不进 sudo/wheel/admin/adm/docker/staff、home 0700**。P12 在 Debian 13 上实际用的是一个可重跑、可撤销的脚本：[scripts/p12-provision-linux-runtime.sh](../scripts/p12-provision-linux-runtime.sh)（要 root；清单先于变更写入，`--revert` 只按清单撤销，不替既有环境做清理）。工具链装在这个身份自己的 home 里，见[运维手册](operations.md#runtime-node-的前置条件与项目工具链)。

**`docker` 组要特别留意**：进了它等于 root，因为能挂载宿主任意路径进容器。检查一眼：

```bash
id ccrun            # groups 里只应该有它自己的组
test -w /var/run/docker.sock && echo "可写 —— 这个身份等于 root"
```

## SSH：只放公钥，不放私钥

Agent Node 需要以 `ccrun` 身份进入 Runtime Node。

在 Runtime Node：

```bash
sudo mkdir -p /Users/ccrun/.ssh
sudo chmod 700 /Users/ccrun/.ssh
sudo tee /Users/ccrun/.ssh/authorized_keys < /path/to/agent-node.pub
sudo chown -R ccrun:staff /Users/ccrun/.ssh
sudo chmod 600 /Users/ccrun/.ssh/authorized_keys
```

这里只应该放 Agent Node 的**公钥**。

`ccrun` 是**入站专用**（inbound-only）：别人连进来，它不连出去。所以它名下不该有任何私钥，也不要把个人 SSH agent 转发给它（`SSH_AUTH_SOCK` 不该在它的环境里）。

### 换个目录藏私钥不算数

以前 `No SSH keys` 只看 `~/.ssh`：把同一把私钥挪到 `~/.config/ccnm/transport/`，这一行就从 FAIL 变成 OK，而账号该能连出去还是能连出去。P7.3 真机上正是这么达标的，那份绿灯不能当隔离证据。

**现在两个目录都查**（`~/.ssh` 与 ccnm 自己的 `~/.config/ccnm`，含子目录），所以这条路走不通了。同时链路那一半也改完了（Batch C）：Runtime 侧发起时拨号的是 Operator 的进程，Agent 侧发起时只把一句只读的问题问过来。**`ccrun` 一把私钥都不需要，把它清空是现在就能做到的目标。**

这一行仍然只说它查过的地方：这两个目录之外没有搜。真正的隔离靠独立账号和 OS 权限，不靠这条启发式。另外一种没有文件的出站凭据是继承来的 `SSH_AUTH_SOCK`，它由"认证环境"那一行按名字拒绝，不在这条里重复。

在 Agent Node 的 `~/.ssh/config` 中，让 `nodes.runtime.ssh` 对应的 alias 使用 `ccrun`：

```sshconfig
Host runtime-ssh-alias
    HostName <runtime-node-address>
    User ccrun
    IdentityFile ~/.ssh/id_ed25519
```

ccnm 只消费这个 alias，不接管 SSH 身份或网络层。

## 只授权目标 workspace

`ccrun` 需要的是目标项目，不是你的整个 home 目录。

macOS 可以用 ACL 精确授权。

例如：

```bash
PROJ=/Users/you/code/project

# 父目录只允许穿过，不需要授予目录内容读取权限。
chmod +a "user:ccrun allow execute" /Users/you /Users/you/code

# 项目目录授予需要的访问能力，并让 ACL 向下继承。
chmod -R +a "user:ccrun allow list,search,add_file,add_subdirectory,delete_child,readattr,writeattr,readextattr,writeextattr,readsecurity,file_inherit,directory_inherit" "$PROJ"
```

实际权限应以项目需求为准，不要机械复制一份比项目需要更大的 ACL。

然后直接验证边界：

```bash
sudo -u ccrun ls "$PROJ"
sudo -u ccrun cat /Users/you/.ssh/id_ed25519
```

第一条应该成功，第二条必须失败。

还要用**同一个身份**验证项目 toolchain，否则“能读代码但跑不了测试”的 Runtime 也没有实际价值：

```bash
sudo -u ccrun git -C "$PROJ" status
# 再执行该项目日常真正使用的 build / test 命令
```

## 给 `ccrun` 安装 ccnm

Agent Node 反向 SSH 到 Runtime Node 后，需要能执行 Runtime 侧 ccnm。

一种安装方式：

```bash
sudo -u ccrun mkdir -p /Users/ccrun/.local/bin
sudo cp target/release/ccnm /Users/ccrun/.local/bin/ccnm.new
sudo chown ccrun:staff /Users/ccrun/.local/bin/ccnm.new
sudo -u ccrun mv /Users/ccrun/.local/bin/ccnm.new /Users/ccrun/.local/bin/ccnm
```

仍然使用 `.new` + `mv`，不要直接覆盖正在运行过的二进制 inode。

Runtime 依赖，例如 `ripgrep`，也必须安装在 `ccrun` 能执行到的位置。

## 配置 Runtime identity

Runtime Node 的 ccnm 配置：

```toml
[nodes.runtime]
runtime_user = "ccrun"
```

`runtime_user` 的含义只有一个：**Runtime Executor 应该是哪个账号**——也就是 Agent 的 SSH MCP transport 落到哪个账号上、项目工具最终以谁的身份跑。

它**不**规定谁可以敲 `ccnm`。Operator 用自己的账号跑 CLI 是正常的，doctor 也不会因此报红——关于 Runtime 的那几行是 Runtime Executor 自己回答的。

Agent Node 那份则是它自己怎么连过来：

```toml
[nodes.runtime]
ssh = "runtime-ssh-alias"
```

真实项目应移除 dogfood bypass：

```toml
[workspaces.my-project]
allow_unconfined_exec = false
```

然后重新：

```bash
ccnm doctor my-project
```

## 凭证边界

Runtime Service Account 不应该持有 AI Provider 凭证。

当前 Claude 架构明确要求：

```text
Claude login / OAuth -> Agent Node
workspace / toolchain -> Runtime Node
```

不要为了让某个测试绿，就在 `ccrun` 下执行：

```bash
claude auth login
```

这会直接破坏凭证边界。

如果项目必须访问私有 Git 仓库，应只为该项目提供最小必要凭证，而不是把个人 Keychain、整个 SSH agent 或全部 Git 身份共享给 `ccrun`。

## sudo 与其他提权面

确认：

```bash
sudo -u ccrun sudo -n true
```

它应该失败。

但这还不是全部。还应检查：

- 自定义 sudoers 规则；
- setuid helper；
- 可写 Docker socket；
- SSH agent forwarding；
- 有特权能力的本地服务；
- 项目自己的管理工具；
- Runtime 账号能调用的其他提权入口。

ccnm 的 doctor 能覆盖一部分明确可验证项，但不能证明整个操作系统不存在其他提权路径。

## 网络出口

**先把边界划清楚，这是 v1 的正式声明：**

- **ccnm 不提供 egress isolation，也不声称提供。** 项目工具能不能连出去，由 OS、网络、防火墙、VM 或容器决定，不由 ccnm 决定。
- **ccnm 保证的是它自己：控制链不要求 Runtime Executor 持有任何出站凭据。** 没有出站 SSH 私钥，没有 SSH agent，正常路径上不发起任何出站连接——这一条有真机证据（会话活着时 `ccrun` 名下只有入站 `sshd-session` 和它的 `mcp-serve` 子进程，没有任何 ssh 客户端）。

两句话不能合并成第三句。"ccnm 不要求它出站"是 ccnm 的属性；"它出不去"是网络的属性，ccnm 说了不算。

网络隔离是单独一层策略。

如果你的安全要求是：

> Runtime 项目代码永远不能访问 Anthropic，甚至不能访问公网。

那么必须在 Runtime Node / `ccrun` 周围通过 OS、网络、防火墙、VM 或容器环境真正执行这个策略。

**"名下没有私钥"不等于"连不出去"，这一条是真机上撞出来的。** Batch E 把 `ccrun` 的出站私钥删干净之后再试，它照样连得到 Agent Node：

```text
debug1: no identity pubkey loaded from ~/.config/ccnm/transport/agent-key
debug1: remote software version Tailscale
Authenticated to <agent> using "none".
```

答话的是 Tailscale SSH，按 tailnet 身份授权，根本不看密钥——这台机器上任何本地账号都到得了。ccnm 能保证的只是"ccnm 自己不要求这个身份出站，也不给它钥匙"；**能不能出站是网络策略的事**，要关就在 tailnet ACL、防火墙或 OS 层关。把 `No SSH keys` 那一行读成"这个账号出不去"，就会把一个没关的门当成关上的。

不要用下面这种方式代替：

```text
禁止 curl
禁止 wget
禁止 python
```

因为这不是可靠安全边界。

### 部署环境本身的高权限风险，也不在 ccnm 的保证里

同一轮真机验证里还撞到一条，跟上面是同一类事：**本轮那台 Agent Node 允许无传统凭据的 root 登录**——`ssh root@<host>` 由 Tailscale SSH 按 tailnet 身份放行，不要密码也不要密钥。

这不是 ccnm 造成的，ccnm 也管不了；但它足以让上面所有身份隔离失去意义——能拿到 root 的人不需要绕过 `ccrun` 的权限，直接就是。**它是部署环境的风险，不计入 ccnm 的安全保证。**

生产部署要自己关掉：在 tailnet ACL 里去掉 root 这个 SSH 用户，或在 OS 层禁掉 root 登录。上线前把这一条当作检查项，跟 `ccnm doctor` 的输出无关——doctor 查不到它。

## 最终门禁

完成 Runtime identity、SSH alias 和 ACL 后：

```bash
ccnm doctor <workspace>
```

**用你自己的账号跑就行，两台机器上都可以。** 关于 Runtime 的行由 Runtime Executor 自己回答，跟你是谁、在哪台敲都无关。

目标是这些行全部成为 OK：

```text
Runtime user
No sudo
Not an admin
No SSH keys
No Claude credential
No Docker socket
Workspace root
exec_command
```

这几行证明的是：**Agent 的 transport 落到的那个账号**没有 sudo/admin、在两个被查目录里没有私钥、够不到已知 Agent 凭据、写不了 Docker socket，而且项目目录它真的能用（存在、是目录、git 不因属主拒绝）。

它们**不**证明这个账号绝对连不出去：查的是两个目录，别的地方没搜。要那种程度的保证，得靠独立账号、OS 权限和网络策略。

达到这个状态后，再让有价值的真实项目脱离 `allow_unconfined_exec` 进入长期 dogfood。

如果还需要更强的数据防外传边界，再继续叠加 network policy / VM / container，而不是继续往 ccnm 命令解析器里堆假的安全规则。
