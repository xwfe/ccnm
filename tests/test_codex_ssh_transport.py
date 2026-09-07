"""Offline guard tests; no SSH, model or credential file is used."""
import base64
import importlib.util
import json
from pathlib import Path
import shlex
import unittest

SPEC = importlib.util.spec_from_file_location(
    "transport", Path(__file__).resolve().parents[1] / "scripts/probe_codex_ssh_transport.py"
)
transport = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(transport)
ARGS = ["runtime-alias", "/runtime/ccnm", "/runtime/project", "/runtime/config.toml", "/runtime/home", "probe-1"]


class TransportTests(unittest.TestCase):
    def test_provider_environment_is_removed_before_starting_ssh(self):
        original = {"CODEX_HOME": "/agent/private", "OPENAI_API_KEY": "synthetic",
                    "CODEX_ACCESS_TOKEN": "synthetic", "CLAUDE_CONFIG_DIR": "/agent/claude",
                    "ANTHROPIC_API_KEY": "synthetic", "PATH": "/usr/bin", "SSH_AUTH_SOCK": "/agent/socket"}
        clean = transport.ssh_environment(original)
        self.assertEqual(clean, {"PATH": "/usr/bin", "SSH_AUTH_SOCK": "/agent/socket"})
        self.assertEqual(original["CODEX_HOME"], "/agent/private")

    def test_remote_environment_and_payload_have_runtime_paths_only(self):
        argv = transport.command(*ARGS)
        self.assertIn("ForwardAgent=no", argv)
        self.assertIn("SendEnv=-*", argv)
        self.assertIn("ControlPath=none", argv)
        remote = shlex.split(argv[-1])
        self.assertEqual(remote[:2], ["/usr/bin/env", "-i"])
        self.assertNotIn("CODEX_HOME", " ".join(remote))
        self.assertNotIn("SSH_AUTH_SOCK", " ".join(remote))
        self.assertEqual(remote[4], "HOME=/runtime/home")
        wire = remote[-1]
        payload = json.loads(base64.urlsafe_b64decode(wire + "=" * (-len(wire) % 4)))
        self.assertEqual(payload, {"protocol": 1, "workspace": "fixture", "root": "/runtime/project",
                                   "session": "probe-1", "policy": "coding", "interactive": True})

    def test_quoted_runtime_paths_remain_one_argument(self):
        args = ARGS.copy()
        args[1] = "/runtime/bin with 'quote'/ccnm"
        args[4] = "/runtime/home with spaces"
        remote = shlex.split(transport.command(*args)[-1])
        self.assertIn(args[1], remote)
        self.assertIn("HOME=" + args[4], remote)

    def test_invalid_alias_session_and_paths_are_rejected(self):
        for index, value in [(0, "-oProxyCommand=x"), (0, "host;id"), (0, "host name"),
                             (5, "../session"), (5, ".."), (0, "."), (1, "relative/ccnm"), (2, "/runtime/../home"),
                             (3, "/runtime/config\nvalue"), (4, "/runtime/\x00home")]:
            with self.subTest(index=index, value=value):
                args = ARGS.copy()
                args[index] = value
                with self.assertRaises(ValueError):
                    transport.command(*args)


if __name__ == "__main__":
    unittest.main()
