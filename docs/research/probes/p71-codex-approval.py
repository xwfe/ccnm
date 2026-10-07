#!/usr/bin/env python3
"""P71：受管 Codex 交互会话里，给 ccnm 的某个 MCP 工具单独设 approval_mode 会怎样。

零额度：模型接口是本机假服务（Code Mode：第一轮回一段调工具的 JS，之后回一句话）；
"ccnm" 是本脚本自己扮的 stdio MCP server，只有 exec_command、read_file 两个工具，
把收到的每次调用记下来；HOME / CODEX_HOME 是临时空目录，进程树套 sandbox-exec，
只许连本机。Codex 跑在一个单独的 tmux server 里（-L p71probe），审批提示要靠人
（或驱动它的人）在那个窗格里按键。

启动参数照 ccnm 交互会话的 `build_launch_cmd`（crates/ccnm-core/src/provider/codex/mod.rs）：
只读 sandbox、approval_policy="on-request"、关掉的 feature 列表、agents 关、Code Mode
（不传 --model），ccnm 这个 server 默认 approve。要测的只是追加的那一项。

用法：
  p71-codex-approval.py serve-model <目录> [--top] 前台跑假模型，端口写进 <目录>/port；
                                                   --top 改成顶层工具的 function_call（实例指定了模型、
                                                   ccnm 不开 Code Mode 时的样子）
  p71-codex-approval.py mcp                        当 MCP server（由 Codex 拉起，日志写 $PROBE_LOG）
  p71-codex-approval.py cmd <目录> <codex> [-c 追加项 ...] [--exec] [--top]
                                                   打印要在 tmux 里跑的命令（--exec 改打 codex exec 的；
                                                   --top 带 --model gpt-5.1-codex、不开 Code Mode）
"""

import json
import os
import shlex
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SANDBOX_PROFILE = """(version 1)
(allow default)
(deny network-outbound)
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket))
"""
# 抄自 ccnm provider/codex/mod.rs 的 DISABLED（P71 开工时的 main）。
DISABLED = [
    "shell_tool", "unified_exec", "unified_exec_tty", "view_image", "apps", "plugins", "hooks",
    "multi_agent", "multi_agent_v2", "browser_use", "computer_use", "image_generation",
    "memories", "workspace_dependencies", "skill_search", "shell_snapshot", "goals", "tool_suggest",
]
# 两次 exec_command：看批准一次之后，第二次还问不问。
SCRIPT = """for (const [label, call] of [
  ["read", () => tools.mcp__ccnm__read_file({path: "README"})],
  ["exec1", () => tools.mcp__ccnm__exec_command({cmd: "echo P71-ONE"})],
  ["exec2", () => tools.mcp__ccnm__exec_command({cmd: "echo P71-TWO"})],
]) {
  try { text(label + ": " + JSON.stringify(await call())); }
  catch (e) { text(label + " threw: " + String(e)); }
}
"""


def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


# --top：按已收到的 function_call_output 个数依次发出，发完回一句话。
PLAN = [("read_file", {"path": "README"}), ("exec_command", {"cmd": "echo P71-ONE"}),
        ("exec_command", {"cmd": "echo P71-TWO"})]


def serve_model(out: Path, top: bool = False):
    out.mkdir(parents=True, exist_ok=True)
    state = {"step": 0, "sent": False}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            body = json.dumps({"data": [], "models": []}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            raw = self.rfile.read(int(self.headers.get("content-length") or 0))
            n = state["step"]
            state["step"] += 1
            (out / f"model-request-{n}.json").write_bytes(raw)
            request = json.loads(raw)
            main_turn = any(isinstance(i, dict) and i.get("type") == "additional_tools"
                            for i in request.get("input", []))
            done = sum(1 for i in request.get("input", [])
                       if isinstance(i, dict) and i.get("type") == "function_call_output")
            if top and done < len(PLAN):
                name, arguments = PLAN[done]
                item = {"type": "function_call", "id": f"fc_{done}", "call_id": f"call_{done}",
                        "namespace": "mcp__ccnm", "name": name, "arguments": json.dumps(arguments)}
            elif top:
                item = {"type": "message", "role": "assistant", "id": f"msg_{n}",
                        "content": [{"type": "output_text", "text": "P71 done"}]}
            elif main_turn and not state["sent"]:
                state["sent"] = True
                item = {"type": "custom_tool_call", "id": "ctc_0", "call_id": "call_0",
                        "name": "exec", "input": SCRIPT}
            else:
                item = {"type": "message", "role": "assistant", "id": f"msg_{n}",
                        "content": [{"type": "output_text", "text": "P71 done"}]}
            rid = f"resp_{n}"
            payload = sse([
                {"type": "response.created", "response": {"id": rid}},
                {"type": "response.output_item.done", "item": item},
                {"type": "response.completed", "response": {"id": rid, "usage": {
                    "input_tokens": 0, "input_tokens_details": None, "output_tokens": 0,
                    "output_tokens_details": None, "total_tokens": 0}}},
            ])
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    (out / "port").write_text(str(server.server_address[1]))
    server.serve_forever()


TOOLS = [
    {"name": "read_file", "description": "Read a file in the workspace.",
     "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}},
     "annotations": {"readOnlyHint": True, "openWorldHint": False}},
    {"name": "exec_command", "description": "Run a shell command on the Runtime.",
     "inputSchema": {"type": "object", "properties": {"cmd": {"type": "string"}}},
     "annotations": {"readOnlyHint": False, "destructiveHint": True, "idempotentHint": False,
                     "openWorldHint": True}},
]


def mcp():
    log = Path(os.environ["PROBE_LOG"])

    def note(entry):
        entry["t"] = round(time.time(), 3)
        with log.open("a") as f:
            f.write(json.dumps(entry) + "\n")

    for line in sys.stdin:
        msg = json.loads(line)
        method, mid = msg.get("method"), msg.get("id")
        note({"method": method, "params": msg.get("params")})
        if mid is None:
            continue
        if method == "initialize":
            result = {"protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
                      "capabilities": {"tools": {}},
                      "serverInfo": {"name": "p71-probe", "version": "0"}}
        elif method == "tools/list":
            result = {"tools": TOOLS}
        elif method == "tools/call":
            name = msg["params"]["name"]
            result = {"content": [{"type": "text", "text": f"probe ran {name}"}]}
        else:
            result = {}
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
        sys.stdout.flush()


def cmd(out: Path, codex: str, extra: list, exec_mode: bool, top: bool = False):
    home, work = out / "home", out / "work"
    (home / ".codex").mkdir(parents=True, exist_ok=True)
    work.mkdir(exist_ok=True)
    profile = out / "no-egress.sb"
    profile.write_text(SANDBOX_PROFILE)
    port = (out / "port").read_text().strip()
    p = "probemock"
    me = str(Path(__file__).resolve())
    argv = ["sandbox-exec", "-f", str(profile), codex]
    if exec_mode:
        argv += ["exec", "--ignore-user-config", "--skip-git-repo-check", "--json"]
    else:
        argv += ["--no-alt-screen"]
    argv += [
        "--sandbox", "read-only",
        "-c", "approval_policy=\"never\"" if exec_mode else "approval_policy=\"on-request\"",
        "-c", "agents.enabled=false",
    ]
    argv += ["--model", "gpt-5.1-codex"] if top else [
        "--enable", "code_mode_only",
        "-c", 'features.code_mode.excluded_tool_namespaces=["functions","collaboration"]',
    ]
    argv += [
        "-c", f'model_provider="{p}"',
        "-c", f'model_providers.{p}.name="{p}"',
        "-c", f'model_providers.{p}.base_url="http://127.0.0.1:{port}/v1"',
        "-c", f'model_providers.{p}.wire_api="responses"',
        "-c", f"model_providers.{p}.requires_openai_auth=false",
        "-c", f"mcp_servers.ccnm.command={json.dumps(sys.executable)}",
        "-c", f"mcp_servers.ccnm.args={json.dumps([me, 'mcp'])}",
        "-c", f'mcp_servers.ccnm.env={{PROBE_LOG="{out / "mcp.jsonl"}"}}',
        "-c", "mcp_servers.ccnm.required=true",
        "-c", 'mcp_servers.ccnm.default_tools_approval_mode="approve"',
        "-c", 'mcp_servers.ccnm.enabled_tools=["exec_command","read_file"]',
    ]
    for feature in DISABLED:
        argv += ["--disable", feature]
    for item in extra:
        argv += ["-c", item]
    argv += ["call the tools"] if exec_mode else ["--", "call the tools"]
    env = (f"env -i HOME={shlex.quote(str(home))} CODEX_HOME={shlex.quote(str(home / '.codex'))} "
           f"PATH=/usr/bin:/bin TERM=xterm-256color")
    print(f"cd {shlex.quote(str(work))} && {env} {shlex.join(argv)}")


if __name__ == "__main__":
    what = sys.argv[1]
    if what == "serve-model":
        serve_model(Path(sys.argv[2]).resolve(), "--top" in sys.argv[3:])
    elif what == "mcp":
        mcp()
    elif what == "cmd":
        rest = sys.argv[4:]
        exec_mode = "--exec" in rest
        extra = [rest[i + 1] for i, a in enumerate(rest) if a == "-c"]
        cmd(Path(sys.argv[2]).resolve(), sys.argv[3], extra, exec_mode, "--top" in rest)
