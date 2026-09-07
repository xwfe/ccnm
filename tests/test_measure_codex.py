"""Offline tests for the optional real-CLI measurement harness."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "measure_codex", Path(__file__).resolve().parents[1] / "scripts/measure_codex.py"
)
probe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(probe)


class MeasurementTests(unittest.TestCase):
    def test_redaction_keeps_json_valid_and_does_not_hide_following_lines(self):
        with tempfile.TemporaryDirectory(prefix=f"ccnm-redaction-{os.getpid()}-") as temp:
            directory = Path(temp)
            probe.save(directory, "auth.json", {
                "stderr": "Logged in using an API key - SECRET\nFollowing diagnostic\n",
                "stdout": "fixture@example.invalid", "exit_code": 0,
            })
            result = json.loads((directory / "auth.json").read_text())
            self.assertNotIn("SECRET", result["stderr"])
            self.assertIn("Following diagnostic", result["stderr"])
            self.assertEqual(result["stdout"], "<redacted-email>")
            self.assertEqual(result["exit_code"], 0)

    def test_timeout_preserves_output_and_stops_the_child(self):
        result = probe.run([
            sys.executable, "-c", "import time; print('started', flush=True); time.sleep(30)",
        ], timeout=0.2)
        self.assertTrue(result["timed_out"])
        self.assertNotEqual(result["exit_code"], 0)
        self.assertIn("started", result["stdout"])

    def test_live_probe_uses_only_fixture_runtime_and_fixed_policy(self):
        def fake_run(argv, *, cwd, stdin, timeout):
            self.assertIn("--ignore-user-config", argv)
            self.assertIn("--ignore-rules", argv)
            self.assertEqual(argv[argv.index("--sandbox") + 1], "read-only")
            self.assertIn("agents.enabled=false", argv)
            self.assertIn('mcp_servers.ccnm.default_tools_approval_mode="approve"', argv)
            self.assertEqual(stdin, probe.PROMPT.encode())
            self.assertEqual(timeout, 120)
            arg = next(arg for arg in argv if arg.startswith("mcp_servers.ccnm.args="))
            transport = json.loads(arg.split("=", 1)[1])
            self.assertEqual(transport[0], "-i")
            self.assertEqual(transport[1:3], ["PATH=/usr/bin:/bin", "USER=fixture"])
            self.assertFalse(any("API_KEY=" in arg or "TOKEN=" in arg for arg in transport))
            runtime = cwd.parent / "runtime"
            (runtime / "probe.txt").write_text("CCNM_RUNTIME_PATCHED_7319\n")
            events = [{"type": "item.completed", "item": {
                "type": "mcp_tool_call", "server": "ccnm", "tool": name,
                "status": "completed", "error": None,
            }} for name in probe.TOOLS]
            if terminal_present:
                events.append({"type": "turn.completed", "usage": {}})
            return {"exit_code": 0, "timed_out": False, "stderr": "",
                    "stdout": "\n".join(json.dumps(event) for event in events)}

        with tempfile.TemporaryDirectory(prefix=f"ccnm-harness-{os.getpid()}-") as temp:
            output = Path(temp)
            for terminal_present in (True, False):
                with patch.object(probe, "run", side_effect=fake_run):
                    self.assertEqual(
                        probe.seven_tools("/fixture-bin/codex", output, Path("/fixture-bin/ccnm")),
                        terminal_present,
                    )
            record = json.loads((output / "seven-tools.json").read_text())
            self.assertEqual(record["agent_file"], "WRONG_AGENT_NODE_9520\n")
            self.assertNotIn("/var/folders/", json.dumps(record))
            self.assertIn("<fixture-payload-generated-above>", json.dumps(record))


if __name__ == "__main__":
    unittest.main()
