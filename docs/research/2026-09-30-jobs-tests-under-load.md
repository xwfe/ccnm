# 后台命令的用例在高负载下偶发失败：查明与修复（2026-09-30）

接的是 [P64 记录第 5.4 节](2026-09-30-p64-stop-outcome-same-number-builds.md#54-后台命令的两条用例在高负载下偶发没动)留下的两条"没查没改"。**只有离线证据**：本机 macOS 26.6.2 arm64、rustc 1.98.0，没有连远端、没有跑模型、没有推送或部署。协议和线格式都没变。

## 1. 结论

`mcp::jobs` 里一共三条用例会在 64 线程加高负载下偶发失败——交接里写了两条，复现时又看到一条。两条背后是产品缺陷，一条是测试自己在赌时序。

| 用例 | 失败信息 | 性质 | 处理 | 提交 |
| --- | --- | --- | --- | --- |
| `a_server_that_died_leaves_an_unknown_ending` | `r-… is running in another server of this session` | **产品缺陷**：探测"还有没有人在跑"时，探测自己被当成了持有者 | 探测改用共享锁；确定性回归先红后绿 | `6f66675` |
| `a_cancelled_call_stops_its_command`（复现时新看到） | `10.035425292s` | **产品缺陷**：停止落在命令刚起来的那一刻，白等 10 秒；会话恰好这时结束还会把写锁留成 abandoned | 停的动作挪到另一条线程；两条确定性回归先红后绿 | `67ed1f1` |
| `a_background_command_returns_at_once_and_is_read_as_it_grows` | `the waiter left the registry` | **测试赌时序**：断言了一个没人承诺过的先后 | 改成等，不放宽要验的事 | `cdd83ee` |

另外两件看到了、没动，见第 6 节：`apply_patch` 的日志探测有同类问题（已验证）；封存的原生链有一条用例在高负载下也会红（只从输出推断了原因）。

## 2. 怎么复现

不用 `cargo test` 反复起，直接循环跑测试二进制，只留失败那几轮的输出：

```bash
cargo test -p ccnm-core --lib --no-run      # 打印出二进制路径
for i in $(seq 1 40); do target/debug/deps/ccnm_core-<hash> --test-threads=64 > run-$i.log 2>&1 || echo "run $i failed"; done
```

只过滤 `mcp::jobs::` 复现不出第一条：它要别的测试线程同时在 fork。要跑整个库。想让它更容易出，就同时开两路这样的循环。

修之前（起点 `204400f`；`a933463` 之后的提交没动 `mcp/` 和 `process.rs`）：

| 跑法 | 次数 | 红 | 其中 |
| --- | --- | --- | --- |
| 单路，负载 20–37 | 31 | 4 | `grows` 3、`died` 1 |
| 两路同时（另一路是改过的构建），负载 34–87 | 40 | 8 | `grows` 2、`cancelled` 1、原生链那条 5 |
| 两路同时，负载 36–80 | 40 | 9 | `grows` 1、`died` 1、原生链那条 7 |

"负载"是 `uptime` 的 1 分钟 load average，这台机器 10 核；同一时间另一个会话也在这台机器上干活，所以数字比 P64 记的 22–30 高。

## 3. `died`：探测自己成了持有者（产品缺陷）

**背景**：一条后台命令在跑的时候，它的输出目录里有个 `running` 文件，跑它的 server 一直攥着这个文件的锁（flock）。"这条命令还在不在跑"就是看两件事：文件在不在、锁有没有人持有。正常结束会先删文件；server 被强杀时文件留着、锁随进程消失——这时读出来应该是"不知道怎么结束的"。

**缺陷**：[`retention::in_progress`](../../crates/ccnm-core/src/mcp/retention.rs) 判断"有没有人持有"的办法是自己去试着拿一把**排他锁**，拿到就说明没人，然后靠关文件把锁放掉。这有两个洞：

1. **两次探测撞在一起。** 第一次探测拿着锁的那一瞬间，第二次探测（同一个 server 的另一条线程、同一会话的另一个 `mcp-serve`、`ccnm cleanup`）拿不到锁，读成"有人在跑"。
2. **fork 把锁带走。** flock 挂在"打开的文件"上，关文件只有在指向它的所有描述符都关掉之后才放锁。别的线程恰好在探测期间 fork（`exec_command` 每次都 fork），子进程在 exec 之前手里有这个描述符的副本，锁就活到它 exec 为止——负载越高这段越长。下一次探测读成"有人在跑"。

用例撞到的是第 2 种：`read_output` 先探了一次，紧接着 `stop_command` 再探时锁还在某个子进程手里。

**怎么确认的**：临时加了一段测量（没提交），对同一个"没人持有的 `running`"连续探测 4 秒，数读成"有人持有"的次数：

| | 修之前 | 修之后 |
| --- | --- | --- |
| 单独跑（没有别的线程在 fork） | 573,156 次里 0 次 | — |
| 和整个库一起跑（64 线程），三轮 | 536,081 次里 225 次；563,171 次里 84 次；547,352 次里 505 次 | 374,818、439,462、523,265 次里都是 0 次 |

只有别人在 fork 时才出现，和第 2 种对得上。

**用户会看到什么**：要有一条"server 死了、`running` 还在"的 run 才会碰到（比如 `mcp-serve` 被 `kill -9` 之后在同一个受管会话里重连）。这时 `read_output` 可能把它报成 `still running`，`stop_command` 可能报 `running in another server of this session`，`ccnm cleanup` 可能把这个会话算成"正在被用"。都是一瞬间的事，再问一次就对了——所以严重程度不高，但它是个错的回答。对"要不要删输出"的判断来说，误判方向是"当成还在跑"，不会误删。

**改法**：探测改拿**共享锁**（`try_lock_shared`）。run 自己持的是排他锁，所以它在跑时共享锁照样拿不到，判断不变；而共享锁之间互不相挡，两次探测撞在一起、或者上一次探测的锁还留在某个子进程手里，都不再影响下一次探测。没有再加"显式放锁"：之后没有谁会对一个已经存在的 `running` 文件去拿排他锁（run 只在建文件时锁一次，那时文件还在没人看得见的暂存目录里），留着的共享锁挡不到任何人。

和 P34 修的 `apply_patch` 日志是同一类问题（[P34 记录](p34-journal-lock-release-2026-09-17.md)）；`in_progress` 当时靠"结束的 run 会先删文件"躲过去了，但 server 死掉的 run 恰好不删文件。P60 给写入 guard 加的观察（`write_guard::observe`）本来就用共享锁。

**先红后绿**：`mcp::retention::tests::a_probe_is_not_what_holds_a_run_whose_server_died`。照 P34 的办法给探测留了一个只给测试用的钩子，钩子在探测持锁期间运行：里面再探一次（第 1 种），再把描述符副本当 stdin 交给一个 `sleep 60`（和 fork 到 exec 之间子进程手里那份是同一个东西，只是活得够久）。旧代码：

```text
assertion `left == right` failed: (the probe, one looking at the same moment, one after a child kept a copy): none may read as in progress
  left: (false, Some(true), true)
 right: (false, Some(false), false)
```

## 4. `cancelled`：停止落在命令刚起来的那一刻（产品缺陷）

这条不在交接里，是两路同时压的时候在**没改过的构建**上看到的（80 次里 1 次；当时还没改这一处的构建 100 次里也有 1 次，`10.018679667s`）：

```text
thread 'mcp::jobs::tests::a_cancelled_call_stops_its_command' panicked at crates/ccnm-core/src/mcp/jobs.rs:937:9:
10.035425292s
```

10 秒正好是 `STOP_GIVE_UP`——停一条命令最多等这么久就放弃。

**缺陷**：每条命令有一个停止句柄（[`jobs::Stop`](../../crates/ccnm-core/src/mcp/jobs.rs)）。停止请求可以比子进程来得早，这时只记下来，等子进程起来、`attach` 的时候再停。问题在 `attach` 是**当场**去停的：发 TERM，然后等子进程"结束"。而"结束"这个标记只有等子进程的那个人（`Started::wait`）才会打上；`exec_command` 是先 `attach`、再在**同一条线程**上等——等的人正卡在 `attach` 里。于是对着一个第一下 TERM 就死了的进程等满 10 秒，放弃，然后才去等它、立刻拿到结果。

能落进去的窗口是"登记命令"到 `attach` 之间：建输出目录，加 fork/exec。本机 debug 构建量到的宽度：单线程 1.1–31 ms（中位 2.5 ms），64 线程压测下 6.5–124 ms（中位 37 ms）。

**用户会看到什么**，两种：

- **取消一次调用**（Claude Code 里按 Esc）落在这个窗口里：命令马上就死了，但这次调用晚 10 秒才回来。
- **会话结束**（Host 关掉、SSH 断、`/mcp Reconnect`）落在这个窗口里：收尾的 `stop_all` 自己也只等 10 秒，而且比 `attach` 早开始数，所以它先到点，把这条命令当成"停不掉的"报回去。按 P43 的规则，有停不掉的命令就不交出写入互斥——marker 留成 `held` 加一行 `abandoned`，下一个 coding 会话被拒，要人按[写入 guard 残留](../operations.md#写入-guard-残留)清。而那条命令其实早就死了。

后一种更要紧，但要会话恰好在某次 `exec_command` 开头的几毫秒里结束，概率很低；没有在真机上见过。

**改法**：`attach` 发现已经有停止请求时，另起一条线程去停，自己马上返回，让调用方去等。TERM 照样立刻发出，等的人一等到，那条线程也就结束了。起不了线程时退回原来的做法（慢，但还是会停）。

**先红后绿**，两条都按 `exec_command` 的顺序走（先 `attach`、再同一条线程 `wait`）：

- `a_stop_just_before_attaching_does_not_hold_up_the_one_who_waits`：旧代码 `10.004986875s`。
- `a_session_ending_as_a_command_starts_stops_it_too`：`stop_all` 在另一条线程里跑，旧代码它返回 `["run #1"]`（要的是空）——也就是上面说的写锁被留成 abandoned 的那条路。

旧代码上这两条各要跑满 10 秒才红；改后整个 `mcp::jobs::` 模块 14 条 1.05 秒。

## 5. `grows`：测试赌时序（改测试）

用例最后三步是：等 run 读作结束 → 读一页，确认写着 `exited 0` → **立刻**断言 registry（server 记着"哪些命令在跑"的那张表）已经空了。红的就是最后这句。

后台命令的 waiter 线程收尾的顺序是：写最终状态 → 放掉 run（删 `running`、放锁）→ 交出 ticket（从 registry 里摘掉自己）。"run 读作结束"发生在第二步，"registry 空了"发生在第三步，中间有一小段。量了一下（临时测量，没提交；起一条 `true` 的后台命令，自旋着等 run 结束，再看 registry）：

| | 40 次里还在 registry 里的次数 | 隔了多久才摘掉 |
| --- | --- | --- |
| 单独跑 | 32 | 3–35 µs |
| 和整个库一起跑（64 线程），三轮 | 14–16 | 多数几十到几百 µs，最长 0.6 ms、1.5 ms、25 ms |

也就是这一段**一直都在**，只是平时窄到用例的下一行赶不上；负载高时 waiter 线程在两步之间被换下去，用例就看到了。

**为什么说是测试的问题**：

- 这个顺序是故意的，不能反过来。server 收尾时等 registry 空了，就去删"没人持有"的 run（外部会话的 `discard_started`）；如果先交 ticket 再放 run，那一刻 run 还被持有，会被跳过、留在盘上等 7 天过期。
- 代码承诺的只有一个方向——`Ticket` 的注释写的是"离开 registry 时，结果或最终状态已经写好"。用例断言的是反方向（run 结束了 ⇒ 已经离开 registry），没有哪里承诺过。
- 两种结果对调用方是一回事：`read_output` 读到的状态、输出、`eof` 都已经是最终的；`stop_command` 对"已结束但还没摘掉"的命令也照实报它的结局。

**改法**：把"立刻看"改成用 `Jobs::wait_gone` 等它离开（最多 10 秒）——这就是 `stop_command` 自己用的那个等法。要验的事没变：waiter 最终会把位置让出来。顺序和原因写在 `exec::background` 里放 run 的那两行上，只写这一处。

**这一段里唯一能被调用方碰到的事**（记下，没改）：后台命令正好开满 8 个、其中一个刚结束，调用方在它被摘掉之前就起第 9 个，会被拒，错误消息里还把刚结束的那个列为"在跑"。消息自己写着"停一个或等一个结束再起"，重试一次就过。要碰到它，得在几十微秒（高负载下几毫秒）内完成"读到结束 → 发起下一次调用"，经过 ssh 的真实调用方一次往返就不止这个数，模型更不可能。真要堵上，做法是数名额时只数子进程还没被收走的命令；没做，因为碰不到，而且改的是 `admit` 这条所有命令都走的路。

## 6. 看到了、没动的

**`apply_patch` 的日志探测有第 3 节第 1 种洞。** [`patch::probe_journal`](../../crates/ccnm-core/src/mcp/patch.rs) 在 P34 之后是"排他锁 + 显式放锁"，fork 带走锁的那种堵上了，两次探测撞在一起的那种还在：探测持锁的那一瞬间，另一次探测把一份被打断的提交记录读成"还在提交"，于是放行这次 patch（只会漏拦，不会误报）。用一条临时用例验过（没提交，在钩子里再调一次 `still_running`）：读出 `(false, Some(true))`。要撞上需要两个 `mcp-serve` 共用状态目录、在同一个微秒级的瞬间各自探测同一份被打断的记录，比第 3 节那种还难碰到。修法应该一样（探测改共享锁），但那是 `apply_patch` 的拦截路径，没在这次的范围里。

**封存的原生链有一条用例在高负载下会红。** `native::serve::tests::a_large_message_the_client_is_still_taking_is_not_silence`，`tests.rs:478` 的 `assert!(total > SIZE)` 失败，留着日志的 22 次值都是 `62`。修之前修之后都有：两路同时压时每 30–40 次里 3–8 次，单路 40 次里 1 次（负载 21–44）；最开始单路 31 次（负载 20–37）没出现。**原因是从输出推的，没有做实验确认**：62 正好是一条探活请求 `{"id":"ccnm-liveness-1","method":"ccnm/liveness","params":{}}` 加换行的字节数；用例里的 reader 把读到的第一个换行当成那条大消息的结尾，而这套用例的探活间隔是 100 ms——假执行端（`bash -c "head -c … | tr …"`）在高负载下超过 100 ms 才开始出字节时，先到的是探活。原生链 2026-09-17 起封存（见 `AGENTS.md`），所以只记不修；它会让 `--test-threads=64` 的门禁在机器很忙时偶尔红一下。

**没查的**：`a_background_command_returns_at_once…` 里还有两处按墙钟判的断言（起后台命令不到 800 ms、`sleep 1` 的命令"1.x 秒后退出"），这一轮 290 多次整库运行里这条用例红了 6 次，看过输出的 4 次都在 registry 那一句上（另外 2 次的日志没留）；这两处没见红过，没动。

## 7. 验证

**修之后同样的跑法**（三处都改了的构建）：

| 跑法 | 次数 | `mcp::jobs` 红 | 其他 |
| --- | --- | --- | --- |
| 两路同时（另一路是没改过的构建），负载 36–80 | 40 | 0 | 原生链那条 6 |
| 单路，负载 21–44 | 40 | 0 | 原生链那条 1 |

同一轮里没改过的构建 40 次红了 `grows` 1、`died` 1。只修了第 3、5 节那两处的构建另跑过 100 次（两路同时），`grows` 和 `died` 都是 0，`cancelled` 1 次——这是第 4 节被发现的那一次。

**门禁**（都在三处改完之后跑）：

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `cargo test --workspace`：1040 passed / 0 failed（22 个测试二进制）；`cargo test --workspace -- --test-threads=64`：同样 1040 / 0。比 P64 的 1033 多出来的是 P65 已提交的用例和这次的 3 条。
- `cargo +1.89 check --workspace --all-targets --locked`：通过（`try_lock_shared` 是 1.89 稳定的，仓库声明的最低版本就是 1.89）。
- `python3 scripts/ci_gates.py`：计划检查、协议检查、Python 256 条 0 跳过 0 失败（其中有 `tests.test_remote_workspace_mcp`，用的是刚构建的二进制）。
- `git diff --check`：通过。
- 门禁前后各列一次 `$TMPDIR` 和 `/tmp` 下的 `ccnm-*`，没有新增。复现时失败的用例留下的 10 个 `ccnm-jobs-<pid>-*` 目录已删；更早轮次留下的 12 个没动。

**没覆盖的**：Linux 没跑（flock 的共享/排他语义两边一样，但没实测）；真机没验；第 4 节写锁被留成 abandoned 的那条路只在 `Jobs` 这一层做了回归，没有用真实二进制加中立客户端去卡那几毫秒的窗口。

## 8. 为什么没另立阶段

- `docs/plan/README.md` 规定同一时刻最多一个 `in_progress`，只有前一阶段 `blocked` 时才能并行认领。现在 main 上 P65 正在由另一个会话做，不是 blocked。
- 这三处都是已完成的 P41/P42 承诺过的行为没做到位，没有新的验收范围。同类先例都是记在 `observed_gaps` 加一份研究记录：P3 的"64 线程下超时用例 30.5 秒"（根因在产品，已修）、P31 期间的两条并发用例。
- 并行分支另起一个编号，合并时会和 P65 之后的编号撞。

所以 `status.json` 只改了 `observed_gaps`：原来那条改成已查明，第 6 节两件各加一条。`current_task`、`handoff`、P65 的任务记录都没碰。
