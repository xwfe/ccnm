"""P57：`workspace remove --purge` 在本机删的是谁的目录（CLEAN-01）。

真实 `ccnm workspace remove demo --purge` + 假 Agent（回一份 PurgeReport）。
Operator 和 Runtime Executor 用两个不同的 XDG_STATE_HOME 模拟分离身份：
**这是路径路由测试，不是不同 UID 的真机授权证明**——两个目录同属当前用户，
权限隔离在这里测不到，留给 P62 在 hpsrv/ccrun 上验。

    cargo build && python3 -B docs/research/probes/p57-purge-routing.py
"""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from p57_common import ccnm_binary, runtime_sandbox  # noqa: E402

SESSION = "0b4c7a1e-2d3f-4a5b-8c6d-7e8f9a0b1c2d"


def seed(state: Path, who: str) -> Path:
    out = state / "sessions" / SESSION / "output" / "r-0000000000000001"
    out.mkdir(parents=True)
    (out / "stdout").write_text(f"{who} retained output\n")
    return state / "sessions" / SESSION


def main() -> None:
    sb = runtime_sandbox("e")
    try:
        operator_copy = seed(sb.state, "operator")
        executor_state = sb.dir / "executor-xdg" / "ccnm"
        executor_copy = seed(executor_state, "executor")
        rpc = sb.state / "rpc"
        (rpc / "sessions").mkdir(parents=True)
        (rpc / "keys/demo").mkdir(parents=True)
        (rpc / "sessions/s-p57-old.json").write_text('{"session":"s-p57-old"}')
        (rpc / "keys/demo/task-1").write_text("s-p57-old")
        (sb.fake / "purge-answer.json").write_text(json.dumps({
            "removed": [f"/fake/agent/state/sessions/{SESSION}"],
            "sessions": [SESSION],
        }))
        out = subprocess.run(
            [str(ccnm_binary()), "workspace", "remove", "demo", "--purge", "--config", str(sb.config)],
            env=sb.env(), capture_output=True, text=True, timeout=60,
        )
        calls = [(c["sub"], c["request"]) for c in sb.calls()]
        report = {
            "id": "E",
            "gap": ["CLEAN-01"],
            "exit": out.returncode,
            "stdout": out.stdout.strip().splitlines(),
            "stderr_tail": out.stderr.strip()[-300:],
            "agent_calls": calls,
            "operator_state_session_removed": not operator_copy.exists(),
            "executor_state_session_still_there": executor_copy.exists(),
            "operator_rpc_record_still_there": (rpc / "sessions/s-p57-old.json").exists(),
            "operator_rpc_start_key_still_there": (rpc / "keys/demo/task-1").exists(),
            "workspace_left_in_config": "[workspaces.demo]" in sb.config.read_text(),
            "note": "本机那半只删调用者自己 state 下的 sessions/<id>；Executor 身份的保留输出、RPC 记录与 start_key 都不在清理范围",
        }
        report["verdict"] = (
            "reproduced"
            if report["operator_state_session_removed"] and report["executor_state_session_still_there"]
            else "conforms"
        )
        report["tmp_dir"] = str(sb.dir)
        print(json.dumps(report, ensure_ascii=False, indent=2))
    finally:
        sb.release_all()
        sb.cleanup()


if __name__ == "__main__":
    main()
