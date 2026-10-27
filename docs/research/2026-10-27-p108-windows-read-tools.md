# P108：Windows 上的只读工具和路径规则（2026-10-25 至 10-27）

阶段范围见 [ROADMAP](../plan/ROADMAP.md) 的 P108，设计见 [Windows 当项目机器](../plan/windows-runtime.md) 的 D5。

**结论：** 七个只读工具在 Windows 上能用：CI 的 `windows-latest` 和 Win11 测试机（经 Windows 自带的 sshd）都跑通了。mcp-serve 在 Windows 上只服务外部客户端的 read 模式，会写的会话在建会话之前就被拒。P106 担心的"服务端一阵不读 stdin 就卡死"没有出现：stdout 堆着 14 MB 回答时，它照样把 25 MB 请求读完了。Defender 开着时只读工具不慢（第 3.3 节）。认领时另外查出一个三个系统共有的缺口：macOS 上 `apply_patch` 能写进 `.git`，已修。

## 1. 认领时查出来的两件事

### 1.1 macOS 上 `apply_patch` 能写进 `.git`

`resolve_write` 原来只按调用方写的字符串比 `.git`。两种写法能绕过去：

- **大小写。** macOS 默认的 APFS 不分大小写，`realpath` 还会把名字纠正成盘上的写法。本机实测（libc `realpath`）：`ws/.GIT/config` → `ws/.git/config`，`WS/.git/CONFIG` → `ws/.git/config`。字符串 `.GIT` 不等于 `.git`，检查放行，解析出来的绝对路径却是 `.git/config`。
- **目录符号链接。** workspace 里 `gitdir -> .git`，写 `gitdir/config`。路径里没有 `.git` 这几个字，符号链接只查了目标本身（`gitdir/config` 是普通文件），父目录的链接跟过去了。

两条测试先在旧代码上红：`resolve_write` 返回 `WriteTarget { rel: ".GIT/config", abs: ".../ws/.git/config", exists: true }` 和 `WriteTarget { rel: "gitdir/config", abs: ".../ws/.git/config", exists: true }`。修法：`.git` 三个系统都不分大小写；写的时候再看解析之后的路径（最深一层已存在的祖先，`canonicalize` 过）里有没有 `.git`。`search_text` 的两处 `.git` 检查也改成不分大小写。提交 de75931。

Linux 上名叫 `.GIT` 的目录和 `.git` 不是一回事，现在也写不进去。没人这么起名，这个代价写在协议文档的变更记录里。

### 1.2 mcp-serve 在 Windows 上起不来

P108.4 要在测试机上经 sshd 起 `internal mcp-serve`，原计划没写的两件前提：

- P107 的入口闸拒绝 `internal mcp-serve`；
- 审计读 `/usr/bin/id`，Windows 上没有。"身份未知"是两条不可豁免的检查之一，所以哪种会话都起不来。

补成 P108.5：入口闸放行，mcp-serve 自己拒绝会写的会话（受管会话、coding 模式，报 `CCNM_E_NOT_READY`，说清是 P109、P110 的事）；Windows 上身份改读 `%SystemRoot%\System32\whoami.exe /user /fo csv /nh`（账号名和 SID），SYSTEM（`S-1-5-18`）当 root。sudo、admin 组两行在 Windows 上写成"还没查"，共用账号下只显示，P111.2 补真检查。

## 2. 改了什么

| 地方 | Windows 上 | macOS、Linux 上 |
| --- | --- | --- |
| 模型给的路径（`mcp::path`） | 多拒四种名字，`CCNM_E_POLICY`：带 `:` 的（备用数据流）；设备名 `CON`、`PRN`、`AUX`、`NUL`、`COM1`–`COM9`、`COM¹²³`、`LPT1`–`LPT9`、`LPT¹²³`，带扩展名也算（微软 "Naming Files, Paths, and Namespaces" 2026-10-25 版的列表），另加打开控制台的 `CONIN$`、`CONOUT$`；以点或空格结尾的；`~` 后面跟数字、再往后是结尾或 `.` 的（8.3 短名的形状）。junction 走标准库的符号链接判断 | 不变（规则是纯字符串函数，三个系统都测，只有 Windows 用） |
| `.git` | 不分大小写；写时按解析之后的路径再查 | 同左（第 1.1 节） |
| 根目录的写法 | `runtime-resolve` 报给 Agent 的、`ws add` 存的，统一成 `C:/Users/x/proj` | 原样 |
| `search_text` | rg 按 PATHEXT 找，不收 `.bat`/`.cmd`；rg 吐出的 `\` 换成 `/`，带 `:` 的丢掉 | 不变 |
| `load_skill` | 已安装 skill 的路径写成 `C:/…`（原来按 `/` 拆目录名会拆错）；skill 目录里的文件也过一遍上面的名字规则（toexec 的解析器不认这些名字，toexec 不动） | 不变 |
| `workspace_info` | `git_subdir` 用 `/` 拼 | 不变 |
| mcp-serve | 只收 read 模式；审计用 `whoami` 认身份 | 不变 |

技能文件的读取（`toexec-skill` 的 `dir::resolve`）和 gld 共用，这一轮没改 toexec，ccnm 在调它之前自己先查。

## 3. 怎么验的

### 3.1 本机（macOS arm64）

- `cargo test --workspace` 1183/0（P107 时 1170，新增 13 条）；clippy 无警告；`python3 scripts/ci_gates.py` 通过（Python 251 条 0 跳过）。
- Windows 目标：`cargo clippy --workspace --lib --bins`、ccnm-core 单元测试代码（`--lib --profile test`）、`windows_gate` 和 `read_tools` 两个集成测试，`--target x86_64-pc-windows-msvc` 都无警告。本机链接不了 Windows 目标，只能查到这一步。
- 探针 [p108-windows-read-tools.py](probes/p108-windows-read-tools.py) 在本机演练：服务端换成本机的 ccnm，全部通过。压测那条：读线程停 8 秒期间，写线程把 25 200 275 字节请求全写进去了；224 个调用 1.0 秒收完 14 434 704 字节，各自内容对得上。
- **探针判据的反例：** 一个单线程"读一行、写 64 KiB"的假服务端（P106 看到会卡的那种结构），同样停 8 秒，写线程只写进去 1 050 106 / 25 200 275 字节，"服务端在 stdout 堆住时还在读 stdin"这条判 FAIL。说明这条判据分得出两种服务端。

### 3.2 Windows CI

GitHub Actions 运行 40498978439（提交 efaabd9，`windows-latest`），windows-runtime job 第一次就全绿，3 分 17 秒：

- `choco install ripgrep` 装的是 ripgrep 15.2.0。
- 单元测试 64/0（P107 时 49）。新增的 15 条都在 Windows 上跑了：`mcp::path::rules` 3 条、`mcp::path::windows_tests` 6 条（真 NTFS 目录；`mklink /J` 建的 junction 指出 workspace 时读写都报 `CCNM_E_POLICY`，指向里面时读跟过去、写它本身被拒；`.GIT/config`、经 junction 写 `jgit/config` 被拒；备用数据流、设备名、结尾点和空格、`GIT~1`、十种绝对路径写法被拒）、`platform::tests` 2 条、`mcp::search::hit_paths` 2 条、`safety::whoami` 2 条。
- `windows_gate` 3/0：mcp-serve 过了入口闸，`runtime-audit`、`runtime-cleanup` 仍报还不支持。
- `read_tools` 3/0：经真二进制起 read 会话，七个工具的回答里没有 `\`、没有根目录的任何一种写法，`git_subdir` 是 `sub/proj`，已安装 skill 的路径是 `C:/Users/runneradmin/…`；coding 会话在 initialize 之前被拒；四种名字经工具（`read_file`、`list_files`、`search_text`、`view_image`、`read_notebook`、`load_skill` 的 `file`）都报 `CCNM_E_POLICY`。这一条同时证明 Windows 上 `whoami` 认出了身份：认不出时会话根本起不来。
- 同一次运行的另外两个系统：macOS（`test` job）`cargo test --workspace` 1183/0，Linux（`linux-runtime`）1184/0，两边 Python 门禁 251 条 0 跳过，`read_tools` 都跑了。这是 P108.3 的前一半。
- release 构建的 `ccnm.exe` 作为 artifact 留了 7 天（`ccnm-windows-x86_64`，ID 12730989604），P108.4 用的就是它，本机下载后 sha256 是 `c871462202cbe1330f775042cb121720a37109ed5e3960636525767f506d725a`。

### 3.3 Win11 测试机（P108.3 的 U6、P108.4）

用户 2026-10-27 同意：只动测试机上 `%TEMP%\ccnm-p108` 这一个目录，测完整个删掉；不改 sshd、注册表、Defender、防火墙，不装软件。

怎么跑（`B` 是 `C:/Users/xdw/AppData/Local/Temp/ccnm-p108`，`P` 是探针）：

```bash
gh run download 40498978439 --repo xwfe/ccnm -n ccnm-windows-x86_64 -D 本地目录   # CI 编的那份
python3 -I P prepare 本地目录/stage --base B && COPYFILE_DISABLE=1 tar -C 本地目录/stage -cf 本地目录/p108.tar .
ssh win11 'New-Item -ItemType Directory -Force $env:TEMP\ccnm-p108 | Out-Null'
scp 本地目录/ccnm.exe 本地目录/p108.tar win11:AppData/Local/Temp/ccnm-p108/
ssh win11 'tar -xf $env:TEMP\ccnm-p108\p108.tar -C $env:TEMP\ccnm-p108; git -C $env:TEMP\ccnm-p108\repo init -q'
python3 -I P run --ssh win11 --base B --out 证据.json
ssh win11 'Remove-Item -Recurse -Force $env:TEMP\ccnm-p108'
```

`prepare` 生成的目录里有：配置（`external_mcp = "read"`、机器技能关掉）；包装脚本 `ccnm-p108.cmd`，它设好 `CCNM_CONFIG`、`XDG_STATE_HOME`、`HOME` 再调 `ccnm.exe`（sshd 不收 `SetEnv`，P106 的 U1）；一个 git 仓库往下一层的项目，里面有 8 MB 的 `big.txt` 和 5000 个小文件。

**连接这一轮走的是局域网。** 10-27 用户叫醒测试机后，它的 Tailscale 服务在跑（`Running`），但节点一直显示离线（"last seen 1d ago"），tailnet 地址连不上；局域网 mDNS 能解析 `WIN11.local`，22 端口开着。所以 ssh 多加 `HostName=192.168.31.157`、`HostKeyAlias=100.118.127.25`、`StrictHostKeyChecking=yes`：按 known_hosts 里 win11 原来那把主机钥匙严格核对，对得上才连。ccnm 自己那组 ssh 参数一项没改，探针的 `--ssh-option` 只能补 ccnm 没写的选项。测试机那边的 Tailscale 没动。

**测试机：** Windows 11 build 26200，账号 `win11\xdw`（管理员、经 sshd 是已提权的令牌，P106），默认 shell 是 PowerShell 5.1，Defender 实时保护开着（`RealTimeProtectionEnabled = True`），rg 是 scoop 的 shim，git 是 scoop 的 2.52。CI artifact 12730989604 的 `ccnm.exe` 拷过去之后 `Get-FileHash` 与本机一致，经包装脚本 `--version` 输出 `ccnm 0.20.0`。

**结果（两轮都全过，第二轮在改严压测判据之后、重新解开小文件之后跑）：**

- 握手约 0.5 秒（含 ssh 建连和起进程）。`tools/list` 正好七个只读工具。`workspace_info` 回 `inside git repository at sub/proj, windows/x86_64`：`whoami` 认出了身份，会话起来了；`git_subdir` 用 `/`。
- 七个工具都对，回答里没有 `\`、没有 `C:/Users/xdw/AppData/Local/Temp/ccnm-p108` 的任何一种写法。`read_file` 对 `README.md:hidden`、`NUL`、`src/con.txt`、`src./main.rs`、`SRC~1/main.rs`、`GIT~1/config` 都报 `CCNM_E_POLICY`。
- **双向大流量（P108.4 的硬要求）：** 224 个调用，其中 24 个各带 1 MiB 没声明的字段（请求共 25 200 275 字节），200 个各读 `big.txt` 的一页（回答共 14 434 751 字节）。读线程停 8 秒不读；写线程 0.69 秒就把 25 MB 请求全写进去了，那时服务端的 stdout 早已堆住。之后 224 个回答 0.3 秒收完，每个都是它自己那一页、那条说明。所以 rmcp 3.5.1 的读循环在输出积压时照样读 stdin，P106 的那种卡死没有出现。第一轮用的是改严之前的探针：写线程一写完读线程就恢复了，没停满 8 秒（判据仍成立，写完那一刻读线程还停着，只是 stdout 堆了多久说不准），从开始到全部收完 1.0 秒。
- 关 stdin 之后 mcp-serve 退出码 0，ssh 正常返回；测试机上没有残留的 ccnm 进程。
- 退出时 stderr 有一行 `WARN cannot list running mcp-serve processes; expired output is kept`：过期输出的清理要先列出在跑的 mcp-serve（`overview::try_scan_servers`），Windows 上列不出来，于是一个都不删。这是安全的那一边，而且只读会话不产生输出；输出要等 P110 的 `exec_command` 才有，记进 ROADMAP 的 P110。

**U6（Defender 拖慢多少）：** 没关 Defender、也没加排除（用户只同意动那一个目录），只量开着时的耗时，所以只能看冷热差，量不出"关掉会快多少"。每轮三件事，三轮：

| 第二轮测量（ms） | 第 1 轮（刚写过的文件） | 第 2 轮 | 第 3 轮 |
| --- | --- | --- | --- |
| `search_text` 扫 5000 个小文件（只有最后一个命中，rg 要读完全部） | 290 | 302 | 283 |
| `list_files` 列 1000 个 | 63 | 61 | 60 |
| 顺序 200 次 `read_file`，每次一个小文件 | 1653 | 1360 | 1313 |

基线：同一连接上顺序 200 次 `workspace_info`（不碰文件）1342 ms，即每次调用约 6.7 ms 的往返加服务端开销。扣掉之后，第一次读一个刚写过的小文件多 1.6 ms（第一轮测量时是 2500 ms，多 5.8 ms），再读几乎不多。rg 扫 5000 个文件冷热都在 0.3 秒左右。结论：Defender 开着时，只读工具的耗时在这台机器上不构成问题，文档里不需要建议加排除；写多读多的场景（编译、跑测试）到 P110 再量一次（设计文档 U6 本来就写的 P108、P110 各量一次）。

清理：测完 `Remove-Item -Recurse -Force %TEMP%\ccnm-p108`，`Test-Path` 为 False，`Get-Process ccnm` 没有进程。

## 4. 已知限制

- 真叫 `notes~1.txt` 这种名字的文件在 Windows 上读不了：规则只看形状，不调 `GetLongPathNameW` 去展开（要 unsafe，设计文档 D7）。
- 根目录比 260 个字符长、系统又没开长路径时，没测过。
- Windows 上只有 read 模式。受管会话（Agent 起的 Claude Code / Codex）还要等 P109 写锁、P110 执行、P111.1 Agent 侧认 Windows 路径。
