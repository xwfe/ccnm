"""黑盒契约测试：只通过 `ccnm rpc` 的字节流验证公开协议。

这里**不 import 任何 ccnm 的 Rust 库**，也不看它的内部状态——除了几处刻意
去磁盘上注入故障的地方，那些会写明理由。客户端是
`clients/python/ccnm_machine_client.py`，把那一个文件复制走就能在别的项目里
跑，这正是 P6.1 要证明的事。

没有一个用例会启动真实 Agent 或拨 ssh：每次启动要么在 Runtime 本地预检就
失败，要么根本不到传输那一层。真 provider 的双机闭环是 P6.3，另记。
"""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "clients/python"))

from ccnm_machine_client import (  # noqa: E402
    E_CONFIG,
    E_CONFLICT,
    E_HANDSHAKE_REQUIRED,
    E_INVALID_PARAMS,
    E_NOT_FOUND,
    E_NOT_READY,
    E_UNSUPPORTED_CAPABILITY,
    E_VERSION_MISMATCH,
    MachineClient,
    RpcError,
)


def ccnm_binary() -> Path | None:
    """构建出来的二进制，或者 CCNM_BIN 指定的那个。"""
    override = os.environ.get("CCNM_BIN")
    if override:
        return Path(override)
    for profile in ("debug", "release"):
        candidate = ROOT / "target" / profile / "ccnm"
        if candidate.is_file():
            return candidate
    return None


BINARY = ccnm_binary()

# 一个 Runtime Node 的配置：持有 workspace 和绑定，没有 instance 定义。
# agent 那台指向 .invalid 保留域，`ssh -G` 解析得动但连不上任何东西。
RUNTIME_CONFIG = """
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "worker.invalid"
[workspaces.demo]
root = "{root}"
agent = {{ node = "worker", instance = "claude-main" }}
[workspaces.other]
root = "{other}"
agent = {{ node = "worker", instance = "codex-main" }}
"""

LEGACY_CONFIG = """
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "worker.invalid"
[workspaces.demo]
agent_node = "worker"
root = "{root}"
"""


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class BlackBoxTests(unittest.TestCase):
    """每个用例一套独立的配置和状态目录。"""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ccnm-blackbox-")
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.state = self.home / "state"
        (self.home / "demo").mkdir()
        (self.home / "other").mkdir()
        self.config = self.write_config(
            RUNTIME_CONFIG.format(
                root=self.home / "demo", other=self.home / "other"
            )
        )

    def write_config(self, body: str) -> Path:
        path = self.home / "config.toml"
        path.write_text(body, encoding="utf-8")
        return path

    def fake_peer(self, *args: str) -> MachineClient:
        """把客户端指向一个演坏对端的替身，而不是真的 ccnm。"""
        client = MachineClient(
            argv=[sys.executable, str(ROOT / "tests/fixtures/fake_rpc_peer.py"), *args]
        )
        self.addCleanup(client.close)
        return client

    def client(self, config: Path | None = None, greet: bool = True) -> MachineClient:
        client = MachineClient(
            ccnm=str(BINARY),
            config=str(config or self.config),
            env={
                "HOME": str(self.home),
                "XDG_STATE_HOME": str(self.state),
                "XDG_CONFIG_HOME": str(self.home / "config"),
            },
        )
        self.addCleanup(client.close)
        if greet:
            client.hello("blackbox/1")
        return client

    # -- P6.1：六个方法走一遍 --

    def test_the_whole_chain_works_over_bytes_alone(self):
        client = self.client(greet=False)
        info = client.hello("blackbox/1")
        self.assertEqual(info["protocol"], "ccnm.machine/1")
        self.assertIn("print", info["capabilities"]["modes"])

        agents = client.agents_list()
        self.assertEqual(
            sorted((a["node"], a["instance"]) for a in agents),
            [("worker", "claude-main"), ("worker", "codex-main")],
        )

        started = client.session_start("demo", "go")
        session = started["session"]
        self.assertFalse(started["reused"])
        self.assertEqual(started["agent"]["instance"], "claude-main")

        result = client.wait(session, timeout=60)
        # 到不了 Agent，所以是 failed；关键是它**到了终态**并且能取到结果。
        self.assertEqual(result["state"], "failed")
        self.assertEqual(result["session"], session)

        status = client.session_status(session)
        self.assertEqual(status["state"], "failed")

        stopped = client.session_stop(session)
        self.assertEqual(stopped["state"], "failed")

    def test_the_caller_chooses_the_instance(self):
        # 明确选谁来干活：两个 workspace 绑不同 instance，另外还能在同一个
        # node 上换 instance。
        client = self.client()
        self.assertEqual(
            client.session_start("other", "go")["agent"]["instance"], "codex-main"
        )
        picked = client.session_start(
            "demo", "go", agent={"node": "worker", "instance": "codex-main"}
        )
        self.assertEqual(picked["agent"]["instance"], "codex-main")

    def test_the_client_file_stands_alone(self):
        # P6.1 的字面要求：把那一个文件复制到一个跟仓库无关的目录，照样跑。
        elsewhere = self.home / "someone-elses-project"
        elsewhere.mkdir()
        shutil.copy(ROOT / "clients/python/ccnm_machine_client.py", elsewhere)
        script = elsewhere / "use_it.py"
        script.write_text(
            "from ccnm_machine_client import MachineClient\n"
            "import sys\n"
            "with MachineClient(ccnm=sys.argv[1], config=sys.argv[2]) as c:\n"
            "    print(c.hello('standalone/1')['protocol'])\n",
            encoding="utf-8",
        )
        out = subprocess.run(
            [sys.executable, str(script), str(BINARY), str(self.config)],
            capture_output=True,
            text=True,
            cwd=elsewhere,
            env={**os.environ, "HOME": str(self.home), "XDG_STATE_HOME": str(self.state)},
            check=False,
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.strip(), "ccnm.machine/1")

    # -- P6.2：契约场景 --

    def test_a_repeated_start_key_runs_the_work_once(self):
        client = self.client()
        first = client.session_start("demo", "go", start_key="task-1")
        client.wait(first["session"], timeout=60)
        second = client.session_start("demo", "go", start_key="task-1")
        self.assertEqual(second["session"], first["session"])
        self.assertTrue(second["reused"])
        # 同一个键配不同输入是冲突，不是"再跑一次"。
        with self.assertRaises(RpcError) as caught:
            client.session_start("demo", "something else", start_key="task-1")
        self.assertEqual(caught.exception.code, E_CONFLICT)
        self.assertEqual(caught.exception.session, first["session"])
        self.assertEqual(caught.exception.effect, "none")

    def test_a_start_key_survives_a_server_restart(self):
        first = self.client().session_start("demo", "go", start_key="task-2")
        session = first["session"]
        # 新连接 = 新的服务端进程。记录在磁盘上，所以键还认得。
        again = self.client().session_start("demo", "go", start_key="task-2")
        self.assertEqual(again["session"], session)
        self.assertTrue(again["reused"])

    def test_a_client_that_reconnects_finds_its_session(self):
        session = self.client().session_start("demo", "go")["session"]
        reconnected = self.client()
        self.assertEqual(reconnected.session_status(session)["session"], session)

    def test_a_peer_that_speaks_another_version_is_refused(self):
        # 旧 peer：只会说别的协议标识。客户端必须收到 -32002 并看到对方支持
        # 什么，而不是硬着头皮往下发请求。
        client = self.fake_peer("--protocol", "ccnm.machine/9")
        with self.assertRaises(RpcError) as caught:
            client.hello("blackbox/1")
        self.assertEqual(caught.exception.code, E_VERSION_MISMATCH)
        self.assertEqual(caught.exception.data["supported"], ["ccnm.machine/9"])

    def test_a_lost_start_response_is_not_read_as_success(self):
        # 服务端答应了握手就消失，start 的响应永远不来。客户端必须报连接
        # 断掉，**不能**当成启动成功——那正是重复执行的来源。
        client = self.fake_peer("--die-after", "1")
        client.hello("blackbox/1")
        with self.assertRaises(ConnectionError):
            client.session_start("demo", "go")

    def test_a_peer_that_exits_zero_instead_of_answering_is_not_success(self):
        # 已知的坑：老 ccnm 收到不认识的东西时以退出码 0 结束、什么都没做。
        # 进程"正常"退出不是协议成功。
        client = self.fake_peer("--exit-zero-on-start")
        client.hello("blackbox/1")
        with self.assertRaises(ConnectionError):
            client.session_start("demo", "go")
        self.assertEqual(client.wait_for_exit(), 0, "对端确实是以 0 退出的")

    def test_sessions_do_not_bleed_into_each_other(self):
        client = self.client()
        a = client.session_start("demo", "prompt A")["session"]
        b = client.session_start("other", "prompt B")["session"]
        self.assertNotEqual(a, b)
        client.wait(a, timeout=60)
        client.wait(b, timeout=60)
        self.assertEqual(client.session_result(a)["workspace"], "demo")
        self.assertEqual(client.session_result(b)["workspace"], "other")
        self.assertEqual(
            client.session_result(a)["agent"]["instance"], "claude-main"
        )
        self.assertEqual(
            client.session_result(b)["agent"]["instance"], "codex-main"
        )

    def test_an_unreachable_runtime_ends_as_a_terminal_state(self):
        client = self.client()
        session = client.session_start("demo", "go")["session"]
        result = client.wait(session, timeout=60)
        self.assertIn(result["state"], {"failed", "unknown"})
        self.assertIsNotNone(result.get("outcome") or result.get("state"))

    def test_a_stop_that_is_still_in_flight_never_claims_completion(self):
        # 把记录改回运行中，模拟"停止请求发出、还没确认结束"。这是唯一一处
        # 碰内部状态的地方：客户端没法凭空造出一个正在跑的会话，而"取消中"
        # 是必须验的场景。
        client = self.client()
        session = client.session_start("demo", "go")["session"]
        client.wait(session, timeout=60)
        record_path = self.state / "ccnm/rpc/sessions" / f"{session}.json"
        record = json.loads(record_path.read_text())
        record["state"] = "running"
        record["owner_pid"] = 999999
        record["owner_started"] = "Thu Jan  1 00:00:00 1970"
        record.pop("finish", None)
        record_path.write_text(json.dumps(record))

        # owner 已经不在，所以状态是 unknown 而不是 failed。
        self.assertEqual(client.session_status(session)["state"], "unknown")
        # 终态下的 stop 是幂等的，而且不会宣称"已完成"。
        stopped = client.session_stop(session)
        self.assertEqual(stopped["state"], "unknown")

    def test_output_is_bounded_and_stale_cursors_are_refused(self):
        client = self.client()
        session = client.session_start("demo", "go")["session"]
        client.wait(session, timeout=60)
        result = client.session_result(session)
        output = result.get("output")
        if output is not None:
            self.assertIn("truncated", output)
            self.assertIn("cursor", output)
        # 这个 build 从不发游标，所以任何游标都是失效的。
        with self.assertRaises(RpcError) as caught:
            client.call(
                "session.result",
                {"session": session, "output": {"cursor": "c-8192"}},
            )
        self.assertEqual(caught.exception.code, -32012)

    # -- P6.4：兼容规则 --

    def test_a_client_survives_a_server_from_the_future(self):
        # 兼容规则里客户端那半边的义务：多出来的字段和 capability 一律忽略，
        # 不认识的 state 当成"还没结束"。这两条一破，加字段就成了破坏性变更。
        client = self.fake_peer("--from-the-future")
        info = client.hello("blackbox/1")
        self.assertEqual(info["protocol"], "ccnm.machine/1")
        self.assertIn("session_events", info["capabilities"])
        self.assertIn("negotiated_extensions", info)

        started = client.session_start("demo", "go")
        self.assertEqual(started["session"], "s-future-1")
        self.assertEqual(started["queue_position"], 3)

        status = client.session_status(started["session"])
        self.assertEqual(status["state"], "queued")
        # 不在终态集合里，所以 wait 会继续轮询——超时退出，而不是把一个
        # 没结束的会话当成结束了。
        with self.assertRaises(TimeoutError):
            client.wait(started["session"], timeout=0.3, poll=0.1)

    def test_the_terminal_set_is_exactly_the_three_the_contract_freezes(self):
        # 客户端"不认识就继续等"之所以安全，全靠终态集合冻结在这三个。
        # 哪天要加第四个终态，那是 ccnm.machine/2 的事。
        from ccnm_machine_client import TERMINAL_STATES

        self.assertEqual(TERMINAL_STATES, {"completed", "failed", "unknown"})
        spec = (ROOT / "docs/protocol/machine-protocol-v1.md").read_text(encoding="utf-8")
        self.assertIn("终态集合冻结在 `completed` / `failed` / `unknown` 三个", spec)

    # -- P6.2：拒绝与不泄漏 --

    def test_refusals_use_codes_a_client_can_branch_on(self):
        client = self.client(greet=False)
        with self.assertRaises(RpcError) as caught:
            client.agents_list()
        self.assertEqual(caught.exception.code, E_HANDSHAKE_REQUIRED)

        client.hello("blackbox/1")
        cases = [
            (E_NOT_FOUND, lambda: client.session_start("nosuch", "go")),
            (
                E_NOT_FOUND,
                lambda: client.session_start(
                    "demo", "go", agent={"node": "elsewhere", "instance": "claude-main"}
                ),
            ),
            (E_NOT_FOUND, lambda: client.session_status("s-nope")),
            (
                E_UNSUPPORTED_CAPABILITY,
                lambda: client.call(
                    "session.start",
                    {"workspace": "demo", "mode": "interactive", "input": {}},
                ),
            ),
            (
                E_INVALID_PARAMS,
                lambda: client.call(
                    "session.start",
                    {"workspace": "demo", "mode": "print", "input": {"prompt": "x"},
                     "tiemout_ms": 1},
                ),
            ),
        ]
        for expected, call in cases:
            with self.assertRaises(RpcError) as caught:
                call()
            self.assertEqual(caught.exception.code, expected, caught.exception.message)
            self.assertEqual(caught.exception.effect, "none")

    def test_a_workspace_without_an_instance_binding_says_so(self):
        config = self.write_config(LEGACY_CONFIG.format(root=self.home / "demo"))
        client = self.client(config=config)
        with self.assertRaises(RpcError) as caught:
            client.session_start("demo", "go")
        self.assertEqual(caught.exception.code, E_NOT_READY)

    def test_a_missing_config_is_a_config_error(self):
        client = self.client(config=Path("/nonexistent/ccnm/config.toml"))
        with self.assertRaises(RpcError) as caught:
            client.agents_list()
        self.assertEqual(caught.exception.code, E_CONFIG)

    def test_nothing_private_comes_back_over_the_wire(self):
        """每一条响应都不该带私有路径、凭据名或内部字段。"""
        client = self.client()
        session = client.session_start("demo", "go")["session"]
        client.wait(session, timeout=60)
        seen = [
            client.hello("blackbox/1"),
            {"agents": client.agents_list()},
            client.session_status(session),
            client.session_result(session),
        ]
        blob = json.dumps(seen, ensure_ascii=False)
        for secret in [
            str(Path.home()),
            ".claude",
            ".codex",
            "auth.json",
            "credentials",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "id_rsa",
            "id_ed25519",
            "session_dir",
            "controller",
        ]:
            self.assertNotIn(secret, blob, f"{secret} 漏进了响应")

    def test_error_text_does_not_leak_the_agent_side_path(self):
        client = self.client()
        with self.assertRaises(RpcError) as caught:
            client.session_start("nosuch", "go")
        # 不存在和无权知道给同一句话，否则错误消息本身就是探测工具。
        self.assertEqual(caught.exception.message, "no such workspace or instance")


if __name__ == "__main__":
    unittest.main()
