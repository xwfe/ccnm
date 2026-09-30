# P63 Machine API 断连与停止、ssh 失败分类（2026-09-30）

修的是 [P62 真机](2026-09-30-p62-real-machine.md)查出的三条：F14、F16、F17。范围见 [ROADMAP P63](../plan/ROADMAP.md)。每一条都先把真机现象改写成对旧代码失败的回归，再修。**只有离线证据**：本机 macOS arm64 上的真实二进制 + 假 Agent（`tests/fixtures/fake_agent_ssh.py`），没有连 hpsrv/fodelf、没有跑模型、没有部署或推送。`ccnm.machine/1` 的线格式和内部协议号都没变——三处都是让实现回到已冻结的契约上。

## 1. 结论

| 验收 | 结果 | 提交 |
| --- | --- | --- |
| P63.1 F16 客户端断开后任务照跑 | 每个会话由自己的 owner 进程带着跑；派发前、派发中断开都照常派发、跑完，重连查得到真实状态与结果；owner 真的死了仍是 `unknown` | `f866fc2`、`7e719d1` |
| P63.2 F17 停止标志不丢、停止确认 | 已派发的会话在问 Agent 之前就落 `stop_requested`；Agent 发 SIGTERM 后最多等 5 秒进程组退出；还确认不了时回 `stopping` | `ebbe258` |
| P63.3 F14 ssh 认证前失败记 failed | 解析、TCP 连接、认证、主机指纹、密钥交换失败记 `failed`，停止失败的 `effect` 记 `none`；超时和会话建立后被关仍是 `unknown`；两条黑盒用例不再被 macOS 的路径长度顶替 | `39c23d1` |
| P63.4 门禁与文档 | fmt、clippy、Rust 1022（64 线程）、`ci_gates.py`（计划、协议、Python 全套）通过；协议实现说明、排错、支持矩阵、README、状态同步 | 见第 4 节 |

阶段外的一处流程改动：计划检查原来严格串行，P62 受阻就只能把修复塞进 P62。`fd06253` 允许"前一阶段 blocked（写明原因和解锁动作）时认领下一阶段"，`current_task` 指向首个未完成且未受阻的阶段；只是没做完的阶段仍然挡住后面的。

## 2. 三条各改了什么

### 2.1 F14：ssh 在远端 shell 起来之前失败

**现象**（P62，hpsrv 上 Operator 是 Linux）：Agent 的别名解析不了，Machine API 把会话记成 `unknown`；Linux 上两条 Python 黑盒用例因此失败。

**原因**：`after_dispatch` 把派发后的所有 `AgentUnreachable` 当作"可能已经在跑"。而 macOS 上同样的用例能过，只是因为 `$TMPDIR` 太长，ControlPath 超过 103 字节，在 ssh 之前就报了配置错误——从来没测到 ssh 这一步。

**改法**：[`ssh::never_reached`](../../crates/ccnm-core/src/ssh.rs) 按 OpenSSH 的原话判断远端 shell 有没有起来：`ssh: Could not resolve hostname`、`ssh: connect to host … port …: …`、`Host key verification failed.`、`kex_exchange_identification:`，以及括号里是认证方式名的 `Permission denied (…)`——`Permission denied (os error 13)` 是远端程序自己的错，不算。命中就是 `failed`；超时、`Connection to … closed by remote host.`、`Broken pipe`、没有消息的 255 仍是 `unknown`。停止失败的 `effect` 用同一个判断。

**先红后绿**：`rpc::session::tests::an_ssh_that_never_reached_the_agent_is_failed_not_unknown` 在旧代码上 `left: Unknown, right: Failed`；两条黑盒用例改放 `/tmp` 并断言记录里的错误来自 ssh 的解析失败之后，旧代码上 macOS 也红（`'unknown' != 'failed'`），修后绿。

### 2.2 F16：客户端断开，任务不能跟着没

**现象**（P62）：`session.start` 返回后客户端立刻关 stdin，那次运行从未派发，状态却是 `unknown`；派发之后断开，运行在 Agent 上跑完了，Machine API 这边永远是 `unknown`。协议 8.1 节承诺的正相反。

**原因**：运行挂在 `ccnm rpc` 进程的一个线程上，`serve` 收到 EOF 就返回，进程退出，线程跟着消失。让 `ccnm rpc` 等运行结束再退出也不行：参考客户端 `close()` 只等 10 秒就杀进程。

**改法**：`session.start` 接受之后起一个独立进程组的 `ccnm --config <同一份> internal rpc-run --handle <句柄>`（`Runs::detach`，[session.rs](../../crates/ccnm-core/src/rpc/session.rs)），**先把它登记为记录的 owner 再回应**，所以拿到句柄就挂断的客户端回来时也能看到有人在跑。owner 从磁盘上的记录读回要跑什么（`RunAsk::from_record`，实例用接受时解析的那一个），走原来的 `run_to_end`，和停止之间仍由记录锁排序。进程内的测试执行器照旧用线程（`detach` 默认返回 `None`）。

**先红后绿**：新文件 [tests/test_rpc_disconnect.py](../../tests/test_rpc_disconnect.py) 4 条在旧代码上全红：`run never reached the Agent`、`'unknown' != 'running'`、挂断后 stop 回的 `stop_requested` 是 false、owner 就是连接本身（`57938 == 57938`）；修后全绿，用时 0.7 秒。

**顺带**：`crates/ccnm-cli/tests/rpc.rs` 的 `a_session_left_behind_by_a_killed_server_reads_as_unknown` 在 `f866fc2` 上会和真正的 owner 抢写记录（owner 比测试里那次连接活得久，稍后写回真实结局）。`7e719d1` 让它等 `finish` 落盘后再注入。所以 **`f866fc2` 单独检出时这条集成测试可能失败**；按规矩不改写历史，这里如实记下。

### 2.3 F17：停止请求不丢，停止能确认

**现象**（P62）：运行中 `session.stop` 回 `-32000 … Agent process group has not ended`，重复调用一样；运行其实已被 SIGTERM 结束（143），终态 `stop_requested: false`。

**原因**：Agent 在 `kill` 返回的那一刻就查进程组，真实进程总要一小会儿才退；RPC 只在 Agent 确认后才落停止标志，任何别的回答都把它丢了。

**改法**：
- Agent（[work.rs](../../crates/ccnm-core/src/work.rs) `group_ended_within`）：发 SIGTERM 后每 100 ms 查一次，最多 5 秒；Agent 的组和 supervisor 的组都这样等。等满还在照旧报 NotReady；只发 TERM，不升级成 SIGKILL。
- RPC（`session.stop`）：已派发的会话，在问 Agent 之前就在记录锁下落 `stop_requested`；Agent 回 NotReady（"还不能确认结束"）时按契约 5.6 回 `stopping`，调用方接着查状态、必要时再发 stop；其他错误照样返回错误，标志保留，状态不改成 `stopping`（没送到的停止不能让人以为在停）。

**先红后绿**：`work::tests::an_assigned_stop_waits_for_the_signalled_group_to_go`（信号后第一次查还在、第二次没了）旧代码报 `Agent process group has not ended`；Python `test_a_stop_the_agent_cannot_confirm_yet_is_kept_and_answered_stopping` 旧代码 `-32000`，`test_a_stop_that_cannot_reach_the_agent_keeps_the_request_on_record` 旧代码 `effect` 是 `unknown`；修后全绿。假 Agent 新增 `stop-mode` `not-ended`（信号送到、回 NOT_READY）。

`crates/ccnm-core/tests/session_identity.rs` 里"组里有成员一直不退"的两个分支原来只给一次 `ps` 观察；现在补足整段等待期的观察，行为和断言不变，这条测试因此多花约 10 秒。

## 3. 调用方看得到的变化

- `session.start` 之后可以马上断开，任务照跑；`ps` 里每个进行中的 Machine API 会话多一个 `ccnm internal rpc-run --handle s-…` 进程，运行结束即退出。
- ssh 认证前就失败的会话是 `failed`（此前 `unknown`），可以换一个新的 `start_key` 重来。
- 运行中 `session.stop` 正常情况下回 `stopping` 且 `stop_requested: true`，终态也带着这个标志；最多多等约 5 秒（Agent 在等进程组退出）。

## 4. 门禁

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace -- --test-threads=64` | 1022 通过（P62 时 1020，新增 2 条） |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议（43 + 29 个 fixture）、Python 全套 0 跳过 |
| `python3 -m unittest tests.test_check_plan tests.test_check_protocol` | 通过 |

## 5. 没覆盖的

- **Linux 没有在本轮复跑。** F14 正是在 Linux 上发现的；现在两条黑盒用例在 macOS 上也真正走到 ssh 并先红后绿，但 Debian 上的 `cargo test` 与 `ci_gates.py` 要么在授权后到 hpsrv 跑，要么推送后看 CI 的 ubuntu job。
- **真机没有复验。** F16、F17 的真实链路（真实 Agent、真实 Claude/Codex 退出时序）留给 P62 续跑。
- **F4 没修**：交互会话的 `ccnm stop` 仍是杀完 tmux 只看一眼 MCP 通道（Codex 上 3/3 先报 NotReady），`ccnm log` 仍把被停止的交互会话写成 `failed to start`。F1–F3、F5 及 F15 以后各项同样不在本阶段。
- 5 秒等待是按 P62 两台 Mac 上 Claude/Codex 的退出时间定的；更慢的 Agent 进程仍会得到 `stopping` 而不是确认，调用方要靠状态轮询。
