"""P12 的 dogfood 工具本身的离线测试。

真机那一轮最贵的不是跑错，是**跑对了但工具读错了**：机器时间、订阅额度花掉，
结论却是假的。所以这里先用一个把 bridge 接到本机真实 server 的替身，把工具的
成功路径和几条失败路径各走一遍。

它证明的是工具读得懂一次真实的 dogfood，**不证明任何真机结论**——真项目、真
Linux Runtime、真 ssh 那半边见 docs/plan/p12-real-project-session.md。

这里的"项目"是一棵真的 git 仓库，但它的"编译器"是一个十行的 shell 脚本：**只
要目标文件里有那行毒，它就失败并指出文件名**。这正是真机上 cargo 做的事，只是
不花两分钟。工具要判的东西——patch 有没有落到工作树、失败的输出翻不翻得动、收
回去之后 git 说不说干净——和编译器是谁无关。

身份审计那一段在这里是跳过的：跑测试的是开发者自己的账号，它本来就在
admin/staff 里、可能还有 sudo。跳过不是通过，所以另有一条用例反过来验：**不跳
过时它必须红**。
"""

from __future__ import annotations

import getpass
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/p12_dogfood_check.py"
FAKE = ROOT / "tests/fixtures/fake_bridge_ccnm.py"

ANCHOR = "pub use config::Config;"
POISON = 'const P12_DOGFOOD_POISON: u32 = "this must not compile";'

# 这个"编译器"要做两件真编译器也做的事：看见毒就失败，并且在错误里指出文件名。
# 后面那 40 行是为了让 read_output 的两页确实不一样——一页 512 字节。
BUILD = """#!/bin/sh
if grep -q P12_DOGFOOD_POISON src/lib.rs; then
    echo "error[E0308]: mismatched types" >&2
    echo "  --> src/lib.rs:3:40" >&2
    i=0
    while [ $i -lt 40 ]; do
        echo "note: expected u32, found &str (padding line $i to make paging real)" >&2
        i=$((i + 1))
    done
    exit 1
fi
echo "Finished dev [unoptimized] target(s)"
"""

TEST = """#!/bin/sh
grep -q P12_DOGFOOD_POISON src/lib.rs && exit 1
echo "test result: ok. 3 passed; 0 failed"
"""

LIB = f"""//! A file that stands in for a real source file.

{ANCHOR}

pub fn answer() -> u32 {{
    42
}}
"""


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
class DogfoodToolTests(unittest.TestCase):
    def setUp(self):
        self.dir = Path(
            # 真实路径：凭据检查对"路径可达性未知"是 fail-closed 的，而 macOS 的
            # /var 是一条符号链接。
            subprocess.run(
                ["mktemp", "-d", "-t", "ccnm-p12"], capture_output=True, text=True, check=True
            ).stdout.strip()
        ).resolve()
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.dir)], check=False))
        for sub in ("readonly", "closed", "home", "state", "runtimehome/.ssh", "otherhome"):
            (self.dir / sub).mkdir(parents=True)
        (self.dir / "runtimehome/.ssh/authorized_keys").write_text("ssh-ed25519 AAAA x\n")
        # 读不到别人的 home：0 权限位对属主同样生效（除了 root，而这里不是）。
        (self.dir / "otherhome").chmod(0o000)
        self.addCleanup(lambda: (self.dir / "otherhome").chmod(0o700))

        self.root = self.dir / "project"
        (self.root / "src").mkdir(parents=True)
        (self.root / "Cargo.toml").write_text('[package]\nname = "stand-in"\n', encoding="utf-8")
        (self.root / "src/lib.rs").write_text(LIB, encoding="utf-8")
        (self.root / "build.sh").write_text(BUILD, encoding="utf-8")
        (self.root / "test.sh").write_text(TEST, encoding="utf-8")
        self.git("init", "-q", "-b", "main")
        self.git("add", "-A")
        self.git(
            "-c",
            "user.email=p12@example.invalid",
            "-c",
            "user.name=p12",
            "commit",
            "-qm",
            "stand-in project",
        )

        self.config = self.dir / "config.toml"
        self.config.write_text(
            f"""
this = "runtime"

[nodes.runtime]

# 一个"没 opt-in"的 workspace 在现实里长这样：它是一个正常的受管 workspace，
# 只是没给外部 MCP 开口子。配置校验也只接受这一种——没有 agent 又 external_mcp
# 关着的 workspace 谁都用不了，它直接报错。
[nodes.agent]
ssh = "p12-agent-alias-not-used"

[workspaces.demo]
root = "{self.root}"
external_mcp = "coding"
allow_unconfined_exec = true

[workspaces.readonly]
root = "{self.dir / "readonly"}"
external_mcp = "read"

[workspaces.closed]
root = "{self.dir / "closed"}"
agent_node = "agent"
""",
            encoding="utf-8",
        )

    def git(self, *args: str) -> None:
        subprocess.run(["git", "-C", str(self.root), *args], check=True, capture_output=True)

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
        command = [
            sys.executable,
            str(SCRIPT),
            "--ccnm",
            str(FAKE),
            "--config",
            str(self.config),
            "--workspace",
            "demo",
            "--read-only-workspace",
            "readonly",
            "--closed-workspace",
            "closed",
            "--patch-target",
            "src/lib.rs",
            "--anchor",
            ANCHOR,
            "--poison",
            POISON,
            "--build-cmd",
            "/bin/sh build.sh",
            "--test-cmd",
            "/bin/sh test.sh",
            "--toolchain",
            "git --version",
            "--guard-dir",
            str(self.dir / "state/ccnm/write-guards"),
            "--out",
            str(self.dir / "evidence.json"),
            *args,
        ]
        return subprocess.run(
            command, env=self.env(**extra), capture_output=True, text=True, check=False
        )

    def evidence(self) -> dict:
        return json.loads((self.dir / "evidence.json").read_text(encoding="utf-8"))

    # -- 成功路径 ------------------------------------------------------

    def test_the_whole_dogfood_passes_and_writes_evidence(self):
        out = self.run_tool("--skip-identity-audit")
        self.assertEqual(out.returncode, 0, out.stderr)
        evidence = self.evidence()
        self.assertTrue(evidence["passed"], evidence)
        checks = evidence["checks"]

        self.assertEqual(len(checks["tools"]), 8)  # 七个，加 P36 的 load_skill
        self.assertIn("skipped", checks["identity"])
        self.assertIn("git version", checks["toolchain"]["git"])

        cycle = checks["cycle"]
        # 带毒时构建必须失败，而且错误里指着我们改的那个文件。
        self.assertEqual(cycle["poisoned_build_exit"], 1)
        self.assertTrue(cycle["build_error_named_the_file"])
        # read_output 两页都拿到了东西，且不是同一页。
        self.assertEqual(len(cycle["read_output_pages"]), 2)
        self.assertTrue(all(size > 0 for size in cycle["read_output_pages"]))
        self.assertTrue(cycle["tree_clean_after_revert"])
        self.assertEqual(cycle["test_exit"], 0)

        self.assertIn("CCNM_E_", checks["writer_busy"])
        self.assertEqual(len(checks["read_leg"]["tools"]), 5)  # 四个，加 P36 的 load_skill
        self.assertEqual(
            sorted(checks["read_leg"]["refusals"]), ["apply_patch", "exec_command", "read_output"]
        )
        self.assertIn("read mode", checks["escalation"])
        self.assertIn("CCNM_E_", checks["not_opted_in"])
        self.assertEqual(checks["leak_scan"], "clean")

        # Host 崩过之后。这里的"Host"和服务端是同一个进程（替身 exec 成了真
        # server），所以 kill -9 连收尾代码一起杀了，锁停在 held——真机上 Host
        # 和服务端隔着一条 ssh，远端读到 EOF 自己收尾，锁是 released。两种结局
        # 都要求"状态确定，且从这个状态往下走的路是通的"。
        crash = checks["host_crash"]
        self.assertTrue(crash["guard_after_crash"].startswith("held "), crash)
        self.assertIn("left held by an interrupted process", crash["coding_refused"])
        self.assertTrue(crash["read_still_opens"])
        self.assertTrue(crash["manual_recovery"]["cleared"])
        self.assertEqual(crash["coding_after_recovery"], "ok")
        self.assertIn("skipped", crash["orphan_check"])
        self.assertIn("skipped", checks["version_mismatch"])
        self.assertIn("skipped", checks["remote_gone"])

        # 真的收回去了：仓库干净，毒不在文件里。
        status = subprocess.run(
            ["git", "-C", str(self.root), "status", "--porcelain"],
            capture_output=True,
            text=True,
            check=True,
        )
        self.assertEqual(status.stdout, "")
        self.assertNotIn("POISON", (self.root / "src/lib.rs").read_text(encoding="utf-8"))

    # -- 失败路径 ------------------------------------------------------

    def test_a_build_that_never_saw_the_patch_is_caught(self):
        # "构建"永远成功：那就说明它跑的不是我们改过的那棵树。
        out = self.run_tool("--skip-identity-audit", "--build-cmd", "/usr/bin/true")
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("构建却成功了", self.evidence()["failure"])

    def test_a_missing_toolchain_is_caught(self):
        out = self.run_tool(
            "--skip-identity-audit", "--toolchain", "ccnm-no-such-tool-p12 --version"
        )
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("叫不动", self.evidence()["failure"])

    def test_the_identity_audit_refuses_the_developers_own_account(self):
        # 不跳过时它必须红：跑测试的账号在 admin/staff 里，正是它要拦的那种。
        out = self.run_tool(
            "--runtime-user",
            getpass.getuser(),
            "--runtime-home",
            str(self.dir / "runtimehome"),
            "--other-home",
            str(self.dir / "otherhome"),
        )
        self.assertNotEqual(out.returncode, 0)
        failure = self.evidence()["failure"]
        self.assertTrue(
            "组里" in failure or "sudo" in failure or "执行身份是" in failure, failure
        )

    def test_a_wrong_runtime_user_is_caught(self):
        out = self.run_tool(
            "--runtime-user",
            "somebody-else-entirely",
            "--runtime-home",
            str(self.dir / "runtimehome"),
        )
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("执行身份是", self.evidence()["failure"])

    def test_a_leaked_private_path_is_caught(self):
        # 替身往 stderr 上塞一个凭据路径，工具必须看见。
        out = self.run_tool("--skip-identity-audit", FAKE_BRIDGE_LEAK="1")
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("私有标记", self.evidence()["failure"])

    def test_a_workspace_that_never_opted_in_cannot_be_the_subject(self):
        out = self.run_tool("--skip-identity-audit", "--workspace", "closed")
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("bridge 起不来", self.evidence()["failure"])


if __name__ == "__main__":
    unittest.main()
