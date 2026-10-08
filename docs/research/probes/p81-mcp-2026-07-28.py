#!/usr/bin/env python3
"""P81：Claude Code 改用 MCP 2026-07-28 协商后，还能不能正常用 ccnm（零额度）。

Claude Code 2.1.292 起对 stdio server 默认按 2026-07-28 协商（CHANGELOG 2.1.292，
`MCP_PROTOCOL_NEGOTIATION=legacy` 可退回旧办法）。这个夹具让一个真实的 Claude Code
二进制连真实的 `ccnm internal mcp-serve`，中间夹一层只抄录、不改动的转发，记下双方
发的每一条 JSON-RPC；模型是本机假服务，API key 是假的，HOME 是空临时目录，进程树用
sandbox-exec 禁掉非本机出站。

每个 Claude Code 二进制跑两个场景：
  basic   假模型让它调一次 mcp__ccnm__workspace_info：看握手用了什么方法、协商到哪一版、
          工具调用回没回结果、ccnm 的 instructions 有没有交到模型手里、在请求的哪个位置
  cancel  MCP_TOOL_TIMEOUT=4000，假模型让它跑一个 27.381 秒的 sleep：看 Claude Code 超时
          后发没发 notifications/cancelled、ccnm 有没有把命令停掉（假模型拿到超时结果后
          先查一次 sleep 还在不在，再等 3 秒查一次，然后才回话）

用法：
  cargo build
  python3 docs/research/probes/p81-mcp-2026-07-28.py <仓库外的新输出目录> \\
      --claude cc-286=/path/to/2.1.286/claude \\
      --claude cc-293=/path/to/2.1.293/claude \\
      --claude cc-293-legacy=/path/to/2.1.293/claude:MCP_PROTOCOL_NEGOTIATION=legacy
环境变量 CCNM_BIN 可指定 ccnm 二进制，默认 target/debug/ccnm。
"""

import argparse
import base64
import hashlib
import json
import os
import subprocess
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
CCNM = Path(os.environ.get("CCNM_BIN", REPO / "target" / "debug" / "ccnm")).resolve()
SANDBOX_PROFILE = """(version 1)
(allow default)
(deny network-outbound)
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket))
"""
SLEEP = "27.381"  # 独一无二的时长，pgrep 靠它认出本轮起的命令

# 只抄录、不改动的转发：Claude Code 起它，它再起 ccnm。每行一条 JSON-RPC。
TAP = r'''
import json, os, signal, subprocess, sys, threading, time
log = open(sys.argv[1], "a", buffering=1)
child = subprocess.Popen(sys.argv[2:], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
def note(direction, line):
    try:
        msg = json.loads(line)
    except Exception:
        msg = {"unparsed": line.decode("utf-8", "replace")}
    log.write(json.dumps({"t": time.time(), "dir": direction, "msg": msg}) + "\n")
def up():
    for line in sys.stdin.buffer:
        note("c2s", line)
        try:
            child.stdin.write(line); child.stdin.flush()
        except BrokenPipeError:
            break
    try:
        child.stdin.close()
    except Exception:
        pass
threading.Thread(target=up, daemon=True).start()
for sig in (signal.SIGINT, signal.SIGTERM):
    signal.signal(sig, lambda s, f: child.send_signal(s))
for line in child.stdout:
    note("s2c", line)
    sys.stdout.buffer.write(line); sys.stdout.buffer.flush()
log.flush()
os._exit(child.wait())  # 读 stdin 的线程还挂着，正常退出会在解释器收尾时卡锁
'''


def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


def message(n, blocks, stop):
    events = [{"type": "message_start", "message": {
        "id": f"msg_{n}", "type": "message", "role": "assistant", "model": "claude-probe",
        "content": [], "stop_reason": None, "stop_sequence": None,
        "usage": {"input_tokens": 1, "output_tokens": 1}}}]
    for i, block in enumerate(blocks):
        if block["type"] == "text":
            events += [
                {"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}},
                {"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": block["text"]}},
            ]
        else:
            events += [
                {"type": "content_block_start", "index": i, "content_block": {
                    "type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}},
                {"type": "content_block_delta", "index": i, "delta": {
                    "type": "input_json_delta", "partial_json": json.dumps(block["input"])}},
            ]
        events.append({"type": "content_block_stop", "index": i})
    events += [
        {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None},
         "usage": {"output_tokens": 1}},
        {"type": "message_stop"},
    ]
    return events


def tool_results(request):
    out = []
    for m in request.get("messages") or []:
        if isinstance(m.get("content"), list):
            out += [p for p in m["content"] if p.get("type") == "tool_result"]
    return out


def sleep_running():
    """本轮那条 sleep 的 pid。

    不用 `pgrep -f`：采样线程和模型那一轮的检查会同时各起一个 pgrep，两个的命令行里都有
    这串字，会互相认成对方（第一版就这样凭空多出过几个 pid）。只认命令行恰好是它的进程。
    """
    listing = subprocess.run(["/bin/ps", "-axo", "pid=,command="],
                             capture_output=True, text=True, check=False).stdout
    return [line.split(None, 1)[0] for line in listing.splitlines()
            if line.split(None, 1)[1:] == [f"/bin/sleep {SLEEP}"]]


class MockModel:
    """按场景回写好的话；拿到工具结果那一轮可以先做检查再回。"""

    def __init__(self, out, tool, tool_input, on_result=None):
        self.out, self.step = out, 0
        self.checks, self.sleep_seen = [], None
        model = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def reply_json(self, value):
                body = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                self.reply_json({"data": []})

            def do_HEAD(self):
                self.send_response(200)
                self.end_headers()

            def do_POST(self):
                raw = self.rfile.read(int(self.headers.get("content-length") or 0))
                if "count_tokens" in self.path:
                    return self.reply_json({"input_tokens": 1})
                if "/v1/messages" not in self.path:
                    return self.reply_json({})
                n = model.step
                model.step += 1
                (model.out / f"model-request-{n}.json").write_bytes(raw)
                request = json.loads(raw)
                names = [t.get("name") for t in request.get("tools") or []]
                results = tool_results(request)
                if tool in names and not results:
                    blocks, stop = [{"type": "tool_use", "id": "toolu_p81", "name": tool,
                                     "input": tool_input}], "tool_use"
                else:
                    if results and on_result:
                        model.checks.append(on_result())
                    blocks, stop = [{"type": "text", "text": "done"}], "end_turn"
                if not request.get("stream"):
                    content = [b if b["type"] == "text" else {
                        "type": "tool_use", "id": b["id"], "name": b["name"], "input": b["input"]}
                        for b in blocks]
                    return self.reply_json({
                        "id": f"msg_{n}", "type": "message", "role": "assistant",
                        "model": "claude-probe", "content": content, "stop_reason": stop,
                        "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 1}})
                payload = sse(message(n, blocks, stop))
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()


def payload(workspace, session, mode):
    body = json.dumps({"protocol": 5, "workspace": workspace, "session": session, "mode": mode})
    return base64.urlsafe_b64encode(body.encode()).decode().rstrip("=")


def workspace(out):
    """一个只给本轮用的 Runtime 配置：共用账号（P78）、外部 MCP coding 模式。"""
    root = out / "project"
    for sub in ("project", "home", "state", "claude-home"):
        (out / sub).mkdir(parents=True)
    (root / "hello.txt").write_text("one\ntwo\n")
    config = out / "config.toml"
    config.write_text(f"""
this = "runtime"

[nodes.runtime]

[nodes.agent]
ssh = "agent-node.invalid"

[workspaces.demo]
root = "{root}"
agent = {{ node = "agent", instance = "claude-main" }}
external_mcp = "coding"
""")
    return config


def run(out, claude, extra_env, tool, tool_input, on_result=None, timeout=180):
    out.mkdir(parents=True)
    config = workspace(out)
    tap_py = out / "tap.py"
    tap_py.write_text(TAP)
    mcp_log = out / "mcp.jsonl"
    mcp = out / "mcp.json"
    # Claude Code 把自己的环境（含 ANTHROPIC_API_KEY）带给 MCP server，ccnm 见到继承来的
    # 认证环境会拒绝服务——它该这样。真实部署中间隔着 ssh，这里用 env -i 模拟那道边界。
    mcp.write_text(json.dumps({"mcpServers": {"ccnm": {
        "type": "stdio", "command": sys.executable,
        "args": [str(tap_py), str(mcp_log), "/usr/bin/env", "-i",
                 f"HOME={out / 'home'}", f"XDG_STATE_HOME={out / 'state'}",
                 f"CCNM_CONFIG={config}", "PATH=/usr/bin:/bin",
                 str(CCNM), "internal", "mcp-serve",
                 "--payload", payload("demo", f"p81-{uuid.uuid4().hex[:8]}", "coding")]}}}))
    settings = out / "settings.json"
    settings.write_text(json.dumps({"permissions": {"allow": ["mcp__ccnm"]}}))
    profile = out / "no-egress.sb"
    profile.write_text(SANDBOX_PROFILE)
    model = MockModel(out, tool, tool_input, on_result)
    cmd = ["sandbox-exec", "-f", str(profile), claude,
           "--tools", "", "--mcp-config", str(mcp), "--strict-mcp-config",
           "--settings", str(settings), "--setting-sources", "user,project,local",
           "--session-id", str(uuid.uuid4()), "--print", "--output-format", "json",
           "--no-session-persistence"]
    env = {"HOME": str(out / "claude-home"), "PATH": "/usr/bin:/bin",
           "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{model.port}",
           "ANTHROPIC_API_KEY": "sk-ant-probe-not-a-real-key",
           "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1", "DISABLE_AUTOUPDATER": "1",
           "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", **extra_env}
    started = time.time()
    # 每 0.25 秒看一次 sleep 在不在：证明命令真的起来过，以及是在哪一刻没的。
    seen, done = [], threading.Event()

    def sample():
        while not done.is_set():
            pids = sleep_running()
            if pids:
                seen.append((round(time.time() - started, 2), pids))
            time.sleep(0.25)

    sampler = threading.Thread(target=sample, daemon=True)
    sampler.start()
    try:
        proc = subprocess.run(cmd, cwd=out / "claude-home", env=env, input=b"run the probe",
                              capture_output=True, timeout=timeout)
        rc, stdout, stderr = proc.returncode, proc.stdout, proc.stderr
    except subprocess.TimeoutExpired as exc:
        rc, stdout, stderr = "timeout", exc.stdout or b"", exc.stderr or b""
    done.set()
    sampler.join()
    # 每个 pid 第一次和最后一次被看到的时刻：pid 中途换了就说明命令被重新起过。
    spans = {}
    for at, pids in seen:
        for pid in pids:
            spans.setdefault(pid, [at, at])[1] = at
    model.sleep_seen = spans or None
    model.started = started
    elapsed = round(time.time() - started, 1)
    model.server.shutdown()
    (out / "claude.stdout").write_bytes(stdout)
    (out / "claude.stderr").write_bytes(stderr)
    leftover = sleep_running()
    for pid in leftover:  # 收掉本轮漏下的命令，免得影响下一轮的 pgrep
        subprocess.run(["/bin/kill", "-9", pid], check=False)
    return rc, elapsed, model, mcp_log, leftover


def traffic(mcp_log):
    lines = [json.loads(l) for l in mcp_log.read_text().splitlines()] if mcp_log.exists() else []
    t0 = lines[0]["t"] if lines else 0
    seq, versions, instructions = [], {}, None
    for line in lines:
        msg = line["msg"]
        label = msg.get("method") or ("result" if "result" in msg else "error" if "error" in msg else "?")
        params, result = msg.get("params") or {}, msg.get("result") or {}
        hints = {k: result[k] for k in ("ttlMs", "cacheScope") if isinstance(result, dict) and k in result}
        seq.append(f"{line['t'] - t0:7.2f}s {line['dir']} {label} id={msg.get('id')}"
                   + (f" {json.dumps(hints)}" if hints else ""))
        if msg.get("method") in ("initialize", "server/discover"):
            meta = params.get("_meta") or {}
            versions[f"client {msg['method']}"] = (params.get("protocolVersion")
                                                  or meta.get("io.modelcontextprotocol/protocolVersion"))
        if isinstance(result, dict) and ("protocolVersion" in result or "supportedVersions" in result):
            versions["server answer"] = result.get("protocolVersion") or result.get("supportedVersions")
        if isinstance(result, dict) and result.get("instructions"):
            instructions = result["instructions"]
    return lines, seq, versions, instructions


def where_found(request, needle):
    """needle 出现在请求的哪些位置。

    逐个字符串找，不能拿 json.dumps 的结果去找：引号会被转义，永远对不上。2.1.286 和
    2.1.293 都把 MCP server 的 instructions 放进一条消息（"MCP Server Instructions"
    那段），不在 system 字段里，所以两处都要看。
    """
    found = []

    def walk(value, path):
        if isinstance(value, str):
            if needle in value:
                found.append(path)
        elif isinstance(value, dict):
            for key, item in value.items():
                walk(item, f"{path}.{key}")
        elif isinstance(value, list):
            for i, item in enumerate(value):
                walk(item, f"{path}[{i}]")

    walk({k: request.get(k) for k in ("system", "messages")}, "request")
    return found


def requests(out):
    paths = sorted(out.glob("model-request-*.json"), key=lambda p: int(p.stem.rsplit("-", 1)[1]))
    return [json.loads(p.read_text()) for p in paths]


def basic(out, claude, extra_env):
    rc, elapsed, _, mcp_log, _ = run(out, claude, extra_env, "mcp__ccnm__workspace_info", {})
    _, seq, versions, instructions = traffic(mcp_log)
    reqs = requests(out)
    results = [r for req in reqs for r in tool_results(req)]
    probe = (instructions or "").strip().splitlines()[0][:60] if instructions else None
    places = sorted({p for req in reqs for p in where_found(req, probe)}) if probe else []
    tools = sorted({t.get("name") for req in reqs for t in req.get("tools") or []
                    if t.get("name", "").startswith("mcp__ccnm__")})
    return {"rc": rc, "seconds": elapsed, "handshake": versions, "traffic": seq,
            "ccnm_tools_offered_to_model": tools,
            "tool_result": [{"is_error": bool(r.get("is_error")),
                             "text": json.dumps(r.get("content"), ensure_ascii=False)[:240]} for r in results],
            "instructions_first_line": probe, "instructions_found_at": places,
            "stderr_tail": (out / "claude.stderr").read_text(errors="replace")[-400:]}


def cancel(out, claude, extra_env):
    def check():
        first = sleep_running()
        time.sleep(3)
        return {"at_timeout_result": first, "three_seconds_later": sleep_running()}

    rc, elapsed, model, mcp_log, leftover = run(
        out, claude, {**extra_env, "MCP_TOOL_TIMEOUT": "4000"}, "mcp__ccnm__exec_command",
        {"cmd": ["/bin/sleep", SLEEP], "timeout_ms": 60000}, on_result=check)
    lines, seq, versions, _ = traffic(mcp_log)
    call_ids = [l["msg"].get("id") for l in lines
                if l["dir"] == "c2s" and l["msg"].get("method") == "tools/call"]
    cancels = [l["msg"].get("params") for l in lines
               if l["dir"] == "c2s" and l["msg"].get("method") == "notifications/cancelled"]
    cancel_at = [round(l["t"] - model.started, 2) for l in lines
                 if l["dir"] == "c2s" and l["msg"].get("method") == "notifications/cancelled"]
    results = [r for req in requests(out) for r in tool_results(req)]
    return {"rc": rc, "seconds": elapsed, "handshake": versions, "traffic": seq,
            "tools_call_ids": call_ids, "cancel_notifications": cancels,
            "cancel_seconds_after_claude_start": cancel_at,
            "sleep_seen_seconds_after_claude_start": model.sleep_seen,
            "sleep_pids_seen_by_model_turn": model.checks, "sleep_left_after_exit": leftover,
            "tool_result": [{"is_error": bool(r.get("is_error")),
                             "text": json.dumps(r.get("content"), ensure_ascii=False)[:240]} for r in results],
            "stderr_tail": (out / "claude.stderr").read_text(errors="replace")[-400:]}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--claude", action="append", required=True,
                    help="label=PATH[:ENV=VAL,...]")
    ap.add_argument("--scenario", action="append", choices=["basic", "cancel"])
    args = ap.parse_args()
    out = Path(args.out).resolve()
    if out.exists():
        sys.exit(f"{out} 已存在，换一个新目录")
    if not CCNM.exists():
        sys.exit(f"找不到 {CCNM}，先 cargo build 或设 CCNM_BIN")
    summary = {"ccnm": subprocess.run([str(CCNM), "--version"], capture_output=True, text=True).stdout.strip(),
               "cases": {}}
    for spec in args.claude:
        label, rest = spec.split("=", 1)
        path, _, envs = rest.partition(":")
        extra_env = dict(e.split("=", 1) for e in envs.split(",") if e)
        case = {"binary": path, "sha256": sha256(path), "extra_env": extra_env,
                "version": subprocess.run([path, "--version"], capture_output=True, text=True).stdout.strip()}
        for name in args.scenario or ["basic", "cancel"]:
            case[name] = globals()[name](out / label / name, path, extra_env)
        summary["cases"][label] = case
    (out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
