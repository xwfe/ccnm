# ccnm

**在一台机器上跑 Claude Code，改另一台机器上的项目。**

你的项目在家里那台机器上（那里有源码、有测试、有 toolchain）。你的 Claude Code 登录在
另一台机器上（那里有 OAuth，能出网到 api.anthropic.com）。ccnm 把这两件事接起来：

```bash
ccnm run xshun        # 在家庭机敲一句，工作机的 Claude 出现在你的终端里
```

模型看到的项目是家庭机上那个真实的目录——不是拷贝、不是挂载、不是快照。它读文件、搜代码、
改代码、跑测试，全都发生在家庭机上；它自己跑在工作机上，凭证也只在工作机上。

---

## 目录

- [它解决什么问题](#它解决什么问题)
- [两台机器分别是什么](#两台机器分别是什么)
- [装](#装)
- [`ccnm doctor`：怎么读那张表](#ccnm-doctor怎么读那张表)
- [日常怎么用](#日常怎么用)
- [模型能做什么、不能做什么](#模型能做什么不能做什么)
- [出错了怎么办](#出错了怎么办)
- [升级](#升级)
- [安全边界](#安全边界)
- [还没做的](#还没做的)

---

## 它解决什么问题

有两台机器，一台有项目一台有 Claude，中间隔着网络。常见的两种做法都不好：

| 做法 | 问题 |
|---|---|
| 把项目同步到工作机（SMB / rsync / 云盘） | 两份视图迟早不一致，`git status` 说谎，构建产物来回污染 |
| ssh 到家庭机直接跑 Claude | 凭证要复制到家庭机，家庭机成了 Anthropic 出口，违反最小权限 |

ccnm 的做法是**只让工具调用过网，源码一步不动**：

```text
家庭机 (项目在这)                         工作机 (Claude 在这)
┌───────────────────────┐                ┌──────────────────────────┐
│ ~/code/xshun          │                │ Claude Code + OAuth      │
│ ccnm internal         │◀── 一条持久 ───▶│ tmux 里的会话             │
│   mcp-serve           │    SSH stdio    │ ccnm work-controller     │
│ 7 个工具在这执行       │    (MCP)        │                          │
└───────────────────────┘                └──────────────────────────┘
     ▲                                              ▲
     └── 你在这敲 ccnm run ─── ssh -t ───────────────┘
```

过网的只有工具调用和结果：读一个文件回来 3 KB，不是整个仓库。

---

## 两台机器分别是什么

**这是最容易搞反的一件事**，搞反了会往错的机器上装 controller，浪费半小时：

```text
工作机 work    跑 Claude Code、持有 OAuth 凭证、能出网到 api.anthropic.com
               不需要有项目源码
               ccnm 在这里跑 work-controller 和 tmux

家庭机 home    项目源码在这、cargo/npm/pytest 在这跑
               ccnm 在这里跑 MCP runtime
               按设计它不该有 Claude 凭证，也不该是 Anthropic 出口
```

分辨方法：**跑 Claude 的是 work，放项目的是 home。** 别按机器名猜。

你人坐在哪台前面都行——`ccnm run` 是在**家庭机**上敲的（也可以先 ssh 到家庭机再敲）。

---

## 装

### 0. 需要什么

```text
两台 macOS，能互相 ssh（Tailscale / 局域网 / 跳板机都行）
工作机：Claude Code 已登录（claude auth status 说 loggedIn）、tmux
家庭机：项目、ripgrep（search_text 要用）、git（可选，有的话工具会守 .gitignore）
两台：同一个 ccnm build
```

工作机装 tmux：`brew install tmux`（只有交互式会话需要，`--print` 模式不用）。

### 1. 编译

```bash
cargo build --release        # 产物在 target/release/ccnm
```

### 2. 两台机器都放同一个二进制

```bash
# 家庭机（就在本机）
cp target/release/ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm

# 工作机
scp target/release/ccnm work:.local/bin/ccnm.new
ssh work 'mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm'
```

**为什么要先 `.new` 再 `mv`**：见[升级](#升级)。第一次装可以直接 cp，但养成习惯更省事。

### 3. 两个方向的 ssh 别名

ccnm 不管身份认证，它只用你 `~/.ssh/config` 里已经能用的别名（第 7 节）。两边都要能通：

```text
家庭机 ~/.ssh/config    Host work   →  工作机
工作机 ~/.ssh/config    Host home   →  家庭机
```

验证（两条都要不问密码就通）：

```bash
ssh work true               # 在家庭机上
ssh work 'ssh home true'    # 反向也要通，MCP 通道走的就是它
```

### 4. 配置文件（只在家庭机上）

`~/.config/ccnm/config.toml`：

```toml
version = 1

[hosts.work]
ssh = "work"                  # 家庭机 ~/.ssh/config 里指向工作机的别名

[hosts.home]
ssh_from_work = "home"        # 工作机 ~/.ssh/config 里指回家庭机的别名

[workspaces.xshun]
work_host = "work"
root = "/Users/me/code/xshun"  # 项目在家庭机上的绝对路径
```

一个 workspace 就是"一个项目 + 它在哪台机器上"。要几个写几个。

完整字段（`claude_permission_mode`、`ccnm_bin`、`claude_config_dir`、`runtime_user`、
`allow_unconfined_exec`）见设计文档第 5 节。

### 5. 在工作机上装 controller

```bash
ssh work 'ccnm work-controller install --dry-run'   # 先看要装什么
ssh work 'ccnm work-controller install'
```

它装一个 LaunchAgent，在工作机的**登录会话**里常驻。为什么非要有它：ssh 会话读不到
登录 Keychain，从 ssh 起的 Claude 会说"没登录"——而机器其实是登录着的。这是个假故障，
会让人在一台已经登录的机器上反复登录。详见设计文档第 21 节。

**工作机必须有人在它自己的屏幕前登录过**（不是 ssh 登录，是 GUI 登录）。锁屏没关系，
但没人登录过就没有登录会话，controller 装不起来。

### 6. 验一遍

```bash
ccnm doctor xshun
```

---

## `ccnm doctor`：怎么读那张表

一行一个事实，四种状态：

```text
OK     查过，没问题
WARN   查过，有值得看一眼的地方；不阻塞
SKIP   没查成：前置项失败，或这个版本还没实现。阻塞
FAIL   查过，坏了；带 CCNM_E_* 错误码和修复提示。阻塞
```

exit code：有 FAIL 就是第一个 FAIL 的码，没 FAIL 但有 SKIP 是 3（`CCNM_E_NOT_READY`），
全是 OK/WARN 才是 0。**"没查成"和"查出坏了"是两个不同的码**——脚本里能分清。

一次正常的输出大概长这样（截掉了几行安全审计）：

```text
ccnm doctor: xshun

Config                  OK     /Users/me/.config/ccnm/config.toml
Workspace config        OK     backend=mcp-ssh work_host=work (ssh work), runtime_host=home (ssh_from_work home)
Home workspace          OK     /Users/me/code/xshun
Project instructions    OK     CLAUDE.md, 552 bytes, all of it reaches the model
Home ccnm               OK     0.1.0 at /Users/me/.local/bin/ccnm
Work SSH                OK     me@work.example.ts.net
Work ccnm               OK     0.1.0 at /Users/me/.local/bin/ccnm
Work controller         OK     ccnm 0.1.0 as me, pid 52873, Aqua
Claude Code             OK     2.1.260 (/Users/me/.local/bin/claude)
Claude authentication   OK     me@example.com via claude.ai (max)
Reverse SSH             OK     home as me, ccnm 0.1.0
Remote MCP handshake    OK     initialize in 541 ms, tools/list (7 tools, 8236 B), instructions 1184 B (CLAUDE.md, 552 bytes), workspace_info x1 p50 22 ms ...
Workspace root          OK     /Users/me/code/xshun is a directory for me
Terminal session        OK     tmux 3.7c, no live session for xshun
Workspace policy        SKIP   not implemented until phase 2
Native tools disabled   SKIP   not checked: only a live session shows which tools Claude ended up with
Runtime identity        SKIP   not implemented until phase 5
Network isolation       SKIP   not implemented until phase 5

NOT READY (0 failed, 4 not checked)
```

**最后那句 `NOT READY` 是正常的**，不是坏了。那四行 SKIP 是这个版本还没实现的检查
（见[还没做的](#还没做的)），SKIP 按定义阻塞 READY——因为"没查过"不等于"没问题"。
`0 failed` 才是你要看的那个数。

几行值得单独说：

- **Work controller** 后面那个 `Aqua` 是工作机的登录会话。写着 `Background` 就说明
  controller 不在登录会话里，Claude 会读不到自己的凭证。
- **Remote MCP handshake** 是真的起了一次 MCP 会话又关掉。它顺带证明项目的
  `CLAUDE.md` 穿过 ssh 到了模型手里。
- **Terminal session** 里那句 `TOOLS DOWN` 是唯一一个"看着在跑其实废了"的状态，
  见[出错了怎么办](#出错了怎么办)。

**doctor 永远是只读的**：不装东西、不写文件、不起 ssh master、不留进程。跑一百遍结果一样。

---

## 日常怎么用

```bash
ccnm run xshun                    # 起会话 + 把当前终端接上去
ccnm run xshun "把登录那块重构一下"   # 同上，开场白直接给
ccnm run xshun --detached         # 只起，不接
ccnm attach xshun                 # 回到已经在跑的那个
ccnm status xshun                 # 工作机上还活着什么
ccnm status xshun --all           # 所有 workspace 的
ccnm stop xshun                   # 结束：Claude、终端、MCP 通道一起收

ccnm run xshun --print "跑一下测试，把失败的修了"   # 非交互，一问一答
ccnm result xshun                 # 上一次 --print 的结果（ssh 断了也能捞回来）
```

### 断开不等于结束

这是 ccnm 最重要的一条行为。Claude 跑在工作机的 tmux 里，**不是**跑在你那条 ssh 上：

```text
C-b d                     detach，Claude 接着跑
ssh 断了 / 合盖 / 网没了    一样，Claude 接着跑
ccnm attach xshun         回去，上下文全在
```

状态栏右下角会写 `ccnm · detach: C-b d`（prefix 是读你自己 `~/.tmux.conf` 的，改过就显示
你改的那个）。

结束一个会话只有两种方式：在 Claude 里退出（`/exit`），或者 `ccnm stop`。detach 不算，
关终端不算，网断了也不算。

### 第一次进一个 workspace

Claude 会问一句 "Is this a project you trust?"。它问的是工作机上
`~/.local/state/ccnm/workspaces/<name>/`——ccnm 自己建的、只放会话记录的空目录。答 yes。
每个 workspace 只问一次，`--print` 模式不问。

### 项目的 CLAUDE.md

家庭机项目根目录下的 `CLAUDE.md` 会被投影到 MCP 握手里，模型开工前就读到。

**只有根目录那一个文件**，不含 `.claude/rules/`、不含 skills。整段上限 16 KiB，超了按行截断，
并在末尾告诉模型截了多少、可以 `read_file CLAUDE.md` 读全文。`ccnm doctor` 的
`Project instructions` 行会说清楚有多少字节真的到了模型手里。

---

## 模型能做什么、不能做什么

会话里 Claude 的原生工具（Read / Edit / Write / Grep / Glob / Bash）**全部关掉**，
换成 7 个只能碰家庭机项目的工具：

```text
workspace_info   我在哪个项目、是不是 git 仓库
read_file        读文件（按行，长文件截断并告诉你从哪续）
list_files       列目录 / glob；git 仓库里 .gitignore 排除的不列
search_text      搜内容（走 rg，只回匹配行）
apply_patch      改文件：add / update / delete / move，全成功或全不动
exec_command     跑命令（argv，不是 shell；没有管道和重定向）
read_output      按字节偏移翻 exec_command 的输出
```

几条守着的规矩：

- **写只有 apply_patch 一条路。** 没有 `write_file(整个文件)`——那会悄悄吞掉你在别处的改动。
- **改文件必须带 `version`。** 那是 `read_file` 给的；文件在这中间被动过，patch 直接拒，
  不会盖掉别人的改动。
- **一次 patch 要么全成要么全不动。** 中间失败会回滚，回滚失败会大声说"工作区被改了一半"。
- **`.git` 目录读不到也写不了。** symlink 一律拒绝，路径出不了 workspace root。
- **`exec_command` 是真正的边界难点。** 它等价于一个远程 shell，ccnm 挡不住，
  挡它的是操作系统——见[安全边界](#安全边界)。

---

## 出错了怎么办

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

**修**：见[升级](#升级)。已经中招的话重新按那个办法装一遍就行。

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

### 会话里的 `apply_patch` 报 "workspace is PARTIALLY CHANGED"

这句只在**极端情况**下出现：staging 全部成功了，commit 阶段文件系统开始失败，回滚也失败。
它会点名每个牵涉到的文件。这时候先 `git status` 看一眼再动别的。

正常的失败（版本过期、`old` 匹配不上、路径出界）都是原子的，**一个字节都不会写**。

---

## 升级

两台机器必须是同一个 build（doctor 会检查）。

```bash
cargo build --release

# 家庭机
cp target/release/ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm

# 工作机
scp target/release/ccnm work:.local/bin/ccnm.new
ssh work 'mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm'
ssh work 'ccnm work-controller install'     # 让 controller 用新二进制重启
```

**必须先传成 `.new` 再 `mv`，不能直接 `cp` 覆盖。** 换 inode，不改原文件。原因见上面的
`Killed: 9`。

正在跑的会话不受影响：tmux server 自己一个进程组，controller 重启带不走它。

---

## 安全边界

**ccnm 从不读凭证。** 不读 Keychain、不读 `.credentials.json`、也不为了证明自己能读而读。
所有关于登录的说法都是 `claude auth status --json` 的原话转述。

**它也从不替你登录。** `claude auth login` 只能人在工作机屏幕前跑。

**它不碰你的 ssh 身份。** 用的是你 `~/.ssh/config` 里已经能用的别名，不加 `-i`、不加
`HostName`、不改你的 known_hosts。

**`exec_command` 是真正的风险面。** 它能跑的东西 = 家庭机上跑 ccnm 那个账号能跑的东西，
包括读私钥、发网络请求、删文件。ccnm 挡不住这个，也不假装能挡——命令解析器不是沙箱。
真正的边界是操作系统：一个专用账号，只能碰这一个项目，没 sudo、没 ssh key、没 Claude 凭证。

`ccnm doctor` 会逐条检查这些性质，不满足时 `exec_command` **直接拒**，除非你在 config 里
显式写了 `allow_unconfined_exec = true`——写了之后每一条命令的结果都会带一句
"this runtime is NOT confined"。

怎么把家庭机弄成那样：**[docs/production-safety.md](docs/production-safety.md)**。
一步一步的，ccnm 一条都不会替你做。

---

## 还没做的

```text
断线自动重连           现在要手动 /mcp Reconnect
工作机重启后恢复会话    tmux 没了就是没了；Claude 自己的 --resume 还没接进来
Git 专用工具           git_status / git_diff 现在靠 exec_command
浏览器                 属于家庭机 runtime，还没做
Linux                  controller 是 launchd LaunchAgent，只有 macOS
```

一个 `exec_command` 起的长命令（比如 dev server），会话没了之后不会被回收——它在自己的
进程组里，会一直跑。`ccnm stop` 收的是工作机那一半。

---

## 更深的东西在哪

- **设计文档**：[Terminal-native_Claude_Remote_Workspace.md](Terminal-native_Claude_Remote_Workspace.md)
  ——为什么这么设计、每个决定的实测数据、所有 invariant。想改这个项目的话从它开始读。
- **生产安全**：[docs/production-safety.md](docs/production-safety.md)
- **调研记录**：[docs/research/](docs/research/)

错误码是稳定的（`CCNM_E_*`，设计文档第 24 节），脚本可以依赖。
