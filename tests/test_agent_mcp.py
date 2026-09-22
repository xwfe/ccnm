"""P50：Agent 机器上装好的 MCP server，经 `ccnm internal agent-skills` 的 call_mcp_tool 交给会话。

真实二进制 + 与 ccnm 无关的 MCP 客户端（和 test_agent_skills 同一个），payload 由测试
自己拼，HOME 里的 ~/.claude.json 由测试写。stdio 的 server 是 tests/fixtures/fake_mcp_server.py
（P49 那份）；HTTP 的在本进程里起在 127.0.0.1 上，回复用 SSE 而且回完不关流——ccnm 经系统的
curl 连它，要自己在拿到回复后停下。
"""

from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_client import McpClient, is_error, result_text  # noqa: E402
from test_remote_workspace_mcp import BINARY  # noqa: E402

FAKE = Path(__file__).resolve().parent / "fixtures" / "fake_mcp_server.py"


def payload(home: Path, mcp: dict | None, skills: bool = True) -> str:
    body = {"protocol": 1, "home": str(home), "session": "sess-agent-mcp"}
    if not skills:
        body["skills"] = False
    if mcp is not None:
        body["mcp"] = mcp
    raw = json.dumps(body).encode("utf-8")
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


class HttpServer:
    """一个 streamable HTTP 的 MCP server：initialize 回 JSON 并给会话号，其余回 SSE。"""

    def __init__(self):
        self.sessions = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def do_DELETE(self):
                self.send_response(200)
                self.send_header("content-length", "0")
                self.end_headers()

            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("content-length") or 0))
                owner.sessions.append(self.headers.get("mcp-session-id"))
                message = json.loads(body)
                if "id" not in message:
                    self.send_response(202)
                    self.send_header("content-length", "0")
                    self.end_headers()
                    return
                method = message.get("method")
                if method == "initialize":
                    reply = {"jsonrpc": "2.0", "id": message["id"], "result": {
                        "protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                        "serverInfo": {"name": "web", "version": "1"}}}
                    data = json.dumps(reply).encode()
                    self.send_response(200)
                    self.send_header("content-type", "application/json")
                    self.send_header("mcp-session-id", "web-session")
                    self.send_header("content-length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                    return
                if method == "tools/list":
                    result = {"tools": [{"name": "echo", "inputSchema": {"type": "object"}}]}
                else:
                    text = json.dumps(message["params"].get("arguments") or {}, sort_keys=True)
                    result = {"content": [{"type": "text", "text": "web " + text}]}
                reply = {"jsonrpc": "2.0", "id": message["id"], "result": result}
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("connection", "close")
                self.end_headers()
                self.wfile.write(b": hi\n\n")
                self.wfile.write(f"data: {json.dumps(reply)}\n\n".encode())
                self.wfile.flush()
                # 回完不关：客户端得自己停。
                time.sleep(20)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/mcp"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class AgentMcpTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="ccnm-agent-mcp-")
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        self.web = HttpServer()
        self.addCleanup(self.web.close)
        (self.home / ".claude.json").write_text(json.dumps({"mcpServers": {
            "db": {"command": sys.executable, "args": [str(FAKE)], "env": {"DB_TOKEN": "t0k"}},
            "web": {"type": "http", "url": self.web.url},
            "far": {"type": "http", "url": "https://example.invalid/mcp"},
        }}), encoding="utf-8")

    def client(self, mcp: dict | None, skills: bool = True) -> McpClient:
        argv = [str(BINARY), "internal", "agent-skills", "--payload", payload(self.home, mcp, skills)]
        # 客户端给它的环境里有 Agent 的登录：转出去的 server 不该拿到。
        env = {"PATH": os.environ.get("PATH", ""), "ANTHROPIC_API_KEY": "agent-login"}
        client = McpClient(argv, env)
        self.addCleanup(client.close)
        client.initialize()
        return client

    def tool(self, client: McpClient, name: str) -> dict:
        return next(t for t in client.tools() if t["name"] == name)

    def test_by_default_only_an_address_on_another_machine_is_offered(self):
        client = self.client({})
        self.assertEqual(sorted(t["name"] for t in client.tools()),
                         ["call_mcp_tool", "load_skill", "read_mcp_result"])
        call = self.tool(client, "call_mcp_tool")
        self.assertTrue(call["description"].endswith("Servers here: far."), call["description"])
        self.assertEqual(call["_meta"]["anthropic/maxResultSizeChars"], 200000)

        overview = result_text(client.call_tool("call_mcp_tool", {}))
        self.assertIn("- db (~/.claude.json): not relayed: it runs as a program on this machine", overview)
        self.assertIn("- web (~/.claude.json): not relayed: its address is on this machine", overview)
        self.assertIn("- far (~/.claude.json): not started", overview)

        refused = client.call_tool("call_mcp_tool", {"server": "db", "tool": "echo"})
        self.assertTrue(is_error(refused), refused)
        self.assertIn("[agent_mcp] local", result_text(refused))
        # 名字对、地址不存在：经 curl 连不上，照实说。
        unreachable = client.call_tool("call_mcp_tool", {"server": "far"})
        self.assertTrue(is_error(unreachable), unreachable)
        self.assertIn("cannot reach https://example.invalid/mcp", result_text(unreachable))

    def test_named_local_servers_answer_over_stdio_and_http_and_long_results_read_back_whole(self):
        client = self.client({"local": ["db", "web"], "hidden": ["far"]})
        self.assertTrue(self.tool(client, "call_mcp_tool")["description"].endswith("Servers here: db, web."))

        echoed = client.call_tool("call_mcp_tool", {"server": "db", "tool": "echo",
                                                     "arguments": {"q": 1, "nested": {"a": [1, 2]}}})
        self.assertEqual(result_text(echoed), '{"nested": {"a": [1, 2]}, "q": 1}')
        env = client.call_tool("call_mcp_tool", {"server": "db", "tool": "whoami"})
        self.assertEqual(result_text(env), "token=t0k agent=")

        started = time.monotonic()
        web = client.call_tool("call_mcp_tool", {"server": "web", "tool": "echo", "arguments": {"x": [1]}})
        self.assertEqual(result_text(web), 'web {"x": [1]}')
        self.assertLess(time.monotonic() - started, 15, "stopped at the reply, not at the end of the stream")
        self.assertIn("web-session", self.web.sessions)

        big = client.call_tool("call_mcp_tool", {"server": "db", "tool": "big"})
        parts = [c["text"] for c in big["content"] if c["type"] == "text"]
        # 一次最多 32 KiB，尽量断在换行后面。
        self.assertLessEqual(len(parts[0].encode()), 32768)
        self.assertIn("read_mcp_result ref=", parts[1])
        ref = parts[1].split("ref=")[1].split()[0]
        offset = int(parts[1].split("offset=")[1].split(".")[0])
        self.assertEqual(offset, len(parts[0].encode()))
        whole = parts[0]
        while True:
            page = client.call_tool("read_mcp_result", {"ref": ref, "offset": offset})
            texts = [c["text"] for c in page["content"] if c["type"] == "text"]
            whole += texts[0]
            offset += len(texts[0].encode())
            if "that is the end" in texts[1]:
                break
        self.assertEqual(len(whole.encode()), 52000)
        self.assertEqual(whole, "".join(f"line {i:05d} {'x' * 40}\n" for i in range(1000)))

        hidden = client.call_tool("call_mcp_tool", {"server": "far"})
        self.assertTrue(is_error(hidden), hidden)

    def test_the_servers_it_started_go_with_it(self):
        client = self.client({"local": ["db"]})
        pid = int(result_text(client.call_tool("call_mcp_tool", {"server": "db", "tool": "pid"})))
        os.kill(pid, 0)
        client.close()
        for _ in range(50):
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.1)
        self.fail(f"server {pid} outlived the session's server")

    def test_without_the_mcp_half_or_with_nothing_to_offer_the_tools_stay_out(self):
        self.assertEqual([t["name"] for t in self.client(None).tools()], ["load_skill"])
        only_mcp = self.client({"local": ["db"]}, skills=False)
        self.assertEqual(sorted(t["name"] for t in only_mcp.tools()), ["call_mcp_tool", "read_mcp_result"])
        nothing = self.client({"hidden": ["far"]})
        self.assertEqual([t["name"] for t in nothing.tools()], ["load_skill"])


if __name__ == "__main__":
    unittest.main()
