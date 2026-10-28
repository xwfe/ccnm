# P113：Windows 上 `cmd` 里不带路径的 `bash` 跑 Git Bash，发 v0.21.1（2026-10-28）

P112 用真实模型时（[记录](2026-10-28-release-0.21.0.md)第 5.1 节），两个模型都在 `exec_command` 的 `cmd` 里写了 `["bash", "build.sh"]`。装了 WSL 的机器上，PATH 先找到的是 `C:\Windows\System32\bash.exe`，它是 WSL 的启动器：Claude 先 `where bash` 看出来、改了全路径；Codex 直接跑进 WSL，WSL 里的 mise 顺手装了 233 MB 工具才失败。用户当天定：和 `shell` 用同一个 Git Bash、结果里说一声、写了路径的照原样；Windows 只当 Runtime；发补丁版、上测试机验、测完清理。

## 1. 改了什么

`crates/ccnm-core/src/mcp/exec.rs`：Windows 上，`cmd` 的第一项是 `bash` 或 `bash.exe`（不分大小写、不带 `/`、`\`）且这台机器找得到 `shell` 用的 Git Bash 时，把它换成那个 Git Bash 的全路径，`SHELL` 也照 `shell` 设，再交给原来的 Windows 程序查找（P110.2）。换了之后，PATH 上第一个 `bash` 不是它时，结果里多一行：

```
[bash ran as Git for Windows' bash (C:/Program Files/Git/bin/bash.exe), the one shell uses, not as the first bash on PATH (C:/WINDOWS/system32/bash.exe, which starts WSL); to run that one, give its full path]
```

PATH 上第一个是 `%SystemRoot%\System32\bash.exe` 时才写 `which starts WSL`；是同一个程序（大小写、`/` 与 `\`、`\\?\` 前缀都不算不同）或 PATH 上没有 `bash` 时不写。判断和文字是 `platform.rs` 的两个纯函数（`is_bare_bash`、`bash_swap_note`），各平台都跑单元测试。

没变的：写了路径的（`C:/…/bash.exe`、`./bash`）；找不到 Git Bash 时（照旧按 PATH 找）；`shell`；工具说明和 `tools/list`（两份 fixture 不动）；macOS、Linux。

## 2. 离线与 CI

本机 macOS arm64（提交 `63ec4fd`）：`cargo fmt --check`、`clippy -D warnings` 通过；`cargo test --workspace` 1204/0（P112 时 1202，多的是两条新单元测试）；`ci_gates.py` 计划、协议、Python 252/0。按 `x86_64-pc-windows-msvc` 目标照开发文档跑的三条 clippy（库和二进制、`ccnm-core` 单元测试、五个 Windows 集成测试文件）都干净。

GitHub Actions 运行 40673370162 四个 job 全绿。windows-runtime 里：两条新单元测试过；`windows_exec` 7/0，其中新的 `a_bare_bash_in_cmd_is_git_bash` 在 PATH 最前面放一个假 `bash.exe`（`whoami.exe` 的拷贝）：`bash` 和 `BASH.EXE` 都进了 Git Bash（`uname -s` 是 MINGW、`BASH_VERSION` 有值、`SHELL` 以 `bash.exe` 结尾），说明点名那个假的；写全路径的跑到了假的、没有说明。

## 3. 测试机

用户 2026-10-28 定修时一并同意上测试机、测完清理。测试机同 P112（Win11 中文系统，装了 WSL，发行版 kali；sshd 默认 shell 是 Windows PowerShell）。`where.exe bash` 依次是 `C:\Windows\System32\bash.exe`、`D:\scoop\shims\bash.exe`、`WindowsApps\bash.exe`。

**CI 打的包（运行 40673370162，提交 `63ec4fd`）**：照安装文档装进 `%USERPROFILE%\.local\bin`（`ccnm.exe` `d2797b448dc7fb2d…`），`init --agent`、`add` 一个只有 README 的 git 项目、写 `external_mcp = "coding"`。这台 macOS 用同一提交的 release 构建、P112 那份临时配置跑 `ccnm mcp bridge p113 --mode coding`，探针 [p113-windows-bare-bash.py](probes/p113-windows-bare-bash.py) **6/6**：

| 查什么 | 结果 |
| --- | --- |
| `["bash", "-c", …]` | 231 ms，`BASH_VERSION` 5.2.37、`uname -s` 是 `MINGW64_NT-10.0-26200`，跑的是 `D:/scoop/apps/git/current/bin/bash.exe`（这台的 Git 是 scoop 装的，从 PATH 上的 `git.exe` 找到） |
| 说明 | `[bash ran as Git for Windows' bash (D:/scoop/apps/git/current/bin/bash.exe), the one shell uses, not as the first bash on PATH (C:/Windows/system32/bash.exe, which starts WSL); to run that one, give its full path]` |
| `["BASH.EXE", "--version"]` | `x86_64-pc-msys`，同一行说明 |
| 写全路径的 Git Bash | 照原样跑，没有说明 |
| `shell` | 不变，没有说明 |
| WSL | 会话前后没有新起的 `wsl.exe`、`wslhost.exe`、`wslrelay.exe`；`wsl -l -v` 前后都是 `Stopped` |

会话结束后测试机上没有 ccnm、bash 进程，写锁 `released`。

探针第一次没跑完是探针的错：它拿 `$SHELL` 当 Windows 路径传给 `cmd`，而 Git Bash 里 `$SHELL` 读出来是 MSYS 的写法（`/d/scoop/...`）；ccnm 设的是 `D:/scoop/...`，进了 Git Bash 被它自己转换了。改成从说明里取路径。

