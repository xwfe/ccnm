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
| Remote Workspace MCP（`ccnm mcp bridge`） | 预发布支持，契约已冻结 | `ccnm.workspace-mcp/1` 于 2026-09-11 冻结。允许矩阵在 macOS→macOS 上验过（[记录](research/p11-real-host-2026-09-11.md)、[证据](research/p11-matrix-20260911.json)）；远端真实项目 dogfood 在 **Debian 13 / x86_64 的 Linux Runtime** 上验过（[记录](research/p12-real-project-2026-09-11.md)、[证据](research/p12-dogfood-20260911.json)）：专用执行身份（uid 1002、只有自己的组、无 sudo、docker socket 不可写、`~/.ssh` 无私钥、无 `SSH_AUTH_SOCK`、读不到别人的 home），`cargo`/`rustc`/`node`/`npm`/`git` 由 `exec_command` 在那台机器上答出版本，read→search→patch→构建失败→`read_output` 分页→收回→**Runtime 侧 `git status` 为空**→测试通过，整套 Rust 测试在 Runtime 上绿。六条失败路径各有具名拒绝：writer busy、没 opt-in、read 请求 coding 不降级、协议号不认识（`CCNM_E_VERSION`，stdout 不说话）、远端进程被杀、Host 被杀。真实 Claude Code 2.1.268 在那棵远端树上完成了一次真改动（改文案、在远端跑测试、自己做了一个 commit），属主是执行身份。跨入口部分另有离线证明（[记录](research/cross-entry-p11-2026-09-11.md)）：受管会话与外部 coding 抢同一把 write guard，两个方向都是启动失败而不是"连上了写不进去"；一个不 import ccnm 代码的中立 MCP 客户端重放同一套矩阵得出同一结论。**边界**：只验过 Debian 13 / x86_64 与 macOS 两种 Runtime、一个 Host（Claude Code 2.1.268 的 `-p` 模式）、一棵中型 Rust 项目；Codex 当 Host、交互式 UI、arm64 Linux、非 Debian 系、monorepo 规模都没验。**egress 不作保证**（见下节）。已知代价：bridge 启动失败时 Claude Code 只显示 `Connection closed`，`CCNM_E_*` 诊断到不了用户面前，要手工跑一遍命令才看得到（见[出错了怎么办](troubleshooting.md)）。 |
| Claude legacy colocated | 明确拒绝 | remote-only 启动参数已从 native 候选命令移除，但 installed Claude 尚未真实验收；本 build 在创建 session 前返回 `CCNM_E_NOT_READY`。 |
| Claude/Codex Agent Instance colocated | 明确拒绝 | 没有可信 Runtime credential boundary 和真实验收，不自动降级为 legacy/native。 |
| Codex legacy/internal protocol 2 | 兼容历史 fixture | 只用于保留已有内部测量与回归，不是新的公共配置入口。 |
| `hybrid-smb`、第三 Provider、Browser/Git 专用 MCP、多 Agent/worktree 编排 | 未实现 | 不在当前范围内，不做隐式 fallback。 |

平台要分两件事说，因为 P12 之后它们不再是同一个答案。

**Agent 那一侧只有 macOS。** Controller 是 launchd LaunchAgent，会话上下文检查直接问 `launchctl` 和 `security`；Linux Controller、Windows 和其他官方 CLI 版本均未验收。CI 有一个 Linux job，但它是 **Runtime 门禁**：绿的意思是代码在 Linux 上编得过、测试过得去，不是说 Agent 那一半在那儿能跑。

**Runtime 那一侧（`internal mcp-serve` 与七工具）在 Debian 13 / x86_64 上验过一次**，作为 Remote Workspace MCP 的 dogfood（[记录](research/p12-real-project-2026-09-11.md)）：`cargo fmt`、严格 clippy 与全套测试在那台机器上 676 passed / 0 failed，与 macOS 同数。**那一次是从两个红开始的**，两个都真修了：`SystemRunner::run` 的超时只杀 leader（macOS 因为 bash 会 exec 而一直看不见），以及一个测试助手用了 BSD 语义的 `date -r`。Linux 上的 Managed 入口（Controller/session）仍未验收，Runtime 侧也只验过这一种发行版和架构。

**第三个红是发布前收口那一轮、由 CI 自己照出来的，它证明 P12 那次只修了一半。** 杀进程组走的是 `kill -KILL -<pgid>`，而 GNU/procps 的 `kill` 把开头带减号的参数当成**信号**读：`-8421` 被读成信号号，命令最后一个 pid 都没有，**退出码 0，什么都没杀**。所以进程组还在、孙进程还占着管道、超时还是不超时——而且这次还报成功。macOS 的 BSD `kill` 两种写法都当进程组，本机永远看不见；Debian 13 上碰巧是绿的（孤儿被别的东西收走了），所以 P12 的真机轮也没照出来。**是 ubuntu-24.04 的 runner 照出来的**：`write_guard` 里那条"残留子进程要人工恢复"的测试红了，残留进程在本该杀掉它的 kill 之后还活着。修法是在负 pid 前加 `--`（macOS 和 Linux 都认），三处都改了。当前 HEAD 在 ubuntu-24.04 与 macOS 上都是 **681 passed / 0 failed**，严格 clippy 都干净。

**发布物也是两个，对应上面这两件事。** 一个版本出两个下载：

| 下载 | 装在哪 | 是什么 |
| --- | --- | --- |
| `ccnm-<版本>-macos-universal.tar.gz` | Agent Node 或 Runtime Node | arm64 + x86_64 通用二进制，两个角色都能跑 |
| `ccnm-<版本>-linux-x86_64.tar.gz` | **只能是 Runtime Node** | 只有 Runtime 那一半有证据；Agent 那一半是 launchd LaunchAgent，在 Linux 上不跑 |

Linux 那个在 `ubuntu-24.04` 上本机构建，**glibc 下限是从二进制里量出来的**（`objdump -T` 里最高的 `GLIBC_x.y`），写在 release notes 里；Debian 13 的 glibc 是 2.41。**arm64 Linux 没有发布物**，因为没有证据。Linux 侧的 CI 门禁（clippy + 全套测试 + 构建发布物）和 macOS 一样每次 push 都跑，但那个 job 绿只说明代码在 Linux 上编得过、测试过得去，**不说明 Agent 那一半支持 Linux**。见[开发与发布](development.md)。

**Runtime Node 的前置条件**（Managed 与 Remote MCP 两个入口都要）：`git`、**`ripgrep`**（`search_text` 调 `rg`，没有它七工具就少一个），加上项目自己需要的工具链。装什么、谁维护，见[运维手册](operations.md#runtime-node-的前置条件与项目工具链)。

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

**"崩了"有两种，结局不同**，两种都在真机上验过（[P12 记录](research/p12-real-project-2026-09-11.md)）：

| 谁死了 | 锁的状态 | 下一个 coding 会话 |
| --- | --- | --- |
| Host / bridge（`kill -9` 客户端那一侧） | `released` | 直接能开，**不需要人工恢复**——远端读到 EOF 后自己跑完了收尾 |
| 远端 `internal mcp-serve`（在 Runtime 上被杀） | `held <session> <workspace>` | 被拒，要按上一段做人工恢复 |

两种情况下 `read` 会话都照常打开，而远端都没有留下孤儿进程。所以"Claude Code 崩了/被关掉"通常什么都不用做；要人动手的是 Runtime 侧的执行者被杀那一种。

## 两个逃生开关：接受了什么，以及绝不会变绿

默认形态是 Runtime 上一个专用低权限账号，读不到任何已知 Agent 凭据。两个 per-workspace 开关可以偏离它，都写在 **Runtime 自己的配置**里（承担风险的机器决定，调用方带不进来），**互不蕴含**：

| 开关 | 接受的是 | 典型场景 |
| --- | --- | --- |
| `allow_unconfined_exec` | 这个账号没通过 confinement 检查（有 sudo/admin、名下有私钥……） | 还没建专用账号的临时项目 |
| `allow_unisolated_credentials` | 这个账号**能读到已知 Agent 登录**，或者可达性"说不清"（symlink、列不出来） | 项目和 Claude 登录在同一个家目录：一台机器、一个账号 |

**接受之后那几行是 WARN，永远不会变成 OK。** 那个性质并没有成立，只是有人签字说能接受；`ccnm doctor` 会一直显示，并注明是哪个开关接受的。另外 ccnm 会在第一次用它起会话时在终端上把风险讲一次（只讲一次，marker 在 state 目录；关掉再打开算新决定，会再讲），每条命令结果里的 unconfined 说明也会写明。

**`allow_unisolated_credentials` 放开的是这个项目唯一那条硬边界**：模型跑的每一条命令都能读到那份登录，而让它跑一条命令只需要一句 prompt——包括从它被要求读的文件里冒出来的那一句。放开之后没有别的东西在挡着。代价的完整说明见[生产安全](production-safety.md#凭据隔离那一条怎么放开代价是什么)。

**两条任何开关都放不开**：执行身份未知（没人能说清是谁接受了什么），以及认证环境是继承来的（凭证被塞进每个子进程，而修法只是别 export）。

这一段描述的是当前代码的行为。**下面"P3 发布门禁结果"第 2 条那个"读不到任何已知 Agent 凭据"是那次真机验收的事实**，它验的是默认形态，不因为存在开关而失效。

## egress：不作保证

ccnm 不实现任何网络策略，**也不声明任何 egress 边界**。`exec_command` 能跑任意程序，任意程序能联网；真机上 Runtime 执行身份的出站是通的——项目工具链就是这么装上去的。本项目的"隔离"只覆盖 OS 身份、文件可达性和写互斥这三件事。

需要限制出站的，在 tailnet ACL、防火墙或 OS 网络策略上做，并且不要拿 `ccnm doctor` 的输出当依据：它查不到网络策略。另外注意 `No SSH keys` 那一行的语义是"Runtime Executor 名下没有 ccnm 出站需要的私钥/agent"，不是"这个账号出不去"。

## P3 发布门禁结果

四条门禁的实测结果，证据见 [Codex 方向](research/p3-public-codex-2026-09-08.md) 与 [Claude 方向](research/p3-public-claude-2026-09-09.md)：

1. **通过（Ctrl-D 除外）**：当前 build 在授权双机上分别跑通 Claude/Codex 公共入口的 print、interactive、stop、detach/reattach、Controller 重启和链路失败。Ctrl-D 未取得证据——官方 CLI 对该键无响应，只用 `/exit` 覆盖了同一条自然退出路径，两者不等价。
2. **通过**：目标 Runtime 的专用执行身份能正常使用项目，但读不到任何已知 Agent 凭据和 SSH 私有状态，没有 sudo/admin，特权 socket 不可写。
3. **未验证**：本轮只做了 transport 进程故障注入，不等同物理断网，egress/网络策略没有逐项验证。**因此这里不声明任何 egress 边界**；需要这种保证的场景，由 OS 和网络层自己落实，不要拿本项目的诊断输出当依据。
4. **通过**：不复制现有订阅凭据，不把 `CODEX_HOME`、`CLAUDE_CONFIG_DIR` 或 profile 路径发给 Runtime。

双机验收用的临时账号、组、公钥、SSH 准入和测试目录已全部清理归零，无无法清理项。

离线门禁通过不等于 production READY，也不授权部署、替换正在运行的 Controller、创建账号或修改 ACL/防火墙——这些动作每次都要单独授权。
