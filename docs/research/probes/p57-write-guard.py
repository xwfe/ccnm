"""P57：写权缺口（AUTH-01/02）的零额度复核。

被测是真实 `ccnm internal mcp-serve`，客户端是不 import ccnm 的中立 MCP 客户端
（tests/mcp_client.py）。只动本轮临时目录和本轮起的进程；强杀只针对本轮
mcp-serve 的 pid，残留命令按命令行里的本轮临时路径识别后再收掉。

    cargo build && python3 -B docs/research/probes/p57-write-guard.py

RPC 那一半（session.start 不给 busy、两个启动都到了 Agent）在
p57-rpc-control.py 的 A1 里；这里看 Runtime 握手这一半。
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "tests"))
from mcp_client import McpClient, is_error, result_text  # noqa: E402
from test_remote_workspace_mcp import RemoteWorkspaceMcpTests  # noqa: E402


def refused_line(completed: subprocess.CompletedProcess) -> str:
    lines = [line.strip() for line in completed.stderr.splitlines() if line.strip()]
    return " ".join(lines[:2])


def markers(case) -> list:
    return [p.read_text(encoding="utf-8").strip() for p in sorted((case.dir / "state").rglob("*.lock"))]


def d1_same_workspace() -> dict:
    case = RemoteWorkspaceMcpTests()
    case.setUp()
    try:
        case.write_config("coding")
        first = case.client("demo", "coding", "p57-d1-first")
        second = case.refused("demo", "coding")
        held = markers(case)
        first.close()
        third = case.client("demo", "coding", "p57-d1-third")
        third.close()
        return {
            "id": "D1",
            "gap": ["AUTH-01"],
            "verdict": "conforms" if second.returncode != 0 else "reproduced",
            "second_writer_exit": second.returncode,
            "second_writer_says": refused_line(second),
            "marker_while_first_open": held,
            "after_first_closed_next_writer_ok": True,
            "note": "拒绝发生在 Runtime 的 MCP 握手；RPC 的 session.start 那时早已回了 starting（A1）",
        }
    finally:
        case.doCleanups()


def d2_worktrees_share_common_dir() -> dict:
    case = RemoteWorkspaceMcpTests()
    case.setUp()
    try:
        repo = case.dir / "repo"
        wt = case.dir / "wt2"
        git = ["git", "-c", "user.name=p57", "-c", "user.email=p57@example.invalid", "-c", "init.defaultBranch=main"]
        subprocess.run([*git, "init", "-q", str(repo)], check=True)
        (repo / "a.txt").write_text("a\n")
        subprocess.run([*git, "-C", str(repo), "add", "a.txt"], check=True)
        subprocess.run([*git, "-C", str(repo), "commit", "-qm", "base"], check=True)
        subprocess.run([*git, "-C", str(repo), "worktree", "add", "-q", "-b", "wt2", str(wt)], check=True)
        case.config.write_text(f"""
this = "runtime"
[nodes.runtime]
[nodes.agent]
ssh = "agent-node.invalid"
[workspaces.main]
root = "{repo}"
agent = {{ node = "agent", instance = "claude-main" }}
external_mcp = "coding"
[workspaces.wt]
root = "{wt}"
agent = {{ node = "agent", instance = "claude-main" }}
external_mcp = "coding"
""", encoding="utf-8")
        first = case.client("main", "coding", "p57-d2-main")
        second = case.refused("wt", "coding")
        first.close()
        return {
            "id": "D2",
            "gap": ["AUTH-01"],
            "verdict": "conforms" if second.returncode != 0 else "reproduced",
            "second_worktree_writer_exit": second.returncode,
            "second_worktree_writer_says": refused_line(second),
        }
    finally:
        case.doCleanups()


def d3_two_state_dirs() -> dict:
    """已知边界（P43 记录）：同一棵树配两个 XDG_STATE_HOME 是两个互不知晓的写域。"""
    case = RemoteWorkspaceMcpTests()
    case.setUp()
    try:
        case.write_config("coding")
        first = case.client("demo", "coding", "p57-d3-a")
        other_state = case.dir / "state-b"
        other_state.mkdir()
        env = {**case.env(), "XDG_STATE_HOME": str(other_state)}
        second = McpClient(case.argv("demo", "coding", "p57-d3-b"), env)
        both = True
        try:
            second.initialize()
        except Exception:  # noqa: BLE001 — 被拒就是结论本身
            both = False
        second.close()
        first.close()
        return {
            "id": "D3",
            "gap": ["AUTH-01"],
            "verdict": "known_boundary" if both else "conforms",
            "both_writers_admitted": both,
            "note": "同一 root、两个 state 域时两个 coding 会话都进得来；这是已写进运维手册的部署边界，不是本轮新缺陷",
        }
    finally:
        case.doCleanups()


def d4_sigkill_leaves_a_command() -> dict:
    case = RemoteWorkspaceMcpTests()
    case.setUp()
    residue_pid = None
    tag = None
    try:
        case.write_config("coding", unconfined=True)
        tag = str(case.dir / "p57-d4-residue")
        first = case.client("demo", "coding", "p57-d4-first")
        started = first.call_tool("exec_command", {
            "cmd": [sys.executable, "-c", "import time; time.sleep(297)", tag],
            "run_in_background": True,
        })
        if is_error(started):
            raise RuntimeError(result_text(started))

        def find_residue():
            rows = subprocess.run(["/bin/ps", "-Ao", "pid=,command="], capture_output=True, text=True).stdout
            for row in rows.splitlines():
                if tag in row and "time.sleep(297)" in row:
                    return int(row.split()[0])
            return None

        for _ in range(100):
            residue_pid = find_residue()
            if residue_pid:
                break
            time.sleep(0.05)
        server_pid = first._proc.pid  # noqa: SLF001 — 只拿本轮 mcp-serve 的 pid
        os.kill(server_pid, signal.SIGKILL)
        first._proc.wait(timeout=10)  # noqa: SLF001
        residue_alive = residue_pid is not None and find_residue() == residue_pid
        after = markers(case)
        next_writer = case.refused("demo", "coding")
        return {
            "id": "D4",
            "gap": ["AUTH-02"],
            "verdict": "known_boundary" if residue_alive and next_writer.returncode != 0 else "observed",
            "background_command_survived_sigkill": residue_alive,
            "marker_after_sigkill": after,
            "next_writer_exit": next_writer.returncode,
            "next_writer_says": refused_line(next_writer),
            "note": "命令留在机器上、写锁停在 held、下一个 writer 被拒：fail-closed，恢复要人工；与 P43 记录一致",
        }
    finally:
        if residue_pid and tag:
            found = subprocess.run(["/bin/ps", "-o", "command=", "-p", str(residue_pid)], capture_output=True, text=True).stdout
            if tag in found:
                os.kill(residue_pid, signal.SIGKILL)
        case.doCleanups()


def main() -> None:
    results = [d1_same_workspace(), d2_worktrees_share_common_dir(), d3_two_state_dirs(), d4_sigkill_leaves_a_command()]
    print(json.dumps({"results": results}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
