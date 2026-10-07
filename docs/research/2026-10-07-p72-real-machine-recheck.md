# P72 真机复验：Approve for me 只管当前会话（2026-10-07）

接 [P72 记录](2026-10-07-p72-approve-for-me-one-session.md)第 5 节：F27 的修法之前只有离线测试与零额度实测。用户授权"用真实模型在真机上复验 F27 的修复"。两端都是 [v0.11.2 发布版](2026-10-07-release-0.11.2.md)（含 P72）。

## 1. 结论

**修法在真机上成立。** 同一份 Codex profile 里一直写着 `approvals_reviewer = "auto_review"`（会话里切 Approve for me 时 Codex 写进去的），之后新起的受管会话照样每次 `exec_command` 前问人；会话里再切 Approve for me，只管那一个会话。

| 步骤 | 会话 | 结果 |
| --- | --- | --- |
| 1 | A（不发消息） | 启动参数带 `approvals_reviewer="user"`；`/status` 是 `Read Only (Ask for approval)`；`/permissions` → Approve for me 后，profile 第一行多了 `approvals_reviewer = "auto_review"`——F27 的前提照样出现 |
| 2 | B（真实模型） | profile 仍是 `auto_review`，`/status` 却是 `Ask for approval`；模型的两次 `exec_command` **都弹出问人的提示**：放行的那次由 ccrun 执行，取消的那次没执行 |
| 3 | B（同一会话） | 会话里切 Approve for me 后，第三次 `exec_command` 没问人、执行了——会话里的人仍能临时切走 |
| 4 | C（不发消息） | profile 仍是 `auto_review`，`/status` 又是 `Ask for approval`——切走没有延续 |

在 v0.11.0 上同样的顺序，步骤 2 和 4 会是 `Approve for me`、一次都不问（[P71 真机复验](2026-10-07-p71-real-machine-recheck.md)第 5 节）。

真实模型：Codex `codex-main`，上限 2 次，用了 1 次（会话 B，三轮消息）。周额度显示 93% 剩余，前后没变。

## 2. 拓扑与授权

和 [P71 真机复验](2026-10-07-p71-real-machine-recheck.md)第 2 节相同，只换了名字：本机当 Codex 的 Agent（日用的 v0.11.2 二进制，配置与 state 另放在 `ccnm-p72` 下），Operator 就在 Agent 上，Runtime 是 hpsrv 的 `ccrun`（v0.11.2），测试仓库 `~/p72/codex` 只有 README。临时 Controller 用标准 Label、plist 手工加 `PATH` 指向按 digest 核过的 Codex 0.154.0；一次性密钥只连 ccrun（`from="100.107.211.119"`、禁转发）；本机 `~/.ssh/config` 与 Codex profile 的 `config.toml` 动手前备份。

`ccnm doctor p72codex`（本机）：`可以用了（3 项不查……）`、退出 0；`命令审批` 一行是 P72 的说法。

## 3. 经过

**会话 A**（`bbc62878…`）：第一次启动停在信任提示，选 Yes。`session.json` 的 `ask_before` 是 `["exec_command"]`；Codex 进程参数里有 `approval_policy="on-request"`、`approvals_reviewer="user"`、`mcp_servers.ccnm.tools.exec_command.approval_mode="prompt"`。`/status` 为 `Read Only (Ask for approval)`。`/permissions` → `2. Approve for me` → `Permissions updated to Approve for me`；与备份比对，profile 的 `config.toml` 多了第一行 `approvals_reviewer = "auto_review"`（另有信任提示加的一段）。停止，退出 0。

**会话 B**（`e22cca81…`）：没有信任提示（已信任）。参数同样钉着 `approvals_reviewer="user"`，profile 第一行仍是 `auto_review`，`/status` 为 `Read Only (Ask for approval)`。

| 本机时间（epoch …） | 事 |
| --- | --- |
| 647.97 | 发消息：逐次跑 `["sh","-c","printf P72-A-7789af > p72-a.txt"]` 与 `…P72-B… > p72-b.txt` |
| 658.2 前 | 第一条弹出 `Allow the ccnm MCP server to run tool "exec_command"?`，只有 Allow / Cancel；Runtime 上只有 README |
| 664.61 | 选 Allow |
| 664.80（hpsrv 文件时间） | `p72-a.txt` 出现，属 ccrun，内容 `P72-A-7789af`；`output_ref r-bd05d1a5a91d45bb` |
| 668.7 前 | 第二条照样弹出提示 |
| 674.71 | 选 Cancel；模型收到 `user cancelled MCP tool call`；之后 5 秒以上 `p72-b.txt` 都不存在，Runtime 上这个会话只有一条输出记录 |
| 700.63 | `/permissions` → Approve for me 之后发消息：跑 `…P72-C… > p72-c.txt` |
| 713.79（hpsrv 文件时间） | 没有弹出问人的提示，`p72-c.txt` 出现，内容 `P72-C-7789af`；`output_ref r-d475fb86c9bf4b21` |

第三条从发消息到执行约 13 秒。Codex 的 "Reviewing approval request" 是一闪而过的状态行，2 秒一次的采样没抓到，回滚里也不留，所以只能确定"没问人、执行了"；自动审查跑了多久没有直接证据（P71 真机复验第 4 节在同一档下看到过它）。停止，退出 0。

**会话 C**（`68a4b51f…`）：参数钉着 `approvals_reviewer="user"`，profile 第一行仍是 `auto_review`，`/status` 为 `Read Only (Ask for approval)`。不发消息，停止，退出 0；本机没有 tmux server，hpsrv 上没有 ccnm 进程，写锁标记 `released`。

## 4. 资源与收尾

| 位置 | 本轮建的 | 现在 |
| --- | --- | --- |
| 本机 | 临时 Controller（标准 Label）、`~/.config/ccnm-p72/`、`~/.local/state/ccnm-p72/`、`~/.local/opt/codex-0.154.0/`、一次性密钥、`~/.ssh/config` 一段；profile 的 `config.toml` 被 Codex 加了 `approvals_reviewer` 与信任条目 | Controller 已 `bootout`、plist 已删，没有 ccnm/codex 进程；目录与密钥已删；`~/.ssh/config` 核对"现状 = 备份 + 本轮追加"后恢复（77 行），`known_hosts` 没动过；profile 的 `config.toml` 核对后从备份恢复，sha256 与开始前一致（`ccf5aca3…`）。日用二进制与配置没动 |
| hpsrv ccrun | `~/.config/`、`~/.local/state/ccnm/`、`~/p72/`、`authorized_keys` 一行 | 已删，`authorized_keys` 回到 0 字节；`~/.local/bin/ccnm` 仍是 v0.11.2 |
| hpsrv root、bing | 没动 | — |

界面截屏、`session.json` 留在本轮会话的临时目录，没有入库。
