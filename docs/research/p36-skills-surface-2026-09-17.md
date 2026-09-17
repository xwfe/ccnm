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

## 2. 做了什么（P36.2–P36.6）

- **共享库 `toexec-skill` 0.1.0**（toexec 仓库 `6377cbf`，tag `toexec-skill-v0.1.0`）：零依赖，三块纯机制——frontmatter 读取（YAML 的一个子集：块状键值、缩进嵌套、列表、`|` / `>` 块标量、可跨行的引号串和 `[…]`）、参数替换、`` !`命令` `` 识别。解析器在一台开发机的真实语料上跑过：941 个 SKILL.md / 命令 / agent 文件，810 个带 frontmatter 的 skill 和命令全部读得出来，读不了的 2 个是 YAML 本身就不合法的 agent 定义。跨行引号串和跨行 `[…]` 就是这么验出来要支持的（Anthropic 官方插件里有）。
- **参数替换对的是 Claude Code 2.1.273 的实际行为，不只是文档**（打包代码里那个函数读出来的）：`$N` 和 `$ARGUMENTS[N]` 没给到就原样留着、声明过的 `$name` 没给到是空串、`\$` 转义、一个占位符都没换成才在末尾补 `ARGUMENTS: …`、`$ARGUMENTS` 是不带词边界的全量替换。
- **`crates/ccnm-core/src/mcp/skills.rs`**：发现、目录、加载。规则写在[协议第 5.1 节](../protocol/remote-workspace-mcp-v1.md#51-load_skill-与-prompts项目自带的-skillsp36-新增)，这里不重复。
- **server 接线**：第八个工具 `load_skill`；`tools/list` 时把它的 description 换成这个 workspace 的目录（`#[tool]` 属性求值时没有 `self`）；`prompts/list`、`prompts/get` 和 `prompts` 能力；会话放行清单 `MCP_TOOLS` 多一项，所以 Claude 的 settings allow-list 和 Codex 的 `enabled_tools` 都带上了它。
- **握手文本不再点名 `SKILL.md`**，名额还给嵌套的 `CLAUDE.md` 和 `.claude/rules/`。

## 3. 和立项时写的不一样的地方

- **P36.6 原来写"标记行里加一段说明有几个 skill、用哪个工具看"，没做。**目录已经在 `load_skill` 的 description 里，而工具 description 本来就一直在模型面前；再在 `instructions` 里说一遍，是从装 `CLAUDE.md` 的那 2048 码元里拿预算去重复一件事。唯一有差别的场景是外部 Claude Code 把 ccnm 的工具延迟加载——那种情况下协议文档早就建议配 `alwaysLoad`。ROADMAP 的 P36.6 已按这个结论改写。
- **prompt 的参数会丢词。**Claude Code 把人敲的参数按空白切开、依次对应 prompt 声明的参数，多出来的词被它丢掉（2.1.273 打包代码）。skill 没声明 `arguments` 时只有一个参数位，`/mcp__ccnm__deploy staging now` 里的 `now` 到不了 server。这是 Host 的行为，server 这边补不回来；写进了协议文档。
- **共享库的 tag 还只在本地。**推送要用户批准，所以 ccnm 的 `Cargo.lock` 是让 cargo 从本地 toexec 仓库解析那个 GitHub 地址得到的（一次性环境变量 `GIT_CONFIG_*` 的 `url.<本地>.insteadOf`，不改任何配置文件）。锁文件里记的提交号 `6377cbf…` 和推送之后完全一致。**推送顺序必须先 toexec 后 ccnm。**

## 4. 验证与门禁（P36.7、P36.8）

macOS 26.6.2 arm64，rustc 1.98.0。

| 什么 | 结果 |
| --- | --- |
| `toexec-skill` 单元测试 | 36 passed（frontmatter 20、参数 12、注入 4） |
| `mcp::skills` 单元测试 | 10 passed：三个位置都找得到、skill 赢同名命令且输家有说明、读不了的说原因、经 symlink 出 workspace 的被拒、目录 60 个长描述 skill 时仍不超过 2048 码元且每个都至少有名字、加载时填参数且不执行注入命令、只给人用的 skill 拒绝模型但对 prompt 开放、不存在的名字列出现有的、长正文截在行边界并给出续读行号、会话中途新写的 skill 能加载 |
| 中立 MCP 客户端（不 import ccnm 代码，对真实二进制） | 新增 5 条：目录在 description 里且不超 2048 码元、加载后工作区里**没有**出现注入命令要建的文件、不带名字返回含失败原因的列表、不存在的名字是 `CCNM_E_INVALID_ARGS`、prompts 列出/获取/拒绝 `user-invocable: false` |
| 已发布工具表逐字节比对（`published_tool_tables_match_the_running_server`） | 通过。两份 `tools-list-*.json` 各手工加了一项——契约新增，不是重录 |
| golden 快照（`provider_compat`） | 没有重录；放行清单多出的一项记为有据可查的差异 |
| `cargo test --workspace` | 822 passed / 0 failed；跑完后 `$TMPDIR` 与 `/tmp` 新增残留 0（P35 的量法） |
| Python 全部测试 | 173 passed |
| 其余门禁 | `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo +1.89 check --workspace --all-targets --locked`、`check_plan.py`、`check_protocol.py`（38 + 24 个 fixture）、`git diff --check` 全过 |

## 5. 没验的，和接下来

- **真实模型会不会因为目录里的描述而主动加载 skill**：没验，没花模型额度。留给 v3 方案最后的对照实验。
- 没上真机；Claude Code 里 `/mcp__ccnm__<名字>` 没有人工点过；Codex 在 Code Mode 下 `load_skill` 的 description 有没有完整到模型面前没有单独量（本轮 Codex 探针用的是 `-m gpt-5.1-codex`）。
- **下一个阶段**：MCP 官方 skills 扩展 SEP-2640（`capabilities.extensions["io.modelcontextprotocol/skills"]`、`skills/list`、`skills/get`、`skill://` 资源、可选的 `resources/directory/read`）。要加 SHA-256 依赖；规范要求 frontmatter "原样渲染成 JSON"，现在的子集读取器够不够要对着规范再看。Claude Code 的开关打开之前它不改变任何用户可见的行为，所以优先级排在 v3 方案的执行面补齐之后也说得通——由用户定。
- **gld**：换用 `toexec-skill`（修多行 description）、compact 档放回 skills、hub 白名单加入 `load_skill`，是 gld 自己的阶段。
