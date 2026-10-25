# P106 Windows 预研记录（2026-10-24 至 10-25）

阶段范围见 [ROADMAP](../plan/ROADMAP.md) 的 P106，未知项编号（U1–U11）见[设计文档](../plan/windows-runtime.md)第 6 节。

**结论（2026-10-25）：** 照 ccnm mcp-serve 的结构（一个线程只管读 stdin、一个线程只管写 stdout，读写都用 Rust 标准库的办法），经 Windows 自带 sshd 双向送 64 MiB 都原样回来了。所以 U3 不是根本性障碍，但对 ccnm 有一条硬要求：**一直要有线程在读 stdin。**

一开始看到的"stdin 送不进去"有两个原因，都和 ccnm 的情况不同：

- **测试程序的读法不对。** Win32-OpenSSH 给子进程的 stdin 是一根异步命名管道，测试程序是 .NET 写的，.NET 用同步读法去读这种句柄会卡死。这是上游公开报过的问题，和默认 shell 是 PowerShell 还是 cmd 无关。
- **单线程"读一块、写一块"会卡。** 读写都换成 Rust 的办法也一样，原因还没证实。

一小时长连接 98 MB 也通过了，见"P106.2 一小时长连接"一节。

另外几条会直接影响后面阶段的：

- 默认 shell 是 PowerShell 时，原生程序的退出码会变成 1，以 `@` 开头的参数会被吃掉。
- 新版 ssh 的后量子警告会让 ccnm 认不出远端的错误码。
- sshd 把会话里的进程放进一个 Job，shell 一退出整棵树都死；客户端断开时则一个都不收。
- 测试机空闲 10 分钟就睡，经 ssh 进来的流量不算活动。

## 怎么测的

- **测试机：** tailnet 里的 `win11`，账号 `xdw`。
  - 一开始用用户在对话里给的密码登录：本机 ssh 走 `SSH_ASKPASS`，加 `PreferredAuthentications=password`、`PubkeyAuthentication=no`。密码没进仓库。
  - 2026-10-25 用户自己放好了钥匙：本机 `~/.ssh/config` 加了 `Host win11`，钥匙是 `~/.ssh/id_ed25519_win11`；测试机上放在 `C:\ProgramData\ssh\administrators_authorized_keys`。最后那轮 P106.1 用的就是钥匙。
- **ssh 参数：** 照 [ssh.rs](../../crates/ccnm-core/src/ssh.rs) 的 `command()` 加 `transport_options()`（传输连接），以及 `command()` 加 `options()`（控制命令），一项项抄过来。用密码那几轮唯一的改动是把 `BatchMode=yes` 换成密码登录；换成钥匙后就是原样的参数。
- **测试程序：** [p106-windows-probe.cs](probes/p106-windows-probe.cs)，在测试机上用 .NET Framework 自带的 `csc.exe` 编译，不用装任何东西。为什么不用 Rust：测试机有 cargo，但没装 MSVC 链接器（报 `linker link.exe not found`）；本机也没有 Windows 链接器。它有这几种模式：
  - `args`：打印收到的参数、`CCNM_TRANSPORT`、三个标准句柄的类型和继承位；
  - `echo`：stdin 原样抄回 stdout；
  - `sink`：读 stdin 直到结束，打印字节数和 sha256；
  - `source N`：往 stdout 写 N 字节；
  - `tree`、`spawnbg`、`injob`：P106.3 用的。
  - `sink-nt`、`sink-ov`、`echo-nt`、`echo-rs`、`echo-mt`、`mode`、`names`：2026-10-25 加的。用不同读写法读 stdin，并查标准句柄是不是异步、是不是同一个对象。见"换读法重测"一节。
  - `awake N`、`awake-check`：运行期间请求系统别睡。
- **本机驱动：** [p106-stdin-driver.py](probes/p106-stdin-driver.py) 负责喂数据、限时、比对；[p106-long-run.py](probes/p106-long-run.py) 跑一小时长连接。

## 测试机环境（P106.4 已测的部分）

| 项 | 实测 |
| --- | --- |
| 系统 | Windows 11 Pro for Workstations，build 26200（`10.0.26200`），x64；系统代码页 936（GBK），`[Console]::OutputEncoding` 是 gb2312 |
| 账号 | `win11\xdw`。在 `BUILTIN\Administrators` 组里，而且经 sshd 登录拿到的是 High Mandatory Level 令牌，等于已经提权的完整管理员 |
| sshd | 服务跑的是 `C:\WINDOWS\System32\OpenSSH\sshd.exe`（文件版本 9.5p2），在 session 0，开机自启；`ssh-agent` 服务是 Disabled。PATH 最前面是 scoop 装的另一份 OpenSSH（`D:\scoop\apps\openssh\current`，客户端 9.5p1），但服务没用它 |
| sshd_config 生效行 | `AuthorizedKeysFile .ssh/authorized_keys`、`Subsystem sftp sftp-server.exe`、`Match Group administrators` → `AuthorizedKeysFile __PROGRAMDATA__/ssh/administrators_authorized_keys`。这两个钥匙文件都还不存在。账号的 `.ssh` 下有 `id_rsa`、`work_id_rsa` 两把出站私钥，P111 的审计应该报出来 |
| 默认 shell | 注册表 `HKLM\SOFTWARE\OpenSSH\DefaultShell` 是 Windows PowerShell 5.1（`C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe`），`DefaultShellCommandOption` 为空。sshd 实际起的是 `powershell.exe -c "<命令>"`，进程树是 `sshd → powershell → 程序`，旁边还挂一个 conhost |
| 命令看到的环境 | `HOME=C:\Users\xdw`；`%TEMP%` 是 `C:\Users\xdw\AppData\Local\Temp` |
| Git / bash | git 2.52.0.windows.1 是 scoop 装的（`D:\scoop\apps\git\current`），Git Bash 在 `D:\scoop\apps\git\current\bin\bash.exe`；没有 `C:\Program Files\Git`。**PATH 上第一个 `bash` 是 `C:\Windows\system32\bash.exe`，那是 WSL 的启动器，不是 Git Bash** |
| `core.autocrlf` | 系统级（scoop git 的 `etc/gitconfig`）是 true，用户级是 false，最终生效 false |
| rg / curl / node | rg 是 scoop 的 shim（`D:\scoop\shims\rg.exe`）。Windows PowerShell 里敲 `curl` 其实是 `Invoke-WebRequest` 的别名，真的 curl 是 `C:\Windows\System32\curl.exe` 8.16.0（Schannel）。node、npx 是 mise 生成的 `.cmd` shim |
| 编译工具 | cargo 1.92.0（msvc），但没有 MSVC 链接器，vswhere 列不出任何 VS 产品；.NET Framework 4.8.1（Release 533509），自带 `csc.exe`；`dotnet` 没装 SDK |
| 8.3 短名 | C: 盘开着（`fsutil 8dot3name query C:` → ENABLED），所以设计文档 D5 的短名规则在这台机器上真用得上 |
| 磁盘 | 空余 C 26.9 GB，D 14.9 GB，E 14.2 GB |

## P106.1 连接和参数

- **U1（`SetEnv`）：** `SetEnv=CCNM_TRANSPORT=1` 不会让连接被拒，连接照常建立；但远端读不到这个变量，因为 sshd_config 里没有 `AcceptEnv`。结论：静默忽略。
- **U2（PowerShell 这一半）：**
  - ccnm 实际会发的命令 `C:/…/p106.exe args internal mcp-serve --payload eyJwIjoxfQ abc_-9` 原样到达，6 个参数都对。
  - 正斜杠写的程序路径能执行，PowerShell 会把它改写成带引号的反斜杠路径再起进程。程序路径不带 `.exe` 也行；用 `~/…` 开头也能执行，PowerShell 会展开 `~`。
  - 但 [`is_remote_safe`](../../crates/ccnm-core/src/ssh.rs) 允许的字符在 PowerShell 下不全安全：以 `@` 开头的参数（试的是 `@lead`）整个消失了，被 PowerShell 当成了 splatting（把变量展开成参数，这里变量不存在就什么都没剩）。`~/t` 当参数时原样传过去、不展开；`x,y` 和 `a-b_c.d/e=f:g@h+i,j~k` 这次都原样到达。
  - ccnm 现在发的参数只有子命令、`--payload` 和 base64url 载荷；载荷是 JSON 对象编码出来的，开头总是 `e`。所以现在碰不到上面这些坑，但"每种登录 shell 下意思都一样"这句注释对 PowerShell 不成立。
- **程序不存在时：** 退出码是 1，不是 127；stderr 是 `… is not recognized as the name of a cmdlet …`。ccnm 的 `classify` 只认退出码 127 和 "command not found" / "no such file or directory" 这两种说法，碰到这种情况会当成普通的远端失败报 Internal，而不是告诉用户"远端没装 ccnm"。
- **退出码：** PowerShell 自己 `exit 3`，回来是 3；原生程序退出 2，经 `powershell -c` 回来变成 1。这意味着在 PowerShell 当默认 shell 时，ccnm 分好的 CLI 退出码（1x、2x、3x）回来只剩 0 或 1。
- **U11（后量子警告）：** 本机 OpenSSH 10.3 连这台 9.5，每次都在 stderr 最前面打三行 `** WARNING: connection is not using a post-quantum key exchange algorithm.` 之类的话（CRLF 结尾），远端程序自己写的 stderr 排在它后面。[`ErrorCode::from_first_line`](../../crates/ccnm-core/src/error.rs) 只看第一行，所以远端 ccnm 报的 `CCNM_E_*` 会被认成 Internal。
  - `-o LogLevel=ERROR` 能压住这三行，`Permission denied (publickey,password,keyboard-interactive)` 照样会打出来（本机 10.3 实测）。老版本 ssh 也认这个选项，比 `WarnWeakCrypto=no` 稳妥。
  - 还没逐条核对：`never_reached` 依赖的另外几条 ssh 消息在 `LogLevel=ERROR` 下还在不在。
- **原样参数，带 `BatchMode=yes`（2026-10-25，用钥匙）：** 两套参数都照 ssh.rs 原样拼（`'SendEnv=-*'` 这类要加引号，免得被本地 shell 展开），连别名 `win11`。
  - 传输连接的形状（`… internal mcp-serve --payload eyJwIjoxfQ`）和控制命令的形状（`… internal runtime-resolve --payload eyJ3IjoieCJ9`），参数都原样到达。
  - 远端没有 ccnm 时退出码 1；stderr 先是三行后量子警告，后面才是 PowerShell 的 `is not recognized as the name of a cmdlet`。
  - 4 MiB 经传输参数跑 `echo-mt`，原样回来。
- **钥匙文件（P106.4）：** `C:\ProgramData\ssh\administrators_authorized_keys`，一行 ssh-ed25519。`icacls` 只有 `NT AUTHORITY\SYSTEM:(F)` 和 `BUILTIN\Administrators:(F)`，和[微软的要求](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_keymanagement)一致。`%USERPROFILE%\.ssh\authorized_keys` 不存在：管理员账号的钥匙放在那里，sshd 不读。
- 默认 shell 是 cmd 时的情况，见"DefaultShell 临时改成 cmd 重测"一节。

## P106.2 前半段：发现 stdin 卡住（2026-10-24）

| 方向 | 实测 |
| --- | --- |
| Windows → Mac | `source` 写 300 000 字节 0.7 秒、写 4 194 304 字节 1.1 秒，收完，sha256 一致 |
| Mac → Windows，`sink` | 300 000 字节试一次、4 MiB 试两次，40 秒内都没读到结尾 |
| Mac → Windows，`echo` | 1000 字节、70 000 字节都正常（0.5 秒）。60 000 字节全部回显了，但收不到结尾。300 000 字节试了两次，分别停在 16 384 和 69 120 字节。4 MiB 那次停在 65 536 字节，等了 5 分钟。停之前回来的部分都和输入前缀逐字节一致 |
| 对照：scp 上传 4 MiB | 2 秒传完，逐字节一致。scp 走 sftp 子系统。当时以为 sshd 是直接起 `sftp-server.exe`、不经过默认 shell；子代理读源码后说其实也经过（"上游的说法"一节），还没实测核对 |

数据是没送到，不是被别的进程读走了。卡住时看了进程的 I/O 计数：

- 三个卡住的会话里，powershell.exe 的 ReadTransferCount 都正好是 171 322。这是它启动时读文件的量，和喂进去多少数据无关，说明它没读 stdin。
- p106 读的字节数，正好等于 26 577（它自己启动时读文件的量）加上它已经回显出去的量。

2026-10-24 当时的判断是"和 PowerShell 当默认 shell 有关的可能性最大"。2026-10-25 在 cmd 下重测，结果推翻了这个判断，见"DefaultShell 临时改成 cmd 重测"一节。

这条不解决，P107 以后的阶段都没意义（设计文档里"U3 不成立就停下交用户定"那条）。

## P106.2 一小时长连接（2026-10-25）

- **怎么跑的：** 用 [p106-long-run.py](probes/p106-long-run.py)，经 ccnm 的传输参数连上去，远端跑 `p106.exe echo-mt awake`：读写各一个线程，运行期间请求系统别睡。节奏是：
  - 每次送随机 1–64 KiB，平均约 33 KB/s；
  - 第 20–25 分钟、第 40–45 分钟两段什么都不送，看空闲时连接还在不在；
  - 每分钟记一次进度；
  - 满 3600 秒后关 stdin，等远端回完。
- **结果：** Mac 时间 14:00:15 开始，15:01:16 结束，`rc=0`。
  - 两个方向各 98 343 832 字节（约 98 MB；目标写的是 100 MB，少 1.7%），逐段比对没有一处不一致，sha256 一致。
  - 远端自己报 `echo-mt done read=98343832 written=98343832`。
  - 两段空闲之后连接照常收发。
  - 61 次每分钟采样里，"送出去还没回来"的字节数有 59 次是 0，最大一次 28 126 字节。
- **结论：** U3 成立。Win32-OpenSSH 的标准输入输出撑得住一条连接跑一小时、几十 MB 的量、任意二进制，前提是程序读写分开、一直有线程在读 stdin。
- **顺带看到：** 测完先停了防睡眠进程，测试机当场就睡了（空闲计时早就超过 10 分钟），同一条命令里排在后面的清理没跑完。以后的顺序应该是先清理，最后再撤防睡眠。ccnm 会话结束时撤请求也会是这个效果：会话一结束机器可能马上睡，这正是想要的。

## P106.3 进程（U4、taskkill）

- **客户端断开时（本机 ssh 被杀）：** 这条连接的 sshd 子进程、powershell、p106 都还活着，40 秒后还在。驱动脚本超时后在本机杀掉的那些会话，远端也都留着。结论：sshd 不会替你收会话里的进程，ccnm 在 Windows 上得自己收。
- **`taskkill /T /F /PID <那个 powershell>`：** powershell、p106、conhost 三个一起结束，本机 ssh 随即退出（rc=1）。
- **会话里的进程在不在 Job 里（2026-10-25，`injob`）：** 在。限制标志是 `0x2800`，也就是 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`（Job 句柄一关，里面的进程全部结束）加 `JOB_OBJECT_LIMIT_BREAKAWAY_OK`（允许子进程带 `CREATE_BREAKAWAY_FROM_JOB` 脱离）。这解释了上面"断开后进程还在"：Job 句柄在这条连接的 sshd 子进程手里，客户端断了它也没退，Job 就一直没关。Windows 8 起允许嵌套 Job，ccnm 以后要用 Job Object（设计文档 D3 第二步）不会被这个挡住。
- **经 WMI 起的进程（`Win32_Process.Create`）：** 父进程是 WmiPrvSE，不在 sshd 的 Job 里，会话结束后照样活着；令牌一样是已提权的管理员（带 `NT AUTHORITY\NETWORK`）。下面"改回 DefaultShell 的保险"就是靠这个起的。
- **`taskkill /T /F` 收进程树（2026-10-25）：** 三层树经 WMI 起（不在 sshd 的 Job 里，结果只反映 taskkill 自己）。
  - 树完整时，对根进程 `taskkill /T /F`，三层连同各自的 conhost 共 6 个进程全部结束。
  - 中间那层起完孩子就退出时，同样对根进程执行，孙进程活下来了：taskkill 是顺着父进程号找后代的，中间断了就找不到。和设计文档 D3 写的局限一致。
- **继承标准句柄会不会让连接挂住（2026-10-25，`spawnbg`）：** 程序起一个 20 秒的后台子进程（三路都重定向成管道，但 `bInheritHandles=TRUE`）就退出。
  - 不改句柄（`inherit`）和先把三个标准句柄改成不可继承（`noinherit`）两种情况，ssh 都在 1 秒左右返回，后台子进程也都已经死了。
  - 原因是 shell 一退出，sshd 那个 Job 就关了，`KILL_ON_JOB_CLOSE` 把里面所有进程一起结束，后台进程也在里面。
  - 所以经 sshd 跑时不会挂。gld 在 Windows CI 上挂过 20 分钟，那里没有 sshd 的 Job。ccnm 在 Windows CI 上跑集成测试，或者在 Windows 本机直接起 mcp-serve 时，仍会碰到同样的问题。
  - 另一个后果：mcp-serve 一退出，它起的后台命令全部跟着死。这和 macOS、Linux 上"mcp-serve 被强杀后后台命令还活着"（[core-hardening.md](../plan/core-hardening.md) 的 AUTH-02）不一样。

## P106.4 其余环境项（2026-10-25）

- **`curl.exe` 和 ccnm 的 `-K` 配置（U8）：** 照 [curl.rs](../../crates/ccnm-core/src/mcp/curl.rs) 生成的那份配置原样写了一份（`url`、`request = "POST"`、`silent`、`show-error`、`no-buffer`、`suppress-connect-headers`、`dump-header = "-"`、`proto`、`max-time`、`connect-timeout`、`noproxy`、`data-binary = "@-"`，以及几行 `header`，其中一行带转义的引号和反斜杠），拿 `C:\Windows\System32\curl.exe` 8.16.0 去请求测试机本地的一个 SSE 服务（PowerShell 的 HttpListener，只听 127.0.0.1）。
  - 请求体从 stdin 读进去了，服务端收到 40 字节；User-Agent 对得上。
  - 响应头打在 stdout 上，`Mcp-Session-Id` 在里面。
  - 两个 SSE 事件分别在 0.1 秒和 3.1 秒到，确实是流式的，不是攒到最后。curl 退出码 0。
  - 结论：Windows 上可以照用 curl。要改的只有 Unix 那部分：建私有目录、配置文件时用的权限位（`mode(0o700)`、`mode(0o600)`），设计文档 D4 已经写了。
- **Git Bash 在 sshd 下（U5）：** 用 `D:\scoop\apps\git\current\bin\bash.exe` 跑一个脚本：
  - `HOME=/c/Users/xdw`，起始目录也是这里；`MSYSTEM=MINGW64`；PATH 前面加了 `/mingw64/bin`、`/usr/bin`，后面接 Windows 的 PATH。
  - git 用的是 Git for Windows 自带的 `/mingw64/bin/git`；rg、cargo、node 都找得到；`python` 找到的是 `WindowsApps` 里的应用商店占位程序。
  - **`SHELL` 是 sshd 设的 PowerShell 路径**：脚本里用 `$SHELL -c …` 起子 shell 的工具，起来的会是 PowerShell，不是 bash。
  - UTF-8 字节原样送回来了；`cmd.exe //c chcp` 显示代码页 936。
  - 退出码 7：经 PowerShell 当默认 shell 时，要 `exit $LASTEXITCODE` 才拿得到 7，不加就变成 1。ccnm 是自己起 bash，不经过默认 shell，所以不受这个影响。
- **sftp 也经过默认 shell（核对子代理的说法）：** scp 上传 32 MB 的过程中看进程树，是 `powershell.exe -c "sftp-server.exe "` 起的 `sftp-server.exe`。因为 sshd_config 里写的是相对名，实际按 PATH 找到的是 scoop 那份（`D:\scoop\apps\openssh\current\sftp-server.exe`），不是系统自带的。scp 能正常传，是因为 sftp-server 自己读法对。

## DefaultShell 临时改成 cmd 重测（2026-10-25，用户同意）

**怎么改、怎么保证能改回来：**

1. 先记原值：`DefaultShell` 是 `C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe`，`DefaultShellCommandOption` 和 `DefaultShellEscapeArguments` 都为空。
2. 经 WMI 起一个保险脚本：每 30 秒看一次租约文件，10 分钟没被续就把 `DefaultShell` 写回原值。测试期间每批测试前续一次租约。
   - 只能经 WMI 起，因为 ssh 会话里起的进程会随会话的 Job 一起被杀。
   - 机器重启的话保险也会没了，这时要手动改回。
3. 再把 `DefaultShell` 改成 `C:\Windows\System32\cmd.exe`。新连接里 `ver` 能跑，`COMSPEC` 是 cmd，改动立即生效，不用重启 sshd。

**顺带踩到的坑：** 保险脚本第一版带中文注释，Windows PowerShell 5.1 读没有 BOM 的 `.ps1` 时按系统代码页（这台是 936，也就是 GBK）解码，注释末尾的 UTF-8 字节把换行吞掉了，下一行赋值整个变成了注释。解析器不报任何错，变量就是空的。模型以后经 `exec_command` 跑 `.ps1` 也会遇到同一个问题：脚本要么只用 ASCII，要么存成带 BOM 的 UTF-8。

**cmd 下的结果：**

| 项 | PowerShell 下（上面几节） | cmd 下 |
| --- | --- | --- |
| ccnm 实际发的命令 | 原样到达 | 原样到达。cmd 把命令行原样交给程序，`cmdline` 里还是正斜杠 |
| 以 `@` 开头的参数 | 消失 | 原样到达。`,`、`=`、`~` 也都原样 |
| `~/…` 当程序路径 | 能执行 | 不能：`'~' is not recognized as an internal or external command` |
| 程序路径不带 `.exe` | 能执行 | 能执行 |
| 程序不存在 | 退出 1，`… is not recognized as the name of a cmdlet …` | 退出 1，`'…' is not recognized as an internal or external command, operable program or batch file.` |
| 原生程序退出 2 | 回来是 1 | 回来是 2 |
| `SetEnv` | 静默忽略 | 静默忽略 |
| stdin，`sink` | 300 KB、4 MiB 都卡 | 一样卡：300 KB 一次、4 MiB 三次，40 秒都没读到结尾 |
| stdin，`echo` | 1000 字节、70 000 字节正常，其余卡 | 1000 字节正常；60 000 字节全部回显但收不到结尾；70 000 字节一个字节都没回来 |

接着连接断了：先是 `Read from remote host …: Operation timed out`，然后 22 端口连不上，Tailscale 显示测试机离线。

**结论：** stdin 卡住和 PowerShell 无关，cmd 下一样卡。退出码和参数这两件事，cmd 比 PowerShell 好：退出码原样回来，参数也不会被吃掉。

## 上游的说法（子代理 2026-10-25 查的，没在测试机上复核）

| 说法 | 依据 | 把握 |
| --- | --- | --- |
| sshd 给子进程 stdin 的是一根带 `FILE_FLAG_OVERLAPPED` 的命名管道（异步句柄，缓冲 4096 字节），子进程拿到的是这个句柄的副本 | [`contrib/win32/win32compat/fileio.c` 第 216–237 行](https://github.com/PowerShell/openssh-portable/blob/v9.5.0.0/contrib/win32/win32compat/fileio.c#L216-L237)、同目录 `w32fd.c` 第 1263 行；子代理说 10.4 预览版这段没改过 | 源码确认 |
| .NET Framework 程序从这种 stdin 读会永久卡住；Windows PowerShell 5.1 当远程服务端时，收几个包就不再收，没修 | Win32-OpenSSH [#1314](https://github.com/PowerShell/Win32-OpenSSH/issues/1314)、[#1938](https://github.com/PowerShell/Win32-OpenSSH/issues/1938)，PowerShell [#14478](https://github.com/PowerShell/PowerShell/issues/14478) | issue 确认 |
| 微软文档：对异步句柄做同步读，"can incorrectly report that the read operation is complete" | [ReadFile 文档](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfile) | 文档确认 |
| sftp、exec、ForceCommand 都经过 DefaultShell，没有官方的绕过办法；`DefaultShellCommandOption` 只决定开关是 `-c` 还是 `/c` | `contrib/win32/win32compat/w32-doexec.c` 第 434–471 行，Win32-OpenSSH [#2430](https://github.com/PowerShell/Win32-OpenSSH/issues/2430) | 源码确认，还没在测试机上看进程树核对 |
| PowerShell 5.1 只有在命令里用到 `$input`，或者是 `-Command -` / `-File -` 时才读 stdin；普通原生程序拿到的是同一个句柄 | PowerShell 6 alpha 的 `Executor.cs`、`ConsoleHost.cs`（5.1 不开源，6 alpha 和它同源） | 中高，和上面 ReadTransferCount 不变对得上 |
| 原生程序退出码不是 0 或 1 时，PowerShell 5.1 会改成 1；要保留就写 `exit $LASTEXITCODE` | [about_powershell_exe（5.1）](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_powershell_exe?view=powershell-5.1) | 文档确认，和实测一致 |
| 客户端断开不会结束子进程，至今没修；sshd 只把 Job 交给 shell，shell 退出时整棵树才结束；`fileio_close` 会无限期等没写完的异步写 | Win32-OpenSSH [#1751](https://github.com/PowerShell/Win32-OpenSSH/issues/1751)，`w32-doexec.c` 第 503–527 行，`fileio.c` 第 1089–1103 行 | issue 和源码确认，和实测一致 |

Rust 标准库读 stdin 走的是 `Handle::synchronous_read`：`NtReadFile` 不带事件，挂起时等句柄本身完成，见本机 Rust 1.85 源码 `std/src/sys/pal/windows/handle.rs` 第 251–277 行。这是专门为异步句柄写的。所以 ccnm 会不会遇到同样的卡顿，要拿 Rust 的读法实测，不能拿 .NET 测试程序的结果推。

## 换读法重测（2026-10-25，DefaultShell 已经写回 PowerShell）

DefaultShell 已经由保险脚本写回原值：测试机是睡眠，不是重启，醒来后保险发现租约过了 2245 秒，就写回了。stdin 卡住和默认 shell 无关，所以这一轮就在 PowerShell 下测，没再动注册表。

| 测什么 | 结果 |
| --- | --- |
| `mode`：三个标准句柄的 FileModeInformation | 都是 `0x0`，也就是三个都是异步句柄。子代理读源码说的对上了 |
| `sink-nt`：照搬 Rust std 的读法 | 300 KB 一次、4 MiB 三次，全部读完，sha256 一致，4 MiB 约 1.6 秒 |
| `sink-ov`：OVERLAPPED 加事件 | 300 KB 一次、4 MiB 两次，全部读完，sha256 一致 |
| `echo-nt`：Rust 读法读，.NET 写 | 1000、60 000、70 000、300 000 字节都对；4 MiB 停在 2 595 328 字节 |
| `echo-rs`：读写都照搬 Rust std，单线程读一块写一块 | 300 KB 停在 129 536 字节，4 MiB 停在 2 328 576 字节 |

结论分三层：

- **"送不进去"是 .NET 同步读法的问题，不是管道本身的问题。** 只读的话，Rust 的读法读 4 MiB 都没事。
- **单线程"读一块、写一块"在双向大流量下会卡。** 读写都换成 Rust 的办法也一样。像是两头互相等：管道缓冲只有 4 KB，程序写 stdout 被堵住就不再读 stdin，sshd 往 stdin 写被堵住也不再收 stdout。这只是推断，没证实。
- **ccnm 的 mcp-serve 不是单线程读写。** 它一个线程读 stdin，另一个线程写 stdout。照这个结构测了 `echo-mt`：读线程只管读、塞进队列，写线程只管写，读写都用 Rust std 的办法。
  - 300 KB、4 MiB 三次、16 MiB、64 MiB 全部原样回来（64 MiB 用了 37.7 秒）。
  - 同一轮里单线程的 `echo-rs` 对照组照样卡在 2 261 504 字节。
- **stdin、stdout、stderr 是三根不同的管道**（`names`）：stdout 叫 `\W32PosixPipe.00002dac.00000005`，stderr 叫 `…00000006`，`CompareObjectHandles` 说 stdin 和 stdout 不是同一个对象。所以两个线程各等各的句柄，不会混。

由此得出对 ccnm 的要求：**Windows 上 mcp-serve 必须一直有线程在读 stdin，不能因为 stdout 写不出去就停下不读。** 卡住的那种模式，可能是 Win32-OpenSSH 在子进程不读 stdin 时也停止收 stdout（推断，没证实）。这一条要在 P107/P110 拿 ccnm 真二进制验证：rmcp 的读循环会不会因为处理或输出积压而暂停读 stdin。

**测试机会自己睡：** 接电源时空闲 600 秒（10 分钟）就睡（`powercfg /q` 的 STANDBYIDLE），经 ssh 进来的流量不算活动。前两次"离线"都是这个原因。

这对 ccnm 是个实际问题：模型跑长命令，或者停下来想一会儿，Windows Runtime 都可能睡过去，连接跟着断。

能用的办法是程序运行期间调 `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`，告诉系统"我在用，别睡"。实测从 ssh 会话里调就有效：`powercfg /requests` 的 SYSTEM 一栏列出了 `[PROCESS] …\p106.exe`。进程一退出，请求就没了，不改任何电源设置。ccnm 要这么做也得调 Windows API（toexec 那个 crate 里），或者在安装文档里叫用户关掉自动睡眠。

## 留下了什么

2026-10-25 17:25（Mac 时间）测完后的状态：

- **测试机上：**
  - 临时目录 `C:\Users\xdw\AppData\Local\Temp\ccnm-p106\` 已整个删掉，里面放过测试程序、保险脚本、日志和上传测试用的文件。
  - 没有残留的测试进程，`powercfg /requests` 里也没有它的请求。
  - `DefaultShell` 是原来的 Windows PowerShell 5.1。
  - 用户放的钥匙文件 `C:\ProgramData\ssh\administrators_authorized_keys` 保留，以后的阶段要用。
  - 试编译用的 cargo 项目在 10-24 就删了。
- **本机：**
  - `~/.ssh/known_hosts` 多了测试机的主机钥匙（第一次连时用的 `StrictHostKeyChecking=accept-new`），用户配的 `Host win11` 也用得上它。
  - 登录用的密码文件和用密码登录的脚本，换成钥匙后就删了；测试脚本留在会话临时目录里，不进仓库。
