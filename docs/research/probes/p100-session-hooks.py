#!/usr/bin/env python3
"""P100–P102：受管 Claude 会话里"工具断了替人接上"和"会话钩子"靠得住的那几件事（零额度）。

要量的：
  1. 输入框的样子：空闲且空、有没发出去的草稿、模型正忙时，`tmux capture-pane -p` 抓到什么
     （ccnm 只在"空闲且空"时替人敲字）。
  2. MCP server 死掉、Claude 自己重启一次也失败之后，经 tmux 敲 `/mcp reconnect ccnm` 能不能
     接回来；界面上留什么字。
  3. 会话钩子：SessionStart（startup / clear / compact / resume+fork）、Stop、PreCompact、
     PostCompact 各在什么时候触发、stdin 里有什么（session_id、source、last_assistant_message），
     SessionStart 的 stdout 有没有进下一次发给模型的请求。

做法同 p87-auto-compact.py、p92-resume.py：真实 Claude Code 接本机假模型，HOME 是空临时目录，
sandbox-exec 只放本机出站，启动参数照 ccnm 的 launch_cmd，放在单独的 tmux server 里。钩子是
一个记下 stdin、往 stdout 打标记的小脚本，不是 ccnm 自己的钩子——这里量的是 Claude Code。

用法（macOS，要 tmux 和 sandbox-exec）：
  python3 docs/research/probes/p100-session-hooks.py <仓库外的新输出目录> \\
      --claude cc-300=/path/to/2.1.300/claude
"""

import argparse
import json
import os
import shlex
import subprocess
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

KEY = "sk-ant-probe-not-a-real-key-000000000000"
SANDBOX = """(version 1)
(allow default)
(deny network-outbound)
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket))
"""
# Exits at once while `dead` exists next to it: a Runtime that cannot be
# reached, so Claude's own single restart fails too.
TINY_SERVER = r'''
import json, os, sys
here = os.path.dirname(os.path.abspath(__file__))
if os.path.exists(os.path.join(here, "dead")):
    sys.stderr.write("ssh: connect to host runtime port 22: Operation timed out\n")
    sys.exit(255)
TOOLS = [{"name": "read_file", "description": "Read a file.",
          "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}}]
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    method, mid = msg.get("method"), msg.get("id")
    if mid is None:
        continue
    if method == "initialize":
        result = {"protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"),
                  "capabilities": {"tools": {}}, "serverInfo": {"name": "tiny", "version": "0"}}
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "TINY-OK"}]}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
'''
HOOK = r'''
import json, sys
event = sys.argv[1]
raw = sys.stdin.read()
try:
    data = json.loads(raw)
except ValueError:
    data = {"unparsed": raw[:500]}
with open(sys.argv[2], "a") as log:
    log.write(json.dumps({"event": event, "input": data}) + "\n")
source = data.get("source") or data.get("trigger") or ""
print(f"HOOK-{event}-{source}-STDOUT")
'''
HANDOFF = "[ccnm handoff]\nGoal: P100 probe. Next: run the probe again."


def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


class FakeModel:
    """Every request is kept. A user turn containing SLOW is answered after a
    pause (so the pane can be seen busy); one containing HANDOFF is answered
    with a handoff block."""

    def __init__(self, out):
        self.out, self.requests = out, []
        model = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def send_json(self, value):
                body = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                self.send_json({"data": []})

            def do_POST(self):
                raw = self.rfile.read(int(self.headers.get("content-length") or 0))
                if "count_tokens" in self.path:
                    return self.send_json({"input_tokens": 1})
                if "/v1/messages" not in self.path:
                    return self.send_json({})
                req = json.loads(raw)
                n = len(model.requests)
                model.requests.append(req)
                (model.out / f"req-{n}.json").write_bytes(raw)
                # 2.1.300 ends the list with a `system` message of its own,
                # so the turn being answered is the last `user` one.
                users = [m for m in req.get("messages") or [] if m.get("role") == "user"]
                last = json.dumps(users[-1] if users else {})
                text = f"reply {n}"
                if "HANDOFF" in last:
                    text = "Done with this part.\n\n" + HANDOFF
                if "SLOW" in last:
                    time.sleep(8)
                usage = {"input_tokens": 1, "output_tokens": 1}
                if not req.get("stream"):
                    return self.send_json({"id": "msg", "type": "message", "role": "assistant",
                                           "model": "claude-opus-5-5",
                                           "content": [{"type": "text", "text": text}],
                                           "stop_reason": "end_turn", "stop_sequence": None,
                                           "usage": usage})
                payload = sse([
                    {"type": "message_start", "message": {
                        "id": f"msg_{n}", "type": "message", "role": "assistant",
                        "model": "claude-opus-5-5", "content": [], "stop_reason": None,
                        "stop_sequence": None, "usage": usage}},
                    {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                    {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
                    {"type": "content_block_stop", "index": 0},
                    {"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                     "usage": usage},
                    {"type": "message_stop"},
                ])
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.port = self.server.server_address[1]


class Probe:
    def __init__(self, out, claude):
        self.out, self.claude = out, claude
        out.mkdir(parents=True)
        self.home = out / "home"
        self.home.mkdir()
        self.cwd = (out / "workspace").resolve()
        self.cwd.mkdir()
        (self.home / ".claude.json").write_text(json.dumps({
            "hasCompletedOnboarding": True, "theme": "dark", "numStartups": 5,
            "customApiKeyResponses": {"approved": [KEY[-20:]], "rejected": []},
            "projects": {str(self.cwd): {"hasTrustDialogAccepted": True,
                                         "hasCompletedProjectOnboarding": True,
                                         "projectOnboardingSeenCount": 5}},
        }))
        (out / "no-egress.sb").write_text(SANDBOX)
        self.server_dir = out / "server"
        self.server_dir.mkdir()
        (self.server_dir / "tiny_server.py").write_text(TINY_SERVER)
        (out / "hook.py").write_text(HOOK)
        self.hook_log = out / "hooks.jsonl"
        (out / "mcp.json").write_text(json.dumps({"mcpServers": {"ccnm": {
            "type": "stdio", "command": sys.executable,
            "args": [str(self.server_dir / "tiny_server.py")]}}}))

        def hook(event):
            return [{"hooks": [{"type": "command", "command":
                                f"{shlex.quote(sys.executable)} {shlex.quote(str(out / 'hook.py'))} "
                                f"{event} {shlex.quote(str(self.hook_log))}"}]}]
        (out / "settings.json").write_text(json.dumps({
            "permissions": {"allow": ["mcp__ccnm__read_file"],
                            "deny": ["Read", "Edit", "Write", "Grep", "Glob", "Bash", "NotebookEdit"]},
            "hooks": {e: hook(e) for e in ["SessionStart", "Stop", "PreCompact", "PostCompact", "SessionEnd"]},
        }))
        self.model = FakeModel(out)
        self.sock = f"/tmp/p100-{uuid.uuid4().hex[:8]}.sock"
        self.tmux = ["tmux", "-S", self.sock, "-f", "/dev/null"]
        self.steps = []

    def argv(self, extra):
        return ["sandbox-exec", "-f", str(self.out / "no-egress.sb"), self.claude,
                "--model", "claude-opus-5-5", "--tools", "",
                "--mcp-config", str(self.out / "mcp.json"), "--strict-mcp-config",
                "--settings", str(self.out / "settings.json"),
                "--setting-sources", "user,project,local", "--permission-mode", "default"] + extra

    def env(self):
        return {"HOME": str(self.home), "PATH": os.environ["PATH"], "TERM": "xterm-256color",
                "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{self.model.port}", "ANTHROPIC_API_KEY": KEY,
                "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1", "DISABLE_AUTOUPDATER": "1",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"}

    def start(self, name, extra):
        inner = "cd " + shlex.quote(str(self.cwd)) + " && env -i " + \
            " ".join(f"{k}={shlex.quote(v)}" for k, v in self.env().items()) + " " + \
            " ".join(shlex.quote(a) for a in self.argv(extra)) + "; echo P100-EXIT=$?; sleep 600"
        subprocess.run(self.tmux + ["new-session", "-d", "-s", name, "-x", "160", "-y", "45", inner],
                       check=True)
        time.sleep(10)

    def pane(self, name, label):
        text = subprocess.run(self.tmux + ["capture-pane", "-p", "-t", name],
                              capture_output=True, text=True).stdout
        (self.out / f"pane-{label}.txt").write_text(text)
        return [line for line in text.splitlines() if line.strip()][-8:]

    def keys(self, name, text, enter=True):
        subprocess.run(self.tmux + ["send-keys", "-t", name, "-l", text], check=True)
        if enter:
            time.sleep(0.5)
            subprocess.run(self.tmux + ["send-keys", "-t", name, "Enter"], check=True)

    def key(self, name, key):
        subprocess.run(self.tmux + ["send-keys", "-t", name, key], check=True)

    def wait_requests(self, before, timeout=60):
        deadline = time.time() + timeout
        while time.time() < deadline and len(self.model.requests) <= before:
            time.sleep(0.3)
        time.sleep(3)

    def server_alive(self):
        out = subprocess.run(["pgrep", "-f", str(self.server_dir / "tiny_server.py")],
                             capture_output=True, text=True).stdout.split()
        return out

    def hooks(self):
        if not self.hook_log.exists():
            return []
        return [json.loads(line) for line in self.hook_log.read_text().splitlines()]

    def step(self, label, **facts):
        facts["label"] = label
        self.steps.append(facts)
        print(json.dumps(facts, ensure_ascii=False))

    def requests_with(self, mark, since=0):
        return [i for i, r in enumerate(self.model.requests[since:], since) if mark in json.dumps(r)]

    def run(self):
        s1 = str(uuid.uuid4())
        name = "probe"
        try:
            self.start(name, ["--session-id", s1])
            self.step("startup", pane=self.pane(name, "1-idle-empty"), hooks=self.hooks(),
                      startup_hook_in=self.requests_with("HOOK-SessionStart-startup-STDOUT"))

            before = len(self.model.requests)
            self.keys(name, "hello one")
            self.wait_requests(before)
            self.step("first-turn", requests=len(self.model.requests),
                      startup_hook_in=self.requests_with("HOOK-SessionStart-startup-STDOUT"),
                      hooks=[h["event"] for h in self.hooks()])

            self.keys(name, "a draft that is never sent", enter=False)
            time.sleep(1)
            draft = self.pane(name, "2-idle-draft")
            self.key(name, "C-u")
            time.sleep(1)
            self.step("draft", pane=draft, after_ctrl_u=self.pane(name, "3-after-ctrl-u"))

            before = len(self.model.requests)
            self.keys(name, "SLOW one")
            time.sleep(3)
            busy = self.pane(name, "4-busy")
            self.wait_requests(before, 30)
            self.step("busy", pane=busy)

            # The Runtime goes away: the server process dies and its one restart fails.
            (self.server_dir / "dead").write_text("")
            pids = self.server_alive()
            for pid in pids:
                subprocess.run(["kill", "-TERM", pid])
            time.sleep(8)
            self.step("server-killed", killed=pids, alive_after=self.server_alive(),
                      pane=self.pane(name, "5-after-kill"))

            # Typed while the Runtime is still away: what does failure look like?
            self.keys(name, "/mcp reconnect ccnm")
            time.sleep(6)
            self.step("reconnect-while-dead", alive=self.server_alive(),
                      pane=self.pane(name, "5b-reconnect-while-dead"))

            # Typed while the model is busy: run at once, queued, or lost?
            before = len(self.model.requests)
            self.keys(name, "SLOW two")
            time.sleep(2)
            (self.server_dir / "dead").unlink()
            self.keys(name, "/mcp reconnect ccnm")
            time.sleep(1.5)
            busy_typed = self.pane(name, "5c-busy-typed")
            alive_busy = self.server_alive()
            self.wait_requests(before, 30)
            time.sleep(4)
            self.step("reconnect-while-busy", pane_while_busy=busy_typed, alive_while_busy=alive_busy,
                      alive_after_turn=self.server_alive(), pane=self.pane(name, "5d-after-busy"))
            # Back to the plain case for the next step.
            (self.server_dir / "dead").write_text("")
            for pid in self.server_alive():
                subprocess.run(["kill", "-TERM", pid])
            time.sleep(8)

            # The network is back, and somebody types the reconnect.
            (self.server_dir / "dead").unlink()
            t0 = time.time()
            self.keys(name, "/mcp reconnect ccnm")
            alive_at = None
            while time.time() - t0 < 20:
                if self.server_alive():
                    alive_at = round(time.time() - t0, 2)
                    break
                time.sleep(0.2)
            time.sleep(3)
            self.step("reconnect", alive_after_s=alive_at, alive=self.server_alive(),
                      pane=self.pane(name, "6-after-reconnect"))

            before = len(self.model.requests)
            self.keys(name, "is the tool back")
            self.wait_requests(before)
            last = self.model.requests[-1] if self.model.requests else {}
            self.step("tools-after-reconnect",
                      tool_names=[t.get("name") for t in last.get("tools") or []])

            before = len(self.model.requests)
            self.keys(name, "/compact")
            self.wait_requests(before, 60)
            time.sleep(3)
            before2 = len(self.model.requests)
            self.keys(name, "after compact")
            self.wait_requests(before2)
            self.step("compact", hooks=[(h["event"], h["input"].get("source") or h["input"].get("trigger"),
                                         h["input"].get("session_id")) for h in self.hooks()],
                      compact_hook_in=self.requests_with("HOOK-SessionStart-compact-STDOUT", before),
                      pane=self.pane(name, "7-after-compact"))

            before = len(self.model.requests)
            self.keys(name, "write the HANDOFF now")
            self.wait_requests(before)
            stops = [h["input"] for h in self.hooks() if h["event"] == "Stop"]
            self.step("handoff-turn", last_stop=stops[-1] if stops else None)

            before = len(self.model.requests)
            self.keys(name, "/clear")
            time.sleep(5)
            self.keys(name, "first after clear")
            self.wait_requests(before)
            self.step("clear", hooks=[(h["event"], h["input"].get("source") or h["input"].get("reason"),
                                       h["input"].get("session_id")) for h in self.hooks()][-6:],
                      clear_hook_in=self.requests_with("HOOK-SessionStart-clear-STDOUT", before))
            cleared = [h["input"]["session_id"] for h in self.hooks()
                       if h["event"] == "SessionStart" and h["input"].get("source") == "clear"]

            self.keys(name, "/exit")
            time.sleep(5)
            subprocess.run(self.tmux + ["kill-session", "-t", name], capture_output=True)

            s2 = str(uuid.uuid4())
            before = len(self.model.requests)
            self.start("probe2", ["--session-id", s2, "--resume", cleared[-1] if cleared else s1,
                                  "--fork-session"])
            self.keys("probe2", "after resume")
            self.wait_requests(before)
            self.step("resume-fork", s2=s2, resumed=cleared[-1] if cleared else s1,
                      hooks=[(h["event"], h["input"].get("source"), h["input"].get("session_id"))
                             for h in self.hooks()][-4:],
                      resume_hook_in=self.requests_with("HOOK-SessionStart-resume-STDOUT", before)
                      + self.requests_with("HOOK-SessionStart-fork-STDOUT", before),
                      pane=self.pane("probe2", "8-resume"))
        finally:
            subprocess.run(self.tmux + ["kill-server"], capture_output=True)
            try:
                os.unlink(self.sock)
            except OSError:
                pass
            self.model.server.shutdown()
        return {"session": s1, "steps": self.steps,
                "transcripts": sorted(str(p.relative_to(self.out)) for p in self.out.rglob("*.jsonl")
                                      if "projects" in str(p))}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("out")
    parser.add_argument("--claude", action="append", required=True, help="name=path")
    args = parser.parse_args()
    out = Path(args.out).resolve()
    out.mkdir(parents=True)
    summary = {}
    for spec in args.claude:
        name, path = spec.split("=", 1)
        version = subprocess.run([path, "--version"], capture_output=True, text=True).stdout.strip()
        summary[name] = {"version": version, **Probe(out / name, path).run()}
    (out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")


if __name__ == "__main__":
    main()
