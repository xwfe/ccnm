"""P53：scripts/ci_gates.py 该红的时候红，工作流在上传之前跑它。

门禁脚本最怕的不是报错，而是该红的时候绿：没有二进制时整类测试静默跳过、
unittest 照样报 OK，C51-03 之前 CI 就是这样一直绿着的。所以这里对每种"看着
像通过"的情况各造一个小测试目录，确认脚本退出码非零。

工作流那一半用文本读 YAML：CI 的系统 Python 不一定有 PyYAML，而这两个文件是
我们自己写的、结构固定。结构改了读不出来，这里会红，照新结构改这里即可。
"""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
GATES = ROOT / "scripts" / "ci_gates.py"
PASSING = "import unittest\n\nclass T(unittest.TestCase):\n    def test_ok(self):\n        pass\n"


class GatePolicyTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="ccnm-ci-gates-")
        self.addCleanup(temp.cleanup)
        self.dir = Path(temp.name)
        self.tests = self.dir / "tests"
        self.tests.mkdir()
        self.binary = self.fake_binary("ccnm 0.0.0-test")

    def fake_binary(self, says: str) -> Path:
        path = self.dir / f"bin-{abs(hash(says))}"
        path.write_text(f"#!/bin/sh\necho '{says}'\n", encoding="utf-8")
        path.chmod(0o755)
        return path

    def gates(self, *, binary: Path | None = None, env: dict | None = None) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, "-B", str(GATES), "--bin", str(binary or self.binary), "--tests", str(self.tests)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
            env={**os.environ, "GITHUB_ACTIONS": "", **(env or {})},
        )

    def write(self, name: str, body: str) -> None:
        (self.tests / name).write_text(body, encoding="utf-8")

    def test_passing_tests_pass_and_see_the_binary_it_was_given(self):
        self.write(
            "test_sees.py",
            "import os, unittest\n\nclass T(unittest.TestCase):\n"
            "    def test_bin(self):\n"
            f"        self.assertEqual(os.environ['CCNM_BIN'], {str(self.binary.resolve())!r})\n",
        )
        done = self.gates()
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("1 ran, 0 skipped", done.stdout)

    def test_a_skip_is_a_failure(self):
        # 正是 C51-03 之前的样子：找不到二进制，整类跳过，unittest 报 OK。
        self.write(
            "test_skips.py",
            "import unittest\n\n@unittest.skipIf(True, '先 cargo build')\nclass T(unittest.TestCase):\n"
            "    def test_x(self):\n        pass\n",
        )
        done = self.gates()
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("跳过 test_skips.T.test_x：先 cargo build", done.stderr)

    def test_a_failing_test_fails(self):
        self.write("test_ok.py", PASSING)
        self.write(
            "test_bad.py",
            "import unittest\n\nclass T(unittest.TestCase):\n    def test_no(self):\n        self.fail('no')\n",
        )
        done = self.gates()
        self.assertEqual(done.returncode, 1)
        self.assertIn("失败 test_bad.T.test_no", done.stderr)

    def test_a_module_that_does_not_import_fails(self):
        self.write("test_broken.py", "import no_such_module_for_ccnm\n")
        done = self.gates()
        self.assertEqual(done.returncode, 1)
        self.assertIn("出错", done.stderr)

    def test_no_tests_at_all_fails(self):
        done = self.gates()
        self.assertEqual(done.returncode, 1)
        self.assertIn("一条测试都没跑", done.stderr)

    def test_a_binary_that_is_not_ccnm_is_refused_before_any_test(self):
        self.write("test_ok.py", PASSING)
        done = self.gates(binary=self.fake_binary("something else 1.0"))
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("不是 ccnm", done.stderr)
        self.assertNotIn("== python", done.stdout)

    def test_on_github_the_failure_is_also_an_annotation(self):
        self.write(
            "test_bad.py",
            "import unittest\n\nclass T(unittest.TestCase):\n    def test_no(self):\n        self.fail('no')\n",
        )
        done = self.gates(env={"GITHUB_ACTIONS": "true"})
        self.assertEqual(done.returncode, 1)
        self.assertIn("::error title=scripts/ci_gates.py failed::失败 test_bad.T.test_no", done.stdout)


def steps(workflow: str) -> dict:
    """job 名 -> 按顺序的 step 文本。只认本仓库两个工作流的缩进：job 两格、step 六格。"""
    jobs: dict = {}
    job = None
    in_jobs = False
    for line in (ROOT / ".github" / "workflows" / workflow).read_text(encoding="utf-8").splitlines():
        if line.startswith("jobs:"):
            in_jobs = True
            continue
        if not in_jobs:
            continue
        found = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if found:
            job = found.group(1)
            jobs[job] = {"head": [], "steps": []}
            continue
        if job is None:
            continue
        if line.startswith("      - "):
            jobs[job]["steps"].append(line)
        elif jobs[job]["steps"] and line.startswith("        "):
            jobs[job]["steps"][-1] += "\n" + line
        else:
            jobs[job]["head"].append(line)
    return jobs


def position(job: dict, needle: str) -> int:
    hits = [i for i, step in enumerate(job["steps"]) if needle in step]
    if len(hits) != 1:
        raise AssertionError(f"{needle!r} 应该正好出现在一个 step 里，实际 {len(hits)} 个")
    return hits[0]


class WorkflowTests(unittest.TestCase):
    def test_ci_runs_the_gates_on_both_platforms(self):
        jobs = steps("ci.yml")
        for name in ("test", "linux-runtime"):
            position(jobs[name], "scripts/ci_gates.py")

    def test_a_release_uploads_only_after_the_gates_and_publishes_only_after_both(self):
        text = (ROOT / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")
        # 任何一条都能让失败的门禁之后照样上传或发布。
        self.assertNotIn("continue-on-error", text)
        self.assertNotIn("always()", text)
        jobs = steps("release.yml")
        for name in ("macos", "linux"):
            gate = position(jobs[name], "scripts/ci_gates.py")
            upload = position(jobs[name], "actions/upload-artifact")
            self.assertLess(gate, upload, name)
        self.assertIn("    needs: [macos, linux]", jobs["publish"]["head"])


if __name__ == "__main__":
    unittest.main()
