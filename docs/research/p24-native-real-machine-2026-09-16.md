# Codex 原生链真机验收（P24，2026-09-16）

计划和授权清单见 [P24 会话计划](../plan/p24-native-real-machine-session.md)，设计见[双执行入口方案](../plan/runtime-surfaces.md)第 12 节。脚本、每一轮的原始结果和复跑方法在 toexec 仓库的 [`evidence/v2-c/p24-real/`](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/p24-real/README.md)。

**一句话结论**：Codex 原生链在"macOS Agent + Linux Runtime"上真机可用。读、改、跑测试的改动和进程都在 Runtime 上、属主是专用执行身份；三个入口抢同一把写锁；七类故障（五类各 20 次，冻住和网络黑洞各 5 次）都没有越权副作用、没有重放、没有残留进程，未确认退出不放锁。**要知道的限制**：Agent 静默离网时 Runtime 察觉不到，锁会一直占着，要人工结束那条孤儿连接（P26 之后 Runtime 会探活、10 分钟无响应自己放锁，见 [P26 记录](p26-native-liveness-2026-09-17.md)；本文记的是 P24 构建的行为）。模型额度用了 3 次。

## 一、环境

| 角色 | 机器 / 账号 | 版本 |
| --- | --- | --- |
| Agent | 本机 macOS 26.6.2 arm64，`bing`，临时 Controller（Aqua），单独的 `CCNM_CONFIG` 和 `XDG_STATE_HOME` | ccnm `7ae2d4b` release 构建（sha256 `4fc16ce6…`）；Codex 0.154.0（Homebrew），ccnm 专用 Codex 登录（ChatGPT） |
| Runtime | `hpsrv`，Debian 13 / 内核 6.12.101 / x86_64，`ccrun`（uid 1002，只在自己的组） | ccnm `7ae2d4b` 离线构建（sha256 `96c1f840…`）；Codex 0.154.0 官方 musl 发行包（包 sha256 `d7e18b25…`，二进制 `3188814c…`）；bubblewrap 0.12.0 |

Runtime 配置：workspace `p24`，`codex_exec_server = true`，`external_mcp = "coding"`，**没有任何豁免开关**（`allow_unconfined_exec`、`allow_unisolated_credentials` 都没写）。测试项目是几十行 Python 加 unittest，偶数长度求中位数的分支故意写错。

## 二、授权清单是怎么执行的，和计划不一样的地方

A0–A9 用户全部同意，root 步骤由我经本机的 root 密钥执行。与计划不同的有四处，原因都写在这里：

| 项 | 计划 | 实际 | 原因 |
| --- | --- | --- | --- |
| A3 | hpsrv 上以 ccrun 直接下载 Codex | 本机下载，两端各核一次 sha256，scp 给 ccrun | hpsrv 直连 GitHub 发行包实测约 12 KB/s，99 MB 要两个多小时；本机 3.6 秒，scp 6.5 秒 |
| A4 | hpsrv 上从 GitHub 拉 `059d20e` 构建 | 本机 `git archive` 已推送的 `7ae2d4b`（与 `059d20e` 只差文档）+ `cargo vendor --locked`，scp 后在 hpsrv 离线构建（72 秒） | hpsrv 到 crates.io 约 68 KB/s；提交哈希和 Cargo.lock 校验和保证内容一致，不经过第三方代理 |
| A7 | 用户在 Terminal.app 里起临时 Controller | 我用一个临时 plist（标签 `dev.ccnm.p24.controller`，不放进 `~/Library/LaunchAgents`）`launchctl bootstrap gui/501`，Controller 自报 `Aqua` | 我的 shell 是 Background 会话，但 `launchctl bootstrap gui/<uid>` 不受这个限制（P15 在 fodelf 上经 ssh 装 Controller 用的就是它）；效果与计划相同，收尾时 `bootout` |
| A9 | 丢弃本机到 22 端口的包 | 只丢弃**这一条** TCP 连接两个方向的包；删除用的 systemd 定时器**先于**规则建立 | 本机的 root 控制连接不受影响；规则加到一半出错也一定会被撤掉 |

另加了一类计划矩阵里没有的故障：**监督进程本身被杀**（20 次），因为 toexec v2 的 V2-G08 点名要它，而 P22 只在假执行端上做过。

## 三、零额度门槛（39 项全部通过）

中立客户端（不 import ccnm 和 Codex，请求取自 P21 录下的 Codex 真实请求）经**真实 ssh** 连 `ccnm internal exec-serve`，副作用在 hpsrv 的磁盘上核对，不看回包：

- 沙箱内命令以 ccrun 在工作区里执行，写入落盘；往工作区外写报 `Read-only file system`（bubblewrap 生效）。
- 提权命令（`sandbox: null`）、批准后的越界写、`http/request`、工作区外读，全部 `-32600`，磁盘上没有文件；根以上的 `.git` 查询由 ccnm 回 `-32004`。
- P12.2 那套身份检查，**以账号本身和在执行端沙箱里各做一遍**：只在自己的组、没有 sudo、docker socket 不可写、`~/.ssh` 里没有私钥候选、没有 `SSH_AUTH_SOCK`、读不到别人的 home、没有凭据形状的环境变量；沙箱里的 `CODEX_HOME` 是 ccnm 生成的目录，里面只有 `tmp`。
- 会话期间锁由这个会话持有、`exec-serve` 和 `codex exec-server` 属主都是 ccrun；关闭后锁 `released`、没有带会话标记的进程、生成的 `CODEX_HOME` 已删。
- `ccnm doctor p24`（Agent 侧）0 项失败；两项"没查"是设计如此。

握手返回 `executorVersion: "0.0.0"`，与 P21 在容器里看到的一致。

## 四、P24.1 真实任务（模型第 1 次）

提示：跑测试、修 `calc/stats.py` 里的 bug、不许改测试、再跑一遍报告结果。

- **两边的进程树**：本机 tmux → `supervise` → `codex --sandbox workspace-write … -C /home/ccrun/p24-demo` → `ssh … internal exec-serve`；hpsrv 上 `sshd-session: ccrun@notty` → `ccnm internal exec-serve` → `codex exec-server --listen stdio`。
- Codex 跑测试看到 1 个失败 → 读文件 → `apply_patch` 把偶数分支改成取中间两个数的平均 → 重跑 3 个全过 → `git diff` 确认只改一个文件。81 秒，会话退出码 0。
- **在 hpsrv 上独立核对**：改动属主 `ccrun:ccrun`；以 ccrun 另开 ssh 跑测试 3 个全过；工作区外目录为空；会话开始后 ccrun 在工作区以外新写的只有 ccnm 自己的锁标记和状态目录；结束后锁 `released`，没有 `exec-serve`、`exec-server` 或 python 残留，生成的 `CODEX_HOME` 已删；本机没有残留的传输进程。

**顺带看到的一件事**：Codex 启动时把 Agent 本机 `~/.agents/skills` 下的个人 skill 列进了提示，模型据此想 `cat /Users/bing/.agents/skills/…/SKILL.md`——**这条命令在 hpsrv 上执行，报 No such file**。Agent 本机没有任何文件被工具读到，也没有退回本机执行；暴露给模型的只是本机 skill 的名字和路径。

## 五、P24.2 跨入口抢锁（74 项全部通过，零额度）

- 中立矩阵 3 轮：原生链（`exec-serve`）、受管 MCP（`mcp-serve` protocol 4）、外部 MCP coding（`ccnm mcp bridge p24 --mode coding`）轮流持锁；持锁时另外两个入口**和同一入口的第二个会话**都在启动时被拒，第一行是 `workspace write guard is busy`；持锁方关闭后锁 `released`，下一个能拿到。
- **真实 Codex 当持锁方**：不发提示的空闲会话一起来，它的 exec-server 连接就占住了锁；三个中立入口都被拒；`/exit` 后释放。
- **真实 Codex 当抢锁方**：中立 `exec-serve` 持锁时 `ccnm run p24` 失败，**没有创建会话记录，也没有 tmux 会话**。

**发现（不是原生链引入的）**：这种情况下 `ccnm run` 报的是 `CCNM_E_RUNTIME_UNREACHABLE`（退出码 21），正文里才带着远端的 `workspace write guard is busy`。人读得懂，程序按错误码会误判成"连不上 Runtime"。根因是 Agent 侧 MCP 预检把一切握手失败都归成 Runtime 不可达，P7 以来就是这样。另立阶段修，没有夹带进 P24。

## 六、P24.3 故障注入

### 中立客户端，真实 ssh

| 故障 | 次数 | 结果 |
| --- | --- | --- |
| 本机 ssh 被 SIGKILL | 20 | 20/20：第一次查询（经 ssh 约 0.3 秒）时锁已 `released`，没有带标记的进程 |
| hpsrv 上 ccrun 的 `sshd-session` 被 SIGKILL | 20 | 20/20，同上 |
| `codex exec-server` 被 SIGKILL | 20 | 20/20，同上 |
| 命令里用 `setsid` 留一个脱离会话的子进程，然后客户端断开 | 20 | 20/20，同上 |
| **`ccnm internal exec-serve` 本身被 SIGKILL** | 20 | 20/20：锁**保持** `held`；exec-server 和它的进程在 1.1 秒内消失；下一个会话被拒，提示 `left held by an interrupted process`；照运维手册确认旧进程都不在、删掉那一个标记后，能重新打开 |
| 本机 ssh 被 SIGSTOP 60 秒再 SIGCONT | 5 | 5/5：冻住期间锁一直由同一会话持有、没有移交；恢复后同一会话还能跑命令；关闭后释放 |
| 网络黑洞（A9） | 5 | 见下 |

**Linux 上 ccnm 的进程扫描一次都没有杀到东西（110 轮中立故障里 0 次）。**原因看得见：Codex 在 Linux 上用 `bwrap --unshare-pid --die-with-parent --new-session --as-pid-1` 包住每条命令，`setsid` 脱离不了这个 PID 命名空间，命令一返回或 exec-server 一死，里面的进程就一起结束。所以 Linux 上扫描是第二道防线；它真正起作用的情形在 macOS 上（P22 实测 setsid 逃逸）和 Linux CI 的假执行端上测过。

### 网络黑洞：Agent 静默离网，Runtime 一直占锁

做法：会话里跑着 `sleep 300`，hpsrv 上以 root 加一条只丢**这一条** TCP 连接两个方向的 nftables 规则（60 秒后由 systemd 定时器删除）；规则生效 20 秒后在本机 SIGKILL ssh——它发出的断开信号全被丢掉，等于 Agent 那台机器突然从网络上消失。规则到期后再观察 3 分钟。

| 轮 | 定时器按时删规则 | 锁自己释放了吗 | 从掐断到观察结束 | 人工恢复后 |
| --- | --- | --- | --- | --- |
| 1–5 | 5/5 | **0/5，一直 `held`** | 222.9–223.4 秒 | 5/5 `released`，无残留 |

**结论**：Agent 静默离网时，Runtime 这头察觉不到，锁由已经不存在的会话一直持有。原因是三件事叠在一起：exec-server 协议没有能发给 Codex 的 ping（P22 已记）；hpsrv 的 sshd `ClientAliveInterval 0`；那条连接上没有数据要发，内核的 TCP keepalive 默认 2 小时才探测。**这不违反验收**——未确认退出本来就不该放锁，而且不会有东西在断线后被执行——但它是运维上必须知道的限制。

**恢复**（每轮都这样做，都成功）：在 Runtime 上找到那个会话的 `ccnm internal exec-serve`，给它的父进程（`ccrun` 的 `sshd-session`）发 TERM；`exec-serve` 读到 EOF，按正常路径关掉 exec-server、扫进程、放锁，**不需要删锁标记**。步骤写进了[运维手册](../operations.md#agent-静默离网之后exec-server-链的锁一直-held)。

**推断，没有实测**：命令在这段时间里有输出或者结束的话，hpsrv 要往这条连接写数据，对端内核会回 RST，应该会更早察觉；本轮的 `sleep 300` 在观察期内没有任何输出。要从根上缩短这段时间，可以在 Runtime 的 sshd 上开 `ClientAliveInterval`（系统配置变更），或者让 ccnm 自己加空闲超时（产品改动）——都没做，留给用户决定。

### 真实 Codex 抽查（模型第 2、3 次）

Codex 正在 hpsrv 上跑 `sleep 45; echo <标记>` 时注入故障：

| 故障 | 结果 |
| --- | --- |
| 掐断本机那条传输 ssh | hpsrv 0.3 秒内放锁，`sleep` 已被清掉；Codex 报 `exec-server transport disconnected`，**没有再起传输、没有重跑命令**，标记从没出现在任何工具输出里；会话退出码 0 |
| 杀掉 hpsrv 上的 `codex exec-server` | 0.4 秒内放锁，`sleep` 已被清掉；Codex 同样只报断开、不重连、不重放；会话退出码 0 |

两次任务结束后 Codex 都弹了"接近额度上限，要不要切到更便宜的模型"，都选了保持当前模型。它还提示本周额度剩不到 5%。

## 七、额度

| 用途 | 次数 |
| --- | --- |
| P24.1 任务 | 1 |
| P24.3 真实 Codex 抽查 | 2 |
| **合计** | **3**（实验单上限 5）；toexec v2 第 10.1 节累计从 20 次到 23 次 |

## 八、清理

按[会话计划](../plan/p24-native-real-machine-session.md)第七节的默认做法，逐项核对过：

| 位置 | 结果 |
| --- | --- |
| hpsrv `ccrun` 的 `authorized_keys` | 本轮那一行已删，0 行；一次性密钥再登录报 `Permission denied (publickey)` |
| hpsrv bubblewrap | **保留**，root 清单 `/var/lib/ccnm-p24-20260916` 记着它是本轮装的（`hpsrv-runtime.sh --revert` 会按清单 purge） |
| hpsrv `ccrun` 名下 | 保留 `~/.local/bin/ccnm` 和 `~/.local/opt/codex-0.154.0/`；删掉测试项目、源码、P24 配置、ccnm 状态目录、A3 手动跑 `codex --version` 留下的 `~/.codex`；`ccrun` 名下无进程 |
| hpsrv `/tmp` | 删掉 Codex 沙箱留下的 `/tmp/.git`、`/tmp/.agents`、`/tmp/.codex`、`/tmp/codex-bwrap-synthetic-mount-targets-1002/`（见下）；没有遗留的 nftables 表和定时器 |
| 本机 | 临时 Controller 已 `bootout`；删掉 `~/.local/state/ccnm-p24/`（含各会话的 `codex-home`，里面的 `auth.json` 只是链接，profile 的登录文件原样在）、`~/.config/ccnm/p24-agent.toml`、一次性密钥；`~/.ssh/config` 逐字节恢复到 P24 之前 |
| 未动过 | 本机 `~/.config/ccnm/config.toml`（最后修改 9 月 13 日）、`~/.local/bin/ccnm`（0.7.0）、fodelf 上的一切 |

**清理时发现的 Codex 行为**：Codex 0.154.0 的 Linux 沙箱为了保护可写 `/tmp` 下的 `.git`、`.agents`、`.codex` 这几个名字，会在**真实的 `/tmp`** 里建这三个空目录，另建 `/tmp/codex-bwrap-synthetic-mount-targets-<uid>/`，属主是执行账号，用完不删。`/tmp` 是 tmpfs，重启就没了。同一台机器上别的账号也跑 Codex 沙箱时会不会被这几个别人名下的目录挡住，没有测。

## 九、没测到的、要记住的

- **只测了一种平台组合**：macOS Agent + Debian 13 x86_64 Runtime。macOS 当 Runtime 的原生链在 P22 用本机真 exec-server 测过，但没有做这一轮的双机故障矩阵。
- **Agent 侧 Codex 只用了默认模型**（Code Mode）。
- **V2-G09（资源上限：200 MiB 连续输出、磁盘写失败等）对原生链没做。**
- **长时间断网**：黑洞规则按授权只存在 60 秒，所以测到的是"Agent 在 60 秒的断网窗口里消失、之后网络恢复"；观察只到掐断后约 223 秒，锁在这段时间里一直没放。
- `ccnm doctor` 对 `codex_exec_server` 的 workspace 只探 MCP 那条链，不探 `exec-serve`；原生链的问题要到 `ccnm run` 的预检才看得到。
- 装在 fodelf 和本机 `~/.local/bin` 的 ccnm 仍是 0.7.0，这条链只在本轮的临时构建上跑过。
