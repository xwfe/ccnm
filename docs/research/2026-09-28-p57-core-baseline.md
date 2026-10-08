# P57 当前构建基线与核心缺口复现（2026-09-28）

范围见[基线与验收方案](../plan/core-verification.md)第 1 节。本轮只做离线基线、零额度反证、样例项目和 P62 进场清单：**没有连 hpsrv/fodelf、没有运行真实模型、没有部署或改系统配置、没有改任何产品代码。** 下面每一条结论都来自本轮在本机跑出的真实二进制结果，不是读代码的推断；读代码只用来说明“为什么会这样”。

## 1. 结论

| 缺口 | 判定 | 一句话 | 证据 |
| --- | --- | --- | --- |
| CTRL-01 | **复现，且比计划写的更重** | RPC 的 `session.stop` 发给 Agent 的请求不带任何会话身份；Agent 收到无 session 的 stop 只查 tmux，而 print 运行不在 tmux 里——**print 运行根本停不到**，同 workspace 有交互会话时改停那个并回报成功 | A1、B1、B2 |
| CTRL-02 | 复现（确定性） | stop 标志被运行线程的旧副本写回 false；完成先落盘时，stop 用旧副本把 `completed` 改回 `stopping`、finish 丢失 | A2、A3、A4 |
| CTRL-02 固定 `.tmp` | 静态待验证 | 两个进程 120 轮交错没撞出 `.tmp` 冲突或半截 JSON；不能据此说不存在 | A4 |
| CTRL-03 | 复现 | start_key 经 `safe_name` 丢字符、截 64，不同键合并；session 句柄直接拼路径，`../`、绝对路径、symlink 都能读到 `sessions/` 以外的记录；运行中改配置，旧句柄的 stop 发往新机器 | A5、A6、A7 |
| CT-05 边界 | 部分偏离 | 空键文件（create 后、写入前崩溃）返回 `uncertain` 但 `data.session` 是空串，不合 schema；指向缺失记录的键按契约返回 `uncertain`；半截记录报 `-32603` | A8 |
| OUT-01 | **复现，并纠正计划的一处事实** | Agent 在 RunReport 里只带回 stdout 末尾 **2 KiB**（前缀 `...`），不是 8 KiB；RPC 把这段尾巴的长度当 `bytes_total`、报 `truncated=false`，3 MiB 输出的头和中间在 Agent 那一步就没了；`max_bytes` 被忽略，非法值也收 | C0、C1、C2、C4 |
| OUT-02 | 复现 | 解析成功时最终回答完整地走 `text`（20 KiB 头中尾都在）、`output` 为空；stderr 在 RPC 结果和记录里都找不到 | C0、C3、C5 |
| AUTH-01 | 复现（符合计划描述） | 同 workspace 第二个 `session.start` 回 `starting` 不回 busy，两次都派到了 Agent；拒绝只发生在 Runtime MCP 握手（同 workspace、同 git common-dir 都拒） | A1、D1、D2 |
| AUTH-02 | 已知边界，行为如文档 | mcp-serve 被 SIGKILL 后后台命令仍在、marker 停在 `held`、下一个 writer 被拒；两个 state 域各自放行 | D3、D4、写锁回归 3 条 |
| CLEAN-01 | 复现（路径路由层面） | `workspace remove --purge` 本机只删调用者 state 下的 `sessions/<id>`；Executor state 下同一会话的保留输出、RPC 记录与 start_key 都不动，而 workspace 配置照样被删 | E |

没有一条计划假设被证伪。OUT-01 的“8192 字节”是计划写错了源头的截断位置；CTRL-01 比计划担心的“可能停错”更重。两处都已回写到[总纲](../plan/core-hardening.md)第 2 节。

## 2. P57.1 构建与测试基线

| 项 | 值 |
| --- | --- |
| 源码 | `main` / `37a151a5ff4e36a428baef59923cc6350443df91`；已跟踪文件无改动，已暂存为空；未跟踪只有用户的 `ccnm-mobile-handoff.md`（未读、未动） |
| 系统 | macOS 26.6.2（25G83），arm64 |
| 工具 | rustc 1.98.0（88d9e12ae 2026-08-18）、cargo 1.98.0、Python 3.12.12、Node v24.9.0、npm 11.6.0、git 2.55.0 |
| 磁盘 | 数据卷剩 5.0 GiB（`target/` 占 20 GiB）；探针全放 `/tmp/p57-*`，跑完即删 |

| 命令 | 退出码 | 耗时 | 结果 |
| --- | --- | --- | --- |
| `cargo fmt --all --check` | 0 | 1 s | 无差异 |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | 0 | 0.4 s | 命中缓存；`-D warnings` 下缓存命中即上次同源码无告警 |
| `cargo test --workspace --locked --offline` | 0 | 72 s | 946 passed / 0 failed / 0 ignored（19 个测试二进制 + 2 组 doc-test） |
| `python3 -B scripts/ci_gates.py` | 0 | 35 s | check_plan 通过（53/59）、check_protocol 通过（38 + 29 fixture）、Python 219 ran / 0 skipped / 0 failed |

`--locked --offline` 是本机额外加的（CI 不带）：它们通过说明 `Cargo.lock` 与清单一致、依赖全在本地缓存，没有借网络把构建“修”好。`ci_gates.py` 按 CI 原样不带这两个参数。`status.json` 的 `baseline`（448 / 7）是 2026-09-07 的历史数，当前数以上表为准。

本机 pnpm（mise 装的 12.6.0）的启动脚本报 `SyntaxError: Invalid or unexpected token`，本轮不需要它，样例也没用；记下只为说明“Node 样例零依赖”不是凑巧。

## 3. P57.2 缺口复现

### 3.1 方法

四个探针加一个公共模块，当时放在 `probes/` 下。P58–P61 已把对应项改写成正式回归（`tests/test_rpc_exact_control.py`、`tests/test_rpc_output.py`、`tests/test_rpc_write_guard.py`、`tests/test_cleanup.py`），探针本身 2026-10-08 删除，原文在 git 历史里：

| 文件 | 覆盖 | 被测的真实二进制 | 假的只有 |
| --- | --- | --- | --- |
| `p57-rpc-control.py` | CTRL-01/02/03、AUTH-01 的 RPC 一半 | `ccnm rpc`、`ccnm internal agent-stop` | 冒充 Agent 的 ssh（文件屏障控制每次运行何时结束）、Agent 上的 tmux |
| `p57-output.py` | OUT-01/02 | `ccnm internal agent-result`、`ccnm rpc` | 冒充 Agent 的 ssh |
| `p57-write-guard.py` | AUTH-01/02 的 Runtime 一半 | `ccnm internal mcp-serve` | 无：客户端是不 import ccnm 的 [mcp_client.py](../../tests/mcp_client.py) |
| `p57-purge-routing.py` | CLEAN-01 | `ccnm workspace remove --purge` | 冒充 Agent 的 ssh |
| `p57_common.py` | 沙盒、假 ssh / 假 tmux 本体、RPC 客户端 | — | — |

时序靠文件屏障和轮询，不靠固定 sleep：假 Agent 的 `agent-run` 停在“等放行文件”上，探针决定先放行还是先 stop。唯一依赖调度的是压力项 A4，只记计数。每个探针退出 0 只表示探针跑完，不表示产品通过；它们**不进 CI**，P58–P61 修复时把对应项改写成正式回归。

沙盒放 `/tmp` 而不是 `$TMPDIR`：本机 `$TMPDIR` 48 个字符，拼上 `ccnm/ssh` 和 41 字节的 socket 名会超过 ControlPath 的 103 字节上限，ccnm 在拨号前就报配置错误，后面什么都测不到。写权探针沿用 `test_remote_workspace_mcp.py` 的临时目录（已 `resolve()`，避开 macOS `/var` 符号链接触发的凭据可达性 fail-closed）。

清理核对：`p57-rpc-control.py` 最初两次运行后留下 4 个假 ssh 进程（沙盒删了它们还在等放行文件，最长 120 秒自行退出），当场按命令行确认是本轮进程后结束，并改成“沙盒目录消失即退出”；改后第三次运行及其余探针跑完，`/tmp/p57-*` 为 0，`pgrep -f p57_common.py` 为 0，`time.sleep(297)` 残留命令为 0。

### 3.2 CTRL-01：RPC stop 停不到 print 运行

**A1**（`ccnm rpc` + 假 Agent）：同 workspace 先后启动 A、B，两者都停在 Agent 上运行；停 B、再停 A。

```json
"second_start_while_first_running": {"state": "starting", "busy_returned": false},
"agent_run_calls": 2,
"agent_stop_request_for_B": {"protocol": 3, "workspace": "demo", "agent": {"node": "worker", "instance": "claude-main"}},
"agent_stop_request_for_A": {"protocol": 3, "workspace": "demo", "agent": {"node": "worker", "instance": "claude-main"}},
"requests_identical": true
```

停 B 的请求和停 A 的逐字节相同，里面没有 B 的任何身份。RPC 仍回 B `stopping` / `stop_requested: true`。

**B**（真实 `ccnm internal agent-stop`，Agent 配置用 `tests/fixtures/agent-instance/agent.toml`，请求就是 A1 抓到的那一份）：

| 场景 | Agent 的回答 | tmux 被调用 | 结果 |
| --- | --- | --- | --- |
| B1 只有一个 print 会话在跑（supervisor pid 指向一个 `sleep`） | `killed: false`，退出 0 | 只有 `has-session -t ccnm-demo` | print 的 supervisor 仍在跑，会话目录没有 `stopping` 标记 |
| B2 同 workspace 另有一个交互会话 | `killed: true`，`session` 是交互会话的 id，退出 0 | `has-session` → `show-environment` → `kill-session -t ccnm-demo` → `has-session` | **交互会话被标 `stopping` 并被 kill**；print 的 supervisor 仍在跑 |
| B3 对照：同一请求加上 print 会话的精确 id | `CCNM_E_POLICY: recorded pid is not the owned ccnm supervisor process group`，退出 33 | 无 | 走 supervisor 校验路径，认出那不是 ccnm 的 supervisor 而拒绝 |

为什么：`SystemRuns::stop` 调 `stop_selected(..., None)`（[rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs)）；`work::stop` 只有拿到 session id 才走 `stop_print_session`，否则只按 `ccnm-<workspace>` 查 tmux（[work.rs](../../crates/ccnm-core/src/work.rs)）。print 运行由 Controller 起、不在 tmux 里。所以现状下一个 RPC print 运行被 stop 后会照常跑到结束或超时（A2 就是这个过程），调用方看到的却是 `stopping`。B3 说明精确停止所需的 Agent 路径已经存在，缺的是 RPC 在运行期间拿不到 ccnm 会话 id。

未覆盖：真实 Controller 起的 supervisor 进程组被信号的实际效果（B3 故意用不是 supervisor 的 `sleep` 验证拒绝分支）。

### 3.3 CTRL-02：stop 与完成的交错

**A2**：运行中 stop（Agent 立即回 `killed: true`），再放行运行结束。

```json
"status_after_stop":      {"state": "stopping",  "stop_requested": true},
"status_after_run_ended": {"state": "completed", "stop_requested": false},
"result_outcome": {"exit_code": 0, "stop_requested": false, ...}
```

运行线程持有启动时的记录副本，结束时整份写回，把 stop 写进去的 `true` 覆盖成 `false`。

**A3**：假 Agent 在回答 stop 之前先放行运行、等记录变成 `completed` 再回话——“完成”先落盘、“stop”后落盘。

```json
"status_right_after_stop": "stopping", "status_1s_later": "stopping",
"result_has_outcome": false, "record_has_finish": false,
"status_after_server_restart": "unknown"
```

Agent 已经跑完并交回结果，RPC 记录被 stop 读到的旧副本改回 `stopping`、`finish` 整个丢失；服务端活着时永远 `stopping`，退出后只剩 `unknown`。结果不可恢复。

**A4**（压力，依赖调度）：两个 `ccnm rpc` 进程，进程 1 的运行线程写终态，进程 2 同时处理 stop。三次各 40 轮：

| 运行 | `completed` / stop 标志丢失 / finish 在 | `stopping` / stop 标志在 / finish 丢失 | 两个事实都保住 |
| --- | --- | --- | --- |
| 第 1 次 | 39 | 1 | 0 |
| 第 2 次 | 36 | 4 | 0 |
| 第 3 次 | 37 | 3 | 0 |

120 轮里没有一次两个事实同时保住。没有观察到 stop 报错、残留 `.tmp` 或服务端“写不进记录”的日志，所以固定 `.tmp` 名的冲突**只能记为静态待验证**；读改写没有互斥这件事已由 A2/A3 确定性复现，不依赖这一项。

### 3.4 CTRL-03：句柄、start_key 与配置漂移

**A5**：协议允许 1..128 字符的任意 `start_key`，存储用 `safe_name`（只留 ASCII 字母数字和 `-_.`、截 64）。每组先用键 1 启动，再用键 2 分别以不同输入、相同输入启动：

| 键 1 | 键 2 | 键 2 + 不同输入 | 键 2 + 相同输入 |
| --- | --- | --- | --- |
| `任务-一` | `任务-二` | `-32010 conflict` | 复用了键 1 的 session |
| `a/b` | `ab` | `-32010 conflict` | 复用了键 1 的 session |
| 64 个 `k` + `x` | 64 个 `k` + `y` | `-32010 conflict` | 复用了键 1 的 session |

磁盘上三组各只有一个键文件：`-`、`ab`、64 个 `k`。九次 start 只有三次真正派给 Agent。按契约键 2 应该是新任务。推而广之：不含 ASCII 字母数字和 `-_.` 的键（比如纯中文）全部落成同一个文件 `key`。

**A6**：`session.result` 的 `session` 参数直接拼成 `sessions/<id>.json`：

| 句柄 | 合协议模式 `^[A-Za-z0-9][A-Za-z0-9._-]*$` | 读到 |
| --- | --- | --- |
| `../p57-outside` | 否 | `rpc/` 下植入的记录（`PLANTED-PARENT`） |
| `/tmp/p57-a6-…/p57-absolute` | 否 | 沙盒根下植入的记录（绝对路径替换了整条路径） |
| `s-link`（`sessions/` 里指向外面的 symlink） | 是 | 同上，symlink 被跟随 |

读到的是同一用户可写位置的文件，本轮没有证明跨用户越权；但服务端没有在读之前拒绝不合模式的句柄，这是 CT-06 要堵的口子。

**A7**：运行中把 `demo` 改绑到 `worker2`：

```json
"run_went_to": "worker-alias", "stop_went_to": "worker2-alias",
"stop_request_agent": {"node": "worker2", "instance": "claude-main"},
"record_still_says": {"instance": "claude-main", "node": "worker"},
"stop_after_workspace_removed": {"code": -32001, "data": {"ccnm_code": "CCNM_E_CONFIG", "effect": "none"}}
```

记录里仍写着 worker，stop 却按新配置发往 worker2；把 workspace 删掉后，旧句柄干脆停不了。

**A8**：

| 注入 | 结果 | 判定 |
| --- | --- | --- |
| 空键文件（`create_new` 后、写入前崩溃） | `-32011 uncertain`，`effect: unknown`，`data.session: ""` | 不重跑是对的；空串不合 `session_id` 的 `minLength: 1`，客户端拿不到可查的句柄 |
| 键指向不存在的记录 | `-32011 uncertain`，`data.session` 是那个 id | 符合契约“崩溃窗口”一节 |
| 半截 JSON 记录的 status / stop | `-32603`，`effect: none`，消息带私有绝对路径 | 记录；路径是否算泄露留给 CT-06 判断 |

两次启动都没有派到 Agent。

### 3.5 OUT-01 / OUT-02：结果从哪一步丢

**C0**（真实 `ccnm internal agent-result`，手工造的 print 会话目录，stdout 3 145 745 字节、头中尾三个标记）：

```json
"stdout_tail_bytes": 2051, "stdout_tail_starts_with": "...vwxyz\nlin",
"stdout_tail_markers": {"P57-EARLY-MARKER": false, "P57-MIDDLE-MARKER": false, "P57-LATE-MARKER": true},
"stderr_tail_bytes": 2051,
"parsed_result_final_text_bytes": 20503, "parsed_result_final_text_markers": {"...": true}
```

`agent-run` 与 `agent-result` 用同一个 `tail()`（[work.rs](../../crates/ccnm-core/src/work.rs)，保留 2048 字节并加 `...`）。所以 RunReport 带回 Operator 的就只有这 2 051 字节；`agent-run` 解析成功时连这段都不带（`stdout_tail` 置空）。

**C1**：把 C0 真实得到的那段尾巴原样交给 `ccnm rpc`：`bytes_total: 2051, truncated: false, cursor: null`。原始输出 3 MiB，调用方看到的是“没有截断”。

**C2**：尾巴本身 20 503 字节时 RPC 再截一次：`bytes_total: 20503, truncated: true, cursor: null`，给出末尾 8 192 字节、是输入的后缀、没有切坏中文。`truncated` 为真但没有下一页。

**C3**：解析成功的结果文档、最终回答 20 503 字节：`text` 完整（头中尾都在），`output` 是 `bytes_total: 0` 的空页。最终回答的大小没有上限。

**C4**：`max_bytes` 为 16、0、-5、`"abc"` 时都回 8 192 字节、都不报错；契约是“可以更少、不会更多”，schema 要求 ≥ 1 的整数。非空 cursor 回 `-32012` / `cursor_expired`，与现有测试一致。

**C5**：RunReport 的 `stderr_tail` 有标记时，RPC 结果和磁盘记录里都没有它。

### 3.6 AUTH-01 / AUTH-02：写权在哪里判

| 探针 | 场景 | 结果 |
| --- | --- | --- |
| D1 | 同 workspace，第一个 coding 会话开着时第二个 | 退出 33：`CCNM_E_POLICY: workspace write guard is busy; another session still owns this working tree`；marker `held p57-d1-first demo pid …`；第一个关掉后下一个进得来 |
| D2 | 同一仓库的两个 worktree 分属两个 workspace | 第二个同样被拒（common-dir 共用一把锁） |
| D3 | 同一 root，两个 `XDG_STATE_HOME` | 两个都进得来——P43 已记入运维手册的部署边界 |
| D4 | coding 会话起后台命令后 mcp-serve 被 SIGKILL | 命令仍在；marker 停在 `held`；下一个 writer：`workspace write guard was left held by an interrupted process; old children may still exist, so authority is not transferred automatically` |

对照：`cargo test -p ccnm-cli --test write_guard --locked --offline` 3 passed（`abrupt_server_exit_never_transfers_authority_by_timeout`、`live_owner_is_busy_and_clean_shutdown_allows_reentry`、`residual_exec_child_keeps_the_workspace_unknown_until_manual_recovery`）。

RPC 那一半：A1 中第二个 `session.start` 回 `starting`，`-32008 busy` 在 `rpc/` 里只出现在 [wire.rs](../../crates/ccnm-core/src/rpc/wire.rs) 的常量和测试中，没有任何路径返回它。所以 AUTH-01 的描述成立：start 从不 busy，被占时的实际表现是会话接受后在 Runtime 握手处失败。

### 3.7 CLEAN-01：清理删的是谁的目录

**E**（真实 `ccnm workspace remove demo --purge`，Operator 与“Executor”用两个 `XDG_STATE_HOME`，同一会话 id 在两边都有保留输出）：

```json
"agent_calls": [["agent-stop", {"protocol": 3, "workspace": "demo", "agent": {"node": "worker", "instance": "claude-main"}}],
                ["agent-purge", {"protocol": 1, "workspace": "demo"}]],
"operator_state_session_removed": true,
"executor_state_session_still_there": true,
"operator_rpc_record_still_there": true,
"operator_rpc_start_key_still_there": true,
"workspace_left_in_config": false
```

另外两点：purge 请求走旧协议 1、不带实例身份，Agent 侧无法按实例区分；Executor 的输出还在、配置里的 workspace 却已删，之后没有命令能再指到它。**这是路径路由测试，两个目录同属当前用户，不是不同 UID 的真机授权证明**；跨 UID 结论留给 P62。

## 4. P57.3 普通项目样例

[tests/fixtures/sample-projects/](../../tests/fixtures/sample-projects/README.md) 下两个零依赖项目，用法和前提写在那份 README 里。离线闭环（[p57-sample-projects.py](probes/p57-sample-projects.py)，复制到临时目录、`git init` 后跑）：

| 步骤 | rust-mini | ts-mini |
| --- | --- | --- |
| 基线测试 | 0 | 0 |
| 应用 `01-hours-test.patch` 后 | 101，失败在 hours 那条 | 1，失败在 hours 那条 |
| 应用 `02-hours-fix.patch` 后 | 0 | 0 |
| `git reset --hard && git clean -fdx` 后 | 0，`git status` 为空 | 0，`git status` 为空 |
| 大输出 `bigout 3` | 3 145 745 字节，三个标记各 1 次，合法 UTF-8，stderr 标记只在 stderr | 同左，sha256 `3e0253f1…` 两边一致 |

补丁是在临时 git 仓库里真实改动后 `git diff` 出来的，不是手写。大输出与 `p57-output.py` 的生成规则相同，P59 可以直接拿它做 OUT-01 的真实输入。

## 5. P57.4 P62 进场清单

规则、授权分项与证据字段以 [core-verification.md](../plan/core-verification.md) 第 2.2、2.6 节为准；这里只列本机能看到、P62 开始时必须逐项现场确认的具名对象。**下表“本机当前”一栏是 2026-09-28 的只读观察，不是 P62 的基线。**

| 对象 | 本机当前（只读） | P62 开始前要确认 |
| --- | --- | --- |
| Agent | 本机 macOS 26.6.2 arm64；`~/.local/bin/ccnm` 为 0.9.0（2026-09-23 装，sha256 前缀 `300dbd1d`），与本轮源码构建不同 | 候选构建的 commit 与 hash；是否替换已装二进制（需单独授权，新文件 + rename） |
| Runtime | `~/.ssh/config` 的 `hpsrv` 指向 `100.116.207.8`、`User root` | 主机身份与 tailnet 现名；ccnm 执行身份是 `ccrun` 而不是 root；root 只用于获批的文件传输，不扩大清理权限 |
| Claude Code | PATH 上 2.1.281 | 受管会话实际用的二进制与版本；登录方式只核对状态，不读认证文件 |
| Codex | PATH 上 **0.156.1**，受管 adapter 只认精确 **0.154.0** | 实例的 `codex_bin` 是否指向 0.154.0；否则 REAL-02/04 会在启动前报 `CCNM_E_VERSION`——按 2.1 节走兼容性评估，不临时放宽 pin |
| 项目 | 无获批的真实项目 | 优先用户批准的真实项目；否则把 `rust-mini` 或 `ts-mini` 复制成 hpsrv 上的测试 workspace，并注明不是生产交付证据；Runtime 上 Node ≥ 22.18 要现场查 |
| 预算 | 未批准 | Provider / 实例 / 回合上限；未批准前不调用模型 |

每个组合（REAL-01…05）的记录按 2.6 节模板填写，另加两行本轮新增的必查项：

```text
P57 反证复验：A1/B（stop 是否带精确会话、print 运行是否真被停）、A2/A3（stop 标志与终态）、
             C1（bytes_total/truncated 是否如实）、E（Executor 身份下的输出是否被清）——
             在 P58–P61 修复后的候选构建上各跑一次，写实际结果，不沿用本记录
跨 UID：Operator 与 ccrun 的 state 目录实际属主、清理是否由对应身份执行
```

## 6. P57.5 对后续阶段的交接

**P58 要先红后绿的反证**：A1/B（CT-01/02：无 session 的 stop；B2 那种停错会话的形态必须在回归里出现）、A2/A3（CT-03：标志回退、终态被覆盖）、A5（CT-04）、A8 空键（CT-05）、A6（CT-06）、A7（CT-07）。A4 的计数只能说明窗口存在，回归要用屏障做确定性版本。

**P58 必须保住的现有行为**（本轮看到它们成立）：终态上的 stop 幂等且不改状态；owner 进程不在时非终态读成 `unknown`；键在服务端重启后仍复用；指向缺失记录的键回 `uncertain` 而不重跑；非空 cursor 回 `-32012`；Agent 侧带精确 id 的 stop 会拒绝不是 ccnm supervisor 的 pid（B3）。

**P59**：源头截断在 Agent（2 KiB），RPC 的 8 KiB 对真实运行从不触发；设计分页时 `bytes_total` 不能再取已截断尾巴的长度。stderr 目前在 RPC 层完全丢失。最终回答 `text` 没有大小上限，也属于 P59 的预算范围。

**P60**：AUTH 行为与计划一致，无需修订。

**P61**：purge 请求不带实例身份、走协议 1；配置先于 Executor 输出被删。这两点并入 CL-05/CL-07 的验收。

## 7. 未覆盖

- 真实 Agent、真实 Controller、真实 SSH、真实模型：一概没跑。假 Agent 只模仿 RunReport / StopReport / PurgeReport 的形状，回答内容由探针指定。
- Linux：本轮所有结果只在 macOS arm64 上；hpsrv 回归留给 P62。
- 跨 UID 权限：E 与 D3 都是同一用户下的两个目录。
- 固定 `.tmp` 名的写冲突：未撞出，静态待验证。
