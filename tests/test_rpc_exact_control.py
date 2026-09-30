"""P58：`ccnm rpc` 的会话控制——精确停止、原子状态、幂等键、句柄边界。

只走真实二进制的字节协议，**不 import 任何 ccnm 代码**。对面的 Agent Node 由
tests/fixtures/fake_agent_ssh.py 冒充（PATH 最前面放一个叫 ssh 的包装脚本），
它只演协议约定的样子；时序靠它的“放行文件”控制，不靠 sleep 猜。

这些用例是 P57 探针（docs/research/probes/p57-rpc-control.py）的反证改写成的
正式回归：P58 之前它们全红，见 docs/research/2026-09-28-p58-exact-session-control.md。

沙盒放在 /tmp：macOS 的 $TMPDIR 太长，拼上 `ccnm/ssh` 和 socket 名会超过
ControlPath 的 103 字节上限，ccnm 在派发前就报配置错误，后面什么都测不到。
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "clients/python"))

from ccnm_machine_client import MachineClient, RpcError  # noqa: E402

E_INVALID_PARAMS = -32602
E_UNCERTAIN = -32011
E_CONFLICT = -32010
E_NOT_FOUND = -32009


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
FAKE_AGENT = ROOT / "tests/fixtures/fake_agent_ssh.py"

CONFIG = """
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "worker-alias"
[nodes.worker2]
ssh = "worker2-alias"
[workspaces.demo]
root = "{root}"
agent = {{ node = "{node}", instance = "claude-main" }}
"""


def key_of(prompt: str) -> str:
    return uuid.uuid5(uuid.NAMESPACE_OID, prompt).hex


class RpcSandbox(unittest.TestCase):
    """一套独立的配置、state 和假 Agent。没有用例，只给子类继承（P59 的输出测试也用它）。"""

    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="ccnm-exact-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, self.dir, True)
        # 先放行、再删目录：假 Agent 看到目录没了也会退出，不留孤儿。
        self.addCleanup(self.release_all)
        self.fake = self.dir / "fake"
        (self.fake / "release").mkdir(parents=True)
        (self.dir / "demo").mkdir()
        (self.dir / "home").mkdir()
        self.state = self.dir / "s" / "ccnm"
        self.config = self.dir / "config.toml"
        self.bind("worker")
        bin_dir = self.dir / "bin"
        bin_dir.mkdir()
        shim = bin_dir / "ssh"
        shim.write_text(
            f"#!/bin/sh\nFAKE_AGENT_DIR='{self.fake}' exec '{sys.executable}' -B '{FAKE_AGENT}' \"$@\"\n",
            encoding="utf-8",
        )
        shim.chmod(0o700)
        self.env = {
            "HOME": str(self.dir / "home"),
            "XDG_STATE_HOME": str(self.dir / "s"),
            "XDG_CONFIG_HOME": str(self.dir / "home/.config"),
            "PATH": f"{bin_dir}:/usr/bin:/bin",
            "CCNM_LOG": "error",
        }

    # -- 沙盒 --

    def bind(self, node: str) -> None:
        self.config.write_text(CONFIG.format(root=self.dir / "demo", node=node), encoding="utf-8")

    def client(self) -> MachineClient:
        client = MachineClient(ccnm=str(BINARY), config=str(self.config), env=self.env)
        self.addCleanup(client.close)
        client.hello("exact-control-test/1")
        return client

    def calls(self, sub: str) -> list:
        log = self.fake / "calls.jsonl"
        if not log.exists():
            return []
        rows = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines() if line]
        return [row for row in rows if row["sub"] == sub]

    def run_request(self, prompt: str) -> dict:
        return next(c["request"] for c in self.calls("agent-run") if c["request"]["prompt"] == prompt)

    def wait_started(self, prompt: str) -> dict:
        self.wait(lambda: any(c["request"]["prompt"] == prompt for c in self.calls("agent-run")), "run never reached the Agent")
        return self.run_request(prompt)

    def release(self, prompt: str) -> None:
        (self.fake / "release" / key_of(prompt)).write_text("go\n")

    def release_all(self) -> None:
        if self.fake.exists():
            (self.fake / "release/ALL").write_text("go\n")

    def reply(self, prompt: str, reply: dict) -> None:
        (self.fake / f"reply-{key_of(prompt)}.json").write_text(json.dumps(reply))

    def stop_mode(self, mode: dict) -> None:
        (self.fake / "stop-mode.json").write_text(json.dumps(mode))

    def record(self, session: str) -> dict:
        return json.loads((self.state / "rpc/sessions" / f"{session}.json").read_text())

    def wait(self, predicate, what: str, timeout: float = 15.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.02)
        self.fail(what)

    def settle(self, client: MachineClient, session: str) -> dict:
        found = {}

        def terminal() -> bool:
            found.update(client.session_status(session))
            return found["state"] in ("completed", "failed", "unknown")

        self.wait(terminal, f"{session} never reached a terminal state")
        return found

    def start(self, client: MachineClient, prompt: str, **kw) -> dict:
        return client.session_start("demo", prompt, **kw)


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class ExactControlTests(RpcSandbox):
    # -- CT-01：停 B 不能碰到 A --

    def test_stopping_one_start_never_reaches_another_on_the_same_workspace(self):
        client = self.client()
        a = self.start(client, "run A")["session"]
        run_a = self.wait_started("run A")
        b = self.start(client, "run B")["session"]
        run_b = self.wait_started("run B")
        self.assertNotEqual(run_a.get("session"), run_b.get("session"))

        client.session_stop(b)
        stops = self.calls("agent-stop")
        self.assertEqual(len(stops), 1)
        self.assertEqual(stops[0]["request"].get("session"), run_b.get("session"), "stop 必须点名 B 在 Agent 上的会话")
        self.assertIsNotNone(stops[0]["request"].get("session"))

        final_b = self.settle(client, b)
        self.assertEqual(final_b["state"], "failed")
        self.assertTrue(final_b["stop_requested"])
        # A 没被停：还在等放行，直到这里才结束。
        self.assertEqual(client.session_status(a)["state"], "running")
        self.release("run A")
        final_a = self.settle(client, a)
        self.assertEqual(final_a["state"], "completed")
        self.assertFalse(final_a["stop_requested"])

    # -- CT-03：stop 与结束交错，两个事实都要留下 --

    def test_a_stop_during_the_run_is_kept_when_the_run_ends(self):
        client = self.client()
        session = self.start(client, "run stop-kept")["session"]
        self.wait_started("run stop-kept")
        # 假 Agent 一收到 stop 就放那次运行结束，所以运行可能抢在 stop 回话前落盘，
        # 那时回的是终态：两种都合协议（30 次里约 1 次是后者）。要守的是标志没丢。
        answer = client.session_stop(session)
        self.assertIn(answer["state"], ("stopping", "failed"))
        self.assertTrue(answer["stop_requested"])
        final = self.settle(client, session)
        self.assertEqual(final["state"], "failed")
        self.assertTrue(final["stop_requested"], "stop 标志被运行线程写回了 false")
        self.assertTrue(client.session_result(session)["outcome"]["stop_requested"])

    def test_an_ending_that_lands_before_the_stop_is_not_overwritten(self):
        client = self.client()
        session = self.start(client, "run ends-first")["session"]
        self.wait_started("run ends-first")
        record = self.state / "rpc/sessions" / f"{session}.json"
        self.stop_mode({"kind": "release-and-wait-final", "prompt": "run ends-first", "record": str(record)})
        answer = client.session_stop(session)
        self.assertEqual(answer["state"], "completed", "终态已落盘，stop 不能把它改回 stopping")
        result = client.session_result(session)
        self.assertEqual(result["state"], "completed")
        self.assertIn("outcome", result, "Agent 交回的结果不能丢")
        self.assertTrue(result["outcome"]["stop_requested"])

    def test_two_servers_racing_stop_and_finish_keep_both_facts(self):
        owner, other = self.client(), self.client()
        for n in range(10):
            prompt = f"race {n}"
            session = self.start(owner, prompt)["session"]
            self.wait_started(prompt)
            other.session_stop(session)
            final = self.settle(owner, session)
            with self.subTest(round=n):
                self.assertIn(final["state"], ("completed", "failed"))
                self.assertTrue(final["stop_requested"])
                self.assertIn("finish", self.record(session))
        leftovers = [p.name for p in (self.state / "rpc/sessions").iterdir() if not p.name.endswith(".json")]
        self.assertEqual(leftovers, [], "临时文件不能留在记录目录里")

    # -- F17（P62 真机）：Agent 还确认不了"已停"时，停止请求也不能丢 --

    def test_a_stop_the_agent_cannot_confirm_yet_is_kept_and_answered_stopping(self):
        client = self.client()
        session = self.start(client, "run not-ended")["session"]
        self.wait_started("run not-ended")
        self.stop_mode({"kind": "not-ended"})
        # 契约 5.6：返回 stopping 不代表已经停了。信号已经发出去，这不是"停止失败"。
        answer = client.session_stop(session)
        self.assertEqual(answer["state"], "stopping")
        self.assertTrue(answer["stop_requested"])
        self.assertTrue(self.record(session)["stop_requested"], "停止标志必须在问 Agent 之前就落盘")
        self.release("run not-ended")
        final = self.settle(client, session)
        self.assertEqual(final["state"], "failed")
        self.assertTrue(final["stop_requested"], "Agent 那一刻没确认，标志也不能丢")

    def test_a_stop_that_cannot_reach_the_agent_keeps_the_request_on_record(self):
        client = self.client()
        session = self.start(client, "run stop-unreachable")["session"]
        self.wait_started("run stop-unreachable")
        self.stop_mode({"kind": "unreachable"})
        with self.assertRaises(RpcError) as caught:
            client.session_stop(session)
        # 连 Agent 都没到：这次 stop 什么都没做成，调用方该知道并重发。
        self.assertEqual(caught.exception.effect, "none")
        self.assertTrue(self.record(session)["stop_requested"])
        self.assertEqual(client.session_status(session)["state"], "running", "没送到的停止不能把状态说成 stopping")
        self.release("run stop-unreachable")
        final = self.settle(client, session)
        self.assertEqual(final["state"], "completed")
        self.assertTrue(final["stop_requested"])

    # -- CT-04：不同的 start_key 就是不同的任务 --

    def test_distinct_start_keys_stay_distinct(self):
        client = self.client()
        for first, second in (("任务-一", "任务-二"), ("a/b", "ab"), ("k" * 64 + "x", "k" * 64 + "y")):
            with self.subTest(first=first, second=second):
                s1 = self.start(client, f"same {first}", start_key=first)
                s2 = self.start(client, f"other {first}", start_key=second)
                self.assertFalse(s2["reused"])
                self.assertNotEqual(s1["session"], s2["session"])
                s3 = self.start(client, f"other {first}", start_key=second)
                self.assertTrue(s3["reused"], "同键同输入复用自己那一个")
                self.assertEqual(s3["session"], s2["session"])
                again = self.start(client, f"same {first}", start_key=first)
                self.assertEqual(again["session"], s1["session"])
        self.wait(lambda: len(self.calls("agent-run")) >= 6, "每组两个键各起一次")
        time.sleep(0.3)
        self.assertEqual(len(self.calls("agent-run")), 6, "复用不能多起 Agent")

    # -- CT-05：旧布局的键、写了一半的键 --

    def plant(self, session: str, start_key: str | None, prompt: str, state: str = "completed") -> None:
        sessions = self.state / "rpc/sessions"
        sessions.mkdir(parents=True, exist_ok=True)
        record = {
            "session": session, "workspace": "demo",
            "agent": {"node": "worker", "instance": "claude-main"},
            "mode": "print", "prompt": prompt, "state": state,
            "accepted_at": "2026-09-01T00:00:00Z", "stop_requested": False,
            "owner_pid": 1, "owner_started": "x",
            "finish": {"exit_code": 0, "timed_out": False, "duration_ms": 1, "text": "old", "output": "", "output_total": 0},
        }
        if start_key is not None:
            record["start_key"] = start_key
        (sessions / f"{session}.json").write_text(json.dumps(record))

    def legacy_key(self, name: str, content: str) -> None:
        keys = self.state / "rpc/keys/demo"
        keys.mkdir(parents=True, exist_ok=True)
        (keys / name).write_text(content)

    def test_keys_written_before_the_upgrade_still_prevent_a_second_run(self):
        client = self.client()
        self.plant("s-legacy-0001", "task-legacy", "legacy work")
        self.legacy_key("task-legacy", "s-legacy-0001")
        reused = self.start(client, "legacy work", start_key="task-legacy")
        self.assertTrue(reused["reused"])
        self.assertEqual(reused["session"], "s-legacy-0001")
        with self.assertRaises(RpcError) as caught:
            self.start(client, "different work", start_key="task-legacy")
        self.assertEqual(caught.exception.code, E_CONFLICT)
        # 旧布局把 `任务-一` 存成文件 `-`；换一个键撞上同一个文件名不算复用。
        self.plant("s-legacy-0002", "任务-一", "cjk work")
        self.legacy_key("-", "s-legacy-0002")
        fresh = self.start(client, "cjk work", start_key="任务-二")
        self.assertFalse(fresh["reused"])
        self.wait(lambda: self.calls("agent-run"), "新键要起一次")
        time.sleep(0.3)
        self.assertEqual([c["request"]["prompt"] for c in self.calls("agent-run")], ["cjk work"])

    def test_a_half_written_legacy_key_is_uncertain_without_a_fake_handle(self):
        client = self.client()
        self.legacy_key("k-empty", "")
        with self.assertRaises(RpcError) as caught:
            self.start(client, "empty key", start_key="k-empty")
        self.assertEqual(caught.exception.code, E_UNCERTAIN)
        self.assertEqual(caught.exception.effect, "unknown")
        self.assertNotEqual(caught.exception.data.get("session"), "", "空串不是一个能查的 session")
        self.legacy_key("k-dangling", "s-00000000-0000-0000-0000-000000000000")
        with self.assertRaises(RpcError) as caught:
            self.start(client, "dangling key", start_key="k-dangling")
        self.assertEqual(caught.exception.code, E_UNCERTAIN)
        self.assertEqual(caught.exception.session, "s-00000000-0000-0000-0000-000000000000")
        time.sleep(0.3)
        self.assertEqual(self.calls("agent-run"), [], "不确定就不能再起一次")

    # -- CT-06：句柄只能是服务端发出的形状 --

    def test_handles_outside_the_issued_shape_are_refused_before_any_file_is_read(self):
        client = self.client()
        self.plant("p57-outside", None, "planted")
        (self.state / "rpc/sessions/p57-outside.json").rename(self.state / "rpc/p57-outside.json")
        absolute = self.dir / "planted"
        self.plant("planted", None, "planted")
        shutil.copy(self.state / "rpc/sessions/planted.json", absolute.with_suffix(".json"))
        (self.state / "rpc/sessions/s-link.json").symlink_to(absolute.with_suffix(".json"))
        for handle in ("../p57-outside", str(absolute), "a/b", "", "x" * 129, "-leading-dash"):
            for method in ("session.status", "session.result", "session.stop"):
                with self.subTest(handle=handle, method=method):
                    with self.assertRaises(RpcError) as caught:
                        client.call(method, {"session": handle})
                    self.assertEqual(caught.exception.code, E_INVALID_PARAMS)
        with self.assertRaises(RpcError) as caught:
            client.session_result("s-link")
        self.assertNotIn(str(self.dir), caught.exception.message, "错误里不能有私有路径")
        with self.assertRaises(RpcError) as caught:
            client.session_status("s-nope")
        self.assertEqual(caught.exception.code, E_NOT_FOUND)

    # -- CT-07：旧句柄不随配置改指新机器 --

    def test_an_old_handle_does_not_follow_a_rebound_workspace(self):
        client = self.client()
        session = self.start(client, "run rebound")["session"]
        self.wait_started("run rebound")
        self.bind("worker2")
        with self.assertRaises(RpcError) as caught:
            client.session_stop(session)
        self.assertEqual(caught.exception.effect, "none")
        self.assertEqual([c for c in self.calls("agent-stop") if c["alias"] == "worker2-alias"], [])
        self.assertFalse(client.session_status(session)["stop_requested"], "没送出去的 stop 不算请求过")
        self.bind("worker")
        client.session_stop(session)
        self.assertEqual(self.calls("agent-stop")[-1]["alias"], "worker-alias")

    # -- CT-08：派发之后连接断了，是 unknown 不是 failed --

    def test_a_connection_lost_after_dispatch_is_unknown_not_failed(self):
        client = self.client()
        self.reply("run lost", {"transport_error": True})
        self.release("run lost")
        session = self.start(client, "run lost")["session"]
        final = self.settle(client, session)
        self.assertEqual(final["state"], "unknown", "Agent 可能已经在跑，failed 会诱导重试")


if __name__ == "__main__":
    unittest.main()
