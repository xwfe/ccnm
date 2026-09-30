"""P65 / F3：会话没起来时，调用方从 `session.result` 就拿得到原因。

P62 真机上见到三种：Agent 上的 CLI 没登录、两台机器的 ccnm 版本不同、版本号相同
但不是同一个构建。三种在调用方那里长得一模一样——`failed`，`exit_code` 和 `text`
是 null，stdout、stderr 各 0 字节；原因只写在 Operator 自己的记录文件里。

被测的是真实的 `ccnm rpc`；对面的 Agent 由 `tests/fixtures/fake_agent_ssh.py` 演，
它照真实 ccnm 拒绝时的样子回话（stderr 第一行是 `CCNM_E_*:`）。这里**不读任何记录
文件**：要证明的正是不用读。
"""

from __future__ import annotations

from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))
sys.path.insert(0, str(ROOT / "clients/python"))

from ccnm_machine_client import (  # noqa: E402
    E_AGENT_UNREACHABLE,
    E_AUTH,
    E_VERSION_MISMATCH,
)
from test_rpc_exact_control import BINARY, RpcSandbox  # noqa: E402


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class FailureReasonTests(RpcSandbox):
    def finish(self, prompt: str, reply: dict | None = None) -> dict:
        """起一个会话、让它结束，返回 `session.result`。"""
        client = self.client()
        if reply is not None:
            self.reply(prompt, reply)
        session = self.start(client, prompt)["session"]
        self.wait_started(prompt)
        self.release(prompt)
        self.settle(client, session)
        return client.session_result(session)

    def test_the_three_reasons_p62_met_are_told_apart(self):
        seen = [
            (
                "run not-logged-in",
                {"ccnm_code": "CCNM_E_AUTH", "exit": 12, "message": "Claude is not authenticated on the Agent Node"},
                E_AUTH,
                "not authenticated",
            ),
            (
                "run other-version",
                {
                    "ccnm_code": "CCNM_E_VERSION",
                    "exit": 11,
                    "message": "the Runtime Node runs ccnm 0.8.0, this one runs 0.9.0; install the same build on both before starting a session",
                },
                E_VERSION_MISMATCH,
                "runs ccnm 0.8.0",
            ),
            (
                "run other-build",
                {
                    "ccnm_code": "CCNM_E_VERSION",
                    "exit": 11,
                    "message": "message is not valid for protocol 1; ccnm versions probably differ",
                },
                E_VERSION_MISMATCH,
                "not valid for protocol",
            ),
        ]
        for prompt, refuse, code, words in seen:
            with self.subTest(prompt=prompt):
                result = self.finish(prompt, {"refuse": refuse})
                self.assertEqual(result["state"], "failed")
                failure = result["failure"]
                self.assertEqual(failure["code"], code)
                self.assertEqual(failure["ccnm_code"], refuse["ccnm_code"])
                self.assertIn(words, failure["detail"])
                # 没有进程跑到退出：这些仍然是 null / 空，和以前一样。
                self.assertIsNone(result["outcome"]["exit_code"])
                self.assertIsNone(result["text"])
                self.assertEqual(result["output"]["bytes_total"], 0)

    def test_a_home_directory_in_the_reason_does_not_say_whose(self):
        # 原因是 Agent 那台机器上的 ccnm 自己的话，里面会有它的 profile、state 目录。
        # 协议第 9 节：任何字段里都不出现私有目录的绝对路径。
        result = self.finish(
            "run private-path",
            {
                "refuse": {
                    "ccnm_code": "CCNM_E_CONFIG",
                    "exit": 10,
                    "message": "profile /Users/agentuser/.config/ccnm/agents/codex must be a private directory",
                }
            },
        )
        detail = result["failure"]["detail"]
        self.assertIn("~/.config/ccnm/agents/codex", detail)
        self.assertNotIn("agentuser", detail)

    def test_a_lost_run_says_why_it_is_unknown_and_stays_unknown(self):
        # 连接断在运行中途：可能已经改了东西。原因是给人排查用的，状态不因此变成可重试。
        result = self.finish("run link-lost", {"transport_error": True})
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(result["failure"]["code"], E_AGENT_UNREACHABLE)
        self.assertIn("closed by remote host", result["failure"]["detail"])

    def test_a_run_that_reached_an_exit_has_no_failure(self):
        # Agent 起来了、自己退出的，不管退出码：看 outcome，没有 failure。
        for prompt, code in (("run ok", 0), ("run exit-3", 3)):
            with self.subTest(prompt=prompt):
                result = self.finish(prompt, {"exit_code": code})
                self.assertNotIn("failure", result)
                self.assertEqual(result["outcome"]["exit_code"], code)

    def test_a_stopped_run_has_no_failure(self):
        client = self.client()
        session = self.start(client, "run stopped")["session"]
        self.wait_started("run stopped")
        client.session_stop(session)
        self.settle(client, session)
        result = client.session_result(session)
        self.assertEqual(result["state"], "failed")
        self.assertTrue(result["outcome"]["stop_requested"])
        self.assertNotIn("failure", result)


if __name__ == "__main__":
    unittest.main()
