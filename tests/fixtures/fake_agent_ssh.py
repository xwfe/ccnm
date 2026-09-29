#!/usr/bin/env python3
"""冒充 Agent Node 的 ssh，给 `ccnm rpc` 的会话控制测试用。

ccnm 以 `ssh <选项> -T <alias> <ccnm> internal <sub> --payload X` 调 Agent；把一个
叫 `ssh` 的包装脚本放在 PATH 最前面，它就调到这里。被测的始终是真实 ccnm，这里
只演对面那台机器，而且只演协议里约定的样子：

- `agent-run` 停在“等放行”上，放行后回一份 RunReport；请求带了 Runtime 分配的
  会话 id，就用它（真实 Agent 也是这样）。
- `agent-stop` 把请求记下来；被停的那次运行随即以“被信号杀掉”结束。
- `agent-status` 答“没有活着的交互会话”；`agent-guard` / `runtime-guard` 见下。
- 控制目录（环境变量 FAKE_AGENT_DIR）里的文件决定每次怎么演：

    release/<key>        放行某个 prompt 的运行（key 见 key_of）
    release/ALL          放行全部
    reply-<key>.json     这次运行回什么：exit_code、stdout_tail、stderr_tail、
                         result，或 {"transport_error": true} 表示连接断在半路
    stop-mode.json       {"kind": "ack"}（默认）、{"kind": "unreachable"}、
                         {"kind": "release-and-wait-final", "prompt": ..., "record": ...}
    view-<key>-<stream>  agent-output 要交回的保留视图（字节原样），按 prompt 找
    output-mode.json     {"kind": "serve"}（默认）、{"kind": "unreachable"}、
                         {"kind": "unknown-command"}（演一个还不认识这个请求的旧 Agent）
    guard-mode.json      agent-guard 怎么演：{"kind": "free"}（默认）、"unreachable"、
                         "unknown-command"，或 {"kind": "relay", ...}——换成真实 ccnm
                         当 Agent 端（ccnm、agent_config、agent_env），它拨 Runtime 时
                         又回到本脚本
    runtime.json         Agent 拨 Runtime 那一跳：{"ccnm", "env"} 以“执行账号”的环境
                         跑真实 `ccnm internal runtime-guard`；{"kind": "unreachable"} 连不上
    calls.jsonl          每次调用：alias、子命令、解开的请求

`agent-output` 只演协议：按偏移切视图、base64 交回。脱敏与 UTF-8 规整是真实 Agent
的事，由 Rust 测试覆盖；这里的视图就是测试放进去的那份。

控制目录被删就立即退出：测试收尾时 ccnm rpc 已经走了，没人再等这个回答。
"""
from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import sys
import time
import uuid


def key_of(prompt: str) -> str:
    return uuid.uuid5(uuid.NAMESPACE_OID, prompt).hex


def unb64(text: str) -> dict:
    return json.loads(base64.urlsafe_b64decode(text + "=" * (-len(text) % 4)))


def identity(agent: dict) -> dict:
    return {"node": agent["node"], "instance": agent["instance"], "provider": "claude", "profile_ref": "default"}


def wait_for(predicate, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.02)
    return False


def run(request: dict, fake: Path) -> int:
    prompt = request["prompt"]
    session = request.get("session") or str(uuid.uuid4())
    # 记下哪个会话跑的是哪个 prompt：agent-output 按会话 id 来问。
    (fake / "sessions").mkdir(exist_ok=True)
    (fake / "sessions" / session).write_text(key_of(prompt))
    released = fake / "release" / key_of(prompt)
    stopped = fake / "stopped" / session

    def done() -> bool:
        return released.exists() or stopped.exists() or (fake / "release/ALL").exists() or not fake.exists()

    if not wait_for(done, 120) or not fake.exists():
        return 255
    reply_file = fake / f"reply-{key_of(prompt)}.json"
    reply = json.loads(reply_file.read_text()) if reply_file.exists() else {}
    if reply.get("transport_error"):
        print("Connection to worker closed by remote host.", file=sys.stderr)
        return 255
    if stopped.exists():
        outcome = {"exit_code": None, "timed_out": False, "duration_ms": 5, "error": None}
    else:
        outcome = {"exit_code": reply.get("exit_code", 0), "timed_out": False, "duration_ms": 7, "error": None}
    report = {
        "protocol": 3,
        "agent_identity": identity(request["agent"]),
        "session": session,
        "session_dir": f"/fake/agent/state/sessions/{session}",
        "controller": {
            "hello": {"protocol": 1, "ccnm_version": "fake", "user": "fake", "platform": "fake/fake", "exe": None, "root": None},
            "pid": 1,
            "manager": {"Ok": "Aqua"},
        },
        "pid": 2,
        "outcome": outcome,
        "result": reply.get("result"),
        "stdout_tail": reply.get("stdout_tail", f"done: {prompt}\n"),
        "stderr_tail": reply.get("stderr_tail", ""),
    }
    sys.stdout.write(json.dumps(report, ensure_ascii=False))
    return 0


def stop(request: dict, fake: Path) -> int:
    mode_file = fake / "stop-mode.json"
    mode = json.loads(mode_file.read_text()) if mode_file.exists() else {"kind": "ack"}
    if mode["kind"] == "unreachable":
        print("ssh: connect to host worker port 22: Connection refused", file=sys.stderr)
        return 255
    if mode["kind"] == "release-and-wait-final":
        # 先让被停的那次运行自己跑完、把终态写进记录，再回答 stop：
        # “完成”和“停止”交错的一种确定顺序。
        (fake / "release" / key_of(mode["prompt"])).write_text("go\n")
        record = Path(mode["record"])

        def final() -> bool:
            try:
                return json.loads(record.read_text()).get("state") in ("completed", "failed")
            except (OSError, ValueError):
                return False

        if not wait_for(final, 20):
            return 255
    elif request.get("session"):
        (fake / "stopped").mkdir(exist_ok=True)
        (fake / "stopped" / request["session"]).write_text("stopped\n")
    report = {"protocol": 1, "tmux_session": "ccnm-" + request["workspace"], "killed": True}
    if request.get("session"):
        report["session"] = request["session"]
    if request.get("agent"):
        report["protocol"] = 3
        report["agent_identity"] = identity(request["agent"])
    sys.stdout.write(json.dumps(report))
    return 0


def output(request: dict, fake: Path) -> int:
    mode_file = fake / "output-mode.json"
    mode = json.loads(mode_file.read_text()) if mode_file.exists() else {"kind": "serve"}
    if mode["kind"] == "unreachable":
        print("ssh: connect to host worker port 22: Connection refused", file=sys.stderr)
        return 255
    if mode["kind"] == "unknown-command":
        print("error: unrecognized subcommand 'agent-output'", file=sys.stderr)
        return 2
    owner = fake / "sessions" / request["session"]
    if not owner.exists():
        print("CCNM_E_NOT_READY:\nno session on this machine", file=sys.stderr)
        return 3
    view_file = fake / f"view-{owner.read_text()}-{request['stream']}"
    view = view_file.read_bytes() if view_file.exists() else b""
    offset, limit = request["offset"], request["limit"]
    report = {
        "protocol": 8,
        "agent_identity": identity(request["agent"]),
        "session": request["session"],
        "stream": request["stream"],
        "generation": "g-" + owner.read_text()[:12],
        "view_bytes": len(view),
        "source_bytes": len(view),
        "source_truncated": False,
        "offset": offset,
        "data": base64.b64encode(view[offset:offset + limit]).decode("ascii"),
    }
    sys.stdout.write(json.dumps(report))
    return 0


def guard(argv: list, request: dict, fake: Path) -> int:
    """agent-guard：默认答 free；relay 时换成真实 ccnm 去问，它再经本脚本拨 Runtime。"""
    mode_file = fake / "guard-mode.json"
    mode = json.loads(mode_file.read_text()) if mode_file.exists() else {"kind": "free"}
    if mode["kind"] == "unreachable":
        print("ssh: connect to host worker port 22: Connection refused", file=sys.stderr)
        return 255
    if mode["kind"] == "unknown-command":
        print("error: unrecognized subcommand 'agent-guard'", file=sys.stderr)
        return 2
    if mode["kind"] == "relay":
        # 真实的 Agent 端：认实例、按自己的配置拨 runtime-alias（又回到本脚本，
        # 见 runtime_guard），把 Runtime 的回答转回来。
        os.execve(
            mode["ccnm"],
            [mode["ccnm"], "--config", mode["agent_config"], *argv[argv.index("internal"):]],
            mode["agent_env"],
        )
    report = {
        "protocol": 9,
        "agent_identity": identity(request["agent"]),
        "runtime": {
            "protocol": 9,
            "workspace": request["workspace"],
            "observation": {
                "state": "free",
                "reason": "never_taken",
                "resource": {"kind": "root", "id": "0000000000000000"},
                "observed_at": int(time.time()),
            },
        },
    }
    sys.stdout.write(json.dumps(report))
    return 0


def runtime_guard(argv: list, fake: Path) -> int:
    """Agent 拨到 Runtime 的那一跳：落到“执行账号”上跑真实 ccnm。"""
    mode = json.loads((fake / "runtime.json").read_text())
    if mode.get("kind") == "unreachable":
        print("ssh: connect to host runtime port 22: Connection refused", file=sys.stderr)
        return 255
    os.execve(mode["ccnm"], [mode["ccnm"], *argv[argv.index("internal"):]], mode["env"])
    return 0


def status(request: dict) -> int:
    """agent-status：这台 Agent 上没有活着的交互会话。"""
    report = {"protocol": 1, "tmux": {"Ok": "3.5a"}, "sessions": []}
    if request.get("agent"):
        report["protocol"] = 3
        report["agent_identity"] = identity(request["agent"])
    sys.stdout.write(json.dumps(report))
    return 0


def main(argv: list) -> int:
    fake = Path(os.environ["FAKE_AGENT_DIR"])
    alias = argv[argv.index("-T") + 1] if "-T" in argv else None
    sub = argv[argv.index("internal") + 1] if "internal" in argv else None
    request = unb64(argv[-1]) if "--payload" in argv else {}
    with open(fake / "calls.jsonl", "a", encoding="utf-8") as log:
        log.write(json.dumps({"alias": alias, "sub": sub, "request": request}) + "\n")
    if sub == "agent-run":
        return run(request, fake)
    if sub == "agent-stop":
        return stop(request, fake)
    if sub == "agent-output":
        return output(request, fake)
    if sub == "agent-guard":
        return guard(argv, request, fake)
    if sub == "runtime-guard":
        return runtime_guard(argv, fake)
    if sub == "agent-status":
        return status(request)
    print(f"fake agent: unexpected call {sub!r}", file=sys.stderr)
    return 97


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
