#!/usr/bin/env python3
"""P92：受管 Claude 交互会话能不能"接着上次的对话"开（零额度）。

ccnm 起 Claude 时用 `--session-id <ccnm 会话 id>`，让一个 id 同时叫 ccnm 的会话目录和
Claude 的转录。续接要不破坏这一条，所以要量的是：
  1. `--resume <上次 id> --fork-session --session-id <新 id>` 在交互界面里认不认，
     发给模型的第一条请求带不带上次的对话，新转录是不是落在新 id 上、旧转录动没动；
  2. 不带 `--fork-session` 时 `--session-id` 配 `--resume` 报什么；
  3. 上次的 id 找不到时报什么、退出码多少（ccnm 要在这之前自己拦住）；
  4. 按 ccnm 的参数顺序（--session-id 新 --resume 旧 --fork-session）带开场 prompt 时，
     prompt 是开场消息而不是被 --resume 吃掉；
  5. 设了 CLAUDE_CONFIG_DIR 时，转录落在 $CLAUDE_CONFIG_DIR/projects/*/<id>.jsonl
     （ccnm 按这个位置判断上次的会话有没有留下对话）。

做法同 p87-auto-compact.py：真实 Claude Code 接本机假模型（ANTHROPIC_BASE_URL + 假 key），
HOME 是空临时目录（预先写好跳过引导、目录信任、API key 确认的最小配置），sandbox-exec
只放本机出站；启动参数照 ccnm 的 launch_cmd（--tools "" + 一个冒充 ccnm 的最小 stdio
MCP server、--settings、--setting-sources、--permission-mode、--session-id），每个场景
放在单独的 tmux server 里，命令结束后打印退出码再停住，好抓屏幕。

用法（macOS，要 tmux 和 sandbox-exec）：
  python3 docs/research/probes/p92-resume.py <仓库外的新输出目录> \\
      --claude cc-286=/path/to/2.1.286/claude [--claude ...]
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
MARK = "P92-MARKER-FROM-THE-FIRST-SESSION"
OPENING = "P92-OPENING-PROMPT carry on"
SANDBOX = """(version 1)
(allow default)
(deny network-outbound)
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket))
"""
TINY_SERVER = r'''
import json, sys
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
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
'''
SETTINGS = {"permissions": {"allow": ["mcp__ccnm__read_file"],
                            "deny": ["Read", "Edit", "Write", "Grep", "Glob", "Bash", "NotebookEdit"]}}


def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


class FakeModel:
    """Answers every main-loop request with `reply <n>` and keeps what it was sent."""

    def __init__(self, out):
        self.out, self.main = out, []
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
                main = "mcp__ccnm__read_file" in [t.get("name") for t in req.get("tools") or []]
                text = "side ok"
                if main:
                    n = len(model.main)
                    model.main.append(req)
                    (model.out / f"main-{n}.json").write_bytes(raw)
                    text = f"reply {n}"
                usage = {"input_tokens": 1, "output_tokens": 1}
                if not req.get("stream"):
                    return self.send_json({"id": "msg", "type": "message", "role": "assistant",
                                           "model": "claude-opus-5-5",
                                           "content": [{"type": "text", "text": text}],
                                           "stop_reason": "end_turn", "stop_sequence": None,
                                           "usage": usage})
                payload = sse([
                    {"type": "message_start", "message": {
                        "id": "msg", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
                        "content": [], "stop_reason": None, "stop_sequence": None, "usage": usage}},
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


class Host:
    """One HOME, one ccnm-style cwd, one fake model: the sessions of one workspace."""

    def __init__(self, out, claude, config_dir=False):
        self.out, self.claude = out, claude
        out.mkdir(parents=True)
        self.home = out / "home"
        self.home.mkdir()
        self.cwd = (out / "workspace").resolve()
        self.cwd.mkdir()
        # Claude keeps its state in CLAUDE_CONFIG_DIR when that is set, and in
        # ~/.claude (with ~/.claude.json beside it) when it is not.
        self.config_dir = (out / "profile").resolve() if config_dir else None
        state = self.home
        if self.config_dir:
            self.config_dir.mkdir()
            state = self.config_dir
        (state / ".claude.json").write_text(json.dumps({
            "hasCompletedOnboarding": True, "theme": "dark", "numStartups": 5,
            "customApiKeyResponses": {"approved": [KEY[-20:]], "rejected": []},
            "projects": {str(self.cwd): {"hasTrustDialogAccepted": True,
                                         "hasCompletedProjectOnboarding": True,
                                         "projectOnboardingSeenCount": 5}},
        }))
        (out / "no-egress.sb").write_text(SANDBOX)
        (out / "tiny_server.py").write_text(TINY_SERVER)
        (out / "mcp.json").write_text(json.dumps({"mcpServers": {"ccnm": {
            "type": "stdio", "command": sys.executable, "args": [str(out / "tiny_server.py")]}}}))
        (out / "settings.json").write_text(json.dumps(SETTINGS))
        self.model = FakeModel(out)

    def argv(self, extra):
        return ["sandbox-exec", "-f", str(self.out / "no-egress.sb"), self.claude,
                "--model", "claude-opus-5-5", "--tools", "",
                "--mcp-config", str(self.out / "mcp.json"), "--strict-mcp-config",
                "--settings", str(self.out / "settings.json"),
                "--setting-sources", "user,project,local", "--permission-mode", "default"] + extra

    def env(self):
        env = {"HOME": str(self.home), "PATH": os.environ["PATH"], "TERM": "xterm-256color",
               "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{self.model.port}", "ANTHROPIC_API_KEY": KEY,
               "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1", "DISABLE_AUTOUPDATER": "1",
               "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"}
        if self.config_dir:
            env["CLAUDE_CONFIG_DIR"] = str(self.config_dir)
        return env

    def transcripts(self):
        """Every transcript under HOME, keyed by where it is relative to HOME."""
        return {str(p.relative_to(self.out)): p.stat().st_size for p in self.out.rglob("*.jsonl")}

    def session(self, name, extra, say=None, wait_for_request=False):
        """Run Claude in its own tmux server; type `say` if given; report what happened."""
        inner = "cd " + shlex.quote(str(self.cwd)) + " && env -i " + \
            " ".join(f"{k}={shlex.quote(v)}" for k, v in self.env().items()) + " " + \
            " ".join(shlex.quote(a) for a in self.argv(extra)) + "; echo P92-EXIT=$?; sleep 600"
        sock = f"/tmp/p92-{uuid.uuid4().hex[:8]}.sock"
        tmux = ["tmux", "-S", sock, "-f", "/dev/null"]
        before = len(self.model.main)
        try:
            subprocess.run(tmux + ["new-session", "-d", "-s", "probe", "-x", "200", "-y", "50", inner],
                           check=True)
            time.sleep(10)
            if wait_for_request:
                deadline = time.time() + 60
                while time.time() < deadline and len(self.model.main) <= before:
                    time.sleep(0.5)
                time.sleep(4)
            if say is not None:
                subprocess.run(tmux + ["send-keys", "-t", "probe", "-l", say], check=True)
                time.sleep(1)
                subprocess.run(tmux + ["send-keys", "-t", "probe", "Enter"], check=True)
                deadline = time.time() + 60
                while time.time() < deadline and len(self.model.main) <= before:
                    time.sleep(0.5)
                time.sleep(4)
            pane = subprocess.run(tmux + ["capture-pane", "-p", "-t", "probe"],
                                  capture_output=True, text=True).stdout
            (self.out / f"{name}.pane.txt").write_text(pane)
        finally:
            subprocess.run(tmux + ["kill-server"], capture_output=True)
            try:
                os.unlink(sock)
            except OSError:
                pass
        exit_line = [line for line in pane.splitlines() if line.startswith("P92-EXIT=")]
        sent = self.model.main[before:]
        first = json.dumps(sent[0]) if sent else ""
        return {
            "scenario": name,
            "exited": exit_line[0] if exit_line else None,
            "main_requests": len(sent),
            "first_request_messages": len(sent[0].get("messages") or []) if sent else 0,
            "first_request_has_marker": MARK in first,
        "first_request_has_opening": OPENING in first,
            "pane_tail": [line for line in pane.splitlines() if line.strip()][-6:],
            "transcripts": self.transcripts(),
        }


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
        host = Host(out / name, path)
        first, second, third, opening, missing = (str(uuid.uuid4()) for _ in range(5))
        runs = [
            host.session("first", ["--session-id", first], say=f"remember {MARK}"),
            # ccnm's order: its own --session-id first, then what to continue.
            host.session("resume-fork", ["--session-id", second, "--resume", first, "--fork-session"],
                         say="what did I ask you to remember?"),
            host.session("resume-fork-opening", ["--session-id", opening, "--resume", first,
                                                 "--fork-session", OPENING], say=None, wait_for_request=True),
            host.session("resume-without-fork", ["--resume", first, "--session-id", third]),
            host.session("resume-missing", ["--session-id", str(uuid.uuid4()), "--resume", missing,
                                            "--fork-session"]),
        ]
        host.model.server.shutdown()
        profile = Host(out / f"{name}-config-dir", path, config_dir=True)
        p_first, p_second = str(uuid.uuid4()), str(uuid.uuid4())
        profile_runs = [
            profile.session("first", ["--session-id", p_first], say=f"remember {MARK}"),
            profile.session("resume-fork", ["--session-id", p_second, "--resume", p_first,
                                            "--fork-session"], say="what did I ask you to remember?"),
        ]
        profile.model.server.shutdown()
        summary[name] = {"version": version,
                         "ids": {"first": first, "second": second, "third": third,
                                 "opening": opening, "missing": missing},
                         "runs": runs,
                         "claude_config_dir": {"ids": {"first": p_first, "second": p_second},
                                               "runs": profile_runs}}
    (out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
