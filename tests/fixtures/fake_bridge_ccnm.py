#!/usr/bin/env python3
"""一个假的 `ccnm`，只认 `mcp bridge`，把它接到本机真实的 server 上。

真的 bridge 会 exec 一条 ssh 到远端 Runtime。离线测试里没有远端，也不该伪造
一个——所以这个替身做的是把同样的 internal 协议 5 payload 直接交给**真实
的** `ccnm internal mcp-serve`。矩阵工具因此跑的是真服务端、真协议、真权限
判定，只是中间少了那一段网络。

    FAKE_BRIDGE_CCNM=<真 ccnm 路径>   必填
    FAKE_BRIDGE_LEAK=1                往 instructions 那侧塞一个凭据名，
                                      用来验证泄漏扫描确实在查

用法和真的一样：

    fake_bridge_ccnm.py [--config X] mcp bridge <workspace> --mode read|coding
"""

import os
import subprocess
import sys
import base64
import json


def payload(workspace: str, mode: str) -> str:
    body = json.dumps(
        {
            "protocol": 5,
            "workspace": workspace,
            # 每条连接一个 session id，和真 bridge 一样。
            "session": f"fakebridge-{os.getpid()}",
            "mode": mode,
        }
    ).encode("utf-8")
    return base64.urlsafe_b64encode(body).decode("ascii").rstrip("=")


def main() -> int:
    argv = sys.argv[1:]
    # `--config X` 可以出现在子命令前面，和真的一样。
    config = None
    if argv[:1] == ["--config"]:
        config = argv[1]
        argv = argv[2:]
    if argv[:2] != ["mcp", "bridge"]:
        print(f"fake bridge ccnm does not implement {argv}", file=sys.stderr)
        return 64
    workspace = argv[2]
    mode = "read"
    if "--mode" in argv:
        mode = argv[argv.index("--mode") + 1]

    real = os.environ.get("FAKE_BRIDGE_CCNM")
    if not real:
        print("FAKE_BRIDGE_CCNM is not set", file=sys.stderr)
        return 64
    command = [real]
    if config:
        command += ["--config", config]
    command += ["internal", "mcp-serve", "--payload", payload(workspace, mode)]
    if os.environ.get("FAKE_BRIDGE_LEAK"):
        # 装成一条从远端回来的话，里面带一个本不该出现的路径。
        print("note: ~/.claude/credentials was consulted", file=sys.stderr)
    os.execvp(command[0], command)


if __name__ == "__main__":
    raise SystemExit(main())
