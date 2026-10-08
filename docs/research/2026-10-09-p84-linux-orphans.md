# P84 Linux 上收掉逃出进程组的后代（2026-10-09）

**结论**：Linux 上，命令里 `setsid` 出去的后代现在会在会话结束时被收掉，收掉才交写锁。P43 记下的两种情况在 Debian 13 上都改了，且都先在旧代码上红：攥着管道的那种不再让写锁一直 `held`；不攥管道的那种不再在写锁 `released` 之后接着跑。macOS 不变。行为说明在[协议](../protocol/remote-workspace-mcp-v1.md)开头 2026-10-09 那条，本文只记证据和覆盖范围。

| 验收 | 做了什么 | 证据 |
| --- | --- | --- |
| P84.1 subreaper | `mcp-serve` 启动时 `prctl(PR_SET_CHILD_SUBREAPER)`（`nix` 0.31，依赖树里本来就有，只在 Linux 上成为 ccnm-core 的直接依赖，`unsafe_code = "forbid"` 不动）；失败只记一行 warn | `crates/ccnm-core/src/mcp/orphans.rs` |
| P84.2 收尾时清扫 | 收尾一开始起后台线程，每 100 ms 找"父进程是自己、换了会话号"的子进程，连同它的进程组 TERM，2 秒后 KILL；`stop_all` 和中继关闭之后，再把还活着的**全部**子进程同样 TERM、2 秒后 KILL 收一遍，KILL 之后 5 秒还在的点名、不交写锁 | 第 1 节 |
| P84.3 测试 | 3 个单元测试；Linux 上两条集成用例，旧代码红、新代码绿 | 第 2 节 |
| P84.4 文档与 CI | 支持矩阵、运维手册、排错手册、README 已知限制等按平台改写；CI 的 msrv 注释写明只在 Linux 跑是有意的 | 本次提交 |

## 1. 实施中查出的问题：中继 server 被当成收养来的

立项时的判定是"父进程是自己，且换了会话号**或**不领自己进程组、又不在 ccnm 的进程组里"。第一版按这个写，在 hpsrv 上跑中立测试 `tests/test_remote_workspace_mcp.py` 的 `test_a_child_left_in_the_servers_process_group_ends_before_the_next_writer`（P52 那条）**3 次全失败**，v0.13.1 的二进制同一台机器 3 次全过。

原因：P52 起中继 server 加入一个 `cat` 锚点进程领的组（`relay::start`）。它是 ccnm 自己起的子进程，但不领自己的组、也不在 ccnm 的组里，正好落进第二个条件。收尾一开始清扫线程就把它连同子进程杀了，那条用例要等的 tick 文件一次都没写出来。

改法：只认会话号不同。ccnm 自己从不建新会话，所以换了会话号的子进程只能是 `setsid` 出去后被收养的。只换了进程组、没换会话的后代（比如 `set -m` 之后的后台任务）不归清扫线程管，由收尾最后那一遍"全部子进程"收掉。

## 2. 实测（hpsrv，Debian 13 x86_64，`ccrun` 身份，Rust 1.98.1）

| 跑什么 | 结果 |
| --- | --- |
| 旧代码 + 新测试：`a_command_that_cannot_be_stopped_keeps_the_guard` | 红：marker 是 `held bridge-abandoned demo pid …\nabandoned 1 command(s) (r-…)`，期望 `released` |
| 旧代码 + 新测试：`a_descendant_that_left_quietly_is_ended_with_the_session` | 红：`pid … outlived its session` |
| 新代码，上面两条 | 2 passed，连跑 3 次，每次 0.16–0.17 秒 |
| `cargo test --workspace --no-fail-fast` | 20 个二进制 1072 passed / 0 failed |
| 中立测试 `tests.test_remote_workspace_mcp tests.test_agent_skills` | 54 ran，OK（35.7 秒），P52 那条在内 |
| `cargo clippy --workspace --all-targets -D warnings` | 干净 |

新用例第一版用 `!gone(pid)` 确认"进程还在跑"。`gone()` 对活着的进程要轮询满 10 秒才返回，所以那条用例每次白等 10 秒。现在改成只查一次的 `alive()`。

## 3. 覆盖到哪、没覆盖什么

- **覆盖**：命令的后代 `setsid` 出去，不管攥不攥管道，也不管它的父进程是在会话中途退出，还是在 `stop_all` 时被杀。中继 server 的这类后代走同一条路（它们也是 `mcp-serve` 的后代），但没单独写用例。
- **按代码推断、没实测**：只换进程组、不换会话、又攥着管道的后代。`stop_all` 仍会等满 10 秒、记 `abandoned`，写锁照 P43 留着，最后那一遍再把它收掉。结果是保守的：不会漏交写锁，只是锁多留了，要人按运维手册确认后再清。
- **不覆盖**：`mcp-serve` 被 `SIGKILL`（收养它们的就是它，P41 那条缺口）；不是从命令派生的进程，比如 `systemd-run --user`、`docker run -d` 交给别的服务起的；macOS（没有 subreaper，`nix` 也没有列子进程的接口，要做得另选 libproc 或轮询 `ps`，等用户定）。
- **不收僵尸**：会话中途被收养、随后自己退出的进程会留成僵尸，直到 `mcp-serve` 退出由 init 收走。替它们 `wait` 会和 `std::process` 对自己子进程的等待抢，所以不做。
