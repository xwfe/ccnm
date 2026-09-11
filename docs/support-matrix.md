# 支持矩阵

本页只描述当前代码和现有证据。`docs/plan/status.json` 是阶段进度的唯一事实来源；历史真机记录不能替代当前 build 的重新验收。

## Provider 与 topology

| 配置 / 入口 | 当前状态 | 证据与限制 |
| --- | --- | --- |
| legacy Claude，remote SSH MCP，从 Runtime Node 发起 | 预发布支持 | 旧公开命令、Claude v1 wire 与 remote CLI golden 保持兼容；双机真机跑通。 |
| legacy Claude，remote SSH MCP，从 Agent Node 发起 | 预发布支持 | `run` 先委托 Runtime 解析 workspace，`attach/status/result/stop` 继续在 Agent 本机管理已有 session；`--print` 仍需在 Runtime Node 执行。 |
| Claude Agent Instance，remote SSH MCP | 预发布支持 | 默认 instance 与 `--agent`、print/interactive、doctor、精确 session、profile 隔离均有测试；公共双机 dogfood 已在授权环境真机通过，见下方门禁结果。 |
| Codex Agent Instance，remote SSH MCP | 预发布支持 | 仅接受实测的 Codex CLI `0.154.0`（见下方版本 pin）；公共入口已在授权双机真机验证。 |
| Machine API（`ccnm rpc`），`print` 模式 | 预发布支持，协议已冻结 | `ccnm.machine/1` 于 2026-09-10 冻结。两个 provider 各跑通一次真机双机闭环并与人类 CLI 对照（[Claude](research/p7-real-machine-2026-09-10.md)、[Codex](research/p7-codex-parity-2026-09-10.md)）：两条腿产物属主相同，`usage` 端到端到达调用方（Codex 不报 `cost`，永远缺席）。实现仍比契约少四条，见[协议说明](protocol/README.md)。 |
| Machine API 的 `interactive` 模式、输出分页、结果过期 | 未实现 | 都不在 `hello` 声明的能力里，调用会被明确拒绝，不静默降级。 |
| Remote Workspace MCP（`ccnm mcp bridge`） | **experimental** | 允许矩阵已在真机上验过一次（[记录](research/p11-real-host-2026-09-11.md)、[证据](research/p11-matrix-20260911.json)）：真 ssh、真实 Claude Code 2.1.268、Runtime 执行身份是专用账号。coding 七工具、产物属主就是那个执行身份、read 正好四工具且硬调被收起来的工具（参数合法）全被拒、只读 workspace 上请求 coding 不降级、同一棵树第二个 coding 被写锁拒、Host 看到的文本无私有路径。跨入口部分另有离线证明（[记录](research/cross-entry-p11-2026-09-11.md)）：受管会话与外部 coding 抢同一把 write guard，两个方向都是启动失败而不是"连上了写不进去"；一个不 import ccnm 代码的中立 MCP 客户端重放同一套矩阵得出同一结论。**仍是 experimental**，因为只验过 macOS→macOS、一个 Host 的一个版本、一棵一次性空树；Linux Runtime 与远端真实项目 dogfood 是 P12。另有一条已知代价：bridge 启动失败时 Claude Code 只显示 `Connection closed`，`CCNM_E_*` 诊断到不了用户面前，要手工跑一遍命令才看得到（见[出错了怎么办](troubleshooting.md)）。不要按"已支持"部署到有价值的项目上。 |
| Claude legacy colocated | 明确拒绝 | remote-only 启动参数已从 native 候选命令移除，但 installed Claude 尚未真实验收；本 build 在创建 session 前返回 `CCNM_E_NOT_READY`。 |
| Claude/Codex Agent Instance colocated | 明确拒绝 | 没有可信 Runtime credential boundary 和真实验收，不自动降级为 legacy/native。 |
| Codex legacy/internal protocol 2 | 兼容历史 fixture | 只用于保留已有内部测量与回归，不是新的公共配置入口。 |
| `hybrid-smb`、第三 Provider、Browser/Git 专用 MCP、多 Agent/worktree 编排 | 未实现 | 不在当前范围内，不做隐式 fallback。 |

当前目标平台是 **macOS，只有 macOS**。CI 也只跑 macOS，这不是"还没顾上 Linux"：Controller 是 launchd LaunchAgent，会话上下文检查直接问 `launchctl` 和 `security`，一个绿色的 Linux job 测的会是这个程序跑不了的东西。Linux Controller、Windows 和其他官方 CLI 版本均未验收。

## Codex 版本 pin 与重新测量

只接受 `codex-cli 0.154.0`，**精确匹配**。别的版本——包括更新的，也包括曾经测过的 `0.153.4`——在启动前就返回 `CCNM_E_VERSION`，消息是 `Codex <版本> has not been measured; this adapter requires 0.154.0`。

上一次换版本的完整记录见 [0.154.0 重新测量](research/codex-0.154.0-2026-09-10.md)：那一次 `unified_exec_tty` 是新出现的 stable 且默认开启的执行路径，旧的禁用列表按名字拦不住它——"改个常量"正好会漏掉这种东西。

**为什么钉死一个版本。** Codex 的 JSONL 输出形状、参数名和工具开关都是实测出来的，不是它的文档承诺的。某个 patch 版本改掉 JSONL 里一个字段，ccnm 不会报错，只会把结果解析错——而解析错比拒绝启动难发现得多。

**版本变了要重新测量，不是改个常量。** 步骤：

1. 在 Agent Node 装新版本，用官方 CLI 独立登录（不要复制 `~/.codex`）。
2. `python3 scripts/measure_codex.py <输出目录> inspect` 采集 `--version`、`--help`、工具开关；`inspect` 不启动模型。要采 JSONL 就再跑 `seven-tools`，那一步**会消耗登录额度**，只在一次性 fixture 文件上操作。
3. 结果落成 `tests/fixtures/codex-<新版本>/`，**不要覆盖旧目录**——旧 fixture 是回归基线。
4. 改 `crates/ccnm-core/src/provider/codex/mod.rs` 的 `VERSION`，跑 `cargo test -p ccnm-core provider::codex`。
5. 逐条比对新旧 fixture 的差异，把行为变化写进研究记录。

**第 5 步不能跳。** 跳过它就是把一次未知的行为变更，混进一次看起来只是"升级版本号"的提交里。

## Codex 的 Code Mode 与工具面：验证到哪一步

Code Mode 是 Codex 的一个 under-development 特性，它把工具包一层，让模型通过代码调用而不是直接调函数。ccnm 用它是为了收窄模型看得见的工具：加上 `features.code_mode.excluded_tool_namespaces`，Codex 自带的 `apply_patch` 在模型眼里就不存在了。

**但模型可以不支持它，而且事前问不到。** `gpt-5.3-codex-spark` 就不支持，Codex 启动时会打一句 `model … does not advertise Code Mode support`；`codex doctor --json` 只报特性开关是否打开，不报模型支不支持。所以 ccnm 只对**实测过的模型**开 Code Mode——目前那就是不写 `model` 时的 CLI 默认模型，全部 fixture 都是在它上面采的。

两种配置的验证范围不一样，按实测写清楚：

| 配置 | 模型看得见的工具 | 挡住 Agent 本机写入的是什么 |
| --- | --- | --- |
| **Code Mode 开**（不写 `model`） | 顶层只剩 exec/wait/用户输入/clock，ccnm 的七个工具在嵌套注册表里；Codex 自带 `apply_patch` **不存在**（[2026-09-07 探测](research/codex-provider-probe-2026-09-07.md)） | 工具被移除，外加只读 sandbox |
| **Code Mode 关**（`model` 写了一个未实测的模型） | 顶层有 `functions.apply_patch`（Codex 自带的），ccnm 的七个工具经 `tool_search` 取用（[tool-surface fixture](../tests/fixtures/codex-0.154.0/tool-surface.json)） | **只有只读 sandbox** |

**这是一次真实的取舍，不是等价替换。** 关掉 Code Mode 之后，拦住 Codex 自带 patch 工具去写 Agent 本机的只剩只读 sandbox 一层；开着它却硬塞给不支持的模型，代价是模型可能根本用不明白工具——实测出现过"回 DONE 但一个字没写"（[parity 记录](research/p7-codex-parity-2026-09-10.md)）。ccnm 选了前者：宁可工具面宽一点也要模型真的能干活，并把范围写在这里，而不是让两种配置看起来一样安全。

想要窄的那一栏，就用默认模型（不写 `model`）。要给某个具体模型开 Code Mode，得先按上面的重新测量流程实测它，再把它加进 `CODE_MODE_MODELS`——Rust 和采集脚本里各有一份，必须一起改。

已经试过但**无效**的路：`-c tools.apply_patch=false`（以及 `disabled_tools` 的几种写法）配置能加载，但实测工具面一点没变，属于被静默忽略的键。不要拿它当开关。

## Agent Instance 入口

Runtime workspace 只保存 `{node, instance}` 引用；Provider 和 `profile_ref` 由 Agent Node 的 `[agents.*]` 解析。在 Runtime Node 发起时，以下命令共享 workspace 默认值或显式选择；Agent Node 的本机生命周期命令不重新查询 Runtime 默认值，跨 instance 管理请同时指定 `--agent` 和 `--session`：

```bash
ccnm doctor demo
ccnm run demo
ccnm run demo --agent codex-main
ccnm attach demo --agent codex-main --session <ccnm-session-id>
ccnm status demo --agent codex-main --session <ccnm-session-id>
ccnm result demo --agent codex-main --session <ccnm-session-id>
ccnm stop demo --agent codex-main --session <ccnm-session-id>
```

`--agent` 只能是同一 Agent Node 上已配置的 instance id，不能传 node、Provider、root、profile 路径或官方 CLI argv。legacy workspace 不接受该参数。

ccnm session id 是生命周期主键；Claude/Codex 自己的 thread/resume id 只作为结果元数据，两者不能混用。精确命令会同时校验 workspace、instance 和 session 记录。

## Runtime 单写 guard

每个真实 `internal mcp-serve` 在 Runtime 上持有工作树级独占 guard，直到整个 MCP server 结束：

- 普通目录按 canonical root 互斥；嵌套 root 和 symlink alias 配置拒绝；
- Git workspace 按 canonical `git-common-dir` 互斥，因此同一仓库的 worktree 也保守串行；
- 正常退出写入精确 `released` 状态并显式解锁；
- live owner 返回 busy；异常退出留下 `held <session> <workspace>`，状态为 unknown，不按时间自动接管；
- guard 覆盖 `exec_command` 和 `apply_patch` 所在的完整 MCP 生命周期，不只是某个工具调用或某个 Agent Node；
- **外部 MCP 的 `coding` 会话抢同一把锁**，`read` 会话不碰它（没有能改东西的工具，让它等写者只会白等）。

异常恢复必须由 Runtime 操作者完成：先按 session/status 和进程列表证明旧 supervisor、Agent、SSH MCP 及其子进程都已结束，再在 Runtime 的 `${XDG_STATE_HOME:-$HOME/.local/state}/ccnm/write-guards/` 中定位包含该 session id 的**单个** marker，备份后删除该文件。不要批量删除，也不要仅因时间过去就清理。删除前无法证明旧执行者结束时，保持 unknown 才是正确状态。

## P3 发布门禁结果

四条门禁的实测结果，证据见 [Codex 方向](research/p3-public-codex-2026-09-08.md) 与 [Claude 方向](research/p3-public-claude-2026-09-09.md)：

1. **通过（Ctrl-D 除外）**：当前 build 在授权双机上分别跑通 Claude/Codex 公共入口的 print、interactive、stop、detach/reattach、Controller 重启和链路失败。Ctrl-D 未取得证据——官方 CLI 对该键无响应，只用 `/exit` 覆盖了同一条自然退出路径，两者不等价。
2. **通过**：目标 Runtime 的专用执行身份能正常使用项目，但读不到任何已知 Agent 凭据和 SSH 私有状态，没有 sudo/admin，特权 socket 不可写。
3. **未验证**：本轮只做了 transport 进程故障注入，不等同物理断网，egress/网络策略没有逐项验证。**因此这里不声明任何 egress 边界**；需要这种保证的场景，由 OS 和网络层自己落实，不要拿本项目的诊断输出当依据。
4. **通过**：不复制现有订阅凭据，不把 `CODEX_HOME`、`CLAUDE_CONFIG_DIR` 或 profile 路径发给 Runtime。

双机验收用的临时账号、组、公钥、SSH 准入和测试目录已全部清理归零，无无法清理项。

离线门禁通过不等于 production READY，也不授权部署、替换正在运行的 Controller、创建账号或修改 ACL/防火墙——这些动作每次都要单独授权。
