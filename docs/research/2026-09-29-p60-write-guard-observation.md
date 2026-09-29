# P60 Runtime 写锁观察与可证实的 busy（2026-09-29）

范围见[会话控制方案](../plan/core-session-control.md)第 2 节，现状来自 [P57 记录](2026-09-28-p57-core-baseline.md) 第 3.6 节（A1、D1–D4）。本轮只做离线实现与回归：**没有连 hpsrv/fodelf、没有运行真实模型或真实 Controller、没有部署或推送。**

## 1. 结论

写锁现在能被问清楚：Runtime 执行账号回答这棵工作树的写入 guard 是空闲、被占、故意留着还是说不清，`ccnm status <workspace>` 最后一行显示它。`session.start` 在分配 session id 之前先问一次：有进程正持有回 `-32008 busy`，没人持有却交不出去回 `-32007` 并给 `data.reason`，两种都 `effect: none`、什么都不启动。之前第二个 start 照样回 `starting`、派到 Agent，冲突要到 Runtime 握手才冒出来，`-32008` 没有任何路径返回（P57 A1/D1）。

**这不是预留**：看完之后别人先拿到锁，后到的会话照旧在握手处被原来那把锁拒绝；问不到时不下结论，启动照 P59 的样子进行。

| 验收 | 结果 | 证据 |
| --- | --- | --- |
| P60.1 观察语义 | free / held / abandoned / unknown 四种（不是 session 状态）＋ 8 个固定原因；锁定状态、marker 定归属，pid 只作诊断；带资源哈希、持有者、观察时间 | 2.1、3.1 |
| P60.2 查询与接线 | Operator → Agent（`agent-guard`）→ Runtime（`runtime-guard`），内部协议 9；`ccnm status <ws>` 显示；观察不建目录、不写 marker、不拿写权、不起 Agent | 2.2、3.2 |
| P60.3 busy 提前拒绝 | 同 `start_key` 先按原记录回答；held → `-32008`；残留/损坏/读不了 → `-32007`+reason；问不到 → 不下结论；握手处的最终拒绝保留 | 2.3、3.3 |
| P60.4 AU 矩阵 | AU-01…06 见 3.4；64 线程全量通过 | 3.4、4 |
| P60.5 文档 | 协议第 10 节、实现差距、fixture、运维、排错、使用说明、支持矩阵、交接、两份参考客户端注释 | 5 |

## 2. 改了什么

### 2.1 观察：`write_guard::observe`

[write_guard.rs](../../crates/ccnm-core/src/mcp/write_guard.rs)。和 `acquire` 用同一套 canonical 资源（git common-dir，不是仓库就用 root）和同一个锁文件，但只读：

| 看到 | 状态 | 原因 |
| --- | --- | --- |
| 没有锁文件，或文件是空的 | free | `never_taken` |
| `released` | free | `released` |
| 有进程持有锁（共享锁试探失败） | held | `live_holder` |
| 没人持锁，marker 是 `held …` 加一行 `abandoned …` | abandoned | `kept_on_purpose` |
| 没人持锁，marker 是 `held …` | unknown | `left_held` |
| 其他内容 | unknown | `malformed_marker` |
| 目录或文件读不了 | unknown | `unreadable` |
| 锁本身问不了 | unknown | `lock_query_failed` |

判断标准是"下一个 `acquire` 能不能拿到"：`observation_agrees_with_acquire_and_changes_nothing` 对 10 种 marker 逐一核对"说 free 当且仅当 acquire 成功"，并确认观察前后 marker 字节不变、没有锁目录时不会建出来。

pid 永远不决定状态：`owner.process` 只报 ccnm / 别的程序（号被复用）/ 已经没了 / 查不了（`ps` 跑不了），`left_held` 不会因为 pid 没了变成 free（评审 X05、P43）。P43 之前没有 pid 的旧格式 marker 标 `legacy: true`。

为区分"活着的持有者"和"残留 marker"，观察要拿一下**共享锁**，读完那一行就显式解锁，不靠关文件：别的线程刚 fork、还没 exec 的子进程共享同一个打开的文件，只靠 close 的锁会跟着它活到 exec（P34 的教训）。`observing_under_fork_pressure_leaves_no_lock_behind` 在 4 个线程不停 fork 的同时观察 200 次，每次之后立刻要得到排他锁。

副作用只剩一个几微秒的窗口：恰好这时拿锁的新 writer 会碰到 `WouldBlock`。`acquire` 因此改成最多再看 5 次、每次隔 20 ms 再判 busy；`a_writer_arriving_during_an_observation_still_gets_the_guard` 用一次拉长到 40 ms 的观察验证，把重试次数改成 0 时这条是红的。代价是真被占时拒绝晚约 100 ms。

### 2.2 谁回答：Runtime 执行账号，经 Agent 转问

写锁在执行账号自己的 state 目录里。推荐部署下 Operator 读不到那个目录，读自己的 `write-guards/` 看到的是另一个写域（D3：两个 `XDG_STATE_HOME` 就是两把锁）。能落到执行账号上的只有 Agent 到 Runtime 的那条 ssh，也就是会话工具本身走的路，所以：

```text
ccnm rpc / ccnm status <ws>  (Operator)
  → ssh Agent: ccnm internal agent-guard   认实例（只查本机登记，不开 profile 目录），按自己的配置拨 Runtime
    → ssh Runtime: ccnm internal runtime-guard   以执行账号的 state 和配置回答
```

- 新内部协议号 **9**（`runtime::GUARD_PROTOCOL`），两段都用它；旧端不认识时报版本错误，而不是答非所问。
- Runtime 只回答"这台就是该 workspace 的 Runtime、问的正是它绑定的 Agent Node"的请求，其余 `CCNM_E_CONFIG`。
- 回答里只有 workspace 名和资源哈希（即锁文件名），没有路径——它要经过 Agent 传回。
- Agent 顺带报它自己对持锁会话的记录（`owner_on_agent`：这台没有 / 运行中 / 已结束 …）：这是 Runtime 看不到的那一半，"会话早结束、锁被孤儿 `mcp-serve` 占着"就靠它认出来。`bridge-…` 这类不是 Agent 会话的名字不报。
- Operator 核对回答的实例和 workspace 与请求一致，不一致当内部错误。

`ccnm status <ws>`（Runtime Node 侧）在原有输出后多一行写锁；问不到时写"问不到 Runtime ——这不等于空闲"。文案只给人看；程序读的是协议 9 的结构和 Machine API 的码。

### 2.3 `session.start` 的预检

[rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs) 的 `preflight`，位置在参数校验之后、创建记录之前：

1. 带了 `start_key` 且已被占用：直接按原记录回答（reused / conflict / uncertain），**不问 Runtime**——占着锁的可能正是它自己的会话（`Store::find_key`，只读）。
2. 问 Runtime：

| Runtime 的回答 | 返回 | `data.reason` |
| --- | --- | --- |
| held | `-32008`，`effect: none` | `live_holder` |
| abandoned | `-32007`，`effect: none` | `kept_on_purpose` |
| unknown | `-32007`，`effect: none` | `left_held` / `malformed_marker` / `unreadable` / `lock_query_failed` |
| free | 照常创建记录、占键、派发 | — |
| 问不到（Agent 连不上、Agent 或 Runtime 太旧、Agent 到 Runtime 不通） | 照常启动，只记一条日志 | — |

`-32007` 而不是 `-32008`：这几种没人持锁，等多久都不会自己好，要人按运维手册处理；给成 busy 会让客户端无限退避。

"问不到就照常启动"是本轮的取舍，理由：这一步从来不授权，真正交权的一直是会话打开时的写入 guard，所以放行不等于把沉默当成空闲；而改成拒绝会让所有"Agent 暂时连不上"的启动从"接受后 `failed`"变成同步错误，P6 黑盒契约测试整套依赖前者（`worker.invalid`）。计划第 2.2 节该行写的是"拒绝或不确定"，这里选"不确定"：不回 busy，也不说空闲。

## 3. 验证

### 3.1 观察本身（Rust 单元）

`mcp::write_guard` 新增 6 条：与 acquire 一致且不改任何东西、活持有者 held 并点名且不受影响、pid 四种说法都不改状态、读不了是 unknown、观察中途到来的 writer 仍拿得到锁、fork 压力下不留锁。

### 3.2 真实二进制：Runtime 回答与 Agent 转问

[crates/ccnm-cli/tests/write_guard.rs](../../crates/ccnm-cli/tests/write_guard.rs) 新增 6 条，另在原有的"强杀后命令仍存活"用例里加了观察断言：

| 用例 | 内容 |
| --- | --- |
| `the_runtime_observes_its_own_guard_without_touching_it` | 无锁 free 且不建目录；真实 `mcp-serve` 持锁时 held，点名会话、workspace、它的 pid（ccnm 进程），marker 字节不变，第二个 writer 仍被拒、第一个正常收尾后 `released` |
| `a_killed_server_leaves_the_guard_unknown_and_its_pid_only_described` | SIGKILL 后 `left_held`，pid 报已没了/被复用，新 writer 仍被拒 |
| `residual_exec_child_keeps_the_workspace_unknown_until_manual_recovery`（加断言） | 强杀后后台命令还活着：观察 unknown、marker 不变；命令被人工收掉后仍 unknown，只有删 marker 才放行 |
| `a_free_observation_reserves_nothing` | 看到 free 后另一个 writer 先进，后到者被原锁拒 |
| `the_runtime_answers_only_its_bound_agent_and_its_own_protocol` | 别的 Agent Node → `CCNM_E_CONFIG`；协议 4 → `CCNM_E_VERSION`；不存在的 workspace → `CCNM_E_CONFIG`；都不建锁目录 |
| `worktrees_of_one_repository_are_observed_as_one_resource` | 真实 git 仓库 + `git worktree add`：问另一个 workspace 得到同一资源（`git_common_dir`）、同一持有者 |
| `the_agent_relays_the_runtime_answer_over_its_own_link` | 真实 `agent-guard` 经假 ssh 调真实 `runtime-guard`：身份、Runtime 回答、`owner_on_agent: not_here`、拨的是 Agent 配置里的 `runtime-alias`；未登记实例在拨号前 `CCNM_E_CONFIG`；Runtime 连不上 `CCNM_E_RUNTIME_UNREACHABLE` |

另有 `runtime::` 1 条（只有权威 Runtime、只答绑定的 Agent Node、协议号不对报版本错误）、`overview::` 1 条（各状态的写锁行文案，问不到时不说空闲）。

### 3.3 先红后绿：`tests/test_rpc_write_guard.py`

整条链只有 ssh 是假的：真实 `ccnm rpc` → 真实 `agent-guard`（读 [agent.toml](../../tests/fixtures/agent-instance/agent.toml)）→ 真实 `runtime-guard`（另一个 `XDG_STATE_HOME`，和推荐部署一样与 Operator 分开）。占锁的是真实 `ccnm internal mcp-serve`，以外部 MCP coding 会话打开（协议 5，payload 由测试自己拼）。

用 P59 的代码（`d779854`，在临时 worktree 里构建）跑这 5 条：

```text
FAIL test_a_guard_left_by_a_killed_writer_is_policy_not_busy        RpcError not raised
FAIL test_a_tree_another_writer_holds_is_busy_and_nothing_starts    RpcError not raised
FAIL test_status_names_the_holder_as_the_runtime_sees_it            'guard  held by session ext-stat' not found in
                                                                     'tmux 3.5a on the Agent Node\nno live sessions\n'
FAIL test_the_same_start_key_is_answered_before_the_guard_is_asked  0 != 1 : a taken key never asks
ok   test_no_answer_about_the_guard_starts_as_before
Ran 5 tests — FAILED (failures=4)
```

第三条就是 P57 抱怨的现象：外部 writer 正占着树，`ccnm status` 说 `no live sessions`。第五条在旧代码上本来就绿：它守的是"问不到时照旧启动"这个要保住的行为。新代码 5/5 通过。

`rpc::` 单元另加 4 条（busy 不建记录也不占键、4 种 policy 原因、3 种问不到照常启动、同键先于预检）。

### 3.4 AU 矩阵

| 编号 | 覆盖 | 未覆盖 |
| --- | --- | --- |
| AU-01 | 外部 coding 会话持锁时 RPC 的 Managed 启动回 `-32008` 且不派发（3.3）；两个入口共用一把锁的握手拒绝沿用 P11/D1 的既有回归 | 真实 Claude/Codex 会话作为持锁方；只用了外部 coding 会话和假 Agent 运行 |
| AU-02 | marker 字节、持有者、目录均不变，不起 Agent；fork 压力 200 次无残留锁（3.1、3.2） | Linux 上的 fork 压力（macOS 上 `Command` 通常走 posix_spawn，窗口更小；这条在 Linux CI 上更有意义） |
| AU-03 | 活持有、残留 held、abandoned、损坏/旧格式 marker、pid 复用、ps 查不了、读不了、Runtime 连不上各有明确说法（3.1、3.2、`overview::`） | 真实跨 UID 的"读不了"（本机用 chmod 000 模拟） |
| AU-04 | free 后插入另一个 writer，后到者被原锁拒（3.2） | RPC 层"预检 free 之后被抢先"只作为 P58 已有的 `failed` 结局存在，没有专门的端到端用例 |
| AU-05 | 强杀 `mcp-serve` 后命令仍活：观察 unknown、新 writer 被拒；命令收掉后仍 unknown，只有人工删 marker 才放行（3.2） | 自动清理——本来就不做 |
| AU-06 | 错误 Agent Node / 协议号 / workspace、同 common-dir worktree、同键重查（3.2、3.3） | 两个 state 域（D3）仍是两把锁；观察只报 Agent 那条 ssh 落到的那个账号 |

## 4. 门禁

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace -- --test-threads=64` | 1008 passed / 0 failed（P59 时 990） |
| `python3 -B scripts/ci_gates.py` | 计划、协议、Python 245 ran / 0 skipped / 0 failed（P59 时 240） |
| `python3 scripts/check_protocol.py`、`tests.test_check_protocol` | 42 + 29 fixture；24 passed |
| 中间提交单独构建 | `347b281`、`2b14a63` 各在临时 worktree 里 `cargo build --workspace --all-targets` 通过 |
| 干净构建复跑 | 见下方注意；`cargo clean -p ccnm-core -p ccnm-cli -p ccnm-testdir` 后重跑，Rust 1008、Python 245 与上面相同 |

**注意：临时 worktree 共用主工作区的 target 目录会留下旧产物。**本仓库 crate 的产物哈希与源码路径无关，worktree 里构建会覆盖它们；回到主工作区后源文件比产物旧，cargo 当成新鲜、不重编，`target/debug/ccnm` 就一直是 worktree 那一版（本轮表现为 `unrecognized subcommand 'runtime-guard'`）。用完 worktree 要 `cargo clean -p` 上面三个 crate 再构建；只 `touch` 一个文件不够，旧的 `libccnm_core` 还在。

## 5. 调用方看得到的变化

- `session.start` 现在可能在分配 session 之前回 `-32008`（`reason: live_holder`）或 `-32007`（`reason` 为上表之一），都 `effect: none`。`ExecutionBackend` 把前者归为 `unavailable`、后者归为 `rejected`，和"要不要自动重试"一致。
- 每次新启动前多一次经 Agent 到 Runtime 的只读查询。Agent 连不上时这一步最多多等一个 ssh 连接超时（10 秒）才照常接受，之后运行本身再失败一次。
- `ccnm status <ws>` 在 Runtime 侧多一次同样的查询和一行输出。
- 写锁真被占时，`mcp-serve` 的拒绝晚约 100 ms（2.1 的重试）。
- 需要 Agent 和 Runtime 都是 P60 的构建才有预检；任一端更旧时问不到，启动照 P59 进行，`ccnm status` 那一行说问不到。

## 6. 未覆盖与边界

- 没有真机、没有真实模型、没有 Linux：全部是 macOS arm64 上的离线结果，hpsrv 复验在 P62。
- 预检不是预留（计划第 2.2 节的明确要求）：看完到握手之间的竞态保留，由握手处的写入 guard 拒绝后到者。
- 锁被孤儿 `mcp-serve` 占着时仍回 `-32008`：锁确实被一个活进程持有，但它可能要等连接超时（30 秒 ping）甚至人工处理才放开。`ccnm status` 那一行会指出"Agent 那边这个会话已经结束"。
- 两个 state 域（D3）仍是两把互不相干的锁，观察只能回答 Agent 那条 ssh 落到的账号；P43 记在运维手册里的部署边界不变。
- 不自动清锁、不杀进程、不加全局锁或强制接管入口；恢复仍按[运维手册](../operations.md#写入-guard-残留)由人做。P52 同组 relay 的修复不重开。
