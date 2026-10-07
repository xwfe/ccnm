# 下一轮开发：执行可靠性、结果完整性与真实项目交付

制定日期：2026-09-28。核对基线：`main/623dbdf`；已跟踪文件无未提交修改，用户已有未跟踪文件 `ccnm-mobile-handoff.md` 保持原样。本文是后续实施方案，不是功能完成或部署记录。唯一进度为 [status.json](status.json)，稳定验收编号为 [ROADMAP.md](ROADMAP.md)。

## 1. 本轮目标与不做的事

**让普通终端中的 ccnm 能可靠启动、精确停止、完整取回结果，并在 Runtime 不确定或清理不完整时给出可操作的真实状态。** 先把当前 Claude/Codex + macOS Agent + macOS/Linux Runtime 的执行链收紧，再用 hpsrv 的真实项目验证，不再扩建入口。

保持 Managed Agent Runtime 与 Remote Workspace MCP 两入口共用 Runtime 安全与写互斥；CLI、Machine API、第三方 MCP 客户端不能各造一套事实。ccnm 管执行，不管需求、任务图、review/retry 决策、分支编排或发布批准。底层共用机制只有确有多个消费者时才抽取；不得为了这些阶段顺带重构整个仓库。

PocketShell 等外部工具仅提供 SSH/PTY 终端，不新增移动端、网页终端、专用适配器、ttyd/Serve、VPN 或隧道实现。P54–P56 已退役，不复用编号。既有 [终端接入决策](terminal-access.md) 保持有效。

本轮不实现 Linux Agent/Windows、交互式 Machine API、持久任务队列、跨断线后台服务、全局分布式锁、自动孤儿进程回收，也不增加 Provider 或重开封存的 Codex exec-server。它们的后续准入条件见第 7 节，不是被宣称已经解决。

## 2. 本次核查：事实与待复现问题分开

制定本表时只读了代码、契约和既有证据。**2026-09-28 P57 已对每一行跑了零额度探针**（真实二进制 + 假 Agent / 中立 MCP 客户端，未连远端、未跑模型），“现状”一栏按复现结果更新，方法与原始输出只写在 [P57 记录](../research/2026-09-28-p57-core-baseline.md)，这里不重复。仍标“静态待验证”的不能据此宣称已经发生事故。

| 标识 | 现状 / 判断等级 | 核查落点与后续处理 |
| --- | --- | --- |
| BASE-01 | **历史验收已完成**：P52 修复同组 relay 子进程收尾，P53 加入 CI 门禁 | [P52 证据](../research/2026-09-25-p52-relay-group-cleanup.md)、[P53 证据](../research/2026-09-25-p53-ci-gates.md)；不重做，不把真机临时构建当现网已部署 |
| CTRL-01 | **P57 复现，比原判断更重**：RPC 的 stop 不传任何会话身份；Agent 收到无 session 的 stop 只查 tmux，print 运行不在 tmux 里，所以**根本停不到**，同 workspace 有交互会话时改停那个并回报成功（[P57 记录](../research/2026-09-28-p57-core-baseline.md) A1、B）。**P58 已修，离线证据**：stop 按 Runtime 预先分配的 Agent 会话 id 精确送达（[P58 记录](../research/2026-09-28-p58-exact-session-control.md)） | [rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs) 的 `SystemRuns::stop`、`stop`；[launcher.rs](../../crates/ccnm-core/src/launcher.rs) 已有 `stop_selected(..., session)`；P57 复现，P58 收口 |
| CTRL-02 | **P57 复现**：stop 标志被运行线程的旧副本写回 false；完成先落盘时 stop 把 `completed` 改回 `stopping`、finish 丢失（A2、A3 确定性，A4 120 轮无一次两者都保住）。固定 `.tmp` 冲突未撞出，仍是静态待验证。**P58 已修，离线证据**：锁内读改写 + 唯一临时文件，A4 复验 40/40 | [rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs) 的 `spawn_run`、[rpc/store.rs](../../crates/ccnm-core/src/rpc/store.rs) 的 `write`、`write_atomically`；P58 修状态并发，不只换临时文件名 |
| CTRL-03 | **P57 复现**：不同 start_key 经 `safe_name` 合并（A5）；`../`、绝对路径、symlink 句柄读到 `sessions/` 以外的记录（A6）；运行中改绑后旧句柄的 stop 发往新机器（A7）；空键文件回 `uncertain` 但 `data.session` 为空串（A8）。**P58 已修，离线证据**：句柄先校验、键按原串比较、改绑后拒绝 | [rpc/store.rs](../../crates/ccnm-core/src/rpc/store.rs) 的 `session_path` / `key_path`、[rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs)；P57 用边界与配置漂移用例核对，P58 修实证缺口 |
| OUT-01 | **P57 复现并纠正**：源头截断在 Agent——RunReport 只带回 stdout 末尾 2 KiB（加 `...`），RPC 的 8 KiB 截断对真实运行从不触发；`bytes_total` 取这段尾巴的长度、`truncated` 报 false；`max_bytes` 被忽略、非法值也收；非空 cursor 回 expired（C0–C2、C4）。**P59 已修，离线证据**：Agent 生成冻结的脱敏视图（每流最后 32 MiB），RPC 拷到本机后倒序分页，`max_bytes` 生效，拿不到时降级并写明原因（[P59 记录](../research/2026-09-29-p59-output-snapshot.md)） | [rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs) 的 `result` / `finish_from`；P59 必须贯通源头，而非仅对尾部再分页 |
| OUT-02 | **P57 复现**：解析成功时最终回答完整走 `text`（无大小上限）、`output` 为空页；stderr 在 RPC 结果和记录里都没有（C0、C3、C5）。**P59 已修，离线证据**：`text` 语义不变，原始 stdout 与 stderr 各自可按流完整读回 | [work.rs](../../crates/ccnm-core/src/work.rs) 的 `run_print` / `result`；P59 分清最终回答、stdout/stderr、Runtime 工具输出 |
| AUTH-01 | **P57 复现**：同 workspace 第二个 start 回 `starting`、两次都派到 Agent，`-32008` 无任何生产路径返回；拒绝发生在 Runtime MCP 握手，同 workspace 与同 git common-dir 都拒（A1、D1、D2）。**P60 已修，离线证据**：`session.start` 先经 Agent 问 Runtime 执行账号的写锁，被占回 `-32008`、残留回 `-32007`+reason，问不到照旧启动；不是预留（[P60 记录](../research/2026-09-29-p60-write-guard-observation.md)） | [协议实现差距](../protocol/README.md)、[write_guard.rs](../../crates/ccnm-core/src/mcp/write_guard.rs)；P60 提供真实占用诊断和有限的提前拒绝，不伪造原子预留 |
| AUTH-02 | **已知边界，P57 复核行为如文档**：mcp-serve 被 SIGKILL 后后台命令仍在、marker 停在 held、下一个 writer 被拒；两个 state 域各自放行（D3、D4）。**P60 补了诊断**：Runtime 回答 free/held/abandoned/unknown，强杀后仍是 unknown、不因 pid 消失放行；恢复仍靠人工 | [jobs.rs](../../crates/ccnm-core/src/mcp/jobs.rs)、[write_guard.rs](../../crates/ccnm-core/src/mcp/write_guard.rs)、[运维](../operations.md)；P60 诊断而非承诺完整自动回收 |
| CLEAN-01 | **P57 复现（同用户两目录的路由测试）**：本机只删调用者 state 下的 `sessions/<id>`，Executor state 的输出、RPC 记录与 start_key 不动，workspace 配置照删；purge 请求走协议 1、不带实例身份（E）。**P61 已修，离线证据**：`ccnm cleanup` 先预览，Agent / Runtime 执行账号 / Operator 各删各的（内部协议 10，经 Agent 转到执行账号），`--purge` 同一服务、有剩余就保留配置；Machine API 记录留墓碑（[P61 记录](../research/2026-09-30-p61-cleanup.md)） | [launcher.rs](../../crates/ccnm-core/src/launcher.rs) 的 `purge`、[retention.rs](../../crates/ccnm-core/src/mcp/retention.rs)、[生命周期](../project-lifecycle.md)；P61 跨身份预览和精确清理 |
| REAL-01 | **证据缺口**：P48/P49/P50 的各类零额度及真机记录覆盖不同范围；当前 Provider pin、发行包和实际安装不能互相替代 | [支持矩阵](../support-matrix.md)、[交接](../orchestrator-handoff.md)；P62 做具名组合验证，不用测试总数代替 |

## 3. 实施队列

| 阶段 | 目标与交付 | 实施文档 | 完成边界 |
| --- | --- | --- | --- |
| P57 | 当前构建基线、普通项目测试样例、缺口复现清单和真实环境验收准备 | [验证与交付](core-verification.md) | 离线准备完成；不得写成 hpsrv 真机通过 |
| P58 | 精确会话身份、原子状态更新、幂等键与路径边界、取消和 unknown 语义 | [会话控制](core-session-control.md) | 可确定重现的安全/并发回归通过，公共契约不倒退 |
| P59 | Agent 到 Machine API 的完整受限结果快照、分页和参考客户端 | [输出与清理](core-output-cleanup.md) | 不重复、不漏段、不越过字节预算；旧尾部不伪装完整 |
| P60 | Runtime 权威占用诊断、可证实的 busy 提前拒绝、残留恢复说明 | [会话控制](core-session-control.md) | 不抢锁、不假放锁、不把未知写者当可重试 busy |
| P61 | 两端/跨身份输出清理预览、确认、防竞态及结果失效语义 | [输出与清理](core-output-cleanup.md) | 不删除项目、活会话、凭据或幂等身份；部分失败可恢复 |
| P62 | 双 Provider、两入口在 hpsrv 的真实项目闭环与候选包核验 | [验证与交付](core-verification.md) | 具名真机证据齐全；实际发布仍需单独授权 |
| P63 | P62 真机查出的 F14/F16/F17：Machine API 断连后任务照跑、停止标志不丢、ssh 认证前失败记 failed | [P62 记录](../research/2026-09-30-p62-real-machine.md) 第 5 节 | 先红后绿、契约不改；真机复验回到 P62 |
| P64 | P62 真机查出的 F4/F2：交互 stop 等通道退出再确认、被停止是独立结局且时长为真；doctor 与握手在版本号相同时再比内部协议最高号 | [P62 记录](../research/2026-09-30-p62-real-machine.md) 第 5 节 | 先红后绿、契约不改、不升版本号；真机复验回到 P62 |
| P65 | P62 真机查出的 F1/F3/F5：Operator 没权限看项目根时不再报"不存在"；Machine API 给出会话没起来的原因；doctor 写明 Codex 登录只看了本地 | [P62 记录](../research/2026-09-30-p62-real-machine.md) 第 5 节 | 先红后绿、契约只做加法、不加联网探测；真机复验回到 P62 |
| P66 | P62 真机查出的八条低影响项（F6–F12、F18）：单项目 status 提示别的实例、Controller 安装带上环境、探测被拒带回原因、帮助中文、节点名；三条文档项核对补缺 | [P62 记录](../research/2026-09-30-p62-real-machine.md) 第 5 节 | 先红后绿、只加可选字段、不写官方 CLI 配置、不动本机 LaunchAgent；真机复验回到 P62 |
| P67 | `apply_patch` 不删别的 patch 正在写的日志（`.json.tmp`）：修 P34 日志锁时验证出的、同账号多个 workspace 并发 patch 会无故失败一次 | [日志探测改共享锁的记录](../research/2026-10-01-patch-journal-probe-shared-lock.md) 第 4 节 | 先红后绿；不改日志名称、格式与报错，不动 `.json` 的中断判断 |
| P68 | P62 续跑查出的 F22/F23：Agent 上的监督进程丢了几秒内按 `unknown` 收尾，不等满超时；启动过的会话原始输出丢了如实降级，不当成空 | [P62 续跑记录](../research/2026-10-04-p62-resume-release.md) 第 6.1、6.2 节 | 先红后绿、不改线格式与协议号；真机复验回到 P62 |
| P69 | P62 续跑查出的 F20/F21/F24：doctor 对旧 Agent 先报版本；`Command approval` 对 Codex 说实话（不问）；`workspace add` 按配置里的节点名写，好几个候选时要 `--agent-node` | [P62 续跑记录](../research/2026-10-04-p62-resume-release.md) 第 7 节 | 先红后绿、不改线格式与协议号、不给 Codex 加审批；真机复验回到 P62 |
| P70 | P62.4 复验查出的 F25/F26：Agent 是别的构建时 `Reverse SSH` 行不再把转述丢的字段算到 Runtime 头上；版本行写实际节点名 | [P62.4 复验记录](../research/2026-10-04-p62-4-recheck.md) 第 6 节 | 先红后绿、只改 Operator 本机的判定与措辞 |
| P71 | F21：受管 Codex 交互会话执行命令前问人——Runtime 标了"要人确认"的工具，Agent 启动 Codex 时设 `approval_mode="prompt"` | [P62 续跑记录](../research/2026-10-04-p62-resume-release.md) 第 7 节 | 先零额度实测 Codex 0.154.0；print 与 exec-server 链不变 |

按 `P53 → P57 → P58 → P59 → P60 → P61 → P62 → P63 → P64 → P65 → P66 → P67 → P68 → P69 → P70 → P71` 串行接续，每轮默认一个阶段；P62 受阻时先做 P63–P69（修它和同期查出的缺陷），之后回到 P62 续跑。P57–P61 的产品验收以离线真实二进制和 CI 为主，远端部署/模型验收集中在 P62；这不是用离线测试替代真机，而是避免每个小改动都要求部署和付费。若发现只能在真机证明的关键正确性问题，仍须具名记录阻塞，不得提前勾选。

P57 复现结果与本计划的静态判断相反时，先更新该缺口的依据与范围，再进入对应实现；不要为兑现计划而制造无必要改动。尚未实现的阶段需要调整时，依既有计划约定修改，不把“没轮到”写成 blocked。

## 4. 跨阶段技术契约

**一份身份、一份写权事实。** RPC 的 `s-*` 句柄、ccnm 受管 session ID、Provider thread ID、Runtime 会话标记分属不同层，必须显式映射。所有控制动作校验原 workspace/Agent identity；配置变更不能把旧句柄转向新机器或最新会话。Runtime 仍是写权唯一权威，同一 canonical Git common-dir 的不同 worktree 不能借机并行。

**进程结束、可交权、业务通过是不同结论。** `completed` 不等于项目验收通过；未知清理或不明副作用不能自动重试。终态、错误 effect 与冻结协议保持一致；确需改变既有公共语义时先做版本设计，不能靠改 fixture 掩盖。内部 wire 如因新请求变动需升级，双端拒绝不认识的版本，不静默回退到弱路径。

**最小实现优先。** 优先拆清 `rpc/session.rs`、`rpc/store.rs`、`launcher/work/protocol` 的责任，不建第二个 Controller、网络 RPC 网关、数据库服务或永久调度器。无明确消费者不抽通用 framework；共享机制跨 toexec 修改必须另查该仓指令、单独提交，不默认有跨仓发布授权。

**不迁移凭据。** Operator 发控制命令，Agent 持官方登录，Runtime Executor 入站执行；ccrun 不持主动回连 key，不通过 sudo 扩大清理权限。日志和诊断不读认证文件正文，不输出 token、授权头或整个环境。外部终端能否连接与 ccnm 正确性分别记录。

## 5. 交付与测试纪律

每个实施阶段提交对应代码、确定性回归、更新后的用户文档与 `docs/research/` 验证记录；在 `status.json` 中逐判据写证据。新源文件/命令名若在分方案标为“拟新增”，当前不能写进 README 已支持列表。用户文档只在行为真正存在后更新。

基础门禁遵循 [AGENTS.md](../../AGENTS.md)：Rust 格式、严格 clippy、全量 Rust；计划/协议/中立 Python 用 `python3 -B scripts/ci_gates.py` 统一跑，脚本自行构建真实 CLI，禁止依赖旧 target 或默许 skip。具体受影响测试在分方案列出；进程相关改动加 64 线程压力路径。历史证据不改日期、不重录 golden 伪装等价。

没有证据不得把 P57–P62 记为完成。本轮仅生成计划，全部 pending，`current_task=P57`。参考路径中的新测试/研究记录在实现时创建，不预填不存在的 evidence。

## 6. 下一位实施者从哪里开始

```text
先读 AGENTS.md、docs/plan/README.md、status.json、ROADMAP 当前阶段。
核对 HEAD、git status 和两种 diff；不动用户已有 ccnm-mobile-handoff.md。
阅读 core-hardening.md 与当前阶段的分方案。本轮先做 P57，不连续实现整条队列。
P57 只建立当前构建基线和无真实模型的缺口复现；不要重做 P52/P53。
静态风险必须先复现并记录，失败探针不伪装成产品测试已通过。
缺远端/系统/凭据配置/付费模型/部署授权时保留离线成果并具名记录，不能继承旧授权。
完成一个阶段后更新 status/evidence、验证和逻辑提交，不自动 push、发版或修改其他项目。
```

## 7. 候选后续方向，不在本轮队列

| 方向 | 何时才应另立任务 | 本轮约束 |
| --- | --- | --- |
| 官方 CLI 版本升级 | 当前 pin 无法日用或新版本有必要能力；先核对官方文档与真实能力探针 | 不按最新版本号直接放宽 allowlist；拒绝伪造 provider/参数/fixture |
| Linux Agent | 明确需要常驻 Linux Agent，且先设计 Controller、登录上下文、监督和身份隔离的 Linux RFC | hpsrv Runtime 不因此被描述成 Linux Agent；不将 launchd 简单替换成 systemd 就算完成 |
| Windows | 完成路径、ACL、进程树、SSH、PTY 和打包 RFC 后，先限定 Runtime 或 Agent 的明确范围 | 三端目标保留；本轮不把 POSIX 测试当 Windows 支持 |
| 交互式 Machine API | 存在明确消费者需要输入、审批、事件流与重连语义 | PocketShell 的终端接入不是理由；不为填满契约而实现 |
| 强杀/脱组后代自动回收 | OS 监督机制能证明归属与清理，且已具备可复验故障模型 | 当前坚持 fail-closed 和人工处理；不靠 PID 扫描/超时清锁 |
| 浏览器、Git/CI 或更多工具 | 真实项目验证表明现有命令/MCP 接口确有不足 | 优先组合外部工具，不扩大成通用 IDE/编排/发布平台 |

这些方向的编号在真正排期时从 P72 起分配；未排期不写成虚假 pending 阶段，也不宣称已支持。
