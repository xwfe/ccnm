# 出错了怎么办

每一条都是真撞过的：先写**你看到的现象**，再写它其实是什么、怎么办。按现象找，不用按功能找；报错码 `CCNM_E_*` 两种语言下都一样，拿它搜这一页总搜得到。

## 按现象查

### doctor 的表怎么读

**行名、状态词和最后一行结论随语言变（默认中文），每行状态后面的说明一律是英文。** 本页大部分样本用中文行名；要和英文资料对上，加 `--lang en` 再跑一遍。行名对照：

| 中文 | 英文 | 中文 | 英文 |
|---|---|---|---|
| 配置文件 | Config | Runtime 执行身份 | Runtime user |
| workspace 列表 | Workspaces | root 身份 | Runs as root |
| workspace 配置 | Workspace config | sudo 权限 | No sudo |
| workspace 根目录 | Workspace root | admin 组 | Not an admin |
| Runtime 上的项目 | Runtime workspace | SSH 私钥 | No SSH keys |
| 项目指令 | Project instructions | Claude 凭据 | No Claude credential |
| Runtime 的 ccnm | Runtime ccnm | Docker socket | No Docker socket |
| Agent 的 ccnm | Agent ccnm | Anthropic 出口 | Anthropic egress |
| 连 Agent 的 SSH | Agent SSH | Runtime 安全 | Runtime safety |
| 选哪个 Agent | Agent selection | 命令审批 | Command approval |
| 反向 SSH | Reverse SSH | 网络隔离 | Network isolation |
| 远端 MCP 握手 | Remote MCP handshake | 本机工具策略 | Native tool policy |
| 终端会话 | Terminal session | 以你身份生效的文件 | Files that act as you |

`Controller`、`exec_command`、`Claude Code`、`Codex CLI`、`Claude authentication`、`Codex authentication` 这几行两种语言下同名。`No …`/`Not …` 这类英文名，中文用的是中性名词（`SSH 私钥`，不是"没有 SSH 私钥"），查得怎么样只看状态那一列。

状态词：`正常`=OK、`注意`=WARN、`不查`=NOTE、`没查`=SKIP、`失败`=FAIL。结论行 `可以用了`=READY、`还不能用`=NOT READY。

**`不查` 和 `没查` 差一个字，意思不一样。** `不查` 是 doctor 在这种配置下本来就不查的行：网络隔离（ccnm 管不着，要你自己在 Runtime 上配）、本机工具策略（只有真开着的会话才说得清）、没有 Agent 的 workspace 里那些 Agent 行、你看不进执行账号家目录时的 `Runtime 上的项目`（同一张表的 `workspace 根目录` 会由执行账号回答）。不管两台机器状态如何它都是这个结果，所以**不挡结论**，结论行会写"可以用了（N 项不查……）"。`没查` 是该查、这次没查成：对面没回答、对面构建太旧没报、前面一步失败了。这时结论是"还不能用"、退出码 3，按那一行的说明处理。

**没有 Agent、只给外部 MCP 用的 workspace**，`Runtime 安全` 和 `exec_command` 两行是 `不查`。这两行的结论属于替 AI 跑命令的执行账号，不属于敲 doctor 的人；没有 Agent 也就没有探测把它带回来。它不是没人管：外部客户端经 `ccnm mcp bridge` 连上来时，服务端以执行账号自己核对，不过就以 `CCNM_E_POLICY` 拒绝并写明原因。

**`注意` 不挡结论。** 最常见的几行：`Runtime 执行身份`、`admin 组`、`SSH 私钥`、`Claude 凭据` 是"注意"，说明 Runtime 没写 `runtime_user`，按默认的共用账号跑，这几行在告诉你模型跑的命令够得到什么；`命令审批` 是"注意"，说明开了 `allow_unattended_exec`。专用账号下 `以你身份生效的文件` 是"注意"，说明项目里有你打开就以你身份生效的配置，列出来让你打开前先看（[运维手册](operations.md#专用账号写进项目的配置你打开时按你的身份生效)）。都是提醒，不是没配好。

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

**Codex 会话**显示的是 `TOOLS DOWN (exit Codex and use official resume with its exact session ID on the Agent Node)`。Codex 里没有 `/mcp` 重连。最简单的做法是 `ccnm stop <项目>` 再 `ccnm <项目>` 开一个新会话，代价是对话上下文不带过来。要接回原对话，必须用 ccnm 那个 Codex 目录（`CODEX_HOME=~/.config/ccnm/agents/codex`）和 ccnm 起会话时的那套参数；直接敲 `codex resume <id>` 用的是你日常的 `~/.codex`，没有 ccnm 的工具，Codex 自带的 shell 还开着，命令会跑在 AI 那台上，别这么做。

### `ccnm run` 报 `CCNM_E_POLICY`，第二行却是 `MCP initialize failed over …`

```text
CCNM_E_POLICY:
MCP initialize failed over `/usr/bin/ssh … internal mcp-serve --payload …`: connection closed: initialize response
stderr: CCNM_E_POLICY:
workspace write guard is busy; another session still owns this working tree
```

**Runtime 连得上，是这个 workspace 的写锁被别的会话占着**（退出码 33）。起会话前，Agent Node 上的 ccnm 先经 ssh 跟 Runtime 做一次 MCP 握手预检；Runtime 在握手之前拒绝，第一行就是它给的理由。`MCP initialize failed over …` 那一行不是另一个错误，只是说明这个拒绝从哪条链路带回来的，看着像网络问题，其实不是。按 [MCP 初始化报 busy 或 unknown](#mcp-初始化报-workspace-write-guard-is-busy-或-unknown) 处理：看 Runtime 上 `write-guards/` 里是谁占着，见[写入 guard 残留](operations.md#写入-guard-残留)。`ccnm doctor` 的 `Remote MCP handshake` 行和 `ccnm mcp probe` 走的是同一次握手，报法一样。

真连不上（ssh 超时、拒绝连接、认证失败）报的是 `CCNM_E_RUNTIME_UNREACHABLE`（退出码 21）。

### 升级后 `ccnm` 报 `unknown field `codex_exec_server``

```text
CCNM_E_CONFIG: …: unknown field `codex_exec_server`, expected one of …
```

**配置里还写着 v0.14.0 删掉的开关。** 它管的是一条早就封存的 Codex 执行方式，v0.14.0 连同开关一起删了，写着它的配置（哪怕是 `false`）不再能解析。在报错的那份 `config.toml` 里删掉这一行即可，Codex 会话不受影响。

### 工具报 `failed to deserialize parameters: unknown field ...`

**症状**：`exec_command`、`apply_patch`、`stop_command` 或 `call_mcp_tool` 回一个 `isError`，说某个字段它不认识，后面跟着它认识的那些名字。

**其实是**：这几个会改东西的工具（连同 `files[]` 里每一项）**不接受它们没声明的字段**，拒绝发生在命令跑起来、补丁落盘之前。悄悄丢掉更糟：调用方会以为自己传的开关生效了。

**修**：照它列出的名字改。每个工具真正收什么，`tools/list` 里的 `inputSchema` 就是权威，规则见[协议第 5.6 节](protocol/remote-workspace-mcp-v1.md#56-参数怎么验有副作用的拒绝只读的说一声p44-新增)。

**只读工具不一样**：`read_file` 这些照常回答，只在结果末尾加一行 `[ignored, this tool has no such argument: …]`。看到那一行说明你以为生效的参数其实没生效，答案是按**没有它**算出来的。

**顺带**：`timeout_ms`、`preview_bytes` 超过上限是**拒绝**，不会自动截到上限。命令要跑更久就 `run_in_background`，它没有期限。`read_output` 的 `wait_ms` 超界仍然钳，但结果里会说钳了多少。

### 开了 `exec_sandbox` 之后命令报 `Operation not permitted`、`git commit` 失败、`cargo build` 下不了依赖

只出现在 workspace 写了 [`exec_sandbox = "codex"`](configuration.md#exec_sandbox) 的会话里；每条 `exec_command` 结果末尾都有一行 `[sandboxed: …]`，看到它就知道命令跑在沙箱里。**这不是坏了，是沙箱在挡**：命令只能写工作区（`.git` 除外）、`$TMPDIR` 和 `/tmp`，不能连网。被挡的命令按普通失败报（退出码非 0，stderr 里 `Operation not permitted`，Linux 上是 `Read-only file system`），不会有"不带沙箱重试"的路。

- `fatal: Unable to create '…/.git/index.lock': Operation not permitted`（Linux：`Read-only file system`）—— `git commit`、`git stash`、`git checkout` 这类要写 `.git` 的都会这样。提交由人在 Runtime 上做，或者这个 workspace 关掉开关。
- `cargo build` 报 `Couldn't resolve host` / `Operation not permitted (os error 1)` 且路径在 `~/.cargo` 下 —— 没网络，而且 HOME 下的缓存写不了。先在沙箱外（关掉开关，或直接在 Runtime 上）`cargo fetch` 一遍，warm cache 的构建和测试在沙箱里是正常的。`npm install` 同理。
- `sh: /bin/ps: Operation not permitted`（Linux：`fatal library error, lookup self`）—— 沙箱里看不到进程表。
- 会话根本起不来，报 `CCNM_E_CONFIG: nodes.<runtime>.codex_bin is not set` 或 `CCNM_E_VERSION` —— 开了开关但 Runtime 给不了沙箱（没配 Codex、版本不是 0.154.0）。补上 [`codex_bin`](configuration.md#node-的其他字段) 或关掉开关；ccnm 不会退回裸跑。
- 会话起不来，报 `CCNM_E_DEPENDENCY: the exec_command sandbox does not work on this Runtime`，后面跟着 Codex 自己的话 —— ccnm 启动时用沙箱试跑了一条空命令没成。常见三种：Linux 没装 bubblewrap（`bubblewrap is unavailable`）；执行账号建不了 user namespace（`No permissions to create new namespace`），见[运维手册](operations.md#runtime-node-的前置条件与项目工具链)；ccnm 的状态目录在 `/tmp` 下（`Refusing to create helper binaries under temporary dir` 然后 `bwrap: execvp codex-linux-sandbox: No such file or directory`）——Codex 不在临时目录里建辅助程序，把 `XDG_STATE_HOME` 挪出 `/tmp`。

实测哪些能跑、哪些被挡，见 [P33 记录](research/p33-exec-sandbox-2026-09-17.md)。

### Linux 上 `ccnm controller install` 报 `Failed to connect to bus`

**症状**：

```text
CCNM_E_INTERNAL: systemctl --user daemon-reload failed (exit Some(1)): Failed to connect to bus: No medium found
this shell cannot reach the account's systemd user manager
if one is running (linger on, or the account is logged in elsewhere), a `su` or `sudo -u` shell only lacks its address: export XDG_RUNTIME_DIR=/run/user/$(id -u) and run this again
if none is running: log in to this account over ssh, or keep one running with: sudo loginctl enable-linger $(id -un)
```

**怎么办**，分两种：

- **开了 linger，或这个账号在别处登录着**（用户实例在跑）：`su` / `sudo -u` 进来的 shell 只是缺了它的地址，`export XDG_RUNTIME_DIR=/run/user/$(id -u)` 之后再跑一次。
- **用户实例没在跑**：直接 `ssh 账号@机器` 登录进来再装；或者开 linger（`sudo loginctl enable-linger <账号>`），实例就一直在。

**原因**：Linux 上 Controller 是 systemd 用户服务，`systemctl --user` 要跟这个账号自己的 systemd 实例说话。这个实例要账号登录过（或开了 linger）才在；`su` 切过来的 shell 不算登录，也不带它的地址。

### Linux 上退出登录之后，Controller 和会话都没了

**原因**：没开 linger。systemd 默认在账号最后一次登录退出时停掉它的用户实例，连同 Controller 和 Controller 起的 tmux、会话。`ccnm controller status` 和 doctor 的 Controller 行（`注意`）会写 `linger is off for <账号>`。

**怎么办**：`sudo loginctl enable-linger <账号>`，之后 `ccnm controller install` 一次。这是改机器对这个账号的处理方式、要管理员权限，所以 ccnm 不替你开。

### 登录 Codex 报 `device code request failed with status 403 Forbidden`，或会话里模型一直连不上

**原因**：跑 AI 的这台机器出口所在的地区，OpenAI 不提供服务（Anthropic 也一样）。2026-10-07 在一台出口在中国大陆的 Linux 机器上撞到：登录接口回 `{"error":{"code":"unsupported_country_region_territory",...}}`，`api.openai.com` 和 `chatgpt.com` 直接连不上。这不是 ccnm 的问题，但跑 AI 的机器必须能访问 AI 服务，模型的每一次调用都是从这台机器发出去的。

**先确认是不是这个**（不带任何凭据，只看回什么）：

```bash
curl -sS -m 15 -o /dev/null -w "%{http_code}\n" https://api.openai.com/v1/models
```

回 `401` 是通的（只是没带凭据）；回 `403` 并带 `unsupported_country_region_territory`，或者超时，就是这个问题。

**怎么办**：给这台机器配一个能出去的代理，登录和 Controller 都要用上。

- 登录时在命令前加上代理：`HTTPS_PROXY=http://<代理> CODEX_HOME=~/.config/ccnm/agents/codex codex login --device-auth`（`codex` 要是 0.154.0 那份，见[快速开始：用 Codex](getting-started.md#7-用-codex)）。
- **Linux 上让 Controller 带上代理**：ccnm 不会去掉 `HTTPS_PROXY` 这类变量，Controller 的环境里有，它起的 Codex 就有。临时的做法（实测过）：`systemctl --user set-environment HTTPS_PROXY=http://<代理> NO_PROXY=127.0.0.1,localhost`，再 `ccnm controller install` 重启一次；用户实例重启后就没了。要长期生效，按 systemd 的常规做法写一个附加配置 `~/.config/systemd/user/dev.ccnm.controller.service.d/proxy.conf`（`[Service]` 下写 `Environment="HTTPS_PROXY=..."`），`controller install` 只重写主单元文件、不碰这个目录——这种写法这次没实测。
- macOS 上通常用系统级代理，Controller 不用另配。

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

### 在 Agent Node 上 `ccnm <ws>` 报 `/xxx/ccnm not found on <home> (the login shell exited 127)`

Runtime Node 的 ccnm 不在 `~/.local/bin/ccnm`，而 Agent Node 这份配置没说它在哪。最省事的是把它挪到 `~/.local/bin/ccnm`；不想挪就在 Agent Node 的配置里补一行：

```toml
[nodes.runtime]
ssh = "runtime-ssh-alias"
ccnm_bin = "/opt/homebrew/bin/ccnm"     # Runtime Node 上的实际路径
```

`ccnm init --runtime <alias>` 只写别名，因为绝大多数情况默认路径就是对的。**报错里的那个路径
就是它试过的那个**——如果它跟你在 Runtime Node 上 `which ccnm` 的结果不一样，那这行就是要补的。

### `zsh: permission denied: ccnm`

二进制在那儿但没有执行位。几乎总是 `scp` 传的时候丢的（OpenSSH 10.3 的 scp 不带 `-p`
不保留 mode）：

```bash
ssh other 'ls -l ~/.local/bin/ccnm'      # 看是不是 -rw-r--r--
ssh other 'chmod +x ~/.local/bin/ccnm'
```

ccnm 自己撞上这个会直接说出来：

```text
CCNM_E_VERSION: ~/.local/bin/ccnm on work is there but not executable (exit 126)
ssh work 'chmod +x ~/.local/bin/ccnm'
this is what copying it over with `scp` and no -p leaves behind
```

### `message is not valid for protocol 1; ccnm versions probably differ`

**怎么办**：两台装同一个 release（或者同一个提交编出来的两份），然后到**新的那台**上跑 doctor：旧构建的 doctor 看不出差别，见下一节。

**两台 `ccnm --version` 明明一样也会这样**，原因有两种。一是号一样、构建不一样：两次发版之间从 main 编出来的构建都叫上一个发布的号，内部协议却可能不同，doctor 会直接说，见下一节。二是 SSH 链路不传退出码。

**SSH 不传退出码**：有的 SSH 传输不传递远程命令的退出码。实测 Tailscale SSH（tailscaled 1.102.2，
`RunSSH = true`）：`ssh work 'exit 3'` 返回 **0**，`ssh work false` 也返回 **0**，
换成 OpenSSH 服务的机器返回 3 和 1。ccnm 靠退出码分辨"命令没找到 / 不可执行 / 远程拒绝"，
在这种链路上全部退化成"成功但没输出"，于是报成版本不一致。

现在 stdout 为空时 ccnm 改看 stderr，shell 的抱怨和远程 ccnm 自己的 `CCNM_E_*` 都能认出来，
所以这种链路一般不会再被误报成版本不一致；还看到这句，就按上面装同一个构建。

顺带：这个特性也意味着**你自己在命令行上 `ssh work '任何会失败的命令'` 都会得到 `$? = 0`**，
调试的时候别信那个退出码。

### doctor 报 `reports ccnm <版本> like this machine, but it is not the same build`

**症状**：两台机器 `ccnm --version` 一样，doctor 的 `Agent 的 ccnm` 或 `反向 SSH` 那一行却失败：

```text
Agent 的 ccnm           失败   CCNM_E_VERSION: the Agent Node agent reports ccnm 0.14.0 like this machine, but it is not the same build: it speaks internal protocols up to 9; this machine speaks up to 10
                               install the same build on both
```

起会话时 Agent 问 Runtime 的那次握手对不上，报的是同一个意思（`the Runtime Node reports ccnm … like this one, but it is not the same build`）。

**怎么办**：两台装同一个构建——同一个 release 的两个平台包，或者同一个提交编出来的两份。哪一边旧，看各自二进制的修改时间，或者拿 release 页上的 sha256 对本平台那一份（**两个平台的 sha256 本来就不同，别拿两台机器互相比**）。`反向 SSH` 那一行是经 Agent 转述的，先把 `Agent 的 ccnm` 那行修好。

**原因**：版本号取自 Cargo.toml，两次发版之间从 main 编出来的每个构建都叫上一个发布的号。号一样，两台 ccnm 互相说话用的那套请求格式（内部协议）可以不一样，所以握手时还会比各自认得的最高协议号。

**这一行只在新的那一端看得出来。** 旧构建的 doctor 只比版本号，所以在旧的那台上跑 doctor 全绿不算数，到新的那台上再跑一次。

### doctor 报 `the Agent Node refused this Agent before probing anything`

**症状**：`选哪个 Agent`（`Agent selection`）那一行失败，冒号后面是 Agent Node 自己给的原因；下面 Controller、Claude/Codex、反向 SSH 这些行全是"没查"：

```text
选哪个 Agent            失败   CCNM_E_AUTH: the Agent Node refused this Agent before probing anything: dedicated Agent home must be private, owned by the execution identity and free of symlinks; …
反向 SSH                没查   not checked: the Agent Node refused the selected Agent
```

**原因**：Agent Node 拒绝了这个 workspace 选的实例，什么都没往下探。冒号后面那句就是要修的东西，比如 Agent 账号的 `~/.claude` 权限是 0755（要 0700）。

**怎么办**：照冒号后面那句修，修完再跑一次 doctor。

### doctor 只说 `Agent probe identity differs from the Runtime selection`，Agent 的版本行没出现

**症状**：`选哪个 Agent` 一行失败，码是 `CCNM_E_VERSION`，下面没有 `Agent 的 ccnm` 那几行。

**多半是两台的 ccnm 版本不一样**，对面看不懂这次的请求。**怎么办**：两台装同一个版本（各跑一次 `ccnm --version` 核对），然后在新的那台再跑 doctor。两台确实是同一个构建还这样，说明 Agent 回答的是另一个实例，按上一节去 Agent Node 上跑 doctor 看原因。

### 会话里工具全废，报 "xxx is not installed"、`workspace_info` 却一切正常

**项目被挪走了，而会话还绑在老路径上。** 一个会话的 root 在启动的那一刻就定死在它的 MCP
payload 里，之后改 config 也好、`mv` 目录也好，都动不了它。

现在不会这么难认了：`workspace_info` 会多一行 WARNING 说根目录不在了，`exec_command`
也不再把这个错怪到程序头上（以前它会说 "`/bin/echo` 没装"，因为 spawn 失败的 errno 一模一样）。

**修**：把 config 里的路径改对，然后

```bash
ccnm workspace add xshun ~/新路径 --replace   # 名字已经有了，不加 --replace 会报错
ccnm xshun                                    # 它会自己发现老会话指向别处，结束它、开一个新的
```

`ccnm run` 遇到"活着但 root 对不上"的会话会**直接换掉它**，并在输出里说明换掉了哪一个。

### doctor 的 `Controller` 行失败，写着 `Background`（macOS）

```text
Controller              失败   CCNM_E_NOT_READY: … Background …
                               it answers, but not from a login session, so Claude started there could not read its own credentials
                               run on the Agent Node: ccnm controller install
```

Controller 不在图形登录会话里，从它起的 Claude 读不到钥匙串里的登录。两种可能：

```text
它是手工起的，不是 launchd 起的     → 在 Agent Node 上跑 ccnm controller install
Agent Node 屏幕前根本没人登录过     → 去那台机器上登录一次（之后锁屏无所谓）
```

### `Claude authentication` 是"没查"不是"失败"

Controller 不在登录会话里的时候，ccnm **不会**去问 Claude 登录状态：从那里问必然得到"没登录"，那是假的。所以它记"没查"，说明写着 `fix the Controller row first`。先按上一节把 Controller 弄好。

### `CCNM_E_DEPENDENCY: tmux is not installed`

Agent Node 没装 tmux：macOS 上 `brew install tmux`，Debian / Ubuntu 上 `sudo apt install tmux`。或者用 `--print` 模式，那个不需要 tmux。

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

第二常见的是 Runtime 写了 `runtime_user`、而那个账号家里就有 Agent 的登录（`does not hold the Agent boundary`，同样是 33）。**只读链也要过这道闸**，一行开关就能签，见下一节。

**别拿退出码当判据，先看 stderr 第一行。** bridge 是 `exec` 成那条 ssh 的，远端的退出码要靠 SSH 的 exit-status 带回来；**服务端不发，你就只能看到 0**。2026-09-16 在 Tailscale SSH 上实测：远端 `mcp-serve` 自己退 33，`ccnm mcp bridge` 退 0，连 `ssh -T <host> "exit 33"` 都退 0。所以上面这条命令**退 0 不代表起来了**——看它有没有在 stderr 上打 `CCNM_E_*`，以及有没有真的回答 `initialize`。这是 SSH 服务端的属性，ccnm 改不了。

### `No Claude credential` 把整个会话挡在门外

**症状**：`ccnm doctor` 一片红，MCP 握手根本起不来：

```text
No Claude credential    FAIL  the Runtime identity can access a known Agent credential file or container
exec_command            FAIL  refused until the runtime account is confined
Remote MCP handshake    FAIL  CCNM_E_POLICY: MCP initialize failed over `…`: connection closed: initialize response
```

**其实是**：跑项目命令的那个账号家里有 `~/.claude` / `~/.codex`，而 Runtime 配置里写了 `runtime_user`（专用账号模式），所以它在 `initialize` 之前就拒了。**`allow_unconfined_exec` 救不了**，那个开关只管账号权限过大那一类。不写 `runtime_user`（共用账号）时这一行只是"注意"，不拦。

**三条路，选一条：**

1. **不需要隔离**：去掉 `runtime_user`，按共用账号跑——和你在那台机器上直接用 Claude Code 一样，模型跑的命令读得到那份登录。代价见[生产安全：要不要建专用账号](production-safety.md#要不要建专用账号)。
2. **要隔离**：让 Agent 的 SSH 落到一个家里没有 Agent 登录的专用低权限账号上（比如 `ccrun`），把项目目录按 ACL 授权给它。见[生产安全](production-safety.md)。代价是 Agent 建出来的文件属主是那个账号。
3. **明确接受**：在 **Runtime 侧**那个 workspace 上写这一个开关：

   ```toml
   allow_unisolated_credentials = true
   ```

   **先读一遍你接受了什么**：模型跑的每一条命令都能读到那份登录，而让它跑一条命令只需要一句 prompt——包括从它被要求读的文件里冒出来的那一句。`doctor` 里那几行会变成"注意"并注明是接受的，**不会变成"正常"**。完整代价见[生产安全](production-safety.md#凭据隔离那一条怎么放开代价是什么)。

**要几个开关看会话类型**：只读的外部连接（`ccnm mcp bridge --mode read`）只写上面这一个就够，它根本没有 `exec_command` 可跑；要跑命令的会话（受管会话、`bridge --mode coding`）还要 `allow_unconfined_exec`，因为 `exec_command` 另外要求账号本身是受限的。为一条只读链去签"允许不受限执行命令"，是接受了比实际需要大得多的东西。

有两条写什么开关都放不开，见[生产安全](production-safety.md#凭据隔离那一条怎么放开代价是什么)。

**报的是 `known credential accessibility is unknown`（说不清能不能读到）时，先看路径**：执行账号的家目录路径上只要有一层是符号链接，凭据检查就判"说不清"，和"能读到"一样被拒。macOS 的 `/tmp` 和 `/var` 都是符号链接，把执行账号的家放在它们下面就会撞到。用真实路径（`/private/tmp/...`）就好了，先别急着开开关。

**看消息里列了哪几行。** 会话被拒时，错误里**只列真正挡住它的那几行**——通常就是 `No Claude credential` 一条。`ccnm doctor` 里同时红着的 `Not an admin`、`No SSH keys` 是真的，但它们拦的是 `exec_command`，不是这次握手；去修它们不会让握手过。

### `exec_command is refused`，理由说有 SSH 私钥，可你明明一把都没有

**症状**：Runtime 写了 `runtime_user`（专用账号模式），外部 MCP 或受管会话里 `exec_command` 被拒（没写 `runtime_user` 的共用账号里这一行只是"注意"，不拦）：

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

### 命令或改文件被拒：`was not run: a PreToolUse hook of skill … stopped it`

**症状**：会话里本来能跑的命令（或 `apply_patch`）突然报：

```text
CCNM_E_POLICY: exec_command was not run: a PreToolUse hook of skill guard stopped it: use git rm, not rm
```

**其实是**：这个会话早些时候加载过一个带 `hooks` 的 skill（模型自己调的 `load_skill`，或你敲的 `/mcp__ccnm__<名字>`），它的 `PreToolUse` 钩子在项目机器上跑了，退出码 2 或回了 `deny` / `ask`，所以这次调用没执行。冒号后面是钩子自己说的话。只有命令不问人的会话会这样（开了 `allow_unattended_exec`、`--print`、`ccnm mcp bridge` 的 coding 模式），见[使用说明](usage.md#项目自带的-skills)。

**怎么办**：

- 按钩子说的改做法，这通常就是 skill 作者的本意。
- 钩子写错了：改项目里那个 SKILL.md。**已经登记的钩子到会话结束前一直有效**，改了文件也不撤——停掉会话再起一个（`ccnm stop <项目>` 后 `ccnm <项目>`）。
- 不想让 skill 的钩子跑：去掉 `allow_unattended_exec`，交互会话里一个都不跑，代价是每条命令又要确认一次。

钩子自己失败（退出码不是 0 也不是 2，或超时）不挡调用，只在结果末尾多一行说明：`[PreToolUse hook of skill … failed (exit 1) and was ignored: …]`，超时是 `… was stopped after N s and ignored`。调用在钩子跑着时被取消，工具不执行，报 `CCNM_E_INVALID_ARGS: … was not run: the call was cancelled while its PreToolUse hooks ran`。

### 加载 skill 报 `was not loaded: a command it runs as it loads failed`

**症状**：

```text
CCNM_E_INVALID_ARGS: skill "deploy" was not loaded: a command it runs as it loads failed, as it would natively
  line 14: gh pr view
  it exited 1: …
```

**其实是**：SKILL.md 里有 `` !`命令` ``，命令不问人的会话里它在加载时于项目机器上执行，有一条失败，整次加载就失败——原生 Claude Code 也是这样。后两行是哪一行、怎么失败的。常见原因：项目机器上没装那个命令（`gh`、`node`），命令要登录或联网，或者它假设自己在别的目录（`` !`命令` `` 在 workspace 根下跑），也可能是超过 120 秒被停掉。

**怎么办**：以执行账号在项目机器的 workspace 根下跑一遍那条命令看报错；装上缺的东西，或者改 SKILL.md。要问人的会话里这些命令本来就不跑，模型只看到一份清单。

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

带这个键的是 `exec_command` 和 `call_mcp_tool`（后者会在项目机器上起程序）：它们以 Runtime 那个账号的全部权限在跑。其余工具出不了 workspace 根目录，不带；给它们也挂上只会制造提示疲劳。

**不想被问，在 Runtime 侧那个 workspace 上打开 `allow_unattended_exec`**（常用交互会话的项目建议开）：

```toml
[workspaces.my-project]
allow_unattended_exec = true
```

只对之后新起的会话生效。它**不授权任何东西**：命令能做什么完全没变，变的只是中间还有没有人。开了之后少了什么、还剩什么、doctor 为什么一直"注意"，见[配置说明](configuration.md#allow_unattended_exec)。

一次性的活也可以走 `--print`：那条路上**不带**这个键（那是"一句问一个答、终端前没人"的模式，挂上只会让模型答"我没处可问"然后拒绝执行）。两条路的对照见[使用说明](usage.md#不想一条条确认开-allow_unattended_exec)。`ccnm mcp bridge` 无论如何都不带这个键——bridge 不知道 Host 那头有没有人，冒充知道比不说更糟。

### 受管 Codex 会话：`exec_command` 每次都弹，或者一次都不弹

**每次都弹是正常行为**，样子是：

```text
• Calling ccnm.exec_command({"cmd":"cargo test --offline"})
  Allow the ccnm MCP server to run tool "exec_command"?
  cmd: cargo test --offline
  › 1. Allow   Run the tool and continue.
    2. Cancel  Cancel this tool call
```

只有"允许 / 取消"，没有"本会话都允许"，下一次照样问；取消（或按 Esc）的那次调用根本到不了 Runtime，模型收到的是 `user cancelled MCP tool call`。`call_mcp_tool` 也一样会问，其余工具不问。不想被问，和 Claude 一样两条路：`--print`，或在 Runtime 侧的 workspace 写 `allow_unattended_exec = true`（见上一节）。

**一次都不弹**，按顺序查：

1. **这个会话里切过权限。** 在 Codex 里用 `/permissions` 选了 Full Access 就不再问；选 Approve for me 是交给 Codex 自己的自动审查，真机上它连 `rm -f` 都直接放行、不问人。这是终端前那个人的选择，ccnm 拦不住，但只管这一个会话，下一个会话照样问。Claude 会话没有这个口子。
2. **workspace 开了 `allow_unattended_exec`。** 这时 doctor 的 `命令审批` 是"注意"，写着这个开关。
3. **两台的 ccnm 不是同一个构建。** 两台装同一个版本，doctor 的版本行会指出哪边不对。

实测见 [记录](research/2026-10-07-p71-codex-asks-before-exec.md)。

### Codex 里报 `timed out awaiting tools/call after 300s`，命令其实还在跑

**症状**：Codex 里一条长命令（或一次长的 `read_output` 等待）过了 5 分钟，模型看到：

```text
tool call error: tool call failed for `ccnm/exec_command`

Caused by:
    timed out awaiting tools/call after 300s
```

模型以为没跑成，往往再跑一遍；去项目机器上看，第一条还在跑。

**其实是**：300 秒是 Codex 自己等一次调用的上限（server 没配 `tool_timeout_sec` 时），不是 ccnm 的——ccnm 允许一次调用最多 10 分钟。Codex 到点只是不等了，不通知 ccnm 取消，所以第一条照跑到它自己的 `timeout_ms`，结果没人收。

**怎么办**：

- **受管 Codex 会话**：ccnm 启动 Codex 时已经自带足够长的 `tool_timeout_sec`，不会撞上。撞上了说明 Agent 上的 ccnm 太旧，两台装同一个新版本。
- **Codex 当 Host 连 `ccnm mcp bridge`**：在 Codex 的 server 配置里加 `tool_timeout_sec = 1870`，见[协议文档](protocol/remote-workspace-mcp-v1.md#codex-当-host写上-tool_timeout_sec)。
- 已经撞上了：第一条会在它自己的 `timeout_ms` 到点时被 ccnm 停掉，在那之前别让模型并行再起一份。

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

而且**连 `Read`/`Bash` 都没有**——受管会话的 `--tools` 本来就只列 workspace 开的那几个 Agent 功能（搜索、抓网页、子代理、待办，见 [`agent_tools`](configuration.md#agent_tools)），项目文件只能走 Runtime，MCP 一没就只剩这类不碰项目的功能。

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

**怎么不再踩**：受管会话里别用后台，要离开就 detach（状态栏右下角写着按键，默认 `C-b d`），回来用 `ccnm attach`。每次 attach 时状态栏会把这句提示一遍。

### 模型说命令跑过了，可是什么都没发生（`command not found`，秒回）

**症状**：模型报告"测试通过"或者"构建完成"，但你去看，产物没变、`target/` 没动过。翻它跑的命令，结果长这样：

```text
bash: cargo: command not found
```

退出码 127，**耗时 0.0 秒**。

**为什么会被当成通过**：一条真的跑起来的 `cargo test` 要几十秒并打印一大片；`command not found` 是瞬间返回、只有一行。模型（和人）扫一眼输出很容易把"没有报错信息"读成"没有错误"。

**根因通常不是 ccnm，是 Runtime 那台机器的 PATH。** 最常见的一种：PATH 里挂着 `~/.cargo/bin`，**而那个目录根本不存在**——rustup 的 shim 没装或者被清过，工具链实体在 `~/.rustup/toolchains/<toolchain>/bin`。2026-09-20 的真机轮就撞上了这个（[记录](research/real-machine-p36-p44-2026-09-20.md)第 5 节）。

**先确认是不是它**，在 Runtime Node 上：

```bash
echo $PATH | tr ':' '\n' | while read d; do [ -d "$d" ] || echo "不存在: $d"; done
```

Rust 的修法是补回 shim：

```bash
rustup default stable
```

**为什么模型那边"自己绕过去了"也不算解决**：它可以把工具链目录前置到 PATH 再跑，那一次能过，但下一个会话、下一条命令又是同样的坑。要么修 Runtime 的 PATH，要么在项目里放 `rust-toolchain.toml` 把版本钉死。

注意 `exec_command` 拿到的是**非交互 shell** 的环境，你在 `~/.zshrc` 里加的 PATH 不一定生效——放 `~/.zshenv` 或者 `~/.profile` 才稳。

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

1. **中间层的单次调用预算。**不是直连 ccnm，而是过了一层 hub（比如 gld）的时候，它一般给每次远端调用一个预算，超时就丢掉这条连接。触发它的往往是一条跑长的**前台** `exec_command`——出事的是前台那条，陪葬的是同一个会话里所有后台命令。
2. **中间层的空闲回收。**hub 还会回收一段时间没人用的连接，判据通常是"上一次调用返回到现在多久"，**在跑的后台命令不算在用**：模型起完任务就去干别的，过一会儿连接就可能被收走。具体多久看那个 hub 的文档。
3. **Host 那边断了。**Claude Code 里 `/mcp` 重连、关掉会话、SSH 掉线，都是连接结束。

**修**：

- 长命令一律 `run_in_background`，然后用 `read_output` 分次看。**别靠把 `wait_ms` 调大来扛**——一次长等待正是触发第 1 条的做法。
- 起了后台任务就别让这条会话静默太久：隔一会儿 `read_output` 一次，既看到进度，也把空闲计时清零。
- **别用 `nohup` / `setsid` 把进程从进程组里摘出去。**项目机器是 macOS 时 Runtime 停不掉它：写入互斥放掉之后它还在改文件，另一个会话进来就是两个人改同一棵树，比任务被杀糟得多。Linux 上会话结束时它会被收掉，想让它活过会话也办不到。真要长活的服务，交给 Runtime 上的 systemd / launchd / tmux，ccnm 只负责起它。

**怎么不再踩**：状态行就是答案，先读它。`stopped when its session ended` 是连接断了；`killed on its timeout` 是你给的 `timeout_ms` 到了；`stopped by stop_command` 是有人显式停的；`no longer running, and its exit status is unknown` 是跑它的 server 被强杀——那种情况它起的进程组**可能还在**，得上 Runtime 自己看。

### 合上笔记本睡一觉，第二天某个项目的工具连不上

**症状**：同时开着几个项目，其他都好，唯独一个 `ccnm <workspace>` 起来之后 Claude 里 MCP 显示连接失败，`ccnm status` 那一行是 `TOOLS DOWN`。

**其实是**：那个项目**昨天那个会话**的 Runtime 端 `mcp-serve` 还活着，占着写锁，新会话被拒成 busy。它没退，是因为连接成了半开：Runtime（笔记本）睡着时，Agent 那头的 ssh 等不到回应就关了，关闭的包在睡眠中丢了；醒来后 Runtime 的 sshd 还以为连接在（`lsof` 显示 `ESTABLISHED`），而 `mcp-serve` 没人调用就从不往外写，也就永远发现不了。**不是 ccnm 不支持多个项目**——每个项目一把锁，互不影响。

在 Runtime Node 上确认：

```bash
ccnm status                 # 不带项目名：会把"Agent 那头已经没有的会话"标成孤儿
```

**修**：`mcp-serve` 空闲时每 30 秒 ping 一次客户端，半开的连接一写就断，锁自己释放。所以等半分钟，在 Claude 里 `/mcp` → `ccnm` → `Reconnect`。过了一分钟还占着，按[运维手册：写入 guard 残留](operations.md#写入-guard-残留)手工收。

### 开盖之后命令行不停打印 `^[[<35;41;12M` 这类字符

**症状**：`ccnm <workspace>` 接着会话时合了盖，开盖后过半分钟左右 ssh 断开、回到本机 shell，接着**鼠标一动就冒出一串坐标字符**。Ghostty 的 quick terminal 收起再打开还在冒，看着像窗口坏了。

**其实是**：受管会话的 tmux 开着 `mouse on`，它会让你的终端打开"鼠标上报"。正常 detach 时 tmux 会发指令把它关掉；连接被合盖掐断时那条指令发不出来，终端就一直把鼠标事件当输入送给 shell。跟 Ghostty 官方讨论里 SSH/tmux/vim 异常退出留下的是同一个问题。

**修**：ccnm 在 attach 的 ssh 返回后，会把 tmux 正常退出时发的那组"关闭"指令补发一遍（鼠标、括号粘贴、焦点事件、键盘模式、光标）。连接断在 30 秒以上的会话里时，也一起退出 tmux 留下的备用屏；30 秒内就断的不动屏幕，因为那种多半是根本没连上，贸然退出备用屏会把光标拉回旧位置。

不是经 ccnm 进的 tmux 也这样时：终端里跑 `reset`，或者用 Ghostty 的 `reset` 快捷键动作。

### MCP 初始化报 `workspace write guard is busy` 或 `unknown`

如果 busy 是**你自己那个会话**的分身造成的，见[按了 Claude Code 的"后台"](#在受管会话里按了-claude-code-的后台工具全没了)。其余情况：busy 表示仍有 writer 持锁——**受管入口和外部 MCP 共用同一把锁**，所以持锁的可能是任一侧；unknown 表示异常退出或 marker 不完整，不能证明旧执行者已经结束。不要循环删锁或按时间强制接管。

**还有第三种，话不一样**：`workspace write guard was kept on purpose`。这不是崩溃——上一个会话结束时有命令**停不掉**（macOS 上离开了进程组又攥着管道，ccnm 的信号够不着；Linux 上这种会被收掉，杀不掉的才会这样），它明知有东西可能还在改这棵树，故意没交出写权。拒绝信息里点名还剩哪些 `output_ref`，Linux 上还有剩下的 pid。**先把那些命令收掉再谈清锁**，顺序反了就是两个写者进同一棵树；每条命令的命令行在 `sessions/<session>/output/<ref>/status` 里，步骤见[写入 guard 残留](operations.md#写入-guard-残留)。

拒绝信息里还会说上一个会话的 pid 现在是什么（还在跑，连命令行一起给；已经不在；或者被别的程序复用了）。**pid 没了不等于可以接管**——它起的命令可能还活着，而 ccnm 看不见它们。

先在 Runtime Node 上跑 `ccnm status <workspace>`，看最后那一行写锁：谁占着、那个 pid 现在是什么、Agent 那边这个会话是不是早结束了（它经 Agent 问 Runtime 执行账号，只看不动）。说"Agent 那边已经结束"的，就是[合上笔记本睡一觉](#合上笔记本睡一觉第二天某个项目的工具连不上)那种孤儿 `mcp-serve`。再在 Agent Node 用 `ccnm status <workspace> --agent <instance-id> --session <ccnm-session-id>` 定位会话，由 Runtime 那边的人确认旧 MCP 和子进程都没了，再按[写入 guard 残留](operations.md#写入-guard-残留)处理。`doctor`/MCP probe 同样经过写 guard，活动 writer 下诊断被拒绝不等于 SSH 损坏；写锁那一行不拿锁，不会被拒，也不会挡别人。

Machine API 那边看到的是同一件事的两种码：有进程正持有，`session.start` 回 `-32008`，等它结束再发；异常退出或故意留下的，回 `-32007`，`data.reason` 是 `left_held` / `kept_on_purpose` 等，按上面处理，重发不会好。

**两个会话都开起来了、都能写同一棵树**，那不是锁坏了：写锁只在一个 state 目录内有效，两边的 `XDG_STATE_HOME` 不同就是两把互不相干的锁。见[运维手册](operations.md#一棵树配两个-state-目录--两个互不知晓的写域)。

### 会话已经结束，工作区却还在被写

**先停止往这个项目派新的写任务。** `ccnm stop` 成功、会话的主进程没了，都不能说明它所有的后代都停了。

- **项目机器是 Linux**：会话结束时，命令和项目机器上的 MCP server 留下的、`setsid` 出去的后台进程会被一起收掉，收掉才交写锁；收不掉的，下一个会话报 `workspace write guard was kept on purpose`，见上一节。
- **项目机器是 macOS**：脱离了进程组的后代（`setsid`、守护进程）ccnm 够不着。还攥着命令输出的，写锁留着（上一节那种 `kept on purpose`）；连输出都放掉了的，写锁照常交出，它会在下一个会话旁边接着改文件。用 `ps -A -o pid,pgid,stat,command` 找到它（命令行里通常带着项目路径），以执行账号结束它。

在这么干的是项目机器上的某个 MCP server（`call_mcp_tool`），而你又要求可靠交接时，在 Runtime 配置里用 `[runtime_mcp] hidden` 藏掉它（或 `enabled = false` 整个停用），结束旧会话后再核实一遍。不要删写锁强行恢复，不要按模糊的名字批量 kill。

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

### Linux 上 `ccnm run` 报 `workspace root … is not a directory on this machine`，目录明明在

**先看是不是路径真写错了**（或者那里是个文件）：这句话现在只在 ccnm 确实看到"不在"时才说。

**项目放在执行账号的家目录里、你自己的账号进不去时**（Debian 12 起新账号的家目录默认 0700），ccnm 分得清"不在"和"这个账号没权限看"：

- `ccnm run` 不拦你。项目在不在由执行账号回答，真不在就报 `workspace <名字> says its root is …, and on that machine it is missing`。
- doctor 的 `Runtime 上的项目` 那一行是"不查"，并告诉你去看 `workspace 根目录` 那一行，那一行才是执行账号的回答：

  ```text
  Runtime 上的项目        不查   not checked: bing is not allowed to look at /home/ccrun/proj (Permission denied), so this account cannot say whether the project is there
                                 the account that runs the tools can: its answer is the `Workspace root` row below
  ```

- `ccnm workspace add <名字> /home/ccrun/proj` 能登记，会提示"按你写的路径登记，没有核对、没有解析符号链接"。这时**必须写绝对路径**，而且要和执行账号自己看到的写法一致（别经过符号链接）。

别为了让自己看得见，把执行账号的家目录改成 0755：那等于让机器上所有账号都能读它家里的东西。

### Machine API 的会话 `failed`，`text`、`exit_code`、输出全是空的

**症状**：`session.result` 回 `state: failed`，`outcome.exit_code` 是 `null`、`duration_ms` 是 0，stdout 和 stderr 都是 0 字节。

**原因**：Agent 进程根本没起来——Agent 上的 CLI 没登录、两端构建不一致、Agent 连不上之类。

**原因就在同一个回答里**，看 `failure`：

```json
"failure": {"code": -32003, "ccnm_code": "CCNM_E_AUTH", "detail": "Claude is not authenticated on the Agent Node"}
```

| `failure.code` | 意思 | 怎么办 |
| --- | --- | --- |
| `-32003` | Agent 上的官方 CLI 没登录 | 去 Agent Node 自己的终端登录（`claude auth login` / `codex login`） |
| `-32002` | 两台机器的 ccnm 版本或构建不同（`detail` 里是 `runs ccnm 0.8.0, this one runs 0.9.0` 或 `message is not valid for protocol 1`） | 两端装同一个构建，先跑 `ccnm doctor` |
| `-32004` / `-32005` | 连不上 Agent / Agent 连不回 Runtime | 查 ssh，见上面几节 |
| `-32007` | 被策略拒绝，比如写锁交不出来 | 看 `detail`，多半要人处理 |
| 没有 `code` | 服务端归不了类（比如还没派发就被 stop） | 看 `detail` |

程序里**按 `code` 分支**，`detail` 只给人看。状态是 `unknown` 时也可能有 `failure`——它说的是服务端为什么说不清，不是"可以重试"。Agent 起来了、自己退出的会话没有 `failure`，那种看 `outcome.exit_code` 和输出。字段定义见[协议 5.5 节](protocol/machine-protocol-v1.md#55-sessionresult)。

### `ccnm stop` 报 `terminal ended but its Runtime MCP transport is still alive`

**症状**：停交互会话，退出码 3：

```text
CCNM_E_NOT_READY:
… terminal ended but its Runtime MCP transport is still alive; state remains stopping
```

**其实是**：终端关掉了，但通往 Runtime 的那条 ssh 通道 5 秒内还没退。ccnm 不确认通道没了，就不报"停成功"。

**怎么办**：等几秒再 stop 一次，这一次会把会话记成"被停止"，时长算到第一次敲 stop 为止。再用 `ccnm status <ws>` 确认写锁那一行是"空闲"；不是的，按[写入 guard 残留](operations.md#写入-guard-残留)查。

**按项目名停（没带 `--session`）时报这一句**：报错第二行是 `to record the stop once it has ended: ccnm stop <ws> --session <完整 id>`。再停一次要照抄这一行：不带 `--session` 的话，终端已经没了，ccnm 找不回这条记录，它会一直停在"正在停"。

**`ccnm log` 里是"没有终端"（`failed to start`）**：终端不是 stop 停的，而是自己没了（tmux server 被杀、机器重启），之后才有人对它 stop。ccnm 不知道它什么时候结束的，只能这么记。

### doctor 说 Codex 已登录，会话里第一条消息却报 `refresh token was revoked`

**症状**：`ccnm doctor <ws> --agent <codex 实例>` 的 `Codex authentication` 是 OK（`logged in via ChatGPT`），会话一发消息，Codex 回：

```text
Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.
```

**原因**：doctor 和 `codex login status` 都只看本地的登录状态，不去服务器验证令牌。在别处登录同一个账号、或者长时间没用，都可能让这份令牌失效。

**doctor 自己会说这一点**，那一行仍是"正常"，但多一句：

```text
Codex authentication    OK     logged in via ChatGPT
                               local login state only, not checked with the server: a revoked or expired token still reads as logged in, and the first message of a session is what shows it
```

它没有变成真的校验。ccnm 不读登录文件的内容，而官方 CLI 里唯一问登录状态的命令不联网；`codex doctor` 也许能验，但它会不会顺手刷新（也就是改写）令牌没有量过，见 [P65 记录](research/2026-09-30-p65-hidden-root-failure-reason-codex-login.md)第 2.3 节。所以**这一行绿只说明"登录过"，不说明"现在还能用"**。

**怎么办**：在 Agent Node 自己的终端，对 **ccnm 用的那份** Codex 目录重新登录（不是你日常的 `~/.codex`），用受管会话实际用的那个 Codex 0.154.0（路径换成你机器上的）：

```bash
CODEX_HOME=~/.config/ccnm/agents/codex /path/to/codex-0.154.0/codex login
```

用更新版本的 Codex 登录这个目录，它可能顺手升级目录里的状态文件，0.154.0 之后未必读得了。

### Machine API：会话一直是 `unknown`

`unknown` 的意思是 ccnm 说不清这次运行到了哪一步，**别自动重试**：那次运行可能已经改过东西。

- 每个会话由自己的 owner 进程（`ccnm internal rpc-run --handle s-…`）带着跑，`ccnm rpc` 断开不影响它，回来能查到真实状态。还是 `unknown`，说明这个 owner 进程真的没了（被 `kill -9`、机器重启）。
- ssh 根本没连进去（解析不了主机名、TCP 连不上、认证被拒）的会话记的是 `failed`，不是 `unknown`，修好连接后换一个新的 `start_key` 重来就行。

先看工作树和 Agent 上的会话，再决定要不要重发。

### Machine API：`session.stop` 回的是 `stopping`

`stopping` 表示停止已经发出、还没看到结束：Agent 发完 SIGTERM 最多等 5 秒进程组退出，确认不了就先回这个。继续查 `session.status`，到终态才算停了；一直不结束（进程不理 SIGTERM）就再发一次 stop，或者按[运维手册](operations.md#写入-guard-残留)去 Agent 上找那个进程组。Agent 连不上时 stop 回错误，`effect` 告诉你它有没有可能已经送到。

### Machine API：会话变成 `unknown`，`failure` 说 `the supervisor is gone`

**症状**：`session.status` 是 `unknown`，`failure` 说 `the supervisor is gone and left no exit record … the Agent it started (pid N) may still be running`；`output.unavailable_reason` 是 `agent_refused`。

**其实是**：Agent 上管这次运行的监督进程（`ccnm internal supervise`）没了：被杀、机器重启、Controller 被强行卸掉。Agent 每 2 秒核一次它，没了几秒内就收尾成 `unknown`。`unknown` 是对的：被留下的 Claude / Codex 进程可能还在跑，ccnm 不替它写结局、也不去杀它。`agent_refused` 也是对的：监督进程是 Agent 输出的转存者，它死后的输出没人接，别据此去查认证。

**怎么办**：在 Agent Node 上看 `ps` 里还有没有这次会话的 `claude` / `codex`（`failure` 里有它的 pid，命令行里有会话 id），有就按进程组结束它；Runtime 上的写锁由执行账号保管，命令都收掉了它自己会放。别重发同一个任务。

### Machine API：`session.result` 带 `unavailable_reason: agent_refused`，完整输出拿不到

**其实是**：Runtime 第一次读结果之前，Agent 上那次会话的原始输出已经没了（被手动删、被清理），Agent 拒绝交出，`session.result` 给的是旧的尾部。`text` 是会话结束时就解析好的，不受影响；完整输出已经找不回来。从没启动的会话（没登录、被提前停掉）本来就没有输出，那种是 `bytes_total` 0、完整。

**怎么避免**：要完整输出，就在会话结束后尽快读一次 `session.result`。第一次读的时候 Runtime 会把整份拷到自己这边，之后 Agent 上删不删都不影响。
