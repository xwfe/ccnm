"""P63（F16）：客户端断开之后，已经接受的会话照常派发、跑完，回来查得到。

协议 8.1 节："客户端断开不等于任务停止……已经接受的 session 继续跑……重新
spawn 一个 ccnm rpc，重新 hello，就能凭 session id 继续 status / result / stop。"
P62 在真机上看到的恰恰相反：`session.start` 返回后客户端马上关掉 stdin，`ccnm rpc`
在派发之前就退出，那次运行根本没发出去，状态却读成 `unknown`；派发之后断开也
一样，运行在 Agent 上跑完了，Machine API 这边永远是 `unknown`。原因是运行由
`ccnm rpc` 进程里的一个线程带着，进程一退线程就没了。

和 test_rpc_exact_control.py 一样只走真实二进制的字节协议，Agent 由
tests/fixtures/fake_agent_ssh.py 冒充，时序靠放行文件控制。
"""

from __future__ import annotations

import os
from pathlib import Path
import signal
import sys
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))
sys.path.insert(0, str(ROOT / "clients/python"))

from ccnm_machine_client import MachineClient  # noqa: E402
from test_rpc_exact_control import BINARY, RpcSandbox  # noqa: E402


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class DisconnectTests(RpcSandbox):
    def hang_up(self, client: MachineClient) -> None:
        """像调用方那样结束连接：关 stdin，等服务端退出。"""
        client.close()
        self.assertIsNotNone(client._proc.poll(), "关掉 stdin 之后 ccnm rpc 应该退出")

    def test_hanging_up_right_after_start_still_sends_the_run(self):
        first = self.client()
        session = self.start(first, "run hang-up-early")["session"]
        self.hang_up(first)

        # 派发不再跟着那条连接走：运行照样到了 Agent。
        self.wait_started("run hang-up-early")
        later = self.client()
        self.assertEqual(later.session_status(session)["state"], "running", "有人在带着它跑，不是 unknown")
        self.release("run hang-up-early")
        final = self.settle(later, session)
        self.assertEqual(final["state"], "completed")
        self.assertIn("outcome", later.session_result(session))

    def test_hanging_up_mid_run_keeps_the_real_outcome(self):
        first = self.client()
        session = self.start(first, "run hang-up-mid")["session"]
        self.wait_started("run hang-up-mid")
        self.hang_up(first)

        later = self.client()
        self.assertEqual(later.session_status(session)["state"], "running")
        self.release("run hang-up-mid")
        final = self.settle(later, session)
        self.assertEqual(final["state"], "completed", "Agent 跑完了，结果要回到记录里")
        self.assertEqual(later.session_result(session)["outcome"]["exit_code"], 0)
        self.assertEqual(len(self.calls("agent-run")), 1, "断开不能让它被派发第二次")

    def test_a_stop_after_hanging_up_still_reaches_the_run(self):
        first = self.client()
        session = self.start(first, "run hang-up-stop")["session"]
        self.wait_started("run hang-up-stop")
        self.hang_up(first)

        later = self.client()
        answer = later.session_stop(session)
        self.assertTrue(answer["stop_requested"])
        final = self.settle(later, session)
        self.assertEqual(final["state"], "failed")
        self.assertTrue(final["stop_requested"])

    def test_an_owner_that_really_died_is_still_unknown(self):
        # 带着运行的那个进程真的没了（被 kill -9），就不能再假装知道结果：
        # 保守的 unknown 要留着，只是它不再是"连接断了"的同义词。
        client = self.client()
        session = self.start(client, "run owner-killed")["session"]
        self.wait_started("run owner-killed")
        owner = self.record(session)["owner_pid"]
        self.assertNotEqual(owner, client._proc.pid, "带着运行的应该是它自己的进程，不是这条连接")
        os.kill(owner, signal.SIGKILL)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and client.session_status(session)["state"] != "unknown":
            time.sleep(0.05)
        self.assertEqual(client.session_status(session)["state"], "unknown")
        self.assertIsNone(client._proc.poll(), "这条连接不受影响")


if __name__ == "__main__":
    unittest.main()
