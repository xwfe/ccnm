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

**现在的代码还没完全做到。** 这是 P7.3 在真机上量出来的，写在这里免得你照着做了却发现对不上：

- 控制链已经不再要求 Runtime Executor 出站（Batch C）：Runtime 侧发起时是 **Operator** 的进程拨号去 Agent Node，Agent 侧发起时只把一句只读的 `internal runtime-resolve` 问过去、会话在 Agent 本机创建。`ccrun` 只接受入站连接。
- `ccnm doctor` 关于 Runtime 的那几行也不再判"敲命令的人"了（Batch D）：它们由 Runtime Executor 自己回答，经 Agent 那条 ssh 取回。**换个人跑同一个 workspace，这几行一字不差**，这条有测试钉着。

所以现在的做法就是直白的那个：**用你自己的账号（Operator）敲 ccnm，让 `ccrun` 名下一把私钥都没有。**

还剩一个已知缺口：**在 Agent Node 上跑 `ccnm doctor` 或 `ccnm mcp probe`**，当前实现仍把整条公共命令 ssh 给 Runtime 执行——落到的账号是 `ccrun`，而那条公共命令自己又要连回 Agent Node，于是执行身份再次出站。

处理方向已经确定，不采用“Agent 侧直接禁用诊断”：P7.4 Batch D2 要把两条命令改成 topology-aware 双端诊断。Agent 本机检查 Agent/Controller/session，并由 Agent Identity 直接 SSH 到 Runtime Executor 获取 `runtime-resolve` / `runtime-audit` / 实际 MCP probe；`ccrun` 只回答或执行，不回拨 Agent。**Batch D2 完成前，Agent Node 上这两个诊断入口仍不能作为符合 inbound-only 安全模型的证据。**具体见[双执行入口方案](plan/runtime-surfaces.md)。

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

`allow_unconfined_exec = true` 是发布前 dogfood 逃生开关。

它的含义只是：

> 我知道当前 Runtime 没有隔离，但这个测试 workspace 暂时允许执行命令。

它不会让 Runtime 变安全，而且命令结果会明确标记为 unconfined。

**P1 后这个开关不能跳过身份未知或 Agent 凭据隔离失败。** 同一物理机器可以有 Agent 和 Runtime 两种角色，但隔离 Runtime 的执行身份不能读取已知 Agent 认证文件/容器。失败会在 MCP 初始化、Git 探测前拒绝；exec 前再次检查。目录/ACL/symlink 不明不能报成“没有凭据”。SSH 检查不读取私钥内容，保留 `authorized_keys` 等已知公开文件，其余可疑候选保守拒绝。具体范围、环境来源及未证明的 OS credential service/其他目录见 [Provider 安全契约](provider-safety.md)。

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

网络隔离是单独一层策略。

如果你的安全要求是：

> Runtime 项目代码永远不能访问 Anthropic，甚至不能访问公网。

那么必须在 Runtime Node / `ccrun` 周围通过 OS、网络、防火墙、VM 或容器环境真正执行这个策略。

不要用下面这种方式代替：

```text
禁止 curl
禁止 wget
禁止 python
```

因为这不是可靠安全边界。

## 最终门禁

完成 Runtime identity、SSH alias 和 ACL 后：

```bash
ccnm doctor <workspace>
```

**在 Runtime Node 上、用你自己的账号跑就行。** 关于 Runtime 的行由 Runtime Executor 自己回答，跟你是谁无关。

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
