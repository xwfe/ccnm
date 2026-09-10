#!/usr/bin/env python3
"""ccnm Machine Protocol v1 的最小客户端。

**这个文件是给外部程序抄走用的**，不是 ccnm 内部代码。它只用 Python 标准库，
不 import 任何 ccnm 的东西，也不需要仓库在场——把这一个文件复制到你的项目里
就能用。协议说明见 docs/protocol/。

怎么用：

    from ccnm_machine_client import MachineClient, RpcError

    with MachineClient() as client:
        info = client.hello("my-orchestrator/0.1")
        print(info["capabilities"]["modes"])          # ['print']
        for agent in client.agents_list():
            print(agent["node"], agent["instance"])
        session = client.session_start(
            workspace="my-project",
            prompt="跑一遍测试",
            start_key="task-4821-attempt-1",          # 想要幂等就给一个
        )
        result = client.wait(session["session"], timeout=900)
        print(result["state"], result.get("text"))

两条最容易踩的：

1. **`session.start` 不等任务跑完**，它拿到 handle 就返回。要结果就轮询
   `session_status`，或者用这里的 `wait()`。
2. **只写不读会卡死。** 这个客户端是一问一答的，天然安全；你要是自己改成
   连发多条再收，就必须另起一个线程读。管道缓冲满了服务端会阻塞在写、
   于是不再读你的输入，两边一起等，谁都不会超时。
"""

from __future__ import annotations

import json
import os
import subprocess
import time
from typing import Any


PROTOCOL = "ccnm.machine/1"


class RpcError(Exception):
    """服务端返回的 error 对象。

    **按 code 判断，别去解析 message**——message 是给人看的，措辞会变。
    `effect` 说的是这次调用有没有留下副作用：`none` 重发安全，`unknown` 必须
    先去查，`applied` 重发会重复。
    """

    def __init__(self, code: int, message: str, data: dict[str, Any] | None = None):
        super().__init__(f"[{code}] {message}")
        self.code = code
        self.message = message
        self.data = data or {}

    @property
    def effect(self) -> str:
        # 契约要求每个错误都带 effect；万一没有，按最保守的读。
        return self.data.get("effect", "unknown")

    @property
    def session(self) -> str | None:
        return self.data.get("session")


# 说明文档第 10 节那张表。这里只列客户端通常要分别处理的几个。
E_PARSE = -32700
E_INVALID_REQUEST = -32600
E_METHOD_NOT_FOUND = -32601
E_INVALID_PARAMS = -32602
E_INTERNAL = -32603
E_NOT_READY = -32000
E_CONFIG = -32001
E_VERSION_MISMATCH = -32002
E_BUSY = -32008
E_NOT_FOUND = -32009
E_CONFLICT = -32010
E_UNCERTAIN = -32011
E_EXPIRED = -32012
E_UNSUPPORTED_CAPABILITY = -32013
E_HANDSHAKE_REQUIRED = -32014

# 终态集合由契约冻结在这三个（协议说明第 13 节）。**不认识的 state 一律当成
# "还没结束"继续轮询**——服务端承诺不再新增终态，所以等下去总会走到这三个之
# 一。反过来做（把不认识的当结束）会让调用方以为一个还在跑的任务已经完了。
TERMINAL_STATES = frozenset({"completed", "failed", "unknown"})


class MachineClient:
    """一条 `ccnm rpc` 连接。"""

    def __init__(
        self,
        ccnm: str = "ccnm",
        config: str | None = None,
        env: dict[str, str] | None = None,
        argv: list[str] | None = None,
    ):
        """`argv` 给出完整命令行，用来代替默认的 `ccnm [--config X] rpc`。

        平时用不上；它的用处是把客户端指向别的东西——比如一个演坏对端的
        测试替身——而不用改这个文件。
        """
        if argv is None:
            argv = [ccnm]
            if config:
                argv += ["--config", config]
            argv.append("rpc")
        self._proc = subprocess.Popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            # 日志走 stderr，和协议分开。这里继承父进程的 stderr，方便调试；
            # 要安静就传 subprocess.DEVNULL。
            stderr=None,
            env={**os.environ, **(env or {})},
            text=True,
            encoding="utf-8",
            # 一行一条消息，所以按行缓冲；攒着不发会让一问一答变成干等。
            bufsize=1,
        )
        self._next_id = 0
        self._greeted = False

    # -- 连接 --

    def close(self) -> None:
        """关掉 stdin，让服务端正常退出。

        **已经接受的 session 不会因此停止**——它们属于磁盘上的记录，不属于这条
        连接。下次连上来用 session id 照样能查。
        """
        if self._proc.poll() is None:
            try:
                if self._proc.stdin:
                    self._proc.stdin.close()
            except OSError:
                pass
            try:
                self._proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self._proc.kill()
                self._proc.wait()
        # 关掉两个管道：只等进程结束不会释放这边的文件描述符。
        for stream in (self._proc.stdin, self._proc.stdout):
            try:
                if stream and not stream.closed:
                    stream.close()
            except OSError:
                pass

    def wait_for_exit(self, timeout: float = 10.0) -> int:
        """等服务端进程结束，返回它的退出码。

        **退出码不是协议结果。** 一个什么都没做就以 0 结束的对端同样返回 0；
        成功与否只看响应。
        """
        if self._proc.stdin and not self._proc.stdin.closed:
            self._proc.stdin.close()
        return self._proc.wait(timeout=timeout)

    def __enter__(self) -> MachineClient:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    # -- 底层 --

    def call(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        """发一个请求，等它的响应。失败抛 `RpcError`。"""
        self._next_id += 1
        request: dict[str, Any] = {"jsonrpc": "2.0", "id": self._next_id, "method": method}
        if params is not None:
            request["params"] = params
        line = json.dumps(request, ensure_ascii=False)
        if "\n" in line:
            raise ValueError("一条消息只能占一行")
        assert self._proc.stdin and self._proc.stdout
        self._proc.stdin.write(line + "\n")
        self._proc.stdin.flush()

        answer = self._proc.stdout.readline()
        if not answer:
            # stdout 到了 EOF：服务端没了。在途请求的结果是**未知**的，可能
            # 执行了也可能没有。带 start_key 的启动可以安全重发，不带的不行。
            raise ConnectionError("ccnm rpc 结束了，没有回答这条请求")
        message = json.loads(answer)
        if "error" in message:
            error = message["error"]
            raise RpcError(error["code"], error["message"], error.get("data"))
        if message.get("id") != request["id"]:
            # 一问一答的前提下这不该发生。id 为 null 的错误响应上面已经抛了；
            # 走到这里说明流对不上了，继续用下去只会张冠李戴。
            raise ConnectionError(f"响应的 id 对不上：{message.get('id')} != {request['id']}")
        return message["result"]

    # -- 六个方法 --

    def hello(self, client: str, protocol_versions: list[str] | None = None) -> dict[str, Any]:
        """握手。必须是第一个调用，否则别的方法回 -32014。"""
        result = self.call(
            "hello",
            {"client": client, "protocol_versions": protocol_versions or [PROTOCOL]},
        )
        self._greeted = True
        return result

    def agents_list(self) -> list[dict[str, Any]]:
        """能寻址的 (node, instance) 绑定。

        条目里**通常没有 provider**：那由 Agent Node 权威解析，而服务端跑在
        Runtime Node 上。要 provider 就看 session 相关调用返回的 `agent`。
        """
        return self.call("agents.list")["agents"]

    def session_start(
        self,
        workspace: str,
        prompt: str,
        agent: dict[str, str] | None = None,
        start_key: str | None = None,
        timeout_ms: int | None = None,
    ) -> dict[str, Any]:
        """启动一个 print 会话，拿到 handle 就返回。

        `agent` 是 `{"node": ..., "instance": ...}`；省略就用 workspace 配置的
        默认 instance。**node 必须是配置里绑定的那个**，客户端不能换机器。

        给 `start_key` 就有幂等：同键同输入返回同一个 session（`reused` 为
        真），同键不同输入抛 -32010。
        """
        params: dict[str, Any] = {
            "workspace": workspace,
            "mode": "print",
            "input": {"prompt": prompt},
        }
        if agent is not None:
            params["agent"] = agent
        if start_key is not None:
            params["start_key"] = start_key
        if timeout_ms is not None:
            params["timeout_ms"] = timeout_ms
        return self.call("session.start", params)

    def session_status(self, session: str) -> dict[str, Any]:
        return self.call("session.status", {"session": session})

    def session_result(self, session: str, max_bytes: int | None = None) -> dict[str, Any]:
        params: dict[str, Any] = {"session": session}
        if max_bytes is not None:
            params["output"] = {"max_bytes": max_bytes}
        return self.call("session.result", params)

    def session_stop(self, session: str) -> dict[str, Any]:
        """请求结束。**返回不代表已经停了**——只有状态变成终态才算。"""
        return self.call("session.stop", {"session": session})

    # -- 便利 --

    def wait(self, session: str, timeout: float = 900.0, poll: float = 0.5) -> dict[str, Any]:
        """轮询到终态，返回 `session.result`。

        `completed` 只表示 Agent 进程正常结束，**不表示活干对了**——那要你自己
        判断（跑测试、看 diff）。`unknown` 是终态，不会自己变好，遇到它去看
        工作树，别重试。
        """
        deadline = time.monotonic() + timeout
        while True:
            status = self.session_status(session)
            if status["state"] in TERMINAL_STATES:
                return self.session_result(session)
            if time.monotonic() >= deadline:
                raise TimeoutError(f"{session} 在 {timeout} 秒内没有结束（当前 {status['state']}）")
            time.sleep(poll)


def main() -> int:
    """`python3 ccnm_machine_client.py [workspace] [prompt]`：一次冒烟。

    `CCNM_BIN` 指定二进制，`CCNM_CONFIG` 指定配置文件；都不给就用 PATH 里的
    `ccnm` 和它的默认配置。
    """
    import sys

    with MachineClient(
        ccnm=os.environ.get("CCNM_BIN", "ccnm"),
        config=os.environ.get("CCNM_CONFIG"),
    ) as client:
        info = client.hello("ccnm-machine-client/1")
        print(f"protocol {info['protocol']}, {info['server']['name']} {info['server']['version']}")
        print(f"modes: {info['capabilities'].get('modes')}")
        for agent in client.agents_list():
            print(f"  {agent['node']}/{agent['instance']} -> {agent.get('workspaces', [])}")
        if len(sys.argv) >= 3:
            started = client.session_start(sys.argv[1], sys.argv[2])
            print(f"started {started['session']}")
            result = client.wait(started["session"])
            print(f"{result['state']}: {result.get('text')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
