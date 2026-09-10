#!/usr/bin/env python3
"""一个假的 `ccnm`，用来离线走通对照工具的**成功**路径。

`.invalid` 的沙盒配置只能让两条腿都失败，那证明不了 `scripts/p7_parity_check.py`
读得懂一次成功的执行。而真机那一次最危险的恰恰是成功路径出错：额度花了，
结论却是错的。

它同时扮演两个入口，因为对照工具会拿同一个二进制跑两次：

    fake_ccnm.py [--config X] run <ws> [--agent I] --print <PROMPT> --timeout N
    fake_ccnm.py [--config X] rpc

副作用是真做的：从提示词里取出路径和 token，把文件写出来。这样对照工具的
产物检查、属主比对和清理都在真实文件上跑一遍。

环境变量控制它怎么"坏"：

    FAKE_CCNM_PROVIDER=codex     Agent 自报的 provider，默认 claude
    FAKE_CCNM_SKIP_ARTIFACT=cli  这条腿只嘴上说完成，不写文件（cli/api/both）
    FAKE_CCNM_LEAK=1             往响应里塞一个凭据名，验证泄漏检查确实在查
"""

import json
import os
import sys
from pathlib import Path

PATH_MARK = "Create a file at exactly this path: "
TOKEN_MARK = "Its entire content must be exactly this line, nothing else: "


def side_effect(prompt: str, leg: str) -> None:
    """按提示词的字面要求写文件，除非这条腿被要求偷懒。"""
    skip = os.environ.get("FAKE_CCNM_SKIP_ARTIFACT", "")
    if skip in (leg, "both"):
        return
    path = token = None
    for line in prompt.splitlines():
        if line.startswith(PATH_MARK):
            path = Path(line[len(PATH_MARK):].strip())
        elif line.startswith(TOKEN_MARK):
            token = line[len(TOKEN_MARK):].strip()
    if path and token:
        path.write_text(token + "\n", encoding="utf-8")


def answer(message: dict) -> None:
    sys.stdout.write(json.dumps(message, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def serve_rpc() -> int:
    session = "s-fake-0001"
    finished = False
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        request = json.loads(line)
        method, params = request.get("method"), request.get("params") or {}

        if method == "hello":
            answer({"jsonrpc": "2.0", "id": request["id"], "result": {
                "protocol": "ccnm.machine/1",
                "server": {"name": "fake-ccnm", "version": "0.0.0"},
                "capabilities": {"modes": ["print"], "session_output": True, "start_key": True},
            }})
        elif method == "agents.list":
            answer({"jsonrpc": "2.0", "id": request["id"], "result": {
                "agents": [{"node": "worker", "instance": "claude-main", "workspaces": ["demo"]}],
            }})
        elif method == "session.start":
            side_effect((params.get("input") or {}).get("prompt", ""), "api")
            finished = True
            answer({"jsonrpc": "2.0", "id": request["id"], "result": {
                "session": session, "state": "starting", "reused": False,
                "workspace": params.get("workspace"),
                "agent": {"node": "worker", "instance": params.get("agent", {}).get("instance", "claude-main")},
                "accepted_at": "2026-09-10T00:00:00Z",
            }})
        elif method in ("session.status", "session.result"):
            state = "completed" if finished else "running"
            result = {
                "session": session, "state": state, "workspace": "demo",
                "agent": {
                    "node": "worker", "instance": "claude-main",
                    "provider": os.environ.get("FAKE_CCNM_PROVIDER", "claude"),
                },
                "started_at": "2026-09-10T00:00:00Z", "stop_requested": False,
            }
            if method == "session.result" and finished:
                result["outcome"] = {"exit_code": 0, "timed_out": False,
                                     "duration_ms": 1234, "stop_requested": False}
                result["text"] = "DONE"
                if os.environ.get("FAKE_CCNM_LEAK"):
                    # 一个真服务端绝不该回的字段，用来确认泄漏检查不是摆设。
                    result["debug_path"] = "/somewhere/.codex/auth.json"
            answer({"jsonrpc": "2.0", "id": request["id"], "result": result})
        else:
            answer({"jsonrpc": "2.0", "id": request["id"], "error": {
                "code": -32601, "message": "method not found", "data": {"effect": "none"},
            }})
    return 0


def main() -> int:
    argv = sys.argv[1:]
    if argv[:1] == ["--config"]:
        argv = argv[2:]
    if not argv:
        return 2
    if argv[0] == "rpc":
        return serve_rpc()
    if argv[0] == "run":
        prompt = argv[argv.index("--print") + 1] if "--print" in argv else ""
        side_effect(prompt, "cli")
        print("DONE")
        return 0
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
