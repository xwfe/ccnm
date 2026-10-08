# P80 受管 Codex 会话给 ccnm 的工具调用配足等待时间（2026-10-08）

**证据范围**：Codex 0.154.0 源码，加上真实 Codex 0.154.0 二进制对着本机假模型、假 MCP server 的零额度实测，加离线测试。**没有调用真实模型；没有让 Codex 连真实的 ccnm server 跑满 300 秒；没有在真机上复验；没有发版**——装着 v0.13.0 及之前构建的 Agent 仍会在 300 秒处断开。

## 1. 结论

| 验收 | 结果 | 依据 |
| --- | --- | --- |
| P80.1 复现 | 不配 `tool_timeout_sec` 时，Codex 在 **300.0 秒**告诉模型 `timed out awaiting tools/call after 300s`，server 一直收不到 `notifications/cancelled`，晚到的结果被丢掉；顶层工具与 Code Mode 一样。单元测试在旧代码上红：启动参数里没有这个键 | 第 3、4 节 |
| P80.2 实现 | 受管 Codex 会话（交互与 `--print`）在 `mcp_servers.ccnm.enabled_tools` 之后加 `-c mcp_servers.ccnm.tool_timeout_sec=1870`；同样的设置下 320 秒的调用正常返回给模型 | 第 2、3 节 |
| P80.3 外部入口文档 | 协议文档第 3.3 节加"Codex 当 Host：写上 `tool_timeout_sec`"（详细说明只在这一处），第 8 节上限表、使用说明、排错手册链接过去 | [协议文档](../protocol/remote-workspace-mcp-v1.md#codex-当-host写上-tool_timeout_sec) |
| P80.4 门禁 | 见第 5 节 | — |

## 2. 为什么是这个修法、这个数

**修法**：两种选择里，在工具描述或 instructions 里告诉模型"Codex 上最多 300 秒"不可取——同一个 ccnm 会因为客户端不同报两套上限，模型照样可能传 600000，撞上了仍是"以为失败、其实在跑"。受管会话的启动参数本来就由 ccnm 拼（approval、`tool_output_token_limit` 都在这里定），加一个键就让 Codex 的上限退到 ccnm 自己的上限后面去。外部入口（`ccnm mcp bridge`）的 Codex 配置是用户自己写的，ccnm 碰不到，只能写进文档。

**数**：Codex 超时不发取消，被它截断的调用照样在 Runtime 上跑完，所以它的上限不能比 ccnm 自己允许一次调用等的时间先到。一次调用 ccnm 最多等：

| 部分 | 上限 | 代码 |
| --- | --- | --- |
| skill 的 `PreToolUse` 钩子（P79，只在命令不问人的会话里跑） | 600 秒 | `mcp::hooks::MAX_TIMEOUT` |
| 前台命令 `timeout_ms` / `read_output` 的 `wait_ms` | 600 秒 | `mcp::exec::MAX_TIMEOUT_MS`、`mcp::output::MAX_WAIT_MS` |
| 停掉超时的命令（TERM，2 秒后 KILL，10 秒放弃） | 10 秒 | `mcp::jobs::STOP_GIVE_UP` |
| skill 的 `PostToolUse` 钩子 | 600 秒 | `mcp::hooks::MAX_TIMEOUT` |
| 链路余量 | 60 秒 | — |

合计 1870 秒，在 `provider::codex::TOOL_TIMEOUT` 里由这几个常量算出，哪个上限改了它跟着变。**还会超过它的情况**：钩子按顺序跑，一次调用上挂了不止一个慢钩子；`call_mcp_tool` 转发的 server 在执行账号的配置里写了比这还长的 `tool_timeout_sec`。两者都是用户配置决定的，ccnm 没法事先知道，文档写明按实际加。

**代价**：ccnm 的 server 真卡死时，Codex 要等到 1870 秒才放弃，比 300 秒晚。实际影响小：ccnm 每个工具都有自己的上限；到 Runtime 的 ssh 带 `ServerAliveInterval=15`、`ServerAliveCountMax=20`（`ssh.rs`），断网约 5 分钟后 ssh 退出，Codex 看到的是连接断开而不是等满；交互会话里人随时能按 Esc；`--print` 会话有自己的整体期限。

**放的位置**：在 MCP 接线之后（`enabled_tools` 后面），所以 `tests/fixtures/codex-0.154.0/seven-tools.json` 记录的、接线之前的那段参数不变，fixture 不用重录。exec-server 链（已封存）没有 MCP server，不受影响。

## 3. 零额度实测

夹具 [`probes/p80-codex-tool-timeout.py`](probes/p80-codex-tool-timeout.py)：模型接口是本机假服务，第一轮让模型调一次 `exec_command`（`{"cmd": "sleep 1000", "timeout_ms": 600000}`），之后每轮先等 `--linger` 秒再回一句话——不等的话 `codex exec` 一回合结束就退出、收掉 server，看不到之后的事。"ccnm" 是脚本自己扮的 stdio server，收到 `tools/call` 后另起线程拖 N 秒再回，主线程照常读 stdin，每条消息带时间记下。HOME / CODEX_HOME 是临时空目录，进程树套 `sandbox-exec` 只许连本机。启动参数照 `build_launch_cmd` 的 print 会话（`codex exec`、只读 sandbox、`approval_policy="never"`、关掉的 feature 列表、`agents.enabled=false`、ccnm 默认 approve）。

Codex 0.154.0（`codex-aarch64-apple-darwin.tar.gz` sha256 `344310a0…ebcef9d7`、`codex-code-mode-host-aarch64-apple-darwin.tar.gz` sha256 `500ee2a0…5dc86fad`，与 GitHub release 的 digest 一致），本机 macOS 26.6.2 arm64。Code Mode 那几轮在脚本第一行加了 `// @exec: {"yield_time_ms": 900000}`，让 `exec` 不在默认的 30 秒就把控制权交回模型——要量的是工具调用本身等多久。

| 轮次 | 工具面 | 追加 | server 拖 | 模型下一轮看到 | server 收到的取消 |
| --- | --- | --- | --- | --- | --- |
| 1 | 顶层（`--model gpt-5.1-codex`） | 无 | 320 秒 | 300.0056 秒，`tool call failed for ccnm/exec_command … timed out awaiting tools/call after 300s` | 无（看到 Codex 退出前的 15.9 秒） |
| 2 | Code Mode（不传 `--model`） | 无 | 320 秒 | `Script completed / Wall time 300.0 seconds`，结果是同一句错误、`isError: true`，脚本里量到 300007 ms | 无（同上） |
| 3 | 顶层 | `tool_timeout_sec=1870` | 320 秒 | 320.0057 秒，`held 320.0s`（server 的回复原样到达） | — |
| 4 | Code Mode | `tool_timeout_sec=1870` | 320 秒 | 320.0 秒，`held 320.0s`，320007 ms | — |
| 5 | 顶层 | `tool_timeout_sec=3` | 10 秒 | 3.0015 秒，`timed out awaiting tools/call after 3s` | 无；server 在第 10 秒把结果写回 stdin 成功，Codex 不报错、模型也没看到，直到 Codex 在第 16 秒退出 |
| 6 | Code Mode | `tool_timeout_sec=3` | 10 秒 | 3.0 秒，同一句错误 | 无；同上 |

第 1、2 轮的晚到结果（第 320 秒）在 Codex 退出（第 315.9 秒）之后，没看到；"结果被丢"由第 5、6 轮证明，走的是同一段代码，只差超时的值。`tool_timeout_sec=3`、`=1870` 都是 TOML 整数，Codex 照收（它按 f64 秒读，见下节）。

## 4. 源码怎么说（Codex `rust-v0.154.0`）

- `codex-rs/codex-mcp/src/rmcp_client.rs:102-103`：`DEFAULT_STARTUP_TIMEOUT` 30 秒、`DEFAULT_TOOL_TIMEOUT` 300 秒。官方文档 <https://developers.openai.com/codex/mcp> 写的是启动 10 秒、调用 60 秒（2026-10-08 取的页面），与源码和第 3 节的实测都对不上。
- `codex-rs/codex-mcp/src/connection_manager.rs:314-318`：server 的 `tool_timeout_sec` 没写就用 `DEFAULT_TOOL_TIMEOUT`；`:945-950` 与调用方给的超时取较小的。
- `codex-rs/rmcp-client/src/rmcp_client.rs`：`call_tool` 经 `run_service_operation("tools/call", timeout, …)` 到 `run_service_operation_once`，用 `active_time_timeout` 包住请求，超时就返回 `ClientOperationError::Timeout`、把请求的 future 丢掉。请求本身用 `PeerRequestOptions::no_options()` 发，没有 rmcp 层的超时。
- rmcp 3.2.0（Codex 锁定的版本）`src/service.rs`：只有 `RequestHandle::await_response` 在**自己**的超时分支里调 `send_timeout_cancel_notification`；Codex 没给它超时，走的是无超时分支；`RequestHandle` 没有 `Drop` 实现。所以 future 被丢掉时什么都不发——与实测一致。
- `codex-rs/config/src/mcp_types.rs:237-239`、`:567-591`：`tool_timeout_sec` 按 `Option<f64>` 秒读。

ccnm 这边：如果 Codex 发了取消，ccnm 的 server 会停掉那条命令（P41 起，`a_cancelled_call_stops_its_command`）；问题全在 Codex 不发。

## 5. 测试与门禁

| 用例 | 证明什么 |
| --- | --- |
| `codex_waits_out_the_longest_call_ccnm_lets_one_take`（`provider::codex::tests`） | print 与交互两种会话都恰好带 `mcp_servers.ccnm.tool_timeout_sec`，值大于工具上限加停命令再加两个钩子上限，位置在 MCP 接线之后。旧代码上红：`no tool timeout for ccnm`，打印出的启动参数里没有这个键 |
| `the_measured_fixture_records_the_launch_this_adapter_builds` 等原有 Codex 启动用例 | 不改、照过：接线之前的参数与 0.154.0 fixture 一致 |

本机 macOS 26.6.2 arm64、rustc 1.98.0，负载约 47（10 核）。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo +1.89 check --locked --workspace --all-targets` | 通过 |
| `cargo test --workspace --no-fail-fast` | 1116 通过、0 失败（清理后 1115，新增 1 条） |
| `python3 scripts/check_protocol.py`、`python3 -m unittest tests.test_check_protocol -q` | 通过：46 + 31 个 fixture；24 条 |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议、Python 244 条 0 跳过（用的是带修复的二进制，含中立 MCP 客户端那一套） |
| `git diff --check` | 通过 |

没跑 64 线程那一轮（没动 `process.rs`），没在 Linux 上跑。

## 6. 没覆盖的、记下不修的

- **真实模型、真实 ccnm 端到端**：没跑。Codex 连真实 `ccnm internal mcp-serve`、跑一条超过 300 秒的 `exec_command` 没做；Codex 一侧的行为由第 3 节的假 server 证明，ccnm 一侧"没收到取消就接着跑"是代码本来的样子。
- **交互会话**：实测只用了 `codex exec`。超时逻辑在 `codex-mcp` 里，两种会话共用；交互会话里按 Esc 中断时会不会发取消，没查。
- **`ccnm_agent`（P50，Agent 上的 MCP 转发）**：Codex 对它同样只等 300 秒。它的 `call_mcp_tool` 默认等 60 秒、起 server 30 秒，用户给 Agent 上某个 server 配的 `tool_timeout_sec` 接近或超过 300 秒时（server 还没起的话，起它的时间也算在这 300 秒里），调用会被 Codex 先截断，被截断的调用照样在 Agent 上跑。值取决于用户配置，不归 ccnm 的常量管，这次不修。
- **Claude Code**：不在本阶段。`mcp::output::MAX_WAIT_MS` 的注释记着它对 stdio server 的空闲超时是 30 分钟（2.1.273），长于 ccnm 的上限；没在本轮重测。
- **旧构建**：v0.13.0 及之前的 Agent 起的 Codex 会话仍在 300 秒处断开。临时办法写在[排错手册](../troubleshooting.md#codex-里报-timed-out-awaiting-toolscall-after-300s命令其实还在跑)。
