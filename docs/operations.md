# 运维：装、退、清、修

装上去之后要做的事。每条都是能照着敲的命令，不是原则说明。角色和 topology 见[架构说明](architecture.md)，能力边界见[支持矩阵](support-matrix.md)。

**先说清楚一件事：这个项目还没发布。** 仓库里有 CI 和 release 配置，那不等于有过一次正式发布。下面的安装办法是给自己两台机器用的。

## 安装与升级

两台机器要装**同一个 build**。`ccnm doctor` 会比对两端版本，不一致就是 FAIL：

```text
<对面> runs ccnm 0.2.0, this machine runs 0.2.1; install the same build on both
```

这是 doctor 拦下来的，不是协议层拦的——协议只在**协议号**不同时才拒。所以版本不同的两端有可能跑起来，只是没人验证过那种组合，别让它发生。

```bash
bash scripts/deploy.sh <另一台的 ssh 别名> [workspace]
```

在有 Rust toolchain 的那台上跑。它编译、装两边、重启 controller、最后跑一次 `ccnm doctor`。

### 升级前先把会话停掉

**正在跑的会话不会被升级杀掉**——tmux server 在自己的进程组里。听起来是好事，实际是这一节存在的原因：那个活下来的会话，连着的 `ccnm internal mcp-serve` 还在用**老代码**跑，而且攥着这棵工作树的写入 guard 不放。

于是升级之后：

```text
Remote MCP handshake    FAIL   CCNM_E_POLICY: MCP initialize failed over ...
                               stderr: CCNM_E_POLICY:
                               workspace write guard is busy; another session still owns this working tree
```

**在会话里看到的完全是另一回事**：Claude Code 对 stdio server 退出只显示 `CONNECTION_CLOSED`，不显示 stderr。所以模型手里一个 ccnm 工具都没有，而它自己机器上的文件工具本来就是关掉的——它会把工具调用**当成普通文本打出来**：

```text
<parameter name="command">ls -la ...</parameter>
```

看着像模型抽风，实际是它一个能用的工具都没有。2026-09-12 真撞过一次，查了半天才定位到是升级留下的孤儿进程。

所以顺序是：

```bash
ccnm stop <workspace>                  # 每个在跑的 workspace 都停
ps aux | grep 'ccnm internal mcp-serve'   # 确认真没了，别只看 ccnm status
bash scripts/deploy.sh <另一台的 ssh 别名> [workspace]
```

第二条不能省：`ccnm status` 只报 Agent Node 上的 tmux 会话，`--print` 的运行和已经断开的 SSH MCP 都不在里面（见下面[状态文件](#状态文件在哪多大怎么清)那一节）。

万一已经升完了才想起来，按[写入 guard 残留](#写入-guard-残留)清：杀掉那个 `mcp-serve`，确认没有残留子进程，再备份删掉那**一个** marker 文件。

### 升级完一定要核对 controller 的进程启动时间

**`ccnm controller install` 不会替换一个已经在监听的 controller。** 它看到有人在听就报告那一个，输出长这样：

```text
listening: ccnm 0.2.0 as fodelf, pid 1716, Aqua
```

读起来像刚重启过，实际那个 pid 可能是好几天前起的，跑的还是旧二进制。`ccnm doctor` 也抓不到——它显示的 `0.2.0` 是版本字符串，同一个版本号的新旧构建长得一模一样。

症状出现在别的地方，而且不指向 controller：

```text
Claude Code             FAIL   CCNM_E_VERSION: remote ccnm speaks protocol 3, this one speaks 1
Claude authentication   FAIL   CCNM_E_VERSION: remote ccnm speaks protocol 3, this one speaks 1
```

所以升级后自己核对一次，比对二进制的 mtime 和进程的启动时间：

```bash
ssh <agent> 'stat -f "%N %Sm" -t "%Y-%m-%d %H:%M" ~/.local/bin/ccnm; ccnm controller status'
ssh <agent> 'ps -o pid=,lstart=,command= -p <上面那个 pid>'
```

进程比二进制还老就是没换掉。真正的重启是先卸再装：

```bash
ssh <agent> 'ccnm controller uninstall && ccnm controller install'
```

`uninstall` 会移除 plist 和 socket，但**不保证旧进程退出**：更老的构建里这个子命令叫 `internal work-controller`，launchd 的当前标签管不到它，卸载之后它会作为孤儿进程留着。socket 已经没了，所以它不会再被连上，但要彻底干净就自己确认一次并按 pid 结束它。

### 千万不要 `cp` 覆盖正在用的二进制

这是这个脚本存在的主要理由。在 Apple Silicon 上，往一个已经执行过的 Mach-O 里写东西会让它的代码签名失效，之后每一次 exec 都直接 SIGKILL（退出码 137），而**已经在跑的那个进程照常用旧代码继续**。

症状极具迷惑性：`ccnm --version` 显示 `Killed: 9`，`doctor` 报空回复，而 `launchctl` 坚称 controller 一切正常。

正确做法是写一个新文件再 rename 覆盖——inode 变了，运行中的进程留着自己那份，下一次 exec 拿到完整且签名正确的新文件：

```bash
install -m 755 target/release/ccnm ~/.local/bin/ccnm.new
mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
```

远端同理，而且 `scp` 要带 `-p` 并显式 `chmod +x`：OpenSSH 10.3 的 scp 不带 `-p` 会丢掉权限位，10.2 的会保留。落一个 0644 的二进制过去，对面报的是 `zsh: permission denied`，看着像安装坏了而不是少了一个执行位。

### 回退

回退就是把旧版本按同样的方式装回去，没有单独的回退命令：

```bash
git checkout <旧的 tag 或 commit>
bash scripts/deploy.sh <另一台的 ssh 别名>
```

两边必须一起退。只退一边的话，下一次 `ccnm doctor` 会把它标成 FAIL——但那是 doctor 在看，没有任何东西会在运行时拦住你，所以别指望它兜底。退完重启 controller（`deploy.sh` 会做），已有会话不受影响。

**状态文件不随回退变化。** 会话记录里带着写它时的协议号：协议号没变，旧版本会忽略不认识的字段照常读；协议号变了则明确拒绝，而不是当成半懂的记录接着用。

保险起见，回退前把正在跑的会话停掉——一个会话的两半分别记在两台机器上，让它跨越一次两端不同步的回退没有意义。

## 配置迁移：legacy → Agent Instance

**没有自动迁移命令**，是手动改配置文件。改动很小，改完用 `doctor` 验。

Runtime Node 的 `~/.config/ccnm/config.toml`，把 workspace 的 `agent_node` 换成 `agent`：

```toml
# 之前
[workspaces.demo]
agent_node = "worker"
root = "/Users/me/demo"

# 之后
[workspaces.demo]
agent = { node = "worker", instance = "claude-main" }
root = "/Users/me/demo"
```

Agent Node 的配置里定义那个 instance：

```toml
[agents.claude-main]
provider = "claude"
profile_ref = "default"
```

然后在 Runtime Node 上验：

```bash
ccnm doctor demo
```

三条容易踩的：

- **两个字段不能同时存在。** 配置校验会拒绝 `agent` 和 `agent_node` 并存，报 `cannot combine agent with legacy agent_node`。
- **instance workspace 的 root 只能定义在它的 Runtime Node 上**，在 Agent Node 的配置里写同名 workspace 会被拒。
- **`claude_permission_mode` 对 instance workspace 无效**，配了会被拒；instance 的策略在 Agent 端。

迁移前有会话在跑的话，先 `ccnm stop <workspace>`：会话记着自己的 identity，配置换了它也不会跟着换。

## 项目目录：属主要对，git 身份要配

真机上撞出来的两条，装完环境第一次放真项目时一定会遇到。

这里说的“执行身份”是 **Runtime Executor**（通常叫 `ccrun`）：Agent 的 MCP transport 落到的那个账号，项目工具真正以它的身份跑。敲 `ccnm` 的是 **Operator**，通常是你自己的账号，两者不该是同一个——四种身份的完整边界见[生产安全](production-safety.md)。

**项目目录必须属于 Runtime 执行身份本人，光可写不够。** 放在别人拥有的 0777 目录里，文件是写得进去，但那个身份跑任何 git 命令都会被拒：

```text
fatal: detected dubious ownership in repository at '/path/to/worktree'
```

以前 `ccnm doctor` 那一行照样是绿的（`Workspace root OK … is a directory for <user>`），因为它只查目录在不在。**现在它连属主和 git 一起查**：git 因属主拒绝时这一行是 FAIL 并直说原因，属主不对但 git 能用时是 WARN。绿灯这才等于"这个身份真的能用这个项目"。

顺带一条：`ccnm workspace add` 用**当前进程的身份**校验路径。以别的账号去注册 Runtime 执行身份自己家目录下的项目，会得到

```text
CCNM_E_WRONG_WORKSPACE:
/Users/<runtime-user>/<project> is not a directory on this machine
caused by: Permission denied (os error 13)
```

第一行读着像路径写错了，真正的原因在第二行。

**这是个已知缺陷，不是设计。** 注册 workspace 是 Operator 的活儿，可它却拿当前进程的身份去 stat 那个目录，于是"项目放在执行身份自己家里"这种最该被支持的布局反而注册不了。眼下的绕法是临时用 Runtime Executor 的身份跑一次 `workspace add`。

（会话打开那条路已经修了：Runtime 自己解析 workspace 和 root，`doctor` 的项目可用性也由执行身份回答。`workspace add` 是**写配置**的命令，还留在 Operator 侧用当前身份校验，没跟着改。）

**新建的执行身份没有 git 身份，第一次 commit 直接失败：**

```text
fatal: unable to auto-detect email address
```

`doctor` 不查这个。在那个身份下配一次就行：

```bash
git config --global user.name "<name>"
git config --global user.email "<email>"
```

不配也能干活——Agent 会退而用 `git -c user.name=… -c user.email=…` 传单次参数，但**下一个会话还会撞同一堵墙**。（[P12 那一轮](research/p12-real-project-2026-09-11.md)里真实 Claude Code 就是这么干的：它没去改那台机器的 git 全局配置，只给自己那一次提交带上了身份。）

## Runtime Node 的前置条件与项目工具链

**ccnm 不装工具链，不升级它，也不代管版本。** 它不知道你的项目要什么。装什么、装在哪、谁维护，是 Runtime Node 管理员的事——这一节说的是怎么装得让工具**真的能被调用到**，因为这里有一脚很容易踩空。

**ccnm 自己要两个程序**，两个入口都要：

| 程序 | 谁要它 | 没有它会怎样 |
| --- | --- | --- |
| `git` | `list_files`、写 guard 的资源判定、项目自己 | 降级成非 git 视图；guard 按目录而不是按仓库互斥 |
| `ripgrep`（`rg`） | `search_text`——它不自己扫文件 | 七工具少一个，报 `ripgrep is not installed on the Runtime Node` |

workspace 开了 [`codex_exec_server`](configuration.md#codex_exec_server) 时，Runtime 上还要：

| 前提 | 为什么 | 没有它会怎样 |
| --- | --- | --- |
| 节点配置里的 `codex_bin` 指向 Codex 0.154.0 | 执行模型命令的是它的 `exec-server` | 会话启动前报 `CCNM_E_CONFIG` 或 `CCNM_E_VERSION` |
| **Linux**：装 `bubblewrap`，并允许执行账号创建 user namespace（Debian 13 默认允许） | Codex 在 Linux 上用 bwrap 实现 workspace-write 沙箱 | 每条命令都失败、不执行（P21 容器实测） |

Codex 的 Linux 沙箱会在真实的 `/tmp` 里留下几个空目录（`/tmp/.git`、`/tmp/.agents`、`/tmp/.codex`、`/tmp/codex-bwrap-synthetic-mount-targets-<uid>/`），属主是执行账号，用完不删；`/tmp` 是 tmpfs 的话重启就没了。这是 Codex 的行为，ccnm 不清理它们（P24 实测）。

剩下的是项目自己的：编译器、包管理器、测试运行器。

**装在执行身份自己的 home 里，不要装成全机共享。** 这不是洁癖：Runtime Executor 的意义就是"除了这个项目什么都没有"，而一个装到 `/usr/local` 的工具链会同时属于机器上每个账号。[P12 那一轮](research/p12-real-project-2026-09-11.md)在 Debian 上的做法是：

- 系统级只装 Rust 链接期要的 C 工具链（`gcc libc6-dev make`）和 `ripgrep`——rustc 自己不带 linker，这一步绕不开；
- rustup（`~/.rustup`、`~/.cargo`）和官方 Node 二进制包（`~/.local/node-<版本>`）以**那个账号自己的身份**装进它的 home；
- 两条命令写成了可重跑、可撤销的脚本：[建执行身份](../scripts/p12-provision-linux-runtime.sh)（要 root，清单先于变更、`--revert` 按清单精确撤销）和[装工具链](../scripts/p12-runtime-toolchain.sh)（**不要 root**）。它们是那一轮的实测做法，可以照抄，也可以只当参考。

### 最容易踩的一脚：非交互 ssh 的 PATH

`exec_command` 的命令跑在一条**非交互** ssh 会话里，而大多数工具链安装器写的 PATH 在那条会话里不生效：

- Debian/Ubuntu 的 `~/.bashrc` 第 6 行就是 `case $- in *i*) ;; *) return;; esac`，非交互直接返回；
- rustup 默认把 PATH **追加在文件末尾**（也就是那个 `return` 之后），另一份写在只有 login shell 才读的 `~/.profile`。

照默认装完，ccnm 报的是

```text
cargo is not installed on the Runtime Node, or is not on its PATH
```

**看着像没装，其实是装了但 PATH 没到。** 做法是把 PATH 那一块写在那个 `return` **之前**（rustup 用 `--no-modify-path`，自己写），然后从客户端问一次——只有这一句话算数：

```bash
ssh <runtime-alias> 'command -v cargo node npm rg git'
```

在 Runtime 上 `echo $PATH` 不算：那是登录 shell 的答案，不是 `exec_command` 会看到的那一条。

### 装工具链需要出站网络

工具链要从网上下载，所以**装的时候** Runtime 得出得去；`exec_command` 之后能不能出去是另一个问题，ccnm 对此[不作保证](support-matrix.md#egress不作保证)。真机上还撞到过出口不均质：`static.rust-lang.org` 直连没问题，`nodejs.org` 会 TLS reset（`curl: (35) Recv failure`）。换镜像可以，但**完整性要用官方哈希校验**——在能连上官方的那台机器上取 `SHASUMS256.txt`，把哈希带过去固定，不要信镜像自己给的清单。

## 状态文件在哪，多大，怎么清

两边都在 `${XDG_STATE_HOME:-~/.local/state}/ccnm/`，但内容分工不同。

**Agent Node：**

```text
sessions/<ccnm-session-id>/
├── session.json     启动这个会话需要的全部信息
├── mcp.json         给官方 CLI 的 --mcp-config：通往 Runtime 的那一条 ssh
├── settings.json    --settings：只允许 ccnm 的那几个工具
├── stdout           官方 CLI 的 stdout（print 模式下是 JSON 结果）
├── stderr           官方 CLI 的 stderr
├── supervisor.log   supervisor 自己的诊断
├── tmux.conf        ccnm 自己那个 tmux server 启动时读的配置（见下）
├── codex-home/      只有 codex_exec_server 的 Codex 会话有：这个会话的 CODEX_HOME（见下）
└── exit             最后写的：它是怎么结束的
workspaces/<name>/   官方 CLI 的工作目录
controller.sock      controller 的监听 socket
```

`codex-home/` 是 exec-server 链（[配置说明](configuration.md#codex_exec_server)）给 Codex 的私有 `CODEX_HOME`：ccnm 写进去的只有 `environments.toml`、指向 profile 里 `auth.json` 的 **symlink** 和只含信任条目的 `config.toml`；其余（`sessions/`、`history.jsonl`、几个 sqlite）是 Codex 自己在会话里写的。symlink 指向的那个文件才是登录凭据，删这个目录不动 profile。

**Runtime Node：**

```text
sessions/<ccnm-session-id>/output/   exec_command 留下的命令输出
write-guards/                        工作树级独占锁
rpc/sessions/<handle>.json           machine API 的会话记录
rpc/keys/<workspace>/<start_key>     启动幂等键
ssh/                                 ControlPath socket
```

`tmux.conf` 写在会话目录里，是因为那是 ccnm 一定拥有、一定存在的目录。tmux **只在启动 server 的那一刻**读它，所以哪个会话的那份起的作用不重要，跟着会话一起被删也不影响任何东西。里面设了什么、怎么改回去，见[使用说明](usage.md#会话在-tmux-里所以滚屏和复制跟你平时不一样)。

**没有自动清理，也没有保留期。** 会话记录一直留着，除非你删。这是刻意的：一个已经结束的会话，它的输出往往比它本身有价值。

单个会话通常几十 KB，`output/` 取决于命令打印了多少。真占地方了按 workspace 清：

```bash
ccnm workspace remove demo --purge     # 先停会话，再删 ccnm 为它保存的东西
```

`--purge` 删的只有 ccnm 自己的记账：会话记录和官方 CLI 的工作目录。**永远不碰项目本身**——那是两台机器上唯一不是 ccnm 创建的东西，一个可能删掉别人源码树的清理命令不叫清理命令。

machine API 的记录（`rpc/`）不在 `--purge` 范围内，目前只能手动删。删之前确认没有正在跑的会话——记录没了，`session.status` 会回 `-32009`，而 Agent 那边可能还在跑。

## 停止

```bash
ccnm stop demo                                   # 停这个 workspace 的会话
ccnm stop demo --agent codex-main --session <id>  # 精确停一个
```

停成功的判据是**三件事都被观察到**：Agent 进程组结束、承载工具调用的 MCP transport 结束、Runtime 的写入 guard 释放。任何一条证明不了，状态停在 unknown 而不是报成功——写权限提前交给下一个人，两个 Agent 就会同时改一棵工作树。

`ccnm status demo` 看当前状态。两个实测出来的坑：

- **`stop` 对已经结束的会话是幂等的**（v1 起）：没有会话在跑时它退出码 **0**，报告里 `killed` 为 false。清理脚本可以无脑调一次，不用先判断有没有人在用。

  但幂等**不等于 stop 永远不报错**。有一种情况仍然是失败：workspace 的终端**确实在跑**，而 ccnm 认不出它是不是你选的那个会话——报 `CCNM_E_NOT_READY: a terminal is running for this workspace but carries no verifiable ccnm session identity`。那道检查是为了不去杀别人的会话，跟幂等无关。

  另外，`--session <id>` 指到一个这台机器上没有记录的 id，仍然报 `no session <id> on this machine`：不知道那个会话，和知道它已经结束，是两件事。

  > 早于 v1 的构建在第一种情况下也报退出码 3。写清理脚本时如果要兼容旧版本，容忍这个码即可。
- **`status` 看不见 `ccnm run --print` 的会话。** 它只报 Agent Node 上的 tmux 会话，非交互的 print 运行不在其中——会话正跑着、写入 guard 是 `held`、MCP 进程也在，`status` 照样说 `no live sessions`。据此判断"没人在用"然后起第二个会话，撞上的就是被占的写入 guard，而那个失败长得像别的毛病。要判断真没人用，看写入 guard 和进程列表，别只看 `status`。

## 故障恢复

### 写入 guard 残留

症状：新会话起不来，报工作树被占，但没有会话在跑。

异常退出会在 Runtime 的 `write-guards/` 里留下 `held <session> <workspace>` 标记，状态是 unknown。**ccnm 不会因为时间过去就自动接管**——它证明不了旧的执行者已经结束。

恢复必须由 Runtime 操作者做，顺序不能反：

1. 先证明旧的都结束了：`ccnm status <workspace>` 加进程列表，确认旧 supervisor、Agent、SSH MCP 及其子进程都没了。
2. 在 `${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/` 里找到包含那个 session id 的**单个** marker 文件。
3. 备份后删掉那**一个**文件。

不要批量删，不要仅因为"过了很久"就清。**证明不了旧执行者结束时，保持 unknown 才是对的状态。**

**占着锁的是 Codex exec-server 链时**（`codex_exec_server = true` 的 workspace），第 1 步要找的是 `ccnm internal exec-serve`、`codex exec-server` 和它们起的命令；Agent Node 那边对应的是 Codex 自己 spawn 的 `ccnm internal exec-transport`——它 exec 成了一条 `ssh … internal exec-serve`，`ps` 里看到的是 ssh。命令不一定还挂在这两个进程下面：exec-server 给每条命令单独开进程组，用 `setsid` 脱离的进程会被 init 收养。它们的环境变量里都有 `CCNM_EXEC_SESSION=<session id>-<随机串>`，按这个找（macOS 用 `ps -axEww -o pid,command`，Linux 看 `/proc/<pid>/environ`）。监督进程自己放不了锁时报的错里就带着这个值。

### Agent 静默离网之后，exec-server 链的锁一直 held

症状：Agent 那台机器断了网、睡着了或者直接关机，之后谁在这个 workspace 上开新会话都报 `workspace write guard is busy`，而 Agent 那边早就没有这个会话了。

**先等：从 Agent 最后一次有动静算起，最多 10 分钟锁会自己释放**（P26 起的构建）。Runtime 上的 `ccnm internal exec-serve` 在连接上连续 30 秒收不到任何字节时，发一个探活请求 `ccnm/liveness`。Codex 不认识这个请求，按它的规矩回一个 `-32601` 错误，回了就说明它还在。**连续 10 分钟一个字节都没收到**（探活的回答也没有），`exec-serve` 就按正常路径收尾：关 exec-server、扫进程、写 `released`。Runtime 的 stderr 里是这三行（时间戳省略），第三行出现才说明锁真的放了：

```text
WARN nothing from the client, not even an answer to a liveness request; ending the exec-server session silent_seconds=600
INFO exec-server relay ended end=ClientSilent
INFO exec-server session ended; write guard released session=<session id>
```

为什么要等这么久、不是立刻判死：TCP 连接在网络抖一下、机器短暂睡眠时是会活过来的，30 秒没回答不代表人走了。10 分钟内回来的会话照常可用：本机把真实 Codex 冻住 2 分钟再恢复，积压的 4 个探活在恢复瞬间全部得到回答，下一条命令正常执行（[P26 记录](research/p26-native-liveness-2026-09-17.md)）。

**代价**：Agent 机器睡眠或断网**超过 10 分钟**，原生会话会被 Runtime 结束。醒来之后 TUI 上**不会**先有任何提示，要等 Codex 的下一条命令报 `exec-server transport disconnected`（[排错手册](troubleshooting.md#codex-会话里模型报-toolsexec_command-is-not-a-function或-exec-server-transport-disconnected)症状 B），在 Codex 里 `/exit` 再起一个会话。结束前没跑完的命令已经被杀掉，不会在断线后继续改文件。

**等了 10 分钟还是 held**，只有两种可能：

- Runtime 上的 ccnm 早于 P26，没有探活。比如 P24 真机验收装在 hpsrv 上的那份（7ae2d4b）就没有。换成新构建，或者按下面的步骤手工结束。
- 收尾时有进程没能证明已经结束，锁按设计留在 `held`。stderr 里会有 `the workspace write guard stays held`，按[写入 guard 残留](#写入-guard-残留)处理，**不要**用下面的步骤。

**手工结束**（不想等，或者 Runtime 是旧构建），在 Runtime 上以执行账号做，顺序不能反：

1. 从 `write-guards/` 里那个 `held <session> <workspace>` 找到 session id，确认 Agent 那边这个会话确实已经不在了（`ccnm status` 或者 Agent 机器的进程表）。
2. 找到这个会话的 `ccnm internal exec-serve`（`ps -u <执行账号> -o pid,ppid,args`，payload 里带 session id；看不出来就按 `CCNM_EXEC_SESSION` 环境变量找它起的进程），它的父进程是这条连接的 `sshd-session: <执行账号>@notty`。
3. 给那个 `sshd-session` 发 TERM。`exec-serve` 读到 EOF，按正常路径关掉 exec-server、扫进程、写 `released`——**不用手工删锁标记**。
4. 再看一眼 `write-guards/` 里是不是 `released`，带 `CCNM_EXEC_SESSION` 的进程是不是一个都没有。

想比 10 分钟更早发现：给 Runtime 的 sshd 配 `ClientAliveInterval`（系统配置变更，按你们的变更流程走），代价是网络短暂抖动更容易把正常会话断掉。10 分钟这个值目前不能配置。

### 会话在 initialize 就断，报 "connection closed: initialize response"

先看 Runtime 执行身份的 home 路径上**有没有一层是符号链接**。macOS 的 `/tmp` 和 `/var` 都是，所以任何把 Runtime home 放在系统临时目录下的做法都会踩到：

```text
ccnm: handshaking with MCP server failed: connection closed: initialize response
```

真正的原因在 mcp-serve 的 stderr 里：

```text
CCNM_E_POLICY: … No Claude credential: known credential accessibility is unknown
Runtime initialization is also refused: this identity can reach a known Agent login.
To accept that for one workspace -- every command the model runs could then read it --
set allow_unisolated_credentials = true on it in config.toml.
```

凭据检查见到祖先目录是 symlink 就判 **unknown**，而 unknown 跟"能读到"走同一条路：`allow_unconfined_exec` 救不了它（那个开关只接受 confinement 风险），要么修路径，要么用 `allow_unisolated_credentials` 明确接受"说不清"。这是刻意的——够不到和"看不清能不能够到"不是一回事，后者得有人签字。

**这种情况下先别急着开开关**，多半只是路径写歪了：用真实路径（`/private/tmp/...` 而不是 `/tmp/...`）就好了。macOS 的 `/tmp` 和 `/var` 都是符号链接，把 Runtime 执行身份的 home 放在系统临时目录下就会撞到这个。

### controller 不响应

```bash
ccnm controller status      # 在监听吗？在哪个安全会话里？
ccnm controller install     # 重装并重启；已有会话不受影响
```

`managername` 必须是 `Aqua`。如果是 `Background`，说明它不在图形登录会话里，那样它启动的 Agent 读不到 Keychain，会以认证失败告终——`ccnm run` 会在创建会话前就拒绝，报 `CCNM_E_NOT_READY`。

### 会话状态是 unknown

**unknown 是终态，不会自己变好。** 它表示 ccnm 证明不了这个会话的下落，不表示失败。

正确做法是去现场看：工作树、`git status`、Runtime 上的进程列表。**不要重试**——那个 Agent 可能已经改了文件、跑了命令。"不确定有没有执行"和"确定没执行"是完全不同的两件事，只有后者重发才安全。

### machine API 那边

`ccnm rpc` 挂掉不会停掉已经接受的会话——它们属于磁盘上的记录，不属于那条连接。重新连上来用 session id 照样查。

服务端死于运行途中会留下 owner 已经不在的记录，之后读出来是 `unknown` 而不是 `failed`，理由同上。丢了 session id 只能靠 `start_key` 找回，所以凡是结果有意义的执行都该给一个键。详见[协议说明](protocol/README.md)。

## 部署与登录相关的动作要单独授权

创建系统账号、改 ACL 或防火墙、配置独立登录、替换正在运行的二进制或 controller——这些每一次都要单独获得明确批准。**规划过不等于授权执行。**
