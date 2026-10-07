# Windows 当 Runtime 要先知道的 OpenSSH 坑（2026-10-07）

**为什么有这份文件**：ccnm 还没做 Windows，[核心开发总纲](../plan/core-hardening.md)第 7 节要求先写路径、ACL、进程树、SSH、PTY 和打包的 RFC。[cc-desk-tunnel](https://github.com/sun168567/cc-desk-tunnel) 已经让 Windows 当"被 SSH 进来执行命令的那台"跑通了，它踩的坑和 ccnm 的 Runtime 是同一类。写 Windows RFC 之前先读这份，别重踩。

**读法**：

- 出处是 cc-desk-tunnel 的提交 `bf01a4f`（2026-10-07，Apache-2.0）。大部分来自它的 `docs/research/windows-shell-windows.md`，另有几条取自它的代码和提示词，每条都注明了。
- 标"对方实测"的是它自己记录的测量，**ccnm 没在 Windows 上复现过**。标"推断"的是对照 ccnm 源码（main `fa1b025`）得出的判断，没验证。

## 对方的场景和 ccnm 差在哪

**它**：Windows 上随包带 Win32-OpenSSH `10.0.0.0p2-Preview`，**以普通用户身份在桌面会话里起一个临时 sshd**（只听 127.0.0.1、只认本次生成的公钥、关掉端口转发），不装系统服务。Linux 上的 Claude 每条命令单独 `ssh` 一次，远端跑 PowerShell 7。

**ccnm 做 Windows Runtime 时大概率不一样**：执行账号（`ccrun`）是专门建的、只让人连进来，用 Windows 自带的 OpenSSH Server 服务更自然；一个会话只有一条常驻 SSH，上面跑 `ccnm internal mcp-serve`，命令由 ccnm 自己起子进程。所以下面每条都单独写了"对 ccnm 意味着什么"。

## 1. 每条远端命令都先经过 `cmd.exe /c`

**对方查到的（读源码）**：Win32-OpenSSH 收到非 PTY 命令，一律执行 `cmd.exe /c "<命令>"`（`w32-doexec.c`）。远端命令里写明要 PowerShell 也绕不过这一层。所以它的提示词专门告诉 Claude：登录命令是 cmd.exe 解析的，直接调用带引号的 PowerShell 路径、别加 `&`；复杂脚本转成 UTF-16LE Base64 走 `-EncodedCommand`。

Win32-OpenSSH 能改默认 shell（`DefaultShell`），但对方没采用：那是全局设置，会影响这台机器上别的 SSH 用法。

**对 ccnm 意味着什么（推断）**：

- ccnm 发往远端的参数本来就不允许需要引号的字符（`crates/ccnm-core/src/ssh.rs` 的 `is_remote_safe`，只许字母数字和 `- _ . / = : @ + , ~`）。这里面没有 cmd.exe 的特殊字符 `& | < > ^ % "`，这条约束到 Windows 上多半还能用。
- **会坏的是默认远端路径**：`crates/ccnm-core/src/config.rs` 的 `DEFAULT_CCNM_BIN` 是 `~/.local/bin/ccnm`，`~` 要靠远端的 POSIX shell 展开，cmd.exe 不认。Windows Runtime 要么换默认值，要么要求配置里写绝对路径。

## 2. sshd 不当服务跑，每条命令弹一个控制台窗口

**对方查到的（读源码 + 实测）**：

- **根因**：sshd 起会话进程时加了 `DETACHED_PROCESS`，会话进程没有控制台；它再起 `cmd.exe` 时没加无窗口标志，Windows 就给 `cmd.exe` 新开一个控制台（`w32fd.c` 的 `spawn_child_internal`）。
- **为什么官方没人修**：系统服务形态下进程在 session 0（不属于任何人的桌面），新窗口谁也看不见。上游维护者在 [Win32-OpenSSH #1898](https://github.com/PowerShell/Win32-OpenSSH/issues/1898) 里说过这点，并把非服务方式运行叫作调试形态。对方故意不装服务，sshd 跑在用户桌面里，窗口就弹出来了，还会抢焦点。
- **解法**：启动 sshd 之前，在 sshd 进程自己的环境里设 `SSH_TEST_ENVIRONMENT=1`。同一个函数里本来就有一个分支：这个变量非零就加 `CREATE_NO_WINDOW`。写进 `sshd_config` 的 `SetEnv` 没用，那只改命令的环境，不改 sshd 自己的。
- **对方实测**（Windows build 26200，跑 4 条命令）：不加开关时新窗口 4 个、焦点被抢 4 次，`echo` 约 140 ms；加了之后都是 0，`echo` 约 46 ms。中文输出、stderr 分开、非零退出码、stdin 管道、SFTP 中文文件名、命令里再起 git 等控制台程序都正常；命令自己打开的 GUI 窗口照常显示。交互式 PTY（`ssh -t`）走另一条代码路径，**没测**。
- **代价**：这是 OpenSSH 的内部测试开关，不是公开选项。它的副作用只有一个：把 `/cygdrive/<盘符>/` 开头的路径转成盘符路径。换 OpenSSH 版本要重查 `spawn_child_internal` 里这个分支还在不在（对方 2026-10-06 查过上游 `latestw_all` 分支，没变）。
- **对方否掉的其他办法**：自己写 SSH 服务端（要自己补 SFTP、PTY、进程归属）；装成服务进 session 0（要管理员，用户想看的 GUI 也看不见了）；放进独立桌面（实测没消掉弹窗）；`ssh -tt` 走 PTY（stderr 不再分开，非零退出码丢了）；改全局 `DefaultShell`；弹出后再隐藏（已经闪过了）；降到 OpenSSH 7.9（[#2465](https://github.com/PowerShell/Win32-OpenSSH/issues/2465) 说没这问题，但要放弃多年安全修复）。

**对 ccnm 意味着什么（推断）**：

- 用系统 OpenSSH Server 服务、`ccrun` 只做入站，进程在 session 0，按上游的说法不会弹窗，这个开关用不上。
- 只有走"普通用户在桌面会话里起 sshd"这条路（比如不想装服务、拿不到管理员）才会撞上。就算撞上，ccnm 一个会话只建一条 SSH，最多弹一次；ccnm 自己起的命令子进程可以直接带 `CREATE_NO_WINDOW`，不靠 sshd。

## 3. Windows 没有进程组，收进程要用 Job Object

**对方的做法（代码，`apps/desktop/electron/component-host.ps1`）**：sshd 和 frpc 放进同一个 Job Object（Windows 上把一组进程绑在一起管的内核对象），设 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`（0x2000）。子进程**先挂起创建，加进 Job 后再恢复运行**，这样它在入 Job 之前没机会再起孙进程逃出去。宿主一退出，Job 句柄关闭，里面的进程一起结束。

**对方提示词里还有一句**（`apps/server/src/claude.ts`，没找到对应的实测记录）：每条命令启动的 Windows 进程，会在这条命令的 SSH 会话结束时被结束，所以在 Windows 上让进程脱离出去也活不下来。

**对 ccnm 意味着什么（推断）**：

- ccnm 现在靠 Unix 进程组收整棵进程树：`crates/ccnm-core/src/process.rs` 和 `crates/ccnm-core/src/mcp/relay.rs` 里的 `process_group(0)`，再对整组发 `kill`。Windows 上对应的就是 Job Object，"挂起创建 → 入 Job → 恢复"这个顺序要照抄。
- 工作区 `Cargo.toml` 设了 `unsafe_code = "forbid"`，Job Object 却要直接调 Win32 API。是引 `windows-sys` 这类依赖并在一处放开 `unsafe`，还是找现成的封装库，要在 Windows RFC 里先定。
- 上面那句"SSH 会话结束就结束进程"如果在服务形态下也成立，会影响后台命令（P41）和"断线后命令还跑不跑"的语义。没验证，RFC 里要实测。

## 4. 中文 Windows 的输出编码

**对方的修复（提交 [`95b87a8`](https://github.com/sun168567/cc-desk-tunnel/commit/95b87a8f186c74c461c667b42f92ba80476bc3c2)）**：安装路径带中文时，准备 SSH 的 PowerShell 脚本输出被父进程按 UTF-8 读坏，握手失败。修法是在脚本开头加 `[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)`。它写 `authorized_keys`、`sshd_config` 时也都显式用不带 BOM 的 UTF-8。

**对 ccnm 意味着什么（推断）**：`crates/ccnm-core/src/mcp/exec.rs` 的 `preview` 用 `String::from_utf8_lossy` 解码命令输出。简体中文 Windows 的控制台默认代码页是 936（GBK），命令输出的中文到这里会变成一串 `�`。Windows RFC 要定：强制子进程输出 UTF-8，还是按代码页转码。

## 这份文件不管的

- **Windows 当 Agent**（跑 AI 那台）：Controller 在 Windows 上靠什么常驻、没有 tmux 怎么办，对方都不涉及，它的 AI 跑在 Linux。
- **ACL、路径（盘符、大小写、长路径）、打包签名**：对方没有成文调研，这里不记。

## 来源

- cc-desk-tunnel `bf01a4f`：`docs/research/windows-shell-windows.md`、`apps/desktop/electron/component-host.ps1`、`apps/desktop/electron/prepare-ssh.ps1`、`apps/server/src/claude.ts`
- 对方引用的上游源码（v10.0.0.0）：[w32fd.c](https://github.com/PowerShell/openssh-portable/blob/v10.0.0.0/contrib/win32/win32compat/w32fd.c)、[w32-doexec.c](https://github.com/PowerShell/openssh-portable/blob/v10.0.0.0/contrib/win32/win32compat/w32-doexec.c)、[misc.c](https://github.com/PowerShell/openssh-portable/blob/v10.0.0.0/contrib/win32/win32compat/misc.c)；[Process Creation Flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags)；Win32-OpenSSH [#1898](https://github.com/PowerShell/Win32-OpenSSH/issues/1898)、[#2465](https://github.com/PowerShell/Win32-OpenSSH/issues/2465)
