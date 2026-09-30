# 公开协议

给**外部程序**用的 ccnm 接口契约。人类用的命令行见 [使用说明](../usage.md)，ccnm 两个二进制之间的内部协议不在这里——那是实现细节。

| 文件 | 是什么 |
| --- | --- |
| [machine-protocol-v1.md](machine-protocol-v1.md) | 正式说明：传输、握手、六个方法、幂等、状态、取消、权限、错误码 |
| [schema/machine-protocol-v1.schema.json](schema/machine-protocol-v1.schema.json) | 每条消息的 JSON Schema |
| [fixtures/](fixtures/) | 成功、拒绝、断线、未知终态、过期结果的样例消息 |
| [../../clients/python/ccnm_machine_client.py](../../clients/python/ccnm_machine_client.py) | 可以直接抄走的单文件客户端，只用标准库 |

第二套契约是给**已经在跑的外部 Agent** 用的。`ccnm.workspace-mcp/1` **已于 2026-09-11 冻结**，依据是允许矩阵在真实 Claude Code 上跑过一次（[P11 记录](../research/p11-real-host-2026-09-11.md)）加一次 Linux Runtime 上的远端真实项目 dogfood（[P12 记录](../research/p12-real-project-2026-09-11.md)）；验收范围和不保证什么见[支持矩阵](../support-matrix.md)。往后加工具、加字段、加错误原因可以；删工具、改权限语义、改 `external_mcp` 三个值的含义要升到 `ccnm.workspace-mcp/2`。

| 文件 | 是什么 |
| --- | --- |
| [remote-workspace-mcp-v1.md](remote-workspace-mcp-v1.md) | Remote Workspace MCP 契约：`ccnm mcp bridge`、read/coding 权限、动态工具表与 annotations、生命周期、错误边界 |
| [schema/remote-workspace-mcp-v1.schema.json](schema/remote-workspace-mcp-v1.schema.json) + [fixtures-mcp/](fixtures-mcp/) | 消息形状和样例；实际数量以 `check_protocol.py` 输出为准 |

写编排项目的人还要看[执行接口交接](../orchestrator-handoff.md)：状态归属边界，以及建在这套协议上的最小 `ExecutionBackend` 示例。

**当前状态：契约已冻结，实现仍有下列差距。** `ccnm rpc` 已经能说这套协议的 `print` 模式：

```bash
ccnm rpc
```

它从 stdin 读、往 stdout 写，没有网络端口。谁能启动这个进程，谁就有这套 API 的全部权限。

**`ccnm.machine/1` 已于 2026-09-10 冻结。** 依据是两个 provider 各跑通过一次真机双机闭环，并与人类 CLI 走同一件事做副作用对照——两条腿的产物属主相同，`usage` 端到端到达调用方（记录见 [Claude](../research/p7-real-machine-2026-09-10.md) 与 [Codex](../research/p7-codex-parity-2026-09-10.md)）。往后加字段、加方法、加非终态可以；删字段、改语义、加终态要升到 `ccnm.machine/2`，规则见[协议第 13 节](machine-protocol-v1.md#13-兼容规则)。

冻结的是**契约**。实现仍然比契约少，下面这份清单就是差在哪里——补上它们属于加法，不需要升版本，也不违反冻结。差距全是这个方向，没有反过来的：

- `interactive` 模式没有实现，也不在 `hello` 的 `capabilities.modes` 里——调用它得到 `-32013`。
- 结果**不会自动过期**，`expires_at` 不出现。P61 起运维人员可以用 `ccnm cleanup` 显式清理：清过的 session `session.result` 回 `-32012`（`reason: cleaned`），`status` 照常，`start_key` 仍指回原 session。
- **`-32008`（`busy`）只覆盖"启动那一刻已经被占"。** P60 起 `session.start` 先请 Runtime 看一眼写入 guard，有进程正持有就回 `-32008`（详见[协议第 10 节](machine-protocol-v1.md#错误码表)下的表）。但这只是一次观察，不是预留：看完之后、会话打开工具之前被别人抢先的，表现仍是**会话起来了然后失败**；Runtime 问不到时这一步不做，启动照常进行。按 `-32008` 退避的客户端，还得处理"接受了但因写锁 `failed`"这种结局。

P58（2026-09-28）起，下面几条是当前实现的行为。它们都在契约允许的范围内，写在这里是因为写客户端时会碰到：

- **`session.stop` 只停这一个 session。** 服务端在派发前就给这次运行定好了 Agent 上的会话 id，stop 点名停它；运行还没到 Agent 时，Agent 先把这个 id 占住，运行到了也不会启动。之前的实现只按 workspace 找 tmux，停不到 print 运行，同 workspace 有交互会话时反而停掉那个（[P57 实测](../research/2026-09-28-p57-core-baseline.md)）。
- **还没派发就收到 stop，这次运行不会发出去**，终态 `failed`、`stop_requested: true`。
- **不合 `session_id` 形状的句柄一律 `-32602`**，在读任何文件之前拒绝；合形状但不存在的仍是 `-32009`。
- **session 创建后改绑 workspace（换了 Agent 节点），旧句柄的 stop 回 `-32007`**，`effect: none`，不会发到新机器上，也不记成请求过 stop。
- **P58 之前的服务端接受的、还没结束的 session 不能用 `session.stop` 停**：那时没记 Agent 上的会话 id，只能按 workspace 猜，现在拒绝猜，回 `-32000`。
- **运行派发之后连接断了或 Agent 在启动后出错，终态是 `unknown`**，不是 `failed`——Agent 那边可能已经在跑。Agent 在启动任何东西之前的拒绝（没登录、Runtime 不通、写锁被占等）仍是 `failed`；P63 起，ssh 自己说还没连进去（解析不了主机名、TCP 连不上、认证被拒、主机指纹不符、密钥交换失败）的也是 `failed`，这时 stop 失败的 `effect` 是 `none`。
- 同一个 `start_key` 按原串逐字节比较，不同的键不会再合并（此前 `任务-一` 与 `任务-二`、`a/b` 与 `ab` 会被当成一个键）；升级前写下的键照样认，不会把已接受的任务当新任务重跑。

P63（2026-09-30）起，P62 真机查出的两处与契约不符的行为修正了（[P62 记录](../research/2026-09-30-p62-real-machine.md) F16、F17）：

- **客户端断开之后，已接受的 session 照常派发、跑完**（第 8.1 节）。每个 session 由自己的 owner 进程（`ccnm internal rpc-run`，独立进程组）带着跑，不再挂在 `ccnm rpc` 进程上；客户端关 stdin 后 `ccnm rpc` 立刻退出，重连能查到真实的 `running` / 终态与结果。此前派发前断开的任务从未发出却读成 `unknown`，派发后断开的永远 `unknown`。owner 进程真的消失（被 `kill -9`）时仍读成 `unknown`。
- **`session.stop` 对已派发的 session，停止标志在联系 Agent 之前就记下**，Agent 怎么回答都不会丢；Agent 发出 SIGTERM 后最多等 5 秒进程组退出，还确认不了时回 `stopping`（第 5.6 节），不再回 `-32000`。Agent 连不上等其他错误照样返回错误，标志同样保留。

P64（2026-09-30）修了上面第一条带出来的一个缺陷：owner 进程先写结局再退出，而 `session.status` / `result` / `stop` 是先读记录、再查 owner 在不在——两步正好夹住它退出的那一刻时，一个正常结束的 session 会被回成 `unknown`（实测轮询时约 40 次 1 次）。现在查到 owner 不在了就把记录重读一遍，结局已经落盘的按结局回；重读后仍没有结局的才是 `unknown`。**P63 的构建上拿到 `unknown` 时再查一次 `session.status`**，结局已经写下的话第二次就是对的。详见 [P64 记录](../research/2026-09-30-p64-stop-outcome-same-number-builds.md)第 5.1 节。

P65（2026-09-30）起 `session.result` 多一个可选字段 `failure`（[协议 5.5 节](machine-protocol-v1.md#55-sessionresult)）：会话不是以 Agent 进程自己结束收场时——没起来，或服务端说不清——给出原因。`code` 与 `ccnm_code` 用的是第 10 节错误码表的同一套，`detail` 给人看、家目录前缀写成 `~`、最长 2048 字节。此前这类会话到调用方手里只是一个 `exit_code`、`text` 都为 null 的 `failed`，原因要去 Operator 的记录文件里找（[P62 记录](../research/2026-09-30-p62-real-machine.md) F3）。按第 13 节这是加法；升级前已结束的会话只有 `detail`。第 9 节"任何字段里都不出现私有目录的绝对路径"目前只有 `failure.detail` 做了这一步，**被拒调用的 `error.message` 仍是 ccnm 给人看的原话，没有过这道处理**。

P59（2026-09-29）起 `session.result` 的输出按[协议第 9 节](machine-protocol-v1.md#输出引用)实现：每个流保留最后 32 MiB，第一页是末尾、`cursor` 往前翻，`max_bytes` 生效，stderr 用 `output.stream` 单独取。完整内容在 Agent 上生成、在第一次读时整份拷到本机，之后翻页不再联系 Agent。Agent 还是 P59 之前的版本或这时联系不上，服务端给的是旧版本留下的那段尾部，并用 `unavailable_reason` 说明，不当成完整输出。游标只在发出它的 `ccnm rpc` 进程里有效。详见 [P59 记录](../research/2026-09-29-p59-output-snapshot.md)。

P60（2026-09-29）起 `session.start` 在分配 session id 之前先问写入 guard：Operator 经 Agent 问到 Runtime 执行账号（内部协议 9），因为锁在执行账号自己的 state 目录里。有进程正持有回 `-32008`；没人持有却也交不出去（上一个会话故意留着、异常退出留下的、标记损坏或读不了）回 `-32007`，`data.reason` 说是哪一种，这种要人处理，重试不会好。同一个 `start_key` 的重发先按原记录回答，不经过这一步。Agent、Runtime 任何一端早于 P60 时问不到，启动照 P59 的样子进行。详见 [P60 记录](../research/2026-09-29-p60-write-guard-observation.md)。

P61（2026-09-30）起，session 的输出只会被运维人员显式清掉（`ccnm cleanup <workspace> --apply <令牌>` 或 `workspace remove --purge`）。清理只删 Operator 这边的输出拷贝和记录里大的部分，记录本身留作墓碑，所以清过的 session 仍然查得到状态，`result` 回 `-32012` 并带 `reason: cleaned`，同一个 `start_key` 不会被当成新任务重跑。还没结束、状态说不清的 session 不会被清。详见 [P61 记录](../research/2026-09-30-p61-cleanup.md)。

校验 schema 和 fixture：

```bash
python3 scripts/check_protocol.py
```

只用 Python 标准库，不需要装任何东西。它一次检查两套契约：fixture 符合各自声明的 schema、错误码（机器协议是数字码，Remote MCP 是 `CCNM_E_*` 名字）和说明文档一致、schema 自己没有拼错的关键字。

**它证明的是这几份文件互相自洽，不是 `ccnm rpc` 的行为和它们一致。** 那要靠 `tests/test_blackbox_client.py` 的契约测试（只走字节流）和上面那两次真机闭环。`-32008` 就是这个区别的例子：P60 之前 fixture 和说明文档一直对得上，实现却从不发它。

## fixture 的格式

每个 fixture 文件外层是元数据，`message` 才是协议消息本体：

```json
{
  "$schema_ref": "#/$defs/session_start_response_ok",
  "$note": "正常启动：拿到 handle 就返回，不等任务跑完",
  "message": {"jsonrpc": "2.0", "id": 3, "result": {"session": "s-...", "state": "starting"}}
}
```

`$schema_ref` 指向 schema 文件里的一个定义，检查脚本按它校验 `message`。加新 fixture 就是加一个这样的文件，不用改脚本。
