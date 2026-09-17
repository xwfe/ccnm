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

`P0 → P1 → P2 → P3 → P4 → P5 → P6 → P7 → P8 → P9 → P10 → P11 → P12 → P13 → P14 → P15 → P16 → P17 → P18 → P19 → P20 → P21 → P22 → P23 → P24 → P25 → P26 → P27 → P28 → P29 → P30 → P31 → P32 → P33 → P34 → P35 → P36`。默认每轮只执行一个阶段。P0–P8 是 ccnm v1 收口和独立 Orchestrator 的接口交接；P9–P12 是 ccnm v1.x 的 Remote Workspace MCP 扩展；P13 是按真实 Host 行为修正两个入口共用的 instructions 投影；P21–P24 是 Codex 原生执行链；P25 修 P24 真机轮发现的预检错误码；P26 补原生链在 Runtime 侧的探活；P27 让 doctor 也探这条链；P28 让 CI 在声明的 rust-version 上编译一遍；P29 补测原生链的并发、在途请求与资源上限，P30 修它查出的 fs helper 活过放锁；P31 给 Runtime 保留输出加会话总量上限、结束即删和过期清理；P32 封存原生链（用户决定）；P33 把沙箱那项收益搬到两个入口共用的 `exec_command` 上；P34 修 `apply_patch` 日志锁探测靠关文件放锁、fork 窗口里漏拦的缺陷；P35 让测试建的临时目录跑完就删（纯测试代码）；P36 起是"工具面对齐原生能力"那条线（跨仓方案在 toexec 的 v3 计划），第一个阶段是 Runtime 上项目自带的 skills。完整边界见 [双执行入口方案](runtime-surfaces.md)。

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
- **P22.3** exec-server 进程本身：二进制路径只来自 Runtime 本机配置；它的 `--version` 必须等于 ccnm 钉的 Codex 版本，否则拒绝，握手返回的 `providerId` 记进日志。**不用握手里的 `executorVersion` 核版本**：官方 Linux 发行包报的是 `0.0.0`（P21 实测）。环境和 MCP `exec_command` 的子进程用同一套清理——它给命令的环境策略是全继承，漏一个变量就等于发给每条命令（立项时写的是"白名单"，实现时改为与 MCP 同一套，原因见 P22 记录）。`CODEX_HOME` 由 ccnm 生成、里面没有任何凭据（`environmentConfig/read` 会把它的配置原样回给客户端），也不放在系统临时目录下（exec-server 会拒绝在那里建辅助程序）。
- **P22.4** 按 [P21 冻结的规则表](../research/p21-codex-native-surface-2026-09-16.md)过滤，表里没列的方法一律拒。要点：握手带 `resumeSessionId` 就拒；`process/start` 和文件写方法的 sandbox **逐条核对内容**，只在场不够——人在 Codex 里批准提权后，命令带 `sandbox: null`，越界 patch 带一条多出来的路径写条目；**文件读方法不论 sandbox，按第 12.2 节的读契约校验路径**，根以上的 `.git` 查询照 `-32004` 回答；`http/request` 一律拒；未知通知丢弃不转发（exec-server 收到会直接断连）；单帧上限定得比 exec-server 的 64 MiB 小，超限由 ccnm 回错误。
- **P22.5** 离线测试：一个不 import ccnm 的中立 JSON-RPC 客户端经真实内部入口，逐行跑规则表的放行和拒绝，**看真实副作用**（文件写没写出、进程起没起来），不只看回包。写锁与 `mcp-serve` 互斥；exec-server、它的子进程、SSH 连接分别异常结束时锁状态正确，写锁移交相关的故障点各重复 20 次。CI 没有 Codex 二进制，规则层用假 exec-server 覆盖；接真 exec-server 的那组在本机跑并记录版本。Rust 全量门禁通过。

停止点：Runtime 侧离线可验；还没有东西从 Agent 侧连它，Codex 的启动参数一行不改。

### P23 — Agent 侧接线：Codex 自带的 stdio 传输接 Runtime 的 exec-serve

**依赖 P22。立项时写的是"WebSocket 网桥 + 查对端 uid"。开工前核对 Codex 0.154.0 源码并零额度实测（[P23 记录](../research/p23-stdio-transport-2026-09-16.md)）：TUI 启动时读 `CODEX_HOME/environments.toml`，里面用 `program`/`args` 声明的 stdio 子进程就是 exec-server 传输——Codex 自己 spawn 它、自己持有两端管道，断线后不重启它、不 resume。于是监听端口、对端 uid、WebSocket 依赖都不需要了；P23.1/P23.2 按实测改写，toexec G05-peer 的 uid 方案留作对照，不进产品。**

- **P23.1** 传输程序：`ccnm internal exec-transport --payload <session>`，由 Codex 按 environments.toml 启动，在 Agent Node 上 exec 成一条 `/usr/bin/ssh`（复用 `session/transport.rs` 那套：清环境、禁 agent forwarding、不复用个人 ControlMaster），远端命令是 `ccnm internal exec-serve --payload <NativeOpenPayload>`。没有监听端口，别的 OS 用户没有东西可连；每会话一条连接由 Codex 保证（实测传输程序死掉后它不再启动第二个，第二条命令报 `exec-server transport disconnected`）。传输程序不解析方法。
- **P23.2** 每会话 CODEX_HOME：Codex 只从 `CODEX_HOME/environments.toml` 读传输配置，而 profile 目录是登录凭据所在、同一实例的多个会话共用，不能往里写会话相关文件。所以每个原生会话在 session 目录下生成自己的 `codex-home/`：`environments.toml`（`default = "ccnm"`、`include_local = false`、`program` 指向本机 ccnm）；`auth.json` 是指向 profile 里 `auth.json` 的 symlink（实测 Codex 读写都穿过 symlink：token 刷新写回 profile，0600 保留，symlink 不变）；`config.toml` 只写一条对 Runtime 根的 `trust_level = "trusted"`（不写的话每个会话都弹一次信任提示，`-c projects.<根>.trust_level` 命令行覆盖实测压不住它）。ccnm 不读、不复制凭据内容；profile 本身的校验不变。
- **P23.3** 启动接线：opt-in 就是 Runtime 上已有的 `codex_exec_server = true`，Agent 从 `runtime-resolve` 得知（报告新增 `codex_exec_server` 字段；打开时 `root` 改报 canonical 路径——Codex 用 `-C` 拼出的每条 URI 都按它来，规则表按 canonical 判）。只对 Codex Agent 生效，同一 workspace 的 Claude 会话仍走 MCP，不打开时行为不变。打开后 Codex 以交互模式启动：`-C <Runtime 根>`、`--sandbox workspace-write`、不注入 ccnm MCP server，`shell_tool` / `unified_exec` / `unified_exec_tty` 三个 feature 保持打开（P21.2 实测的组合），其余仍关。**print 模式在创建 session 前拒绝**（P21.1：`codex exec` 要求 Agent 本机有同一路径），不静默退回 MCP 或 Agent 本机执行。创建会话前先经 `exec-serve` 做一次空会话预检：Runtime 没 opt-in、没 `codex_bin`、版本不对，都在起 Codex 之前报出来。
- **P23.4** 离线端到端（零额度）：真实 Codex 0.154.0 + 本机假模型接口 + 禁出站沙箱，经 environments.toml → `ccnm internal exec-serve` → 真 exec-server 完成读、改、跑命令；Runtime 拒绝的请求（批准提权后的命令）在 Codex 里表现为工具失败、不退回本机执行；传输断线后第二条命令哪里都没执行、Codex 不重启传输程序；Runtime 连不上时同样只报失败；会话期间 Codex 进程树没有监听端口。SSH 那一跳用本机 TCP 管道代替，原因写在记录里。Rust 与 Python 门禁通过。

停止点：离线闭环可复现。不部署、不跑真实模型、不改默认执行方式。

### P24 — 原生链真机验收

**依赖 P23。在 Runtime 上装 Codex、替换任何机器上已装的 ccnm、真实模型回合，都要针对该动作单独授权。**拓扑、现场事实和逐项授权清单见[会话计划](p24-native-real-machine-session.md)。

- **P24.1** 授权的双机环境里，Runtime 以 ccrun 运行 exec-server，Agent 侧真实 Codex 完成一次读 → 改 → 跑测试 → 看结果的小任务；进程属主是 ccrun，Runtime 执行身份上没有 Agent 凭据和出站私钥（沿用 P12.2 的检查）。Runtime 是 Linux 时先确认装了 bubblewrap、ccrun 能创建 user namespace（P21 实测缺任一项沙箱都起不来）。
- **P24.2** 跨入口：原生链 coding 会话与 Managed MCP、外部 MCP 的 coding 会话竞争同一把写锁，只有一个拿到。
- **P24.3** 故障：Agent 侧断网、SSH 断开、exec-server 被杀、Runtime 上有子进程残留，写锁移交相关的各重复 20 次、其余各 5 次；未确认退出不放锁，断线不 resume、不重放。
- **P24.4** 文档：支持矩阵、使用说明、运维手册写明原生链的平台、版本 pin、与 MCP 路径的区别和已知限制；模型回合计入 toexec v2 第 10.1 节的累计额度并记进 evidence。

停止点：原生链成为 opt-in 的可用能力。要不要改成默认，另做决定。

### P25 — MCP 握手失败时保留远端报的错误码

**依赖 P24。用户 2026-09-16 指定立项，起因是 P24 真机轮（[记录](../research/p24-native-real-machine-2026-09-16.md)第五节）。**写锁被别的会话占着时，从 Agent Node 起会话，`ccnm run` 报 `CCNM_E_RUNTIME_UNREACHABLE`（退出码 21），真正的原因 `CCNM_E_POLICY` / `workspace write guard is busy` 只出现在正文的 `stderr:` 后面。人读得懂，按错误码判断的程序会误判成"连不上 Runtime"。根因在 `mcp::probe`：`initialize` 失败一律用调用方给的"不可达"码包起来，不看远端 stderr 第一行已经写明的 `CCNM_E_*`。P7 以来就这样，不是原生链引入的。

两份冻结契约都已经写着正确答案，所以这是实现向契约靠拢，不改协议：机器协议 `-32005` 只表示"Agent 到 Runtime 的 SSH 不通"、`-32007` 是策略拒绝；Remote Workspace MCP 第 11.3 节把写入互斥 busy/unknown 列在 `CCNM_E_POLICY` 下。`ssh.rs` 的 `remote_failure` 对一次性远端命令早就按 stderr 首行保留错误码，握手这条路没跟上。

- **P25.1** 先红：经真实二进制复现。Runtime 侧起一个真实 `internal mcp-serve` 握着写锁；Agent 侧真实 ccnm 经假 ssh 打到同一 Runtime 状态上的真实 `mcp-serve`，分别走 `internal agent-run`（与 `ccnm run` 同一个 `provider_runtime_preflight`）和 Agent 侧 `ccnm mcp probe`（doctor「远端 MCP 握手」行用的同一个 `mcp_handshake`）。断言退出码 33、stderr 首行 `CCNM_E_POLICY:`、正文同时带传输命令和 `write guard is busy`。修复前这组测试是红的，红的输出记进证据。
- **P25.2** 修复：`initialize` 失败时，传输 stderr **首行**是已知 `CCNM_E_*` 名，错误就带这个码，消息仍保留传输命令、握手错误和 stderr 尾部；首行不是（ssh 自己的失败、不是 ccnm 的进程）、超时、spawn 失败，分类一律不变。"首行是不是错误码"只写一处，`ssh.rs` 与 `mcp::probe` 共用；按完整 stderr 的首行判，不按截到 4 KiB 的尾巴判。`probe()` 的三个调用方逐个核对：`mcp_probe_local`（Runtime 本机，原来归 `Internal`）、`mcp_handshake`（doctor 的远端 MCP 握手行、Agent 侧 `ccnm mcp probe`）、`provider_runtime_preflight`；`native_runtime_preflight` 经 `remote_failure` 本来就保留错误码，不改。
- **P25.3** 文档：排错手册里"`ccnm run` 报 `CCNM_E_RUNTIME_UNREACHABLE`，正文里却写着 `workspace write guard is busy`"那一条按新行为改写；运维手册、排错手册里 doctor 示例中因远端拒绝而写成 `CCNM_E_RUNTIME_UNREACHABLE` 的握手行改成新码；删掉 `status.json` 里对应的 observed_gaps 条目。协议文档核对后不改，理由写进证据。
- **P25.4** 门禁：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`；`check_plan`、`check_protocol` 与其单测、`git diff --check`。

停止点：只改握手失败时错误码怎么归类。不补 `session.start` 的占用预检（`-32008` 仍不可达，那是待定的产品决定）；RPC 后台运行失败仍只记消息不记码，不改；不改 doctor 行的结构和 ssh 失败、超时的分类；不跑真机——真机复验要替换已装二进制，需要单独授权。

### P26 — 原生链：Runtime 侧探活与无响应超时

**依赖 P25。用户 2026-09-16 在 P24 暴露的三条路里选了"ccnm 加空闲超时"。**（编号：立项时记作 P25，提交 4dae491、1d6ac20 的消息里的 P25 指的就是本阶段。P24 之后四个阶段几乎同时立项，照 P20 的先例按开工先后排：握手错误码 23:58:30 是 P25，本阶段 23:59:18 是 P26，doctor 探原生链 P27，MSRV CI P28。P25 合并进 main 之后依赖改为 P25。）P24 黑洞 5/5：Agent 静默离网时 `exec-serve` 察觉不到，锁一直由已消失的会话持有。MCP 入口没有这个问题，因为 `mcp::server::HEARTBEAT` 每 30 秒往连接上写一次 ping——对面进程没了而机器还在，内核回 RST，sshd 退出，stdin 读到 EOF。exec-server 协议里没有给客户端的 ping，但 Codex 0.154.0 的客户端对服务端发来的、它不认识的**请求**一律回 `-32601`（源码 `exec-server/src/client_recovery.rs`；不认识的**通知**则会让它断连），所以可以借它探活。

- **P26.1** 实测（零额度）：真实 Codex 0.154.0 经 environments.toml 传输连本机 `exec-serve`，空闲期间收到探活请求时回 `-32601`、不断连、TUI 上没有提示；多次探活之后工具调用照常。结果不符就停，回到设计。
- **P26.2** 实现：`exec-serve` 在客户端静默满 30 秒（与 `HEARTBEAT` 同值）时发一个 `ccnm/liveness` 请求（字符串 id，不与执行端的请求撞号）；回复由 ccnm 消费，**不转给 exec-server**。客户端连续 10 分钟没有任何字节到达，并且也没有一条多块大消息在向它推进，就按正常关闭路径结束会话（关 exec-server stdin → 等退出 → 扫进程 → 放锁），stderr 写明原因；往客户端写失败同样结束。只在写锁移交上加一条"确认对面不在了"的途径，放锁条件不变：扫不干净仍然 `held`。
- **P26.3** 测试：探活状态机是纯函数、单测覆盖（回应的空闲客户端不结束；不回应的在超时后结束；任何客户端字节重置计时；慢速大消息推进中不误判，卡住不推进的才判）；转发循环能注入计时参数，在 core 里用假 exec-server 和管道客户端跑：回应探活的会话活过多个周期、探活回复没有到达执行端、不回应的会话超时后锁 `released` 且无残留。真实二进制按默认值（30 秒 / 10 分钟）各跑一次不回应的客户端，看 stderr 和锁。Rust 全量门禁通过。
- **P26.4** 文档：运维手册"静默离网之后锁一直 held"一节按新行为改写（多久自动释放、什么时候仍要人工）；支持矩阵的限制行、配置说明、排错手册同步；代价写清楚——Agent 机器睡眠或断网超过 10 分钟，原生会话会被结束，Codex 里需要 `/exit` 重开。

停止点：离线与本机真实 Codex 验证。**hpsrv 上的黑洞复测不在本阶段**：P24 的一次性公钥已撤，重做要重新授权。

### P27 — doctor 探 Codex 原生链

**实际只依赖 P24。**（编号说明：P24 之后有几个阶段几乎同时立项，各自都记作 P25——「MCP 握手失败时保留远端报的错误码」（分支 `claude/jovial-ramanujan-60da56`，57658de，23:58 开工）、「原生链 Runtime 侧探活与无响应超时」（本地 `main`，4dae491，23:59 开工），以及比本阶段晚开工的「CI 在声明的 rust-version 上编译一遍」（分支 `claude/quirky-faraday-99ba46`，00:10 开工）。照 P20 的先例按开工先后排：握手错误码 P25、探活 P26、本阶段 P27、MSRV P28。合并进 main 后路线图顺序上排在 P26 之后，`depends_on` 随之改为 P26。）P24 真机轮发现（[记录](../research/p24-native-real-machine-2026-09-16.md)第九节）：`codex_exec_server = true` 的 workspace，`ccnm doctor` 只做 MCP 握手；Runtime 节点没配 `codex_bin`、Codex 版本不是 0.154.0、exec-server 起不来，都要到 `ccnm run` 的预检（`native_runtime_preflight`：一次空的 `exec-serve` 会话，stdin 立刻关闭）才报出来。

- **P27.1** 探测与行：`internal probe` 的请求带上 workspace 的 `codex_exec_server`（只在为真时发送，其余请求逐字节不变）。开了它、Agent 是 Codex、反向 hello 通过时，probe 调 `ccnm run` 用的同一个 `native_runtime_preflight`，结果放进报告的新字段（没跑就不出现）。判断条件跟 Runtime 安全、MCP 握手两行一样只看 hello，不看 MCP 握手成没成：doctor 要把能查的都列出来，两行各报各的。Runtime 侧和 Agent 侧 doctor 表都多一行（英文 `Codex exec-server`，中文 `Codex 原生链`）：空会话成功是 OK；失败是 FAIL，带远端报的 `CCNM_E_*`；没开、不是 Codex、前提没过、对端 build 不报这个字段时是 SKIP 并写明原因。这一行在所有固定行集合里都出现（Agent SSH 失败、反向 SSH 失败、没有 Agent、同机、Runtime 没回答），所以不用这条链的 workspace 表里也多一个 SKIP：退出码不变（原本就有两行固定 SKIP），「N 项没查」加 1。Runtime 侧调 `internal probe` 的外层超时加上预检自己的上限。
- **P27.2** 测试：单测覆盖行的选择（上面每种 SKIP、OK、FAIL）和中英两种渲染；集成测试用 `tests/fixtures/fake_exec_server.py`，经真实二进制的 `ccnm internal exec-serve` 跑 `work::probe` → `doctor::from_agent`：正常时 OK；`--version` 不对时 FAIL `CCNM_E_VERSION`；写锁被一个活着的 `exec-serve` 会话占着时 FAIL `CCNM_E_POLICY`（busy）；探完写锁回到 `released`，执行端没收到任何请求。
- **P27.3** Linux 前提不进 runtime-audit，只留在文档（运维手册已列）。理由：(1) Codex 0.154.0 找 bwrap 的地方有两处——PATH 上的系统 bwrap，或它自带的 `codex-resources/bwrap`（[P21 记录](../research/p21-codex-native-surface-2026-09-16.md)第 5 节的报错原文），后者放在哪才算数没实测过（P24 会话计划 A2′），照猜的路径查会把能用的机器报红；(2) 「能不能建 user namespace」不是一个开关能读出来的：P24 在 hpsrv 上看了 `kernel.unprivileged_userns_clone`、`user.max_user_namespaces` 和 AppArmor 三处，P21 在容器里是 seccomp 挡住的、那里任何 sysctl 都看不出来——读 sysctl 会在容器里报 OK 而 bwrap 实际起不来，这正是 doctor 最不能犯的错（没证实的东西读成通过）；唯一靠得住的是真去建一次沙箱，那要照抄 Codex 自己的 bwrap 参数，参数属于 Codex、随版本变；(3) 缺了它是失败即拒：命令和带 sandbox 的文件方法都不执行，Codex 随后的「不带沙箱重试」被规则表按 `sandbox: null` 拒掉，是可用性问题不是越权；(4) runtime-audit 是所有入口、所有平台共用的安全审计，结论喂给会话的闸，不该混进一条平台加链路专用的猜测。代价是 Linux 上缺前提时 doctor 这一行照样 OK，要到第一条命令才看到 bwrap 的报错——行的 detail 和排错手册都写明这一行不证明沙箱。
- **P27.4** 文档与门禁：usage、troubleshooting、support-matrix 里讲 doctor 行的地方写上这一行、它和 MCP 握手一样会取放写锁（有人在写时报 busy，所以别在会话进行中拿它判断链路）、它不证明 Linux 沙箱；删掉 `status.json` 里对应的 observed_gaps 条目。`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`python3 scripts/check_plan.py`、`git diff --check`。

停止点：doctor 多一行，别的不动——不改 `exec-serve` 本身、规则表、runtime-audit 的 wire 和 MCP 握手的错误码分类（后者是另一个分支的 P25）；不跑真机、不耗额度、不换任何机器上的二进制。

### P28 — CI 在声明的 rust-version 上编译一遍

**依赖 P24。用户 2026-09-17 指定立项，起因是 toexec v2 计划第 11 节"对齐检查"第 5 行。**（编号说明：P24 之后有四个阶段在不同分支上几乎同时开工，开发时都叫 P25。按 P20 的先例以开工先后排：握手错误码（23:58）、原生链探活（23:59）、doctor 探原生链（00:05）、本阶段（00:10），所以本阶段是 P28，提交 f8df7e4、2a4190b、31a2d28 消息里的 P25 指的就是它。合并进 main 后排在 P27 之后，depends_on 已改为 P27。）计划要求 ccnm、gld、toexec 统一 `rust-version` 时各加一个 MSRV（最低支持的 Rust 版本）CI 任务：`cargo +<版本> check --workspace --all-targets --locked`。三仓都已是 1.89，任务没加。ccnm 的 CI 只跑 stable，代码里用了比 1.89 更新的 std API 照样全绿，要等有人拿 1.89 编译才炸。gld 和 toexec 的对应改动记在各自仓库，这里只管 ccnm。

- **P28.1** CI 加 `msrv` job：版本从根 `Cargo.toml` 的 `rust-version` 读，不在 workflow 里再写一遍，读不到或读到不止一行就失败；装该版本后跑上面那条命令；缓存 key 与 stable 的 job 分开。只跑一个平台，前提是全仓按 OS 分支的代码只有 `native::serve::marked_processes`（Linux 读 `/proc`，其他平台调 `ps`），也没有按平台区分的依赖；以后加了平台专属代码或依赖，要重新判断。
- **P28.2** 推送前本机验证：1.89 上 check 通过且没有警告，本机 macOS 目标和 Linux 目标各一遍；依赖闭包里没有声明高于 1.89 的 crate（有就停下报告，不悄悄升）；从全新 clone、空 `CARGO_HOME`、没有 git 凭据的环境匿名拉到 toexec 的 tag 并通过 check。
- **P28.3** 推送后 GitHub Actions 上 `msrv` job 通过。推送要用户批准。

停止点：rust-version 有了 CI 门禁。以后升级仍按 toexec 计划第 11 节三仓同步，提交说明写明是哪个依赖或 std API 要求。

### P29 — 原生链补测：同会话并发、在途请求与资源上限

**依赖 P28。起因是 toexec v2 计划第 11 节"对齐检查"第 7–9 行：原生链上 V2-G07 的"同会话并发修改串行"和 V2-G08 的"在途超时"没测，V2-G09 资源上限没做。**门禁定义不改。先按 Codex 0.154.0 源码把每个子项落到"归谁管"，再用真实 `exec-serve` + 真实 `codex exec-server` 零额度实测 ccnm 那一份。开工前读源码（tag `rust-v0.154.0`）已知的：exec-server 对同一连接的请求并发处理（`exec-server/src/server/request_dispatcher.rs` 的信号量），发给客户端的消息走容量 128 的有界通道（`connection.rs`）；每个进程只留最近 1 MiB 输出，结束后再留 30 秒（`local_process.rs` 的 `RETAINED_OUTPUT_BYTES_PER_PROCESS`、`EXITED_PROCESS_RETENTION`）；`fs/readFile` 把整个文件放进一个回包，上限 512 MiB（`local_file_system.rs` 的 `MAX_READ_FILE_BYTES`）；Codex 客户端只给 `environment/info`、`environment/status` 设了超时，文件和进程请求一直等回包（`rpc.rs` 的 `call_with_timeout` 只有 `client.rs` 这两处调用）；工具默认不并行（`tools/src/tool_executor.rs`），`apply_patch` 没改默认，执行时拿每轮一把读写锁的写锁（`core/src/tools/parallel.rs`），`exec_command` 声明可并行。

- **P29.1** 适用性表：G07、G08、G09 的每个子项写明归谁（ccnm 转发层、exec-server、Codex 客户端、不适用）和依据（源码位置或本阶段实测），写进研究记录；toexec 对齐检查第 7–9 行改成指向它。实测推翻上面任何一条源码结论，先改表再往下测。
- **P29.2** 并发：(1) 同一连接不等回包连发一批请求——放行的读、写、起进程，加上 ccnm 要拒的越界写和提权命令——同时有命令在持续输出：客户端收到的每一行都是完整 JSON，每个请求 id 恰好一个回答，被拒的请求执行端没见到、磁盘上没有副作用。能用假执行端表达的部分进 CI 测试，真执行端本机跑。(2) 真执行端上两个 `fs/writeFile` 同时写同一路径（长短不同的两份内容），20 次：记录最终文件是不是其中一份的完整内容。要不要在 ccnm 里把写方法串起来，看结果另做决定，本阶段不改。
- **P29.3** 在途请求：(1) 一个还没回的请求（`process/read` 带很长的 `wait_ms`，等一个不出声的命令）在途时客户端断开：会话在 `EXIT_WAIT` 加 `SWEEP_WAIT` 之内结束、锁 `released`、没有带会话标记的进程，执行端日志里这条请求只出现一次，20 次。(2) `process/terminate` 一棵正在跑的进程树（同进程组的子孙，加一个 `setsid` 脱离的）：同组的 5 秒内消失、`process/read` 报 exited 和 closed；脱离的那个活到会话结束，被扫掉后锁才放，5 次。
- **P29.4** 资源：(1) 命令连续输出 200 MiB、客户端照常读：客户端解出的字节数恰好 200 MiB，记下耗时、`exec-serve` 和执行端的峰值 RSS（按 `ps` 采样）；中途 60 秒不读再恢复：这段时间命令不前进、`exec-serve` 的 RSS 不涨，恢复后读完。5 次。(2) `fs/readFile` 读 200 MiB 文件：`exec-serve` 峰值 RSS 与 (1) 同一量级（逐块转发，不把整行读进内存）；大于 512 MiB 的文件拿到执行端原样的错误。(3) 写：base64 之后单行不超过 32 MiB 的 `fs/writeFile` 成功，超过的结束会话（P22 已有测试，这里只复核）；工作区放在用户级挂载的 16 MiB 磁盘映像上（测完卸载删除）写满时，`fs/writeFile` 拿到错误回包、会话继续、之后的小文件写入成功、结束时锁放掉。(4) 过期引用：一条很快结束的命令，30 秒内 `process/read` 仍拿到全部输出，超过 30 秒拿到执行端的错误；`fs/close` 之后的 `fs/readBlock` 报错。这些都应是原样转发，ccnm 不改写。
- **P29.5** 记录：研究记录 `docs/research/p29-native-gates-2026-09-17.md`，脚本与每轮 `summary.json` 放 toexec `evidence/v2-c/p29-gates/`。用户会撞上的上限（原生链单文件写入上限、磁盘写满时的表现）写进支持矩阵或排错手册，只写一处。改了 Rust 就跑全量门禁。

停止点：只测和记录，新增的测试只钉住现有行为。发现的 ccnm 缺陷记进 observed_gaps，不在本阶段修，修复另立阶段；不改规则表、探活计时和冻结协议，不跑真机、不耗额度、不换任何机器上的二进制。

### P30 — 原生链：放锁前清空执行端的进程组

**依赖 P29。起因是 [P29 记录](../research/p29-native-gates-2026-09-17.md)第 5 节查出的缺陷。**exec-server 做带沙箱的文件读写（`fs/writeFile` 等）时自己起 `codex --codex-run-as-fs-helper`，先 `env_clear()` 再只放回 `PATH`/`TMPDIR`/`TMP`/`TEMP`，所以 helper 不带会话标记。exec-server 在一次这样的操作中被强杀时，helper 被挂到 pid 1 继续运行；`exec-serve` 按标记扫不到它，写 `released`，之后 helper 的写入才落地（macOS 20/20）。客户端正常断开时 tokio 的 `kill_on_drop` 会杀掉它，只有强杀时漏。P29 实测 helper 与 exec-server 同一个进程组（ccnm 以 `process_group(0)` 启动 exec-server），exec-server 死后这个组还在。

- **P30.1** 先红：假执行端加一个开关，收到指定方法时先起一个清空环境、留在自己进程组里的子进程（扮演 helper），再立刻崩溃；经真实二进制的 `exec-serve` 跑 20 次（写锁移交相关的故障点），断言会话结束时那个子进程已经不在、下一个会话能开。修复前这组测试是红的，红的输出记进证据。
- **P30.2** 修复：收尾扫描的判据从"带这个会话的标记"扩成"带标记，**或**进程组号等于 exec-server 的 pid"，两条在同一次进程表扫描里判，找到的逐个杀掉、再扫，扫不干净仍然 `held`。按 OS 分支的仍只有这一个函数（Linux 读 `/proc/<pid>/stat` 的进程组字段，其他平台 `ps` 多取一列 `pgid`），所以 P28 的 msrv job 只跑 Linux 的前提不变；`/proc/<pid>/stat` 的解析写成不分平台的纯函数，本机单测覆盖命令名里带空格和括号的情况。进程组号被复用的风险写清楚：只在组已空、同一个号又被新进程拿去当组长的几秒窗口里存在，与按 pid 杀标记进程时 pid 被复用是同一量级。
- **P30.3** 实测：修复后的 release 构建重跑 toexec `evidence/v2-c/p29-gates/` 的 `helper-crash`（20 次，期望 helper 在放锁前已不在），以及 `helper-close`、`client-leaves`、`terminate` 做回归。Linux 上 helper 由 `bwrap --new-session --die-with-parent` 启动，不在这个进程组里，靠 die-with-parent 随 exec-server 结束——按源码说明，不实测（本机和 CI 都没有 Linux 上的 Codex）。
- **P30.4** 文档与门禁：`serve.rs` 开头讲"怎么证明进程都没了"的注释、P29 记录第 5 节、支持矩阵里的"已知缺陷"、`status.json` 的对应 observed_gaps 条目按新行为更新。`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`--target x86_64-unknown-linux-gnu` 的 check（覆盖 Linux 分支）、`python3 scripts/check_plan.py`、`git diff --check`。

停止点：只改收尾扫描的判据。不改规则表、探活、放锁的其他条件，不跑真机、不耗额度、不换任何机器上的二进制。

### P31 — Runtime 保留输出：会话总量上限、结束即删、过期清理

**依赖 P30。**（编号说明：开工时记作 P31，与另一分支上同日更早开工的 P31、P30 撞号，照 P20 和 P25–P28 的先例按开工先后顺延为 P31；提交 1f7aafe、b86d599、d5353cb、b3c4ce5、75061ca 消息里的 P31 指的就是本阶段。）**用户 2026-09-17 指定立项，起因是协议第 8 节保留输出的措辞更正（32ec5fd，经 4aeb641 合并）。**更正时核实的现状（`crates/ccnm-core/src/mcp/exec.rs` 的 `Sink`、`prune`，2026-09-03 起没改过）：stdout、stderr 每次运行各自最多落盘 64 MiB；每个 session 留最新 100 次；没有 session 级字节上限，一个 session 最坏留 100 × 2 × 64 MiB = 12800 MiB（12.5 GiB）。toexec V2-P0 基线里一个 session 连跑 5 次输出 64 MiB 的命令，Runtime 上就留了约 320 MiB。更大的口子在跨 session：Runtime 侧没有任何清理，占用随 session 个数一直累加；外部 MCP 的 `sessions/bridge-<uuid>/` 在 Agent 上没有记录，`ccnm workspace remove --purge` 一个也删不到，现在只能照运维手册手动 `rm`。三件事放在一个阶段里做，是因为只加单 session 上限的话，总占用照样没有上限。

**两个值用户 2026-09-17 已定，就用提议值**：(1) 会话总量上限 256 MiB，出自 toexec v2 计划第 9 节，那里写明是"首轮拟定上限，不是已测试保证"；(2) 已结束会话的输出留 7 天。第 (2) 条改变了"Runtime 输出不会被自动删"的现状，但会话结束后 ccnm 本来就没有命令还能读它：Runtime 上的 `sessions/<id>/output/` 只有 `read_output` 读，而它只认本 session 的目录。两个值都做成常量，不做配置项。

- **P31.1** 会话总量上限：一个 session 所有**已结束**运行的 stdout + stderr 合计不超过上限。每次运行结束后，从最旧的已结束运行开始整份删，直到合计不超过上限；刚结束的这次不删（单次最多 128 MiB，一定放得下）。100 次的上限保留。**进行中的运行永远不删，跨进程也成立**：rmcp 3.2.0 每个请求单独起一个任务，同一 session 的 `exec_command` 可以并发；Managed 会话 `/mcp Reconnect` 会用同一个 session id 起新的 `mcp-serve`，新旧两个可能同时在。持有者进程已经不在的运行不算进行中，否则崩溃留下的运行永远删不掉。现有 `prune` 按目录修改时间删、不看运行结没结束，一并改掉：按代码推断，同 session 里一条长命令还没结束、期间又开始了 100 次运行，它的目录就会被删（未复现；Claude Code 一轮的并行调用到不了这个数，别的 Host 不一定）。并发时总量可以暂时超过上限，超出部分不超过"进行中的运行数 × 128 MiB"，协议里照实写。被删运行的 `output_ref` 报的错与现在按次数删的一样（`CCNM_E_INVALID_ARGS`，`no output kept for r-…`），不加错误码。删除失败不影响命令结果，只写进 stderr 诊断。
- **P31.2** 外部 MCP 会话结束即删：外部入口（按 payload 的协议号判，不按 `bridge-` 这个名字）的 `mcp-serve` 结束时，删掉**本进程建的**运行，目录空了再删 `sessions/<id>/output` 和 `sessions/<id>/`（都不递归）。只删自己建的，因为 session id 来自对端：一个只读客户端报了别人的 id，断开时不该能删掉别人的输出。依据是协议第 6 节：一个 bridge 进程就是一个 session，断了不重连，手里的 `output_ref` 跟着作废。Managed 会话**不**在 `mcp-serve` 退出时删：`/mcp Reconnect` 之后新的 `mcp-serve` 还用这个 id，旧的 `output_ref` 仍然有效，删了就是把一个还活着的会话的输出弄丢。
- **P31.3** 过期清理：`mcp-serve` 启动时扫一遍本机 `sessions/`，某个 session 的 `output/` 同时满足三条才删——最新一次运行已超过保留期；本机没有服务这个 session id 的 `mcp-serve`；进程枚举本身成功了。`overview::scan_servers` 在 `ps` 失败时返回空列表，直接拿来用会把"查不到"当成"没人在用"，要换成能区分失败的版本，失败就一个都不删。只删 `output/`，删完 `sessions/<id>/` 空了才删这一层（非递归）：Agent 的会话记录和 Runtime 的输出用的是同一种路径 `sessions/<id>/`，一个状态目录两种角色都当的时候，那一层里还放着 Agent 的记录。扫描出任何错都不影响这次会话启动。
- **P31.4** `--purge` 不改，限制写进运维手册（用户 2026-09-17 开工后决定，原先的 P31.4 是"`--purge` 按 workspace 记录删本机的 session 输出"）。开工后核实原来的前提不成立：推荐部署里 Operator 和 Runtime Executor 是两个账号，输出在执行账号的状态目录，而 `ccnm workspace remove --purge`（`launcher::purge`）删的是**敲命令那个账号自己**目录里、Agent 报回来的会话——不光 bridge 会话，Managed 会话的 Runtime 输出也删不到，照原计划按记录在本机删结果一样。真要删到得走 Operator → Agent → Runtime Executor 两跳，要改 `agent-purge` 的内部 wire，而 `PurgeRequest` 拒绝未知字段，两端版本不一致时整个 `--purge` 会失败。P31.2、P31.3 已经让输出不会无限累积，所以只在运维手册写明这个限制和手动删的办法，observed_gaps 记一条。
- **P31.5** 测试：两个上限做成可注入参数，单测用 KiB 级的小值（真写 64 MiB 太慢）。覆盖：单流超限时截断、管道照样排空、命令照常结束、结果带说明（补上现在完全没有的测试）；按次数删掉的是最旧的（现在的测试只数个数）；按字节删最旧的已结束运行，刚结束的和进行中的都不删，另一个进程持有的进行中运行也不删，持有者已经不在的照删；过期清理三个条件缺任何一个都不删，`ps` 失败不删，只删 `output/`。经真实二进制（`cargo test -p ccnm-cli --test external_mcp`）：bridge 会话正常结束后目录没了；Managed 形状的 session 在 `mcp-serve` 退出后目录还在，同一个 id 再起一个 `mcp-serve`，旧的 `output_ref` 仍能读。
- **P31.6** 文档与门禁：协议第 8 节"保留输出"一行和下一段按新行为改写（会话总量上限、并发时的暂时超出、bridge 结束即删、过期清理）；运维手册手动 `rm` 那一段改成新行为，写明"`ps` 跑不了时不删"和 P31.4 的 `--purge` 限制。证据里写明为什么不升 `ccnm.workspace-mcp/2`：契约从没承诺保留时长，被删 ref 的错误码和消息不变，只是删得更早。门禁：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo test -p ccnm-cli --test external_mcp`、`python3 -m unittest tests.test_remote_workspace_mcp -q`、`python3 scripts/check_protocol.py` 与 `python3 -m unittest tests.test_check_protocol -q`、`python3 scripts/check_plan.py`、`git diff --check`。

停止点：只管 Runtime 上的 `sessions/*/output/`。不做整台机器的总配额（toexec 计划第 9 节写明"另定"）；不改单流 64 MiB 和 100 次这两个现有上限；不清 Agent 侧会话记录和 `rpc/` 记录；不做配置项；不跑真机、不换任何机器上的二进制。

### P32 — 封存 Codex 原生执行链

**依赖 P31。用户 2026-09-17 决定。**（编号说明：立项时记作 P31；并行分支上 13:30 开工的「Runtime 保留输出」按开工先后排在前面成为 P31，本阶段顺延为 P32、随后的沙箱阶段为 P33，提交 35aeadf 消息里的 P31/P32 指的就是这两个。）决定本身、依据、封存后的行为和解封条件写在[双执行入口方案](runtime-surfaces.md)第 12.0 节，这里只列要改的东西和停止点。一句话：收益没量过，量过的那项（OS 沙箱）`codex sandbox` 不经 RPC 就能拿到，只开交互模式让 Machine API 用不上它，而每次 Codex 升级都要重做 P21 的规则表——不值。最终目标定为三种客户端（Claude Code、Codex、Web AI 经 gld hub）× 三种操作系统，只留一条执行路径才做得到。

- **P32.1** 决定入档：runtime-surfaces 第 12 节加封存小节；AGENTS.md 告诉后续模型别再投入，并指向跨仓库目标。
- **P32.2** 用户文档：支持矩阵那一行状态改成封存、证据保留并标明只对 0.154.0 成立；配置说明、README、使用说明、运维手册、排错手册里介绍这条链的地方各加一句封存和指向，原因只写在 12.0 一处。
- **P32.3** 跨仓库：toexec v2 计划第 0 节改成"三种客户端都走 MCP + 共享库"，写入最终目标和当前覆盖表（客户端 × 操作系统，事实以各仓库支持文档为准）；第 8、11 节 V2-C 状态改封存；toexec README 同步。gld RFC-0002 头部那句和第 9 节补记指向新状态。
- **P32.4** 取消的事写清楚：hpsrv 黑洞复测、Linux 上 fs helper 实测、为原生链发版并替换机器上的二进制，都不做；交接里对应的待办删掉。`python3 scripts/check_plan.py`、`git diff --check`。

停止点：不改代码、不删代码，CI 里原生链的测试照跑；版本门、opt-in 开关、规则表都不动。

### P33 — MCP 路径的 `exec_command` 加 OS 沙箱

**依赖 P32。**原生链唯一实测过的额外收益是命令有 OS 沙箱；toexec V2-P1 证明 `codex sandbox --sandbox-state-json '{"permissionProfile":…,"sandboxCwd":…,"workspaceRoots":[…]}' -- argv` 不经 RPC 就挡住同一集合（工作区外写、HOME 写、`.git` 写、网络；macOS Seatbelt 实测，多约 30 ms）。把它搬到两个入口共用的 `exec_command` 上，Claude、Codex、Web AI 三种客户端都拿到。

先要定、不能默认的三件事（**用户 2026-09-17 按建议定了**：opt-in、默认不变；Linux 先在本机容器测；被挡就报失败，不给"不带沙箱重试"的路）：

1. **默认路径要不要依赖 Codex 二进制。**现在 MCP 路径的 Runtime 不需要 Codex；用 `codex sandbox` 就需要。备选是直接调 `sandbox-exec`（macOS）/ `bwrap`（Linux）自己生成 profile——那等于把 Codex 的沙箱策略代码抄一遍，随它版本漂。建议先做成 per-workspace opt-in（例如 `exec_sandbox = "codex"`，要求节点有 `codex_bin`），默认不变。
2. **Linux 前提。**Codex 的 Linux 沙箱要 bubblewrap 和 user namespace（P21）；V2-P1 只在 macOS 上测过 `codex sandbox`。Linux 那一半先在本机容器里测（P21 的做法）。
3. **合法操作被挡怎么办。**V2-P1 实测沙箱里 `git commit` 失败（`.git` 只读）。`exec_command` 现在能跑的东西（构建、测试、`git commit`）哪些会被挡要先列出来；挡住了是报错，还是给模型一条"不带沙箱重试"的路——后者等于没有沙箱。

- **P33.1** 实测清单：用 `codex sandbox` 包一套日常项目操作（cargo 构建/测试/运行、cold cache 构建、git 只读与 commit、node、python、各种目标的写、网络、`ps`），对照直接跑，记下哪些被挡；macOS 本机，Linux 容器。顺带量 `.git` 可写和网络放开两个变体，只为决定用。
- **P33.2** 按结果定开关形状和默认值（定为 workspace 字段 `exec_sandbox = "off" | "codex"`，权限对象取 Codex 自己那份、一字不改），写进配置说明和支持矩阵；实现时沙箱起不来和命令失败要分开报，不能把前者报成后者。
- **P33.3** 离线测试：有沙箱时工作区外写、HOME 写、网络被挡且有具名错误；`codex_bin` 缺失或版本不对时按开关语义拒绝；`cargo test --workspace` 及全部门禁。
- **P33.4** 版本关系写清楚：`codex sandbox` 的参数和 profile 形状也是按 0.154.0 实测的，同样受版本 pin 约束；比原生链省下的是协议、规则表、监督进程和 fs helper 那一整层，不是版本核对。

停止点：opt-in、默认不变；不跑真机、不耗额度、不换任何机器上的二进制。

### P34 — `apply_patch` 日志锁：探测完显式放锁

**依赖 P33。起因是修两条并发测试的分支（ecstatic-bose，2026-09-17 合并）顺带查出的产品缺陷，记在 status.json 的 observed_gaps。**`apply_patch` 开工前先看状态目录里有没有上一次被打断的提交记录（journal）；判据是它的 flock：`still_running` 打开文件、`try_lock` 拿到就说明写它的进程已经不在，然后靠关文件放锁。可是 flock 挂在打开文件描述上，关文件只在**所有 fd 副本都关掉后**才放锁，而同一个 `mcp-serve` 里 `exec_command`/`list_files` 会 fork，fork 出的子进程在 exec 之前就持有这个 fd 的副本（Rust 打开文件都带 CLOEXEC，exec 之后才没有）。fork 恰好落在探测拿锁到关文件之间时，锁被那个子进程延长几毫秒到几十毫秒；紧接着的另一次检查（同一个进程的下一次 patch，或共用状态目录的另一个 `mcp-serve`）拿不到锁，把已中断的记录当成"还在提交"而跳过，这次 patch 就放行了。只会漏拦，不会误报；被 `abandon` 保留的 Journal 在 drop 时也靠关文件放锁，同理。分支上用 Python 实测过机制：只 close 时没 exec 的子进程仍占着锁，先 `LOCK_UN` 就立即能拿；`WriteGuard` 已经是先显式 unlock 再关。

- **P34.1** 先红：不靠并发碰运气，把 fork 窗口做成确定的——探测拿到锁时，把描述符的一个副本交给一个活得比探测久的子进程（`Stdio::from(file.try_clone())` 当它的 stdin，和 fork 到 exec 之间子进程手里的那份是同一个打开文件描述），然后断言下一次探测把这份记录读成已中断；被保留的 Journal 也一样：副本交给子进程、`abandon` 后 drop，下一次探测读成已中断。测试先确认前提（只关文件时那份副本确实还占着锁），修复前这两条是红的，红的输出记进记录。
- **P34.2** 修复：`still_running` 拿到锁后先显式 `unlock` 再关；`Journal` drop 时显式 `unlock`。`LOCK_UN` 作用于打开文件描述本身，副本在谁手里都一起放掉。判据、错误码、报错文字、journal 格式都不动。
- **P34.3** 同一条记录里顺带的测试卫生：`the_write_policy_is_the_one_the_read_tools_use` 把 `outside.txt` 写在 `$TMPDIR` 根下、不带 pid，改到本测试自己的目录里。其余两件（fixture 目录跑完不删、两条墙钟阈值测试在超额负载下超时）只记录不改：前者涉及 43 个文件 86 处，要单独立阶段。
- **P34.4** 记录与门禁：研究记录 `docs/research/p34-journal-lock-release-2026-09-17.md`，observed_gaps 那条改成已修。`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo +1.89 check --workspace --all-targets --locked`、`python3 scripts/check_plan.py`、`git diff --check`。

停止点：只改放锁的时机。不改 journal 的判定规则、格式和用户看到的报错，不改用户文档，不跑真机、不耗额度、不换任何机器上的二进制。

### P35 — 测试建的临时目录跑完就删

**依赖 P34。起因记在 status.json 的 observed_gaps 和 [P34 记录](../research/p34-journal-lock-release-2026-09-17.md)第 4 节：测试的 fixture 目录没有 teardown。**目录名里带 pid 或会话 id，所以每跑一次测试二进制就在 `$TMPDIR`（和 `/tmp`——ssh ControlPath、Unix socket 路径有 103 字节上限，那几处故意放在 `/tmp` 下）留一批新的，永远不会被下一次运行覆盖。本机 `$TMPDIR` 里攒到过 18 万个 `ccnm-*` 目录、把盘写满；排查偶发失败时直接循环跑测试二进制几十上百次是这个仓库的常规做法，所以不是"偶尔清一下"能解决的。

workspace 的 lint 是 `unsafe_code = "forbid"`，也没有 libc 依赖，进程退出钩子（`atexit`）用不了；能用的机制只有 Drop。

- **P35.1** 基线：全量 `cargo test --workspace` 跑一遍，前后各数一次 `$TMPDIR` 与 `/tmp` 下的 `ccnm-*`、`cp3-*` 条目，新增数按名字模式分组记进记录。这是改之前的红灯。
- **P35.2** 一个共用的守卫类型，放在只作 dev-dependency 的 workspace 成员 `crates/ccnm-testdir` 里（单元测试、`ccnm-core/tests`、`ccnm-cli/tests` 三处编译上下文用同一份定义，且不进产品二进制）：接管一个调用方自己算好的路径，drop 时 `remove_dir_all`；**所在线程正在 panic（测试失败）时不删，并把路径打到 stderr**，留给人看现场。路径怎么起名、放 `$TMPDIR` 还是 `/tmp`、要不要 canonicalize，都还是各个 fixture 自己定——守卫只管删，测试看到的路径一个字节都不变。
- **P35.3** 把 `crates/` 下所有建临时目录或 socket 文件的测试过一遍（`temp_dir()` 86 处，加上直接写 `/tmp/ccnm-*`、`/tmp/cp3-*` 的那些）：每一处要么交给守卫，要么在记录里写明为什么不用（只拼路径、从不建东西的）。几个测试共用的"每进程一个"目录（`/tmp/ccnm-ctl-<pid>`、`ccnm-open-home-<pid>`、`/tmp/ccnm-cli-home-<pid>` 这类）不能由某一个测试的 Drop 删掉——别的测试可能还在用——改成每个测试一个。**不改任何断言，不改产品代码。**
- **P35.4** 验证与门禁：同 P35.1 的量法，全量测试后新增残留为 0；直接循环跑 `ccnm-core` 测试二进制 20 次（`--test-threads=64`）后新增残留为 0、没有新的失败——守卫删得太早、删到别的测试的目录，在高并发下才看得出来。研究记录 `docs/research/p35-test-dir-cleanup-2026-09-17.md`，observed_gaps 那条改成已修。`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo +1.89 check --workspace --all-targets --locked`、`python3 scripts/check_plan.py`、`git diff --check`。

停止点：只动测试代码和一个只作 dev-dependency 的新 crate。不改产品代码、断言、用户文档；不清理本机已有的历史残留（那是另一件事，P34 清过一次）；被 SIGKILL 或超时杀掉的测试进程留下的目录不在范围内——Drop 跑不到，没有 `unsafe` 也没有别的钩子。

### P36 — Runtime 上项目自带的 skills

**依赖 P35。用户 2026-09-17 决定：不引入第三方 harness，但 ccnm / gld 自己的工具要达到原生工具的全部能力，并支持 Runtime 上的 skills。**整条线的差距表、三类能力的划分和顺序在 toexec 仓库的 [v3 方案](https://github.com/xwfe/toexec/blob/main/docs/plan/implementation-plan-v3-native-parity.md)，这里只登记第一个阶段；后面的阶段开工时再登记（并行会话经常撞号，提前占号没有意义）。

现状：官方 CLI 靠"当前目录"发现 `.claude/skills/`，而受管会话里 CLI 的当前目录在 Agent Node 上，项目在 Runtime 上，所以一个都发现不了。ccnm 现在只在 MCP 握手文本里点了 `SKILL.md` 的路径（和规则文件共用 40 个名额、768 个码元），没有 name 和 description——模型不知道有哪些 skill、什么时候该用；Codex 会话连路径都没有（`provider::context::named` 对 Codex 返回空）。

做法：skills 在 Runtime 上发现，经一个新的 MCP 工具交给模型；skill 目录里的脚本由模型用现有的 `exec_command` 跑，因此天然在 Runtime 上、以执行账号的身份、受同一套写互斥和 `exec_sandbox` 约束。三种入口（Claude 受管、Codex 受管、外部 bridge）共用一个实现。`ccnm.workspace-mcp/1` 冻结时写明"加工具属于加法"，不升版本。

- **P36.1** 实测，零额度：本机的 Claude Code（记下版本）和 Codex 0.154.0 各自怎么处理 MCP 工具的 description（有没有长度上限、截在哪）、怎么呈现 MCP `prompts`（变成什么名字的斜杠命令、参数怎么传）。办法沿用 V2-Q1：读 CLI 打包代码的静态证据，加一次不登录也会发生的 MCP 连接的 debug 日志；Codex 用现成的本机假模型服务抓它实际发出的请求。结论决定目录放哪、放多少，以及 P36.5 做不做。
- **P36.2** 共享库：toexec 新 crate `toexec-skill`（零依赖，发 tag），只放纯机制——frontmatter 读取（单行值、引号、`>` / `|` 块标量、简单列表）、`$ARGUMENTS` / `$N` / `$name` 替换、`` !`命令` `` 注入行的识别。gld 已有一份解析且不认多行 description，它换用这个 crate 是 gld 自己的阶段，不挡 P36。
- **P36.3** 发现：`mcp-serve` 扫工作区里的 `.claude/skills/*/SKILL.md`、`.claude/commands/**/*.md`、`.agents/skills/*/SKILL.md`。个数和单个大小有上限，排序确定——同一个项目每次得到同一份目录。路径走现有的读策略（不出工作区、不跟穿出去的 symlink）。重名时 skill 优先于同名命令（官方语义）。
- **P36.4** 工具：新增一个只读工具（`read` 与 `coding` 两种模式都给）。不带名字调用返回完整目录；带名字返回 SKILL.md 正文：去掉 frontmatter，做参数替换，`${CLAUDE_SKILL_DIR}` 换成 skill 目录的工作区相对路径，`${CLAUDE_PROJECT_DIR}` 换成 `.`；正文超上限时截在行边界并写明用 `read_file` 从哪一行接着读。**`` !`命令` `` 注入不执行**：原样保留并在正文开头列出，由模型自己决定要不要用 `exec_command` 跑——自动执行等于一次"读"调用触发了项目指定的命令，绕过 `exec_command` 上的人工确认。`disable-model-invocation: true` 的 skill 不进目录、也不能由这个工具加载。
- **P36.5** 目录放进工具 description（工作区没有 skill 时 description 是固定文本，fixture 逐字节比对的就是它）；`prompts`：可由用户调用的 skill 和命令登记成 MCP prompts。两件事的形状都以 P36.1 的结论为准；Host 不呈现 prompts 就不做，并写明。
- **P36.6** 握手文本：不再点名 `SKILL.md`，把那部分预算还给规则文件和 `CLAUDE.md`。（立项时还写了"标记行里说明有几个 skill、用哪个工具看"；实现时去掉了——目录已经在工具 description 里、一直在模型面前，再从装 `CLAUDE.md` 的 2048 码元里拿预算重复一遍不值。依据见记录第 3 节。）
- **P36.7** 契约与文档：`docs/protocol/remote-workspace-mcp-v1.md` 加一节，fixture 与 schema 做加法（两份 `tools-list-*.json` 各多一个工具，这是契约新增，不是为了过测试重录）；中立客户端测试覆盖目录、加载、不存在的名字、`read` 模式；`usage.md`、`support-matrix.md` 写明验到哪一步。
- **P36.8** 门禁：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo +1.89 check --workspace --all-targets --locked`、`python3 scripts/check_plan.py`、`python3 scripts/check_protocol.py`、`python3 -m unittest tests.test_check_protocol tests.test_remote_workspace_mcp -q`、`git diff --check`。

停止点：只做工作区里的 skills，不读执行账号 HOME 下的用户级 skills；不执行 `` !`命令` `` 注入；`allowed-tools`、`context: fork`、`agent`、`model`、`effort`、`hooks` 这些 frontmatter 字段忽略并在文档里写明；不放开 CLI 的任何内置工具（那是 v3 方案的另一个阶段）；不耗模型额度，所以"模型会不会主动去用 skill"这一条**没有验**，留给 v3 方案最后的对照实验；不发版、不换任何机器上的二进制。

### P37 — 执行面第一批：搜索模式、整文件覆盖、一行 shell

**依赖 P36。v3 方案第 5 节第 2 步。用户 2026-09-17 定：先把执行面补齐，再放开 Agent 面**（同时定了 WebSearch 默认放开、WebFetch 做成 opt-in，记在 toexec 的 v3 方案第 7 节，不是本阶段的事）。

现状，对照的是本机 Claude Code 2.1.273 打包代码里的工具定义：

| 能力 | 原生 | ccnm |
| --- | --- | --- |
| 搜索 | Grep：`output_mode` 三种（内容 / 只列文件 / 计数，默认只列文件）、`multiline`（`rg -U --multiline-dotall`）、`type`（`rg --type`）；一律带 `--hidden`，再排除版本库目录 | `search_text` 只有内容模式；dotfile 永远不搜 |
| 整文件覆盖 | Write | 没有。`add` 只能建不存在的文件，覆盖要先 `delete` 再 `add`，两次调用之间文件不存在 |
| 一行 shell | Bash（用户的 bash / zsh） | `exec_command` 只收 argv，要管道得自己写 `["sh","-c",…]` |

做法：全部是给已有工具加可选参数、加一种操作。`ccnm.workspace-mcp/1` 冻结时写明这属于加法，不升版本；不加工具，所以 `session::MCP_TOOLS`（Claude 放行清单、Codex `enabled_tools`）不动。已有参数的默认值和语义一个都不改——包括 `search_text` 默认仍是内容模式（原生默认只列文件，但 ccnm 冻结时就是内容）。

- **P37.1** 实测，零额度：本机 rg 在 `--max-count 1`、`-U --multiline-dotall`、`--type`（含不认识的类型）、`--hidden` 下 `--json` 输出的实际形状；`--glob` 和 `!.git` 排除的先后关系；Claude Code 2.1.273 Grep / Write / Bash 的参数和它拼的 rg 参数。结论决定参数名和语义，记进研究记录。
- **P37.2** `search_text` 加 `output_mode`（`content` 默认 / `files_with_matches` / `count`）、`multiline`、`type`、`include_hidden`。`.git` 不论怎么设都不搜，事后丢弃越界路径的那道检查不变。后两种模式下 `max_results` 数的是文件，`context_lines` 不起作用；多行匹配按行展开，每行照样受 512 字节和总量 32 KiB 限制。（实现时加了两条，依据见记录第 2.2、3 节：`type` 和 `glob` 同时给时拒绝，因为 rg 里命中 glob 的文件不看类型；排除 dotfile 和 `.git` 的 glob 挪到调用方 glob 之后——原来的顺序让 `glob: "**"` 把 dotfile 发给了模型，这是单独提交的行为修正。）
- **P37.3** `apply_patch` 加 op `write`：用 `content` 整体替换一个**已存在**的文件，必须带 `read_file` 给的 `version`；文件不存在时拒绝并指向 `add`。和 `update` 走同一套三阶段提交、中断日志、回滚和权限保留。
- **P37.4** `exec_command` 加 `shell`（字符串），和 `cmd` 二选一，都没给或都给了报 `CCNM_E_INVALID_ARGS`；执行的是 `bash -c <shell>`，Runtime 上没有 bash 报 `CCNM_E_DEPENDENCY`，不退回 `sh`（同一行命令在 dash 和 bash 下意思可以不同，悄悄换解释器比报错更糟）。`required` 从 `["cmd"]` 变成空，旧调用照样合法。权限门、人工确认、`exec_sandbox`、超时和输出保留一律不变；开了 `exec_sandbox` 时被包起来的就是 `bash -c …` 这条 argv。
- **P37.5** 契约与文档：两份 `tools-list-*.json` 手工同步说明文字和参数（契约新增，不是为过测试重录）；`remote-workspace-mcp-v1.md` 页首记这次加法并加一节；中立客户端测试覆盖三种输出模式、多行、类型过滤、dotfile、`write`、`shell`；`usage.md`、`support-matrix.md` 写明验到哪一步。
- **P37.6** 门禁：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo +1.89 check --workspace --all-targets --locked`、`python3 scripts/check_plan.py`、`python3 scripts/check_protocol.py`、`python3 -m unittest tests.test_check_protocol tests.test_remote_workspace_mcp -q`、`git diff --check`。

停止点：不做分开的 `-A` / `-B`、`head_limit` / `offset` 分页、`-o`（只输出匹配部分）；不做跨调用保持工作目录（原生 Bash 有，ccnm 每次传 `cwd`）；不耗模型额度，所以"模型会不会用这些新参数"没有验；不发版、不换任何机器上的二进制；gld 的同步是 v3 方案第 5 步。

### P38 — `search_text` 的 glob 不再越过 `.gitignore`

**依赖 P37。P37 发现、记在 status.json 的 observed_gaps；用户 2026-09-17 让按建议修。**现象：给了 `glob` 时，rg 会搜 `.gitignore` 排除的东西——`glob: "**"` 搜进 `target/`、`node_modules/`，`glob: "**/*.yml"` 搜到被忽略的 `secret.yml`。原因是 rg 15.2.0 里 glob 一旦命中（文件或目录都算）就不再看 `.gitignore`。

P37 交接时建议的修法是"拒绝能匹配目录的 glob"。开工前实测否定了它：`*.log`、`**/*.yml` 这种只匹配文件的 glob 同样越过 `.gitignore`。改用的做法：glob 不再作为 `--glob` 交给 rg；每个备选的文件名部分用 `--type-add` 登记成一个临时类型交给 rg 缩小范围（rg 的类型判断排在 `.gitignore` 之后、不作用于目录），整条 glob 由 ccnm 按 rg 原本的规则（不含 `/` 的只比文件名、含 `/` 的从 workspace 根比整条路径、以 `/` 结尾的只匹配目录）过滤结果。这样 `*.rs` 的含义不变，唯一的结果变化是被忽略的文件不再出现——这正是工具说明一直写的。

- **P38.1** 实测，零额度：rg 15.2.0 上 glob 越过文件级和目录级 `.gitignore` 的最小复现；`--type-add` 是否遵守 `.gitignore`、是否受 `!.*` 约束、`*` / `**` 作为类型 glob 的效果、带 `:` 的 glob 和 `include:` 前缀怎么被解析；rg 对含 `/` 与不含 `/` 的 glob 分别怎么锚定（含 `path` 不是根时）。
- **P38.2** 实现：`Glob` 记下是否含 `/`、是否以 `/` 结尾，提供按 rg 规则匹配文件路径的方法；`search_text` 用 `--type-add` 缩小范围（文件名部分含 `:` 时不缩小，只靠过滤），结果逐条过滤。以 `!` 开头的 glob 是排除规则、不会越过 `.gitignore`，照旧交给 rg。
- **P38.3** 顺带：`type` 和 `glob` 同时给时不再拒绝，改成两者都要满足——glob 不再是 rg 的 `--glob`，P37 拒绝它的理由（命中 glob 的文件不看类型）不存在了。
- **P38.4** 测试：旧实现下失败的复现测试（目录级、文件级各一）；glob 语义不变的回归（`*.rs` 任意深度、`src/*.rs` 不进子目录、`**/x/*.rs`、花括号、以 `/` 结尾、排除规则）；`type` + `glob` 取交集；中立客户端同步。
- **P38.5** 文档与门禁：协议第 5.2 节、使用说明、支持矩阵、P37 记录加后续说明；门禁同 P37.6。

停止点：不改 `list_files`（它走 `git ls-files`，没有这个问题）；不优化"文件名部分是 `**`、没法缩小范围"时的扫描量（这时 rg 扫全部未忽略的文件、ccnm 丢掉不匹配的，受 60 秒超时约束）；不耗模型额度；不发版。

### P39 — `view_image`：把 Runtime 上的图片交给模型

**依赖 P38。v3 方案第 5 节第 3 步的第一项**（图片；PDF 和 notebook 另立阶段）。现状：原生 Read 能直接看图，ccnm 的 `read_file` 遇到二进制文件就拒绝，模型看不到项目里的截图、设计稿、测试产出的图。

- **P39.1** 实测，零额度（toexec `evidence/v3-parity/media-surface/`）：Codex 0.154.0 用本机假模型真的调一次返回图片的 MCP 工具，分别在不开 Code Mode 和 ccnm 受管会话用的 Code Mode 下看工具结果变成什么；Claude Code 2.1.273 读打包代码，看 MCP 图片块怎么转换、有没有缩放和上限；MCP `resource` blob 在两边的下场。结论决定用哪种内容块、上限多少、ccnm 要不要自己缩放。
- **P39.2** 工具：新增只读工具 `view_image`（`read` 与 `coding` 都给），参数 `path`，走和 `read_file` 同一套读路径策略。按文件头认 PNG / JPEG / GIF / WebP，返回一段说明文本加一个 MCP `image` 内容块。超过上限、不是这四种格式、目录、特殊文件都报 `CCNM_E_INVALID_ARGS`，并说清楚下一步（SVG 用 `read_file`，其余先用 `exec_command` 转换或缩小）。`read_file` 拒绝二进制时，是这四种图片的顺带指向 `view_image`。
- **P39.3** 接线：加一个工具要动的全部地方——`session::MCP_TOOLS`（Claude 放行清单、Codex `enabled_tools`）、两份 `tools-list-*.json` 与 schema 的工具名、`provider_compat` 记为有据可查的差异、P11/P12 脚本及其测试里的只读工具清单、`external_mcp` / `cli` / `mcp_read_file` 集成测试、中立客户端。
- **P39.4** 契约与文档：协议文档加一节（内容块形状、上限、两个 Host 的差异——Codex Code Mode 下模型要自己调 `image()`）、新增 `call-view-image-ok.json` 样例；`usage.md`、`support-matrix.md` 写明验到哪一步。
- **P39.5** 门禁：同 P37.6。

停止点：不在 Runtime 上缩放或转码图片（不加图像处理依赖；Claude Code 自己会缩放，依据见 P39.1）；不做 SVG 渲染、HEIC/BMP/TIFF 转换；不做 PDF、notebook；不耗模型额度，所以"模型拿到图后看得对不对"没验；不发版。
