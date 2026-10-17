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

## 5. 不在本次范围、建议另做

- `read_output` 的 offset 落在字符中间、或输出中间有非 UTF-8 字节时报"limit 太小"（2.4 节，10 次）：缺陷，修法是 offset 落在字符中间时挪到下一个字符开头、页尾只切掉没写完的最后一个字符。
- `search_text` 的 `path` 收单个文件（2.4 节，51 次）：照原生 Grep。要在冻结协议里记一笔加法。
