#!/usr/bin/env python3
"""P93：Claude Code 先探测新协议、再退回旧协议时，ccnm 的 mcp-serve 还给不给工具（零额度）。

Claude Code 连 MCP server 先发 2026-07-28 的 `server/discover`；回答来得比它愿意等的晚，
它就改发旧协议的 `initialize`，之后的请求不带 `_meta`。这里按同样的顺序直接跟
`ccnm internal mcp-serve` 说话，不经过 Claude、不调模型：

  对照    initialize -> tools/list
  先探测  server/discover（带新协议 _meta）-> initialize -> 不带 _meta 的 tools/list

rmcp 3.2.0（ccnm 0.16.1 及以前）上"先探测"那一行的 tools/list 报
`request _meta is missing or has malformed required fields`；修好的版本两行都拿到工具。
两行都拿到工具时退出码 0，否则 1。

用法：
  python3 docs/research/probes/p93-discover-fallback.py <ccnm 路径> <workspace> <项目根目录>
  python3 docs/research/probes/p93-discover-fallback.py --ssh <Runtime 的 ssh 别名> \\
      <那台上的 ccnm 路径> <workspace> <那台上的项目根目录>

本机直接起时用 `env -i` 只留 HOME、PATH、USER：在 Claude Code 会话里跑，继承来的认证
环境变量会让 mcp-serve 按设计拒绝服务。经 `--ssh` 时 ssh 本来就不带这些变量。
只开只读会话（policy=read），不写项目。
"""

import argparse
import base64
import json
import os
import shlex
import subprocess
import sys
import uuid

MODERN_META = {
    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
    "io.modelcontextprotocol/clientCapabilities": {},
    "io.modelcontextprotocol/clientInfo": {"name": "p93-probe", "version": "0"},
}


def payload(workspace, root):
    body = {"protocol": 1, "workspace": workspace, "root": root,
            "session": str(uuid.uuid4()), "policy": "read"}
    return base64.urlsafe_b64encode(json.dumps(body).encode()).decode().rstrip("=")


def start(args):
    serve = [args.ccnm, "internal", "mcp-serve", "--payload", payload(args.workspace, args.root)]
    if args.ssh:
        return subprocess.Popen(["ssh", "-T", args.ssh, shlex.join(serve)],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True)
    env = {k: os.environ[k] for k in ("HOME", "USER", "LOGNAME") if k in os.environ}
    env["PATH"] = "/usr/bin:/bin:/usr/sbin:/sbin"
    return subprocess.Popen(serve, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True)


def run(args, probe_first):
    p = start(args)

    def send(obj):
        p.stdin.write(json.dumps(obj) + "\n")
        p.stdin.flush()

    def answer(id_):
        while True:
            line = p.stdout.readline()
            if not line:
                raise SystemExit("mcp-serve closed stdout; stderr: " + p.stderr.read()[:800])
            msg = json.loads(line)
            if msg.get("id") == id_ and "method" not in msg:
                return msg

    def show(msg, ok):
        return "error: " + msg["error"]["message"] if "error" in msg else ok(msg["result"])

    out = {}
    try:
        if probe_first:
            send({"jsonrpc": "2.0", "id": 1, "method": "server/discover",
                  "params": {"_meta": MODERN_META}})
            out["discover"] = show(answer(1), lambda r: "ok")
        send({"jsonrpc": "2.0", "id": 2, "method": "initialize",
              "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                         "clientInfo": {"name": "p93-probe", "version": "0"}}})
        out["initialize"] = show(answer(2), lambda r: r["protocolVersion"])
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {}})
        out["tools/list"] = show(answer(3), lambda r: f"{len(r['tools'])} tools")
    finally:
        p.stdin.close()
        try:
            p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p.kill()
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument("--ssh", help="mcp-serve 在这个 ssh 别名的机器上起，和受管会话一样")
    ap.add_argument("ccnm")
    ap.add_argument("workspace")
    ap.add_argument("root")
    args = ap.parse_args()
    ok = True
    for name, probe_first in (("对照", False), ("先探测", True)):
        out = run(args, probe_first)
        print(f"{name}: {json.dumps(out, ensure_ascii=False)}")
        ok = ok and out.get("tools/list", "").endswith(" tools")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
