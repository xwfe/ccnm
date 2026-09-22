# P46：受管会话默认开 WebSearch，其他 Agent 功能按工作区开（2026-09-22）

零额度，macOS arm64。脚本和每个场景的汇总在 toexec 仓库的 `evidence/v3-parity/agent-surface/`（toexec e0b5c7d）；这里只记 ccnm 按它做了什么决定。用法见[配置说明](../configuration.md#agent_tools)。

## 起因

v3 方案第 5 节第 6 步"放开 Agent 面"一直卡在一个问题上：除 WebSearch 外的 Agent 功能是默认开，还是按 workspace opt-in。用户 2026-09-22 定：**默认开 WebSearch，其他 Agent 功能做成合理的开关，按建议设置。** 建议并已实施：WebSearch 默认开，WebFetch、子代理、待办清单 opt-in。

## 先量的五件事

Claude Code 2.1.278 连本机假模型接口（`ANTHROPIC_BASE_URL` + 假 key，临时 HOME，`sandbox-exec` 禁非本机出站），启动参数照 ccnm 的 `launch_cmd` 拼；Codex 0.154.0（ccnm 钉的版本，连同 `codex-code-mode-host`）同样连假接口，参数照 `build_launch_cmd` 拼。

| 问题 | 结果 | 对设计的影响 |
| --- | --- | --- |
| `--tools` 认哪些名字 | 认 `WebSearch`、`WebFetch`、`Agent`、`Skill`、`TaskCreate/Get/List/Update/Stop`、`NotebookEdit`；`TodoWrite`、`Task`、`AskUserQuestion`、`EnterPlanMode`、`ExitPlanMode`、`ToolSearch` 等在 print 模式下**被悄悄忽略、不报错** | 名字表钉在代码里并有测试；计划模式和"问用户"在 print 下不存在，不做开关 |
| 子代理能不能绕过限制 | 子代理那次请求的工具表和主会话**完全一样**（`Agent`、`WebSearch` 加 ccnm 的工具），没有 Read / Bash | `subagents` 可以做成开关，不会成为碰 Agent 磁盘的路 |
| 白名单非空以后 MCP 工具还是不是全量加载 | 强开工具搜索也一样：白名单里**没有** `ToolSearch` 时照旧全量；**有**的话 ccnm 的工具和 WebSearch 全进延迟加载池，模型只看得到 `ToolSearch` | `ToolSearch` 永远不进白名单，P15 的结论照旧成立 |
| print 模式哪些要写进允许表 | 允许表只有 ccnm 工具时，`WebSearch`、`WebFetch` 被自动拒绝（"this session has no approval surface"）；`Agent`、`TaskCreate` 照常执行 | 开了的都写进允许表（四个一视同仁，免得"开了却用不了"随工具而异） |
| Codex 的 `web_search` 加了什么 | 指定 `gpt-5.1-codex`：`cached` 加 `{"type":"web_search","external_web_access":false}`，`live` 是 `true`，Code Mode 不排除它；**不写 model（CLI 默认 `gpt-6-astra`）三种取值的请求一字不差** | 开 = `cached`；默认模型下"开了也不见效"写进文档 |

Codex 默认模型那条排除过两个原因：改走内置 openai provider（`openai_base_url` 指到假接口）结果一样；本机 0.155.1 也一样。模型目录里这个模型声明了 `web_search_tool_type: "text_and_image"`。没查明的是这个请求形态下托管工具本来就不发，还是要别的条件。

## 做了什么

**配置**：workspace 字段 `agent_tools`，取值 `web_search` / `web_fetch` / `subagents` / `tasks` 的集合；不写等于 `["web_search"]`，`[]` 全关，未知名字整份配置报错。写在 Runtime 上：`web_fetch` 能把项目内容带到任意 URL，担风险的是 Runtime，和 `allow_*` 那几个开关同一个理由。

**线上格式**：`ResolveReport`、`RunRequest`、`StartRequest`、session 记录各加一个 `agent_tools`，**只在不等于默认值时写**。于是默认配置的请求和以前逐字节相同，旧 Agent 照常读；非默认的请求旧 Agent 按未知字段拒绝（这几个结构都是 `deny_unknown_fields`），而不是悄悄用另一套工具起会话。协议版本号不动，和 P23 的 `codex_exec_server` 同一个做法。

**Claude**（`provider/claude/policy.rs`）：

| 开关 | `--tools` 里的名字 |
| --- | --- |
| `web_search` | `WebSearch` |
| `web_fetch` | `WebFetch` |
| `subagents` | `Agent`、`TaskStop`（子代理默认在后台跑，`TaskStop` 是停它的） |
| `tasks` | `TaskCreate`、`TaskGet`、`TaskList`、`TaskUpdate` |

settings.json 的允许表是 ccnm 的工具加上开了的这些名字；拒绝表是原来的六个原生工具，加上本次实测 `--tools` 认得的 `NotebookEdit`、`Skill`（一个写本机磁盘，一个从本机磁盘读；项目自己的由 `read_notebook`、`load_skill` 在 Runtime 上提供），再加上没开的那些名字。colocated 拓扑不受影响（它本来就在建会话前被拒绝）。

**Codex**（`provider/codex/mod.rs`）：只接 `web_search`，开就是 `web_search="cached"`（Codex 自己的默认值：用 OpenAI 的索引，不现抓网页），关是 `disabled`。另外三个开关对 Codex 不起作用：没有对应工具（`web_fetch`、`tasks`），或者没量过（子代理：`agents.enabled` 仍是 `false`，Codex 会不会把关掉的 feature 和 ccnm 的工具白名单传给子代理没有证据）。

**默认行为的变化**：P46 之前每个远端会话都是 `--tools ""` + `web_search="disabled"`。现在不写 `agent_tools` 的工作区会多出搜索；要回到原样写 `agent_tools = []`。

## 测试和 fixture

两份实测 fixture 都没重录：

- `tests/fixtures/claude-provider-baseline.json`：比对它的测试照 P36–P41 的做法，把本阶段的有意改动逐条施加到期望值上（`--tools` 从空变成 `WebSearch`、允许表加 `WebSearch`、拒绝表加九个名字），并写明原因。
- `tests/fixtures/codex-0.154.0/seven-tools.json`：比对 argv 的测试改用 `agent_tools = []`，因为那次实测就是在搜索关着的时候录的；新增一条测试钉住"默认配置和它只差 `web_search` 这一个值"。

Claude 那条"真机证明过的 argv"测试同理改用全关配置。新增 5 条：配置解析（默认、空、重复、未知名字、默认不写回）、resolve 的线上格式、Claude 的 `--tools` 与允许 / 拒绝表各一条、Codex 的 `web_search`。

## 门禁

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | 898 passed / 0 failed（P45 时 893，多的 5 条是本阶段新增） |
| `cargo +1.89 check --workspace --all-targets --locked` | 通过 |
| `python3 -m unittest discover -s tests` | 194 passed |
| `check_plan`、`check_protocol`（38 + 29 个 fixture）、`git diff --check` | 通过 |

## 没验的

- **真实模型一次都没跑**：模型会不会用搜索、搜索和抓取的真实结果、子代理真实跑起来的额度开销。
- Codex 默认模型下搜索不见效的原因。
- Codex 的子代理（仍关）。
- Linux 上没跑；以上全部只在 macOS arm64。
