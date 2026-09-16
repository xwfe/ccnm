# P24 Codex 原生链真机会话计划（授权清单）

> **状态（2026-09-16 22:16）：只列了清单，没有做任何变更。**第四节每一条都要用户明确同意才做；同意其中一条不等于同意别的。第三节的现场事实是本机和 hpsrv 上的只读查询得来的。

## 一、这一轮要证明什么

| 验收 | 怎么算通过 |
| --- | --- |
| **P24.1** | 真实 Codex 在 Agent 机器上完成一次"读 → 改 → 跑测试 → 看结果"；改动和测试进程都在 Runtime 上、属主是 `ccrun`；`ccrun` 名下没有 Agent 凭据和出站私钥（沿用 P12.2 的检查）；会话结束后锁是 `released`、`ccrun` 名下没有残留进程 |
| **P24.2** | 原生链会话、受管 MCP 会话、外部 MCP coding 会话抢同一个 workspace 的写锁，任何时刻只有一个拿到，其余启动即报 busy；持有者结束后下一个能拿到 |
| **P24.3** | 第五节的故障矩阵：写锁移交相关的各 20 次、其余各 5 次；未确认退出不放锁，断线不 resume、不重放 |
| **P24.4** | 支持矩阵、使用说明、运维手册写明平台、版本 pin、与 MCP 路径的区别和已知限制；模型回合记进 toexec v2 的额度账本 |

## 二、拓扑：本机当 Agent，hpsrv 当 Runtime

| 角色 | 机器 / 账号 | 为什么是它 |
| --- | --- | --- |
| Agent | 本机 `bing`，**临时 Controller**，独立配置和状态目录 | Codex 0.154.0 和 ccnm 专用的 Codex 登录目录（`~/.config/ccnm/agents/codex`，P3、P7 用过）已经在这里；fodelf 一点不动 |
| Runtime | `hpsrv` / `ccrun`（uid 1002，Debian 13，x86_64） | 账号和工具链是 P12 留下的；exec-server 加 ccnm 监督进程的组合在 Linux 上一次都没接过（P22 只在 macOS 上接过），这一轮顺带补上 |

**没选的两种**：

- **fodelf 当 Agent**：要在你正在用的 Agent 机器上装 Codex、重新登录，还要替换正在跑 Controller 的 ccnm。动的东西最多。
- **本机同机双身份**（P7 那套，Runtime 是本机 `ccrun`）：不是双机，网络故障测不了；还要 sudo 把 `ccrun` 加回 SSH 准入组。

**这一轮不碰的**：本机 `~/.config/ccnm/config.toml`（fodelf 配对用的 Runtime 配置）、本机 `~/.local/bin/ccnm`（0.7.0）、fodelf 上的一切、你那四个真实项目。临时 Controller 读 `CCNM_CONFIG` 指向的单独文件，状态放在 `XDG_STATE_HOME` 指向的单独目录。本机现在没有装 Controller，socket 不会和已有的东西冲突。

## 三、现场事实（2026-09-16，只读）

**本机**

- 装着 ccnm 0.7.0（`~/.local/bin/ccnm`），配置是给 fodelf 配对用的 Runtime（`this = "runtime"`，`runtime_user = "bing"`）。没有 ccnm Controller。
- Codex 0.154.0 在 `/opt/homebrew/bin/codex`（brew cask，属主 `bing`）。
- ccnm 专用 Codex 目录在，权限 0700。**登录是否还有效没有查**，见 A0。
- **我这个 shell 是 `Background` 会话**（`launchctl managername`）。从这里起的 Controller 会被 ccnm 拒绝（"not from a login session"），所以 A7 要你在自己的终端里起。
- 连 hpsrv 的 `hpsrv` 别名是 root 密钥登录。

**hpsrv**（root，只读）

- Debian 13，内核 6.12.101，x86_64，6 核 15 GiB，根分区空闲 423 GiB。
- `ccrun`：uid 1002，只在自己的组里，home 0700，`authorized_keys` **0 行**，名下没有进程。
- **`ccrun` 名下没有 ccnm，也没有 `~/.config/ccnm`**：这一轮是新装，不是替换。
- 工具：`cargo`、`rustc`、`python3`、`git`、`rg`、`node` 都在。
- **没装 bubblewrap**。`kernel.unprivileged_userns_clone = 1`，`user.max_user_namespaces = 62843`，没有 AppArmor 的 user namespace 限制，所以不用改 sysctl。
- sshd：`ClientAliveInterval 0`，`TCPKeepAlive yes`。Tailscale SSH 关着，22 端口是 OpenSSH。`nft` 在，nftables 服务没开。
- 出站：`github.com`、`index.crates.io`、`static.rust-lang.org` 都返回 200。
- P12 的 root 清单 `/var/lib/ccnm-p12-20260911` 还在。

**Codex 发行包**：GitHub API 给出的 `codex-aarch64-unknown-linux-musl.tar.gz` digest 与 P21 自己算的 sha256 一致，说明这个接口的校验值可信。本轮要用的是：

```text
codex-x86_64-unknown-linux-musl.tar.gz
sha256:d7e18b2597ae8f242f5f31ee9e90deef48dbc9edd634d9868fb6435d08c07f02
```

## 四、授权清单

"回退"一列是这一步单独撤销的办法。整轮收尾的默认做法在第七节。

| 编号 | 动作 | 在哪 / 以谁 | 谁来跑 | 影响 | 回退 |
| --- | --- | --- | --- | --- | --- |
| **A0** | 查 ccnm 专用 Codex 目录的登录状态：`CODEX_HOME=~/.config/ccnm/agents/codex codex login status`。只看官方 CLI 自己打印的结论，不读 `auth.json` | 本机 / bing | 我 | 官方 CLI 可能顺手刷新一次 token。登录失效时**你**在自己终端里 `codex login`（同样的 `CODEX_HOME`），我不碰 | 无 |
| **A1** | 本机生成本轮一次性密钥对 `~/.ssh/ccnm-p24-hpsrv`，私钥不离开本机；`~/.ssh/config` 追加别名 `ccnm-p24-hpsrv`（`User ccrun`、`IdentitiesOnly yes`）。hpsrv 上以 root 把公钥追加进 `/home/ccrun/.ssh/authorized_keys`，清单记在新建的 `/var/lib/ccnm-p24-20260916` | 本机 / bing；hpsrv / root | hpsrv 那一半：你 `sudo` 跑脚本，或授权我经 `ssh hpsrv`（root）跑 | `ccrun` 从本机可以登录。只有这一把钥匙 | 按清单删那一行；本机删别名和私钥 |
| **A2** | hpsrv 装 `bubblewrap`（apt）。Codex 发来的 workspace-write 沙箱在 Linux 上靠它，没有它命令一律失败（P21.4） | hpsrv / root | 同 A1 | 多一个系统包 | 按清单 `apt purge` |
| A2′ | A2 的不用 root 的备选：官方同版本发布的 `bwrap-x86_64-unknown-linux-musl.tar.gz` 放在 `ccrun` 名下的 Codex 旁边。**没有实测过**，P21 测的是系统包，所以推荐 A2 | hpsrv / ccrun | 我 | 无系统变更 | 删文件 |
| **A3** | 以 `ccrun` 身份下载 Codex 0.154.0 发行包，sha256 必须等于第三节那个值，解到 `/home/ccrun/.local/opt/codex-0.154.0/` | hpsrv / ccrun | 我（A1 之后） | `ccrun` 名下多一个二进制 | 删目录 |
| **A4** | 以 `ccrun` 身份从 GitHub 拉 ccnm 已推送的 `059d20e`，`cargo build --release --locked`，装到 `/home/ccrun/.local/bin/ccnm`，记 sha256。**不推 tag、不发 release**：推 tag 不可撤销，而这条链还没真机验收 | hpsrv / ccrun | 我 | 依赖从 github.com 和 crates.io 拉；`ccrun` 名下多一份源码和一个二进制 | 删二进制和源码目录 |
| **A5** | 新建 `ccrun` 的 `~/.config/ccnm/config.toml`：Runtime node 写 `runtime_user = "ccrun"` 和 A3 的 `codex_bin`；一个 workspace `p24`，root 是 `/home/ccrun/p24-demo`，agent 是本机的 `codex-main`，`codex_exec_server = true`，`external_mcp = "coding"`（P24.2 要外部入口来抢锁）。**不写** `allow_unconfined_exec` 和 `allow_unisolated_credentials`，`ccrun` 要靠自己过审计。另建测试项目 `/home/ccrun/p24-demo`：几十行 Python 加 unittest，故意留一个会失败的测试，不装任何依赖（沙箱里不能联网） | hpsrv / ccrun | 我 | 无系统变更 | 删这两样 |
| **A6** | 本机从 `059d20e` 构建 release 版 ccnm，放到 `~/.local/state/ccnm-p24/bin/ccnm`，**不替换** `~/.local/bin/ccnm`。新文件 `~/.config/ccnm/p24-agent.toml`：`this = "mbp"`、`runtime_node = "hpsrv"`，Runtime node 的 `ssh` 是 A1 的别名、`ccnm_bin` 是 A4 的绝对路径，`[agents.codex-main]` 的 provider 是 codex、`profile_ref = "default"` | 本机 / bing | 我 | 无系统变更，不影响 fodelf 配对 | 删文件和目录 |
| **A7** | 起临时 Controller：`CCNM_CONFIG=~/.config/ccnm/p24-agent.toml XDG_STATE_HOME=~/.local/state/ccnm-p24 ~/.local/state/ccnm-p24/bin/ccnm internal controller`。不装 LaunchAgent | 本机 / bing | **你**，在 Terminal.app 或 iTerm 里（原因见第三节） | 它存在期间，ccnm 可以在本机 tmux 里起 Codex | 在那个终端里 Ctrl-C |
| **A8** | 真实模型回合，见下面的实验单 | 本机 ChatGPT 订阅（专用 Codex 登录，默认模型） | 见实验单 | 花额度，不可撤销 | 无 |
| **A9** | 网络黑洞：hpsrv 上加一条临时 nftables 规则，丢弃本机到 22 端口的包，**60 秒后由 `systemd-run` 定时自动删**。nftables 服务本来没开，规则只在内存里，重启就没了 | hpsrv / root | 同 A1 | 60 秒内本机连不上 hpsrv 的 22 端口；hpsrv 上别的连接不受影响 | 自动过期；也可手动 `nft delete` |

**A8 实验单**

| 项 | 值 |
| --- | --- |
| `max_runs` | **5**：P24.1 最多 3 次（1 次加 2 次重试）；P24.3 用真 Codex 抽查 2 次（传输断开、exec-server 被杀各 1 次） |
| 一次怎么算 | 在 Codex TUI 里提交一次提示算 1 次，不论它内部发多少请求 |
| 记账 | toexec v2 第 10.1 节的累计上限 145 次，已用 20 次，本轮做完最多到 25 次。授权引用 `user-consent-2026-09-15`，那次说的是 Claude/ChatGPT 订阅，覆盖这里 |
| 截止 | 2026-09-20 |
| 立刻停 | hpsrv 上工作区外出现任何新文件；规则表该拒的请求到了 exec-server；写锁留在 `held` 且说不清原因；Codex 报额度不足 |
| 谁敲提示 | 推荐由我经 tmux 发一段写死在计划里的提示，方便复现，你可以 `ccnm attach` 旁观；也可以你自己敲 |

除这 5 次之外，P24.2 抢锁、P24.3 的循环和 P24.1 的身份检查全部用**不 import ccnm 的中立客户端**跑，走真实 ssh 和真实 `exec-serve`，不花额度。

## 五、P24.3 故障矩阵

| 故障 | 怎么造 | 要 root 吗 | 次数 |
| --- | --- | --- | --- |
| Agent 侧 ssh 被杀 | kill 本机那条 ssh | 否 | 20 |
| Runtime 侧 sshd 会话被杀 | `ccrun` 杀自己的 `sshd-session` | 否 | 20 |
| exec-server 被杀 | `ccrun` kill `codex exec-server` | 否 | 20 |
| 命令留下脱离会话的子进程 | 命令里 `setsid sleep` | 否 | 20 |
| 网络黑洞（代替"Agent 侧断网"） | A9 | 是 | 5；要 20 次就要约 2 小时，你定 |
| Agent 侧冻住 | `SIGSTOP` 本机那条 ssh | 否 | 5 |

不断本机的网：本机断网会同时断掉这个对话和你手上别的事。A9 不授权的话，"断网"这一项只剩 `SIGSTOP`，而冻住进程不等于断网，记录里会写明。

每一轮都检查：锁的状态和预期一致；`ccrun` 名下没有残留进程；断线之后的命令在哪里都没执行。

## 六、预计会撞到、到时单独问的

1. **静默断线时 Runtime 一直占锁。**P22 已经预言过：exec-server 协议没有能发给 Codex 的 ping，而 hpsrv 的 `ClientAliveInterval` 是 0，TCP keepalive 默认 2 小时才探测。黑洞或冻住时，本机那头约 5 分钟后就断了（ssh 的 `ServerAliveInterval=15 × 20`），hpsrv 这头的 `exec-serve` 可能要到 2 小时后才发现，期间锁一直是 `held`。真撞到时有三种修法，都要你定：改 hpsrv 的 sshd 配置（系统变更）、ccnm 加空闲超时（产品改动，另立阶段）、写成已知限制。
2. **登录失效或额度不足。**只能由你去登录或等额度恢复。
3. **bwrap 在 `ccrun` 下起不来。**按第三节的 sysctl 值不应该发生；真发生了再问要不要改系统设置。

## 七、收尾（默认做法，结束时再确认一次）

| 对象 | 默认 |
| --- | --- |
| hpsrv 上 `authorized_keys` 里本轮那一行 | 删 |
| hpsrv 的 bubblewrap | 保留，hpsrv 以后当这条链的 Runtime 还要用；你说删就按清单 purge |
| `ccrun` 名下的 ccnm 和 Codex 二进制 | 保留 |
| `ccrun` 的 `p24` workspace 配置和测试项目 | 删 |
| 本机的密钥、别名、`p24-agent.toml`、`~/.local/state/ccnm-p24` | 删 |
| 本机专用 Codex 目录的登录 | 不动 |

## 八、这一轮不申请的

fodelf 上的任何东西；本机已装的 ccnm、`config.toml`、LaunchAgent 和 sshd；hpsrv 的 sshd 配置、持久防火墙规则和 Tailscale；推 tag 或发 release；给 Claude 开这条链；替你登录任何账号。

## 九、顺序

1. A0，然后 A1–A5，hpsrv 上每一步做完都只读复核一次。
2. A6、A7。
3. **零额度门槛**，不过就不花额度：`ccnm doctor p24`；中立客户端经真实 ssh 跑一遍 P22 规则表的放行和拒绝；P12.2 那套身份检查。
4. A8 的 P24.1，然后 P24.2，然后 P24.3（A9 放在最后）。
5. P24.4 文档，然后按第七节收尾。

任何一步不通过就停下来记录，不往下走。
