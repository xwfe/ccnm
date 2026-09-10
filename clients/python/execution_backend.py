#!/usr/bin/env python3
"""独立 Orchestrator 用的最小执行接口，外加 ccnm adapter 和一个假后端。

**这个文件是给外部程序抄走用的**，不是 ccnm 内部代码。它只用 Python 标准库，
不 import 任何 ccnm 的 Rust 库；`CcnmBackend` 依赖的
`ccnm_machine_client.py` 也只是同一个目录下另一个可以抄走的单文件客户端。
边界为什么这么画，见 docs/orchestrator-handoff.md。

里面三块东西：

1. `ExecutionBackend`：协调层唯一能碰执行层的入口，五个方法。
2. `CcnmBackend`：把它接到 ccnm 的公开协议（`ccnm rpc`）上。
3. `FakeBackend`：内存实现。**没有 ccnm、没有 Agent、没有订阅额度也能跑**，
   用来测你自己的协调逻辑——任务怎么排、什么时候算验收通过、失败重试几次。

为什么要有这一层，直接调 `MachineClient` 不行吗？可以，但你会把 ccnm 的
JSON-RPC 错误码、`node/instance` 结构、`session` 这些词写进协调逻辑里。那样
一来协调逻辑就没法脱离 ccnm 单独测试，二来将来接第二种执行后端要改的地方遍布
全项目。这个接口把"执行"收成五个方法和五种错误，ccnm 特有的东西只留在
`CcnmBackend` 这一个类里。

最小用法：

    from execution_backend import CcnmBackend, ExecutionRequest

    with CcnmBackend.spawn() as backend:
        started = backend.start(ExecutionRequest(
            workspace="my-project",
            prompt="跑一遍测试",
            start_key="task-4821-attempt-1",   # 想要幂等就必须自己给
        ))
        # 先把 started.id 存进你自己的 attempt 记录，再去等结果。顺序反了，
        # 中间崩一次就再也找不回这次执行——执行层没有"列出我的 session"。
        result = backend.wait(started.id, timeout=900)
        print(result.state, result.exit_code, result.text)

换成 `FakeBackend()` 之后同一段协调代码照样跑，只是不会有 Agent 被启动。
"""

from __future__ import annotations

import abc
from dataclasses import dataclass, field
import time
from typing import Any, Callable, Dict, List, Optional, Tuple

try:
    # 只用 ExecutionBackend + FakeBackend 的人可以不带这个文件，所以这里
    # 允许它缺席；真去构造 CcnmBackend 时才报错，报错点就在缺的东西旁边。
    from ccnm_machine_client import MachineClient, RpcError
except ImportError:  # pragma: no cover - 取决于抄走了哪几个文件
    MachineClient = None  # type: ignore[assignment]
    RpcError = None  # type: ignore[assignment]


# 三个终态。和 `ccnm_machine_client.TERMINAL_STATES` 是同一件事的两份声明——
# 两个文件都必须能单独被抄走，所以不能互相 import。协议第 13 节把这三个冻住
# 了，不会再加终态，所以这份复制不会漂移；仓库里有一条测试盯着它们相等。
#
# **不认识的 state 一律当成"还没结束"继续等**。反过来做（把没见过的状态当成
# 结束了）会让协调层以为一个还在改文件的执行已经收工。
TERMINAL_STATES = frozenset({"completed", "failed", "unknown"})

# 五种错误。分这五种的标准是"协调层拿到它之后该做什么"，不是"底层出了什么事"。
KIND_REJECTED = "rejected"        # 输入/配置/权限不对，原样重发没有意义
KIND_NOT_FOUND = "not_found"      # 这个 id 后端不认识，或者不让你知道存不存在
KIND_CONFLICT = "conflict"        # 同一个 start_key 撞上了不同的输入
KIND_UNCERTAIN = "uncertain"      # 可能执行了也可能没有，**必须去现场看**
KIND_UNAVAILABLE = "unavailable"  # 现在不行，等会儿可能行

# 每种错误默认的副作用。**这只是默认值**：后端报了 effect 就以后端的为准。
_DEFAULT_EFFECT = {
    KIND_REJECTED: "none",
    KIND_NOT_FOUND: "none",
    KIND_CONFLICT: "none",
    KIND_UNCERTAIN: "unknown",
    KIND_UNAVAILABLE: "unknown",
}


class BackendError(Exception):
    """执行后端拒绝了这次调用，或者没法回答。

    `effect` 说的是这次调用**留没留下副作用**：`none` 什么都没发生，重发安全；
    `unknown` 可能已经执行了，重发不安全；`applied` 已经生效，重发会重复。

    注意 `effect` 和"这件事还能不能再试"是两码事。一个执行失败了、副作用是
    `none`，不代表换个 prompt 重来一次就会成功；那是协调层自己的判断。
    """

    def __init__(
        self,
        kind: str,
        message: str,
        effect: Optional[str] = None,
        execution: Optional[str] = None,
        code: Optional[int] = None,
    ):
        super().__init__(f"[{kind}] {message}")
        self.kind = kind
        self.message = message
        self.effect = effect or _DEFAULT_EFFECT.get(kind, "unknown")
        # 冲突和不确定这两种错误会带上"是哪一次执行"，协调层照着它去查现场。
        self.execution = execution
        # 后端自己的原始错误码，只用来写日志和对账，不要拿它做分支判断——
        # 换一个后端这个值就没了，而上面四个字段还在。
        self.code = code

    @property
    def safe_to_resend(self) -> bool:
        """原样重发这次调用安不安全。"""
        return self.effect == "none"


@dataclass(frozen=True)
class AgentRef:
    """一个可以被指派活的执行对象。

    `id` 是**不透明字符串**：协调层只负责把它存下来、原样传回去，不要解析它的
    结构。ccnm 这边它长得像 `work/claude-main`，换个后端就是别的样子。
    """

    id: str
    workspaces: Tuple[str, ...] = ()


@dataclass(frozen=True)
class ExecutionRequest:
    """要执行的一件事。

    `start_key` 是**启动幂等键**，作用域是"一次要执行的任务"，不是一次网络调用。
    同键同输入拿回同一次执行，同键不同输入报冲突。想重跑就换一个键（约定是
    带上 attempt 序号），复用旧键只会拿回旧结果。
    """

    workspace: str
    prompt: str
    agent: Optional[str] = None
    start_key: Optional[str] = None
    timeout_ms: Optional[int] = None


@dataclass(frozen=True)
class Execution:
    """`start` 的返回：拿到 handle 就返回，**不代表活已经开始干**。"""

    id: str
    state: str
    reused: bool = False

    @property
    def terminal(self) -> bool:
        return self.state in TERMINAL_STATES


@dataclass(frozen=True)
class ExecutionStatus:
    id: str
    state: str
    stop_requested: bool = False

    @property
    def terminal(self) -> bool:
        return self.state in TERMINAL_STATES


@dataclass(frozen=True)
class ExecutionResult:
    """一次执行的结果。

    **`state == "completed"` 只表示进程正常结束，不表示活干对了。** 干没干对由
    协调层自己判断：跑测试、看 diff、人工验收。这个接口不回答那个问题。
    """

    id: str
    state: str
    exit_code: Optional[int] = None
    timed_out: bool = False
    stop_requested: bool = False
    text: Optional[str] = None
    agent: Optional[str] = None
    # 后端报的执行引擎标识（ccnm 这边是 provider 名）。可能没有；只用于记录，
    # 不要拿它反过来做路由决策。
    engine: Optional[str] = None
    usage: Dict[str, Any] = field(default_factory=dict)
    # 只有后端真的报了钱才有。**缺席不等于零。**
    cost_usd: Optional[float] = None

    @property
    def terminal(self) -> bool:
        return self.state in TERMINAL_STATES


class ExecutionBackend(abc.ABC):
    """协调层能对执行层做的全部事情。

    五个方法之外没有别的口子：想直接 SSH 上去、自己起进程、自己往工作树写文件，
    都是绕过这层边界。真需要新能力就给后端提一个新方法，不要在协调层里另起一套
    执行路径——那样两边会同时改同一棵工作树，而写入互斥在执行层。
    """

    @abc.abstractmethod
    def agents(self) -> List[AgentRef]:
        """能指派的对象。可能是空列表；空不等于出错。"""

    @abc.abstractmethod
    def start(self, request: ExecutionRequest) -> Execution:
        """接受一次执行，拿到 handle 就返回，不等它跑完。"""

    @abc.abstractmethod
    def status(self, execution_id: str) -> ExecutionStatus:
        """按 id 查状态。没有"最近那一次"这种查法。"""

    @abc.abstractmethod
    def result(self, execution_id: str) -> ExecutionResult:
        """按 id 取结果。**没到终态不是错误**，返回的是当前状态和已有内容。"""

    @abc.abstractmethod
    def stop(self, execution_id: str) -> ExecutionStatus:
        """请求停止。幂等。**返回不代表已经停了**，到终态才算。"""

    def close(self) -> None:
        """释放这条连接。**已经接受的执行不会因此停止。**"""

    def __enter__(self) -> "ExecutionBackend":
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def wait(
        self, execution_id: str, timeout: float = 900.0, poll: float = 0.5
    ) -> ExecutionResult:
        """轮询到终态再取结果。

        不认识的状态会继续等，所以后端将来加了新的非终态状态也不会让这里提前
        收工；代价是等到超时才发现异常，那正是 `timeout` 存在的意义。
        """
        deadline = time.monotonic() + timeout
        while True:
            current = self.status(execution_id)
            if current.terminal:
                return self.result(execution_id)
            if time.monotonic() >= deadline:
                raise TimeoutError(
                    f"{execution_id} 在 {timeout} 秒内没有结束（当前 {current.state}）"
                )
            time.sleep(poll)


# -- ccnm adapter ----------------------------------------------------------

# JSON-RPC 错误码 → 五种 kind。分类标准还是"协调层该做什么"：
# `unavailable` 是环境问题，退避之后重来可能就好了；`rejected` 是这次请求本身
# 不对，原样重发一万次也一样。**effect 不查这张表**，永远读服务端实际返回的
# `data.effect`（协议第 10 节明说码表里的 effect 只是典型值）。
_KIND_BY_CODE = {
    -32000: KIND_UNAVAILABLE,  # not_ready：没验证通过，不是失败
    -32004: KIND_UNAVAILABLE,  # agent_unreachable
    -32005: KIND_UNAVAILABLE,  # runtime_unreachable
    -32008: KIND_UNAVAILABLE,  # busy：当前 build 从不返回，留着以防将来实现
    -32603: KIND_UNAVAILABLE,  # 服务端内部错误，不是调用方的输入问题
    -32009: KIND_NOT_FOUND,
    -32010: KIND_CONFLICT,
    -32011: KIND_UNCERTAIN,
}


class CcnmBackend(ExecutionBackend):
    """把 `ExecutionBackend` 接到 ccnm 的公开协议上。

    **这个类是 ccnm 特有的东西的全部落点**：JSON-RPC 错误码、`node/instance`
    这种 agent id 写法、`session` 这个词，都只出现在这里。协调层看不到它们。

    `connect` 是一个"给我一条新连接"的函数，返回 `MachineClient`。之所以不直接
    收一个已经连好的客户端，是因为 `ccnm rpc` 挂了之后要能重连：**断连不会停掉
    已经接受的执行**，重连之后凭 id 照样查得到。
    """

    def __init__(
        self,
        connect: Callable[[], "MachineClient"],
        client_name: str = "execution-backend/1",
    ):
        if MachineClient is None:  # pragma: no cover - 取决于抄走了哪几个文件
            raise RuntimeError(
                "CcnmBackend 需要同目录下的 ccnm_machine_client.py，把那个文件也抄过来"
            )
        self._connect = connect
        self._client_name = client_name
        self._client: Optional[Any] = None

    @classmethod
    def spawn(
        cls,
        ccnm: str = "ccnm",
        config: Optional[str] = None,
        env: Optional[Dict[str, str]] = None,
        argv: Optional[List[str]] = None,
        client_name: str = "execution-backend/1",
    ) -> "CcnmBackend":
        """常规用法：每次连接就起一个 `ccnm rpc` 子进程。"""
        if MachineClient is None:  # pragma: no cover
            raise RuntimeError(
                "CcnmBackend 需要同目录下的 ccnm_machine_client.py，把那个文件也抄过来"
            )

        def connect() -> "MachineClient":
            return MachineClient(ccnm=ccnm, config=config, env=env, argv=argv)

        return cls(connect, client_name=client_name)

    # -- 连接 --

    def _live(self) -> Any:
        """拿一条握过手的连接，没有就建一条。"""
        if self._client is None:
            client = self._connect()
            try:
                client.hello(self._client_name)
            except Exception:
                client.close()
                raise
            self._client = client
        return self._client

    def _drop(self) -> None:
        if self._client is not None:
            try:
                self._client.close()
            finally:
                self._client = None

    def close(self) -> None:
        self._drop()

    def _call(self, method: Callable[[Any], Any], retry_on_disconnect: bool) -> Any:
        """调一次，把协议错误翻译成 `BackendError`。

        `retry_on_disconnect` 只给**只读或幂等**的调用开（status/result/stop）。
        这几个重发不会多干一次活，所以连接断了直接换一条重来。`start` 永远不开
        它——重发一个没有幂等键的启动就是多起一个 Agent。
        """
        try:
            return method(self._live())
        except ConnectionError:
            self._drop()
            if not retry_on_disconnect:
                raise
        # 走到这里说明第一次断了。第二次再断就让它抛出去，不无限重连。
        try:
            return method(self._live())
        except ConnectionError as exc:
            raise BackendError(
                KIND_UNAVAILABLE, f"ccnm rpc 连不上：{exc}", effect="unknown"
            ) from exc

    def _translate(self, exc: Any) -> BackendError:
        data = getattr(exc, "data", None) or {}
        return BackendError(
            _KIND_BY_CODE.get(exc.code, KIND_REJECTED),
            exc.message,
            # 永远用服务端实际给的 effect；契约要求每个错误都带，缺了按最保守的读。
            effect=data.get("effect", "unknown"),
            execution=data.get("session"),
            code=exc.code,
        )

    # -- 五个方法 --

    def agents(self) -> List[AgentRef]:
        try:
            raw = self._call(lambda c: c.agents_list(), retry_on_disconnect=True)
        except RpcError as exc:
            raise self._translate(exc) from exc
        return [
            AgentRef(
                id=f"{item['node']}/{item['instance']}",
                workspaces=tuple(item.get("workspaces", ())),
            )
            for item in raw
        ]

    def start(self, request: ExecutionRequest) -> Execution:
        agent = None
        if request.agent is not None:
            if "/" not in request.agent:
                raise BackendError(
                    KIND_REJECTED,
                    f"ccnm 的 agent id 是 node/instance，收到 {request.agent!r}",
                )
            node, instance = request.agent.split("/", 1)
            agent = {"node": node, "instance": instance}
        try:
            raw = self._call(
                lambda c: c.session_start(
                    workspace=request.workspace,
                    prompt=request.prompt,
                    agent=agent,
                    start_key=request.start_key,
                    timeout_ms=request.timeout_ms,
                ),
                retry_on_disconnect=False,
            )
        except RpcError as exc:
            raise self._translate(exc) from exc
        except ConnectionError as exc:
            # 响应没回来。这次启动**可能已经被接受了**，两种情况差别很大：
            self._drop()
            if request.start_key is not None:
                # 有幂等键：同键再调一次是安全的，会拿到 reused 或者 uncertain。
                raise BackendError(
                    KIND_UNCERTAIN,
                    f"启动响应没收到，用同一个 start_key 重调查明下落：{exc}",
                    effect="unknown",
                ) from exc
            # 没有键：重发就是再起一个 Agent。只能去现场看。
            raise BackendError(
                KIND_UNAVAILABLE,
                f"启动响应没收到，而这次启动没有 start_key，重发会重复执行：{exc}",
                effect="unknown",
            ) from exc
        return Execution(
            id=raw["session"], state=raw.get("state", "starting"), reused=bool(raw.get("reused"))
        )

    def status(self, execution_id: str) -> ExecutionStatus:
        try:
            raw = self._call(
                lambda c: c.session_status(execution_id), retry_on_disconnect=True
            )
        except RpcError as exc:
            raise self._translate(exc) from exc
        return ExecutionStatus(
            id=raw["session"],
            state=raw["state"],
            stop_requested=bool(raw.get("stop_requested")),
        )

    def result(self, execution_id: str) -> ExecutionResult:
        try:
            raw = self._call(
                lambda c: c.session_result(execution_id), retry_on_disconnect=True
            )
        except RpcError as exc:
            raise self._translate(exc) from exc
        outcome = raw.get("outcome") or {}
        agent = raw.get("agent") or {}
        cost = raw.get("cost") or {}
        return ExecutionResult(
            id=raw["session"],
            state=raw["state"],
            exit_code=outcome.get("exit_code"),
            timed_out=bool(outcome.get("timed_out")),
            stop_requested=bool(outcome.get("stop_requested") or raw.get("stop_requested")),
            text=raw.get("text"),
            agent=(
                f"{agent['node']}/{agent['instance']}"
                if agent.get("node") and agent.get("instance")
                else None
            ),
            engine=agent.get("provider"),
            usage=raw.get("usage") or {},
            cost_usd=cost.get("total_usd"),
        )

    def stop(self, execution_id: str) -> ExecutionStatus:
        try:
            raw = self._call(
                lambda c: c.session_stop(execution_id), retry_on_disconnect=True
            )
        except RpcError as exc:
            raise self._translate(exc) from exc
        return ExecutionStatus(
            id=raw["session"],
            state=raw["state"],
            stop_requested=bool(raw.get("stop_requested", True)),
        )


# -- 测协调逻辑用的假后端 --------------------------------------------------


class FakeBackend(ExecutionBackend):
    """内存实现，不启动任何东西。

    它存在的理由：协调逻辑（任务怎么排、什么算验收通过、失败重试几次）应该能在
    没有 ccnm、没有 Agent、不烧订阅额度的情况下测。**它不是 ccnm 的模拟器**，
    只保证这个接口承诺的那几条语义成立：启动幂等、冲突、停止后是失败、终态只有
    三个、不认识的状态不算结束。

    默认行为：每次执行在第一次 `status` 时还是 `running`，第二次变成终态。想控制
    结果就用 `on()`；想演崩溃窗口就用 `crash_window()`；想演一个这一版没定义过的
    状态就用 `force_state()`。
    """

    def __init__(self, agents: Tuple[str, ...] = ("fake/one",), polls_before_done: int = 1):
        self._agents = [AgentRef(id=a) for a in agents]
        self._polls_before_done = polls_before_done
        self._records: Dict[str, Dict[str, Any]] = {}
        self._by_key: Dict[Tuple[str, str], str] = {}
        self._script: Dict[str, Dict[str, Any]] = {}
        self._counter = 0
        #: 真正被启动过的执行 id，按顺序。测幂等就断言它的长度。
        self.executions: List[str] = []

    # -- 剧本 --

    def on(
        self,
        prompt: str,
        state: str = "completed",
        exit_code: int = 0,
        text: Optional[str] = None,
        timed_out: bool = False,
    ) -> None:
        """指定某个 prompt 的结局。"""
        self._script[prompt] = {
            "state": state,
            "exit_code": exit_code,
            "text": text,
            "timed_out": timed_out,
        }

    def crash_window(self, workspace: str, start_key: str) -> str:
        """演"键已经落盘、但没人知道 Agent 起没起来"。

        之后用这个键调 `start` 会拿到 `uncertain`，**不会**重新启动一次。返回那次
        下落不明的执行 id。
        """
        record = self._new_record(workspace, "(unknown)", None, start_key)
        record["state"] = "unknown"
        record["uncertain"] = True
        self._by_key[(workspace, start_key)] = record["id"]
        # 注意这里没有 append 到 self.executions：崩溃窗口的意思就是不知道它跑没跑。
        return record["id"]

    def force_state(self, execution_id: str, state: str) -> None:
        """把某次执行的状态改成任意字符串，包括这一版没定义过的。"""
        self._records[execution_id]["state"] = state
        self._records[execution_id]["frozen"] = True

    # -- 五个方法 --

    def agents(self) -> List[AgentRef]:
        return list(self._agents)

    def start(self, request: ExecutionRequest) -> Execution:
        if request.start_key is not None:
            known = self._by_key.get((request.workspace, request.start_key))
            if known is not None:
                record = self._records[known]
                if record.get("uncertain"):
                    raise BackendError(
                        KIND_UNCERTAIN,
                        "这个 start_key 上一次的下落不明，去看现场，别重发",
                        execution=known,
                    )
                if (record["agent"], record["prompt"]) != (request.agent, request.prompt):
                    raise BackendError(
                        KIND_CONFLICT,
                        "同一个 start_key 配了不同的输入",
                        execution=known,
                    )
                return Execution(id=known, state=record["state"], reused=True)

        record = self._new_record(
            request.workspace, request.prompt, request.agent, request.start_key
        )
        if request.start_key is not None:
            self._by_key[(request.workspace, request.start_key)] = record["id"]
        self.executions.append(record["id"])
        return Execution(id=record["id"], state=record["state"], reused=False)

    def status(self, execution_id: str) -> ExecutionStatus:
        record = self._get(execution_id)
        self._advance(record)
        return ExecutionStatus(
            id=record["id"], state=record["state"], stop_requested=record["stop_requested"]
        )

    def result(self, execution_id: str) -> ExecutionResult:
        record = self._get(execution_id)
        if record["state"] not in TERMINAL_STATES:
            # 没到终态也照样回答，只是没有 outcome——和真实契约一致。
            return ExecutionResult(
                id=record["id"], state=record["state"], agent=record["agent"]
            )
        script = self._script.get(record["prompt"], {})
        stopped = record["stop_requested"]
        return ExecutionResult(
            id=record["id"],
            state=record["state"],
            exit_code=1 if stopped else script.get("exit_code", 0),
            timed_out=bool(script.get("timed_out")),
            stop_requested=stopped,
            text=None if stopped else script.get("text", f"fake ran: {record['prompt']}"),
            agent=record["agent"],
            engine="fake",
        )

    def stop(self, execution_id: str) -> ExecutionStatus:
        record = self._get(execution_id)
        record["stop_requested"] = True
        if record["state"] not in TERMINAL_STATES:
            # 收到了，还没确认结束。**不能直接报终态**——真实执行层要证明进程组
            # 结束、transport 结束、写入 guard 释放，才敢改状态。
            record["state"] = "stopping"
        return ExecutionStatus(id=record["id"], state=record["state"], stop_requested=True)

    # -- 内部 --

    def _new_record(
        self, workspace: str, prompt: str, agent: Optional[str], start_key: Optional[str]
    ) -> Dict[str, Any]:
        self._counter += 1
        record = {
            "id": f"x-{self._counter}",
            "workspace": workspace,
            "prompt": prompt,
            "agent": agent,
            "start_key": start_key,
            "state": "starting",
            "stop_requested": False,
            "polls": 0,
            "frozen": False,
        }
        self._records[record["id"]] = record
        return record

    def _get(self, execution_id: str) -> Dict[str, Any]:
        record = self._records.get(execution_id)
        if record is None:
            raise BackendError(KIND_NOT_FOUND, f"没有这次执行：{execution_id}")
        return record

    def _advance(self, record: Dict[str, Any]) -> None:
        if record["frozen"] or record["state"] in TERMINAL_STATES:
            return
        record["polls"] += 1
        if record["polls"] <= self._polls_before_done:
            if record["state"] == "starting":
                record["state"] = "running"
            return
        if record["stop_requested"]:
            # 被停掉的执行终态是失败，不是单独的 "stopped"——执行层给不出那个状态。
            record["state"] = "failed"
        else:
            record["state"] = self._script.get(record["prompt"], {}).get("state", "completed")
