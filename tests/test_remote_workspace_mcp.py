"""P11：用一个与 ccnm 无关的 MCP 客户端重放同一套允许矩阵。

Rust 那边的 `crates/ccnm-cli/tests/external_mcp.rs` 已经测过同样的事。这里
再来一遍不是重复：那边的客户端和被测的服务端在同一个仓库、同一种语言、共用
同一批类型；这里的客户端只知道"一行一条 JSON-RPC"，连 payload 都是自己拼
的。两个独立实现得出同一个结论，才说明结论属于协议而不属于某段代码。

**这里没有真实 MCP Host。** Claude Code / Codex 的验证是 P11.3 剩下的那半
边，需要真机和订阅额度，status.json 里记着 blocker。
"""

# macOS 自带 Python 3.9：没有这一行，下面的 `dict | None` 注解会在 import 时
# 就抛 TypeError。
from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_client import McpClient, is_error, result_text  # noqa: E402


def ccnm_binary() -> Path | None:
    override = os.environ.get("CCNM_BIN")
    if override:
        return Path(override)
    for profile in ("debug", "release"):
        candidate = ROOT / "target" / profile / "ccnm"
        if candidate.is_file():
            return candidate
    return None


BINARY = ccnm_binary()

# read 模式该有的全部工具，以及三个不该有的。
READ_TOOLS = ["list_files", "read_file", "search_text", "workspace_info"]
WITHHELD = {
    "exec_command": {"cmd": ["/bin/echo", "hi"]},
    "apply_patch": {"files": [{"op": "add", "path": "sneaked.txt", "content": "x\n"}]},
    "read_output": {"output_ref": "r-0000000000000000"},
}


def external_payload(workspace: str, session: str, mode: str) -> str:
    """internal 协议 5 的 payload，由这个测试自己拼。

    真实部署里拼它的是 `ccnm mcp bridge`；这里手拼是为了不经过 ccnm 的任何
    代码，也顺带证明这个形状简单到别的实现照着文档就能写。
    """
    body = json.dumps(
        {"protocol": 5, "workspace": workspace, "session": session, "mode": mode},
        ensure_ascii=False,
    ).encode("utf-8")
    return base64.urlsafe_b64encode(body).decode("ascii").rstrip("=")


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class RemoteWorkspaceMcpTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ccnm-neutral-")
        self.addCleanup(self.temp.cleanup)
        # 解析成真实路径。macOS 的 /var 是指向 /private/var 的符号链接，而
        # 凭据检查对"路径可达性未知"是 fail-closed 的：HOME 藏在符号链接
        # 后面时它报 unknown，服务端于是拒绝启动。那是它该有的谨慎，不是这
        # 里要测的东西。
        self.dir = Path(self.temp.name).resolve()
        self.root = self.dir / "project"
        for sub in ("project", "other", "home", "state"):
            (self.dir / sub).mkdir()
        (self.root / "hello.txt").write_text("one\ntwo\n", encoding="utf-8")
        self.config = self.dir / "config.toml"
        self.write_config("read")

    def write_config(self, access: str) -> None:
        self.config.write_text(
            f"""
this = "runtime"

[nodes.runtime]

[nodes.agent]
ssh = "agent-node.invalid"

[workspaces.demo]
root = "{self.root}"
agent = {{ node = "agent", instance = "claude-main" }}
external_mcp = "{access}"

[workspaces.private]
root = "{self.dir / "other"}"
agent_node = "agent"
""",
            encoding="utf-8",
        )

    def env(self) -> dict:
        return {
            "PATH": os.environ.get("PATH", ""),
            "HOME": str(self.dir / "home"),
            "XDG_STATE_HOME": str(self.dir / "state"),
            "CCNM_CONFIG": str(self.config),
        }

    def argv(self, workspace: str, mode: str, session: str) -> list:
        return [
            str(BINARY),
            "internal",
            "mcp-serve",
            "--payload",
            external_payload(workspace, session, mode),
        ]

    def client(self, workspace: str, mode: str, session: str) -> McpClient:
        client = McpClient(self.argv(workspace, mode, session), self.env())
        self.addCleanup(client.close)
        client.initialize()
        return client

    def refused(self, workspace: str, mode: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            self.argv(workspace, mode, "neutral-refused"),
            env=self.env(),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=False,
        )

    # -- 允许矩阵 --

    def test_a_read_session_offers_exactly_the_read_tools(self):
        client = self.client("demo", "read", "neutral-read")
        self.assertEqual(client.tool_names(), READ_TOOLS)

    def test_the_read_tools_actually_work(self):
        client = self.client("demo", "read", "neutral-works")
        got = client.call_tool("read_file", {"path": "hello.txt"})
        self.assertFalse(is_error(got), got)
        self.assertIn("one", result_text(got))

    def test_a_read_session_refuses_what_it_did_not_offer(self):
        # 一个不看 tools/list 的 Host：直接按名字调。参数都是合法的，否则被
        # 拦下的会是参数检查，那什么都证明不了。
        client = self.client("demo", "read", "neutral-refuse")
        for tool, arguments in WITHHELD.items():
            with self.subTest(tool=tool):
                got = client.call_tool(tool, arguments)
                self.assertTrue(is_error(got), got)
                self.assertTrue(result_text(got).startswith("CCNM_E_POLICY:"), result_text(got))
        self.assertFalse((self.root / "sneaked.txt").exists(), "被拒的写不能真的发生")

    def test_a_coding_session_gets_all_seven(self):
        self.write_config("coding")
        client = self.client("demo", "coding", "neutral-coding")
        self.assertEqual(len(client.tool_names()), 7)
        for tool in WITHHELD:
            self.assertIn(tool, client.tool_names())

    def test_annotations_say_what_the_runtime_enforces(self):
        self.write_config("coding")
        client = self.client("demo", "coding", "neutral-hints")
        hints = {tool["name"]: tool["annotations"] for tool in client.tools()}
        for tool in READ_TOOLS + ["read_output"]:
            self.assertIs(hints[tool]["readOnlyHint"], True, tool)
            self.assertIs(hints[tool]["openWorldHint"], False, tool)
        self.assertIs(hints["apply_patch"]["readOnlyHint"], False)
        self.assertIs(hints["apply_patch"]["destructiveHint"], True)
        # 只能改这个 workspace 里的文件。
        self.assertIs(hints["apply_patch"]["openWorldHint"], False)
        # 任意命令永远算 open-world，不看这次是什么命令。
        self.assertIs(hints["exec_command"]["openWorldHint"], True)
        self.assertIs(hints["exec_command"]["destructiveHint"], True)

    # -- 拒绝 --

    def test_a_workspace_without_the_opt_in_never_hands_out_a_session(self):
        for workspace in ("private", "no-such-workspace"):
            with self.subTest(workspace=workspace):
                out = self.refused(workspace, "read")
                self.assertNotEqual(out.returncode, 0)
                self.assertEqual(out.stdout, "", "不能有半个握手")
                self.assertTrue(out.stderr.startswith("CCNM_E_POLICY:"), out.stderr)

    def test_asking_for_more_than_configured_is_refused(self):
        out = self.refused("demo", "coding")  # 配置是 read
        self.assertNotEqual(out.returncode, 0)
        self.assertEqual(out.stdout, "")
        self.assertIn("read mode", out.stderr)

    def test_an_unknown_protocol_number_stops_the_session(self):
        # 比这个 build 认识的都大：必须停，不能被当成别的形状试着解。
        body = json.dumps({"protocol": 99, "workspace": "demo", "session": "x"}).encode()
        payload = base64.urlsafe_b64encode(body).decode().rstrip("=")
        out = subprocess.run(
            [str(BINARY), "internal", "mcp-serve", "--payload", payload],
            env=self.env(),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertNotEqual(out.returncode, 0)
        self.assertTrue(out.stderr.startswith("CCNM_E_VERSION:"), out.stderr)

    # -- 流本身 --

    def test_stdout_carries_nothing_but_protocol(self):
        client = self.client("demo", "read", "neutral-stdout")
        client.tools()
        client.call_tool("read_file", {"path": "hello.txt"})
        self.assertTrue(client.lines, "总得说过点什么")
        for line in client.lines:
            message = json.loads(line)  # 不是 JSON 就直接抛
            self.assertEqual(message.get("jsonrpc"), "2.0", line)

    def test_the_handshake_does_not_describe_the_machine(self):
        client = self.client("demo", "read", "neutral-leak")
        instructions = client.initialize().get("instructions", "")
        # 说的是 workspace 和模式，不是这台机器在哪。
        self.assertIn("demo", instructions)
        self.assertNotIn(str(self.root), instructions)
        self.assertNotIn(str(self.dir / "home"), instructions)


if __name__ == "__main__":
    unittest.main()
