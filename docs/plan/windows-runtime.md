# Windows 当项目机器（Runtime Node）：设计草案

制定日期：2026-10-24。核对基线：`main/6a2fe2c`。

**这是草案，还没排期。** ROADMAP 和 `status.json` 里都没有对应阶段。文中的 W0–W7 只在本文里用，批准排期时换成正式的 P 编号写进 [ROADMAP.md](ROADMAP.md)。本文只有设计，没有代码；写"实测"的地方都指向已有记录，其余是读代码和文档得出的。

## 1. 要做什么

一句话：Agent Node 照旧是 macOS 或 Linux，Runtime Node 可以是 Windows 10/11 x64。项目、Git、编译工具链留在 Windows 上，Claude Code / Codex 经 SSH 调用 ccnm 的工具，读、改、跑都在 Windows 上做。

为什么只做 Runtime 这一侧：

- **需求在这一侧。** MSVC、.NET、Unity、各种模拟器只在 Windows 上，项目搬不走；AI 跑在哪台 Mac 或 Linux 上都行。同类开源项目 [cc-desk-tunnel](https://github.com/sun168567/cc-desk-tunnel)（2026-10）就是为这个场景做的。
- **Agent 那一侧等于重写 Controller。** 那边要换掉 tmux、LaunchAgent / systemd 和 Unix socket，另立设计。
- toexec 仓库的 `docs/plan/implementation-plan-v2.md` 第 0 节写着"先定 Windows 当 Agent、当 Runtime、还是两者都要，再谈方案"。本文的回答是：先只做 Runtime。

### 1.1 第一版做

| | 范围 |
| --- | --- |
| Runtime 系统 | Windows 10 1809 及以上、Windows 11，x64 |
| Agent 系统 | 不变：macOS、Linux x86_64 |
| Provider | Claude Code、Codex 0.154.0。Provider 只跑在 Agent 上，Runtime 看到的只是 MCP 工具调用，两者对 Runtime 没有区别 |
| 入口 | 在 Agent 上发起（[架构说明](../architecture.md)里的拓扑 2 `agent -> runtime`）。Windows 上只需要能跑 `ccnm internal` 的五个子命令（`mcp-serve`、`runtime-resolve`、`runtime-audit`、`runtime-guard`、`runtime-cleanup`），加上 Runtime 侧的配置命令（`init`、`add` / `rm`、`ws`）。外部 MCP 客户端走的 `ccnm mcp bridge` 用的也是 `internal mcp-serve`，顺带就能用 |
| 账号 | 只做共用账号（P78 起的默认）：执行身份就是你在 Windows 上的账号 |
| 工具 | 现有 12 个 MCP 工具全部支持 |

### 1.2 第一版不做

- Windows 当 Agent Node。
- 在 Windows 上敲 `ccnm 项目名` 发起会话（拓扑 1，要从 Windows ssh 到 Agent 接 tmux）。放在 W7，可选。
- 专用执行账号（`runtime_user`）。Windows 上建本地账号、配 ACL、限制登录是另一套东西；第一版 Runtime 读到 `runtime_user` 就拒绝，并说明原因。
- `exec_command` 的沙箱（P33 借用的 codex sandbox）。第一版在 Windows 上拒绝打开。
- ARM64、安装程序（.msi）、图形界面、托盘。
- 随包自带一份 sshd（cc-desk-tunnel 的做法）。为什么不做见决定 D1。

不想等原生支持的话，还有一条没验过的替代路线：在 WSL2 里装 sshd 和 Linux 版 ccnm，把 WSL 当一台 Linux Runtime。代价是项目文件要放在 WSL 的文件系统里才快，Windows 原生工具链（MSVC、.NET）用不上。见未知项 U10。

## 2. 现在差在哪：项目机器一侧绑了 Unix 的地方

两侧合计有 36 个源文件 `use std::os::unix`（`grep -rl 'std::os::unix' crates --include='*.rs'` 只数 `src/` 下的）。依赖这边已经干净：唯一的 Unix 专用依赖 `nix` 只在 Linux 上引入（[ccnm-core/Cargo.toml](../../crates/ccnm-core/Cargo.toml) 第 25 行）。挡住编译的是代码，不是依赖。

| 位置 | 现在靠什么 | 到 Windows 上会怎样 | 打算 |
| --- | --- | --- | --- |
| SSH 入口：[ssh.rs](../../crates/ccnm-core/src/ssh.rs) 第 431 行 `is_remote_safe` | 远端命令里只许出现 POSIX shell 不当特殊字符看待的字符；载荷用 base64；默认的 ccnm 路径 `~/.local/bin/ccnm` 靠登录 shell 展开 `~` | **好消息**：Windows 的 sshd 对每条不带终端的命令都套一层 `cmd.exe /c "<命令>"`（cc-desk-tunnel 读 OpenSSH 源码 `w32-doexec.c` 得出），base64 里没有 cmd 的特殊字符 `^ & \| < > ( ) %`，能原样过去。**坏消息**：cmd 不展开 `~`；允许的字符里没有 `\` 和空格 | Windows Runtime 必须在 Agent 配置里写绝对路径，用正斜杠，比如 `C:/Users/<名字>/.local/bin/ccnm.exe`。cmd 能不能执行正斜杠路径要实测（U2）。路径里有空格的账号（`C:\Users\Zhang San`）第一版不支持，doctor 直接说明 |
| 跑命令：[exec.rs](../../crates/ccnm-core/src/mcp/exec.rs) 第 468 行 | `shell` 参数固定用 `bash -c` | Windows 上默认没有 bash | 见决定 D2 |
| 停命令：[process.rs](../../crates/ccnm-core/src/process.rs) 第 516、732、907 行 | 每条命令放进自己的进程组（`process_group(0)`），超时、停止、会话结束时另起一个 `kill -- -<组号>` 整组收掉。用 `kill` 程序而不用 `killpg` 系统调用，是因为整个 workspace 禁止 unsafe 代码（[Cargo.toml](../../Cargo.toml) 第 75 行 `unsafe_code = "forbid"`） | Windows 没有进程组信号 | 见决定 D3 |
| 收逃出去的后代：[orphans.rs](../../crates/ccnm-core/src/mcp/orphans.rs) | 只在 Linux 上做：把 mcp-serve 设成子进程收养者（`PR_SET_CHILD_SUBREAPER`），扫 `/proc` 找脱离出去的后代，收掉再交出写锁（P84）。macOS 上做不到，README 已写成已知限制 | Windows 上第一版和 macOS 一样做不到 | D3 选 Job Object 的话可以做到，选 `taskkill` 就和 macOS 一样写成已知限制 |
| 写锁：[write_guard.rs](../../crates/ccnm-core/src/mcp/write_guard.rs)、[patch.rs](../../crates/ccnm-core/src/mcp/patch.rs) | 标准库的 `File::try_lock` / `try_lock_shared`，在 Unix 上是 flock（建议锁：只管同样来加锁的人，不挡读写）；新建文件时用 Unix 权限位（`OpenOptionsExt::mode`） | 标准库在 Windows 上用 LockFileEx，那是**强制锁**：锁住的范围别的句柄连读都读不了。write_guard 会在持锁的同时把写者信息写进**同一个文件**，观察者要读这个文件，Windows 上很可能读失败 | 见决定 D4 |
| 路径边界：[path.rs](../../crates/ccnm-core/src/mcp/path.rs) | 规范化、符号链接越界检查，按 Unix 路径的规矩比较 | 大小写不敏感；`canonicalize` 返回带 `\\?\` 前缀的路径；8.3 短名（`PROGRA~1`）、NTFS 备用数据流（`a.txt:隐藏`）、设备保留名（`CON`、`NUL`、`COM1`）、结尾的点和空格会被系统吞掉、junction（目录联接），这些都能用来绕过"只许在项目目录里"的检查 | 见决定 D5 |
| 改文件：[patch.rs](../../crates/ccnm-core/src/mcp/patch.rs) | 已经保留 CRLF 和 BOM（第 679、899、909 行）；保留可执行位 | CRLF 是 Windows 项目里的常态，这块不用重做；Windows 没有可执行位 | 跳过可执行位相关逻辑，测试改成按平台断言 |
| 读文件：[read.rs](../../crates/ccnm-core/src/mcp/read.rs) | 已经识别 CRLF，展示时去掉，并告诉模型这个文件是 CRLF | — | 不用改 |
| 搜索：[search.rs](../../crates/ccnm-core/src/mcp/search.rs) 第 867 行 `locate_rg` | 在 PATH 里找名为 `rg` 的文件，再查 Homebrew 的两个位置 | Windows 上程序叫 `rg.exe`，这样找不到；装在 scoop / winget 的位置也不在列表里 | 按 PATHEXT 补扩展名找（gld 有现成写法，见 4.1）；doctor 说清怎么装 |
| 转发项目里的 MCP server：`mcp/relay.rs` | 照 `.mcp.json` 里写的命令起 server | `npx`、`uvx` 在 Windows 上其实是 `npx.cmd`，`.cmd` 不能直接起，要经 `cmd.exe` | 照 gld 的规矩：`.bat` / `.cmd` 经 `cmd.exe /d /s /c call ...` 起（见 4.1） |
| HTTP MCP：[curl.rs](../../crates/ccnm-core/src/mcp/curl.rs) | 调系统的 `curl`，URL 和请求头写在 `-K` 配置文件里，不上命令行 | Windows 10 1803 起自带 `curl.exe`，但它对 `-K` 和 SSE 流的表现没验过 | U8 |
| 技能钩子：[hooks.rs](../../crates/ccnm-core/src/mcp/hooks.rs) | 钩子命令用 shell 跑 | 同"跑命令"那一行 | 跟着 D2 走 |
| 沙箱：[sandbox.rs](../../crates/ccnm-core/src/mcp/sandbox.rs) | 借 codex sandbox（macOS 是 seatbelt，Linux 是 bwrap / landlock） | Codex 在 Windows 上有自己的沙箱，0.154.0 的没验过 | 第一版拒绝打开，doctor 写明 |
| 安全审计：[safety.rs](../../crates/ccnm-core/src/safety.rs) 第 454 行 `audit` | 查是不是 root、sudo、wheel / admin / docker 组、`~/.ssh` 私钥、Docker | 这些都是 Unix 概念 | 换成 Windows 的对应项：是否 Administrators 组成员、当前令牌是否提权、`%USERPROFILE%\.ssh` 里的私钥、OpenSSH Authentication Agent 服务、docker-users 组、凭据环境变量。共用账号模式下只显示、不拒绝，和 P78 一致 |
| 配置和状态目录：[paths.rs](../../crates/ccnm-core/src/paths.rs) | `~/.config/ccnm/config.toml`、`~/.local/state/ccnm/`，有 XDG 变量就按变量 | Windows 的惯例是 `%APPDATA%` / `%LOCALAPPDATA%` | 见决定 D6 |
| Agent 侧怎么处理 Runtime 的根目录 | 根目录字符串从 `runtime-resolve` 拿回来，Agent 上可能当成 Unix 路径用（拼提示、算目录名、`-c` 找上一段对话） | Windows 根目录长这样：`C:/Users/x/proj` | W1 里先把 Agent 侧所有把 root 变成 `Path` 的地方列出来再定怎么改；没查之前不能说 Agent 侧不用改 |
| 发版：[release.yml](../../.github/workflows/release.yml) | 只出 macOS 通用二进制和 Linux x86_64 两个包 | — | 加 `x86_64-pc-windows-msvc`，出 zip + sha256 |
| CI：[ci.yml](../../.github/workflows/ci.yml) | 只有 macOS 和 Ubuntu 两个 job | — | 加 `windows-latest` job，见 W1 |

## 3. 关键决定

每条都写了推荐和备选。标"待用户定"的，排期前要拍板。

### D1 SSH 服务：用 Windows 自带的 OpenSSH Server（待用户定）

**推荐：** Windows 自带的 OpenSSH Server。它是"可选功能"，装好后作为系统服务开机自启。

- **为什么：** ccnm 在 macOS 和 Linux 上用的也是系统 sshd，由 Agent 连进来，ccnm 自己不常驻。沿用同一个模型，Runtime 侧不多出一个要管的进程。
- **代价一：装的时候要管理员权限，只装一次。** 用 `Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0`，见 [Microsoft 的安装说明](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse)。
- **代价二：命令打开的图形窗口用户看不见。** 系统服务跑在 session 0（Windows 给服务用的后台会话），命令启动的 GUI 程序不会出现在你的桌面上（上游 [Win32-OpenSSH #1898](https://github.com/PowerShell/Win32-OpenSSH/issues/1898)）。编译、跑测试、命令行工具不受影响；要让模型打开 GUI 程序或截屏就不行。

**备选：** 像 cc-desk-tunnel 那样，在用户的登录会话里起一份用户态 sshd，不用管理员，GUI 也看得见。代价是每条命令都会弹出一个 cmd 窗口。它靠 OpenSSH 的内部测试开关 `SSH_TEST_ENVIRONMENT=1` 压住弹窗，每次升级 OpenSSH 都要复查（它的[调研记录](https://github.com/sun168567/cc-desk-tunnel/blob/e2ec3db/docs/research/windows-shell-windows.md)）。另外还要有个常驻进程来起停这份 sshd，这正是 ccnm 现在没有的东西。

**要用户定：** 第一版能不能接受"GUI 看不见"。不能接受就选备选，工作量多出一块"Runtime 侧常驻"。

**会撞上的坑**（写进安装文档）：

- **账号在 Administrators 组里时，sshd 不读 `%USERPROFILE%\.ssh\authorized_keys`。** 默认配置只认 `C:\ProgramData\ssh\administrators_authorized_keys`，而且那个文件的权限必须只给 Administrators 和 SYSTEM（[Microsoft 密钥管理说明](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_keymanagement)）。不知道这条的话，Agent 连过来就是 `Permission denied (publickey)`，看着像钥匙不对，其实是放错了文件。
- 默认 shell 是 cmd.exe，可以在注册表 `HKLM\SOFTWARE\OpenSSH` 的 `DefaultShell` 里改（[服务端配置说明](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_server_configuration)）。用户改成 PowerShell 之后 ccnm 的远端命令还过不过得去，要实测（U2）。

### D2 `exec_command` 的 `shell` 在 Windows 上怎么跑（待用户定）

**推荐：** 照 Claude Code 自己在 Windows 上的做法。找得到 Git for Windows 带的 bash，就用它跑 `shell`（模型写出来的本来就是 bash 语法）；找不到就报错，提示改用 `cmd` 参数直接跑程序。要用 PowerShell，模型可以直接写 `cmd: ["pwsh", "-NoProfile", "-Command", "..."]`，不用改协议。

- **依据：** [Claude Code 安装文档](https://code.claude.com/docs/en/setup)："With Git for Windows, Claude Code uses Git Bash for the Bash tool"；没有 Git for Windows 时改用 PowerShell 工具；bash 找不到时用 `CLAUDE_CODE_GIT_BASH_PATH` 指定。ccnm 照抄这个规矩：先找 `C:\Program Files\Git\bin\bash.exe`，再看 Runtime 配置里的路径。
- **备选 A：** 在 Windows 上把 `shell` 直接换成 PowerShell（cc-desk-tunnel 的选择）。同一个参数在不同系统上意思不一样，模型按 bash 写的命令会大面积失败，不推荐。
- **备选 B：** 加一个可选参数 `powershell`（一段脚本，用 pwsh 7 跑）。[Remote Workspace MCP](../protocol/remote-workspace-mcp-v1.md) 的冻结规则允许加可选参数（属于加法，不用升 `/2`）。等真机上看到模型老是用 `cmd` 拼 pwsh 拼错了再加。真要加时照 cc-desk-tunnel 的办法：脚本从标准输入整段读进来、用 `[scriptblock]::Create` 执行，输入输出都设成 UTF-8，保留退出码。**别用 `-Command -` 或 `-File -`**：它们逐行读标准输入，跨行的代码块会丢（cc-desk-tunnel [`tunnel.ts` 第 46–54 行](https://github.com/sun168567/cc-desk-tunnel/blob/e2ec3db/apps/server/src/tunnel.ts)）。

### D3 停命令：先用 `taskkill /T /F`，Job Object 留作第二步（待用户定）

**推荐：** 第一版照 gld 的办法：停止、超时、会话结束时跑 `taskkill /T /F /PID <pid>` 杀整棵进程树，失败了再单独结束根进程。

- **为什么：** 不用写 unsafe 代码，守住 workspace 的 `unsafe_code = "forbid"`，和 [process.rs](../../crates/ccnm-core/src/process.rs) 现在"另起一个 `kill` 程序"的做法是一个思路。gld 有 Windows CI 上跑通的证据（gld 的 `crates/core/src/tools/session.rs` 第 1662 行起，`crates/cli/tests/daemon_lifecycle.rs`）。
- **局限：** `taskkill /T` 按父进程号找后代。父进程先退出的孙进程找不到，和 macOS 现在的已知限制一样，写进支持矩阵。

**第二步（可选）：** Job Object，Windows 的进程容器。把每条命令放进它自己的 Job，并设 `KILL_ON_JOB_CLOSE`：Job 一关，里面整棵树一起结束，父进程先退出的孙进程也跑不掉；mcp-serve 被强杀时句柄自动关闭，子进程跟着走。cc-desk-tunnel 用它管 sshd 和所有子进程，实测关闭时能结束正在跑的命令和子进程（它的 [`apps/desktop/README.md`](https://github.com/sun168567/cc-desk-tunnel/blob/e2ec3db/apps/desktop/README.md)）。代价有三：

- **要 unsafe 代码**，放在哪见 D7；
- **有竞态**：进程要先挂起再放进 Job，否则它可能在放进去之前就生了孙进程。标准库的 `Child` 拿不到主线程句柄，没法恢复挂起的进程，要么自己调 `CreateProcessW`，要么用 ToolHelp 枚举线程后逐个恢复；
- **语义会变**：今天在 Unix 上 mcp-serve 被强杀后，后台命令还活着（[core-hardening.md](core-hardening.md) 的 AUTH-02）；用 Job Object 之后会跟着死。写锁和清理的说明要跟着改。

**无论选哪个都要做：** 起命令时带 `CREATE_NO_WINDOW`（不弹控制台窗口）和 `CREATE_NEW_PROCESS_GROUP`（gld 的 `tools/exec.rs` 第 2095–2102 行），并且不让子进程继承 mcp-serve 自己的标准输入输出句柄。后者为什么必须做：Rust 在 Windows 上起子进程时，会把父进程里所有可继承的句柄都交出去（[rust-lang/rust#38227](https://github.com/rust-lang/rust/issues/38227)）。mcp-serve 的标准输入输出就是那条 SSH 通道，后台命令拿到它之后，mcp-serve 退出了通道也不关，Agent 那边就一直等着。gld 第一次在 Windows CI 上真跑时就是这样卡满了 20 分钟（gld 的 `crates/core/src/platform/mod.rs` 第 57–67 行）。这一步本身也要 unsafe（`SetHandleInformation`）。

### D4 写锁：锁文件只用来加锁，不放数据

**推荐：** write_guard 和 apply_patch 日志里那种"一边持锁一边在同一个文件里读写"的地方，拆成两个文件：一个只用来加锁，另一个放写者信息。

- **为什么：** Windows 上的锁是强制锁，按句柄生效。gld 的测试注释写得很清楚："Unix 是 flock（建议锁），Windows 是 LockFileEx（强制锁、按句柄），语义不同，编得过不代表拦得住"（gld 的 `crates/cli/tests/daemon_lifecycle.rs` 第 140–141 行）。
- 另外两条 Windows 规矩要一起处理：
  - **打开着的文件默认删不掉、也改不了名**，除非打开时带了 `FILE_SHARE_DELETE`。清理正在写的输出文件会失败，要能重试。
  - **只读属性的目标文件替换时报 os error 5**，要先摘掉只读属性（toexec-fs 的 `atomic.rs` 第 97–121 行、第 290–372 行的测试）。
- 新建文件时 Unix 权限位那段在 Windows 上去掉：用户目录下的文件默认只有本人、管理员和 SYSTEM 能读。

### D5 路径：先规范化，再拒绝 Windows 特有的绕法

**推荐：** 所有进出工具的路径先规范化：去掉 `\\?\` 前缀、统一成 `/`、大小写折叠后再比较。然后一律拒绝：

- 盘符之外还带 `:` 的组件（备用数据流）；
- 设备保留名（`CON`、`PRN`、`AUX`、`NUL`、`COM1`–`COM9`、`LPT1`–`LPT9`，带不带扩展名都算）；
- 以点或空格结尾的组件；
- 含 `~` 的组件，先用 `GetLongPathNameW` 展开成长名再判断（8.3 短名）。

junction 按符号链接同样处理。返回给模型的路径一律是相对项目根、用 `/` 的形式。

- **为什么：** 这些都是 Windows 上"只许在项目目录里"这类检查的经典绕法，gld 和 toexec 里都没有现成实现（gld 的审查文档自己记着"8.3 短路径还没进 Windows 小组"）。规范化可以借 gld：它去掉了 `\\?\`，并用 `canonicalize` 纠正大小写来给工作区锁定身份，在 Windows CI 上跑过（gld 的 `tools/workspace_runtime.rs` 第 570–576 行、第 679–705 行的测试）。

### D6 配置和状态放在哪

**推荐：** 沿用和 macOS / Linux 一样的相对位置：`%USERPROFILE%\.config\ccnm\config.toml` 和 `%USERPROFILE%\.local\state\ccnm\`，设了 XDG 变量就按变量。二进制放在 `%USERPROFILE%\.local\bin\ccnm.exe`。

- **为什么：** 文档三个系统写一套就行。Claude Code 在 Windows 上也装在 `%USERPROFILE%\.local\bin\claude.exe`（[官方卸载说明](https://code.claude.com/docs/en/setup)），gld 三个平台都用 `~/.config/gld`。
- **备选：** `%APPDATA%\ccnm` 和 `%LOCALAPPDATA%\ccnm`，更符合 Windows 惯例，但文档要分系统写。

### D7 Windows 系统调用的代码放在哪（待用户定）

Job Object、`SetHandleInformation`、`GetLongPathNameW`、令牌提权检查都要调 Windows API，都得写 unsafe，而 ccnm 整个 workspace 禁止 unsafe。

- **推荐：** 放进 toexec，新开一个只放平台进程原语的 crate。toexec 已经有 Windows CI（`windows-latest` 跑 clippy、test，再用 MSRV 1.89 跑一遍），gld 也需要同样的东西（gld 的 `machine_mcp/open.rs` 第 126–137 行在接 toexec-mcp 的 `Stop` 回调时，自己写了一遍杀进程树）。但 toexec 的去重审计当时定过"进程与输出：短期不要碰"（toexec 的 `evidence/v2-k/duplication-audit.md` 第 76–86 行），挪进去要先改那个决定。
- **备选：** ccnm 仓库里另建一个小 crate，单独放开 unsafe，只放 Windows FFI；其余 crate 照旧禁止。
- **第一版能不能不碰 unsafe：** D3 选 `taskkill`、D5 的短名展开先改成拒绝含 `~` 的组件，就只剩"不继承标准句柄"这一处绕不开。

### D8 第一版只支持共用账号

Runtime 配置里写了 `runtime_user` 时，Windows 版直接拒绝，说明原因和替代办法。专用账号要解决 Windows 本地账号、ACL、"拒绝本地登录"这些，等第一版真机跑通再单独设计。

## 4. 从哪儿借什么

### 4.1 gld 和 toexec（同一作者，优先借）

**先说清楚证据的分量：** 两个仓库都没有在真 Windows 机器上验过的记录，证据全部来自 GitHub 的 `windows-latest` runner。gld 的 `docs/install.md` 原话："下载包没在 Windows 真机跑过"。gld 的 `tools/exec.rs` 里那批 Windows 单测，CI 只编译、不运行：Windows job 只跑 `tools::patch tools::workspace_runtime` 两组（gld 的 `.github/workflows/ci.yml` 第 84 行）。

| 借什么 | 在哪 | 证据 |
| --- | --- | --- |
| 原子写和替换（`write_durable`、`replace`），ccnm 已经依赖 | toexec 的 `crates/toexec-fs/src/atomic.rs`；生产代码没有平台分支，Windows 上靠标准库的 `MoveFileExW` 加一次 `FileRenameInfoEx` 重试 | toexec 的 Windows CI 实跑，含"只读目标被拒""被独占打开时替换失败、旧内容不变"两条 Windows 专属用例 |
| 杀进程树：`taskkill /T /F`，失败了退回 `TerminateProcess` | gld 的 `crates/core/src/tools/session.rs` 第 1662–1685 行 | gld 的守护进程端到端测试在 Windows CI 上跑过"停命令不留孤儿" |
| 杀进程树：ToolHelp 枚举版，按映像路径确认 pid 身份 | gld 的 `crates/core/src/platform/windows/process.rs`（`terminate_process_tree`、`process_image_path`、`is_process_alive`） | 只扫一遍快照，父进程已经退出的后代收不到 |
| 不让子进程继承标准句柄 | 同上文件第 205–221 行，注释在 `platform/mod.rs` 第 57–67 行 | Windows CI 上的真实卡死修好了 |
| 起程序的规矩：`CREATE_NEW_PROCESS_GROUP \| CREATE_NO_WINDOW`；`.bat` / `.cmd` 经 `cmd.exe /d /s /c` 加 `call "<路径>"`，参数里的 `"` 转成 `""`；`.ps1` 用 `-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File`；按 PATHEXT 补扩展名；去掉 `\\?\` | gld 的 `crates/core/src/tools/exec.rs` 第 2095–2190 行、第 1279–1302 行 | 单测 CI 不运行；`.cmd` 和 PowerShell 那两条路在守护进程端到端测试里实际跑过 |
| 主线程栈调到 8 MiB | gld 的 `crates/cli/build.rs` | 默认 1 MiB 时 debug 构建栈溢出（`0xC00000FD`），CI 上修好 |
| 让 Python 子进程输出 UTF-8：`PYTHONUTF8=1` 等三个变量 | gld 的 `tools/exec.rs` 第 817–821 行 | 中文测试只断言了"命令成功"，没检查输出内容 |

toexec-mcp 的 `ChildTransport` 不管进程怎么起、怎么杀，由产品传进 `Stop` 回调。ccnm 在 Windows 上把这个回调接到 D3 的杀树办法上就行。注意它的测试只在 Unix 上跑（toexec 的 `crates/toexec-mcp/src/child.rs` 第 205 行）。

### 4.2 cc-desk-tunnel（Apache-2.0，借思路和踩坑记录，不搬代码）

| 借什么 | 在哪（固定在 `e2ec3db`） |
| --- | --- |
| PowerShell 脚本的跑法：整段从标准输入读、UTF-8 进出、纯文本输出、保留退出码；`-Command -` 逐行读会丢跨行块 | [`apps/server/src/tunnel.ts`](https://github.com/sun168567/cc-desk-tunnel/blob/e2ec3db/apps/server/src/tunnel.ts) 第 46–54 行 |
| Windows sshd 每条命令都套 `cmd.exe /c`；用户态 sshd 弹窗的根因和处理；系统服务在 session 0 看不见 GUI | [`docs/research/windows-shell-windows.md`](https://github.com/sun168567/cc-desk-tunnel/blob/e2ec3db/docs/research/windows-shell-windows.md) |
| 用 Job Object 管 sshd 和子进程树 | [`apps/desktop/README.md`](https://github.com/sun168567/cc-desk-tunnel/blob/e2ec3db/apps/desktop/README.md) 的"自动远程连接"一节 |
| 显式用 UTF-8 输出，不靠控制台代码页 | 同上 |
| sshd 给命令重建 PATH，不继承登录时的 PATH，所以工具位置不能靠 PATH | 同上 |
| 项目路径按"不区分大小写、两种分隔符等同"比较 | [`docs/native-runtime.md`](https://github.com/sun168567/cc-desk-tunnel/blob/e2ec3db/docs/native-runtime.md) |

### 4.3 没处可借、要自己做的

两个仓库里都没有：Job Object、控制台代码页转换、设备保留名、长路径、8.3 短名、备用数据流的处理。D3 的第二步和 D5 都属于这一类，工作量要按从头写来估。

## 5. 分阶段和验收（草案编号）

每个阶段先离线做完，再上真机。真机结论只认写下来的记录，离线测试的数量不能替代。

| 阶段 | 做什么 | 验收 |
| --- | --- | --- |
| W0 预研 | 不写产品代码，只在一台 Windows 机器上实测，回答挡路的未知项 | W0.1 装好 Windows 自带的 OpenSSH Server 后，从 Agent 用 ccnm 现在的 ssh 参数（`-T`、`BatchMode`、`SetEnv`、`ServerAliveInterval`）连进来，执行一个带 base64 参数、用正斜杠路径写的 exe，参数原样到达（U1、U2）。W0.2 用一个回显程序在这条连接上跑一小时、来回传 100 MB 随机字节，不丢、不改（U3）。W0.3 断开 SSH 后，sshd 会不会结束会话里的进程，有没有给它们套 Job（U4）。W0.4 记下管理员账号的 `authorized_keys` 位置和权限要求。结论写进 `docs/research/` |
| W1 能编译、CI 有 Windows | Runtime 侧代码按平台分开；只属于 Agent 的模块（controller、launchagent、systemd、tmux、launcher 等）在 Windows 上明确报"Windows 上不支持"；列出 Agent 侧所有把 Runtime 根目录变成 `Path` 的地方 | W1.1 `cargo build --target x86_64-pc-windows-msvc` 通过，clippy 无警告。W1.2 CI 新增 `windows-latest` job，跑 Runtime 侧单元测试。W1.3 macOS、Linux 现有测试全过，没有回归 |
| W2 只读工具 | `workspace_info`、`read_file`、`list_files`、`search_text`、`view_image`、`read_notebook`、`load_skill`，加上 D5 的路径规则 | W2.1 七个工具在 Windows CI 上有端到端测试。W2.2 D5 列的每种绕法都有一条被拒的用例。W2.3 返回的路径一律用 `/` |
| W3 写 | `apply_patch`（含 `write`）、单写者锁（D4）、补丁日志锁 | W3.1 CRLF、BOM 保留的现有用例在 Windows 上跑通。W3.2 两个 mcp-serve 争同一个项目，第二个被拒；观察者读得到写者信息，不报错。W3.3 目标文件只读、被别的进程打开时，报清楚的错，原内容不变 |
| W4 执行 | `exec_command`（`cmd` 直接起程序；`shell` 走 Git Bash，见 D2）、后台命令、`read_output`、`stop_command`、会话结束时收尾；D3 的杀树；不继承标准句柄；`.cmd` / `.bat` / `.ps1` 的起法；`call_mcp_tool` 转发起 `npx.cmd` 这类 server | W4.1 超时、停止、会话结束后，进程树里没有残留（父进程先退出的孙进程按 D3 的选择，要么收掉、要么写成已知限制）。W4.2 后台命令还在跑时 mcp-serve 退出，Agent 那边的连接照常关闭，不挂住。W4.3 中文输出、独立的 stderr、非零退出码都对。W4.4 没装 Git for Windows 时 `shell` 报清楚的错 |
| W5 配置、诊断、发布 | `ccnm init --agent`、`ccnm add`、从 Agent 跑 `ccnm doctor`；`runtime-audit` 的 Windows 检查项；`runtime_user` 拒绝（D8）；安装文档（OpenSSH Server、管理员钥匙的位置、Git for Windows、rg）；发布包加 Windows zip | W5.1 doctor 在 Windows Runtime 上逐行给出结论，没配好的地方说清下一步。W5.2 安装文档按新手视角从零走一遍能跑通。W5.3 发布流程出 `ccnm-<版本>-windows-x86_64.zip` 和 sha256，只验构建，实际发版另外授权 |
| W6 真机验收 | macOS Agent → Windows 11 Runtime，真实项目、真实模型 | W6.1 Claude Code 和 Codex 各跑一次：读代码、改代码、编译、跑测试、提交，都在 Windows 上完成。W6.2 断网 30 秒后恢复、按 Esc 停命令、`ccnm stop` 之后都没有残留进程，写锁正常交出。W6.3 支持矩阵和 README 按实测更新：做到哪一步就写到哪一步 |
| W7（可选）在 Windows 上发起 | 拓扑 1：在 Windows 上敲 `ccnm 项目名`，用 Windows 自带的 ssh 客户端 ssh 到 Agent 接 tmux | W7.1 Windows Terminal 里画面、按键、复制、滚屏正常（U9）。W7.2 `ccnm ls`、`attach`、`stop` 在 Windows 上能用 |

依赖关系：W0 → W1 → W2 → W3 → W4 → W5 → W6，W7 在 W6 之后。W2 和 W3 可以并行，但 W3 的锁要等 W2 的路径规范化先定（锁的身份用规范化后的路径）。

## 6. 未知项

| 编号 | 问题 | 怎么验 | 不成立会怎样 |
| --- | --- | --- | --- |
| U1 | Windows 自带 sshd 对 `SetEnv` 的态度：不接受时是忽略，还是拒绝连接 | W0.1 | 拒绝的话，Windows Runtime 要去掉这个选项 |
| U2 | cmd.exe 能不能执行 `C:/Users/x/.local/bin/ccnm.exe` 这种正斜杠绝对路径；`DefaultShell` 改成 PowerShell 后还能不能过 | W0.1 | 不能的话，`is_remote_safe` 要为 Windows 放开 `\`，并重新论证 cmd 和 PowerShell 两种解析下都不会出岔子 |
| U3 | MCP 长连接：Win32-OpenSSH 的标准输入输出能不能撑住一条连接跑几个小时、传大输出、二进制原样。cc-desk-tunnel 验过中文、独立 stderr、退出码、stdin 管道，但它每条命令一条连接，没有长连接的数据 | W0.2 | 撑不住就是根本性障碍，要回头考虑每次工具调用一条连接，那是另一个方案 |
| U4 | 系统服务 sshd 起的进程是不是已经在某个 Job 里；允不允许嵌套 Job（Windows 8 起支持嵌套）；SSH 断开时 sshd 会不会结束会话里的进程 | W0.3 | 影响 D3 第二步能不能做，以及断网后残留进程的表现 |
| U5 | Git for Windows 的 bash 在 sshd 服务环境里：`PATH`、`HOME`、`core.autocrlf` 的表现 | W4 前先手测 | 模型的命令找不到工具，或 git 状态对不上 |
| U6 | Windows Defender 实时扫描对 rg 搜索、大量小文件读写拖慢多少 | W2、W4 各量一次耗时 | 太慢就在文档里建议把项目目录加入排除（由用户决定，ccnm 不改安全设置） |
| U7 | 下载的 `ccnm.exe` 带"来自网络"标记，经 ssh 启动时会不会被 SmartScreen 或 Defender 拦 | W5 用发布包实测 | 要在文档里写怎么解除标记（`Unblock-File`） |
| U8 | Windows 自带的 `curl.exe` 支不支持 ccnm 用的 `-K` 配置文件和 SSE 读法 | W4 | 不支持就在 Windows 上先关掉 HTTP MCP 转发 |
| U9 | Windows Terminal 加 Windows ssh 客户端 `-t` 接 Agent 的 tmux，表现如何 | W7 | 只影响 W7 |
| U10 | WSL2 里跑 Linux 版 ccnm 当 Runtime，能不能直接用 | 任何时候都可以单独试 | 可以的话，原生支持之前文档里先给这条路 |

## 7. 验收要的环境和授权

- **一台 Windows 10 或 11 x64 机器**（物理机或虚拟机都行），Agent 能 SSH 进去。现在没有这台机器，要用户提供，或者授权建一台虚拟机。没有它 W0 和 W6 就是受阻，不能拿 Linux 上的单测顶替，也不能拿交叉编译顶替（toexec 计划里的原话："不以 Windows 交叉编译代替进程树/文件替换实测"）。
- 在那台机器上装 OpenSSH Server、开防火墙端口、放管理员钥匙，每一项都要用户单独授权（[AGENTS.md](../../AGENTS.md) 的规定）。
- W6 用真实模型，按以往真机轮的规模估，一次不超过几美元，跑之前要用户点头。
- 零额度的验法照旧可用：假模型接口，加上不依赖 ccnm 代码的中立 MCP 客户端（`tests/test_remote_workspace_mcp.py`），从 Mac 连到 Windows 把 12 个工具都调一遍。

## 8. 风险和代价

- **改动面大。** Runtime 侧 12 个工具的执行底层都要分平台，macOS 和 Linux 有回归风险。每个阶段都要跑全套现有测试，动 `process.rs` 时还要按 AGENTS.md 的要求跑 `--test-threads=64` 的压力测试。
- **多一个平台的长期成本。** CI 时间、每次发版多一个包、只在 Windows 上出现的 bug。作者手边没有 Windows 机器时，这些 bug 只能靠 CI 和用户报告。
- **新的攻击面。** D5 那些路径绕法是 Windows 独有的，漏掉一种就是越界读写。共用账号下模型能碰到的东西和 macOS / Linux 一样多，README 里写的那个口子（项目机器能 ssh 回 Agent）在 Windows 上同样存在：你的 `%USERPROFILE%\.ssh` 里有连 Agent 的钥匙，模型就能连回去。
- **协议。** 第一版不改 Remote Workspace MCP。D2 备选 B 要加参数时，按冻结规则只做加法。

## 9. 排期前要用户定的事

1. D1：第一版能不能接受"命令打开的 GUI 看不见"。决定用系统 sshd 还是自带用户态 sshd。
2. D2：`shell` 走 Git Bash、PowerShell 先不加专门参数，行不行。
3. D3 和 D7：第一版用 `taskkill`、只为"不继承标准句柄"破一次 unsafe，还是直接上 Job Object；unsafe 代码放进 toexec 还是 ccnm 里单独的 crate。
4. 要不要排期；排的话谁提供 Windows 机器。

这几条定下来以后，把 W0–W7 换成 P 编号写进 ROADMAP 和 status.json；toexec 那份计划第 0 节的 Windows 一栏，再改成指向本文。
