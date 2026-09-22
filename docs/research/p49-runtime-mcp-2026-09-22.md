# P49：Runtime 上的 MCP server 转给会话（2026-09-22）

零额度，macOS arm64。共用机制在 toexec 的 `toexec-mcp` 0.1.0（`1e8b06a`，本地 tag `toexec-mcp-v0.1.0`，gld 同用）；实测脚本和结果在 toexec 的 `evidence/v4-mcp/`。这里只记 ccnm 按它做了什么决定。怎么开关见[配置说明](../configuration.md#runtime_mcp)，契约见[协议第 5.7 节](../protocol/remote-workspace-mcp-v1.md#57-call_mcp_toolruntime-上的-mcp-serverp49-新增)。

## 起因

用户 2026-09-22 要 gld 和 ccnm 都能用上两台机器上已经装好的 skills 和 MCP server，ccnm 默认全开，共用代码进 toexec，不要了能整块删；第 3 步是"ccnm 在 Runtime 上代理 MCP server（含项目自带的 `.mcp.json`）"。数据库这类 server 只能跑在项目旁边，而受管会话的 Claude / Codex 在 Agent 机器上。

## 决定

| 问题 | 决定 | 为什么 |
| --- | --- | --- |
| 一个工具还是几个 | 一个 `call_mcp_tool`：不带参数列 server、带 `server` 列工具、再带 `tool` 调用 | 起 server 就是以执行账号跑程序，有人值守的会话里必须每次问人（和 `exec_command` 一样）；`requiresUserInteraction` 是按工具打的，分成"列"和"调"两个工具只会让人被问两次。`load_skill` 已经是"不带名字就列"的写法 |
| 大结果 | 超过 32 KiB 的文字进这个会话的留存目录，用 `read_output` 接着读 | 同一套分页、上限、过期和"只在本会话有效"，不另造一个第三个工具。32 KiB 是 `read_output` 一页的上限 |
| 哪些 server | 项目 `.mcp.json` > 执行账号 `~/.claude.json` > `~/.codex/config.toml`，同名先列的赢 | 项目压过 user 级是 Claude Code 的规矩 |
| HTTP server | 不转，列出来写明原因 | 不需要跑在项目旁边，从 Agent 那边连（v4 第 4 步）；也省得 ccnm 为它带一个 HTTP 客户端和 TLS |
| 过什么门 | 和 `exec_command` 完全一样（read 模式没有、执行门、凭据检查、OS 沙箱、环境清理、有人值守时问人），只列清单不起东西就不过门 | 起一个 server 和跑一条命令没有区别；项目的 `.mcp.json` 就是项目点名要跑的程序 |
| server 的 `env` | 照传（token 也传），Agent 的登录变量除外；`${VAR}` 不查像凭据的名字 | 配置里写给 server 的 token 是写配置的人授权给它的；而 Agent 的登录不能借一个 `.mcp.json` 换个名字流进 server。ccnm 的执行门本来就不许 Runtime 进程的环境里有像凭据的变量，所以这条在正常部署里只是再拦一道 |
| 杀进程 | 没在 3 秒宽限内退出的，起一个 `kill -KILL -- -<pgid>` 杀它的进程组；自己退了的不动 | ccnm 禁 `unsafe`（没有 `killpg`），且不对收过尸的 pid 发信号（`process::Tracked` 的规矩）；这一步交给产品正是 `toexec-mcp` 这样设计的原因 |
| 会话结束 | 先停 server，再放写锁 | server 能写工作树，和 P43 同一个理由 |
| 开关 | `[runtime_mcp]`：`enabled`、`project`、`hidden`，默认全开，只在 Runtime 上读 | 在这台机器上以执行账号跑什么，是这台机器的决定 |

**共用代码**：v4 第 2 步 gld 先写了读配置、握手调用、子进程通道、连接池和结果整理；这一步 ccnm 也要，整块搬进 toexec 成了 `toexec-mcp`，gld 改链它。它是 toexec 里第一个有依赖的 crate（serde_json、toml），例外写进了 toexec 的开发规矩。ccnm 这边只有 `mcp/relay.rs`（怎么起、怎么杀、工具的样子、错误码、大结果放哪）。

## 验证

- Rust：`cargo test --workspace` 921 passed / 0 failed（P48 时 914，新增 7 条：`mcp::relay` 6 条用一个 sh 写的假 server 真起进程，`mcp::server` 1 条钉住"有 server 才列、有人值守要问、read 模式没有"）。另两条原有断言按有意改动调整：两处"工具数等于 `MCP_TOOLS`"改成"少 `call_mcp_tool` 一个"（测试 server 没有可转的），Claude 基线测试把允许表多的 `mcp__ccnm__call_mcp_tool` 施加到期望值上，fixture 没重录。fmt、clippy `-D warnings`、`cargo +1.89 check` 通过。
- 中立 MCP 客户端对真实二进制：`tests/test_remote_workspace_mcp.py` 新增 6 条（coding 会话调通且配置里的 token 到了、Agent 登录没到；大结果经 `read_output` 一个字节不少地读回 52 000 字节；read 会话没有也调不到；没隔离又没 opt-in 时列清单行、起 server 被拒；`project = false` / `enabled = false` 时没有；会话结束时 server 进程已经没了、下一个 coding 会话进得来）。`python3 -m unittest discover -s tests` 205 passed。`check_protocol` 通过，`external_mcp` 29 passed。
- gld 连真实 ccnm：gld 的 `crates/core/tests/ccnm_background_lifecycle.rs` 新增一条，经真实 `internal mcp-serve` 调到项目 `.mcp.json` 里的 server，关会话后它的进程消失。
- 真实客户端（toexec `evidence/v4-mcp/runtime-relay/`，假模型、零额度）：Claude Code 2.1.278 print 模式下工具出现、被允许表放行、嵌套 `arguments` 原样到达、大结果照说明接 `read_output`；Codex 0.155.1（`gpt-5.1-codex`）在 `mcp__ccnm` 命名空间里调通。

**没验的**：真实模型会不会主动用；受管会话经 SSH 那条路，以及有人值守时 Claude Code 真的每次问人（只有单元测试钉住那个键）；Codex 0.154.0（受管会话钉的版本，本机是 0.155.1）；Linux；OS 沙箱里跑 server。

## 不要了怎么删

| 仓库 | 删什么 |
| --- | --- |
| ccnm | `mcp/relay.rs`；`server.rs` 里的 `call_mcp_tool` 工具、`Inner::relay`、`relayed()`、`run` 里那段 `close_all`，`WITHHELD_WITHOUT_WRITE` / `INTERACTION_TOOLS` / `annotations_for` 里的名字；`config.rs` 的 `RuntimeMcp`；`session::MCP_TOOLS` 的最后一项；`toexec-mcp` 依赖；`tests/fixtures/fake_mcp_server.py` 和中立客户端里 P49 那几条 |
| gld | hub 的 `remote_call_mcp_tool`（`bridge/tools.rs` 一项、`remote_failure` 里那段专门的话）；gld 自己转本机 server 的那一块另见它的 RFC-0006 |
| toexec | `toexec-mcp` 整个 crate——gld 第 2 步的功能也在用它，删之前先删 gld 那边 |
