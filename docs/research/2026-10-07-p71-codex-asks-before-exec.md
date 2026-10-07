# P71 受管 Codex 交互会话执行命令前问人（2026-10-07）

接 [P62 续跑记录](2026-10-04-p62-resume-release.md)第 5.1 节与第 7 节的 F21：受管 Codex 交互会话里 `exec_command` 不经审批就执行。P69 只让 doctor 照实说"不问"，要不要补审批另行决定；用户 2026-10-07 定补。

**证据范围**：Codex 0.154.0（ccnm 钉住的版本）的零额度实测加离线测试。本机 macOS 26.6.2 arm64、rustc 1.98.0（clippy 另用 1.99.0）。**没有调用真实模型、没有在真机上复验**。`ccnm.machine/1` 不变；内部消息只加了两个可选字段。

## 1. 结论

| 验收 | 结果 | 提交 |
| --- | --- | --- |
| P71.1 实测 | 按工具设 `approval_mode="prompt"`：Code Mode 与顶层工具两种工具面都**每次调用前问**，只有"允许 / 取消"两项，取消的调用到不了 server；`codex exec`（`approval_policy="never"`）下同一设置**把调用拒掉**；会话里 `/permissions` 切到 Full Access 后**不再问** | 夹具 `docs/research/probes/p71-codex-approval.py`（本记录同一提交） |
| P71.2 实现 | 交互启动的预检以交互身份打开 workspace，读回 Runtime 标了"要人确认"的工具，记进会话记录 `ask_before`；Codex 启动时只给这些工具加 `approval_mode="prompt"`，print 会话、exec-server 链不变 | `40d40c4` |
| P71.3 doctor 与文档 | `Command approval` 对 Codex：OK、写明会话里切到 Full Access / Approve for me 就交回 Codex；设了 `allow_unattended_exec` 时与 Claude 同一句 WARN | `ba26866`；文档本记录同一提交 |
| P71.4 门禁 | 见第 5 节 | — |

## 2. 实测

**做法**（夹具开头有完整说明）：Codex 0.154.0 从 GitHub release 下载 `codex` 与 `codex-code-mode-host` 两个包，按 release 页给的 digest 核对 sha256（`344310a0…`、`500ee2a0…`），只放临时目录。模型接口是本机假服务：Code Mode 下第一轮回一段 JS（依次调 `read_file`、两次 `exec_command`，每次的结果或异常都 `text()` 出来），之后回一句话；`--top` 变体改成顶层工具的三次 `function_call`、启动带 `--model gpt-5.1-codex`。名叫 `ccnm` 的 MCP server 是夹具自己扮的，只有这两个工具，把收到的每次调用记下来。HOME / CODEX_HOME 是空临时目录，进程树套 `sandbox-exec` 只许连本机。启动参数照 ccnm 交互会话的 `build_launch_cmd`（只读 sandbox、`approval_policy="on-request"`、同一份关掉的 feature、`agents.enabled=false`、ccnm 这个 server 默认 `approve`），交互界面跑在单独的 tmux server 里，用 `send-keys` 回答。

| 情况 | 追加的设置 | 交互界面里 | server 收到 | 模型拿到 |
| --- | --- | --- | --- | --- |
| 基线（P71 之前的 ccnm） | 无 | 三次调用都直接执行，没有任何提示 | `read_file`、`exec_command` ×2 | 三个结果 |
| Code Mode | `mcp_servers.ccnm.tools.exec_command.approval_mode="prompt"` | `read_file` 直接执行；每次 `exec_command` 前停住：`Allow the ccnm MCP server to run tool "exec_command"?` / `cmd: echo P71-ONE` / `1. Allow  2. Cancel`。第一次选 Allow，**第二次照样问**，选 Cancel | `read_file`、第一次 `exec_command` | 第二次是 `{"content":[{"type":"text","text":"user cancelled MCP tool call"}],"isError":true}`，脚本照常走完 |
| 顶层工具（`--model gpt-5.1-codex`） | 同上 | 同样的提示；第一次按 Esc（等于取消），第二次 Allow | `read_file`、第二次 `exec_command` | 第一次 `user cancelled MCP tool call`，第二次结果 |
| `codex exec`（print） | 同上 | — | 只有 `read_file` | 两次都是 `MCP tool call requires approval, but approval policy is never` |
| 会话里切到 Full Access | 同上；启动后先 `/permissions` → Full Access → 确认警告，再发消息 | 三次调用都直接执行 | 三次都收到 | 三个结果 |

和源码对得上（`codex-rs/core/src/mcp_tool_call.rs`、`codex-rs/config/src/mcp_types.rs`、`codex-rs/codex-mcp/src/mcp/mod.rs`，tag `rust-v0.154.0`）：

- 按工具的 `approval_mode` 优先于 `default_tools_approval_mode`；取值 `auto` / `prompt` / `writes` / `approve`。
- `prompt` 一律要审批；"本会话都允许"只给 `auto` 记，`prompt` 下被降成只批这一次——所以每次都问。
- 审批策略是 `never` 时，要审批的调用直接拒；但 `never` 加上能全盘写的权限时，`mcp_permission_prompt_is_auto_approved` 直接放行——Full Access 就是这个组合，所以不再问。
- 审批默认由人来答（`approvals_reviewer` 默认 `User`）；`/permissions` 里的 Approve for me 把它换成 `AutoReview`，交给 Codex 自己的自动审查。**这一档怎么判没测**（它要调真实模型）。

所以：交互会话可以用 `prompt`；print 会话必须保持 `approve`；会话里的人能自己把这道闸交回 Codex，ccnm 拦不住——这一点和 Claude 不同，Claude 认的 `anthropic/requiresUserInteraction` 在任何权限模式下都生效。

## 3. 实现

**谁决定"要不要问"**：仍是 Runtime。Claude 会话的 `tools/list` 里，`exec_command`（以及有东西可转时的 `call_mcp_tool`）挂着 `anthropic/requiresUserInteraction`，条件是交互会话、不是外部客户端、workspace 没开 `allow_unattended_exec`（`mcp/server.rs`）。Codex 不认这个键，但 Agent 在启动 Codex 之前本来就要对 Runtime 做一次 MCP 预检。

| 步骤 | 改了什么 |
| --- | --- |
| 预检 | `work::provider_runtime_preflight` 多一个 `interactive` 参数：交互启动传 `true`（`OpenPayload` / `ServePayload` 的 `with_interactive`），print 传 `false`。返回 Runtime 标了键的工具名 |
| 探测报告 | `protocol::mcp::ProbeReport` 加可选字段 `asks_user`（空时不序列化，doctor 收到的旧报告照样读）；`mcp::probe` 按 `mcp::server::asks_the_user` 填它 |
| 会话记录 | `session::Spec` 加可选字段 `ask_before`（空时不写，P71 之前的记录读成空）。`start_fresh` 只给走 MCP 的 Codex 会话写；Claude 自己从工具表读，exec-server 链没有 ccnm 的工具 |
| 启动 Codex | `provider::codex::build_launch_cmd`：交互会话里，对 `ask_before` 中属于 ccnm 工具表 `MCP_TOOLS` 的每一个加 `-c mcp_servers.ccnm.tools.<工具>.approval_mode="prompt"`；不在表里的名字不会拼进配置键。`default_tools_approval_mode="approve"` 不变，所以其余工具照旧不问 |
| doctor | `Command approval`：Codex 不再是 WARN"不问"，而是 OK 并说明会话里的 `/permissions` 能交回 Codex；`allow_unattended_exec` 两边同一句 WARN |

`start_fresh` 顺带去掉了一直没用的 `tmux` 参数（加一个参数后超过 clippy 的 7 个上限）。

**兼容**：`asks_user` 是 Agent 探测时自己从 `tools/list` 的 `_meta` 里读出来的，Runtime 不用为此改任何东西——它在交互会话里挂那个键早在 P71 之前就是这样，所以新 Agent 对旧 Runtime 一样读得到。旧 Agent 不读这个键，Codex 照旧不问。两台本来就要装同一个构建。Codex 的兼容性 fixture（`tests/fixtures/codex-0.154.0/`）录的是 print 启动参数，这次没有变化，没有重录。

## 4. 测试

| 用例 | 证明什么 |
| --- | --- |
| `an_interactive_preflight_learns_which_tools_ask_from_the_runtime`（`crates/ccnm-cli/tests/instance_execution.rs`） | 真实二进制的 `internal mcp-serve`，按预检的方式探测三种情况：交互 → `asks_user == ["exec_command"]`；非交互、开了 `allow_unattended_exec` → 空 |
| `the_tools_the_runtime_marks_ask_in_an_interactive_session_only`（`provider::codex::tests`） | 交互会话每个标记过的工具恰好一个 `prompt` 键、去重、不认识的名字不写；print 会话一个都没有、仍是 `approval_policy="never"`；空列表时启动参数与以前相同 |
| `only_exec_command_makes_the_client_ask_every_time`（`mcp::server::tests`，补一句） | 生产版 `asks_the_user` 与测试里按序列化 JSON 的判定一致（防 rmcp 改字段名） |
| `the_approval_row_says_how_codex_sessions_ask`（`doctor::tests`，原 `…_do_not_ask` 按新行为改写） | Codex 是 OK、提到 `/permissions`、不说"任何权限模式"；Claude 不变；`allow_unattended_exec` 时 Codex 也是 WARN |

**没有先红后绿的那一条**：旧代码没有这条路径（`asks_user` / `ask_before` 字段都不存在，编译不过），所以红的证据是第 2 节的基线实测——同样的启动参数下三次调用都不问。

**没有端到端测试的一段**：交互启动从预检拿到列表、写进会话记录这一步（`work::preflight` → `start_fresh`）。要跑通得有 tmux、Controller 和 Codex 的就绪检查，现有夹具没有覆盖交互启动；两端各自有测试，中间这一段靠代码审读。

## 5. 门禁

本机负载 17–35（10 核）。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`（1.98 与 `+1.99.0`） | 通过 |
| `cargo test --workspace`（默认线程数与 `--test-threads=64`） | 各 1075 通过（P70 后 1073，新增 2 条，另有 1 条按新行为改写） |
| `cargo +1.89 check --locked --workspace --all-targets` | 通过 |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议（46 + 29 fixture）、Python 262 条 0 跳过 |
| `python3 scripts/check_plan.py`、`check_protocol.py`（改完文档后重跑） | 通过 |

实测用的 Codex 二进制、假模型的请求记录和交互界面截屏留在本轮会话的临时目录，没有入库；tmux 的 socket 因为 Unix socket 路径上限放在 `/tmp/ccnm-p71/`，测完已删。

## 6. 没覆盖的

- **真机与真实模型**：没跑。真机复验要在 Agent 装 P71 构建后，起一个受管 Codex 交互会话让模型调一次 `exec_command`，看提示、选取消、确认 Runtime 上没有执行；花一次 Codex 额度，需要另行授权。
- Approve for me 那一档怎么判（要真实模型）。
- 只测了 Codex 0.154.0。换版本时按支持矩阵的流程重测；这个设置的键名与取值是从 0.154.0 源码与实测来的。
