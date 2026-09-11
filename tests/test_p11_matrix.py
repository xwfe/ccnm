"""P11.3 的允许矩阵工具本身的离线测试。

真机那一轮最贵的不是跑错，是**跑对了但工具读错了**：额度花掉，结论却是假
的。所以这里先用一个把 bridge 接到本机真实 server 的替身，把工具的成功路径
和几条失败路径各走一遍。

它证明的是工具读得懂一次真实的矩阵，**不证明任何真机结论**——真实 Host 那
半边见 docs/plan/p11-real-host-session.md。
"""

from __future__ import annotations

import getpass
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/p11_matrix_check.py"
FAKE = ROOT / "tests/fixtures/fake_bridge_ccnm.py"


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


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class MatrixToolTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ccnm-p11-")
        self.addCleanup(self.temp.cleanup)
        # 真实路径：凭据检查对"路径可达性未知"是 fail-closed 的，而 macOS 的
        # /var 是一条符号链接。
        self.dir = Path(self.temp.name).resolve()
        for sub in ("project", "readonly", "home", "state"):
            (self.dir / sub).mkdir()
        self.root = self.dir / "project"
        (self.root / "keep.txt").write_text("kept\n", encoding="utf-8")
        self.config = self.dir / "config.toml"
        self.config.write_text(
            f"""
this = "runtime"

[nodes.runtime]

[workspaces.demo]
root = "{self.root}"
external_mcp = "coding"
allow_unconfined_exec = true

[workspaces.readonly]
root = "{self.dir / "readonly"}"
external_mcp = "read"
""",
            encoding="utf-8",
        )

    def env(self, **extra: str) -> dict:
        env = {
            "PATH": os.environ.get("PATH", ""),
            "HOME": str(self.dir / "home"),
            "XDG_STATE_HOME": str(self.dir / "state"),
            "CCNM_CONFIG": str(self.config),
            "FAKE_BRIDGE_CCNM": str(BINARY),
            "PYTHONDONTWRITEBYTECODE": "1",
        }
        env.update(extra)
        return env

    def run_tool(self, *args: str, **extra: str) -> subprocess.CompletedProcess:
        out = self.dir / "evidence.json"
        command = [
            sys.executable,
            str(SCRIPT),
            "--ccnm",
            str(FAKE),
            "--config",
            str(self.config),
            "--workspace",
            "demo",
            "--out",
            str(out),
            *args,
        ]
        return subprocess.run(
            command, env=self.env(**extra), capture_output=True, text=True, check=False
        )

    def evidence(self) -> dict:
        return json.loads((self.dir / "evidence.json").read_text(encoding="utf-8"))

    def test_a_whole_matrix_passes_and_writes_evidence(self):
        out = self.run_tool(
            "--runtime-user", getpass.getuser(), "--read-only-workspace", "readonly"
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        evidence = self.evidence()
        self.assertTrue(evidence["passed"], evidence)

        coding = evidence["checks"]["coding"]
        self.assertEqual(len(coding["tools"]), 7)
        # 产物属主由 Runtime 自己报，不是这边猜的。
        self.assertEqual(coding["artifact_owner"], getpass.getuser())
        self.assertIn("CCNM_E_", coding["second_coding_refused"])

        read = evidence["checks"]["read"]
        self.assertEqual(len(read["tools"]), 4)
        self.assertEqual(sorted(read["refusals"]), ["apply_patch", "exec_command", "read_output"])
        for line in read["refusals"].values():
            self.assertTrue(line.startswith("CCNM_E_POLICY"), line)

        self.assertIn("read mode", evidence["checks"]["escalation"]["refused"])
        self.assertEqual(evidence["checks"]["leak_scan"], "clean")
        self.assertEqual(evidence["checks"]["cleanup"], "removed")

        # 清理是真的：临时产物没了，本来就在的文件还在。
        left = sorted(p.name for p in self.root.iterdir())
        self.assertEqual(left, ["keep.txt"], left)

    def test_the_escalation_check_can_be_skipped_but_is_recorded(self):
        out = self.run_tool("--runtime-user", getpass.getuser())
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("skipped", self.evidence()["checks"]["escalation"])

    def test_a_wrong_owner_is_a_failure(self):
        out = self.run_tool("--runtime-user", "somebody-else")
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("产物属主", self.evidence()["failure"])

    def test_a_leaked_private_path_is_caught(self):
        # 替身往 stderr 上塞一个凭据路径，工具必须看见。
        out = self.run_tool(
            "--runtime-user", getpass.getuser(), FAKE_BRIDGE_LEAK="1"
        )
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("私有标记", self.evidence()["failure"])

    def test_a_workspace_that_never_opted_in_fails_loudly(self):
        out = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--ccnm",
                str(FAKE),
                "--config",
                str(self.config),
                "--workspace",
                "no-such-workspace",
                "--out",
                str(self.dir / "evidence.json"),
            ],
            env=self.env(),
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("bridge 起不来", self.evidence()["failure"])


if __name__ == "__main__":
    unittest.main()
