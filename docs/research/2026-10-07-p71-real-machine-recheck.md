# P71 真机复验，hpsrv 升 v0.11.0（2026-10-07）

接 [P71 记录](2026-10-07-p71-codex-asks-before-exec.md)第 6 节"没覆盖的"：受管 Codex 交互会话执行命令前问人，之前只有零额度实测与离线测试。用户授权"真机复验 P71，hpsrv 也升 v0.11.0"。两端都是 [v0.11.0 发布版](2026-10-07-release-0.11.0.md)。

## 1. 结论

**P71 在真机上成立，但查出一条中等影响的新问题 F27（第 5 节），没修。**

| 项 | 结果 |
| --- | --- |
| hpsrv 升级 | ccrun 的 `~/.local/bin/ccnm` 由 v0.10.1 换成 v0.11.0 发布版（二进制 `c837a445…`），v0.10.1 原样留在 `~/.local/opt/ccnm-0.10.1/` |
| 预检 → 会话记录 → 启动参数 | P71 记录说"没有端到端测试"的那一段真机通了：`session.json` 里 `ask_before: ["exec_command"]`，Codex 启动参数带 `mcp_servers.ccnm.tools.exec_command.approval_mode="prompt"` |
| 每次都问 | 每次 `exec_command` 前停住，`Allow the ccnm MCP server to run tool "exec_command"?`，只有 Allow / Cancel；放行一次后下一次照样问 |
| 放行 | 命令在 Runtime 上以 ccrun 执行（文件、内容、属主都对） |
| 取消 | 模型收到 `user cancelled MCP tool call`；被取消的是一条合法命令，Runtime 上没有它的文件、也没有它的输出记录——没有执行 |
| doctor | 0 失败；`Command approval` 是 P71 之后的说法 |
| 精确停止 | 0.13 秒、退出 0，tmux 与 Runtime 进程都没了，写锁 `released` |
| Approve for me（P71 记录列为没测） | Codex 自动审查放行了两条命令（含 `rm -f`），没问人；**这个选择被写进 Codex profile 的 `config.toml`，之后用这个 profile 起的受管会话都不再问**（F27） |

真实模型：Codex `codex-main`，上限 3 次，用了 2 次（会话 2 个；第一个会话里发了两轮消息）。

## 2. 授权与拓扑

| 授权项 | 这一轮 |
| --- | --- |
| 部署 | hpsrv ccrun 换成 v0.11.0（用户要的，保留）。本机用日用的 v0.11.0 发布版二进制本身，配置与 state 另放（`CCNM_CONFIG=~/.config/ccnm-p71/config.toml`、`XDG_STATE_HOME=~/.local/state/ccnm-p71`），日用配置不动 |
| Controller | 本机原来没有。用 `controller install` 装在标准 Label 下（自己写了这两个变量），plist 手工加 `PATH` 让它找到钉住的 Codex 0.154.0（Homebrew 是 0.159.3） |
| Codex 0.154.0 | P71 那轮下载的两个包，按 GitHub release 的 digest 核 sha256（`344310a0…`、`500ee2a0…`）后解到 `~/.local/opt/codex-0.154.0/` |
| SSH | 本机一把一次性密钥只连 ccrun：`authorized_keys`（原本 0 字节）加一行，`from="100.107.211.119"`、禁端口/agent/X11 转发；本机 `~/.ssh/config` 先备份再追加一段 `Host ccnm-p71-ccrun`。`known_hosts` 里本来就有这个 IP，没改 |
| 测试项目 | ccrun 家目录 `~/p71/codex`，一个只有 README 的 git 仓库，不是真项目 |
| 真实模型 | Codex `codex-main`（默认 profile `~/.config/ccnm/agents/codex`），workspace `p71codex`，上限 3 次 |
| 清理 | 本轮建的全删，ssh 配置与 Codex profile 的 `config.toml` 从备份恢复 |

**Operator 就在 Agent 上。** P62 是 hpsrv 的 bing 当 Operator、ssh 进 Agent 起会话；这次用文档支持的另一种拓扑：Agent 机器上只有 Agent 配置（顶层 `runtime_node = "hpsrv"`），`ccnm run` 先去 Runtime 取 workspace 定义，再在本机起会话。两种入口走的是同一个 `work::start`（`preflight` → `start_fresh`），P71 改的正是这一段。这样不用往本机加入站密钥，也不用在 hpsrv 的 bing 下装东西。代价是 `--print` 与 `cleanup` 只能在 Runtime 那一侧发起（报 `--print has to be run where the projects are`），这两样这轮没做（第 6 节）。

`ccnm doctor p71codex`（本机）：0 失败，3 项没查（Codex exec-server、Native tool policy、Network isolation，都是有意的）。Controller Aqua、Codex 0.154.0、ChatGPT 登录（只看本地）、反向 SSH `hpsrv as ccrun, ccnm 0.11.0`、执行账号各项安全检查、Workspace root、MCP 握手（11 个工具，16 293 字节）都是 OK。`Command approval` 一行：`interactive sessions ask before each exec_command, until the person at the terminal switches the session to Full Access or Approve for me in /permissions`。

## 3. 交互会话：问、放行、取消（模型 1/3）

`ccnm run p71codex --detached` 2.4 秒返回，会话 `5366f2e5…`。第一次启动停在 Codex 的信任提示（问的是 ccnm 的空占位目录），按使用说明选 Yes。hpsrv 上随即出现 ccrun 的 `internal mcp-serve`。

**第一轮消息**：让模型逐次调两次 `exec_command`（`printf … > p71-a.txt`、`printf … > p71-b.txt`）。

- 发出后 6 秒内弹出审批提示。这时 Runtime 上只有 README、没有 `printf` 进程。选 Allow。
- 这次调用到了 server，但模型把 `cmd` 写成了字符串，被参数校验拒掉：`failed to deserialize parameters: invalid type: string "…", expected a sequence`。没有执行任何命令。
- 模型改成数组再调，而且用了 `["zsh","-lc",…]`——它按 Agent 这台 macOS 猜的 shell，hpsrv 上没有 zsh。**照样问**；选 Cancel，模型收到 `user cancelled MCP tool call`。

这一轮证明了"放行后下一次还问"和"取消能拦住"，但被取消的那条就算放行也会失败，被放行的那条又没执行，证据不干净。**第二轮消息**（同一会话），命令写死成 `["sh","-c","printf P71-A-… > p71-a.txt"]` 和 `…p71-b.txt`：

| 本机时间（epoch …） | 事 |
| --- | --- |
| 408.2 | 第一条的审批提示出现，Runtime 上还没有 `p71-*` 文件 |
| 413.0 | 选 Allow |
| 413.1（hpsrv 文件时间） | `p71-a.txt` 出现，属 ccrun，内容 `P71-A-f907b4`；界面 `ok in 2 ms`，`output_ref r-a8e0328b68bf4107` |
| 419.1 | 第二条的审批提示出现 |
| 426.3 | 选 Cancel；模型收到 `user cancelled MCP tool call` |
| 之后 5 秒以上 | `p71-b.txt` 始终不存在 |

Runtime 上这个会话的输出目录只有一条 `r-a8e0328b68bf4107`，就是放行的那一次。被取消的第二条是合法命令，到了 Runtime 就会执行并留下输出记录，两样都没有，所以它没到 Runtime（与 P71 零额度实测里探针 server 没收到一致）。反过来，"没有输出记录"本身不够：第一轮那条参数格式错的调用到过 server，也没留记录。

`ccnm stop p71codex --session 5366f2e5…`：0.13 秒、退出 0；本机 tmux server 没了；hpsrv 上没有 ccnm 进程；写锁标记 `released`。

## 4. Approve for me（模型 2/3）

新会话 `5a137163…`（`ask_before` 同样是 `["exec_command"]`）。不发消息先 `/permissions` → `2. Approve for me`，界面回 `Permissions updated to Approve for me`。再让模型逐次跑 `["sh","-c","printf P71-D-… > p71-d.txt"]` 和 `["sh","-c","rm -f p71-a.txt"]`。

- 界面先显示 `Reviewing approval request … MCP exec_command on ccnm`（Codex 自己的自动审查在判），之后**两条都执行了，没有任何提示问人**：`p71-d.txt` 出现，`p71-a.txt` 被删；Runtime 上多了两条输出记录。
- 第一条从发消息到文件出现约 15 秒，其中审查至少 8 秒。第二条有没有单独审查一次，界面收起后看不出来。

也就是说这一档等于把这道闸整个交给 Codex，连删文件都放行。这和文档"交给 Codex 自己的自动审查"一致；没料到的是下一节。

## 5. F27：Approve for me 被记进 profile，之后的会话都不问（零额度）

收尾时比对 Codex profile 的 `config.toml`（本轮开始前已备份；只看这个文件，没碰凭据）：除了信任提示加的那一段，第一行多了

```toml
approvals_reviewer = "auto_review"
```

受管 Codex 会话的 `CODEX_HOME` 就是这个 profile 目录，所以这一行会作用到之后每个会话。以下都没发消息，不耗额度：

| 情况 | 新会话里 `/status` 的 Permissions |
| --- | --- |
| 配置里有那一行，`ccnm run` 新起会话 | `Read Only (Approve for me)` |
| 只删掉那一行，再起 | `Read Only (Ask for approval)` |
| 在会话里切 Full Access（确认页写的是 "Apply full access for this session"） | 配置里只多了 `approvals_reviewer = "user"`，沙箱、审批策略都没写；下一个会话 `Read Only (Ask for approval)` |
| 直接起 Codex 0.154.0（同一 profile、配置里是 `auto_review`、同样的 `--sandbox read-only -c approval_policy="on-request"`） | 不加别的：`Read Only (Approve for me)`；加 `-c approvals_reviewer="user"`：`Read Only (Ask for approval)`。两次都没改配置文件 |

**原因**：ccnm 启动 Codex 时用命令行钉住了沙箱（`--sandbox read-only`）和审批策略（`approval_policy="on-request"`），所以 Full Access 只管当前会话；但审批由谁来答（`approvals_reviewer`）没钉，Codex 又把 `/permissions` 里的这一项存进 `CODEX_HOME/config.toml`，于是一次选择延续下去。

| 编号 | 影响 | 现象（真机） | 建议 |
| --- | --- | --- | --- |
| F27 | 中；没修 | 受管 Codex 会话里有人选过一次 Approve for me，之后用同一 profile 起的所有受管会话都由 Codex 自动审查，`exec_command` 再也不问人（第 4 节里连 `rm -f` 都放行）。doctor 的 `Command approval` 仍是 OK、说"直到终端前的人把这个会话切走"，doctor 看不到这一行 | 交互会话启动参数加 `-c approvals_reviewer="user"`（上表最后一行实测命令行能盖过配置）。这样会话里的人仍能临时切走（ccnm 拦不住，文档本来就这么写），但只管那一个会话；doctor 的说法随之成立。exec-server 链用 ccnm 自己生成的 `CODEX_HOME`，不读 profile 的配置，按文档推断不受影响，没测 |

在修好之前，文档（排错手册、配置说明、使用说明）已经按实测写明：这一档会延续到之后的会话、怎么看出来、怎么去掉。

## 6. 没覆盖的

- **`--print` 的真机回归**：要在 Runtime 一侧起 Operator、ssh 进 Agent，即往本机加入站密钥，不在这轮范围里。离线用例守着 print 会话不加 `prompt`、仍是 `approval_policy="never"`。
- 在会话里选回 "Ask for approval" 会往配置写什么：没单独测（Full Access 写的是 `user`，第 5 节；删掉那一行实测有效）。
- Linux 门禁没重跑：这轮没有改代码，发布版的 CI 已在 ubuntu 上跑过。
- Claude 会话不受 P71、F27 影响，这轮没碰 fodelf。

## 7. 资源与收尾

| 位置 | 本轮建的 | 现在 |
| --- | --- | --- |
| 本机 | Controller（标准 Label，plist 加了 `PATH`）、`~/.config/ccnm-p71/`、`~/.local/state/ccnm-p71/`、`~/.local/opt/codex-0.154.0/`、一次性密钥、`~/.ssh/config` 一段；Codex profile 的 `config.toml` 被 Codex 加了信任条目和 `approvals_reviewer` | Controller 已 `bootout`、plist 已删，没有 ccnm/codex 进程、没有 tmux server；目录与密钥已删；`~/.ssh/config` 核对"现状 = 备份 + 本轮追加"后从备份恢复（77 行），`known_hosts` 没被改过；profile 的 `config.toml` 从备份恢复，sha256 与本轮开始前一致（`ccf5aca3…`）。日用二进制、配置没动 |
| hpsrv ccrun | `~/.config/`、`~/.local/state/ccnm/`、`~/p71/`、`authorized_keys` 一行 | 已删，`authorized_keys` 回到 0 字节，家目录与本轮开始前一致；**保留** `~/.local/bin/ccnm` = v0.11.0 与副本 `~/.local/opt/ccnm-0.11.0/`，旧版在 `~/.local/opt/ccnm-0.10.1/` |
| hpsrv root | `/root/ccnm-p71-20261007/`（解包目录） | 已删 |
| hpsrv bing | 没动 | — |

Codex profile 里 Codex 自己写的会话记录、历史、sqlite 日志随两次会话增加，和以前几轮一样没动。原始界面截屏与 `session.json` 留在本轮会话的临时目录，没有入库（里面有家目录路径与账号信息）。

**回退 hpsrv**（以 ccrun）：

```bash
install -m 755 ~/.local/opt/ccnm-0.10.1/ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
```
