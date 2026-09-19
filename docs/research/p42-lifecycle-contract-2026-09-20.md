# P42 后台命令的生命周期契约（2026-09-20）

环境：macOS 26.6.2 arm64 开发机，rustc 1.98.0。零模型额度，没上真机，没跑真实 Host。

本轮只定语义、补证据，**没有改任何运行行为**。跨仓来源是主评审 `toexec/docs/plan/2026-09-19-cross-project-refactor-review.md` 的 X04，和本仓[落地清单](2026-09-19-cross-project-refactor-actions.md)的 C-A。

## 1. 结论

- ccnm 这边有四个时钟，此前散在协议文档四处；调用方那边还有第五个（它自己的单次调用预算），它不是 Runtime 的运行时限，但**在 gld 上它比谁都先要命**。
- 后台命令的命脉是**连接**。连接怎么断的不重要——Host 关掉、SSH 断、hub 因为调用超时主动丢掉——ccnm 一律停掉这条连接起的所有命令再放写锁。
- 三条此前没有测试盯着的语义，实测都与契约一致，已补成回归测试：取消一次等待不等于取消命令；重复 `stop_command` 照实报状态不算错；旧 `output_ref` 活不过连接。
- gld 那两条组合风险**本轮仍未运行复现**，只有源码依据。修和验都在 gld。

## 2. 四个时钟（ccnm 侧，源码事实）

| 时钟 | 谁定 | 现在是多少 | 源码 | 到点会怎样 |
| --- | --- | --- | --- | --- |
| 单次等待 | 调用方给 `read_output.wait_ms` | ≤ 600000 ms | `mcp::output::MAX_WAIT_MS`，用在 `mcp/server.rs` 的 `read_output` | 这一次读返回，**命令继续跑** |
| 命令运行期限 | 调用方给 `exec_command.timeout_ms` | 前台默认 120000、上限 600000；后台不给就没有期限 | `mcp::exec::DEFAULT_TIMEOUT_MS` / `MAX_TIMEOUT_MS`，`exec.rs` 里 `(None, false) => Some(DEFAULT_TIMEOUT_MS)` | 杀掉整个进程组（TERM，2 秒后 KILL，10 秒放弃：`jobs::STOP_GRACE` / `STOP_GIVE_UP`） |
| 会话 | 连接本身，**没有别的东西** | 没有租约、没有续租、没有空闲回收 | `mcp/server.rs` 的收尾路径；空闲时每 30 秒一次 `ping`（`HEARTBEAT`）只用来发现半开连接 | 连接一断，这条连接起的命令全停并等管道读完，然后才放写锁 |
| 输出保留 | Runtime | 外部入口连接结束即删；跨入口是最后一次运行过去 7 天 | `mcp::retention` | 旧 ref 报 `CCNM_E_INVALID_ARGS` |

同时在跑的后台命令最多 8 个（`jobs::MAX_BACKGROUND`）。

## 3. 第五个时钟：调用方自己的调用预算

**这一节是读 gld 源码得出的（`35026f5`），不是运行证据。**

| 事实 | 位置 | 值 |
| --- | --- | --- |
| 单次远端调用预算 | `crates/core/src/bridge/session.rs` `CALL_TIMEOUT` | 60 秒 |
| 超时后怎么办 | 同文件 `run_locked`：超时属于"传输层已经不可信"那一支，`guard.take()` 丢掉连接 | 连接没了 |
| 只读槽空闲回收 | `IDLE_AFTER` | 5 分钟 |
| coding 槽空闲回收 | `CODING_IDLE_AFTER` | 2 分钟 |
| 空闲判据 | `Slot::is_idle`：没有别的持有者、锁拿得到、`last_used.elapsed() >= after` | `last_used` 是**上一次调用返回**的时刻 |
| 回收时机 | `Connections` 找槽位时顺带关掉别的空闲槽 | 不是定时器，是下一个请求触发 |
| gld 自己怎么说 | `crates/core/src/bridge/tools.rs` 里 `remote_exec_command` 的说明 | "this hub cuts a call off after 60 seconds and that ends the coding session" |

两条推导出来的组合风险，**都还没人跑过**：

1. **空闲回收杀掉后台任务**（评审 X04 原本指的那条）：在跑的后台命令不更新 `last_used`，所以模型起了后台任务、两分钟不碰这个 workspace，下一个别的请求进来找槽位时就可能把这条连接关掉，ccnm 随即停掉那个任务。
2. **一次跑长的前台命令连带杀掉后台任务**（比第一条更快）：前台 `exec_command` 的默认期限是 120 秒，而 hub 的调用预算是 60 秒。超过 60 秒 hub 丢连接，同一会话里所有后台任务跟着没。gld 的工具说明已经写了这个后果，但没有测试盯着它。

复现需要可控时钟 + 两边真实二进制，gld 的 `Connections::with_budgets(idle_after, call_timeout)` 就是为此准备的。**修和验都在 gld**：ccnm 这边"连接结束即停"是有意的权威语义，不是缺陷。

## 4. P42.1 实测：三条没人盯着的语义

真实 `ccnm internal mcp-serve` 二进制 + 中立 MCP 客户端（`tests/test_remote_workspace_mcp.py`，不 import 任何 ccnm 代码，自己拼 JSON-RPC）。三条全部与契约一致，已留成回归测试。

| 测的是什么 | 怎么测 | 结果 |
| --- | --- | --- |
| 取消一次等待 ≠ 取消命令 | 后台跑 `sleep 3`，`read_output` 带 `wait_ms: 30000` 等它，0.2 秒后对这个 requestId 发 `notifications/cancelled` | 命令还在（0.7 秒时 `kill(pid, 0)` 成功），后来自己 `exited 0`，输出完整 |
| 重复 `stop_command` | 停一个 `exec sleep 30`，进程确认没了之后再停一次 | 第二次不是错误，照样报 `stopped by stop_command after …` |
| 旧 `output_ref` 活不过连接 | 后台命令跑着时 `close()`，用**同一个 session 名**重连，拿旧 ref 去 `read_output` 和 `stop_command` | 进程 2 秒内没了；两个工具都报 `CCNM_E_INVALID_ARGS` 并带上那个 ref |

第一条是三条里最值得写死的：取消 `exec_command` 的调用会停掉命令，取消 `read_output` 的等待不会，**两件事只差一个工具名**。hub 和模型都会按"超时了就取消"去做，语义不写清楚就会各按各的理解实现。

## 5. 这一轮没有证明的事

- gld + ccnm 的真实组合：上面第 3 节两条风险都只有源码依据，没有运行证据。
- 真实 Host（Claude Code / Codex）在这些边界上的行为：没有额度，没跑。
- 网络真断（不是本机 `close()`）时的表现：本轮用的是本机 stdio，`ping` 写失败那条路径没有真机证据，沿用 P11/P12 的旧结论。
- 服务进程被 `SIGKILL` 之后的残留：P41 已记为已知缺口（状态报 `no longer running, and its exit status is unknown`，进程组没人收），本轮没动它，属于 C-B。
