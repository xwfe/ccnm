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
- 输出**不分页**：`session.result` 一次给最后 8 KiB，`cursor` 永远是 `null`。把任何游标填回去都会得到 `-32012`，因为这个 build 从没发过游标。
- 结果**不过期**：记录一直留着，`expires_at` 不出现。清理靠 ccnm 本来的维护动作。
- **`-32008`（`busy`）从来不会返回。** 工作树的写入 guard 是 Runtime 侧的 MCP 进程在会话跑起来之后才去拿的，`session.start` 那一刻没人检查它。所以工作树被别人占着时，你看到的不是启动被拒，而是**会话起来了然后失败**。按 `-32008` 写退避重试的客户端等不到这个码。

P58（2026-09-28）起，下面几条是当前实现的行为。它们都在契约允许的范围内，写在这里是因为写客户端时会碰到：

- **`session.stop` 只停这一个 session。** 服务端在派发前就给这次运行定好了 Agent 上的会话 id，stop 点名停它；运行还没到 Agent 时，Agent 先把这个 id 占住，运行到了也不会启动。之前的实现只按 workspace 找 tmux，停不到 print 运行，同 workspace 有交互会话时反而停掉那个（[P57 实测](../research/2026-09-28-p57-core-baseline.md)）。
- **还没派发就收到 stop，这次运行不会发出去**，终态 `failed`、`stop_requested: true`。
- **不合 `session_id` 形状的句柄一律 `-32602`**，在读任何文件之前拒绝；合形状但不存在的仍是 `-32009`。
- **session 创建后改绑 workspace（换了 Agent 节点），旧句柄的 stop 回 `-32007`**，`effect: none`，不会发到新机器上，也不记成请求过 stop。
- **P58 之前的服务端接受的、还没结束的 session 不能用 `session.stop` 停**：那时没记 Agent 上的会话 id，只能按 workspace 猜，现在拒绝猜，回 `-32000`。
- **运行派发之后连接断了或 Agent 在启动后出错，终态是 `unknown`**，不是 `failed`——Agent 那边可能已经在跑。Agent 在启动任何东西之前的拒绝（没登录、Runtime 不通、写锁被占等）仍是 `failed`。
- 同一个 `start_key` 按原串逐字节比较，不同的键不会再合并（此前 `任务-一` 与 `任务-二`、`a/b` 与 `ab` 会被当成一个键）；升级前写下的键照样认，不会把已接受的任务当新任务重跑。

校验 schema 和 fixture：

```bash
python3 scripts/check_protocol.py
```

只用 Python 标准库，不需要装任何东西。它一次检查两套契约：fixture 符合各自声明的 schema、错误码（机器协议是数字码，Remote MCP 是 `CCNM_E_*` 名字）和说明文档一致、schema 自己没有拼错的关键字。

**它证明的是这几份文件互相自洽，不是 `ccnm rpc` 的行为和它们一致。** 那要靠 `tests/test_blackbox_client.py` 的契约测试（只走字节流）和上面那两次真机闭环。上面 `-32008` 那条就是这个区别的例子：fixture 和说明文档对得上，实现却从不发它。

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
