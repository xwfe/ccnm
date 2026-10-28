#!/usr/bin/env python3
"""P112.1 零额度：真的 Claude Code、Codex CLI 接本机假模型接口，ccnm 的工具经 `ccnm mcp bridge`
连到 Windows 上的项目，跑一个完整的工具回合：读文件、加文件、跑命令、调项目里转接的 MCP server。

    python3 -I p112-fake-model.py claude --bridge "<ccnm 绝对路径> --config X mcp bridge proj --mode coding" --out 目录
    python3 -I p112-fake-model.py codex  --bridge "..." --out 目录

假模型按对话里已经有几个工具结果决定下一步，每一步检查上一步的结果里有没有该有的东西，
最后回一句话收尾；每次请求原样存进 --out。API key 是假的，模型接口只在 127.0.0.1。
"""
from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import tempfile
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

# 每一步：(ccnm 工具名, 参数, 下一次请求里这一步的结果要包含的字)
STEPS = [
    ("read_file", {"path": "README.md"}, "needle-p112"),
    ("apply_patch", {"files": [{"op": "add", "path": "{who}.txt", "content": "written by {who}\n"}]}, "{who}.txt"),
    ("exec_command", {"shell": "cat {who}.txt && git status --porcelain"}, "written by {who}"),
    ("call_mcp_tool", {"server": "echo", "tool": "echo", "arguments": {"text": "{who}"}}, "echo-p112: {who}"),
]


def fill(value, who):
    return json.loads(json.dumps(value).replace("{who}", who))


class Script:
    """记下每一步的结果对不对。"""

    def __init__(self, who):
        self.who = who
        self.checks = []

    def check(self, step, said):
        expect = STEPS[step][2].replace("{who}", self.who)
        self.checks.append({"tool": STEPS[step][0], "ok": expect in said, "expect": expect, "got": said[:600]})


def serve(handler_factory):
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler_factory)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


# -- Claude Code（Anthropic Messages，SSE） --

def claude_events(n, block):
    events = [{"type": "message_start", "message": {
        "id": f"msg_{n}", "type": "message", "role": "assistant", "model": "claude-probe", "content": [],
        "stop_reason": None, "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 1}}}]
    if block["type"] == "text":
        events += [{"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                   {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": block["text"]}}]
        stop = "end_turn"
    else:
        events += [{"type": "content_block_start", "index": 0, "content_block": {
                        "type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}},
                   {"type": "content_block_delta", "index": 0, "delta": {
                        "type": "input_json_delta", "partial_json": json.dumps(block["input"])}}]
        stop = "tool_use"
    events += [{"type": "content_block_stop", "index": 0},
               {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None},
                "usage": {"output_tokens": 1}},
               {"type": "message_stop"}]
    return events


def claude_results(request):
    """对话里每个 tool_result 的文字，按出现顺序。"""
    said = []
    for message in request.get("messages") or []:
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for part in content:
            if part.get("type") == "tool_result":
                inner = part.get("content")
                if isinstance(inner, list):
                    inner = "\n".join(p.get("text", "") for p in inner if isinstance(p, dict))
                said.append(str(inner))
    return said


def run_claude(args, out: Path, script: Script) -> dict:
    lock = threading.Lock()
    seen = {"requests": 0, "tools": []}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def reply(self, body: bytes, kind: str):
            self.send_response(200)
            self.send_header("content-type", kind)
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            self.reply(b'{"data": []}', "application/json")

        def do_POST(self):
            raw = self.rfile.read(int(self.headers.get("content-length") or 0))
            if "/v1/messages" not in self.path or "count_tokens" in self.path:
                return self.reply(b'{"input_tokens": 1}', "application/json")
            request = json.loads(raw)
            with lock:
                n = seen["requests"]
                seen["requests"] += 1
            (out / f"request-{n}.json").write_bytes(raw)
            tools = [t.get("name") for t in request.get("tools") or []]
            if not any(name and name.startswith("mcp__ccnm__") for name in tools):
                block = {"type": "text", "text": "ok"}   # 旁路的小请求（起标题之类）
            else:
                seen["tools"] = tools
                results = claude_results(request)
                if results:
                    script.check(len(results) - 1, results[-1])
                step = len(results)
                if step < len(STEPS):
                    name, arguments, _ = STEPS[step]
                    block = {"type": "tool_use", "id": f"toolu_{step}", "name": f"mcp__ccnm__{name}",
                             "input": fill(arguments, script.who)}
                else:
                    block = {"type": "text", "text": "p112 round done"}
            events = claude_events(n, block)
            if not request.get("stream"):
                content = [block if block["type"] == "text" else
                           {"type": "tool_use", "id": block["id"], "name": block["name"], "input": block["input"]}]
                body = json.dumps({"id": f"msg_{n}", "type": "message", "role": "assistant", "model": "claude-probe",
                                   "content": content, "stop_reason": events[-2]["delta"]["stop_reason"],
                                   "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 1}}).encode()
                return self.reply(body, "application/json")
            self.reply(sse(events), "text/event-stream")

    server = serve(Handler)
    home = Path(tempfile.mkdtemp(prefix="p112-claude-home-", dir=out))
    bridge = shlex.split(args.bridge)
    mcp = out / "mcp.json"
    mcp.write_text(json.dumps({"mcpServers": {"ccnm": {
        "type": "stdio", "command": bridge[0], "args": bridge[1:], "alwaysLoad": True,
        "env": {"XDG_STATE_HOME": os.environ.get("XDG_STATE_HOME", "")}}}}))
    cmd = ["claude", "--print", "--output-format", "json", "--tools", "",
           "--mcp-config", str(mcp), "--strict-mcp-config", "--allowedTools", "mcp__ccnm",
           "--permission-mode", "acceptEdits", "--session-id", str(uuid.uuid4()), "--no-session-persistence"]
    env = {"HOME": str(home), "PATH": os.environ["PATH"], "USER": os.environ.get("USER", ""),
           "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_address[1]}",
           "ANTHROPIC_API_KEY": "sk-ant-p112-not-a-real-key",
           "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1", "DISABLE_AUTOUPDATER": "1",
           "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"}
    done = subprocess.run(cmd, cwd=home, env=env, input=b"do the p112 round", capture_output=True, timeout=600)
    (out / "claude.stdout").write_bytes(done.stdout)
    (out / "claude.stderr").write_bytes(done.stderr)
    server.shutdown()
    version = subprocess.run(["claude", "--version"], capture_output=True, text=True).stdout.strip()
    return {"host": version, "rc": done.returncode, "requests": seen["requests"],
            "ccnm_tools": sorted(t for t in seen["tools"] if t and t.startswith("mcp__ccnm__"))}


# -- Codex（Responses，SSE） --

def codex_tools(request):
    """请求里 ccnm 的工具：(调用时用的 name, namespace)。顶层 tools 和 input 里的 additional_tools 都看。"""
    tools = list(request.get("tools") or [])
    for item in request.get("input") or []:
        if isinstance(item, dict) and item.get("type") == "additional_tools":
            tools += item.get("tools") or []
    found = {}
    for tool in tools:
        if tool.get("type") == "namespace" and "ccnm" in (tool.get("name") or ""):
            for inner in tool.get("tools") or []:
                found[inner.get("name")] = (inner.get("name"), tool.get("name"))
        elif "ccnm" in (tool.get("name") or ""):
            short = tool["name"].split("__")[-1]
            found[short] = (tool["name"], None)
    return found


def codex_results(request):
    said = []
    for item in request.get("input") or []:
        if isinstance(item, dict) and item.get("type") in ("function_call_output", "custom_tool_call_output"):
            output = item.get("output")
            if isinstance(output, list):
                output = "\n".join(p.get("text", "") for p in output if isinstance(p, dict))
            elif isinstance(output, dict):
                output = output.get("content") or json.dumps(output)
            said.append(str(output))
    return said


def run_codex(args, out: Path, script: Script) -> dict:
    lock = threading.Lock()
    seen = {"requests": 0, "tools": {}}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def reply(self, body: bytes, kind: str):
            self.send_response(200)
            self.send_header("content-type", kind)
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            self.reply(json.dumps({"data": [], "models": []}).encode(), "application/json")

        def do_POST(self):
            raw = self.rfile.read(int(self.headers.get("content-length") or 0))
            request = json.loads(raw)
            with lock:
                n = seen["requests"]
                seen["requests"] += 1
            (out / f"request-{n}.json").write_bytes(raw)
            tools = codex_tools(request)
            if tools:
                seen["tools"] = tools
            results = codex_results(request)
            if results:
                script.check(len(results) - 1, results[-1])
            step = len(results)
            rid = f"resp_{n}"
            if tools and step < len(STEPS):
                name, arguments, _ = STEPS[step]
                call_name, namespace = tools.get(name, (f"mcp__ccnm__{name}", None))
                item = {"type": "function_call", "id": f"fc_{n}", "call_id": f"call_{step}", "name": call_name,
                        "arguments": json.dumps(fill(arguments, script.who))}
                if namespace:
                    item["namespace"] = namespace
            else:
                item = {"type": "message", "role": "assistant", "id": f"msg_{n}",
                        "content": [{"type": "output_text", "text": "p112 round done"}]}
            self.reply(sse([
                {"type": "response.created", "response": {"id": rid}},
                {"type": "response.output_item.done", "item": item},
                {"type": "response.completed", "response": {"id": rid, "usage": {
                    "input_tokens": 0, "input_tokens_details": None, "output_tokens": 0,
                    "output_tokens_details": None, "total_tokens": 0}}},
            ]), "text/event-stream")

    server = serve(Handler)
    home = Path(tempfile.mkdtemp(prefix="p112-codex-home-", dir=out))
    (home / ".codex").mkdir()
    work = home / "work"
    work.mkdir()
    bridge = shlex.split(args.bridge)
    provider = "p112mock"
    cmd = ["codex", "exec", "--ignore-user-config", "--skip-git-repo-check", "--ephemeral", "--json",
           "--color", "never", "--model", "gpt-5.1-codex", "--sandbox", "read-only",
           "-c", 'approval_policy="never"',
           "-c", f'model_provider="{provider}"',
           "-c", f'model_providers.{provider}.name="{provider}"',
           "-c", f'model_providers.{provider}.base_url="http://127.0.0.1:{server.server_address[1]}/v1"',
           "-c", f'model_providers.{provider}.wire_api="responses"',
           "-c", f"model_providers.{provider}.requires_openai_auth=false",
           "-c", f"mcp_servers.ccnm.command={json.dumps(bridge[0])}",
           "-c", f"mcp_servers.ccnm.args={json.dumps(bridge[1:])}",
           "-c", f"mcp_servers.ccnm.env={{XDG_STATE_HOME={json.dumps(os.environ.get('XDG_STATE_HOME', ''))}}}",
           "-c", "mcp_servers.ccnm.required=true",
           "-c", "mcp_servers.ccnm.tool_timeout_sec=1870",
           "-c", 'mcp_servers.ccnm.default_tools_approval_mode="approve"',
           "-"]
    env = {"HOME": str(home), "CODEX_HOME": str(home / ".codex"), "PATH": os.environ["PATH"],
           "USER": os.environ.get("USER", "")}
    # `input` closes stdin once written: codex exec reads it to EOF before it
    # starts, and an inherited stdin that never closes hangs it (P80).
    done = subprocess.run(cmd, cwd=work, env=env, input=b"do the p112 round", capture_output=True, timeout=600)
    (out / "codex.stdout").write_bytes(done.stdout)
    (out / "codex.stderr").write_bytes(done.stderr)
    server.shutdown()
    version = subprocess.run(["codex", "--version"], capture_output=True, text=True).stdout.strip()
    return {"host": version, "rc": done.returncode, "requests": seen["requests"],
            "ccnm_tools": sorted(seen["tools"])}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("host", choices=["claude", "codex"])
    parser.add_argument("--bridge", required=True)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    script = Script(args.host)
    summary = (run_claude if args.host == "claude" else run_codex)(args, args.out, script)
    summary["checks"] = script.checks
    summary["ok"] = len(script.checks) == len(STEPS) and all(c["ok"] for c in script.checks)
    (args.out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({k: v for k, v in summary.items() if k != "checks"}, ensure_ascii=False))
    for check in script.checks:
        print(f"{'PASS' if check['ok'] else 'FAIL'} {check['tool']}: {check['got'][:200]!r}")
    return 0 if summary["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
