#!/usr/bin/env python3
"""一个按剧本行事的假 `ccnm rpc`，用来测客户端遇到坏对端时的表现。

真实服务端不会在这些场景里配合：它不会宣称自己说别的协议版本，也不会在答应
了一次启动之后凭空消失。要验证客户端**不会**把这些误当成功，就得有一个愿意
这么演的对端。

用法（都从 stdin 读、往 stdout 写，和真的一样）：

    fake_rpc_peer.py --protocol ccnm.machine/9   # 只说别的版本的旧 peer
    fake_rpc_peer.py --die-after 1               # 答完 1 条就退出，下一条无声无息
    fake_rpc_peer.py --exit-zero-on-start        # 收到 start 直接退出且退出码 0
"""

import argparse
import json
import sys


def answer(message: dict) -> None:
    sys.stdout.write(json.dumps(message, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--protocol", default="ccnm.machine/1")
    parser.add_argument("--die-after", type=int, default=None)
    parser.add_argument("--exit-zero-on-start", action="store_true")
    args = parser.parse_args()

    answered = 0
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        request = json.loads(line)
        method = request.get("method")

        # 旧 peer 的经典失败：收到不认识的东西，什么都不做就以退出码 0 结束。
        # 客户端必须靠协议结果判断，不能因为进程"正常"退出就当成功。
        if args.exit_zero_on_start and method == "session.start":
            return 0

        if method == "hello":
            offered = request.get("params", {}).get("protocol_versions", [])
            if args.protocol in offered:
                answer({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "result": {
                        "protocol": args.protocol,
                        "server": {"name": "fake", "version": "0.0.0"},
                        "capabilities": {"modes": ["print"]},
                    },
                })
            else:
                answer({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "error": {
                        "code": -32002,
                        "message": "no shared protocol version",
                        "data": {"effect": "none", "supported": [args.protocol]},
                    },
                })
        else:
            answer({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {"agents": []},
            })

        answered += 1
        if args.die_after is not None and answered >= args.die_after:
            # 走掉，不说一声。客户端下一次调用会读到 EOF。
            return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
