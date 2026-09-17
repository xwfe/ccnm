# Runtime 上项目自带的 skills（P36，2026-09-17）

验收见 ROADMAP 的 P36；整条"工具面对齐原生能力"的线见 toexec 仓库的 [v3 方案](https://github.com/xwfe/toexec/blob/main/docs/plan/implementation-plan-v3-native-parity.md)。

## 1. 实测：skills 能经哪条路到模型（P36.1）

脚本、每次运行的汇总和静态证据的原文在 toexec 的 [`evidence/v3-parity/skills-surface/`](https://github.com/xwfe/toexec/blob/main/evidence/v3-parity/skills-surface/README.md)。全程零模型额度：Codex 接本机假模型，Claude Code 用未登录的 CLI（`input_tokens: 0`）加打包代码的静态证据。macOS arm64。

| | Claude Code 2.1.273 | Codex 0.154.0 |
| --- | --- | --- |
| 连上之后调的 MCP 方法 | `tools/list`、`prompts/list`、`resources/list` | **只有** `tools/list` |
| 工具 description | 每个工具单独截到 **2048 个 UTF-16 码元**，后面接 `… [truncated]`——和 `instructions` 是同一个函数、同一个上限 | **不截**：6000 字符原样进了发给模型的请求 |
| MCP prompt | 变成斜杠命令 `/mcp__<server>__<名字>`（界面显示 `<server>:<名字> (MCP)`）；用户敲的参数按空白切开，依次对应 prompt 声明的 `arguments` | 看不到 |
| MCP 官方 skills 扩展（SEP-2640，2026-09-13 定稿） | 客户端代码已经在 CLI 里，但挂在特性开关 `tengu_mcp_skills`（默认 `false`）后面；实测**没有**调 `skills/list` | 看不到 |

**对设计的影响：**

1. **目录放进工具 description，不放 `instructions`。** `instructions` 的 2048 码元要装 `CLAUDE.md`；工具 description 有自己独立的 2048。Codex 不截，但目录仍按 2048 做预算——同一个 workspace 三种客户端都会来连，目录不该因客户端而异。
2. **prompts 只对 Claude Code 有用**，但对它确实有用：用户能敲 `/mcp__ccnm__deploy staging`。做。
3. **Codex 只有工具这一条路。** 它连 `prompts/list` 都不调，所以"模型自己发现并加载 skill"必须是一个普通工具能完成的事。
4. **标准扩展是下一个阶段，不在 P36 里。** 它现在对哪个客户端都不生效（Claude Code 的开关默认关，Codex 不支持），但它是 MCP 官方标准、Claude Code 的客户端已经写好——开关打开那天，实现了扩展的 server 上的 skill 会以 `ccnm:<skill>` 出现在 CLI **原生**的 Skill 机制里，比任何自造工具都接近"原生能力"。它要求给每个文件报 SHA-256，ccnm 现在的依赖树里没有，要加依赖，所以单独立阶段。
5. **Claude Code 对 MCP 来的 skill 自己就不认 `hooks` 和 `allowed-tools`**（打包代码里的日志原话：`MCP-sourced skills cannot register hooks` / `cannot bypass permissions`），SEP-2640 的安全一节也要求 Host 这么做。P36 忽略这两个字段，和官方客户端、官方规范是同一个方向。

**没验的**：截断后的 description 确实进了 Claude 的模型上下文——没登录，没有模型请求可看；截断发生在客户端、发请求之前，由静态证据支持。模型会不会主动去用 skill，整个 P36 都不验（不耗额度），留给 v3 方案最后的对照实验。
