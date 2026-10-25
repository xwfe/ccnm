# P106 Windows 预研记录（2026-10-24，没做完）

阶段范围见 [ROADMAP](../plan/ROADMAP.md) 的 P106，未知项编号（U1–U11）见[设计文档](../plan/windows-runtime.md)第 6 节。

**一句话结论：** 从 Mac 往 Windows 送 stdin 会卡住。这台测试机的默认 shell 被改成了 PowerShell；在这种配置下，经 ssh 执行的命令收 300 KB 就收不到结尾。ccnm 的 MCP 请求全从 stdin 进，所以照现在的样子，Windows Runtime 在这台机器上用不了。反方向（Windows 往 Mac 送 stdout）没问题；不经过默认 shell 的 scp 上传也没问题。默认 shell 换成 cmd 还没测。测到一半测试机离线了，P106.2–P106.4 都没做完。

## 怎么测的

- **测试机：** tailnet 里的 `win11`，账号 `xdw`，用户在对话里给的密码；密码没进仓库。本机没给这台机器配钥匙，所以全程用密码登录：本机 ssh 走 `SSH_ASKPASS`，加 `PreferredAuthentications=password`、`PubkeyAuthentication=no`。
- **ssh 参数：** 照 [ssh.rs](../../crates/ccnm-core/src/ssh.rs) 的 `command()` 加 `transport_options()` 一项项抄过来，唯一的改动是把 `BatchMode=yes` 换成密码登录。所以 BatchMode 本身没测到。
- **测试程序：** [p106-windows-probe.cs](probes/p106-windows-probe.cs)，在测试机上用 .NET Framework 自带的 `csc.exe` 编译，不用装任何东西。为什么不用 Rust：测试机有 cargo，但没装 MSVC 链接器（报 `linker link.exe not found`）；本机也没有 Windows 链接器。它有这几种模式：
  - `args`：打印收到的参数、`CCNM_TRANSPORT`、三个标准句柄的类型和继承位；
  - `echo`：stdin 原样抄回 stdout；
  - `sink`：读 stdin 直到结束，打印字节数和 sha256；
  - `source N`：往 stdout 写 N 字节；
  - `tree`、`spawnbg`、`injob`：P106.3 用的，已经编好，还没跑。
- **本机驱动：** [p106-stdin-driver.py](probes/p106-stdin-driver.py)，负责喂数据、限时、比对。

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
- **没测：** `BatchMode=yes`（要先放钥匙）；默认 shell 是 cmd 时的情况（要改注册表）。

## P106.2 长连接（U3）：发现阻断问题，一小时那轮没跑

| 方向 | 实测 |
| --- | --- |
| Windows → Mac | `source` 写 300 000 字节 0.7 秒、写 4 194 304 字节 1.1 秒，收完，sha256 一致 |
| Mac → Windows，`sink` | 300 000 字节试一次、4 MiB 试两次，40 秒内都没读到结尾 |
| Mac → Windows，`echo` | 1000 字节、70 000 字节都正常（0.5 秒）。60 000 字节全部回显了，但收不到结尾。300 000 字节试了两次，分别停在 16 384 和 69 120 字节。4 MiB 那次停在 65 536 字节，等了 5 分钟。停之前回来的部分都和输入前缀逐字节一致 |
| 对照：scp 上传 4 MiB | 2 秒传完，逐字节一致。scp 走 sftp 子系统，sshd 直接起 `sftp-server.exe`，中间不经过默认 shell |

数据是没送到，不是被别的进程读走了。卡住时看了进程的 I/O 计数：

- 三个卡住的会话里，powershell.exe 的 ReadTransferCount 都正好是 171 322。这是它启动时读文件的量，和喂进去多少数据无关，说明它没读 stdin。
- p106 读的字节数，正好等于 26 577（它自己启动时读文件的量）加上它已经回显出去的量。

现在的判断：问题出在"经默认 shell 执行命令"这条路上，和 PowerShell 当默认 shell 有关的可能性最大，但还没证实。可能的下一步：

1. 把默认 shell 临时改回 cmd.exe（Windows 的出厂值）重测。要改注册表、要授权，测完改回去。
2. 绕开默认 shell：在 sshd_config 里加一个 `Subsystem`。要改 sshd 配置，ccnm 也要改成从 stdin 传载荷。
3. 查上游有没有人报过同样的问题。

这条不解决，P107 以后的阶段都没意义（设计文档里"U3 不成立就停下交用户定"那条）。

## P106.3 进程（U4、taskkill）：做了一部分

- **客户端断开时（本机 ssh 被杀）：** 这条连接的 sshd 子进程、powershell、p106 都还活着，40 秒后还在。驱动脚本超时后在本机杀掉的那些会话，远端也都留着。结论：sshd 不会替你收会话里的进程，ccnm 在 Windows 上得自己收。
- **`taskkill /T /F /PID <那个 powershell>`：** powershell、p106、conhost 三个一起结束，本机 ssh 随即退出（rc=1）。
- **没测：** 父进程先退出的孙进程能不能收到（`tree … orphan`）；进程是不是在某个 Job 里（`injob`）；继承了标准句柄会不会让连接挂住（`spawnbg inherit` / `noinherit`）。程序都编好了。

## P106.4 没测的

- `curl.exe` 认不认 `-K` 配置文件、能不能读 SSE（U8）。
- Git Bash 在 sshd 下跑起来是什么样（U5 剩下的部分）。

## 留下了什么

- **测试机上：** `C:\Users\xdw\AppData\Local\Temp\ccnm-p106\`，里面有 `p106.cs`、`p106.exe` 和 4 MiB 的 `up.bin`。离线前最后三个 `sink` 会话没来得及收，可能还挂着；机器重启过的话就没了。试编译用的 cargo 项目已经删了。
- **本机：** `~/.ssh/known_hosts` 多了测试机的主机钥匙（第一次连时用的 `StrictHostKeyChecking=accept-new`）。会话临时目录里有测试脚本和登录用的密码文件，不进仓库。
- **测试机状态：** 2026-10-25 再看时 Tailscale 显示它离线（`last seen 11h ago`），22 端口连不上。
