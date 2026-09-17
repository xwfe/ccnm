# P31 Runtime 保留输出：实现、实测与限制（2026-09-17）

## 结论

Runtime 上 `sessions/<id>/output/` 不再无限增长。实现在 `crates/ccnm-core/src/mcp/retention.rs`（d5353cb），规则写在[协议第 8 节](../protocol/remote-workspace-mcp-v1.md#8-输出预算与保留)和[运维手册](../operations.md#状态文件在哪多大怎么清)，这里只记为什么这么做、实测了什么、没做什么。

| | 之前 | 之后 |
| --- | --- | --- |
| 一个 session 最多留 | 100 × 2 × 64 MiB = 12.5 GiB | 已结束运行合计 256 MiB（加上并发中的运行，每个最多 128 MiB） |
| 外部 MCP 会话结束 | 不删，`--purge` 也删不到 | 删掉本进程建的运行 |
| 没人用的 session | 永远留着 | 最后一次运行过去 7 天、没有 `mcp-serve` 在服务它，就删 |

两个值（256 MiB、7 天）是用户 2026-09-17 定的。

## 设计里会被问到的四件事

**1. 怎么知道一个运行还在跑：`running` 文件加 flock，只用锁不行。** 同一个 session 可以同时有两个 server（Managed 会话 `/mcp Reconnect` 后新旧两个 `mcp-serve`），所以不能靠进程内存里的列表；进程死了锁自动释放，所以不用判断 pid 死活。但只用锁的第一版，结束清理那条测试在多线程下偶发失败：

| 测试线程数 | 只用锁：40 次里失败 | 锁 + `running` 文件：40 次里失败 |
| --- | --- | --- |
| 1 | 0 | 0 |
| 2 | 6 | — |
| 8 | 15 | 0 |
| 64 | — | 0 |

失败时加诊断：刚结束的运行，当下 `try_lock` 拿不到，300 毫秒后再试就拿到了。原因是同进程别的线程在 fork 子进程——子进程在 exec 之前持有父进程所有 fd 的副本，锁跟着延长。生产里也会这样（并发跑命令时每条命令都要 fork），后果是结束清理漏删、淘汰晚一轮。现在运行结束时先删 `running` 文件再放锁，判断时文件不在就算结束，锁副本不再起作用。`a_lock_copy_left_behind_does_not_keep_a_finished_run` 用 `try_clone` 造一个同样的副本钉住它；把"先删文件"这一步去掉，这条测试就失败。

**2. Managed 会话为什么断开不删。** `/mcp Reconnect` 用同一个 session id 起新的 `mcp-serve`，模型手里的旧 `output_ref` 还要能读（`a_managed_session_keeps_its_output_across_a_reconnect`）。外部会话一个 bridge 进程就是一个 session、断线不重连（协议第 6 节），所以可以结束即删。判断按 payload 的协议号（外部入口是 5），不按 `bridge-` 这个名字。

**3. 结束清理为什么只删本进程建的运行。** session id 来自对端，Runtime 只校验它是合法标识符。一个只读外部会话如果报了别人的 id，断开时删整个目录就等于让只读客户端删掉别人的输出。

**4. 过期清理为什么在 `mcp-serve` 启动时、后台线程里做。** Runtime 上没有常驻进程，`mcp-serve` 启动是唯一稳定的时机；放后台线程是为了不拖慢握手，扫描出错也不影响这次会话。要问 `ps` 哪些 session 有人在服务，而现有的 `overview::scan_servers` 在 `ps` 失败时返回空列表——拿它判断会把"查不到"当成"没人在用"，所以加了 `try_scan_servers`，失败时一个都不删。

## 反向验证

每条都是临时把实现改坏，确认对应测试失败，再改回去（改回后逐字节比对过源文件）。

| 改坏了什么 | 失败的测试 |
| --- | --- |
| 淘汰不跳过进行中的运行 | `too_many_bytes_…_never_the_current_one`、`a_run_still_held_is_kept_until_its_holder_is_gone` |
| 总字节不算刚结束的这次 | 同上两条 |
| 过期清理不看有没有进程在服务 | `output_expires_only_when_nobody_serves_it_and_nothing_is_new` |
| 过期清理不看时间 | 同上 |
| 结束清理删整个目录 | `discarding_removes_only_the_runs_this_process_started` |
| 单流不截断 | `a_stream_past_its_limit_is_cut_and_the_command_still_finishes` |
| 运行结束不删 `running` 文件 | `a_lock_copy_left_behind_does_not_keep_a_finished_run` |
| 真实二进制：外部会话结束不清理 | `an_external_session_leaves_no_output_behind` |
| 真实二进制：Managed 会话结束也清理 | `a_managed_session_keeps_its_output_across_a_reconnect`、`an_output_ref_does_not_cross_sessions` |
| 真实二进制：启动时不做过期清理 | `a_starting_server_removes_output_nobody_has_touched_for_a_week` |

`an_output_ref_does_not_cross_sessions` 改了写法：第一个会话从外部会话换成 Managed 会话。外部会话的输出现在结束就删，原写法里第二个会话读不到的原因变成"已经不在"，证明不了隔离；现在读之前和读之后都断言那个运行目录还在。

## 门禁（macOS，本机）

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `cargo test --workspace`：793 passed / 0 failed。
- `cargo test -p ccnm-cli --test external_mcp`：28 passed。
- `python3 -m unittest tests.test_remote_workspace_mcp`（`CCNM_BIN` 指向本轮构建）：10 passed，无跳过。
- `python3 scripts/check_protocol.py`（38 + 21 个 fixture）与 `tests.test_check_protocol`（24 passed）、`python3 scripts/check_plan.py` 与 `test_check_plan.py`（14 passed）、`git diff --check`：通过。
- `cargo +1.89 check --workspace --all-targets --locked`：通过，无警告（`File::try_lock` 是 1.89 才稳定的）。

## 没做的和限制

- **`--purge` 没改（原 P31.4），用户开工后决定。** 推荐部署里 Operator 和 Runtime Executor 是两个账号，`--purge` 删的是敲命令那个账号自己的状态目录，删不到执行账号那边的输出；真要删到得改 `agent-purge` 的内部 wire。运维手册已写明，observed_gaps 记了一条。
- **没跑真机，没换任何机器上的二进制。** Linux 上只靠 CI；本轮在 macOS 上验证。
- 一个 Managed 会话如果 MCP 连接断开超过 7 天（没有 `mcp-serve` 在服务它）再 Reconnect，之前的 `output_ref` 读不到了。
- 过期清理只在执行账号起 `mcp-serve` 时发生；一台 7 天里一次会话都没起的 Runtime，旧输出会一直留到下一次。
- 淘汰检查和运行结束恰好交错时（检查时 `running` 文件还在、同一瞬间锁被 fork 副本占着），这一轮会跳过它，下一次运行再删。只会晚删，不会删错。
- 顺带发现两个与本阶段无关、原来就有的并发失败，没修：`mcp::path` 有两条测试共用 fixture 名 `inside`，并发时互删目录；`mcp::patch` 的 `whether_a_journal_is_abandoned_does_not_depend_on_the_clock` 在 fork 压力下 `try_lock().unwrap()` 失败，是上面第 1 条同一个机制。直接跑测试二进制 `mcp:: --test-threads=64` 时 15 次里 15 次至少挂其中一条。
