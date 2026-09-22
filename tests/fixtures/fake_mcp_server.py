"""一个最小的 stdio MCP server，给 P49 的中立客户端测试当"项目里声明的 server"。

只认 initialize、tools/list、tools/call，按行读写 JSON-RPC。说 2024-11-05：
真实世界里还有这么老的 server（`@modelcontextprotocol/server-github`），ccnm
得接。四个工具：

- echo：原样回 arguments（带一份内容相同的 structuredContent，ccnm 应该只留文字）；
- big：回 52 000 字节的文字（1000 行，每行 52 字节），超过一次交回的上限，剩下的要用 read_output 读；
- whoami：回这个进程看得到的两个环境变量，验证配置里给的 token 到了、Agent 的
  登录没到；
- pid：回自己的进程号，验证会话结束时它被收掉了。
"""

import json
import os
import sys


def reply(message, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}) + "\n")
    sys.stdout.flush()


TOOLS = [
    {"name": "echo", "description": "Echo the arguments back.", "inputSchema": {"type": "object"}},
    {"name": "big", "description": "A lot of text.", "inputSchema": {"type": "object"}},
    {"name": "whoami", "description": "Two variables it can see.", "inputSchema": {"type": "object"}},
    {"name": "pid", "description": "Its own process id.", "inputSchema": {"type": "object"}},
]

for line in sys.stdin:
    try:
        message = json.loads(line)
    except ValueError:
        continue
    method = message.get("method")
    if "id" not in message:
        continue
    if method == "initialize":
        reply(message, {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake", "version": "1.0"},
            "instructions": "Keep calls small.",
        })
    elif method == "tools/list":
        reply(message, {"tools": TOOLS})
    elif method == "tools/call":
        name = message["params"]["name"]
        arguments = message["params"].get("arguments") or {}
        if name == "echo":
            text = json.dumps(arguments, sort_keys=True)
            reply(message, {"content": [{"type": "text", "text": text}], "structuredContent": arguments})
        elif name == "big":
            body = "".join(f"line {i:05d} {'x' * 40}\n" for i in range(1000))
            reply(message, {"content": [{"type": "text", "text": body}]})
        elif name == "whoami":
            text = "token=%s agent=%s" % (
                os.environ.get("DB_TOKEN", ""),
                os.environ.get("ANTHROPIC_API_KEY", ""),
            )
            reply(message, {"content": [{"type": "text", "text": text}]})
        elif name == "pid":
            reply(message, {"content": [{"type": "text", "text": str(os.getpid())}]})
        else:
            reply(message, {"content": [{"type": "text", "text": "no such tool"}], "isError": True})
    else:
        sys.stdout.write(json.dumps({
            "jsonrpc": "2.0", "id": message["id"],
            "error": {"code": -32601, "message": "method not found"},
        }) + "\n")
        sys.stdout.flush()
