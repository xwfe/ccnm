# P59 完整结果快照与有界分页（2026-09-29）

范围见[输出与清理方案](../plan/core-output-cleanup.md)第 1 节，反证来自 [P57 记录](2026-09-28-p57-core-baseline.md) 第 3.5 节（C0–C5）。本轮只做离线实现与回归：**没有连 hpsrv/fodelf、没有运行真实模型或真实 Controller、没有部署或推送。**

## 1. 结论

`session.result` 现在能把一个已结束会话的 stdout 和 stderr 各自完整读回（每个流保留最后 32 MiB），一页不超过 `max_bytes`、不切字符，先给末尾一页、游标往前翻。之前 Agent 只交回 2 KiB 尾巴，RPC 却报 `truncated: false`、把尾巴长度当总长度，`max_bytes` 被忽略，stderr 整个丢掉——这些在同一份黑盒回归里对旧代码全红、对新代码全绿。拿不到完整内容时（Agent 联系不上、Agent 是旧版本、P58 之前的记录），服务端给旧尾巴并用 `unavailable_reason` 写明，不再冒充完整输出。

| 验收 | 结果 | 证据 |
| --- | --- | --- |
| P59.1 语义与兼容 | 契约只做加法：`output.stream`、`output_ref` 的 `stream` / `source_bytes` / `source_truncated` / `unavailable_reason`、能力 `output_streams`、错误原因 `max_bytes_too_small` + `min_bytes`；第 9 节写明倒序分页、保留上限、UTF-8 规整；`text` / `usage` / `cost` 语义不变 | 2、4.3 |
| P59.2 源头快照 | Agent 在会话结束后第一次被读时生成冻结的脱敏视图，按会话 id + 流 + 偏移分块读（内部协议 8）；RPC 第一次读时整份拷到本机 | 2 |
| P59.3 分页 | `max_bytes` 校验并生效；不切字符；太小时报 `-32602` 并给 `min_bytes`；游标绑定进程、会话、流、视图版本 | 3.2、3.3 |
| P59.4 参考客户端 | `MachineClient.session_result` 加 `cursor`/`stream`，新增有总量上限的 `read_output()`；`ExecutionBackend` 新增第六个方法 `output()`，`result()` 仍不带完整输出 | 3.2 |
| P59.5 OUT 矩阵与门禁 | 见第 3 节；Rust 990（64 线程）、Python 全套通过 | 3、4 |

## 2. 改了什么

| 位置 | 改动 |
| --- | --- |
| [session/view.rs](../../crates/ccnm-core/src/session/view.rs)（新） | 视图生成：流式替换 Agent 私有目录（跨块保留模式长度减一的重叠；截头时从截点前一个模式长度开始读，截点上那段路径整段替换，不留后半截），流式 UTF-8 规整（与 `String::from_utf8_lossy` 逐字节一致），只保留流的最后 32 MiB 并从字符边界开始，按生成时的文件长度冻结；在会话控制锁下只生成一次。内存只有 64 KiB 缓冲加一个模式长度 |
| [provider/mod.rs](../../crates/ccnm-core/src/provider/mod.rs) | 抽出 `output_redaction`：报告里的尾巴和完整视图用同一条规则，`redact_output_at` 改成调用它，行为不变 |
| [protocol/run.rs](../../crates/ccnm-core/src/protocol/run.rs)、[instance.rs](../../crates/ccnm-core/src/instance.rs) | `OutputRequest` / `OutputReport`，协议号 `OUTPUT_PROTOCOL = 8`；只收会话 id、流名、偏移、长度，不收路径 |
| [work.rs](../../crates/ccnm-core/src/work.rs)、`ccnm internal agent-output` | 校验 id、身份与 workspace，只给 print 会话、只在已结束后给；单次最多 4 MiB |
| [launcher.rs](../../crates/ccnm-core/src/launcher.rs) | `read_output_assigned`：回答的身份、会话、流或偏移对不上报 `Internal` |
| [rpc/output.rs](../../crates/ccnm-core/src/rpc/output.rs)（新） | 参数校验；本机快照（`rpc/outputs/<handle>/<stream>.view`，1 MiB 一块拷，每块核对视图版本与长度，拷不全不留文件）；倒序分页；进程内游标表（最多 1024 个，同一页复用同一个游标名）；回退与原因 |
| [rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs)、[rpc/store.rs](../../crates/ccnm-core/src/rpc/store.rs)、[rpc/wire.rs](../../crates/ccnm-core/src/rpc/wire.rs)、[rpc/mod.rs](../../crates/ccnm-core/src/rpc/mod.rs) | `Runs::output`（和 stop 一样核对节点）；`Finish.stderr` 保存 stderr 尾巴供回退；`min_bytes` 错误字段；`hello` 声明 `output_streams` |
| [ccnm_machine_client.py](../../clients/python/ccnm_machine_client.py)、[execution_backend.py](../../clients/python/execution_backend.py) | 见上表 P59.4；两个文件仍只依赖标准库、能单独抄走 |
| 契约 | [schema](../protocol/schema/machine-protocol-v1.schema.json)、第 9 节、新增 fixture `session-result-stderr-page.json`、`session-result-legacy-tail.json`、`reject-max-bytes-too-small.json`；`hello-ok.json` 加上新能力（契约新增，不是为测试重录） |

调用方能看到的变化：解析成功的 Claude 运行，`output` 以前是空的，现在是原始 stdout（结果文档）的末尾；超过一页时 `cursor` 非空、`truncated` 为真，而不是报 `false`；`max_bytes` 为 0、负数、字符串、小数时回 `-32602`（以前被忽略）；`bytes_total` 是保留视图的字节数，不再是尾巴长度。不给 `output` 参数的旧调用仍得到 stdout 最后 8 KiB。

## 3. 验证

环境：macOS 26.6.2 arm64，rustc/cargo 1.98.0，Python 3.12.12。

### 3.1 先红后绿

[tests/test_rpc_output.py](../../tests/test_rpc_output.py) 只走真实二进制的字节协议，对面由 [fake_agent_ssh.py](../../tests/fixtures/fake_agent_ssh.py) 冒充（新增 `agent-output`：按偏移切测试放进去的视图；另可演“联系不上”和“旧 Agent 不认识这个请求”）。公共沙盒抽成 `RpcSandbox` 基类，P58 的 10 条照样通过。

| 被测二进制 | 结果 |
| --- | --- |
| 旧代码（`crates/` 已跟踪改动临时 `git stash`，`6637954` 源码构建），测试文件是最终版 | 11 条里 10 条失败（子用例计 3 failures + 13 errors）；唯一通过的是“运行中的会话还没有 output”，那是要保住的现有行为 |
| 新代码 | 11 条全过，约 4 秒 |

stash 随即 `pop`，`git stash list` 为空；`/tmp/ccnm-exact-*` 与假 Agent 进程为 0。

### 3.2 OUT 矩阵

| 编号 | 覆盖 | 用例 |
| --- | --- | --- |
| OUT-01 | 3 MiB 输出头、中、尾标记全部取回，64 KiB 一页、50 页左右，与视图逐字节一致；stderr 单独读、不混 stdout | `test_a_multi_megabyte_output_comes_back_whole_and_the_streams_stay_apart`；Rust `out_pages_reassemble_to_the_view_under_any_budget` |
| OUT-02 | 中文、Emoji、无换行长行在预算 4、5、7、64、1000 下完整重组；1–3 字节预算遇到多字节字符时报 `max_bytes_too_small` 且 `min_bytes` 大于预算；空输出是空视图；非法字节换成 U+FFFD，流式结果在 1–4096 字节各种切块下都与整篇 `from_utf8_lossy` 一致 | `test_small_budgets_...`、`test_an_empty_output_...`；`session::view` 的 `streaming_matches_the_whole_document_at_any_chunk_size`；Rust `out_a_budget_below_one_character_says_what_it_needs` |
| OUT-03 | 解析成功时 `text` 是完整的 20 KiB 最终回答，原始 stdout（整份结果文档）另外完整读回 | `test_a_parsed_result_keeps_the_final_text_and_the_raw_stdout_apart` |
| OUT-04 | 同一游标重读同一页且游标名不变；别的会话、伪造、乱码、换流、服务端换进程之后都回 `-32012`，从 `null` 重来照样读到；视图生成后源文件再写入不影响；P58 之前的记录分页旧尾巴并标 `legacy_tail` | `test_cursors_are_bound_...`；Rust `out_cursors_expire_outside_what_issued_them`、`out_records_without_an_agent_run_page_what_they_kept`；`a_view_is_built_once_and_frozen`、`agent_output_is_a_redacted_frozen_view_of_a_finished_session` |
| OUT-05 | 私有 profile 路径跨 1000 字节切片边界、跨截头截点、跨任意流式分块时都被整段替换；真实二进制在沙盒里完成实例 profile 解析后替换路径 | `agent_output_is_a_redacted_...`（session_identity）；`over_the_cap_the_head_goes_at_a_character_and_nothing_private_survives`；`assigned_session.rs` 的 `agent_output_serves_the_redacted_view_of_a_finished_session` |
| OUT-06 | 超过保留上限只留尾部并标 `source_truncated`；拷到一半失败、拷的过程中视图换代，都不留任何快照文件，响应降级并写原因；Agent 联系不上 → `agent_unreachable`，旧 Agent → `agent_refused`，恢复后下一次拿到完整内容 | `a_view_built_under_a_smaller_cap_reports_what_it_dropped`；Rust `out_a_broken_or_shifting_copy_leaves_no_snapshot`、`out_an_unreachable_agent_falls_back_...`；Python `test_an_unreachable_agent_...`、`test_an_agent_that_refuses_...` |
| OUT-07 | 两份参考客户端重组与视图逐字节一致；`read_output` 到总量上限就停并标 `complete: False`；旧的无参数调用、`usage` 缺省语义不变（原有用例全过） | `test_the_reference_client_...`、`test_the_execution_backend_reads_output_through_its_own_method`；`tests.test_blackbox_client`、`tests.test_execution_backend` |

内存上限靠结构保证，没有单独测量：视图生成只用固定缓冲；Agent 一次最多回 4 MiB（RPC 每次只要 1 MiB）；RPC 每页从本机文件按区间读，最多 1 MiB。这条路径上没有一次读完整个输出的地方。

### 3.3 其他单元与集成测试

| 文件 | 新增 |
| --- | --- |
| `session/view.rs` | 6 条：流式与整篇一致、小流完整、超上限截头不漏路径且从字符开始、withheld、生成一次并冻结、上限下报告丢弃量 |
| `rpc/mod.rs` | 7 条 `out_*`；FakeRuns 可按流提供视图、从第 n 次起失败、从第 n 次起换代 |
| `tests/session_identity.rs` | 2 条：Agent 侧脱敏、冻结、分流；不跨 workspace、实例、模式，不认识的 id 与畸形 id 拒绝 |
| `crates/ccnm-cli/tests/assigned_session.rs` | 2 条：真实二进制的成功路径（含脱敏）；协议号 7 报 `CCNM_E_VERSION`、未知会话 `CCNM_E_NOT_READY`、畸形 id `CCNM_E_INVALID_ARGS`，询问不创建任何目录 |

原有用例的调整跟着行为走：测试替身的 `FakeRuns::ok` 让 Agent 视图等于它报告的内容；超长输出那条的 `cursor` 由“永远 null”改为“有更早的内容可翻”。

P57 的 `p57-output.py`（后来改写成 `tests/test_rpc_output.py`，探针已删）在新构建上重跑：它的假 Agent 早于 `agent-output`，所以走的是回退路径——输出带 `unavailable_reason: agent_refused`，stderr 也能用 `stream` 取到；探针的判定字段是 P57 时的，不再作为结论依据，结论以上面的正式回归为准。

## 4. 门禁

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | 通过 |
| `cargo test --workspace --locked --offline -- --test-threads=64` | 990 passed / 0 failed / 0 ignored（P58 为 973） |
| `python3 -B scripts/ci_gates.py` | 见 status 的 planning_validation_latest |
| `python3 scripts/check_protocol.py`、`tests.test_check_protocol` | 41 + 29 个 fixture 通过 |

## 5. 边界与未覆盖

- **两份拷贝都不会自动清理**：Agent 会话目录里的 `*.view`、RPC 的 `rpc/outputs/<session>/`。每个流通常至多约 32 MiB（全是非法字节的极端情况至多约 96 MiB）。有预览的清理是 P61。
- **`text` 没有上限**：计划要求保持它的语义，本轮未改；超大最终回答仍在一条响应里。
- **Agent 侧 `agent-run` / `agent-result` 为解析结果文档仍把整个 stdout 读进内存**，这是原有行为，不在本轮的结果快照路径上，留作记录。
- **首次读有延迟**：一个 32 MiB 的流要 32 次 ssh 往返（当前每次都完整握手）。
- **日志没有单独查脱敏**：回退时的 warn 日志写的是 Agent 返回的错误消息，那些消息本来就不带私有路径，但没有针对日志的测试。
- **Linux、真实 Agent 与 Controller、真机**未跑；真机结论在 P62。
- **提交 `e0e047a` 单独不能编译**：它纳入了整份 `crates/ccnm-cli/src/main.rs`，其中一行 `cursors` 字段属于下一个提交 `3e7b5fc` 才加的 `rpc::Context`。两者合起来（以及之后的每个提交）都能构建并通过全部测试；按不主动改写历史的约定没有修补这个中间提交，二分查找时把这两个提交当成一个。
