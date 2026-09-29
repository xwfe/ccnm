"""P59：`session.result` 的输出——完整保留、分流、按字节预算分页、游标失效。

和 test_rpc_exact_control.py 一样只走真实二进制的字节协议，不 import ccnm 代码；
对面的 Agent 由 tests/fixtures/fake_agent_ssh.py 冒充，它的 `agent-output` 只按偏移
切测试放进去的视图。脱敏、UTF-8 规整、超长截尾是真实 Agent 的事，由 Rust 测试覆盖。

这些用例对应 P57 探针 docs/research/probes/p57-output.py 的 C0–C5：P59 之前
Agent 只交回 2 KiB 尾巴，RPC 报 `truncated=false`，`max_bytes` 被忽略，stderr 丢失。
"""

from __future__ import annotations

import json
from pathlib import Path
import sys
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))
sys.path.insert(0, str(ROOT / "clients/python"))

from ccnm_machine_client import MachineClient, RpcError  # noqa: E402
from test_rpc_exact_control import BINARY, RpcSandbox, key_of  # noqa: E402

E_INVALID_PARAMS = -32602
E_EXPIRED = -32012


def marked(total: int) -> bytes:
    """确定的大输出：头、正中、尾各一个标记，尾部带中文。与样例项目的 bigout 同一规则。"""
    head, mid, tail = "P57-EARLY-MARKER\n", "P57-MIDDLE-MARKER\n", "P57-LATE-MARKER 结束了，中文收尾。\n"
    parts, size, n, middle = [head], len(head.encode()), 0, False
    while size < total - len(tail.encode()):
        if not middle and size >= total // 2:
            parts.append(mid)
            size += len(mid.encode())
            middle = True
            continue
        line = f"line {n:07d} 这是一行确定的填充文本 abcdefghijklmnopqrstuvwxyz\n"
        parts.append(line)
        size += len(line.encode())
        n += 1
    parts.append(tail)
    return "".join(parts).encode("utf-8")


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class OutputTests(RpcSandbox):
    def view(self, prompt: str, stream: str, data: bytes) -> None:
        (self.fake / f"view-{key_of(prompt)}-{stream}").write_bytes(data)

    def output_mode(self, kind: str) -> None:
        (self.fake / "output-mode.json").write_text(json.dumps({"kind": kind}))

    def finished(self, client: MachineClient, prompt: str, reply: dict | None = None) -> str:
        if reply:
            self.reply(prompt, reply)
        self.release(prompt)
        session = self.start(client, prompt)["session"]
        self.settle(client, session)
        return session

    def page(self, client: MachineClient, session: str, **output) -> dict:
        return client.call("session.result", {"session": session, "output": output})["output"]

    def read_all(self, client: MachineClient, session: str, stream: str, max_bytes: int) -> tuple:
        """按协议说的办法重组：第一页是末尾，之后的页拼在前面。返回 (字节, 页数, 首页)。"""
        first = self.page(client, session, stream=stream, max_bytes=max_bytes)
        pages, whole, cursor = 1, first["tail"].encode("utf-8"), first["cursor"]
        self.assertLessEqual(len(first["tail"].encode("utf-8")), max_bytes)
        while cursor is not None:
            page = self.page(client, session, stream=stream, max_bytes=max_bytes, cursor=cursor)
            body = page["tail"].encode("utf-8")
            self.assertLessEqual(len(body), max_bytes, "一页不能超过 max_bytes")
            self.assertGreater(len(body), 0, "不能回空页原地打转")
            whole = body + whole
            cursor = page["cursor"]
            pages += 1
        return whole, pages, first

    # -- OUT-01 / OUT-07：几 MiB 的输出一个字节不少，两个流不混 --

    def test_a_multi_megabyte_output_comes_back_whole_and_the_streams_stay_apart(self):
        client = self.client()
        stdout = marked(3 * 1024 * 1024)
        stderr = b"P57-STDERR-MARKER\n" * 4000
        self.view("run big", "stdout", stdout)
        self.view("run big", "stderr", stderr)
        session = self.finished(client, "run big")

        whole, pages, first = self.read_all(client, session, "stdout", 65536)
        self.assertEqual(whole, stdout, "重组结果必须和保留视图逐字节一致")
        self.assertGreater(pages, 40)
        self.assertEqual(first["bytes_total"], len(stdout))
        self.assertEqual(first["source_bytes"], len(stdout))
        self.assertFalse(first["source_truncated"])
        self.assertTrue(first["truncated"])
        self.assertEqual(first["stream"], "stdout")
        self.assertIn(b"P57-EARLY-MARKER", whole)
        self.assertIn(b"P57-MIDDLE-MARKER", whole)

        errs, _, first_err = self.read_all(client, session, "stderr", 65536)
        self.assertEqual(errs, stderr)
        self.assertEqual(first_err["stream"], "stderr")
        self.assertNotIn(b"P57-EARLY-MARKER", errs)

        # 不给 output 参数的旧调用照旧：stdout 的最后 8 KiB。
        default = client.session_result(session)["output"]
        self.assertEqual(default["tail"].encode("utf-8"), stdout[-len(default["tail"].encode("utf-8")):])
        self.assertLessEqual(len(default["tail"].encode("utf-8")), 8192)
        self.assertEqual(default["bytes_total"], len(stdout))
        self.assertTrue(default["truncated"])

    # -- OUT-02：中文、Emoji、无换行长行、小预算 --

    def test_small_budgets_never_split_a_character_exceed_or_spin(self):
        client = self.client()
        text = ("没有换行的一长行" * 300 + "😀🙂" * 50 + "ascii tail").encode("utf-8")
        self.view("run cjk", "stdout", text)
        session = self.finished(client, "run cjk")
        for budget in (4, 5, 7, 64, 1000):
            with self.subTest(budget=budget):
                whole, _, _ = self.read_all(client, session, "stdout", budget)
                self.assertEqual(whole, text)
        # 1 字节装得下末尾的 ASCII，但一路往前总会遇到装不下的中文或 Emoji。
        for budget in (1, 2, 3):
            with self.subTest(budget=budget), self.assertRaises(RpcError) as caught:
                self.read_all(client, session, "stdout", budget)
            self.assertEqual(caught.exception.code, E_INVALID_PARAMS)
            self.assertEqual(caught.exception.data["reason"], "max_bytes_too_small")
            self.assertGreater(caught.exception.data["min_bytes"], budget)
            self.assertEqual(caught.exception.effect, "none")

    def test_an_empty_output_is_an_empty_view_not_a_missing_one(self):
        client = self.client()
        self.view("run empty", "stdout", b"")
        session = self.finished(client, "run empty")
        out = self.page(client, session)
        self.assertEqual(
            {k: out[k] for k in ("bytes_total", "truncated", "cursor", "tail")},
            {"bytes_total": 0, "truncated": False, "cursor": None, "tail": ""},
        )
        self.assertNotIn("unavailable_reason", out)

    # -- max_bytes 与游标的边界 --

    def test_max_bytes_is_validated_and_honoured(self):
        client = self.client()
        self.view("run budget", "stdout", b"x" * 20000)
        session = self.finished(client, "run budget")
        self.assertEqual(len(self.page(client, session, max_bytes=16)["tail"]), 16)
        for bad in (0, -1, "16", 1.5, None):
            with self.subTest(bad=bad), self.assertRaises(RpcError) as caught:
                self.page(client, session, max_bytes=bad)
            self.assertEqual(caught.exception.code, E_INVALID_PARAMS)
        with self.assertRaises(RpcError) as caught:
            self.page(client, session, stream="both")
        self.assertEqual(caught.exception.code, E_INVALID_PARAMS)

    def test_cursors_are_bound_to_their_session_and_to_this_server(self):
        client = self.client()
        self.view("run a", "stdout", b"a" * 5000)
        self.view("run b", "stdout", b"b" * 5000)
        a = self.finished(client, "run a")
        b = self.finished(client, "run b")
        first = self.page(client, a, max_bytes=1000)
        cursor = first["cursor"]
        again = self.page(client, a, max_bytes=1000, cursor=cursor)
        self.assertEqual(self.page(client, a, max_bytes=1000, cursor=cursor), again, "同一个游标重读同一页")
        for label, session, bad in (("other session", b, cursor), ("forged", a, "c-" + "0" * 32), ("garbage", a, "../x")):
            with self.subTest(label), self.assertRaises(RpcError) as caught:
                self.page(client, session, max_bytes=1000, cursor=bad)
            self.assertEqual(caught.exception.code, E_EXPIRED)
        with self.assertRaises(RpcError) as caught:
            self.page(client, a, max_bytes=1000, cursor=cursor, stream="stderr")
        self.assertEqual(caught.exception.code, E_EXPIRED, "游标属于 stdout，不能拿去翻 stderr")
        # 服务端换了一个进程：旧游标失效，从 null 重新开始仍然拿得到。
        fresh = self.client()
        with self.assertRaises(RpcError) as caught:
            self.page(fresh, a, max_bytes=1000, cursor=cursor)
        self.assertEqual(caught.exception.code, E_EXPIRED)
        restarted = self.page(fresh, a, max_bytes=1000)
        self.assertEqual((restarted["tail"], restarted["bytes_total"]), (first["tail"], first["bytes_total"]))
        self.assertIsNotNone(restarted["cursor"])

    # -- OUT-03：结果文档解析成功时，最终回答和原始 stdout 各自完整 --

    def test_a_parsed_result_keeps_the_final_text_and_the_raw_stdout_apart(self):
        client = self.client()
        answer = marked(20 * 1024).decode("utf-8")
        doc = json.loads((ROOT / "tests/fixtures/claude-print-2.1.260.json").read_text())
        doc["result"] = answer
        raw = json.dumps(doc, ensure_ascii=False).encode("utf-8")
        self.view("run parsed", "stdout", raw)
        session = self.finished(client, "run parsed", {"stdout_tail": "", "result": doc})
        result = client.session_result(session)
        self.assertEqual(result["text"], answer)
        whole, _, first = self.read_all(client, session, "stdout", 65536)
        self.assertEqual(whole, raw, "解析成功不等于没有原始 stdout")
        self.assertEqual(first["bytes_total"], len(raw))

    # -- OUT-06：拿不到完整内容时明说，不假装完整 --

    def test_an_unreachable_agent_gives_the_old_tail_and_says_so(self):
        client = self.client()
        data = marked(200 * 1024)
        self.view("run far", "stdout", data)
        session = self.finished(client, "run far", {"stdout_tail": "...the old two KiB tail"})
        self.output_mode("unreachable")
        out = self.page(client, session)
        self.assertEqual(out["unavailable_reason"], "agent_unreachable")
        self.assertEqual(out["tail"], "...the old two KiB tail")
        self.assertNotIn("source_truncated", out, "不知道源头多大，就不说没截断")
        # Agent 回来了：这次拿到的是完整的。
        self.output_mode("serve")
        out = self.page(client, session)
        self.assertNotIn("unavailable_reason", out)
        self.assertEqual(out["bytes_total"], len(data))

    def test_an_agent_that_refuses_the_request_is_named_not_hidden(self):
        client = self.client()
        session = self.finished(client, "run old agent", {"stdout_tail": "tail kept by the rpc record"})
        self.output_mode("unknown-command")
        out = self.page(client, session)
        self.assertEqual(out["unavailable_reason"], "agent_refused")
        self.assertEqual(out["tail"], "tail kept by the rpc record")

    def test_a_running_session_has_no_output_yet(self):
        client = self.client()
        session = self.start(client, "run still going")["session"]
        self.wait_started("run still going")
        self.assertNotIn("output", client.session_result(session))
        self.release("run still going")
        self.settle(client, session)
        time.sleep(0.1)

    # -- P59.4：两份参考客户端 --

    def test_the_reference_client_reads_everything_or_stops_at_its_cap(self):
        client = self.client()
        stdout = marked(3 * 1024 * 1024)
        self.view("run client", "stdout", stdout)
        self.view("run client", "stderr", b"only on stderr\n")
        session = self.finished(client, "run client")
        whole = client.read_output(session)
        self.assertTrue(whole["complete"])
        self.assertEqual(whole["data"], stdout)
        self.assertEqual(whole["bytes_total"], len(stdout))
        self.assertIs(whole["source_truncated"], False)
        self.assertIsNone(whole["unavailable_reason"])
        capped = client.read_output(session, page_bytes=40000, limit_bytes=100000)
        self.assertFalse(capped["complete"], "到上限就停，不假装读完")
        self.assertLessEqual(len(capped["data"]), 100000)
        self.assertEqual(capped["data"], stdout[-len(capped["data"]):])
        self.assertEqual(client.read_output(session, stream="stderr")["data"], b"only on stderr\n")
        with self.assertRaises(ValueError):
            client.read_output(session, page_bytes=3)

    def test_the_execution_backend_reads_output_through_its_own_method(self):
        sys.path.insert(0, str(ROOT / "clients/python"))
        from execution_backend import CcnmBackend  # noqa: E402

        client = self.client()
        stdout = marked(300 * 1024)
        self.view("run backend", "stdout", stdout)
        session = self.finished(client, "run backend")
        backend = CcnmBackend.spawn(ccnm=str(BINARY), config=str(self.config), env=self.env)
        self.addCleanup(backend.close)
        out = backend.output(session)
        self.assertEqual(out.data, stdout)
        self.assertTrue(out.complete)
        self.assertIs(out.lost, False)
        self.assertIsNone(out.unavailable)
        self.assertEqual(backend.result(session).state, "completed")


if __name__ == "__main__":
    unittest.main()
