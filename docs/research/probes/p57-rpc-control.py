"""P57：Machine API 会话控制缺口（CTRL-01/02/03）的零额度复现。

被测：`cargo build` 出来的真实 `ccnm rpc` 与 `ccnm internal agent-stop`。
假的只有对面机器：假 ssh 冒充 Agent Node，用文件屏障决定每次运行何时结束；
假 tmux 冒充 Agent 上的 tmux。全部在 /tmp/p57-* 里，跑完删除。

    cargo build && python3 -B docs/research/probes/p57-rpc-control.py

输出一份 JSON：每项的 verdict 是 reproduced（按计划描述复现）/ conforms（行为
符合契约，计划担心的没发生）/ observed（只记录现象）。探针跑通不代表产品通过，
故意失败的断言不进 CI；P58 修复时把对应项改写成正式回归。
压力项 A4 依赖调度，次数用 P57_STRESS 调（默认 40），结论只按实际计数写。
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
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
    write_runtime_config,
)


def verdict(flag: bool, yes: str = "reproduced", no: str = "conforms") -> str:
    return yes if flag else no


def a1_stop_carries_no_session_identity() -> dict:
    """CTRL-01 / AUTH-01：同 workspace 两个在途启动，停 B 时 Agent 收到的请求能否区分 A、B。"""
    sb = runtime_sandbox("a1")
    peer = RpcPeer(sb)
    try:
        a = start(peer, "p57 A1 run-A")["result"]
        wait_until(lambda: sb.started("p57 A1 run-A"))
        b = start(peer, "p57 A1 run-B")
        wait_until(lambda: sb.started("p57 A1 run-B"))
        hb = b["result"]["session"]
        stop_b = peer.call("session.stop", {"session": hb})
        stop_a = peer.call("session.stop", {"session": a["session"]})
        stops = sb.calls_of("agent-stop")
        sb.release_all()
        final_a, final_b = settle(peer, a["session"]), settle(peer, hb)
        same = len(stops) == 2 and stops[0]["request"] == stops[1]["request"]
        return {
            "id": "A1",
            "gap": ["CTRL-01", "AUTH-01"],
            "verdict": verdict(same and "session" not in stops[0]["request"]),
            "second_start_while_first_running": {"state": b["result"]["state"], "busy_returned": "error" in b},
            "agent_run_calls": len(sb.calls_of("agent-run")),
            "stop_b_response": stop_b.get("result", stop_b.get("error")),
            "agent_stop_request_for_B": stops[0]["request"] if stops else None,
            "agent_stop_request_for_A": stops[1]["request"] if len(stops) > 1 else None,
            "requests_identical": same,
            "final_states": [final_a["state"], final_b["state"]],
            "note": "停 B 发给 Agent 的请求里没有任何 B 的身份，和停 A 的请求逐字节相同；Agent 侧据此停哪一个见 B1/B2",
        }
    finally:
        sb.release_all()
        peer.close()
        sb.cleanup()


def a2_stop_flag_lost_when_run_finishes() -> dict:
    """CTRL-02：stop 先落盘，运行线程随后用旧副本写终态。"""
    sb = runtime_sandbox("a2")
    peer = RpcPeer(sb)
    try:
        h = start(peer, "p57 A2 run")["result"]["session"]
        wait_until(lambda: sb.started("p57 A2 run"))
        stopped = peer.call("session.stop", {"session": h})["result"]
        during = peer.call("session.status", {"session": h})["result"]
        sb.release("p57 A2 run")
        after = settle(peer, h)
        result = peer.call("session.result", {"session": h})["result"]
        lost = during["stop_requested"] is True and after["stop_requested"] is False
        return {
            "id": "A2",
            "gap": ["CTRL-02"],
            "verdict": verdict(lost),
            "stop_response": stopped,
            "status_after_stop": {k: during[k] for k in ("state", "stop_requested")},
            "status_after_run_ended": {k: after[k] for k in ("state", "stop_requested")},
            "result_outcome": result.get("outcome"),
            "record_on_disk": {k: sb.record(h).get(k) for k in ("state", "stop_requested")},
        }
    finally:
        sb.release_all()
        peer.close()
        sb.cleanup()


def a3_terminal_state_overwritten_by_stop() -> dict:
    """CTRL-02：运行先写完 completed，stop 再用自己读到的旧副本写 stopping。"""
    sb = runtime_sandbox("a3")
    peer = RpcPeer(sb)
    try:
        h = start(peer, "p57 A3 run")["result"]["session"]
        wait_until(lambda: sb.started("p57 A3 run"))
        record_path = sb.state / "rpc/sessions" / f"{h}.json"
        sb.stop_mode({"kind": "release-and-wait-final", "prompt": "p57 A3 run", "record": str(record_path)})
        stopped = peer.call("session.stop", {"session": h})
        right_after = peer.call("session.status", {"session": h})["result"]
        time.sleep(1.0)
        later = peer.call("session.status", {"session": h})["result"]
        result = peer.call("session.result", {"session": h})["result"]
        record = sb.record(h)
        peer.close()
        # 服务端退出后换一个进程来问：owner 不在了，非终态读成 unknown。
        again = RpcPeer(sb)
        after_restart = again.call("session.status", {"session": h})["result"]
        again.close()
        overwritten = right_after["state"] == "stopping" and "finish" not in record
        return {
            "id": "A3",
            "gap": ["CTRL-02"],
            "verdict": verdict(overwritten),
            "stop_response": stopped.get("result", stopped.get("error")),
            "status_right_after_stop": right_after["state"],
            "status_1s_later": later["state"],
            "result_has_outcome": "outcome" in result,
            "record_has_finish": "finish" in record,
            "status_after_server_restart": after_restart["state"],
            "note": "Agent 侧已经跑完并回报 completed，记录却被 stopping 覆盖、finish 丢失；服务端退出后只剩 unknown",
        }
    finally:
        sb.release_all()
        sb.cleanup()


def a4_cross_process_stop_vs_finish_stress() -> dict:
    """CTRL-02 压力项：进程 1 的运行线程写终态，进程 2 同时写 stopping。依赖调度，只记计数。"""
    rounds = int(os.environ.get("P57_STRESS", "40"))
    sb = runtime_sandbox("a4")
    owner = RpcPeer(sb)
    other = RpcPeer(sb)
    counts: dict = {}
    stop_errors = []
    try:
        for i in range(rounds):
            prompt = f"p57 A4 run-{i}"
            h = start(owner, prompt)["result"]["session"]
            wait_until(lambda p=prompt: sb.started(p))
            sb.stop_mode({"kind": "release", "prompt": prompt})
            stopped = other.call("session.stop", {"session": h})
            if "error" in stopped:
                stop_errors.append(stopped["error"])
            # 运行线程在假 Agent 回话后几毫秒内写终态；等 0.3 秒只为分类，不作结论依据。
            time.sleep(0.3)
            record = sb.record(h)
            key = f"{record['state']}/stop_requested={record['stop_requested']}/finish={'finish' in record}"
            counts[key] = counts.get(key, 0) + 1
        tmp_left = sorted(p.name for p in (sb.state / "rpc/sessions").glob("*.tmp"))
        logs = owner.stderr() + other.stderr()
        return {
            "id": "A4",
            "gap": ["CTRL-02"],
            "verdict": "observed",
            "rounds": rounds,
            "final_record_kinds": counts,
            "stop_errors": stop_errors[:5],
            "tmp_files_left": tmp_left,
            "server_log_write_failures": [line for line in logs.splitlines() if "cannot" in line][:5],
            "note": "只有终态为 completed 且 stop_requested=true 的组合才同时保住两个事实；其余组合各丢一个",
        }
    finally:
        sb.release_all()
        owner.close()
        other.close()
        sb.cleanup()


def a5_start_key_lossy_mapping() -> dict:
    """CTRL-03 / CT-04：协议允许 1..128 字符的任意串，存储用 safe_name 丢字符、截 64。"""
    sb = runtime_sandbox("a5")
    peer = RpcPeer(sb)
    cases = [("任务-一", "任务-二"), ("a/b", "ab"), ("k" * 64 + "x", "k" * 64 + "y")]
    rows = []
    try:
        for n, (first, second) in enumerate(cases):
            same, other = f"p57 A5 same-{n}", f"p57 A5 other-{n}"
            s1 = start(peer, same, key=first)["result"]["session"]
            different_input = start(peer, other, key=second)
            same_input = start(peer, same, key=second)
            rows.append({
                "key_1": first,
                "key_2": second,
                "key_2_other_input": different_input.get("error", {}).get("code", "new session"),
                "key_2_same_input_reused_session_of_key_1": same_input.get("result", {}).get("session") == s1,
            })
        keys = sorted(p.name for p in (sb.state / "rpc/keys/demo").iterdir())
        # 九次 start 里只有三次是新会话；等这三次都到了假 Agent，再多等半秒确认没有第四次。
        wait_until(lambda: len(sb.calls_of("agent-run")) >= len(cases), timeout=10)
        time.sleep(0.5)
        merged = all(r["key_2_other_input"] == -32010 and r["key_2_same_input_reused_session_of_key_1"] for r in rows)
        return {
            "id": "A5",
            "gap": ["CTRL-03"],
            "verdict": verdict(merged),
            "cases": rows,
            "key_files_on_disk": keys,
            "agent_runs_started": len(sb.calls_of("agent-run")),
            "note": "三组不同的键各只落成一个文件；第二个键被当成第一个：不同输入报 conflict，相同输入复用了别人的 session",
        }
    finally:
        sb.release_all()
        peer.close()
        sb.cleanup()


def planted_record(session: str, text: str) -> dict:
    return {
        "session": session,
        "workspace": "demo",
        "agent": {"node": "worker", "instance": "claude-main"},
        "mode": "print",
        "prompt": "planted",
        "state": "completed",
        "accepted_at": "2026-09-28T00:00:00Z",
        "stop_requested": False,
        "owner_pid": 1,
        "owner_started": "x",
        "finish": {"exit_code": 0, "timed_out": False, "duration_ms": 1, "text": text, "output": "", "output_total": 0},
    }


def a6_session_handle_is_joined_into_a_path() -> dict:
    """CTRL-03 / CT-06：session 参数直接拼进 sessions/<id>.json。"""
    sb = runtime_sandbox("a6")
    peer = RpcPeer(sb)
    try:
        sessions = sb.state / "rpc/sessions"
        sessions.mkdir(parents=True, exist_ok=True)
        (sb.state / "rpc/p57-outside.json").write_text(json.dumps(planted_record("p57-outside", "PLANTED-PARENT")))
        absolute = sb.dir / "p57-absolute"
        absolute.with_suffix(".json").write_text(json.dumps(planted_record("p57-absolute", "PLANTED-ABSOLUTE")))
        (sessions / "s-link.json").symlink_to(absolute.with_suffix(".json"))
        rows = {}
        for label, handle in (("parent", "../p57-outside"), ("absolute", str(absolute)), ("symlink", "s-link")):
            answer = peer.call("session.result", {"session": handle})
            rows[label] = {
                "handle": handle,
                "matches_schema_pattern": bool(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", handle)),
                "answer": answer.get("result", {}).get("text") if "result" in answer else answer.get("error"),
            }
        escaped = rows["parent"]["answer"] == "PLANTED-PARENT" and rows["absolute"]["answer"] == "PLANTED-ABSOLUTE"
        return {
            "id": "A6",
            "gap": ["CTRL-03"],
            "verdict": verdict(escaped),
            "cases": rows,
            "note": "不合协议 session_id 模式的句柄没有被拒，读到了 sessions/ 以外、乃至绝对路径处的记录；symlink 也被跟随",
        }
    finally:
        peer.close()
        sb.cleanup()


def a7_old_handle_follows_edited_config() -> dict:
    """CTRL-03 / CT-07：运行中把 workspace 改绑到另一台 Agent，旧句柄的 stop 发往哪里。"""
    sb = runtime_sandbox("a7")
    peer = RpcPeer(sb)
    try:
        h = start(peer, "p57 A7 run")["result"]["session"]
        wait_until(lambda: sb.started("p57 A7 run"))
        write_runtime_config(sb, node="worker2")
        stopped = peer.call("session.stop", {"session": h})
        status = peer.call("session.status", {"session": h})["result"]
        run_alias = sb.calls_of("agent-run")[0]["alias"]
        stop_call = sb.calls_of("agent-stop")[0]
        # 再删掉 workspace：旧句柄还能不能停。
        sb.config.write_text('this = "runtime"\n[nodes.runtime]\n[nodes.worker]\nssh = "worker-alias"\n')
        stop_removed = peer.call("session.stop", {"session": h})
        redirected = stop_call["alias"] != run_alias
        return {
            "id": "A7",
            "gap": ["CTRL-03"],
            "verdict": verdict(redirected),
            "run_went_to": run_alias,
            "stop_went_to": stop_call["alias"],
            "stop_request_agent": stop_call["request"].get("agent"),
            "record_still_says": status["agent"],
            "stop_response": stopped.get("result", stopped.get("error")),
            "stop_after_workspace_removed": stop_removed.get("result", stop_removed.get("error")),
        }
    finally:
        sb.release_all()
        peer.close()
        sb.cleanup()


def a8_key_and_record_boundaries() -> dict:
    """CT-05：键文件建了但没写完、键指向不存在的记录、记录 JSON 半截。"""
    sb = runtime_sandbox("a8")
    peer = RpcPeer(sb)
    try:
        keys = sb.state / "rpc/keys/demo"
        keys.mkdir(parents=True, exist_ok=True)
        (sb.state / "rpc/sessions").mkdir(parents=True, exist_ok=True)
        (keys / "k-empty").write_text("")
        (keys / "k-dangling").write_text("s-00000000-0000-0000-0000-000000000000")
        (sb.state / "rpc/sessions/s-truncated.json").write_text('{"session": "s-truncated", "worksp')
        empty = start(peer, "p57 A8 empty", key="k-empty")
        dangling = start(peer, "p57 A8 dangling", key="k-dangling")
        truncated = peer.call("session.status", {"session": "s-truncated"})
        stop_truncated = peer.call("session.stop", {"session": "s-truncated"})
        return {
            "id": "A8",
            "gap": ["CTRL-03"],
            "verdict": "observed",
            "empty_key_file": empty.get("error", empty.get("result")),
            "dangling_key_file": dangling.get("error", dangling.get("result")),
            "truncated_record_status": truncated.get("error", truncated.get("result")),
            "truncated_record_stop": stop_truncated.get("error", stop_truncated.get("result")),
            "agent_runs_started": len(sb.calls_of("agent-run")),
        }
    finally:
        sb.release_all()
        peer.close()
        sb.cleanup()


# -- Agent 侧：RPC 发出的"无 session 的 stop"在 Agent 上落到哪 ------------------

AGENT_IDENTITY = {"node": "worker", "instance": "claude-main", "provider": "claude", "profile_ref": "default"}


def agent_session(state: Path, mode: dict) -> str:
    sid = str(uuid.uuid4())
    d = state / "sessions" / sid
    d.mkdir(parents=True)
    spec = {
        "protocol": 3,
        "agent_identity": AGENT_IDENTITY,
        "id": sid,
        "workspace": "demo",
        "root": "/srv/p57/demo",
        "runtime_node": "runtime",
        "runtime": {"alias": "runtime-alias", "ccnm_bin": "ccnm"},
        "claude_config_dir": None,
        "permission_mode": "acceptEdits",
        "mode": mode,
        "timeout_secs": 900,
        "cwd": str(state / "workspaces/demo"),
    }
    (d / "session.json").write_text(json.dumps(spec))
    return sid


def agent_stop(sb: Sandbox, request: dict, tmux_script: dict) -> dict:
    (sb.fake / "tmux-script.json").write_text(json.dumps(tmux_script))
    for stale in sb.fake.glob("tmux-count-*"):
        stale.unlink()
    log = sb.fake / "tmux.jsonl"
    if log.exists():
        log.unlink()
    out = subprocess.run(
        [str(ccnm_binary()), "internal", "agent-stop", "--payload", b64(request)],
        env=sb.env(CCNM_CONFIG=str(sb.config)),
        capture_output=True,
        text=True,
        timeout=60,
    )
    calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
    return {"exit": out.returncode, "stdout": out.stdout.strip(), "stderr": out.stderr.strip()[-400:], "tmux_calls": calls}


def alive(pid: int) -> bool:
    state = subprocess.run(["/bin/ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return bool(state) and not state.startswith("Z")


def b_agent_side_stop_selection(rpc_request: dict | None) -> dict:
    """CTRL-01 另一半：Agent 用 work::stop 处理 RPC 发来的 stop（有 agent、无 session）。"""
    sb = Sandbox("b")
    shutil.copy(ROOT / "tests/fixtures/agent-instance/agent.toml", sb.config)
    sb.install("tmux")
    request = rpc_request or {"protocol": 3, "workspace": "demo", "agent": {"node": "worker", "instance": "claude-main"}}
    # 冒充一个正在跑的 print 会话的 supervisor：只是 sleep，探针结束就收掉。
    dummy = subprocess.Popen(["/bin/sleep", "300"])
    try:
        printed = agent_session(sb.state, {"mode": "print", "prompt": "p57 B print"})
        (sb.state / "sessions" / printed / "supervisor.pid").write_text(f"{dummy.pid}\n")
        interactive = agent_session(sb.state, {"mode": "interactive", "prompt": None})

        only_print = agent_stop(sb, request, {"has-session": [{"exit": 1, "stderr": "can't find session: ccnm-demo\n"}]})
        b1 = {
            "agent_stop": only_print,
            "print_supervisor_still_running": alive(dummy.pid),
            "print_session_marked_stopping": (sb.state / "sessions" / printed / "stopping").exists(),
        }

        with_interactive = agent_stop(sb, request, {
            "has-session": [{"exit": 0}, {"exit": 1}],
            "show-environment": [{"exit": 0, "stdout": f"CCNM_SESSION={interactive}\n"}],
            "kill-session": [{"exit": 0}],
        })
        b2 = {
            "agent_stop": with_interactive,
            "kill_session_issued": any("kill-session" in c for c in with_interactive["tmux_calls"]),
            "interactive_session_marked_stopping": (sb.state / "sessions" / interactive / "stopping").exists(),
            "print_supervisor_still_running": alive(dummy.pid),
        }

        exact = agent_stop(sb, {**request, "session": printed}, {"has-session": [{"exit": 1}]})
        b3 = {"agent_stop": exact, "print_supervisor_still_running": alive(dummy.pid)}

        missed = only_print["exit"] == 0 and '"killed":false' in only_print["stdout"].replace(" ", "") and b1["print_supervisor_still_running"]
        return {
            "id": "B",
            "gap": ["CTRL-01"],
            "verdict": verdict(missed and b2["kill_session_issued"]),
            "request_sent_by_rpc_stop": request,
            "B1_only_a_print_run": b1,
            "B2_an_interactive_session_also_exists": b2,
            "B3_same_request_with_exact_session": b3,
            "note": "print 运行不在 tmux 里；无 session 的 stop 只看 tmux，所以停不到它（B1），同 workspace 有交互会话时改停那个（B2）。B3 是带精确 id 的对照：走 supervisor 校验路径",
        }
    finally:
        dummy.kill()
        dummy.wait()
        sb.cleanup()


def main() -> None:
    report = {"binary": str(ccnm_binary()), "results": []}
    a1 = a1_stop_carries_no_session_identity()
    report["results"].append(a1)
    for probe in (
        a2_stop_flag_lost_when_run_finishes,
        a3_terminal_state_overwritten_by_stop,
        a4_cross_process_stop_vs_finish_stress,
        a5_start_key_lossy_mapping,
        a6_session_handle_is_joined_into_a_path,
        a7_old_handle_follows_edited_config,
        a8_key_and_record_boundaries,
    ):
        report["results"].append(probe())
    report["results"].append(b_agent_side_stop_selection(a1.get("agent_stop_request_for_B")))
    leftovers = sorted(str(p) for p in Path("/tmp").glob("p57-*"))
    report["tmp_dirs_left"] = leftovers
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
