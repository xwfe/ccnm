# skill 文件头的 `allowed-tools`、`hooks`、`model` 在远端会话里怎么生效（设计，2026-10-07）

用户 2026-10-07 问：skill 文件头里的 `allowed-tools`、`hooks`、`model` 怎么设计才能生效。本文是设计；用户 2026-10-08 定第 7 节四件事全按建议，P79 已按它实现，做了什么、和本文的出入、怎么验的见 [P79 记录](2026-10-08-p79-skill-frontmatter.md)。

## 结论

| 字段 | 原生 Claude Code 的意思 | ccnm 里建议怎么做 | 为什么 |
| --- | --- | --- | --- |
| `hooks` | 调用 skill 时登记，本会话剩下的时间一直跑；`PreToolUse` 能拦下工具调用，`PostToolUse` 能把话带给模型 | **由 Runtime 跑**：加载 skill 后，Runtime 在它自己服务的工具调用前后执行这些命令；只在"这个会话里跑命令本来就不问人"时跑（开了 `allow_unattended_exec`、`--print`、`ccnm mcp bridge`） | 只有 Runtime 看得到项目工具调用；hook 是一条没人确认的命令，只能出现在已经没人确认命令的会话里 |
| `allowed-tools` | 调用 skill 的那一轮里，列出的工具不再问 | **不做逐条放行**。会话不问命令时，返回里写"已经不用问"；会问时写"没生效，要不问去开 `allow_unattended_exec`" | 问不问是 Host 按工具的静态标记决定的，MCP server 改不了"这一轮"；另做一个不问的工具，放行规则就写在模型自己能改的文件里 |
| `model` | 这一轮换模型；配 `context: fork` 时是子代理的模型 | 配了 `context: fork`、会话是 Claude 且开着子代理（P77 起默认开）时，返回开头告诉模型用 `Agent` 工具派子代理、带上 `model`；其余情况写明没生效 | MCP 换不了 Host 的模型；`Agent` 工具有按次指定模型的参数 |

`effort` 没法生效（`Agent` 工具没有这个参数），照旧写明。`` !`命令` ``（正文里加载时先跑的命令）不是这次问的，但和 hooks 是同一道门，第 5 节建议一起做。

## 0. 远端会话里 skill 是怎么到模型面前的

受管会话里 Claude Code 自带的 `Skill` 工具是关着的（`NATIVE_TOOLS_DENIED`，它要从 Agent 本机的磁盘读）。skill 只经两条 MCP 路到模型：

- Runtime 的 `load_skill`：项目里的 skill 和执行账号 HOME 下装好的 skill（P36、P48）；
- Agent 上 `ccnm_agent` 的 `load_skill`：Agent 机器上装好的 skill（P48）。

所以 **Host 根本不知道"某个 skill 正在生效"**，原生按 frontmatter 做的事一件都不会自动发生。要生效只能靠 ccnm 自己：要么在返回 skill 的那个 server 里做，要么告诉模型怎么做。三方各看得到什么：

| | 看得到什么 | 能做什么 |
| --- | --- | --- |
| Runtime 的 `mcp-serve`（每个会话一个进程） | 这个会话所有的项目工具调用、`load_skill` | 在工具调用前后做事；在 `load_skill` 返回里写字 |
| Agent 上的 `ccnm_agent` | 只有它自己的几个工具 | 只能在返回里写字 |
| Agent 上拉起 CLI 的那一步 | 启动参数和会话设置 | 只在会话开始时生效，之后改不了 |

## 1. 原生语义（2026-10-07 读官方文档）

[Skills 文档](https://code.claude.com/docs/en/skills)：

- `allowed-tools`："grants permission for the listed tools during the turn that invokes the skill … The grant clears when you send your next message … It does not restrict which tools are available"。
- `model`："The override applies for the rest of the current turn and isn't saved to settings … or `inherit` to keep the active model … With `context: fork`, the value sets the forked subagent's model instead"。
- `hooks`："Hooks that Claude Code registers when the skill is invoked and keeps running for the rest of the session"。
- `context: fork`："Claude Code starts a new subagent of the type set in the `agent` field and gives it the skill content as its prompt"。
- `` !`command` ``："runs shell commands before the skill content is sent to Claude"；"Injected commands never prompt for permission"；"A failed command aborts the entire skill invocation"。

[Hooks 文档](https://code.claude.com/docs/en/hooks)：skill 里的 hook 写法和设置文件一样；`once: true` 跑成功一次就撤；`PreToolUse` 退出码 2 拦下调用，`PostToolUse` 退出码 2 把 stderr 给模型；JSON 输出里 `permissionDecision`（allow / deny / ask / defer）、`permissionDecisionReason`、`updatedInput`、`additionalContext`、`updatedMCPToolOutput`；command hook 默认超时 600 秒；输入 JSON 有 `session_id`、`cwd`、`hook_event_name`、`tool_name`、`tool_input`、`tool_use_id` 等。项目级 hook 要先在原生里接受工作区信任。

[Subagents 文档](https://code.claude.com/docs/en/sub-agents)："When Claude invokes a subagent, it can also pass a `model` parameter for that specific invocation"；子代理继承主会话的 MCP 工具。

[Codex skills 文档](https://learn.chatgpt.com/docs/build-skills)：`SKILL.md` 只要求 `name`、`description`；`agents/openai.yaml` 有界面字段、`allow_implicit_invocation`、依赖的 MCP server。**没有** `allowed-tools`、`model`、`hooks` 的对应物，所以下面说的在 Codex 会话里只有 hooks（由 Runtime 跑，和 Host 无关）能生效。

## 2. `hooks`：Runtime 跑，只在命令本来就不问的会话里

**谁跑、什么时候登记**：模型调 `load_skill(name)` 加载一个带 `hooks` 的 skill 时，Runtime 的 `mcp-serve` 把这些 hook 记在这个进程里，之后这个会话的工具调用都过一遍，直到进程结束。断线重连会起新的 `mcp-serve`，登记就没了——返回里写明，模型要的话再加载一次。

**跑哪些事件**：`PreToolUse`、`PostToolUse`。`Stop`、`SubagentStop`、`UserPromptSubmit`、`SessionStart`、`Notification`、`PreCompact` 这些发生在 Host 里，Runtime 看不到，返回开头列出"在这里不跑"。

**匹配哪些调用**：只有这个 `mcp-serve` 自己服务的工具。`matcher` 按原生规则（精确名、`A|B`、正则、空或 `*` 是全部）同时对两种名字比：ccnm 的 MCP 名（`mcp__ccnm__exec_command`），以及它顶替的原生工具名——这样为本地 Claude Code 写的 `matcher: "Bash"` 照样能用：

| ccnm 工具 | 按原生名 | 交给 hook 的 `tool_input` |
| --- | --- | --- |
| `exec_command` | `Bash` | `{"command": 实际执行的 shell 文本, "timeout": 毫秒}` |
| `read_file`、`view_image`、`read_notebook` | `Read` | `{"file_path": Runtime 上的绝对路径}` |
| `apply_patch` | 改已有文件是 `Edit`，新建是 `Write` | 每个被改的文件跑一次，`{"file_path": 绝对路径}` |
| `search_text` | `Grep` | `{"pattern", "path"}` |
| `list_files` | `Glob` | `{"pattern", "path"}` |
| `load_skill` | `Skill` | `{"skill": 名字}` |
| 其余 | 只按 MCP 名 | ccnm 收到的原始参数 |

**怎么跑**：在 Runtime 上、以执行账号、`cwd` 是 workspace 根，环境里多 `CLAUDE_PROJECT_DIR`；和 `exec_command` 走同一套进程组、超时、输出上限，`exec_sandbox` 开着就同样套上。stdin 是原生形状的 JSON（`session_id` 用 ccnm 的会话 id；`transcript_path` 没有，不给）。超时用 hook 自己的 `timeout`，没写是原生的 600 秒。

**结果怎么用**：

| 情况 | `PreToolUse` | `PostToolUse` |
| --- | --- | --- |
| 退出 0，没有 JSON | 照常执行 | 结果不变 |
| 退出 2 | **不执行**，模型收到工具错误：哪个 skill 的 hook 拦的、stderr 原文 | stderr 附在结果后面给模型 |
| JSON `permissionDecision: "deny"` | 不执行，理由同上 | — |
| JSON `"ask"` | 不执行：这个会话里没人可问（第 2 节的门保证了这一点），理由里写明 | — |
| JSON `additionalContext` / `decision: "block"` + `reason` | — | 附在结果后面 |
| JSON `updatedInput` / `updatedMCPToolOutput` | 第一版不认，结果里写明被忽略 | 同左 |
| 其他非零退出、超时 | 照常执行，结果里附一行"hook 失败"和 stderr 开头 | 同左 |

`once: true` 跑成功一次（退出 0）就撤，和原生一样。

**门：只在"这个会话里跑命令本来就不问人"时跑。** 具体是同时满足：

1. 会话是 coding 模式，且 `exec_command` 在这里本来就能跑（同一个 `exec_gate`：身份、`allow_unconfined_exec` 那些）；
2. `exec_command` 在这个会话里不带"要人确认"的标记：`--print`、`ccnm mcp bridge`，或交互会话开了 `allow_unattended_exec`。

不满足时 hook 不登记，`load_skill` 返回开头写明没跑、为什么、开哪个开关能跑。理由：hook 就是一条来自仓库文件（或执行账号 HOME）的命令，跑的时候没人确认。P36 不执行 `` !`命令` `` 正是这个理由——一次"读"不该绕过 `exec_command` 上那一问。反过来，在已经不问的会话里，模型本来就能不经确认跑任何命令，认 hook 不多给它任何东西；**包括模型自己刚用 `apply_patch` 写出来的一个带 hook 的 SKILL.md**，所以不需要原生那种"信任这个文件夹"的额外一步。

**哪些 skill 的 hook 跑**：只有 Runtime 交出去的（项目里的、执行账号 HOME 下的）。Agent 机器上的 skill（`ccnm_agent` 交出去的）的 hook 不跑：那个 server 看不到项目工具调用，而在 Agent 上跑命令——那台机器上有 AI 登录——正是 ccnm 不做的事。返回里写明。

## 3. `allowed-tools`：不逐条放行，由 `allow_unattended_exec` 回答

**为什么做不到原生那样**：ccnm 工具里只有 `exec_command` 和 `call_mcp_tool` 会问人。问不问是 Host 决定的：Claude Code 看 `tools/list` 里这个工具带没带 `anthropic/requiresUserInteraction`，Codex 看启动时给这个工具设的 `approval_mode`。两者都是"这个工具"的属性，不是"这一轮"或"这条命令"的属性，MCP server 没有办法说"这一次别问"。

**考虑过、不建议的做法**：另加一个不带确认标记的工具（比如 `exec_allowed_command`），Runtime 只在命令匹配某个已加载 skill 的 `allowed-tools`（`Bash(git *)`）时才跑。不建议，三个原因：

1. **放行规则写在模型自己能改的文件里。** 模型用 `apply_patch` 写一个 `allowed-tools: Bash(*)` 的 SKILL.md，加载它，就给自己放行了。这等于把只有 Runtime 配置才能关的那道门交给调用方——`allow_unattended_exec` 当初刻意不做命令行参数，就是为了不出现这种事（status.json 里那条产品决定）。要堵上就得给 skill 文件分信任等级（只认 HOME 下的？只认会话开始后没改过的？），每一种都有绕法。
2. **要可靠地匹配命令得有 shell 解析器。** `Bash(git *)` 不能放行 `git status; rm -rf ~`；原生是把复合命令拆开逐段比的。自己写一个等于新造一个"命令解析器当沙箱"，ROADMAP 的原则里明说过命令解析器不是 sandbox。
3. **P76 之后它基本用不上**：文档建议交互会话常用的项目开 `allow_unattended_exec`，开了以后什么都不问，`allowed-tools` 没东西可放行。

**所以建议**：`load_skill` 返回开头按会话情况写一行，不再笼统地说"不起作用"：

- 会话里命令本来就不问：`allowed-tools: nothing to grant, commands in this session already run without asking`；
- 交互会话、会问：`allowed-tools has no effect: this workspace asks before every exec_command and a skill cannot turn that off; the Runtime's allow_unattended_exec does`；
- 列了 Agent 那边的工具（`WebFetch`、`WebSearch`、`Agent` 等）：它们开没开由 Runtime 的 `agent_tools` 定，开了的已经在允许表里，返回里写明哪些开着、哪些这个 workspace 关了。

## 4. `model`：借子代理生效，只在 `context: fork` 时

**为什么不能直接换**：原生是 `Skill` 工具被调用时由 Claude Code 自己换模型。经 MCP 来的 skill，Host 不知道它在生效；MCP 也没有"请 Host 换模型"的消息。Codex 更没有这个概念。

**能做的**：原生里 `context: fork` 的 skill 本来就是"派一个子代理去跑，`model` 是子代理的模型"。Claude Code 的 `Agent` 工具有 `subagent_type` 和按次指定的 `model` 参数，子代理也拿得到 ccnm 的工具（P46 实测子代理工具表和主会话一样）。P77 起子代理默认开。所以：

- **`context: fork`、会话是 Claude、`agent_tools` 里有 `subagents`**：`load_skill` 返回开头写一段指示——这个 skill 要在子代理里跑：调 `Agent`，`subagent_type` 用 `agent` 字段（没写就 `general-purpose`），`model` 用这里的值（`inherit` 或没写就不带），把下面的正文当它的提示，自己不要照着做。这是**给模型的指示，不是强制**：Runtime 看不到模型照没照做。
- **写了 `context: fork` 但没有子代理**（Codex、`subagents` 被关、外部 MCP 客户端不知道是谁）：写明 fork 和 model 都没生效，正文照常给。外部 MCP 客户端那条写成"如果你的客户端能派子代理……"。
- **只写了 `model`、没写 `context: fork`**：写明没生效、会话保持原来的模型。不建议改成"派子代理"：子代理看不到对话历史，那会悄悄改变 skill 的意思。
- `effort`：没有对应参数，写明没生效。

## 5. 顺带：`` !`命令` ``

不是这次问的字段，但门和 hooks 一样：在命令本来就不问的会话里，照原生在加载时先跑、把输出填进正文，失败就整次加载报错（原生如此）；会问的会话里照旧不跑、列出来。建议和 hooks 一起做，复用同一个"在 Runtime 上跑一条短命令"的实现。

## 6. 改了会怎样

- **协议**：[Remote Workspace MCP](../protocol/remote-workspace-mcp-v1.md) 第 5.1 节写着这些字段"不起作用"，hooks 还会让工具调用多出一种被拦的结果、结果末尾多出 hook 的附言。按冻结后的规矩加带日期的说明、加 fixture，跑中立客户端。工具和参数不变。
- **会慢**：一个 `PreToolUse` hook 最长可以让每次工具调用多等 600 秒（原生同样）。
- **断线重连丢登记**：见第 2 节。
- **不影响**：会问的交互会话、`read` 模式的外部会话，行为和今天一样（只是返回开头的说明更具体）。

## 7. 要用户定的

1. **hooks 的门**：建议"命令本来就不问的会话才跑"。另两个选项：任何 coding 会话都跑（交互会话里等于多了一条不经确认就执行的路）；或者干脆不跑。
2. **`allowed-tools`**：建议不逐条放行，只把说明写准。
3. **`` !`命令` `` 要不要一起做**：建议一起做。
4. **`model` / `context: fork`**：建议用"告诉模型派子代理"的办法；另一个选项是维持"不起作用"。
