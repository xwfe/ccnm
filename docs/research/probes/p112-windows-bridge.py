#!/usr/bin/env python3
"""P112：经 `ccnm mcp bridge`（外部客户端那条路，AI 那台 macOS → sshd → Windows 上的 mcp-serve）
把 coding 模式的工具全调一遍，并看取消、后台命令、转接项目的 MCP server、会话结束之后
Windows 上还剩什么。

    # 1. 生成要放到 Windows 上的项目（打包、拷过去、git init 见 P112 记录）
    python3 -I p112-windows-bridge.py prepare 本地目录
    # 2. 跑：--bridge 是一条能起 bridge 的命令（本机临时配置、同一提交的 ccnm），
    #    --win 是一个 `<脚本> <PowerShell 命令>` 形状的 ssh 包装，用来在 Windows 上查进程
    python3 -I p112-windows-bridge.py run --bridge "ccnm --config X mcp bridge p112 --mode coding" \
        --win ./w --out 证据.json

客户端用仓库里的 tests/mcp_client.py：它和 ccnm 没有一行共用代码，只知道"一行一条
JSON-RPC"。这里不 import 任何 ccnm 的东西。
"""
from __future__ import annotations

import argparse
import base64
import json
import shlex
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tests"))
from mcp_client import McpClient, is_error, result_text  # noqa: E402

CODING_TOOLS = sorted([
    "apply_patch", "call_mcp_tool", "exec_command", "list_files", "load_skill", "read_file",
    "read_notebook", "read_output", "search_text", "stop_command", "view_image", "workspace_info",
])
# 一像素的 PNG。
DOT_PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
)
ECHO_MCP = r"""const rl = require('readline').createInterface({ input: process.stdin });
const send = (m) => process.stdout.write(JSON.stringify(m) + '\n');
rl.on('line', (line) => {
  const m = JSON.parse(line);
  if (m.method === 'initialize') {
    send({ jsonrpc: '2.0', id: m.id, result: { protocolVersion: m.params.protocolVersion,
      capabilities: { tools: {} }, serverInfo: { name: 'echo', version: '0' } } });
  } else if (m.method === 'tools/list') {
    send({ jsonrpc: '2.0', id: m.id, result: { tools: [{ name: 'echo', description: 'Echo text back',
      inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] } }] } });
  } else if (m.method === 'tools/call') {
    send({ jsonrpc: '2.0', id: m.id, result: { content: [{ type: 'text',
      text: 'echo-p112: ' + m.params.arguments.text + ' pid=' + process.pid }] } });
  } else if (m.id !== undefined) {
    send({ jsonrpc: '2.0', id: m.id, result: {} });
  }
});
"""


def prepare(out: Path) -> None:
    files = {
        "README.md": "# p112\nneedle-p112 lives here\n",
        "src/lib.rs": "pub fn needle() -> u32 {\n    112\n}\n",
        "notes/nb.ipynb": json.dumps({
            "cells": [{"cell_type": "code", "id": "c1", "metadata": {}, "execution_count": None,
                       "outputs": [], "source": ["print('nb-marker-p112')"]}],
            "metadata": {}, "nbformat": 4, "nbformat_minor": 5}),
        ".claude/skills/hello/SKILL.md": "---\nname: hello\ndescription: Says hello\n---\nskill-body-p112\n",
        "tools/echo-mcp.js": ECHO_MCP,
        ".mcp.json": json.dumps({"mcpServers": {"echo": {"command": "node", "args": ["tools/echo-mcp.js"]}}}),
    }
    for rel, text in files.items():
        path = out / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(text.encode())
    (out / "img").mkdir(exist_ok=True)
    (out / "img" / "dot.png").write_bytes(DOT_PNG)
    print(f"prepared {out}")


# -- Windows 上看进程 --

WATCHED = ["ccnm.exe", "ping.exe", "sleep.exe", "node.exe", "bash.exe", "cmd.exe", "conhost.exe"]


def windows(win: str, script: str) -> str:
    encoded = base64.b64encode(script.encode("utf-16-le")).decode()
    done = subprocess.run([win, f"powershell -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}"],
                          capture_output=True, text=True, timeout=120)
    return done.stdout


def processes(win: str) -> dict[int, str]:
    """Windows 上被盯着的那几种进程：pid -> 小写的名字和命令行。查询自己这次 ssh 会话
    起的进程（它的 PowerShell、conhost）不算：每查一次就换一批 pid。"""
    names = " OR ".join(f"Name='{n}'" for n in WATCHED)
    said = windows(win, (
        "Import-Module CimCmdlets; $all = Get-CimInstance Win32_Process; "
        "$mine = @{}; $p = $PID; while ($p -and -not $mine.ContainsKey($p)) { $mine[$p] = 1; "
        "$p = ($all | Where-Object ProcessId -eq $p).ParentProcessId }; "
        "Get-CimInstance Win32_Process -Filter \"" + names + "\" | "
        "Where-Object { -not $mine.ContainsKey([int]$_.ParentProcessId) -and -not $mine.ContainsKey([int]$_.ProcessId) } | "
        "ForEach-Object { '{0}|{1}|{2}' -f $_.ProcessId, $_.Name, $_.CommandLine }"))
    found = {}
    for line in said.splitlines():
        pid, _, rest = line.partition("|")
        if pid.strip().isdigit():
            found[int(pid)] = rest.strip().lower()
    return found


def new_since(win: str, before: dict[int, str]) -> dict[int, str]:
    return {pid: what for pid, what in processes(win).items() if pid not in before}


def left_over(win: str, before: dict[int, str]) -> dict[int, str]:
    """两次查询都在的新进程：一次性的那种（别人的、正在退出的）不算。"""
    first = new_since(win, before)
    time.sleep(3)
    second = new_since(win, before)
    return {pid: what for pid, what in second.items() if pid in first}


# -- 跑 --

class Run:
    def __init__(self, args):
        self.args = args
        self.results = []
        self.failed = 0

    def record(self, name: str, ok: bool, detail: str = "", **extra):
        self.results.append({"check": name, "ok": ok, "detail": detail[:1500], **extra})
        self.failed += 0 if ok else 1
        print(f"{'PASS' if ok else 'FAIL'} {name}: {detail[:300]}", flush=True)

    def client(self) -> McpClient:
        client = McpClient(shlex.split(self.args.bridge))
        client.initialize()
        return client

    def tool(self, client, name, arguments, expect=None):
        started = time.monotonic()
        got = client.call_tool(name, arguments)
        said = result_text(got)
        ok = not is_error(got) and (expect is None or expect in said)
        self.record(name, ok, said, seconds=round(time.monotonic() - started, 2))
        return said

    def run(self):
        before = processes(self.args.win)
        client = self.client()
        names = sorted(client.tool_names())
        self.record("tools/list", names == CODING_TOOLS, ", ".join(names))
        self.tool(client, "workspace_info", {}, "windows")
        version = self.tool(client, "read_file", {"path": "README.md"}, "needle-p112").rsplit("; version ", 1)[-1].rstrip("]")
        self.tool(client, "list_files", {"path": "src"}, "lib.rs")
        self.tool(client, "search_text", {"query": "needle-p112"}, "README.md")
        self.tool(client, "view_image", {"path": "img/dot.png"})
        self.tool(client, "read_notebook", {"path": "notes/nb.ipynb"}, "nb-marker-p112")
        self.tool(client, "load_skill", {"name": "hello"}, "skill-body-p112")
        self.tool(client, "apply_patch", {"files": [
            {"op": "add", "path": "added.txt", "content": "added-by-p112\r\n"},
            {"op": "write", "path": "README.md", "content": "# p112\nneedle-p112 rewritten\n", "version": version},
        ]}, "added.txt")
        self.tool(client, "exec_command", {"cmd": ["git", "status", "--porcelain"]}, "added.txt")
        self.tool(client, "exec_command", {"shell": "echo shell-p112 && cat added.txt"}, "added-by-p112")
        self.tool(client, "call_mcp_tool", {"server": "echo", "tool": "echo", "arguments": {"text": "hi"}}, "echo-p112: hi")

        # 后台命令：起来、读得到、停得掉。
        started = self.tool(client, "exec_command", {"shell": "for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do echo tick-$i; sleep 1; done", "run_in_background": True}, "output_ref")
        ref = next((w for w in started.replace(",", " ").split() if w.startswith("r-")), "")
        time.sleep(3)
        self.tool(client, "read_output", {"output_ref": ref}, "tick-1")
        self.tool(client, "stop_command", {"output_ref": ref})

        # 取消：像在 Host 里按 Esc。请求发出去 2 秒后取消，看 ping 和 Git Bash 的 sleep 是否 10 秒内都没了。
        for label, arguments, image in [
            ("cmd ping", {"cmd": ["ping", "-n", "60", "127.0.0.1"]}, "ping.exe"),
            ("Git Bash sleep", {"shell": "sleep 60"}, "sleep.exe"),
        ]:
            pending = client.send("tools/call", {"name": "exec_command", "arguments": arguments})
            time.sleep(2)
            running = [p for p, w in new_since(self.args.win, before).items() if w.startswith(image)]
            client.notify("notifications/cancelled", {"requestId": pending, "reason": "p112 Esc"})
            gone_at = None
            for _ in range(10):
                time.sleep(1)
                left = [p for p, w in new_since(self.args.win, before).items() if w.startswith(image)]
                if not left:
                    gone_at = _ + 1
                    break
            self.record(f"cancel {label}", bool(running) and gone_at is not None,
                        f"running before cancel: {running}; gone after ~{gone_at} s")

        # 会话结束：bridge 关掉以后 Windows 上不该剩下这次起的任何东西（mcp-serve、node、bash…）。
        client.close()
        time.sleep(5)
        left = left_over(self.args.win, before)
        self.record("nothing left after the session", not left, json.dumps(left, ensure_ascii=False))

        # 写锁交出来了：马上再开一个 coding 会话能写。
        second = self.client()
        self.tool(second, "apply_patch", {"files": [{"op": "add", "path": "second.txt", "content": "second\n"}]}, "second.txt")
        second.close()

        summary = {"failed": self.failed, "results": self.results}
        if self.args.out:
            Path(self.args.out).write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
        print(f"{len(self.results) - self.failed} passed, {self.failed} failed")
        return 1 if self.failed else 0


def main() -> int:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("out", type=Path)
    r = sub.add_parser("run")
    r.add_argument("--bridge", required=True)
    r.add_argument("--win", required=True)
    r.add_argument("--out")
    args = parser.parse_args()
    if args.cmd == "prepare":
        prepare(args.out)
        return 0
    return Run(args).run()


if __name__ == "__main__":
    sys.exit(main())
