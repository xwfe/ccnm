#!/usr/bin/env python3
"""P108.3、P108.4：经 Windows 自带的 sshd 起 `ccnm internal mcp-serve`（外部客户端的 read 模式），
用一个和 ccnm 无关的客户端把七个只读工具调一遍，量一次 Defender 下的耗时（U6），再压一次双向大流量。

    # 1. 在本机生成要放上去的目录；--base 是它在测试机上的位置，配置和包装脚本里写的都是它
    python3 -I p108-windows-read-tools.py prepare 本地目录 --base C:/Users/xdw/AppData/Local/Temp/ccnm-p108
    # 2. 打包、拷过去、解开、git init（见 docs/research/2026-10-25-p108-windows-read-tools.md）
    # 3. 跑
    python3 -I p108-windows-read-tools.py run --ssh win11 --base C:/Users/xdw/AppData/Local/Temp/ccnm-p108 --out 证据.json
    # 本机演练：同一套检查，服务端换成本机的 ccnm
    python3 -I p108-windows-read-tools.py prepare 目录 --base 目录 --git-init
    python3 -I p108-windows-read-tools.py run --local 目录 --ccnm target/debug/ccnm --out 证据.json

只用标准库，不 import 任何 ccnm 代码：两个独立实现都同意，结论才属于协议。

**压测在看什么。** P106 查出 sshd 给的 stdin 是异步管道，服务端只要有一阵不读 stdin 就会卡死。这里一个
线程只管写请求、另一个线程先**故意不读**回答 `HOLD` 秒，让服务端的 stdout 堆住。服务端这时还在读
stdin，写线程就能把全部请求写完；它要是因为输出积压停下不读，写线程就卡在 ssh 的窗口那么大的地方。
所以判据是"读线程开始读之前，写线程写出去了多少"，不只是"最后都回来了"——后者在读线程开始之后总会
自己解开。
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

READ_TOOLS = sorted([
    "list_files", "load_skill", "read_file", "read_notebook", "search_text", "view_image", "workspace_info",
])
WORKSPACE = "p108"
SESSION = "p108-probe"

# 压测：大请求各带 1 MiB 没声明的字段（只读工具收下、在结果里说忽略了它），大结果各是 big.txt 的一页。
BIG_REQUESTS = 24
BIG_REQUEST_PAD = 1024 * 1024
BIG_RESULTS = 200
BIG_LINES = 80_000
HOLD = 8.0

# U6：这么多个小文件，搜和列各跑几轮。第一轮 Defender 还没扫过它们。
MANY_FILES = 5000
ROUNDS = 3

# ccnm 传输连接的 ssh 参数，照 crates/ccnm-core/src/ssh.rs 的 command() 加 transport_options() 抄。
SSH_OPTIONS = [
    "SendEnv=-*", "SetEnv=CCNM_TRANSPORT=1", "ForwardAgent=no", "ClearAllForwardings=yes", "LogLevel=ERROR",
    "BatchMode=yes", "ConnectTimeout=10", "ControlMaster=no", "ControlPath=none",
    "ServerAliveInterval=15", "ServerAliveCountMax=20", "SendEnv=-ANTHROPIC_*",
]


# -- prepare --

def prepare(out: Path, base: str, git_init: bool) -> None:
    """生成 base 下那棵树：配置、包装脚本、一个 git 仓库里往下一层的项目。"""
    base = base.rstrip("/")
    root = out / "repo" / "sub" / "proj"
    files = {
        "src/main.rs": "fn main() { needle(); }\n",
        "src/lib/deep.rs": "pub fn needle() {}\n",
        "README.md": "# p108\n",
        "nb.ipynb": json.dumps({
            "cells": [{"cell_type": "code", "id": "c1", "metadata": {}, "execution_count": None,
                       "outputs": [], "source": ["print('nb-marker')"]}],
            "metadata": {}, "nbformat": 4, "nbformat_minor": 5,
        }),
        ".claude/skills/hello/SKILL.md": "---\nname: hello\ndescription: Says hello\n---\nproject-skill-body\n",
        ".claude/skills/hello/notes.md": "project-skill-notes\n",
    }
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(text.encode())
    (root / "pixel.png").write_bytes(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR")
    with open(root / "big.txt", "wb") as big:
        for n in range(1, BIG_LINES + 1):
            big.write(f"L{n:06d} {'x' * 92}\n".encode())
    for n in range(MANY_FILES):
        path = root / "many" / f"d{n // 100:02d}" / f"f{n:04d}.txt"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(f"small-file-{n:04d} needle-many\n".encode())
    for d in ("state", "home"):
        (out / d).mkdir(parents=True, exist_ok=True)
    # 机器技能关掉：HOME 换成了 base/home，关掉是为了这一轮不碰账号自己的东西也不依赖它。
    (out / "config.toml").write_text(
        f'this = "runtime"\n\n[nodes.runtime]\n\n[nodes.agent]\nssh = "agent-node.invalid"\n\n'
        f'[machine_skills]\nenabled = false\n\n'
        f'[workspaces.{WORKSPACE}]\nroot = "{base}/repo/sub/proj"\nagent_node = "agent"\nexternal_mcp = "read"\n'
    )
    win = base.replace("/", "\\")
    # sshd 不收 SetEnv（P106 的 U1），所以环境靠这个包装脚本给；CRLF，cmd 认。
    (out / "ccnm-p108.cmd").write_bytes((
        "@echo off\r\n"
        f'set "CCNM_CONFIG={win}\\config.toml"\r\n'
        f'set "XDG_STATE_HOME={win}\\state"\r\n'
        f'set "HOME={win}\\home"\r\n'
        f'"{win}\\ccnm.exe" %*\r\n'
        "exit /b %ERRORLEVEL%\r\n"
    ).encode())
    if git_init:
        subprocess.run(["git", "-c", "init.defaultBranch=main", "init", "-q"], cwd=out / "repo", check=True)


# -- run --

class Conn:
    """一条 stdio MCP 连接。读写各一个线程：自己先别成了卡住的那一方。"""

    def __init__(self, argv: list[str], env: dict[str, str] | None):
        self.proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, env=env)
        self.next_id = 0
        self.answers: dict[int, dict] = {}
        self.cond = threading.Condition()
        self.reading = threading.Event()
        self.reading.set()
        self.received_bytes = 0
        self.stderr = b""
        self.eof = False
        self.write_lock = threading.Lock()
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._drain_stderr, daemon=True).start()

    def _read(self):
        for line in self.proc.stdout:
            # 压测时在这里停：手里这一行先不交，管道和 ssh 的窗口就会被填满。
            self.reading.wait()
            self.received_bytes += len(line)
            message = json.loads(line)
            # 服务端自己发的 ping 之类：有 method 的不是回答。
            if "method" in message:
                continue
            with self.cond:
                self.answers[message.get("id")] = message
                self.cond.notify_all()
        with self.cond:
            self.eof = True
            self.cond.notify_all()

    def _drain_stderr(self):
        self.stderr = self.proc.stderr.read()

    def send(self, method: str, params: dict | None = None) -> int:
        with self.write_lock:
            self.next_id += 1
            request = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
            if params is not None:
                request["params"] = params
            data = (json.dumps(request) + "\n").encode()
            self.proc.stdin.write(data)
            self.proc.stdin.flush()
            return self.next_id

    def wait(self, ids: list[int], timeout: float) -> dict[int, dict]:
        deadline = time.monotonic() + timeout
        with self.cond:
            while not all(i in self.answers for i in ids):
                missing = [i for i in ids if i not in self.answers]
                if self.eof:
                    raise RuntimeError(f"stdout closed with {missing[:5]} unanswered; stderr: {self.stderr[-2000:]!r}")
                left = deadline - time.monotonic()
                if left <= 0:
                    raise TimeoutError(f"no answer to {missing[:5]} ({len(missing)} in all)")
                self.cond.wait(min(left, 0.5))
            return {i: self.answers.pop(i) for i in ids}

    def call(self, method: str, params: dict | None = None, timeout: float = 120) -> dict:
        i = self.send(method, params)
        message = self.wait([i], timeout)[i]
        if "error" in message:
            raise RuntimeError(f"{method}: {message['error']}")
        return message["result"]

    def close(self, timeout: float = 60) -> int:
        self.proc.stdin.close()
        return self.proc.wait(timeout)


def text_of(result: dict) -> str:
    return "".join(b.get("text", "") for b in result.get("content", []) if b.get("type") == "text")


class Probe:
    def __init__(self, args):
        self.args = args
        self.records: list[dict] = []
        payload = base64.urlsafe_b64encode(json.dumps(
            {"protocol": 5, "workspace": WORKSPACE, "session": SESSION, "mode": "read"}
        ).encode()).decode().rstrip("=")
        serve = ["internal", "mcp-serve", "--payload", payload]
        if args.ssh:
            base = args.base.rstrip("/")
            self.argv = ["ssh", "-T"] + [x for o in SSH_OPTIONS for x in ("-o", o)] + [
                args.ssh, f"{base}/ccnm-p108.cmd", *serve]
            self.env = None
            self.leaks = [base, base.replace("/", "\\"), "\\\\?\\"]
        else:
            local = Path(args.local).resolve()
            self.argv = [args.ccnm, *serve]
            self.env = {"PATH": os.environ.get("PATH", ""), "HOME": str(local / "home"),
                        "XDG_STATE_HOME": str(local / "state"), "CCNM_CONFIG": str(local / "config.toml")}
            self.leaks = [str(local)]

    def record(self, name: str, ok: bool, detail: str = "", **extra):
        self.records.append({"check": name, "ok": ok, "detail": detail[:500], **extra})
        print(("PASS " if ok else "FAIL ") + name + (f": {detail[:200]}" if detail and not ok else ""), flush=True)

    def clean(self, name: str, said: str):
        bad = [s for s in self.leaks if s in said] + (["\\"] if "\\" in said else [])
        if bad:
            self.record(f"{name} leaks nothing", False, f"{bad}: {said[:300]}")

    def tool(self, conn: Conn, name: str, args: dict, expect: str | None = None, refused: bool = False):
        started = time.monotonic()
        result = conn.call("tools/call", {"name": name, "arguments": args})
        ms = round((time.monotonic() - started) * 1000)
        said = text_of(result)
        is_error = bool(result.get("isError"))
        if refused:
            ok = is_error and said.startswith("CCNM_E_POLICY:")
        else:
            ok = not is_error and (expect is None or expect in said)
        self.record(f"{name} {json.dumps(args, ensure_ascii=False)[:80]}", ok, said, ms=ms)
        self.clean(name, said)
        return result, said, ms

    def run(self):
        started = time.monotonic()
        conn = Conn(self.argv, self.env)
        hello = conn.call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                         "clientInfo": {"name": "p108-probe", "version": "0"}}, timeout=60)
        self.record("initialize", "serverInfo" in hello, json.dumps(hello)[:300],
                    ms=round((time.monotonic() - started) * 1000))
        conn.proc.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        conn.proc.stdin.flush()
        tools = sorted(t["name"] for t in conn.call("tools/list", {})["tools"])
        self.record("tools/list is the seven read tools", tools == READ_TOOLS, str(tools))

        _, info, _ = self.tool(conn, "workspace_info", {}, "inside git repository at sub/proj")
        self.platform = info
        self.tool(conn, "read_file", {"path": "src/lib/deep.rs"}, "pub fn needle")
        _, listed, _ = self.tool(conn, "list_files", {"glob": "**/*.rs"})
        self.record("list_files paths", {"src/main.rs", "src/lib/deep.rs"} <= set(listed.splitlines()), listed)
        _, found, _ = self.tool(conn, "search_text", {"query": "needle", "path": "src"}, "src/lib/deep.rs")
        image, _, _ = self.tool(conn, "view_image", {"path": "pixel.png"}, "pixel.png: PNG")
        self.record("view_image block", image["content"][1].get("mimeType") == "image/png", json.dumps(image)[:300])
        self.tool(conn, "read_notebook", {"path": "nb.ipynb"}, "nb-marker")
        self.tool(conn, "load_skill", {"name": "hello"}, "project-skill-body")
        self.tool(conn, "load_skill", {"name": "hello", "file": "notes.md"}, "project-skill-notes")
        if self.args.ssh:
            for path in ["README.md:hidden", "NUL", "src/con.txt", "src./main.rs", "SRC~1/main.rs", "GIT~1/config"]:
                self.tool(conn, "read_file", {"path": path}, refused=True)

        self.u6(conn)
        self.stress(conn)
        code = conn.close()
        self.record("closing stdin ends the session cleanly", code == 0, f"exit {code}: {conn.stderr[-500:]!r}")

    def u6(self, conn: Conn):
        """Defender 下的耗时：没关它，只量，第一轮是冷的。"""
        self.timings = []
        last = MANY_FILES - 1
        for n in range(ROUNDS):
            # Only the last file matches, so rg reads all of them: a query
            # that matches early stops at max_results and measures nothing.
            _, said, search_ms = self.tool(conn, "search_text", {
                "query": f"small-file-{last:04d}", "path": "many", "output_mode": "files_with_matches"})
            hits = [line for line in said.splitlines() if line.startswith("many/")]
            _, listed, list_ms = self.tool(conn, "list_files", {"path": "many", "glob": "**/*.txt", "max_entries": 1000})
            entries = sum(1 for line in listed.splitlines() if line.startswith("many/"))
            read_started = time.monotonic()
            for k in range(200):
                conn.call("tools/call", {"name": "read_file", "arguments": {"path": f"many/d{k // 100:02d}/f{k:04d}.txt"}})
            read_ms = round((time.monotonic() - read_started) * 1000)
            self.timings.append({"round": n + 1, "search_all_ms": search_ms, "list_1000_ms": list_ms,
                                 "read_200_small_ms": read_ms})
            self.record(f"U6 round {n + 1}",
                        hits == [f"many/d{last // 100:02d}/f{last:04d}.txt"] and entries == 1000,
                        f"search over {MANY_FILES} files {search_ms} ms / list 1000 {list_ms} ms / 200 reads {read_ms} ms")

    def stress(self, conn: Conn):
        """写线程不停地写；读线程先停 HOLD 秒，看服务端在 stdout 堆住时还读不读 stdin。"""
        pad = "p" * BIG_REQUEST_PAD
        plan = []
        for k in range(BIG_RESULTS):
            start = (k * 997) % (BIG_LINES - 2000) + 1
            plan.append(("page", start, {"path": "big.txt", "start_line": start, "max_lines": 2000, "max_bytes": 65536}))
            if k % (BIG_RESULTS // BIG_REQUESTS) == 0 and sum(1 for p in plan if p[0] == "pad") < BIG_REQUESTS:
                plan.append(("pad", 1, {"path": "README.md", "pad": pad}))
        ids: dict[int, tuple] = {}
        sent = {"bytes": 0, "done": False, "at_hold_end": None, "error": None}
        conn.reading.clear()
        received_before = conn.received_bytes

        def writer():
            try:
                for kind, start, arguments in plan:
                    i = conn.send("tools/call", {"name": "read_file", "arguments": arguments})
                    ids[i] = (kind, start)
                    sent["bytes"] += len(json.dumps(arguments)) + 80
                sent["done"] = True
            except Exception as e:  # noqa: BLE001 -- 记下来交给判据
                sent["error"] = repr(e)

        started = time.monotonic()
        t = threading.Thread(target=writer, daemon=True)
        t.start()
        t.join(HOLD)
        sent["at_hold_end"] = sent["bytes"]
        written_while_held = sent["done"]
        conn.reading.set()
        t.join(600)
        total_request_bytes = sum(len(json.dumps(a)) + 80 for _, _, a in plan)
        self.record("server kept reading stdin while its stdout was backed up", written_while_held,
                    f"{sent['at_hold_end']} of {total_request_bytes} request bytes written in the first {HOLD} s",
                    request_bytes=total_request_bytes, written_while_held_bytes=sent["at_hold_end"])
        try:
            answers = conn.wait(list(ids), timeout=600)
        except Exception as e:  # noqa: BLE001
            self.record("every stressed call answered", False, repr(e))
            return
        elapsed = round(time.monotonic() - started, 1)
        wrong = []
        for i, (kind, start) in ids.items():
            said = text_of(answers[i]["result"]) if "result" in answers[i] else json.dumps(answers[i])
            if kind == "page" and f"L{start:06d} " not in said:
                wrong.append((i, kind, said[:120]))
            if kind == "pad" and ("pad" not in said or "# p108" not in said):
                wrong.append((i, kind, said[:120]))
        self.record("every stressed call answered, each with its own content", not wrong and not sent["error"],
                    f"{len(ids)} calls in {elapsed} s, {conn.received_bytes - received_before} bytes back; wrong={wrong[:3]} error={sent['error']}",
                    calls=len(ids), seconds=elapsed, response_bytes=conn.received_bytes - received_before)


def main() -> int:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("out")
    p.add_argument("--base", required=True)
    p.add_argument("--git-init", action="store_true")
    r = sub.add_parser("run")
    r.add_argument("--ssh")
    r.add_argument("--base")
    r.add_argument("--local")
    r.add_argument("--ccnm")
    r.add_argument("--out", required=True)
    args = parser.parse_args()
    if args.cmd == "prepare":
        prepare(Path(args.out), args.base, args.git_init)
        return 0
    if not (args.ssh and args.base) and not (args.local and args.ccnm):
        parser.error("run needs --ssh and --base, or --local and --ccnm")
    probe = Probe(args)
    started = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    try:
        probe.run()
    except Exception as e:  # noqa: BLE001 -- 证据里要有这一条
        probe.record("probe ran to the end", False, repr(e))
    evidence = {
        "started": started, "argv_target": args.ssh or "local", "records": probe.records,
        "u6": getattr(probe, "timings", None), "workspace_info": getattr(probe, "platform", None),
        "passed": all(r["ok"] for r in probe.records),
    }
    Path(args.out).write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")
    print(f"{'ALL PASSED' if evidence['passed'] else 'FAILED'} -> {args.out}")
    return 0 if evidence["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
