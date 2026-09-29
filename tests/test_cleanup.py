"""P61：清理先预览、各自的东西由各自的账号删，删不干净就不忘掉 workspace。

只走真实二进制，**不 import 任何 ccnm 代码**。三个 state 目录模拟三个账号：

    Operator  self.state         `ccnm rpc`、`ccnm cleanup`、`workspace remove` 在这里跑
    Agent     self.agent_state   真实 `ccnm internal agent-cleanup`（经假 ssh 转过去）
    Runtime   self.runtime_state 真实 `ccnm internal runtime-cleanup`（Agent 再经假 ssh 拨过去）

**这是分目录的路由测试，不是跨 UID 的权限证明**：三个目录同属当前用户，
OS 隔离留给 P62 在 hpsrv/ccrun 上验。

改写自 P57 的 E（docs/research/probes/p57-purge-routing.py）：P61 之前 purge 只删
Operator 自己 state 下的 sessions/<id>，Runtime 执行账号那份输出没人删，workspace
配置却照删，之后再没有命令找得到它。见 docs/research/2026-09-30-p61-cleanup.md。
"""

from __future__ import annotations

import base64
import json
from pathlib import Path
import re
import subprocess
import sys
import unittest
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_client import McpClient  # noqa: E402
from test_rpc_exact_control import BINARY, RpcError, RpcSandbox  # noqa: E402

E_EXPIRED = -32012
EXIT_NOT_READY = 3
EXIT_VERSION = 11
EXIT_INVALID_ARGS = 34

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


def b64(value: dict) -> str:
    return base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip("=")


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class CleanupTests(RpcSandbox):
    def setUp(self):
        super().setUp()
        # 执行账号的 HOME 要是解析过的真实路径（macOS 的 /tmp 是符号链接）。
        real = self.dir.resolve()
        (real / "rt-home").mkdir()
        (real / "agent-home").mkdir()
        self.runtime_state = real / "rt" / "ccnm"
        self.agent_state = self.dir / "a" / "ccnm"
        self.runtime_env = {
            "PATH": "/usr/bin:/bin",
            "HOME": str(real / "rt-home"),
            "XDG_STATE_HOME": str(real / "rt"),
            "CCNM_CONFIG": str(self.config),
        }
        self.agent_env = {
            "PATH": self.env["PATH"],
            "HOME": str(real / "agent-home"),
            "XDG_STATE_HOME": str(self.dir / "a"),
        }
        self.runtime_reachable(True)
        (self.fake / "cleanup-mode.json").write_text(
            json.dumps(
                {
                    "kind": "relay",
                    "ccnm": str(BINARY),
                    "agent_config": str(AGENT_CONFIG),
                    "agent_env": self.agent_env,
                }
            )
        )

    def bind(self, node: str) -> None:
        self.config.write_text(CONFIG.format(root=self.dir.resolve() / "demo"), encoding="utf-8")

    # -- 沙盒 --

    def runtime_reachable(self, yes: bool) -> None:
        mode = {"ccnm": str(BINARY), "env": self.runtime_env} if yes else {"kind": "unreachable"}
        (self.fake / "runtime.json").write_text(json.dumps(mode))

    def ccnm(self, *args: str, env: dict | None = None) -> subprocess.CompletedProcess:
        return subprocess.run(
            [str(BINARY), "--config", str(self.config), "--lang", "en", *args],
            env=env or self.env,
            capture_output=True,
            text=True,
            check=False,
            timeout=120,
        )

    def agent_session(self, sid: str, workspace: str = "demo", ended: bool = True) -> Path:
        d = self.agent_state / "sessions" / sid
        d.mkdir(parents=True)
        spec = {
            "protocol": 1,
            "id": sid,
            "workspace": workspace,
            "root": "/srv/demo",
            "runtime": None,
            "claude_config_dir": None,
            "permission_mode": "acceptEdits",
            "mode": {"mode": "print", "prompt": "go"},
            "timeout_secs": 900,
            "cwd": "/tmp/nowhere",
        }
        (d / "session.json").write_text(json.dumps(spec))
        (d / "stdout").write_text("done\n")
        if ended:
            self.end(sid)
        return d

    def end(self, sid: str) -> None:
        (self.agent_state / "sessions" / sid / "exit").write_text(
            json.dumps({"exit_code": 0, "timed_out": False, "duration_ms": 5, "error": None})
        )

    def runtime_output(self, sid: str, state: Path | None = None) -> Path:
        run = (state or self.runtime_state) / "sessions" / sid / "output" / "r-0000000000000001"
        run.mkdir(parents=True)
        (run / "stdout").write_bytes(b"x" * 48 * 1024)
        return run.parent

    def finished_rpc_session(self, prompt: str, key: str) -> tuple:
        """一次真正经过 `ccnm rpc` 的 print 会话，读过一次结果（Operator 有了拷贝）。"""
        client = self.client()
        handle = self.start(client, prompt, start_key=key)["session"]
        managed = self.wait_started(prompt)["session"]
        self.release(prompt)
        self.settle(client, handle)
        client.session_result(handle)
        self.assertTrue((self.state / "rpc/outputs" / handle).is_dir())
        return client, handle, managed

    def token(self, preview: subprocess.CompletedProcess) -> str:
        found = re.search(r"--apply (\S+)", preview.stdout)
        self.assertIsNotNone(found, preview.stdout + preview.stderr)
        return found.group(1)

    def record(self, handle: str) -> dict:
        return json.loads((self.state / "rpc/sessions" / f"{handle}.json").read_text())

    # -- CL-01 / CL-07：谁的东西谁删，删不干净不忘掉 workspace --

    def test_purge_takes_each_half_from_its_owner_and_keeps_the_config_until_done(self):
        _, handle, m1 = self.finished_rpc_session("task one", "k1")
        self.agent_session(m1)
        m1_output = self.runtime_output(m1)
        running = str(uuid.uuid4())
        self.agent_session(running, ended=False)
        running_output = self.runtime_output(running)
        other = str(uuid.uuid4())
        self.agent_session(other, workspace="other")
        other_output = self.runtime_output(other)
        # Operator 自己 state 里同名的目录：不是执行账号的东西，按 id 跨域删是 P57 E 的错。
        decoy = self.runtime_output(m1, state=self.state)

        out = self.ccnm("workspace", "remove", "demo", "--purge")
        self.assertEqual(out.returncode, EXIT_NOT_READY, out.stdout + out.stderr)
        self.assertIn("[workspaces.demo]", self.config.read_text(), "config kept while something is left")
        self.assertFalse(m1_output.exists(), "the Executor's output is removed by the Executor")
        self.assertFalse((self.agent_state / "sessions" / m1).exists())
        self.assertFalse((self.state / "rpc/outputs" / handle).exists())
        self.assertIn("cleaned_at", self.record(handle))
        self.assertTrue(running_output.exists() and (self.agent_state / "sessions" / running).exists())
        self.assertTrue(other_output.exists() and (self.agent_state / "sessions" / other).exists())
        self.assertTrue(decoy.exists(), "the Operator's own state is not where the Executor keeps output")

        self.end(running)
        out = self.ccnm("workspace", "remove", "demo", "--purge")
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertNotIn("[workspaces.demo]", self.config.read_text())
        self.assertFalse(running_output.exists())
        self.assertFalse((self.agent_state / "sessions" / running).exists())
        self.assertTrue(other_output.exists(), "another workspace is never touched")

    # -- CL-02 / CL-03 / CL-06：只预览；令牌不对、过期、清单变了都不删；写锁和墓碑 --

    def test_cleanup_previews_and_applies_nothing_that_changed(self):
        client, handle, m1 = self.finished_rpc_session("task two", "k2")
        self.agent_session(m1)
        m1_output = self.runtime_output(m1)
        # 写锁标着的会话：一个以它的 id 打开、然后被强杀的外部 coding 会话。
        guarded = str(uuid.uuid4())
        self.agent_session(guarded)
        guarded_output = self.runtime_output(guarded)
        writer = McpClient(
            [
                str(BINARY),
                "internal",
                "mcp-serve",
                "--payload",
                b64({"protocol": 5, "workspace": "demo", "session": guarded, "mode": "coding"}),
            ],
            self.runtime_env,
        )
        self.addCleanup(writer.close)
        writer.initialize()
        writer.kill()
        markers = list((self.runtime_state / "write-guards").iterdir())
        marker_before = markers[0].read_bytes()

        preview = self.ccnm("cleanup", "demo")
        self.assertEqual(preview.returncode, 0, preview.stdout + preview.stderr)
        self.assertIn("nothing has been removed", preview.stdout)
        self.assertIn("the write guard names it", preview.stdout)
        token = self.token(preview)
        self.assertTrue(m1_output.exists() and (self.state / "rpc/outputs" / handle).exists())

        self.assertEqual(self.ccnm("cleanup", "demo", "--apply", "not-a-token").returncode, EXIT_INVALID_ARGS)
        expired = self.ccnm("cleanup", "demo", "--apply", "1-0000000000000000")
        self.assertEqual(expired.returncode, EXIT_NOT_READY)
        self.assertIn("expired", expired.stderr)

        # 预览之后 Runtime 上又写了东西：整份拒绝，一个都不删。
        with open(m1_output / "r-0000000000000001" / "stdout", "ab") as f:
            f.write(b"late\n")
        changed = self.ccnm("cleanup", "demo", "--apply", token)
        self.assertEqual(changed.returncode, EXIT_NOT_READY, changed.stdout + changed.stderr)
        self.assertIn("no longer what was previewed", changed.stderr)
        self.assertTrue(m1_output.exists() and (self.agent_state / "sessions" / m1).exists())
        self.assertTrue((self.state / "rpc/outputs" / handle).exists())

        done = self.ccnm("cleanup", "demo", "--apply", self.token(self.ccnm("cleanup", "demo")))
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertFalse(m1_output.exists())
        self.assertFalse((self.agent_state / "sessions" / m1).exists())
        self.assertFalse((self.state / "rpc/outputs" / handle).exists())
        self.assertTrue(guarded_output.exists(), "recovery evidence stays")
        self.assertTrue((self.agent_state / "sessions" / guarded).exists())
        self.assertEqual(markers[0].read_bytes(), marker_before, "cleanup is not recovery")

        # 清理过的会话：result 说 expired，status 照常，同一个键指回它、不重跑。
        with self.assertRaises(RpcError) as caught:
            client.session_result(handle)
        self.assertEqual(caught.exception.code, E_EXPIRED)
        self.assertEqual(caught.exception.data.get("reason"), "cleaned")
        self.assertEqual(client.session_status(handle)["state"], "completed")
        again = client.session_start("demo", "task two", start_key="k2")
        self.assertEqual((again["session"], again["reused"]), (handle, True))
        self.assertEqual(len(self.calls("agent-run")), 1, "never run again")

    # -- CL-05：Runtime 不通时部分完成，重试不误删、不丢配置 --

    def test_an_unreachable_runtime_leaves_the_agent_record_and_the_config(self):
        sid = str(uuid.uuid4())
        self.agent_session(sid)
        output = self.runtime_output(sid)
        self.runtime_reachable(False)
        out = self.ccnm("workspace", "remove", "demo", "--purge")
        self.assertEqual(out.returncode, EXIT_NOT_READY, out.stdout + out.stderr)
        self.assertIn("could not ask the Runtime", out.stdout)
        self.assertIn("[workspaces.demo]", self.config.read_text())
        self.assertTrue((self.agent_state / "sessions" / sid).exists(), "how a retry finds the Runtime half")
        self.assertTrue(output.exists())

        self.runtime_reachable(True)
        out = self.ccnm("workspace", "remove", "demo", "--purge")
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertFalse(output.exists())
        self.assertFalse((self.agent_state / "sessions" / sid).exists())
        self.assertNotIn("[workspaces.demo]", self.config.read_text())

    # -- 旧 Operator 的 agent-purge：按版本拒绝，不再照旧删 --

    def test_an_old_agent_purge_is_refused_and_removes_nothing(self):
        sid = str(uuid.uuid4())
        running = self.agent_session(sid, ended=False)
        out = subprocess.run(
            [
                str(BINARY),
                "--config",
                str(AGENT_CONFIG),
                "internal",
                "agent-purge",
                "--payload",
                b64({"protocol": 1, "workspace": "demo"}),
            ],
            env=self.agent_env,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(out.returncode, EXIT_VERSION, out.stderr)
        self.assertIn("agent-cleanup", out.stderr)
        self.assertTrue(running.exists())


if __name__ == "__main__":
    unittest.main()
