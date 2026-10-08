# 会话控制与 Runtime 写权：P58 / P60

共同边界见[总纲](core-hardening.md)。本文件定义 P58 / P60 要实现的行为（两阶段都已完成，见 status），不修改当前已冻结的公共协议；进度和验收编号仍以 [ROADMAP](ROADMAP.md) / [status](status.json) 为准。P57 先建立反证与测试基线。

## 1. P58：精确控制，而不是按 workspace 猜测

### 1.1 当前落点

[rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs) 的 `SystemRuns::stop` 调用 `launcher::stop_selected` 时 session 为 `None`；正在运行的 RPC Record 只在最终 Finish 中得到 ccnm 受管 ID。`spawn_run` 和 `stop` 保存不同 Record 副本。[rpc/store.rs](../../crates/ccnm-core/src/rpc/store.rs) 的 rename 只保证文件不被半读，不保证读改写互斥或状态不回退。P57 必须覆盖这些窗口，不能以现有单写锁推导“必定不会停错”。P57 实测还发现 Agent 侧的另一半：[work.rs](../../crates/ccnm-core/src/work.rs) 的 `stop` 没有 session 时只按 `ccnm-<workspace>` 查 tmux，而 print 运行由 Controller 起、不在 tmux 里，所以 RPC stop 停不到 print 运行，同 workspace 有交互会话时改停那个；带精确 id 的 `stop_print_session` 路径已经存在并会校验 supervisor（[P57 记录](../research/2026-09-28-p57-core-baseline.md) 第 3.2 节）。P58 的精确停止要落到这条路径上。

主要文件：上述 RPC 模块、[launcher.rs](../../crates/ccnm-core/src/launcher.rs)、[work.rs](../../crates/ccnm-core/src/work.rs)、[protocol/run.rs](../../crates/ccnm-core/src/protocol/run.rs)、[session.rs](../../crates/ccnm-core/src/session.rs)。确需拆文件时按 admission / store / observation 分职责，不把大文件机械切块或另建执行层。

### 1.2 身份与启动流程

对外 `s-*` 句柄保持 opaque；内部显式保存它与 Agent 确认的 `ccnm_session_id`、Agent identity、workspace 和 Runtime 资源的关系。Provider thread/resume ID 只作结果元数据，不用于 stop 或锁定位。

目标流程为：

```text
校验输入和当前配置 → 原子建立启动幂等记录 → 返回/保留 public handle
    → Agent 准备受管会话并回报准确身份 → 持久化绑定
    → 复查取消请求 → 启动或精确取消该受管会话
    → 按已绑定 ID 观察/等待/停止 → 原子记录终态及来源
```

这允许 `session.start` 保持快速返回 starting，而不等待模型跑完。内部可以拆成 prepare/start/wait，或用等价的受管启动回执；**必须保证受管 ID 在可安全发送 stop 时已获得并持久化**。不得把客户端提交的任意目录、PID 或 Provider ID 当准备回执。内部协议改变需版本校验；旧 peer 不认识新请求时明确拒绝。

在准备回执前收到 stop：先持久化取消意图，启动路径在真正执行前消费它；绝不能停止“这个 workspace 现在的任意会话”。回执已返回但本地落盘前崩溃：保留原 start_key 的不确定记录，不重新创建任务；再次联系 Agent 只能按原请求关联键查询，不使用“最新会话”兜底。禁止用临时生成另一个 start_key 重试。

运行中按精确绑定停止。配置被改成另一个 Agent/Runtime/instance 时，旧句柄不得随新配置改指目标；解析新的连接信息后必须核对原身份与资源，无法证明一致就拒绝或报告 unknown。不能通过保留旧目标绕过当前权限撤销。

### 1.3 存储、幂等与状态合同

存储仍可使用私有本地文件，不要求数据库。需提供跨线程、跨 `ccnm rpc` 进程的原子读改写入口，不能只用进程内 Mutex。每条记录以唯一临时文件、创建即私有权限、同目录原子替换写入；避免固定 `.tmp` 被两个写者竞争。持久性保证按实际 sync/文件系统测试说明，不额外宣称任意断电下零丢失。

| 对象 | 必须满足的性质 |
| --- | --- |
| session 输入 | 只允许服务端句柄的单一安全路径分量；拒绝 `/`、`\\`、NUL、越界及 symlink 等路径替换。兼容已发布合法句柄，不擅自改成只接受 UUID |
| start_key | 协议允许的不同原串不能经有损 `safe_name` 合并；使用无损索引或摘要索引加原串比对。workspace 作用域保持；timeout 不参与幂等输入比较 |
| key 与 record | 任何 Agent 执行前两者都已建立可恢复关联；同键并发只有一次实际启动；未完整发布或崩溃不明时返回 uncertain，不将半写的空 key 当新任务 |
| stop_requested | 单调的取消事实：一旦 true，启动线程的旧副本不能写回 false |
| 终态 | 不被迟到的 running/stopping 覆盖；完成与 stop 竞争时同时保留取消事实和实际进程结论，不凭请求顺序捏造 exit code |
| unknown | 按现有 machine/1 作为终态；观察不到不等于可重试 failed。后续观测可留为诊断，不能偷偷把同一终态改为 completed |

`completed / failed / unknown` 与效应标记必须满足[公共协议](../protocol/machine-protocol-v1.md)。在发送启动前可证实被拒是 `effect=none`；启动请求已发出、但回执丢失或进程身份不可判定时不得一概落成“确定失败”，更不能提供安全重试暗示。停止请求被接受与 Runtime 已清理是两回事；P60 的写权诊断提供额外事实，不改变 Agent 的业务验收职责。

### 1.4 必须先红后绿的回归

| 编号 | 受控窗口 | 通过条件 |
| --- | --- | --- |
| CT-01 | A 正在运行，B 为同 workspace 的新 starting 请求，停止 B | A 的 ID/进程不被 stop；B 取消或具名拒绝，无 workspace 兜底 |
| CT-02 | stop 分别发生在 prepare 前、回执前后、启动后 | 每个窗口只影响对应会话；取消后不额外启动一次 |
| CT-03 | run 完成写入与 stop 写入强制交错；两个 RPC 进程同时访问 | JSON 完整，无 `.tmp` 冲突，stop 标志不丢，终态不回退 |
| CT-04 | 同键同输入、同键不同输入、不同原串被旧 safe_name 映成同名 | 重用/冲突符合契约；不同键不误重用；只执行一次 |
| CT-05 | 任一 key/record/启动回执持久化边界中断 | 后续为原句柄或 uncertain，不重执行，不假 effect=none |
| CT-06 | 非法句柄、symlink、权限/属主错误、超长或边界 key | 在越界读写前拒绝；不泄露真实私有路径正文；旧合法句柄仍可查 |
| CT-07 | 配置切换节点/instance/root 后用旧句柄 stop | 不操作新目标；身份不符可诊断，权限撤销不被旧快照绕过 |
| CT-08 | Agent/transport 结果不明、PID 重用、进程查询失败 | 记录 unknown/uncertain，不能变成可自动重试失败；真实已结束会话 stop 仍幂等 |

以可控 barrier、独立进程及 fake transport 驱动，不用随机 sleep 作为主要反证。Rust 单测可注入 runner；Python 中立客户端只走真实二进制和字节协议，不 import ccnm 内部状态实现。不要增加可在生产开启的“跳过身份检查”测试开关。

### 1.5 P58 任务与交付

P58.1 固化 P57 的 CT 反证及合法旧行为；P58.2 完成精确启动/取消/stop 绑定；P58.3 修复记录事务、句柄路径和幂等索引；P58.4 修正错误效应、终态与取消事实；P58.5 同步协议实现差距、两份 Python 示例及用户说明，并通过相应测试。

重点测试：`cargo test -p ccnm-core --lib rpc::`、`cargo test -p ccnm-cli --test rpc`、`cargo test -p ccnm-cli --test instance_execution`、构建后的 `tests.test_blackbox_client` / `tests.test_execution_backend`，以及总纲的全量门禁。旧 store 数据无法完整迁移时保留可读旧记录或显式拒绝，绝不悄悄重跑旧任务。

停止点：不做 interactive RPC，不做 busy 预留服务，不升级 Provider，不推送或部署。无法确定启动身份/取消效应时停止该子项并保留反证，不以 workspace stop 暂时代替。

## 2. P60：Runtime 占用诊断与可证明的 busy

### 2.1 一个权威，不是另一把锁

复用 [write_guard.rs](../../crates/ccnm-core/src/mcp/write_guard.rs) 的 canonical 资源解析和 state 域。新增结构化观察函数，优先挂到现有 `ccnm status` 的适当输出中；如提供 JSON，明确版本化、只报告必要事实。不要让调用方解析现有中文/英文错误措辞，也不要把 doctor/MCP probe 当无副作用轮询。

观察结果至少区分 `free`、`held`、`abandoned`、`unknown`，并提供经过身份校验的 workspace/resource 引用、已知 owner session、观察时间和有限原因。**这些是写权观察枚举，不是新增 Machine API 终态。** PID/命令行仅用于诊断；无权限、查询失败、marker 损坏和只有旧格式记录都要显式说明，不假装“空闲”。

观察不创建或覆盖 marker，不获取/续约一份写权限，不起 Agent 或 Runtime 工具 server。若必须短暂试锁现有文件来区别实际持锁与残留，必须明确释放，不能写成 held/released，也不能影响现有 writer；参照仓库已有 fork/flock 显式 unlock 经验补回归。

Runtime 上的普通命令、relay、MCP 通道、Agent 进程分别列事实。mcp-serve 被 SIGKILL 后即便 PID 消失，也保留原 held/unknown；组外后代缺乏可靠监督时仍需人工确认。此阶段不引入自动杀进程、清锁、cgroup/容器守护程序或跨 state 全局锁。

### 2.2 提前拒绝能承诺到哪里

`session.start` 复用 Runtime 权威观察做有限 preflight：当前可证实由另一 writer 占用，且本请求尚未启动 Agent 时，返回 `-32008 busy` / `effect=none`。已有同 start_key 的重用查询优先于新任务 admission，不能因它自己的 writer 正在工作而拒绝复查原句柄。

**preflight 不是预留。** 检查后仍可能有另一个客户端抢先；真正取得写权的仍是 MCP 初始化。保留该竞态的第二道拒绝，不把一次 free 观察当授权，也不为避免竞态另做外部 lease。接受句柄后的启动失败按 P58 状态/效应记录，不能发送第二个 start 响应，也不能倒称没有发生任何副作用。

| 观察 | 新启动处理 |
| --- | --- |
| 原 start_key 已存在 | 先返回原执行或 conflict/uncertain，不启动第二个 |
| 另一 writer 实际 held，尚未启动 Agent | busy / effect=none，可供上层选择退避 |
| abandoned、损坏 marker、归属未知 | policy/not_ready/uncertain 中符合实际效应的具名拒绝；不是可盲目重试 busy |
| Runtime 不可达/无权查询 | 拒绝或不确定；不放宽为 free，不切换别名或身份 |
| free | 允许进入原启动过程，最终仍受 Runtime guard 限制 |

不要求为每一次启动维持“零竞争窗口”的新分布式事务；明确有限保证比无证据的强保证更重要。`completed`、stop 返回和一份曾经 free 的快照均不代表当前可交权，连续执行仍检查实际 Runtime。

### 2.3 验收与交付

| 编号 | 验收场景 |
| --- | --- |
| AU-01 | Managed Claude/Codex、外部 coding client 竞争同一 canonical 资源；只有一个 writer，其他具名拒绝 |
| AU-02 | 状态观察前后 marker 内容/持锁者不变，不启动 Agent、不产生项目写入；fork 压力下不遗留观察锁 |
| AU-03 | 活跃 held、残留 held、abandoned、损坏/旧 marker、PID 重用、ps 失败、Runtime 不可达分别显示真实状态 |
| AU-04 | free 检查与握手之间插入另一个 writer；后到者仍被原 guard 拒绝，不误用 preflight 作为授权 |
| AU-05 | mcp-serve 强杀后的命令仍存活时，新 writer 被拒；仅补诊断不把它登记为自动清理通过 |
| AU-06 | 错误配置/state/identity、不相干 workspace、同 common-dir worktree 和同键重查均符合边界 |

P60.1 固化观察语义和来源；P60.2 实现 Runtime 查询与现有 status 接线；P60.3 实现安全的 busy 提前拒绝及竞态分支；P60.4 跑 AU 矩阵、跨进程/64线程路径；P60.5 同步排错、运维、Machine API 差距与支持矩阵。

重点测试：`cargo test -p ccnm-cli --test write_guard`、`--test external_mcp`、`--test runtime_open`、RPC 测试、`tests.test_remote_workspace_mcp`、`tests.test_blackbox_client`；内部查询改变 runtime wire 时追加对应版本拒绝测试。P52 同组 relay 的反证保持可复验，不重开已解决缺陷。

停止点：没有足够 OS 归属证据时保留 fail-closed 和人工恢复说明，不新增“强制清锁”按钮。不要把 Windows/Linux Agent 移植混进诊断实现。
