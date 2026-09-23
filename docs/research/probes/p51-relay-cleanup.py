"""P51：零额度复核 Runtime MCP server 正常退出后，其子进程是否仍能写。

只操作临时工作区、合成 HOME 和本次创建的进程，不读取真实配置，不连接 SSH。
先 cargo build，再从仓库根执行 python3 -B docs/research/probes/p51-relay-cleanup.py。
输出 defect_reproduced，不把探针执行成功等同于产品通过验收。
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
from mcp_client import is_error, result_text  # noqa: E402
from test_remote_workspace_mcp import RemoteWorkspaceMcpTests  # noqa: E402


def main() -> None:
    case = RemoteWorkspaceMcpTests()
    case.setUp()
    child_pid = None
    report = {}
    try:
        case.write_config("coding", unconfined=True)
        tick = case.root / "child-tick"
        child_file = case.root / "child.pid"
        child_code = (
            "import time; from pathlib import Path; "
            f"p=Path({str(tick)!r}); "
            "[(p.write_text(str(i)),time.sleep(0.1)) for i in range(150)]"
        )
        server = case.root / "audit-server.py"
        server.write_text(
            "import subprocess,sys,runpy; from pathlib import Path\n"
            f"p=subprocess.Popen([sys.executable,'-c',{child_code!r}],"
            "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\n"
            f"Path({str(child_file)!r}).write_text(str(p.pid))\n"
            f"runpy.run_path({str(ROOT / 'tests/fixtures/fake_mcp_server.py')!r},run_name='__main__')\n",
            encoding="utf-8",
        )
        (case.root / ".mcp.json").write_text(json.dumps({"mcpServers": {
            "audit": {"command": sys.executable, "args": [str(server)]}
        }}), encoding="utf-8")
        first = case.client("demo", "coding", "p51-relay-first")
        got = first.call_tool("call_mcp_tool", {"server": "audit", "tool": "pid"})
        if is_error(got):
            raise RuntimeError(result_text(got))
        server_pid = int(result_text(got))
        child_pid = int(child_file.read_text())
        report["same_process_group"] = os.getpgid(child_pid) == server_pid
        report["first_close_exit"] = first.close()
        before = tick.read_text() if tick.exists() else None
        time.sleep(0.4)
        after = tick.read_text() if tick.exists() else None
        report["old_child_wrote_after_close"] = before != after
        report["guard_markers"] = [p.read_text() for p in (case.dir / "state").rglob("*.lock")]
        second = case.client("demo", "coding", "p51-relay-second")
        changed = second.call_tool("apply_patch", {"files": [
            {"op": "add", "path": "second-writer.txt", "content": "second writer\n"}
        ]})
        report["second_writer_succeeded"] = not is_error(changed) and (case.root / "second-writer.txt").exists()
        report["second_close_exit"] = second.close()
        report["defect_reproduced"] = bool(
            report["same_process_group"] and report["old_child_wrote_after_close"]
            and report["second_writer_succeeded"]
        )
    finally:
        if child_pid is not None:
            # 只清本轮命令；进程已退出或 pid 已换人时不发送信号。
            found = subprocess.run(["ps", "-p", str(child_pid), "-o", "command="], capture_output=True, text=True)
            if str(case.root / "child-tick") in found.stdout:
                os.kill(child_pid, signal.SIGTERM)
                for _ in range(50):
                    check = subprocess.run(["ps", "-p", str(child_pid), "-o", "command="], capture_output=True, text=True)
                    if str(case.root / "child-tick") not in check.stdout:
                        break
                    time.sleep(0.1)
                else:
                    raise RuntimeError("本轮子进程尚未退出，保留现场并检查")
            report["owned_child_cleaned"] = True
        case.doCleanups()
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
