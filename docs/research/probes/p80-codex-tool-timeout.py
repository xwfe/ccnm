#!/usr/bin/env python3
"""P80：Codex 等一次 MCP 工具调用最多等多久，等不到时会不会通知 server 取消。

零额度：模型接口是本机假服务；"ccnm" 是本脚本自己扮的 stdio MCP server，只有一个
exec_command，收到调用后故意拖 PROBE_HOLD 秒才回，期间照常读 stdin，把收到的每条
消息（含 notifications/cancelled）带时间记下来。HOME / CODEX_HOME 是临时空目录，
进程树套 sandbox-exec，只许连本机。

启动参数照 ccnm 的 `build_launch_cmd`（crates/ccnm-core/src/provider/codex/mod.rs）
的 print 会话：codex exec、只读 sandbox、approval_policy="never"、关掉的 feature 列表、
agents 关，ccnm 这个 server 默认 approve。要测的只是追加的 `-c` 项。

用法：
  p80-codex-tool-timeout.py run <目录> <codex> <拖住的秒数> [--top] [--linger 秒] [-c 追加项 ...]
      跑一次 codex exec，结果写进 <目录>/summary.json：Codex 等了多久、server 收到了什么、
      模型下一轮看到的工具结果。--top 带 --model gpt-5.1-codex、工具在顶层；不带就是
      不传 --model 的默认模型（Code Mode，codex-code-mode-host 要和 codex 在同一目录）。
      --linger 让假模型拿到工具结果后先等这么久再回（默认 0），见 serve_model。
  p80-codex-tool-timeout.py mcp
      当 MCP server（由 Codex 拉起，日志写 $PROBE_LOG，拖的秒数读 $PROBE_HOLD）
"""

import json
import os
import subprocess
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
# 抄自 ccnm provider/codex/mod.rs 的 DISABLED（P80 开工时的 main）。
DISABLED = [
    "shell_tool", "unified_exec", "unified_exec_tty", "view_image", "apps", "plugins", "hooks",
    "multi_agent", "multi_agent_v2", "browser_use", "computer_use", "image_generation",
    "memories", "workspace_dependencies", "skill_search", "shell_snapshot", "goals", "tool_suggest",
]
ARGS = {"cmd": "sleep 1000", "timeout_ms": 600000}
# 第一行的 pragma 让 exec 不在默认的 30 秒就让出：要量的是工具调用本身等多久，
# 不是 Code Mode 什么时候把控制权交回模型。
SCRIPT = f"""// @exec: {{"yield_time_ms": 900000}}
const t0 = Date.now();
try {{ text("result: " + JSON.stringify(await tools.mcp__ccnm__exec_command({json.dumps(ARGS)}))); }}
catch (e) {{ text("threw: " + String(e)); }}
text("ms: " + (Date.now() - t0));
"""


def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


def serve_model(out: Path, top: bool, linger: float):
    """第一轮让模型调一次 exec_command，之后每轮先等 linger 秒再回一句话；每次请求原样存下。

    等那一下是为了让 Codex 在工具超时之后还活着：codex exec 一回合结束就退出并收掉
    MCP server，看不到 server 晚到的回复和之后会不会收到取消；交互会话里 server 会一直在。
    """
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
            main_turn = top or any(isinstance(i, dict) and i.get("type") == "additional_tools"
                                   for i in request.get("input", []))
            if main_turn and not state["sent"]:
                state["sent"] = True
                item = ({"type": "function_call", "id": "fc_0", "call_id": "call_0",
                         "namespace": "mcp__ccnm", "name": "exec_command",
                         "arguments": json.dumps(ARGS)} if top else
                        {"type": "custom_tool_call", "id": "ctc_0", "call_id": "call_0",
                         "name": "exec", "input": SCRIPT})
            else:
                time.sleep(linger)
                item = {"type": "message", "role": "assistant", "id": f"msg_{n}",
                        "content": [{"type": "output_text", "text": "P80 done"}]}
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
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


TOOLS = [
    {"name": "exec_command", "description": "Run a shell command on the Runtime.",
     "inputSchema": {"type": "object", "properties": {
         "cmd": {"type": "string"}, "timeout_ms": {"type": "integer"}}},
     "annotations": {"readOnlyHint": False, "destructiveHint": True, "idempotentHint": False,
                     "openWorldHint": True}},
]


def mcp():
    log = Path(os.environ["PROBE_LOG"])
    hold = float(os.environ["PROBE_HOLD"])
    out_lock, log_lock = threading.Lock(), threading.Lock()

    def note(entry):
        entry["t"] = round(time.time(), 3)
        with log_lock, log.open("a") as f:
            f.write(json.dumps(entry) + "\n")

    def send(mid, result):
        with out_lock:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
            sys.stdout.flush()

    def answer_late(mid):
        time.sleep(hold)
        try:
            send(mid, {"content": [{"type": "text", "text": f"held {hold}s"}]})
            note({"sent": "tools/call result", "id": mid})
        except BrokenPipeError:
            note({"sent": "tools/call result failed: client gone", "id": mid})

    for line in sys.stdin:
        msg = json.loads(line)
        method, mid = msg.get("method"), msg.get("id")
        note({"method": method, "id": mid, "params": msg.get("params")})
        if mid is None:
            continue
        if method == "initialize":
            send(mid, {"protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
                       "capabilities": {"tools": {}},
                       "serverInfo": {"name": "p80-probe", "version": "0"}})
        elif method == "tools/list":
            send(mid, {"tools": TOOLS})
        elif method == "tools/call":
            threading.Thread(target=answer_late, args=(mid,), daemon=True).start()
        else:
            send(mid, {})
    note({"stdin": "closed"})


def run(out: Path, codex: str, hold: float, top: bool, linger: float, extra: list):
    out.mkdir(parents=True, exist_ok=True)
    home, work = out / "home", out / "work"
    (home / ".codex").mkdir(parents=True, exist_ok=True)
    work.mkdir(exist_ok=True)
    profile = out / "no-egress.sb"
    profile.write_text(SANDBOX_PROFILE)
    log = out / "mcp.jsonl"
    log.unlink(missing_ok=True)
    server = serve_model(out, top, linger)
    port = server.server_address[1]
    p = "probemock"
    me = str(Path(__file__).resolve())
    argv = ["sandbox-exec", "-f", str(profile), codex, "exec", "--ignore-user-config",
            "--ignore-rules", "--skip-git-repo-check", "--ephemeral", "--json",
            "--color", "never", "--sandbox", "read-only",
            "-c", 'approval_policy="never"', "-c", "agents.enabled=false"]
    argv += ["--model", "gpt-5.1-codex"] if top else [
        "--enable", "code_mode_only",
        "-c", 'features.code_mode.excluded_tool_namespaces=["functions","collaboration"]',
    ]
    for feature in DISABLED:
        argv += ["--disable", feature]
    argv += [
        "-c", f'model_provider="{p}"',
        "-c", f'model_providers.{p}.name="{p}"',
        "-c", f'model_providers.{p}.base_url="http://127.0.0.1:{port}/v1"',
        "-c", f'model_providers.{p}.wire_api="responses"',
        "-c", f"model_providers.{p}.requires_openai_auth=false",
        "-c", f"mcp_servers.ccnm.command={json.dumps(sys.executable)}",
        "-c", f"mcp_servers.ccnm.args={json.dumps([me, 'mcp'])}",
        "-c", f'mcp_servers.ccnm.env={{PROBE_LOG="{log}",PROBE_HOLD="{hold}"}}',
        "-c", "mcp_servers.ccnm.required=true",
        "-c", 'mcp_servers.ccnm.default_tools_approval_mode="approve"',
        "-c", 'mcp_servers.ccnm.enabled_tools=["exec_command"]',
    ]
    for item in extra:
        argv += ["-c", item]
    argv += ["call the tool"]
    env = {"HOME": str(home), "CODEX_HOME": str(home / ".codex"), "PATH": "/usr/bin:/bin"}
    started = time.time()
    # stdin 不是终端时 codex exec 会把它读到 EOF 再开工，继承来的管道可能永远不关。
    done = subprocess.run(argv, cwd=work, env=env, stdin=subprocess.DEVNULL,
                          capture_output=True, text=True, timeout=hold + linger + 600)
    ended = time.time()
    # 让晚到的回复有机会写进日志：server 在 Codex 退出、stdin 关上之后才会知道。
    time.sleep(2)
    server.shutdown()
    (out / "codex-stdout.jsonl").write_text(done.stdout)
    (out / "codex-stderr.txt").write_text(done.stderr)
    entries = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
    call = next((e for e in entries if e.get("method") == "tools/call"), None)
    cancels = [e for e in entries if e.get("method") == "notifications/cancelled"]
    outputs = []
    for path in sorted(out.glob("model-request-*.json")):
        for item in json.loads(path.read_text()).get("input", []):
            if isinstance(item, dict) and item.get("type") in (
                    "function_call_output", "custom_tool_call_output"):
                outputs.append({"request": path.name, "type": item["type"],
                                "output": item.get("output")})
    summary = {
        "codex": codex,
        "version": subprocess.run([codex, "--version"], capture_output=True,
                                  text=True).stdout.strip(),
        "top_level_tools": top,
        "extra": extra,
        "hold_s": hold,
        "linger_s": linger,
        "exit": done.returncode,
        "codex_wall_s": round(ended - started, 1),
        "call_received_after_s": round(call["t"] - started, 1) if call else None,
        "cancelled": [{"after_call_s": round(e["t"] - call["t"], 1), "params": e["params"]}
                      for e in cancels] if call else cancels,
        "late_result": [e for e in entries if "sent" in e],
        "tool_outputs_seen_by_model": outputs,
        "mcp_methods": [e.get("method") or e.get("sent") or e.get("stdin") for e in entries],
    }
    (out / "summary.json").write_text(json.dumps(summary, indent=1, ensure_ascii=False) + "\n")
    print(json.dumps(summary, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    what = sys.argv[1]
    if what == "mcp":
        mcp()
    elif what == "run":
        rest = sys.argv[5:]
        extra = [rest[i + 1] for i, a in enumerate(rest) if a == "-c"]
        linger = next((float(rest[i + 1]) for i, a in enumerate(rest) if a == "--linger"), 0.0)
        run(Path(sys.argv[2]).resolve(), sys.argv[3], float(sys.argv[4]), "--top" in rest,
            linger, extra)
