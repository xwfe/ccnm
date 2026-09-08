"""准备脚本的无特权入口检查；不创建真实系统账号。"""
import os
from pathlib import Path
import subprocess
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/p3-create-runtime-user.sh"


class RuntimeUserScriptTests(unittest.TestCase):
    def run_script(self, *args, **env):
        return subprocess.run(
            ["/bin/bash", str(SCRIPT), *args],
            env={**os.environ, **env}, capture_output=True, text=True, check=False,
        )

    def test_syntax(self):
        for script in [SCRIPT, SCRIPT.with_name("p3-authorize-runtime-key.sh"),
                       SCRIPT.with_name("p3-isolate-runtime-group.sh"),
                       SCRIPT.with_name("p3-authorize-local-runtime.sh")]:
            result = subprocess.run(["/bin/bash", "-n", str(script)], check=False)
            self.assertEqual(result.returncode, 0)

    def test_authorization_wrong_operator_refused(self):
        result = subprocess.run(
            ["/bin/bash", str(SCRIPT.with_name("p3-authorize-runtime-key.sh")), "--apply"],
            env={**os.environ, "SUDO_USER": "not-authorized"},
            capture_output=True, text=True, check=False,
        )
        self.assertNotEqual(result.returncode, 0)

    def test_group_wrong_operator_refused(self):
        result = subprocess.run(
            ["/bin/bash", str(SCRIPT.with_name("p3-isolate-runtime-group.sh")), "--apply"],
            env={**os.environ, "SUDO_USER": "not-authorized"},
            capture_output=True, text=True, check=False,
        )
        self.assertNotEqual(result.returncode, 0)

    def test_local_authorization_wrong_operator_refused(self):
        result = subprocess.run(
            ["/bin/bash", str(SCRIPT.with_name("p3-authorize-local-runtime.sh")), "--apply"],
            env={**os.environ, "SUDO_USER": "not-authorized"},
            capture_output=True, text=True, check=False,
        )
        self.assertNotEqual(result.returncode, 0)

    def test_missing_or_unknown_action(self):
        for args in [(), ("--delete",), ("--create", "extra")]:
            self.assertEqual(self.run_script(*args).returncode, 2)

    def test_wrong_operator_refused(self):
        result = self.run_script("--create", SUDO_USER="not-the-authorized-host-user")
        self.assertNotEqual(result.returncode, 0)

    @unittest.skipIf(os.geteuid() == 0, "无特权路径只在普通用户下运行")
    def test_unprivileged_create_refused(self):
        result = self.run_script("--create", SUDO_USER="fodelf")
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
