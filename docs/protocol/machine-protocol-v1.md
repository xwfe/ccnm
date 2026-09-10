# ccnm Machine Protocol v1（草案）

**这是设计契约，不是已有功能。** 实现 `ccnm rpc` 是 P5 的事；现在仓库里没有任何命令会说这套协议。读到本文任何"服务端返回…"的句子，都要理解成"实现之后必须返回…"。

面向的读者是**要写一个外部程序去驱动 ccnm 的人**——比如独立 Orchestrator。你不需要读 ccnm 的 Rust 源码，也不需要链接它的库；你只要能启动一个子进程、往它 stdin 写字节、从 stdout 读字节。

## 1. 一次完整调用长什么样

先看全貌，细节在后面。客户端启动子进程 `ccnm rpc`，然后按行收发 JSON：

```text
→ {"jsonrpc":"2.0","id":1,"method":"hello","params":{"client":"my-orchestrator/0.1","protocol_versions":["ccnm.machine/1"]}}
← {"jsonrpc":"2.0","id":1,"result":{"protocol":"ccnm.machine/1","server":{"name":"ccnm","version":"0.2.0"},"capabilities":{"modes":["print"],"session_output":true,"start_key":true}}}

→ {"jsonrpc":"2.0","id":2,"method":"agents.list","params":{}}
← {"jsonrpc":"2.0","id":2,"result":{"agents":[{"node":"work","instance":"claude-main","provider":"claude","capabilities":{"print":true,"interactive":true,"ssh_mcp":true,"native_colocated":false},"workspaces":["ccnm"]}]}}

→ {"jsonrpc":"2.0","id":3,"method":"session.start","params":{"workspace":"ccnm","agent":{"node":"work","instance":"claude-main"},"mode":"print","input":{"prompt":"跑一遍测试"},"start_key":"task-4821-attempt-1"}}
← {"jsonrpc":"2.0","id":3,"result":{"session":"s-2026091012-7f3a","state":"starting","reused":false,"agent":{...},"workspace":"ccnm","accepted_at":"2026-09-10T12:00:03+09:00"}}

→ {"jsonrpc":"2.0","id":4,"method":"session.status","params":{"session":"s-2026091012-7f3a"}}
← {"jsonrpc":"2.0","id":4,"result":{"session":"s-2026091012-7f3a","state":"running",...}}

（轮询直到 state 是终态）

→ {"jsonrpc":"2.0","id":5,"method":"session.result","params":{"session":"s-2026091012-7f3a"}}
← {"jsonrpc":"2.0","id":5,"result":{"session":"s-2026091012-7f3a","state":"completed","outcome":{"exit_code":0,...},"text":"…","usage":{...}}}
```

**`session.start` 不等任务跑完。** 它拿到一个 session handle 就返回，之后靠 `session.status` 轮询。这是刻意的：一个可能跑十分钟的任务，不应该占着一条同步调用，客户端崩溃重连后也还能凭 session id 找回它。

## 2. 传输与消息格式

### 怎么连

客户端 `spawn("ccnm", ["rpc"])`，用它的 stdin/stdout。没有网络端口，没有 socket 文件。谁能启动这个进程，谁就有这套 API 的全部权限——权限边界由操作系统身份和 ccnm 自己的配置决定，见第 9 节。

三条流的分工是硬规定：

| 流 | 内容 |
| --- | --- |
| stdin | 只有请求，一行一条 |
| stdout | **只有协议消息**，一行一条 |
| stderr | 日志、诊断、警告 |

stdout 里出现任何非协议内容都是 bug。Agent 自己的 stdout（模型输出、CLI 的进度条）绝不会流到这里——它属于 session 的结果，通过 `session.result` 取。

### 消息格式

**JSON-RPC 2.0**，UTF-8 编码，一条消息占一行，以 `\n`（U+000A）结束。这不是自创格式：请求对象、响应对象、错误对象的成员和取值都严格按 [JSON-RPC 2.0 规范](https://www.jsonrpc.org/specification)，本文只在规范允许的地方（实现定义的错误码区间、`error.data` 的内容）做约定。

行内不允许出现没转义的换行字节——JSON 字符串里的换行必须写成 `\n` 这两个字符，标准 JSON 编码器本来就这么做，你不用额外处理。

为什么用换行分隔而不是 LSP 那种 `Content-Length` 头？因为一条消息一行，用任何语言的标准库都能读（`readline` 就够），不需要先写一个头解析器。代价是消息里不能有裸换行，而这个代价 JSON 本来就替你付了。

### 大小上限

| 限制 | 值 | 超过会怎样 |
| --- | --- | --- |
| 单条请求（含换行） | 1 MiB | 整行丢弃，回 `-32600`，`data.reason` 是 `"oversize"`，连接**不断** |
| 单条响应 | 1 MiB | 服务端自己保证：输出一律截断并给引用，见第 9 节 |

超长行为什么能安全丢弃？因为分隔符是换行：读到下一个 `\n` 就重新对齐了，不像带长度头的格式那样一错就再也找不到边界。

### 明确不支持的

- **批量请求**（JSON-RPC 允许的数组形式）：收到数组一律回 `-32600`，`data.reason` 是 `"batch_unsupported"`。批量会把幂等和顺序语义搅浑，v1 不需要它。
- **通知**（不带 `id` 的请求）：JSON-RPC 规定服务端**不得**回复通知，所以服务端只能把它丢掉并在 stderr 记一行。**别用通知**，你不会知道它有没有被执行。
- **请求的 `id` 是 `null` 或小数**：一律回 `-32600`。请求的 `id` 必须是字符串（1 到 128 个字符）或整数。

### 但响应的 `id` 可以是 `null`

有两种情况服务端根本取不到请求的 `id`：那一行不是合法 JSON（`-32700`），或者是合法 JSON 但不是合法的 Request 对象（`-32600`）。JSON-RPC 规范规定这时响应的 `id` **必须**是 `null`。

所以客户端要能处理一条 `id` 为 `null` 的错误响应。它配不到任何一个在途请求——**通常意味着你刚发出去的那一行有问题**，而不是某个具体调用失败了。收到它别去猜是哪个请求出的错，检查你的编码器。

### 服务端推送？没有

v1 里服务端**只回应请求，不主动发消息**。没有进度事件，没有流式输出。想知道进展就轮询 `session.status`。

这是为了让第一版的客户端足够简单：一个 `write` 配一个 `read`，不需要在读循环里分辨"这是我的响应还是一条推送"。将来加推送会是一次带 capability 协商的兼容扩展。

## 3. 握手

**第一条请求必须是 `hello`。** 在它之前调用任何其他方法，回 `-32014`（`handshake_required`）。

握手做三件事：确认双方说的是同一套协议、告诉客户端这个服务端有哪些能力、把版本不匹配挡在任何副作用之前。

```json
{"jsonrpc":"2.0","id":1,"method":"hello","params":{
  "client":"my-orchestrator/0.1",
  "protocol_versions":["ccnm.machine/1"]
}}
```

- `client`：自由文本，只进日志，服务端不解析它做任何决策。
- `protocol_versions`：客户端能说的协议标识数组，按偏好从高到低。

响应：

```json
{"jsonrpc":"2.0","id":1,"result":{
  "protocol":"ccnm.machine/1",
  "server":{"name":"ccnm","version":"0.2.0"},
  "capabilities":{
    "modes":["print"],
    "session_output":true,
    "start_key":true,
    "stop":true
  }
}}
```

`protocol` 是服务端从 `protocol_versions` 里选中的那一个。选不出来就不是成功响应，而是 `-32002`（`version_mismatch`），`data.supported` 列出服务端支持的标识——客户端据此决定降级还是退出。

### 协议标识长什么样

`ccnm.machine/<major>`。**只有主版本进标识**：加字段、加方法、加 capability 都不换标识；只有破坏性变更才会出现 `ccnm.machine/2`。

`server.version` 是 ccnm 这个程序的版本，**不要拿它做兼容判断**。判断能力只看两样：`protocol` 和 `capabilities`。

### capabilities 怎么用

`capabilities` 里每个键代表一项可选能力。**键不存在等于没有这项能力**，客户端必须按缺省不可用处理，不能因为自己知道有这个特性就直接调用。

| 键 | 含义 |
| --- | --- |
| `modes` | 支持的 `session.start` 模式数组，见 5.3 |
| `session_output` | `session.result` 能返回有界输出 |
| `start_key` | 支持启动幂等键 |
| `stop` | 支持 `session.stop` |

调用一个服务端没声明的能力，回 `-32013`（`unsupported_capability`）。**未知的 capability 键客户端要忽略**，不能当成错误——这是以后加能力时不破坏老客户端的前提。

## 4. 方法总览

v1 只有六个方法，名字在本阶段定稿：

| 方法 | 干什么 | 有副作用吗 |
| --- | --- | --- |
| `hello` | 握手 | 否 |
| `agents.list` | 列出可用的 Agent Instance | 否 |
| `session.start` | 启动一个 session，立刻返回 handle | **是** |
| `session.status` | 查一个 session 现在什么状态 | 否 |
| `session.result` | 取一个 session 产生了什么 | 否 |
| `session.stop` | 结束一个 session | **是** |

未知方法回 `-32601`。方法名里带 `.` 只是命名习惯，服务端按整串精确匹配，不做前缀路由。以 `rpc.` 开头的名字按 JSON-RPC 规范保留，本协议不使用。

### 参数校验

每个方法的 `params` 必须是对象（不接受位置参数数组）。多余的键一律拒绝，回 `-32602`，`data.unknown` 列出多余的键名。

**为什么不宽容地忽略多余的键？** 因为拼错的字段名会静默变成"没传"。`{"sesion":"s-1"}` 被忽略，客户端会以为查的是那个 session，实际服务端收到的是"没给 session"——报错比让人查半天强。

## 5. 方法详解

### 5.1 `hello`

见第 3 节。

### 5.2 `agents.list`

参数：`{}`（也可以省略 `params`）。

```json
{"jsonrpc":"2.0","id":2,"result":{"agents":[
  {
    "node":"work",
    "instance":"claude-main",
    "provider":"claude",
    "capabilities":{"print":true,"interactive":true,"ssh_mcp":true,"native_colocated":false},
    "workspaces":["ccnm"]
  }
]}}
```

- `node` + `instance` 一起构成寻址用的 instance 引用，`session.start` 就用这两个字段。
- `workspaces` 是这个 instance 被绑定为默认 Agent 的 workspace 名字。空数组表示配置里定义了它，但还没有 workspace 用它。

### 这里为什么没有 provider

一条 instance 是 Claude 还是 Codex，由 **Agent Node** 权威解析——这是配置模型的基本约定，Runtime Node 不保存第二份。而 `ccnm rpc` 跑在持有项目的 Runtime Node 上，它的配置里只有 workspace 到 `{node, instance}` 的绑定。

这不是"通常没有"，是**结构上不可能有**：配置校验要求 instance workspace 的 root 只在它的 Runtime Node 上定义，同时要求 instance 模式不是 colocated，两条加起来就决定了绑定里的那个 node 永远不是本机。既然给不出，协议里就不留这个字段——留一个永远缺席的字段，只会让调用方写一段永远不执行的分支。

想知道 provider，看 `session.start` / `session.status` / `session.result` 返回的 `agent.provider`——那是 Agent Node 自己报的，权威。

列表只反映**当前配置**。它不探测网络、不检查登录、不启动任何进程——想知道能不能真的跑起来，只有 `session.start` 会告诉你。

**不分页。** instance 是本机配置文件里的条目，几个到几十个，离 1 MiB 的响应上限差着几个数量级。真撞上上限说明配置本身出了问题，那时服务端回 `-32603`，而不是悄悄截断一个列表——半截列表会让调用方以为某个 instance 不存在。

### 5.3 `session.start`

```json
{"jsonrpc":"2.0","id":3,"method":"session.start","params":{
  "workspace":"ccnm",
  "agent":{"node":"work","instance":"claude-main"},
  "mode":"print",
  "input":{"prompt":"跑一遍测试并报告失败项"},
  "start_key":"task-4821-attempt-1",
  "timeout_ms":900000
}}
```

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `workspace` | 是 | 已配置的 workspace 名 |
| `agent` | 否 | 省略就用该 workspace 配置的默认 Agent |
| `mode` | 是 | 必须在 `hello` 返回的 `capabilities.modes` 里 |
| `input` | 是 | `mode` 决定它的形状；`print` 模式是 `{"prompt": "..."}` |
| `start_key` | 否 | 启动幂等键，见第 6 节 |
| `timeout_ms` | 否 | 服务端结束这个 session 的上限，见 8.2 |

成功响应：

```json
{"jsonrpc":"2.0","id":3,"result":{
  "session":"s-2026091012-7f3a",
  "state":"starting",
  "reused":false,
  "workspace":"ccnm",
  "agent":{"node":"work","instance":"claude-main","provider":"claude"},
  "accepted_at":"2026-09-10T12:00:03+09:00"
}}
```

`session` 是这次执行在本协议里的**唯一 handle**，之后所有方法都用它。它和 provider 自己的 thread/resume id 是两回事——后者出现在 `session.result` 的 `provider_session_id` 里，永远不要拿它调本协议的方法。

**`accepted_at` 只表示请求被接受。** 它不表示 Agent 已经在跑，更不表示任务开始执行。

**`agent.provider` 这时通常没有。** 同 `agents.list` 的道理：provider 由 Agent Node 权威解析，而 `session.start` 一拿到 handle 就返回，那一刻 Agent Node 还没回过话。等 `session.result` 拿到它自己报的身份，`provider` 就有了。`node` 和 `instance` 一直都在。

关于 `mode`：

- `print`：一次性执行，有结构化最终结果。这是 v1 唯一要求所有实现都支持的模式。
- `interactive`：拉起一个交互会话。**本协议不通过 JSON 传 PTY**，也**不承诺 interactive session 有结构化最终结果**——`session.result` 对它可能只有 outcome 而没有 `text`。要真正操作终端得走 ccnm 的人类 CLI（`ccnm attach`）。服务端可以不支持这个模式，那就不在 `capabilities.modes` 里出现。

启动失败时**必须留下失败记录**：如果服务端已经分配了 session id 才失败，它返回错误，同时那个 session 用 `session.status` 查得到，状态是 `failed`。不允许出现"报了错但查无此 session"或者"session 停在 starting 再也不动"。

### 5.4 `session.status`

参数 `{"session":"s-..."}`。

```json
{"jsonrpc":"2.0","id":4,"result":{
  "session":"s-2026091012-7f3a",
  "state":"running",
  "workspace":"ccnm",
  "agent":{"node":"work","instance":"claude-main","provider":"claude"},
  "started_at":"2026-09-10T12:00:03+09:00",
  "stop_requested":false
}}
```

状态模型见第 7 节。**只能按 session id 查**，没有"这个 workspace 最近那次"这种查法——那种查法在两个客户端同时用一个 workspace 时会串。

### 5.5 `session.result`

参数：

```json
{"session":"s-2026091012-7f3a","output":{"max_bytes":65536,"cursor":null}}
```

终态下的响应：

```json
{"jsonrpc":"2.0","id":5,"result":{
  "session":"s-2026091012-7f3a",
  "state":"completed",
  "workspace":"ccnm",
  "agent":{"node":"work","instance":"claude-main","provider":"claude"},
  "provider_session_id":"018f...",
  "outcome":{"exit_code":0,"timed_out":false,"duration_ms":48213,"stop_requested":false},
  "text":"三个用例失败，都在 mcp::exec 的超时分支…",
  "usage":{"input_tokens":18422,"output_tokens":1204},
  "cost":{"total_usd":0.42},
  "output":{"bytes_total":8213,"truncated":false,"cursor":null,"tail":"…"}
}}
```

- `outcome` 是**进程层面**的结果：Agent 进程怎么结束的。
- `text` 是 Agent 的最终文本，只有 provider 真的产出了结构化结果时才有。
- `usage` / `cost` 可选，**只有 provider 报了才有**，缺席不代表零。
- `output` 是有界输出，见第 9 节。

对还没到终态的 session 调用 `session.result` **不是错误**：返回当前 `state` 和已有的部分内容，`outcome` 缺席。客户端不能把"没有 outcome"理解成失败。

### 5.6 `session.stop`

参数 `{"session":"s-...","mode":"graceful"}`（`mode` 可选，默认 `graceful`）。

```json
{"jsonrpc":"2.0","id":6,"result":{
  "session":"s-2026091012-7f3a",
  "state":"stopping",
  "stop_requested":true
}}
```

**`stop` 是幂等的**：对一个已经在停的、或者已经结束的 session 再调一次，返回成功和当前状态，不报错。这样客户端重试不需要先查状态。

**返回 `stopping` 不代表已经停了。** 只有 `session.status` 报出终态才算停成功。判据在 8.3。

## 6. `id`、`start_key` 与幂等

这两个东西经常被搞混，分清楚很重要：

| | JSON-RPC 的 `id` | `session.start` 的 `start_key` |
| --- | --- | --- |
| 作用范围 | 一次 RPC 调用 | 一次"要执行的任务" |
| 谁生成 | 客户端 | 客户端 |
| 重复使用会怎样 | **响应会串**，服务端不去重 | 复用同一个 session，不重复执行 |
| 进程重启后还有效吗 | 否 | 是 |

**`id` 不是幂等键。** 它只负责把响应配回请求。同一条连接上重复用一个 `id`，服务端不保证能分辨——客户端自己保证连接内唯一即可（简单做法：自增计数器）。

### `start_key` 的规则

1. 同一个 `start_key` + **相同**的启动输入 → 返回**同一个** session，`reused` 为 `true`。不会起第二个 Agent。
2. 同一个 `start_key` + **不同**的启动输入 → `-32010`（`conflict`），`data.session` 是原来那个 session id。服务端**不猜**哪个是对的。
3. 没给 `start_key` → 每次调用都是一次新的启动。想要幂等就必须自己给键。

"相同的启动输入"指 `workspace`、`agent`、`mode`、`input` 全都逐字节相同。`timeout_ms` 不参与比较——它是执行参数，不是任务标识。

**`start_key` 的作用域是单个 workspace。** 两个不同 workspace 用同一个字符串当键，互不影响，不会撞车。想在多个 workspace 上并行跑同一个任务，直接用同一个键就行。

**没有 `session.list`。** v1 的六个方法里没有"列出我的 session"，这意味着：**session id 必须由客户端自己持久化**。客户端崩溃后如果丢了 id，唯一找回执行中 session 的办法是拿 `start_key` 再调一次 `session.start`——所以只要这次执行的结果对你有意义，就给它一个 `start_key`。不给键又丢了 id，那个 session 会一直跑到结束，而你再也拿不到它的结果。

### 崩溃窗口

`start_key` 在**拉起任何进程之前**落盘。这就留下一个窗口：键已经记下了，但服务端还没确认 Agent 到底起没起来，就崩了。

这时候下一次同 key 的 `session.start` **不会重启**，而是返回 `-32011`（`uncertain`），`data.session` 给出那个 session id，`data.effect` 是 `"unknown"`。客户端要做的是去查这个 session 的状态、去看工作树，而不是重发。

**为什么不自动重试？** 因为那个 Agent 可能已经改了文件、提交了代码、发了请求。"不确定有没有执行"和"确定没执行"是完全不同的两件事，只有后者重发才安全。

## 7. 状态与结果模型

### 状态

| `state` | 含义 | 终态 |
| --- | --- | --- |
| `starting` | 已接受，还没交给执行链路 | 否 |
| `running` | 已经交给执行链路，还没结束 | 否 |
| `stopping` | 收到过 stop，还没确认结束 | 否 |
| `completed` | Agent 自己正常结束了 | **是** |
| `failed` | Agent 异常结束、启动失败或被停掉 | **是** |
| `unknown` | 服务端无法确定 | **是**（但不是"完成"） |

三件事必须分开，混起来就会做出错误决策：

1. **IPC 成功** —— 这次 RPC 调用得到了响应。
2. **Agent 退出成功** —— `outcome.exit_code` 是 0。
3. **任务达成了业务验收** —— 代码对不对、测试过没过。

**本协议只回答前两个。** `state: "completed"` 只表示 Agent 进程正常结束，绝不表示它把活干对了。第三个问题由调用方自己判断（跑测试、看 diff、人工验收），那是 Orchestrator 的职责，不是执行层的。

### `unknown` 不是"稍后会变好"

`unknown` 表示服务端**证明不了**这个 session 的下落——进程记录丢了、supervisor 异常退出、Runtime 联系不上。它是终态：不会自己变成 `completed`。

处理 `unknown` 的正确姿势是去现场看（工作树、Git 状态、Runtime 上的进程），不是重试。服务端也不会因为等得够久就把它改成别的状态。

### 被停掉的 session 是 `failed`

调用 `session.stop` 成功结束一个 session，它的终态是 `failed`，不是单独的 `stopped`。想知道是不是自己停的，看 `stop_requested` 字段。

**为什么不给一个 `stopped` 状态？** 因为底层实测就是这样：ccnm 现在的实现把被停掉的 session 记为失败，协议造一个不存在的状态出来只会让两边对不上。宁可让客户端多读一个布尔字段，也不虚构一个服务端给不出的状态。

## 8. 生命周期、取消与断连

### 8.1 RPC 进程退出 / 管道 EOF

**客户端断开不等于任务停止。** 这是本协议最重要的一条：

- 客户端关闭 stdin（EOF）→ 服务端处理完在途请求后退出。
- **已经接受的 session 继续跑。** 不会因为 API 断了就被杀掉。
- 客户端重新 spawn 一个 `ccnm rpc`，重新 `hello`，就能凭 session id 继续 `status` / `result` / `stop`。

反过来同样重要：**断连也不会让任务被执行两遍**。已经接受的启动请求不会因为响应没送达就重跑——那正是 `start_key` 要解决的问题。

如果 `ccnm rpc` 进程自己异常退出，客户端读到 stdout 的 EOF。这时**在途请求的结果是未知的**：可能已经执行了，可能还没。带 `start_key` 的 `session.start` 可以安全地重发（会得到 `reused` 或 `uncertain`）；不带键的重发**不安全**，会起第二个 Agent。

### 8.2 超时

有两层，别混：

- **RPC 层**：单个方法调用的响应时间。客户端自己设，超时了就当作 8.1 的"未知"处理，不要假设服务端没收到。
- **session 层**：`session.start` 的 `timeout_ms`。到点服务端结束这个 session，终态是 `failed`，`outcome.timed_out` 为 `true`。

**`session.result` 不会阻塞等待。** 想等就自己轮询。

### 8.3 取消完成的判据

`session.stop` 返回成功，只表示"停止请求被接受"。真正停下来的判据是**服务端能证明这三件事全都成立**：

1. Agent 进程组已经结束；
2. 承载工具调用的 MCP transport 已经结束；
3. Runtime 上的写入 guard 已经释放。

三条里有任何一条证明不了，`state` 停在 `stopping` 或者变成 `unknown`，**绝不报 `completed`，也绝不提前释放写权限**。

**为什么这么严？** 因为写权限提前交给下一个人，两个 Agent 就会同时改一棵工作树。宁可停在 `unknown` 等人来看，也不能猜。

### 8.4 重试

服务端不替客户端重试任何东西。要不要重试由客户端决定，依据是错误里的 `data.effect`：

| `effect` | 含义 | 重发同一个请求 |
| --- | --- | --- |
| `none` | 请求被拒绝，什么都没发生 | 安全 |
| `unknown` | 可能执行了，也可能没有 | **不安全**，先去查 |
| `applied` | 已经生效了 | 会重复，别发 |

**这里刻意没有 `recoverable` 这种字段。** 一个叫"可恢复"的布尔值，读的人十有八九会理解成"自动重试是安全的"，而这两件事根本不是一回事：一个错误可以既是暂时的，又已经产生了副作用。

## 9. 权限、输出与保留

### 权限

- 能操作的只有**已配置的** workspace 和 instance。没配置的一律 `-32009`（`not_found`）。
- **不区分"不存在"和"无权访问"**，两种情况给同一个错误、同一段文本。否则错误消息本身就成了探测别人机器上有什么的工具。
- 客户端**不能**指定 workspace 的根路径、auth 路径、profile 路径或原始 provider argv。这些只由持有项目的那一侧解析，协议里根本没有对应字段。
- session 的身份（workspace + instance）**创建后不可变**。没有"把这个 session 换个 provider 接着跑"这种操作。

### 输出引用

Agent 的输出可能很大，协议不会把它整个塞进一条响应：

```json
"output":{"bytes_total":184320,"truncated":true,"cursor":"c-8192","tail":"…最后这一段…"}
```

- `bytes_total`：服务端保留的总字节数。
- `truncated`：这次给的是不是被截断了。
- `cursor`：下一页的游标，`null` 表示没有更多。把它填回 `session.result` 的 `output.cursor` 继续取。
- `max_bytes`：客户端一次想要多少，服务端可以给得更少，不会给得更多。

游标**会失效**。session 被清理、结果过期、服务端重启之后，旧游标返回 `-32012`（`expired`）。客户端必须能处理这个——不要把游标存进长期状态。

### 输出里不会出现什么

输出片段、错误消息和任何字段里都不会有：绝对的私有目录路径（home、profile、session 目录）、凭据、token、Keychain 内容、SSH 私钥路径。

需要在两台机器之间指认同一个东西时，用的是 workspace 名、instance 引用和 session id，**不是路径**。

### 保留与过期

服务端**不承诺**固定的保留期限。`session.result` 可以带一个 `expires_at`：

- 是时间戳 → 这个结果在那之后可能就取不到了；
- 是 `null` 或缺席 → 保留期未定义，**不等于永久保留**。

对一个已经不在的 session 调 `status` / `result`，得到 `-32012`（`expired`）而不是 `-32009`（`not_found`）——"曾经存在但已清理"和"从来没有过"对调用方是不同的信息，前者说明你的记录没错、只是过期了。

不过这要求服务端留一条墓碑记录才知道这个 id 曾经存在。**墓碑本身也会过期**：那之后同一个 id 会得到 `-32009`（`not_found`）。所以 `not_found` 的准确含义是"服务端不知道这个东西"，既可能从来没有过，也可能久到连墓碑都清了。客户端不要把 `not_found` 当成"我记错了"的证据。

## 10. 错误

错误对象严格按 JSON-RPC 2.0：`code`（整数）、`message`（短描述）、可选 `data`。

### `data` 的形状

```json
"data":{
  "effect":"none",
  "ccnm_code":"CCNM_E_POLICY",
  "detail":"这个 workspace 已经有一个受管写 session"
}
```

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `effect` | 是 | `none` / `unknown` / `applied`，见 8.4 |
| `ccnm_code` | 否 | ccnm 内部的稳定错误码名，便于跟 CLI 输出对照 |
| `detail` | 否 | 人类可读补充，受第 9 节约束 |
| `session` | 否 | 与错误相关的 session id |
| 其他 | 否 | 各错误码自己定义，如 `supported`、`unknown`、`reason` |

**客户端要按 `code` 判断，不要解析 `message`。** `message` 是给人看的，措辞会变。

### 错误码表

JSON-RPC 预定义的五个，含义与规范一致：

| code | 什么时候 |
| --- | --- |
| `-32700` | stdin 上那一行不是合法 JSON |
| `-32600` | 不是合法的 Request 对象；也用于批量、`id` 非法、超长行 |
| `-32601` | 方法名不认识 |
| `-32602` | 参数类型不对、缺必填、有多余的键 |
| `-32603` | 服务端内部错误 |

`-32000` 到 `-32099` 是规范留给实现自己用的区间，本协议这样分配：

| code | 名字 | 什么时候 | 典型 `effect` |
| --- | --- | --- | --- |
| `-32000` | `not_ready` | 没有已知的失败，但也没能验证通过（doctor 有 SKIP、功能没实现） | `none` |
| `-32001` | `config` | 配置缺失、解析失败或校验不过 | `none` |
| `-32002` | `version_mismatch` | 协议版本谈不拢，或两端 ccnm 版本不兼容 | `none` |
| `-32003` | `auth` | Agent 侧官方 CLI 没登录 | `none` |
| `-32004` | `agent_unreachable` | 到 Agent Node 的 SSH 不通 | `none` |
| `-32005` | `runtime_unreachable` | Agent 到 Runtime 的 SSH 不通 | `none` |
| `-32006` | `workspace` | workspace 校验失败：挂载缺失、两边不是同一个项目 | `none` |
| `-32007` | `policy` | 被安全策略拒绝 | `none` |
| `-32008` | `busy` | 同一棵工作树已经有受管写 session | `none` |
| `-32009` | `not_found` | 不存在，或无权知道它存不存在 | `none` |
| `-32010` | `conflict` | `start_key` 撞上了不同的输入 | `none` |
| `-32011` | `uncertain` | 执行状态无法确定 | `unknown` |
| `-32012` | `expired` | 结果或游标已经不在了 | `none` |
| `-32013` | `unsupported_capability` | 要用的能力这个服务端没声明 | `none` |
| `-32014` | `handshake_required` | 还没 `hello` 就调别的方法 | `none` |

**表里的 `effect` 是典型值，不是保证。** 客户端永远读实际返回的 `data.effect`，不要按错误码去查表推断。

### 旧 peer 的退出码 0 不算数

已知的坑：老版本 ccnm 收到不认识的内部协议时，可能以**退出码 0** 结束，什么都没做。

服务端必须**看协议结果**判断成功与否，绝不能因为子进程退出码是 0 就报成功。这种情况映射成 `-32002`（`version_mismatch`），而不是让一次静默的空操作冒充成功。

## 11. 和内部协议是什么关系

ccnm 内部已经有两套编号，本协议**跟它们都不是一回事**：

| | 内部控制协议 | instance session 协议 | 本协议 |
| --- | --- | --- | --- |
| 在哪 | `PROTOCOL = 1`，`crates/ccnm-core/src/protocol/` | `INSTANCE_SESSION_PROTOCOL = 3` | `ccnm.machine/1` |
| 谁跟谁说 | 两台机器上的 ccnm 之间 | 同上，带 instance 身份的消息 | 外部程序 ↔ `ccnm rpc` |
| 怎么传 | base64url(JSON) 挂在 ssh 命令行上 | 同上 | stdio 上的 JSON-RPC 行 |
| 会变吗 | 内部实现，随时可改 | 同上 | 公开契约，破坏性变更要升主版本 |

三者的版本号**各走各的**，不要求同步。内部 payload 结构（`session_dir`、`controller`、`pid`、`tmux_session` 之类）**不会**原样出现在本协议里——那些是实现细节，暴露出去就等于把内部结构冻结成公开契约。

同理，本协议的实现**不允许**通过调用人类 CLI 再解析它打印的文案来完成。人类 CLI 和 RPC 共用同一套应用逻辑，不是互相包装。

## 12. 这一版明确不做的

- 网络传输、HTTP、鉴权、远程客户端。stdio 之外的接入是后续阶段的事。
- 服务端推送、流式输出、进度事件。
- 通过 JSON 传 PTY，或者对 interactive session 承诺结构化最终结果。
- 批量请求。
- 多 Agent 编排、任务图、路由、重试策略、worktree 调度——那些属于独立 Orchestrator，不是执行层。
- 第三个 provider。

## 13. 稳定性

**这是草案，不是稳定的 v1。** 在有外部消费者真的按它写出程序并验证之前（P6），字段和语义都还可能改。现在不要在 README 或任何对外文档里宣称"v1 已稳定"。

定稿之后的兼容规则会是：加可选字段、加方法、加 capability 都属于兼容变更，客户端必须忽略不认识的字段；改字段含义、删字段、改错误码语义属于破坏性变更，要出 `ccnm.machine/2`。

## 14. 机器可检查的部分

本文的每个消息形状都有对应的 JSON Schema 和 fixture：

- schema：[`schema/`](schema/)
- fixture：[`fixtures/`](fixtures/)，覆盖成功、拒绝、断线、未知终态和过期结果

跑一遍：

```bash
python3 scripts/check_protocol.py
```

它校验每个 fixture 都符合自己声明的 schema、错误码都在本文的表里、以及 schema 自身没有拼写错的关键字。**它不证明实现正确**——现在还没有实现。
