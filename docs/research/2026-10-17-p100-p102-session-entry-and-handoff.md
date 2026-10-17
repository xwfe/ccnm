# P100–P102：进会话时接上工具、`-c` 接最新的对话、长对话交接（2026-10-17）

## 1. 用户提了什么

1. 用 ccnm 进项目时，有时一进去工具就是断的（可能是 `-c` 的时候）。能不能进入时主动帮着连，实在连不上就报错并提示。
2. 上下文到 56 万就自动压缩，用户会以为一个窗口能无限做下去。能不能在合适的时候（压缩了好几次，或者一件事做完了）主动把工作交接给新窗口。
3. 还有什么该优化的。

## 2. 实况（fodelf 上的日志、会话记录和转录，只读）

### 2.1 "进去就断"是接上了一个早就断了的会话

10-17 13:50 前后 fodelf 上开着四个受管会话：

| 项目 | 会话 | 开了多久 | 工具 |
| --- | --- | --- | --- |
| codingagent | e418d3a4（`-c` 起的） | 3 小时 46 分 | 通 |
| shun | 0d82fc1b | 18 小时 | 通（1 小时 38 分前重连过） |
| keex | d13df3ae（`-c` 起的） | 1 天 4 小时 | **断了** |
| xdo | 4c67301c | 3 天 16 小时 | **断了** |

断的原因在 Claude 的 MCP 日志里：10-15 和 10-16 的 09:00 UTC 前后各有一次 `ssh: connect to host xdwmbp port 22: Operation timed out`——本机（项目机器）那会儿连不上，Claude 当场重启 MCP server 一次也失败，之后不再试（排错手册已写）。

`ccnm <项目>` 和 `ccnm <项目> -c` 遇到会话已经在跑，就直接接上（`work::start` 的 already-running 分支，P92 也说明过 `-c` 这时不另起）。所以人进去时工具已经断了几个小时到几天，ccnm 既没查也没说。`ccnm status` 是知道的：它按会话的传输进程判断，此刻正写着 keex、xdo "断了——在 Claude 里：/mcp → ccnm → Reconnect"。

10-14 起 fodelf 上**新起**的会话，MCP 连接失败只有两类，都已修：`runtime_user` 让 Runtime 拒绝（P91 起开会话前就报错）、rmcp 3.2.0 锁死新协议（P93）。所以"进去就断"不在新起会话这条路上。

### 2.2 `-c` 接错了对话

codingagent 的转录（`~/.claude/projects/-Users-fodelf--local-state-ccnm-workspaces-codingagent/`）：

| 转录 | 时间（UTC） | 怎么来的 |
| --- | --- | --- |
| f464cc19 | 10-15 05:40–12:04 | 第一次会话 |
| 5daf6e98 | 10-15 07:58（续接时复制来的历史）–14:01 | `-c` 续 f464cc19 |
| b6388640 | 10-15 14:27 – 10-16 02:11 | 5daf6e98 那个会话里 `/clear` |
| 4eec8166 | 10-16 03:05–08:47 | `/clear` |
| 2e2c53a6 | 10-16 08:49 – 10-17 00:54 | `/clear` |
| e418d3a4 | 复制来的历史 – 10-17 01:02 | **10-17 00:56 `-c`，续的是 5daf6e98** |
| 37f61ffc、12670e43 | 10-17 01:55 起 | `/clear` |

用户做完一件事就 `/clear`。Claude Code 每次 `/clear` 都换一个对话 id、写一个新转录；ccnm 只知道起 Claude 时给的 `--session-id`。P92 的 `-c` 找"最近一次留了转录的 ccnm 会话"，再用它的 ccnm 会话 id 续，于是续的是那个会话**开头**那段对话：10-17 00:56 的 `-c` 接上的是 10-15 的 5daf6e98，10-16 一整天（b6388640、4eec8166、2e2c53a6）都被跳过。接上之后 3 分钟就压缩了一次（5daf6e98 本来就 52.7 万 token）。

10-16 00:20 那次 `-c`（会话 a2dd668d）续的同样是 5daf6e98；它的转录 a2dd668d.jsonl 不存在，而 b6388640.jsonl 在那个进程里继续被写到 02:11——看着是进去后用 `/resume` 手动切到了 b6388640（没有直接证据）。

### 2.3 压缩有多频繁

同一批转录里的自动压缩，压缩前都在 52.7 万–53.1 万 token（P88 的 56 万窗口）：b6388640 两次（相隔约 10 小时，中间大半是空闲），4eec8166 两次（相隔约 2.6 小时），37f61ffc 两次（相隔约 55 分钟），e418d3a4 接上后 3 分钟一次。也就是说，干活密的时候一两个小时压缩一次。

Claude Code 2.1.300 自己的系统提示写的是压缩之后"you don't need to wrap up early or hand off mid-task"；空闲一阵后界面角落出 `new task? /clear to save … tokens`（四个会话的界面上都看得到），但 `/clear` 之后什么都不带。

### 2.4 模型最常撞的 ccnm 工具报错（10-08 起，fodelf 全部 ccnm 日志）

| 次数 | 工具 | 报错 |
| --- | --- | --- |
| 51 | `search_text` | `<path> is not a directory; search_text searches a tree`（模型传了一个文件，原生 Grep 收文件） |
| 31 | `apply_patch` | `version is required for this op; call read_file first`（没读就改，原生 Edit 也要求先 Read） |
| 12 | `exec_command` | `Connection closed`（通道断了） |
| 10 | `read_output` | `limit N is too small to hold one character at this offset`，N 是 4000–7000 |
| 5 | `exec_command` | `timeout_ms is at most 600000` |

`read_output` 那条是缺陷：`output.rs` 的 `boundary_before` 用 `from_utf8(...).valid_up_to()` 找页尾，它返回的是**第一个**不合法字节的位置。offset 落在多字节字符中间（模型拿 `total_bytes` 减出来的 offset 很常见，输出里有中文、`✓` 时就会落进去），第一个字节就不合法，整页变 0 字节，报"limit 太小"；输出中间有一个非 UTF-8 字节时，读到那里之后每一页都这样。这两条不在本次范围，见第 5 节。

## 3. 零额度实测：Claude Code 2.1.300 能靠什么

夹具 [p100-session-hooks.py](probes/p100-session-hooks.py)：真实 Claude Code 接本机假模型，HOME 是空临时目录，`sandbox-exec` 只放本机出站，启动参数照 ccnm 的 `launch_cmd`，一个冒充 ccnm 的最小 stdio MCP server（目录里有 `dead` 文件时一启动就退出，模拟项目机器连不上），settings.json 挂一个把 stdin 记下、往 stdout 打标记的钩子。全部在单独的 tmux server 里。

```bash
python3 docs/research/probes/p100-session-hooks.py <仓库外的新目录> \
    --claude cc-300=/opt/homebrew/Caskroom/claude-code@latest/2.1.300/claude
```

### 3.1 重连（P100）

| 做了什么 | 结果 |
| --- | --- |
| 杀掉 MCP server，`dead` 在 | Claude 自己重启一次也失败，界面上什么都不说 |
| `send-keys -l '/mcp reconnect ccnm'`，0.5 秒后 `Enter`，`dead` 还在 | 界面 `⎿  Failed to reconnect to ccnm`，server 没起来 |
| 去掉 `dead` 再敲一次 | 0.86–0.89 秒 server 起来，界面 `⎿  Successfully reconnected to ccnm`，下一次请求里 `mcp__ccnm__read_file` 回来了 |
| 模型正在回答（底栏 `esc to interrupt`）时敲 | 当场执行，接上的提示出现在右侧，不排队 |

输入框的样子（`capture-pane -p`，没有颜色）：

```text
──────────────────────────────────────
❯                                   ← 空闲且空
──────────────────────────────────────
  ⏸ manual mode on · ? for shortcuts · ← for agents
```

有没发出去的字时是 `❯ a draft that is never sent`；模型正忙时输入框照样是 `❯ `，底栏是 `esc to interrupt`。`Ctrl+U` 会删掉草稿（界面提示 `Ctrl+Y to paste deleted text`），ccnm 不用它：删别人的草稿不该是 ccnm 替人决定的。

### 3.2 会话钩子（P101、P102）

| 时机 | 触发 | `session_id` | stdout 去哪 |
| --- | --- | --- | --- |
| 启动 | `SessionStart`，source `startup` | `--session-id` 给的 | 第一条请求 |
| 每轮结束 | `Stop`，带 `last_assistant_message` | 当前 | 不给模型 |
| `/compact` | `PreCompact`（trigger `manual`）→ `SessionStart` source `compact` → `PostCompact` | 不变 | `compact` 那次进下一条请求；`PreCompact`、`PostCompact` 的输出连同钩子命令行显示在界面上 |
| `/clear` | `SessionEnd`（reason `clear`，旧 id）→ `SessionStart` source `clear` | **新 id** | 下一条请求里的一条 system 消息：`SessionStart:clear hook success: …` |
| `--session-id B --resume A --fork-session` | `SessionStart` source **`fork`** | B | 第一条请求 |

`Stop` 的 `last_assistant_message` 是最后一条回复的原文，例如 `"Done with this part.\n\n[ccnm handoff]\nGoal: …"`，不用去读转录文件。

## 4. 定下的做法

- **P100**：只在人进入一个已经在跑、工具又断了的 Claude 会话时，替人敲一次 `/mcp reconnect ccnm`。敲之前先像起新会话那样问 Runtime，问不通就报错、不接上；输入框里有字就不敲。这不是撤掉的 P98（会话跑着时自己接回来）。
- **P101**：`SessionStart` 钩子把 Claude 此刻的对话 id 记进 ccnm 会话目录，`-c` 从最后一段往前找留了转录的。
- **P102**：交接由模型写、由人 `/clear` 确认，ccnm 不替人清上下文。对话开始时告诉模型怎么写交接；每次压缩后告诉它第几次，第 3 次起要它到下一个能停的地方就交接；`Stop` 存下以 `[ccnm handoff]` 起头的最后一条回复；`/clear` 之后把它交给新对话。

## 5. 实现与验证

### 5.1 P100：进会话时接上工具

`work::start` 的"已经在跑"分支（`crates/ccnm-core/src/work.rs` 的 `tools_on_entry`）：会话是远端的、传输进程不在（`ccnm status` 判"断了"的同一个依据）时——

1. Codex：不问不敲，报 P96 那句。
2. Claude：先 `dial_runtime`（`internal hello` + `internal runtime-audit`，起新会话前的同一套），问不通或 Runtime 不肯就报它的错误码。
3. `tmux capture-pane -p` 读屏，`provider/claude/tui.rs` 认输入框：只有两条横线夹着的空 `❯` 才敲；有字、开着别的界面、vim 模式都不敲，照常进去并提示自己敲。
4. `send-keys -l '/mcp reconnect ccnm'`，500 ms 后 `send-keys Enter`；每 250 ms 看一次、最多 80 次（20 秒）：新出现一行 `Failed to reconnect to ccnm` 即失败；传输进程在、且新出现 `Successfully reconnected`（或连续 5 秒都在）即接上。按"新出现几行"判断，因为上一次重连的结果常还留在屏幕上（真实截屏 `6-after-reconnect` 里就同时有一行旧的失败）。

结果放在 `StartReport.tools`（`Reconnected` / `NotTyped` / `Failed(错误)`），两条 CLI 路径（在项目机器上 `ccnm <项目>`、在 Agent 上直接起）在摘要之后按 `refusal()` 说原因和 `ccnm attach` 退路、用失败本身的退出码返回、不 attach。

测试：`work.rs` 六条（接上并核对 hello、audit、读屏、敲字、回车的顺序；工具通着时只跑一次 `ps`；Runtime 连不上报 `CCNM_E_RUNTIME_UNREACHABLE` 且不敲；草稿不敲；Claude 说失败和等不到都拒；Codex 不问不敲、中文提示含 `ccnm stop` 与 `ccnm attach`）；`tui.rs` 六条，其中一条用夹具抓的六张真实 2.1.300 截屏（`tests/fixtures/claude-tui-2.1.300/`，只改了工作目录那一行）；CLI 一条（假 ssh 回"接不上"：退出码 21、没调 `internal attach`；把两处 `refusal` 换成 `None` 后它红，退出码 0、照样 attach）。

### 5.2 P101、P102：会话钩子

`crates/ccnm-core/src/session/hook.rs`，由交互远端会话 settings.json 的 `SessionStart`、`Stop` 调起 `<ccnm 绝对路径> internal session-hook --payload <会话目录>`；`--print` 不挂，`PreCompact` 照旧是 `printf`。会话目录里多两个文件：`conversations.json`（对话 id 依次记下、当前这段压缩过几次）和 `handoff.md`（最后一条回复里的交接，`/clear` 后交出去就删）。

| 事件 | 做什么 | 对模型说什么 |
| --- | --- | --- |
| `SessionStart` startup | 记 id，次数归 0 | 怎么写交接、`/clear` 后会带过去 |
| `SessionStart` compact | 次数 +1 | 第几次；1、2 次"这件事做完再建议换"，3 次起"到能停的地方就停、写交接" + 怎么写交接 |
| `SessionStart` clear | 记新 id，次数归 0，取走交接 | 有交接时"用户敲了 /clear，上一段以这份交接结束……用户另有吩咐以用户为准" + 交接原文；再加怎么写交接 |
| `SessionStart` fork（`-c`） | 记 id，次数沿用起会话时种下的 | 怎么写交接 |
| `Stop` | 最后一条回复有单独一行 `[ccnm handoff]` 就从那行起存下（超过 32 KiB 截断），没有就删掉旧的 | 不说话 |

`-c` 选对话（`work.rs` 的 `latest_kept_conversation`）：最近那次合格会话里，按 `conversations.json` 从最后一段往前找第一个留了转录的；没有记录的旧会话照旧用 ccnm 会话 id。接的是最后那段时把它的次数种给新会话（`hook::seed`）。用旧选法时新测试是红的（拿到的是第一段）。`ccnm status` 的会话那行在压缩过时多半句"这段对话压缩过 N 次"。

**真二进制端到端**（夹具加 `--ccnm target/debug/ccnm`，同样零额度）：

| 步骤 | `conversations.json` | 发给模型的请求里 |
| --- | --- | --- |
| 启动、说一句 | `ids` 一个、次数 0 | 第一条请求有"怎么写交接" |
| `/compact` 第 1、2、3 次后各说一句 | 次数 1、2、3 | 分别有 `compacted once`、`compacted 2 times`、`compacted 3 times`；`Do not wait for the task` 只在第 3 次 |
| 假模型回一段带 `[ccnm handoff]` 的回复 | — | `handoff.md` 存下的正是从那行起的部分 |
| `/clear` 后说一句 | `ids` 两个、次数 0；`handoff.md` 没了 | 新对话第一条请求里有"the user typed /clear"和交接原文 |
| 种下次数 1，`--session-id C --resume B --fork-session` 起、`/compact` | `ids` 只有 C、次数 2 | `compacted 2 times` |

全程界面上看不到 ccnm 钩子的输出（`PreCompact`、`PostCompact` 的输出 Claude Code 会连命令行一起显示，`SessionStart`、`Stop` 的不显示）。

### 5.3 没验的

- 真机：没换装 fodelf，没在真实的受管会话里进过工具断了的会话、没跑过真实的 `/clear` 交接。要换装得另外授权。
- 真实模型：会不会照着在合适的时候建议换对话，交接写得够不够用。
- 自动压缩：只用 `/compact` 测了，自动压缩走的是同一个 `SessionStart` source（打包代码里 `compact` 只有一种），没单独量。
- 2.1.300 以外的界面布局；fodelf 上受管会话用的是 2.1.302。
- Linux：没跑。

## 6. 不在本次范围、建议另做

- `read_output` 的 offset 落在字符中间、或输出中间有非 UTF-8 字节时报"limit 太小"（2.4 节，10 次）：缺陷，修法是 offset 落在字符中间时挪到下一个字符开头、页尾只切掉没写完的最后一个字符。
- `search_text` 的 `path` 收单个文件（2.4 节，51 次）：照原生 Grep。要在冻结协议里记一笔加法。
