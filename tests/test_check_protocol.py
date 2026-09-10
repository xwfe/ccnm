"""协议 schema/fixture 校验的离线测试。

重点不是"仓库现状能通过"——那条只是基线。真正要证明的是这个校验器**抓得住
错误**：一个永远返回通过的检查脚本比没有检查更糟，因为它让人以为查过了。
"""

import importlib.util
import json
import shutil
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/check_protocol.py"
SPEC = importlib.util.spec_from_file_location("check_protocol", SCRIPT)
check_protocol = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check_protocol)


class ProtocolTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ccnm-protocol-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "docs").mkdir()
        shutil.copytree(ROOT / "docs/protocol", self.root / "docs/protocol")

    def fixture(self, name):
        return self.root / check_protocol.FIXTURES / (name + ".json")

    def load(self, name):
        return json.loads(self.fixture(name).read_text(encoding="utf-8"))

    def save(self, name, doc):
        self.fixture(name).write_text(
            json.dumps(doc, ensure_ascii=False, indent=2), encoding="utf-8")

    def schema(self):
        return json.loads((self.root / check_protocol.SCHEMA).read_text(encoding="utf-8"))

    def save_schema(self, schema):
        (self.root / check_protocol.SCHEMA).write_text(
            json.dumps(schema, ensure_ascii=False, indent=2), encoding="utf-8")

    def errors(self):
        return check_protocol.check(self.root)

    def test_repository_passes(self):
        self.assertEqual(check_protocol.check(ROOT), [])

    def test_extra_field_is_rejected(self):
        doc = self.load("session-start-ok")
        doc["message"]["result"]["extra"] = 1
        self.save("session-start-ok", doc)
        self.assertTrue(any("多出字段 extra" in e for e in self.errors()))

    def test_missing_required_field_is_rejected(self):
        doc = self.load("session-start-ok")
        del doc["message"]["result"]["reused"]
        self.save("session-start-ok", doc)
        self.assertTrue(any("缺少必填字段 reused" in e for e in self.errors()))

    def test_wrong_type_is_rejected(self):
        doc = self.load("session-start-ok")
        doc["message"]["result"]["reused"] = "no"
        self.save("session-start-ok", doc)
        self.assertTrue(any("类型应为 boolean" in e for e in self.errors()))

    def test_unknown_state_is_rejected(self):
        doc = self.load("session-status-running")
        doc["message"]["result"]["state"] = "paused"
        self.save("session-status-running", doc)
        self.assertTrue(any("应为" in e and "之一" in e for e in self.errors()))

    def test_error_code_outside_the_spec_table_is_rejected(self):
        doc = self.load("reject-busy")
        doc["message"]["error"]["code"] = -32077
        self.save("reject-busy", doc)
        self.assertTrue(any("不在说明文档的表里" in e for e in self.errors()))

    def test_documented_code_without_a_fixture_is_rejected(self):
        # 文档里定义了码，却没有样例，等于没定义。
        self.fixture("reject-busy").unlink()
        self.assertTrue(any("没有对应的 fixture" in e for e in self.errors()))

    def test_effect_is_required_on_every_error(self):
        doc = self.load("reject-busy")
        del doc["message"]["error"]["data"]["effect"]
        self.save("reject-busy", doc)
        self.assertTrue(any("缺少必填字段 effect" in e for e in self.errors()))

    def test_bad_timestamp_is_rejected(self):
        # 没有时区偏移的时间戳在两台机器之间毫无意义。
        doc = self.load("session-start-ok")
        doc["message"]["result"]["accepted_at"] = "2026-09-10T12:00:03"
        self.save("session-start-ok", doc)
        self.assertTrue(any("不匹配" in e for e in self.errors()))

    def test_bad_identifier_is_rejected(self):
        doc = self.load("session-start-ok")
        doc["message"]["result"]["workspace"] = "-starts-with-dash"
        self.save("session-start-ok", doc)
        self.assertTrue(any("不匹配" in e for e in self.errors()))

    def test_dangling_schema_ref_is_rejected(self):
        doc = self.load("session-start-ok")
        doc["$schema_ref"] = "#/$defs/no_such_definition"
        self.save("session-start-ok", doc)
        self.assertTrue(any("引用了不存在的定义" in e for e in self.errors()))

    def test_foreign_schema_ref_is_rejected(self):
        doc = self.load("session-start-ok")
        doc["$schema_ref"] = "other.json#/$defs/x"
        self.save("session-start-ok", doc)
        self.assertTrue(any("只支持本文件内" in e for e in self.errors()))

    def test_fixture_needs_a_note(self):
        doc = self.load("session-start-ok")
        doc["$note"] = "  "
        self.save("session-start-ok", doc)
        self.assertTrue(any("$note 不能为空" in e for e in self.errors()))

    def test_unknown_schema_keyword_is_rejected(self):
        # 拼错的关键字被静默忽略，等于这条约束从来没生效过。
        schema = self.schema()
        schema["$defs"]["session_state"]["enumm"] = ["x"]
        self.save_schema(schema)
        self.assertTrue(any("不认识的关键字：enumm" in e for e in self.errors()))

    def test_broken_json_is_reported_not_raised(self):
        self.fixture("reject-busy").write_text("{ not json", encoding="utf-8")
        self.assertTrue(any("不是合法 JSON" in e for e in self.errors()))

    def test_boolean_is_not_an_integer(self):
        # Python 里 True 是 int 的实例；漏掉这一步，schema 的整数约束就形同虚设。
        self.assertFalse(check_protocol.type_ok(True, "integer"))
        self.assertTrue(check_protocol.type_ok(True, "boolean"))
        self.assertTrue(check_protocol.type_ok(1, "integer"))

    def test_print_mode_requires_a_prompt(self):
        # input 的形状由 mode 决定，schema 用 oneOf 把两种模式分开表达。
        # 一个共用的 input 定义做不到这件事：它没法说"print 模式下 prompt 必填"。
        schema = self.schema()
        params = schema["$defs"]["session_start_params"]
        base = {"workspace": "ccnm", "mode": "print"}
        self.assertTrue(check_protocol.validate(
            dict(base, input={}), params, schema, "p"))
        self.assertEqual(check_protocol.validate(
            dict(base, input={"prompt": "x"}), params, schema, "p"), [])
        # interactive 的输入形状还没定稿，先保持开放，不假装已经定义。
        self.assertEqual(check_protocol.validate(
            {"workspace": "ccnm", "mode": "interactive", "input": {}}, params, schema, "p"), [])

    def test_one_of_needs_exactly_one_branch(self):
        root = {"$defs": {}}
        schema = {"oneOf": [{"type": "string"}, {"type": "integer"}]}
        self.assertEqual(check_protocol.validate("s", schema, root, "x"), [])
        self.assertEqual(check_protocol.validate(1, schema, root, "x"), [])
        self.assertTrue(check_protocol.validate(True, schema, root, "x"))


if __name__ == "__main__":
    unittest.main()
