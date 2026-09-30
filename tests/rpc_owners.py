"""删临时目录之前，等 Machine API 会话的 owner 进程退出。测试共用，不是用例。

P63 起，`session.start` 接受的会话由一个脱离 `ccnm rpc` 的进程（owner）跑完，
连接关了它还在。用例一结束就删目录，它正好在这时把结局写进 `rpc/sessions/`：
`TemporaryDirectory` 报 `Directory not empty`，用例记成 error，/tmp 下留一个只剩
一份记录的目录。P64 跑门禁时两个测试类各撞到一次，单跑 25 次约 1 次；留下的记录
里写着 `config not found`——owner 启动时配置已经被删了。

owner 的 pid 在 `session.start` 回应之前就写进了记录，所以用例结束时读得到。
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import time


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def wait_for_owners(state_home: Path, timeout: float = 30.0) -> list[int]:
    """等 `state_home`（那次运行的 XDG_STATE_HOME）下每条记录的 owner 退出。

    返回到期还活着的 pid；空列表才可以放心删目录。
    """
    pids = set()
    for record in (state_home / "ccnm/rpc/sessions").glob("*.json"):
        try:
            pid = json.loads(record.read_text(encoding="utf-8")).get("owner_pid")
        except (OSError, ValueError):
            continue
        if isinstance(pid, int):
            pids.add(pid)
    deadline = time.monotonic() + timeout
    while any(_alive(pid) for pid in pids) and time.monotonic() < deadline:
        time.sleep(0.02)
    return sorted(pid for pid in pids if _alive(pid))
