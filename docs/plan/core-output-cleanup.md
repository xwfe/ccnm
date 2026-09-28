# 结果完整性与跨身份清理：P59 / P61

本文件是待实施设计；共同边界、顺序及授权见[总纲](core-hardening.md)。沿用 `ccnm.machine/1` 与现有 Runtime 输出协议，不能把新增设计误写成当前能力。

## 1. P59：从源头保留结果，不给尾部伪装分页

### 1.1 三种输出不能混在一起

| 数据 | 现有来源 | 本阶段处理 |
| --- | --- | --- |
| Provider 最终回答、usage/cost、thread ID | Agent stdout 解析后的结构化结果 | 保持现有 text/outcome/usage/cost 语义；不存在的字段不制造零值 |
| 官方 CLI 的 stdout/stderr 诊断 | Agent session 目录；`RunReport` 当前只传末尾 2 KiB（P57 实测，见[记录](../research/2026-09-28-p57-core-baseline.md)第 3.5 节），解析成功时 stdout_tail 为空；stderr 到 RPC 层整个丢掉 | 新增受控、可分页的完整保留视图，源头为 Agent，不是已经裁剪的 RunReport |
| Runtime 工具输出与第三方 MCP 大结果 | Runtime `read_output`、Agent `read_mcp_result` | 保持各自 ref 与连接/内存寿命；不冒充可跨重连永久访问的任务日志 |

核查落点：[work.rs](../../crates/ccnm-core/src/work.rs) 的 `run_print` / `result`，[protocol/run.rs](../../crates/ccnm-core/src/protocol/run.rs)，[rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs) 的 `result` / `finish_from`，[rpc/store.rs](../../crates/ccnm-core/src/rpc/store.rs)，[Provider 结果](../../crates/ccnm-core/src/provider/result.rs)。Runtime 的 [output.rs](../../crates/ccnm-core/src/mcp/output.rs) / [retention.rs](../../crates/ccnm-core/src/mcp/retention.rs) 可参考边界，但不得把两类身份的数据目录合并。

### 1.2 结果通道与内存边界

利用 P58 的精确受管 ID，在 Agent 提供按该 session 读取输出的内部请求；只接受会话/流/游标或偏移，不接受任意文件路径。绑定 workspace、Agent identity 与私有目录，旧 peer 不认识请求时明确拒绝，不回退为读取最新会话或裸 SSH cat。

首版提供**终态后的不可变输出快照**，不做实时日志订阅、SSE 或 interactive RPC。快照应分块读取和传输，不能在返回 JSON 前一次性 `read_to_end` 任意大的日志。Operator 侧可以缓存已脱敏快照以便之后的 RPC 进程读取；选择缓存时记录来源/会话/流/生成版本与保留范围，私有权限创建，配额可控。不能因为完整性要求取消磁盘和内存上限。

源头未保留、已清理、超配额或只剩旧尾部时，明确区分“这次只给一页”和“历史内容不可恢复”。`bytes_total` 按公共契约表示服务端实际保留的视图总字节数，不拿最后 8 KiB 的长度冒充原始 stdout 总量。可新增 `source_truncated` / `unavailable_reason` 等可选字段表达永久缺失，字段在实施时连同 schema/fixture 冻结。

不将 stdout 和 stderr 按无法证明的时间顺序混排。现有 `output` 保持 stdout 视图；stderr 用明确的附加流选择或结果字段表达，兼容性设计先于实现，不能让旧客户端把两个流错当连续 stdout。结构化最终回答与原始诊断仍可分别读取。

### 1.3 分页合同

| 项目 | 设计要求 |
| --- | --- |
| 首次请求 | 保留现有 tail-first 行为：先给末尾一页，而不是无提示改成从头开始 |
| 后续页 | cursor 指向更早的未读区间；返回的片段本身按正序，客户端重组时前置，不能按请求顺序直接 append |
| 定位 | cursor 绑定 session、stream、快照 generation、端点和服务端游标 epoch；调用方不能利用可改偏移读其他文件/会话 |
| 预算 | 按返回文本的 UTF-8 字节计；请求合法域以现有 schema 为准，服务端页上限允许更小但绝不超过 max_bytes。JSON 包装开销另受 RPC 消息上限约束 |
| 字符边界 | 不截断中文/Emoji 字符；极小预算不足容纳下一字符时，明确返回需要更大预算的可识别结果，不超限、不丢字、不陷入无限空页；字段/错误形状先补契约与客户端测试 |
| 完整性 | 每页稳定范围与 generation 可核验；重读同页内容一致；完整重组后字节数/摘要与快照一致 |
| 失效 | 跨 session、被篡改、过期、清理或 RPC 重启后的旧游标返回现有 expired 语义；重新以 cursor=null 可建立新读取，不重新执行 Agent |
| 活会话 | 首版保留当前未结束只返回状态的行为；不凭正在增长的日志假定得到最终快照 |

`max_bytes` 本次必须真正生效，包括输入类型/范围校验；不再只允许字段存在却完全忽略。默认和上限先核对现有 schema、CLI 传输预算及客户端，写入实现文档和测试，禁止只改示例。

脱敏应在外发前形成稳定视图，分页在该视图上进行；不能每页独立正则替换而使横跨页边界的敏感内容漏出。沿用 Provider 现有脱敏范围并写明局限，不宣称识别所有用户自定义秘密。不得让新增缓存包含原始认证头、整份环境或复制来的登录文件；原始 Agent 文件的访问和保留按其账号权限处理。

### 1.4 客户端与测试

更新 [ccnm_machine_client.py](../../clients/python/ccnm_machine_client.py) 与 [execution_backend.py](../../clients/python/execution_backend.py)：提供显式逐页迭代或有总量上限的读取，识别 tail-first 顺序、失效游标、源头丢失和字节预算不足。默认 `result()` 不无限读入内存；`wait()` 不自动重启任务；两个文件仍能脱离仓库运行，不 import ccnm-core。

| 编号 | 验收 |
| --- | --- |
| OUT-01 | 用真实二进制 + 无付费 fake Agent 跑超过 8 KiB 和数 MiB 输出；早期/中间/末尾 marker 全能取回，stderr 不混流 |
| OUT-02 | 包含中文、Emoji、无换行长行、空输出、无效编码输入的既定处理；各种合法预算下不超限、不漏段 |
| OUT-03 | stdout 解析成功与失败均覆盖；完整最终 text 与原始 stdout 分别核对，不以 empty stdout_tail 当无输出 |
| OUT-04 | 同页重读、跨会话/篡改 cursor、重启失效、源头清理、旧尾部记录、文件增长后冻结，各有明确结果 |
| OUT-05 | 人工测试秘密跨分页边界与缓存边界；对外结果及日志无测试秘密，原生模型凭据从不用于测试 |
| OUT-06 | 输出超过预算/磁盘配额、部分传输失败、无法访问 Agent，明确不完整/unknown，不假 full；内存使用随页上限而非全文件增长 |
| OUT-07 | 黑盒客户端重组与快照逐字节一致；旧 result 调用/旧字段/usage 缺省语义仍成立 |

P59.1 固定结果与兼容性合同；P59.2 接通 Agent 源头及受限快照；P59.3 实现 cursor/max_bytes/失效；P59.4 更新参考客户端；P59.5 完成 OUT 矩阵及文档/协议门禁。

重点测试：RPC 单测与 CLI 集成、`tests.test_blackbox_client`、`tests.test_execution_backend`、`tests.test_check_protocol`；使用总纲全量门禁，不能仅测静态 JSON fixture。停止点是不实现实时事件流、交互输入或自动过期清理；清理由 P61 独立处理。

## 2. P61：先预览，再由正确身份清理

### 2.1 范围和入口

当前 [launcher::purge](../../crates/ccnm-core/src/launcher.rs) 向 Agent 请求后删除调用者本地 state；分离身份部署下，这不是 Runtime Executor 的目录。P61 只清理 ccnm 持有的结果/输出/已结束会话元数据，不清项目源码、Git、构建物、Provider 登录、用户 SSH 配置、Controller 或外部工具部署。

拟新增通用维护命令 `ccnm cleanup <workspace>`，**默认仅预览**；明确确认后使用 `--apply <preview-token>` 执行。该名称是设计目标，不是当前已有命令。与现有 `workspace remove --purge` 共用同一清理服务，不再保留两套不同删除逻辑；后者必须先完成所需清理/报告后再移除发现目标所需配置，部分失败不能删掉后续恢复入口。

调用路由遵守现有身份边界：Operator 发起；Agent 只清自身资源；Runtime 清理由 Agent 经原受控通道请求 Executor 完成；Operator 本地 RPC 记录由该 Operator 清理。ccrun 不持主动回连 key，不引入 sudo/root/任意路径删除 RPC。

### 2.2 预览令牌与 apply

预览返回经过脱敏的清单：节点角色、归属 UID、workspace、精确 session/资源 ID、类型、大小、保留状态、被跳过的理由，以及整体 token/有效期。token 绑定实际配置身份、候选集合与文件版本；URL/argv 不携带凭据，清单不输出私有认证路径。

apply 重新从权威节点解析同一清单，验证 token、实际 UID、配置/资源身份和当前活跃状态。仅提供一个 fingerprint 字符串而不重新检查文件变化，不足以防止预览后换绑。文件替换/symlink/资源转活/目标不可达时拒绝对应删除，不盲目复用旧清单。拒绝越界的路径在检查存在性之前完成。

| 资源 | 处置 |
| --- | --- |
| 活跃 session、被读取或写入的输出 | 跳过；需要释放既有资源锁才能清理，不能先删再让调用失败 |
| unknown、held、abandoned 或归属不明 | 保留并给原因；cleanup 不等于 recover，不调用 kill、不删 guard |
| 明确结束且归属一致的结果/输出 | 经预览确认后删除；由持有该目录权限的身份执行 |
| RPC start_key 与执行身份 | 删除大输出不能让旧 key 被重新解释为新任务；保留最小幂等记录/墓碑及原输入一致性检查 |
| 已清理输出的 session | 按协议返回 expired / retained metadata；没有任何墓碑时才是 not_found，不能以磁盘空了推导“从未执行” |
| 不在这次清单的其他项目/用户数据 | 不触碰；不能按用户名/通配符扫整台机器 |

初版不加自动 TTL sweep，不擅自改变现有 Runtime retention 默认值。`expires_at` 未给出不表示永久保留。明确清理先补齐 tombstone/游标失效行为；start_key 的长期遗忘政策单独说明，禁止由普通 cleanup 静默释放可重执行的旧任务键。

跨节点删除不是原子事务，不承诺回滚已删内容。每项报告 `removed/skipped/failed` 和原因；部分失败用非零退出与完整进度报告表达，可对尚未完成且版本未变的清单再次预览重试。成功项不伪回滚；权限不足不退回 root 或直接操作私有目录。

### 2.3 验收与落点

主要落点：[launcher.rs](../../crates/ccnm-core/src/launcher.rs)、[work.rs](../../crates/ccnm-core/src/work.rs) 的 purge，[protocol/run.rs](../../crates/ccnm-core/src/protocol/run.rs)、Runtime request 分派与 [retention.rs](../../crates/ccnm-core/src/mcp/retention.rs)、[rpc/store.rs](../../crates/ccnm-core/src/rpc/store.rs)、CLI 参数与输出。清理传输要复用现有 SSH；如改内部 wire，按版本拒绝旧端，不能沿用旧 PurgeRequest 的行为假装协议没变。

| 编号 | 验收 |
| --- | --- |
| CL-01 | Operator 与 Executor 使用不同私有目录/UID 的测试；预览指出真实归属，apply 只删除各自许可资源 |
| CL-02 | 默认命令只预览；缺少、过期或变更 token 不执行；预览后配置/文件/活跃状态改变能拒绝 |
| CL-03 | 活会话、分页正在读、unknown、held/abandoned、其他 workspace 全部保留；Runtime guard 未被修改 |
| CL-04 | symlink/遍历/错误身份/权限拒绝；不存在资源不泄露越界信息，不升级权限 |
| CL-05 | Agent 清理成功但 Runtime 不可达，给出部分结果；重新预览后重试不误删、不丢 workspace 配置 |
| CL-06 | 输出已清理后，旧 cursor/session 返回对应 expired；同 start_key 仍关联原执行，不重启 Agent |
| CL-07 | `workspace remove --purge` 与 standalone cleanup 共享逻辑，保留现有无 purge 行为，删除顺序可恢复 |

P61.1 固定清理资源和墓碑合同；P61.2 实现只读清单/令牌；P61.3 接通正确身份的 apply 与部分失败；P61.4 统一现有 purge/结果失效/幂等保留；P61.5 完成 CL 回归和手册。

跨 UID 的真实权限结论在授权环境验证；临时目录/fake transport 只证明路由和拒绝逻辑，不能冒充 OS 隔离。P62 需在 hpsrv/ccrun 再验一次。停止点：不做凭据撤销平台、生产服务卸载、自动杀进程或自动清锁，不以“清理”授权发版/删除项目。
