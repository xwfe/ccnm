"""P57 探针共用部分：临时沙盒、冒充 Agent Node 的假 ssh、假 tmux。

只在本次新建的 /tmp/p57-* 目录里读写，不读真实配置、不连网络、不启动模型。
被测的始终是 `cargo build` 出来的真实 ccnm；这里假的只是它对面的机器。

为什么放 /tmp 而不是 $TMPDIR：macOS 的 $TMPDIR 有 48 个字符，拼上
`<state>/ccnm/ssh` 再加 41 字节的 socket 名会超过 ControlPath 的 103 字节上限，
ccnm 在拨号前就报配置错误，探针就测不到后面的东西了。

同一个文件还是假 ssh / 假 tmux 的程序本体：`python3 p57_common.py ssh ...`。
"""
from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve()


def ccnm_binary() -> Path:
    override = os.environ.get("CCNM_BIN")
    candidate = Path(override) if override else ROOT / "target/debug/ccnm"
    if not candidate.is_file():
        raise SystemExit("先 cargo build，或用 CCNM_BIN 指定 ccnm")
    return candidate


def b64(obj: object) -> str:
    raw = json.dumps(obj, ensure_ascii=False).encode("utf-8")
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


def unb64(text: str) -> object:
    padded = text + "=" * (-len(text) % 4)
    return json.loads(base64.urlsafe_b64decode(padded.encode("ascii")))


def wait_until(predicate, timeout: float = 20.0, poll: float = 0.02):
    """轮询到条件成立；到点还不成立就抛，探针不靠固定 sleep 猜时序。"""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(poll)
    raise TimeoutError("等待的条件没有出现")


class Sandbox:
    """一个探针用例的全部文件：配置、HOME、state、假程序和它们的控制目录。"""

    def __init__(self, name: str):
        self.dir = Path(tempfile.mkdtemp(prefix=f"p57-{name}-", dir="/tmp"))
        self.home = self.dir / "home"
        self.xdg_state = self.dir / "s"
        self.state = self.xdg_state / "ccnm"
        self.bin = self.dir / "bin"
        self.fake = self.dir / "fake"
        for sub in (self.home, self.xdg_state, self.bin, self.fake / "release"):
            sub.mkdir(parents=True, exist_ok=True)
        self.config = self.dir / "config.toml"

    def cleanup(self) -> None:
        shutil.rmtree(self.dir, ignore_errors=True)

    def env(self, **extra: str) -> dict:
        env = {
            "HOME": str(self.home),
            "XDG_STATE_HOME": str(self.xdg_state),
            "XDG_CONFIG_HOME": str(self.home / ".config"),
            "PATH": f"{self.bin}:/usr/bin:/bin",
            "CCNM_LOG": "warn",
        }
        env.update(extra)
        return env

    def install(self, program: str) -> None:
        """在 bin/ 下放一个名叫 program 的假程序，把调用转给本文件。"""
        shim = self.bin / program
        shim.write_text(
            f"#!/bin/sh\nP57_FAKE_DIR='{self.fake}' exec '{sys.executable}' -B '{HERE}' {program} \"$@\"\n",
            encoding="utf-8",
        )
        shim.chmod(0o700)

    # -- 读假程序留下的记录 --

    def calls(self) -> list:
        log = self.fake / "calls.jsonl"
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines() if line]

    def calls_of(self, sub: str) -> list:
        return [c for c in self.calls() if c.get("sub") == sub]

    def started(self, prompt: str) -> bool:
        return any(c["request"].get("prompt") == prompt for c in self.calls_of("agent-run"))

    def release(self, prompt: str) -> None:
        (self.fake / "release" / key_of(prompt)).write_text("go\n", encoding="utf-8")

    def release_all(self) -> None:
        """收尾用：放行所有还在等的假运行，免得它们在后台挂到超时。"""
        (self.fake / "release" / "ALL").write_text("go\n", encoding="utf-8")

    def reply_for(self, prompt: str, reply: dict) -> None:
        """让某个 prompt 的 agent-run 回指定内容（stdout_tail、result 等）。"""
        (self.fake / f"reply-{key_of(prompt)}.json").write_text(json.dumps(reply), encoding="utf-8")

    def stop_mode(self, mode: dict) -> None:
        (self.fake / "stop-mode.json").write_text(json.dumps(mode), encoding="utf-8")

    def record(self, session: str) -> dict | None:
        path = self.state / "rpc/sessions" / f"{session}.json"
        return json.loads(path.read_text(encoding="utf-8")) if path.exists() else None


def key_of(prompt: str) -> str:
    return uuid.uuid5(uuid.NAMESPACE_OID, prompt).hex


class RpcPeer:
    """一个 `ccnm rpc` 进程。stderr 落到沙盒里的文件，留作原始证据。

    不复用 clients/python 的 MachineClient：它把 stderr 直接继承给终端，
    而探针要把服务端日志（比如"写不进记录"）当结果收下来。
    """

    _count = 0

    def __init__(self, sandbox: Sandbox, config: Path | None = None):
        RpcPeer._count += 1
        self.stderr_path = sandbox.dir / f"rpc-{RpcPeer._count}.stderr"
        self._stderr = open(self.stderr_path, "w", encoding="utf-8")
        self.proc = subprocess.Popen(
            [str(ccnm_binary()), "--config", str(config or sandbox.config), "rpc"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self._stderr,
            env=sandbox.env(),
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self._next = 0
        greeting = self.call("hello", {"client": "p57-probe/1", "protocol_versions": ["ccnm.machine/1"]})
        if "result" not in greeting:
            raise RuntimeError(f"hello failed: {greeting}")

    def call(self, method: str, params: dict) -> dict:
        """原样返回响应：成功是 {"result":...}，失败是 {"error":...}。"""
        self._next += 1
        line = json.dumps({"jsonrpc": "2.0", "id": self._next, "method": method, "params": params}, ensure_ascii=False)
        assert self.proc.stdin and self.proc.stdout
        self.proc.stdin.write(line + "\n")
        self.proc.stdin.flush()
        answer = self.proc.stdout.readline()
        if not answer:
            raise ConnectionError("ccnm rpc exited without answering")
        return json.loads(answer)

    def close(self) -> int:
        if self.proc.stdin and not self.proc.stdin.closed:
            self.proc.stdin.close()
        try:
            code = self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            code = self.proc.wait()
        self._stderr.close()
        return code

    def stderr(self) -> str:
        if not self._stderr.closed:
            self._stderr.flush()
        return self.stderr_path.read_text(encoding="utf-8")


def start(peer: RpcPeer, prompt: str, key: str | None = None, workspace: str = "demo") -> dict:
    params = {"workspace": workspace, "mode": "print", "input": {"prompt": prompt}}
    if key is not None:
        params["start_key"] = key
    return peer.call("session.start", params)


def settle(peer: RpcPeer, session: str, timeout: float = 20.0) -> dict:
    """轮询 status 到终态；返回最后一次 status 的 result。"""
    def terminal():
        answer = peer.call("session.status", {"session": session}).get("result", {})
        return answer if answer.get("state") in ("completed", "failed", "unknown") else None

    return wait_until(terminal, timeout=timeout, poll=0.05)


RUNTIME_CONFIG = """\
this = "runtime"
[nodes.runtime]
[nodes.worker]
ssh = "worker-alias"
[nodes.worker2]
ssh = "worker2-alias"
[workspaces.demo]
root = "{root}"
agent = {{ node = "{node}", instance = "claude-main" }}
"""


def runtime_sandbox(name: str) -> Sandbox:
    """Operator/Runtime 这一侧：一个绑定到 worker/claude-main 的 workspace，假 ssh 就位。"""
    sb = Sandbox(name)
    (sb.dir / "demo").mkdir()
    write_runtime_config(sb, node="worker")
    sb.install("ssh")
    return sb


def write_runtime_config(sb: Sandbox, node: str) -> None:
    sb.config.write_text(RUNTIME_CONFIG.format(root=sb.dir / "demo", node=node), encoding="utf-8")


# ---------------------------------------------------------------------------
# 假 Agent Node：ccnm 用 `ssh <选项> -T <alias> <ccnm> internal <sub> --payload X`
# 调它。这里只认 agent-run / agent-stop / agent-purge 三种，其余一律 exit 97，
# 让没预料到的调用显形而不是被悄悄当成功。


def identity(agent: dict) -> dict:
    return {
        "node": agent["node"],
        "instance": agent["instance"],
        "provider": "claude",
        "profile_ref": "default",
    }


def controller() -> dict:
    return {
        "hello": {
            "protocol": 1,
            "ccnm_version": "fake",
            "user": "p57-fake",
            "platform": "fake/fake",
            "exe": None,
            "root": None,
        },
        "pid": 1,
        "manager": {"Ok": "Aqua"},
    }


def fake_ssh(argv: list, fake: Path) -> int:
    alias = argv[argv.index("-T") + 1] if "-T" in argv else None
    sub = argv[argv.index("internal") + 1] if "internal" in argv else None
    request = unb64(argv[-1]) if "--payload" in argv else {}
    with open(fake / "calls.jsonl", "a", encoding="utf-8") as log:
        log.write(json.dumps({"alias": alias, "sub": sub, "request": request, "t": time.time()}) + "\n")

    if sub == "agent-run":
        key = key_of(request["prompt"])
        released = fake / "release" / key
        release_all = fake / "release" / "ALL"
        try:
            # 沙盒被删也算放行：探针收尾时 ccnm rpc 已退出，没人再等这个回答，
            # 不这样的话它会作为孤儿进程挂满 120 秒。
            wait_until(lambda: released.exists() or release_all.exists() or not fake.exists(), timeout=120, poll=0.02)
        except TimeoutError:
            print("fake agent: never released", file=sys.stderr)
            return 255
        if not fake.exists():
            return 255
        reply_file = fake / f"reply-{key}.json"
        reply = json.loads(reply_file.read_text()) if reply_file.exists() else {}
        session = str(uuid.uuid4())
        report = {
            "protocol": 3,
            "agent_identity": identity(request["agent"]),
            "session": session,
            "session_dir": f"/fake/agent/state/sessions/{session}",
            "controller": controller(),
            "pid": 2,
            "outcome": {"exit_code": reply.get("exit_code", 0), "timed_out": False, "duration_ms": 7, "error": None},
            "result": reply.get("result"),
            "stdout_tail": reply.get("stdout_tail", f"done: {request['prompt']}\n"),
            "stderr_tail": reply.get("stderr_tail", ""),
        }
        sys.stdout.write(json.dumps(report, ensure_ascii=False))
        return 0

    if sub == "agent-stop":
        mode_file = fake / "stop-mode.json"
        mode = json.loads(mode_file.read_text()) if mode_file.exists() else {"kind": "ack"}
        if mode["kind"] == "release-and-wait-final":
            # 让被停的那次运行先结束、把终态写进记录，再回答 stop：
            # 这是"完成"和"停止"交错的一种确定顺序。
            (fake / "release" / key_of(mode["prompt"])).write_text("go\n")
            record = Path(mode["record"])

            def final() -> bool:
                try:
                    return json.loads(record.read_text()).get("state") in ("completed", "failed")
                except (OSError, ValueError):
                    return False

            try:
                wait_until(final, timeout=20)
            except TimeoutError:
                print("fake agent: run never recorded a terminal state", file=sys.stderr)
                return 255
        elif mode["kind"] == "release":
            (fake / "release" / key_of(mode["prompt"])).write_text("go\n")
        report = {"protocol": 3, "tmux_session": "ccnm-" + request["workspace"], "killed": True}
        if request.get("agent"):
            report["agent_identity"] = identity(request["agent"])
        else:
            report["protocol"] = 1
        sys.stdout.write(json.dumps(report))
        return 0

    if sub == "agent-purge":
        answer_file = fake / "purge-answer.json"
        answer = json.loads(answer_file.read_text()) if answer_file.exists() else {"removed": [], "sessions": []}
        sys.stdout.write(json.dumps({"protocol": 1, **answer}))
        return 0

    print(f"fake agent: unexpected call {sub!r}", file=sys.stderr)
    return 97


def fake_tmux(argv: list, fake: Path) -> int:
    """按 fake/tmux-script.json 里的顺序回答；每次调用都记下来。"""
    with open(fake / "tmux.jsonl", "a", encoding="utf-8") as log:
        log.write(json.dumps(argv) + "\n")
    script_file = fake / "tmux-script.json"
    script = json.loads(script_file.read_text()) if script_file.exists() else {}
    verb = next((a for a in argv if a in ("has-session", "show-environment", "kill-session", "-V")), None)
    answers = script.get(verb, [])
    count_file = fake / f"tmux-count-{verb}"
    n = int(count_file.read_text()) if count_file.exists() else 0
    count_file.write_text(str(n + 1))
    answer = answers[min(n, len(answers) - 1)] if answers else {"exit": 1, "stderr": "no server running\n"}
    sys.stdout.write(answer.get("stdout", ""))
    sys.stderr.write(answer.get("stderr", ""))
    return int(answer.get("exit", 0))


if __name__ == "__main__":
    program, args = sys.argv[1], sys.argv[2:]
    fake_dir = Path(os.environ["P57_FAKE_DIR"])
    if program == "ssh":
        raise SystemExit(fake_ssh(args, fake_dir))
    if program == "tmux":
        raise SystemExit(fake_tmux(args, fake_dir))
    raise SystemExit(f"unknown fake program {program}")
