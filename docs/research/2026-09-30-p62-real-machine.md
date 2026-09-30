# P62 双 Provider 真实闭环与候选包验收（2026-09-30）

范围见[基线与真机验收方案](../plan/core-verification.md)第 2 节。这一轮是 P57–P61 之后**第一次上真机**：macOS Agent（本机 xdwmbp）→ hpsrv（Debian 13 / x86_64）Linux Runtime，执行账号 `ccrun`。用户当轮逐项批准了：两端部署候选构建、两把一次性 SSH 密钥、每个 Provider 最多 8 次真实模型运行、只杀本轮进程的故障注入、跑完撤钥匙但保留 hpsrv 上的候选构建。**没有 push、没有打 tag、没有发版。**

## 1. 结论

<!-- P62-STATUS -->**阶段未完成，受阻于两个 Provider 的登录。** 部署、零额度真机和两个平台的门禁都做完了，候选构建在 hpsrv 上以真实的跨 UID 身份跑通了外部 MCP 全流程、Machine API 的失败分支、回退与升级、P61 的跨账号清理和 SIGKILL 故障恢复；同时查出 14 条问题（第 5 节），其中两条高影响：Debian 上照手册放项目会被 Operator 拒绝（F1），候选构建和旧构建版本号相同、doctor 分辨不出（F2）。需要模型的 REAL-01…05 那几步，一次真实模型回合都还没跑成：本机 Claude CLI 没登录，ccnm 专用 Codex profile 的刷新令牌已被吊销（两次尝试都在模型回合之前失败，不耗额度）。

| 判据 | 状态 | 依据 |
| --- | --- | --- |
| P62.1 授权、身份、版本与候选构建 | 完成 | 2.1、2.2；授权逐项记录在第 6 节 |
| P62.2 两 Provider 受管闭环 | **受阻**：登录 | 受管 Codex 会话起得来、工具连得上、能精确停止（F4），但模型第一条消息就报令牌吊销；受管 Claude 未登录 |
| P62.3 外部 MCP 与 Machine API 真实组合 | 部分：零额度部分完成 | 3.2、3.3；两个 Host 的模型回合与 Machine API 的完成/分页/停止/断连未跑 |
| P62.4 失败、写权、断线、清理 | 部分 | 3.3–3.6 做完；需要模型运行中的停止与 RPC 断连未跑 |
| P62.5 候选包、安装、升级回退 | 部分 | 3.4、3.7；Linux Python 门禁因 F14 未过，线上 CI 未跑（未推送） |
| P62.6 文档、状态、资源 | 进行中 | 第 6 节 |

## 2. 环境与部署

### 2.1 三个身份

| 身份 | 机器 / 账号 | 装了什么 | 配置与 state |
| --- | --- | --- | --- |
| Agent | xdwmbp（macOS 26.6.2 arm64）/ `bing` uid 501 | 候选构建在 `~/.local/opt/ccnm-p62-dabec34/`，**没有替换**日用的 `~/.local/bin/ccnm`（0.9.0，`300dbd1d`，是 fodelf 日用配对的 Runtime）；Controller 是 `dev.ccnm.controller` LaunchAgent；Claude Code 2.1.284（`/opt/homebrew/bin`，本轮中途被自动升级过，开始时是 2.1.281）；Codex 0.154.0（官方 release，放在 `~/.local/opt/codex-0.154.0/`，不在用户 PATH 上） | `~/.config/ccnm-p62/config.toml`（`this = "agent"`、`runtime_node = "hpsrv"`、`claude-main` / `codex-main` 两个实例）；state 在 `~/.local/state/ccnm-p62/` |
| Operator | hpsrv / `bing` uid 1000（在 sudo 组，是用户本人） | 候选构建 `~/.local/bin/ccnm` | `~/.config/ccnm/config.toml`（Runtime 配置，和 ccrun 那份逐字相同） |
| Runtime 执行账号 | hpsrv / `ccrun` uid 1002（只在自己的组） | 候选构建 `~/.local/bin/ccnm`，0.8.0 备份在 `~/.local/opt/ccnm-0.8.0/` | 同上一份配置；`mcp-serve` 从它解析 workspace 与 root |

SSH：Agent → `ccrun@hpsrv` 用本机新生成的 `~/.ssh/ccnm-p62-ccrun`，ccrun 的 `authorized_keys`（原本 0 行）加一行并限定 `from="100.107.211.119"`；Operator → Agent 用 hpsrv 上 bing 新生成的 `~/.ssh/ccnm-p62-xdwmbp`，本机 `authorized_keys` 加一行限定 `from="100.116.207.8"`。两端 `~/.ssh/config` 各加一段带 `ccnm-p62` 标记的 alias，改前都有备份。本机主机指纹从 hpsrv 侧 keyscan 后与本机 `/etc/ssh/ssh_host_ed25519_key.pub` 核对一致（`SHA256:nICfPd4s…`）。ccrun 仍然一把私钥都没有。

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
- **`~/.claude` 从 0755 收紧到 0700**：ccnm 要求 Claude profile 目录私有（P7 就撞过）。同账号使用不受影响；收尾时恢复原权限。

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

这一轮执行账号那边没有可清的东西：受管和 bridge 会话正常结束时自己就删了输出。

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

<!-- P62-MODEL -->**还没跑成。** 额度：每个 Provider 最多 8 次（一次会话或一次 print 算 1 次），已用 Codex 3 次、Claude 1 次，全部在模型回合之前因登录失败，没有产生模型用量。计划的分配：受管交互闭环 1、Machine API 完成任务并分页 1、运行中精确停止 1、人类 CLI `--print` 对照 1、外部 Host coding 1、外部 Host read 1、RPC 进程被杀后的 unknown 1。驱动脚本在 hpsrv `~bing/p62/p62_rpc.py`（只 import 独立客户端），任务提示词固定写在脚本里；交互会话在 Agent 的 tmux（socket `ccnm`，会话 `ccnm-p62rust`）里驱动，审批与 detach/reattach 在那里做。

## 5. 发现

按对用户的影响排序。"已修"只有 F13 一条；其余都是记录，修复另立阶段。

| 编号 | 影响 | 现象（真机） | 原因 | 建议 |
| --- | --- | --- | --- | --- |
| F1 | 高 | Debian 13 上照手册把项目放进执行账号家目录（`HOME_MODE 0700`），Operator 的 `ccnm run p62rust` 退出 30：`CCNM_E_WRONG_WORKSPACE: workspace root … is not a directory on this machine`；doctor 的 "Runtime workspace" 行 `CCNM_E_INTERNAL: cannot stat`。实际是 Operator 没有权限进入那个目录 | `launcher.rs` 的 `check_local_root` 与 doctor 的 `runtime_workspace` 用 Operator 自己的身份 `stat`，和已知的 `workspace add` 缺陷同源 | 权限不足时不当作"不存在"，交给执行账号回答（它已经有 `Workspace root` 那一行）；在修好之前，手册写明 Linux 上把项目放在执行账号家目录之外（父目录可进入、项目目录归执行账号） |
| F2 | 高 | 候选构建和已装的旧构建都报 `0.9.0`。旧 Agent 对新 Runtime：doctor **0 失败**，会话启动才被内部协议拒绝 | 两端一致性只比版本号 | **发版前必须升版本号**；doctor 比较内部协议版本或构建标识 |
| F3 | 中 | Machine API：Provider 未登录、两端版本不符、内部协议不符时，调用方拿到的只是 `failed`，`text`、`exit_code`、stdout、stderr 全空；原因只在 Operator 自己的记录 `finish.error` 里 | 协议 v1 没有"Agent 进程没起来"的原因字段 | 按"只做加法"补一个可选字段（例如 `outcome.error` 或 `failure`），schema、fixture、两份客户端同步 |
| F4 | 中 | 交互会话 `ccnm stop --session` 第一次必报 `CCNM_E_NOT_READY: terminal ended but its Runtime MCP transport is still alive`（2/2）；再 stop 一次说 "nothing to stop"，`ccnm log` 把这次正常启动、被人停掉的会话显示成 **failed to start** | `work.rs` 杀掉 tmux 后只查一次通道进程、不等；第二次走 `already_stopped`，写下 "no managed terminal was running" 当作终端失败 | 杀完后有界地等通道退出（秒级）；`already_stopped` 见到 `stopping` 标记时记"被停止"而不是"没启动" |
| F5 | 中 | ccnm 专用 Codex profile 的刷新令牌已被吊销，`codex login status` 和 doctor 的 "Codex authentication" 都说 OK；会话里第一条消息才报 `refresh token was revoked` | 只检查本地登录文件 | doctor 写明"只看本地文件"；或在 doctor 里加一次不耗额度的令牌刷新探测（要先确认官方 CLI 有这种命令） |
| F6 | 低 | `ccnm status p62rust` 不带 `--agent` 时说 `no live sessions`，而同一 workspace 的 Codex 会话正在跑；不带项目名的 `ccnm status` 能看到 | 单项目 status 只看默认实例 | 列出该 workspace 所有实例的会话，或至少提示"默认实例之外还有会话" |
| F7 | 低 | 受管 Codex 交互会话第一次启动，停在 Codex 的"是否信任这个目录"提示，目录是 Agent 上 ccnm 的占位目录；选"信任"后记住，下次不再问 | Codex 对新 cwd 的首启提示 | 使用说明补一句；或者像原生链那样由 ccnm 写入信任条目 |
| F8 | 低 | `CCNM_CONFIG` / `XDG_STATE_HOME` 指向非默认位置时 `ccnm controller install` 写出的 plist 不带它们，Controller 在默认目录监听，安装等 10 秒后报 "nothing is listening"，提示"请安装 controller" | plist 只写 `CCNM_LOG` | 安装时把这两个变量写进 plist，或在非默认时拒绝并说明 |
| F9 | 低 | `ccnm --help` 里只有 `cleanup` 一行是英文 | P61 漏了中文 | 补中文 |
| F10 | 低 | Agent 端 `select_agent` 失败（这里是 `~/.claude` 权限）时，Operator 侧 doctor 只说 `Agent probe identity differs from the Runtime selection`；Agent 侧 doctor 的反向 SSH 行说 `agent and project are both on agent` | 拒绝的探测报告不带原因，且 `runtime_ssh = None` 被当成"同机" | 把拒绝原因带回 Operator，同机判断别和"没探"共用一个值 |
| F11 | 低（文档） | 外部 MCP 会话的 `mcp-serve` 被 SIGKILL：后台命令继续运行；输出不会"连接一断就删"，`ccnm cleanup` 也不列 bridge 会话，只能等 7 天过期 | 删除靠 `mcp-serve` 正常退出；清理按设计不收 `bridge-*` | 使用说明和运维手册写明 |
| F12 | 低（工具） | `p12_dogfood_check.py` 说明里"同一棵树再配一个 read workspace"会被 `roots overlap` 拒绝 | 规则后加 | 改脚本说明为"各用一份克隆" |
| F13 | 已修 | P61 的 3 条 Agent 侧清理单元测试在 Linux（ccrun uid 1002）上失败 | 假 `id -u` 写死 501 | `8ffe7a5` 改用当前 uid，本机与 hpsrv 都 1020/1020 |
| F14 | 中 | Linux 上 Operator 连 Agent 时 ssh 报 `Could not resolve hostname`，Machine API 把会话记成 `unknown`；两条 Python 黑盒用例因此在 Linux 上失败（CI 的 ubuntu job 会同样失败）。macOS 上同一用例通过，是因为临时目录太长、ControlPath 超过 103 字节的配置检查先失败，根本没走到 ssh | P58 把所有 `AgentUnreachable` 都当作"可能已派发" | ssh 在认证完成前就失败（解析不了、拒绝连接、没有路由）时一定没有派发，应记 `failed`；测试别依赖平台的路径长度 |

另记两件环境事实：本轮中途本机到 hpsrv 的直连 SSH 有两次 `Operation timed out`，几秒后恢复，同时段走另一个账号的连接正常；Claude Code 在本轮期间被 Homebrew 从 2.1.281 自动升到 2.1.284。

## 6. 授权、资源与收尾

用户当轮批准的范围（不延续到下一轮）：部署候选构建到本机（并存，不替换日用 0.9.0）和 hpsrv（ccrun 替换 0.8.0 并备份；bing 新装）；两把一次性密钥及对应的 `authorized_keys` / `~/.ssh/config` 改动；每个 Provider 最多 8 次真实模型运行；只杀本轮进程的故障注入；收尾撤钥匙、删测试项目与本轮 Controller，保留 hpsrv ccrun 上的候选构建和 0.8.0 备份。没有 push、tag、release。

本轮建的东西（hpsrv 上的清单在 `/root/ccnm-p62-20260930/manifest.txt`）：

| 位置 | 东西 | 收尾 |
| --- | --- | --- |
| 本机 | `~/.ssh/ccnm-p62-ccrun`、`~/.ssh/config` 的 `ccnm-p62` 段、`authorized_keys` 里 `ccnm-p62-operator-hpsrv-to-xdwmbp` 那一行（均有 `*.pre-ccnm-p62-20260930` 备份） | 删 |
| 本机 | LaunchAgent `dev.ccnm.controller`（指向候选构建，plist 加了三个变量）、`~/.config/ccnm-p62/`、`~/.local/state/ccnm-p62/`、`~/.local/opt/ccnm-p62-dabec34/`、`~/.local/opt/codex-0.154.0/` | `ccnm controller uninstall` 后删 |
| 本机 | `~/.claude` 权限 0755 → 0700 | 恢复 0755 |
| 本机 | `x86_64-apple-darwin` 编译目标、`dist/` | `rustup target remove`；`dist/` 已被 `.gitignore` 忽略 |
| hpsrv ccrun | `authorized_keys` 一行（原 0 行） | 删 |
| hpsrv ccrun | `~/.local/bin/ccnm` = 候选 `fdd898df…`，`~/.local/opt/ccnm-0.8.0/`、`~/.local/opt/ccnm-p62-dabec34/` | **保留**（用户选择） |
| hpsrv ccrun | `~/.config/ccnm/config.toml`、`~/p62-build/`、`~/p62-guard-backup/`、`~/.local/state/ccnm/`（含 1.3 MB bridge 残留） | 删 |
| hpsrv bing | `~/.local/bin/ccnm`、`~/.local/opt/ccnm-p62-dabec34/`、`~/.config/ccnm/`、`~/p62/`、`~/.ssh/ccnm-p62-xdwmbp*`、`~/.ssh/config` 的 `ccnm-p62` 段、`~/.ssh/known_hosts`（本轮新建） | 删 |
| hpsrv root | `/srv/ccnm-p62/`（三棵测试树） | 删 |
