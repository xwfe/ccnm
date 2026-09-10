"""对照工具自己的测试。

真机那一次要靠 `scripts/p7_parity_check.py` 得出结论，所以它自己不能是没验过
的代码。这里离线证明两件事：判定逻辑该红的时候红，以及**没跑成的时候绝不报
绿**——那是它唯一不可原谅的失败模式，因为没人会去复查一个绿灯。

这里不会启动真实 Agent：端到端那条用 `.invalid` 的配置，两条腿都注定失败，
测的是它如何如实地失败。
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from p7_parity_check import (  # noqa: E402
    blames_the_guard,
    resolve_agent,
    build_checks,
    check_side_effect,
    clear_target,
    guards_held,
    probe_prompt,
    remove_artifact,
    scan_for_private,
    settle_between_legs,
    verdict,
)


HARNESS = ROOT / "scripts/p7_parity_check.py"

# 和 tests/test_blackbox_client.py 同样的沙盒：worker 指向保留域 .invalid，
# `ssh -G` 解析得动但连不上任何东西。
SANDBOX_CONFIG = """
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "worker.invalid"
[workspaces.demo]
root = "{root}"
agent = {{ node = "worker", instance = "claude-main" }}
"""


def ccnm_binary():
    override = os.environ.get("CCNM_BIN")
    if override:
        return Path(override)
    for profile in ("debug", "release"):
        candidate = ROOT / "target" / profile / "ccnm"
        if candidate.is_file():
            return candidate
    return None


BINARY = ccnm_binary()


def passing_inputs():
    """一整套"全部通过"的输入，各用例只改自己关心的那一项。"""
    human = {"ok": True, "reason": None}
    human_effect = {"present": True, "matches": True, "owner": {"uid": 504, "name": "ccrun"}}
    machine = {"ok": True, "reason": None, "provider": "claude"}
    machine_effect = {"present": True, "matches": True, "owner": {"uid": 504, "name": "ccrun"}}
    return human, human_effect, machine, machine_effect


class VerdictTests(unittest.TestCase):
    def test_all_true_passes(self):
        self.assertEqual(verdict([{"name": "a", "passed": True, "detail": ""}]), "pass")

    def test_any_false_fails(self):
        checks = [
            {"name": "a", "passed": True, "detail": ""},
            {"name": "b", "passed": False, "detail": ""},
        ]
        self.assertEqual(verdict(checks), "fail")

    def test_undecidable_is_not_a_pass(self):
        """判不出来算 inconclusive，不能当通过——这是整个工具的底线。"""
        checks = [
            {"name": "a", "passed": True, "detail": ""},
            {"name": "b", "passed": None, "detail": ""},
        ]
        self.assertEqual(verdict(checks), "inconclusive")

    def test_false_outranks_undecidable(self):
        checks = [
            {"name": "a", "passed": None, "detail": ""},
            {"name": "b", "passed": False, "detail": ""},
        ]
        self.assertEqual(verdict(checks), "fail")


class ChecksTests(unittest.TestCase):
    def test_everything_agreeing_passes(self):
        checks = build_checks(*passing_inputs(), [], "claude")
        self.assertEqual(verdict(checks), "pass")

    def test_different_owners_fail(self):
        """两个入口落到不同身份上，就不是同一条隔离执行链。"""
        human, human_effect, machine, machine_effect = passing_inputs()
        machine_effect["owner"] = {"uid": 501, "name": "bing"}
        checks = build_checks(human, human_effect, machine, machine_effect, [], "claude")
        self.assertEqual(verdict(checks), "fail")
        named = {check["name"]: check["passed"] for check in checks}
        self.assertIs(named["same_runtime_owner"], False)

    def test_missing_owner_is_undecidable_not_pass(self):
        human, human_effect, machine, machine_effect = passing_inputs()
        machine_effect.pop("owner")
        checks = build_checks(human, human_effect, machine, machine_effect, [], "claude")
        named = {check["name"]: check["passed"] for check in checks}
        self.assertIsNone(named["same_runtime_owner"])

    def test_leaked_marker_fails(self):
        checks = build_checks(*passing_inputs(), ["auth.json"], "claude")
        named = {check["name"]: check["passed"] for check in checks}
        self.assertIs(named["machine_api_kept_private_data_out"], False)

    def test_wrong_provider_fails(self):
        human, human_effect, machine, machine_effect = passing_inputs()
        machine["provider"] = "codex"
        checks = build_checks(human, human_effect, machine, machine_effect, [], "claude")
        named = {check["name"]: check["passed"] for check in checks}
        self.assertIs(named["provider_matches_declaration"], False)

    def test_absent_provider_is_undecidable(self):
        """Agent 没自报身份时不能替它断言，也不能当通过。"""
        human, human_effect, machine, machine_effect = passing_inputs()
        machine["provider"] = None
        checks = build_checks(human, human_effect, machine, machine_effect, [], "claude")
        named = {check["name"]: check["passed"] for check in checks}
        self.assertIsNone(named["provider_matches_declaration"])


class GuardTests(unittest.TestCase):
    """两条腿之间的排队。

    guard 由 Runtime 侧的 MCP 进程持有、进程退出才释放，而 `ccnm run --print`
    走另一条 SSH 通道返回，两者没有同步。第一条腿刚回来就起第二条，可能撞上
    guard 还锁着——那看起来像 machine API 坏了，实际只是没排开。
    """

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ccnm-guard-")
        self.addCleanup(self.temp.cleanup)
        self.guards = Path(self.temp.name)

    def test_unreadable_directory_is_unknown_not_empty(self):
        """读不到就说读不到，不能当成"没有人占着"。"""
        self.assertIsNone(guards_held(None))
        self.assertIsNone(guards_held(self.guards / "nosuch"))

    def test_all_released_reads_as_free(self):
        (self.guards / "a.lock").write_text("released\n", encoding="utf-8")
        self.assertEqual(guards_held(self.guards), [])

    def test_a_held_guard_is_reported(self):
        (self.guards / "a.lock").write_text("released\n", encoding="utf-8")
        (self.guards / "b.lock").write_text("held s-1 demo\n", encoding="utf-8")
        self.assertEqual(guards_held(self.guards), ["held s-1 demo"])

    def test_settle_observes_the_directory_when_it_can(self):
        (self.guards / "a.lock").write_text("released\n", encoding="utf-8")
        result = settle_between_legs(self.guards, 5)
        self.assertEqual(result["method"], "guard-dir")
        self.assertTrue(result["observed"])
        self.assertNotIn("still_held", result)

    def test_settle_gives_up_and_says_so(self):
        (self.guards / "a.lock").write_text("held s-1 demo\n", encoding="utf-8")
        result = settle_between_legs(self.guards, 0.6)
        self.assertEqual(result["still_held"], ["held s-1 demo"])

    def test_settle_falls_back_to_blind_waiting(self):
        """读不到目录只能盲等，证据里必须写明这一步是假设不是观察。"""
        result = settle_between_legs(None, 0)
        self.assertEqual(result["method"], "fixed-delay")
        self.assertFalse(result["observed"])

    def test_guard_failure_is_told_apart_from_a_protocol_failure(self):
        busy = {"ok": False, "reason": "终态是 failed，不是 completed",
                "text_tail": "workspace write guard is busy; another managed session…"}
        self.assertTrue(blames_the_guard(busy))
        other = {"ok": False, "reason": "协议错误 -32009：no such workspace or instance"}
        self.assertFalse(blames_the_guard(other))

    def test_a_successful_leg_never_blames_the_guard(self):
        self.assertFalse(blames_the_guard({"ok": True, "text_tail": "write guard"}))


class ResolveAgentTests(unittest.TestCase):
    """把 --instance 补成协议要的 {node, instance}。

    session.start 的 agent.node 是必填的，哪怕调用方不许换机器——真机第一轮就
    栽在只给 instance 上，回 -32602。
    """

    LISTED = [
        {"node": "agent", "instance": "claude-main", "workspaces": ["p7parity"]},
        {"node": "agent", "instance": "codex-main", "workspaces": ["other"]},
    ]

    def test_neither_given_means_use_the_configured_default(self):
        self.assertIsNone(resolve_agent(self.LISTED, None, None))

    def test_instance_alone_gets_its_node_looked_up(self):
        self.assertEqual(
            resolve_agent(self.LISTED, None, "claude-main"),
            {"node": "agent", "instance": "claude-main"},
        )

    def test_both_given_are_passed_through(self):
        self.assertEqual(
            resolve_agent(self.LISTED, "agent", "claude-main"),
            {"node": "agent", "instance": "claude-main"},
        )

    def test_an_unknown_instance_stops_rather_than_guessing(self):
        with self.assertRaises(SystemExit):
            resolve_agent(self.LISTED, None, "nosuch")

    def test_an_ambiguous_instance_stops_rather_than_picking_one(self):
        """同名 instance 在两个 node 上：挑错 node 比报错糟得多。"""
        listed = self.LISTED + [{"node": "other-node", "instance": "claude-main"}]
        with self.assertRaises(SystemExit):
            resolve_agent(listed, None, "claude-main")


class SideEffectTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ccnm-parity-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.token = "ccnmp7-deadbeef"
        self.path = self.root / f"ccnm-parity-cli-{self.token}.txt"

    def test_missing_file_does_not_match(self):
        result = check_side_effect(self.path, self.token)
        self.assertFalse(result["present"])
        self.assertFalse(result["matches"])

    def test_correct_file_matches_and_records_owner(self):
        self.path.write_text(self.token + "\n", encoding="utf-8")
        result = check_side_effect(self.path, self.token)
        self.assertTrue(result["matches"])
        self.assertEqual(result["owner"]["uid"], os.getuid())

    def test_wrong_content_does_not_match(self):
        self.path.write_text("something else", encoding="utf-8")
        result = check_side_effect(self.path, self.token)
        self.assertTrue(result["present"])
        self.assertFalse(result["matches"])

    def test_symlink_does_not_match(self):
        """指向别处的软链接不算产物，否则一条链接就能伪造通过。"""
        real = self.root / "real.txt"
        real.write_text(self.token, encoding="utf-8")
        self.path.symlink_to(real)
        result = check_side_effect(self.path, self.token)
        self.assertFalse(result["matches"])

    def test_clear_target_refuses_an_occupied_path(self):
        self.path.write_text("stale", encoding="utf-8")
        with self.assertRaises(SystemExit):
            clear_target(self.path)

    def test_clear_target_accepts_a_free_path(self):
        clear_target(self.path)

    def test_remove_artifact_only_touches_this_round(self):
        self.path.write_text(self.token, encoding="utf-8")
        self.assertTrue(remove_artifact(self.path, self.token))
        self.assertFalse(self.path.exists())

    def test_remove_artifact_keeps_a_mismatching_file(self):
        """内容不对的文件是"没通过"的现场，删了就没法查了。"""
        self.path.write_text("wrong", encoding="utf-8")
        self.assertFalse(remove_artifact(self.path, self.token))
        self.assertTrue(self.path.exists())

    def test_remove_artifact_ignores_a_foreign_name(self):
        other = self.root / "someone-elses.txt"
        other.write_text(self.token, encoding="utf-8")
        self.assertFalse(remove_artifact(other, self.token))
        self.assertTrue(other.exists())


class PromptAndScanTests(unittest.TestCase):
    def test_prompt_carries_the_exact_path_and_token(self):
        path = Path("/tmp/ws/ccnm-parity-api-ccnmp7-1234.txt")
        prompt = probe_prompt(path, "ccnmp7-1234")
        self.assertIn(str(path), prompt)
        self.assertIn("ccnmp7-1234", prompt)

    def test_scan_finds_a_credential_name(self):
        found = scan_for_private([{"path": "/x/.codex/auth.json"}], "/Users/nobody")
        self.assertIn("auth.json", found)
        self.assertIn(".codex", found)

    def test_scan_finds_the_home_path(self):
        found = scan_for_private([{"cwd": "/Users/nobody/work"}], "/Users/nobody")
        self.assertIn("/Users/nobody", found)

    def test_clean_responses_scan_clean(self):
        self.assertEqual(scan_for_private([{"session": "s-1", "state": "completed"}], "/Users/nobody"), [])


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class OfflineEndToEndTests(unittest.TestCase):
    """真跑一遍脚本，对端是连不上的 `.invalid`。

    两条腿都会失败，这正是要测的：它必须给出 pass 以外的判定、写下证据、
    并以非零退出码结束。
    """

    def test_it_reports_failure_instead_of_inventing_success(self):
        with tempfile.TemporaryDirectory(prefix="ccnm-parity-e2e-") as temp:
            home = Path(temp)
            workspace = home / "demo"
            workspace.mkdir()
            config = home / "config.toml"
            config.write_text(SANDBOX_CONFIG.format(root=workspace), encoding="utf-8")
            evidence = home / "evidence.json"

            done = subprocess.run(
                [
                    sys.executable, str(HARNESS),
                    "--workspace", "demo",
                    "--root", str(workspace),
                    "--provider", "claude",
                    "--ccnm", str(BINARY),
                    "--config", str(config),
                    "--timeout", "15",
                    "--settle-seconds", "0",
                    "--out", str(evidence),
                ],
                capture_output=True,
                text=True,
                timeout=300,
                env={
                    **os.environ,
                    "HOME": str(home),
                    "XDG_STATE_HOME": str(home / "state"),
                    "XDG_CONFIG_HOME": str(home / "config"),
                },
            )

            self.assertNotEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertTrue(evidence.is_file(), "没写证据文件")
            record = json.loads(evidence.read_text(encoding="utf-8"))
            self.assertNotEqual(record["verdict"], "pass")
            # 两条腿都没跑成，所以两个副作用检查都必须是明确的未通过。
            named = {check["name"]: check["passed"] for check in record["checks"]}
            self.assertIs(named["human_cli_side_effect"], False)
            self.assertIs(named["machine_api_side_effect"], False)
            # 没留下产物，也没留下半截文件。
            self.assertEqual(list(workspace.iterdir()), [])

    def test_it_checks_before_it_acts(self):
        """先检查、不动手：工作树不存在就停下，不去创建、不去猜。"""
        with tempfile.TemporaryDirectory(prefix="ccnm-parity-taken-") as temp:
            home = Path(temp)
            workspace = home / "demo"
            workspace.mkdir()
            config = home / "config.toml"
            config.write_text(SANDBOX_CONFIG.format(root=workspace), encoding="utf-8")
            done = subprocess.run(
                [
                    sys.executable, str(HARNESS),
                    "--workspace", "demo",
                    "--root", str(home / "nosuch"),
                    "--ccnm", str(BINARY),
                    "--config", str(config),
                ],
                capture_output=True,
                text=True,
                timeout=60,
            )
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("工作树不存在", done.stdout + done.stderr)


class SuccessPathTests(unittest.TestCase):
    """成功路径：对端换成 tests/fixtures/fake_ccnm.py。

    真 ccnm 配上 `.invalid` 只能演失败，而真机那一次最贵的错误是**读错一次
    成功**——额度已经花掉，结论却是错的。这里让假二进制真把产物写出来，把
    产物检查、属主比对、清理和泄漏扫描整条串起来跑一遍。
    """

    FAKE = ROOT / "tests/fixtures/fake_ccnm.py"

    def run_harness(self, extra_env=None, extra_args=()):
        temp = tempfile.TemporaryDirectory(prefix="ccnm-parity-ok-")
        self.addCleanup(temp.cleanup)
        home = Path(temp.name)
        workspace = home / "demo"
        workspace.mkdir()
        config = home / "config.toml"
        config.write_text(SANDBOX_CONFIG.format(root=workspace), encoding="utf-8")
        evidence = home / "evidence.json"
        done = subprocess.run(
            [
                sys.executable, str(HARNESS),
                "--workspace", "demo",
                "--root", str(workspace),
                "--provider", "claude",
                "--ccnm", str(self.FAKE),
                "--config", str(config),
                "--timeout", "15",
                "--settle-seconds", "0",
                "--out", str(evidence),
                *extra_args,
            ],
            capture_output=True, text=True, timeout=120,
            env={**os.environ, **(extra_env or {})},
        )
        record = json.loads(evidence.read_text(encoding="utf-8")) if evidence.is_file() else None
        return done, record, workspace

    def named(self, record):
        return {check["name"]: check["passed"] for check in record["checks"]}

    def test_a_real_success_reads_as_pass(self):
        done, record, workspace = self.run_harness()
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertEqual(record["verdict"], "pass")
        self.assertTrue(all(value is True for value in self.named(record).values()))
        self.assertEqual(record["machine_api"]["provider"], "claude")
        # 两条腿的产物都清掉了，工作树回到原样。
        self.assertEqual(len(record["artifacts_removed"]), 2)
        self.assertEqual(list(workspace.iterdir()), [])

    def test_claiming_done_without_writing_the_file_is_a_failure(self):
        """Agent 回 DONE 但什么都没做——这正是不能只信文字的理由。"""
        done, record, _ = self.run_harness({"FAKE_CCNM_SKIP_ARTIFACT": "api"})
        self.assertNotEqual(done.returncode, 0)
        self.assertEqual(record["verdict"], "fail")
        named = self.named(record)
        # 进程层面它是成功的，只有副作用戳穿了它。
        self.assertIs(named["machine_api_succeeded"], True)
        self.assertIs(named["machine_api_side_effect"], False)
        self.assertIs(named["human_cli_side_effect"], True)

    def test_a_leaked_credential_name_is_caught_in_a_real_response(self):
        done, record, _ = self.run_harness({"FAKE_CCNM_LEAK": "1"})
        self.assertNotEqual(done.returncode, 0)
        self.assertIs(self.named(record)["machine_api_kept_private_data_out"], False)
        self.assertIn("auth.json", record["private_markers_found"])

    def test_a_different_provider_than_declared_is_caught(self):
        done, record, _ = self.run_harness({"FAKE_CCNM_PROVIDER": "codex"})
        self.assertNotEqual(done.returncode, 0)
        self.assertIs(self.named(record)["provider_matches_declaration"], False)

    def test_a_busy_work_tree_stops_it_before_spending_anything(self):
        """开跑前就有 guard 锁着，一条腿都不该起——起了也是白花额度。"""
        temp = tempfile.TemporaryDirectory(prefix="ccnm-parity-busy-")
        self.addCleanup(temp.cleanup)
        home = Path(temp.name)
        workspace = home / "demo"
        workspace.mkdir()
        guards = home / "write-guards"
        guards.mkdir()
        (guards / "a.lock").write_text("held s-old demo\n", encoding="utf-8")
        config = home / "config.toml"
        config.write_text(SANDBOX_CONFIG.format(root=workspace), encoding="utf-8")

        done = subprocess.run(
            [
                sys.executable, str(HARNESS),
                "--workspace", "demo", "--root", str(workspace),
                "--ccnm", str(self.FAKE), "--config", str(config),
                "--guard-dir", str(guards),
            ],
            capture_output=True, text=True, timeout=60,
        )
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("held s-old demo", done.stdout + done.stderr)
        # 一个产物都没有，说明两条腿都没起。
        self.assertEqual(list(workspace.iterdir()), [])

    def test_keep_artifacts_leaves_them_for_inspection(self):
        _, record, workspace = self.run_harness(extra_args=["--keep-artifacts"])
        self.assertEqual(record["artifacts_removed"], [])
        self.assertEqual(len(list(workspace.iterdir())), 2)


if __name__ == "__main__":
    unittest.main()
