# P110：Windows 上执行命令（2026-10-27 至 10-28）

阶段范围见 [ROADMAP](../plan/ROADMAP.md) 的 P110，设计见 [Windows 当项目机器](../plan/windows-runtime.md) 的 D2、D3、D7。

**结论：** Windows Runtime 能执行命令了：`exec_command`（`cmd` 和 `shell`）、后台命令、`read_output`、`stop_command`、`call_mcp_tool`、skill 的命令。中途 CI 推翻了 D3 的一个前提——`taskkill /T` 收不到 Git Bash 起的任何外部程序——用户据此把 Job Object 提到本阶段做，每条命令放进它自己的 Job。真机还没跑（P112）。

## 1. 做了什么

| 地方 | Windows 上 | 依据 |
| --- | --- | --- |
| 标准句柄 | `main` 一开始清掉 stdin/stdout/stderr 的继承位（toexec-win） | 不清的话后台命令拿着 mcp-serve 的 stdout（ssh 通道），mcp-serve 被强杀后通道不关。CI 用例：杀掉 mcp-serve 后 5 秒内读到结束 |
| 防睡眠 | mcp-serve 会话期间 `SetThreadExecutionState`，`run` 里最先拿、最后撤 | P106：测试机空闲 10 分钟就睡，ssh 流量不算活动 |
| `cmd` | 按 PATHEXT 找程序（`npm` 找到 `npm.cmd`）；`.bat`/`.cmd` 交给标准库，`.ps1` 交给 PowerShell（有 `pwsh` 用它） | 见 2.1 |
| `shell` | Git for Windows 的 bash：先 `%ProgramFiles%\Git\bin\bash.exe`，再从 PATH 上的 `git.exe` 往上找所在安装的 `bin\bash.exe`，最后看配置项 `git_bash`；不用 PATH 上的 `bash.exe`；`SHELL` 设成这个 bash（`C:/` 写法）；找不到报 `CCNM_E_DEPENDENCY` 并提示 `cmd` | D2；P106 测试机上 PATH 上第一个 `bash` 是 WSL 的、git 是 scoop 装的 |
| 停命令 | 每个子进程起来后马上放进它自己的 Job（`KILL_ON_JOB_CLOSE`，不许脱离），超时、停止时结束整个 Job；放不进 Job 时退回 `taskkill /T /F`；Windows 没有 TERM，不等宽限期 | 见 2.2 |
| 命令留下的进程 | 命令结束后 Job 里还有进程的，Job 不关，会话收尾时统一结束（`orphans`），结束不掉的不交写锁 | 和 Linux 上 P84 是同一个时机；命令一结束就关会当场杀掉 `npm run dev > log &` 这类，和 Linux 不一致 |
| MCP 转发 | server 的命令按 PATHEXT 找；server 进 Job，关连接时结束 Job（连它留下的进程），5 秒没清空记进写锁 | 和 Unix 用锚进程组的效果一样 |
| 过期输出清理 | 列在跑的 mcp-serve 改用 PowerShell 的 `Win32_Process`（`-EncodedCommand`） | Windows 没有 `ps`，`tasklist` 没有命令行 |
| 输出目录 | 先建好三个文件再关掉、改名、再打开加锁 | 见 2.3 |
| 沙箱 | `exec_sandbox` 让会话起不来（`CCNM_E_NOT_READY`） | Codex 的 Windows 沙箱没验过 |
| System32 程序 | `whoami`、`tasklist`、`taskkill`、PowerShell 都按完整路径起 | PATH 上同名程序不能替它们回答 |

## 2. 中途定下的几件事

### 2.1 `.bat` / `.cmd` 交给标准库，不照 gld 拼 `call "…"`

设计文档写的是照 gld：`cmd.exe /d /s /c` 加 `call "<路径>" "<参数>"`，参数里的 `"` 换成 `""`。这个写法挡不住 `%VAR%`：引号里的 `%PATH%` 照样被 cmd 展开。Rust 标准库从 1.77.2 起（CVE-2024-24576，"BatBadBut"）对 `.bat`/`.cmd` 有自己的一套：经 `cmd.exe /e:ON /v:OFF /d /c` 起，`%` 也转义，参数里有换行就拒绝。所以 ccnm 只负责把 `npm` 解析成 `npm.cmd` 的完整路径交给它（标准库自己只会补 `.exe`）。

### 2.2 `taskkill /T` 收不到 Git Bash 起的程序，改用 Job Object

D3（用户 2026-10-24 定）：第一版用 `taskkill /T /F`，Job Object 留作第二步。前提是 taskkill 只漏掉"父进程先退出"的孙进程，P106.3 实测过这是少数情况。

CI 运行 40524315270 起的超时用例：Git Bash 里 `sleep 30 & sleep 30`，2 秒超时确实触发了，命令却过了 31.8 秒才返回。运行 40529531958 在超时后 2 秒抓了进程表：这条命令的 bash 都已被收掉，两个 `sleep.exe` 还在，父进程号（6120、8748）指向已经不存在的进程。原因：Git Bash 是 MSYS2，底下是 Cygwin，它执行外部程序的办法是另起一个 Windows 进程、原进程退出；在 Windows 看来，bash 起的每个程序都没有活着的父进程。同一用例里 `cmd /c ping` 是原生程序，超时正常结束。

也就是说，在 Git Bash 里 taskkill 漏掉的不是少数，是全部：`shell` 的超时、`stop_command`、会话结束都停不掉 bash 起的程序。2026-10-28 把证据给用户，用户选择现在就做 Job Object。

- toexec-win 0.2.0 加 `Job`（建、放进去、结束、查还剩几个），toexec 的 Windows CI 上测过：父进程起完孙进程就退出（taskkill 收不到的那种），结束 Job 和丢掉 Job 都让孙进程拿着的管道在 5 秒内关上。
- 有一个空档：标准库没法让进程挂起着起来，所以是先起后放，中间几微秒里它自己起的进程会漏。一个程序起到能再起别的程序要几毫秒，实际碰不上。
- Job 不设 `BREAKAWAY_OK`：成员带 `CREATE_BREAKAWAY_FROM_JOB` 起会失败，不会悄悄跑出去。

### 2.3 输出目录：Windows 不让改名一个里面有打开文件的目录

一次运行的目录原来是在 staging 里把 stdout、stderr、running 标记都打开、给标记加锁，再整个改名成正式目录——这样目录一出现，标记就已经锁着，别的 server 腾地方时不会把它当成结束了的运行删掉。CI 运行 40524315270 里所有 `exec_command` 都报 `cannot create a file for the command's output`（os error 5）：Windows 不让改名一个里面有打开文件的目录。P109 查代码时记下过这一处。

Windows 上改成：文件建好就关，改名，再重新打开、给标记加锁。中间几微秒目录看得见而标记还没锁；同一会话的另一个 server 要等所有更早结束的运行都删完、仍然超限，才会轮到这个最新的，轮到了也只是这条命令起不来。macOS、Linux 照旧。

### 2.4 中文输出：不按代码页转码（P110.4 要定的那条）

测试机的系统代码页是 936，cmd 内建命令和一部分原生工具输出 GBK，不是 UTF-8。定下来：**不转**，照现在的做法——原始字节完整留在输出文件里；`exec_command` 的预览把不合法的字节换成 U+FFFD，`read_output` 也换，并标明"this stream is not valid UTF-8"。理由：

- 一段字节是 GBK 还是坏掉的 UTF-8 判断不了。Git Bash 下的 git、rg、cargo 输出 UTF-8，和 cmd 的 GBK 会混在一个流里，整段按代码页转会把正确的 UTF-8 弄坏。
- 转码要么调 `MultiByteToWideChar`（unsafe，得再进 toexec-win），要么加 encoding_rs 依赖。
- 要中文对，模型可以用 `shell`（Git Bash 下是 UTF-8），或者 `cmd /c chcp 65001 >nul & …`。CI 用例里 Git Bash 输出的中文原样回来了。

## 3. 怎么验的

- 本机 macOS：`cargo test --workspace` 1188/0，`--test-threads=64` 1188/0；Windows 目标 clippy（库、二进制、单元测试代码、四个集成测试）无警告；Python 门禁 251 条 0 跳过。
- toexec：运行 40521179743（toexec-win 0.1.0）、40613554398（0.2.0）三个 job 全绿，toexec-win 的测试在 Windows 上真跑了；继承那条的对照组等满了 10 秒，证明测的就是这件事。
- ccnm Windows CI：`windows_exec` 集成测试六条——`.cmd`（不带扩展名）、`.ps1`、PATH 上的 `npm.cmd`、找不到的程序；Git Bash 的 `SHELL`、`[[ ]]`、退出码 3、stderr 分开、带引号的参数、中文；超时结束整棵树（Git Bash 后台 `sleep`、cmd 下的 `ping`）；`read_output` 等后台命令结束、`stop_command`、会话结束收后台命令；mcp-serve 被强杀后通道 5 秒内关；`.cmd` 包装的 node MCP server 经 `call_mcp_tool` 调通。最终那次运行的结果见第 4 节。

## 4. 最终 CI

运行 40614212914（提交 3050941）四个 job 全绿：

- Windows：单元测试 133/0、`windows_gate` 3/0、`read_tools` 3/0、`write_guard` 7/0、`windows_exec` 6/0（单线程、边跑边打，17.8 秒；换 Job 之前那次 108 秒，超时用例自己就等了 31.8 秒）。release 构建的 `ccnm.exe` 照常留成 artifact。
- macOS 1188/0、Linux 1189/0，两边 Python 门禁 251 条 0 跳过。

为了拿到 2.2 的证据，中间几次运行改过测试：超时用例在超时后 2 秒抓进程表；抓进程表的 `powershell` 一开始继承了测试进程的伪控制台，结果 libtest 打完 `failures:` 就退出，失败详情一个字没留下（运行 40527946104）。改成不挂控制台、stdin 给 null，CI 里这一步加 `--nocapture`，详情才出来（运行 40529531958）。

## 5. 没做到、没验的

- **真机没跑。** Win11 测试机上经 sshd 跑一遍要另外征得同意，排在 P112。CI 的 runner 上 Git 装在 `%ProgramFiles%`，"从 PATH 上的 git.exe 往上找"（测试机 scoop 那种）只有单元测试，没有端到端。
- **Defender 对写多读多的命令（编译、跑测试）拖慢多少**（U6 的后一半）没量，同样等真机。
- `cmd` 起的原生程序输出的 GBK 不转码（2.4）。
- `curl.exe` 走 HTTP MCP 只有 P106.4 的实测和编译，没有 CI 端到端。
