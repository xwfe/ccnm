# P77：`agent_tools` 默认全开（2026-10-07）

## 结论

Runtime 的 workspace 不写 `agent_tools`，P77 起等于五项全开：`web_search`、`web_fetch`、`subagents`、`tasks`、`mcp_servers`。写明了的值意思不变，`[]` 仍是全关。用户 2026-10-07 定的。

Claude 受管会话因此默认多了 `WebFetch`、`Agent`、`TaskStop`、`TaskCreate`、`TaskGet`、`TaskList`、`TaskUpdate` 七个工具（都写进会话设置的允许表）；Codex 会话不变——这三项在 Codex 上本来就没有对应工具。

## 为什么两台机器之间的"不带这个字段"不跟着变

`agent_tools` 是 Runtime 说了算的东西，经三条内部消息和一份会话记录交给 Agent：`ResolveReport`、`RunRequest`、`StartRequest`，以及 Agent 落盘的 `Spec`。P46 起这四处都是"等于默认值就不写"，读的时候"没写就是默认值"。

如果默认值变了而"没写"的读法跟着变，新 Agent 碰上旧 Runtime 时，旧 Runtime 的默认 workspace（它的意思是"搜索 + MCP server"）会被读成五项全开——抓网页和子代理是那台 Runtime 从来没同意过的。所以拆成两个概念：

| | 是什么 | 用在哪 |
| --- | --- | --- |
| `AgentTools::default()` | 五项全开 | `config.toml` 里不写这一行 |
| `AgentTools::omitted()` | `web_search` + `mcp_servers`（P50 到 P76 的默认） | 内部消息和会话记录里"不带这个字段" |

结果：

- 新 Runtime 的默认 workspace 会把五项写明发出去。
- 新 Agent 读到旧 Runtime 不带字段的消息，按旧默认开，不多开。
- 旧 Agent（v0.8.0 及之前）读到写明的字段报 `unknown field agent_tools`，P46–P49 的开发构建报 `unknown variant mcp_servers`，都是拒绝而不是换一套工具起会话。实际上两端版本不同时会话在 `greet` 那一步就被 `CCNM_E_VERSION` 拒了，这一层是多一道保险。
- 一份 P50–P76 写下、没带这个字段的会话记录，读回来仍是"搜索 + MCP server"。

## 用例

先把新用例写好，再把 `AgentTools::default()` 临时改回 P50 的值跑一遍，下面四条都红；改回新值都绿：

- `config::tests::agent_tools_default_to_all_and_refuse_unknown_names`：不写等于五项；写明 `["web_search", "mcp_servers"]` 读出来是它、写回去也写明。
- `runtime::tests::a_resolve_carries_the_workspace_agent_tools`：默认 workspace 的 `ResolveReport` 写明五项；`omitted()` 那种不写；不带字段的 JSON 读回来是 `omitted()`。
- `session::tests::a_record_without_agent_tools_reads_as_the_p50_default`：会话记录同上。
- `provider::claude::tests::a_remote_session_keeps_only_the_workspace_agent_tools`：默认 `--tools` 是八个名字（原来的 `WebSearch` 加上新开的七个）；`omitted()` 时只有 `WebSearch`（`mcp_servers` 不是原生工具）。

## 没做的

- 没跑模型，没在交互会话里量过 `WebFetch` 写进允许表后是否真的不问（`--print` 下 P46 量过必须写进去，交互会话用同一份设置）。
- Codex 的映射不变。
- 没动任何机器上已装的 ccnm 或配置；没发版。
