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

在有 Rust toolchain 的那台上跑。它编译、装两边、重启 controller、最后跑一次 `ccnm doctor`。**正在跑的会话不受影响**——tmux server 在自己的进程组里。

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

不配也能干活——Agent 会退而用 `git -c user.name=… -c user.email=…` 传单次参数，但**下一个会话还会撞同一堵墙**。

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
└── exit             最后写的：它是怎么结束的
workspaces/<name>/   官方 CLI 的工作目录
controller.sock      controller 的监听 socket
```

**Runtime Node：**

```text
sessions/<ccnm-session-id>/output/   exec_command 留下的命令输出
write-guards/                        工作树级独占锁
rpc/sessions/<handle>.json           machine API 的会话记录
rpc/keys/<workspace>/<start_key>     启动幂等键
ssh/                                 ControlPath socket
```

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

- **`stop` 不是幂等的。** 对已经自己结束的会话再停一次，退出码 **3**，报 `CCNM_E_NOT_READY: no verifiable selected session is running`。话没说错（确实没有可停的会话），但清理脚本无脑调一次 stop 就会拿到非零退出，看起来像清理失败。脚本里要么容忍这个码，要么先用下面的办法确认还有没有会话。
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
