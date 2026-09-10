# 公开协议

给**外部程序**用的 ccnm 接口契约。人类用的命令行见 [使用说明](../usage.md)，ccnm 两个二进制之间的内部协议不在这里——那是实现细节。

| 文件 | 是什么 |
| --- | --- |
| [machine-protocol-v1.md](machine-protocol-v1.md) | 正式说明：传输、握手、六个方法、幂等、状态、取消、权限、错误码 |
| [schema/machine-protocol-v1.schema.json](schema/machine-protocol-v1.schema.json) | 每条消息的 JSON Schema |
| [fixtures/](fixtures/) | 成功、拒绝、断线、未知终态、过期结果的样例消息 |

**当前状态：草案。** 没有任何命令实现它——`ccnm rpc` 是 P5 的事，外部消费者验证和 v1 兼容承诺是 P6 的事。在那之前字段和语义都可能改，不要对外宣称 v1 已稳定。

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
