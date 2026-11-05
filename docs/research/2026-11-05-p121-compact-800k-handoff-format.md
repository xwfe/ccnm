# P121：压缩窗口改 80 万；交接写成固定格式；提醒做得醒目（2026-11-05）

用户 2026-11-05 要求：默认到 80 万再自动压缩；交接要求设计得合理优雅；交接提醒要醒目点。这一轮全是本机零额度实测，没跑真实模型。

## 压缩窗口 80 万

`crates/ccnm-core/src/provider/claude/policy.rs` 的 `AUTO_COMPACT_WINDOW` 从 560 000 改成 800 000。窗口越大，压缩得越少，每次丢的细节越少；代价是 context 大的时候每次请求读缓存更贵（P87 的账：拖到 96.9 万才压，缓存读占了会话费用的七成）。

用 `docs/research/probes/p87-auto-compact.py` 实测（Claude Code 2.1.318 接假模型，探针直接读源码里的窗口值）：

| 场景 | 上报的 context | 压没压 |
| --- | --- | --- |
| ccnm 的会话设置 | 790 000 | 压了；压缩请求里带着 ccnm 压缩指令的全文，压完那条消息指向转录 |
| ccnm 的会话设置 | 730 000 | 没压 |
| 不带 `autoCompactWindow` 的旧设置 | 790 000 | 没压（按模型自己的 1M） |
| 环境变量 `CLAUDE_CODE_AUTO_COMPACT_WINDOW=200000` | 190 000 | 压了：环境变量仍然优先 |

`--print` 下设置文件多一个键，允许表照常生效（工具照跑）。

## 交接写成固定格式

以前的说明是一长句："目标；做完的；没做完或没验证的；检查命令；用户定的要求和边界；下一步"。模型每次写的结构都不一样，读的人和下一段对话都得自己找。改成固定的七个字段，前五个和压缩总结开头的 Status 块（`COMPACT_INSTRUCTIONS` 第 1 条）用同一套说法，后两个是新对话自己查不到的东西：

```text
[ccnm handoff]
Goal: the task, in one sentence.
Done: what changed, by workspace-relative path; commits (short hash and subject); what is still uncommitted.
In progress: what you were doing and how far it got.
Blocked: what waits on the user or on something else, and what would unblock it.
Check: the exact commands that verify the work, and what still fails (test names, the decisive error line).
Keep: the user's decisions and limits that still hold, close to their words.
Next: the first step, as a command or tool call.
```

写法规则：每项单独一行，列表一项一行；只写事实，路径和命令照抄；不贴代码和长输出（文件就在项目里）；没有的写 none；不超过 40 行；用用户说话的语言写。仍然只在人要的时候写（P120）。

## 提醒做得醒目

先量 Claude Code 2.1.318 怎么画钩子的 `systemMessage`（`capture-pane -e` 抓带颜色码的屏）：

- 默认是灰色：`\x1b[38;5;246m  ⎿  SessionStart:startup says: …\x1b[39m`。这就是"不醒目"的原因。
- 消息里带的 ANSI 颜色码原样生效：`\x1b[1;33m…\x1b[0m` 画成加粗黄色。
- 多行也行，续行缩进，同样是灰色。行首放符号也能显示。

做法（`crates/ccnm-core/src/session/hook.rs` 的 `person_line`）：每一行以一个彩色的 `ccnm` 标签开头，接着是要紧的那句，加粗上色；补充说明另起一行，留在 Claude Code 自己的灰色里。压缩够了次数（要你决定）用黄色；交接已记下、已交给新对话用绿色。只用 16 色的码。

```text
  ⎿  SessionStart:compact says:  ccnm  这段对话已压缩 3 次，建议敲 /clear 开新对话
     早期细节只剩摘要。/clear 会把这段对话的结尾带过去；要完整交接，先说“写交接”
```

## 验证

本机 macOS arm64：

- 先红后绿：窗口测试的期望值先改成 800 000，在旧值上红，改了常量后绿；`the_handoff_has_one_shape`（七个字段都在、40 行、只在人要时写）放到改动前的代码（`822225c`）上是红的；`lines_for_the_person_stand_out` 用到新函数，旧代码上编译不过。
- 零额度端到端（`docs/research/probes/p120-hook-notices.py --ccnm target/debug/ccnm`，真 Claude Code 2.1.318、真 ccnm 钩子、假模型）：第 1、2 次压缩没有提醒；第 3 次之后屏上是黄色标签那两行；没写交接时 `/clear` 后是绿色标签，第一条请求带着上一段的结尾；写下交接后是绿色"交接已记下"，`/clear` 后交接进了第一条请求。
- `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、Windows 目标的两条 clippy、`cargo +1.89 check --locked` 都干净；`cargo test --workspace --no-fail-fast` 1217 过、0 挂；`python3 -B scripts/ci_gates.py` 通过（Python 249 条、0 跳过）。
- 没做：真实模型照新格式写交接写得怎么样；80 万窗口下真实会话的花费；AI 机器上的真会话。
