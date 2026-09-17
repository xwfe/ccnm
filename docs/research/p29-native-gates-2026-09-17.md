# 原生链补测：同会话并发、在途请求与资源上限（P29，2026-09-17）

验收见 ROADMAP 的 P29，起因是 toexec v2 计划第 11 节"对齐检查"第 7–9 行把 V2-G07 的"同会话并发修改"、V2-G08 的"在途超时"记为没测，V2-G09 资源上限记为没做。脚本和每个场景的原始结果在 toexec 仓库的 [`evidence/v2-c/p29-gates/`](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/p29-gates/README.md)。

**一句话结论**：查出一个缺陷——**macOS Runtime 上 exec-server 崩溃时，正在做带沙箱文件读写的 fs helper 活过了放锁，锁 `released` 之后还把数据写了出去（20/20）**。原因是 helper 的环境被 exec-server 清空、不带会话标记，而收尾只在 exec-server 自己不退出时才杀它的进程组。按停止点本阶段只记录，修复另立阶段（第 5 节）。其余各项都符合门禁：不等回包连发 280 个请求 20/20 全对；同一路径并发写 20/20 是某一份完整内容；请求在途时断开 20/20 干净；200 MiB 连续输出时 `exec-serve` 峰值 RSS 5.5 MiB，客户端不读时命令停住不前进；磁盘写满只回错误、会话照常。**用户会撞上的上限**：原生链上 Codex 一次写的单个文件超过约 24 MiB，整个会话被结束（第 4.3 节）。全程零模型额度，只在本机 macOS arm64 上测。

## 环境

本机 macOS 26.6.2 arm64，Codex 0.154.0（Homebrew 的 `codex exec-server`，Seatbelt 沙箱），ccnm 为 P29 认领提交 `d5a5b76` 的 release 构建。客户端是一个不 import ccnm 或 Codex 的 Python JSON-RPC 客户端，直接当 `exec-serve` 的 stdin/stdout（没有 ssh 那一跳，P24 已在真机上测过 ssh/sshd 故障）；请求用 P21 录下的 Codex 原始请求（`tests/fixtures/codex-0.154.0/exec-server/`）。没有 Codex TUI、没有模型接口。

源码位置都指 [openai/codex](https://github.com/openai/codex) 的 tag `rust-v0.154.0`，下文省略 `codex-rs/` 前缀。

## 1. 适用性：每个子项归谁（P29.1）

先按源码分清楚：原生链上"并发、在途、资源"涉及三方——Codex 客户端（Agent 侧）、ccnm `exec-serve`（Runtime 侧转发与监督）、exec-server（Runtime 侧执行）。ccnm 只该对自己那一段负责，但放锁条件必须覆盖执行端的全部写入。

| 门禁子项 | 归谁 | 依据 | 本阶段结果 |
| --- | --- | --- | --- |
| G07 只读不取写锁、可与 coding 共存 | 不适用 | 原生链只开 coding（P22.1），只读走 MCP（P11 已验） | — |
| G07 coding 跨入口竞争同一资源 | ccnm | P24.2 真机 74/74 | 不重测 |
| G07 同会话并发修改串行 | Codex 客户端串行；exec-server 并发执行；ccnm 按到达顺序转发、不串行也不重排 | `tools/src/tool_executor.rs` 的 `supports_parallel_tool_calls` 默认 `false`，`core/src/tools/handlers/apply_patch.rs` 没改，执行时拿每轮一把读写锁的写锁（`core/src/tools/parallel.rs`）；`exec_command` 声明可并行（`handlers/unified_exec/exec_command.rs`）；exec-server 对同一连接的请求并发处理（`exec-server/src/server/request_dispatcher.rs` 的信号量） | 第 2 节：280 个混发请求 20/20；同路径并发写 20/20 完整 |
| G07 监督进程持锁覆盖写进程生命周期 | ccnm | 命令进程带会话标记（P22）；fs helper 不带（`exec-server/src/fs_sandbox.rs` 的 `FS_HELPER_ENV_ALLOWLIST` 只留 `PATH`、`TMPDIR`、`TMP`、`TEMP`，并 `env_clear()`） | 第 3 节：**exec-server 崩溃时 helper 活过放锁，20/20** |
| G08 前端断开、SSH 黑洞、监督器/服务/子进程崩溃 | ccnm | P24.3 真机 | 不重测 |
| G08 在途超时 | 两边都没有请求级超时，这是对的 | Codex 客户端只给 `environment/info`、`environment/status` 设超时（`exec-server/src/client.rs` 里 `call_with_timeout` 仅这两处），文件和进程请求一直等；传输断了不重连（P23）。命令由 Codex 结束时发 `process/terminate`（进程超过 64 个时修剪、失败、结束全部，`core/src/unified_exec/process_manager.rs`） | 第 3 节：`terminate` 5/5；请求在途时断开 20/20 |
| G08 已执行未回包保留 unknown、不重放 | Codex 不重放；ccnm 每行只转发一次 | P23/P24；本阶段并发测试每个 id 恰好一个回答 | 符合 |
| G08 stdio/ws 生命周期分别记录 | 不适用 | 产品只有 stdio（P23） | — |
| G09 头尾边界、多字节 | Codex 客户端 | exec-server 按字节块发，不按字符切；截断和 token 预算在 Codex（`core/src/unified_exec/mod.rs` 的 `UNIFIED_EXEC_OUTPUT_MAX_BYTES` 1 MiB）；ccnm 不看内容 | 第 4.4 节：多字节输出原样到达 |
| G09 200 MiB 连续输出，内存有界 | ccnm 转发层与 exec-server 各自有界 | ccnm 逐块转发（64 KiB 缓冲）；exec-server 每进程只留最近 1 MiB（`exec-server/src/local_process.rs` 的 `RETAINED_OUTPUT_BYTES_PER_PROCESS`），发往客户端的通道容量 128（`exec-server/src/connection.rs`），客户端不读就一路反压到命令的管道 | 第 4.1 节 |
| G09 大文件读 | exec-server 自己的上限 | `fs/readFile` 整个文件进一个回包，上限 512 MiB（`exec-server/src/local_file_system.rs` 的 `MAX_READ_FILE_BYTES`） | 第 4.2 节 |
| G09 磁盘配额/写失败 | exec-server 报错，ccnm 原样转发 | — | 第 4.3 节 |
| G09 过期引用、快速完成后续读 | exec-server | 进程结束后再留 30 秒（`local_process.rs` 的 `EXITED_PROCESS_RETENTION`）；每连接最多 128 个打开的读句柄（`exec-server/src/file_read.rs`）。Codex 0.154.0 实际不调 `process/read`、`fs/open`（P21 录到的方法里没有），输出走通知 | 第 4.4 节 |
| G09 磁盘有界 | ccnm 不存输出 | 与 MCP 入口每流 64 MiB 保留不同，原生链上输出只在 exec-server 内存里 | — |

**为什么不给转发层加请求级超时**：Codex 对文件和进程请求不设超时也不重试。ccnm 替它回一个"超时"错误，Codex 会当作失败继续，而执行端那条请求可能正在做完——这正是门禁要避免的"已执行未回包"被说成失败。客户端整体消失由 P26 的探活管，单个请求慢不是 ccnm 能替执行端判断的事。

## 2. 并发（P29.2）

**不等回包连发**（`concurrency.py pipelined`，20 轮）：一条命令往 stdout 刷 16 MiB 的同时，一口气发 280 个请求——40 个带沙箱的写、40 个读、40 个起进程、40 个 `fs/getMetadata`，交错着 40 个越界写、40 个提权命令、40 个 `http/request`（后三类 ccnm 要拒）。每轮收到 2412 行：

| 检查 | 20 轮结果 |
| --- | --- |
| 解析不了的行 | 0 |
| 没回答 / 回答了不止一次的 id | 0 / 0 |
| 放行的请求回 `result`、被拒的回 ccnm 自己的 `-32600 ccnm refused …` | 280/280 |
| 磁盘上：40 个写入内容正确、40 个命令留下文件、工作区外 0 个文件 | 全对 |
| 刷屏命令的输出字节 | 16777216，一个不少 |
| 退出码 0、锁 `released`、无带标记的残留进程 | 全对 |

**同一路径并发写**（`same-path`，20 轮）：同一个文件连发两个 `fs/writeFile`，一个 8 MiB、一个 64 KiB，先后顺序每轮交换。20 轮最终文件都是其中一份的**完整**内容，没有撕裂；两个回包总是按发送顺序到达。但**落下哪一份与发送顺序、回包顺序都无关**：每轮新开会话时 20/20 是先发的那份，同一个会话里连着再测一次则是后发的那份。Codex 按源码不会对同一文件并发写（`apply_patch` 串行），所以这不影响现有使用；以后若有别的客户端，要知道"后回包的就是最后写的"不成立。

**CI 里的对应测试**：`crates/ccnm-cli/tests/exec_serve.rs` 的 `a_burst_of_requests_during_executor_output_gets_whole_lines_and_one_answer_each`。假执行端新加 `FAKE_EXEC_CHATTER` 开关，另起线程发 200 条 20 万字符的通知（每条都比转发缓冲的 64 KiB 长，要分几块写），同时客户端连发 450 个请求（150 个放行写、150 个越界写、150 个 `http/request`）：每行完整、每个 id 一个回答、执行端日志里恰好是那 150 个放行的 id、工作区外没有文件。**先红后绿**：临时把 `copy_executor` 改成每块单独拿输出锁，这个测试立刻报 `a torn line`（第 5 条通知被拒绝消息插断）；改回后通过。

## 3. 在途请求（P29.3）

| 场景（`inflight.py`） | 轮数 | 结果 |
| --- | --- | --- |
| `client-leaves`：`process/read` 带 `waitMs=600000` 等一条不出声的命令，请求在途时客户端关连接 | 20 | 请求确实在途 20/20；`exec-serve` 0.17–0.23 秒退出、退出码 0；锁 `released`；命令被清、无带标记进程、执行端 home 已删 |
| `terminate`：一条命令下有同进程组的子孙两个、`setsid` 脱离的一个，发 `process/terminate` | 5 | 回 `{"running": true}`；同组两个 0.08–0.09 秒内消失；收到 `process/exited`（137）和 `process/closed`；脱离的那个会话期间仍在、锁仍 `held`；关会话 0.44 秒后它被扫掉、锁 `released` |
| `helper-close`：往工作区里的 FIFO 发带沙箱的 `fs/writeFile`，fs helper 卡在打开上；客户端关连接 | 20 | helper 不带会话标记 20/20，与 exec-server 同进程组 20/20；关连接后 exec-server 退出时把 helper 一起杀掉（tokio 的 `kill_on_drop`），锁 `released`，无残留 |
| `helper-crash`：同上，但 SIGKILL exec-server 本身 | 20 | **helper 活下来 20/20**，被挂到 pid 1；`exec-serve` 0.17–0.22 秒后报会话结束、锁 `released`；之后脚本打开 FIFO 读端，**收到了那次写入的内容 20/20** |

`process/read`、`fs/open` 这类 Codex 不调的方法在这里只是拿来制造"一定在途"的请求：它们和 Codex 实际用的请求走同一条转发和收尾路径。

## 4. 资源（P29.4）

### 4.1 200 MiB 连续输出（`resources.py output`，5 轮）

命令循环 200 次、每次往 stdout 写 1 MiB 并把计数写进工作区文件；客户端读到 50 MiB 时停读 60 秒再恢复。

| 检查 | 5 轮结果 |
| --- | --- |
| 客户端解出的字节 | 209715200，5/5 恰好 |
| 除去暂停的耗时 | 1.5–1.6 秒（125–133 MiB/s，上限在 Python 客户端解码） |
| `exec-serve` 峰值 RSS | 5.4–5.5 MiB |
| exec-server 峰值 RSS / 监督进程之下全部进程 | 39.1–39.3 MiB / 42.4–42.5 MiB |
| 停读 58 秒里命令的计数 | 53→53（4 轮）、54→54（1 轮）：不前进 |
| 停读期间 `exec-serve` RSS | 5.5 MiB 上下，不涨 |
| 恢复后 | 读完、`process/closed`、退出码 0、锁 `released`、无残留 |

每轮有一处序号跳 1（在 199 MiB 附近）：5 轮跳过的号都恰好等于 `process/exited` 事件的号——命令退出时管道里还有约 1 MiB 没读完，exec-server 先给退出事件分了号，再继续发剩下的输出。字节数恰好对，没有丢输出。

### 4.2 大文件读（`readfile`，5 轮）

| 检查 | 带沙箱（经 fs helper） | 不带沙箱 |
| --- | --- | --- |
| 200 MiB 文件，回包解出的字节 | 5/5 恰好 | 5/5 恰好 |
| 耗时 | 1.55–2.0 秒 | 0.98–1.07 秒 |
| `exec-serve` 峰值 RSS | 5.4 MiB | 5.4 MiB |
| exec-server 峰值 RSS（按 50 毫秒采样，是下限） | 1012–1034 MiB | 1033–1035 MiB |
| 512 MiB + 1 字节的稀疏文件 | `-32600 file is too large to read: limit is 536870912 bytes` | 同左 |

ccnm 这一段逐块转发，不把 267 MiB 的一行读进内存；**执行端读一个 200 MiB 的文件要约 1 GiB**，读到 512 MiB 上限时按比例约 2.6 GiB。这是 exec-server 自己的上限，ccnm 不再加一道：Codex 只在 `apply_patch` 和 `view_image` 时整读文件（P21），真要限制得改规则表，属于产品决定。

### 4.3 写：单文件上限与磁盘写满

**单文件上限**（`writelimit`）：23 MiB 的文件 base64 之后整行 30.67 MiB，写入成功；25 MiB 的文件整行 33.33 MiB，超过 `exec-serve` 的 32 MiB 单条上限，**会话直接结束**（`exec-serve` 记 `ClientTooLong`，退出码 0，锁 `released`，文件没写）。Codex 的 `apply_patch` 每次把整个文件放进一个 `fs/writeFile`，所以**原生链上 Codex 能写的单个文件约 24 MiB**；超过时 Codex 那边的传输随之断开，之后的命令应当报 P23 实测过的 `exec-server transport disconnected`——这一次写入本身在 Codex 里报什么，没有用真实 Codex 复现。这是 P22 的设计（exec-server 自己 64 MiB 超限时一声不吭断连，ccnm 在前面先停并说明原因），离线测试早就有；本阶段补上了用户能看见的表现，写进[排错手册](../troubleshooting.md#codex-会话里模型报-toolsexec_command-is-not-a-function或-exec-server-transport-disconnected)。

**磁盘写满**（`diskfull`，5 轮）：工作区放在当前用户用 `hdiutil` 挂载的 16 MiB HFS+ 映像上（不需要 root，测完卸载删除）。

| 步骤 | 5 轮结果 |
| --- | --- |
| 写一个 20 MiB 的文件 | `-32603 No space left on device (os error 28)`；磁盘上留下 0 字节的同名文件，剩余空间不变 |
| 紧接着写一个小文件 | 成功，内容正确：会话没断 |
| 命令往盘里写满 | 命令自己报 `head: stdout: No space left on device` |
| 写满之后再写 1 KiB | `-32603 No space left on device` |
| 关会话 | 退出码 0、锁 `released`、无残留 |

### 4.4 过期引用（`expired`，5 轮）

| 步骤 | 5 轮结果 |
| --- | --- |
| 很快结束的命令，`process/closed` 之后立刻 `process/read` | 拿到全部输出，`héllo 多字节` 原样 |
| 31 秒后再读 | `-32600 unknown process id fast` |
| 31 秒后 `process/terminate` | `{"running": false}`，不报错 |
| `fs/open` → `fs/readBlock` → `fs/close` → 再 `fs/readBlock` | 前三步正常；最后一步 `-32004 unknown file read handle \`h1\`` |

都是执行端原样的回答，ccnm 没有改写。

## 5. 发现的缺陷：fs helper 活过放锁

**现象**：macOS Runtime 上，exec-server 进程死掉（被 OOM、被人 `kill -9` 这个 pid、自己崩溃）的那一刻，如果它正有一个带沙箱的文件操作在 fs helper 里没做完，helper 会被挂到 pid 1 上继续运行。`exec-serve` 看到 exec-server 的输出结束，按标记扫进程表、没找到，报告干净并写 `released`。之后 helper 完成那次写入——落在一个别的会话已经可以拿锁的工作区上。`helper-crash` 20/20 复现，写入内容在放锁之后到达。

**为什么 P22 的扫描没覆盖到**：P22 的前提是"exec-server 起的每个进程都继承它的环境、带着会话标记"，规则表拒绝不继承环境的 `process/start` 来保证这一点。但 fs helper 不是经 `process/start` 起的：exec-server 对带沙箱的文件方法（`fs/writeFile`、`fs/remove`、`fs/createDirectory`、`fs/copy`，以及带沙箱的读）自己 spawn `codex --codex-run-as-fs-helper`，先 `env_clear()` 再只放回四个变量。P22 和 P24 的"exec-server 被杀"测试里没有在途的文件操作（P22 用的假执行端根本没有 helper），所以没撞上。

**客户端正常断开时没有这个问题**（`helper-close` 20/20）：exec-server 按 stdin 关闭正常退出时，运行时销毁在途任务，helper 的 `kill_on_drop(true)` 把它杀掉。只有 exec-server 被强杀、来不及跑析构时才漏。

**修法方向（另立阶段，本阶段不改）**：实测 helper 与 exec-server 在同一个进程组（`ps -o pgid` 20/20 相同；ccnm 启动 exec-server 时 `process_group(0)`，helper 没另建组）。exec-server 死后这个进程组还在，`pgrep -g <exec-server 的 pid>` 能找到 helper，而 POSIX 保证进程组还有成员时这个组号不会被新进程复用。所以放锁前除了按标记扫，再把执行端的进程组杀空并确认为空，就能覆盖 helper 以及以后任何不带标记、但仍在这个组里的执行端子进程。收尾里现在只有"exec-server 10 秒不退出"时才杀这个组。

**Linux**：按源码，helper 在 Linux 上由 `codex-linux-sandbox` 套 `bwrap --new-session --die-with-parent` 启动（`linux-sandbox/src/bwrap.rs`），外层还设了 `PR_SET_PDEATHSIG`（`linux-sandbox/src/linux_run_main.rs`），exec-server 死时应随之结束；但 `--new-session` 让它不在 exec-server 的进程组里，上面的修法在 Linux 上够不着它，要靠 die-with-parent。**Linux 没有实测**（本机没有 Linux 上的 Codex，CI 也没有），P24 真机"exec-server 被杀"20 次里同样没有在途文件操作。

**影响范围**：需要"exec-server 恰好在一次文件操作进行中被强杀"，正常的 Codex 文件操作是毫秒级。实际风险窗口很小，但它违反的是放锁条件本身——未确认结束的写入不能放锁——而且复现是确定的。

## 6. 门禁

- `cargo fmt --all --check`：通过
- `cargo clippy --workspace --all-targets -- -D warnings`：通过
- `cargo test --workspace`：784 passed / 0 failed（含新增的 1 个并发测试）
- `cargo test -p ccnm-cli --test exec_serve a_burst_of_requests`：1 passed；先红后绿见第 2 节
- `python3 scripts/check_plan.py`、`git diff --check`：通过

## 7. 没做到、没测到的

- **只在 macOS 上测。**Linux 上的 exec-server 用 bubblewrap，fs helper 的生死关系不同（第 5 节），并发与资源的数字也没有 Linux 版。
- **没有真实 Codex。**请求是录下的 Codex 原始请求，但 Codex 自己怎么调度（并发发几个、会不会在同一文件上并发）只按源码判断；Codex 收 200 MiB 输出、1 GiB 读回包时 Agent 侧的内存没测。
- **没有 ssh。**反压经过 ssh 和 TCP 缓冲时停读多久命令才停住，没测；P26 的探活在停读超过 10 分钟时会结束会话，本阶段只停了 60 秒。
- 磁盘写满只测了工作区所在的盘；Runtime 的 ccnm 状态目录（写锁文件、执行端 home）所在的盘写满没测。
- RSS 按 `ps` 采样，短时峰值是下限；fs helper 的内存在 1–2 秒的读里没采到。
- 同路径并发写只看了最终内容，没查 exec-server 为什么让先发的那份落下（第 2 节）。
