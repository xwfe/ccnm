# 公开协议

给**外部程序**用的 ccnm 接口契约。人类用的命令行见 [使用说明](../usage.md)，ccnm 两个二进制之间的内部协议不在这里——那是实现细节。

| 文件 | 是什么 |
| --- | --- |
| [machine-protocol-v1.md](machine-protocol-v1.md) | 正式说明：传输、握手、六个方法、幂等、状态、取消、权限、错误码 |
| [schema/machine-protocol-v1.schema.json](schema/machine-protocol-v1.schema.json) | 每条消息的 JSON Schema |
| [fixtures/](fixtures/) | 成功、拒绝、断线、未知终态、过期结果的样例消息 |

**当前状态：草案，有实现。** `ccnm rpc` 已经能说这套协议的 `print` 模式：

```bash
ccnm rpc
```

它从 stdin 读、往 stdout 写，没有网络端口。谁能启动这个进程，谁就有这套 API 的全部权限。

**但它还没跟真实 Agent 跑通过一次。** 所有测试都是离线的：单元测试注入替身执行器，集成测试跑真实二进制但每次调用要么在本地预检就失败、要么只读配置。用真实 provider 做双机闭环是 P6.3 的事，v1 兼容承诺也要等到那时。在那之前字段和语义都可能改，不要对外宣称 v1 已稳定。

当前实现与契约的差距，都是"实现得比契约少"，没有反过来的：

- `interactive` 模式没有实现，也不在 `hello` 的 `capabilities.modes` 里——调用它得到 `-32013`。
- 输出**不分页**：`session.result` 一次给最后 8 KiB，`cursor` 永远是 `null`。把任何游标填回去都会得到 `-32012`，因为这个 build 从没发过游标。
- 结果**不过期**：记录一直留着，`expires_at` 不出现。清理靠 ccnm 本来的维护动作。

校验 schema 和 fixture：

```bash
python3 scripts/check_protocol.py
```

只用 Python 标准库，不需要装任何东西。它检查 fixture 符合声明的 schema、错误码和说明文档一致、schema 自己没有拼错的关键字。**它不证明实现正确**，因为现在还没有实现。

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
