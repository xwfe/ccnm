"""计划校验的离线测试；不写真实状态，不调用模型或网络。"""

import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/check_plan.py"
SPEC = importlib.util.spec_from_file_location("check_plan", SCRIPT)
check_plan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check_plan)


class PlanTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ccnm-plan-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "proof.md").write_text("历史记录", encoding="utf-8")
        self.roadmap = "\n".join(f"### P{i} — 阶段\n- **P{i}.1** 验收" for i in range(3))
        now = "2026-09-07T23:22:36+09:00"
        self.state = {
            "schema_version": 1, "plan_id": "test", "last_updated": now,
            "current_task": "P1",
            "baseline": {"source": "proof.md", "implementation_commit": "4f8e678", "verification_kind": "historical"},
            "handoff": {"next_action": "认领 P1 并补边界失败测试"},
            "tasks": [
                {"id": f"P{i}", "title": "阶段", "status": "pending",
                 "depends_on": [f"P{i - 1}"] if i else [], "owner": None,
                 "started_at": None, "last_updated": now, "completed_criteria": [],
                 "evidence": [], "blockers": []} for i in range(3)
            ],
        }
        self.complete(0)

    def complete(self, index):
        task = self.state["tasks"][index]
        task["status"] = "completed"
        task["completed_criteria"] = [f"P{index}.1"]
        task["evidence"] = [{"kind": "historical", "criteria": [f"P{index}.1"],
                             "ref": "proof.md", "result": "历史记录，不是本轮重跑"}]

    def errors(self):
        return check_plan.validate(self.state, self.roadmap, self.root)

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def test_valid_state_is_read_only(self):
        before = copy.deepcopy(self.state)
        self.assertEqual(self.errors(), [])
        self.assertEqual(self.state, before)

    def test_completion_requires_all_criteria_and_evidence(self):
        self.state["tasks"][1]["status"] = "completed"
        self.assertTrue(any("全部验收" in e for e in self.errors()))
        self.state["tasks"][1]["completed_criteria"] = ["P1.1"]
        self.assertTrue(any("缺少 evidence" in e for e in self.errors()))

    def test_missing_dependency_and_out_of_order_execution(self):
        self.state["tasks"][2]["depends_on"] = []
        self.assertTrue(any("顺序依赖" in e for e in self.errors()))
        self.state["tasks"][2]["depends_on"] = ["P1"]
        self.state["tasks"][2]["status"] = "in_progress"
        self.assertTrue(any("前置阶段未完成" in e for e in self.errors()))

    def test_duplicate_phase_and_unknown_criterion(self):
        self.state["tasks"][1]["completed_criteria"] = ["P1.999"]
        self.assertTrue(any("未知验收" in e for e in self.errors()))
        self.state["tasks"][2]["id"] = "P1"
        self.assertTrue(any("同序" in e for e in self.errors()))

    def test_blocker_requires_reason_and_unblock_action(self):
        task = self.state["tasks"][1]
        task.update(status="blocked", started_at=self.state["last_updated"])
        self.assertTrue(any("必须记录 blocker" in e for e in self.errors()))
        task["blockers"] = [{"criteria": ["P1.1"], "reason": "缺少授权"}]
        self.assertTrue(any("解锁动作" in e for e in self.errors()))
        task["blockers"][0]["unblock"] = "由用户授权专用测试环境"
        self.assertEqual(self.errors(), [])

    def test_only_one_active_task_and_correct_cursor(self):
        for task in self.state["tasks"][1:]:
            task.update(status="in_progress", started_at=self.state["last_updated"], owner="test")
        self.assertTrue(any("多个 in_progress" in e for e in self.errors()))
        self.state["current_task"] = "P2"
        self.assertTrue(any("首个未完成" in e for e in self.errors()))

    def test_all_done_cursor_is_null(self):
        self.complete(1)
        self.complete(2)
        self.state["current_task"] = None
        self.assertEqual(self.errors(), [])

    def test_timezone_and_malformed_types(self):
        self.state["last_updated"] = "2026-09-07T23:22:36"
        self.assertTrue(any("时区" in e for e in self.errors()))
        self.state["tasks"][0].update(status=[], completed_criteria={}, evidence=False)
        self.assertTrue(self.errors())
        self.assertTrue(check_plan.validate([], self.roadmap, self.root))

    def test_duplicate_json_key_is_rejected(self):
        with self.assertRaises(ValueError):
            json.loads('{"status":"pending","status":"completed"}', object_pairs_hook=check_plan.unique_object)

    def test_evidence_cannot_be_missing_or_escape(self):
        ref = self.state["tasks"][0]["evidence"][0]
        for name in ["missing.md", "../proof.md", "/tmp/proof.md", "a\\proof.md"]:
            ref["ref"] = name
            self.assertTrue(any("evidence.ref" in e for e in self.errors()), name)

    def test_symlink_cannot_escape(self):
        with tempfile.TemporaryDirectory(prefix="ccnm-plan-outside-") as outside:
            target = Path(outside) / "outside.md"
            target.write_text("synthetic", encoding="utf-8")
            (self.root / "link.md").symlink_to(target)
            self.assertFalse(check_plan.local_file(self.root, "link.md"))

    def test_links_are_checked_in_every_markdown_file(self):
        self.write("AGENTS.md", "[计划](docs/plan/README.md)")
        self.write("docs/plan/README.md", "[记录](../research/note.md)")
        self.write("docs/architecture.md", "# 架构")
        note = self.write("docs/research/note.md", "[架构](../architecture.md)")
        self.assertEqual(check_plan.check_links(self.root), [])
        # docs/research/note.md 不在原来那 5 个入口文档里，以前它写错没人发现。
        note.write_text("[架构](../architectrue.md)", encoding="utf-8")
        self.assertEqual(check_plan.check_links(self.root),
                         ["docs/research/note.md 链接不存在：../architectrue.md"])

    def test_build_and_git_directories_are_skipped(self):
        for name in ["target/doc/x.md", "crates/app/target/x.md", ".git/x.md"]:
            self.write(name, "[没有这个文件](does-not-exist.md)")
        self.assertEqual(check_plan.markdown_files(self.root), [Path("proof.md")])
        self.assertEqual(check_plan.check_links(self.root), [])

    def test_protocol_links_and_anchors_are_skipped_but_escapes_are_not(self):
        self.write("docs/usage.md",
                   "[站点](https://example.com/none.md) [本页](#一节) [上级](../../outside.md)")
        self.assertEqual(check_plan.check_links(self.root),
                         ["docs/usage.md 链接越界：../../outside.md"])


if __name__ == "__main__":
    unittest.main()
