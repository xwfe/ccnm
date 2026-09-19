# 出错了怎么办

每一条都是真撞过的：先写**你看到的现象**，再写它其实是什么、怎么办。
按现象找，不用按功能找。

**这页里的 `ccnm doctor` 样本是英文那版**（`ccnm doctor <workspace> --lang en`）。ccnm 默认说中文，所以你屏幕上的行名跟这里贴的不一样。排查时最省事的办法是加 `--lang en` 跑一遍，跟这里逐行对上；要按中文找，常见的几行是这个对应关系：

| 中文 | 英文 | 中文 | 英文 |
|---|---|---|---|
| 配置文件 | Config | Runtime 执行身份 | Runtime user |
| workspace 配置 | Workspace config | Claude 凭据 | No Claude credential |
| workspace 根目录 | Workspace root | Runtime 安全 | Runtime safety |
| 远端 MCP 握手 | Remote MCP handshake | admin 组 | Not an admin |
| 连 Agent 的 SSH | Agent SSH | SSH 私钥 | No SSH keys |
| 终端会话 | Terminal session | 命令审批 | Command approval |
| Runtime 上的项目 | Runtime workspace | sudo 权限 | No sudo |
| Codex 原生链 | Codex exec-server | | |

英文那几个 `No …` / `Not …` 开头的名字，中文用的是中性名词（`SSH 私钥` 而不是 `没有 SSH 私钥`）。原因是英文那种否定式当表头读得通，中文读起来却像一句陈述——`不在 admin 组｜注意｜this account is in admin` 会让人读到跟事实相反的结论。所以名字只说查了什么，结果全看状态那一列。

状态词：`正常`=OK、`注意`=WARN、`没查`=SKIP、`失败`=FAIL。结论行 `可以用了`=READY、`还不能用`=NOT READY。

**错误码不跟着变。** `CCNM_E_*` 两种语言下都一样，所以拿错误码搜这一页永远搜得到。

### `TOOLS DOWN` —— 会话看着在跑，模型却什么都够不着

```text
ccnm-xshun  xshun  detached  TOOLS DOWN (in Claude: /mcp -> ccnm -> Reconnect)  (...)
```

**症状**：Claude 还能聊天，但一让它读文件就开始瞎猜，或者去调它自己机器上的 Bash
（会被拒，那是第二道锁）。原因是那条 MCP ssh 断了——网断太久、Agent Node睡了、有人 kill 了它。
Claude 不会自己重连。

**修**：在 Claude 里敲

```text
/mcp  →  选 ccnm  →  Reconnect
```

会话、上下文、正在做的事全都留着，只是把通道接回来。

`ccnm status <ws>` 和 `ccnm doctor <ws>` 都会明说这个状态——**看着正常的会话是不会显示
这行的**，所以看到了就是真断了。

### `ccnm run` 报 `CCNM_E_POLICY`，第二行却是 `MCP initialize failed over …`

```text
CCNM_E_POLICY:
MCP initialize failed over `/usr/bin/ssh … internal mcp-serve --payload …`: connection closed: initialize response
stderr: CCNM_E_POLICY:
workspace write guard is busy; another session still owns this working tree
```

**Runtime 连得上，是这个 workspace 的写锁被别的会话占着**（退出码 33）。起会话前，Agent Node 上的 ccnm 先经 ssh 跟 Runtime 做一次 MCP 握手预检；Runtime 在握手之前拒绝，第一行就是它给的理由。`MCP initialize failed over …` 那一行不是另一个错误，只是说明这个拒绝从哪条链路带回来的，看着像网络问题，其实不是。按 [MCP 初始化报 busy 或 unknown](#mcp-初始化报-workspace-write-guard-is-busy-或-unknown) 处理：看 Runtime 上 `write-guards/` 里是谁占着，见[写入 guard 残留](operations.md#写入-guard-残留)；占锁的是已经离网的 Codex exec-server 会话时，见[静默离网之后锁一直 held](operations.md#agent-静默离网之后exec-server-链的锁一直-held)。`ccnm doctor` 的 `Remote MCP handshake` 行和 `ccnm mcp probe` 走的是同一次握手，报法一样。

**旧版本的第一行是 `CCNM_E_RUNTIME_UNREACHABLE`（退出码 21），原因相同。** 已发布的 0.7.0 及更早版本把握手时 Runtime 的拒绝一律报成"连不上 Runtime"，真正的码只在 `stderr:` 后面（P24 真机撞到，P25 修）。预检在 Agent Node 上跑，所以看的是**Agent Node 上** ccnm 的版本；脚本按退出码判断时，那边还是旧版就得先看 `stderr:` 后面第一行。真连不上（ssh 超时、拒绝连接、认证失败）新旧版本都还是 `CCNM_E_RUNTIME_UNREACHABLE`。

### Codex 会话里模型报 `tools.exec_command is not a function`，或 `exec-server transport disconnected`

只出现在 workspace 写了 `codex_exec_server = true` 的 Codex 交互会话里（[配置说明](configuration.md#codex_exec_server)；这条链 2026-09-17 起封存，只认 Codex 0.154.0）。这条链上 Codex 用自带的执行工具，工具在 Runtime 上由 exec-server 执行，Codex 通过它自己 spawn 的 `ccnm internal exec-transport` → ssh → `ccnm internal exec-serve` 连过去。

**症状 A**：一开始就没有工具——模型调 `exec_command` 时报 `TypeError: tools.exec_command is not a function`，TUI 上什么也不说。**原因**：Codex 起来时连不上 Runtime（ssh 失败、Runtime 那边拒绝了），于是它的"远端环境"不可用，`include_local = false` 又让它没有本地环境可退，工具表就是空的。Codex **不会**退回到本机执行，这是设计。**修**：在 Agent Node 上手工跑一遍会话目录里 `codex-home/environments.toml` 写的那条 `program`/`args`（就是 `ccnm internal exec-transport --payload …`），ssh 或 Runtime 的 `CCNM_E_*` 错误会直接打出来。正常情况下这一步在创建会话前的预检就会失败，走不到 Codex；走到了多半是会话启动之后网络或 Runtime 变了。

**症状 B**：跑着跑着某条命令报 `exec_command failed: ProcessFailed { message: "exec-server transport disconnected" }`，之后每条都报 `Rejected("Failed to create unified exec process: exec-server transport disconnected")`。**原因**：那条 ssh 断了，或者 Agent 机器睡眠、断网超过 10 分钟，Runtime 已经按无响应结束了这个会话（P26，见[运维手册](operations.md#agent-静默离网之后exec-server-链的锁一直-held)）；还有一种是**模型让 Codex 写了一个超过约 24 MiB 的文件**——Codex 把整个文件 base64 之后放进一条消息，Runtime 上的 `exec-serve` 单条消息上限 32 MiB，超过就结束整个会话、文件不写（exec-server 自己到 64 MiB 会一声不吭断连，ccnm 在前面先停；`exec-serve` 的 stderr 里是 `client message too long; ending the exec-server session`；[P29](research/p29-native-gates-2026-09-17.md#43-写单文件上限与磁盘写满) 用录下的 Codex 请求实测了会话结束，没在真实 Codex 里撞过，Codex 界面上先报什么不确定）。大文件让模型用命令生成，别用 patch 写。Codex 对这种传输**不重连、不 resume**，断线之后的命令哪里都没执行；Runtime 侧的 `exec-serve` 看到 EOF 会关掉 exec-server、扫进程、放锁。**修**：`/exit` 结束会话再起一个。断线时若 Codex 正好有 patch 没写完，它会问"command failed; retry without sandbox?"——答"是"也到不了 Runtime（传输已经死了），到了也会被 ccnm 拒绝（`sandbox: null` 一律拒）。

### doctor 里 Codex 原生链那一行失败

只出现在写了 `codex_exec_server = true`、Agent 是 Codex 的 workspace 上。这一行做的就是 `ccnm run` 起 Codex 之前那次预检（[它查什么、不查什么](usage.md#codex-原生链那一行)），所以这里红，起会话也会停在同一处。看 `CCNM_E_*` 后面 Runtime 自己说的原因：

```text
Codex exec-server       FAIL   CCNM_E_CONFIG: ccnm internal exec-serve on runtime-alias failed (exit 10): nodes.runtime.codex_bin is not set; the exec-server chain needs this Runtime to name its Codex binary
```

**Runtime 没配 `codex_bin`。**在 **Runtime 的** config.toml 里给那个节点写上 Codex 0.154.0 的绝对路径（`[nodes.<runtime>]` 下的 `codex_bin`，见[配置说明](configuration.md#codex_exec_server)）。写在 Agent 的配置里没用：这个值只从 Runtime 自己的配置读，不上 wire。

```text
Codex exec-server       FAIL   CCNM_E_VERSION: ccnm internal exec-serve on runtime-alias failed (exit 11): Codex 0.155.0 has not been measured; this adapter requires 0.154.0
```

**`codex_bin` 指的不是 0.154.0。**这条链只对实测过的那一个版本开。以执行账号跑一遍 `<codex_bin> --version` 核对，把 `codex_bin` 指到 0.154.0 那份。

```text
Codex exec-server       FAIL   CCNM_E_POLICY: ccnm internal exec-serve on runtime-alias failed (exit 33): workspace write guard is busy; another session still owns this working tree
```

**链没坏，是这个 workspace 正有会话在写。**这一行和 `Remote MCP handshake` 都要取一次写锁再放掉，会话进行中跑 doctor，两行都会带这句话。等会话结束再跑；要是占锁的会话其实已经不在了，按[写入 guard 残留](operations.md#写入-guard-残留)处理，别为了让 doctor 变绿去删锁标记。`CCNM_E_POLICY` 后面换成审计的拒绝理由（执行账号没隔离、读得到 Agent 登录）时，处理办法和 `exec_command` 被拒一样，见[生产安全](production-safety.md)：这条链能跑任意命令，要的是 `exec_command` 那一级放行。

**这一行正常，Codex 里第一条命令却报 `bubblewrap is unavailable: no system bwrap was found on PATH and no bundled codex-resources/bwrap binary`**（P21 容器实测的原文）：Linux Runtime 没装 bubblewrap。执行账号建不了 user namespace 时也是第一条命令才失败。空会话一条命令都不跑，doctor 看不出来，这是有意的（理由见上面那个链接）。按[运维手册](operations.md#runtime-node-的前置条件与项目工具链)补上前提。Codex 接着会问要不要不带沙箱重试，答"是"也会被 ccnm 拒掉。

### 开了 `exec_sandbox` 之后命令报 `Operation not permitted`、`git commit` 失败、`cargo build` 下不了依赖

只出现在 workspace 写了 [`exec_sandbox = "codex"`](configuration.md#exec_sandbox) 的会话里；每条 `exec_command` 结果末尾都有一行 `[sandboxed: …]`，看到它就知道命令跑在沙箱里。**这不是坏了，是沙箱在挡**：命令只能写工作区（`.git` 除外）、`$TMPDIR` 和 `/tmp`，不能连网。被挡的命令按普通失败报（退出码非 0，stderr 里 `Operation not permitted`，Linux 上是 `Read-only file system`），不会有"不带沙箱重试"的路。

- `fatal: Unable to create '…/.git/index.lock': Operation not permitted`（Linux：`Read-only file system`）—— `git commit`、`git stash`、`git checkout` 这类要写 `.git` 的都会这样。提交由人在 Runtime 上做，或者这个 workspace 关掉开关。
- `cargo build` 报 `Couldn't resolve host` / `Operation not permitted (os error 1)` 且路径在 `~/.cargo` 下 —— 没网络，而且 HOME 下的缓存写不了。先在沙箱外（关掉开关，或直接在 Runtime 上）`cargo fetch` 一遍，warm cache 的构建和测试在沙箱里是正常的。`npm install` 同理。
- `sh: /bin/ps: Operation not permitted`（Linux：`fatal library error, lookup self`）—— 沙箱里看不到进程表。
- 会话根本起不来，报 `CCNM_E_CONFIG: nodes.<runtime>.codex_bin is not set` 或 `CCNM_E_VERSION` —— 开了开关但 Runtime 给不了沙箱（没配 Codex、版本不是 0.154.0）。补上 [`codex_bin`](configuration.md#node-的其他字段) 或关掉开关；ccnm 不会退回裸跑。
- 会话起不来，报 `CCNM_E_DEPENDENCY: the exec_command sandbox does not work on this Runtime`，后面跟着 Codex 自己的话 —— ccnm 启动时用沙箱试跑了一条空命令没成。常见三种：Linux 没装 bubblewrap（`bubblewrap is unavailable`）；执行账号建不了 user namespace（`No permissions to create new namespace`），见[运维手册](operations.md#runtime-node-的前置条件与项目工具链)；ccnm 的状态目录在 `/tmp` 下（`Refusing to create helper binaries under temporary dir` 然后 `bwrap: execvp codex-linux-sandbox: No such file or directory`）——Codex 不在临时目录里建辅助程序，把 `XDG_STATE_HOME` 挪出 `/tmp`。

实测哪些能跑、哪些被挡，见 [P33 记录](research/p33-exec-sandbox-2026-09-17.md)。

### `Killed: 9` / exit 137 —— 升级完二进制就全炸

**症状**：`ccnm --version` 直接被杀，doctor 走 ssh 拿到空回复报 `CCNM_E_VERSION`，
但 `launchctl` 显示 controller 好好的。

**原因**：Apple Silicon 上直接 `cp` 覆盖一个正在跑（或跑过）的二进制，代码签名的页面校验
会失效，之后每次 exec 都 SIGKILL。而**老进程还在用老代码跑**，所以现象特别迷惑。

**修**：见[运维的「千万不要 `cp` 覆盖正在用的二进制」](operations.md#千万不要-cp-覆盖正在用的二进制)。已经中招的话重新按那个办法装一遍就行。

### `zsh:1: command not found: ccnm`（在 ssh 命令里）

`ssh host 'ccnm ...'` 起的是非交互 shell，读不到你 `.zshrc` 里加的 `~/.local/bin`。
**ssh 里写全路径**：`ssh host '~/.local/bin/ccnm ...'`。

ccnm 自己调对面时一直是全路径（`nodes.<x>.ccnm_bin`，默认 `~/.local/bin/ccnm`），所以
`ccnm doctor` 能通而你手敲的那条不通，是正常的，不是配置坏了。

### 在Agent Node上 `ccnm <ws>` 报 `/xxx/ccnm not found on <home> (the login shell exited 127)`

Runtime Node的 ccnm 不在 `~/.local/bin/ccnm`，而Agent Node这份 config 没说它在哪。补一行：

```toml
[nodes.runtime]
ssh = "xdwmbp"
ccnm_bin = "/opt/homebrew/bin/ccnm"     # Runtime Node上的实际路径
```

`ccnm init --runtime <alias>` 只写别名，因为绝大多数情况默认路径就是对的。**报错里的那个路径
就是它试过的那个**——如果它跟你在Runtime Node上 `which ccnm` 的结果不一样，那这行就是要补的。

### `zsh: permission denied: ccnm`

二进制在那儿但没有执行位。几乎总是 `scp` 传的时候丢的（OpenSSH 10.3 的 scp 不带 `-p`
不保留 mode）：

```bash
ssh other 'ls -l ~/.local/bin/ccnm'      # 看是不是 -rw-r--r--
ssh other 'chmod +x ~/.local/bin/ccnm'
```

ccnm 自己撞上这个会直接说出来：

```text
Work SSH   FAIL   CCNM_E_VERSION: ~/.local/bin/ccnm on work is there but not executable (exit 126)
                  ssh work 'chmod +x ~/.local/bin/ccnm'
                  this is what copying it over with `scp` and no -p leaves behind
```

### `message is not valid for protocol 1; ccnm versions probably differ`

如果你看到的是这句、而两台机器的 `ccnm --version` 明明一样——那不是版本问题。

**背景**：有的 SSH 传输不传递远程命令的退出码。实测 Tailscale SSH（tailscaled 1.102.2，
`RunSSH = true`）：`ssh work 'exit 3'` 返回 **0**，`ssh work false` 也返回 **0**，
换成 OpenSSH 服务的机器返回 3 和 1。ccnm 靠退出码分辨"命令没找到 / 不可执行 / 远程拒绝"，
在这种链路上全部退化成"成功但没输出"，于是报成版本不一致。

**现在不会了**：stdout 为空时 ccnm 改看 stderr，shell 的抱怨和远程 ccnm 自己的
`CCNM_E_*` 都能认出来。要是你还看到这句，那才是真的版本对不上——
`ssh work '~/.local/bin/ccnm --version'` 跟本机比一下。

顺带：这个特性也意味着**你自己在命令行上 `ssh work '任何会失败的命令'` 都会得到 `$? = 0`**，
调试的时候别信那个退出码。

### 会话里工具全废，报 "xxx is not installed"、`workspace_info` 却一切正常

**项目被挪走了，而会话还绑在老路径上。** 一个会话的 root 在启动的那一刻就定死在它的 MCP
payload 里，之后改 config 也好、`mv` 目录也好，都动不了它。

现在不会这么难认了：`workspace_info` 会多一行 WARNING 说根目录不在了，`exec_command`
也不再把这个错怪到程序头上（以前它会说 "`/bin/echo` 没装"，因为 spawn 失败的 errno 一模一样）。

**修**：把 config 里的路径改对，然后

```bash
ccnm workspace add xshun ~/新路径     # 或者手动改 root
ccnm xshun                            # 它会自己发现老会话指向别处，结束它、开一个新的
```

`ccnm run` 遇到"活着但 root 对不上"的会话会**直接换掉它**，并在输出里说明换掉了哪一个。

### `Work controller ... Background`

controller 不在登录会话里。两种可能：

```text
它是手工起的，不是 launchd 起的       → ssh work 'ccnm controller install'
Agent Node屏幕前根本没人登录过            → 去那台机器上登录一次（之后锁屏无所谓）
```

### `Claude authentication` 是 SKIP 不是 FAIL

没有 controller 的时候 ccnm **不会**去问 Claude 登录状态——从 ssh 会话问必然得到
"没登录"，那是假的。所以它报 SKIP 并指向 `Work controller` 那一行。先把 controller 弄好。

### `CCNM_E_DEPENDENCY: tmux is not installed`

Agent Node没装 tmux。`brew install tmux`。或者用 `--print` 模式，那个不需要 tmux。

### `Project instructions ... WARN`

项目的 `CLAUDE.md` 放不进握手，模型只读到前面一截。上限是 Claude Code 定的：整段 instructions 只保留
**2048 个 UTF-16 码元**（中文一个字算 1、英文一个字母算 1，不是按字节）。ccnm 自己的说明约占 700，
其他说明文件清单最多再占 768，剩下的才给根文件：没有清单时根文件大约能放 1350 个字符，清单很长时只剩 600 左右。

把模型用不上的东西挪出根文件，或者拆进 `.claude/rules/`（那里的文件只列路径、不占正文）。模型随时可以
`read_file CLAUDE.md` 读全文，开场读到的那一行 `[project instructions: …, first N shown; read_file CLAUDE.md for the rest]`
就是在告诉它这件事。

### `--print` 跑到一半 ssh 断了

会话没断（它是 supervisor 的孩子，不是那条 ssh 的），结果照样写进了会话目录。捞：

```bash
ccnm result xshun                 # 最近一次 --print 的结果
ccnm result xshun --session <id>  # 指定某一次
```

**两台机器上都能敲**。会话目录在Agent Node上，所以在Agent Node上敲它读的是本地文件，链路彻底
不通的时候也能捞——而那恰好是最想看看那次跑出了什么的时候。

### 会话里的 `apply_patch` 报 "workspace is PARTIALLY CHANGED"

这句只在**极端情况**下出现：staging 全部成功了，commit 阶段文件系统开始失败，回滚也失败。
它会点名每个牵涉到的文件。这时候先 `git status` 看一眼再动别的。

正常的失败（版本过期、`old` 匹配不上、路径出界）都是原子的，**一个字节都不会写**。

### 会话里的 `apply_patch` 报 "a previous apply_patch was interrupted"

```text
a previous apply_patch was interrupted while it was renaming files,
so these may not agree with each other:
  update src/config.rs   original kept at src/.ccnm-a1b2c3-config.rs
  update src/main.rs
check them before changing anything else -- git status and git diff will show which ones landed.
```

**上一次 patch 在改名的中途整个进程没了**（`kill -9`、ssh 断开、断电）。这是三阶段事务里
`Drop` 唯一盖不住的洞：回滚代码跑在那个进程里，进程没了就没人回滚。

**每个文件本身都是完整的**（一次原子 rename，不存在半截文件），坏的是文件**之间**对不上——
比如改了函数名，没改调用它的地方。

怎么处理：

```bash
git -C <项目> status        # 哪些落了、哪些没落，一眼就看出来
git -C <项目> diff
```

看完，按报错最后一行说的把那个 journal 文件删掉，patch 就恢复正常。

**ccnm 不会自动回滚**，这是故意的：等你看到这条消息时，那半个改动可能已经是你想要的，
甚至已经 commit 了。为了一个一小时前的事务把你的活默默还原，比中断本身更糟。
要求是"不能悄悄地乱"，不是"让机器替你决定"。

---

### Claude Code 说 `Connection closed`，再没别的话了

**症状**：给 Claude Code 配了 `ccnm mcp bridge`，`/mcp` 里那个 server 是红的，全部信息只有一句：

```text
ccnm-xxx (CONNECTION_CLOSED): "Connection closed"
```

**其实是**：bridge 起不来，理由写在它的 stderr 上——但 Host 把子进程的 stderr 丢掉了。ccnm 这边按契约输出了 `CCNM_E_*` 和一句人话解释，只是**一个字都没到你面前**。2026-09-11 真机实测，Claude Code 2.1.268 就是这个行为；这是 Host 怎么处理 stderr 的问题，ccnm 单方面改不掉。

**修**：把同一条命令手工跑一遍，理由就出来了：

```bash
ccnm mcp bridge <workspace> --node <node> --mode read < /dev/null
```

最常见的是那个 workspace 根本没开放给外部 MCP（`workspace X is not available to external MCP`，退出码 33）——`external_mcp` 默认就是 `disabled`，要在 **Runtime 的**配置里给它写 `read` 或 `coding`。

第二常见的是 Runtime 那个账号家里就有 Agent 的登录（`does not hold the Agent boundary`，同样是 33）。**只读链也要过这道闸**，一行开关就能签，见下一节。

**别拿退出码当判据，先看 stderr 第一行。** bridge 是 `exec` 成那条 ssh 的，远端的退出码要靠 SSH 的 exit-status 带回来；**服务端不发，你就只能看到 0**。2026-09-16 在 Tailscale SSH 上实测：远端 `mcp-serve` 自己退 33，`ccnm mcp bridge` 退 0，连 `ssh -T <host> "exit 33"` 都退 0。所以上面这条命令**退 0 不代表起来了**——看它有没有在 stderr 上打 `CCNM_E_*`，以及有没有真的回答 `initialize`。这是 SSH 服务端的属性，ccnm 改不了。

### 一台机器就能跑吗：`No Claude credential` 把整个会话挡在门外

**症状**：项目和 Claude Code 在同一台机器、同一个账号下，`ccnm doctor` 一片红，MCP 握手根本起不来：

```text
No Claude credential    FAIL  the Runtime identity can access a known Agent credential file or container
exec_command            FAIL  refused until the runtime account is confined
Remote MCP handshake    FAIL  CCNM_E_POLICY: MCP initialize failed over `…`: connection closed: initialize response
```

**其实是**：跑项目命令的那个账号，家里有 `~/.claude` / `~/.codex`。ccnm 存在的理由就是把这两件事分开，所以它在 `initialize` 之前就拒了。**`allow_unconfined_exec` 救不了**，那个开关只接受 confinement 风险。

**两条路，选一条：**

1. **正路**：在 Runtime 上建一个专用低权限账号（`ccrun`），把项目目录按 ACL 授权给它，Agent 的 SSH 落到那个账号上。见[生产安全](production-safety.md)。代价是 Agent 建出来的文件属主是那个账号。
2. **明确接受**：在 **Runtime 侧**那个 workspace 上写这一个开关：

   ```toml
   allow_unisolated_credentials = true
   ```

   **先读一遍你接受了什么**：模型跑的每一条命令都能读到那份登录，而让它跑一条命令只需要一句 prompt——包括从它被要求读的文件里冒出来的那一句。第一次用它起会话时终端上会把这段讲一遍（只讲一次），`doctor` 里那几行会变成 WARN 并注明是接受的，**不会变 OK**。完整代价见[生产安全](production-safety.md#凭据隔离那一条怎么放开代价是什么)。

**只有想让模型跑命令时才加第二个开关。** `allow_unisolated_credentials` 让会话起得来；`exec_command` 还要求这个账号本身是受限的，那一条由 `allow_unconfined_exec` 单独接受：

```toml
allow_unconfined_exec = true      # 只为 exec_command，跟上面那条无关
```

顺序别搞反：**先只写凭据那一条，看会话起不起得来。** 起得来就说明你不需要第二条——最典型的是 `ccnm mcp bridge --mode read`，它一共只有 `workspace_info` / `read_file` / `list_files` / `search_text` 四个工具，根本没有 `exec_command` 可跑。为了开一条只读链去签一个名字叫"允许不受限执行命令"的开关，是接受了比实际需要大得多的东西，而且它一个 finding 都不豁免，握手照样失败。

**这两条放不开**，写什么开关都一样：执行身份未知（identity 探针答不出来），以及认证环境是继承来的（`ANTHROPIC_*` / `CLAUDE_*` 出现在 Runtime 的服务环境里）。后者的修法只是别 export 它。

**看消息里列了哪几行。** 会话被拒时，错误里**只列真正挡住它的那几行**——通常就是 `No Claude credential` 一条。`ccnm doctor` 里同时红着的 `Not an admin`、`No SSH keys` 是真的，但它们拦的是 `exec_command`，不是这次握手；去修它们不会让握手过。

### `exec_command is refused`，理由说有 SSH 私钥，可你明明一把都没有

**症状**：外部 MCP 或受管会话里 `exec_command` 被拒：

```text
CCNM_E_POLICY: the runtime is running as ccrun and is not confined, so exec_command is refused:
  - No SSH keys: a possible private SSH key is accessible or unknown (names and contents withheld)
```

去 `~/.ssh` 翻一遍，只有 `authorized_keys`、`config`、`known_hosts`，没有任何私钥。

**其实是**：凭据审计查的是**两个**目录——`~/.ssh` 和 `~/.config/ccnm`。后者里除了 `*.toml` 和 `*.pub`，任何文件都算"排除不掉的凭据候选"。所以 `config.toml.bak`、`config.toml.pre-升级`、编辑器留下的 `config.toml~`，都会让这一行变红，而消息说的是 SSH key。

这是**故意 fail-closed**：一个叫 `config.toml.pre-x` 的文件确实可能是私钥，检查不去读内容判断。代价就是消息把人指错地方。

**修**：把备份挪出 `~/.config/ccnm/`，放家目录根下或别处都行。

```bash
mv ~/.config/ccnm/config.toml.bak ~/config.toml.bak
```

**不要**为了这个去开 `allow_unconfined_exec = true`——那是把这个账号的整套 confinement 判定都接受下来，为了一个备份文件不值得。

### 开了 `bypassPermissions`，`exec_command` 还是每次都问

**症状**：权限模式确实是 bypass（状态栏写着 `⏵⏵ bypass permissions on`），`workspace_info`、`read_file` 这些也确实不问了，但每次 `exec_command` 都还是弹：

```text
 Tool use
   ccnm — Exec Command Tool: (MCP)
 Do you want to proceed?
 ❯ 1. Yes
   2. No
```

**这是故意的，不是没配对。** ccnm 给 `exec_command` 挂了一个 `_meta` 键 `anthropic/requiresUserInteraction`，Claude Code **在任何权限模式下都认它，`bypassPermissions` 也不例外**——这正是它值得挂的理由：一个用户能关掉的闸门不叫闸门。

只有 `exec_command` 带这个键。另外六个工具被路径策略框在 workspace 根目录里，而这一个是别人机器上的一个 shell，以 Runtime 那个账号的全部权限在跑。给只读工具也挂上只会制造提示疲劳。

**两条路，先想想哪条是你要的**：

1. **`--print`**——那条路上**不带**这个键（那是"一句问一个答、终端前没人"的模式，挂上只会让模型答"我没处可问"然后拒绝执行）。大部分"它老问我"其实是这种场景：你想让它做一件明确的事，不需要一个常驻会话。

   ```bash
   ccnm my-project --print "跑一遍 cargo test，把失败的贴给我"
   ```

2. **`allow_unattended_exec`**——真的要常驻会话又不想被问，在 **Runtime 侧**那个 workspace 上写：

   ```toml
   allow_unattended_exec = true
   ```

   只对之后新起的会话生效。它**不授权任何东西**：命令能做什么完全没变，变的只是中间还有没有人。开了之后 `ccnm doctor` 里 `Command approval` 那行永远是 WARN，第一次用它起会话时终端上会把代价讲一次。

两条路的对照见[使用说明](usage.md#不想被打断先想想---print)。`ccnm mcp bridge` 无论如何都不带这个键——bridge 不知道 Host 那头有没有人，冒充知道比不说更糟。

**开之前想一下**：如果这个 workspace 已经写了 `allow_unconfined_exec`、`allow_unisolated_credentials`，权限模式又是 `bypassPermissions`，那这个弹窗就是**最后一个还有人在场的环节**了。

### 自己的 settings.json 里写了 `bypassPermissions`，ccnm 会话里还是一个个问

**症状**：Agent Node 的 `~/.claude/settings.json` 里明明有

```json
{ "permissions": { "defaultMode": "bypassPermissions" } }
```

但 ccnm 起的那个会话每调一次 ccnm 工具都要你按一次确认。

**其实是**：ccnm 是用命令行参数起官方 CLI 的，`--permission-mode acceptEdits`（默认值）——**命令行赢设置文件**。你改 `~/.claude` 改不动它。

**修**：改 **Runtime 侧**那个 workspace（workspace 定义在哪台机器上就改哪台）：

```toml
[workspaces.my-project]
claude_permission_mode = "bypassPermissions"
```

**只对之后新起的会话生效**。正在跑的那个不会变，要 `ccnm stop <ws>` 再起一次。

开之前看一眼[配置说明](configuration.md#claude_permission_mode)里那段代价——尤其是这个 workspace 还开着 `allow_unisolated_credentials` 的时候。

### 在受管会话里按了 Claude Code 的"后台"，工具全没了

**症状**：会话一直好好的，某一刻屏幕上出现 `Backgrounding after the current tool finishes…`，紧接着每个工具都报：

```text
Error: No such tool available: mcp__ccnm__read_file. Its MCP server 'ccnm' has disconnected.
```

而且**连 `Read`/`Bash` 都没有**——受管会话本来就是用 `claude --tools ''` 起的，项目文件只能走 Runtime，MCP 一没就什么都不剩。

**其实是**：Claude Code 的"后台"会把会话 **fork 成第二个进程**，那个进程照抄 ccnm 写的 `mcp.json`，于是**又去 Runtime 起了一个 MCP server**。同一棵工作树只允许一个写者，Runtime 当场拒了：

```text
CCNM_E_POLICY:
workspace write guard is busy; another session still owns this working tree
who holds it, on the Runtime Node: the `held <session> <workspace>` file in
${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/
`ccnm status` alone does not prove nobody is using it: a --print run holds
this guard and never appears there
```

server 退出，Claude Code 对这种情况只显示 `CONNECTION_CLOSED: Connection closed`，**不显示 server 的 stderr**，所以上面这几行一个字都不会到你面前。要看见它们，在 Runtime Node 上跑 `ccnm doctor <workspace>`——那条 `Remote MCP handshake` 会把 server 的 stderr 原样带出来。

不是链路断了，也不是闲置超时：原会话的那条 SSH MCP 连接**一直好好的**，fork 出来那个从来就没连上过。

**修**：原会话还在，回去就行。

```bash
ccnm attach <workspace>
```

后台那个分身直接关掉——只要原会话还握着锁，它起一个被拒一个。

**别做的事**：不要为了让分身能跑去删锁。那把锁挡住的正是"两个 Claude 同时改同一棵树"。

**怎么不再踩**：受管会话里别用后台，要离开就 detach（状态栏右下角写着按键，默认 `C-b d`），回来用 `ccnm attach`。从 v0.4.0 起，每次 attach 时状态栏会把这句提示一遍。

### 后台命令跑着跑着就没了

**症状**：`exec_command` 加 `run_in_background` 起了一个 dev server 或者很长的构建，过一阵去 `read_output`，看到的是

```text
[stopped when its session ended, after 137.0 s]
```

或者干脆

```text
CCNM_E_INVALID_ARGS: no output kept for r-0193f2c8a1b74e05
```

命令本身没报错，日志也没写完。

**其实是**：**后台命令活不过连接**（[协议第 6 节](protocol/remote-workspace-mcp-v1.md#6-连接生命周期)）。连接一断，Runtime 就停掉这条连接起的所有命令，再放写入互斥——这是有意的，否则一个没人管的进程会一直占着那棵树的写权，谁也接不上。所以要查的不是 Runtime，是**谁把连接断掉了**。按"最容易中"的顺序：

1. **中间层的单次调用预算。**不是直连 ccnm，而是过了一层 hub 的时候，它一般给每次远端调用一个预算，超时就丢掉这条连接（gld 现在是 60 秒）。触发它的往往是一条跑长的**前台** `exec_command`——出事的是前台那条，陪葬的是同一个会话里所有后台命令。
2. **中间层的空闲回收。**hub 还会回收一段时间没人用的连接（gld 的 coding 会话现在是 2 分钟）。判据通常是"上一次调用返回到现在多久"，**在跑的后台命令不算在用**：模型起完任务就去干别的，两分钟后连接就可能被收走。
3. **Host 那边断了。**Claude Code 里 `/mcp` 重连、关掉会话、SSH 掉线，都是连接结束。

**修**：

- 长命令一律 `run_in_background`，然后用 `read_output` 分次看。**别靠把 `wait_ms` 调大来扛**——一次长等待正是触发第 1 条的做法。
- 起了后台任务就别让这条会话静默太久：隔一会儿 `read_output` 一次，既看到进度，也把空闲计时清零。
- **别用 `nohup` / `setsid` 把进程从进程组里摘出去。**那样 Runtime 停不掉它：写入互斥放掉之后它还在改文件，另一个会话进来就是两个人改同一棵树，比任务被杀糟得多。真要长活的服务，交给 Runtime 上的 systemd / launchd / tmux，ccnm 只负责起它。

**怎么不再踩**：状态行就是答案，先读它。`stopped when its session ended` 是连接断了；`killed on its timeout` 是你给的 `timeout_ms` 到了；`stopped by stop_command` 是有人显式停的；`no longer running, and its exit status is unknown` 是跑它的 server 被强杀——那种情况它起的进程组**可能还在**，得上 Runtime 自己看。

### 合上笔记本睡一觉，第二天某个项目的工具连不上

**症状**：同时开着几个项目，其他都好，唯独一个 `ccnm <workspace>` 起来之后 Claude 里 MCP 显示连接失败，`ccnm status` 那一行是 `TOOLS DOWN`。

**其实是**：那个项目**昨天那个会话**的 Runtime 端 `mcp-serve` 还活着，占着写锁，新会话被拒成 busy。它没退，是因为连接成了半开：Runtime（笔记本）睡着时，Agent 那头的 ssh 等不到回应就关了，关闭的包在睡眠中丢了；醒来后 Runtime 的 sshd 还以为连接在（`lsof` 显示 `ESTABLISHED`），而 `mcp-serve` 没人调用就从不往外写，也就永远发现不了。**不是 ccnm 不支持多个项目**——每个项目一把锁，互不影响。

在 Runtime Node 上确认：

```bash
ccnm status                 # 不带项目名：会把"Agent 那头已经没有的会话"标成孤儿
```

**修**：v0.6.0 之后的 `mcp-serve` 空闲时每 30 秒 ping 一次客户端，半开的连接一写就断，锁自己释放。所以等半分钟，在 Claude 里 `/mcp` → `ccnm` → `Reconnect`。

还在跑 v0.6.0 或更早的 Runtime：没有这个 ping，只能人工结束。**别直接 kill `mcp-serve`**——那会留下 `held` 标记，还得再做一遍[写入 guard 残留](operations.md#写入-guard-残留)。结束它背后那个 sshd 会话，`mcp-serve` 读到 EOF 会正常收尾、锁变 `released`：

```bash
ps -o pid,ppid,lstart,command -p <mcp-serve 的 pid>   # PPID 那列是 sshd-session
kill <那个 sshd-session 的 pid>
```

动手前先确认 Agent Node 上那个会话确实结束了（会话目录里有 `exit` 文件，没有对应的 `ccnm internal supervise` 进程）。

### 开盖之后命令行不停打印 `^[[<35;41;12M` 这类字符

**症状**：`ccnm <workspace>` 接着会话时合了盖，开盖后过半分钟左右 ssh 断开、回到本机 shell，接着**鼠标一动就冒出一串坐标字符**。Ghostty 的 quick terminal 收起再打开还在冒，看着像窗口坏了。

**其实是**：受管会话的 tmux 开着 `mouse on`，它会让你的终端打开"鼠标上报"。正常 detach 时 tmux 会发指令把它关掉；连接被合盖掐断时那条指令发不出来，终端就一直把鼠标事件当输入送给 shell。跟 Ghostty 官方讨论里 SSH/tmux/vim 异常退出留下的是同一个问题。

**修**：ccnm 在 attach 的 ssh 返回后，会把 tmux 正常退出时发的那组"关闭"指令补发一遍（鼠标、括号粘贴、焦点事件、键盘模式、光标）。连接断在 30 秒以上的会话里时，也一起退出 tmux 留下的备用屏；30 秒内就断的不动屏幕，因为那种多半是根本没连上，贸然退出备用屏会把光标拉回旧位置。

还在用旧版本、或者不是经 ccnm 进的 tmux：终端里跑 `reset`，或者用 Ghostty 的 `reset` 快捷键动作。

### MCP 初始化报 `workspace write guard is busy` 或 `unknown`

如果 busy 是**你自己那个会话**的分身造成的，看上一条。其余情况：busy 表示仍有 writer 持锁——**受管入口和外部 MCP 共用同一把锁**，所以持锁的可能是任一侧；unknown 表示异常退出或 marker 不完整，不能证明旧执行者已经结束。不要循环删锁或按时间强制接管。

先在 Agent Node 用 `ccnm status <workspace> --agent <instance-id> --session <ccnm-session-id>` 定位会话，再由 Runtime 操作者确认旧 MCP 和子进程。完整人工恢复边界见[支持矩阵](support-matrix.md#runtime-单写-guard)。`doctor`/MCP probe 同样经过写 guard，活动 writer 下诊断被拒绝不等于 SSH 损坏。

### doctor 报 `ssh <别名>: connect to host ... port 22: Operation timed out`

**症状**：`Runtime 安全`、`远端 MCP 握手` 两行失败，错误是 TCP 层超时；同一次 doctor 里
`反向 SSH` 那行却正常。看着像配置写错了别名，其实多半是**那一刻连不上**——Tailscale 刚
重连、对面刚从睡眠醒、或者网络切换。

**别急着改配置。** 先在 **Agent Node** 上按这个顺序查（三条都不需要 ccnm）：

```bash
ssh -G <别名> | grep -E '^(hostname|user|port) '   # 别名解析成什么
ping -c1 <别名>                                    # 名字解析得到哪个地址
nc -z -w 8 <别名> 22                               # 22 端口这一刻通不通
ssh <用户>@<别名> '~/.local/bin/ccnm --version'     # 真连一次
```

**Runtime 上 `lsof -iTCP:22 -sTCP:LISTEN` 是空的，不代表 22 不通。** Tailscale SSH 接管时
本机不跑 sshd，端口由 Tailscale 应答，`nc -z` 照样通。反过来也成立：本机 sshd 开着，
Tailscale 掉线时对面照样连不上。2026-09-16 真机上就是这样：先是超时，几分钟后同一个别名
`nc` 通、`ssh` 也通，配置一个字没改。

配置真写错的样子不一样：`ssh -G` 里 `hostname` 是个解析不了的名字，`ping` 直接报
`cannot resolve`，而且**每次都失败**，不会自己好。
