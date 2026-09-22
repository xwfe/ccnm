# 支持矩阵

本页只描述当前代码和现有证据。`docs/plan/status.json` 是阶段进度的唯一事实来源；历史真机记录不能替代当前 build 的重新验收。

## Provider 与 topology

| 配置 / 入口 | 当前状态 | 证据与限制 |
| --- | --- | --- |
| legacy Claude，remote SSH MCP，从 Runtime Node 发起 | 支持 | 旧公开命令、Claude v1 wire 与 remote CLI golden 保持兼容；双机真机跑通。 |
| legacy Claude，remote SSH MCP，从 Agent Node 发起 | 支持 | `run` 先委托 Runtime 解析 workspace，`attach/status/result/stop` 继续在 Agent 本机管理已有 session；`--print` 仍需在 Runtime Node 执行。 |
| Claude Agent Instance，remote SSH MCP | 支持 | 默认 instance 与 `--agent`、print/interactive、doctor、精确 session、profile 隔离均有测试；公共双机 dogfood 已在授权环境真机通过，见下方门禁结果。 |
| Codex Agent Instance，remote SSH MCP | 支持 | 仅接受实测的 Codex CLI `0.154.0`（见下方版本 pin）；公共入口已在授权双机真机验证。 |
| Codex Agent Instance，exec-server 链（workspace 写 `codex_exec_server = true`） | **封存**（2026-09-17）：opt-in 保留、只认 0.154.0、不再维护 | **这条链已封存，默认路径是上面那行的 MCP 七工具。**为什么、封存后会怎样、什么情况下解封，见[双执行入口方案](plan/runtime-surfaces.md)第 12.0 节。下面是封存前的证据，只对 Codex 0.154.0 成立。Codex 用自带的执行工具，由 Runtime 上受 ccnm 监督和过滤的官方 `codex exec-server` 执行；只开交互模式，print 在创建会话前拒绝；Claude 不受影响。**真机**（[P24 记录](research/p24-native-real-machine-2026-09-16.md)）：macOS Agent + Debian 13 x86_64 Runtime（专用执行身份，无豁免开关），真实 Codex 0.154.0 完成读→改→跑测试，改动和进程属主都是执行身份；原生链、受管 MCP、外部 MCP coding 抢同一把写锁；Agent 侧 ssh、Runtime 侧 sshd 会话、exec-server、监督进程被杀和 setsid 子进程各 20 次，冻住与网络黑洞各 5 次，零越权、零重放、零残留，未确认退出不放锁。**前提**：Linux Runtime 装 bubblewrap 并允许执行账号创建 user namespace。**边界**：只验了这一种平台组合和 Codex 默认模型；Agent 静默离网（断网、睡眠）时，Runtime 连续 10 分钟收不到任何字节才结束会话、放锁（P26，[运维手册](operations.md#agent-静默离网之后exec-server-链的锁一直-held)），所以**离开超过 10 分钟的原生会话会被结束**，Codex 里要 `/exit` 重开——这一条只在本机用真实 Codex 验过，真机网络黑洞没有复测；`ccnm doctor` 的 `Codex 原生链` 一行做的是 `ccnm run` 起会话前的同一次空会话预检（`codex_bin`、版本、exec-server 能不能起、写锁；离线测试覆盖，没上真机），不跑命令，所以查不出 Linux 沙箱前提（[使用说明](usage.md#codex-原生链那一行)）；资源上限只在本机 macOS 测过（[P29 记录](research/p29-native-gates-2026-09-17.md)）：200 MiB 连续输出时 Runtime 上 `exec-serve` 峰值 RSS 约 5.5 MiB，exec-server 读一个 200 MiB 文件约用 1 GiB，磁盘写满只回错误、会话照常，**单个文件写入约 24 MiB 为上限**（超过结束会话，见[排错手册](troubleshooting.md#codex-会话里模型报-toolsexec_command-is-not-a-function或-exec-server-transport-disconnected)）。exec-server 在一次带沙箱的文件操作中途被强杀时，它留下的 fs helper 在放锁前被清掉（P30，macOS 实测；Linux 上 helper 靠 bubblewrap 的 die-with-parent 随之结束，未实测）。`v0.8.0`（2026-09-20）里有这条链的代码，但它仍然是 opt-in 且不维护，也不为它单独发版。 |
| `exec_command` 的 OS 沙箱（workspace 写 `exec_sandbox = "codex"`） | 支持，opt-in，本机 macOS 与 Linux 容器实测 | 三种入口（受管 Claude/Codex、外部 MCP）的每条 `exec_command` 包进 `codex sandbox`，权限对象是 Codex 0.154.0 发给自己命令的那份，一字不改（[配置说明](configuration.md#exec_sandbox)、[P33 记录](research/p33-exec-sandbox-2026-09-17.md)）。**实测**：warm cache 的 `cargo build/test/run`、node、python、git 只读操作正常，编译耗时无差别，每条命令多约 40 ms（macOS）/ 14 ms（Linux）；挡住工作区外写、HOME 写、`.git` 写（`git commit` 失败）、网络（依赖下不了）、`ps`。Runtime 给不了沙箱时会话启动就拒。**前提**：Runtime 节点 `codex_bin` = Codex 0.154.0（与封存的原生链同一个 pin）；Linux 装 bubblewrap 并允许执行账号建 user namespace。**边界**：没上真机、没跑模型回合；Linux 只在本机 aarch64 容器里验过（真实 ccnm 二进制在容器里过了集成测试，含超时杀到沙箱内命令）；`codex sandbox` 的参数和权限对象形状随 Codex 版本变，升级 Codex 要重测。 |
| 项目自带的 skills（`load_skill` 工具 + MCP prompts） | 支持，**只有离线证据** | 三种入口（受管 Claude/Codex、外部 MCP 的 `read` 与 `coding`）共用一个实现（[使用说明](usage.md#项目自带的-skills)、[协议第 5.1 节](protocol/remote-workspace-mcp-v1.md#51-load_skill-与-prompts项目自带的-skillsp36-新增)、[P36 记录](research/p36-skills-surface-2026-09-17.md)）。**验过的**：发现三个位置、重名与读不了的文件给出原因、经 symlink 出 workspace 的 skill 被拒、目录不超过 2048 个 UTF-16 码元、参数替换对齐 Claude Code 2.1.273 的实际行为、`` !`命令` `` 不被执行、prompts 列出与获取——Rust 单元测试加一个不 import ccnm 代码的中立 MCP 客户端对真实二进制跑。frontmatter 解析器在一台开发机的 810 个真实 skill / 命令文件上全部读得出来。通道选择依据是对 Claude Code 2.1.273 和 Codex 0.154.0 的零额度实测（前者每个工具的 description 留 2048 码元、认 prompts；后者不截、只调 `tools/list`）。**真机验过了**（2026-09-20，[记录](research/real-machine-p36-p44-2026-09-20.md)）：给这个仓库临时放一个写着三步门禁的 `SKILL.md`，只对模型说"跑一遍这个项目的提交门禁"，它跑出来的三步内容和顺序与 skill 一字不差（其中 `scripts/check_plan.py` 是本项目专有，不读 skill 猜不出来）——**模型会因为目录里的描述主动去加载 skill**。**没验的**：Claude Code 里 `/mcp__ccnm__<名字>` 的交互没有人工点过；Codex 在 Code Mode 下看到的是嵌套注册表里的工具，`load_skill` 的 description 在那条路径上有没有完整到模型面前没有单独量。**故意不做的**：`` !`命令` `` 自动执行、`allowed-tools` / `hooks` 等 frontmatter、执行账号 HOME 下的用户级 skills、MCP 官方 skills 扩展（SEP-2640，另一个阶段）。 |
| `search_text` 的输出模式 / 跨行 / 文件类型 / dotfile，`apply_patch` 的 `write`，`exec_command` 的 `shell` | 支持，**只有离线证据** | 对齐 Claude Code 2.1.273 的 Grep、Write、Bash，三种入口共用一个实现（[使用说明](usage.md#当前模型能做什么)、[协议第 5.2 节](protocol/remote-workspace-mcp-v1.md#52-搜索模式整文件覆盖一行-shellp37-新增)、[P37 记录](research/p37-execution-surface-batch1-2026-09-17.md)）。**验过的**：rg 15.2.0 上三种输出模式、跨行匹配、`type` 过滤（和 `glob` 同给时取交集）、dotfile 只在 `include_hidden` 时搜且 `.git` 永远不搜、给了 `glob` 也不搜 `.gitignore` 排除的文件（P38，[记录](research/p38-glob-gitignore-2026-09-17.md)）且 glob 的含义在旧实现和新实现上由同一组回归测试证明一致；`write` 的版本检查、不新建文件、失败回滚和权限保留；`shell` 用 bash 跑、和 `cmd` 二选一、开了 `exec_sandbox` 时被包起来的就是 `bash -c` 那条 argv（假沙箱），另在本机**真实 Codex 0.154.0 沙箱**里跑通一次带管道和重定向的 `shell`——Rust 单元与集成测试，加一个不 import ccnm 代码的中立 MCP 客户端对真实二进制跑，都只在 macOS arm64 上。**`shell` 真机验过了**（2026-09-20，[记录](research/real-machine-p36-p44-2026-09-20.md)）：真实 Claude Code 自己拼了 `bash -c export PATH=...; cargo test --workspace 2>&1; echo "..."` 这么一条——带 `;` 和重定向，`cmd` 那种 argv 数组拼不出来。**没验的**：`search_text` 的 `output_mode` / `multiline` / `type` 那次任务没引出来；Linux 上只有中立客户端证据（见[下面那节](#linux-上验到哪一步)）；更老或更新的 rg 版本。**代价**：`src/**` 这种文件名部分是 `**` 的 glob 没法交给 rg 缩小范围，扫得比以前多（协议第 5.2 节末尾）。 |
| `view_image`（看 workspace 里的图片） | 支持，**只有离线证据** | 三种入口共用一个实现（[使用说明](usage.md#当前模型能做什么)、[协议第 5.3 节](protocol/remote-workspace-mcp-v1.md#53-view_image看-workspace-里的图片p39-新增)、[P39 记录](research/p39-view-image-2026-09-17.md)）。**验过的**：按文件头认四种格式、上限与超限提示、SVG 与其他格式的错误、读路径策略、`read_file` 指向 `view_image`——Rust 单元与集成测试，加中立 MCP 客户端对真实二进制跑；**真实 Codex 0.154.0 连真实 ccnm**（外部 MCP read 模式、本机假模型、零额度），图片以 `input_image` 进了模型请求且逐字节一致；Code Mode 下模型脚本调 `image()` 后同样生效（用探针 server 测的）。Claude Code 2.1.273 怎么转换、缩放 MCP 图片来自读它的打包代码。**真机验过了**（2026-09-20，[记录](research/real-machine-p36-p44-2026-09-20.md)）：真实 Claude Code 2.1.272 经双机 SSH 链路看了一张 48×48 的三色横杠图，答出"上红、中绿、下蓝，126 字节"，和夹具逐条对得上——**图确实到了模型面前**，而且那次会话 `Read` 被 deny，它没有第二条路。**没验的**：模型看更复杂图片的效果；Code Mode 下模型会不会自己想到调 `image()`；Linux 上只有中立客户端证据（见[下面那节](#linux-上验到哪一步)）；GIF 动图只发文件本身，Host 怎么处理没测。 |
| Jupyter notebook（`read_notebook` 工具、`apply_patch` 的 `edit_notebook`） | 支持，**只有离线证据** | 三种入口共用一个实现（[使用说明](usage.md#当前模型能做什么)、[协议第 5.4 节](protocol/remote-workspace-mcp-v1.md#54-read_notebook-与-edit_notebookjupyter-notebook-按-cell-读写p40-新增)、[P40 记录](research/p40-notebook-2026-09-17.md)）。**验过的**：cell 与输出渲染、输出图片按位置作为图片块、traceback 去颜色码、按 cell 边界分页、超大 cell / 输出截断、非 notebook 与 nbformat 3 报错；replace / insert / delete、改类型、无 id 的老格式、按顺序应用、版本检查、和其他文件一起原子提交；notebook 读进来原样写出逐字节一致——Rust 单元测试加中立 MCP 客户端对真实二进制跑；另用 nbformat 5.11.1 手工核对（临时 venv，脚本 `tests/fixtures/notebook/check_with_nbformat.py`）：样例是 nbformat 自己的写法，经真实 ccnm 做五项编辑后的文件通过 nbformat schema 校验、nbformat 重写后逐字节一致。编辑语义对照的是 Claude Code 2.1.273 打包代码里的 NotebookEdit。**读那一半真机验过了**（2026-09-20，[记录](research/real-machine-p36-p44-2026-09-20.md)）：真实 Claude Code 答出了夹具 notebook 的第一个 cell 是 markdown 不是代码、真正计算的是第二个、以及**存储的输出** `rows: 3` / `columns: 2`——`read_file` 拿到的是一坨 JSON 文本，答不出这个形状，而那次会话 `Read` 被 deny。**没验的**：`edit_notebook` 那一半（那次任务只读不写）；在 Jupyter / VS Code 的界面里打开编辑后的文件；Linux 上只有中立客户端证据（见[下面那节](#linux-上验到哪一步)）；元数据浮点数的写法和 Python 不同（`1e-5` / `1e-05`）。**故意不做的**：执行 cell、渲染 HTML / LaTeX / SVG 输出、nbformat 3。 |
| 后台命令（`exec_command` 的 `run_in_background`、`read_output` 的 `wait_ms`、`stop_command` 工具）；取消调用或断开时停掉命令 | 支持，**只有离线证据** | 三种入口共用一个实现（[使用说明](usage.md#当前模型能做什么)、[协议第 5.5 节](protocol/remote-workspace-mcp-v1.md#55-后台命令run_in_backgroundwait_msstop_commandp41-新增)、[P41 记录](research/p41-background-commands-2026-09-18.md)）。**验过的**：后台起了马上返回、边跑边读、`wait_ms` 在命令结束时提前返回、`stop_command` 先 TERM 后 KILL 整个进程组、到 `timeout_ms` 被杀、同时最多 8 个；客户端发 `notifications/cancelled` 后命令被停掉（之前照跑到结束或超时）；连接结束时 server 先停掉所有命令再放写锁退出（之前要等命令自己跑完，8 秒的命令让 server 多占 8.0 秒写锁）；跑它的 server 被强杀后状态读作"不知道怎么结束的"——Rust 单元与集成测试（进程相关的在 `--test-threads=64` 下也跑过），加中立 MCP 客户端对真实二进制跑，都只在 macOS arm64 上。两个 Host 的后台语义和等待时长：Codex 0.154.0 用本机假模型实测，Claude Code 2.1.273 读打包代码。P42 补了三条此前没有护栏的语义（[记录](research/p42-lifecycle-contract-2026-09-20.md)、[协议第 6 节](protocol/remote-workspace-mcp-v1.md#6-连接生命周期)）：取消一次带 `wait_ms` 的 `read_output` 只停等待、命令照跑；同一个 ref 连停两次照实报状态；断线后同名会话重连，旧 `output_ref` 在 `read_output` 和 `stop_command` 上都报 `CCNM_E_INVALID_ARGS`。**和 hub 的组合也跑过了**（真实 gld + 真实 ccnm 二进制，测试在 gld 仓 `crates/core/tests/ccnm_background_lifecycle.rs`，`85bda77`）：调用方的单次调用预算一到就丢连接、ccnm 随即停掉这个会话的所有后台命令这条因果链成立；调用在预算内时后台命令不受影响，会话正常结束时才停。查出来的三条都修在 gld 那边（前台命令期限超过它的调用预算、空闲回收看不见在跑的后台命令、连接关得太急导致 ccnm 被强杀后在 Runtime 上留孤儿进程），**ccnm 这边一个字节没改**。**试过一次真机，但没拿到证据**（2026-09-20，[记录](research/real-machine-p36-p44-2026-09-20.md)）：prompt 和项目 skill 都写了"这一步要跑一两分钟，别干等在那儿"，模型跑的 `cargo test --workspace` 花了 81.3 秒——**而前台上限是 120 秒，前台跑得完**；`--print` 模式下 Claude Code 不留 JSONL，进程树上前台和后台命令又长得一样，所以**用没用后台看不出来**，不是"没用"。要拿到这条得给 `mcp-serve` 开 `CCNM_LOG=debug`，或者换交互模式。**没验的**：Claude Code 里按 Esc 实际发出的取消通知（`--print` 里没有 Esc）；Linux 上只有中立客户端证据（见[下面那节](#linux-上验到哪一步)）；`wait_ms` 等满 10 分钟时两个 Host 会不会先超时（Codex 只测到 75 秒）；真实 Host 或真实模型经 hub 用后台命令（上面那组组合测试的客户端是 gld 自己的连接层，不是 Claude Code / Codex，也没花模型额度）。P43 修了收尾时的一处写权缺口（[记录](research/p43-guard-recovery-2026-09-20.md)、[协议第 7 节](protocol/remote-workspace-mcp-v1.md#7-busy-和-unknown-怎么表达)）：会话结束时有命令**停不掉**（离开进程组又攥着管道，信号够不着）时，写入互斥不再被标成可用——之前这种情况下会放锁，下一个 coding 会话就和那个还在跑的东西并排写同一棵树。故障注入实测（真实二进制 + 中立客户端）：现在 marker 留成 `held` 加一行 `abandoned`，下一个会话被拒并看到还剩哪个 `output_ref`；`ccnm status` 也分得清"故意留着的"和"异常退出留下的"。**已知限制**：后台命令活不过会话，`/mcp` 重连也会停掉它们；经过 hub 时它自己的调用预算通常比 Runtime 这边的期限先到；命令结束时不会通知模型，要模型自己用 `read_output` 问；`mcp-serve` 被 `SIGKILL` 时它起的进程组没人收（前台命令也一样）；**一个脱离了进程组、但没有攥着管道的后代，ccnm 发现不了**——`stop_all` 会正常返回，锁照常交出去，要堵住它得上进程容器（cgroup / job object / supervisor），这一版没做；不能给命令喂 stdin。 |
| 服务端的参数校验（未知字段、超界值、枚举与类型） | 支持，**只有离线证据** | 三种入口共用一个实现（[协议第 5.6 节](protocol/remote-workspace-mcp-v1.md#56-参数怎么验有副作用的拒绝只读的说一声p44-新增)、[P44 记录](research/p44-input-validation-2026-09-20.md)）。**验过的**：`exec_command`、`apply_patch`、`stop_command` 连同 `files[]` 的嵌套结构拒绝它们没声明的字段（列出合法字段名，且在任何东西落盘之前拒）；超上限的 `timeout_ms` / `preview_bytes` 是拒不是钳，并指路 `run_in_background` / `read_output`；只读七个照常回答但结果里写明忽略了什么；`read_output` 的 `wait_ms` 超界仍钳但说一声；`tools/list` 里每个工具的 `additionalProperties` 与服务端真实解析一致（`workspace_info` 没有参数结构，不发这个键）——Rust 单元与集成测试，加一个不 import ccnm 代码的中立 MCP 客户端对真实二进制跑，都只在 macOS arm64 上。不认识的枚举值、类型不对、必填缺失一直都是拒绝，这一轮只是补了证据。所有拒绝都是 `isError` 工具结果，不作废 coding 句柄。**真机跑过一次**（2026-09-20，[记录](research/real-machine-p36-p44-2026-09-20.md)）：真实 Claude Code 2.1.272 经双机链路做完一个三小问的任务，20 轮工具调用、`0 permission denials`，**没有一次 unknown field 报错**——这一批收紧没有咬到它。**边界**：一次会话、一个任务、一个 Host，覆盖不了所有调用形状；Codex 当 Host 没试过；Linux 上只有中立客户端证据（见[下面那节](#linux-上验到哪一步)）。**边界**：只读工具不拒绝未知字段（有意如此，一个多余字段不该让一次读失败）；没有 MCP 层的能力协商扩展，客户端要知道服务端收什么，读 `tools/list` 的 schema。 |
| 受管会话里 Agent 自带的功能（workspace 的 `agent_tools`：`web_search` 默认开，`web_fetch` / `subagents` / `tasks` 可选） | 支持，**只有零额度证据** | 只管远端受管会话；Claude Code 的文件、shell、notebook、skill 工具永远关（[配置说明](configuration.md#agent_tools)、[P46 记录](research/p46-agent-tools-2026-09-22.md)）。**验过的**（本机假模型接口，不花额度）：Claude Code 2.1.278 上 `--tools` 认这些名字、模型拿到的恰好是它们加 ccnm 的工具；子代理的工具表和主会话一样；print 模式下开了的工具不进允许表会被自动拒绝（所以 ccnm 写进去）；`ToolSearch` 进白名单会把 ccnm 的工具推进延迟加载池（所以永远不写）。Codex 0.154.0 上 `web_search = "cached"` 给指定了 `gpt-5.1-codex` 的请求加上托管搜索工具，Code Mode 不排除它。**没验的**：真实模型会不会用、真实搜索和抓取的结果；Codex **不写 `model` 时搜索开了也看不到效果**（默认模型下三种取值的请求一字不差，原因没查明）；Codex 的子代理（仍关，没量过它会不会继承关掉的 feature）。 |
| 按名字再关掉工具、给项目 skill 分四档（Runtime 执行账号的 `~/.agents/mcp.json`，gld 读同一个文件） | 支持，**只有离线证据** | 三种入口共用一个实现（[配置说明](configuration.md#agentsmcpjson再关掉一些工具和-skills)、[P47 记录](research/p47-agents-exposure-2026-09-22.md)）。**验过的**：真实二进制上 `tools/list` 少掉被关的工具、按名字硬调被拒并写明规则、`off` 的 skill 从目录和 prompts 里一起消失、文件写坏时会话拒绝打开并指出位置；单元测试覆盖四档在目录 / 列表 / `load_skill` / prompts 上的效果、`on` 放不开 frontmatter 的 `disable-model-invocation`、doctor 的"暴露规则"一行（执行账号不是自己时跳过）。**没验的**：真实 Host 缓存了旧工具表时怎么显示被拒；Linux 上没跑。**它不是对模型的约束**：开着 `exec_command` 的会话能改执行账号 HOME 下的这个文件。 |
| Machine API（`ccnm rpc`），`print` 模式 | 支持，协议已冻结 | `ccnm.machine/1` 于 2026-09-10 冻结。两个 provider 各跑通一次真机双机闭环并与人类 CLI 对照（[Claude](research/p7-real-machine-2026-09-10.md)、[Codex](research/p7-codex-parity-2026-09-10.md)）：两条腿产物属主相同，`usage` 端到端到达调用方（Codex 不报 `cost`，永远缺席）。实现仍比契约少四条，见[协议说明](protocol/README.md)。 |
| Machine API 的 `interactive` 模式、输出分页、结果过期 | 未实现 | 都不在 `hello` 声明的能力里，调用会被明确拒绝，不静默降级。 |
| Remote Workspace MCP（`ccnm mcp bridge`） | 支持，契约已冻结 | `ccnm.workspace-mcp/1` 于 2026-09-11 冻结。允许矩阵在 macOS→macOS 上验过（[记录](research/p11-real-host-2026-09-11.md)、[证据](research/p11-matrix-20260911.json)）；远端真实项目 dogfood 在 **Debian 13 / x86_64 的 Linux Runtime** 上验过（[记录](research/p12-real-project-2026-09-11.md)、[证据](research/p12-dogfood-20260911.json)）：专用执行身份（uid 1002、只有自己的组、无 sudo、docker socket 不可写、`~/.ssh` 无私钥、无 `SSH_AUTH_SOCK`、读不到别人的 home），`cargo`/`rustc`/`node`/`npm`/`git` 由 `exec_command` 在那台机器上答出版本，read→search→patch→构建失败→`read_output` 分页→收回→**Runtime 侧 `git status` 为空**→测试通过，整套 Rust 测试在 Runtime 上绿。六条失败路径各有具名拒绝：writer busy、没 opt-in、read 请求 coding 不降级、协议号不认识（`CCNM_E_VERSION`，stdout 不说话）、远端进程被杀、Host 被杀。真实 Claude Code 2.1.268 在那棵远端树上完成了一次真改动（改文案、在远端跑测试、自己做了一个 commit），属主是执行身份。跨入口部分另有离线证明（[记录](research/cross-entry-p11-2026-09-11.md)）：受管会话与外部 coding 抢同一把 write guard，两个方向都是启动失败而不是"连上了写不进去"；一个不 import ccnm 代码的中立 MCP 客户端重放同一套矩阵得出同一结论。**边界**：只验过 Debian 13 / x86_64 与 macOS 两种 Runtime、一个 Host（Claude Code 2.1.268 的 `-p` 模式）、一棵中型 Rust 项目；Codex 当 Host、交互式 UI、arm64 Linux、非 Debian 系、monorepo 规模都没验。**egress 不作保证**（见下节）。已知代价：bridge 启动失败时 Claude Code 只显示 `Connection closed`，`CCNM_E_*` 诊断到不了用户面前，要手工跑一遍命令才看得到（见[出错了怎么办](troubleshooting.md)）。**写入互斥的作用范围是一个 state 目录**：同一棵工作树，两个 ccnm 进程各用一个 `XDG_STATE_HOME` 时就是两把互不相干的锁，两个 coding 会话能同时写（2026-09-20 实测，[P43 记录](research/p43-guard-recovery-2026-09-20.md)）。这是设计边界，不是缺陷——ccnm 不往工作树里放状态、也不占系统级固定路径；服务同一棵树的进程必须共用一个 state 目录，两个系统用户各跑各的也踩这条。 |
| Claude legacy colocated | 明确拒绝 | remote-only 启动参数已从 native 候选命令移除，但 installed Claude 尚未真实验收；本 build 在创建 session 前返回 `CCNM_E_NOT_READY`。 |
| Claude/Codex Agent Instance colocated | 明确拒绝 | 没有可信 Runtime credential boundary 和真实验收，不自动降级为 legacy/native。 |
| Codex legacy/internal protocol 2 | 兼容历史 fixture | 只用于保留已有内部测量与回归，不是新的公共配置入口。 |
| `hybrid-smb`、第三 Provider、Browser/Git 专用 MCP、多 Agent/worktree 编排 | 未实现 | 不在当前范围内，不做隐式 fallback。 |

平台要分两件事说，因为 P12 之后它们不再是同一个答案。

**Agent 那一侧只有 macOS。** Controller 是 launchd LaunchAgent，会话上下文检查直接问 `launchctl` 和 `security`；Linux Controller、Windows 和其他官方 CLI 版本均未验收。CI 有一个 Linux job，但它是 **Runtime 门禁**：绿的意思是代码在 Linux 上编得过、测试过得去，不是说 Agent 那一半在那儿能跑。

**Runtime 那一侧（`internal mcp-serve` 与七工具）在 Debian 13 / x86_64 上验过一次**，作为 Remote Workspace MCP 的 dogfood（[记录](research/p12-real-project-2026-09-11.md)）：`cargo fmt`、严格 clippy 与全套测试在那台机器上 676 passed / 0 failed，与 macOS 同数。**那一次是从两个红开始的**，两个都真修了：`SystemRunner::run` 的超时只杀 leader（macOS 因为 bash 会 exec 而一直看不见），以及一个测试助手用了 BSD 语义的 `date -r`。Linux 上的 Managed 入口（Controller/session）仍未验收，Runtime 侧也只验过这一种发行版和架构。

**第三个红是发布前收口那一轮、由 CI 自己照出来的，它证明 P12 那次只修了一半。** 杀进程组走的是 `kill -KILL -<pgid>`，而 GNU/procps 的 `kill` 把开头带减号的参数当成**信号**读：`-8421` 被读成信号号，命令最后一个 pid 都没有，**退出码 0，什么都没杀**。所以进程组还在、孙进程还占着管道、超时还是不超时——而且这次还报成功。macOS 的 BSD `kill` 两种写法都当进程组，本机永远看不见；Debian 13 上碰巧是绿的（孤儿被别的东西收走了），所以 P12 的真机轮也没照出来。**是 ubuntu-24.04 的 runner 照出来的**：`write_guard` 里那条"残留子进程要人工恢复"的测试红了，残留进程在本该杀掉它的 kill 之后还活着。修法是在负 pid 前加 `--`（macOS 和 Linux 都认），三处都改了。当前 HEAD 在 ubuntu-24.04 与 macOS 上都是 **681 passed / 0 failed**（当时的数字；当前 HEAD 是 688），严格 clippy 都干净。

**发布物也是两个，对应上面这两件事。** 一个版本出两个下载：

| 下载 | 装在哪 | 是什么 |
| --- | --- | --- |
| `ccnm-<版本>-macos-universal.tar.gz` | Agent Node 或 Runtime Node | arm64 + x86_64 通用二进制，两个角色都能跑 |
| `ccnm-<版本>-linux-x86_64.tar.gz` | **只能是 Runtime Node** | 只有 Runtime 那一半有证据；Agent 那一半是 launchd LaunchAgent，在 Linux 上不跑 |

Linux 那个在 `ubuntu-24.04` 上本机构建，**glibc 下限是从二进制里量出来的**（`objdump -T` 里最高的 `GLIBC_x.y`），写在 release notes 里；Debian 13 的 glibc 是 2.41。**arm64 Linux 没有发布物**，因为没有证据。Linux 侧的 CI 门禁（clippy + 全套测试 + 构建发布物）和 macOS 一样每次 push 都跑，但那个 job 绿只说明代码在 Linux 上编得过、测试过得去，**不说明 Agent 那一半支持 Linux**。见[开发与发布](development.md)。

**Runtime Node 的前置条件**（Managed 与 Remote MCP 两个入口都要）：`git`、**`ripgrep`**（`search_text` 调 `rg`，没有它七工具就少一个），加上项目自己需要的工具链。装什么、谁维护，见[运维手册](operations.md#runtime-node-的前置条件与项目工具链)。

## 机器上装的是哪个版本

**最新的 release 是 `v0.8.0`**（2026-09-20，tag 指向 `a5b6a3e`），四个产物齐全：macOS universal 和 linux-x86_64 各一个 tar.gz 加 sha256。

发版前先在自己的两台 macOS 上跑了一轮（`scripts/deploy.sh fodelf`，Runtime 与 Agent 装同一个构建），确认 P43 的写权语义和 P44 的参数校验这两处收紧不咬真实 Host，再推的 tag。

这一步换来的第一个真机事实：`ccnm doctor` 的远端 MCP 握手报 **11 个工具**（16137 B），之前是 7 个——P36–P41 加的 `load_skill`、`view_image`、`read_notebook`、`stop_command` 第一次经真实 SSH 链路握手并列出成功。**它只证明工具表到得了 Agent 那一侧，不证明模型会调它们**——那一步在同一天单跑了一轮，花了 $0.57，见[真实模型第一次用上这批工具](research/real-machine-p36-p44-2026-09-20.md)：`read_notebook`、`view_image`、`load_skill`、`exec_command` 的 `shell` 都被用上了，`run_in_background` 没拿到证据。

**版本号必须跟着代码走，不能只换二进制。** 双机握手按版本字符串比，两台都报 `0.7.0` 就会被判成一致而放行，然后一边有 P43/P44 的收紧、一边没有——比直接拒绝难查得多。

## Linux 上验到哪一步

P36–P44 的十一个工具，2026-09-20 之前**只在 macOS arm64 上跑过**。那天用 v0.8.0 的**发布产物**（CI 在 ubuntu-24.04 构建的 `linux-x86_64`，sha256 对过）在 **Debian 13 / x86_64** 上跑了一轮，身份是 P12 建的专用执行账号 `ccrun`（uid 1002，只在自己的组里，没有 sudo）：**194 passed / 1 skipped**，其中 P36–P44 的核心 36 条单独跑 11.0 秒全过。完整记录见 [P36–P44 第一次在 Linux 上跑](research/linux-runtime-2026-09-20.md)。

**这一轮验的是什么**：外部 MCP 入口，也就是 `internal mcp-serve` 加十一个工具的行为，由一个不 import 任何 ccnm 代码的中立客户端对真实二进制跑。产品代码一行没改。

**这一轮不验什么**：真实模型（中立客户端不花额度，也不说明模型会不会用）、Managed 入口（Controller 是 launchd LaunchAgent，Linux 上根本不跑）、Codex 当 Host、`exec_sandbox` 与原生链、`cargo test` 那套（这台机器没有 Rust 工具链）。**那台机器的 rg 是 14.1.1 而不是验 P37 时用的 15.2.0，但这一轮只证明测试在它上面也过，没有逐条比对两个版本的搜索语义。**

**顺带修了两个测试自己的毛病**（产品没问题）：`mktemp -d -t ccnm-p12` 是 BSD 写法，GNU 要求模板自带 6 个 `X`，报 `too few X's in template` 后 32 个用例一次都没跑起来——和 P12 那次的 `date -r` 是同一类；另一条用例的前提是"跑测试的账号在 admin/staff 里"，而 `ccrun` 没有 sudo、审计本来就该放行，现在遇到这种账号 skip 并说明。

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
| 连接半开：Agent 那头早断了，Runtime 的 sshd 没收到（典型是 Runtime 笔记本睡眠时断的） | 空闲 ≤30 秒后 server 主动 `ping`，写失败即正常结束，`released` | 过了这 30 秒直接能开；在这之前被拒成 busy。v0.6.0 及更早没有这个 ping，server 会一直占着锁（真机上占了 12 小时） |

两种情况下 `read` 会话都照常打开，而远端都没有留下孤儿进程。所以"Claude Code 崩了/被关掉"通常什么都不用做；要人动手的是 Runtime 侧的执行者被杀那一种。

## 三个逃生开关：接受了什么，以及绝不会变绿

默认形态是 Runtime 上一个专用低权限账号，读不到任何已知 Agent 凭据，而且每条命令执行前有人确认。三个 per-workspace 开关各偏离其中一件，都写在 **Runtime 自己的配置**里（承担风险的机器决定，调用方带不进来），**互不蕴含**：

| 开关 | 放弃的那个性质 | 典型场景 |
| --- | --- | --- |
| `allow_unconfined_exec` | 跑命令的账号是受限的（没 sudo/admin、名下没私钥……） | 还没建专用账号的临时项目 |
| `allow_unisolated_credentials` | 那个账号**读不到已知 Agent 登录**（含可达性"说不清"：symlink、列不出来） | 项目和 Claude 登录在同一个家目录：一台机器、一个账号 |
| `allow_unattended_exec` | **每条 `exec_command` 执行前有人确认** | 自己的机器、自己的项目，确认弹窗已经变成纯噪音 |

**前两个是授权，第三个不是。** `allow_unattended_exec` 只决定"要不要问人"，不改变任何一条命令**能做什么**——那由 `exec_gate` 和 Runtime 执行身份决定，没有任何开关能动它。它也完全不影响 `--print` 和 `ccnm mcp bridge`：那两条路上本来就不问（两边都没人在等）。

**接受之后那几行是 WARN，永远不会变成 OK。** 那个性质并没有成立，只是有人签字说能接受；`ccnm doctor` 会一直显示，并注明是哪个开关接受的。另外 ccnm 会在第一次用它起会话时在终端上把风险讲一次（只讲一次，marker 在 state 目录；关掉再打开算新决定，会再讲），每条命令结果里的 unconfined 说明也会写明。

**`allow_unisolated_credentials` 放开的是这个项目唯一那条硬边界**：模型跑的每一条命令都能读到那份登录，而让它跑一条命令只需要一句 prompt——包括从它被要求读的文件里冒出来的那一句。放开之后没有别的东西在挡着。代价的完整说明见[生产安全](production-safety.md#凭据隔离那一条怎么放开代价是什么)。

**三个都开的时候**：模型跑的任何命令都不经人确认、都能读到你的 Agent 登录、账号本身也没受限。这时候还站着的只有两样——那个账号自己的 OS 权限，和工具够不到 workspace 根目录外面这件事。

**两条任何开关都放不开**：执行身份未知（没人能说清是谁接受了什么），以及认证环境是继承来的（凭证被塞进每个子进程，而修法只是别 export）。

这一段描述的是当前代码的行为。**下面"P3 发布门禁结果"第 2 条那个"读不到任何已知 Agent 凭据"是那次真机验收的事实**，它验的是默认形态，不因为存在开关而失效。

## 界面语言：验到哪一步

0.6.0 起 ccnm 对人说话默认中文，`--lang en` / `CCNM_LANG` / `[ui] lang` 切英文。

**验过的**（2026-09-12，macOS 15 / Apple Silicon，Terminal.app + UTF-8 locale，Runtime 侧发起）：doctor 四个真实 workspace 的完整表在中英两种语言下逐行对照，detail 列对齐正确；`--help` 及子命令、`init`、`workspace add/list`、`status`、两段逃生开关风险警告，中英两版都实跑过；英文那版与 0.5.0 逐字一致。自动化那侧，702 个测试里 CLI 集成测试统一走 `CCNM_LANG=en`，另有专门钉住中文路径和"英文页面不含任何 CJK 字符"的用例。

**没验的，按可能咬人的顺序**：

- **非 UTF-8 locale 的终端**。中文会画成一串下划线或问号。ccnm 自己不设 `LANG`（那样会破坏它对 git/ssh/tmux 英文输出的匹配），所以这取决于你终端和 ssh 带过去的 locale。撞上了就 `--lang en`。
- **Linux Runtime 上的中文输出**。Debian 13 那台只验过英文；`ccnm doctor` 在 Runtime 侧本地渲染，理论上一样，但没跑过。
- **窄终端下的换行**。doctor 的表按 31 列缩进排，中文行名比英文短，实际更不容易折，但没在 80 列以下量过。

**不受语言影响、两种语言下一模一样**：`CCNM_E_*` 错误码与退出码、`ccnm rpc` 的全部 JSON、七个 MCP 工具名与参数、给模型的 MCP 文本、write-guard 锁文件内容，以及 `--- stdout` 这类被脚本解析的锚点。p7/p11/p12 三套真机脚本都只认这些，因此中文化不需要重跑真机轮。

**翻不动的**：clap 内置的 `Usage:` / `Options:` / `error:`（4.x 没有接口），参数写错时那句报错仍是英文。`Error` 消息这一版也没翻——它同时走终端、`ccnm.machine/1` 的 `error.message` 和 MCP 工具正文三条路。

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
