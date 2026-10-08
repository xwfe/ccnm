#!/usr/bin/env python3
"""P82：真实 Claude Code 收到 ccnm 的进度通知后，调用是否照常返回（零额度）。

复用 P81 夹具（同目录 p81-mcp-2026-07-28.py）的假模型、抄录转发和隔离办法：假模型让
Claude Code 跑一条约 25 秒、先打一行再睡的命令；看抄录里 ccnm 发了哪些
notifications/progress（token 是不是 Claude Code 给的那个、内容是什么）、Claude Code 的
MCP 日志里有没有报错、工具结果是否正常交回模型。

用法：
  cargo build
  python3 docs/research/probes/p82-progress.py <仓库外的新输出目录> \\
      --claude cc-286=/path/to/2.1.286/claude --claude cc-293=/path/to/2.1.293/claude
"""

import argparse
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("p81", HERE / "p81-mcp-2026-07-28.py")
p81 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p81)

LINE = "P82 step one"


def progress(out, claude, extra_env):
    rc, elapsed, _, mcp_log, leftover = p81.run(
        out, claude, extra_env, "mcp__ccnm__exec_command",
        {"shell": f"echo '{LINE}'; sleep 25", "timeout_ms": 60000}, timeout=240)
    lines, seq, versions, _ = p81.traffic(mcp_log)
    call = next(l["msg"] for l in lines
                if l["dir"] == "c2s" and l["msg"].get("method") == "tools/call")
    token = (call["params"].get("_meta") or {}).get("progressToken")
    t0 = lines[0]["t"]
    reports = [{"at": round(l["t"] - t0, 2), **{k: v for k, v in l["msg"]["params"].items() if k != "_meta"}}
               for l in lines
               if l["dir"] == "s2c" and l["msg"].get("method") == "notifications/progress"]
    results = [r for req in p81.requests(out) for r in p81.tool_results(req)]
    logs = sorted(out.glob("claude-home/Library/Caches/claude-cli-nodejs/*/mcp-logs-ccnm/*"))
    log_errors = [json.loads(l).get("error") for f in logs for l in f.read_text().splitlines()
                  if '"error"' in l]
    return {"rc": rc, "seconds": elapsed, "handshake": versions, "token_sent_by_host": token,
            "progress_reports": reports,
            "tool_result": [{"is_error": bool(r.get("is_error")),
                             "text": json.dumps(r.get("content"), ensure_ascii=False)[-200:]} for r in results],
            "mcp_log_errors": log_errors, "sleep_left_after_exit": leftover}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--claude", action="append", required=True, help="label=PATH[:ENV=VAL,...]")
    args = ap.parse_args()
    out = Path(args.out).resolve()
    if out.exists():
        sys.exit(f"{out} 已存在，换一个新目录")
    summary = {"ccnm": subprocess.run([str(p81.CCNM), "--version"], capture_output=True, text=True).stdout.strip(),
               "cases": {}}
    for spec_text in args.claude:
        label, rest = spec_text.split("=", 1)
        path, _, envs = rest.partition(":")
        extra_env = dict(e.split("=", 1) for e in envs.split(",") if e)
        summary["cases"][label] = {
            "version": subprocess.run([path, "--version"], capture_output=True, text=True).stdout.strip(),
            "sha256": p81.sha256(path), "extra_env": extra_env,
            "progress": progress(out / label, path, extra_env)}
    (out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
