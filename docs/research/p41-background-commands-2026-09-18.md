# P41 后台命令；客户端取消或断开时停掉命令（2026-09-18）

环境：macOS arm64 开发机；Claude Code 2.1.273（只读打包代码，未登录）；Codex 0.154.0（本机假模型，`sandbox-exec` 禁非本机出站）。零模型额度，没上真机。

## 1. 结论

- `exec_command` 加 `run_in_background`：起了就返回 `output_ref`，命令在 Runtime 上接着跑，不给 `timeout_ms` 就没有期限。
- `read_output` 加 `wait_ms`：命令还在跑时等它结束（最多 600000 毫秒），结束就提前返回；后台命令的页多一行状态，读到末尾写"到目前为止"。
- 第十一个工具 `stop_command`（只在 coding 模式有）：进程组先 TERM，2 秒后 KILL。
- 同一个 server 进程最多 8 个后台命令同时在跑；server 结束前停掉它起的所有命令，再放写锁。
- 顺带修了两个现有缺陷：`notifications/cancelled` 之后命令照跑；客户端断开时 server 要等命令自己跑完才退出。

## 2. P41.1 实测

### 2.1 两个 Host 怎么做

脚本和原始结果在 toexec 仓库 `evidence/v3-parity/background-exec/`（3274b1f），打包代码摘录也在那里。这里只写结论和它决定了什么。

| 问题 | Claude Code 2.1.273 | Codex 0.154.0 | 决定了什么 |
| --- | --- | --- | --- |
| 怎么起后台命令 | Bash 的 `run_in_background`；前台超时后**转后台**而不是杀掉 | `exec_command` 等满 `yield_time_ms`（默认 10000）还没结束，就返回会话号 | 参数名照 Claude Code；前台超时语义不改（冻结契约里是杀掉） |
| 怎么看、怎么等 | 命令结束时推通知叫醒模型；TaskOutput（`block`、`timeout` 默认 30000、上限 600000，每 100 ms 看一次）已标弃用，改让模型读输出文件 | 空 `write_stdin` 轮询：实测进程在 1.34 秒结束，调用在那时返回，只给上次之后的新输出 | `read_output` 加 `wait_ms`，结束即返回，上限 600000，每 100 ms 看一次 |
| 怎么停 | TaskStop（`task_id`） | 没有单独的工具（tty 里发 Ctrl-C） | 新工具 `stop_command` |
| 会话结束时 | 有"给最终回答时终止"的分支（`reapedAtFinalResponse`） | 实测：`codex exec` 退出 1 秒后，它起的 `sleep 60` 已经不在 | 连接结束时停掉所有命令 |
| stdin | 没有 | 不开 tty 时 stdin 是关的 | 不做 |

MCP 调用本身能等多久（决定 `wait_ms` 上限）：

- Claude Code：总超时默认 1e8 ms；**空闲超时**（server 既不回答也不发进度）stdio 30 分钟、其他传输 5 分钟；交互会话里一次调用超过 120 秒，Host 自己把它转成后台任务，结果照样回来。
- Codex：调一个睡 75 秒的探针工具，等满 75.1 秒，没超时，server 没收到取消。更长的没测。

**为什么命令结束时不通知模型**：MCP 里 server 叫醒模型只有两条路，这个版本都用不上——Claude Code 的 channels（`notifications/claude/channel`）要组织设置 `channelsEnabled`；MCP 标准长任务扩展（SEP-2663，`tasks/get` 等）的客户端代码在，入口判断是 `function CL(){return!1}`。

### 2.2 ccnm 的两个现有缺陷

用真实 ccnm 二进制（外部 MCP coding 模式）和一个手拼 JSON-RPC 的脚本复现，改之前：

| 做了什么 | 结果 |
| --- | --- |
| `exec_command` 跑 `echo $$ > a.pid; exec sleep 30`，进程起来后发 `notifications/cancelled` | 2 秒后那个 `sleep` **还活着**；server 照常回答 `ping` |
| `exec_command` 跑 `exec sleep 8`，进程起来后关掉 stdin（客户端断开） | server **8.0 秒**后才退出，这期间一直持有 workspace 写锁 |

原因：rmcp 3.2.0 收到取消只是取消请求的令牌，工具处理函数不看它就照跑；`run()` 结束时 drop tokio runtime，会等 `spawn_blocking` 里还在跑的命令。前台命令最长 600 秒，所以最坏是断开后又占 10 分钟写锁；后台命令没有期限，不修的话就是永远。

这个通知 Claude Code 会发：它调 MCP 工具时把这次工具调用的中止信号（`signal`）交给 MCP SDK，SDK 在信号触发时发 `notifications/cancelled`（2.1.273 打包代码）。按 Esc、停掉被自动转后台的调用是不是正好触发这个信号，没抓包确认。

## 3. 实现与决定

**`process.rs`：起命令和等它结束拆成两步**（71b8fa9）。`spawn_captured` 返回 `Started`，`wait()` 等管道读完再收回子进程；`stopper()` 给出能克隆到别的线程的 `Stopper`。`stop(grace, give_up)`：TERM 整个进程组 → 等 `grace` → 反复 KILL 到管道读完 → 超过 `give_up` 放弃。超时 watchdog、`stream_lines`、`SystemRunner::run` 共用同一个 `Stopper`，反复杀的逻辑只剩一份。

- 放弃是为了一种信号够不着的子进程：离开了进程组（`setsid`）又占着管道。不放弃的话，会话结束时 server 永远退不出去。单测 `stop_gives_up_on_what_left_the_process_group`。
- 收回子进程和发信号在同一把锁里，收回之后不再对那个 pid 发信号——它可能已经被 OS 分给别的进程。
- **连带修掉的缺陷**：关掉 stdout / stderr 后继续跑的子进程，原来管道一 EOF watchdog 就收手，之后的 `wait` 一直等它自己结束。`sh -c 'exec >/dev/null 2>&1; sleep 30'` 配 400 ms 超时，旧代码 30.02 秒才返回且不算超时（把新测试放到旧代码上跑的结果）；现在到点被杀。

**`mcp/jobs.rs`：本 server 进程起的所有命令**（d1af8ab）。

- 每条命令开始前登记，由等它的那个线程在写完结果后注销。
- `Stop`：命令开始前到达的停止请求会被记住，命令一起来就停；调用方开始前先查，被取消的调用不启动命令。第一个原因为准。
- `stop_all`：关门（之后不再起新命令）→ 并行停掉所有命令 → 等它们都注销，最多 10 秒。8 个命令一起停只花一次 2 秒的宽限。
- 后台命令的 `status` 文件放在 run 目录里（`command`、开始时间、`timeout_ms`，结束后加退出码、是否超时、停的原因、字节数），先写临时名再改名。`read_output` 看它和 run 的锁判断在跑还是结束；状态说在跑、锁却没人持有，就是 server 被强杀了，照实写"不知道怎么结束的"。

**上限 8 个**：P31 规定进行中的 run 不参与回收，每个 run 最多 128 MiB（两个流各 64 MiB），8 个最多多占 1 GiB。一个 dev server、一个 watch、一个测试是 3 个。第 9 个报 `CCNM_E_INVALID_ARGS`（"改一下再调"），不报 `CCNM_E_POLICY`（"别再试了"）——停掉一个就能起，消息里列出在跑的 `output_ref`。

**宽限 2 秒**：停的常常是有东西要收拾的服务（dev server 的锁文件、测试用的临时库）。超时仍然直接 KILL：那是命令已经用完了自己的时间。

**取消**：`exec_command` 从 rmcp 的 `RequestContext` 拿取消令牌（没加 `tokio-util` 直接依赖），和执行任务 `select!`，取消时在阻塞线程里停命令，再等执行任务写完结果。`read_output` 的等待放在 async 一侧、同样听取消——断开时不留一个要 server 等着的阻塞线程。

**会话结束的顺序**：`run()` 在服务结束后先持有一份 `Inner`（写锁在里面）→ `jobs.stop_all()` → drop runtime → 外部入口删自己的输出 → 最后放 `Inner`。先停命令再放锁，新会话拿到 workspace 时不会还有旧命令在改文件。

**不改的**：前台命令的结果一个字节都不变（`read_output` 对没有 `status` 文件的 run 不加状态行，单测 `stop_command_stops_it_and_reports_what_became_of_it` 断言了前台 run 的页逐字节不变）；冻结契约里的前台超时语义；`read_file` 等其他工具。

## 4. 验证

| 检查 | 结果 |
| --- | --- |
| `process::` 单元测试 | 25 个，新增 5 个：先 TERM、忽略 TERM 就 KILL、`Duration::MAX` 不起 watchdog、关掉输出的子进程到点被杀、离开进程组的放弃；`--test-threads=64` 连跑 3 轮通过 |
| `mcp::jobs` 单元测试 | 12 个：开始前的停止、上限与关门、`stop_all` 等到注销、状态行、状态文件往返；端到端的后台边跑边读、`stop_command` 与再停、前台 ref 被拒、超时、会话结束停掉前台和后台、取消停掉命令且开始前取消不启动、第 9 个被拒、server 被强杀后的状态 |
| 接线 | `MCP_TOOLS` 11 个；`tools-list-coding.json` 和 schema 的 `tool_name` 各加一项，`external_mcp` 的 `published_tool_tables_match_the_running_server` 逐字节比对通过；`provider_compat` 记为有据可查的差异；P11 / P12 脚本和测试的工具清单 |
| 中立 MCP 客户端（真实二进制） | 新增 5 个：后台读与 `wait_ms`（1 秒的命令在 5 秒内返回）、`stop_command` 后进程不在、第 9 个被拒且断开后 8 个进程都不在、取消后进程不在且连接照常、断开时前台和后台进程都不在、server 8 秒内退出、同一个 workspace 马上能开新的 coding 会话 |
| 样例 | `call-exec-background-ok.json`、`call-read-output-running.json`、`call-stop-command-ok.json` 取自真实 server 输出 |

门禁数字见 status.json 里 P41 的 evidence。

## 5. 没验的、限制

- 真实模型会不会用 `run_in_background` / `wait_ms`；Claude Code 里按 Esc 实际发出的通知（没登录，看不到请求）。
- `wait_ms` 等满 10 分钟时两个 Host 会不会先超时：Claude Code 按代码是 30 分钟空闲超时，Codex 只测到 75 秒。
- Linux 上没跑。`kill -TERM -- -pgid` 在 GNU `kill` 上的写法和 `-KILL` 同一个形状（P12 查过 `--` 的问题），没实测。
- 后台命令活不过会话：断开、Managed 会话 `/mcp Reconnect` 都会停掉它们。
- `mcp-serve` 被 `SIGKILL` 时它起的进程组没人收，前台命令今天也一样；没有做"下次启动时按标记清孤儿"。
- 命令结束时不通知模型；不做 stdin / tty、逐行推送。
