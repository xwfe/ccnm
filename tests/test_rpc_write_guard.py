"""P60：`session.start` 先问 Runtime 的写锁——可证实被占就回 busy，要人来看就回 policy。

只走真实二进制的字节协议，**不 import 任何 ccnm 代码**。整条链上只有 ssh 是假的：

    ccnm rpc ──假 ssh──> 真实 `ccnm internal agent-guard`（Agent 端，读 Agent 配置）
             ──假 ssh──> 真实 `ccnm internal runtime-guard`（“Runtime 执行账号”，另一个 state）

占着锁的是真实的 `ccnm internal mcp-serve`，以外部 MCP coding 会话打开（协议 5，
payload 由本测试自己拼）。执行账号的 state 和 `ccnm rpc` 自己的 state 是两个目录，
和推荐部署一样：答案只可能来自执行账号那边。

这些用例改写自 P57 的 A1/D1/D4：P60 之前前两条是红的（第二个 start 回 starting、
照样派到 Agent），见 docs/research/2026-09-29-p60-write-guard-observation.md。
"""

from __future__ import annotations

import base64
import json
from pathlib import Path
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_client import McpClient  # noqa: E402
from test_rpc_exact_control import BINARY, RpcSandbox, RpcError  # noqa: E402

E_POLICY = -32007
E_BUSY = -32008

CONFIG = """
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "worker-alias"
[workspaces.demo]
root = "{root}"
agent = {{ node = "worker", instance = "claude-main" }}
external_mcp = "coding"
"""

AGENT_CONFIG = Path(__file__).resolve().parent / "fixtures/agent-instance/agent.toml"


def external_payload(session: str) -> str:
    body = json.dumps({"protocol": 5, "workspace": "demo", "session": session, "mode": "coding"})
    return base64.urlsafe_b64encode(body.encode()).decode().rstrip("=")


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class RpcWriteGuardTests(RpcSandbox):
    def setUp(self):
        super().setUp()
        # 执行账号的 HOME 必须是解析过的真实路径：macOS 的 /tmp 是符号链接，
        # 凭据可达性判不清时 mcp-serve 按设计拒绝启动（见 test_remote_workspace_mcp）。
        real = self.dir.resolve()
        (real / "rt-home").mkdir()
        (real / "agent-home").mkdir()
        self.runtime_state = real / "rt"
        self.runtime_env = {
            "PATH": "/usr/bin:/bin",
            "HOME": str(real / "rt-home"),
            "XDG_STATE_HOME": str(self.runtime_state),
            "CCNM_CONFIG": str(self.config),
        }
        (self.fake / "runtime.json").write_text(json.dumps({"ccnm": str(BINARY), "env": self.runtime_env}))
        self.relay = {
            "kind": "relay",
            "ccnm": str(BINARY),
            "agent_config": str(AGENT_CONFIG),
            "agent_env": {
                "PATH": self.env["PATH"],
                "HOME": str(real / "agent-home"),
                "XDG_STATE_HOME": str(self.dir / "a"),
            },
        }
        self.guard_mode(self.relay)

    def bind(self, node: str) -> None:
        self.config.write_text(CONFIG.format(root=self.dir.resolve() / "demo"), encoding="utf-8")

    def guard_mode(self, mode: dict) -> None:
        (self.fake / "guard-mode.json").write_text(json.dumps(mode))

    def writer(self, session: str) -> McpClient:
        """一个外部 MCP coding 会话：在执行账号的 state 里拿到写锁。"""
        client = McpClient(
            [str(BINARY), "internal", "mcp-serve", "--payload", external_payload(session)],
            self.runtime_env,
        )
        self.addCleanup(client.close)
        client.initialize()
        return client

    def refused_start(self, client, prompt: str) -> dict:
        with self.assertRaises(RpcError) as caught:
            client.session_start("demo", prompt)
        return {"code": caught.exception.code, "data": caught.exception.data}

    # -- 用例 --

    def test_a_tree_another_writer_holds_is_busy_and_nothing_starts(self):
        external = self.writer("ext-writer")
        client = self.client()
        refused = self.refused_start(client, "second writer")
        self.assertEqual(refused["code"], E_BUSY, refused)
        self.assertEqual(refused["data"]["effect"], "none")
        self.assertEqual(refused["data"]["reason"], "live_holder")
        self.assertNotIn("session", refused["data"], "no handle was made")
        self.assertEqual(self.calls("agent-run"), [], "nothing reached the Agent")
        # 答案来自执行账号那边：Agent 按自己的配置拨了 runtime-alias，
        # ccnm rpc 自己的 state 里根本没有写锁目录。
        asked = self.calls("runtime-guard")
        self.assertEqual([c["alias"] for c in asked], ["runtime-alias"])
        self.assertEqual(asked[0]["request"], {"protocol": 9, "workspace": "demo", "node": "worker"})
        self.assertFalse((self.state / "write-guards").exists())
        self.assertFalse((self.state / "rpc/sessions").exists() and any((self.state / "rpc/sessions").iterdir()))

        # 那个 writer 正常收尾之后，同样的 start 进得去，而且是一个新任务。
        external.close()
        started = self.start(client, "second writer")
        self.assertEqual(started["state"], "starting")
        self.wait_started("second writer")

    def test_a_guard_left_by_a_killed_writer_is_policy_not_busy(self):
        self.writer("ext-killed").kill()
        refused = self.refused_start(self.client(), "after the crash")
        self.assertEqual(refused["code"], E_POLICY, refused)
        self.assertEqual(refused["data"]["effect"], "none")
        self.assertEqual(refused["data"]["reason"], "left_held")
        self.assertEqual(self.calls("agent-run"), [])
        # 问过之后标记原样：看一眼不是清理。
        markers = list((self.runtime_state / "ccnm/write-guards").iterdir())
        self.assertEqual(len(markers), 1)
        self.assertTrue(markers[0].read_text().startswith("held ext-killed demo pid "))

    def test_the_same_start_key_is_answered_before_the_guard_is_asked(self):
        client = self.client()
        first = self.start(client, "keyed task", start_key="task-1")
        self.wait_started("keyed task")
        self.writer("ext-late")
        again = client.session_start("demo", "keyed task", start_key="task-1")
        self.assertEqual((again["session"], again["reused"]), (first["session"], True))
        self.assertEqual(len(self.calls("agent-guard")), 1, "a taken key never asks")
        # 换个键就是新任务，照样问、照样 busy。
        with self.assertRaises(RpcError) as caught:
            client.session_start("demo", "keyed task", start_key="task-2")
        self.assertEqual(caught.exception.code, E_BUSY)

    def test_no_answer_about_the_guard_starts_as_before(self):
        # Agent 连不上、Agent 太旧、Agent 连得上但它到 Runtime 那一跳不通。
        for mode, runtime in [
            ({"kind": "unreachable"}, None),
            ({"kind": "unknown-command"}, None),
            (self.relay, {"kind": "unreachable"}),
        ]:
            with self.subTest(mode=mode["kind"], runtime=runtime):
                self.guard_mode(mode)
                if runtime is not None:
                    (self.fake / "runtime.json").write_text(json.dumps(runtime))
                prompt = f"no verdict {mode['kind']} {runtime}"
                started = self.start(self.client(), prompt)
                self.assertEqual(started["state"], "starting")
                self.wait_started(prompt)

    def test_status_names_the_holder_as_the_runtime_sees_it(self):
        self.writer("ext-status")
        out = subprocess.run(
            [str(BINARY), "--config", str(self.config), "--lang", "en", "status", "demo"],
            env=self.env,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("guard  held by session ext-stat", out.stdout)
        # 连不上 Runtime 时说连不上，不说空闲。
        (self.fake / "runtime.json").write_text(json.dumps({"kind": "unreachable"}))
        out = subprocess.run(
            [str(BINARY), "--config", str(self.config), "--lang", "en", "status", "demo"],
            env=self.env,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertIn("could not ask the Runtime", out.stdout)
        self.assertIn("does not mean free", out.stdout)
        self.assertNotIn("guard  free", out.stdout)


if __name__ == "__main__":
    unittest.main()
