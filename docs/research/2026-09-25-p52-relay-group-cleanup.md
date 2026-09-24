# P52：MCP relay 的进程组收尾与写权交接（修 C51-01）

日期：2026-09-25。缺陷的复现和修复验收条件见 [P51 审计的 C51-01](2026-09-23-lifecycle-and-docs-audit.md)。起点：`main` / `ae06327`，工作区只有一份未跟踪的交接摘要（不属于本轮）。平台：macOS 26.6.2 / arm64，rustc 1.98.0。**Linux 回归本轮没有跑**，所以 P52 仍是进行中，C51-01 不关闭。

## 结论

C51-01 在代码上已修，macOS 上有正反两面的证据：

- 修复后：server 读到 EOF 正常退出，它留在同一进程组里的子进程在写锁交出前被杀掉；写锁 `released`，第二个 coding 会话进得来，旧子进程不再写文件。
- 修复前的代码跑同一个新回归：关闭后子进程仍在运行，测试失败。

清不掉（SIGKILL 后 5 秒组里还有能运行的进程）或查不了（进程列表拿不到）时，结果不再被当成功：Runtime 写锁标 `abandoned`，下一个 writer 被拒，拒绝信息写明是哪个 server、哪个进程组、哪些 pid。

## 改了什么（行为变更）

1. **起 server 前先起一个锚进程。** `/bin/cat`，stdin 接 ccnm 持有的管道，输出丢弃、环境清空、cwd 为 `/`，自己当进程组组长；server 加入这个组。为什么要锚、为什么这样就不会误杀 pid 被复用的进程，写在 [`relay::start`](../../crates/ccnm-core/src/mcp/relay.rs) 的注释里，这里不重复。
2. **每次关闭都清整组。** 不管 server 是读到 EOF 自己退、宽限期后还在、调用超时断开、被外部杀掉、闲置回收，还是会话结束：向整组发 SIGKILL，再用 `/bin/ps -A -o pid=,pgid=,stat=` 看组里除了锚和僵尸还有没有进程。没有才算清完；5 秒内清不完或 `ps` 跑不起来，就记成残留。最后才回收锚。
3. **残留参与写锁判定。** Runtime 会话结束时，relay 的残留和停不掉的命令一起决定写锁：有任何一项就 `abandoned`，都没有才 `released`。marker 第二行形如 `abandoned MCP server db (process group 9: 12 still running after SIGKILL)`，和命令残留并列时用 `; ` 分隔。
4. **relay 的关闭挪到 tokio runtime 收尾之后。** 原来在它之前关：一个还在跑的 `call_mcp_tool` 可能在那次关闭之后又起一个 server，活得比写锁久。
5. **Agent 侧同一套。** `ccnm internal agent-skills` 起的本机 stdio server 用同一个 `start`；Agent 上没有写锁，残留只记 warn 日志。
6. 写锁拒绝信息和[运维手册的写入 guard 残留](../operations.md#写入-guard-残留)补了 MCP server 残留的查法。

对使用者可见的变化：

- server 故意留一个同组后台进程、指望它比 server 活得久的，现在关闭时会被杀。这是本意：写锁交出去之后还能写工作树的东西不能留。
- 每个运行中的 server 多一个 `cat` 进程；每次关闭多跑一次 `kill` 和至少一次 `ps`，正常情况毫秒级。
- 依赖 `/bin/cat`、`/bin/kill`、`/bin/ps`。**没有 `ps` 的机器（比如没装 procps 的精简 Linux 镜像）每次关闭都会报"查不了"，写锁每次都 `abandoned`**——这是故意的保守，不是误报；这种机器要么装 procps，要么关 `[runtime_mcp]`。

## 验收对照

| 编号 | 证据 | 状态 |
| --- | --- | --- |
| P52.1 | 中立客户端回归 `tests/test_remote_workspace_mcp.py` 的 `test_a_child_left_in_the_servers_process_group_ends_before_the_next_writer`：同组、关管道、持续写的子进程；断言关闭后它不在运行、文件不再变、marker 为 `released`、第二个 writer 能写。旧代码（`git stash` 掉 `crates/` 后重编）上失败在"关闭后子进程仍在运行"，新代码上通过。P51 探针保留为复现记录，改成比较两者组号后在新构建上输出 `defect_reproduced=false` | macOS 完成 |
| P52.2 | `clear()` 只在进程列表显示组里没有能运行的进程时返回成功；信号只在锚未回收时发，组号不可能被别人占用。单测：杀不掉时按 pid 报告、`ps` 失败时报"查不了"而不是当空组、进程列表解析（排除锚和僵尸、格式不对就报错）；`left_behind` 把命令残留和 server 残留一起交给 `abandon` | macOS 完成；"真实二进制里组清不掉导致 abandoned"没有端到端测，原因见下 |
| P52.3 | 单测覆盖：正常 EOF（leader 先退）、宽限期后仍在、外部强杀 leader、闲置回收、调用超时、会话结束后下一个 writer（Python 回归）、脱组子进程（钉住边界）。Agent 侧单独一条 `a_local_servers_helper_goes_with_it`。反证：把 Stop 改回"server 自己退了就不管进程组"，EOF、强杀、闲置/超时 3 条失败 | macOS 完成 |
| P52.4 | macOS 门禁见下节。文档：架构、支持矩阵、使用、配置、排错、生命周期、编排交接、协议披露、运维同步 | **Linux 未跑**，C51-01 保持未关闭 |

"取消"没有单独的测试：MCP 请求被取消后，阻塞中的 relay 调用照常跑到结束或超时，server 留在连接池里，之后由超时、闲置回收或会话结束关闭——这三条都测了。

"组清不掉 → 写锁 `abandoned`"只做了分段证明（`clear` 的报告 + `left_behind` + 现有的 `abandon` 测试）：在真实二进制里造一个 SIGKILL 杀不掉的组员需要 setuid 程序或让进程卡在内核里，都要系统权限，本轮不做。

## 边界与未覆盖

- **离开进程组的后代**（`setsid`、守护进程式的两次 fork）：信号够不着、`ps` 按组也看不见，写锁照常交出。`a_child_that_left_the_group_is_beyond_reach_and_not_reported` 把这一点钉住。普通命令（P43）还能靠"它攥着管道"发现其中一部分；relay 这边拿不到管道状态（管道在 toexec-mcp 的 `ChildTransport` 里），要做到同等得改 toexec 或在 ccnm 里代理管道，本轮没做。
- **ccnm 自己被 SIGKILL**：锚读到 EOF 自己退出，server 读到 EOF 退出，同组残留没人收；写锁留 `held`（异常退出），按原有规则人工恢复。
- **工作区开了 OS 沙箱**（P33 的 `codex sandbox`）时，server 在沙箱里是否另起会话、进程组关系如何，没有验证。
- 没有跑真实模型、没有连 hpsrv/fodelf、没有部署或替换已安装的二进制。

## 本轮执行记录（macOS 26.6.2 / arm64）

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `cargo test --workspace --offline --locked`：946 passed，0 failed。`-- --test-threads=64`：全部通过。relay 与 Agent MCP 的 20 条测试在 64 线程下连跑 5 轮：全部通过。
- `cargo build` 后 `python3 -B -m unittest discover -s tests -q`：210 passed，无 skip。
- `python3 scripts/check_protocol.py`：38 + 29 个 fixture 通过；`python3 -m unittest tests.test_check_protocol -q`：24 passed。
- 文档与状态更新后：`python3 scripts/check_plan.py` 通过（P52 为 blocked，下一阶段仍是 P52）；`test_check_plan.py` 14 passed；`git diff --check` 通过。
- 清理：反证那一轮（旧行为）失败的测试留下 4 个 `sleep 60` 孤儿进程和 6 个测试目录；核对命令行和父进程后按 pid 杀掉、目录删除。之后检查没有残留的 `sleep 60`、`cat` 锚或 `setsid` 子进程。
