# P13：instructions 按 Host 实际上限投影（2026-09-16）

## 结论

- Claude Code 2.1.269 只保留 MCP `instructions` 的前 **2048 个 UTF-16 码元**，多出的换成 `… [truncated]`。不是 2KB 字节：中文一个字算 1。
- 改前 ccnm 按 16 KiB 字节预算，标记行 `[project instructions: …]` 在最后一行。真实握手 4600 个码元，Claude Code 截掉的正是标记行和后半个项目文件，模型不知道少了东西、也不知道怎么读全文。
- 改后：Claude Managed 和外部 `project` 模式按 2048 个 UTF-16 码元投影，顺序改为 ccnm 自己那段 → 标记行 → 其他说明文件清单 → 项目文件正文；同一个真实 Claude Code 连接不再报截断。Codex Managed 仍是 16 KiB 字节。
- **没验到的**：模型实际看到的文本（本机 CLI 未登录，两次连接都没发出模型请求）；Managed 路径没有走真实 Claude Code 连接，只有单元测试和 golden 重排测试；Codex 延迟加载工具时只显示说明首行前 250 个字符，本阶段不处理。

## Host 行为依据

打包在本机 `claude` 二进制里的 JS（2.1.269，`GIT_SHA d0733697`）：

```js
var FT=2048
function Ro(e,n){if(!e)return e;return Ao(e,"Server instructions",n)}
function Ao(e,n,r){if(e.length<=FT)return e;
  if(r!==void 0)J(r,`${n} truncated from ${e.length} to ${FT} chars`);
  return ne(e,FT)+"… [truncated]"}
```

连接建立时 `instructions: Ro(getInstructions(), name)` 存入连接对象，之后拼成 `## <server>\n<instructions>` 进入 `# MCP Server Instructions`，中间不再读原文。独立探针（不含 ccnm 代码）的复现在 workspace-kernel 仓库 `evidence/v2-q1/README.md`：3018 码元的说明被记为 `truncated from 3018 to 2048 chars`。

Codex 0.154 源码（`codex-mcp/src/rmcp_client.rs`）把 instructions 作为工具命名空间说明；code-mode 路径整段放进 exec 工具描述，没有找到截断，所以 Codex 预算不变。延迟加载时 `core/src/context/world_state/tools.rs` 只取首行前 `MAX_NAMESPACE_DESCRIPTION_CHARS = 250` 个字符——ccnm 的首行约 390 个字符，那种模式下会被截，记入 observed_gaps。

## 真实连接对照

同一脚本、同一台 macOS arm64、同一个 Claude Code 2.1.269，只换 ccnm 二进制：改前是提交 `16a6991` 的构建，改后是本阶段代码的构建。

做法：临时目录里放一份 Runtime 配置（workspace `demo`，`external_mcp = "read"`，`external_instructions = "project"`），项目文件用 ccnm 仓库自己的 `AGENTS.md`（6306 字节、3806 个码元，sha256 前缀 `e59304387de2bcce`）。先用中立客户端发一次 `initialize` 取原文量长度，再让 `claude -p --strict-mcp-config --mcp-config … --debug-file …` 连同一个 server。

两个坑，照做会撞上：

1. **Claude Code 会给 MCP 子进程注入 `CLAUDE_CODE_MESSAGING_TOKEN`。** ccnm 按设计把 `_TOKEN` 结尾的变量当成继承来的凭据，拒绝初始化（`CCNM_E_POLICY … authentication inherited from the environment`），Host 只显示 `Connection closed`。真实部署里 mcp-serve 在 ssh 那头，`SendEnv=-*` 不带任何环境，所以本地复现时 MCP 命令写成 `/usr/bin/env -i PATH=… HOME=… XDG_STATE_HOME=… CCNM_CONFIG=… ccnm internal mcp-serve --payload …`，等价于 ssh 落地后的干净环境。这不是 ccnm 的缺陷，不要为此放宽检查。
2. **在 Claude 桌面端会话里跑，要 `env -i` 起 `claude`。** 否则子进程继承宿主的环境变量，同样被上面那条检查拒绝。本机 `claude auth status` 为 `loggedIn: false`，所以两次都是连接完成后在认证阶段停下（`Not logged in`，约 80 ms，`input_tokens` 0），不耗订阅额度。

| | 改前（`16a6991`） | 改后 |
| --- | --- | --- |
| 握手原文 | 4600 个 UTF-16 码元 / 7100 字节，47 行 | 2030 个 UTF-16 码元 / 3140 字节，22 行（ccnm 自己的部分 847，项目文件 1183） |
| 标记行位置 | 第 47 行（最后一行） | 第 4 行（模式句之后） |
| 标记行内容 | `[project instructions: AGENTS.md, 6306 bytes]` | `[project instructions: AGENTS.md, 6306 bytes, first 2294 shown; read_file AGENTS.md for the rest]` |
| Claude Code debug 日志 | `Server instructions truncated from 4600 to 2048 chars` | 无截断行 |

改前日志原样摘录（去掉时间戳）：

```text
[DEBUG] MCP server "ccnm": Successfully connected (transport: stdio) in 98ms
[DEBUG] MCP server "ccnm": Server instructions truncated from 4600 to 2048 chars
[DEBUG] MCP server "ccnm": Connection established with capabilities: {"hasTools":true,...}
```

改后：

```text
[DEBUG] MCP server "ccnm": Successfully connected (transport: stdio) in 97ms
[DEBUG] MCP server "ccnm": Connection established with capabilities: {"hasTools":true,...}
```

改前那份握手里标记行说"6306 bytes"、没说被截——ccnm 自己确实没截（16 KiB 放得下），截的是 Host，所以标记行一直在说假话。

## 离线门禁

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `cargo test --workspace`：716 passed / 0 failed。
- `python3 -m unittest tests.test_remote_workspace_mcp -q`：10 passed；`python3 scripts/check_protocol.py` 与 `tests.test_check_protocol`：通过；全量 Python `unittest discover`：168 passed。

新增或改写的测试：

| 测试 | 证明什么 |
| --- | --- |
| `provider::claude::context::tests::utf16_caps_count_what_javascript_counts` | 中文算 1、字节算 3；BMP 外字符算 2 且不劈开代理对 |
| `…::the_marker_and_the_list_come_before_the_projects_file` | 标记行 < 清单 < 正文；正文里伪造的标记行不会被 `parse_marker` 读到 |
| `…::naming_is_bounded_and_takes_its_room_from_the_projected_file` | 200 个规则文件（扫描上限 40）时清单写"N of 40 or more listed"，整段不超 2048，正文仍有位置 |
| `…::a_huge_file_is_cut_at_a_line_and_the_marker_admits_it` | 中文长文件用满 2048 码元（>2008），按行截断 |
| `mcp::server::tests::a_long_claude_md_cannot_push_the_handshake_over_the_cap` | 真实 server 对 Claude Managed 用 2048 码元上限 |
| `ccnm-cli` `external_mcp::a_long_project_file_is_cut_to_what_claude_code_keeps_marker_first` | 真实二进制外部 `project` 模式：2000–2048 码元、标记行在正文前 |
| `provider_compat::claude_behavior_matches_snapshot_…` | golden fixture **没有重录**：测试把旧文本拆成四段按新顺序重拼后比对，内容一字不差，只动位置和段间换行 |
| `provider::codex::tests::root_context_follows_measured_override_priority_and_budget` | Codex 仍是 16 KiB 字节 |

## 双机复查（2026-09-16，升级之后）

Runtime（xdw_mbp）装上 0.7.0 后，在 Agent（fodelf，仍是 0.6.0）上跑 `ccnm doctor xdo`，远端 MCP 握手这一行：

```text
远端 MCP 握手  正常  initialize in 570 ms, tools/list (7 tools, 8852 B),
                     instructions 3021 B (CLAUDE.md, 3370 bytes, first 2341 shown;
                     read_file CLAUDE.md for the rest), workspace_info x1 …
```

这是真实双机、真实 SSH、真实项目文件（xdo 的 `CLAUDE.md` 3370 字节）下的新格式：标记行写明给了多少、怎么读全文。发起方是 ccnm 自己的 probe，不是 Claude Code，所以仍然不能由此推断模型看到了什么。旧 Agent 读新 Runtime 的握手没有问题——`parse_marker` 在 0.6.0 里是从后往前找，这段文本里只有一条标记行。

## 行为变化与兼容

- 只改 `initialize.result.instructions` 的长度和段落顺序；工具、权限、错误码、wire 版本都不变。`ccnm.workspace-mcp/1` 冻结条款没有覆盖上下文文本长度，协议第 8、10 节已同步。
- 项目文件较长的 workspace，开场给模型的正文变短了（例：ccnm 自己的 `AGENTS.md` 从名义上"全部 6306 字节"变成实际 2294 字节）。改前名义上更多，但 Claude Code 实际只留前 2048 码元而且不告诉模型；改后给的少一些，但标记行如实说明并指向 `read_file`。
- 新 Agent 读旧 Runtime 的握手时，`parse_marker` 改为取第一条标记行；只有旧 Runtime 的项目文件里本身含一行 `[project instructions: ` 开头的文字时，doctor/probe 才会读错，不影响会话。
- 其他说明文件清单最多占 768 个码元（约 15 条短路径）；放不下时写明总数，不再静默丢弃。
- 超长 workspace 名（上千字符）会让 ccnm 自己那段就超过上限，此时正文为 0；实际配置里的名字远短于此，没有另加限制。
