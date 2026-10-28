# P111：Windows Runtime 的配置、诊断和发布包（2026-10-28）

阶段范围见 [ROADMAP](../plan/ROADMAP.md) 的 P111，设计见 [Windows 当项目机器](../plan/windows-runtime.md) 第 2 节和 D6、D8。装法写在 [Windows 安装](../windows-runtime-setup.md)。

**结论（写到这里时）：** 代码和文档都做完，macOS 上全套测试过；Windows 那一半只在本机按 Windows 目标做了 clippy，**还没在 Windows CI 上跑过**，安装文档**还没在测试机上照着走过**（U7 也就没测）。下面第 3 节列着还差什么。

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

## 3. 还差什么

- **Windows CI**：`windows_gate`（从只有 `USERPROFILE` 的环境走 init/add/ws/rm、D8 报错）、`windows_doctor`（真二进制回答审计和清理；清理第一次端到端用 P110 的 Win32_Process 列 mcp-serve）、`safety::windows`、`safety::credentials::windows_tests`、`paths::remote`，以及把 `HOME` 换成 `USERPROFILE` 之后的 `read_tools`、`write_guard`、`windows_exec`，和 `scripts/dist-windows.sh`。要推送才跑得到。
- **测试机**（P111.3 的"从零走一遍"和 U7）：按安装文档装 CI 打的 zip，`init`、`add`；从本机用同一提交的构建跑 `ccnm doctor`，看 Windows 那几行在真机上的样子（P106.4 记着测试机 `.ssh` 里有两把出站私钥、ssh-agent 服务是 Disabled、DefaultShell 是 PowerShell）；U7：给 zip 加上浏览器下载会打的 Zone.Identifier，不跑 `Unblock-File`，看经 ssh 起 ccnm.exe 会不会被拦。动测试机要用户另外同意。
- 中文 Windows 上 `sc.exe` 和 `reg.exe` 的输出形状：解析只认 `STOPPED`/`RUNNING` 这两个词、`REG_SZ`/`REG_EXPAND_SZ` 和退出码，没在中文系统上实测过。
