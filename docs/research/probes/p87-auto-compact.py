#!/usr/bin/env python3
"""P87：受管 Claude 会话的自动压缩窗口和压缩指令，真实 Claude Code 认不认（零额度）。

做法：真实 Claude Code 接本机假模型（ANTHROPIC_BASE_URL + 假 key），HOME 是空临时目录
（预先写好跳过引导、目录信任、API key 确认的最小配置），sandbox-exec 只放本机出站。
启动参数照 ccnm 的 launch_cmd：--tools "" + 一个内嵌的最小 stdio MCP server 冒充
ccnm、--settings <会话设置>、--setting-sources user,project,local，cwd 是一个当作
ccnm 工作目录的临时目录。

交互式场景在单独的 tmux server 里跑：连发三轮消息，假模型每次回复都在用量里报 N 个
token，看有没有出现压缩（请求里出现总结指令，下一次请求以"从上一段对话继续"开头），
以及压缩请求里有没有 ccnm 的指令。`-p` 测不出主动压缩（30 万到 90 万都不压，只在约
98 万快撞上限时压），所以只用它测"设置文件多一个键时允许表还生效"。

会话设置按 crates/ccnm-core/src/provider/claude/policy.rs 拼：压缩指令从源码里的
COMPACT_INSTRUCTIONS 常量读，形状与单元测试 settings_compact_early_and_say_what_to_keep
锁定的一致。P88 起还看压缩请求里是不是指令全文、压缩后那条消息带不带转录路径
（受管会话读不到它，指令为此要求总结自给自足）。

用法（macOS，要 tmux 和 sandbox-exec）：
  python3 docs/research/probes/p87-auto-compact.py <仓库外的新输出目录> \\
      --claude cc-286=/path/to/2.1.286/claude [--claude cc-296=...]
"""

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
POLICY = REPO / "crates/ccnm-core/src/provider/claude/policy.rs"
KEY = "sk-ant-probe-not-a-real-key-000000000000"
SANDBOX = """(version 1)
(allow default)
(deny network-outbound)
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket))
"""
SUMMARY_MARK = "detailed summary of the conversation so far"
CONTINUED_MARK = "being continued from a previous conversation"
TRANSCRIPT_MARK = "read the full transcript at"
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
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "TINY-OK"}]}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
'''


def compact_instructions():
    src = POLICY.read_text()
    m = re.search(r'pub const COMPACT_INSTRUCTIONS: &str = "\\\n(.*?)";', src, re.S)
    if not m or "\\" in m.group(1) or "'" in m.group(1):
        raise SystemExit("COMPACT_INSTRUCTIONS not found in the expected shape; update the probe")
    return m.group(1)


def window():
    m = re.search(r"pub const AUTO_COMPACT_WINDOW: u32 = ([0-9_]+);", POLICY.read_text())
    return int(m.group(1).replace("_", ""))


PERMISSIONS = {"allow": ["mcp__ccnm__read_file"],
               "deny": ["Read", "Edit", "Write", "Grep", "Glob", "Bash", "NotebookEdit", "Skill"]}


def ccnm_settings():
    return {"permissions": PERMISSIONS, "autoCompactWindow": window(),
            "hooks": {"PreCompact": [{"hooks": [{
                "type": "command",
                "command": "printf '%s\\n' '" + compact_instructions() + "'"}]}]}}


def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


def reply(n, block, stop, usage):
    if block["type"] == "text":
        start = {"type": "text", "text": ""}
        delta = {"type": "text_delta", "text": block["text"]}
    else:
        start = {"type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}
        delta = {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}
    return [
        {"type": "message_start", "message": {
            "id": f"msg_{n}", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
            "content": [], "stop_reason": None, "stop_sequence": None, "usage": usage}},
        {"type": "content_block_start", "index": 0, "content_block": start},
        {"type": "content_block_delta", "index": 0, "delta": delta},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None}, "usage": usage},
        {"type": "message_stop"},
    ]


class FakeModel:
    """Every request is saved; `script(n, request, body)` returns (block, stop, usage)."""

    def __init__(self, run, script):
        self.run, self.script, self.log, self.step = run, script, [], 0
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
                n = model.step
                model.step += 1
                (model.run / f"request-{n}.json").write_bytes(raw)
                body = raw.decode(errors="replace")
                block, stop, usage = model.script(n, json.loads(raw), body)
                req = json.loads(raw)
                if not req.get("stream"):
                    content = [block if block["type"] == "text" else {
                        "type": "tool_use", "id": block["id"], "name": block["name"], "input": block["input"]}]
                    return self.send_json({"id": f"msg_{n}", "type": "message", "role": "assistant",
                                           "model": "claude-opus-5-5", "content": content,
                                           "stop_reason": stop, "stop_sequence": None, "usage": usage})
                payload = sse(reply(n, block, stop, usage))
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.port = self.server.server_address[1]


def prepare(run, settings, workspace_local):
    run.mkdir(parents=True)
    home = run / "home"
    home.mkdir()
    cwd = (run / "workspace").resolve()
    (cwd / ".claude").mkdir(parents=True)
    if workspace_local is not None:
        (cwd / ".claude" / "settings.local.json").write_text(json.dumps(workspace_local))
    (home / ".claude.json").write_text(json.dumps({
        "hasCompletedOnboarding": True, "theme": "dark", "numStartups": 5,
        "customApiKeyResponses": {"approved": [KEY[-20:]], "rejected": []},
        "projects": {str(cwd): {"hasTrustDialogAccepted": True, "hasCompletedProjectOnboarding": True,
                                "projectOnboardingSeenCount": 5}},
    }))
    (run / "no-egress.sb").write_text(SANDBOX)
    (run / "tiny_server.py").write_text(TINY_SERVER)
    (run / "mcp.json").write_text(json.dumps({"mcpServers": {"ccnm": {
        "type": "stdio", "command": sys.executable, "args": [str(run / "tiny_server.py")]}}}))
    (run / "settings.json").write_text(json.dumps(settings, indent=2))
    return home, cwd


def argv(run, claude):
    return ["sandbox-exec", "-f", str(run / "no-egress.sb"), claude, "--model", "claude-opus-5-5",
            "--tools", "", "--mcp-config", str(run / "mcp.json"), "--strict-mcp-config",
            "--settings", str(run / "settings.json"), "--setting-sources", "user,project,local",
            "--permission-mode", "default", "--session-id", str(uuid.uuid4())]


def env(home, model):
    return {"HOME": str(home), "PATH": os.environ["PATH"], "TERM": "xterm-256color",
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{model.port}", "ANTHROPIC_API_KEY": KEY,
            "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1", "DISABLE_AUTOUPDATER": "1",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"}


def interactive(out, claude, name, settings, tokens, workspace_local=None):
    """Three turns, every reply reporting `tokens` of context. Did it compact, and with what?"""
    run = out / name

    def script(n, req, body):
        main = "mcp__ccnm__read_file" in [t.get("name") for t in req.get("tools") or []]
        kind = ("main" if main else "side") + ("+SUMMARY" if SUMMARY_MARK in body else "") + \
            ("+CONTINUED" if CONTINUED_MARK in body else "")
        model.log.append({"n": n, "kind": kind, "messages": len(req.get("messages") or [])})
        if SUMMARY_MARK in body:
            return {"type": "text", "text": "<analysis>a</analysis><summary>probe summary</summary>"}, \
                "end_turn", {"input_tokens": 1, "output_tokens": 1}
        if main:
            return {"type": "text", "text": f"reply {n}"}, "end_turn", \
                {"input_tokens": 1, "cache_read_input_tokens": tokens, "output_tokens": 1}
        return {"type": "text", "text": "side ok"}, "end_turn", {"input_tokens": 1, "output_tokens": 1}

    model = FakeModel(run, script)
    home, cwd = prepare(run, settings, workspace_local)
    inner = "cd " + shlex.quote(str(cwd)) + " && exec env -i " + \
        " ".join(f"{k}={shlex.quote(v)}" for k, v in env(home, model).items()) + " " + \
        " ".join(shlex.quote(a) for a in argv(run, claude))
    # Its own tmux server; the socket path must stay short (104-byte limit).
    sock = f"/tmp/p87-{uuid.uuid4().hex[:8]}.sock"
    tmux = ["tmux", "-S", sock, "-f", "/dev/null"]
    try:
        subprocess.run(tmux + ["new-session", "-d", "-s", "probe", "-x", "200", "-y", "50", inner], check=True)
        time.sleep(12)
        for i in range(1, 4):
            before = len(model.log)
            subprocess.run(tmux + ["send-keys", "-t", "probe", "-l", f"hello number {i}"], check=True)
            time.sleep(1)
            subprocess.run(tmux + ["send-keys", "-t", "probe", "Enter"], check=True)
            deadline = time.time() + 60
            while time.time() < deadline and len(model.log) <= before:
                time.sleep(0.5)
            time.sleep(8)
        pane = subprocess.run(tmux + ["capture-pane", "-p", "-t", "probe"], capture_output=True, text=True).stdout
        (run / "pane.txt").write_text(pane)
    finally:
        subprocess.run(tmux + ["kill-server"], capture_output=True)
        try:
            os.unlink(sock)
        except OSError:
            pass
        model.server.shutdown()
    requests = [p.read_text() for p in sorted(run.glob("request-*.json"),
                                               key=lambda p: int(p.stem.split("-")[1]))]
    summaries = [t for t in requests if SUMMARY_MARK in t]
    extra = None
    if summaries:
        text = summaries[0]
        continued = [t for t in requests if CONTINUED_MARK in t]
        extra = {"additional_instructions": "Additional Instructions:" in text,
                 "ccnm_full_text": json.dumps(compact_instructions())[1:-1] in text,
                 "transcript_pointer_after": bool(continued) and TRANSCRIPT_MARK in continued[0]}
    return {"scenario": name, "reported_tokens": tokens, "requests": model.log,
            "compacted": any("SUMMARY" in r["kind"] for r in model.log),
            "continued": any("CONTINUED" in r["kind"] for r in model.log),
            "compaction_request": extra}


def print_mode(out, claude, name, settings):
    """One tool call in -p: does the settings file still allow it (nobody can answer a prompt)?"""
    run = out / name

    def script(n, req, body):
        main = "mcp__ccnm__read_file" in [t.get("name") for t in req.get("tools") or []]
        results = [p for m in req.get("messages", []) if isinstance(m.get("content"), list)
                   for p in m["content"] if p.get("type") == "tool_result"]
        model.log.append({"n": n, "main": main, "tool_results": results})
        if main and not results:
            return {"type": "tool_use", "id": "toolu_p87", "name": "mcp__ccnm__read_file",
                    "input": {"path": "x"}}, "tool_use", {"input_tokens": 1, "output_tokens": 1}
        return {"type": "text", "text": "done"}, "end_turn", {"input_tokens": 1, "output_tokens": 1}

    model = FakeModel(run, script)
    home, cwd = prepare(run, settings, None)
    cmd = argv(run, claude) + ["--print", "--output-format", "json", "--permission-prompts", "none",
                               "--no-session-persistence"]
    proc = subprocess.run(cmd, cwd=cwd, env=env(home, model), input=b"call the tool",
                          capture_output=True, timeout=180)
    model.server.shutdown()
    (run / "stdout").write_bytes(proc.stdout)
    (run / "stderr").write_bytes(proc.stderr)
    results = [r for entry in model.log for r in entry["tool_results"]]
    text = json.dumps(results[0]) if results else ""
    return {"scenario": name, "rc": proc.returncode, "tool_ran": "TINY-OK" in text,
            "tool_result": text[:300], "stderr": proc.stderr.decode(errors="replace")[-300:]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("out")
    parser.add_argument("--claude", action="append", required=True, help="name=path")
    args = parser.parse_args()
    out = Path(args.out).resolve()
    out.mkdir(parents=True)
    w = window()
    old = {"permissions": PERMISSIONS}
    new = ccnm_settings()
    summary = {"auto_compact_window": w, "hosts": {}}
    for spec in args.claude:
        name, path = spec.split("=", 1)
        version = subprocess.run([path, "--version"], capture_output=True, text=True).stdout.strip()
        host = out / name
        runs = [
            interactive(host, path, "old-settings-above", old, w - 10_000),
            interactive(host, path, "ccnm-above", new, w - 10_000),
            interactive(host, path, "ccnm-below", new, w - 70_000),
            interactive(host, path, "ccnm-env-200k-190k", new, 190_000,
                        {"env": {"CLAUDE_CODE_AUTO_COMPACT_WINDOW": "200000"}}),
            print_mode(host, path, "print-unknown-key", {"permissions": PERMISSIONS,
                                                         "ccnmProbeUnknownKey": {"x": 1}}),
            print_mode(host, path, "print-ccnm-settings", new),
        ]
        summary["hosts"][name] = {"version": version, "runs": runs}
    (out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
