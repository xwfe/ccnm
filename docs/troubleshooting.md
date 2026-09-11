# 出错了怎么办

每一条都是真撞过的：先写**你看到的现象**，再写它其实是什么、怎么办。
README 里有一张按症状索引的表，指到这里。

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

### `Killed: 9` / exit 137 —— 升级完二进制就全炸

**症状**：`ccnm --version` 直接被杀，doctor 走 ssh 拿到空回复报 `CCNM_E_VERSION`，
但 `launchctl` 显示 controller 好好的。

**原因**：Apple Silicon 上直接 `cp` 覆盖一个正在跑（或跑过）的二进制，代码签名的页面校验
会失效，之后每次 exec 都 SIGKILL。而**老进程还在用老代码跑**，所以现象特别迷惑。

**修**：见 [README 的「升级」](../README.md#升级)。已经中招的话重新按那个办法装一遍就行。

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

项目的 `CLAUDE.md` 比 16 KiB 大，模型只读到前面一截。把模型用不上的东西挪出根文件——
它随时可以 `read_file CLAUDE.md` 读全文，但**开场读到的**只有截断后的那部分。

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

### 一台机器就能跑吗：`No Claude credential` 把整个会话挡在门外

**症状**：项目和 Claude Code 在同一台机器、同一个账号下，`ccnm doctor` 一片红，MCP 握手根本起不来：

```text
No Claude credential    FAIL  the Runtime identity can access a known Agent credential file or container
exec_command            FAIL  refused until the runtime account is confined
Remote MCP handshake    FAIL  CCNM_E_RUNTIME_UNREACHABLE: connection closed: initialize response
```

**其实是**：跑项目命令的那个账号，家里有 `~/.claude` / `~/.codex`。ccnm 存在的理由就是把这两件事分开，所以它在 `initialize` 之前就拒了。**`allow_unconfined_exec` 救不了**，那个开关只接受 confinement 风险。

**两条路，选一条：**

1. **正路**：在 Runtime 上建一个专用低权限账号（`ccrun`），把项目目录按 ACL 授权给它，Agent 的 SSH 落到那个账号上。见[生产安全](production-safety.md)。代价是 Agent 建出来的文件属主是那个账号。
2. **明确接受**：在 **Runtime 侧**那个 workspace 上把两个开关都写上：

   ```toml
   allow_unconfined_exec = true
   allow_unisolated_credentials = true
   ```

   **先读一遍你接受了什么**：模型跑的每一条命令都能读到那份登录，而让它跑一条命令只需要一句 prompt——包括从它被要求读的文件里冒出来的那一句。第一次用它起会话时终端上会把这段讲一遍（只讲一次），`doctor` 里那几行会变成 WARN 并注明是接受的，**不会变 OK**。完整代价见[生产安全](production-safety.md#凭据隔离那一条怎么放开代价是什么)。

**这两条放不开**，写什么开关都一样：执行身份未知（identity 探针答不出来），以及认证环境是继承来的（`ANTHROPIC_*` / `CLAUDE_*` 出现在 Runtime 的服务环境里）。后者的修法只是别 export 它。

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

### MCP 初始化报 `workspace write guard is busy` 或 `unknown`

busy 表示仍有 writer 持锁——**受管入口和外部 MCP 共用同一把锁**，所以持锁的可能是任一侧；unknown 表示异常退出或 marker 不完整，不能证明旧执行者已经结束。不要循环删锁或按时间强制接管。

先在 Agent Node 用 `ccnm status <workspace> --agent <instance-id> --session <ccnm-session-id>` 定位会话，再由 Runtime 操作者确认旧 MCP 和子进程。完整人工恢复边界见[支持矩阵](support-matrix.md#runtime-单写-guard)。`doctor`/MCP probe 同样经过写 guard，活动 writer 下诊断被拒绝不等于 SSH 损坏。
