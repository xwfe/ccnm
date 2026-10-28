# P111：Windows Runtime 的配置、诊断和发布包（2026-10-28）

阶段范围见 [ROADMAP](../plan/ROADMAP.md) 的 P111，设计见 [Windows 当项目机器](../plan/windows-runtime.md) 第 2 节和 D6、D8。装法写在 [Windows 安装](../windows-runtime-setup.md)。

**结论：** P111 做完。macOS 上全套测试过；Windows CI 第二次（40627674700）四个 job 全绿；在 Win11 测试机上照安装文档第 4–6 步用 CI 打的包走了一遍，从这台 macOS 跑 doctor，Windows 那几行都有结论、和机器实际一致（第 4 节）。走查查出两件没在本阶段修的事：`ccnm cleanup` 对 Windows 上的项目两边都跑不了；Windows 上 `ccnm rm` 碰到连不上的 AI 那台要等满 60 秒（4.4）。

## 1. 做了什么

| 验收 | 做法 | 提交 |
| --- | --- | --- |
| P111.1 远端路径 | 描述另一台机器的路径按它的写法判断：`/…` 和 `X:/…` 都是绝对路径，拒绝 `\`、`.`、`..`、NUL（`paths::is_remote_absolute`）。用在 Agent 侧对 Runtime 根目录的两处检查（`WorkspaceBinding::validate`、`Spec::validate_identity`；`profiles::absolute` 留给本机的 profile 目录）和配置里另一台机器的 `ccnm_bin`、`claude_config_dir`、`codex_bin`、workspace 的 `root`。描述本机的照旧按本机规则。`ccnm_bin` 允许开头一个盘符 `C:`，反斜杠单独报"用正斜杠写" | 4bb02a0 |
| P111.1 ssh 提示 | Windows 的 shell 说找不到程序时，提示写正斜杠全路径带 `.exe`、`~/` 只在 PowerShell 下才行；`ccnm_bin` 是 `C:/…` 时"跑不了"不再提示 `chmod +x` | 5be42e0 |
| P111.1 init/add/ws | Windows 上家目录读 `USERPROFILE`（不看 `HOME`）；`init --runtime` 拒绝；`init --agent` 结尾只说 AI 那台要写什么，并打出按 Agent 读配置同一条规则检查过的 `ccnm_bin` 行；本机节点写了 `runtime_user` 读配置时就拒绝（D8） | 9095d28、3c62f73 |
| P111.2 审计 | `whoami /groups` 按 SID 认 Administrators（S-1-5-32-544）、完整性级别（High 及以上算提权）、按名字认 docker-users；`sc.exe query ssh-agent` 看服务在不在跑；私钥和 Agent 凭据能不能读，Windows 上用打开文件判断。Windows 上行名 `Not elevated`（替 `No sudo`）、`No SSH agent`、`No Docker access` | a1c51c4 |
| P111.1/2 doctor | `runtime-audit` 在 Windows 上带回 sshd 的 `DefaultShell`；Agent 的 doctor 对 Windows Runtime 多一行 `Runtime shell`；Windows 根目录不再被说成"属于另一个账号" | 03e8b77 |
| 闸门 | `internal runtime-audit`、`runtime-cleanup` 在 Windows 上放行；清理删目录改用会重试的 `remove_tree` | b60eb1a |
| P111.3 发布包 | `scripts/dist-windows.sh`（Git Bash 里跑，PowerShell 打 zip）；release.yml 新增 windows job，gate 和 CI 的 Windows job 逐条相同（`tests/test_ci_gates.py` 比对）；CI 每次推送都打一次包并留下 zip | 19a23f1 |
| P111.3 文档 | `docs/windows-runtime-setup.md` | df93374 |

### 1.1 和计划不一样的地方

- **认领时补的三条**（ROADMAP P111.1 下记着）：家目录在 Windows 上读 `HOME` 会让 PowerShell 里的 `ccnm add` 直接报错；`init --runtime` 在 Windows 上没拦；`runtime-cleanup` 的闸门写着"P110 会做"但 P110 的验收里没有它。
- **D8 在读配置时拒绝，而不是在审计里加一条拒绝**。写了 `runtime_user` 的 Windows 配置，`ws ls`、`runtime-resolve`、`mcp-serve` 都过不去，报错说删掉那行。在审计里做的话，要么新开一种"不可豁免"的检查，要么把其余各行按专用账号判一遍——都比在一个地方拒绝复杂，结论却一样。
- **DefaultShell 放在 `runtime-audit` 的回答里，不进 `safety::Audit`**：它不是安全属性，而审计每次会话开始都跑一遍，多查一次注册表没有用。
- **`Workspace root` 行在 Windows 上不比较归属**：Windows 没有 uid，`owned` 永远是 false，原来会说成"属于另一个账号"。D8 下只有共用账号，git 自己的 dubious ownership 检查照样说话。

## 2. 验证

本机 AI 那台 macOS（arm64），提交 df93374 之后：

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` 通过。
- `cargo test --workspace --no-fail-fast`：1202 passed / 0 failed（P110 结束时 1188）。
- `python3 -B scripts/ci_gates.py`：计划、协议通过，Python 252 条 0 跳过。
- Windows 目标（本机只能 check 不能链接）：`cargo clippy --workspace --lib --bins --target x86_64-pc-windows-msvc -- -D warnings`、`cargo clippy -p ccnm-core --lib --profile test --target …`、五个 Windows 集成测试文件各自 clippy，都干净。
- 先红后绿：Windows 根目录的绑定和 `-c`（`work::tests::a_windows_runtime_root_binds_and_continues`、`instance_config` 的 `a_windows_runtime_root_binds_on_an_agent_of_another_system`）在撤掉 instance.rs/session.rs 改动后都红；`instance_execution` 的 `a_windows_runtimes_ccnm_bin_is_used_as_written` 在撤掉 config.rs 改动后红（ssh 一次没调，日志为空）。发版清单比对测试：删掉 release.yml 里一条 Windows 测试命令，红。

## 3. 写这一节时还差什么（第 4 节补上了）

- **Windows CI**：`windows_gate`（从只有 `USERPROFILE` 的环境走 init/add/ws/rm、D8 报错）、`windows_doctor`（真二进制回答审计和清理；清理第一次端到端用 P110 的 Win32_Process 列 mcp-serve）、`safety::windows`、`safety::credentials::windows_tests`、`paths::remote`，以及把 `HOME` 换成 `USERPROFILE` 之后的 `read_tools`、`write_guard`、`windows_exec`，和 `scripts/dist-windows.sh`。要推送才跑得到。
- **测试机**（P111.3 的"从零走一遍"和 U7）：按安装文档装 CI 打的 zip，`init`、`add`；从本机用同一提交的构建跑 `ccnm doctor`，看 Windows 那几行在真机上的样子（P106.4 记着测试机 `.ssh` 里有两把出站私钥、ssh-agent 服务是 Disabled、DefaultShell 是 PowerShell）；U7：给 zip 加上浏览器下载会打的 Zone.Identifier，不跑 `Unblock-File`，看经 ssh 起 ccnm.exe 会不会被拦。动测试机要用户另外同意。
- 中文 Windows 上 `sc.exe` 和 `reg.exe` 的输出形状：解析只认 `STOPPED`/`RUNNING` 这两个词、`REG_SZ`/`REG_EXPAND_SZ` 和退出码，没在中文系统上实测过。

## 4. Windows CI 和测试机（2026-10-28）

### 4.1 CI

- **第一次，运行 40626856262（提交 2763147）**：macOS、Linux、MSRV 绿，Windows 红在 `windows_doctor` 的清理用例。两个原因：测试把会话写在 `$XDG_STATE_HOME/sessions` 下，少了 `ccnm\` 那一层；Runtime 列在跑的 mcp-serve 那一步（P110 的 PowerShell `Win32_Process`）用满了 30 秒超时，清理会把一切都当成"查不了"留着。审计用例第一次就过了。
- **第二次，运行 40627674700（提交 72026d4）**：四个 job 全绿。Windows：单元测试 141/0、`windows_gate` 3/0、`windows_doctor` 2/0、`read_tools` 3/0、`write_guard` 7/0、`windows_exec` 6/0（家目录改成 `USERPROFILE` 之后），`dist-windows.sh` 打出 `ccnm-0.20.0-windows-x86_64.zip`（4 138 637 字节，sha256 `15f68fdc…`），artifact 已上传。
- **runner 上的慢没查清**：72026d4 先按名字导入 CimCmdlets，理由是"PowerShell 自己找 `Get-CimInstance` 要翻遍模块"。第二次运行里清理预览照样用了 19.7 秒，所以这个理由不成立，8bc44d8 把注释改成只写量到的。测试机上同样精简的环境里这段只要 0.25–0.7 秒，sshd 下整个 `runtime-cleanup` 0.3–0.46 秒（4.3）。runner 上它离 30 秒的上限不远，这条用例可能偶尔红。

### 4.2 测试机上照安装文档走

用户 2026-10-28 同意上测试机，测完清理。tailnet 节点又离线，走局域网，主机钥匙按 tailnet 那条核对；本机的 `~/.ssh/config` 没动，ccnm 自己的 ssh 用临时放在 scratchpad 里的包装脚本加 `HostName`。测试期间用一个两小时租约的 `SetThreadExecutionState` 进程撑着别睡，最后撤。

- **先看现场（只读）**：`whoami /groups /fo csv /nh` 是四列 CSV，有两行组名是中文，Administrators 那行 SID 是 S-1-5-32-544，完整性级别 S-1-16-12288（High）；`sc.exe query ssh-agent` 打 `1  STOPPED`，不存在的服务退出码 1060；`reg.exe query` 读到 DefaultShell 是 Windows PowerShell，不存在的值退出码 1、英文报错。和解析器的假设一致。
- **第 4 步**：CI 的 zip 本机核过 sha256，scp 到 `%TEMP%`，那边 `Get-FileHash` 也一致。给 zip 打上浏览器下载会打的 `Zone.Identifier`（ZoneId=3）之后 `Expand-Archive`：**`ccnm.exe` 上没有标记**（Windows PowerShell 5.1 不传），于是手动给它也打上，模拟资源管理器解压。**带着标记经 ssh 起 `ccnm.exe --version`，PowerShell 和 cmd 下都正常**（U7 在 ssh 这条路上不成问题）。之后的 doctor 都是带着标记跑的；最后照文档 `Unblock-File`，标记没了、照样能起。
- **第 5 步**：`ccnm init --agent mac`、`ccnm add <临时项目>`、`ccnm ws ls` 都对，配置在 `%USERPROFILE%\.config\ccnm\config.toml`，根目录存成 `C:/Users/…/proj`；再跑一次 `init` 打出的就是文档里那行 `ccnm_bin = "C:/Users/<账号>/.local/bin/ccnm.exe"`。看到的毛病：配置路径显示成 `…\.config\ccnm/config.toml`（de7435b 修）；在 PowerShell 脚本里接住 ccnm 的输出，中文是乱码（PowerShell 按 GBK 读 UTF-8），经 ssh 直接看是好的，文档里写了怎么办。
- **第 6 步**：这台 macOS 用临时配置 `ccnm init --runtime win11`，先不写 `ccnm_bin` 跑 `doctor proj`，2.2 秒出表：反向 SSH 正常；Runtime 执行身份、管理员令牌、admin 组、SSH 私钥（`.ssh` 里那几把出站私钥）都是注意；SSH agent 服务、Claude 凭据、Docker 权限正常；exec_command 允许（共用账号）；workspace 根目录"是 git 仓库"；Runtime 的 shell 注意，点明 `~/.local/bin/ccnm` 只因 PowerShell 才能用，给出 `ccnm_bin = "C:/Users/xdw/.local/bin/ccnm.exe"`；远端 MCP 握手 548 ms、11 个工具、同一个进程。总结论是"还不能用"，原因是临时配置下 AI 这边没有 Controller，和 Windows 无关。补上 `ccnm_bin` 再跑，shell 那行只剩 PowerShell 的影响。看到的毛病：Codex 凭据和认证环境变量两行都显示成"Runtime 安全"（老问题，de7435b 修）。

### 4.3 计时

测试机上 sshd 会话里连跑三次 `ccnm internal runtime-cleanup`（含列 mcp-serve 的 PowerShell）：0.46、0.30、0.31 秒。

### 4.4 走查查出、没在本阶段修的

- **`ccnm cleanup` 对 Windows 上的项目两边都跑不了。** 在 AI 那台跑，它说 workspace 定义在 Runtime 上、去那边跑；Windows 的闸门不让跑 `cleanup`（它要从 Windows 连 AI 那台，在 Windows 上发起是没排的 W7）。所以 P111 放开的 `runtime-cleanup` 眼下只有 CI 用例会调。旧输出照样按 7 天过期。修法大概是让 AI 那台对"定义在别处的 workspace"也能发起清理（和 doctor 一样先问 Runtime），这对任何只许连进来的 Runtime 都有用，要单独设计。
- **Windows 上 `ccnm rm` 碰到连不上的 AI 那台要等满 60 秒。** 它先试着去 AI 那台停会话（best effort）。测试机 PATH 上第一个 ssh 是 scoop 的 OpenSSH 9.5p1，对一个解析不出来的别名一直挂着、不认 `ConnectTimeout=10`（直接跑也一样，几分钟都不返回），是 ccnm 的 60 秒把它停下的。原因在那台机器的 ssh 客户端设置里，没查清。文档写了先在 AI 那台 `ccnm stop` 再删。
- **清理之后测试机的 `powercfg /requests` 多了一条 `[DRIVER] Legacy Kernel Caller / Sleep Idle State Disabled`**，测试开始前没有。不是进程发的，`/requestsoverride` 也是空的，一分钟后还在。本次起的进程（ccnm、ssh、PowerShell、防睡眠进程）都已不在，原因没查清。

### 4.5 清理

测试机：`ccnm rm proj`；删 `%USERPROFILE%\.local\bin\ccnm.exe`（同目录原有的 uv、python 等没动）、`%USERPROFILE%\.config\ccnm`、`%USERPROFILE%\.local\state\ccnm`、`%TEMP%\ccnm-p111`；结束卡住的诊断 ssh 和它的 PowerShell；最后结束防睡眠进程。核对：没有 ccnm.exe、ssh.exe 残留，那几个路径都不在。本机：删临时配置、ssh 包装脚本和 `target/p111-state`。
