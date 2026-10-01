# P62 双 Provider 真实闭环与候选包验收（2026-09-30）

范围见[基线与真机验收方案](../plan/core-verification.md)第 2 节。这一轮是 P57–P61 之后**第一次上真机**：macOS Agent → hpsrv（Debian 13 / x86_64）Linux Runtime，执行账号 `ccrun`。Codex 的 Agent 是本机 xdwmbp；本机 Claude CLI 没有登录，用户指出 Claude 只在 fodelf 上登录，于是 Claude 的 Agent 改成 fodelf。用户当轮逐项批准了：三台机器部署候选构建（与日用版本并存）、三把一次性 SSH 密钥、每个 Provider 最多 8 次真实模型运行、只杀本轮进程的故障注入、跑完撤钥匙但保留 hpsrv 上的候选构建。**没有 push、没有打 tag、没有发版。**

## 1. 结论

**阶段没有完成：Claude 这一半在真机上走通了，Codex 这一半被账号额度挡住，另外查出 3 条需要先修的产品缺陷。**

- **Claude（Agent = fodelf）**：受管交互闭环（审批中与命令运行中各一次 detach/reattach、先红后绿、精确停止）、外部 Claude Code 经 bridge 修 bug、Machine API 完成任务与分页读回、人类 CLI 对照、运行中停止、RPC 进程被杀，全部在 hpsrv/ccrun 上真实执行并由执行账号独立核对。P61 的清理第一次在三个真实账号（hpsrv Operator uid 1000、fodelf Agent uid 501、hpsrv ccrun uid 1002）上各删各的。
- **Codex（Agent = 本机）**：ccnm 专用 profile 先是刷新令牌被吊销（doctor 仍说已登录，F5），用户重新登录后又碰到 ChatGPT 账号额度用尽，要到 **2026-10-04 13:21** 才恢复。受管会话起得来、工具连得上、busy 与精确停止都验了，但一次模型回合都没跑成；外部 Codex Host 同样没跑。
- **需要先修才能算通过的**：F16（冻结协议 8.1 节承诺"客户端断开任务照跑"，真机上 `session.start` 后立刻断开，任务根本没派发、永远 `unknown`）、F17（Machine API 运行中停止：停下了却回错误、`stop_requested` 记成 false）、F14（Linux Operator 连不上 Agent 记 `unknown`，Linux 门禁因此 2 条红）。另有两条高影响但有绕法的：F1（Debian 家目录 0700 让 Operator 误报项目不存在）、F2（候选与旧构建都叫 0.9.0，doctor 分辨不出）。共 19 条，见第 5 节。

| 判据 | 状态 | 依据 |
| --- | --- | --- |
| P62.1 授权、身份、版本与候选构建 | 完成 | 2.1、2.2、第 6 节 |
| P62.2 两 Provider 受管闭环 | **受阻**：Claude 通过（4.1）；Codex 账号额度到 10-04 | 4.1、4.2 |
| P62.3 外部 MCP 与 Machine API 真实组合 | **部分**：外部 Claude Code、Machine API（Claude）通过；外部 Codex、Machine API（Codex）未跑 | 3.2、3.3、4.3、4.4 |
| P62.4 失败、写权、断线、清理 | **部分**：多数通过；F16、F17 未过；Agent/监督进程失联、分页源头丢失、清理部分失败未跑 | 3.3–3.6、4.4、4.5 |
| P62.5 候选包、安装、升级回退 | **部分**：两平台打包、安装、回退升级通过；Linux Python 门禁因 F14 未过；线上 CI 未跑（未推送） | 2.2、3.4、3.7 |
| P62.6 文档、状态、资源 | 完成 | 第 6 节；手册、支持矩阵、README 已同步 |

## 2. 环境与部署

### 2.1 三个身份

| 身份 | 机器 / 账号 | 装了什么 | 配置与 state |
| --- | --- | --- | --- |
| Agent | xdwmbp（macOS 26.6.2 arm64）/ `bing` uid 501 | 候选构建在 `~/.local/opt/ccnm-p62-dabec34/`，**没有替换**日用的 `~/.local/bin/ccnm`（0.9.0，`300dbd1d`，是 fodelf 日用配对的 Runtime）；Controller 是 `dev.ccnm.controller` LaunchAgent；Claude Code 2.1.284（`/opt/homebrew/bin`，本轮中途被自动升级过，开始时是 2.1.281）；Codex 0.154.0（官方 release，放在 `~/.local/opt/codex-0.154.0/`，不在用户 PATH 上） | `~/.config/ccnm-p62/config.toml`（`this = "agent"`、`runtime_node = "hpsrv"`、`claude-main` / `codex-main` 两个实例）；state 在 `~/.local/state/ccnm-p62/` |
| Agent（Claude） | fodelf（macOS 26.3 arm64）/ `fodelf` uid 501 | 候选构建同样并存在 `~/.local/opt/ccnm-p62-dabec34/`，日用 0.9.0（`300dbd1d`）和它的 Controller（`dev.ccnm.controller`，pid 1075）一直在跑、没有动；本轮另起 `dev.ccnm.controller.p62`；Claude Code 2.1.285（`~/.local/bin/claude`），claude.ai max 登录 | `~/.config/ccnm-p62/config.toml`（`this = "fodelf"`，只有 `claude-main`）；state 与 tmux socket 都在 `~/.local/state/ccnm-p62/` |
| Operator | hpsrv / `bing` uid 1000（在 sudo 组，是用户本人） | 候选构建 `~/.local/bin/ccnm` | `~/.config/ccnm/config.toml`（Runtime 配置，和 ccrun 那份逐字相同） |
| Runtime 执行账号 | hpsrv / `ccrun` uid 1002（只在自己的组） | 候选构建 `~/.local/bin/ccnm`，0.8.0 备份在 `~/.local/opt/ccnm-0.8.0/` | 同上一份配置；`mcp-serve` 从它解析 workspace 与 root |

SSH：Agent → `ccrun@hpsrv` 用本机新生成的 `~/.ssh/ccnm-p62-ccrun`，ccrun 的 `authorized_keys`（原本 0 行）加一行并限定 `from="100.107.211.119"`；Operator → Agent 用 hpsrv 上 bing 新生成的 `~/.ssh/ccnm-p62-xdwmbp`，本机 `authorized_keys` 加一行限定 `from="100.116.207.8"`。两端 `~/.ssh/config` 各加一段带 `ccnm-p62` 标记的 alias，改前都有备份。本机主机指纹从 hpsrv 侧 keyscan 后与本机 `/etc/ssh/ssh_host_ed25519_key.pub` 核对一致（`SHA256:nICfPd4s…`）。Claude 改到 fodelf 之后，fodelf → ccrun 另用一把只在 fodelf 上的一次性密钥（`from="100.79.121.33"`）；Operator → fodelf **不需要密钥**：fodelf 的 22 端口是 Tailscale SSH，hpsrv 上的 `bing` 按 tailnet 规则就能以 `fodelf` 身份登录（实测，这是 tailnet 的授权，不是 ccnm 的），fodelf 的主机指纹从 hpsrv 与本机各扫一次、一致后才写入。ccrun 仍然一把私钥都没有。

项目：仓库样例 `tests/fixtures/sample-projects/rust-mini`（不含 `task/`），在 hpsrv 上 `git init`，两次提交：`4ac65f6` 基线、`71b0014` 加上 P57 的 `01-hours-test.patch`（`cargo test` 退出 101，失败在 `hours_are_supported`）。`target/` 写进 `.git/info/exclude`，不改项目文件。**这是测试 workspace，不是生产项目交付证据。**

### 2.2 候选包

| 包 | 怎么来的 | sha256 |
| --- | --- | --- |
| `ccnm-0.9.0-macos-universal.tar.gz` | 本机 `bash scripts/dist.sh`（与 release.yml 同一条命令），2 分 17 秒；临时装了 `x86_64-apple-darwin` 编译目标 | 包 `2c30c618…`，二进制 `9993b64e…`（x86_64 + arm64） |
| `ccnm-0.9.0-linux-x86_64.tar.gz` | hpsrv 上以 ccrun 身份 `bash scripts/dist-linux.sh`（原生构建，源码是 `git archive HEAD` 的 `dabec34`），1 分 26 秒 | 包 `244335fa…`，二进制 `fdd898df…`，glibc ≥ 2.39 |

两端都从包里装，装前按 `.sha256` 校验；落地一律"新文件 + rename"。`scripts/dist.sh` 在仓库里没有执行位（`100644`），直接 `scripts/dist.sh` 会报 permission denied，release.yml 用的是 `bash scripts/dist.sh`，所以线上不受影响。

### 2.3 为了隔离做的三处偏离

- **Controller 的 plist 手工加了三个环境变量**：`CCNM_CONFIG`、`XDG_STATE_HOME`、`PATH`（只含 Codex 0.154.0 所在目录和系统目录）。本机日用配置是 `this = "runtime"`，Controller 默认读它就当不了 Agent；`ccnm controller install` 生成的 plist 不带这些变量（见发现 F8）。这是测试部署的偏离，不是产品支持的装法。
- **项目放在 `/srv/ccnm-p62/`**（父目录 root 0755，项目目录 ccrun 0700），而不是运维手册推荐的执行账号家目录——后者在 Debian 13 上让 Operator 的 `ccnm run` 直接被拒（发现 F1）。
- **`~/.claude` 从 0755 收紧到 0700**：ccnm 要求 Claude profile 目录私有（P7 就撞过）。同账号使用不受影响；收尾时已恢复。
- **fodelf 上第二个 Controller 用了另一个 Label**（`dev.ccnm.controller.p62`，手写 plist，环境里多 `CCNM_CONFIG`、`XDG_STATE_HOME`、`TMUX_TMPDIR`）：那台机器的 `dev.ccnm.controller` 是用户日用的。`TMUX_TMPDIR` 让它的 tmux server 和日用那个分开（ccnm 固定用 `-L ccnm`，不分开就会进同一个 server）。代价：`ccnm controller status/uninstall` 只认日用那个 Label，收尾要手动 `launchctl bootout`。

## 3. 零额度的真机结果

这一节没有调用任何模型。两个 Provider 的登录在开始时都不可用（Claude CLI 未登录；ccnm 专用 Codex profile 的刷新令牌已被吊销，见 F5），所以先把不需要模型的都做完。

### 3.1 doctor：两个方向、两个实例

Operator（hpsrv bing）跑 `ccnm doctor p62rust`，项目移到 `/srv` 之后，除 Claude 未登录外全绿：Agent SSH `bing@100.107.211.119`、Controller `pid 53497, Aqua`、反向 SSH `hpsrv as ccrun`、Runtime user / No sudo / Not an admin / No SSH keys / No Claude credential / No Docker socket / exec_command confined 全部 OK，MCP 握手 1034 ms、11 个工具、`tools/list` 16 293 字节。`--agent codex-main` 是 **0 失败**：Codex 0.154.0 从隔离的 PATH 找到、认证 OK（但见 F5）。

Agent 侧（本机）跑同一个 doctor 时，`~/.claude` 还是 0755，Controller / Claude Code / Claude authentication 三行报 `CCNM_E_AUTH: dedicated Agent home must be private…`；同一时刻 Operator 侧只报 `Agent probe identity differs from the Runtime selection`，把原因吞了（F10）。

### 3.2 外部 MCP：`p12_dogfood_check.py` 全过

本机当 Host，候选构建的 `ccnm mcp bridge` 打到 ccrun。三个 workspace：`p62rust`（`external_mcp = "coding"`）、`p62read`（`read`）、`p62off`（没开放，只绑受管实例）。第一次跑被拒：`CCNM_E_POLICY: Runtime workspace roots overlap after canonicalization`——脚本说明里"同一棵树上再配一个 read"的做法在 roots 不许重叠的规则之后已经不成立（F12），改成各克隆一份之后：

```text
scripts/p12_dogfood_check.py --workspace p62rust --read-only-workspace p62read --closed-workspace p62off \
  --node hpsrv --ccnm ~/.local/opt/ccnm-p62-dabec34/ccnm-p62 --runtime-user ccrun \
  --runtime-home /home/ccrun --other-home /home/bing --ssh-alias hpsrv-ccrun \
  --patch-target src/lib.rs --anchor 'pub fn parse_duration_ms(text: &str) -> Result<u64, String> {' \
  --build-cmd 'cargo build --offline --locked' \
  --test-cmd 'cargo test --offline --locked --test duration -- --skip hours'
→ exit 0，24.6 秒
```

| 检查 | 实际 |
| --- | --- |
| 工具表 | coding 11 个，read 7 个；read 腿按名字硬调 `exec_command` / `apply_patch` / `read_output` / `stop_command` 全部 `CCNM_E_POLICY` |
| 身份 | `ccrun`，组只有 `ccrun`，`sudo -n` 退出 1，docker socket 不可写，`~/.ssh` 里只有 `authorized_keys`，无 `SSH_AUTH_SOCK`，读不到 `/home/bing` |
| 工具链（由 `exec_command` 在 Runtime 上问出） | cargo 1.98.1、rustc 1.98.1、node v24.21.0、npm 11.19.0、git 2.47 |
| 改 → 编 → 收回 | 在 `src/lib.rs` 插一行必定编不过的代码，Runtime 上构建退出 101 且报错指向该文件；`read_output` 分两页（557、33 字节）读完；删掉后测试 `2 passed; 1 filtered out`，`git status --porcelain` 为空 |
| 写锁 busy | 第二个 coding 会话 `CCNM_E_POLICY: workspace write guard is busy` |
| 越权 / 没开放 | `p62read` 要 coding：`allows external MCP in read mode; coding was requested`；`p62off`：`is not available to external MCP` |
| 断开重连 | 同一棵树、同一版本 |
| 协议 99 | `CCNM_E_VERSION`，退出 11 |
| 远端 `mcp-serve` 被杀 | 客户端 Broken pipe；写锁**不自动交权**（`left held by an interrupted process`），read 照开；人工删掉那一个 marker 后 coding 能进 |
| Host 崩（杀 bridge） | 远端无孤儿，写锁 released |
| 泄漏扫描 | 返回内容里没有 `.claude`、`.codex`、`auth.json`、私钥名 |

### 3.3 Machine API：失败分支（独立客户端，在 hpsrv 上以 Operator 身份跑）

驱动脚本只 import 仓库里的 [`ccnm_machine_client.py`](../../clients/python/ccnm_machine_client.py)（复制到 hpsrv，不带仓库）。

| 场景 | 实际 |
| --- | --- |
| `hello` / `agents.list` | `ccnm.machine/1`，modes `print`，output_streams `stdout`/`stderr`；只列出被 workspace 绑定的 `claude-main`（`codex-main` 要在 start 时显式指定） |
| Claude 不可用（未登录） | 2.5 秒到 `failed`；`text` null、`exit_code` null、stdout/stderr 都是 0 字节——**调用方看不到任何原因**。Operator 自己的记录里 `finish.error` 写着 `Claude is not authenticated on the Agent Node`（F3） |
| Codex 不可用（令牌吊销） | 16.8 秒到 `failed`，`exit_code` 1，`text` 带出 Codex 的原话 `refresh token was revoked`，stderr 7 146 字节可分页读回 |
| 同键同输入 / 同键异输入 | `reused: true` 同一句柄 / `-32010`，`effect: none` |
| 对终态 stop | 幂等，状态不变 |
| 非法句柄 `../../etc/passwd`、`/tmp/x`、空串 | `-32602`（`CCNM_E_INVALID_ARGS`），status/result/stop 三个方法一致 |
| 格式合法但不存在 | `-32009 no such session` |
| 错 node / 未知 workspace | `-32009 no such workspace or instance`（不告诉你是哪个错） |
| 只开外部 MCP 的 workspace | `-32000 this workspace has no Agent instance binding` |

### 3.4 回退、升级与"版本号相同"

- **ccrun 退回 0.8.0**（新文件 + rename）：Operator doctor 的反向 SSH 行 `CCNM_E_VERSION: the Runtime Node runs ccnm 0.8.0, this machine runs 0.9.0`；MCP 握手行照样 OK（旧 `mcp-serve` 还认协议 4）。Machine API `session.start` → `failed`，原因只在 Operator 记录里（`install the same build on both before starting a session`）。换回候选构建后 doctor 恢复。
- **旧 0.9.0 当 Agent、候选 0.9.0 当 Runtime**（临时 wrapper 指向本机日用的 `300dbd1d`）：doctor **0 失败**——两边都说自己是 0.9.0；`session.start` 在 Agent 端被内部协议拒绝（`message is not valid for protocol 1; ccnm versions probably differ … unknown field session`），无副作用，但调用方仍只看到无原因的 `failed`（F2、F3）。

### 3.5 跨账号清理（P61 第一次跨 UID）

`ccnm cleanup p62rust` 在 hpsrv 上由 Operator 发起：预览列出 Operator（uid 1000）4 份 Machine API 结果拷贝、Agent（uid 501）2 条会话记录，给出令牌；`--apply` 删掉 6 项 30.6 KiB，退出 0。本机的会话目录是由 uid 501 自己删的，Operator 没有碰别人的目录。清过的句柄：`status` 仍是 `failed`，`result` 回 `-32012`（`reason: cleaned`），同一个 `start_key` 再 start 回原句柄 `reused: true`、不重跑。

零额度阶段执行账号那边没有可清的东西：bridge 会话正常结束时自己就删了输出。模型回合之后三个账号都有东西可清，完整验收见 4.5。

### 3.6 故障注入：SIGKILL 远端 `mcp-serve`

经 bridge 开 coding 会话，前台跑 `seq 1 200000`（1 288 895 字节 stdout），后台跑 `sleep 300`，然后只对那一个 `mcp-serve` pid 发 SIGKILL：

- 客户端 Broken pipe；**后台命令还活着**（`sleep 300` 仍在，P52 写明的范围外情形：ccnm 自身被 SIGKILL）；
- Operator `ccnm status p62rust` 的写锁行：`unknown: the marker says bridge-c holds it and no process does (interrupted); what it started may still run, new sessions are refused`，并提示 `pid 1096959 is gone -- which does not mean its children are`——说法准确；
- `ccnm cleanup p62rust` 预览：`kept nothing for it that could be cleaned`，而 ccrun 那边留着 1.3 MB 的 `sessions/bridge-…/output/`：bridge 会话按设计不归 `cleanup` 管，手册说它"连接一断就删"，但那只在 `mcp-serve` 正常退出时成立（F11）；
- 照[运维手册"写入 guard 残留"](../operations.md#写入-guard-残留)恢复：按 `output/<ref>/status` 里的命令行找到进程组、整组结束，备份后删掉那**一个** marker，写锁回到 `free`；
- 之后新起的 `mcp-serve` 没有删这 1.3 MB：命令的 `running` 锁已放，按 7 天过期处理。

### 3.7 候选构建上的门禁（两个平台）

| 平台 | 命令 | 结果 |
| --- | --- | --- |
| macOS arm64（本机） | `python3 -B scripts/ci_gates.py` | 通过：计划、协议（43 + 29 个 fixture）、Python 249 条 0 跳过，48.7 秒 |
| macOS arm64 | `cargo fmt --check`、`clippy -D warnings`、`cargo test --workspace` | 通过，1020 条 |
| Debian 13 x86_64（hpsrv，ccrun） | `cargo test --workspace --locked` | 第一次 829/832（清理 3 条，F13）；`8ffe7a5` 之后 **1020/1020** |
| Debian 13 x86_64 | `python3 -B scripts/ci_gates.py` | **没过**：249 条里 2 条失败（F14），1 条跳过（ccrun 本身就是合格的执行身份，"拒绝开发者自己账号"那条用例的前提不成立——CI 的 runner 账号不会这样） |

P58–P61 一直没有推送，所以这四个阶段**没有任何线上 CI 结果**；按上表，推送当前 main 之后 ubuntu job 会因 F14 变红，macOS job 预期通过。

## 4. 真实模型回合

额度：每个 Provider 最多 8 次（一次会话或一次 print 算 1 次）。Claude 用满 8 次，其中 6 次真正跑到模型（合计约 $0.6，Machine API 报的 `cost` 与 CLI 显示一致）；另 2 次在模型之前就结束（未登录 1 次；我的一条诊断命令误起 1 次，见 F16，没有派发）。Codex 用了 4 次，全部在模型之前结束（令牌吊销 3 次、额度用尽 1 次）。

### 4.1 REAL-01 受管 Claude（fodelf → hpsrv）

Operator 在 hpsrv 上 `ccnm run p62rust --detached`，会话在 fodelf 的 tmux 里；我从本机经 ssh 往那个 tmux 发按键，等同于人在终端里操作。

1. 第一次启动先后停在两个 Claude Code 自己的提示：信任 Agent 上 ccnm 的占位目录（默认选中 "No, exit"），和"要不要把 auto mode 设成默认权限模式"——选"是"会改写 fodelf 上日用 Claude 的全局默认，所以选了"否"（F7）。
2. 发任务："2h 解析失败了，先 `sleep 20 && cargo test`，再修，再测，不要 commit"。第一次 `exec_command` 弹 Claude Code 的审批。**审批等待中**：hpsrv 上用真终端 `ccnm attach p62rust` 接上，再从 fodelf 侧 detach，attach 端收到"会话还在 Agent Node 上跑着"并退出 0；`mcp-serve` pid 前后都是 1148145，审批提示还在。
3. 批准后 **命令运行中**：hpsrv 上 `sleep 20` 与 `cargo test` 属于 ccrun（进程组 1148565）；再 attach/detach 一次，命令照跑，`mcp-serve` pid 不变——断开终端不等于断开 MCP。
4. 模型看到 `hours_are_supported` 失败 → 用 `apply_patch` 在 `src/lib.rs` 加 `"h" => 3_600_000`（批准第二次 `cargo test` 前我先在 hpsrv 上以 ccrun `git diff` 看到了这处改动）→ 3 条全过。ccrun 独立再跑：`test_exit=0`，改动只在 `src/lib.rs`，属主 ccrun，没有 commit。
5. `ccnm stop p62rust --session d4ba76a2…` 第一次就 `stopped`、退出 0，写锁 `free`，hpsrv 上没有残留进程；**但 `ccnm log` 把这次跑了 7 分钟的会话写成 `failed to start`、时长 `<1m`**（F4）。

### 4.2 REAL-02 受管 Codex（本机 → hpsrv）：受阻

三次会话：第一次停在 Codex 的信任提示，答完后工具连上（`tools connected`、写锁 held），第一条消息报 `refresh token was revoked`；doctor 此前一直说 `Codex authentication OK`（F5）。用户重新登录（`codex login` 先删旧文件，15:00:30 写入新的）后第三次会话报 `You've hit your usage limit … try again at Oct 4th, 2026 1:21 PM`，并提示可切到 gpt-5.6-luna——那不是受管配置测过的模型，没有切。三次精确停止**都**先报 `CCNM_E_NOT_READY: terminal ended but its Runtime MCP transport is still alive`（3/3），几秒后才真正干净（F4）。同一会话占着写锁时，Machine API `session.start` 回 `-32008 live_holder`、`effect: none`。

### 4.3 REAL-03 外部 Claude Code + Remote Workspace MCP

在 fodelf 上以 `claude -p` 当外部 Host（Claude Code 2.1.285），`--mcp-config` 只配 `ccnm mcp bridge p62rust --mode coding`（`alwaysLoad`）、`--strict-mcp-config`，只允许 `mcp__ccnm`，禁用 Bash/Edit/Write/Read/Glob/Grep/WebFetch/WebSearch/Task，在一个空临时目录里跑：8 个回合、13.5 秒、$0.238，`permission_denials` 为空，修好并跑过测试。hpsrv 上 ccrun 独立核对：3 条全过，改动同 4.1，属主 ccrun，没有 commit；临时目录里只有我放的三个文件——Host 没有绕开 ccnm 改本机。

一处测试手法的瑕疵要如实记：我用 `ssh … bash -s` 喂脚本，`claude -p` 继承了 stdin，把脚本后面几行也读成了提示词的一部分；模型回答里说那几行"看着像误贴的脚本，没有执行"。任务结果不受影响，但这一次的提示词不是纯净的。read 模式的拒绝与同 state 写互斥由 3.2 的零额度轮覆盖，没有再花模型额度。

### 4.4 REAL-05 Machine API + 独立 Python 客户端（Claude）

驱动脚本在 hpsrv 上以 Operator 身份跑，只 import 仓库的 `ccnm_machine_client.py`。任务要求新建一个带随机 token 的文件、跑测试、只报告不修，副作用由 ccrun 独立核对。

| 场景 | 结果 |
| --- | --- |
| 完成任务 | 21.8 秒 `completed`，`exit_code` 0，`usage` 与 `cost`（$0.128）到达调用方；`text` 正确指出 `hours_are_supported` 与原因；文件 `P62-rpc151834.txt` 属 ccrun、10 字节 |
| 分页 | stdout 2 631 字节：64 KiB 一页与 97 字节一页各读一遍，都 `complete`、sha256 前缀一致（`ec0ba9c4…`）、`source_truncated` false；stderr 0 字节；`max_bytes=1` 回 1 字节加游标 |
| 同键重发 | `reused: true`，同一句柄，没有第二次运行 |
| 人类 CLI 对照 | `ccnm run p62rust --print` 同一任务、另一个 token：17.6 秒退出 0，结论一致，文件同样属 ccrun、664、10 字节 |
| 运行中停止 | 等 hpsrv 上出现 ccrun 的 `sleep 150` 再 `session.stop`：**回 `-32000 CCNM_E_NOT_READY: Agent process group has not ended`**，再调一次同样的错；1 秒后状态 `failed`、`exit_code` 143（SIGTERM）、**`stop_requested: false`**（F17）。停止后约 1 秒 hpsrv 上 `sleep 150` 还在，25 秒后已没有、写锁 `released`；两者先后这轮没有精确采样，列为待复现疑点 |
| RPC 进程被杀（派发后 3 秒 SIGKILL） | 新连接看到 `unknown`（记录停在 `running`，已分配 Agent 会话）；同键重发 `reused: true`、不重跑；Agent 上那次运行独立跑完（退出 0、15.1 秒），副作用只出现一次；`ccnm result p62rust --session 91024bb8…` 能取回结果，Machine API 这边永远是 `unknown` |
| 启动后立即断开（误操作中发现） | `session.start` 返回后客户端关掉 stdin，`ccnm rpc` 在派发前退出：记录停在 `starting`、`dispatched: null`，Agent 上没有这次会话，状态读成 `unknown`（F16） |

### 4.5 三个真实账号的清理

模型回合结束后 ccrun 的 state 里留着 5 个受管会话的输出（按设计保留 7 天）。Operator 预览列出 14 项：Operator（uid 1000）4 项、Agent（fodelf uid 501）5 项、Runtime（ccrun uid 1002）5 项，其中两条 `unknown` 的 Machine API 会话"说不清结束没有"而保留。改过一位的令牌 → `CCNM_E_NOT_READY`，一项不删；正确令牌 → 删 12 项、保留 2 项，退出 0。三处目录分别由各自账号删空；ccrun 那边只剩 bridge 残留（F11）。

## 5. 发现

F1–F14 按对用户的影响排序，F15 起按发现顺序追加。F13 在本轮修了；F14、F16、F17 在 [P63](2026-09-30-p63-rpc-disconnect-stop.md) 离线修好、等 P62 续跑时真机复验；后续各阶段的处理写在"影响"一列，F6–F12 与 F18 见 [P66 记录](2026-10-01-p66-low-impact-findings.md)；F19 仍是记录。

| 编号 | 影响 | 现象（真机） | 原因 | 建议 |
| --- | --- | --- | --- | --- |
| F1 | 高；**P65 已修（离线）** | Debian 13 上照手册把项目放进执行账号家目录（`HOME_MODE 0700`），Operator 的 `ccnm run p62rust` 退出 30：`CCNM_E_WRONG_WORKSPACE: workspace root … is not a directory on this machine`；doctor 的 "Runtime workspace" 行 `CCNM_E_INTERNAL: cannot stat`。实际是 Operator 没有权限进入那个目录 | `launcher.rs` 的 `check_local_root` 与 doctor 的 `runtime_workspace` 用 Operator 自己的身份 `stat`，和已知的 `workspace add` 缺陷同源 | 权限不足时不当作"不存在"，交给执行账号回答（它已经有 `Workspace root` 那一行）；在修好之前，手册写明 Linux 上把项目放在执行账号家目录之外（父目录可进入、项目目录归执行账号） |
| F2 | 高；**P64 已修一半（离线）**：新的一端 doctor 与握手能指出来，旧的一端仍要靠发版升号，见 [P64 记录](2026-09-30-p64-stop-outcome-same-number-builds.md) | 候选构建和已装的旧构建都报 `0.9.0`。旧 Agent 对新 Runtime：doctor **0 失败**，会话启动才被内部协议拒绝 | 两端一致性只比版本号 | **发版前必须升版本号**；doctor 比较内部协议版本或构建标识 |
| F3 | 中；**P65 已修（离线）**：`session.result` 多了可选的 `failure` | Machine API：Provider 未登录、两端版本不符、内部协议不符时，调用方拿到的只是 `failed`，`text`、`exit_code`、stdout、stderr 全空；原因只在 Operator 自己的记录 `finish.error` 里 | 协议 v1 没有"Agent 进程没起来"的原因字段 | 按"只做加法"补一个可选字段（例如 `outcome.error` 或 `failure`），schema、fixture、两份客户端同步 |
| F4 | 中；**P64 已修（离线）** | 交互会话精确停止：Codex 3/3 第一次报 `CCNM_E_NOT_READY: terminal ended but its Runtime MCP transport is still alive`，再 stop 说 "nothing to stop"；Claude 1/1 第一次就成功。**两种情况下 `ccnm log` 都把会话写成 `failed to start`、时长 `<1m`**（Claude 那次实际跑了 7 分钟） | `work.rs` 杀掉 tmux 后只查一次通道进程、不等；两条停止路径都用 `record_terminal_failure` 记结局，`log` 把它显示成启动失败 | 杀完后有界地等通道退出（秒级）；停止写成独立结局（"被停止"），`log` 的时长取真实起止 |
| F5 | 中；**P65 只做了前一半（离线）**：doctor 写明只看本地，没加联网探测，原因见 [P65 记录](2026-09-30-p65-hidden-root-failure-reason-codex-login.md) 2.3 | ccnm 专用 Codex profile 的刷新令牌已被吊销，`codex login status` 和 doctor 的 "Codex authentication" 都说 OK；会话里第一条消息才报 `refresh token was revoked` | 只检查本地登录文件 | doctor 写明"只看本地文件"；或在 doctor 里加一次不耗额度的令牌刷新探测（要先确认官方 CLI 有这种命令） |
| F6 | 低；**P66 已修（离线）** | `ccnm status p62rust` 不带 `--agent` 时说 `no live sessions`，而同一 workspace 的 Codex 会话正在跑；不带项目名的 `ccnm status` 能看到 | 单项目 status 只看默认实例 | 列出该 workspace 所有实例的会话，或至少提示"默认实例之外还有会话" |
| F7 | 低；**P66 核对了文档，不预写信任条目**，原因见 [P66 记录](2026-10-01-p66-low-impact-findings.md) | 受管会话第一次启动会停在官方 CLI 自己的提示：Codex 与 Claude 都问"是否信任" Agent 上 ccnm 的占位目录（Claude 默认选中 "No, exit"）；Claude Code 2.1.285 还问要不要把 auto mode 设为默认权限模式——选"是"会改 Agent 账号上日用 Claude 的全局默认。答完之后同一 workspace 不再问 | 官方 CLI 的首启提示 | 使用说明写明每个提示该怎么答；可考虑由 ccnm 预写信任条目 |
| F8 | 低；**P66 已修（离线，只用 `--dry-run` 验过）** | `CCNM_CONFIG` / `XDG_STATE_HOME` 指向非默认位置时 `ccnm controller install` 写出的 plist 不带它们，Controller 在默认目录监听，安装等 10 秒后报 "nothing is listening"，提示"请安装 controller" | plist 只写 `CCNM_LOG` | 安装时把这两个变量写进 plist，或在非默认时拒绝并说明 |
| F9 | 低；**P66 已修** | `ccnm --help` 里只有 `cleanup` 一行是英文 | P61 漏了中文 | 补中文 |
| F10 | 低；**P66 已修（离线）** | Agent 端 `select_agent` 失败（这里是 `~/.claude` 权限）时，Operator 侧 doctor 只说 `Agent probe identity differs from the Runtime selection`；Agent 侧 doctor 的反向 SSH 行说 `agent and project are both on agent` | 拒绝的探测报告不带原因，且 `runtime_ssh = None` 被当成"同机" | 把拒绝原因带回 Operator，同机判断别和"没探"共用一个值 |
| F11 | 低（文档）；**P62 当轮已写进运维手册，P66 核对过** | 外部 MCP 会话的 `mcp-serve` 被 SIGKILL：后台命令继续运行；输出不会"连接一断就删"，`ccnm cleanup` 也不列 bridge 会话，只能等 7 天过期 | 删除靠 `mcp-serve` 正常退出；清理按设计不收 `bridge-*` | 使用说明和运维手册写明 |
| F12 | 低（工具）；**P62 当轮已改（`f25b4ab`），P66 核对过** | `p12_dogfood_check.py` 说明里"同一棵树再配一个 read workspace"会被 `roots overlap` 拒绝 | 规则后加 | 改脚本说明为"各用一份克隆" |
| F13 | 已修 | P61 的 3 条 Agent 侧清理单元测试在 Linux（ccrun uid 1002）上失败 | 假 `id -u` 写死 501 | `8ffe7a5` 改用当前 uid，本机与 hpsrv 都 1020/1020 |
| F14 | 中；**P63 已修（离线）** | Linux 上 Operator 连 Agent 时 ssh 报 `Could not resolve hostname`，Machine API 把会话记成 `unknown`；两条 Python 黑盒用例因此在 Linux 上失败（CI 的 ubuntu job 会同样失败）。macOS 上同一用例通过，是因为临时目录太长、ControlPath 超过 103 字节的配置检查先失败，根本没走到 ssh | P58 把所有 `AgentUnreachable` 都当作"可能已派发" | ssh 在认证完成前就失败（解析不了、拒绝连接、没有路由）时一定没有派发，应记 `failed`；测试别依赖平台的路径长度 |
| F15 | 低（已有文档） | 执行账号 `~/.config/ccnm/` 里放一份配置备份（`config.toml.p62-before-fodelf`），doctor 的 `No SSH keys` 报 `a possible private SSH key is accessible or unknown`，`exec_command` 整个被拒 | 检查不读内容，`~/.config/ccnm` 里除 `*.toml`、`*.pub` 外一律当作可能的私钥（fail-closed） | [排错手册](../troubleshooting.md#exec_command-is-refused理由说有-ssh-私钥可你明明一把都没有)早有这一条；这次是我自己踩到，不需要改 |
| F16 | 高；**P63 已修（离线）** | 冻结协议 8.1 节："客户端断开不等于任务停止，已经接受的 session 继续跑"。真机上 `session.start` 返回后客户端立刻关掉 stdin，`ccnm rpc` 在派发前退出：记录停在 `starting`、`dispatched: null`，Agent 上没有这次会话，状态读成 `unknown` | 派发在 `session.start` 返回之后由后台线程做，真机上派发前还要经 ssh 问一次写锁（P60），有好几秒；进程在 EOF 后不等这个线程 | EOF 后先把已接受的启动派发完（或至少记成"没派发"的 `failed`，而不是 `unknown`）；加一条"start 后立即 EOF"的回归 |
| F17 | 中；**P63 已修（离线）** | Machine API 运行中 `session.stop`：返回 `-32000 CCNM_E_NOT_READY: Agent process group has not ended`（重复调用同样），实际已停（`exit_code` 143），终态 `failed` 且 `stop_requested: false`；契约说 stop 幂等、返回 `stopping` | Agent 侧停止后立刻查进程组、不等（与 F4 同一模式）；RPC 把这个错误原样回给调用方，stop 标志没有落下 | 停止请求先落标志再下发；Agent 侧有界等待；调用方始终拿到 `stopping` 或终态 |
| F18 | 低；**P66 已修** | 人类 CLI `--print` 输出末尾写 `session directory on work: …`，而节点叫 fodelf | 历史叫法残留 | 用节点名 |
| F19 | 待复现 | 运行中停止后约 1 秒，hpsrv 上 ccrun 的 `sleep 150` 仍在；25 秒后已没有、写锁 `released`。"命令收掉"与"写锁放开"谁先谁后这轮没有精确采样 | — | 用 P52 的采样方法（进程表与 guard 同一时刻采）复现，确认没有"命令还在、写锁已放"的窗口 |

另记三件环境事实：fodelf 的时钟在 UTC+8，本机与 hpsrv 在 UTC+9，fodelf 日志里的时间要加一小时；本轮中途本机到 hpsrv 的直连 SSH 有两次 `Operation timed out`，几秒后恢复，同时段走另一个账号的连接正常；Claude Code 在本轮期间被 Homebrew 从 2.1.281 自动升到 2.1.284。

## 6. 授权、资源与收尾

用户当轮批准的范围（不延续到下一轮）：三台机器部署候选构建（与日用版本并存；hpsrv ccrun 替换 0.8.0 并备份）；三把一次性密钥及对应的 `authorized_keys` / `~/.ssh/config` 改动；每个 Provider 最多 8 次真实模型运行；只杀本轮进程的故障注入；收尾撤钥匙、删测试项目与本轮 Controller，保留 hpsrv ccrun 上的候选构建和 0.8.0 备份。没有 push、tag、release。

收尾结果（2026-09-30 15:25 前后，每一项删之前先确认是本轮建的：改过的文件核对"现状 = 备份 + 本轮追加"，目录看创建时间）：

| 位置 | 本轮建的 | 现在 |
| --- | --- | --- |
| 本机 | LaunchAgent（`ccnm controller uninstall` 卸掉，这台机器原来没有 Controller）、`~/.config/ccnm-p62/`、`~/.local/state/ccnm-p62/`、`~/.local/opt/ccnm-p62-dabec34/`、`~/.local/opt/codex-0.154.0/`、`~/.ssh/ccnm-p62-ccrun*` | 已删；`~/.ssh/config` 与 `authorized_keys` 从备份恢复，备份已删 |
| 本机 | `~/.claude` 0755 → 0700 | 已恢复 0755 |
| 本机 | `x86_64-apple-darwin` 编译目标、`target/{x86_64,aarch64}-apple-darwin/`（共 542 MB） | 已移除；`dist/` 里的两个候选包保留（被 `.gitignore` 忽略） |
| 本机 | 日用 `~/.local/bin/ccnm`（`300dbd1d`）与 `~/.config/ccnm/config.toml` | 全程没有改动，前后哈希一致 |
| fodelf | `dev.ccnm.controller.p62` 及其 tmux server、`~/.config/ccnm-p62/`、`~/.local/state/ccnm-p62/`、`~/.local/opt/ccnm-p62-dabec34/`、`~/.ssh/ccnm-p62-ccrun*`、`/tmp/p62-host.*` | 已删；`~/.ssh/config` 与 `known_hosts` 从备份恢复；日用 Controller（pid 1075）与 0.9.0 没动过 |
| hpsrv ccrun | `authorized_keys` 两行 | 已删，文件与本轮前的备份逐字节相同（0 字节） |
| hpsrv ccrun | `~/.config/`（整个是本轮建的）、`~/.local/state/ccnm/`（含 1.3 MB bridge 残留）、`~/p62-build/`、`~/p62-guard-backup/` | 已删 |
| hpsrv ccrun | `~/.local/bin/ccnm` = 候选 `fdd898df…`，`~/.local/opt/ccnm-0.8.0/`、`~/.local/opt/ccnm-p62-dabec34/` | **保留**（用户选择）。回滚：`install -m 755 ~/.local/opt/ccnm-0.8.0/ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm` |
| hpsrv bing | `~/.local/{bin,opt,state}`、`~/.config/`、`~/p62/`、`~/.ssh/ccnm-p62-xdwmbp*`、`~/.ssh/config` 与 `known_hosts`（都是本轮新建的） | 已删；原有的 `~/.local/share`、`authorized_keys` 未动 |
| hpsrv root | `/srv/ccnm-p62/`（三棵测试树）、`/root/ccnm-p62-20260930/`（清单与脚本） | 已删；清单内容保存在本轮会话的临时目录，要点已写进本记录 |

原始证据（Machine API 各场景的 JSON、外部 MCP 零额度轮的 JSON）没有放进仓库：它们含本机和两台机器的家目录路径与会话 id，结论和关键数字已抄进本记录。
