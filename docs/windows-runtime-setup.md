# 把 Windows 当放项目的机器

本页从零搭一台 Windows 的 Runtime Node（放项目、跑工具的那台）。跑 AI 的那台（Agent Node）照旧是 macOS 或 Linux，按[快速开始](getting-started.md)装好；Windows 不能当跑 AI 的那台，也不能在 Windows 上敲 `ccnm <项目>` 起会话。

> **现状：** 2026-10-28 在一台 Win11（中文系统）上真机验收过（P112），真实的 Claude Code 和 Codex 都在上面做完过改代码、编译、跑测试、提交；验到哪一步、哪些没验，以[支持矩阵](support-matrix.md)里"Windows 当项目机器"那一段为准。本页第 4–6 步在那台机器上照着走过（[P111 记录](research/2026-10-28-p111-windows-config-doctor.md)第 4 节）；第 1–3 步那台机器早就装好了，没有重做，winget 那两条也没在那儿跑过。

下面的命令分两种：标了"在 Windows 上"的，在 Windows 的 **PowerShell** 里敲；标了"在 AI 那台"的，在 macOS / Linux 的终端里敲。

## 先准备

- Windows 10 1809 以后，或 Windows 11，x86_64。
- 一个本地账号或微软账号，**用户名里没有空格**。有空格（`C:\Users\Zhang San`）的话，ccnm.exe 得放到别的没有空格的目录，见[第 4 步](#4-装-ccnm)。
- AI 那台已经装好 ccnm，版本和要装到 Windows 上的一样。

## 1. 装 OpenSSH Server

AI 那台是经 SSH 连过来的，用 Windows 自带的 OpenSSH Server。**在 Windows 上，用"以管理员身份运行"打开的 PowerShell：**

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Start-Service sshd
Set-Service -Name sshd -StartupType Automatic
Get-NetFirewallRule -Name *OpenSSH-Server* | Select-Object Name, Enabled
```

最后一条要列出一条 `Enabled` 是 `True` 的规则（装的时候自动建的，放行 22 端口）。没有的话，22 端口被防火墙挡着，AI 那台连过来会一直等到超时。微软的原始说明：[安装 OpenSSH](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse)。

**不用改默认 shell。** sshd 默认用 cmd.exe 跑每条命令，ccnm 按这个设计。改成 PowerShell 也能用，doctor 会多一行"注意"说明影响，见[第 6 步](#6-在-ai-那台跑-doctor)。

## 2. 放钥匙

AI 那台用 SSH 钥匙登录，不能要密码。先看这个账号是不是管理员——**钥匙放哪取决于这个**：

```powershell
whoami /groups | findstr S-1-5-32-544
```

**打出一行**：是管理员。sshd 只读 `C:\ProgramData\ssh\administrators_authorized_keys`，而且这个文件只能给 Administrators 和 SYSTEM 两个组有权限。在管理员 PowerShell 里，把 AI 那台的公钥（`~/.ssh/xxx.pub` 的那一行）放进去：

```powershell
Add-Content -Force -Path "$env:ProgramData\ssh\administrators_authorized_keys" -Value '<公钥那一整行>'
icacls.exe "$env:ProgramData\ssh\administrators_authorized_keys" /inheritance:r /grant "*S-1-5-32-544:F" /grant "*S-1-5-18:F"
```

`*S-1-5-32-544`、`*S-1-5-18` 是 Administrators 和 SYSTEM 的 SID，中文 Windows 上组名不一样，写 SID 不受影响。

**什么都没打出**：是普通账号，钥匙放在自己的 `%USERPROFILE%\.ssh\authorized_keys`，不用管理员。

**放错了会怎样：** 管理员账号的钥匙放进了 `%USERPROFILE%\.ssh\authorized_keys`，或者那个文件权限不对，AI 那台连过来报 `Permission denied (publickey,...)`，看着像钥匙不对，其实是放错了文件。微软的说明：[密钥管理](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_keymanagement)。

**在 AI 那台**，`~/.ssh/config` 加一段（别名随便起，下面都叫 `win`）：

```
Host win
  HostName <Windows 的地址>
  User <Windows 的账号名>
  IdentityFile ~/.ssh/<那把钥匙>
```

试一下，不能问密码：

```bash
ssh -o BatchMode=yes win whoami
```

## 3. 装 Git for Windows 和 ripgrep

- **Git for Windows**：模型的 `shell` 命令在它带的 Git Bash 里跑（和 Claude Code 在 Windows 上的做法一样），`cmd` 里写的 `bash` 也是它。没装的话，`shell` 报"没找到 Git for Windows 的 bash"，只能用 `cmd` 直接跑程序。
- **ripgrep**（`rg`）：模型搜代码靠它。没装的话，第一次搜代码时报 `ripgrep is not installed on the Runtime Node…`。

用 winget 装最省事（在 Windows 上）：

```powershell
winget install --id Git.Git -e
winget install --id BurntSushi.ripgrep.MSVC -e
```

用 scoop 装的也行：ccnm 会从 PATH 上的 `git.exe` 往上找到那份 Git 的 bash。都找不到时，在 Windows 的配置里写 [`git_bash`](configuration.md#git_bash顶层只给-windows-项目机器)。

装完**从 AI 那台**确认，要打出路径来（sshd 给每次连接重新读一遍 PATH，你在 Windows 上新开的窗口里能用不算数）：

```bash
ssh win where.exe git rg
```

## 4. 装 ccnm

去 [Releases](https://github.com/xwfe/ccnm/releases) 下载和 AI 那台同一个版本的 `ccnm-<版本号>-windows-x86_64.zip` 和 `.sha256`。在 Windows 上，在下载目录里：

```powershell
$v = '<版本号>'
Get-FileHash ".\ccnm-$v-windows-x86_64.zip"                 # 和 .sha256 文件里那串比一下
New-Item -ItemType Directory -Force "$env:USERPROFILE\.local\bin" | Out-Null
Expand-Archive ".\ccnm-$v-windows-x86_64.zip" -DestinationPath "$env:USERPROFILE\.local\bin" -Force
Unblock-File "$env:USERPROFILE\.local\bin\ccnm.exe"
& "$env:USERPROFILE\.local\bin\ccnm.exe" --version         # 要和 AI 那台一样
```

`Unblock-File` 去掉浏览器给下载文件打的"来自网络"标记。实测（设计文档的 U7）：Windows PowerShell 5.1 的 `Expand-Archive` 不会把 zip 上的标记带到 `ccnm.exe` 上，资源管理器的"全部解压缩"会；就算 `ccnm.exe` 带着标记，经 SSH 照样能起来。去掉它没有坏处，以后在资源管理器里双击也不会弹警告。

想在 Windows 上直接敲 `ccnm`，把这个目录加进自己的 PATH，然后新开一个 PowerShell：

```powershell
$user = [Environment]::GetEnvironmentVariable('Path', 'User')
[Environment]::SetEnvironmentVariable('Path', "$user;$env:USERPROFILE\.local\bin", 'User')
```

**用户名里有空格的**：放到一个路径里没有空格的目录，比如 `C:\ccnm\ccnm.exe`（在 C 盘根目录建文件夹可能要管理员）。下一步 `init` 会说这个路径能不能用。

## 5. 初始化这台

在 Windows 上：

```powershell
ccnm init --agent <AI 那台的别名>
ccnm add D:\work\myproj
```

`--agent` 后面写什么名字都行，只是在配置里给 AI 那台起个名字：Windows 不会连过去。配置写在 `%USERPROFILE%\.config\ccnm\config.toml`，状态在 `%USERPROFILE%\.local\state\ccnm\`，和 macOS、Linux 是同一个相对位置。

`init` 最后会打出 AI 那台要写的一行，形如：

```
  ccnm_bin = "C:/Users/me/.local/bin/ccnm.exe"
```

记下来，下一步要用。**为什么要写这一行：** sshd 用 cmd.exe 跑命令，cmd 不认 `~`，AI 那台默认按 `~/.local/bin/ccnm` 找，在 Windows 上就是"找不到程序"。只能写全路径，用正斜杠、带 `.exe`；写成 `C:\Users\...` 会被拒。

`ccnm add` 存的根目录也是正斜杠的写法（`D:/work/myproj`）。

## 6. 在 AI 那台跑 doctor

**在 AI 那台：**

```bash
ccnm init --runtime win
```

然后打开 `~/.config/ccnm/config.toml`，在 `[nodes.runtime]` 下面加上一步记下的那行：

```toml
[nodes.runtime]
ssh = "win"
ccnm_bin = "C:/Users/me/.local/bin/ccnm.exe"
```

再跑：

```bash
ccnm doctor myproj
```

Windows 那台的几行通常是这样：

| 行 | 一般是 | 说明 |
| --- | --- | --- |
| Runtime 执行身份 | 注意 | 没写 `runtime_user`，命令以你连过去的账号跑，那个账号能碰到的东西模型的命令都能碰到。Windows 上只支持这种共用账号 |
| 管理员令牌 | 管理员账号是注意 | 管理员用钥匙经 sshd 登录，拿到的就是提权后的令牌：模型的每条命令都以管理员权限跑，不弹确认 |
| admin 组 | 管理员账号是注意 | 在 Administrators 组里 |
| SSH 私钥 | `.ssh` 里有私钥就是注意 | 模型的命令能读到它、用它连出去 |
| SSH agent 服务 | 一般是正常 | OpenSSH Authentication Agent 服务在跑的话，加载进去的钥匙本账号任何进程都能用，不经过任何文件或环境变量 |
| Claude 凭据、Codex 凭据 | 一般是正常 | 这台上有 Claude Code 或 Codex 的登录文件、模型的命令读得到时是注意 |
| 认证环境变量 | 正常 | 环境里有 `ANTHROPIC_*` 这类认证变量时是失败，不能豁免 |
| Docker 权限 | 一般是正常 | 在 docker-users 组里就能驱动 Docker，相当于管理员 |
| Runtime 的 shell | cmd.exe 是正常 | 见下 |

"注意"不拦东西，只是说清楚。想收紧，就让 AI 那台用一个**不在 Administrators 组里**的 Windows 账号连过来（钥匙放在那个账号自己的 `.ssh\authorized_keys`）。Windows 上暂不支持 `runtime_user` 那种专用账号模式：在 Windows 的配置里写了它，读配置时直接报错，说清楚怎么改。

**Runtime 的 shell 那一行：** sshd 的默认 shell 被改成了 PowerShell 时是"注意"。ccnm 自己的命令照样能用，但经这条 SSH 跑的别的程序，退出码只剩 0 或 1，以 `@` 开头的参数会被吞掉。AI 那台的 `ccnm_bin` 还写着 `~/` 时，这一行会点明它只是因为 PowerShell 才碰巧能用，并给出该写的全路径。

表里还有 AI 那台自己的几行（Controller、Claude 登录、终端会话），和 Windows 无关，按[快速开始](getting-started.md#5-跑-doctor)看。都没有"失败"，就可以 `ccnm myproj` 开始用了。

## 用起来和 macOS、Linux 不一样的地方

- **机器会自己睡。** Windows 接电源时默认空闲一段时间就睡，经 SSH 进来的流量不算"在用"。ccnm 只在会话期间请求系统别睡，会话一结束就撤。所以两次会话之间机器可能睡着、连不上：要么在电源设置里把"接通电源时睡眠"改成"从不"，要么用网络唤醒。
- **命令打开的图形窗口你看不见。** sshd 是系统服务，它起的程序在后台会话里。编译、跑测试、命令行工具不受影响。
- **会话一结束，它起的后台命令全部跟着结束**，被强杀也一样。macOS、Linux 上后台命令会活过 mcp-serve。
- **`.ps1` 脚本里有中文，要存成带 BOM 的 UTF-8。** Windows PowerShell 5.1 按系统代码页（中文 Windows 是 GBK）读没有 BOM 的 `.ps1`：中文注释会把下一行吞掉，而且不报任何错。模型写的脚本也一样。
- **原生命令行工具输出的中文是 GBK**，ccnm 不转码，模型看到的是替换字符；Git Bash 里的程序输出 UTF-8，不受影响。
- **沙箱（`exec_sandbox`）开不了**，开了会话直接被拒。
- **`ccnm cleanup` 对 Windows 上的项目用不了**：AI 那台会说"去定义它的那台跑"，而 Windows 上不让跑这个命令（它要从 Windows 连 AI 那台）。旧的命令输出照样会自己删：最后一次运行过去 7 天、没有会话在用的，下次有会话连进来时删掉（[运维手册](operations.md)"Runtime 上的 `output/` 例外，它自己清"那段）。
- **在 Windows 上 `ccnm rm` 会先试着去 AI 那台停掉这个项目的会话**，连不上就只删配置。连不上时最多要等 60 秒：实测那台的 ssh 客户端碰到连不上的别名一直挂着，不认 `ConnectTimeout`，是 ccnm 自己的 60 秒把它停下的。先在 AI 那台 `ccnm stop <项目>`，再在 Windows 上 `ccnm rm`，就不用等。
- **模型在 `cmd` 里写的 `bash` 是 Git Bash，不是 PATH 上第一个 `bash`**（v0.21.1 起）。装了 WSL 的机器上，PATH 上排第一的 `bash` 是 `C:\Windows\System32\bash.exe`，那是 WSL 的启动器；ccnm 把不带路径的 `bash`、`bash.exe` 换成 `shell` 用的那个 Git Bash，结果里多一行 `bash ran as Git for Windows' bash (…), not as the first bash on PATH (…, which starts WSL)`。真要跑 WSL 的，写全路径或用 `wsl.exe`。v0.21.0 没有这一步，`["bash", "build.sh"]` 会进 WSL：构建报找不到 `javac`、`cargo` 之类，WSL 里要是有 mise 之类会自动装工具的东西，还会顺手装一堆（验收时一次装了 233 MB）。还在用 v0.21.0 的，在项目的 `CLAUDE.md` / `AGENTS.md` 里写一句"bash 命令用 exec_command 的 `shell`"。
- **Rust 项目要装 Visual Studio 生成工具**（"使用 C++ 的桌面开发"）才能链接。没装的话 `cargo check` 能过，`cargo build`、`cargo test` 报 ``linker `link.exe` not found``。这是 Rust 在 Windows 上的要求，跟 ccnm 无关。
- **Windows Defender** 对搜代码、读小文件的拖慢实测很小，不用为此加排除；编译这类写多读多的场景还没量。

## 出错时

| 看到 | 原因 | 怎么办 |
| --- | --- | --- |
| `Permission denied (publickey,...)` | 管理员账号的钥匙放错了文件，或那个文件权限不对 | 照[第 2 步](#2-放钥匙)放进 `administrators_authorized_keys`，跑那条 `icacls` |
| `... not found on win: its Windows shell does not know that program` | AI 那台没写 `ccnm_bin`，或写的路径上没有 ccnm.exe | 照[第 6 步](#6-在-ai-那台跑-doctor)写；在 Windows 上 `dir $env:USERPROFILE\.local\bin` 看文件在不在 |
| `ccnm_bin must be an absolute path written with forward slashes` | `ccnm_bin` 写成了 `C:\Users\...` | 反斜杠全换成 `/` |
| `a Windows Runtime has no dedicated-account mode yet` | Windows 的配置里写了 `runtime_user` | 删掉那一行 |
| `Windows can only be the machine holding the projects` | 在 Windows 上敲了 `ccnm init --runtime` | Windows 上用 `ccnm init --agent <别名>` |
| doctor 连不上、一直等 | 机器睡着了，或防火墙挡了 22 端口 | 唤醒它；看[第 1 步](#1-装-openssh-server)最后那条命令 |
| 在 PowerShell 里把 ccnm 的输出存进变量或接管道之后，中文成了乱码 | ccnm 输出的是 UTF-8，PowerShell 按系统代码页（GBK）去读它接住的输出 | 先跑 `[Console]::OutputEncoding = [Text.Encoding]::UTF8`；经 SSH 从 AI 那台看是好的 |

其他报错见[排错手册](troubleshooting.md)。
