# 同类工具调研（2026-09-04）

**为什么有这份文件**：ccnm 的很多设计当初是推理出来的，不是比着别人做的。这轮把外面
的东西扫了一遍，结论是**大部分推理站得住，而且现在有别人的实测数字撑着**。数字比论证
有用——README 和设计文档里该硬气的地方可以引这里。

**读法**：每条都带出处。凡是我核过 ccnm 源码的，写了核的结果；没核的写"未核"。

---

## 1. 有没有人做了同样的事

**没有。** 单条 SSH stdio + 项目根约束的少量工具 + deny 掉原生工具 + `--strict-mcp-config`
+ CLAUDE.md 投影，这个组合没找到第二家。最接近的两个：

**Zed remote development**（[blog](https://zed.dev/blog/remote-development) ·
[docs](https://github.com/zed-industries/zed/blob/main/docs/src/remote-development.md)）
——架构同构：本地 UI，远端 headless `zed-remote-server`，中间一条 SSH。三处不同：

1. **远端 server 是 daemon**，"连接断掉时远端 server 继续跑，重连后 language server
   仍是完全初始化好的"。ccnm 的 MCP server 是 ssh stdio 的子进程，ssh 一断就没了。
2. 用 ControlMaster 复用多条流；ccnm 锁定不用（第 7 节），单流够用，多流会变成成本。
3. 远端二进制按版本号存放，不匹配**自动下载**，离线可从本地推。

**[54yyyu/code-mcp](https://github.com/54yyyu/code-mcp/)** —— MCP over SSH 里最接近的。
自动装远端、起 tunnel。缺三样：没禁原生工具（模型照样能摸本机）、没有 root 约束、
走 tunnel 不是 stdio。

其余（[mcp-ssh-manager](https://github.com/bvisible/mcp-ssh-manager) 37 个工具、
[remote-shell-mcp](https://github.com/jaenster/remote-shell-mcp)、
[AiondaDotCom/mcp-ssh](https://github.com/aiondadotcom/mcp-ssh)）都是面向"管一堆服务器"
而不是"改一个项目"。官方
[server-filesystem](https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem)
只能本地，但它的 Roots 语义值得看：客户端给了 Roots 就**完全替换**服务端的 allowed dirs。

**云沙箱那一派目标相反**：他们把代码搬到云（Claude Code on the web、Cursor background
agents、OpenHands），ccnm 把 agent 留在本地。[OpenHands
runtime](https://docs.openhands.dev/openhands/usage/architecture/runtime) 跟 ccnm 是同一个
骨架（沙箱里跑 action server，后端 send Action / receive Observation），只是传输选了
容器内 HTTP。

最值得读的一篇：[Anthropic 工程师为什么用 Coder 远程跑 Claude
Code](https://coder.com/blog/building-for-2026-why-anthropic-engineers-are-running-claude-code-remotely-with-c)
——第三条理由（agent 同时握着完整凭证和外网出口是外泄风险）正是 ccnm 的立论。

---

## 2. 工具数量：数字全站在"少"这边

```
聚焦工具集 → 完整 GitHub MCP server   工具选择准确率  ~95% → ~71%
10 个工具 → 100 个工具                三个模型一致下降约 10%
GitHub + Playwright + IDE 同挂        光 schema 吃掉 200k 里的 143k（72%）
93 个工具的 GitHub MCP server         注入 55,000 token
200 个工具静态加载                    261,700 token，超 Claude 200k 窗口
```

出处：[getunblocked](https://getunblocked.com/blog/mcp-tool-overload/) ·
[arXiv:2508.12566](https://arxiv.org/pdf/2508.12566) ·
[MCP spec #2808](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/2808)。
厂商硬封顶：Cursor **40**，OpenAI 128，Claude 约 120。机理是 Stanford 的
"lost in the middle"：相关信息落在长 context 中段时性能掉 20%+。

Anthropic 官方[《Writing effective tools for
agents》](https://www.anthropic.com/engineering/writing-tools-for-agents)：**"更多工具不一定
带来更好结果"**，建议"构建少量深思熟虑的工具"。同文给出 Claude Code 默认工具响应上限
**25,000 token**。

**所以 ccnm 的 7 个工具 / 16 KiB 预算不是保守，是这个领域里少见的克制。**

**最小集有人文档化了**：[pi](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/) 用
4 个——`read` / `write` / `edit` / `bash`，"结果证明这四个就是一个高效编码 agent 的全部
所需"，系统提示 + 工具定义 **1000 token 以下**。它**故意砍掉 todo 和 plan mode**：
"待办列表通常让模型更困惑而不是更有帮助。它们增加了模型必须跟踪和更新的状态，也就增加
了出错的机会。"

反过来"只给 bash 就够"那派被 SWE-agent 的消融否掉了：定制工具集比裸 shell **多解
10.7 个百分点**（300 个 SWE-bench 实例，[arXiv:2405.15793](https://arxiv.org/pdf/2405.15793)），
收益最大的两个部件是**带 lint 的 edit（+3.0pt）**和 **100 行窗口的文件查看器**
（ccnm 的 `read_file` 一次 2000 行，比这个大 20 倍——未验证是否有影响）。

---

## 3. 编辑安全：事务是全行业空白

**各家的落地方式：**

| 工具 | 格式 | 匹配策略 |
|---|---|---|
| aider | `whole` / `diff`(SEARCH/REPLACE) / `udiff` | 宽容解析，容忍标记漂移 |
| Codex CLI | V4A（`*** Begin Patch` 信封） | 三级递降：精确 → 忽略行尾 → 忽略所有空白 |
| Claude Code | 精确字符串替换 | 必须唯一命中，否则要 `replace_all` |
| Cline | SEARCH/REPLACE | **order-invariant**，按模型切格式 |
| Desktop Commander | `edit_block` | 精确失败后 fuzzy fallback，近似命中写日志 |
| pi | 单个 `edit` | 完全不归一化 |
| **ccnm** | **精确替换 + 唯一 + CRLF 归一化 + 强制 version** | **order-invariant，退回顺序；失败时诊断不修**（本轮改的） |

**已发表的失败率**（[aider unified-diffs](https://aider.chat/docs/unified-diffs.html)，
89 个重构任务）：

```
GPT-4 Turbo   SEARCH/REPLACE 20%  →  unified diff 61%   偷懒占位注释 12 → 4
GPT-4-0613    26% → 59%
消融：关掉 flexible patching       编辑错误 ×9
消融：去掉 "high level diff" 提示   编辑错误 +30–50%
```

**注意别误用这组数**：它说的是 unified diff 里模型自己生成的上下文行会漂移。ccnm 是
"把刚读到的原文抄回来做精确替换"，而且 `version` 强制模型必须真读过。两种情况不同，
×9 那个数**不能**直接推出"ccnm 该放松匹配"。

[Cline 的 diff 改进](https://cline.bot/blog/improving-diff-edits-by-10)：成功率平均
**+10%**，Claude 3.5 Sonnet **+近 25%**、GPT-4.1 系 +21%、Opus 4 +近 15%。关键发现：

> 很多 LLM **尽管被明确提示要按正确顺序产出 diff，仍然经常乱序返回。**

**事务 / 回滚：没有人做。**

- [V4A 规范](https://codex.danielvaughan.com/2026/03/31/codex-cli-apply-patch-v4a-diff-format/)
  明确把"全有全无 vs 每文件成败"推给 harness 决定，规范层不保证。
- [aider#462](https://github.com/paul-gauthier/aider/issues/462) 是真实车祸：一个 hunk
  失败后成功的照样落盘、失败的跳过，重试导致**已成功的编辑被重复追加多次**；`/undo`
  报 "The repository has uncommitted changes in files that were modified in the last
  commit."，救不回来。

**ccnm 的三阶段 plan/stage/commit + rollback 在这件事上是领先的，不是追平的。**

**[claude-code#32658](https://github.com/anthropics/claude-code/issues/32658) "blind
edits" 的三种静默失败，对照 ccnm：**

```
① 没匹配上                     ccnm 拒绝并报错，不静默
② 匹配到错的块                  ccnm 要求唯一命中，>1 处直接拒
③ 多处编辑部分成功但报告全部完成   ccnm 在内存里改完整个文件再一次原子写，
                               文件内不可能部分成功
```

原文那句值得记：在 ~200 万行的库里"本意改一个函数的替换会静默命中另一个。**没有回读
验证**，这些误改变成潜伏 bug，往往在完全另一个 session 里才浮出来"。

**两个极端都不行**：零归一化栽在 LF/CRLF、多余空格、缩进漂移（Cline
[#1195](https://github.com/cline/cline/issues/1195) /
[#1511](https://github.com/cline/cline/issues/1511)「cline unusable」等一串，加
[claude-code#13456](https://github.com/anthropics/claude-code/issues/13456) 的 CRLF）；
**静默 fuzzy 更危险**——匹配到错的块比匹配不上难查得多。**要 fuzzy 就必须记录并告知。**

---

## 4. 权限模型：弹窗从来没在拦人

Anthropic 官方[《How we contain
Claude》](https://www.anthropic.com/engineering/how-we-contain-claude)：

> **"用户批准了大约 93% 的权限弹窗。"**
> **"用户看到的批准越多，对每一个的注意力就越少，随时间推移监督会变得远不如从前尽职。"**

2026 年 2 月红队演练：员工收到要求外泄 AWS 凭证的钓鱼提示，**Claude 在 25 次重试里完成
了 24 次**。模型层测不出来——"当指令是用户自己敲进去的，分类器眼里没有任何异常"。

官方原则：**"先在环境层设计围栏，再在模型层引导行为。"** 这正是 ccnm 的做法（服务端
deny 原生工具 + root 强制 + `--strict-mcp-config`）。

**社区现象记录**：`--dangerously-skip-permissions`（"YOLO mode"）关掉全部弹窗。主流做法
是**只在容器里开**——[Trail of Bits 的
devcontainer](https://github.com/trailofbits/claude-code-devcontainer)、[Docker 官方专文
](https://www.docker.com/blog/what-is-yolo-mode/) 都给这个命令。没有使用比例的调查数据。
出的问题是没沙箱兜底时改到系统文件，以及"一百个开发者各自决定什么时候跳过权限"的
ungoverned autonomy。**ccnm 不靠弹窗，所以不受这条影响。**

### 三个客户端机制（[官方 MCP 文档](https://code.claude.com/docs/en/mcp)）

1. **`_meta: { "anthropic/requiresUserInteraction": true }`** —— 标了的工具**每次调用都要
   用户批准，`auto` 和 `bypassPermissions` 模式下都不例外**。**唯一一个 YOLO 也绕不过的
   闸**，天生适合挂在 `exec_command` 上。
2. **`MAX_MCP_OUTPUT_TOKENS` 默认 25,000**，超 10,000 出警告；单工具可用
   `_meta["anthropic/maxResultSizeChars"]` 提到 500,000 字符。
3. **stdio idle timeout 默认 30 分钟**（`CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT`，0 关闭）。
   `--strict-mcp-config` 跳过项目级 server 审批需要 **v2.1.246+**。

### EscapeRoute：两个 CVE，ccnm 都不中（已核源码）

[CVE-2025-53109 / 53110](https://cymulate.com/blog/cve-2025-53109-53110-escaperoute-anthropic/)，
打的是官方 filesystem MCP server：

- **53110**：包含性写成**字符串前缀**，允许 `/tmp/allow_dir` 就等于允许
  `/tmp/allow_dir_sensitive_credentials`。ccnm 的 `contained()` 比的是两个 `Path`，
  `Path::starts_with` **按分量匹配**，天然免疫。**但这是类型的性质不是代码写着的**，
  谁"顺手简化"成字符串就把 CVE 请回来了——所以补了回归测试
  `a_sibling_directory_sharing_the_roots_name_is_not_inside_it`。
- **53109**：symlink 校验失败时回退检查**符号链接自己的父目录**。ccnm 那个向上找祖先的
  循环**不是降级检查**——它向上走只因为目标可能还不存在，落到哪层都要 canonicalize
  后过 `contained()`；再加"目标位置是 symlink 一律拒"。两道都能挡。

**剩下的是 TOCTOU**：canonicalize 到 open/rename 之间目标可被换成 symlink。利用它需要先
有项目目录的写权限——人已经在里面了。**判断：不值得为它改代码。**

---

## 5. 僵尸会话不是 ccnm 的 bug，是全生态的

[官方文档](https://code.claude.com/docs/en/mcp)明写：**"Stdio servers are NOT
automatically reconnected."** 远程 server 有指数退避（最多 5 次），stdio 没有。

[claude-code#43177](https://github.com/anthropics/claude-code/issues/43177) 有人挖出源码：

```typescript
// Skip stdio (local process) and sdk (internal) - they don't support reconnection
if (configType !== 'stdio' && configType !== 'sdk') { /* 重连 */ }
else { updateServer({ ...client, type: 'failed' }) }
```

并论证这条注释是**错的**——`reconnectMcpServerImpl()` 对所有 transport 都能用，stdio
重连不过是重新 spawn 子进程。**closed as not planned。** 配套要求加
`claude mcp reconnect` 的 [#57207](https://github.com/anthropics/claude-code/issues/57207)
也没落地。Codex、Continue、opencode、VS Code 全有同样的 issue。

**结论**：ccnm 现在报 `TOOLS DOWN` 并让用户去 `/mcp` → Reconnect，在客户端不改的前提下
已经是上限。根治只有 Zed 那条路——远端做成 daemon，ssh 断了不死，重连接管。

---

## 6. 被否掉的路线（ccnm 已经绕开的）

1. **网络挂载当远程编码方案**：[sshfs#300](https://github.com/libfuse/sshfs/issues/300)
   ——SFTP 无 pipelining，每请求一次 SSH 往返，高延迟链路上比 rclone mount **慢 100 倍**。
   **2026-09-03 从 SMB Hybrid 转出来，现在有外部证据。**
2. **VS Code Remote-SSH 里跑 Claude Code**：
   [#20286](https://github.com/anthropics/claude-code/issues/20286)，RTT 500ms 时**权限
   对话框要 30 多分钟才弹**。根因是 `extension.js` 把消息队列串行化，每条消息等一个往返
   才发下一条。**closed as not planned。**
3. **远端装 Claude Code + tmux**：
   [#49136](https://github.com/anthropics/claude-code/issues/49136) ——OAuth 凭证必须落在
   远端文件系统，共享机器上有 root 的人都能拿。**ccnm 把登录留在工作机正是绕开这个，
   这是相对这条路线最实的优势。**
4. **双向同步（mutagen / unison / Syncthing）**：mutagen 用递归 watch 而不是全盘 rescan，
   是这几个里对开发场景调得最好的。代价是两份副本、冲突处理，以及"改完之后哪边是真的"
   这个问题永远在。
5. **一个 server 塞几十个工具**：见第 2 节的数字。

---

## 7. 待办候选（本轮结论，尚未实施）

按"收益 ÷ 成本"排：

### 已做（2026-09-04 当天）

| | 事 | 结果 |
|---|---|---|
| 1 | `exec_command` 挂 `requiresUserInteraction` | ✅ 每次调用都问，**任何权限模式下都关不掉** |
| 2 | `apply_edits` 做成 order-invariant | ✅ 两遍：先按原文定位，不行才退回顺序应用 |
| 3 | 匹配失败时诊断（不修） | ✅ 空白/缩进差异报到行，打出文件真正的字节 |
| 4 | `start()` 加版本 + root 握手 | ✅ 两条路径都做；真机实测 **430–490 ms**（一次完整 SSH 握手，不是复用连接上的往返） |
| 6 | commit 中途被 kill 的 journal | ✅ 撞见就拒绝并列出文件；**不自动回滚** |

第 2 条有个**要记住的更正**：一开始以为它能修"两个无关的 edit 顺序反了"，**那是错的**——
无关的 edit 本来任何顺序都能过。它真正修的是**edit 之间互相干扰**：前一个 edit 的替换文本
里含有后一个 edit 的 `old`，于是后者被当成有歧义拒掉。最小的例子是交换两个名字，以前根本
做不到。**这个错是变异测试抓出来的**——当时那个测试关掉功能也照样绿。

第 6 条的判定用**时间**不用"pid 还活着吗"：这个 crate `forbid(unsafe)`，查进程存活要么用
libc 要么 spawn 一个进程，而一次 commit 比 60 秒短四个数量级，两种判法答案一样。代价是
中断后最多 60 秒内看不出来——而那段时间里 transport 本来就是死的。

### 还没做

| | 事 | 依据 | 成本 |
|---|---|---|---|
| 5 | 输出上限跟客户端 25k token 对齐 | 官方阈值，ccnm 的上限是自己定的，未对齐 | 小 |
| 7 | 编辑后回读改动区域返回给模型 | claude-code#32658 的三种静默失败 | 中 |
| 8 | 远端 MCP server 做成可重新接管的 daemon | Zed；根治僵尸会话 | 大 |

**不做**：放松匹配到 fuzzy（危险大于收益，见第 3 节）；为 TOCTOU 改路径解析。
