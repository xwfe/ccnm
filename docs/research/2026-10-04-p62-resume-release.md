# P62 续跑：发版 v0.10.1，补齐 Codex 那一半（2026-10-04）

接 [P62 第一轮](2026-09-30-p62-real-machine.md)。用户 10-04 要求"先发版升号，P62 恢复实施"：先发版，再在 Codex 额度恢复后续跑 P62。授权沿用第一轮的分项和上限（第一轮的授权不延续，这一轮按同一份清单重新执行）：三台机器与日用版本并存部署、三把一次性 SSH 密钥、每个 Provider 最多 8 次真实模型运行、只杀本轮进程的故障注入、跑完收尾。拓扑同第一轮：Operator 是 hpsrv 上的 `bing`，Runtime 执行账号是 hpsrv 上的 `ccrun`，Codex 的 Agent 是本机 xdwmbp，Claude 的 Agent 是 fodelf。

## 1. 结论

**发版完成：[v0.10.1](https://github.com/xwfe/ccnm/releases/tag/v0.10.1)。Codex 那一半全部跑通。P63–P67 修的各条在真机上复验通过。阶段仍没完成**：第一轮没跑的失败矩阵三项这次都跑了，其中两项结果不对（F22、F23）。另外查出 F20、F21、F24，见第 7 节。

| 判据 | 状态 | 依据 |
| --- | --- | --- |
| P62.1 授权、身份、版本与候选构建 | 完成（第一轮） | 本记录第 3 节为续跑的部署 |
| P62.2 两 Provider 受管闭环 | **完成**：Codex 先红后绿、精确停止一次确认（5.1）；Claude 第一轮已过，这次复验停止（5.7） | 5.1、5.7 |
| P62.3 外部 MCP 与 Machine API 真实组合 | **完成**：外部 Codex Host（5.6）、Machine API（Codex，5.2）与人类 CLI 对照（5.3）；Claude 的这几条第一轮已过 | 4.4、5.2、5.3、5.6 |
| P62.4 失败、写权、断线、清理 | **未完成**（本轮）；**2026-10-04 晚复验后完成**：F22、F23 由 P68 修好、真机复验通过，见 [P62.4 复验记录](2026-10-04-p62-4-recheck.md) | 5.4、5.5、第 6 节 |
| P62.5 候选包、安装、升级回退 | **完成**：v0.10.1 从 release 下载、按 sha256 校验后装到三台；升级与回退都对；Linux 门禁在 hpsrv 与线上 CI 都过 | 第 2 节、4.6、4.7 |
| P62.6 文档、状态、资源 | 完成 | 第 8 节 |

## 2. 发版

| 时间（JST） | 事 | 结果 |
| --- | --- | --- |
| 15:48 | 推送 main（`25b05f5`，含 0.10.0 版本号），线上 CI 第一次跑 P58 以来的代码 | 两个平台在 Clippy 就红：Rust 1.99.0 在 10-01 成为 stable，CI 装的是 stable，新 lint `needless_borrows_for_generic_args` 报 `work.rs` 的 `map_err(&redact)`。本机是 1.98，所以本地门禁看不到 |
| 15:55 | `ce75af3` 修掉，本机另装 1.99.0 工具链复验 | Clippy 过；Linux 的 Python 门禁红了一条：`test_rpc_exact_control` 里假 Agent 先写"已停"再回 NOT_READY，被测运行抢先结束，stop 照契约回终态 `failed`，用例只认 `stopping`。0.5 秒延迟稳定复现，`8ca3341` 改夹具 |
| 16:15 | CI 全绿（run 37184674226），打 `v0.10.0` 的 tag | release 的 macOS 门禁红：`exact_stop_against_a_real_terminal_waits_out_a_transport_that_lingers` 525 ms 就返回。重跑同样红（92 ms）。原因在用例：替身进程一出现在 ps 里就开始停，那时 Python 还没执行到忽略 SIGHUP；release 没有构建缓存、机器更忙，所以只在这里中。`f7b5914` 改成替身装好处理器后写 ready 文件再停，0.5 秒延迟下验证 |
| 16:30 | `v0.10.0` 的 tag 已公开、没有任何产物；不改写公开 tag，升 `0.10.1`（`33f9193`），只差那条测试与版本号 | CI 全绿后打 `v0.10.1` |
| 16:39 | release run 37185984467：Linux 4 分 33 秒、macOS 8 分 43 秒，publish 成功 | 四个文件齐全 |

| 文件 | sha256 |
| --- | --- |
| `ccnm-0.10.1-macos-universal.tar.gz` | `0cdabf87fbe7f162b0412f8afef6f05c2c3c4e2894d81c51ae84ec55e28b4e8a`（二进制 `5d8420fe…`，x86_64 + arm64） |
| `ccnm-0.10.1-linux-x86_64.tar.gz` | `08dc00a766059effe6b888561286c601c0a416286372e21b409c9ff66bd90bab`（二进制 `e913e4fd…`，glibc ≥ 2.39） |

release 页正文就是 tag 的注释：`7f0e018` 修的"读成提交说明"第一次在线上验证通过。推送时本机到 GitHub 22 端口的 SSH 握手卡在 `KEX_ECDH_REPLY`（TCP 是通的），改走本机 HTTP 代理连 `ssh.github.com:443`；这是本机网络的事，没有改用户的 ssh 配置。

## 3. 部署

| 身份 | 装了什么 | 配置与 state |
| --- | --- | --- |
| Agent（Codex）：本机 `bing` | release 的 macOS 包解到 `~/.local/opt/ccnm-0.10.1/`，日用 `~/.local/bin/ccnm`（0.9.0，`300dbd1d`）没动；Codex 0.154.0 与 `codex-code-mode-host` 从官方 release 下载、按 GitHub 给的 sha256 核对后放 `~/.local/opt/codex-0.154.0/`（不在用户 PATH 上）；Controller 用 v0.10.1 的 `controller install` 装在标准 Label 下（本机原来没有） | `~/.config/ccnm-p62/config.toml`（`this = "xdwmbp"`，`codex-main`）；state 在 `~/.local/state/ccnm-p62/` |
| Agent（Claude）：fodelf `fodelf` | 同一个包，解到 `~/.local/opt/ccnm-0.10.1/`；日用 0.9.0 和它的 Controller（pid 1075）没动；Claude Code 2.1.289 | `~/.config/ccnm-p62/config.toml`（`this = "fodelf"`，`claude-main`）；另起 Label `dev.ccnm.controller.p62` |
| Operator：hpsrv `bing` | release 的 Linux 包，`~/.local/bin/ccnm` | `~/.config/ccnm/config.toml`（Runtime 配置，与 ccrun 那份逐字相同） |
| Runtime 执行账号：hpsrv `ccrun` | `~/.local/bin/ccnm` 由第一轮的候选 `fdd898df` 换成 v0.10.1（候选在 `~/.local/opt/ccnm-p62-dabec34/` 有同一份，0.8.0 备份仍在） | 同上一份配置 |

项目：仓库样例 `rust-mini`（基线 + P57 的失败用例两个提交），这次**放在 ccrun 的家目录里**（`/home/ccrun/p62/`，家目录 0700），专门复验 F1；`claude`、`codex`、`read`、`off` 各一份克隆（root 不许重叠）。四个 workspace：`p62claude`（fodelf/claude-main，外部 coding）、`p62codex`（xdwmbp/codex-main，外部 coding）、`p62read`（只给外部 read）、`p62off`（只绑受管实例）。SSH：本机与 fodelf 各一把只能连 ccrun 的一次性密钥（`from=` 限定本机 IP、禁转发），hpsrv 的 `bing` 一把连本机的（同样限定）；两台 Agent 的主机指纹从两个方向各扫一次、一致后才写入。

和第一轮一样的偏离：

- **两台 Agent 各有一个 wrapper**（`~/.local/opt/ccnm-0.10.1/ccnm-p62`）导出这一轮的 `CCNM_CONFIG`、`XDG_STATE_HOME`（fodelf 还有 `TMUX_TMPDIR`），Runtime 配置的 `ccnm_bin` 指向它。经 ssh 调起的 ccnm 不继承 Controller 的环境，不这样就读到日用配置。第一次忘了它，doctor 报 `the Agent Node refused this Agent before probing anything: instance reference names another Agent Node`——这正好是 F10 的新说法在真机上第一次出现。
- **本机 Controller 的 plist 手工加了 `PATH`**，只为让 Controller 找到钉住的 Codex 0.154.0 而不是 Homebrew 的 0.158.0。ccnm 有意不写 `PATH`。`CCNM_CONFIG` 与 `XDG_STATE_HOME` 这次是 `controller install` 自己写的（F8，见 4.1）。
- **fodelf 上的第二个 Controller 用另一个 Label**，plist 由 `controller install --dry-run` 的输出改 Label、加 `TMUX_TMPDIR` 得来。

## 4. 零额度的真机结果

### 4.1 F8：`controller install` 带上非默认位置

本机以 `CCNM_CONFIG`、`XDG_STATE_HOME` 指向这一轮的位置跑 `controller install`：安装计划多两行 `with CCNM_CONFIG=…`、`with XDG_STATE_HOME=…`，plist 里两个变量都在，装完 `listening: ccnm 0.10.1 as bing, pid 42174, Aqua`，在 `~/.local/state/ccnm-p62/ccnm/controller.sock` 上。第一轮同样的做法等满 10 秒报 `nothing is listening`。

### 4.2 doctor：两个 workspace 都是 0 失败

Operator 跑 `ccnm doctor p62claude` 与 `p62codex`：**0 失败**。Agent SSH、Controller（Aqua）、Claude Code 2.1.289 / Codex 0.154.0、两边登录、反向 SSH（`hpsrv as ccrun, ccnm 0.10.1`）、执行账号的各项安全检查、Workspace root、MCP 握手（11 个工具，16 293 字节）都是 OK。没查的四行都是有意的：`Runtime workspace`（F1，见下）、Codex exec-server（没开）、Native tool policy、Network isolation。Codex 认证那一行带着 P65 加的"只看了本地"（F5）。

### 4.3 F1：项目在执行账号 0700 的家目录里

| 位置 | 第一轮 | 这次 |
| --- | --- | --- |
| doctor `Runtime workspace` | `FAIL CCNM_E_INTERNAL: cannot stat` | `SKIP not checked: bing is not allowed to look at /home/ccrun/p62/claude (Permission denied)…`，并指向 `Workspace root` 行；那一行是 OK |
| `ccnm run` | 退出 30 | 正常开会话（5.1 的会话就是这么开的） |
| `ccnm workspace add`（用默认节点名的配置） | 退出 30 | 登记，并说明"按写下的路径登记，没核对、没解析符号链接" |
| `ccnm workspace list` | "不在这台机器上" | `this account may not look at it` |

`workspace add` 用这一轮的配置时被拒：它写出的条目是默认的 `agent_node = "agent"`、`runtime_node = "runtime"`，而这份配置里节点叫 hpsrv、fodelf、xdwmbp。拒得清楚、什么都没写，见 F24。

### 4.4 外部 MCP：`p12_dogfood_check.py` 全过

本机当 Host，`ccnm-p62 mcp bridge` 打到 ccrun：`p62claude`（coding）、`p62read`（read）、`p62off`（没开放）。退出 0，**12.6 秒**（第一轮 24.6 秒）：工具表、read 腿硬调四个写工具被拒、身份审计、改坏后在 Runtime 上构建失败并指向文件、收回后 git 干净、写锁 busy、越权与未开放被拒、协议 99、远端 `mcp-serve` 被杀后写锁不自动交权（按手册恢复后可进）、Host 崩没有孤儿、泄漏扫描干净。

### 4.5 版本号与构建（F2）、失败原因（F3）、连不上（F14）

| 场景 | 实际 |
| --- | --- |
| 新 Operator（0.10.1）→ fodelf 日用的旧 Agent（0.9.0） | doctor 只有一行失败：`Agent selection FAIL CCNM_E_VERSION: Agent probe identity differs from the Runtime selection`，版本那几行根本没出现（F20）。Machine API `session.start` → 1 秒 `failed`，`failure` 是 `-32002 CCNM_E_VERSION` 加 Agent 那边的原话（F3 复验通过） |
| 旧 Operator（0.9.0 候选）→ 新 Agent（0.10.1） | `Agent ccnm` 与 `Reverse SSH` 都是 `CCNM_E_VERSION: … runs ccnm 0.10.1, this machine runs 0.9.0`。**旧的一端现在也看得出来**——F2 剩下的那一半靠升号解决了 |
| Agent 的 ssh 别名解析不了（Linux Operator） | Machine API 0 秒 `failed`，`failure` 是 `-32004 CCNM_E_AGENT_UNREACHABLE` 加 `Could not resolve hostname`。第一轮是 `unknown`（F14 复验通过） |

### 4.6 回退与升级

ccrun 换回第一轮的候选（0.9.0）：Operator doctor 的反向 SSH 行 `CCNM_E_VERSION: the Runtime Node runs ccnm 0.9.0, this machine runs 0.10.1`，MCP 握手照样 OK（旧 `mcp-serve` 还认协议 4）。换回 v0.10.1：0 失败。两次都是"新文件 + rename"。

### 4.7 Linux 门禁

hpsrv 上以 ccrun 用当前源码：`cargo test --workspace --locked` **1059/1059**；`ci_gates.py` 的 Python 262 条 0 失败、1 条跳过（ccrun 本身就是合格的执行身份，"拒绝开发者自己账号"那条用例的前提不成立，与第一轮相同）。第一轮因 F14 红的两条这次过了。线上 CI 的 ubuntu job 也全绿。

## 5. 真实模型回合

额度：每个 Provider 最多 8 次。**Codex 用了 6 次，Claude 用了 4 次**。Claude 报了价的两次合计约 $0.04（`start` 后立即断开 $0.0145、分页源头丢失那次 $0.024），另两次被停或被杀，没有报价。

### 5.1 REAL-02 受管 Codex（本机 → hpsrv）

Operator `ccnm run p62codex --detached`，会话在本机的 tmux 里，我往那个 tmux 发按键。第一次启动停在 Codex 的信任提示（问的是 ccnm 的空占位目录，按使用说明选 Yes）。之后 `tools connected`、写锁由这个会话持有。任务："`hours_are_supported` 失败了，先 `sleep 20 && cargo test --offline`，再修 `src/lib.rs`，再测，不要 commit"。

- 模型（`gpt-6-astra`，0.154.0 的默认）用 `exec_command` 跑命令：hpsrv 上 `sleep 20` 属于 ccrun、自己一个进程组。**没有任何审批提示**（F21）。
- 看到失败 → `apply_patch` 加 `"h" => 3_600_000` → 3 条全过。ccrun 独立再跑：退出 0，改动只在 `src/lib.rs`（+1 行），属主 ccrun，没有 commit。
- `ccnm stop p62codex --session 1c456e87…`：**第一次就 `stopped`**，0.6 秒，退出 0；写锁 `free`；hpsrv 上没有残留；`ccnm log` 记 `stopped`、`2m`。第一轮 Codex 停止 3/3 先报 NOT_READY，log 写成 `failed to start`（F4 复验通过）。

### 5.2 Machine API + Codex

驱动脚本在 hpsrv 上以 Operator 身份跑，只 import 仓库的 `ccnm_machine_client.py`。任务：新建带随机 token 的文件、跑测试、只报告不修。31.9 秒 `completed`，退出 0，`usage` 到达调用方（Codex 不报 `cost`，与支持矩阵一致）；`text` 正确指出 `hours_are_supported` 与原因；stdout 4 286 字节以 64 KiB 与 97 字节两种页大小读回、sha256 一致、`complete`；`max_bytes=1` 正常；同键重发 `reused: true`。文件属 ccrun。

### 5.3 人类 CLI 对照

`ccnm run p62codex --print` 同一任务、另一个 token：退出 0，Codex 30.6 秒，结论一致，文件属 ccrun。末尾一行 `session directory on xdwmbp: …`（F18 复验通过）。

### 5.4 运行中停止（F17）与命令收尾和写锁的先后（F19）

两个 Provider 各一次：Machine API 起一个前台跑 `sleep 150` 的任务，等 hpsrv 上出现 ccrun 的 `sleep 150` 就 `session.stop`。同时以 root 在 hpsrv 上每 20 ms 采一次进程表与写锁标记（只在变化时记一行）。

| | Codex | Claude |
| --- | --- | --- |
| `session.stop` | `failed`，`stop_requested: true`；再调一次相同 | 同左；`outcome.exit_code` 143（SIGTERM） |
| 第一轮 | — | `-32000 Agent process group has not ended`，`stop_requested: false`（F17） |
| 命令出现 → 停止 | 16:49:03.755 → 16:49:05 | 16:52:11.795 → 16:52:13 |
| 命令消失与写锁 `released` | **同一次采样**，16:49:10.859 | **同一次采样**，16:52:19.232 |

F17 复验通过。F19：两次都没有"命令还在、写锁已放"的窗口（20 ms 分辨率）。顺带看到一件要知道的事：Machine API 在停止的那一刻就回了终态 `failed`，而 Runtime 上的命令又跑了约 6 秒才收掉——这期间写锁一直 `held`，新的写者会被拒成 busy，不会进来。终态说的是 Agent 进程结束了，不是 Runtime 上的命令都收完了。

### 5.5 `session.start` 后立即断开（F16）

两个 Provider 各一次：`session.start` 一返回就关掉客户端。另一个连接看到 `running`，随后 `completed`，`text` 正是要求的那一行，记录 `dispatched: true`。第一轮是"根本没派发、永远 `unknown`"。复验通过。

### 5.6 外部 Codex Host（第一轮没跑）

本机 Codex 0.154.0 当 Host，在一个空临时目录里 `codex exec`，参数取 ccnm 受管适配器在 0.154.0 上测过的那一套（关掉 shell、unified_exec 等全部内建执行路径，`--sandbox read-only`，Code Mode 排除 `functions` 命名空间），唯一的工具来源是 `ccnm mcp bridge p62codex --mode coding`。24 秒，9 次工具调用全部走 ccnm（`workspace_info`、`exec_command` ×4、`read_file` ×2、`list_files`、`apply_patch`），修好并测过；ccrun 独立核对通过，改动只在 `src/lib.rs`，没有 commit；Host 的临时目录跑完还是空的——没有绕开 ccnm 改本机。

### 5.7 受管 Claude 的精确停止（不花额度）

fodelf 上起 `p62claude` 交互会话，答完信任提示（默认选中 "No, exit"，按一次下箭头再回车；这次没有问 auto mode），`tools connected` 后不发消息直接精确停止：第一次 `stopped`，0.6 秒；`ccnm log` 记 `stopped`；写锁 `free`。F4 在 Claude 上也复验通过。

## 6. 失败矩阵补跑

### 6.1 Agent 上的监督进程丢了：结果不对（F22）

Machine API 起一个跑 `sleep 60` 的 Claude 任务，等命令出现后，在 fodelf 上只对这一轮的 `ccnm internal supervise`（pid 2711）发 SIGKILL。

- `claude` 被 launchd 收养后继续跑；它的 stdout 是通往已死监督进程的管道，后来退出时一个字节都没留下。Runtime 上命令照常跑完，`mcp-serve` 退出，写锁 `released`——这一侧没问题。
- `agent-run` 一直在等：它每 250 ms 看一次有没有结局文件，直到"超时 + 30 秒"，从不检查监督进程还在不在。Agent 自己的 `ccnm log` 写 `no end record`。
- **调用方看到的**：`running` 一直到我发 `session.stop`，之后 `stopping`；**15 分半之后**（900 秒超时加 30 秒）才变成 `unknown`，`failure` 是 `-32603 … no exit record after 930s … the supervisor did not finish`，`output.unavailable_reason` 写成了 `agent_refused`（Agent 实际说的是"还没结束"）。
- `unknown` 本身是对的终态（被收养的 `claude` 可能还在跑，证明不了结束），错在两处：一个已经死了的监督进程，要等满超时才被发现，这期间调用方被告知 `running`；以及原因标签不对。（后注：P68 核对后，标签这一条不成立——Agent 确实答复了，这份输出以后也拿不到，正是 `agent_refused` 的定义；等满超时那一条已修，同日晚真机复验通过。见 [P68 记录](2026-10-04-p68-supervisor-gone-lost-output.md)第 3 节、[P62.4 复验记录](2026-10-04-p62-4-recheck.md)。）
- 余波：Runtime 那边这次运行的输出目录只有 `stdout`/`stderr`、没有 `status`，`ccnm cleanup` 因此一直列成"没结束"而保留，只能等 7 天过期。（后注：保留是对的，原因说错了——正常结束的会话也没有 `status` 文件，决定保留的是这次会话没有结局，见 [P62.4 复验记录](2026-10-04-p62-4-recheck.md)第 6 节。）

### 6.2 分页的源头丢了：结果不对（F23）

Machine API 跑完一个 Claude 任务（stdout 2 509 字节），**只用 `session.status` 等到 `completed`、不调 `session.result`**——RPC 这时还没拷过快照。然后把 fodelf 上那次会话的 `stdout` 挪走，再调 `session.result` 和 `read_output`：

- 得到 `bytes_total: 0`、`source_bytes: 0`、`complete: true`、`unavailable_reason: null`——等于说"输出就是空的，你已经拿全了"。
- `text` 还在，因为它在会话结束时就从结果里解析出来了。

P59 的约定是拿不到完整内容时如实降级、写明 `unavailable_reason`。原因在 `session/view.rs`：建视图时原始输出文件 `NotFound` 被当成 0 字节。每个结束了的会话在启动时就建了这个文件，所以"没有"只能是丢了，不是"本来就空"。

### 6.3 清理部分失败：通过

在 fodelf 上给一份会删的会话记录里的文件加 `chflags uchg`，重新预览后 apply：那一项 `FAILED … PermissionDenied`，其余 9 项删掉，退出 3，并提示"再预览一次重试剩下的，删掉的不会回来"。去掉标志后重新预览只剩那一项，apply 删掉，退出 0。整个过程中 F22 那次会话的三处东西一直按"说不清结束没有"保留。最后 `p62codex` 的清理一次删掉 12 项（Operator 3、本机 Agent 5、ccrun 4），0 剩余。

## 7. 新发现

续第一轮的编号。

| 编号 | 影响 | 现象（真机） | 原因 | 建议 |
| --- | --- | --- | --- | --- |
| F20 | 低；**P69 已修，2026-10-04 晚真机复验通过** | 新 Operator 对着版本号不同的旧 Agent，doctor 唯一的失败行是 `Agent probe identity differs from the Runtime selection`（码是 VERSION），`Agent ccnm` 的版本行根本不出现 | instance workspace 先比身份、再出版本行；旧 Agent 不认这次的请求，回来的报告没有身份 | 先比版本：Agent 报的版本号或内部协议与本机不同，就以版本行为主要失败，不再比身份 |
| F21 | 中；doctor 那一行 **P69 已修（离线）**，Codex 仍不审批 | 受管 Codex 交互会话里 `exec_command` 不经审批就执行；doctor 对 Codex workspace 仍显示 `Command approval OK: interactive sessions ask before each exec_command, in every permission mode`，使用说明和配置说明也这么写 | "每次都问"靠的是 Claude Code 才认的 `anthropic/requiresUserInteraction`；Codex 一侧 ccnm 设了 `default_tools_approval_mode="approve"`（不设的话 `approval_policy="never"` 下调用全被拒，见 [Codex 探针记录](codex-provider-probe-2026-09-07.md)） | doctor 这一行按 Provider 说实话；文档写明 Codex 会话不问（已改，见第 8 节）；要不要给 Codex 补一道审批另行决定 |
| F22 | 中；**P68 已修，2026-10-04 晚真机复验通过**，标签核对后不改，见 P68 记录第 3 节 | Agent 上的监督进程被杀后，Machine API 报 `running`/`stopping` 15 分半，才变成 `unknown`；`unavailable_reason` 写成 `agent_refused` | `agent-run` 只等结局文件，不看监督进程是否还活着 | 等结局时同时看监督进程：没了又没有结局，立刻按 `unknown` 收尾；原因标签按 Agent 的回答映射 |
| F23 | 中；**P68 已修，2026-10-04 晚真机复验通过** | Agent 上的原始输出丢了之后，`session.result` 回"空且完整"的输出，`unavailable_reason` 为空 | 建视图时 `NotFound` 当成 0 字节 | 结束了的会话缺原始输出时报"拿不到"，让 RPC 降级并写明原因 |
| F24 | 低；**P69 已修，2026-10-04 晚真机复验通过**，加了 `--agent-node` | `ccnm workspace add` 在节点不叫 `agent`/`runtime` 的配置里写不进去（被拒，什么都没写） | 生成的条目固定写默认节点名，命令没有选节点的参数 | Runtime 上 `runtime_node` 默认取 `this`；需要时加 `--agent` 之类的参数 |

另记：Machine API 的终态早于 Runtime 上命令收完约 6 秒（5.4），期间写锁一直持有，这是设计如此，不是缺陷；Claude Code 这次升到 2.1.289，首启没再问 auto mode；fodelf 的 Claude 登录提示"3 天后过期"。

## 8. 授权、资源与收尾

没有替换任何日用二进制或 Controller，日用配置一个字没动：本机与 fodelf 的 `~/.local/bin/ccnm` 仍是 `300dbd1d`（0.9.0），fodelf 的日用 Controller（pid 1075）一直在跑。

| 位置 | 本轮建的 | 现在 |
| --- | --- | --- |
| 本机 | Controller（`ccnm controller uninstall` 卸掉；本机原来没有）、`~/.config/ccnm-p62/`、`~/.local/state/ccnm-p62/`、`~/.local/opt/ccnm-0.10.1/`、`~/.local/opt/codex-0.154.0/`、`~/.ssh/ccnm-p62-ccrun*` | 已删；`~/.ssh/config` 与 `authorized_keys` 核对"现状 = 备份 + 本轮追加"后从备份恢复 |
| 本机 | Rust 1.99.0 工具链（`--profile minimal`，带 clippy/rustfmt，默认工具链没变） | **保留**：CI 用的就是它。不要可以 `rustup toolchain uninstall 1.99.0` |
| fodelf | `dev.ccnm.controller.p62` 及其 tmux server、`~/.config/ccnm-p62/`、`~/.local/state/ccnm-p62/`、`~/.local/opt/ccnm-0.10.1/`、`~/.ssh/ccnm-p62-ccrun*` | 已删；`~/.ssh/config` 与 `known_hosts` 从备份恢复 |
| hpsrv ccrun | `authorized_keys` 两行、`~/.config/`、`~/.local/state/ccnm/`、`~/p62/`、`~/p62b-build/` | 已删；`authorized_keys` 与本轮前逐字节相同（0 字节） |
| hpsrv ccrun | `~/.local/bin/ccnm` = **v0.10.1**（`e913e4fd…`），`~/.local/opt/ccnm-0.10.1/` | **保留**：它替换的是第一轮留下的未发布候选，留发布版更不意外。回滚：`install -m 755 ~/.local/opt/ccnm-p62-dabec34/ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm`（0.8.0 备份也还在） |
| hpsrv bing | `~/.local/bin/`、`~/.local/state/`、`~/.config/`、`~/p62b/`、`~/.ssh/config`、`known_hosts`、`known_hosts.old`、`ccnm-p62-xdwmbp*` | 已删；原有的 `~/.local/share`、`authorized_keys` 未动 |
| hpsrv root | `/root/ccnm-p62b-20261004/`（清单、脚本、采样结果） | 已删；内容保存在本轮会话的临时目录 |

原始证据（Machine API 各场景的 JSON、F19 采样、外部 Host 的事件流）没有放进仓库：它们含家目录路径与会话 id，结论和关键数字已抄进本记录。
