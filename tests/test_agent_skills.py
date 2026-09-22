"""P48：Agent 机器上装好的 skills，由 `ccnm internal agent-skills` 交给会话。

真实二进制 + 与 ccnm 无关的 MCP 客户端（和 test_remote_workspace_mcp 同一个），
payload 由测试自己拼。Claude Code / Codex 在受管会话里就是这样起它的：同一台机器、
stdin/stdout，不经 ssh。
"""

from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_client import McpClient, RpcError, is_error, result_text  # noqa: E402
from test_remote_workspace_mcp import BINARY  # noqa: E402


def payload(home: Path, session: str, hidden: list | None = None) -> str:
    body = {"protocol": 1, "home": str(home), "session": session}
    if hidden:
        body["hidden"] = hidden
    raw = json.dumps(body).encode("utf-8")
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class AgentSkillsTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="ccnm-agent-skills-")
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()

    def install(self, where: str, name: str, text: str, files: dict | None = None) -> Path:
        skill = self.home / where / name
        skill.mkdir(parents=True, exist_ok=True)
        (skill / "SKILL.md").write_text(text, encoding="utf-8")
        for rel, content in (files or {}).items():
            (skill / rel).parent.mkdir(parents=True, exist_ok=True)
            (skill / rel).write_text(content, encoding="utf-8")
        return skill

    def client(self, hidden: list | None = None) -> McpClient:
        argv = [str(BINARY), "internal", "agent-skills", "--payload",
                payload(self.home, "sess-agent", hidden)]
        # 故意不给 HOME：Codex 只把一部分环境变量交给 MCP server，家目录必须
        # 从 payload 来。
        client = McpClient(argv, {"PATH": os.environ.get("PATH", "")})
        self.addCleanup(client.close)
        client.initialize()
        return client

    def test_one_tool_that_carries_this_machines_catalog(self):
        self.install(".agents/skills", "pdf", "---\ndescription: Fill PDF forms.\n---\nUse ${CLAUDE_SKILL_DIR}/fill.py\n",
                     {"fill.py": "print('fill')\n"})
        # skills CLI 的装法：同一个 skill 经软链再出现一次，只算一个。
        (self.home / ".claude" / "skills").mkdir(parents=True)
        (self.home / ".claude" / "skills" / "pdf").symlink_to(self.home / ".agents" / "skills" / "pdf")
        # Codex 自带的那一包不是用户装的。
        self.install(".codex/skills/.system", "imagegen", "---\ndescription: Images.\n---\nx\n")
        client = self.client()
        tools = client.tools()
        self.assertEqual([t["name"] for t in tools], ["load_skill"])
        text = tools[0]["description"]
        self.assertTrue(text.startswith("Load a skill installed on the machine you run on."), text)
        self.assertTrue(text.endswith("Installed here:\n- pdf: Fill PDF forms."), text)
        self.assertIs(tools[0]["annotations"]["readOnlyHint"], True)

        loaded = result_text(client.call_tool("load_skill", {"name": "pdf"}))
        self.assertIn("not on the project machine", loaded)
        self.assertIn(f"Use {self.home}/.claude/skills/pdf/fill.py", loaded)
        got = client.call_tool("load_skill", {"name": "pdf", "file": "fill.py"})
        self.assertFalse(is_error(got), got)
        self.assertIn("print('fill')", result_text(got))

    def test_hidden_ones_are_not_there_at_all(self):
        self.install(".claude/skills", "noise", "---\ndescription: Noise.\n---\nx\n")
        client = self.client(hidden=["noise"])
        self.assertTrue(client.tools()[0]["description"].endswith("Nothing is installed here right now."))
        got = client.call_tool("load_skill", {"name": "noise"})
        self.assertTrue(is_error(got), got)

    def test_a_skill_for_a_person_is_a_prompt_and_not_for_the_model(self):
        self.install(".claude/skills", "release", "---\ndescription: Cut a release.\n"
                     "disable-model-invocation: true\narguments: [version]\n---\nTag $version.\n")
        client = self.client()
        refused = client.call_tool("load_skill", {"name": "release"})
        self.assertTrue(result_text(refused).startswith("CCNM_E_POLICY:"), result_text(refused))
        prompts = client.call("prompts/list", {})["prompts"]
        self.assertEqual([p["name"] for p in prompts], ["release"])
        got = client.call("prompts/get", {"name": "release", "arguments": {"version": "v1.2"}})
        self.assertIn("Tag v1.2.", got["messages"][0]["content"]["text"])
        with self.assertRaises(RpcError):
            client.call("prompts/get", {"name": "missing", "arguments": {}})


if __name__ == "__main__":
    unittest.main()
