"""P8.2 的接口测试：最小 ExecutionBackend、ccnm adapter 和假后端。

两组用例，目的不同：

- `FakeBackendTests` **不需要 ccnm**，证明协调逻辑可以在没有执行层的情况下测。
- `CcnmBackendTests` 只通过 `ccnm rpc` 的字节流验证 adapter，不 import 任何
  ccnm 的库，也不启动真实 Agent、不拨 ssh——每次启动都在 Runtime 本地预检就失败。

真机闭环是 P7.3 的事，那里另有记录，这里一条都不重复。
"""

# macOS 自带 Python 3.9，没有这一行下面 `Path | None` 这类注解会在 import 时
# 就抛 TypeError，整个文件一个用例都跑不了。
from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "clients/python"))

from execution_backend import (  # noqa: E402
    KIND_CONFLICT,
    KIND_NOT_FOUND,
    KIND_UNAVAILABLE,
    KIND_UNCERTAIN,
    TERMINAL_STATES,
    BackendError,
    CcnmBackend,
    ExecutionRequest,
    FakeBackend,
)


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

# 和黑盒测试同一套：agent 那台指向 .invalid 保留域，`ssh -G` 解析得动但连不上。
RUNTIME_CONFIG = """
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "worker.invalid"
[workspaces.demo]
root = "{root}"
agent = {{ node = "worker", instance = "claude-main" }}
"""


def delegate(backend, store: dict, task: str, attempt: int, prompt: str) -> str:
    """一段最小的协调逻辑，两个后端上跑的是同一份代码。

    它演示的是**顺序**：先把 start_key 和执行 id 记进自己的存储，再去等结果。
    反过来（先启动再记）中间崩一次，那次执行就找不回来了——执行层没有"列出我的
    session"，只认 id 和 start_key。
    """
    key = f"{task}-attempt-{attempt}"
    known = store.get(key)
    if known is not None:
        # 崩溃重来：不重新启动，拿着记下的 id 接着等。
        return known
    started = backend.start(
        ExecutionRequest(workspace="demo", prompt=prompt, start_key=key)
    )
    store[key] = started.id
    return started.id


class FakeBackendTests(unittest.TestCase):
    """协调逻辑在没有 ccnm、没有 Agent、没有订阅额度时也能测。"""

    def test_the_same_start_key_runs_the_work_once(self):
        backend = FakeBackend()
        first = backend.start(
            ExecutionRequest(workspace="demo", prompt="go", start_key="t-1")
        )
        again = backend.start(
            ExecutionRequest(workspace="demo", prompt="go", start_key="t-1")
        )
        self.assertEqual(again.id, first.id)
        self.assertTrue(again.reused)
        self.assertEqual(backend.executions, [first.id], "只该真启动一次")

    def test_the_same_key_with_different_input_is_a_conflict(self):
        backend = FakeBackend()
        first = backend.start(
            ExecutionRequest(workspace="demo", prompt="go", start_key="t-1")
        )
        with self.assertRaises(BackendError) as caught:
            backend.start(
                ExecutionRequest(workspace="demo", prompt="别的活", start_key="t-1")
            )
        self.assertEqual(caught.exception.kind, KIND_CONFLICT)
        self.assertEqual(caught.exception.execution, first.id)
        # 什么都没发生，所以原样重发是安全的（虽然重发还会冲突）。
        self.assertTrue(caught.exception.safe_to_resend)
        self.assertEqual(backend.executions, [first.id])

    def test_a_start_key_is_scoped_to_one_workspace(self):
        backend = FakeBackend()
        here = backend.start(
            ExecutionRequest(workspace="demo", prompt="go", start_key="t-1")
        )
        there = backend.start(
            ExecutionRequest(workspace="other", prompt="go", start_key="t-1")
        )
        self.assertNotEqual(here.id, there.id)

    def test_a_stopped_execution_ends_as_failed(self):
        backend = FakeBackend()
        started = backend.start(ExecutionRequest(workspace="demo", prompt="go"))
        stopping = backend.stop(started.id)
        # 收到了停止请求 ≠ 已经停了。
        self.assertEqual(stopping.state, "stopping")
        self.assertFalse(stopping.terminal)
        # 幂等：再调一次不报错。
        self.assertEqual(backend.stop(started.id).state, "stopping")
        result = backend.wait(started.id, timeout=1, poll=0)
        self.assertEqual(result.state, "failed")
        self.assertTrue(result.stop_requested)
        # 终态之后再停还是幂等，而且不会把状态改回去。
        self.assertEqual(backend.stop(started.id).state, "failed")

    def test_an_unknown_state_is_not_treated_as_finished(self):
        # 兼容规则里调用方那半边的义务：没见过的状态当成"还没结束"。破了这条，
        # 执行层将来加一个非终态状态就会让协调层以为活干完了。
        backend = FakeBackend()
        started = backend.start(ExecutionRequest(workspace="demo", prompt="go"))
        backend.force_state(started.id, "queued")
        self.assertFalse(backend.status(started.id).terminal)
        with self.assertRaises(TimeoutError):
            backend.wait(started.id, timeout=0.05, poll=0.01)

    def test_a_crash_window_is_uncertain_and_does_not_restart(self):
        backend = FakeBackend()
        lost = backend.crash_window("demo", "t-9")
        with self.assertRaises(BackendError) as caught:
            backend.start(
                ExecutionRequest(workspace="demo", prompt="go", start_key="t-9")
            )
        self.assertEqual(caught.exception.kind, KIND_UNCERTAIN)
        self.assertEqual(caught.exception.effect, "unknown")
        self.assertEqual(caught.exception.execution, lost)
        self.assertFalse(caught.exception.safe_to_resend)
        self.assertEqual(backend.executions, [], "不确定的执行绝不能自动重启")

    def test_completed_does_not_mean_the_work_is_right(self):
        # 接口只回答"进程怎么结束的"。活干没干对由协调层自己判断，这里用一个
        # 假的验收函数表示那一步确实是另一件事。
        backend = FakeBackend()
        backend.on("go", state="completed", exit_code=0, text="改完了")
        started = backend.start(ExecutionRequest(workspace="demo", prompt="go"))
        result = backend.wait(started.id, timeout=1, poll=0)
        self.assertEqual(result.state, "completed")
        self.assertEqual(result.exit_code, 0)
        accepted = "测试通过" in (result.text or "")
        self.assertFalse(accepted, "进程成功不等于业务验收通过")

    def test_a_failed_execution_reports_its_exit_code(self):
        backend = FakeBackend()
        backend.on("go", state="failed", exit_code=2, text="编译不过")
        started = backend.start(ExecutionRequest(workspace="demo", prompt="go"))
        result = backend.wait(started.id, timeout=1, poll=0)
        self.assertEqual((result.state, result.exit_code), ("failed", 2))
        self.assertTrue(result.terminal)

    def test_an_unknown_id_is_not_found(self):
        with self.assertRaises(BackendError) as caught:
            FakeBackend().status("x-404")
        self.assertEqual(caught.exception.kind, KIND_NOT_FOUND)

    def test_coordination_logic_needs_no_execution_layer(self):
        # P8.2 的字面要求：fake backend 可独立测协调逻辑。这里的"协调逻辑"是
        # delegate()：先记录、后等待、重来时不重启。
        backend = FakeBackend()
        store: dict = {}
        first = delegate(backend, store, "task-4821", 1, "go")
        # 协调层崩了一次，存储还在：同一个 attempt 不会再起一个 Agent。
        again = delegate(backend, store, "task-4821", 1, "go")
        self.assertEqual(again, first)
        self.assertEqual(backend.executions, [first])
        # 重试是**新的 attempt**，也就是新的 key，这才会真的再跑一次。
        retry = delegate(backend, store, "task-4821", 2, "go")
        self.assertNotEqual(retry, first)
        self.assertEqual(len(backend.executions), 2)


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class CcnmBackendTests(unittest.TestCase):
    """adapter 只通过公开协议的字节流跟 ccnm 说话。"""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ccnm-backend-")
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        (self.home / "demo").mkdir()
        self.config = self.home / "config.toml"
        self.config.write_text(
            RUNTIME_CONFIG.format(root=self.home / "demo"), encoding="utf-8"
        )
        self.env = {
            "HOME": str(self.home),
            "XDG_STATE_HOME": str(self.home / "state"),
            "XDG_CONFIG_HOME": str(self.home / "config"),
        }

    def backend(self) -> CcnmBackend:
        backend = CcnmBackend.spawn(
            ccnm=str(BINARY), config=str(self.config), env=self.env
        )
        self.addCleanup(backend.close)
        return backend

    def fake_peer_backend(self, *args: str) -> CcnmBackend:
        """指向一个演坏对端的替身，每次重连都起一个新的。"""
        backend = CcnmBackend.spawn(
            argv=[sys.executable, str(ROOT / "tests/fixtures/fake_rpc_peer.py"), *args]
        )
        self.addCleanup(backend.close)
        return backend

    def test_the_adapter_covers_the_whole_chain(self):
        backend = self.backend()
        self.assertEqual([a.id for a in backend.agents()], ["worker/claude-main"])
        started = backend.start(
            ExecutionRequest(workspace="demo", prompt="go", agent="worker/claude-main")
        )
        self.assertFalse(started.reused)
        result = backend.wait(started.id, timeout=60)
        # 到不了 Agent，所以是 failed。要证明的是它**到了终态**、结果取得回来。
        self.assertEqual(result.state, "failed")
        self.assertEqual(result.id, started.id)
        self.assertTrue(result.terminal)
        self.assertEqual(backend.status(started.id).state, "failed")
        # 终态下 stop 幂等，且不宣称完成。
        self.assertEqual(backend.stop(started.id).state, "failed")

    def test_an_agent_id_that_is_not_node_slash_instance_is_refused_locally(self):
        with self.assertRaises(BackendError) as caught:
            self.backend().start(
                ExecutionRequest(workspace="demo", prompt="go", agent="claude-main")
            )
        self.assertEqual(caught.exception.kind, "rejected")

    def test_an_unknown_session_is_not_found(self):
        with self.assertRaises(BackendError) as caught:
            self.backend().status("s-does-not-exist")
        self.assertEqual(caught.exception.kind, KIND_NOT_FOUND)
        self.assertEqual(caught.exception.effect, "none")
        self.assertEqual(caught.exception.code, -32009)

    def test_a_conflicting_start_key_carries_the_first_execution(self):
        backend = self.backend()
        first = backend.start(
            ExecutionRequest(workspace="demo", prompt="go", start_key="t-1")
        )
        again = backend.start(
            ExecutionRequest(workspace="demo", prompt="go", start_key="t-1")
        )
        self.assertTrue(again.reused)
        self.assertEqual(again.id, first.id)
        with self.assertRaises(BackendError) as caught:
            backend.start(
                ExecutionRequest(workspace="demo", prompt="别的活", start_key="t-1")
            )
        self.assertEqual(caught.exception.kind, KIND_CONFLICT)
        self.assertEqual(caught.exception.execution, first.id)
        self.assertTrue(caught.exception.safe_to_resend)

    def test_a_missing_config_is_a_plain_rejection(self):
        backend = CcnmBackend.spawn(
            ccnm=str(BINARY), config=str(self.home / "nope.toml"), env=self.env
        )
        self.addCleanup(backend.close)
        with self.assertRaises(BackendError) as caught:
            backend.agents()
        self.assertEqual(caught.exception.kind, "rejected")
        self.assertEqual(caught.exception.effect, "none")

    def test_a_peer_that_exits_zero_is_never_read_as_started(self):
        # 已知的坑：老 peer 收到不认识的东西时以退出码 0 结束、什么都没做。
        # 两种启动的下场必须不同——这正是 start_key 的作用。
        backend = self.fake_peer_backend("--exit-zero-on-start")
        with self.assertRaises(BackendError) as caught:
            backend.start(ExecutionRequest(workspace="demo", prompt="go"))
        self.assertEqual(caught.exception.kind, KIND_UNAVAILABLE)
        self.assertEqual(caught.exception.effect, "unknown", "没有键，重发会重复执行")

        with self.assertRaises(BackendError) as keyed:
            backend.start(
                ExecutionRequest(workspace="demo", prompt="go", start_key="t-1")
            )
        self.assertEqual(keyed.exception.kind, KIND_UNCERTAIN)
        self.assertFalse(keyed.exception.safe_to_resend)

    def test_a_state_from_the_future_is_not_terminal(self):
        backend = self.fake_peer_backend("--from-the-future")
        started = backend.start(ExecutionRequest(workspace="demo", prompt="go"))
        self.assertEqual(started.id, "s-future-1")
        self.assertFalse(backend.status(started.id).terminal, "queued 不是终态")
        with self.assertRaises(TimeoutError):
            backend.wait(started.id, timeout=0.05, poll=0.01)

    def test_a_dropped_connection_is_reconnected_for_read_only_calls(self):
        # 断连不会停掉已经接受的执行，凭 id 还能查。这里第一条连接答完握手就
        # 死掉，adapter 必须自己换一条再问一次，而不是把断线当成执行失败。
        session = self.backend().start(
            ExecutionRequest(workspace="demo", prompt="go")
        ).id

        from ccnm_machine_client import MachineClient  # noqa: E402

        clients = [
            lambda: MachineClient(
                argv=[
                    sys.executable,
                    str(ROOT / "tests/fixtures/fake_rpc_peer.py"),
                    "--die-after",
                    "1",
                ]
            ),
            lambda: MachineClient(
                ccnm=str(BINARY), config=str(self.config), env=self.env
            ),
        ]

        def connect():
            return clients.pop(0)()

        backend = CcnmBackend(connect)
        self.addCleanup(backend.close)
        self.assertEqual(backend.status(session).id, session)
        self.assertEqual(clients, [], "两条连接都用上了，说明确实重连过")

    def test_both_files_stand_alone(self):
        # 抄走两个文件、放进一个跟仓库无关的目录，照样能驱动 ccnm。这是
        # "adapter 只依赖公共协议"的字面检查。
        elsewhere = self.home / "someone-elses-orchestrator"
        elsewhere.mkdir()
        for name in ("ccnm_machine_client.py", "execution_backend.py"):
            shutil.copy(ROOT / "clients/python" / name, elsewhere)
        script = elsewhere / "use_it.py"
        script.write_text(
            "import sys\n"
            "from execution_backend import CcnmBackend\n"
            "with CcnmBackend.spawn(ccnm=sys.argv[1], config=sys.argv[2]) as b:\n"
            "    print([a.id for a in b.agents()])\n",
            encoding="utf-8",
        )
        out = subprocess.run(
            [sys.executable, str(script), str(BINARY), str(self.config)],
            capture_output=True,
            text=True,
            cwd=elsewhere,
            env={**os.environ, **self.env},
            check=False,
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.strip(), "['worker/claude-main']")


class ContractDriftTests(unittest.TestCase):
    def test_the_terminal_states_match_the_client(self):
        # 两个文件各自声明了一份终态集合（它们都必须能单独被抄走）。协议第 13
        # 节冻结了这三个，所以两份必须一样；这条测试就是防它们悄悄分家。
        from ccnm_machine_client import TERMINAL_STATES as CLIENT_STATES

        self.assertEqual(TERMINAL_STATES, CLIENT_STATES)


if __name__ == "__main__":
    unittest.main()
