# P93：Claude Code 探测新协议后退回旧协议，受管会话拿不到工具（2026-10-15）

**证据范围**：fodelf（Agent，Claude Code 2.1.300）上 Claude 自己写的 MCP 日志、xdwmbp（Runtime）的系统日志，本机 macOS arm64 上的 Rust 测试和夹具。没有调用模型。

## 1. 用户看到的和实际原因

用户问"最近的 codingagent 会话总是遇到 ccnm MCP 没起来"。fodelf 上 ccnm 各项目的 MCP 日志一共 83 个文件，从 10-14 起有四次没拿到工具，原因有三种：

| 时间（东京） | 项目 | 日志里写的 | 原因 | 现状 |
| --- | --- | --- | --- | --- |
| 10-14 20:23 | shun | `Server stderr: CCNM_E_POLICY: … does not hold the Agent boundary`，`Connection failed after 701ms` | xdwmbp 写着 `runtime_user = "bing"`（专用账号模式），Runtime 握手时按设计拒绝 | 10-15 已删那一行；P91 起这种情况在开会话之前就报错 |
| 10-15 14:39 | codingagent | 同上，`after 631ms` | 同上 | 同上 |
| 10-15 17:59 | codingagent | Claude 重启一次 MCP server，`ssh: connect to host xdwmbp port 22: Operation timed out`，之后不再试 | xdwmbp 的 Wi-Fi 收不到路由器信号（系统日志 `roamed due to lost beacons`、`no matching network found`），17:59:28 到 18:05:54 没有 IPv4；两台之间走 Tailscale 中继，断网期间 ssh 连不上 | 环境问题，ccnm 管不了；恢复后在 Claude 里 `/mcp` → `ccnm` → `Reconnect` |
| 10-15 21:02 | codingagent | `Failed to fetch tools: request _meta is missing or has malformed required fields` | **ccnm 用的 rmcp 3.2.0 的缺陷**，见第 2 节 | 本阶段修 |

## 2. 21:02 那次

fodelf 上 `~/Library/Caches/claude-cli-nodejs/-Users-fodelf--local-state-ccnm-workspaces-codingagent/mcp-logs-ccnm/2026-10-15T12-02-58-789Z.jsonl`（文件名是 UTC）原文：

```text
12:02:58.791Z  Starting connection with timeout of 30000ms
12:03:01.833Z  Successfully connected (transport: stdio) in 3074ms
12:03:01.834Z  Connection established with capabilities: {… "serverVersion":{"name":"ccnm","version":"0.16.1"},"protocolEra":"legacy","negotiatedProtocolVersion":"2025-11-25"}
12:03:01.834Z  stdio server answered the server/discover probe after its bound, so it was only slow to start and speaks the new protocol; not remembering it as a legacy peer
12:03:01.873Z  Failed to fetch tools: request _meta is missing or has malformed required fields: io.modelcontextprotocol/protocolVersion, io.modelcontextprotocol/clientCapabilities
12:03:01.875Z  Failed to fetch commands: request _meta is missing or has malformed required fields: …
```

过程：

1. Claude Code 先发 2026-07-28 协议的 `server/discover`（带 `_meta`）。
2. 这次 `mcp-serve` 经 ssh 起了 3.07 秒，超过 Claude 愿意等的时间，Claude 就改发旧协议的 `initialize`，协商到 2025-11-25，之后的请求不带 `_meta`。
3. rmcp 3.2.0 的 `serve_server_with_ct_inner`（`src/service/server.rs`）把第一条不是 `initialize` 的请求当作新协议的开场，给 peer 置了 `require_request_metadata()`，以后不再清。所以 `initialize` 照常回答，`tools/list`、`prompts/list` 却都因为缺 `_meta` 被拒。
4. 会话里有 ccnm 写在 `initialize` 回答里的说明，但没有一个工具；模型调用时报 `No such tool available: mcp__ccnm__exec_command`。

rmcp 3.5.1（2026-10-05）的 #1248 "keep discover lifecycle bootstrap-neutral" 修了这一点：discover 只当探测，回答它但不选定协议，等到 `initialize` 或第一个带完整 `_meta` 的其他请求才定。

## 3. 多常碰到

fodelf 上 ccnm 各项目的连接（每个日志文件一般一次）：

- 09-30 以前的都是 `legacy`（Claude 直接发 `initialize`）。那时有过 3.24 秒、7.59 秒的慢连接，都没出事，因为没有探测。
- 09-30 起都是 `modern`（先 discover，协商到 2026-07-28）。到 10-15 共 31 次：30 次在 0.40–1.38 秒内连上，全部正常；只有 21:02 那次 3.07 秒，退回旧协议，撞上这个缺陷。

所以 Claude 的等待时限在 1.38 秒到 3.07 秒之间（没去量具体值）。平时连接在 0.4–1.4 秒，所以只是偶尔碰到；两台之间走 Tailscale 中继（DERP，往返约 82 毫秒）时，ssh 建连慢一点就会撞上。P81（[记录](2026-10-08-p81-mcp-2026-07-28.md)）测过 discover 这条路，但当时 ccnm 起得快，没有走到"超时退回"这一支。

## 4. 复现与修

**测试**：`crates/ccnm-core/src/mcp/server.rs` 的 `a_client_that_probes_then_falls_back_still_gets_the_tools`，在内存管道上按 discover → `initialize`（2025-11-25）→ 不带 `_meta` 的 `tools/list` 跟 ccnm 的 server 说话。rmcp 3.2.0 上红，报错与真机一字不差：

```text
no tools in {"error":{"code":-32602,"message":"request _meta is missing or has malformed required fields: io.modelcontextprotocol/protocolVersion, io.modelcontextprotocol/clientCapabilities"},"id":3,"jsonrpc":"2.0"}
```

另一条 `a_client_that_stays_on_the_new_protocol_gets_the_tools` 走 discover → 带 `_meta` 的 `tools/list`，3.2.0 和 3.5.1 上都绿，防止升级丢了平时正常跑的那条路。

**夹具**：[p93-discover-fallback.py](probes/p93-discover-fallback.py) 用真实二进制按同样的顺序重放（本机直接起，或 `--ssh` 经 ssh 起，和受管会话一样）。本机结果：

| 二进制 | 对照（直接 `initialize`） | 先探测再退回 | 退出码 |
| --- | --- | --- | --- |
| 已装的 ccnm 0.16.1（rmcp 3.2.0） | 12 个工具 | `tools/list` 报 `request _meta is missing …` | 1 |
| 本阶段构建（rmcp 3.5.1） | 12 个工具 | 12 个工具 | 0 |

**修**：rmcp 3.2.0 → 3.5.1。锁文件只动三处：rmcp、rmcp-macros，以及 rmcp 3.3.0 起要求的 process-wrap 9.1.0 → 10.0.1；nix 还是 ccnm 和 process-wrap 共用的 0.31.3。`Cargo.toml` 的下限改成 3.5.1，免得重新生成锁文件时退回去。`ServerInfo` 是 3.4.0 起弃用的别名，换成同一个类型的新名字 `ServerConfig`。

升级的其他影响核对过：

- ccnm 转发别的 MCP server 时用的是 rmcp 客户端，`ServiceExt::serve` 在 3.5.1 里仍是旧协议 `initialize`（`src/service/client.rs` 的注释："Existing [`ServiceExt::serve`] behavior remains legacy initialization."），转发不受影响。
- 3.5.1 的 #1308 会给新协议下没带缓存提示的列表补默认值；ccnm 自己就带（P81），`external_mcp` 里那条缓存提示测试和中立客户端测试照旧通过。

## 5. 没做的

- **心跳的 ping**：ccnm 的 `mcp-serve` 空闲时每 30 秒向客户端发一个 ping，用来发现写不出去的半开连接。新协议下 Claude Code 2.1.300 不接受服务端主动发来的请求，日志里约每 60 秒一条 `Dropped inbound request 'ping': not servable on this connection's protocol era`。14:45 那个会话跑了 3 小时 27 分、446 次工具调用都正常；半开检测靠的是"写不出去"，不靠回答，所以仍然有效。只是日志噪音，没改。
- Claude 的探测等待时限没量具体值，只从日志知道在 1.38–3.07 秒之间。
- 真机上的验证（换装后经 ssh 重放）记在发版记录里。
