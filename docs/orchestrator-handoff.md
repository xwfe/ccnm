# 交给独立 Orchestrator 的边界

这份文档给**准备写一个独立编排项目（Orchestrator）的人**：它决定谁做什么、按什么顺序做、做完算不算通过、失败要不要重试；真正把 Agent 跑起来的活交给 ccnm。

Orchestrator 是**另一个仓库里的另一个产品**。ccnm 这边只交付三样东西：一条状态归属的边界线、一个最小执行接口，和一个能直接抄走的适配器示例。**新项目本身还没创建**，创建它需要单独授权，见最后一节。

先记住一句话：

> **ccnm 管执行机制**——在哪台机器上跑哪个 Agent，怎么启动、隔离、观察、停止。
> **Orchestrator 管协作策略**——谁做什么、顺序、验收、重试、分支怎么合。

---

## 1. 谁拥有哪份状态

同一件事只能有一个地方说了算。下面这张表就是分界线：

| 状态 | 谁说了算 | 另一边怎么用 |
| --- | --- | --- |
| Task（要做的事）、Assignment（派给谁）、Attempt（第几次尝试）、Handoff（交接内容）、Acceptance（验收结论） | **Orchestrator** | ccnm 完全不知道它们存在，也不需要知道 |
| execution（一次执行）、Agent 进程、Runtime 上的工具调用、workspace 写入互斥、provider/instance 身份 | **ccnm** | Orchestrator 只存一个 id，要什么现问 |

**Attempt 里存的是执行 id，不是执行状态。** 这是最容易搞错的一点：

```python
# 对：attempt 记一个外键，状态现查
attempt = {"task": "task-4821", "n": 1, "start_key": "task-4821-attempt-1",
           "execution": "s-2026091012-7f3a", "acceptance": None}
state = backend.status(attempt["execution"]).state      # 每次都问

# 错：把状态抄进自己的表，然后拿它做判断
attempt["state"] = "running"                             # 从这一刻起它就开始过期
if attempt["state"] == "running": ...                    # 可能那边早就结束了
```

为什么这么较真？因为执行状态会在你不知情的时候变——Agent 自己退出了、被超时结束了、Runtime 断了。一旦你的表里有一份"看起来也挺对"的状态，你就会拿它做决策，而它是旧的。**要展示可以缓存，但缓存要带取回时间，并且不参与任何判断。**

反过来也一样：ccnm 不存 Task、不存"这次算不算通过"。它不知道你的验收标准，也不该知道。

## 2. 一次 attempt 长什么样

```text
Orchestrator                                   ccnm
  │
  ├─ 决定：task-4821 派给 work/claude-main
  ├─ 生成 start_key = task-4821-attempt-1
  ├─ **先落盘**（key + workspace + prompt）
  │
  ├─ backend.start(...)  ───────────────────▶  接受，返回执行 id
  ├─ **把 id 落盘**
  │
  ├─ backend.status(id)  ───────────────────▶  starting / running / …
  │        （轮询，或者用 backend.wait）
  ├─ backend.result(id)  ───────────────────▶  终态 + 进程结果 + 文本
  │
  ├─ 自己判断验收：跑测试、看 diff、人工确认
  └─ 写 Acceptance（通过 / 打回 / 重试为 attempt-2）
```

两个顺序不能反：

1. **先落盘 `start_key`，再调 `start`。** 中间崩了，重来时用同一个键调一次就知道下落。
2. **先落盘执行 id，再去等结果。** 执行层**没有"列出我的执行"这种方法**，只认 id 和 `start_key`。id 丢了又没给键，那次执行会一直跑到结束，而你再也拿不到它的结果。

## 3. `start_key` 的约定

`start_key` 是**启动幂等键**：同键同输入拿回同一次执行，同键不同输入报冲突。约定写法是 `<task>-attempt-<n>`：

- 重试 = **新的 attempt = 新的键**。复用旧键只会把旧结果原样还给你，不会重跑。
- 改了 prompt 又用旧键 = 冲突（`conflict`），执行层**不猜**哪个是对的，你自己决定是换键还是接受旧结果。
- 键的作用域是单个 workspace，两个 workspace 用同一个字符串互不干扰。

崩溃窗口要单独说：键落了盘、但执行层还没确认 Agent 起没起来的时候崩了，同键再调会得到 `uncertain`——意思是**可能已经改了文件、提交了代码、发了请求**。这时候正确动作是去看现场（工作树、Git 状态），不是重发。规则细节见[协议第 6 节](protocol/machine-protocol-v1.md)。

## 4. 三个"成功"不是一回事

1. **调用成功**：这次 RPC 得到了响应。
2. **进程成功**：Agent 的退出码是 0（`state == "completed"`）。
3. **业务验收通过**：代码写对了、测试过了。

**执行层只回答前两个。** 第三个永远是 Orchestrator 自己的活：跑测试、看 diff、人工确认。把 `completed` 当成"活干对了"是这条边界上最贵的错误——Agent 完全可以一本正经地报告成功，同时什么都没改对。

同理，`unknown` 是终态，不会自己变好，遇到它去看现场而不是重试；被停掉的执行终态是 `failed`，没有单独的 `stopped`。这些语义由[协议第 7、8 节](protocol/machine-protocol-v1.md)冻结。

## 5. 最小执行接口

[clients/python/execution_backend.py](../clients/python/execution_backend.py) 是可以直接抄走的示例实现，三块：

| 名字 | 是什么 |
| --- | --- |
| `ExecutionBackend` | 协调层唯一能碰执行层的入口：`agents` / `start` / `status` / `result` / `stop` |
| `CcnmBackend` | 接到 ccnm 公开协议上的适配器，只依赖 [ccnm_machine_client.py](../clients/python/ccnm_machine_client.py)，不 import 任何 ccnm 库 |
| `FakeBackend` | 内存实现。**没有 ccnm、没有 Agent、不烧订阅额度也能跑**，用来测协调逻辑 |

```python
from execution_backend import CcnmBackend, ExecutionRequest

with CcnmBackend.spawn() as backend:
    started = backend.start(ExecutionRequest(
        workspace="my-project", prompt="跑一遍测试",
        start_key="task-4821-attempt-1",
    ))
    store.save(attempt_id, started.id)          # 先记，后等
    result = backend.wait(started.id, timeout=900)
```

错误只有五种，分类标准是"拿到它之后该做什么"，不是"底层出了什么事"：

| kind | 意思 | 该做什么 |
| --- | --- | --- |
| `rejected` | 输入、配置或权限不对 | 原样重发没用，改了再说 |
| `not_found` | 这个 id 不认识，或者不让你知道存不存在 | 查自己的记录 |
| `conflict` | 同一个 `start_key` 撞上了不同输入 | 换键，或者接受返回的那次执行 |
| `uncertain` | 可能执行了，也可能没有 | **去现场看**，不要重发 |
| `unavailable` | 现在不行，等会儿可能行 | 退避后重试 |

每个错误还带一个 `effect`：`none` 什么都没发生（重发安全）、`unknown` 不确定、`applied` 已经生效。**判断重发安不安全看 `effect`，不看 kind，也不要去解析错误文本。**

为什么不直接用 `MachineClient` 调协议？可以，但那样 JSON-RPC 错误码、`node/instance` 这种写法会散进整个协调层，协调逻辑就再也没法脱离 ccnm 单独测试了。这一层把执行收成五个方法五种错误，ccnm 特有的东西只留在 `CcnmBackend` 一个类里。接口测试见 [tests/test_execution_backend.py](../tests/test_execution_backend.py)。

## 6. 几条不能越的线

- **选了 ccnm backend，就不要在旁边另开一条执行路径。** 自己 SSH 上去、自己起进程、自己往工作树写文件，都会绕过 Runtime 的写入互斥——然后两个 Agent 同时改一棵工作树。
- **worktree 的分配、调度、合并策略在 Orchestrator；执行授权、写互斥、必要的低层操作在 ccnm。** 需要新的执行能力就给 ccnm 提一个新方法，别让协调层自己动手。
- **Orchestrator 不链接 `ccnm-core`。** 它通过自己的窄接口接执行层；ccnm 也不 import Orchestrator 的任何代码。两个产品各自发版，版本号不绑定。
- **别在 Orchestrator 里做一个 lease 就以为锁住了工作树。** 那只拦得住自己；另一个人开个 CLI 照样能写。真正的互斥在 Runtime 那一侧。
- **Agent 的输出是不可信数据。** 模型说"我修好了"不是证据，Runtime 上的副作用（文件、退出码、测试结果）才是。

## 7. 新项目的路线：交接就绪，未实施

下面四个阶段是**新项目自己的计划**，搬进它自己的仓库之后由它自己维护进度。ccnm 这边只记范围，不维护第二份进度表。

| 阶段 | 最小交付 | 验收重点 |
| --- | --- | --- |
| O1 | 显式选 Agent、持久任务/attempt、单 Agent 委派、结果和取消；无自动规划 | 重启能恢复、业务状态不与进程退出码混淆、deadline/调用预算/停止条件有效 |
| O2 | 固定 implementer→reviewer 流程，结构化 Handoff/证据/人工验收 | Agent 输出当不可信数据；review 不自动授写权；失败有限重试，不无限互聊 |
| O3 | 有数据支持的能力路由、依赖图与可控并行，确需时才独立 worktree | 不硬编码"哪个模型擅长什么"；合并前统一测试；worktree 不是安全沙箱 |
| O4 | CLI/MCP 集成及按需插件、远程客户端入口 | 先确认客户端真实 transport/auth 支持；第三方插件仍需最小权限和审计 |

**当前状态：交接就绪，未实施。** 新仓库还没创建——创建它属于新的授权范围，ccnm 这边不会替它开工，也不会在 ccnm 的进度里标记它已完成。真要开始时，第一步是把上面这张表搬进新仓库自己的计划文件，然后按它自己的验收编号推进。

搬过去时把这三样一起带上：本文第 1 节的状态归属表、第 5 节的接口，以及 [clients/python/](../clients/python/) 下的两个文件（复制，不是依赖——ccnm 不为它们提供 API 稳定性之外的任何承诺）。稳定的是[协议](protocol/README.md)，不是这两个 Python 文件。

## 8. 这份文档不保证什么

- **没有真实消费者验证过这个接口。** 它是照着已冻结的协议和一份离线测试写出来的示例；等新项目真写起来，接口大概率还要动。动的是这两个 Python 文件，不是协议。
- 接口测试**不启动任何 Agent、不拨 ssh**：`CcnmBackend` 的用例每次启动都在本地预检阶段就失败，`FakeBackend` 根本不出进程。真机闭环的证据在 [P7.3 的记录](research/p7-real-machine-2026-09-10.md)里，跟这份接口无关。
- ccnm 不会因为这份文档多出任何编排能力。Planner、Router、任务图、review/retry 策略、worktree 调度，一件都不做。
