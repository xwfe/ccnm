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

## 4. 实测

（P87.1 完成后补。）
