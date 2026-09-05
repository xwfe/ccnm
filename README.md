# ccnm

**在一台机器上跑 Claude Code，改另一台机器上的项目。**

你的项目在家里那台机器上（那里有源码、有测试、有 toolchain）。你的 Claude Code 登录在
另一台机器上（那里有 OAuth，能出网到 api.anthropic.com）。ccnm 把这两件事接起来：

```bash
ccnm xshun            # 两台机器上都能敲，Claude 出现在你的终端里
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
- [开发、测试、发版](#开发测试发版)
- [安全边界](#安全边界)
- [还没做的](#还没做的)
- [License](#license)

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

### 你坐在哪台前面都行

命令是同一条，两边都能敲：

```bash
ccnm xshun          # 起会话并接管这个终端
ccnm attach xshun   # 接管一个已经在跑的
ccnm status xshun   # 它在干什么
ccnm stop xshun     # 结束它
```

区别只在配置：**家庭机有 workspace 列表，工作机没有**。工作机的配置只说"projects 在哪台
机器上"，遇到一个它不认识的名字就去问那台。这是故意的——workspace 的 root 定义在一个地方，
两份副本迟早对不上，而对不上的后果是会话绑在一个已经不存在的目录上。

底下真正跑的是这两条链，**`ccnm xshun` 敲在哪台就是哪条**：

```text
坐在家庭机   家庭机 ──ssh──> 工作机 ──ssh──> 家庭机
             要一个会话      起 Claude       给它项目
                             （tmux 里）     （MCP transport）

坐在工作机   工作机 ──ssh──> 家庭机 ──ssh──> 工作机 ──ssh──> 家庭机
             要一个会话      ↑ 上面那条，从头走一遍
             然后本地 tmux attach——接管只要名字，不要配置
```

**第二条多一跳，是故意的。** 工作机大可以自己拼出那个启动请求，代价是它得知道每个 workspace
的 root——那就成了第二份 workspace 列表。所以它不拼，它去家庭机上跑那条**你自己会敲的命令**，
让家庭机走它原本那条完整路径（config 解析、版本+root 握手、controller）。多的那一跳买到的是
每个 workspace 只有一处定义，以及一行都没重复的启动代码。

会话本来就起在工作机上，所以**接管、看状态、停、捞 `--print` 的结果都是本地的**——不走网络。
这点重要：最想停掉一个会话、或者最想看看它到底跑出了什么的时候，往往正是链路出问题的时候。

**开场白两边都能给**，`ccnm xshun "把登录那块重构一下"` 在哪台敲都一样。带引号、带撇号、带
换行都行：

```bash
ccnm xshun --prompt-stdin <<'EOF'      # 多行开场白，两台机器都认
把 login.rs 里的 "remember me" 那段重构一下
测试在 tests/auth.rs
EOF
```

工作机上敲的时候，这句话**不走命令行**。ccnm 发给家庭机的是一行 ssh 命令，而 ccnm 不往 ssh
命令行上送任何需要引号的东西（硬规则，见设计文档第 8 节）——一句带引号的话过去要么被拒，
要么被对面的登录 shell 拆开。所以它走同一条连接的 stdin，字节进字节出，家庭机那边用
`--prompt-stdin` 读。`--prompt-stdin` 不是内部开关，你自己 `echo` 或者用上面那个 heredoc
管进去一样用。

**`--print` 仍然只能在家庭机上给**，工作机上会直接拒绝并告诉你去哪敲：它要在项目所在的机器上跑。

---

## 装

### 0. 需要什么

两台 macOS，能互相 ssh（Tailscale / 局域网 / 跳板机都行）。**两个方向都要通**——不只是
你从家里连工作机，工作机也要能连回来，因为项目在家庭机上。

#### 家庭机（项目在这台）

| 装什么 | 必需？ | 不装会怎样 |
|---|---|---|
| **ccnm** 二进制，`~/.local/bin/ccnm` | 必需 | doctor 那行直接 FAIL 并告诉你怎么装 |
| **ripgrep**（`brew install ripgrep`） | **必需** | `search_text` 每次都报 `CCNM_E_DEPENDENCY: ripgrep is not installed on the workspace machine`。**doctor 查不出来**，只有模型第一次搜代码时才炸 |
| **git** | 强烈建议 | 不是 git 仓库也能用，但 `list_files` 就不再守 `.gitignore`（`target/`、`node_modules/` 全列出来），`workspace_info` 也没有分支信息 |
| **sshd 开着**（系统设置 → 通用 → 共享 → 远程登录） | 必需 | 工作机连不回来 |
| **项目自己的工具链**（cargo / node / python / …） | 看你要它干什么 | 模型能读能改，但 `exec_command` 跑不了测试 |

**不需要**：Claude Code、任何 Anthropic 凭证、Rust 工具链。家庭机**不该**有 Claude 凭证
——这是[安全边界](#安全边界)那一节的核心不变式。它也不用能编译 ccnm，二进制是从另一台传过去的。

#### 工作机（Claude 在这台）

| 装什么 | 必需？ | 不装会怎样 |
|---|---|---|
| **Claude Code**，已登录 | 必需 | `claude auth status --json` 要说 `loggedIn`；doctor 会转述它的原话 |
| **ccnm** 二进制，`~/.local/bin/ccnm` | 必需 | 同上 |
| **work controller**（`ccnm work-controller install`） | 必需 | 见[第 5 步](#5-在工作机上装-controller)。**某些机器**上不装就读不到 Keychain |
| **tmux**（`brew install tmux`） | 只有交互式要 | `ccnm <workspace>` 报 `CCNM_E_DEPENDENCY: tmux is not installed`；`--print` 模式不用 tmux |
| **能出网到 api.anthropic.com** | 必需 | 这台是唯一的 Anthropic 出口 |

**不需要**：项目源码、项目的工具链。工作机上根本没有那个目录。

#### 两台都要

**同一个 ccnm build。** 不是"版本号差不多"，是**同一个**——控制协议有版本号，工具没有，两个
能互相解码消息的 build 完全可能对同一个工具有不同理解。从 v0.1.0 起，`ccnm <workspace>`
**起会话之前**就会握手，对不上直接拒：

```text
CCNM_E_VERSION:
the workspace machine runs ccnm 0.1.1, this one runs 0.1.0;
install the same build on both before starting a session
```

`scripts/deploy.sh <另一台别名>` 就是干这个的。

#### 先自查一遍

装之前想确认两边环境，在**家庭机**上敲：

```bash
which rg git ssh                    # rg 必需、git 强烈建议
sudo systemsetup -getremotelogin    # 应该说 On
ssh <工作机别名> 'which claude tmux' # 工作机那两个
```

装完之后用 `ccnm doctor <workspace>` 复查——但注意**它不查 ripgrep**（见
[还没做的](#还没做的)），rg 得你自己确认。

### 1. 拿到二进制

两条路，选一条。**下载**不需要 Rust：

```bash
curl -sSLO https://github.com/xwfe/ccnm/releases/latest/download/ccnm-0.2.0-macos-universal.tar.gz
curl -sSLO https://github.com/xwfe/ccnm/releases/latest/download/ccnm-0.2.0-macos-universal.tar.gz.sha256
shasum -a 256 -c ccnm-0.2.0-macos-universal.tar.gz.sha256   # 必须打印 OK
tar -xzf ccnm-0.2.0-macos-universal.tar.gz                  # 解出来就是 ccnm，执行位在
```

是 arm64 + x86_64 的通用二进制，两台机器一台 M 系列一台 Intel 也是同一个文件。**用 `curl` 下，
别用浏览器**——浏览器下的会被 macOS 隔离，跑起来报"无法打开"，得 `xattr -d com.apple.quarantine ccnm`
解开；`curl` 不会加那个标记。

**编译**则在哪台机器有 Rust toolchain 就在哪台编（常常是工作机；家庭机不装 cargo 也完全能用）：

```bash
cargo build --release        # 产物在 target/release/ccnm
```

### 2. 两台机器都放同一个二进制

```bash
# 本机
install -m 755 target/release/ccnm ~/.local/bin/ccnm.new
mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm

# 另一台（把 other 换成你的别名）
scp -p target/release/ccnm other:.local/bin/ccnm.new
ssh other 'chmod +x ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm'
```

**装完立刻验一句，两台都要：**

```bash
~/.local/bin/ccnm --version
ssh other '~/.local/bin/ccnm --version'
```

**三个坑都在这两条命令里**，所以要验：`scp` 不带 `-p` 会丢执行位；ssh 里必须写全路径
（非交互 shell 不读 `.zshrc`）；升级时先 `.new` 再 `mv`，别 `cp` 覆盖。三条的症状和原因都在
[troubleshooting](docs/troubleshooting.md) 里。

`scripts/deploy.sh <另一台别名>` 把这一段全做了。

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

### 4. 配置（一条命令，不用手写 TOML）

在**家庭机**上：

```bash
ccnm init --work fodelf --home xdwmbp
```

两个参数分别是：`--work` 是**本机** `~/.ssh/config` 里指向工作机的别名，`--home` 是
**工作机** `~/.ssh/config` 里指回本机的别名。写进 `~/.config/ccnm/config.toml`。

想在**工作机**上也能敲命令的话，在那台上再来一条——**只给 `--home`**：

```bash
ccnm init --home xdwmbp     # 在工作机上
```

它只写"projects 在 xdwmbp 上"，**不写 workspace 列表**。之后工作机上 `ccnm xshun` /
`attach` / `status` / `stop` / `result` 都能用：遇到不认识的名字就去问 xdwmbp，会话本来就起在
工作机上，所以接管、看状态、停、捞结果全是本地的。

**家庭机上的 ccnm 不在 `~/.local/bin/ccnm` 的话**（比如你装到了 `/opt/homebrew/bin`），
在工作机这份配置里补一行，否则 `ccnm xshun` 会报 `not found ... (the login shell exited 127)`：

```toml
[hosts.home]
ssh_from_work = "xdwmbp"
ccnm_bin = "/opt/homebrew/bin/ccnm"
```

再把项目加进去——`cd` 到项目目录，然后：

```bash
cd ~/code/xshun
ccnm ws add            # 名字默认取目录名，路径默认取当前目录
ccnm ws add xshun      # 也可以自己起名字
```

`ws` 是 `workspace` 的简称，两个都行。其他几条：

```bash
ccnm ws list                    # 都有哪些，目录还在不在
ccnm ws remove xshun            # 忘掉它（会先停掉它的会话）
ccnm ws remove xshun --purge    # 再顺手删掉 ccnm 给它留的记录
```

**重名了它会问你，不会自己决定。** 两个项目目录都叫 `web` 是常事：撞上了会拒绝并给出两条
可以直接抄的命令（换个名字，或者 `--replace` 明确改指向）。反过来——**同一个目录起第二个
名字**——也会被拦，因为两个名字就是两个会话、两个 Claude 在改同一份文件。

**都可以重复跑。** 第二次 `init` 会说 "already says that"，不重写文件。手写过的注释和顺序
都保留——ccnm 在原文件上改，不是拿结构体重新生成一份。

生成出来长这样（没有 `version` 行）：

```toml
[hosts.work]
ssh = "fodelf"

[hosts.home]
ssh_from_work = "xdwmbp"

[workspaces.xshun]
work_host = "work"
root = "/Users/me/code/xshun"
```

要手写也行，字段表在设计文档第 5 节。

### 5. 在工作机上装 controller

```bash
ssh work '~/.local/bin/ccnm work-controller install --dry-run'   # 先看要装什么
ssh work '~/.local/bin/ccnm work-controller install'
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
ccnm xshun                        # 起会话 + 把当前终端接上去（run 可以省掉）
ccnm run xshun                    # 同上，完整写法
ccnm run xshun "把登录那块重构一下"   # 同上，开场白直接给
ccnm run xshun --prompt-stdin     # 开场白从 stdin 读到 EOF（多行、带引号用这个）
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

### 项目的规则：一份带进去，其余点名

项目根目录的 `CLAUDE.md` **整份**投影进 MCP 握手，模型开工前就读到（上限 16 KiB，超了按行
截断并告诉它怎么读全文）。

其余的**只点名不搬运**——子目录的 `CLAUDE.md`、`.claude/rules/*.md`、每个 skill 的
`SKILL.md`，握手里列出路径和大小，模型需要哪份自己 `read_file`：

```text
This project has further instructions in these files. They are not
included here; read the ones that apply to what you are doing:
  .claude/rules/commits.md (89 bytes)
  crates/core/CLAUDE.md (412 bytes)
```

为什么不全搬进去：**16 KiB 是所有会话共付的**，而规则多到装不下时就会被悄悄截掉一部分。
点名对任意多的文件都只花几百字节，而且一个只改后端的会话不用替前端的规则买单。真机实测：
加一个 rules 文件，握手从 1184 B 长到 1370 B，模型确实读了它并照着做。

**可执行的东西一样都不带**——hooks、MCP server 定义、plugins 全部不进来。它们会跑在
**工作机**上，也就是**放着 Anthropic 凭证的那台**。真要继承，任何一个仓库只要放个
`.claude/settings.json` 就能在凭证所在的机器上执行命令——那正是这套架构存在的理由的反面。

你自己的 Claude 配置照常生效，从工作机的 `~/.claude/` 加载——Claude 在那台跑，那才是它该
待的地方。

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
  连进程被 `kill -9` 打断都盖住了：提交前会写一份 journal，下一次 patch 撞见它就**拒绝**，
  并列出当时正在改的每个文件——撞上了怎么办见
  [docs/troubleshooting.md](docs/troubleshooting.md#会话里的-apply_patch-报-a-previous-apply_patch-was-interrupted)。
- **`old` 找不到时会告诉你差在哪。** 同一段文字只是空白/缩进不同，报错会说在第几行、
  并把文件里真正的字节打出来给你抄。**只诊断，不替你改**——在大仓库里"改成最像的那个"
  往往改到另一个函数上去，几周后才发现。
- **`.git` 目录读不到也写不了。** symlink 一律拒绝，路径出不了 workspace root。
- **`exec_command` 是真正的边界难点。** 它等价于一个远程 shell，ccnm 挡不住，
  挡它的是操作系统——见[安全边界](#安全边界)。它**每次调用都会问你**（MCP 的
  `requiresUserInteraction`），而且这个询问**在任何权限模式下都关不掉**，
  `--dangerously-skip-permissions` 也绕不过。其他 6 个不会问。

---

## 出错了怎么办

按**你看到的现象**查，每一条都是真撞过的。详情在
**[docs/troubleshooting.md](docs/troubleshooting.md)**：

| 你看到的 | 其实是 |
|---|---|
| `TOOLS DOWN`，会话在跑但模型够不着任何工具 | MCP 通道断了。`/mcp` → ccnm → Reconnect，上下文不丢 |
| `Killed: 9` / exit 137，升级完全炸 | 用 `cp` 覆盖了跑过的二进制。改用 rename |
| `command not found: ccnm`（在 ssh 命令里） | 非交互 shell 不读 `.zshrc`，写全路径 |
| 在**工作机**上 `ccnm <ws>` 报 `not found ... exited 127` | 家庭机的 ccnm 不在默认路径。工作机这份 config 里补 `[hosts.home] ccnm_bin` |
| `--prompt-stdin, but nothing arrived on stdin` | 管子是空的（`</dev/null`、heredoc 写错）。空开场白会被拒，不会当成"没开场白" |
| `permission denied: ccnm` | `scp` 丢了执行位。`scp -p` + `chmod +x` |
| `ccnm versions probably differ` | 两台的 build 不一样，或者 Tailscale SSH 吞了退出码 |
| 工具全废却说 "xxx is not installed" | 项目目录被移走了，会话还绑在旧路径上 |
| `CCNM_E_DEPENDENCY: tmux is not installed` | 工作机没装 tmux（`--print` 模式不需要） |
| `apply_patch` 说 "PARTIALLY CHANGED" | 极端情况：提交到一半失败且回滚也失败。先 `git status` |
| `apply_patch` 说 "a previous ... was interrupted" | 上次 patch 改名到一半进程没了。`git status` 看完再删那个 journal |
| doctor 里 `Claude authentication` 是 SKIP | 正常：没有 Aqua controller 时它不判断 |
| doctor 里 `Work controller ... Background` | 正常：managername 不是 Keychain 的终审 |

## 升级

两台机器必须是同一个 build。**现在 `ccnm <workspace>` 启动会话前会先握一次手**：版本对不上
直接拒（报 `CCNM_E_VERSION`，两个版本号都打出来），项目根目录不在了也直接拒并告诉你怎么
重新指向——都发生在会话建起来之前，不用等进去之后每个工具都出莫名其妙的错。`doctor` 也仍然查。

```bash
scripts/deploy.sh work xshun     # 编译、装两边、重启 controller、跑一次 doctor
```

手动的等价物：

```bash
cargo build --release
install -m 755 target/release/ccnm ~/.local/bin/ccnm.new
mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
scp -p target/release/ccnm work:.local/bin/ccnm.new
ssh work 'chmod +x ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm'
ssh work '~/.local/bin/ccnm work-controller install'   # 让 controller 用新二进制重启
```

**必须先传成 `.new` 再 `mv`，不能直接 `cp` 覆盖。** 换 inode，不改原文件。原因见上面的
`Killed: 9`。

正在跑的会话不受影响：tmux server 自己一个进程组，controller 重启带不走它。

---

## 开发、测试、发版

改 ccnm 本身（跑测试、变异测试、打包、GitHub 发版）：
**[docs/development.md](docs/development.md)**。

只是用它的话，这一节跟你无关。

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

### 功能

```text
断线自动重连           现在要手动 /mcp Reconnect。这不是 ccnm 的 bug：Claude Code
                      官方文档明写 stdio transport 不自动重连，一行修复的提案被
                      closed as not planned，整个生态同病（docs/research/ 有出处）
工作机重启后恢复会话    tmux 没了就是没了；Claude 自己的 --resume 还没接进来
Git 专用工具           git_status / git_diff 现在靠 exec_command，输出啰嗦费 token
后台长任务             exec_command 最长 10 分钟，没有"起个 dev server 然后轮询"
目录操作               apply_patch 只处理普通文件。删目录、改目录名、建空目录只能
                      走 exec_command，而那条路没有事务、没有版本检查、没有回滚
项目 skills 的脚本部分   SKILL.md 会被点名，模型能读；skill 附带的脚本不会被执行
二进制文件             content 是字符串，写不了；read_file 也拒绝二进制
浏览器                 属于家庭机 runtime，还没做
Linux                  controller 是 launchd LaunchAgent，只有 macOS
```

### doctor 查不到的

```text
ripgrep                家庭机没装 rg，doctor 全绿，等模型第一次 search_text 才炸
项目工具链              cargo / node / python 装没装、版本对不对，一律不查
原生工具是否真被禁       只有活会话能看出 Claude 最后拿到哪些工具，doctor 报 SKIP
同版本号的不同 build     版本比的是 Cargo.toml 里那个号，两个都叫 0.1.0 的 build
                       比出来就是相等。开发期正是 build 最容易漂的时候，而这个检查
                       在那时候恒为通过（下面有细节）
```

**"两台机器 build 必须一样"这句话，检查兑现不了全部。** `ccnm <workspace>` 起会话前会握手
比版本，但比的是 `CARGO_PKG_VERSION`——发版时有用，开发时没用：你在一台上 `cargo build` 改了
代码没改版本号，两边仍然都报 `0.1.0`，检查照过。

能抓到的只有一种：对面的 build **老到少一个协议字段**，那会被认出来并明说"不是同一个 build"。
其余情况只能靠纪律——**永远用 `scripts/deploy.sh` 装两边**，它从同一个二进制复制过去。

### 已知的洞

**`exec_command` 起的长命令不会被回收。** 比如一个 dev server：它在自己的进程组里，会话
结束了它还在跑。`ccnm stop` 收的是工作机那一半。

**`exec_command` 可以拿到 shell。** 工具描述说"argv 不是 shell 行"，那是**给模型的引导，
不是强制**——`["sh","-c","..."]` 照样能传。这是[明确的设计决定](#安全边界)：禁程序名是能绕
的，假的安全感比没有更糟。真正的边界是那个专用账号。同理，"`apply_patch` 是唯一的写入路径"
这句话，在 `exec_command` 可用时也不是强制的。

**canonicalize 到真正 open 之间有 TOCTOU 窗口。** 有项目目录写权限的人能在那一瞬间把某段
路径换成 symlink。要利用它得先能写那个目录——人已经在里面了。**判断是不值得为它改代码**，
记在这里是因为它真实存在。

### 没跑过的

**没在真实项目上日用过。** 目前所有验证都在一个 Python fixture 上：搜索、读、改、跑测试、
读输出、CLAUDE.md 投影、断开重连、被中断的 patch，都是真机跑通的，但那是一个小项目。

**v0.2.0 新加的东西只有测试，没上过真机。** 工作机上给开场白（`--prompt-stdin`）和工作机上的
`ccnm result` 是这个版本才有的，它们在两层测试里都验过——库层走完整圈，真二进制层对着一个
假 `ssh`——但**没有在两台真机器之间跑过一次**。这两层证明的是"发出去的是对的东西、收到的
被正确解开"，证明不了真的 ssh、真的 tmux、真的 Claude 合起来是什么样。v0.1.0 那批命令是真机
跑通过的，这批还没有。

---

## 更深的东西在哪

| 想知道 | 看哪儿 |
|---|---|
| 出了具体的错，怎么办 | [docs/troubleshooting.md](docs/troubleshooting.md) |
| 怎么改 ccnm 本身、怎么发版 | [docs/development.md](docs/development.md) |
| 怎么把家庭机弄成一个专用的受限账号 | [docs/production-safety.md](docs/production-safety.md) |
| **为什么**这么设计、每个决定背后的实测数据 | [设计文档](Terminal-native_Claude_Remote_Workspace.md) |
| 别人怎么做的、哪些数字支持这些选择 | [docs/research/](docs/research/) |

想改这个项目，从设计文档开始读——README 只讲"我该敲什么"，它讲"为什么是这样"。

错误码是稳定的（`CCNM_E_*`，设计文档第 24 节），脚本可以依赖。

---

## License

MIT，见 [LICENSE](LICENSE)。
