# 出错了怎么办

每一条都是真撞过的：先写**你看到的现象**，再写它其实是什么、怎么办。
README 里有一张按症状索引的表，指到这里。

### `TOOLS DOWN` —— 会话看着在跑，模型却什么都够不着

```text
ccnm-xshun  xshun  detached  TOOLS DOWN (in Claude: /mcp -> ccnm -> Reconnect)  (...)
```

**症状**：Claude 还能聊天，但一让它读文件就开始瞎猜，或者去调它自己机器上的 Bash
（会被拒，那是第二道锁）。原因是那条 MCP ssh 断了——网断太久、工作机睡了、有人 kill 了它。
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

ccnm 自己调对面时一直是全路径（`hosts.<x>.ccnm_bin`，默认 `~/.local/bin/ccnm`），所以
`ccnm doctor` 能通而你手敲的那条不通，是正常的，不是配置坏了。

### 在工作机上 `ccnm <ws>` 报 `/xxx/ccnm not found on <home> (the login shell exited 127)`

家庭机的 ccnm 不在 `~/.local/bin/ccnm`，而工作机这份 config 没说它在哪。补一行：

```toml
[hosts.home]
ssh_from_work = "xdwmbp"
ccnm_bin = "/opt/homebrew/bin/ccnm"     # 家庭机上的实际路径
```

`ccnm init --home <alias>` 只写别名，因为绝大多数情况默认路径就是对的。**报错里的那个路径
就是它试过的那个**——如果它跟你在家庭机上 `which ccnm` 的结果不一样，那这行就是要补的。

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
它是手工起的，不是 launchd 起的       → ssh work 'ccnm work-controller install'
工作机屏幕前根本没人登录过            → 去那台机器上登录一次（之后锁屏无所谓）
```

### `Claude authentication` 是 SKIP 不是 FAIL

没有 controller 的时候 ccnm **不会**去问 Claude 登录状态——从 ssh 会话问必然得到
"没登录"，那是假的。所以它报 SKIP 并指向 `Work controller` 那一行。先把 controller 弄好。

### `CCNM_E_DEPENDENCY: tmux is not installed`

工作机没装 tmux。`brew install tmux`。或者用 `--print` 模式，那个不需要 tmux。

### `Project instructions ... WARN`

项目的 `CLAUDE.md` 比 16 KiB 大，模型只读到前面一截。把模型用不上的东西挪出根文件——
它随时可以 `read_file CLAUDE.md` 读全文，但**开场读到的**只有截断后的那部分。

### `--print` 跑到一半 ssh 断了

会话没断（它是 supervisor 的孩子，不是那条 ssh 的），结果照样写进了会话目录。捞：

```bash
ccnm result xshun                 # 最近一次 --print 的结果
ccnm result xshun --session <id>  # 指定某一次
```

**两台机器上都能敲**。会话目录在工作机上，所以在工作机上敲它读的是本地文件，链路彻底
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
