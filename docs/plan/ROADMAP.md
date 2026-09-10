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

`P0 → P1 → P2 → P3 → P4 → P5 → P6 → P7 → P8 → P9 → P10 → P11 → P12`。默认每轮只执行一个阶段。P0–P8 是 ccnm v1 收口和独立 Orchestrator 的接口交接；P9–P12 是 ccnm v1.x 的 Remote Workspace MCP 扩展。完整边界见 [双执行入口方案](runtime-surfaces.md)。

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
- **P7.4** 修正 P7.3 真机证明的 OS 身份/控制链矛盾：public CLI/RPC 的 Operator、Agent Identity、Runtime Executor 分离；`ccrun` 成为 inbound-only executor，不持 ccnm 所需出站 SSH credential；Agent 侧发起不再走 `Agent → Runtime public run → Agent` 回跳；Runtime workspace/root 与 safety verdict 由真正 Runtime Executor 权威解析/报告。按 [双执行入口方案](runtime-surfaces.md) Batch A→E 分批实现，至少重新跑一次新链路 Claude CLI + Machine API parity，证明执行属主仍为 ccrun、ccrun 无出站 key/agent、资源归零。旧“把 key 移出 ~/.ssh 让 No SSH keys 变绿”不能作为验收。
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

## 三、首次规划提交的范围（历史说明）

首次规划提交 `7c41f6f` 只落地计划、状态、模型入口与检查工具；当时不执行 P1…P8 的产品改动、不创建 Orchestrator、不部署/登录/更改 OS 策略，P1 保持 pending。之后按用户请求与 `status.json.current_task` 逐阶段执行，不能用这段历史说明覆盖当前状态，也不能把“规划已提交”当作“产品验收已完成”。系统与部署动作仍需逐项授权，不恢复用户已删除的历史文档。
