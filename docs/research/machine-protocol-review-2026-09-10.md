# Machine Protocol v1 草案评审（2026-09-10）

对 [协议草案](../protocol/machine-protocol-v1.md)、[schema](../protocol/schema/machine-protocol-v1.schema.json) 和 38 个 fixture 的评审记录。

**评审形式先说清楚：这是单人自审，不是多人评审。** 做法是逐条对照 ROADMAP 的 P4.1…P4.5 验收、逐条对照 JSON-RPC 2.0 官方规范、再对照 ccnm 现有实现里的真实数据结构。没有第二个人复核，也没有任何外部消费者试写过客户端——那是 P6 的事。所以本文的结论是"草案自洽且与现状对得上"，不是"协议已经好用"。

## 一、逐条核对

| 验收 | 落点 | 状态 |
| --- | --- | --- |
| P4.1 协议独立于内部 v1/v2；握手声明 protocol/version/capabilities；framing、大小上限、非法消息、未知 method/param/capability、版本不匹配；采用标准前核对规范 | 说明第 2、3、11 节；schema 的 request/response 定义 | 覆盖 |
| P4.2 六个方法定稿；start 尽快返回 handle；不传 PTY；不承诺 interactive 有结构化结果 | 第 4、5 节 | 覆盖 |
| P4.3 request_id 与启动幂等键的区别；崩溃可恢复/报 uncertain；状态含执行中/终态/取消中/未知；结果含身份、结构化错误、可选 usage/cost、有界输出引用 | 第 6、7 节；`uncertain-after-crash` 等 fixture | 覆盖 |
| P4.4 重试、超时、stop 幂等、取消判据、EOF 与 Agent 生命周期；三种"成功"分开；recoverable 不等于可自动重试 | 第 8 节 | 覆盖 |
| P4.5 权限最小授予；不泄露私有路径/凭据；分页/保留/过期有契约；旧 peer 退出码 0 不掩盖协议错误；写 fixture 并评审 | 第 9、10 节；`reject-stale-peer-exit-zero`、`reject-expired-*`、本文 | 覆盖 |

### 与 JSON-RPC 2.0 规范的核对

逐项对过[官方规范](https://www.jsonrpc.org/specification)：请求对象的 `jsonrpc`/`method`/`params`/`id` 成员、通知的定义与"服务端不得回复通知"、响应对象 `result` 与 `error` 互斥、错误对象的 `code`/`message`/`data`、五个预定义错误码的数值、`-32000..-32099` 是实现定义区间、`rpc.` 前缀保留、批量请求规则。

本协议在规范允许的范围内做了三处收紧，都写进了文档：不支持批量、不使用通知、请求的 `id` 不接受 `null` 和小数。收紧不改变规范语义，只是缩小了允许的输入集合。

### 与现有实现的核对

协议里的每个概念都对得上仓库里已有的东西，没有凭空发明：

| 协议 | 实现依据 |
| --- | --- |
| `provider` 取值 | `AgentProvider`（claude / codex） |
| instance 引用 `node` + `instance` | `InstanceRef`、`AgentIdentity` |
| identifier 的字符规则 | `instance.rs` 的 `identifier()`，1..64 个 `[A-Za-z0-9][A-Za-z0-9_-]*` |
| `capabilities`（print/interactive/ssh_mcp/native_colocated） | `Capabilities` / `instance_capabilities()` |
| `state` 六个取值 | `SessionState`（Starting/Running/Completed/Failed/Stopping/Unknown） |
| `outcome` 字段 | `session::Outcome`（exit_code/timed_out/duration_ms） |
| `usage` / `cost` | `RunResult` 的 `usage`、`total_cost_usd` |
| `ccnm_code` | `ErrorCode` 的 14 个 `CCNM_E_*` 名字 |

## 二、评审当场改掉的问题

这些是评审真正的产出——写的时候没发现，对着规范和自己的 fixture 才看出来。

### 1. 响应的 `id` 不允许 `null`，与规范冲突

schema 里请求和响应共用同一个 `id` 定义，都不许 `null`。但 JSON-RPC 规定：那一行不是合法 JSON、或者不是合法 Request 对象时，服务端**必须**回 `id: null`——它根本取不到 id。

更难堪的是这个矛盾就摆在自己的 fixture 里：`reject-batch` 的说明写着"id 为 null，因为服务端无法从一个数组里确定 id"，而它的 `message` 里 `id` 是 `19`。说明和内容自己打自己。

改：新增 `response_id`（多一个 `null`），`response_error` 改用它；请求侧保持不许 `null`。文档补了一节说明客户端会收到 `id: null` 的错误响应意味着什么。

### 2. 预定义错误码没有 fixture，而检查脚本查不出来

检查脚本原本只强制 `-32000..-32099` 区间的码有 fixture，于是 `-32700`（parse error）和 `-32603`（internal error）写进了文档表格却一个样例都没有。**写进文档却没有样例的码，等于没定义**——尤其这两个的 `id` 该填什么正是第 1 条踩的坑。

改：补 `reject-parse-error`、`reject-invalid-request`、`reject-internal` 三个 fixture，检查脚本对文档表里的**每个**码都要求有样例。

### 3. `start_key` 的作用域没定义

两个不同 workspace 用同一个键会怎样？文档没说。这种事留白，两个实现会给出两种答案。

改：明确作用域是单个 workspace，跨 workspace 同名键互不影响。

### 4. 没有 `session.list`，但后果没写

v1 的六个方法里没有"列出我的 session"。这不是疏漏（P4.2 就定了这六个），但它有一个必须让调用方知道的后果：**session id 只能由客户端自己持久化**，丢了就只能靠 `start_key` 找回。

改：在幂等那一节写清楚，并给出直接建议——只要这次执行的结果对你有意义，就给它一个 `start_key`。

### 5. `expired` 和 `not_found` 的边界不自洽

文档说"曾经存在但已清理"返回 `expired`。可服务端要知道一个 id 曾经存在，就得留墓碑；墓碑也会过期，那之后只能返回 `not_found`。原文写得太绝对，会让调用方以为 `not_found` 能证明"我记错了"。

改：说明墓碑本身也会过期，`not_found` 的准确含义是"服务端不知道这个东西"。

### 6. schema 表达不出"print 模式必须有 prompt"

原先 `input` 是一个共用定义，`prompt` 可选、`additionalProperties: true`，于是 `{"mode":"print","input":{}}` 能过 schema。而文档说 print 模式的 input 是 `{"prompt": "..."}`——文档比 schema 严，schema 就没起到作用。

改：`session_start_params` 按 `mode` 拆成两个 `oneOf` 分支，print 分支 `prompt` 必填且不允许多余字段。interactive 分支保持开放，并在 schema 里注明形状尚未定稿——P3 的真机验收没有覆盖它的结构化输入，不假装已经定义。

### 7. `agents.list` 撞上响应上限怎么办没说

改：明确不分页，并写明真撞上上限时返回 `-32603` 而不是悄悄截断——半截列表会让调用方以为某个 instance 不存在。

## 三、明知而留下的

这些不是疏忽，是权衡后的决定，记在这里以免下一轮当成新发现。

1. **客户端不声明自己的 capabilities。** `hello` 只有客户端报 `protocol_versions`，没有反向的能力声明。v1 没有服务端推送，用不上。将来加推送时必须一并解决，否则服务端无从知道对方能不能收。
2. **`-32006 workspace` 合并了内部的两个码。** `CCNM_E_MOUNT` 和 `CCNM_E_WRONG_WORKSPACE` 在公开协议里是同一个码，靠 `data.ccnm_code` 区分。理由是对调用方而言这两种情况的处理方式一样：都是去修那台机器，不是改请求。
3. **1 MiB 的行上限是拍的。** 没有实测依据，只是一个明显够用又不至于让实现分配无界缓冲的数。P5 实现时如果发现真实 prompt 会超，就改这个数字并记录依据。
4. **interactive 模式的输入形状没定稿。** 见第二部分第 6 条。
5. **`session.result` 的 `text` 对 interactive 可能永远是 null。** 这是 P3 实测的结论（Claude 的 `--print` 才写结构化结果），不是协议想不想给的问题。
6. **状态里没有 `stopped`。** 被停掉的 session 终态是 `failed` + `stop_requested: true`，因为 ccnm 现在的实现就是这么记的。宁可让客户端多读一个布尔，也不虚构一个服务端给不出的状态。

## 四、这份草案还没有被什么验证过

说清楚边界，免得后面拿这次评审当验收：

- **没有任何实现。** 文档里每一句"服务端必须…"都还没有被任何代码执行过。`ccnm rpc` 是 P5。
- **没有外部消费者。** 没人按这份文档写过客户端，所以"够不够用"完全没有证据。P6 会建一个不链接任何 ccnm crate 的黑盒 client 来回答这个问题，那时大概率要改草案。
- **检查脚本只做结构检查。** 38 个 fixture 符合 schema、错误码和文档一致——这只证明这几份文件互相自洽，不证明协议设计得对。
- **schema 是 JSON Schema 的一个子集。** 校验器只实现了用到的关键字，白名单外一律报错。它挡得住拼错的关键字，但不等于这份 schema 拿任何标准校验器跑都完全等价。

## 五、本轮验证

```text
python3 scripts/check_protocol.py                       通过：38 个 fixture
python3 -m unittest tests.test_check_protocol -q        18 passed
python3 scripts/check_plan.py                           通过
python3 -m unittest discover -s tests -p 'test_*.py'    见 status.json 的本轮记录
git diff --check                                        通过
```

没有修改 Rust，本轮不重跑也不重报 Rust 门禁数字。
