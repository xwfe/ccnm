# P87：受管 Claude 会话的自动压缩提前到 30 万，并带上压缩指令（2026-10-11）

## 1. 起因：一次 xdo 会话花在哪

用户 10-10 18:44 的 xdo 受管会话（Agent 是 fodelf，Claude Code 2.1.296，Opus 5.5，4 小时 24 分），Claude 自己的会话记录在 fodelf 上，逐条算：

| 项 | 数 |
| --- | --- |
| 请求 | 主会话 503 次，另有 1 个子代理 37 次 |
| context | 从 2.2 万起步，涨到 96.9 万才第一次自动压缩（压到 2.2 万），之后又涨到 29 万 |
| 按 API 价折算 | 约 $69（Opus 5.5：输入 $4、输出 $20、缓存读 $0.20、1 小时缓存写 $8，每百万 token）；子代理另 $0.9 |
| 钱花在哪 | 缓存读 2.41 亿 token = $48（70%）；输出 57 万 = $11（16%）；缓存写 122 万 = $10（14%） |

每次请求都要把整段 context 从缓存再读一遍。503 次里 396 次 context 超过 20 万，这 396 次的缓存读就是 $45。按记录里每一步的增量粗算：窗口是 20 万时缓存读约 $11–15、40 万时约 $20–25（压缩之后模型会回头重读文件，这部分算不出来，所以只是粗算）。

压缩前那段 context 的成分（用每步增量对"上一步输出 token、这一步收到的工具结果字符数"回归估的）：约一半是模型自己的输出（含看不见的思考，约 29 万 token），约一半是工具结果——`exec_command` 结果占字符数 38%、`read_file` 9%。开场 4 分钟读交接和状态文档就用了约 11 万 token；中文约 1.3 token 一个字。

## 2. 为什么受管会话更依赖整段压缩

读本机 Claude Code 2.1.286 打包代码（`strings` 导出后按变量名追，没跑实验）：

- 两套把旧工具结果换成 `[Old tool result content cleared]` 的机制（客户端 keep-recent，和闲置约 65 分钟后的服务端 `clear_tool_uses_20250919`）共用一个写死的名单：Read、Bash、PowerShell、Grep、Glob、WebSearch、WebFetch、Edit、Write。名单外的工具名，包括所有 `mcp__ccnm__*`，被放进 `exclude_tools`。名字映射表只有 `Task→Agent` 这类旧名。
- 原生 Read 读没变的文件只回一句 "File unchanged since last read"，MCP 工具没有。
- Claude Code 认的 `anthropic/*` 工具 `_meta` 里没有"可清理"这一项。

所以 ccnm 会话里每一次 `read_file`、`exec_command` 的结果都原样留着，直到整段压缩。

## 3. 能调的两样东西（2.1.296 打包代码）

- **压缩窗口**：解析顺序是环境变量 `CLAUDE_CODE_AUTO_COMPACT_WINDOW`（取值 10 万到 100 万）→ 设置项 `autoCompactWindow` → 服务端下发 → 实验 → 模型默认。环境变量排第一。
- **压缩指令**：总结提示末尾写着"附加的总结指令可能在上下文里给出"（例子是 `## Compact Instructions` 一节）；另外 `PreCompact` 钩子"退出码 0 时 stdout 作为附加压缩指令"，进总结提示的 `Additional Instructions:`，自动和手动压缩都走。2.1.296 的压缩不另发请求：把总结指令塞进下一次主请求，压完的下一次请求以 "This session is being continued from a previous conversation" 开头。
- `/recap` 是"一句话会话回顾"，离开一阵回来时也有类似的摘要。压缩指令第一条照它的样子要一句"在做什么、做到哪"。

## 4. 实测（零额度）

夹具 [`probes/p87-auto-compact.py`](probes/p87-auto-compact.py)：真实 Claude Code 在单独的 tmux server 里交互式运行，接本机假模型（假 key、空 HOME、`sandbox-exec` 只放本机出站），启动参数照 ccnm 的 `launch_cmd`，cwd 是一个当作 ccnm 工作目录的临时目录。连发三轮消息，假模型每次回复都在用量里报 N 个 token，看请求里有没有出现总结指令、下一次请求是否以"从上一段对话继续"开头。会话设置按 `policy.rs` 拼：压缩指令从源码里的常量读，形状与单元测试锁定的一致。

| 场景 | 2.1.286（本机） | 2.1.296（fodelf） |
| --- | --- | --- |
| 旧设置（只有允许表），报 30 万 | 不压缩 | 不压缩 |
| ccnm 新设置，报 29 万 | 第 3 轮压缩；压缩请求里有 `Additional Instructions:` 和 ccnm 指令全文 | 同左 |
| ccnm 新设置，报 23 万 | 不压缩 | 不压缩 |
| ccnm 新设置 + workspace 目录 `.claude/settings.local.json` 写 `CLAUDE_CODE_AUTO_COMPACT_WINDOW=200000`，报 19 万 | 压缩（环境变量盖过设置项），也带 ccnm 指令 | 同左 |
| `-p`：允许表 + 一个不认识的键，假模型调一次 MCP 工具 | 工具照常执行（设置文件没被整份丢掉） | 同左 |
| `-p`：ccnm 新设置，同上 | 工具照常执行 | 同左 |

此前（用户要求在 fodelf 配 20 万时）用同样办法量过：窗口 20 万时 18 万压、15 万不压；那批 workspace 目录的 `settings.local.json` 现在仍在 fodelf 上，所以那四个项目实际是 20 万，删掉才回到 30 万。

测法上踩过的坑：

- **`-p` 测不出主动压缩。** 30 万到 90 万都不压，只在约 98 万快撞上限时压一次（被动压缩），所以压缩相关的都在交互式里测，`-p` 只测"设置文件还能用"。
- 2.1.296 不另发总结请求：总结指令（`Your task is to create a detailed summary of the conversation so far`）就在下一次主请求里，假模型那次回什么都会被当成总结。
- 第一次报大用量的那一轮不触发，第 3 轮才压：压缩检查用的是上一次回复的用量，加上 2.1.296 先在后台准备。实际会话里请求多，差这一两轮无所谓。
- tmux socket 路径要短（`/tmp/p87-<8 位>.sock`），scratchpad 下会超 104 字节。

没测：真实模型按这段指令写出来的总结质量（要花额度）；Codex 会话（本阶段不改）。

## 5. 实现

`crates/ccnm-core/src/provider/claude/policy.rs`：远端会话的 settings.json 多两个键——`autoCompactWindow: 300000`（`AUTO_COMPACT_WINDOW`），和 `hooks.PreCompact` 里一个不带 matcher 的 command 钩子 `printf '%s\n' '<COMPACT_INSTRUCTIONS>'`。钩子跑在 Agent 机器上（有登录的那台），所以只输出 ccnm 写死的文字，不读 stdin、项目或模型给的任何东西；`printf` 是 shell 内建，不依赖 PATH；指令里不能有单引号，单元测试经 `/bin/sh -c` 真跑一遍钩子并逐字比对输出。同地会话（目前启动前就拒绝）不变。

测试：`settings_compact_early_and_say_what_to_keep` 在旧代码上红（`autoCompactWindow` 是 Null）；`session::tests::settings_allow_exactly_the_ccnm_tools_and_deny_the_native_ones` 原来断言设置里只有 `permissions` 一个键，本意是"不改变你在别处用 Claude 的行为"——这个文件只作用于本会话，本意不变，改成恰好三个键；`provider_compat` 的 baseline 比对按惯例把这处有意改动加到期望值上，fixture 没重录。

`--settings` 的优先级高于用户自己的设置文件，所以你在 `~/.claude/settings.json` 里写的 `autoCompactWindow` 在受管会话里不生效；环境变量优先级最高，改值走它，写法见[配置说明](../configuration.md#受管-claude-会话的自动压缩)。
