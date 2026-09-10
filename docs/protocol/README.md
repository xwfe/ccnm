# 公开协议

给**外部程序**用的 ccnm 接口契约。人类用的命令行见 [使用说明](../usage.md)，ccnm 两个二进制之间的内部协议不在这里——那是实现细节。

| 文件 | 是什么 |
| --- | --- |
| [machine-protocol-v1.md](machine-protocol-v1.md) | 正式说明：传输、握手、六个方法、幂等、状态、取消、权限、错误码 |
| [schema/machine-protocol-v1.schema.json](schema/machine-protocol-v1.schema.json) | 每条消息的 JSON Schema |
| [fixtures/](fixtures/) | 成功、拒绝、断线、未知终态、过期结果的样例消息 |
| [../../clients/python/ccnm_machine_client.py](../../clients/python/ccnm_machine_client.py) | 可以直接抄走的单文件客户端，只用标准库 |

写编排项目的人还要看[执行接口交接](../orchestrator-handoff.md)：状态归属边界，以及建在这套协议上的最小 `ExecutionBackend` 示例。

**当前状态：草案，有实现。** `ccnm rpc` 已经能说这套协议的 `print` 模式：

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

校验 schema 和 fixture：

```bash
python3 scripts/check_protocol.py
```

只用 Python 标准库，不需要装任何东西。它检查 fixture 符合声明的 schema、错误码和说明文档一致、schema 自己没有拼错的关键字。

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
