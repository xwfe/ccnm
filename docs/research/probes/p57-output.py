"""P57：结果通道缺口（OUT-01/02）的零额度复现。

两段各用真实二进制：
- Agent 段：真实 `ccnm internal agent-result` 读一个手工造的 print 会话目录，
  stdout 是 3 MiB（头、中、尾各一个标记），看 RunReport/ResultReport 带回多少。
  agent-run 与 agent-result 用的是 work.rs 里同一个 `tail()`。
- RPC 段：真实 `ccnm rpc` + 假 Agent，把上一段真实得到的尾巴原样当 RunReport
  交回去，再加 20 KiB 尾巴、解析成功的结果文档、stderr、max_bytes、cursor。

    cargo build && python3 -B docs/research/probes/p57-output.py
"""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from p57_common import (  # noqa: E402
    ROOT,
    RpcPeer,
    Sandbox,
    b64,
    ccnm_binary,
    runtime_sandbox,
    settle,
    start,
    wait_until,
)

MIB = 1024 * 1024


def marked(total: int) -> bytes:
    """确定的大输出：头、正中、尾各一个标记，中间是编号行；尾部带中文。"""
    head = "P57-EARLY-MARKER\n"
    tail = "P57-LATE-MARKER 结束了，中文收尾。\n"
    mid = "P57-MIDDLE-MARKER\n"
    line = "line {:07d} 这是一行确定的填充文本 abcdefghijklmnopqrstuvwxyz\n"
    body, n = [head], 0
    size = len(head.encode())
    half_done = False
    while size < total - len(tail.encode()):
        if not half_done and size >= total // 2:
            body.append(mid)
            size += len(mid.encode())
            half_done = True
            continue
        text = line.format(n)
        body.append(text)
        size += len(text.encode())
        n += 1
    body.append(tail)
    return "".join(body).encode("utf-8")


def markers(text: str) -> dict:
    return {m: (m in text) for m in ("P57-EARLY-MARKER", "P57-MIDDLE-MARKER", "P57-LATE-MARKER")}


def legacy_print_session(state: Path, stdout: bytes, stderr: bytes) -> str:
    sid = str(uuid.uuid4())
    d = state / "sessions" / sid
    d.mkdir(parents=True)
    spec = {
        "protocol": 1,
        "id": sid,
        "workspace": "demo",
        "root": "/srv/p57/demo",
        "runtime": None,
        "claude_config_dir": None,
        "permission_mode": "acceptEdits",
        "mode": {"mode": "print", "prompt": "p57 output"},
        "timeout_secs": 900,
        "cwd": str(state / "workspaces/demo"),
    }
    (d / "session.json").write_text(json.dumps(spec))
    (d / "stdout").write_bytes(stdout)
    (d / "stderr").write_bytes(stderr)
    (d / "exit").write_text(json.dumps({"exit_code": 0, "timed_out": False, "duration_ms": 5, "error": None}))
    return sid


def agent_result(sb: Sandbox, sid: str) -> dict:
    out = subprocess.run(
        [str(ccnm_binary()), "internal", "agent-result", "--payload",
         b64({"protocol": 1, "workspace": "demo", "session": sid})],
        env=sb.env(), capture_output=True, text=True, timeout=60,
    )
    if out.returncode != 0:
        raise RuntimeError(f"agent-result exit {out.returncode}: {out.stderr[-400:]}")
    return json.loads(out.stdout)


def claude_result_doc(text: str) -> dict:
    doc = json.loads((ROOT / "tests/fixtures/claude-print-2.1.260.json").read_text())
    doc["result"] = text
    return doc


def c0_agent_side_tail() -> dict:
    sb = Sandbox("c0")
    try:
        raw = marked(3 * MIB)
        sid = legacy_print_session(sb.state, raw, b"P57-STDERR-MARKER\n" * 200)
        report = agent_result(sb, sid)
        tail = report["stdout_tail"]

        final_text = marked(20 * 1024).decode()
        doc = json.dumps(claude_result_doc(final_text)).encode()
        sid_parsed = legacy_print_session(sb.state, doc, b"")
        parsed = agent_result(sb, sid_parsed)
        return {
            "id": "C0",
            "gap": ["OUT-01", "OUT-02"],
            "verdict": "reproduced" if len(tail.encode()) <= 2051 and not markers(tail)["P57-EARLY-MARKER"] else "conforms",
            "unparsed_stdout_bytes_on_agent": len(raw),
            "stdout_tail_bytes": len(tail.encode()),
            "stdout_tail_starts_with": tail[:12],
            "stdout_tail_markers": markers(tail),
            "stderr_tail_bytes": len(report["stderr_tail"].encode()),
            "parsed_result_final_text_bytes": len((parsed.get("result") or {}).get("result", "").encode()),
            "parsed_result_final_text_markers": markers((parsed.get("result") or {}).get("result", "")),
            "note": "Agent 只带回末尾 2 KiB（前缀 ...）；3 MiB 的头和中间在 RunReport 里就没了。解析成功时最终回答是完整的另一字段",
            "_tail_for_rpc": tail,
            "_raw_len": len(raw),
        }
    finally:
        sb.cleanup()


def c_rpc_side(agent_tail: str, raw_len: int) -> list:
    sb = runtime_sandbox("c")
    peer = RpcPeer(sb)
    rows = []
    try:
        def run(prompt: str, reply: dict) -> tuple:
            sb.reply_for(prompt, reply)
            h = start(peer, prompt)["result"]["session"]
            wait_until(lambda: sb.started(prompt))
            sb.release(prompt)
            settle(peer, h)
            return h, peer.call("session.result", {"session": h})["result"]

        # C1：Agent 真实给出的 2 KiB 尾巴（原始 stdout 3 MiB）。
        _, r1 = run("p57 C1", {"stdout_tail": agent_tail})
        out1 = r1["output"]
        rows.append({
            "id": "C1",
            "gap": ["OUT-01"],
            "verdict": "reproduced" if out1["truncated"] is False else "conforms",
            "original_stdout_bytes_on_agent": raw_len,
            "rpc_output": {k: out1[k] for k in ("bytes_total", "truncated", "cursor")},
            "tail_markers": markers(out1["tail"]),
            "note": "bytes_total 是那段尾巴自己的长度，truncated=false；调用方看不出原始输出有 3 MiB",
        })

        # C2：尾巴本身超过 8 KiB 时 RPC 再截一次；尾部落在中文字符中间。
        big = marked(20 * 1024).decode()
        _, r2 = run("p57 C2", {"stdout_tail": big})
        out2 = r2["output"]
        rows.append({
            "id": "C2",
            "gap": ["OUT-01"],
            "verdict": "reproduced" if out2["truncated"] and out2["cursor"] is None else "conforms",
            "rpc_output": {k: out2[k] for k in ("bytes_total", "truncated", "cursor")},
            "tail_bytes": len(out2["tail"].encode()),
            "tail_markers": markers(out2["tail"]),
            "tail_is_suffix_of_input": big.endswith(out2["tail"]),
            "note": "truncated=true 但 cursor=null：没有第二页可取，早先的字节已不可恢复",
        })

        # C3：结果文档解析成功（stdout_tail 为空），最终回答 20 KiB。
        final_text = marked(20 * 1024).decode()
        _, r3 = run("p57 C3", {"stdout_tail": "", "result": claude_result_doc(final_text)})
        rows.append({
            "id": "C3",
            "gap": ["OUT-02"],
            "verdict": "observed",
            "text_bytes": len((r3.get("text") or "").encode()),
            "text_markers": markers(r3.get("text") or ""),
            "rpc_output": {k: r3["output"][k] for k in ("bytes_total", "truncated", "cursor")},
            "note": "最终回答完整走 text 字段、没有上限；output 是空的——不能把空 output 读成没有输出",
        })

        # C4：max_bytes 与 cursor。
        h, _ = run("p57 C4", {"stdout_tail": big})
        asks = {}
        for label, output in (
            ("max_bytes_16", {"max_bytes": 16}),
            ("max_bytes_zero", {"max_bytes": 0}),
            ("max_bytes_negative", {"max_bytes": -5}),
            ("max_bytes_string", {"max_bytes": "abc"}),
            ("cursor_non_null", {"cursor": "c-8192"}),
        ):
            answer = peer.call("session.result", {"session": h, "output": output})
            asks[label] = (
                {"tail_bytes": len(answer["result"]["output"]["tail"].encode())}
                if "result" in answer else {"error": answer["error"]["code"], "reason": answer["error"].get("data", {}).get("reason")}
            )
        ignored = asks["max_bytes_16"].get("tail_bytes", 0) > 16
        rows.append({
            "id": "C4",
            "gap": ["OUT-01"],
            "verdict": "reproduced" if ignored else "conforms",
            "answers": asks,
            "note": "契约写服务端给得可以更少、不会更多；max_bytes=16 仍回 8 KiB，非法值（0、负数、字符串）也被接受",
        })

        # C5：stderr 有内容时，RPC 结果里哪里都找不到。
        h5, r5 = run("p57 C5", {"stdout_tail": "stdout-ok\n", "stderr_tail": "P57-STDERR-MARKER\n"})
        rows.append({
            "id": "C5",
            "gap": ["OUT-02"],
            "verdict": "reproduced" if "P57-STDERR-MARKER" not in json.dumps(r5) else "conforms",
            "stderr_marker_anywhere_in_result": "P57-STDERR-MARKER" in json.dumps(r5),
            "stderr_marker_in_record_on_disk": "P57-STDERR-MARKER" in json.dumps(sb.record(h5)),
        })
        return rows
    finally:
        sb.release_all()
        peer.close()
        sb.cleanup()


def main() -> None:
    c0 = c0_agent_side_tail()
    tail, raw_len = c0.pop("_tail_for_rpc"), c0.pop("_raw_len")
    report = {"binary": str(ccnm_binary()), "results": [c0, *c_rpc_side(tail, raw_len)]}
    report["tmp_dirs_left"] = sorted(str(p) for p in Path("/tmp").glob("p57-*"))
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
