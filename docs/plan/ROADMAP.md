# ccnm 实施路线

本文是后续实施契约，不是现有能力宣传。实时进度只见 [status.json](status.json)，执行与交接规则见 [README.md](README.md)。`P*.N` 为稳定验收编号，删除或调整须同时更新状态及变更原因。

## 一、两个项目的边界

**ccnm 管执行机制：在哪运行哪个 Agent，如何启动、隔离、观察和停止。独立 Orchestrator 管协作策略：谁做什么、顺序、验收、重试和分支合并。**

```text
人类 CLI / 外部程序 / 后续控制 MCP
                  │
             ccnm 应用层
                  │
Agent Instance → Provider → Controller / session
                  │
         SSH stdio → Runtime workspace

独立 Orchestrator → ExecutionBackend → ccnm Machine API
                                    → 其他明确实现的 backend
```

ccnm 不需要安装 Orchestrator 也能独立使用。Orchestrator 核心不链接 `ccnm-core`，通过自己的窄 `ExecutionBackend` 接口接 ccnm；后续允许其他 backend，不能把“两个产品独立”做成“另一个仓库里的 ccnm 前端”。**选用 ccnm backend 时，不绕过它另做 SSH、进程启动或 workspace 写入。**暂不为了这个接口做动态插件平台。

现有 Runtime MCP 提供项目七工具；未来控制 MCP 提供 Agent/session 操作，属于不同权限面。CLI、Machine API 和可选控制 MCP 共用 ccnm 应用逻辑，不解析人类终端文案，也不直接公开 `internal` payload。本地 stdio 可先供程序消费；ChatGPT 的远程接入、认证和部署另立集成阶段，不能把本地 stdio 写成已经可直接连接云端客户端。

### 不可漂移的原则

- Node 是机器标识，Agent Instance 是 provider + node + Agent-local profile 的运行身份。workspace 的 root 只由持有项目的一侧权威解析；调用方不能覆盖成任意路径。
- 凭据边界落实到**进程身份、文件权限和传输**，不是“某台物理机器永远不能装 AI”。一台机器可承担多角色，但执行进程不应因此读到 Agent 登录状态。native colocated 是显式受信任本地执行形态，不能冒充隔离 Runtime。
- 官方 CLI 自己认证，ccnm 不提取、复制、代理或返回订阅凭据；目录路径不是密钥，但私有 profile 路径仍不下发 Runtime。不能保证官方 CLI 包装就自动满足所有订阅、共享或再分发规则；面向他人开放前另行核实。
- Provider 负责声明经过验证的需求；通用安全层统一落实。Runtime 检查**执行身份可接触的全部已知 Agent 凭据**，不能因当前选 Claude 就忽略 Codex 凭据。不得把检查范围夸大为已证明机器上不存在任何秘密。
- 环境清理区分 Agent→SSH 的认证环境与 Runtime 自己的项目环境。前者不转发 Agent 私密状态，后者只允许显式授权的项目变量；不把 `OPENAI_*` / `GOOGLE_*` 一刀切当成永远正确的通用策略，也不因此放宽现有 Codex 隔离。来源不明的凭据 fail-closed。
- `ccrun`/ACL、无 sudo/admin、特权 socket、凭据隔离和网络策略共同约束执行。诊断不能代替 OS 策略；egress 元数据不是防火墙，命令解析器不是 sandbox。
- Orchestrator 选择写入者，**ccnm Runtime 执行拒绝/互斥机制**。只在 Orchestrator 记一个 lease，不能拦住另一 CLI；只锁 `apply_patch` 也挡不住 `exec_command`。读者不开放任意 exec，除非有独立验证的只读隔离。
- 一台 Runtime 上多个 alias/Agent/Controller 不能绕过同一工作树的写入互斥。进程仍存活时不能因超时到期就将写权限交给下一人；不宣称对任意 shell 获得 exactly-once 或原子回滚。
- 合盖后能继续工作的前提是 Agent **和项目所在 Runtime** 仍在线。项目只在睡眠笔记本上时，不承诺云端继续读写；本轮不做源码迁移、同步或离线缓存。
- 核心只消费 OpenSSH alias，不绑定网络产品。保持 Rust 内部 enum dispatch；只有真实需求证明必要才拆新 crate/插件 ABI。两产品通过协议独立发版，不绑定版本号。

## 二、顺序和基线

`P0 → P1 → P2 → P3 → P4 → P5 → P6 → P7 → P8 → P9 → P10 → P11 → P12 → P13 → P14 → P15 → P16 → P17 → P18 → P19 → P20 → P21 → P22 → P23 → P24`。默认每轮只执行一个阶段。P0–P8 是 ccnm v1 收口和独立 Orchestrator 的接口交接；P9–P12 是 ccnm v1.x 的 Remote Workspace MCP 扩展；P13 是按真实 Host 行为修正两个入口共用的 instructions 投影；P21–P24 是 Codex 原生执行链。完整边界见 [双执行入口方案](runtime-surfaces.md)。

### P0 — 已有内部验证基线

范围是已提交的内部能力，**不包括公开 Codex、生产隔离或全部 topology 已验证**。

- **P0.1** Claude Provider 等价抽取有提交和固定 fixture；不重录快照掩盖差异。
- **P0.2** Codex `0.153.4` 的 print、interactive、Controller/supervisor、tmux、真实 SSH MCP 七工具有可追溯证据。
- **P0.3** 历史离线门禁与未覆盖范围已记录：448 Rust、7 Python；引用 [内部接线记录](../research/codex-internal-wiring-2026-09-07.md)，不是本次重跑。

已知缺口必须保留：Claude colocated 的启动参数仍带 remote 工具限制，已有假 supervisor 测试不能证明可用；Codex colocated 未开放；旧 Runtime 拒绝 v2 时可能返回进程退出码 0，必须看协议结果。详见 [架构说明](../architecture.md)。这些由 P3/P4 定点处理，不能把 P0 完成解释为缺口已修复。

### P1 — Provider 安全契约收敛

**依赖 P0。**先写明安全契约，再集中散落的 Claude/Codex 特判。不改公开配置，不新增第三 Provider，不开始 RPC。

主要落点：`provider/`、`safety.rs`、`ssh.rs`、`mcp/exec.rs`、Controller/session 的 preflight。Provider 返回策略/需求，通用层执行；不要将所有 SSH/MCP 机制搬到 Codex 模块。

- **P1.1** 形成可审查的安全契约，分别定义 Agent 私有状态、认证环境、Runtime 项目环境、执行身份检查、网络要求和“未知”的处理；同机不同身份与 native 模式界线明确。
- **P1.2** 所有已知 Agent 敏感项按可访问性检查；选不同 provider 不能漏检另一个的凭据。覆盖 Agent transport、预检 SSH、Runtime child 三处，检查/日志不读取或泄露 secret 值。
- **P1.3** 合成凭据/目录 fixture 验证错误属主、权限、symlink、未知状态、敏感环境、agent forwarding、连接复用和普通项目环境；不读取真实私人文件内容。策略失败不产生未受控子进程。
- **P1.4** Claude 远程行为及 Codex 已测策略回归通过；记录每一层的单独证据，不能用 Runtime `env -i` 最终为空推断上游清理正确。运行 Rust 三门禁及现有 Python 测试。

停止点：提交契约、代码和证据，更新状态后停止。系统账户/ACL/网络生产实测排入 P3；P1 离线通过不是生产安全通过。

### P2 — Agent Instance 公共配置模型

**依赖 P1。**让同一 Node 上两个 provider 可分别寻址；不同时增加路由/角色评分/任意 profile 管理平台。

主要落点：`config.rs`、`configedit.rs`、`paths.rs`、provider/session 的身份字段，配置文档和 fixture。

建议模型（字段草案，验收前不作为可运行示例）：`workspace.agent → instance { id, provider, node, profile_ref }`。共享配置只含可公开的引用；profile 路径和登录由 Agent 端权威解析。不能把另一端发来的绝对目录当作本机配置指令。

- **P2.1** 配置契约定义 workspace/instance/node/profile 的唯一事实来源和两端解析流程；不复制第二份 root 或可漂移的 profile 定义。选择结果返回并绑定 provider/node/instance identity。
- **P2.2** 同 Node 的 Claude/Codex instance 能解析，未知/重复/冲突引用拒绝；拓扑和能力不支持时明确报错，不默默降级成 Claude。capability 表示真实技术支持，不表示“擅长架构”等主观标签。
- **P2.3** 现有 Claude 配置继续可用，新增模型与旧字段冲突有明确处理；迁移可预览且不静默改文件。Codex 已独立登录的专用 HOME 不因重构被改名、复制或失联；新 profile 必须用户独立官方登录，不能靠 symlink 共享 auth。
- **P2.4** 解析、序列化、legacy、跨端绑定和私有字段不出现在 Runtime payload 的测试通过；配置参考仅写已实现语法，未来示例另标草案。CLI 尚不开放 Codex 运行入口。

停止点：模型与兼容策略定下，保留尚未开放的产品开关。不为两个 instance 就引入数据库服务或动态注册中心。

### P3 — 公共入口与单 Agent 执行闭环

**依赖 P2。**开放显式 Agent 选择，但只有通过能力与安全门禁的模式可用。它是执行产品，不是多 Agent 编排。

主要落点：CLI、launcher/work、session/controller/tmux、Runtime 写入控制、doctor；复用既有体系，不建第二套 supervisor。

- **P3.1** 默认 workspace Agent、显式 `--agent` 覆盖、doctor、交互/print、status/result/attach/stop 全链路指向同一 instance；旧 Claude 命令不变。运行中的 session 绑定身份与 workspace 不可变，provider 不同不得复用或静默替换。
- **P3.2** 状态及结果能按稳定 `session_id` 精确寻址；不只靠“workspace 最新一次”猜结果。ccnm session ID 与 provider thread/resume ID 分开，状态与实际进程结束一致，错误启动留下失败记录。
- **P3.3** 同一工作树的两个受管写 session 在 Runtime 端原子互斥，包括不同 Agent Node、CLI/RPC 调用和 exec/patch；重入、竞争、异常退出、断线和残留子进程有测试。v1 可简单拒绝并发写，不必做分布式 lease 服务；无法证明旧执行者结束时维持 busy/unknown，不自动转让。嵌套/别名路径和 worktree 共享 `.git` 的限制必须定义，不能只锁 session 名称。
- **P3.4** Claude 已知 colocated 缺陷定点修复并单独更新相应测试/证据；与 remote 快照隔离。没有真实验收的模式明确拒绝并从支持矩阵移除，不能文档声称支持但入口不能用。Codex colocated 继续拒绝，不强求本阶段实现。
- **P3.5** 授权的双机 dogfood 覆盖两 provider 的公共链路、Ctrl-D/stop、detach/reattach、Controller 重启和链路失败；明确客户端断开不等于 MCP 已恢复，attach 不等于 resume。通过专用执行身份验证项目可用、敏感资源不可访问、无提权及所声明 egress 边界；scratch 只记 scratch。缺少系统授权/环境则记 blocked，不提升生产声明。
- **P3.6** Rust/Python 门禁和文档支持矩阵完成；只更新明确授权的部署，不替换用户正在运行的二进制/Controller，不混入已有修改。稳定入口不允许任意 root、auth path 或原始 provider argv 注入。

停止点：Claude/Codex 已是公共可选的单 Agent 执行能力。不得随手加入第三 provider、worktree 调度或自动 Agent 选择。

### P4 — Machine Protocol v1 草案与契约评审

**依赖 P3。**此阶段先写正式协议与机器可检查示例，不开始 HTTP 或冻结未验证接口。

新增 `docs/protocol/` 的正式说明与 schema/fixture；具体模块位置按实际规模决定，不提前拆很多 crate。

- **P4.1** 协议独立于内部 v1/v2：握手声明 public protocol/version/capabilities；分别描述请求 framing、大小上限、非法消息、未知 method/param/capability 与版本不匹配行为。首选 JSON-RPC 2.0 + UTF-8 单行消息的 stdio；采用标准前核对规范，不将自定义格式称为标准。
- **P4.2** 最小方法覆盖 hello、agents.list、session.start/status/result/stop；名字在本阶段定稿。start 尽快返回 session handle，状态/结果可轮询；公共 run 若提供只作为同一契约上的 wait 便利封装。首版不通过 JSON 传完整 PTY，不承诺 interactive 有结构化最终结果。
- **P4.3** 定义调用方 `request_id` 与启动幂等键的区别：相同启动键+相同输入复用同一 session，不同输入冲突；在持久化和拉起进程间崩溃时可恢复/报告 uncertain，不能盲目重启。状态含执行中、终态、取消中和未知；结果含 provider/instance/session、结构化错误、可选 usage/cost 与有界输出引用。
- **P4.4** 明确重试、超时、stop 幂等、取消完成判据、RPC 进程退出/管道 EOF 与 Agent 生命周期的关系；API 断连不默默停止或重复执行已被接受的任务。把 IPC 成功、Agent 退出成功、任务是否满足业务验收分开。`recoverable` 不能被解读为“自动重试安全”。
- **P4.5** 规定权限按已配置 workspace/instance 最小授予；输出引用和错误不能泄露私有目录/凭据，分页/保留/过期有契约。旧 peer 的退出码 0 不掩盖协议错误。写出成功、拒绝、断线、未知终态和过期结果等 fixture，并完成评审。

停止点：协议是草案/候选，不在 README 宣称稳定 v1。只有 P6 外部消费者验证后才建立兼容承诺。

### P5 — 本地 stdio Machine API

**依赖 P4。**实现 `ccnm rpc`，与人类 CLI 共用应用服务。stdio 不公开网络端口，但本地调用权限仍由 OS 身份与授权配置约束。

- **P5.1** 严格按协议处理请求/响应和有界 framing；stdout 只有协议，日志在 stderr；provider stdout 不混入 RPC。未知方法、非法 JSON、超大输入、EOF、背压和超时都有测试。
- **P5.2** 人类 CLI/RPC 使用同一身份解析、状态、结果、取消和 Runtime 权限/互斥，不通过调用 CLI 再解析文案实现 API；不把 internal payload 原样转成公共参数。
- **P5.3** 启动幂等与恢复状态持久化；客户端/RPC 重启后仍能按 session ID 获取状态/结果，重复请求不重复启动 Agent。进程崩溃窗口用故障注入验证，无法确认的执行不伪装成 failed 或可自动重试。
- **P5.4** 取消涵盖真实子进程组与待执行操作；确认结束前不能报告 cancelled 或释放写权。输出大小/分页、失效引用、终态缺失和格式损坏不返回假成功；非交互权限请求不能无限等待。
- **P5.5** RPC 单元/集成、Claude 兼容、Codex 回放及 Rust/Python 门禁通过；不开 HTTP，不做云鉴权、ChatGPT 接入和控制 MCP。标准库可用的持久化与锁优先复用，不先上外部队列。

停止点：本地 Machine API 可验，不等于已支持远程客户端或可直接发布。

### P6 — 黑盒消费者与 v1 兼容承诺

**依赖 P5。**建立不链接任何 ccnm crate 的最小外部 client，语言按测试维护成本选择，不建完整 Orchestrator。

- **P6.1** 黑盒 client 仅 spawn `ccnm rpc` 并通过字节流完成 hello/list/start/status/result/stop；明确选 Claude/Codex，能在没有 Orchestrator、没有内部库的环境运行。
- **P6.2** fixture/契约测试覆盖 provider 错配、旧 peer、重复启动键、启动响应丢失、重连、RPC 重启、取消中、Runtime 不可达、输出截断/过期；验证没有重复有副作用的执行、串 session 或凭据泄漏。
- **P6.3** 离线黑盒闭环：fake peer 覆盖旧版本对端、启动响应丢失、以退出码 0 代替回答等坏对端场景；离线 fake-agent 与付费真机测试分开记录，未测部分写明，CI 默认不消费登录凭据或订阅。**真机双机闭环（公共 API 各跑一次真实 provider、与人类 CLI 结果对照）改由 P7.3 执行**，原因见下。
- **P6.4** 根据消费者反馈修改草案，提交规范、schema、golden 和兼容测试；新增字段/能力与破坏性变更有明确规则。两产品版本独立；尚无第二协议版本时不用假造跨版本测试通过。**此时协议是 v1 候选，正式确立随 P7.3 的真机结果**；在那之前任何对外文档都不得宣称 v1 已稳定。

停止点：达到独立项目可消费的 integration contract（协议为候选）。规划中允许建立候选 client，不要求等待“永不变化的完美协议”。

#### 变更说明：真机验证并入 P7.3（2026-09-10）

原 P6.3 要求单独跑一次真机双机闭环，P7.3 又要求一次真实项目的完整 dogfood。两次都要重建同一套临时环境——Runtime 账号、SSH 准入、两端部署、真实订阅额度、事后清理归零——而 P3 的记录显示这个流程来回了十几轮。

经用户决定，真机验证集中到 P7.3 一次执行。**验收编号一个没删，只是移动了执行位置**：P7.3 里先完成原 P6.3 的内容再做完整 dogfood，同一套环境两件事。代价写在 P6.4 里：协议 v1 要晚一个阶段才敢确立，P6 收口时它只是候选。

不接受的替代方案：拿离线绿灯直接确立 v1。兼容承诺一旦对外就收不回来，而支撑它的证据里不能没有真实 provider。

### P7 — 发布前收口

**依赖 P6。**做实际目标平台的 dogfood 和证据审查，不把现有 macOS 结果推广到 Linux/Windows。

- **P7.1** 公开文档只描述真实支持的版本、平台、模式、topology 和安全级别；README 保持简短中英简介，其余中文，细节进入 docs。明确 Codex pin/version 拒绝与重新测量流程。
- **P7.2** 配置迁移、安装/回退、状态/日志保留、停止/清理和故障恢复有可执行记录；部署与登录相关动作单独获得授权。不因存在 CI/release 文件就宣称已正式发布。
- **P7.3** 在同一套授权环境内先完成原 P6.3 的内容——用公共 API 各跑一次真实 provider 的双机闭环，与人类 CLI 结果对照，据结果确立或修改协议 v1——再完成真实项目从启动→修改→测试→结果→停止/恢复的验收及生产边界复核；Rust、Python、计划检查、契约测试通过，失败/跳过/未测平台如实列出。
- **P7.4** 修正 P7.3 真机证明的 OS 身份/控制链矛盾：public CLI/RPC 的 Operator、Agent Identity、Runtime Executor 分离；`ccrun` 成为 inbound-only executor，不持 ccnm 所需出站 SSH credential；Agent 侧发起不再走 `Agent → Runtime public run → Agent` 回跳；Runtime workspace/root 与 safety verdict 由真正 Runtime Executor 权威解析/报告；**Agent Node 上的 doctor/mcp probe 也不能再把整条公共命令委托给 Runtime 后要求 ccrun 回拨 Agent，而应在 Agent 本机组合本地检查并直接请求 Runtime resolve/audit/MCP probe。**按 [双执行入口方案](runtime-surfaces.md) Batch A→D→D2→E 分批实现，至少重新跑一次新链路 Claude CLI + Machine API parity，以及 Agent 侧 doctor/probe，证明执行属主仍为 ccrun、ccrun 无出站 key/agent、资源归零。旧“把 key 移出 ~/.ssh 让 No SSH keys 变绿”不能作为验收。
- **P7.5** 评审所有剩余阻塞项，确认 P7.3 Codex 缺口已补齐且 P7.4 新身份链有真机证据，冻结 Machine Protocol v1 与本次支持范围，提交发布候选和限制说明。发布、推送、打 tag 按用户授权执行；本阶段完成可表示发布候选可交付，不强制未经授权发布。

停止点：ccnm 收口为可独立使用的执行产品。第三 provider、TUI、后台长进程、Browser/Git 专用工具和新 transport 按真实需求单独立项，不自动续做。

### P8 — 独立 Orchestrator 交接

**依赖 P7。此阶段在 ccnm 只交付边界/接口使用示例；新项目代码必须另开仓库，创建动作另获授权。**

- **P8.1** 输出新项目设计入口：自身 Task/Assignment/Attempt/Handoff/Acceptance 状态归新项目；Agent/session/进程/Runtime 执行状态归 ccnm，引用 session ID 不复制成第二份真相。
- **P8.2** 定义最小 `ExecutionBackend` 和 ccnm adapter 示例，adapter 只依赖公共协议。fake backend 可独立测协调逻辑；不为了“独立”立即重复实现所有 provider/SSH。ccnm 不导入新项目代码。
- **P8.3** 将下列 O1…O4 搬入新项目自己的计划/状态；新项目未获授权或未创建时，明确记录交接就绪/未实施，不能在 ccnm 标记其产品已完成。

后续新项目路线（这里只记交接范围，不维护第二项目进度）：

| 阶段 | 最小交付 | 验收重点 |
| --- | --- | --- |
| O1 | 显式选 Agent、持久任务/attempt、单 Agent 委派、结果和取消；无自动规划 | 重启能恢复、任务业务状态不与进程退出码混淆、deadline/调用预算/停止条件有效 |
| O2 | 固定 implementer→reviewer 流程，结构化 Handoff/证据/人工验收 | Agent 输出当不可信数据；review 不自动授写权；失败有限重试，不无限互聊；ccnm 执行写入门禁 |
| O3 | 经数据支持的能力路由、依赖图与可控并行，确需时才独立 worktree | 模型擅长什么不硬编码；合并前统一测试；worktree 不是安全沙箱，共用 Git 元数据的副作用有隔离方案 |
| O4 | CLI/MCP 集成及按需插件、远程客户端入口 | 先确认客户端真实 transport/auth 支持；第三方插件仍需最小权限/审计，不能假定协调插件天然安全 |

worktree **分配、调度、合并策略**在 Orchestrator；受管 workspace 的执行授权、写互斥和必要低层操作在执行 backend。确需新增 backend 操作时另提 ccnm capability，不让协调层绕过执行端边界。

### P9 — Remote Workspace MCP 契约

**依赖 P8。**这是 ccnm 自己的 v1.x 扩展，不是 Orchestrator 功能。先定义外部 Claude Code/Codex/其他 MCP Host 如何把 ccnm 当一个 remote workspace tool server 使用；不启动/代理 Agent。

- **P9.1** 定稿标准 stdio MCP bridge 的 CLI/配置形状：一次进程绑定一个已配置 workspace/Runtime；root 在 Runtime 权威解析，禁止把任意 host/root/private key 作为 MCP tool 参数。命令名在本阶段定稿，规划文档中的 `ccnm mcp connect` 只是占位。
- **P9.2** Runtime 侧定义 external MCP opt-in 与 `disabled/read/coding` 最大权限；read 模式没有 `apply_patch` 和 `exec_command`，coding 模式才竞争 writer guard。调用方不能请求高于 Runtime 配置的能力；多租户 token/共享账号不在首版。
- **P9.3** 给七工具形成稳定 ToolSemantics/标准 MCP annotations 映射；annotations 只用于 Host UX，Runtime enforcement 不依赖 Host 是否尊重它。`exec_command` 永远按 write/open-world-capable 保守处理，不解析 shell 猜只读。
- **P9.4** 定义连接/EOF/interrupt、busy/unknown、输出预算、版本不匹配、instructions/context policy 和错误泄漏边界；External MCP 不猜调用方 Provider，不读取 Client 的 Agent profile。契约、fixture 和评审先行，不开始真机付费调用。

停止点：Remote Workspace MCP 仍是设计候选；不顺手加 raw SSH/SFTP/端口转发。

### P10 — Remote Workspace MCP stdio bridge

**依赖 P9。**实现外部 MCP Host → 本地 ccnm bridge → persistent OpenSSH → Runtime 的最小数据面，复用现有 Runtime server。

- **P10.1** stdio MCP initialize/tools/list/tools/call 全链使用一个有界 SSH 子进程；stdout 只承载 MCP，日志 stderr；EOF/interrupt/bridge crash 能回收自己的 transport，不终止无关 Managed session。
- **P10.2** Runtime open/binding 从自己的配置解析 workspace/root/runtime_user，client payload 不能覆盖；需要新 internal protocol 时 fail-closed 版本握手，不静默降级旧 root-trusting 路径。
- **P10.3** 七工具不复制实现；Remote MCP 与 Managed path 复用 path policy、credential/environment gate、output retention 和同一 workspace write guard。read/coding 两种工具列表/权限有离线测试。
- **P10.4** 本地 fake SSH、坏 peer、断线、超大/坏 MCP 消息、Runtime 不可达、旧 ccnm 和资源清理测试通过；Rust/Python/plan/link 门禁通过，不宣称真实 Host 已验证。

停止点：得到离线可验 bridge；不加 HTTP/remote MCP 公网 transport。

### P11 — 跨入口安全、并发与真实 MCP Host

**依赖 P10。**证明第二入口没有绕开第一入口已经建立的 Runtime 边界。

- **P11.1** Managed coding session 与 Remote coding MCP 同 workspace 竞争同一 writer guard；不同入口同时启动时只有一个可写，旧执行者未确认退出不能转让。read 模式可按定义并行，但无任意 exec。
- **P11.2** Tool annotations 与 Runtime 实际权限一致；用忽略 annotations 的测试 Host 再跑一次，越权仍被 Runtime 拒绝，证明提示不是安全机制。
- **P11.3** 至少用真实 Claude Code 标准 MCP 配置跑 workspace_info/read/list/search/patch/exec/output 的允许矩阵，并用一个 provider-neutral MCP 测试 client 重放同一协议；验收看 Runtime 副作用/属主/错误，不采信模型文字。
- **P11.4** 凭据泄漏、private path、SSH agent forwarding、连接复用、断连残留、输出保留与 busy/unknown 有跨入口回归；不要求外部 Host 向 ccnm透露其 Claude/Codex 登录。

停止点：功能仍可标 experimental；真正远端项目/非 macOS 支持由 P12 决定。

### P12 — Remote Workspace MCP 真项目 dogfood 与 v1.x 冻结

**依赖 P11。**用真实远端项目和实际目标 OS 做支持声明，不把本机/假 SSH 结果推广出去。

- **P12.1** 至少一个远端真实项目完成 read→search→patch→exec/test→read_output、结果核对、断开/重连和资源归零；优先 Linux Runtime，以补当前 Managed 路线主要是 macOS 的证据空白。
- **P12.2** 专用 Runtime identity 复核无 Agent credential、无 ccnm 出站私钥/SSH agent、无 sudo/admin/特权 socket；项目 toolchain 实际可用。egress 没配置就明确“不保证”，不靠禁止 curl 冒充隔离。
- **P12.3** 版本错配、workspace 未 opt-in、read→coding 升权、writer busy、远端消失和 Host crash 在真/准真环境至少各有可追溯结果；失败后无孤儿 transport/guard。
- **P12.4** 更新 README/usage/support-matrix/operations，写明与 Managed Agent Runtime 的区别和支持平台；全部门禁通过后冻结 Remote Workspace MCP v1.x 契约。第三 Provider、HTTP gateway、generic SSH 管理继续另立项。

停止点：ccnm 拥有两个独立可用入口，但仍只有一个 Runtime 执行核心；不继续自动扩功能。

### P13 — MCP instructions 按 Host 实际上限投影

**依赖 P12。用户 2026-09-15 指定立项。**起因是跨仓计划 toexec v2 的 V2-Q1：Claude Code 2.1.269 把 MCP `instructions` 截到 **2048 个 UTF-16 码元**（打包代码 `FT=2048` 按 JS 字符串长度截，真实连接日志 `Server instructions truncated from 3018 to 2048 chars`）。ccnm 却按 16 KiB 字节做预算，还把其他说明文件清单和 `[project instructions: …]` 标记行放在正文后面——文件一长，Host 先截掉的正是告诉模型"少了多少、怎么读全文"的那两段。不改工具、权限、错误码语义，不升 `ccnm.workspace-mcp` 或内部 wire 版本。

- **P13.1** 预算按 Host 的计量方式：Claude Managed 和外部 `external_instructions = "project"` 按 2048 个 UTF-16 码元（bridge 不知道对面是哪个 Host，只能按已知最严的算）；Codex Managed 保持 16 KiB 字节。上限、计量单位和依据的版本只写在一处；长清单、超长文件、多字节字符的最坏情况有测试证明不超限。
- **P13.2** 顺序改为：基础说明（外部模式另有模式句）→ 标记行 → 其他说明文件清单 → 项目说明正文。正文由 ccnm 按行截断，标记行写明文件多大、给了多少、怎么读全文；清单自身有上限，不能把正文之前的部分挤出上限。
- **P13.3** 兼容：不重录 golden fixture，测试里把旧文本按新顺序重排后比对，证明内容不变、只改位置；`parse_marker`、doctor 的 Project instructions 行和 probe 在新布局下结果正确；协议、配置、排障文档里"16 KiB"的说法按 Host 更正。
- **P13.4** 证据：离线全量门禁；本机真实 Claude Code 连接真实 `ccnm internal mcp-serve`，改前 debug 日志出现 `Server instructions truncated`、改后不出现（连接阶段在模型认证之前完成，不耗模型额度，不代表模型行为已验）。

停止点：只修 instructions 的预算和顺序。Codex 延迟加载工具时只显示说明首行前 250 个字符（Codex 0.154 源码），不在本阶段处理，记入 observed_gaps。

## 三、首次规划提交的范围（历史说明）

首次规划提交 `7c41f6f` 只落地计划、状态、模型入口与检查工具；当时不执行 P1…P8 的产品改动、不创建 Orchestrator、不部署/登录/更改 OS 策略，P1 保持 pending。之后按用户请求与 `status.json.current_task` 逐阶段执行，不能用这段历史说明覆盖当前状态，也不能把“规划已提交”当作“产品验收已完成”。系统与部署动作仍需逐项授权，不恢复用户已删除的历史文档。

### P14 — read_file 读超长单行时内存有界

**依赖 P13。用户 2026-09-16 要求在共享内核大改前先修已知缺陷。**`read_file` 用 `read_until(b'\n')` 先把整行读进内存，读完才检查 64 MiB 扫描上限：一个 2 GB 的单行文件（压缩过的 JS、一行 JSON 导出）会先分配 2 GB 再报错，Runtime 可能先被 OOM 杀掉。输出契约不变，只改读法。

- **P14.1** 一行最多保留 `max_bytes` 加少量余量（BOM、被切开的多字节字符），其余只计数不存；扫描上限在读的过程中检查，越过 64 MiB 立即停，不再读完整行。测试用一个越界即报错的生成式读取器证明：修前会读穿、修后在上限处停。
- **P14.2** 输出与修前一致：部分行截断、`next_start_line`、CRLF/LF/混合、无尾换行、BOM、非法 UTF-8、范围读取的既有测试不改断言；新增超长行跨缓冲区边界的 CRLF 与多字节字符用例。
- **P14.3** Rust 门禁、`external_mcp` 与中立 MCP 客户端测试通过；`read_file` 不是 schema 或文档层面的变化，不改协议文档。

停止点：只修这一处读法，不做 V2-K 的共享文本原语抽取。

### P15 — 外部入口配置示例启用 alwaysLoad

**依赖 P14。用户 2026-09-16 指定立项。**起因是跨仓计划 toexec v2 的 V2-Q2：Claude Code 默认把 MCP 工具放进**延迟加载池**（工具表里只有名字，模型要先调一次 `ToolSearch` 才拿得到 schema）。外部入口（用户自己的 Claude Code 接 `ccnm mcp bridge`）因此每个任务多一个回合；那次 18 格模型对照里，加 `alwaysLoad` 的一组 `ToolSearch` 调用为 0、成功率不变、墙钟和 token 都不更差，结论是采纳。本阶段只把这个配置写进外部入口的示例和说明。

**Managed 路径不受影响，也不改**：ccnm 启动 Claude Code 时传 `--tools ""`，`ToolSearch` 本身不可用，七个工具本来就全量加载。**也不在工具上加 `_meta["anthropic/alwaysLoad"]`**：两种写法对延迟加载效果相同，但只有服务器配置那条会让 Host 在首轮请求前等这台 server 连上，而且模型对照测的就是配置这条；`_meta` 没有实测数据。

- **P15.1** 协议文档的 `mcpServers` 示例加 `"alwaysLoad": true`，并写明：这是 Claude Code 自己的配置键、不是 MCP 标准字段，实测生效的版本，不加会怎样（模型先调一次 `ToolSearch`），以及代价（首轮请求前会等 bridge 连上 Runtime，Runtime 不可达时启动更慢）。
- **P15.2** 使用说明的外部入口一节指到这一条，不复制第二份说明；Managed 路径为什么不需要改，全仓只写一处。
- **P15.3** 证据：本机零额度复现延迟池差异（`coding` 与 `read` 两种模式各一对），记录客户端与 ccnm 版本、复跑方式和未覆盖范围；模型侧收益引用 toexec 的 V2-Q2，不在本仓重跑、不再消耗订阅额度。

停止点：只改文档与示例，不动 ccnm 代码、工具元数据和 `ccnm.workspace-mcp` 版本；其他 Host（Codex 等）没有对应机制，不替它们编配置。

### P16 — 接入共享库的有界行读取

**依赖 P15。用户 2026-09-16 指定推进跨仓计划 toexec 的 V2-K 主线。**开工前先做了重复度盘点（toexec 仓库 `evidence/v2-k/duplication-audit.md`）：两个产品的 `read_file` **契约不一样**（非法 UTF-8 一个报错一个有损替换、一个一定读到文件尾一个撞预算就停），不能也不该统一；真正共有的内核只有「读一行但不把整行读进内存」。P14 改出来的 `next_line` 就是它，gld 的搜索路径上还是 `reader.lines()`，同一类问题——但它有 `max_file_bytes` 兜底（默认 2 MiB、最大 64 MiB），没有 ccnm 当时那种无上限的 2 GB 风险。

本阶段只做 ccnm 这一侧：`next_line` 移到共享 crate `toexec-text`，ccnm 改为调用它。**行为逐字节不变**——这是一次纯粹的搬家，不是重写。

**依赖方式：按 tag 固定的 git 依赖。**共享 crate 在 `https://github.com/xwfe/toexec.git`（公开仓库，与 ccnm、gld 一致），tag `toexec-text-v0.1.0`。本阶段中途先用过本地 `path` 依赖，那让两边 CI 都构建不了；同日用户决定把仓库推上去，改成 git 依赖解决。

- **P16.1** `mcp/read.rs` 用 `toexec_text::next_line`，删掉本地那份；`Ending` 换成 `toexec_text::Terminator`，写死的 `MAX_SCAN_BYTES` 作为 `scan_limit` 参数传进去，仍由 ccnm 决定它是多少。既有 24 个 `mcp::read` 测试**一条断言都不改**，包括 P14 新增的三条（扫描上限、超长行切法、跨缓冲区 CRLF）。
- **P16.2** `Cargo.toml` 按 tag 固定共享 crate，不跟 `main` 走——共享库改了不会在某次 `cargo update` 之后突然改变 ccnm 的行为，升级是显式的一步。旁边写清楚为什么用 https（公开仓库，本地和 CI 都不必配凭据）和本地开发怎么办（临时改 path，不提交）。`rust-version` 已经是 1.89，与共享 crate 一致，不需要改。验收要证明的是**在一个旁边没有 toexec 的目录里也能构建**，这正是 CI runner 的处境。
- **P16.3** 离线全量门禁通过（fmt、严格 clippy、`cargo test --workspace`、`external_mcp` 与中立 MCP 客户端测试）；`read_file` 不是 schema 或文档层面的变化，不改协议文档。

停止点：只搬这一个函数。原子写入与回滚是盘点认定收益最大的下一块，但它在写入路径上，等这次的跨仓联动被证明可用之后另立阶段；进程/输出不碰（gld 是 tokio async + 要支持 Windows，ccnm 是同步 + 只跑 Unix）。

### P17 — 原子写入接共享库

**依赖 P16。用户 2026-09-16 指定继续推进跨仓计划 toexec 的 V2-K。**盘点（toexec 仓库 `evidence/v2-k/duplication-audit.md`）认定这是收益最大的一块：两个产品都写过「临时文件 → rename → 失败回滚」，ccnm 这份 `sync_all()` 之后才 rename、保留原权限、回滚失败留 journal；gld 那份没有 fsync、不保留权限、备份是整个原文件读进内存。

**只抽两步纯机制**：`write_durable`（落盘 + 权限）和 `replace`（一次 rename），共享 crate 是 `toexec-fs`。**临时文件命名、备份、回滚编排都不抽**——两边差得远（ccnm 按操作类型分并写 journal，gld 一刀切恢复），而且 ccnm 的 `TEMP_PREFIX` 是 `sweep_stale_temps` 的判据，改了会波及清理逻辑。

- **P17.1** `write_atomic_temp` 的三步（create/write_all/sync_all + set_permissions）换成 `toexec_fs::write_durable`，`commit_one` 里的 `fs::rename` 换成 `toexec_fs::replace`。ccnm 自己的错误包装（哪个文件、哪一步）原样保留——共享库只回 `io::Error`，措辞是 ccnm 的对外契约。
- **P17.2** 行为逐字节不变：`apply_patch` 的既有测试**一条断言都不改**，包括 journal、回滚、中途失败、权限保留、stale 版本这些。`TEMP_PREFIX` 和 `sweep_stale_temps` 不动。
- **P17.3** 离线全量门禁通过（fmt、严格 clippy、`cargo test --workspace`、`external_mcp`、中立 MCP 客户端）；不改协议、schema 和 `ccnm.workspace-mcp` 版本。

停止点：只换这两处调用。journal、备份策略、回滚编排、`apply_patch` 的语义都不动；父目录 fsync 是另一个决定（共享库文档里记了这个已知边界），本阶段不做。

### P18 — 冻结协议的工具表 fixture 由代码兜住

**依赖 P17。用户 2026-09-16 报告，起因是给 gld 写远端工具白名单时逐条核对 fixture 与实现。**`fixtures-mcp/tools-list-coding.json` 里 `apply_patch` 的参数写的是 `changes`，实现（`ApplyPatchArgs`）在 wire 上叫 `files`，结构体没有 `serde(rename)` 也没有 `alias`——照 fixture 实现的 Host 发 `{"changes": [...]}` 会被直接拒。两份 fixture 另外漏了 6 个真实存在的参数（`read_file` 的 `end_line`/`max_bytes`、`list_files` 的 `include_hidden`、`search_text` 的 `glob`/`case_sensitive`/`context_lines`、`exec_command` 的 `preview_bytes`）和 `apply_patch` 的 `dry_run`。

**`check_protocol.py` 拦不住，是设计如此**：它把 fixture 对着 `schema/` 里手写的 JSON Schema 校验，而那份 schema 把 `inputSchema` 写成"任意 object"。fixture 和 schema 互相一致，两边一起跟二进制不一致——冻结的是两份手写文件，不是实现。

**fixture 的定位按证据判定为字面 wire 样本**：七个工具的 `description` 与 `mcp/server.rs` 的 `#[tool(description = ...)]` 逐字节相同，其余 fixture 也都是实测报文（`call-read-file-ok` 的 `$note` 记的就是 Claude Code 2.1.260 的实测行为）。而且**协议正文里没有任何参数表**——第 5 节只有 annotations，第 8 节只顺带提了 4 个上限参数，所以这两份 fixture 是参数名在文档侧的唯一记载。因此参数名和 `required` 必须完整且精确；每个参数的类型、上下界和说明仍写简写，因为它们是 schemars 从 Rust 类型生成的，把生成细节冻进来会制造假失败。

- **P18.1** 两份 fixture 的参数名集合与 `required` 补齐到与实现一致：`changes` 改成 `files`，补上 `dry_run` 和那 6 个漏掉的参数。`$note` 写明哪部分是精确的（名字、必填）、哪部分是简写（类型与边界），以及以谁为准。
- **P18.2** 加一道**从代码生成**的检查：`external_mcp` 里起真实 `internal mcp-serve`，两种模式各取一次 `tools/list`，与对应 fixture 逐工具比参数名集合和 `required` 集合，对不上就失败。这条只比名字和必填，不比 description、类型和数值边界——比多了会在升 schemars 或改一句说明时假红。
- **P18.3** `schema/remote-workspace-mcp-v1.schema.json` 不再把 `inputSchema` 当任意 object，至少要求 `type` 和 `properties` 在场；协议文档第 13 节写明名字这道检查在哪条命令里，以及 `check_protocol.py` 为什么证明不了它。四条协议命令全过。

停止点：只对齐工具表 fixture 的参数名并加这道检查。不改任何工具的行为、参数、默认值和 `ccnm.workspace-mcp` 版本——补的全是本来就在 wire 上的名字，属于修正记载而不是加字段；也不给别的 fixture（调用结果、启动诊断）加代码驱动检查，那些要对的是报文正文，是另一件事。

### P19 — 工具说明文字也纳入同一道检查

**依赖 P18。用户 2026-09-16 指定立项。**P18 那道检查只比参数名和 `required`，把 `description` 划在外面，理由是"会在改一句措辞时假红"。**这个理由对类型和上下界成立，对 `description` 不成立**：类型和边界是 schemars 从 Rust 类型生成的副产品，而 `description` 是人手写进 `#[tool(description = ...)]` 的，fixture 里也是逐字节复制过去的。措辞漂开不是假红，是真漂——而且漂的正是**模型实际读到的那段文本**，比参数名更直接地决定 Host 那头的行为。

七段说明当前与代码逐字节相同（P18 核对过），所以这一阶段是加一道拦住未来的检查，不是修一个现有缺陷。

- **P19.1** 检查纳入 `description`，逐字节比。测试随之改名——它比的已经不只是参数——并同步四处引用：两份 fixture 的 `$note`、schema 里 `$defs/input_schema` 的说明、协议文档第 13 节那一小节。`docs/research/p18-*.md` 和 `status.json` 里 P18 的 evidence 是那一轮的历史记录，**不改**，由本阶段的记录说明改名。
- **P19.2** 先红后绿：改掉一段 `description` 的措辞重跑，检查如期失败；改回后通过。证明它不是恒真断言。
- **P19.3** 门禁全过（四条协议命令 + fmt/clippy/`cargo test --workspace` + Python 全量）；不改工具行为、参数、说明文字本身和 `ccnm.workspace-mcp` 版本。

**故意不做自动重录。**加一个"跑一次就把 server 的输出写回 fixture"的开关能省掉维护成本，但 `AGENTS.md` 写着"不为通过测试重录 golden fixture"——那个开关会让下一个人把一次没想清楚的措辞改动一键洗成绿的，而冻结契约的意义恰恰是改它要费一点劲。改了说明就手动同步 fixture，失败信息里两段文本都会打出来，照着贴即可。

停止点：只把 `description` 纳入。**annotations 不纳入**——它们已经被两处证明着：`every_tool_publishes_its_annotations` 对着硬编码期望比真实 server，schema 的 `read_tool` 又把 read 模式的 `readOnlyHint`/`openWorldHint` 钉成常量；再加一处比对是第三份说明，按"一件事只写一处"不加。每个参数的类型、上下界和说明仍然不比，理由与 P18 相同。调用结果和启动诊断那 19 份 fixture 仍只有 schema 层检查，要做另立阶段。

### P20 — 会话拒绝消息不再冒充 exec 拒绝

**实际只依赖 P17，按顺序规则记在 P19 之后。2026-09-16 在验证 gld 远端只读链（gld RFC-0002 的 H2）时撞到。**（编号说明：本阶段和 P18/P19 在两个分支上同时从 P17 开工，开发时也叫 P18，提交 c720154、721b1fb 消息里的 `p18` 指的就是它；合并时按开工先后顺延为 P20。）给 workspace 配 `external_mcp = "read"`，从另一台机器跑 `ccnm mcp bridge --mode read`，握手失败，消息说 `exec_command is refused`——而只读会话一共四个只读工具，根本没有 `exec_command`；结尾还无条件推荐 `allow_unconfined_exec`，把人推去为一条只读链签一个名字叫「允许不受限执行命令」的开关。

**闸本身是对的，不动。** `Server::new` 无条件判 `agent_boundary_clear` 是有意的：`external_mcp.rs` 的 `agent_credentials_stop_the_external_entry_too` 用 read 模式 fixture 钉着它，P11.4 把它列为跨入口回归项。本阶段只改消息和文档。

**真正的缺陷是消息在指错地方**（实测，见证据）：`refusal` 打印全部 Fail finding，而这道闸只读 `waived_by` 不豁免的那几条——`Not an admin`、`No SSH keys` 从来没参与判断；`allow_unconfined_exec` 在这道闸里**一个 finding 都不豁免**，只写它照样被拒。只写 `allow_unisolated_credentials = true` 就能开只读会话。

- **P20.1** `Audit::refusal` 带上「在拒什么」：`Refused::Session` 用自己的抬头、只列真正挡住它的 finding、结尾只指 `allow_unisolated_credentials` 并说明另一个开关开不了这道门；`Refused::ExecCommand` 的措辞**一个字不改**。顺带把 `with_gate` 里 `write_guard.is_some() &&` 这个误导性条件去掉——它是 P3 遗留，对现有全部调用方是 no-op。
- **P20.2** 回归：只读外部会话被拒时消息不出现 `exec_command`、不推荐 `allow_unconfined_exec`、不列没参与判断的行；只写 `allow_unisolated_credentials` 能开出四个只读工具并真的读到文件。exec 那条路径的既有断言一条不改。
- **P20.3** 文档写清楚两个开关各开哪道门、只读会话为什么也要过这道闸、操作员该怎么办；协议文档不改——第 11.2 节本来就写着「不要解析第一行之后的措辞」，退出码和 `CCNM_E_POLICY` 都没变。

停止点：只改怎么把结论讲给人听。audit 判定、finding 分类、豁免规则、`doctor` 输出一律不动；不按入口分开判凭据类 finding（理由记在证据里）；不跑真机、不耗额度。

### P21 — Codex 原生执行链：先测会推翻设计的事

**依赖 P20。用户 2026-09-16 指定立项（跨仓计划 toexec v2 的 V2-C），同日决定原生读和 MCP 读同一契约。**链路、分工和读边界见[双执行入口方案](runtime-surfaces.md)第 12 节。连接身份、协议、权限三件事已在 toexec 测过；下面几件还没测，任何一件结果不对都要改设计，所以先测。全部零模型额度（本机假 Responses 接口 + 禁出站沙箱），不写 ccnm 代码。

- **P21.1** 工作目录：Codex 把**自己本机**的工作目录原样发给 exec-server，而 Agent Node 上没有项目目录。按 0.154.0 源码，交互模式在默认环境是远端时不要求目录在本机存在（`tui/src/lib.rs` 的 `config_cwd_for_app_server_target`），`codex exec` 却先在本机 canonicalize 工作目录（`exec/src/lib.rs` 的 `canonicalize_existing_preserving_symlinks`）。两种模式各实测：exec-server 那一侧有该路径、Codex 这一侧没有。某个模式必须本机有同名目录时，写明 ccnm 的做法（只开另一种模式，或在 Agent 侧建同名空目录——后者要实测 Codex 会不会把本机那个空目录当项目读）。
- **P21.2** 工具面与方法表：按 ccnm 会用的开关组合（Code Mode、已禁用的 feature），让假模型依次调 Codex 原生的命令执行、`apply_patch` 和其余会碰文件的工具，录下全部 JSON-RPC，得出"每个工具调哪些方法、带不带 sandbox、路径指向哪"；同时确认远端模式下 Codex 是否还在 Agent 本机访问项目路径，以及 `capabilityRoots/discoverV1`、`environmentConfig/read` 什么时候调、依赖返回里的哪些字段。
- **P21.3** 读边界要用的错误形状：exec-server 对不存在路径、越界路径各回什么；受管入口替它回答"根以上的 `.git` 查询"时照哪个形状回，Codex 后续行为才和确实不存在一致（对照：同一目录真的没有 `.git` 时 Codex 发出的后续请求）。
- **P21.4** Linux 沙箱：Runtime 是 Linux 时，Codex 发来的 workspace-write sandbox 是否同样挡住工作区外写入。toexec 的权限实测只在 macOS（Seatbelt）上做过。在本机容器里测，不动任何真实 Runtime。
- **P21.5** 冻结方法规则表：每个方法写放行条件和拒绝时回什么，未列出的方法一律拒绝；P22 照表实现。实测与第 12 节设计不符的地方，在同一提交里改设计并写原因。

停止点：只测和定表。不写 ccnm 代码、不在任何 Runtime 装 Codex、不跑真实模型。

### P22 — Runtime 侧受管 exec-server

**依赖 P21。**Runtime Executor 上新增一个内部入口（名字本阶段定）：按 Runtime 自己的配置打开 workspace、审计、取写锁，再以子进程启动固定版本的 `codex exec-server --listen stdio` 并一直监督它；SSH 进来的每条请求先过 P21.5 的规则表再转发。

- **P22.1** 打开流程复用 `internal mcp-serve` 的权威解析：wire 不带 root 和任何路径，版本不认识就停（不回退）；安全审计用同一套 finding 与豁免。只接受 coding 权限的 workspace，只读请求在启动前拒绝，理由见第 12.3 节。
- **P22.2** 写锁与监督：启动 exec-server **之前**取与 `mcp-serve` 同一资源的 writer guard，Managed、外部 MCP、原生链三者互斥。关闭顺序：停止转发新请求 → exec-server 退出且它起的进程确认结束 → 写 `released`。哪一步证明不了就保持 `held`/unknown，不按时间放锁。不用 `exec` 替换自身（Drop 不会跑，锁留在 `held`）。
- **P22.3** exec-server 进程本身：二进制路径只来自 Runtime 本机配置；`--version` 和握手返回的 `executorVersion` 都必须等于 ccnm 钉的 Codex 版本，否则拒绝。环境按白名单构造——它给命令的环境策略是全继承，漏一个变量就等于发给每条命令。`CODEX_HOME` 由 ccnm 生成、里面没有任何凭据（`environmentConfig/read` 会把它的配置原样回给客户端）。
- **P22.4** 按表过滤：握手带 `resumeSessionId` 就拒；`process/start` 必须带 sandbox，`cwd` 与 `workspaceRoots` 钉在 workspace 根，权限条目不宽于实测的 workspace-write 形状，网络为 restricted；文件写方法要求 sandbox 且根同上；**文件读方法不论 sandbox，按第 12.2 节的读契约校验路径**；`http/request` 一律拒；未知方法回 `-32601`，未知通知丢弃不转发（exec-server 收到会直接断连）；单帧上限定得比 exec-server 的 64 MiB 小，超限由 ccnm 回错误。
- **P22.5** 离线测试：一个不 import ccnm 的中立 JSON-RPC 客户端经真实内部入口，逐行跑规则表的放行和拒绝，**看真实副作用**（文件写没写出、进程起没起来），不只看回包。写锁与 `mcp-serve` 互斥；exec-server、它的子进程、SSH 连接分别异常结束时锁状态正确，写锁移交相关的故障点各重复 20 次。CI 没有 Codex 二进制，规则层用假 exec-server 覆盖；接真 exec-server 的那组在本机跑并记录版本。Rust 全量门禁通过。

停止点：Runtime 侧离线可验；还没有东西从 Agent 侧连它，Codex 的启动参数一行不改。

### P23 — Agent 侧网桥与 Codex 启动接线

**依赖 P22。**

- **P23.1** 网桥：session supervisor 在 Agent Node 监听 `127.0.0.1` 的随机端口，accept 之后查对端 socket 属于哪个 uid（macOS 读 `net.inet.tcp.pcblist64`，Linux 用 sock_diag），只放行与 Codex 同一 uid 的连接，查不到就拒。**每个会话只放行一条连接**，断了就不再接受——否则 Codex 会自己 resume，把断线后的命令继续执行。带 Origin 头的升级请求拒绝。URL 里不放任何秘密。
- **P23.2** 传输：WebSocket 文本帧与 SSH stdio 上的逐行 JSON 互转；SSH 复用 `session/transport.rs`（清环境、禁 agent forwarding、不复用个人 ControlMaster）。网桥不解析方法。
- **P23.3** 启动接线：原生链是显式 opt-in 的配置项（名字和挂在 instance 还是 workspace 上，本阶段定），默认仍是 MCP 七工具，旧配置行为不变。打开后 Codex 带 `CODEX_EXEC_SERVER_URL` 启动，工具开关用 P21.2 实测过的组合；P21.1 判定不支持的模式在创建 session 前拒绝，不静默退回 MCP 或 Agent 本机执行。
- **P23.4** 离线端到端（零额度）：真实 Codex + 本机假模型接口 + 禁出站沙箱，经网桥和本机内部入口完成读、改、跑命令；另一个 OS 用户连网桥被拒（Linux 容器）；断线后第二条命令哪里都没执行；Runtime 拒绝的请求在 Codex 里表现为工具失败，不退回本机执行。Rust 与 Python 门禁通过。

停止点：离线闭环可复现。不部署、不跑真实模型、不改默认执行方式。

### P24 — 原生链真机验收

**依赖 P23。在 Runtime 上装 Codex、替换任何机器上已装的 ccnm、真实模型回合，都要针对该动作单独授权。**

- **P24.1** 授权的双机环境里，Runtime 以 ccrun 运行 exec-server，Agent 侧真实 Codex 完成一次读 → 改 → 跑测试 → 看结果的小任务；进程属主是 ccrun，Runtime 执行身份上没有 Agent 凭据和出站私钥（沿用 P12.2 的检查）。
- **P24.2** 跨入口：原生链 coding 会话与 Managed MCP、外部 MCP 的 coding 会话竞争同一把写锁，只有一个拿到。
- **P24.3** 故障：Agent 侧断网、SSH 断开、exec-server 被杀、Runtime 上有子进程残留，写锁移交相关的各重复 20 次、其余各 5 次；未确认退出不放锁，断线不 resume、不重放。
- **P24.4** 文档：支持矩阵、使用说明、运维手册写明原生链的平台、版本 pin、与 MCP 路径的区别和已知限制；模型回合计入 toexec v2 第 10.1 节的累计额度并记进 evidence。

停止点：原生链成为 opt-in 的可用能力。要不要改成默认，另做决定。
