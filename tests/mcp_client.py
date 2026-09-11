#!/usr/bin/env python3
"""一个和 ccnm 无关的最小 stdio MCP 客户端，用来重放同一套协议。

**它故意什么都不 import**：没有 ccnm 的库，没有官方 MCP SDK，没有第三方包。
这正是它存在的理由——Rust 那边的集成测试和这里如果共用同一份实现，那"两个
客户端都同意"就只是同一段代码说了两遍。这个文件只知道 MCP 是"一行一条
JSON-RPC"，其余全靠协议说明。

它也不是 Claude Code，不是 Codex，不假装是任何 provider：ccnm 不该从
`clientInfo` 推断调用方是谁，所以这里填的名字就是一句无意义的字符串。
"""

from __future__ import annotations

import json
import subprocess
from typing import Any


class McpClient:
    """一条 stdio MCP 连接。"""

    def __init__(self, argv: list[str], env: dict[str, str] | None = None):
        self._proc = subprocess.Popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self._next_id = 0
        #: stdout 上收到的每一行原文。用来证明那里除了协议什么都没有。
        self.lines: list[str] = []
        #: `initialize.result.instructions`，握手之后才有。
        self.instructions: str = ""

    # -- 底层 --

    def call(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        """发一个请求等它的响应；服务端在中间说的别的话按 MCP 规矩跳过。"""
        self._next_id += 1
        request: dict[str, Any] = {"jsonrpc": "2.0", "id": self._next_id, "method": method}
        if params is not None:
            request["params"] = params
        assert self._proc.stdin and self._proc.stdout
        self._proc.stdin.write(json.dumps(request, ensure_ascii=False) + "\n")
        self._proc.stdin.flush()
        while True:
            line = self._proc.stdout.readline()
            if not line:
                # 服务端没了。它为什么没了写在 stderr 上，而 MCP 的规矩是
                # stdout 只放协议——所以诊断只能从那边捞，捞不到就等于没有。
                raise ConnectionError(
                    f"服务端在回答 {method} 之前关掉了 stdout\n{self._diagnostic()}"
                )
            self.lines.append(line.rstrip("\n"))
            message = json.loads(line)
            if message.get("id") == request["id"]:
                if "error" in message:
                    raise RpcError(message["error"]["code"], message["error"]["message"])
                return message["result"]

    def _diagnostic(self) -> str:
        """服务端的退出码和它留在 stderr 上的话。"""
        self._proc.wait(timeout=10)
        said = self._proc.stderr.read() if self._proc.stderr else ""
        return f"退出码 {self._proc.returncode}；stderr：\n{said.strip()}"

    def notify(self, method: str) -> None:
        assert self._proc.stdin
        self._proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
        self._proc.stdin.flush()

    # -- MCP --

    def initialize(self, protocol_version: str = "2025-06-18") -> dict[str, Any]:
        result = self.call(
            "initialize",
            {
                "protocolVersion": protocol_version,
                "capabilities": {},
                # 不假装是任何 provider：服务端不该据此改行为。
                "clientInfo": {"name": "provider-neutral-test-client", "version": "0"},
            },
        )
        self.instructions = result.get("instructions", "")
        self.notify("notifications/initialized")
        return result

    def tools(self) -> list[dict[str, Any]]:
        return self.call("tools/list", {})["tools"]

    def tool_names(self) -> list[str]:
        return sorted(tool["name"] for tool in self.tools())

    def call_tool(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        return self.call("tools/call", {"name": name, "arguments": arguments})

    # -- 收尾 --

    def close(self) -> int:
        """关掉 stdin 让服务端正常结束，返回它的退出码。"""
        if self._proc.stdin and not self._proc.stdin.closed:
            self._proc.stdin.close()
        try:
            code = self._proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            self._proc.kill()
            code = self._proc.wait()
        # 等进程结束不会释放这边的文件描述符；剩下的管道要自己关。
        for stream in (self._proc.stdout, self._proc.stderr):
            if stream and not stream.closed:
                stream.close()
        return code

    @property
    def pid(self) -> int:
        """本地那个进程的 pid。真 bridge 已经 exec 成 ssh，所以它就是 transport。"""
        return self._proc.pid

    def kill(self) -> int:
        """SIGKILL，不给它收尾的机会——这是"Host 崩了"的样子。

        正常结束用 `close()`。这里要的恰恰是不正常：Host 被 kill -9 之后，远端
        那半边和写锁该由谁回收，只有这样才问得出来。
        """
        self._proc.kill()
        code = self._proc.wait(timeout=20)
        for stream in (self._proc.stdin, self._proc.stdout, self._proc.stderr):
            if stream and not stream.closed:
                stream.close()
        return code

    def __enter__(self) -> McpClient:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()


class RpcError(Exception):
    """JSON-RPC 层的错误。**工具干的活失败不走这里**，那是 isError 结果。"""

    def __init__(self, code: int, message: str):
        super().__init__(f"[{code}] {message}")
        self.code = code
        self.message = message


def result_text(result: dict[str, Any]) -> str:
    """模型实际读到的那段文本。"""
    return result["content"][0]["text"]


def is_error(result: dict[str, Any]) -> bool:
    return bool(result.get("isError"))
